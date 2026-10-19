//! 共享为 USB DAP：在本机端口上提供 USB/IP 设备，Keil、pyOCD、probe-rs、OpenOCD 挂载后
//! 当 USB 探针用（见 xrdap 库的 `usbip` 模块）。共享期间探针归共享线程，界面不能读写变量；
//! 停止共享时探针交回后台线程。

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use xrdap::dap::{Probe, ID_PRODUCT, ID_SERIAL};
use xrdap::usbip::{self, DeviceInfo};

/// SUB_CTRL 停止采样：外部工具开过采样的话，断开后替它停掉
const STOP_SAMPLING: [u8; 2] = [0x84, 0x00];
/// 探针没有应答时回的响应：0xFF 表示命令不支持，调试工具会报错而不是一直等
const NO_RESPONSE: [u8; 1] = [0xFF];
/// 等客户端时多久看一次停止标志
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// 共享线程报告的变化。
#[derive(Debug, Clone)]
pub enum ShareEvent {
    /// 客户端连上（挂载或查询设备列表）
    Client(SocketAddr),
    /// 客户端断开
    Idle,
}

/// 正在进行的共享。
pub struct Share {
    addr: SocketAddr,
    info: DeviceInfo,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Probe>,
}

impl Share {
    /// 在 `listen` 上开始共享。失败时把探针还回来。
    pub fn start(
        mut probe: Probe,
        listen: &str,
        notify: impl Fn(ShareEvent) + Send + 'static,
    ) -> Result<Self, (Probe, String)> {
        let bound = TcpListener::bind(listen).and_then(|l| {
            l.set_nonblocking(true)?;
            let addr = l.local_addr()?;
            Ok((l, addr))
        });
        let (listener, addr) = match bound {
            Ok(b) => b,
            Err(e) => return Err((probe, format!("监听 {listen} 失败：{e}"))),
        };
        let product = probe.dap_info_string(ID_PRODUCT).ok().flatten();
        let serial = probe.dap_info_string(ID_SERIAL).ok().flatten();
        let info = DeviceInfo::new(
            product.as_deref().unwrap_or("XR-DAP"),
            serial.as_deref().unwrap_or("XRDAP"),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (info, stop) = (info.clone(), stop.clone());
            std::thread::Builder::new()
                .name("xrdap-usbip".into())
                .spawn(move || serve(listener, &info, probe, &stop, notify))
                .expect("启动共享线程")
        };
        Ok(Self {
            addr,
            info,
            stop,
            thread,
        })
    }

    /// 虚拟 USB 设备的产品名和序列号。
    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// 挂载命令（usbip-win2 和 Linux 的 usbip 写法相同）。
    pub fn attach_command(&self) -> String {
        let host = if self.addr.ip().is_unspecified() {
            "127.0.0.1".to_string()
        } else {
            self.addr.ip().to_string()
        };
        let port = if self.addr.port() == usbip::PORT {
            String::new()
        } else {
            format!(" --tcp-port {}", self.addr.port())
        };
        format!("usbip{port} attach -r {host} -b 1-1")
    }

    /// 停止共享：0.1 秒内断开正在用的客户端（对它来说设备被拔掉），取回探针。
    pub fn stop(self) -> Probe {
        self.stop.store(true, Ordering::Release);
        self.thread.join().expect("共享线程崩溃")
    }
}

fn serve(
    listener: TcpListener,
    info: &DeviceInfo,
    mut probe: Probe,
    stop: &AtomicBool,
    notify: impl Fn(ShareEvent),
) -> Probe {
    while !stop.load(Ordering::Acquire) {
        let (stream, peer) = match listener.accept() {
            Ok(c) => c,
            // 没有客户端（WouldBlock）或一次失败的 accept：过一会儿再看
            Err(_) => {
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
        };
        if stream.set_nonblocking(false).is_err() {
            continue;
        }
        notify(ShareEvent::Client(peer));
        let _ = usbip::serve_connection_until(
            stream,
            info,
            &mut |req| probe.xfer(req).unwrap_or_else(|_| NO_RESPONSE.to_vec()),
            stop,
        );
        let _ = probe.xfer(&STOP_SAMPLING);
        notify(ShareEvent::Idle);
    }
    probe
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::mpsc::channel;
    use xrdap::fake::FakeProbe;

    fn be(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    #[test]
    fn share_attach_dap_info_and_stop() {
        let (tx, rx) = channel();
        let probe = Probe::new(Box::new(FakeProbe::new()));
        let share = Share::start(probe, "127.0.0.1:0", move |e| {
            let _ = tx.send(e);
        })
        .map_err(|(_, e)| e)
        .unwrap();
        let port: u16 = share
            .attach_command()
            .split_whitespace()
            .skip_while(|w| *w != "--tcp-port")
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();

        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut req = vec![0x01, 0x11, 0x80, 0x03, 0, 0, 0, 0];
        req.extend(b"1-1".iter().copied().chain(std::iter::repeat_n(0, 29)));
        c.write_all(&req).unwrap();
        let mut rep = vec![0u8; 8 + 312];
        c.read_exact(&mut rep).unwrap();
        assert!(matches!(rx.recv().unwrap(), ShareEvent::Client(_)));

        // IN 先到，再发 DAP_Info(产品名)
        let mut m = be(&[1, 1, 0x10001, 1, 1, 0, 512, 0, 0, 0]);
        m.extend([0; 8]);
        m.extend(be(&[1, 2, 0x10001, 0, 1, 0, 2, 0, 0, 0]));
        m.extend([0; 8]);
        m.extend([0x00, 0x02]);
        c.write_all(&m).unwrap();
        let mut h = [0u8; 48];
        c.read_exact(&mut h).unwrap(); // OUT 的回复，不带数据
        c.read_exact(&mut h).unwrap();
        let n = u32::from_be_bytes(h[24..28].try_into().unwrap()) as usize;
        let mut d = vec![0; n];
        c.read_exact(&mut d).unwrap();
        assert_eq!(&d[..1], &[0x00]);

        // 停止共享：客户端被断开，探针交回来还能用
        let t = std::time::Instant::now();
        let mut probe = share.stop();
        assert!(t.elapsed() < Duration::from_secs(1));
        assert!(probe.xfer(&[0x00, 0xFF]).is_ok());
        assert!(c.read(&mut h).map_or(true, |n| n == 0));
    }
}
