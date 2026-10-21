//! 在类型表里给语法树求值，得到 [`Target`]。

use crate::types::{Member, SymbolTable, Type, TypeId};
use crate::vendor::MAX_DEPTH;
use crate::{Error, Result};

use super::syntax::{parse, Expr, TypeSpec};
use super::target::Target;

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
                "找不到变量 {name}：没有加载 ELF 符号表；也可以直接写地址，例如 0x20000000:u32"
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
