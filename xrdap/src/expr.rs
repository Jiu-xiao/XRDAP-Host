//! 变量表达式：把 `counter`、`state.speed`、`motor[1].pid->out`、`*ptr`、`p->q->x`、
//! `0x20000010:u32`、`(uint32_t***)chain_p1` 这样的写法解析成厂商条目
//! （base、各级解引用偏移、最后偏移、访问宽度）。
//!
//! 语法：
//!
//! ```text
//! expr    := '*' expr | '(' 类型 ')' expr | postfix
//! postfix := primary { '.' 名字 | '->' 名字 | '[' 整数 ']' }
//! primary := 名字 | 整数 ':' 类型 | '(' expr ')'
//! 类型    := [struct|union|enum] 名字 {名字} {'*'}      例如 u32、float、struct motor *、unsigned int
//! ```

use std::fmt;

use crate::dap::Probe;
use crate::types::{Member, SymbolTable, Type, TypeId, ValueType};
use crate::vendor::{self, Item, MAX_DEPTH};
use crate::{Error, Result};

/// 解析好的一个变量：可以直接变成厂商条目。
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// 表达式的写法，输出时用作名字
    pub name: String,
    pub ap: u8,
    pub base: u32,
    pub derefs: Vec<i16>,
    pub final_off: i16,
    pub ty: ValueType,
    /// 各级指针的表达式，第 k 个对应第 k 级解引用，用于错误提示
    pub chain: Vec<String>,
}

impl Target {
    pub fn item(&self, id: u16) -> Item {
        Item {
            ap: self.ap,
            id,
            size: self.ty.size,
            base: self.base,
            derefs: self.derefs.clone(),
            final_off: self.final_off,
        }
    }

    /// 不经过指针时的地址。
    pub fn direct_addr(&self) -> Option<u32> {
        self.derefs.is_empty().then_some(self.base)
    }

    /// 位置的简短说明：`0x20000010`；经过指针时 `[x]` 表示读 x 处的指针，
    /// 例如 `[[0x20000034]+4]+8`。
    pub fn location(&self) -> String {
        let off = |d: i64| match d {
            0 => String::new(),
            d if d > 0 => format!("+{d}"),
            d => format!("{d}"),
        };
        let mut s = format!("0x{:08X}", self.base);
        for &d in &self.derefs {
            s = format!("[{s}{}]", off(d as i64));
        }
        s + &off(self.final_off as i64)
    }

    /// 读这个变量的当前值（`ty.size` 字节，小端）。探针要先连上目标（[`Probe::connect_swd`]）。
    pub fn read(&self, probe: &mut Probe) -> Result<Vec<u8>> {
        let r = probe.read(&[self.item(0)])?;
        match r.into_iter().next() {
            Some(v) if v.status == vendor::STATUS_OK => Ok(v.value),
            Some(v) => Err(self.item_error(v.status)),
            None => Err(Error::protocol("READ 没有返回结果")),
        }
    }

    /// 读出并按类型格式化，例如 `1.5`、`42 (0x2A)`、`RUN (1)`。
    pub fn read_text(&self, probe: &mut Probe) -> Result<String> {
        Ok(self.ty.format(&self.read(probe)?))
    }

    /// 写入 `ty.size` 字节的小端值（可以用 `ty.parse("1.5")` 从文字得到）。
    pub fn write(&self, probe: &mut Probe, value: &[u8]) -> Result<()> {
        let r = probe.write(&[(self.item(0), value.to_vec())])?;
        match r.into_iter().next() {
            Some(v) if v.status == vendor::STATUS_OK => Ok(()),
            Some(v) => Err(self.item_error(v.status)),
            None => Err(Error::protocol("WRITE 没有返回结果")),
        }
    }

    fn item_error(&self, status: u8) -> Error {
        Error::Io(format!(
            "{}：{}",
            self.name,
            vendor::item_status_text(status, &self.chain)
        ))
    }
}

// ------------------------------------------------------------------ 词法

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(u64),
    Star,
    Dot,
    Arrow,
    LBr,
    RBr,
    LP,
    RP,
    Colon,
}

