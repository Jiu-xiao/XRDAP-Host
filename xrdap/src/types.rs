//! 变量的类型模型（从 DWARF 读出，或用内置的 u8..f64），以及按类型格式化和解析数值。

use std::collections::HashMap;

use crate::{Error, Result};

pub type TypeId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Unsigned,
    Signed,
    Float,
    Bool,
    /// char、signed char、unsigned char 按整数处理，这里只区分有无符号
    UnsignedChar,
    SignedChar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aggregate {
    Struct,
    Union,
    Class,
}

#[derive(Debug, Clone)]
pub struct Member {
    /// 匿名结构体或联合体成员为 None，它的字段可以直接访问。
    pub name: Option<String>,
    pub offset: u64,
    pub ty: TypeId,
    /// 位域的位数；不是位域时为 None。
    pub bit_size: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum Type {
    Void,
    Base {
        name: String,
        size: u64,
        enc: Encoding,
    },
    Pointer {
        target: Option<TypeId>,
        size: u64,
    },
    Struct {
        kind: Aggregate,
        name: Option<String>,
        size: Option<u64>,
        members: Vec<Member>,
        /// 只有声明、没有定义（例如 `struct opaque;`）
        declaration: bool,
    },
    Array {
        elem: TypeId,
        /// 每一维的元素个数，None 表示长度未知（`int a[]`）
        dims: Vec<Option<u64>>,
    },
    Typedef {
        name: String,
        target: Option<TypeId>,
    },
    /// const、volatile、restrict、_Atomic：对访问没有影响
    Qualified {
        target: Option<TypeId>,
    },
    Enum {
        name: Option<String>,
        size: u64,
        signed: bool,
        values: Vec<(i64, String)>,
    },
    Function,
    /// DWARF 里有但这里不认识的类型
    Unknown(String),
}

/// 一个全局变量。
#[derive(Debug, Clone)]
pub struct Variable {
    pub name: String,
    pub addr: u64,
    pub ty: TypeId,
    /// 定义它的编译单元（源文件名），用来提示重名
    pub unit: String,
}

/// 类型表和变量表。没有 ELF 时只有内置类型，只能用原始地址。
#[derive(Debug, Clone)]
pub struct SymbolTable {
    pub types: Vec<Type>,
    /// 变量名 -> 定义（同名的 static 变量可能有多个）
    pub vars: HashMap<String, Vec<Variable>>,
    /// typedef 和基本类型的名字 -> 类型
    pub type_names: HashMap<String, TypeId>,
    /// struct/union/enum 的标签名 -> 类型（只收完整定义）
    pub tags: HashMap<String, TypeId>,
    /// ELF 符号表里的数据符号：名字 -> (地址, 大小)。没有 DWARF 信息时用它。
    pub elf_symbols: HashMap<String, (u64, u64)>,
    /// 目标的指针宽度
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

    pub const VOID: TypeId = 0;

    pub fn add(&mut self, ty: Type) -> TypeId {
        self.types.push(ty);
        self.types.len() - 1
    }

    pub fn get(&self, id: TypeId) -> &Type {
        &self.types[id]
    }

    pub fn builtin(&self, name: &str) -> Option<TypeId> {
        self.builtins.get(name).copied()
    }

    /// 指向 target 的指针类型（按需新建）。
    pub fn pointer_to(&mut self, target: TypeId) -> TypeId {
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
    pub fn strip(&self, mut id: TypeId) -> TypeId {
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
    pub fn complete(&self, id: TypeId) -> TypeId {
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
    pub fn value_type(&self, id: TypeId) -> Option<ValueType> {
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
    pub fn lookup_type(&self, name: &str) -> Option<TypeId> {
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
    pub fn lookup_var(&self, name: &str) -> Result<Option<&Variable>> {
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

#[derive(Debug, Clone, PartialEq)]
pub enum ScalarKind {
    Unsigned,
    Signed,
    Float,
    Bool,
    Pointer,
    Enum { signed: bool, values: Vec<(i64, String)> },
}

/// 一个标量变量怎么解释：类别、字节数、类型名。
#[derive(Debug, Clone, PartialEq)]
pub struct ValueType {
    pub kind: ScalarKind,
    pub size: u8,
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
pub fn parse_int(text: &str) -> Option<i128> {
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
            f32::from_le_bytes(bytes[..4].try_into().unwrap()) as f64
        } else {
            f64::from_le_bytes(bytes[..8].try_into().unwrap())
        }
    }

    /// 给人看的格式：整数带十六进制，枚举带名字，指针为十六进制。
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
                let v = self.int_value(bytes).unwrap() as i64;
                match values.iter().find(|(x, _)| *x == v) {
                    Some((_, name)) => format!("{name} ({v})"),
                    None => format!("{v}"),
                }
            }
        }
    }

    /// 画曲线用的数值：整数、枚举、布尔、指针按整数值，浮点按原值。
    pub fn to_f64(&self, bytes: &[u8]) -> f64 {
        match &self.kind {
            ScalarKind::Float => self.float_value(bytes),
            _ => self.int_value(bytes).unwrap_or(0) as f64,
        }
    }

    /// 写 CSV 用的格式：只有数值。
    pub fn format_plain(&self, bytes: &[u8]) -> String {
        let raw = raw_u64(bytes);
        match &self.kind {
            ScalarKind::Float => format_float(self.float_value(bytes), self.size),
            ScalarKind::Pointer => format!("0x{raw:0w$X}", w = self.size as usize * 2),
            _ => self.int_value(bytes).unwrap().to_string(),
        }
    }

    /// 解析要写入的值，返回 size 字节的小端数据。
    pub fn parse(&self, text: &str) -> Result<Vec<u8>> {
        let text = text.trim();
        let size = self.size as usize;
        let bad = |why: &str| Error::usage(format!("值 {text} 不能写给 {} 类型的变量：{why}", self.name));
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
                    ScalarKind::Enum { values, .. } => match values.iter().find(|(_, n)| n == text) {
                        Some((v, _)) => *v as i128,
                        None => parse_int(text).ok_or_else(|| bad("不是整数，也不是枚举名"))?,
                    },
                    _ => parse_int(text).ok_or_else(|| bad("不是整数"))?,
                };
                let bits = size as u32 * 8;
                let umax: i128 = if bits >= 64 { u64::MAX as i128 } else { (1i128 << bits) - 1 };
                let smin: i128 = -(1i128 << (bits - 1));
                let signed = matches!(kind, ScalarKind::Signed | ScalarKind::Enum { signed: true, .. });
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
        assert_eq!(vt("u64").parse("0xFFFFFFFFFFFFFFFF").unwrap(), vec![0xFF; 8]);
        assert_eq!(vt("f32").parse("2.5").unwrap(), 2.5f32.to_le_bytes().to_vec());
        assert_eq!(vt("i64").format_plain(&(-5i64).to_le_bytes()), "-5");
    }
}
