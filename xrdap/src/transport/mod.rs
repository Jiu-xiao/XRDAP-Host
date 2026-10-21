//! 三种传输：USB（CMSIS-DAP v2 Bulk）、串口（libxr Topic 帧）、TCP（`"DAP\0"` 帧）。
//! 传输只负责把一个 DAP 包发出去、把下一个 DAP 包收回来；请求和响应怎么对应由
//! [`Probe`](crate::dap::Probe) 决定。
//!
//! Three transports: USB (CMSIS-DAP v2 bulk), serial (libxr topic frames) and TCP (`"DAP\0"`
//! frames). A transport only sends one DAP packet and receives the next one; pairing requests
//! with responses is up to [`Probe`](crate::dap::Probe).

#[cfg(test)]
pub(crate) mod script;
pub mod serial;
pub mod tcp;
pub mod usb;

use std::time::Duration;

use crate::Result;

/// 收发 DAP 包的一条链路。
///
/// A link that sends and receives DAP packets.
pub trait Transport: Send {
    /// 发出一个 DAP 请求包。
    ///
    /// Send one DAP request packet.
    fn send(&mut self, request: &[u8]) -> Result<()>;

    /// 等下一个收到的 DAP 包，最多等 `timeout`；超时返回 [`Error::Timeout`](crate::Error::Timeout)。
    ///
    /// Wait up to `timeout` for the next DAP packet; returns
    /// [`Error::Timeout`](crate::Error::Timeout) when none arrives.
    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>>;

    /// 给用户看的连接描述，例如 `USB XRobot-XRDAP-xxxx`。
    ///
    /// Human-readable description, e.g. `USB XRobot-XRDAP-xxxx`.
    fn describe(&self) -> String;

    /// 探针报告的 DAP 包长（USB 的 IN 传输长度依赖它）。
    ///
    /// The DAP packet size reported by the probe (USB IN transfers depend on it).
    fn set_packet_size(&mut self, _size: usize) {}
}
