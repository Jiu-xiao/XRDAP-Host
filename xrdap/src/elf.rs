//! 从 ELF 读变量的地址和 DWARF 类型，得到 [`SymbolTable`]。
//! 只处理有固定地址的变量：全局变量、命名空间和类的静态成员、文件内和函数内的 static 变量。
//! 名字带作用域，例如 `motor::speed`、`Ctrl::pid`、`tick::count`。
//!
//! Read the addresses and DWARF types of variables from an ELF file into a [`SymbolTable`].
//! Only variables with a fixed address are handled: global variables, static members of
//! namespaces and classes, and static variables at file and function scope. Names carry their
//! scope, e.g. `motor::speed`, `Ctrl::pid`, `tick::count`.

use std::collections::HashMap;
use std::path::Path;

use gimli::{AttributeValue, EndianSlice, RunTimeEndian, UnitOffset};
use object::{Object, ObjectSection, ObjectSymbol, SymbolKind};

use crate::types::{Aggregate, Encoding, Member, SymbolTable, Type, TypeId, Variable};
use crate::{Error, Result};

type R<'a> = EndianSlice<'a, RunTimeEndian>;

fn derr(e: gimli::Error) -> Error {
    Error::usage(crate::trf!(
        "Parsing DWARF failed: {e}",
        "解析 DWARF 失败：{e}"
    ))
}

/// 数组一维的元素个数：上界减下界加一；结果是负数或超出 u64 时不知道（损坏的 DWARF 不能让
/// 解析溢出）。
///
/// Element count of one array dimension: upper bound minus lower bound plus one; unknown when
/// the result is negative or does not fit in u64 (broken DWARF must not overflow the parser).
fn subrange_len(lb: i64, ub: i64) -> Option<u64> {
    u64::try_from(i128::from(ub) - i128::from(lb) + 1).ok()
}

/// 位置表达式给出的固定地址：`DW_OP_addr`（或 `DW_OP_addrx`），或者后面再跟一个
/// `DW_OP_plus_uconst`。armclang 把几个全局变量合并进 `.L_MergedGlobals` 时写成后一种。
///
/// The fixed address given by a location expression: `DW_OP_addr` (or `DW_OP_addrx`), optionally
/// followed by one `DW_OP_plus_uconst`. armclang writes the latter when it merges global
/// variables into `.L_MergedGlobals`.
fn fixed_address<T: gimli::Reader>(
    expr: gimli::Expression<T>,
    encoding: gimli::Encoding,
    index: impl Fn(gimli::DebugAddrIndex<T::Offset>) -> Option<u64>,
) -> Option<u64> {
    let mut ops = expr.operations(encoding);
    let base = match ops.next().ok()?? {
        gimli::Operation::Address { address } => address,
        gimli::Operation::AddressIndex { index: i } => index(i)?,
        _ => return None,
    };
    match ops.next().ok()? {
        None => Some(base),
        Some(gimli::Operation::PlusConstant { value }) if ops.next().ok()?.is_none() => {
            base.checked_add(value)
        }
        _ => None,
    }
}

/// 编译器为每个函数生成的函数名字符串，不是程序里的变量。
///
/// Function-name strings the compiler generates for every function; not program variables.
fn is_function_name_string(name: &str) -> bool {
    let last = name.rsplit("::").next().unwrap_or(name);
    matches!(last, "__func__" | "__FUNCTION__" | "__PRETTY_FUNCTION__")
}

/// 读 ELF 文件。
///
/// Read an ELF file.
pub fn load(path: &Path) -> Result<SymbolTable> {
    let data = std::fs::read(path).map_err(|e| {
        Error::usage(crate::trf!(
            "Cannot read {}: {e}",
            "读不了 {}：{e}",
            path.display()
        ))
    })?;
    load_bytes(&data).map_err(|e| Error::usage(crate::trf!("{}: {e}", "{}：{e}", path.display())))
}

