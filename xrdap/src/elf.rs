//! 从 ELF 读全局变量的地址和 DWARF 类型，得到 [`SymbolTable`]。
//! 只处理有固定地址（`DW_OP_addr`）的变量：全局变量和文件内的 static 变量。

use std::collections::HashMap;
use std::path::Path;

use gimli::{AttributeValue, EndianSlice, RunTimeEndian, UnitOffset};
use object::{Object, ObjectSection, ObjectSymbol, SymbolKind};

use crate::types::{Aggregate, Encoding, Member, SymbolTable, Type, TypeId, Variable};
use crate::{Error, Result};

type R<'a> = EndianSlice<'a, RunTimeEndian>;

fn derr(e: gimli::Error) -> Error {
    Error::usage(format!("解析 DWARF 失败：{e}"))
}

/// 读 ELF 文件。
pub fn load(path: &Path) -> Result<SymbolTable> {
    let data =
        std::fs::read(path).map_err(|e| Error::usage(format!("读不了 {}：{e}", path.display())))?;
    load_bytes(&data).map_err(|e| Error::usage(format!("{}：{e}", path.display())))
}

fn parse_file(data: &[u8]) -> Result<object::File<'_>> {
    object::File::parse(data).map_err(|e| Error::usage(format!("不是可以识别的 ELF 文件：{e}")))
}

fn load_bytes(data: &[u8]) -> Result<SymbolTable> {
    let file = parse_file(data)?;
    let endian = if file.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };
    let mut syms = SymbolTable::new();

    for s in file.symbols() {
        if s.kind() == SymbolKind::Data && s.is_definition() {
            if let Ok(name) = s.name() {
                syms.elf_symbols
                    .entry(name.to_string())
                    .or_insert((s.address(), s.size()));
            }
        }
    }

    let load_section = |id: gimli::SectionId| -> std::result::Result<R<'_>, Error> {
        let bytes = match file.section_by_name(id.name()) {
            Some(sec) => sec
                .data()
                .map_err(|e| Error::usage(format!("读段 {} 失败：{e}", id.name())))?,
            None => &[][..],
        };
        Ok(EndianSlice::new(bytes, endian))
    };
    let dwarf = gimli::Dwarf::load(load_section)?;

    let mut units = Vec::new();
    let mut headers = dwarf.units();
    while let Some(h) = headers.next().map_err(derr)? {
        units.push(dwarf.unit(h).map_err(derr)?);
    }
    if let Some(u) = units.first() {
        syms.pointer_size = u.header.address_size() as u64;
    }
    let mut loader = Loader {
        dwarf: &dwarf,
        units: &units,
        map: HashMap::new(),
        syms,
    };
    for i in 0..units.len() {
        loader.scan_unit(i)?;
    }
    Ok(loader.syms)
}

struct Loader<'a, 'd> {
    dwarf: &'a gimli::Dwarf<R<'d>>,
    units: &'a [gimli::Unit<R<'d>>],
    /// .debug_info 里的全局偏移 -> 类型
    map: HashMap<usize, TypeId>,
    syms: SymbolTable,
}

/// 一个 DIE 里本程序关心的属性。
#[derive(Clone)]
struct Die {
    tag: gimli::DwTag,
    name: Option<String>,
    ty: Option<(usize, UnitOffset)>,
    byte_size: Option<u64>,
    encoding: Option<gimli::DwAte>,
    declaration: bool,
    location: Option<u64>,
    member_offset: Option<u64>,
    bit_size: Option<u64>,
    data_bit_offset: Option<u64>,
    count: Option<u64>,
    upper_bound: Option<i64>,
    lower_bound: Option<i64>,
    const_value: Option<i64>,
    specification: Option<(usize, UnitOffset)>,
    children: Vec<UnitOffset>,
}

impl<'a, 'd> Loader<'a, 'd> {
    fn unit_start(&self, i: usize) -> usize {
        self.units[i]
            .header
            .debug_info_offset()
            .map(|o| o.0)
            .unwrap_or(0)
    }

    fn global(&self, unit: usize, off: UnitOffset) -> usize {
        self.unit_start(unit) + off.0
    }

