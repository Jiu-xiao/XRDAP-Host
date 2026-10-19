//! 把一块探针共享给别的调试工具。
//!
//! | 协议 | 端口 | 谁来用 |
//! |---|---|---|
//! | [`Protocol::Usbip`] | 3240 | 用 USB/IP 客户端挂载（Linux 内核自带 `usbip`，Windows 装 usbip-win2）后，系统里出现一个 USB CMSIS-DAP v2 探针，Keil、pyOCD、probe-rs、OpenOCD 都能用 |
//! | [`Protocol::OpenOcd`] | 3241 | OpenOCD 的 `cmsis-dap backend tcp` |
//!
//! 探针本身经串口、TCP 还是 USB 连着都可以，常见用法是把插在电脑上的 ESP32-C3（串口）变成
//! USB 探针。共享在后台线程里进行，探针归这个线程所有；[`Share::stop`] 断开正在用的客户端
//! （对它来说设备被拔掉），把探针交回来。同一时间只服务一个客户端，客户端离开时如果探针还在
//! 采样就替它停掉。
//!
//! Share one probe with other debug tools: as a USB CMSIS-DAP v2 device over USB/IP
//! (Keil, pyOCD, probe-rs, OpenOCD) or as OpenOCD's CMSIS-DAP TCP backend. The probe may
//! be attached over serial, TCP or USB; the share runs in a background thread that owns
//! the probe until [`Share::stop`] hands it back.
//!
//! ```no_run
//! use xrdap::open::{open, OpenOptions};
//! use xrdap::share::{Protocol, Share};
//!
//! # fn main() -> xrdap::Result<()> {
//! let probe = open(Some("COM7"), &OpenOptions::default())?;
//! let share = Share::start(probe, Protocol::Usbip, "127.0.0.1:3240", |e| println!("{e:?}"))
//!     .map_err(|(_, e)| e)?;
//! println!("挂载：{}", share.attach_hint());
//! std::thread::sleep(std::time::Duration::from_secs(60));
//! let _probe = share.stop();
//! # Ok(())
//! # }
//! ```

mod link;
mod openocd;
mod usbip;

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub use usbip::DeviceInfo;

use crate::dap::Probe;
use crate::{Error, Result};
use link::{is_normal_end, Link};

/// 探针没有应答时回的 DAP 响应：0xFF 表示命令不支持，调试工具会报错而不是一直等
const DAP_INVALID: u8 = 0xFF;
/// 等客户端时多久看一次停止标志
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// 共享给别的工具时用的协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// USB/IP 虚拟 USB 探针
    Usbip,
    /// OpenOCD 的 CMSIS-DAP TCP 后端
    OpenOcd,
}

impl Protocol {
    /// 默认端口：USB/IP 3240，OpenOCD 3241
    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Usbip => 3240,
            Protocol::OpenOcd => 3241,
        }
    }
}

/// 共享线程报告的变化。
#[derive(Debug, Clone)]
pub enum Event {
    /// 客户端连上（USB/IP 的挂载或设备列表查询，或 OpenOCD 连接）
    Client(SocketAddr),
    /// 客户端断开；`Some` 是异常断开的原因
    Idle(Option<String>),
}

/// 正在进行的共享。
pub struct Share {
    protocol: Protocol,
    addr: SocketAddr,
    device: DeviceInfo,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Probe>,
}

impl Share {
    /// 在 `listen`（例如 `127.0.0.1:3240`）上开始共享。失败时把探针还回来。
    /// `notify` 在共享线程里调用。
    pub fn start(
        mut probe: Probe,
        protocol: Protocol,
        listen: &str,
        notify: impl Fn(Event) + Send + 'static,
    ) -> std::result::Result<Self, (Probe, Error)> {
        let bound = TcpListener::bind(listen).and_then(|l| {
            l.set_nonblocking(true)?;
            let addr = l.local_addr()?;
            Ok((l, addr))
        });
        let (listener, addr) = match bound {
            Ok(b) => b,
            Err(e) => {
                let msg = format!("监听 {listen} 失败：{e}");
                return Err((probe, Error::Io(msg)));
            }
        };
        let device = match device_info(&mut probe) {
            Ok(d) => d,
            Err(e) => return Err((probe, e)),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            let device = device.clone();
            std::thread::Builder::new()
                .name("xrdap-share".into())
                .spawn(move || run(listener, protocol, &device, probe, &stop, notify))
                .expect("创建共享线程")
        };
        Ok(Self {
            protocol,
            addr,
            device,
            stop,
            thread,
        })
    }

    /// USB/IP 虚拟设备的产品名和序列号
    pub fn device(&self) -> &DeviceInfo {
        &self.device
    }

    /// 客户端一侧怎么用：USB/IP 的挂载命令，或 OpenOCD 的配置行。
    pub fn attach_hint(&self) -> String {
        let host = if self.addr.ip().is_unspecified() {
            "<本机 IP>".to_string()
        } else {
            self.addr.ip().to_string()
        };
        let port = self.addr.port();
        match self.protocol {
            Protocol::Usbip if port == Protocol::Usbip.default_port() => {
                format!("usbip attach -r {host} -b 1-1")
            }
            Protocol::Usbip => format!("usbip --tcp-port {port} attach -r {host} -b 1-1"),
            Protocol::OpenOcd => format!(
                "adapter driver cmsis-dap; cmsis-dap backend tcp; \
                 cmsis-dap tcp host {host}; cmsis-dap tcp port {port}"
            ),
        }
    }

    /// 停止共享：0.1 秒内断开正在用的客户端，取回探针。
    pub fn stop(self) -> Probe {
        self.stop.store(true, Ordering::Release);
        self.thread
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }
}

fn device_info(probe: &mut Probe) -> Result<DeviceInfo> {
    let id = probe.identity()?;
    Ok(DeviceInfo::new(
        id.product.as_deref().unwrap_or("XR-DAP"),
        id.serial.as_deref().unwrap_or("XRDAP"),
    ))
}

fn run(
    listener: TcpListener,
    protocol: Protocol,
    device: &DeviceInfo,
    mut probe: Probe,
    stop: &AtomicBool,
    notify: impl Fn(Event),
) -> Probe {
    while !stop.load(Ordering::Acquire) {
        let Ok((stream, peer)) = listener.accept() else {
            // 没有客户端（WouldBlock）或一次失败的 accept：过一会儿再看
            std::thread::sleep(ACCEPT_POLL);
            continue;
        };
        let Ok(mut link) = Link::new(stream, stop) else {
            continue;
        };
        notify(Event::Client(peer));
        let mut exec = |req: &[u8]| probe.xfer(req).unwrap_or_else(|_| vec![DAP_INVALID]);
        let result = match protocol {
            Protocol::Usbip => usbip::serve(&mut link, device, &mut exec),
            Protocol::OpenOcd => openocd::serve(&mut link, &mut exec),
        };
        // 客户端开过采样的话替它停掉，否则探针会一直占着 SWD
        let _ = probe.stop_sampling();
        notify(Event::Idle(
            result
                .err()
                .filter(|e| !is_normal_end(e))
                .map(|e| e.to_string()),
        ));
    }
    probe
}
