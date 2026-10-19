//! TCP 传输：探针的 3241 端口或 xrdap-forward 的监听端口。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::Transport;
use crate::frame::{parse_tcp_header, tcp_frame, TCP_HEADER_LEN, TCP_TYPE_REQUEST, TCP_TYPE_RESPONSE};
use crate::{Error, Result};

pub const DEFAULT_PORT: u16 = 3241;

pub struct TcpTransport {
    stream: TcpStream,
    peer: String,
}

impl TcpTransport {
    /// `addr` 为 `host` 或 `host:port`，没有端口时用 3241。
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
                    stream.set_read_timeout(Some(timeout))?;
                    stream.set_write_timeout(Some(timeout))?;
                    return Ok(Self { stream, peer: full });
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(Error::Io(format!(
            "连不上 {full}：{}",
            last_err.map(|e| e.to_string()).unwrap_or_else(|| "没有可用的地址".into())
        )))
    }
}

impl Transport for TcpTransport {
    fn xfer(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        self.stream
            .write_all(&tcp_frame(request, TCP_TYPE_REQUEST))
            .map_err(|e| Error::Io(format!("TCP 发送失败：{e}")))?;
        let mut header = [0u8; TCP_HEADER_LEN];
        read_exact(&mut self.stream, &mut header)?;
        let len = parse_tcp_header(&header, TCP_TYPE_RESPONSE)
            .ok_or_else(|| Error::protocol(format!("TCP 响应帧头无效：{header:02X?}")))?;
        let mut packet = vec![0u8; len];
        read_exact(&mut self.stream, &mut packet)?;
        Ok(packet)
    }

    fn describe(&self) -> String {
        format!("TCP {}", self.peer)
    }
}

/// 没写端口时补上 3241。IPv6 地址要写成 `[::1]:3241` 或不带端口的 `::1`。
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

fn read_exact(stream: &mut TcpStream, buf: &mut [u8]) -> Result<()> {
    stream.read_exact(buf).map_err(|e| match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            Error::Timeout("等待 TCP 响应超时".into())
        }
        std::io::ErrorKind::UnexpectedEof => Error::Io("TCP 连接被对方关闭".into()),
        _ => Error::Io(format!("TCP 接收失败：{e}")),
    })
}
