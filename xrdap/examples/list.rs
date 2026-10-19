//! 列出 USB、串口、局域网上的探针。
//!
//! ```text
//! cargo run -p xrdap --example list
//! ```

use xrdap::discover::{find, group, table, Options};

fn main() {
    let probes = group(find(&Options::default()));
    if probes.is_empty() {
        println!("没有找到探针");
        return;
    }
    print!("{}", table(&probes));
}