fn tokenize(s: &str) -> Result<Vec<Tok>> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            while i < c.len() {
                if c[i].is_ascii_alphanumeric() || c[i] == '_' {
                    i += 1;
                } else if c[i] == ':' && i + 1 < c.len() && c[i + 1] == ':' {
                    i += 2;
                } else {
                    break;
                }
            }
            out.push(Tok::Ident(c[start..i].iter().collect()));
        } else if ch.is_ascii_digit() {
            let start = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let text: String = c[start..i].iter().collect();
            let v = crate::types::parse_int(&text)
                .filter(|v| *v >= 0 && *v <= u64::MAX as i128)
                .ok_or_else(|| Error::usage(format!("{text} 不是整数")))?;
            out.push(Tok::Num(v as u64));
        } else {
            let tok = match ch {
                '*' => Tok::Star,
                '.' => Tok::Dot,
                '[' => Tok::LBr,
                ']' => Tok::RBr,
                '(' => Tok::LP,
                ')' => Tok::RP,
                ':' => Tok::Colon,
                '-' if c.get(i + 1) == Some(&'>') => {
                    i += 1;
                    Tok::Arrow
                }
                _ => return Err(Error::usage(format!("表达式 {s} 里有不认识的字符 '{ch}'"))),
            };
            i += 1;
            out.push(tok);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ 语法

#[derive(Debug, Clone, PartialEq)]
struct TypeSpec {
    words: String,
    stars: usize,
}

impl fmt::Display for TypeSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.words, "*".repeat(self.stars))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Ident(String),
    Num(u64),
    Raw(u64, TypeSpec),
    Deref(Box<Expr>),
    Member(Box<Expr>, String),
    Arrow(Box<Expr>, String),
    Index(Box<Expr>, u64),
    Cast(TypeSpec, Box<Expr>),
    Paren(Box<Expr>),
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Ident(n) => f.write_str(n),
            Expr::Num(v) => write!(f, "0x{v:X}"),
            Expr::Raw(v, t) => write!(f, "0x{v:08X}:{t}"),
            Expr::Deref(e) => write!(f, "*{e}"),
            Expr::Member(e, m) => write!(f, "{e}.{m}"),
            Expr::Arrow(e, m) => write!(f, "{e}->{m}"),
            Expr::Index(e, i) => write!(f, "{e}[{i}]"),
            Expr::Cast(t, e) => write!(f, "({t}){e}"),
            Expr::Paren(e) => write!(f, "({e})"),
        }
    }
}

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    syms: &'a SymbolTable,
    text: &'a str,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn err(&self, what: &str) -> Error {
        Error::usage(format!("表达式 {} 写得不对：{what}", self.text))
    }

    fn expect(&mut self, tok: Tok, what: &str) -> Result<()> {
        if self.next() == Some(tok) {
            Ok(())
        } else {
            Err(self.err(what))
        }
    }

    fn ident(&mut self, what: &str) -> Result<String> {
        match self.next() {
            Some(Tok::Ident(n)) => Ok(n),
            _ => Err(self.err(what)),
        }
    }

    /// 从当前位置开始的若干个名字是不是一个类型名（用来区分 `(类型)expr` 和 `(expr)`）。
    fn looks_like_type(&self) -> bool {
        let mut words = Vec::new();
        let mut n = 0;
        while let Some(Tok::Ident(w)) = self.peek_at(n) {
            words.push(w.as_str());
            n += 1;
        }
        if words.is_empty() {
            return false;
        }
        if matches!(words[0], "struct" | "union" | "enum" | "class") {
            return true;
        }
        if words.len() == 1 && self.syms.vars.contains_key(words[0]) {
            return false;
        }
        self.syms.lookup_type(&words.join(" ")).is_some()
    }

    fn type_spec(&mut self) -> Result<TypeSpec> {
        let mut words = Vec::new();
        while let Some(Tok::Ident(w)) = self.peek() {
            words.push(w.clone());
            self.pos += 1;
        }
        if words.is_empty() {
            return Err(self.err("这里要写类型名"));
        }
        let mut stars = 0;
        while self.peek() == Some(&Tok::Star) {
            stars += 1;
            self.pos += 1;
        }
        Ok(TypeSpec {
            words: words.join(" "),
            stars,
        })
    }

    fn expr(&mut self) -> Result<Expr> {
        match self.peek() {
            Some(Tok::Star) => {
                self.pos += 1;
                Ok(Expr::Deref(Box::new(self.expr()?)))
            }
            Some(Tok::LP) => {
                self.pos += 1;
                if self.looks_like_type() {
                    let t = self.type_spec()?;
                    self.expect(Tok::RP, "类型转换少了 ')'")?;
                    Ok(Expr::Cast(t, Box::new(self.expr()?)))
                } else {
                    self.pos -= 1;
                    self.postfix()
                }
            }
            _ => self.postfix(),
        }
    }

    fn primary(&mut self) -> Result<Expr> {
        match self.next() {
            Some(Tok::Ident(n)) => Ok(Expr::Ident(n)),
            Some(Tok::Num(v)) => {
                if self.peek() == Some(&Tok::Colon) {
                    self.pos += 1;
                    Ok(Expr::Raw(v, self.type_spec()?))
                } else {
                    Ok(Expr::Num(v))
                }
            }
            Some(Tok::LP) => {
                let e = self.expr()?;
                self.expect(Tok::RP, "少了 ')'")?;
                Ok(Expr::Paren(Box::new(e)))
            }
            _ => Err(self.err("这里要写变量名、地址或 '('")),
        }
    }

    fn postfix(&mut self) -> Result<Expr> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Some(Tok::Dot) => {
                    self.pos += 1;
                    e = Expr::Member(Box::new(e), self.ident("'.' 后面要写成员名")?);
                }
                Some(Tok::Arrow) => {
                    self.pos += 1;
                    e = Expr::Arrow(Box::new(e), self.ident("'->' 后面要写成员名")?);
                }
                Some(Tok::LBr) => {
                    self.pos += 1;
                    let i = match self.next() {
                        Some(Tok::Num(i)) => i,
                        _ => return Err(self.err("下标要写非负整数")),
                    };
                    self.expect(Tok::RBr, "少了 ']'")?;
                    e = Expr::Index(Box::new(e), i);
                }
                _ => return Ok(e),
            }
        }
    }
}

