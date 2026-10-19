//! 三种传输：USB（CMSIS-DAP v2 Bulk）、串口（libxr Topic 帧）、TCP（`"DAP\0"` 帧）。
//! 每种传输都是"发一个 DAP 包，收一个 DAP 包"。

pub mod serial;
pub mod tcp;
#[cfg(feature = "usb")]
pub mod usb;

use crate::Result;

/// 一次一个请求的 DAP 包传输。
pub trait Transport: Send {
    /// 发一个 DAP 请求，等它的响应。
    fn xfer(&mut self, request: &[u8]) -> Result<Vec<u8>>;

    /// 给用户看的连接描述，例如 `USB XRobot-XRDAP-xxxx`。
    fn describe(&self) -> String;

    /// 一个 DAP 包最多的字节数（探针报告的包长），由上层在查询 DAP_Info 后告诉传输。
    fn set_packet_size(&mut self, _size: usize) {}
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn xfer(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        (**self).xfer(request)
    }

    fn describe(&self) -> String {
        (**self).describe()
    }

    fn set_packet_size(&mut self, size: usize) {
        (**self).set_packet_size(size)
    }
}
