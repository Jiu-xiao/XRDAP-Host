//! 变量的类型模型（从 DWARF 读出，或用内置的 u8..f64），以及按类型格式化和解析数值。
//!
//! The type model of variables (read from DWARF, or the built-in u8..f64), and formatting and
//! parsing of values by type.

use std::collections::HashMap;

use crate::bytes::{le_u32, le_u64};
use crate::{Error, Result};

/// 类型在 [`SymbolTable::types`] 里的下标。
///
/// Index of a type in [`SymbolTable::types`].
pub type TypeId = usize;

/// 基本类型的编码。
///
/// Encoding of a base type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// 无符号整数。
    ///
    /// Unsigned integer.
    Unsigned,
    /// 有符号整数。
    ///
    /// Signed integer.
    Signed,
    /// 浮点数。
    ///
    /// Floating point.
    Float,
    /// 布尔。
    ///
    /// Boolean.
    Bool,
    /// 无符号 char，按整数处理。
    ///
    /// Unsigned char, handled as an integer.
    UnsignedChar,
    /// 有符号 char，按整数处理。
    ///
    /// Signed char, handled as an integer.
    SignedChar,
}

/// 聚合类型的种类。
///
/// Kind of an aggregate type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aggregate {
    /// `struct`。
    ///
    /// `struct`.
    Struct,
    /// `union`。
    ///
    /// `union`.
    Union,
    /// `class`。
    ///
    /// `class`.
    Class,
}

/// 结构体或联合体的一个成员。
///
/// One member of a struct or union.
#[derive(Debug, Clone)]
pub struct Member {
    /// 匿名结构体或联合体成员为 None，它的字段可以直接访问。
    ///
    /// None for an anonymous struct or union member, whose fields are accessed directly.
    pub name: Option<String>,
    /// 成员相对结构体起点的字节偏移。
    ///
    /// Byte offset of the member from the start of the struct.
    pub offset: u64,
    /// 成员的类型。
    ///
    /// Type of the member.
    pub ty: TypeId,
    /// 位域的位数；不是位域时为 None。
    ///
    /// Width of a bit-field in bits; None when the member is not a bit-field.
    pub bit_size: Option<u64>,
}

/// 一个类型。
///
/// A type.
#[derive(Debug, Clone)]
pub enum Type {
    /// `void`。
    ///
    /// `void`.
    Void,
    /// 基本类型。
    ///
    /// A base type.
    Base {
        /// 类型名。
        ///
        /// Type name.
        name: String,
        /// 字节数。
        ///
        /// Size in bytes.
        size: u64,
        /// 编码。
        ///
        /// Encoding.
        enc: Encoding,
    },
    /// 指针。
    ///
    /// A pointer.
    Pointer {
        /// 指向的类型；None 表示 `void`。
        ///
        /// The pointed-to type; None means `void`.
        target: Option<TypeId>,
        /// 指针的字节数。
        ///
        /// Size of the pointer in bytes.
        size: u64,
    },
    /// 结构体、联合体或类。
    ///
    /// A struct, union or class.
    Struct {
        /// 种类。
        ///
        /// Kind.
        kind: Aggregate,
        /// 标签名；匿名时为 None。
        ///
        /// Tag name; None when anonymous.
        name: Option<String>,
        /// 字节数；未知时为 None。
        ///
        /// Size in bytes; None when unknown.
        size: Option<u64>,
        /// 成员。
        ///
        /// Members.
        members: Vec<Member>,
        /// 只有声明、没有定义（例如 `struct opaque;`）。
        ///
        /// Declaration only, without a definition (e.g. `struct opaque;`).
        declaration: bool,
    },
    /// 数组。
    ///
    /// An array.
    Array {
        /// 元素类型。
        ///
        /// Element type.
        elem: TypeId,
        /// 每一维的元素个数，None 表示长度未知（`int a[]`）。
        ///
        /// Element count of each dimension; None means the length is unknown (`int a[]`).
        dims: Vec<Option<u64>>,
    },
    /// typedef。
    ///
    /// A typedef.
    Typedef {
        /// 别名。
        ///
        /// The alias name.
        name: String,
        /// 指向的类型；None 表示 `void`。
        ///
        /// The aliased type; None means `void`.
        target: Option<TypeId>,
    },
    /// const、volatile、restrict、_Atomic：对访问没有影响。
    ///
    /// const, volatile, restrict, _Atomic: they do not affect access.
    Qualified {
        /// 被修饰的类型；None 表示 `void`。
        ///
        /// The qualified type; None means `void`.
        target: Option<TypeId>,
    },
    /// 枚举。
    ///
    /// An enum.
    Enum {
        /// 标签名；匿名时为 None。
        ///
        /// Tag name; None when anonymous.
        name: Option<String>,
        /// 字节数。
        ///
        /// Size in bytes.
        size: u64,
        /// 底层类型是否有符号。
        ///
        /// Whether the underlying type is signed.
        signed: bool,
        /// 枚举值和名字。
        ///
        /// Enumerator values and names.
        values: Vec<(i64, String)>,
    },
    /// 函数。
    ///
    /// A function.
    Function,
    /// DWARF 里有但这里不认识的类型。
    ///
    /// A DWARF type that is not recognized here.
    Unknown(String),
}