fn parse_file(data: &[u8]) -> Result<object::File<'_>> {
    object::File::parse(data).map_err(|e| {
        Error::usage(crate::trf!(
            "Not a recognized ELF file: {e}",
            "不是可以识别的 ELF 文件：{e}"
        ))
    })
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

    // 压缩的调试段（-gz、--compress-debug-sections）先解压
    let mut debug: HashMap<String, std::borrow::Cow<'_, [u8]>> = HashMap::new();
    for sec in file.sections() {
        let Ok(name) = sec.name() else { continue };
        let key = if let Some(rest) = name.strip_prefix(".zdebug_") {
            format!(".debug_{rest}")
        } else if name.starts_with(".debug_") {
            name.to_string()
        } else {
            continue;
        };
        let data = sec.uncompressed_data().map_err(|e| {
            Error::usage(crate::trf!(
                "Reading section {name} failed: {e}",
                "读段 {name} 失败：{e}"
            ))
        })?;
        debug.insert(key, data);
    }
    // 变量地址要落在加载到目标上的段里。被 --gc-sections 丢掉的变量在 DWARF 里地址是 0，地址 0
    // 在 Flash 从 0 开始的芯片（CH32V）上也属于加载段，所以只在符号表里有数据符号在 0 时才算数
    let alloc: Vec<(u64, u64)> = file
        .sections()
        .filter(|s| {
            matches!(
                s.flags(),
                object::SectionFlags::Elf { sh_flags, .. } if sh_flags.contains(object::elf::SHF_ALLOC)
            ) && s.size() > 0
        })
        .map(|s| (s.address(), s.address() + s.size()))
        .collect();
    let load_section = |id: gimli::SectionId| -> std::result::Result<R<'_>, Error> {
        let bytes = debug.get(id.name()).map(|c| &c[..]).unwrap_or(&[][..]);
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
        parent: HashMap::new(),
        found: Vec::new(),
        defs: HashMap::new(),
    };
    for i in 0..units.len() {
        loader.index_unit(i)?;
    }
    let zero_symbol = loader.syms.elf_symbols.values().any(|&(a, _)| a == 0);
    loader.collect_vars(|addr| {
        (addr != 0 || zero_symbol) && alloc.iter().any(|&(lo, hi)| (lo..hi).contains(&addr))
    })?;
    loader.complete_declarations()?;
    Ok(loader.syms)
}