fn parse(text: &str, syms: &SymbolTable) -> Result<Expr> {
    let toks = tokenize(text)?;
    if toks.is_empty() {
        return Err(Error::usage("表达式是空的"));
    }
    let mut p = Parser {
        toks,
        pos: 0,
        syms,
        text,
    };
    let e = p.expr()?;
    if p.pos != p.toks.len() {
        return Err(p.err("后面多了内容"));
    }
    Ok(e)
}

// ------------------------------------------------------------------ 求值

#[derive(Debug, Clone)]
struct Loc {
    base: u64,
    /// 第 i 次读指针前加的偏移
    derefs: Vec<i64>,
    fin: i64,
    chain: Vec<String>,
}

#[derive(Debug, Clone)]
enum Val {
    /// 目标内存里的一个对象
    L { loc: Loc, ty: TypeId },
    /// 常数（只能用在类型转换里当指针）
    C { value: u64, ty: TypeId },
}

/// 表达式求值器。会往类型表里加指针类型和数组的子类型，所以要可变引用。
pub struct Resolver<'a> {
    pub syms: &'a mut SymbolTable,
    /// 求值过程中的提示（例如没有 DWARF 信息、位域被跳过）
    pub notes: Vec<String>,
    /// 结构体、数组展开成多个变量时最多展开多少个
    pub expand_limit: usize,
}

impl<'a> Resolver<'a> {
    pub fn new(syms: &'a mut SymbolTable) -> Self {
        Self {
            syms,
            notes: Vec::new(),
            expand_limit: 256,
        }
    }

    /// 解析一个表达式，结果必须是标量。
    pub fn resolve(&mut self, text: &str) -> Result<Target> {
        let mut out = self.resolve_inner(text, false)?;
        Ok(out.remove(0))
    }

    /// 解析一个表达式；结果是结构体或数组时展开成各个标量成员（位域跳过）。
    pub fn resolve_all(&mut self, text: &str) -> Result<Vec<Target>> {
        self.resolve_inner(text, true)
    }

