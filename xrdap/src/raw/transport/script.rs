//! 测试用的链路：按 CMSIS-DAP 的规则应答——每个响应的第一个字节回显命令号，
//! DAP_TransferAbort（0x07）没有响应；可以让某个响应迟到，模拟超时后才到的响应。
//!
//! Test link following the CMSIS-DAP rules: every response echoes the command ID in its first
//! byte and DAP_TransferAbort (0x07) gets none. A response can be made late, to model one that
//! arrives after its request timed out.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::Transport;
use crate::{Error, Result};

/// 迟到的响应什么时候到。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Late {
    /// 请求超时后、下一次接收时到
    NextRecv,
    /// 下一个请求发出后才到（比下一个请求的响应先到）
    NextSend,
}

#[derive(Default)]
struct State {
    sent: Vec<Vec<u8>>,
    ready: VecDeque<Vec<u8>>,
    late: Option<(Late, Vec<u8>)>,
    make_late: Option<Late>,
}

/// 响应是 `命令号 01 序号`：序号是第几个请求（从 0 起），用来分辨是哪个请求的响应；
/// 对 DAP_Info 来说就是一个字节长的信息。
#[derive(Clone, Default)]
pub(crate) struct Script(Arc<Mutex<State>>);

impl Script {
    /// 下一个请求的响应迟到。
    pub(crate) fn delay_next(&self, when: Late) {
        self.0.lock().unwrap().make_late = Some(when);
    }

    /// 到现在为止发出的请求。
    pub(crate) fn sent(&self) -> Vec<Vec<u8>> {
        self.0.lock().unwrap().sent.clone()
    }
}

impl Transport for Script {
    fn send(&mut self, request: &[u8]) -> Result<()> {
        let mut st = self.0.lock().unwrap();
        if let Some((Late::NextSend, r)) = st.late.take_if(|(w, _)| *w == Late::NextSend) {
            st.ready.push_back(r);
        }
        let seq = st.sent.len() as u8;
        st.sent.push(request.to_vec());
        if request.first() == Some(&0x07) {
            return Ok(());
        }
        let resp = vec![request[0], 1, seq];
        match st.make_late.take() {
            Some(when) => st.late = Some((when, resp)),
            None => st.ready.push_back(resp),
        }
        Ok(())
    }

    fn recv(&mut self, _timeout: Duration) -> Result<Vec<u8>> {
        let mut st = self.0.lock().unwrap();
        if let Some(r) = st.ready.pop_front() {
            return Ok(r);
        }
        if let Some((Late::NextRecv, r)) = st.late.take_if(|(w, _)| *w == Late::NextRecv) {
            // 这一次接收已经超时，响应在它之后才到
            st.ready.push_back(r);
        }
        Err(Error::Timeout("没有响应".into()))
    }

    fn describe(&self) -> String {
        "测试链路".into()
    }
}