    fn reference(&self, unit: usize, v: AttributeValue<R<'d>>) -> Option<(usize, UnitOffset)> {
        match v {
            AttributeValue::UnitRef(o) => Some((unit, o)),
            AttributeValue::DebugInfoRef(d) => {
                let i = (0..self.units.len())
                    .rev()
                    .find(|&i| self.unit_start(i) <= d.0)?;
                Some((i, UnitOffset(d.0 - self.unit_start(i))))
            }
            _ => None,
        }
    }

    fn string(&self, unit: usize, v: AttributeValue<R<'d>>) -> Option<String> {
        self.dwarf
            .attr_string(&self.units[unit], v)
            .ok()
            .map(|s| s.to_string_lossy().into_owned())
    }

    fn exprloc_address(&self, unit: usize, v: AttributeValue<R<'d>>) -> Option<u64> {
        let AttributeValue::Exprloc(expr) = v else {
            return None;
        };
        let u = &self.units[unit];
        let mut ops = expr.operations(u.encoding());
        let first = ops.next().ok()??;
        if ops.next().ok()?.is_some() {
            return None;
        }
        match first {
            gimli::Operation::Address { address } => Some(address),
            gimli::Operation::AddressIndex { index } => self.dwarf.address(u, index).ok(),
            _ => None,
        }
    }

    fn member_location(&self, unit: usize, v: AttributeValue<R<'d>>) -> Option<u64> {
        if let Some(x) = v.udata_value() {
            return Some(x);
        }
        let AttributeValue::Exprloc(expr) = v else {
            return None;
        };
        let mut ops = expr.operations(self.units[unit].encoding());
        match ops.next().ok()?? {
            gimli::Operation::PlusConstant { value } => Some(value),
            gimli::Operation::UnsignedConstant { value } => Some(value),
            _ => None,
        }
    }

    fn read_die(&self, unit: usize, off: UnitOffset) -> Result<Die> {
        let u = &self.units[unit];
        let mut tree = u.entries_tree(Some(off)).map_err(derr)?;
        let root = tree.root().map_err(derr)?;
        let entry = root.entry();
        let mut d = Die {
            tag: entry.tag(),
            name: None,
            ty: None,
            byte_size: None,
            encoding: None,
            declaration: false,
            location: None,
            member_offset: None,
            bit_size: None,
            data_bit_offset: None,
            count: None,
            upper_bound: None,
            lower_bound: None,
            const_value: None,
            specification: None,
            children: Vec::new(),
        };
        for attr in entry.attrs() {
            let v = attr.value();
            match attr.name() {
                gimli::DW_AT_name => d.name = self.string(unit, v),
                gimli::DW_AT_type => d.ty = self.reference(unit, v),
                gimli::DW_AT_byte_size => d.byte_size = v.udata_value(),
                gimli::DW_AT_encoding => {
                    if let AttributeValue::Encoding(e) = v {
                        d.encoding = Some(e);
                    }
                }
                gimli::DW_AT_declaration => d.declaration = matches!(v, AttributeValue::Flag(true)),
                gimli::DW_AT_location => d.location = self.exprloc_address(unit, v),
                gimli::DW_AT_data_member_location => {
                    d.member_offset = self.member_location(unit, v)
                }
                gimli::DW_AT_bit_size => d.bit_size = v.udata_value(),
                gimli::DW_AT_data_bit_offset => d.data_bit_offset = v.udata_value(),
                gimli::DW_AT_count => d.count = v.udata_value(),
                gimli::DW_AT_upper_bound => {
                    d.upper_bound = match v {
                        AttributeValue::Sdata(x) => Some(x),
                        other => other.udata_value().map(|x| x as i64),
                    }
                }
                gimli::DW_AT_lower_bound => {
                    d.lower_bound = match v {
                        AttributeValue::Sdata(x) => Some(x),
                        other => other.udata_value().map(|x| x as i64),
                    }
                }
                gimli::DW_AT_const_value => {
                    d.const_value = match v {
                        AttributeValue::Sdata(x) => Some(x),
                        other => other.udata_value().map(|x| x as i64),
                    }
                }
                gimli::DW_AT_specification | gimli::DW_AT_abstract_origin => {
                    d.specification = self.reference(unit, v)
                }
                _ => {}
            }
        }
        let mut children = root.children();
        while let Some(child) = children.next().map_err(derr)? {
            d.children.push(child.entry().offset());
        }
        Ok(d)
    }

