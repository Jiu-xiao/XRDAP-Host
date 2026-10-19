//! DWARF 解析和表达式求值，对照 tests/fixtures 里的两个真实 ELF。

use std::path::PathBuf;

use xrdap::expr::{Resolver, Target};
use xrdap::types::{ScalarKind, SymbolTable};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn types_elf() -> SymbolTable {
    xrdap::elf::load(&fixture("types.elf")).unwrap()
}

fn g431_elf() -> SymbolTable {
    xrdap::elf::load(&fixture("g431-test.elf")).unwrap()
}

fn resolve(syms: &mut SymbolTable, expr: &str) -> Target {
    Resolver::new(syms)
        .resolve(expr)
        .unwrap_or_else(|e| panic!("{expr}: {e}"))
}

fn resolve_err(syms: &mut SymbolTable, expr: &str) -> String {
    match Resolver::new(syms).resolve(expr) {
        Ok(t) => panic!("{expr} 应该失败，却得到 {t:?}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn g431_variables_match_symbol_table() {
    // 地址与 target-test/symbols.txt（arm-none-eabi-nm）一致
    let mut s = g431_elf();
    for (name, addr, size) in [
        ("tick", 0x2000_0000, 4),
        ("cnt_a", 0x2000_0008, 4),
        ("cnt_b", 0x2000_000C, 4),
        ("cnt64", 0x2000_0010, 8),
        ("v_u8", 0x2000_0018, 1),
        ("v_u16", 0x2000_001A, 2),
        ("v_u32", 0x2000_001C, 4),
        ("v_u64", 0x2000_0020, 8),
        ("chain_final", 0x2000_0028, 4),
        ("chain_p3", 0x2000_002C, 4),
        ("chain_p2", 0x2000_0030, 4),
        ("chain_p1", 0x2000_0034, 4),
        ("null_p1", 0x2000_0038, 4),
        ("bad_p1", 0x2000_003C, 4),
    ] {
        let t = resolve(&mut s, name);
        assert_eq!(t.direct_addr(), Some(addr), "{name}");
        assert_eq!(t.ty.size, size, "{name}");
    }
    assert_eq!(resolve(&mut s, "v_u64").ty.kind, ScalarKind::Unsigned);
    assert_eq!(resolve(&mut s, "chain_p1").ty.kind, ScalarKind::Pointer);
    assert_eq!(
        resolve(&mut s, "rw_block[3]").direct_addr(),
        Some(0x2000_004C)
    );
}

#[test]
fn g431_pointer_chain() {
    let mut s = g431_elf();
    // chain_p1 声明为 uint32_t *，实际指向 chain_p2；三级要先转换类型
    let t = resolve(&mut s, "***(uint32_t***)chain_p1");
    assert_eq!(
        (t.base, t.derefs.clone(), t.final_off),
        (0x2000_0034, vec![0, 0, 0], 0)
    );
    assert_eq!(t.ty.size, 4);
    assert_eq!(
        t.chain,
        vec![
            "(uint32_t***)chain_p1",
            "*(uint32_t***)chain_p1",
            "**(uint32_t***)chain_p1"
        ]
    );
    let t = resolve(&mut s, "*chain_p3");
    assert_eq!((t.base, t.derefs.clone()), (0x2000_002C, vec![0]));
    let t = resolve(&mut s, "*null_p1");
    assert_eq!((t.base, t.derefs.len()), (0x2000_0038, 1));
    // 同样的链用原始地址写
    let raw = resolve(&mut s, "***0x20000034:u32***");
    assert_eq!((raw.base, raw.derefs.clone()), (0x2000_0034, vec![0, 0, 0]));
    assert!(resolve_err(&mut s, "**chain_p1").contains("不是指针"));
}

#[test]
fn struct_array_pointer_paths() {
    let mut s = types_elf();
    // pid_ctrl_t 20 字节，motor_t 32 字节，见 types.c
    let t = resolve(&mut s, "pids[1].out");
    assert_eq!(t.direct_addr(), Some(0x2000_0020));
    assert_eq!(t.ty.kind, ScalarKind::Float);

    let t = resolve(&mut s, "motor[1].pid->out");
    assert_eq!(
        (t.base, t.derefs.clone(), t.final_off),
        (0x2000_0050, vec![0], 12)
    );
    assert_eq!(t.chain, vec!["motor[1].pid"]);
    assert_eq!(t.ty.size, 4);

    let t = resolve(&mut s, "motor[0].angle");
    assert_eq!((t.direct_addr(), t.ty.size), (Some(0x2000_0038), 8));
    assert_eq!(t.ty.kind, ScalarKind::Float);
    assert_eq!(
        resolve(&mut s, "motor[0].temps[2]").direct_addr(),
        Some(0x2000_0042)
    );
    assert_eq!(
        resolve(&mut s, "motor[0].flags.raw").direct_addr(),
        Some(0x2000_0044)
    );
    assert_eq!(
        resolve(&mut s, "motor[0].speed").ty.kind,
        ScalarKind::Signed
    );

    let t = resolve(&mut s, "active->speed");
    assert_eq!(
        (t.base, t.derefs.clone(), t.final_off),
        (0x2000_00C4, vec![0], 0)
    );
    let t = resolve(&mut s, "(*active).current");
    assert_eq!(
        (t.base, t.derefs.clone(), t.final_off),
        (0x2000_00C4, vec![0], 2)
    );

    let t = resolve(&mut s, "head->next->next->value");
    assert_eq!(
        (t.base, t.derefs.clone(), t.final_off),
        (0x2000_00EC, vec![0, 4, 4], 0)
    );
    assert_eq!(t.chain, vec!["head", "head->next", "head->next->next"]);

    let t = resolve(&mut s, "*ptr");
    assert_eq!((t.base, t.derefs.clone()), (0x2000_00C0, vec![0]));
    let t = resolve(&mut s, "ptr[0]");
    assert_eq!((t.base, t.derefs.clone()), (0x2000_00C0, vec![0]));

    assert_eq!(
        resolve(&mut s, "matrix[1][2]").direct_addr(),
        Some(0x2000_00BC)
    );
    assert_eq!(
        resolve(&mut s, "state.voltage").direct_addr(),
        Some(0x2000_0070)
    );
    assert_eq!(resolve(&mut s, "state.enabled").ty.kind, ScalarKind::Bool);
    // enabled 在 +12，pair 按 2 字节对齐在 +14，hi 在 +16
    assert_eq!(
        resolve(&mut s, "state.pair.hi").direct_addr(),
        Some(0x2000_0078)
    );
    assert_eq!(resolve(&mut s, "word.f").direct_addr(), Some(0x2000_00C8));
    assert_eq!(
        resolve(&mut s, "word.bytes[3]").direct_addr(),
        Some(0x2000_00CB)
    );
    assert_eq!(resolve(&mut s, "s8").ty.kind, ScalarKind::Signed);
    assert_eq!(resolve(&mut s, "neg64").ty.size, 8);
    assert_eq!(resolve(&mut s, "ratio").ty.kind, ScalarKind::Float);
    assert_eq!(resolve(&mut s, "cv").direct_addr(), Some(0x0800_0020));
    assert_eq!(
        resolve(&mut s, "file_static").direct_addr(),
        Some(0x2000_00D0)
    );
    assert!(matches!(
        resolve(&mut s, "motor[1].mode").ty.kind,
        ScalarKind::Enum { .. }
    ));
    // 类型名可以用在转换里
    let t = resolve(&mut s, "((pid_ctrl_t*)0x20000000)->ki");
    assert_eq!(t.direct_addr(), Some(0x2000_0004));
    let t = resolve(&mut s, "((struct motor*)0x20000028)[1].current");
    assert_eq!(t.direct_addr(), Some(0x2000_004A));
}

#[test]
fn clear_errors() {
    let mut s = types_elf();
    let e = resolve_err(&mut s, "motor[0].flags.ready");
    assert!(e.contains("位域"), "{e}");
    let e = resolve_err(&mut s, "motor[2].speed");
    assert!(e.contains("越界"), "{e}");
    let e = resolve_err(&mut s, "pids[0]");
    assert!(
        e.contains("不是能直接读写的数值") && e.contains("kp"),
        "{e}"
    );
    let e = resolve_err(&mut s, "state.nope");
    assert!(e.contains("没有成员 nope"), "{e}");
    let e = resolve_err(&mut s, "*counter");
    assert!(e.contains("不是指针"), "{e}");
    let e = resolve_err(&mut s, "opaque_p->x");
    assert!(e.contains("只有声明"), "{e}");
    let e = resolve_err(&mut s, "motorr");
    assert!(e.contains("找不到变量"), "{e}");
    let e = resolve_err(&mut s, "(u8)counter");
    assert!(e.contains("同样大小"), "{e}");
}

#[test]
fn struct_expansion_skips_bitfields() {
    let mut s = types_elf();
    let mut r = Resolver::new(&mut s);
    let all = r.resolve_all("motor[0]").unwrap();
    let names: Vec<&str> = all.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "motor[0].speed",
            "motor[0].current",
            "motor[0].mode",
            "motor[0].pid",
            "motor[0].angle",
            "motor[0].temps[0]",
            "motor[0].temps[1]",
            "motor[0].temps[2]",
            "motor[0].flags.raw",
        ]
    );
    assert_eq!(r.notes.len(), 2, "{:?}", r.notes);
    let all = r.resolve_all("matrix").unwrap();
    assert_eq!(all.len(), 6);
    assert_eq!(all[5].direct_addr(), Some(0x2000_00BC));
}
