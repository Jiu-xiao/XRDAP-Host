//! OpenOCD `cmsis-dap backend tcp` 的服务端：一个请求帧对应一个响应帧。
//! 帧格式：`"DAP\0" + 长度(u16 小端) + 类型(1 请求 2 响应) + 保留(1) + DAP 包`，见
//! [`crate::frame`]。

use std::io;

use super::link::{bad, Link};
use crate::frame::{
    parse_tcp_header, tcp_frame, TCP_HEADER_LEN, TCP_TYPE_REQUEST, TCP_TYPE_RESPONSE,
};

/// 处理一个客户端连接，直到对方断开或出错。
pub(super) fn serve(s: &mut Link, exec: &mut dyn FnMut(&[u8]) -> Vec<u8>) -> io::Result<()> {
    let mut header = [0u8; TCP_HEADER_LEN];
    loop {
        s.read_full(&mut header)?;
        let len = parse_tcp_header(&header, TCP_TYPE_REQUEST)
            .ok_or_else(|| bad(format!("OpenOCD TCP：帧头无效 {header:02X?}")))?;
        let mut request = vec![0u8; len];
        s.read_full(&mut request)?;
        s.write_all(&tcp_frame(&exec(&request), TCP_TYPE_RESPONSE))?;
    }
}