/// 一个全局变量。
///
/// A global variable.
#[derive(Debug, Clone)]
pub struct Variable {
    /// 变量名。
    ///
    /// Variable name.
    pub name: String,
    /// 变量地址。
    ///
    /// Variable address.
    pub addr: u64,
    /// 变量类型。
    ///
    /// Variable type.
    pub ty: TypeId,
    /// 定义它的编译单元（源文件名），用来提示重名。
    ///
    /// The compilation unit (source file name) that defines it, used to report duplicates.
    pub unit: String,
}

/// 类型表和变量表。没有 ELF 时只有内置类型，只能用原始地址。
///
/// The type table and the variable table. Without an ELF only the built-in types exist and
/// only raw addresses can be used.
#[derive(Debug, Clone)]
pub struct SymbolTable {
    /// 所有类型，下标 0 是 `void`。
    ///
    /// All types; index 0 is `void`.
    pub types: Vec<Type>,
    /// 变量名 -> 定义（同名的 static 变量可能有多个）。
    ///
    /// Variable name -> definitions (static variables of the same name may have several).
    pub vars: HashMap<String, Vec<Variable>>,
    /// typedef 和基本类型的名字 -> 类型。
    ///
    /// Names of typedefs and base types -> type.
    pub type_names: HashMap<String, TypeId>,
    /// struct/union/enum 的标签名 -> 类型（只收完整定义）。
    ///
    /// Tag names of struct/union/enum -> type (complete definitions only).
    pub tags: HashMap<String, TypeId>,
    /// ELF 符号表里的数据符号：名字 -> (地址, 大小)。没有 DWARF 信息时用它。
    ///
    /// Data symbols of the ELF symbol table: name -> (address, size). Used when there is no
    /// DWARF information.
    pub elf_symbols: HashMap<String, (u64, u64)>,
    /// 目标的指针宽度，字节；默认 4。
    ///
    /// Pointer width of the target in bytes; 4 by default.
    pub pointer_size: u64,
    builtins: HashMap<&'static str, TypeId>,
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

/// 内置的类型名：原始地址 `0x20000000:u32` 和类型转换里可以用。
const BUILTINS: &[(&str, u64, Encoding)] = &[
    ("u8", 1, Encoding::Unsigned),
    ("u16", 2, Encoding::Unsigned),
    ("u32", 4, Encoding::Unsigned),
    ("u64", 8, Encoding::Unsigned),
    ("i8", 1, Encoding::Signed),
    ("i16", 2, Encoding::Signed),
    ("i32", 4, Encoding::Signed),
    ("i64", 8, Encoding::Signed),
    ("f32", 4, Encoding::Float),
    ("f64", 8, Encoding::Float),
    ("bool", 1, Encoding::Bool),
    ("uint8_t", 1, Encoding::Unsigned),
    ("uint16_t", 2, Encoding::Unsigned),
    ("uint32_t", 4, Encoding::Unsigned),
    ("uint64_t", 8, Encoding::Unsigned),
    ("int8_t", 1, Encoding::Signed),
    ("int16_t", 2, Encoding::Signed),
    ("int32_t", 4, Encoding::Signed),
    ("int64_t", 8, Encoding::Signed),
    ("float", 4, Encoding::Float),
    ("double", 8, Encoding::Float),
    ("char", 1, Encoding::UnsignedChar),
    ("int", 4, Encoding::Signed),
    ("unsigned", 4, Encoding::Unsigned),
    ("unsigned int", 4, Encoding::Unsigned),
    ("short", 2, Encoding::Signed),
    ("unsigned short", 2, Encoding::Unsigned),
    ("long", 4, Encoding::Signed),
    ("unsigned long", 4, Encoding::Unsigned),
    ("long long", 8, Encoding::Signed),
    ("unsigned long long", 8, Encoding::Unsigned),
];

impl SymbolTable {
    /// 只含内置类型的符号表。
    ///
    /// A symbol table that holds only the built-in types.
    pub fn new() -> Self {
        let mut t = Self {
            types: vec![Type::Void],
            vars: HashMap::new(),
            type_names: HashMap::new(),
            tags: HashMap::new(),
            elf_symbols: HashMap::new(),
            pointer_size: 4,
            builtins: HashMap::new(),
        };
        for &(name, size, enc) in BUILTINS {
            let id = t.add(Type::Base {
                name: name.to_string(),
                size,
                enc,
            });
            t.builtins.insert(name, id);
        }
        t
    }