    fn resolve_inner(&mut self, text: &str, expand: bool) -> Result<Vec<Target>> {
        let e = parse(text, self.syms)?;
        let v = self.eval(&e)?;
        let (loc, ty) = match v {
            Val::L { loc, ty } => (loc, ty),
            Val::C { .. } => {
                return Err(Error::usage(format!(
                    "{text} 是一个常数，不是变量。直接读地址要写成 {text}:u32 这样的形式"
                )))
            }
        };
        let name = text.trim().to_string();
        let mut out = Vec::new();
        if expand {
            self.expand(&name, &loc, ty, &mut out)?;
        } else {
            out.push(self.leaf(&name, &loc, ty)?);
        }
        Ok(out)
    }

    fn leaf(&mut self, name: &str, loc: &Loc, ty: TypeId) -> Result<Target> {
        let Some(vt) = self.syms.value_type(ty) else {
            let full = self.syms.complete(ty);
            let tname = self.syms.name_of(ty);
            let hint = match self.syms.get(full) {
                Type::Struct { members, .. } => {
                    let names: Vec<String> = members
                        .iter()
                        .filter_map(|m| m.name.clone())
                        .take(8)
                        .collect();
                    format!("，可以写 {name}.成员（成员有 {}）", names.join("、"))
                }
                Type::Array { .. } => format!("，可以写 {name}[下标]"),
                _ => String::new(),
            };
            return Err(Error::usage(format!(
                "{name} 的类型是 {tname}，不是能直接读写的数值{hint}"
            )));
        };
        if !matches!(vt.size, 1 | 2 | 4 | 8) {
            return Err(Error::usage(format!(
                "{name} 的类型 {} 有 {} 字节，探针只能按 1/2/4/8 字节访问",
                vt.name, vt.size
            )));
        }
        let (base, derefs, final_off) = self.encode_loc(name, loc)?;
        Ok(Target {
            name: name.to_string(),
            ap: 0,
            base,
            derefs,
            final_off,
            ty: vt,
            chain: loc.chain.clone(),
        })
    }

    fn expand(&mut self, name: &str, loc: &Loc, ty: TypeId, out: &mut Vec<Target>) -> Result<()> {
        if out.len() > self.expand_limit {
            return Err(Error::usage(format!(
                "展开后超过 {} 个变量，请写具体的成员",
                self.expand_limit
            )));
        }
        let full = self.syms.complete(ty);
        match self.syms.get(full).clone() {
            Type::Struct {
                members,
                declaration: false,
                ..
            } => {
                for m in members {
                    let mut l = loc.clone();
                    l.fin += m.offset as i64;
                    match (&m.name, m.bit_size) {
                        (Some(n), Some(bits)) => {
                            self.notes.push(format!("跳过位域 {name}.{n}（{bits} 位）"))
                        }
                        (None, _) => self.expand(name, &l, m.ty, out)?,
                        (Some(n), None) => self.expand(&format!("{name}.{n}"), &l, m.ty, out)?,
                    }
                }
                Ok(())
            }
            Type::Array { .. } => {
                let (elem, count, stride) = self.array_step(full)?;
                let Some(count) = count else {
                    return Err(Error::usage(format!("{name} 是长度未知的数组，请写下标")));
                };
                for i in 0..count {
                    let mut l = loc.clone();
                    l.fin += (i * stride) as i64;
                    self.expand(&format!("{name}[{i}]"), &l, elem, out)?;
                }
                Ok(())
            }
            _ => {
                out.push(self.leaf(name, loc, ty)?);
                Ok(())
            }
        }
    }

    /// 把求出的位置变成条目字段：第一级偏移并进 base，其余偏移必须放得进 i16。
    fn encode_loc(&self, name: &str, loc: &Loc) -> Result<(u32, Vec<i16>, i16)> {
        if loc.derefs.len() > MAX_DEPTH {
            return Err(Error::usage(format!(
                "{name} 要经过 {} 级指针，探针最多支持 {MAX_DEPTH} 级",
                loc.derefs.len()
            )));
        }
        let to_u32 = |v: i128| -> Result<u32> {
            u32::try_from(v).map_err(|_| Error::usage(format!("{name} 的地址 0x{v:X} 超出 32 位")))
        };
        let to_i16 = |v: i64| -> Result<i16> {
            i16::try_from(v).map_err(|_| {
                Error::usage(format!(
                    "{name} 在指针之后的偏移 {v} 超出 -32768..32767，探针的条目放不下"
                ))
            })
        };
        if loc.derefs.is_empty() {
            let addr = to_u32(loc.base as i128 + loc.fin as i128)?;
            return Ok((addr, Vec::new(), 0));
        }
        let base = to_u32(loc.base as i128 + loc.derefs[0] as i128)?;
        let mut derefs = vec![0i16];
        for &d in &loc.derefs[1..] {
            derefs.push(to_i16(d)?);
        }
        Ok((base, derefs, to_i16(loc.fin)?))
    }