    fn type_ref(&mut self, r: Option<(usize, UnitOffset)>) -> Result<Option<TypeId>> {
        match r {
            Some((u, o)) => Ok(Some(self.type_at(u, o)?)),
            None => Ok(None),
        }
    }

    /// 取 DIE 处的类型；第一次遇到时解析并记下。
    fn type_at(&mut self, unit: usize, off: UnitOffset) -> Result<TypeId> {
        let key = self.global(unit, off);
        if let Some(&id) = self.map.get(&key) {
            return Ok(id);
        }
        // 先占位，结构体通过指针引用自己时不会无限递归
        let id = self.syms.add(Type::Unknown("解析中".into()));
        self.map.insert(key, id);
        let d = self.read_die(unit, off)?;
        let ty = self.build_type(unit, &d)?;
        if let (Some(name), Type::Base { .. } | Type::Typedef { .. }) = (&d.name, &ty) {
            self.syms.type_names.entry(name.clone()).or_insert(id);
        }
        if let (
            Some(name),
            Type::Struct {
                declaration: false, ..
            }
            | Type::Enum { .. },
        ) = (&d.name, &ty)
        {
            self.syms.tags.entry(name.clone()).or_insert(id);
        }
        self.syms.types[id] = ty;
        Ok(id)
    }

    fn build_type(&mut self, unit: usize, d: &Die) -> Result<Type> {
        let ptr_size = self.units[unit].header.address_size() as u64;
        Ok(match d.tag {
            gimli::DW_TAG_base_type => {
                let size = d.byte_size.unwrap_or(0);
                let enc = match d.encoding {
                    Some(gimli::DW_ATE_signed) => Encoding::Signed,
                    Some(gimli::DW_ATE_signed_char) => Encoding::SignedChar,
                    Some(gimli::DW_ATE_unsigned) | Some(gimli::DW_ATE_UTF) => Encoding::Unsigned,
                    Some(gimli::DW_ATE_unsigned_char) => Encoding::UnsignedChar,
                    Some(gimli::DW_ATE_float) => Encoding::Float,
                    Some(gimli::DW_ATE_boolean) => Encoding::Bool,
                    _ => {
                        return Ok(Type::Unknown(
                            d.name.clone().unwrap_or_else(|| "未知基本类型".into()),
                        ))
                    }
                };
                if enc == Encoding::Float && size != 4 && size != 8 {
                    return Ok(Type::Unknown(d.name.clone().unwrap_or_default()));
                }
                Type::Base {
                    name: d.name.clone().unwrap_or_default(),
                    size,
                    enc,
                }
            }
            gimli::DW_TAG_pointer_type
            | gimli::DW_TAG_reference_type
            | gimli::DW_TAG_rvalue_reference_type => Type::Pointer {
                target: self.type_ref(d.ty)?,
                size: d.byte_size.unwrap_or(ptr_size),
            },
            gimli::DW_TAG_structure_type | gimli::DW_TAG_union_type | gimli::DW_TAG_class_type => {
                let kind = match d.tag {
                    gimli::DW_TAG_union_type => Aggregate::Union,
                    gimli::DW_TAG_class_type => Aggregate::Class,
                    _ => Aggregate::Struct,
                };
                let mut members = Vec::new();
                for &c in &d.children {
                    let m = self.read_die(unit, c)?;
                    let is_member = m.tag == gimli::DW_TAG_member && !m.declaration;
                    let is_base = m.tag == gimli::DW_TAG_inheritance;
                    if !is_member && !is_base {
                        continue;
                    }
                    let Some(ty) = self.type_ref(m.ty)? else {
                        continue;
                    };
                    // DWARF 5 的位域只有 data_bit_offset，DWARF 4 有 bit_size + data_member_location
                    let offset = match (m.member_offset, m.data_bit_offset) {
                        (Some(o), _) => o,
                        (None, Some(bits)) => bits / 8,
                        (None, None) => 0,
                    };
                    members.push(Member {
                        name: if is_base { None } else { m.name.clone() },
                        offset,
                        ty,
                        bit_size: m.bit_size,
                    });
                }
                Type::Struct {
                    kind,
                    name: d.name.clone(),
                    size: d.byte_size,
                    members,
                    declaration: d.declaration,
                }
            }
            gimli::DW_TAG_array_type => {
                let elem = self.type_ref(d.ty)?.unwrap_or(SymbolTable::VOID);
                let mut dims = Vec::new();
                for &c in &d.children {
                    let s = self.read_die(unit, c)?;
                    if s.tag != gimli::DW_TAG_subrange_type {
                        continue;
                    }
                    let n = match (s.count, s.upper_bound) {
                        (Some(n), _) => Some(n),
                        (None, Some(ub)) => {
                            let lb = s.lower_bound.unwrap_or(0);
                            (ub >= lb - 1).then(|| (ub - lb + 1) as u64)
                        }
                        _ => None,
                    };
                    dims.push(n);
                }
                if dims.is_empty() {
                    dims.push(None);
                }
                Type::Array { elem, dims }
            }
            gimli::DW_TAG_typedef => Type::Typedef {
                name: d.name.clone().unwrap_or_default(),
                target: self.type_ref(d.ty)?,
            },
            gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type
            | gimli::DW_TAG_restrict_type
            | gimli::DW_TAG_atomic_type => Type::Qualified {
                target: self.type_ref(d.ty)?,
            },
            gimli::DW_TAG_enumeration_type => {
                let underlying = self.type_ref(d.ty)?;
                let mut values = Vec::new();
                for &c in &d.children {
                    let e = self.read_die(unit, c)?;
                    if e.tag == gimli::DW_TAG_enumerator {
                        if let (Some(n), Some(v)) = (e.name, e.const_value) {
                            values.push((v, n));
                        }
                    }
                }
                let signed = match underlying.and_then(|t| self.syms.value_type(t)) {
                    Some(vt) => vt.kind == crate::types::ScalarKind::Signed,
                    None => values.iter().any(|(v, _)| *v < 0),
                };
                Type::Enum {
                    name: d.name.clone(),
                    size: d.byte_size.unwrap_or(4),
                    signed,
                    values,
                }
            }
            gimli::DW_TAG_subroutine_type => Type::Function,
            gimli::DW_TAG_unspecified_type => Type::Void,
            other => Type::Unknown(format!("{other}")),
        })
    }