    /// `void` 的类型编号。
    ///
    /// The type id of `void`.
    pub(crate) const VOID: TypeId = 0;

    /// 添加一个类型，返回它的编号。
    ///
    /// Add a type and return its id.
    pub fn add(&mut self, ty: Type) -> TypeId {
        self.types.push(ty);
        self.types.len() - 1
    }

    /// 按编号取类型。
    ///
    /// Get a type by id.
    pub fn get(&self, id: TypeId) -> &Type {
        &self.types[id]
    }

    /// 按名字取内置类型。
    ///
    /// Get a built-in type by name.
    pub(crate) fn builtin(&self, name: &str) -> Option<TypeId> {
        self.builtins.get(name).copied()
    }

    /// 指向 target 的指针类型（按需新建）。
    ///
    /// The pointer type to `target` (created when missing).
    pub(crate) fn pointer_to(&mut self, target: TypeId) -> TypeId {
        let size = self.pointer_size;
        if let Some(i) = self.types.iter().position(
            |t| matches!(t, Type::Pointer { target: Some(x), size: s } if *x == target && *s == size),
        ) {
            return i;
        }
        self.add(Type::Pointer {
            target: Some(target),
            size,
        })
    }

    /// 去掉 typedef 和 const/volatile，得到实际的类型。
    ///
    /// Strip typedefs and const/volatile to get the underlying type.
    pub(crate) fn strip(&self, mut id: TypeId) -> TypeId {
        for _ in 0..64 {
            match &self.types[id] {
                Type::Typedef { target, .. } | Type::Qualified { target } => match target {
                    Some(t) => id = *t,
                    None => return Self::VOID,
                },
                _ => return id,
            }
        }
        id
    }

    /// 只有声明的结构体换成同名的完整定义（如果别的编译单元里有）。
    ///
    /// Replace a declaration-only struct with the complete definition of the same name, if
    /// another compilation unit has one.
    pub(crate) fn complete(&self, id: TypeId) -> TypeId {
        let id = self.strip(id);
        if let Type::Struct {
            declaration: true,
            name: Some(name),
            ..
        } = &self.types[id]
        {
            if let Some(&full) = self.tags.get(name) {
                return full;
            }
        }
        id
    }

    /// 类型的字节数；未知时为 None。
    ///
    /// Size of a type in bytes; None when unknown.
    pub fn size_of(&self, id: TypeId) -> Option<u64> {
        match &self.types[self.complete(id)] {
            Type::Base { size, .. } | Type::Pointer { size, .. } | Type::Enum { size, .. } => {
                Some(*size)
            }
            Type::Struct { size, .. } => *size,
            Type::Array { elem, dims } => {
                let mut n = self.size_of(*elem)?;
                for d in dims {
                    n *= (*d)?;
                }
                Some(n)
            }
            _ => None,
        }
    }

    /// 类型的 C 写法，用于提示。
    ///
    /// The C spelling of a type, for messages.
    pub fn name_of(&self, id: TypeId) -> String {
        match &self.types[id] {
            Type::Void => "void".into(),
            Type::Base { name, .. } => name.clone(),
            Type::Pointer { target, .. } => match target {
                Some(t) => format!("{} *", self.name_of(*t)),
                None => "void *".into(),
            },
            Type::Struct { kind, name, .. } => {
                let k = match kind {
                    Aggregate::Struct => "struct",
                    Aggregate::Union => "union",
                    Aggregate::Class => "class",
                };
                match name {
                    Some(n) => format!("{k} {n}"),
                    None => format!("{k} <匿名>"),
                }
            }
            Type::Array { elem, dims } => {
                let mut s = self.name_of(*elem);
                for d in dims {
                    match d {
                        Some(n) => s.push_str(&format!("[{n}]")),
                        None => s.push_str("[]"),
                    }
                }
                s
            }
            Type::Typedef { name, .. } => name.clone(),
            Type::Qualified { target } => match target {
                Some(t) => self.name_of(*t),
                None => "void".into(),
            },
            Type::Enum { name, .. } => match name {
                Some(n) => format!("enum {n}"),
                None => "enum <匿名>".into(),
            },
            Type::Function => "函数".into(),
            Type::Unknown(s) => s.clone(),
        }
    }

