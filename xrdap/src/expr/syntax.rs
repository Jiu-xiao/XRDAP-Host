//! 表达式的词法和语法分析，产出语法树。语法见 [`super`] 模块文档。
//!
//! Lexing and parsing of expressions into a syntax tree. The grammar is in the documentation of
//! the [`super`] module.

use std::fmt;

use crate::lang::tr;
use crate::types::SymbolTable;
use crate::{Error, Result};

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
        } else if ch == '\'' {
            // 带引号的名字：模板、匿名命名空间、lambda 里的变量，名字里有 <>、空格、括号
            let start = i + 1;
            let Some(len) = c[start..].iter().position(|&x| x == '\'') else {
                return Err(Error::usage(crate::trf!(
                    "Unmatched quote in expression {s}",
                    "表达式 {s} 里的引号没有配对"
                )));
            };
            if len == 0 {
                return Err(Error::usage(crate::trf!(
                    "Empty name '' in expression {s}",
                    "表达式 {s} 里有空的名字 ''"
                )));
            }
            out.push(Tok::Ident(c[start..start + len].iter().collect()));
            i = start + len + 1;
        } else if ch.is_ascii_digit() {
            let start = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let text: String = c[start..i].iter().collect();
            let v = crate::types::parse_int(&text)
                .filter(|v| *v >= 0 && *v <= u64::MAX as i128)
                .ok_or_else(|| {
                    Error::usage(crate::trf!("{text} is not an integer", "{text} 不是整数"))
                })?;
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
                _ => {
                    return Err(Error::usage(crate::trf!(
                        "Unknown character '{ch}' in expression {s}",
                        "表达式 {s} 里有不认识的字符 '{ch}'"
                    )))
                }
            };
            i += 1;
            out.push(tok);
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct TypeSpec {
    pub words: String,
    pub stars: usize,
}

impl fmt::Display for TypeSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.words, "*".repeat(self.stars))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Expr {
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
        Error::usage(crate::trf!(
            "Bad expression {}: {what}",
            "表达式 {} 写得不对：{what}",
            self.text
        ))
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
        if words.len() == 1 && self.syms.has_var(words[0]) {
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
            return Err(self.err(tr("a type name is expected here", "这里要写类型名")));
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
                    self.expect(Tok::RP, tr("the type cast lacks ')'", "类型转换少了 ')'"))?;
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
                self.expect(Tok::RP, tr("missing ')'", "少了 ')'"))?;
                Ok(Expr::Paren(Box::new(e)))
            }
            _ => Err(self.err(tr(
                "a variable name, an address or '(' is expected here",
                "这里要写变量名、地址或 '('",
            ))),
        }
    }

    fn postfix(&mut self) -> Result<Expr> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Some(Tok::Dot) => {
                    self.pos += 1;
                    e = Expr::Member(
                        Box::new(e),
                        self.ident(tr(
                            "a member name is expected after '.'",
                            "'.' 后面要写成员名",
                        ))?,
                    );
                }
                Some(Tok::Arrow) => {
                    self.pos += 1;
                    e = Expr::Arrow(
                        Box::new(e),
                        self.ident(tr(
                            "a member name is expected after '->'",
                            "'->' 后面要写成员名",
                        ))?,
                    );
                }
                Some(Tok::LBr) => {
                    self.pos += 1;
                    let i = match self.next() {
                        Some(Tok::Num(i)) => i,
                        _ => {
                            return Err(self
                                .err(tr("an index is a non-negative integer", "下标要写非负整数")))
                        }
                    };
                    self.expect(Tok::RBr, tr("missing ']'", "少了 ']'"))?;
                    e = Expr::Index(Box::new(e), i);
                }
                _ => return Ok(e),
            }
        }
    }
}

pub(super) fn parse(text: &str, syms: &SymbolTable) -> Result<Expr> {
    let toks = tokenize(text)?;
    if toks.is_empty() {
        return Err(Error::usage(tr("The expression is empty", "表达式是空的")));
    }
    let mut p = Parser {
        toks,
        pos: 0,
        syms,
        text,
    };
    let e = p.expr()?;
    if p.pos != p.toks.len() {
        return Err(p.err(tr("unexpected text at the end", "后面多了内容")));
    }
    Ok(e)
}
