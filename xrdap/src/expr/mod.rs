//! 变量表达式：把 `counter`、`state.speed`、`motor[1].pid->out`、`*ptr`、`p->q->x`、
//! `0x20000010:u32`、`(uint32_t***)chain_p1` 这样的写法解析成厂商条目
//! （base、各级解引用偏移、最后偏移、访问宽度）。
//!
//! 语法：
//!
//! ```text
//! expr    := '*' expr | '(' 类型 ')' expr | postfix
//! postfix := primary { '.' 名字 | '->' 名字 | '[' 整数 ']' }
//! primary := 变量名 | 整数 ':' 类型 | '(' expr ')'
//! 变量名  := 名字 {'::' 名字} | "'" 全名 "'"
//! 类型    := [struct|union|enum] 名字 {名字} {'*'}      例如 u32、float、struct motor *、unsigned int
//! ```
//!
//! 变量名可以带作用域（`motor::speed`、`tick::count`），也可以只写末尾的几段（`speed`），
//! 只对应一个变量时就用它。名字里有 `<>`、空格或括号时（模板、匿名命名空间、lambda）加单引号，
//! 例如 `'Holder<3>::v'`。
//!
//! Variable expressions: forms such as `counter`, `state.speed`, `motor[1].pid->out`, `*ptr`,
//! `p->q->x`, `0x20000010:u32` and `(uint32_t***)chain_p1` are resolved into vendor items
//! (base, offset of each dereference level, final offset, access width).
//!
//! Grammar (`type` is a type name, `name` an identifier, `integer` an integer literal):
//!
//! ```text
//! expr    := '*' expr | '(' type ')' expr | postfix
//! postfix := primary { '.' name | '->' name | '[' integer ']' }
//! primary := variable | integer ':' type | '(' expr ')'
//! variable := name {'::' name} | "'" full-name "'"
//! type    := [struct|union|enum] name {name} {'*'}      e.g. u32, float, struct motor *, unsigned int
//! ```
//!
//! A variable name can carry its scope (`motor::speed`, `tick::count`) or give only its last
//! components (`speed`), which works when they match one variable. Names containing `<>`, spaces
//! or parentheses (templates, anonymous namespaces, lambdas) are written in single quotes, e.g.
//! `'Holder<3>::v'`.

mod resolve;
mod syntax;
mod target;

pub use resolve::Resolver;
pub use target::Target;

/// 变量在表达式里的写法：名字里只有标识符和 `::` 时原样写，否则加单引号。
///
/// How a variable is written in an expression: as is when its name has only identifiers and
/// `::`, otherwise in single quotes.
pub fn name_expr(name: &str) -> String {
    let plain = name.split("::").all(|part| {
        let mut ch = part.chars();
        ch.next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && ch.all(|c| c.is_ascii_alphanumeric() || c == '_')
    });
    if plain || name.contains('\'') {
        name.to_string()
    } else {
        format!("'{name}'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SymbolTable;

    #[test]
    fn name_exprs() {
        assert_eq!(name_expr("counter"), "counter");
        assert_eq!(name_expr("motor::speed"), "motor::speed");
        assert_eq!(name_expr("Holder<3>::v"), "'Holder<3>::v'");
        assert_eq!(
            name_expr("(anonymous namespace)::x"),
            "'(anonymous namespace)::x'"
        );
        assert_eq!(name_expr("a::"), "'a::'");
    }

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
            .contains("没有加载 ELF"));
        assert!(r.resolve("0x20000000:u32 +").is_err());
    }
}
