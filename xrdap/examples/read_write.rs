//! 按变量名读写目标内存。
//!
//! ```text
//! cargo run -p xrdap --example read_write -- app.elf motor.speed
//! cargo run -p xrdap --example read_write -- app.elf motor.kp 1.5
//! XRDAP_PROBE=COM3 cargo run -p xrdap --example read_write -- app.elf motor.speed
//! ```
//!
//! 探针用环境变量 `XRDAP_PROBE` 指定（写法同 `xrdap -p`），不设时自动选唯一的那块。

use std::path::PathBuf;

use xrdap::addr::Spec;
use xrdap::expr::Resolver;
use xrdap::open::{open, OpenOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(elf), Some(expr)) = (args.next(), args.next()) else {
        eprintln!("用法：read_write <ELF> <变量> [<新值>]");
        std::process::exit(2);
    };
    let value = args.next();

    // 1. 从 ELF 的 DWARF 信息里找变量的地址和类型
    let mut syms = xrdap::elf::load(&PathBuf::from(elf))?;
    let target = Resolver::new(&mut syms).resolve(&expr)?;

    // 2. 打开探针并连接 SWD 目标
    let spec = Spec::parse(&std::env::var("XRDAP_PROBE").unwrap_or_default());
    let mut probe = open(&spec, &OpenOptions::default())?;
    xrdap::watch::connect(&mut probe, 4_000_000)?;

    // 3. 写（如果给了新值），再读
    if let Some(v) = value {
        target.write(&mut probe, &target.ty.parse(&v)?)?;
    }
    println!("{} = {}", target.name, target.read_text(&mut probe)?);
    Ok(())
}