    fn type_of(&mut self, spec: &TypeSpec) -> Result<TypeId> {
        let mut id = self.syms.lookup_type(&spec.words).ok_or_else(|| {
            Error::usage(format!(
                "不认识类型 {}（可以用 u8/u16/u32/u64/i8../f32/f64，或 ELF 里的类型名）",
                spec.words
            ))
        })?;
        for _ in 0..spec.stars {
            id = self.syms.pointer_to(id);
        }
        Ok(id)
    }

    /// 内置类型（u8 到 u64、i8 到 i64、f32、f64）
    fn builtin(&self, name: &str) -> Result<TypeId> {
        self.syms
            .builtin(name)
            .ok_or_else(|| Error::usage(format!("没有内置类型 {name}")))
    }

    fn eval(&mut self, e: &Expr) -> Result<Val> {
        match e {
            Expr::Ident(name) => self.variable(name),
            Expr::Num(v) => Ok(Val::C {
                value: *v,
                ty: self.builtin("u32")?,
            }),
            Expr::Raw(addr, spec) => Ok(Val::L {
                loc: Loc {
                    base: *addr,
                    derefs: Vec::new(),
                    fin: 0,
                    chain: Vec::new(),
                },
                ty: self.type_of(spec)?,
            }),
            Expr::Paren(inner) => self.eval(inner),
            Expr::Deref(inner) => {
                let v = self.eval(inner)?;
                self.deref(v, &inner.to_string())
            }
            Expr::Member(inner, m) => {
                let v = self.eval(inner)?;
                self.member(v, m, &inner.to_string())
            }
            Expr::Arrow(inner, m) => {
                let v = self.eval(inner)?;
                let text = inner.to_string();
                let v = self.deref(v, &text)?;
                self.member(v, m, &format!("{text}->"))
            }
            Expr::Index(inner, i) => {
                let v = self.eval(inner)?;
                self.index(v, *i, &inner.to_string())
            }
            Expr::Cast(spec, inner) => {
                let to = self.type_of(spec)?;
                let v = self.eval(inner)?;
                self.cast(v, to, &e.to_string())
            }
        }
    }

    fn variable(&mut self, name: &str) -> Result<Val> {
        if let Some(var) = self.syms.lookup_var(name)? {
            return Ok(Val::L {
                loc: Loc {
                    base: var.addr,
                    derefs: Vec::new(),
                    fin: 0,
                    chain: Vec::new(),
                },
                ty: var.ty,
            });
        }
        if let Some(&(addr, size)) = self.syms.elf_symbols.get(name) {
            if matches!(size, 1 | 2 | 4 | 8) {
                self.notes.push(format!(
                    "{name} 没有 DWARF 类型信息，按符号大小当作 u{} 处理",
                    size * 8
                ));
                let ty = self.builtin(&format!("u{}", size * 8))?;
                return Ok(Val::L {
                    loc: Loc {
                        base: addr,
                        derefs: Vec::new(),
                        fin: 0,
                        chain: Vec::new(),
                    },
                    ty,
                });
            }
            return Err(Error::usage(format!(
                "{name} 没有 DWARF 类型信息，符号大小 {size} 字节，请写成 0x{addr:08X}:u32 这样的形式"
            )));
        }
        if self.syms.vars.is_empty() && self.syms.elf_symbols.is_empty() {
            return Err(Error::usage(format!(
                "找不到变量 {name}：没有加载 ELF（用 --elf 指定），或者直接写地址，例如 0x20000000:u32"
            )));
        }
        let mut similar: Vec<&String> = self
            .syms
            .vars
            .keys()
            .filter(|k| k.contains(name) || name.contains(k.as_str()))
            .collect();
        similar.sort();
        similar.truncate(6);
        let hint = if similar.is_empty() {
            String::new()
        } else {
            format!(
                "，相近的有 {}",
                similar
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("、")
            )
        };
        Err(Error::usage(format!("ELF 里找不到变量 {name}{hint}")))
    }

