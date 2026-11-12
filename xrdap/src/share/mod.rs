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
//! Share one probe with other debug tools.
//!
//! | Protocol | Port | Used by |
//! |---|---|---|
//! | [`Protocol::Usbip`] | 3240 | After a USB/IP client attaches it (`usbip` in the Linux kernel tools, usbip-win2 on Windows), the system shows a USB CMSIS-DAP v2 probe that Keil, pyOCD, probe-rs and OpenOCD can use |
//! | [`Protocol::OpenOcd`] | 3241 | OpenOCD's `cmsis-dap backend tcp` |
//!
//! The probe may be attached over serial, TCP or USB; a common use is turning an ESP32-C3
//! plugged into the computer (serial) into a USB probe. The share runs in a background thread
//! that owns the probe. [`Share::stop`] disconnects the client in use (the device appears
//! unplugged to it) and hands the probe back. Only one client is served at a time; when a
//! client leaves while the probe is still sampling, sampling is stopped for it.
//!
//! ```no_run
//! use xrdap::addr::Spec;
//! use xrdap::open::{open, OpenOptions};
//! use xrdap::share::{Protocol, Share};
//!
//! # fn main() -> xrdap::Result<()> {
//! let probe = open(&Spec::parse("COM3"), &OpenOptions::default())?;
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

use std::io;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

pub use usbip::DeviceInfo;

use crate::dap::Probe;
use crate::lang::tr;
use crate::{trf, Error, Result};
use link::{is_normal_end, Link};

/// 把客户端的 DAP 请求转给探针：发出去不等应答，应答按发出的顺序收（[`Probe::forward_send`]）。
/// 客户端（OpenOCD 等）按探针报的包数同时挂几个请求；共享在客户端暂时没有新请求时才去收应答，
/// 这样经 Wi-Fi、串口时几个请求的来回叠在一起，不是一个一个地等。
trait Forward {
    /// 发一个请求，不等应答。
    fn send(&mut self, request: &[u8]) -> io::Result<()>;
    /// 有没有到期没收的应答。
    fn due(&self) -> bool;
    /// 探针那里的包已经到了它报的包数，要先收应答才能再发。
    fn full(&self) -> bool;
    /// 收下一个到期的应答。
    fn recv(&mut self) -> io::Result<Vec<u8>>;
}

/// 转发到一块真实的探针。
struct ProbeForward<'a> {
    probe: &'a mut Probe,
    /// 探针报的 DAP 包数（DAP_Info 0xFE）：探针最多同时收这么多个包
    packet_count: usize,
}

impl Forward for ProbeForward<'_> {
    fn send(&mut self, request: &[u8]) -> io::Result<()> {
        self.probe.forward_send(request).map_err(probe_error)
    }

    fn due(&self) -> bool {
        self.probe.forward_due()
    }

    fn full(&self) -> bool {
        self.probe.forward_in_flight() >= self.packet_count
    }

    fn recv(&mut self) -> io::Result<Vec<u8>> {
        self.probe.forward_recv().map_err(probe_error)
    }
}

/// 探针读写出错（超时、断开）时结束这个客户端，原因经 [`Event::Idle`] 报告
fn probe_error(e: Error) -> io::Error {
    io::Error::other(trf!("Probe: {e}", "探针：{e}"))
}

/// 探针报的 DAP 包数；没报或读不出时按 1 个，一问一答。
fn packet_count(probe: &mut Probe) -> usize {
    probe
        .dap_info(crate::dap::ID_PACKET_COUNT)
        .ok()
        .and_then(|v| v.first().copied())
        .map_or(1, |n| usize::from(n).max(1))
}

/// 等客户端时多久看一次停止标志
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// 共享给别的工具时用的协议。
///
/// The protocol used to share the probe with other tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// USB/IP 虚拟 USB 探针。
    ///
    /// USB/IP virtual USB probe.
    Usbip,
    /// OpenOCD 的 CMSIS-DAP TCP 后端。
    ///
    /// OpenOCD's CMSIS-DAP TCP backend.
    OpenOcd,
}

impl Protocol {
    /// 默认端口：USB/IP 3240，OpenOCD 3241。
    ///
    /// Default port: 3240 for USB/IP, 3241 for OpenOCD.
    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Usbip => 3240,
            Protocol::OpenOcd => 3241,
        }
    }
}

/// 共享线程报告的变化。
///
/// A change reported by the share thread.
#[derive(Debug, Clone)]
pub enum Event {
    /// 客户端连上（USB/IP 的挂载或设备列表查询，或 OpenOCD 连接）。
    ///
    /// A client connected (a USB/IP attach or device-list query, or an OpenOCD connection).
    Client(SocketAddr),
    /// 客户端断开；`Some` 是异常断开的原因。
    ///
    /// The client disconnected; `Some` carries the reason of an abnormal disconnect.
    Idle(Option<String>),
}