    /// 标量类型（基本类型、指针、枚举）的取值方式；不是标量时返回 None。
    ///
    /// How a scalar type (base type, pointer, enum) is interpreted; None for a non-scalar.
    pub(crate) fn value_type(&self, id: TypeId) -> Option<ValueType> {
        let stripped = self.strip(id);
        let name = self.name_of(id);
        match &self.types[stripped] {
            Type::Base { size, enc, .. } => Some(ValueType {
                kind: match enc {
                    Encoding::Unsigned | Encoding::UnsignedChar => ScalarKind::Unsigned,
                    Encoding::Signed | Encoding::SignedChar => ScalarKind::Signed,
                    Encoding::Float => ScalarKind::Float,
                    Encoding::Bool => ScalarKind::Bool,
                },
                size: *size as u8,
                name,
            }),
            Type::Pointer { size, .. } => Some(ValueType {
                kind: ScalarKind::Pointer,
                size: *size as u8,
                name,
            }),
            Type::Enum {
                size,
                signed,
                values,
                ..
            } => Some(ValueType {
                kind: ScalarKind::Enum {
                    signed: *signed,
                    values: values.clone(),
                },
                size: *size as u8,
                name,
            }),
            _ => None,
        }
    }

    /// 按名字找类型：先找 DWARF 里的 typedef 和基本类型，再找内置类型。
    ///
    /// Find a type by name: typedefs and base types from DWARF first, then the built-in types.
    pub(crate) fn lookup_type(&self, name: &str) -> Option<TypeId> {
        if let Some(rest) = name
            .strip_prefix("struct ")
            .or_else(|| name.strip_prefix("union "))
            .or_else(|| name.strip_prefix("enum "))
            .or_else(|| name.strip_prefix("class "))
        {
            return self.tags.get(rest.trim()).copied();
        }
        self.type_names
            .get(name)
            .copied()
            .or_else(|| self.builtin(name))
    }

    /// 找一个变量。重名时报错并列出各个定义。
    ///
    /// Find a variable. A duplicate name is an error that lists the definitions.
    pub(crate) fn lookup_var(&self, name: &str) -> Result<Option<&Variable>> {
        let Some(list) = self.vars.get(name) else {
            return Ok(None);
        };
        let mut uniq: Vec<&Variable> = Vec::new();
        for v in list {
            if !uniq.iter().any(|u| u.addr == v.addr) {
                uniq.push(v);
            }
        }
        if uniq.len() > 1 {
            let places = uniq
                .iter()
                .map(|v| format!("{}（0x{:08X}）", v.unit, v.addr))
                .collect::<Vec<_>>()
                .join("、");
            return Err(Error::usage(format!(
                "{name} 在多个编译单元里都有定义：{places}。请直接写地址，例如 0x{:08X}:u32",
                uniq[0].addr
            )));
        }
        Ok(uniq.first().copied())
    }
}

// ------------------------------------------------------------------ 数值

/// 标量的类别。
///
/// The category of a scalar.
#[derive(Debug, Clone, PartialEq)]
pub enum ScalarKind {
    /// 无符号整数。
    ///
    /// Unsigned integer.
    Unsigned,
    /// 有符号整数。
    ///
    /// Signed integer.
    Signed,
    /// 浮点数（4 或 8 字节）。
    ///
    /// Floating point (4 or 8 bytes).
    Float,
    /// 布尔。
    ///
    /// Boolean.
    Bool,
    /// 指针。
    ///
    /// Pointer.
    Pointer,
    /// 枚举。
    ///
    /// Enum.
    Enum {
        /// 底层类型是否有符号。
        ///
        /// Whether the underlying type is signed.
        signed: bool,
        /// 枚举值和名字。
        ///
        /// Enumerator values and names.
        values: Vec<(i64, String)>,
    },
}

/// 一个标量变量怎么解释：类别、字节数、类型名。
///
/// How a scalar variable is interpreted: category, size in bytes and type name.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueType {
    /// 类别。
    ///
    /// Category.
    pub kind: ScalarKind,
    /// 字节数。
    ///
    /// Size in bytes.
    pub size: u8,
    /// 类型名。
    ///
    /// Type name.
    pub name: String,
}

