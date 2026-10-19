//! 查询或设置探针的 Wi-Fi。
//!
//! ```text
//! cargo run -p xrdap --example wifi                   # 查询
//! cargo run -p xrdap --example wifi -- 热点 密码       # 保存，探针随后重启
//! ```
//!
//! 探针用环境变量 `XRDAP_PROBE` 指定（写法同 `xrdap --probe`），不设时自动选唯一的那块。

use xrdap::open::{open, OpenOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = std::env::var("XRDAP_PROBE").ok();
    let mut probe = open(spec.as_deref(), &OpenOptions::default())?;
    let mut args = std::env::args().skip(1);
    match args.next() {
        None => println!("{}", xrdap::wifi::status(&mut probe)?.describe()),
        Some(ssid) => {
            let password = args.next().unwrap_or_default();
            xrdap::wifi::set(&mut probe, &ssid, &password)?;
            println!("已保存，探针重启后连接 {ssid}");
        }
    }
    Ok(())
}
