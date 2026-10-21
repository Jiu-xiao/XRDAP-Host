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

mod resolve;
mod syntax;
mod target;

pub use resolve::Resolver;
pub use target::Target;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SymbolTable;

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