/// 正在进行的共享。
///
/// A running share.
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
    ///
    /// Start sharing on `listen` (e.g. `127.0.0.1:3240`). On failure the probe is handed back.
    /// `notify` is called on the share thread.
    pub fn start(
        probe: Probe,
        protocol: Protocol,
        listen: &str,
        notify: impl Fn(Event) + Send + 'static,
    ) -> std::result::Result<Self, (Probe, Error)> {
        let thread = std::thread::Builder::new().name("xrdap-share".into());
        Self::start_on(thread, probe, protocol, listen, notify)
    }

    /// [`Share::start`] 的主体，共享线程按 `thread` 创建。
    fn start_on(
        thread: std::thread::Builder,
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
                let msg = trf!(
                    "Listening on {listen} failed: {e}",
                    "监听 {listen} 失败：{e}"
                );
                return Err((probe, Error::Io(msg)));
            }
        };
        let device = match device_info(&mut probe) {
            Ok(d) => d,
            Err(e) => return Err((probe, e)),
        };
        let packet_count = packet_count(&mut probe);
        let stop = Arc::new(AtomicBool::new(false));
        // 探针在线程建好之后才交过去：建线程失败时探针还在这里，可以还给调用方
        let (handoff, receive) = mpsc::sync_channel::<Probe>(1);
        let spawned = {
            let stop = stop.clone();
            let device = device.clone();
            thread.spawn(move || {
                let probe = receive.recv().expect("start 建好线程后立即交出探针");
                run(
                    listener,
                    protocol,
                    &device,
                    probe,
                    packet_count,
                    &stop,
                    notify,
                )
            })
        };
        let thread = match spawned {
            Ok(t) => t,
            Err(e) => {
                return Err((
                    probe,
                    Error::Io(trf!(
                        "Creating the share thread failed: {e}",
                        "创建共享线程失败：{e}"
                    )),
                ))
            }
        };
        if let Err(mpsc::SendError(probe)) = handoff.send(probe) {
            return Err((
                probe,
                Error::Io(tr("The share thread did not start", "共享线程没有启动").into()),
            ));
        }
        Ok(Self {
            protocol,
            addr,
            device,
            stop,
            thread,
        })
    }

    /// USB/IP 虚拟设备的产品名和序列号。
    ///
    /// Product name and serial number of the USB/IP virtual device.
    pub fn device(&self) -> &DeviceInfo {
        &self.device
    }

    /// 客户端一侧怎么用：USB/IP 的挂载命令，或 OpenOCD 的配置行。
    ///
    /// How the client side uses it: the USB/IP attach command, or the OpenOCD configuration
    /// line.
    pub fn attach_hint(&self) -> String {
        let host = if self.addr.ip().is_unspecified() {
            tr("<this computer's IP>", "<本机 IP>").to_string()
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

    /// 停止共享：断开正在用的客户端，取回探针。
    ///
    /// Stop sharing: disconnect the client in use and take the probe back.
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
    packet_count: usize,
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
        let mut forward = ProbeForward {
            probe: &mut probe,
            packet_count,
        };
        let result = match protocol {
            Protocol::Usbip => usbip::serve(&mut link, device, &mut forward),
            Protocol::OpenOcd => openocd::serve(&mut link, &mut forward),
        };
        // 客户端走时还在途的请求作废，它们迟到的应答不能给到下面的命令
        probe.forward_abandon();
        // 客户端开过采样的话替它停掉，否则探针会一直占着 SWD
        let _ = crate::watch::stop(&mut probe);
        // 客户端做的连接也结束：探针只能靠这条命令知道（协议文档 3.1 节）
        probe.end_session();
        notify(Event::Idle(
            result
                .err()
                .filter(|e| !is_normal_end(e))
                .map(|e| e.to_string()),
        ));
    }
    probe
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::frame::{parse_tcp_header, tcp_frame, TCP_TYPE_REQUEST, TCP_TYPE_RESPONSE};
    use crate::raw::transport::script::{Late, Script};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::mpsc;

    fn share(link: &Script) -> (Share, mpsc::Receiver<Event>) {
        let probe = Probe::new(Box::new(link.clone()), Duration::from_secs(2));
        let (tx, rx) = mpsc::channel();
        let share = Share::start(probe, Protocol::OpenOcd, "127.0.0.1:0", move |e| {
            let _ = tx.send(e);
        })
        .map_err(|(_, e)| e)
        .unwrap();
        (share, rx)
    }

    fn client(share: &Share) -> TcpStream {
        let c = TcpStream::connect(share.addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c
    }

    fn read_frame(c: &mut TcpStream) -> Vec<u8> {
        let mut h = [0u8; 8];
        c.read_exact(&mut h).unwrap();
        let mut p = vec![0u8; parse_tcp_header(&h, TCP_TYPE_RESPONSE).unwrap()];
        c.read_exact(&mut p).unwrap();
        p
    }

    /// CMSIS-DAP：DAP_TransferAbort 没有响应，共享时也不回帧；下一个命令的响应不错位。
    #[test]
    fn openocd_transfer_abort_gets_no_frame() {
        let link = Script::default();
        let (share, _events) = share(&link);
        let mut c = client(&share);
        c.write_all(&tcp_frame(&[0x07], TCP_TYPE_REQUEST)).unwrap();
        c.write_all(&tcp_frame(&[0x00, 0xFF], TCP_TYPE_REQUEST))
            .unwrap();
        // 探针的第 0 个请求是共享开始时读身份的 DAP_Info，客户端的 0x07、DAP_Info 排在后面
        let first = read_frame(&mut c);
        assert_eq!(first[0], 0x00);
        let sent = link.sent();
        assert_eq!(sent[sent.len() - 2], vec![0x07]);
        drop(c);
        share.stop();
    }

    /// 客户端连发几个请求不等应答：都转给探针，应答按请求的顺序回来。
    #[test]
    fn openocd_requests_in_flight_answer_in_order() {
        let link = Script::default();
        let (share, _events) = share(&link);
        let mut c = client(&share);
        for cmd in [0x05u8, 0x06, 0x05, 0x00] {
            c.write_all(&tcp_frame(&[cmd, 0], TCP_TYPE_REQUEST))
                .unwrap();
        }
        let got: Vec<u8> = (0..4).map(|_| read_frame(&mut c)[0]).collect();
        assert_eq!(got, vec![0x05, 0x06, 0x05, 0x00]);
        // 每个应答的序号依次加一：没有错位，也没有丢
        let sent = link.sent();
        assert_eq!(
            &sent[sent.len() - 4..],
            &[vec![0x05, 0], vec![0x06, 0], vec![0x05, 0], vec![0x00, 0]]
        );
        drop(c);
        share.stop();
    }

    /// 建不了共享线程时不 panic，探针交还给调用方。
    #[test]
    fn spawn_failure_returns_the_probe() {
        crate::lang::set_chinese(true);
        let link = Script::default();
        let probe = Probe::new(Box::new(link.clone()), Duration::from_secs(2));
        // 栈大到系统给不出来，建线程失败
        let huge = std::thread::Builder::new().stack_size(usize::MAX / 2);
        let Err((mut probe, e)) =
            Share::start_on(huge, probe, Protocol::OpenOcd, "127.0.0.1:0", |_| {})
        else {
            panic!("共享线程不应该建得起来");
        };
        assert!(e.to_string().contains("共享线程"), "{e}");
        assert!(probe.identity().is_ok());
    }

    /// 客户端离开后探针收到 DAP_Disconnect，客户端做的连接结束。
    #[test]
    fn client_leaving_ends_the_probe_session() {
        let link = Script::default();
        let (share, events) = share(&link);
        let mut c = client(&share);
        c.write_all(&tcp_frame(&[0x00, 0xFF], TCP_TYPE_REQUEST))
            .unwrap();
        let _ = read_frame(&mut c);
        drop(c);
        // Event::Idle 在连接结束之后发出
        assert!(events.iter().any(|e| matches!(e, Event::Idle(_))));
        assert_eq!(link.sent().last(), Some(&vec![0x03]));
        share.stop();
    }

    /// 探针不应答时断开这个客户端，并报告原因；不回假的 0xFF。
    #[test]
    fn probe_failure_ends_client_with_reason() {
        crate::lang::set_chinese(true);
        let link = Script::default();
        let (share, events) = share(&link);
        let mut c = client(&share);
        link.delay_next(Late::NextSend);
        c.write_all(&tcp_frame(&[0x00, 0xFF], TCP_TYPE_REQUEST))
            .unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(c.read(&mut buf).unwrap(), 0, "客户端应被断开，不应收到响应");
        let reason = events
            .iter()
            .find_map(|e| match e {
                Event::Idle(r) => Some(r),
                Event::Client(_) => None,
            })
            .unwrap();
        assert!(reason.is_some_and(|r| r.contains("探针")));
        share.stop();
    }
}