struct Loader<'a, 'd> {
    dwarf: &'a gimli::Dwarf<R<'d>>,
    units: &'a [gimli::Unit<R<'d>>],
    /// .debug_info 里的全局偏移 -> 类型
    map: HashMap<usize, TypeId>,
    syms: SymbolTable,
    /// 作用域类 DIE 和变量 DIE 的全局偏移 -> 最近的作用域祖先（命名空间、类、函数、块）
    parent: HashMap<usize, usize>,
    /// 有固定地址的变量：(编译单元, DIE)
    found: Vec<(usize, UnitOffset)>,
    /// 有名字的 struct/class/union 完整定义：名字 -> (编译单元, DIE)，第一个为准
    defs: HashMap<String, (usize, UnitOffset)>,
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

    /// .debug_info 里的全局偏移 -> (编译单元, 单元内偏移)
    fn locate(&self, key: usize) -> Option<(usize, UnitOffset)> {
        let i = (0..self.units.len())
            .rev()
            .find(|&i| self.unit_start(i) <= key)?;
        Some((i, UnitOffset(key - self.unit_start(i))))
    }

    fn reference(&self, unit: usize, v: AttributeValue<R<'d>>) -> Option<(usize, UnitOffset)> {
        match v {
            AttributeValue::UnitRef(o) => Some((unit, o)),
            AttributeValue::DebugInfoRef(d) => self.locate(d.0),
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
        fixed_address(expr, u.encoding(), |i| self.dwarf.address(u, i).ok())
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
        let id = self.syms.add(Type::Unknown(
            crate::lang::tr("being parsed", "解析中").into(),
        ));
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
                        return Ok(Type::Unknown(d.name.clone().unwrap_or_else(|| {
                            crate::lang::tr("unknown base type", "未知基本类型").into()
                        })))
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
                        (None, Some(ub)) => subrange_len(s.lower_bound.unwrap_or(0), ub),
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

    /// 记下一个编译单元里作用域的父子关系，收集有固定地址的变量（函数和块里的 static 也算）。
    fn index_unit(&mut self, unit: usize) -> Result<()> {
        let mut cursor = self.units[unit].entries();
        let mut stack: Vec<(isize, Option<usize>)> = Vec::new();
        while let Some(entry) = cursor.next_dfs().map_err(derr)? {
            let depth = entry.depth();
            while stack.last().is_some_and(|&(d, _)| d >= depth) {
                stack.pop();
            }
            let key = self.global(unit, entry.offset());
            let scope = stack.iter().rev().find_map(|&(_, k)| k);
            let tag = entry.tag();
            let is_scope = matches!(
                tag,
                gimli::DW_TAG_namespace
                    | gimli::DW_TAG_class_type
                    | gimli::DW_TAG_structure_type
                    | gimli::DW_TAG_union_type
                    | gimli::DW_TAG_subprogram
                    | gimli::DW_TAG_lexical_block
            );
            if is_scope || tag == gimli::DW_TAG_variable || tag == gimli::DW_TAG_member {
                if let Some(p) = scope {
                    self.parent.insert(key, p);
                }
            }
            if tag == gimli::DW_TAG_variable {
                if let Some(v) = entry.attr_value(gimli::DW_AT_location) {
                    if self.exprloc_address(unit, v).is_some() {
                        self.found.push((unit, entry.offset()));
                    }
                }
            }
            if matches!(
                tag,
                gimli::DW_TAG_structure_type | gimli::DW_TAG_class_type | gimli::DW_TAG_union_type
            ) && !matches!(
                entry.attr_value(gimli::DW_AT_declaration),
                Some(AttributeValue::Flag(true))
            ) {
                if let Some(name) = entry
                    .attr_value(gimli::DW_AT_name)
                    .and_then(|v| self.string(unit, v))
                {
                    self.defs.entry(name).or_insert((unit, entry.offset()));
                }
            }
            stack.push((depth, if is_scope { Some(key) } else { None }));
        }
        Ok(())
    }

    /// 带作用域的名字：命名空间、类和函数名用 :: 连起来，词法块不出现在名字里。名字和父作用域
    /// 沿 DW_AT_specification / DW_AT_abstract_origin 走到声明处取（LTO 时要走两步：LTRANS 里的
    /// 定义 -> 早期调试信息里的定义 -> 类或命名空间里的声明）。
    fn qualified(&self, unit: usize, off: UnitOffset, depth: u32) -> Result<Option<String>> {
        if depth > 32 {
            return Ok(None);
        }
        let mut d = self.read_die(unit, off)?;
        let (mut u, mut o) = (unit, off);
        let mut name = d.name.clone();
        for _ in 0..8 {
            let Some((nu, no)) = d.specification else {
                break;
            };
            d = self.read_die(nu, no)?;
            (u, o) = (nu, no);
            if name.is_none() {
                name = d.name.clone();
            }
        }
        let base = match (name, d.tag) {
            (Some(n), _) => n,
            (None, gimli::DW_TAG_namespace) => "(anonymous namespace)".into(),
            (None, gimli::DW_TAG_lexical_block) => String::new(),
            (None, _) => "(anonymous)".into(),
        };
        let Some((pu, po)) = self
            .parent
            .get(&self.global(u, o))
            .and_then(|&p| self.locate(p))
        else {
            return Ok(Some(base));
        };
        let prefix = self.qualified(pu, po, depth + 1)?.unwrap_or_default();
        Ok(Some(match (prefix.is_empty(), base.is_empty()) {
            (true, _) => base,
            (false, true) => prefix,
            (false, false) => format!("{prefix}::{base}"),
        }))
    }

    /// 变量用到的类型里只有声明的 struct/class/union（GCC 在没有定义虚函数的编译单元里只写声明），
    /// 从别的编译单元读出同名的完整定义，[`SymbolTable::complete`] 用它展开成员。完整定义的成员
    /// 也可能只有声明，所以重复到没有新的为止。
    fn complete_declarations(&mut self) -> Result<()> {
        loop {
            let missing: Vec<String> = self
                .syms
                .types
                .iter()
                .filter_map(|t| match t {
                    Type::Struct {
                        declaration: true,
                        name: Some(n),
                        ..
                    } if !self.syms.tags.contains_key(n) && self.defs.contains_key(n) => {
                        Some(n.clone())
                    }
                    _ => None,
                })
                .collect();
            if missing.is_empty() {
                return Ok(());
            }
            for name in missing {
                if let Some((unit, off)) = self.defs.remove(&name) {
                    self.type_at(unit, off)?;
                }
            }
        }
    }

    /// 把找到的变量加进符号表。跳过只是声明的、地址不在目标上的（`placed` 判断）、沿声明也找不到
    /// 类型的、编译器生成的函数名字符串，以及几个编译单元里重复的同一个定义（C++ 的模板静态成员
    /// 和 inline 变量）。
    fn collect_vars(&mut self, placed: impl Fn(u64) -> bool) -> Result<()> {
        let found = std::mem::take(&mut self.found);
        for (unit, off) in found {
            let d = self.read_die(unit, off)?;
            if d.declaration {
                continue;
            }
            let Some(addr) = d.location else { continue };
            if !placed(addr) {
                continue;
            }
            // 类型可能在定义上，也可能在一层或两层之外的声明上
            let mut ty = d.ty;
            let mut cur = d.clone();
            for _ in 0..8 {
                if ty.is_some() {
                    break;
                }
                let Some((nu, no)) = cur.specification else {
                    break;
                };
                cur = self.read_die(nu, no)?;
                ty = cur.ty;
            }
            let Some(ty) = ty else { continue };
            let Some(full) = self.qualified(unit, off, 0)? else {
                continue;
            };
            if is_function_name_string(&full) {
                continue;
            }
            if self
                .syms
                .vars
                .get(&full)
                .is_some_and(|list| list.iter().any(|v| v.addr == addr))
            {
                continue;
            }
            let ty = self.type_at(ty.0, ty.1)?;
            let unit_name = self.units[unit]
                .name
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.syms
                .vars
                .entry(full.clone())
                .or_default()
                .push(Variable {
                    name: full,
                    addr,
                    ty,
                    unit: unit_name,
                });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr_of(bytes: &[u8]) -> Option<u64> {
        let enc = gimli::Encoding {
            format: gimli::Format::Dwarf32,
            version: 4,
            address_size: 4,
        };
        let expr = gimli::Expression(EndianSlice::new(bytes, RunTimeEndian::Little));
        fixed_address(expr, enc, |_| None)
    }

    #[test]
    fn fixed_addresses() {
        // DW_OP_addr 0x20000010
        assert_eq!(addr_of(&[0x03, 0x10, 0, 0, 0x20]), Some(0x2000_0010));
        // DW_OP_addr 0x20000010; DW_OP_plus_uconst 0x88（armclang 的 .L_MergedGlobals）
        assert_eq!(
            addr_of(&[0x03, 0x10, 0, 0, 0x20, 0x23, 0x88, 0x01]),
            Some(0x2000_0098)
        );
        // 两个偏移、寄存器相对的位置、空表达式都不是固定地址
        assert_eq!(addr_of(&[0x03, 0x10, 0, 0, 0x20, 0x23, 4, 0x23, 4]), None);
        assert_eq!(addr_of(&[0x91, 0x08]), None);
        assert_eq!(addr_of(&[]), None);
        // DW_OP_addr 后面跟的不是 DW_OP_plus_uconst（DW_OP_deref）
        assert_eq!(addr_of(&[0x03, 0x10, 0, 0, 0x20, 0x06]), None);
    }

    #[test]
    fn function_name_strings() {
        assert!(is_function_name_string("tick::__func__"));
        assert!(is_function_name_string("__PRETTY_FUNCTION__"));
        assert!(!is_function_name_string("my__func__"));
    }

    #[test]
    fn subrange_lengths() {
        assert_eq!(subrange_len(0, 3), Some(4));
        assert_eq!(subrange_len(1, 0), Some(0));
        assert_eq!(subrange_len(0, -1), Some(0));
        assert_eq!(subrange_len(5, 3), None);
        // 损坏的 DWARF：极端的上下界不能让解析溢出
        assert_eq!(subrange_len(i64::MIN, 0), Some((1 << 63) + 1));
        assert_eq!(subrange_len(i64::MIN, i64::MAX), None);
        assert_eq!(subrange_len(0, i64::MAX), Some(1 << 63));
    }
}