    /// 扫描一个编译单元的顶层和命名空间里的变量定义。
    fn scan_unit(&mut self, unit: usize) -> Result<()> {
        let unit_name = self.units[unit]
            .name
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let root_off = {
            let mut cursor = self.units[unit].entries();
            let Some(root) = cursor.next_dfs().map_err(derr)? else {
                return Ok(());
            };
            root.offset()
        };
        self.scan_scope(unit, root_off, "", &unit_name)
    }

    fn scan_scope(
        &mut self,
        unit: usize,
        off: UnitOffset,
        prefix: &str,
        unit_name: &str,
    ) -> Result<()> {
        let scope = self.read_die(unit, off)?;
        for &c in &scope.children {
            let d = self.read_die(unit, c)?;
            match d.tag {
                gimli::DW_TAG_namespace => {
                    let ns = d
                        .name
                        .clone()
                        .unwrap_or_else(|| "(anonymous namespace)".into());
                    self.scan_scope(unit, c, &format!("{prefix}{ns}::"), unit_name)?;
                }
                gimli::DW_TAG_variable => {
                    let Some(addr) = d.location else { continue };
                    let (mut name, mut ty) = (d.name.clone(), d.ty);
                    if let Some((su, so)) = d.specification {
                        let spec = self.read_die(su, so)?;
                        name = name.or(spec.name);
                        ty = ty.or(spec.ty);
                    }
                    let (Some(name), Some(ty)) = (name, ty) else {
                        continue;
                    };
                    let ty = self.type_at(ty.0, ty.1)?;
                    let full = format!("{prefix}{name}");
                    self.syms
                        .vars
                        .entry(full.clone())
                        .or_default()
                        .push(Variable {
                            name: full,
                            addr,
                            ty,
                            unit: unit_name.to_string(),
                        });
                }
                _ => {}
            }
        }
        Ok(())
    }
}