fn raw_u64(bytes: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    b[..bytes.len().min(8)].copy_from_slice(&bytes[..bytes.len().min(8)]);
    u64::from_le_bytes(b)
}

fn sign_extend(v: u64, size: u8) -> i64 {
    let bits = size as u32 * 8;
    if bits >= 64 {
        v as i64
    } else {
        let shift = 64 - bits;
        ((v << shift) as i64) >> shift
    }
}

/// 解析整数：十进制、0x 十六进制、0b 二进制，可以带负号和下划线。
///
/// Parse an integer: decimal, 0x hexadecimal or 0b binary, with an optional sign and
/// underscores.
pub(crate) fn parse_int(text: &str) -> Option<i128> {
    let t = text.trim().replace('_', "");
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest.to_string()),
        None => (false, t.strip_prefix('+').unwrap_or(&t).to_string()),
    };
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i128::from_str_radix(h, 16).ok()?
    } else if let Some(b) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        i128::from_str_radix(b, 2).ok()?
    } else {
        body.parse::<i128>().ok()?
    };
    Some(if neg { -v } else { v })
}

impl ValueType {
    /// `size` 字节的无符号整数类型，名字形如 `u32`。
    ///
    /// An unsigned integer type of `size` bytes, named like `u32`.
    pub fn unsigned(size: u8) -> Self {
        Self {
            kind: ScalarKind::Unsigned,
            size,
            name: format!("u{}", size as u32 * 8),
        }
    }

    /// 整数值（浮点返回 None）。
    fn int_value(&self, bytes: &[u8]) -> Option<i128> {
        let raw = raw_u64(bytes);
        match &self.kind {
            ScalarKind::Signed | ScalarKind::Enum { signed: true, .. } => {
                Some(sign_extend(raw, self.size) as i128)
            }
            ScalarKind::Float => None,
            _ => Some(raw as i128),
        }
    }

    fn float_value(&self, bytes: &[u8]) -> f64 {
        if self.size == 4 {
            f32::from_bits(le_u32(bytes, 0)) as f64
        } else {
            f64::from_bits(le_u64(bytes, 0))
        }
    }

    /// 给人看的格式：整数带十六进制，枚举带名字，指针为十六进制。
    ///
    /// Human-readable format: integers with hexadecimal, enums with the name, pointers in
    /// hexadecimal.
    pub fn format(&self, bytes: &[u8]) -> String {
        let raw = raw_u64(bytes);
        let width = self.size as usize * 2;
        match &self.kind {
            ScalarKind::Unsigned => format!("{raw} (0x{raw:0width$X})"),
            ScalarKind::Signed => {
                let v = sign_extend(raw, self.size);
                format!("{v} (0x{raw:0width$X})")
            }
            ScalarKind::Float => format_float(self.float_value(bytes), self.size),
            ScalarKind::Bool => match raw {
                0 => "false".into(),
                1 => "true".into(),
                v => format!("{v}（不是 0 或 1）"),
            },
            ScalarKind::Pointer => format!("0x{raw:0width$X}"),
            ScalarKind::Enum { values, .. } => {
                let v = self.int_value(bytes).unwrap_or(0) as i64;
                match values.iter().find(|(x, _)| *x == v) {
                    Some((_, name)) => format!("{name} ({v})"),
                    None => format!("{v}"),
                }
            }
        }
    }

    /// 画曲线用的数值：整数、枚举、布尔、指针按整数值，浮点按原值。
    ///
    /// The value for plotting: integers, enums, booleans and pointers as their integer value,
    /// floats as they are.
    pub fn to_f64(&self, bytes: &[u8]) -> f64 {
        match &self.kind {
            ScalarKind::Float => self.float_value(bytes),
            _ => self.int_value(bytes).unwrap_or(0) as f64,
        }
    }