    fn deref(&mut self, v: Val, text: &str) -> Result<Val> {
        let ty = match &v {
            Val::L { ty, .. } | Val::C { ty, .. } => *ty,
        };
        let stripped = self.syms.strip(ty);
        let (target, size) = match self.syms.get(stripped) {
            Type::Pointer { target, size } => (*target, *size),
            Type::Array { .. } => {
                // 数组名当指针用：*arr 就是 arr[0]
                return self.index(v, 0, text);
            }
            _ => {
                return Err(Error::usage(format!(
                    "{text} 的类型是 {}，不是指针，不能解引用",
                    self.syms.name_of(ty)
                )))
            }
        };
        let target = match target {
            Some(t) if !matches!(self.syms.get(self.syms.strip(t)), Type::Void) => t,
            _ => {
                return Err(Error::usage(format!(
                    "{text} 是 void 指针，要先转换类型，例如 *(u32*){text}"
                )))
            }
        };
        if matches!(self.syms.get(self.syms.strip(target)), Type::Function) {
            return Err(Error::usage(format!("{text} 是函数指针，不能解引用")));
        }
        match v {
            Val::C { value, .. } => Ok(Val::L {
                loc: Loc {
                    base: value,
                    derefs: Vec::new(),
                    fin: 0,
                    chain: Vec::new(),
                },
                ty: target,
            }),
            Val::L { mut loc, .. } => {
                if size != 4 {
                    return Err(Error::usage(format!(
                        "{text} 是 {size} 字节的指针，探针只支持 32 位指针"
                    )));
                }
                loc.derefs.push(loc.fin);
                loc.fin = 0;
                loc.chain.push(text.to_string());
                Ok(Val::L { loc, ty: target })
            }
        }
    }

