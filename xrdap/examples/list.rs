//! 列出 USB、局域网上的探针和按描述符认出的乐鑫 USB 串口，不打开任何串口。
//!
//! List probes on USB, on the LAN and on Espressif USB ports recognised by descriptor,
//! without opening any serial port.
//!
//! ```text
//! cargo run -p xrdap --example list
//! ```

use xrdap::discover::{find, group, Options};

fn main() {
    let probes = group(find(&Options::default()));
    if probes.is_empty() {
        println!("没有找到探针");
    }
    for p in probes {
        println!(
            "{}  {}",
            p.serial.as_deref().unwrap_or("?"),
            p.product.as_deref().unwrap_or("")
        );
        for l in &p.links {
            println!("    {}", l.addr);
        }
    }
}
