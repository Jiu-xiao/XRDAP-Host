//! OpenOCD `cmsis-dap backend tcp` 的服务端：一个请求帧对应一个响应帧。
//! 帧格式：`"DAP\0" + 长度(u16 小端) + 类型(1 请求 2 响应) + 保留(1) + DAP 包`，见
//! [`crate::frame`]。
//!
//! Server side of OpenOCD's `cmsis-dap backend tcp`: one request frame gets one response
//! frame. Frame format: `"DAP\0"`, length (u16 little-endian), type (1 request, 2 response),
//! a reserved byte and the DAP packet; see [`crate::frame`].

use std::io;

use super::link::{bad, Link};
use crate::frame::{
    parse_tcp_header, tcp_frame, TCP_HEADER_LEN, TCP_TYPE_REQUEST, TCP_TYPE_RESPONSE,
};

/// 处理一个客户端连接，直到对方断开或出错。没有响应的命令（DAP_TransferAbort，`exec`
/// 返回空包）不回帧。
pub(super) fn serve(s: &mut Link, exec: &mut super::Exec) -> io::Result<()> {
    let mut header = [0u8; TCP_HEADER_LEN];
    loop {
        s.read_full(&mut header)?;
        let len = parse_tcp_header(&header, TCP_TYPE_REQUEST)
            .ok_or_else(|| bad(format!("OpenOCD TCP：帧头无效 {header:02X?}")))?;
        let mut request = vec![0u8; len];
        s.read_full(&mut request)?;
        let response = exec(&request)?;
        if !response.is_empty() {
            s.write_all(&tcp_frame(&response, TCP_TYPE_RESPONSE))?;
        }
    }
}