    /// 写 CSV 用的格式：只有数值。
    ///
    /// The format for CSV: the number only.
    pub fn format_plain(&self, bytes: &[u8]) -> String {
        let raw = raw_u64(bytes);
        match &self.kind {
            ScalarKind::Float => format_float(self.float_value(bytes), self.size),
            ScalarKind::Pointer => format!("0x{raw:0w$X}", w = self.size as usize * 2),
            _ => self.int_value(bytes).unwrap_or(0).to_string(),
        }
    }

    /// 解析要写入的值，返回 size 字节的小端数据。
    ///
    /// Parse a value to write and return `size` bytes of little-endian data.
    pub fn parse(&self, text: &str) -> Result<Vec<u8>> {
        let text = text.trim();
        let size = self.size as usize;
        let bad = |why: &str| {
            Error::usage(format!(
                "值 {text} 不能写给 {} 类型的变量：{why}",
                self.name
            ))
        };
        match &self.kind {
            ScalarKind::Float => {
                let v: f64 = text.parse().map_err(|_| bad("不是数"))?;
                Ok(if size == 4 {
                    (v as f32).to_le_bytes().to_vec()
                } else {
                    v.to_le_bytes().to_vec()
                })
            }
            ScalarKind::Bool => {
                let v = match text {
                    "true" | "1" => 1u8,
                    "false" | "0" => 0u8,
                    _ => return Err(bad("要写 true、false、1 或 0")),
                };
                let mut out = vec![0u8; size];
                out[0] = v;
                Ok(out)
            }
            kind => {
                let v = match kind {
                    ScalarKind::Enum { values, .. } => match values.iter().find(|(_, n)| n == text)
                    {
                        Some((v, _)) => *v as i128,
                        None => parse_int(text).ok_or_else(|| bad("不是整数，也不是枚举名"))?,
                    },
                    _ => parse_int(text).ok_or_else(|| bad("不是整数"))?,
                };
                let bits = size as u32 * 8;
                let umax: i128 = if bits >= 64 {
                    u64::MAX as i128
                } else {
                    (1i128 << bits) - 1
                };
                let smin: i128 = -(1i128 << (bits - 1));
                let signed = matches!(
                    kind,
                    ScalarKind::Signed | ScalarKind::Enum { signed: true, .. }
                );
                // 有符号变量：十进制按有符号范围检查，十六进制、二进制可以直接写原始位（0xFF 写给 int8_t 为 -1）
                let raw_bits = {
                    let t = text.trim_start_matches(['+', '-']).to_ascii_lowercase();
                    t.starts_with("0x") || t.starts_with("0b")
                };
                let min = if signed { smin } else { 0 };
                let max = if signed && !raw_bits { -smin - 1 } else { umax };
                if v < min || v > max {
                    return Err(bad(&format!("超出 {size} 字节的范围")));
                }
                Ok((v as u64).to_le_bytes()[..size].to_vec())
            }
        }
    }
}

fn format_float(v: f64, size: u8) -> String {
    if size == 4 {
        format!("{}", v as f32)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse_values() {
        let t = SymbolTable::new();
        let vt = |n| t.value_type(t.builtin(n).unwrap()).unwrap();
        assert_eq!(vt("u8").format(&[0xA5]), "165 (0xA5)");
        assert_eq!(vt("i16").format(&(-2i16).to_le_bytes()), "-2 (0xFFFE)");
        assert_eq!(vt("f32").format(&1.5f32.to_le_bytes()), "1.5");
        assert_eq!(vt("f64").format(&0.25f64.to_le_bytes()), "0.25");
        assert_eq!(vt("bool").format(&[1]), "true");
        assert_eq!(vt("i32").parse("-1").unwrap(), vec![0xFF; 4]);
        assert_eq!(vt("u16").parse("0xBEEF").unwrap(), vec![0xEF, 0xBE]);
        assert!(vt("u8").parse("256").is_err());
        assert!(vt("u8").parse("-1").is_err());
        assert!(vt("i8").parse("-129").is_err());
        assert!(vt("i8").parse("200").is_err());
        assert_eq!(vt("i8").parse("0xFF").unwrap(), vec![0xFF]);
        assert_eq!(vt("i8").parse("127").unwrap(), vec![0x7F]);
        assert_eq!(
            vt("u64").parse("0xFFFFFFFFFFFFFFFF").unwrap(),
            vec![0xFF; 8]
        );
        assert_eq!(
            vt("f32").parse("2.5").unwrap(),
            2.5f32.to_le_bytes().to_vec()
        );
        assert_eq!(vt("i64").format_plain(&(-5i64).to_le_bytes()), "-5");
    }
}