    fn find_member(&self, members: &[Member], name: &str) -> Option<(u64, Member)> {
        for m in members {
            match &m.name {
                Some(n) if n == name => return Some((m.offset, m.clone())),
                None => {
                    let inner = self.syms.complete(m.ty);
                    if let Type::Struct { members: sub, .. } = self.syms.get(inner) {
                        if let Some((off, found)) = self.find_member(sub, name) {
                            return Some((m.offset + off, found));
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn member(&mut self, v: Val, name: &str, text: &str) -> Result<Val> {
        let Val::L { mut loc, ty } = v else {
            return Err(Error::usage(format!("常数没有成员 {name}")));
        };
        let full = self.syms.complete(ty);
        let shown = text.trim_end_matches("->");
        let sep = if text.ends_with("->") { "" } else { "." };
        match self.syms.get(full) {
            Type::Struct {
                declaration: true, ..
            } => Err(Error::usage(format!(
                "{shown} {} {} 只有声明，ELF 里没有它的定义",
                if sep.is_empty() {
                    "指向的类型"
                } else {
                    "的类型"
                },
                self.syms.name_of(ty)
            ))),
            Type::Struct { members, .. } => {
                let Some((off, m)) = self.find_member(members, name) else {
                    let names: Vec<String> =
                        members.iter().filter_map(|m| m.name.clone()).collect();
                    return Err(Error::usage(format!(
                        "{} 没有成员 {name}（成员有 {}）",
                        self.syms.name_of(ty),
                        names.join("、")
                    )));
                };
                if let Some(bits) = m.bit_size {
                    let hint = if loc.derefs.is_empty() {
                        let addr = loc.base as i64 + loc.fin + off as i64;
                        format!("，它所在的字节可以写成 0x{addr:08X}:u8")
                    } else {
                        String::new()
                    };
                    return Err(Error::usage(format!(
                        "{text}{sep}{name} 是 {bits} 位的位域。探针只能按 1/2/4/8 字节访问，不支持位域{hint}"
                    )));
                }
                loc.fin += off as i64;
                Ok(Val::L { loc, ty: m.ty })
            }
            _ => Err(Error::usage(format!(
                "{shown} 的类型是 {}，不是结构体，没有成员 {name}",
                self.syms.name_of(ty)
            ))),
        }
    }

    /// 数组类型取一个下标后的元素类型、第一维长度、步长。
    fn array_step(&mut self, arr: TypeId) -> Result<(TypeId, Option<u64>, u64)> {
        let Type::Array { elem, dims } = self.syms.get(arr).clone() else {
            unreachable!()
        };
        let sub = if dims.len() > 1 {
            self.syms.add(Type::Array {
                elem,
                dims: dims[1..].to_vec(),
            })
        } else {
            elem
        };
        let stride = self.syms.size_of(sub).ok_or_else(|| {
            Error::usage(format!("数组元素 {} 的大小未知", self.syms.name_of(sub)))
        })?;
        Ok((sub, dims[0], stride))
    }

    fn index(&mut self, v: Val, i: u64, text: &str) -> Result<Val> {
        let ty = match &v {
            Val::L { ty, .. } | Val::C { ty, .. } => *ty,
        };
        let full = self.syms.complete(ty);
        match self.syms.get(full).clone() {
            Type::Array { .. } => {
                let Val::L { mut loc, .. } = v else {
                    unreachable!()
                };
                let (elem, count, stride) = self.array_step(full)?;
                if let Some(n) = count {
                    if i >= n {
                        return Err(Error::usage(format!(
                            "{text}[{i}] 越界：{text} 只有 {n} 个元素"
                        )));
                    }
                }
                loc.fin += (i * stride) as i64;
                Ok(Val::L { loc, ty: elem })
            }
            Type::Pointer { .. } => {
                let target = self.deref(v, text)?;
                let Val::L { mut loc, ty } = target else {
                    unreachable!()
                };
                if i > 0 {
                    let stride = self.syms.size_of(ty).ok_or_else(|| {
                        Error::usage(format!("{text} 指向的类型大小未知，不能取下标"))
                    })?;
                    loc.fin += (i * stride) as i64;
                }
                Ok(Val::L { loc, ty })
            }
            _ => Err(Error::usage(format!(
                "{text} 的类型是 {}，不能取下标",
                self.syms.name_of(ty)
            ))),
        }
    }

    fn cast(&mut self, v: Val, to: TypeId, text: &str) -> Result<Val> {
        let to_scalar = self.syms.value_type(to);
        match v {
            Val::C { value, .. } => match to_scalar {
                Some(_) => Ok(Val::C { value, ty: to }),
                None => Err(Error::usage(format!("{text}：常数只能转换成指针或整数"))),
            },
            Val::L { loc, ty } => {
                let from = self.syms.value_type(ty);
                match (from, to_scalar) {
                    (Some(a), Some(b)) if a.size == b.size => Ok(Val::L { loc, ty: to }),
                    (Some(a), Some(b)) => Err(Error::usage(format!(
                        "{text}：{} 有 {} 字节，{} 有 {} 字节，只能在同样大小的类型之间转换",
                        a.name, a.size, b.name, b.size
                    ))),
                    _ => Err(Error::usage(format!("{text}：只能转换数值和指针"))),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_addresses() {
        let mut syms = SymbolTable::new();
        let mut r = Resolver::new(&mut syms);
        let t = r.resolve("0x20000010:u32").unwrap();
        assert_eq!((t.base, t.ty.size, t.derefs.len()), (0x2000_0010, 4, 0));
        let t = r.resolve("*0x20000034:u32*").unwrap();
        assert_eq!(
            (t.base, t.derefs.clone(), t.final_off),
            (0x2000_0034, vec![0], 0)
        );
        let t = r.resolve("***(u32***)0x20000034:u32").unwrap();
        assert_eq!(t.derefs, vec![0, 0, 0]);
        assert_eq!(t.chain.len(), 3);
        let t = r.resolve("*(f32*)0x20000100").unwrap();
        assert_eq!((t.base, t.derefs.len()), (0x2000_0100, 0));
        assert!(matches!(t.ty.kind, crate::types::ScalarKind::Float));
        let t = r.resolve("((u16*)0x20000000)[3]").unwrap();
        assert_eq!(t.base, 0x2000_0006);
        assert!(r.resolve("0x20000000").is_err());
        assert!(r.resolve("*0x20000000").is_err());
        assert!(r.resolve("0x20000000:foo").is_err());
        assert!(r
            .resolve("counter")
            .unwrap_err()
            .to_string()
            .contains("--elf"));
        assert!(r.resolve("0x20000000:u32 +").is_err());
    }
}
