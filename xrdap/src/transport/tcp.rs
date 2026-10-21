//! TCP 传输：探针的 3241 端口，或 `xrdap share openocd` 的监听端口。帧格式见
//! [`frame`](crate::frame)。
//!
//! TCP transport: the probe's port 3241, or the listening port of `xrdap share openocd`.
//! Frame format: [`frame`](crate::frame).

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use super::Transport;
use crate::frame::{
    parse_tcp_header, tcp_frame, TCP_HEADER_LEN, TCP_TYPE_REQUEST, TCP_TYPE_RESPONSE,
};
use crate::{Error, Result};

/// 探针的 TCP DAP 端口。
///
/// The probe's TCP DAP port.
pub const DEFAULT_PORT: u16 = 3241;

/// 连到探针或转发程序的 TCP 连接。超时时收到一半的帧留在缓冲里，下次接着收，帧边界保持对齐。
///
/// A TCP connection to a probe or relay. A frame half received when a wait times out stays
/// buffered and is completed by the next receive, so frame boundaries stay aligned.
pub struct TcpTransport {
    stream: TcpStream,
    peer: String,
    buf: Vec<u8>,
}

impl TcpTransport {
    /// `addr` 为 `host` 或 `host:port`，没有端口时用 3241；`timeout` 是建立连接的最长时间。
    ///
    /// `addr` is `host` or `host:port` (port 3241 by default); `timeout` bounds connecting.
    pub fn connect(addr: &str, timeout: Duration) -> Result<Self> {
        let full = with_default_port(addr);
        let mut last_err = None;
        let addrs = full
            .to_socket_addrs()
            .map_err(|e| Error::Io(format!("解析地址 {full} 失败：{e}")))?;
        for sa in addrs {
            match TcpStream::connect_timeout(&sa, timeout) {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    stream.set_write_timeout(Some(timeout))?;
                    return Ok(Self {
                        stream,
                        peer: full,
                        buf: Vec::new(),
                    });
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(Error::Io(format!(
            "连不上 {full}：{}",
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "没有可用的地址".into())
        )))
    }

    /// 缓冲里已有一个完整的响应帧时取出它的 DAP 包。
    fn take_frame(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(header) = self.buf.get(..TCP_HEADER_LEN) else {
            return Ok(None);
        };
        let header: &[u8; TCP_HEADER_LEN] = header.try_into().expect("长度已检查");
        let len = parse_tcp_header(header, TCP_TYPE_RESPONSE)
            .ok_or_else(|| Error::protocol(format!("TCP 响应帧头无效：{header:02X?}")))?;
        if self.buf.len() < TCP_HEADER_LEN + len {
            return Ok(None);
        }
        let packet = self.buf[TCP_HEADER_LEN..TCP_HEADER_LEN + len].to_vec();
        self.buf.drain(..TCP_HEADER_LEN + len);
        Ok(Some(packet))
    }
}

impl Transport for TcpTransport {
    fn send(&mut self, request: &[u8]) -> Result<()> {
        self.stream
            .write_all(&tcp_frame(request, TCP_TYPE_REQUEST))
            .map_err(|e| Error::Io(format!("TCP 发送失败：{e}")))
    }

    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut chunk = [0u8; 2048];
        loop {
            if let Some(packet) = self.take_frame()? {
                return Ok(packet);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Timeout("等待 TCP 响应超时".into()));
            }
            self.stream.set_read_timeout(Some(left))?;
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(Error::Io("TCP 连接被对方关闭".into())),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {}
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::Io(format!("TCP 接收失败：{e}"))),
            }
        }
    }

    fn describe(&self) -> String {
        format!("TCP {}", self.peer)
    }
}

/// 没写端口时补上 3241。IPv6 地址要写成 `[::1]:3241` 或不带端口的 `::1`。
///
/// Append port 3241 when none is given. Write IPv6 as `[::1]:3241` or a bare `::1`.
pub fn with_default_port(addr: &str) -> String {
    if addr.starts_with('[') {
        if addr.contains("]:") {
            addr.to_string()
        } else {
            format!("{addr}:{DEFAULT_PORT}")
        }
    } else {
        match addr.matches(':').count() {
            0 => format!("{addr}:{DEFAULT_PORT}"),
            1 => addr.to_string(),
            _ => format!("[{addr}]:{DEFAULT_PORT}"),
        }
    }
}
