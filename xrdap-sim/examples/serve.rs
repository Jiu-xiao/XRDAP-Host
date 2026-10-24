//! 在本机提供一个模拟探针的 TCP DAP 服务，不接硬件也能试图形界面和命令行。
//!
//! ```text
//! cargo run -p xrdap-sim --example serve -- app.elf counter state.count
//! xrdap read -p 127.0.0.1:3241 --elf app.elf counter
//! ```
//!
//! 按 ELF 的初值填模拟内存；后面列出的变量随时间递增（第 k 个每 (k + 1) ms 加 1），用来看曲线。
//! 监听地址用环境变量 `XRDAP_SIM_LISTEN` 改，默认 `127.0.0.1:3241`。
//!
//! Serve a simulated probe over TCP on this host, to try the GUI and the command line without
//! hardware. Memory is filled from the ELF; the listed variables count up over time.

use std::path::Path;

use xrdap::expr::Resolver;
use xrdap_sim::Sim;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let elf = args.next().ok_or("用法：serve <ELF> [递增的变量…]")?;
    let sim = Sim::new();
    sim.load_elf(&std::fs::read(&elf)?);
    let mut syms = xrdap::elf::load(Path::new(&elf))?;
    let mut resolver = Resolver::new(&mut syms);
    for (k, name) in args.enumerate() {
        let t = resolver.resolve(&name)?;
        let addr = t.direct_addr().ok_or("递增的变量不能经过指针")?;
        sim.add_counter(addr, t.ty.size, 1000 * (k as u64 + 1));
    }
    let listen = std::env::var("XRDAP_SIM_LISTEN").unwrap_or_else(|_| "127.0.0.1:3241".into());
    let addr = sim.serve_tcp_on(&listen)?;
    println!("模拟探针在 {addr}，按 Ctrl-C 停止");
    loop {
        std::thread::park();
    }
}
