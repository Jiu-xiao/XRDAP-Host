//! 变量的作用域和名字，对照 tests/fixtures 里由 scopes.cpp 编出的 ELF（普通、LTO、压缩调试段）。

use std::path::PathBuf;

use xrdap::expr::Resolver;
use xrdap::types::SymbolTable;

fn load(name: &str) -> SymbolTable {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    xrdap::elf::load(&path).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// (名字, 地址)，按名字排序。
fn listed(s: &SymbolTable) -> Vec<(String, u64)> {
    let mut v: Vec<_> = s
        .vars
        .values()
        .flatten()
        .map(|v| (v.name.clone(), v.addr))
        .collect();
    v.sort();
    v
}

fn names(s: &SymbolTable) -> Vec<String> {
    listed(s).into_iter().map(|(n, _)| n).collect()
}

/// 表达式解析出的地址；结构体取第一个成员的地址。
fn base(s: &mut SymbolTable, expr: &str) -> u64 {
    let all = Resolver::new(s)
        .resolve_all(expr)
        .unwrap_or_else(|e| panic!("{expr}: {e}"));
    u64::from(all[0].base)
}

fn addr_of(s: &SymbolTable, name: &str) -> u64 {
    s.vars[name][0].addr
}

const ALL: &[&str] = &[
    "(anonymous namespace)::anon_ns",
    "Ctrl::count",
    "Ctrl::pid",
    "Holder<3>::v",
    "Reset_Handler::(anonymous)::operator()::lam_static",
    "g_global",
    "g_name",
    "left::dup",
    "motor::speed",
    "right::dup",
    "s_file",
    "tick::blk_static",
    "tick::f_static",
];

/// 名字里有 `<>`、空格或括号，表达式里要加引号。
const QUOTED: &[&str] = &[
    "(anonymous namespace)::anon_ns",
    "Holder<3>::v",
    "Reset_Handler::(anonymous)::operator()::lam_static",
];

fn expected(skip: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = ALL
        .iter()
        .filter(|n| !skip.contains(n))
        .map(|n| n.to_string())
        .collect();
    v.sort();
    v
}

/// 命名空间和类的静态成员带作用域；函数和块里的 static 变量也列出；编译器生成的 `__func__`、
/// 被 --gc-sections 丢掉的变量（DWARF 里地址是 0）和没有 DWARF 的变量不列出。
#[test]
fn scoped_names() {
    let s = load("scopes.elf");
    assert_eq!(names(&s), expected(&[]));
    assert!(s.vars.values().flatten().all(|v| v.addr >= 0x2000_0000));
}

/// LTO 时定义要隔两层引用才找到名字和类型；Ctrl::pid 被优化掉了。
#[test]
fn lto_names() {
    let s = load("scopes-lto.elf");
    assert_eq!(names(&s), expected(&["Ctrl::pid"]));
}

/// 压缩的调试段先解压，结果与不压缩的相同。
#[test]
fn compressed_debug_sections() {
    assert_eq!(listed(&load("scopes-gz.elf")), listed(&load("scopes.elf")));
}

/// 名字末尾的几段只对应一个变量时就用它；对应多个时报错并列出全名。
#[test]
fn short_names() {
    let mut s = load("scopes.elf");
    for (short, full) in [
        ("speed", "motor::speed"),
        ("pid", "Ctrl::pid"),
        ("count", "Ctrl::count"),
        ("f_static", "tick::f_static"),
        ("blk_static", "tick::blk_static"),
        ("anon_ns", "(anonymous namespace)::anon_ns"),
        ("v", "Holder<3>::v"),
        (
            "lam_static",
            "Reset_Handler::(anonymous)::operator()::lam_static",
        ),
        ("left::dup", "left::dup"),
        ("'Holder<3>::v'", "Holder<3>::v"),
        (
            "'(anonymous namespace)::anon_ns'",
            "(anonymous namespace)::anon_ns",
        ),
    ] {
        let want = addr_of(&s, full);
        assert_eq!(base(&mut s, short), want, "{short}");
    }
    let t = Resolver::new(&mut s).resolve("pid.ki").unwrap();
    assert_eq!(u64::from(t.base), addr_of(&s, "Ctrl::pid") + 4);

    let e = Resolver::new(&mut s)
        .resolve("dup")
        .unwrap_err()
        .to_string();
    assert!(e.contains("left::dup") && e.contains("right::dup"), "{e}");

    // 没有 DWARF 的变量按 ELF 符号表读
    let mut r = Resolver::new(&mut s);
    r.resolve("lib_nodebug").unwrap();
    assert!(r.notes.iter().any(|n| n.contains("lib_nodebug")));

    for bad in ["unused_var", "__func__", "'Holder<3>::v", "''"] {
        assert!(Resolver::new(&mut s).resolve(bad).is_err(), "{bad}");
    }
}

/// `vars` 和图形界面按 name_expr 的写法给出名字，写回表达式能找到同一个变量。
#[test]
fn listed_names_resolve() {
    let mut s = load("scopes.elf");
    for (name, addr) in listed(&s) {
        let expr = xrdap::expr::name_expr(&name);
        assert_eq!(
            expr.starts_with('\''),
            QUOTED.contains(&name.as_str()),
            "{name}"
        );
        assert_eq!(base(&mut s, &expr), addr, "{expr}");
    }
}
