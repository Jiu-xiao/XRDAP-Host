//! 把一块探针（例如经串口连接的 ESP32-C3）共享成标准的 USB CMSIS-DAP 探针。
//!
//! ```text
//! cargo run -p xrdap --example share                 # 在 127.0.0.1:3240 等待挂载
//! sudo usbip attach -r 127.0.0.1 -b 1-1              # Linux；Windows 装 usbip-win2 后同样写法
//! ```
//!
//! 挂载后 Keil、pyOCD、probe-rs、OpenOCD 都能把它当 USB 探针用。按回车停止，对挂载的一方
//! 来说设备被拔掉。探针用环境变量 `XRDAP_PROBE` 指定（写法同 `xrdap -p`），不设时自动选
//! 唯一的那块。

use xrdap::addr::Spec;
use xrdap::lang::tr;
use xrdap::open::{open, OpenOptions};
use xrdap::share::{Event, Protocol, Share};
use xrdap::trf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = Spec::parse(&std::env::var("XRDAP_PROBE").unwrap_or_default());
    let probe = open(&spec, &OpenOptions::default())?;
    let share = Share::start(probe, Protocol::Usbip, "127.0.0.1:3240", |e| match e {
        Event::Client(peer) => println!("{}", trf!("{peer} connected", "{peer} 已连接")),
        Event::Idle(None) => println!("{}", tr("Disconnected", "已断开")),
        Event::Idle(Some(why)) => println!("{}", trf!("Connection lost: {why}", "异常断开：{why}")),
    })
    .map_err(|(_, e)| e)?;
    let device = share.device();
    println!(
        "{}",
        trf!("{} ({})", "{}（{}）", device.product, device.serial)
    );
    println!("{}", trf!("Attach: {}", "挂载：{}", share.attach_hint()));
    println!("{}", tr("Press Enter to stop", "按回车停止"));
    std::io::stdin().read_line(&mut String::new())?;
    let _probe = share.stop();
    Ok(())
}
