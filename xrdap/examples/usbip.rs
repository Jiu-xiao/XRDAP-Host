//! 把一块探针（例如插在 COM7 上的 ESP32-C3）共享成标准的 USB CMSIS-DAP 探针。
//!
//! ```text
//! cargo run -p xrdap --example usbip                 # 在 127.0.0.1:3240 等待挂载
//! sudo usbip attach -r 127.0.0.1 -b 1-1              # Linux；Windows 装 usbip-win2 后同样写法
//! ```
//!
//! 挂载后 Keil、pyOCD、probe-rs、OpenOCD 都能把它当 USB 探针用。按 Ctrl-C 结束，
//! 对挂载的一方来说设备被拔掉。探针用环境变量 `XRDAP_PROBE` 指定（写法同 `xrdap --probe`），
//! 不设时自动选唯一的那块。

use std::net::TcpListener;

use xrdap::dap::{ID_PRODUCT, ID_SERIAL};
use xrdap::open::{open, OpenOptions};
use xrdap::usbip::{self, DeviceInfo, Event};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = std::env::var("XRDAP_PROBE").ok();
    let mut probe = open(spec.as_deref(), &OpenOptions::default())?;
    let product = probe
        .dap_info_string(ID_PRODUCT)?
        .unwrap_or("XR-DAP".into());
    let serial = probe.dap_info_string(ID_SERIAL)?.unwrap_or("XRDAP".into());
    let info = DeviceInfo::new(&product, &serial);

    let listener = TcpListener::bind(("127.0.0.1", usbip::PORT))?;
    println!("{}（{}）", info.product, info.serial);
    println!("挂载：usbip attach -r 127.0.0.1 -b 1-1");
    usbip::serve(
        &listener,
        &info,
        // 探针出错时回 0xFF（命令不支持），调试工具会报错而不是一直等
        &mut |req| probe.xfer(req).unwrap_or_else(|_| vec![0xFF]),
        |e| match e {
            Event::Connected(peer) => println!("{peer} 已连接"),
            Event::Disconnected(peer, _) => println!("{peer} 已断开"),
        },
    )?;
    Ok(())
}
