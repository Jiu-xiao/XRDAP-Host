//! CMSIS-DAP 标准命令、SWD/ADIv5 的最小初始化，以及厂商命令的封装。

use std::time::{Duration, Instant};

use crate::transport::Transport;
use crate::vendor::{self, Item, ItemResult, SubItem, SubRead, SubSetResult, VendorInfo};
use crate::{Error, Result};

pub const ID_VENDOR: u8 = 0x01;
pub const ID_PRODUCT: u8 = 0x02;
pub const ID_SERIAL: u8 = 0x03;
pub const ID_PROTOCOL: u8 = 0x04;
pub const ID_TARGET_VENDOR: u8 = 0x05;
pub const ID_TARGET_NAME: u8 = 0x06;
pub const ID_FIRMWARE: u8 = 0x09;
pub const ID_CAPABILITIES: u8 = 0xF0;
pub const ID_PACKET_COUNT: u8 = 0xFE;
pub const ID_PACKET_SIZE: u8 = 0xFF;

/// DAP_Transfer 请求字节里的 DP 寄存器（APnDP=0）。
const DP_READ_DPIDR: u8 = 0x02;
const DP_WRITE_CTRL_STAT: u8 = 0x04;
const DP_READ_CTRL_STAT: u8 = 0x06;
const DP_WRITE_SELECT: u8 = 0x08;

const CSYSPWRUPREQ: u32 = 1 << 30;
const CDBGPWRUPREQ: u32 = 1 << 28;
const CSYSPWRUPACK: u32 = 1 << 31;
const CDBGPWRUPACK: u32 = 1 << 29;

/// SWD 线复位（56 个 1）+ JTAG 转 SWD（0xE79E）+ 线复位，共 128 位。
const JTAG_TO_SWD: [u8; 16] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x9E, 0xE7, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF,
];

/// 连接目标后得到的信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetInfo {
    pub dpidr: u32,
}

impl TargetInfo {
    /// DPIDR 的可读说明：设计者、DP 版本、部件号。
    pub fn describe(&self) -> String {
        let d = self.dpidr;
        let designer = (d >> 1) & 0x7FF;
        let version = (d >> 12) & 0xF;
        let partno = (d >> 20) & 0xFF;
        let who = if designer == 0x23B { "ARM" } else { "其他厂商" };
        format!(
            "DPIDR 0x{d:08X}（{who}，设计者代码 0x{designer:03X}，DPv{version}，部件号 0x{partno:02X}）"
        )
    }
}

/// 一个探针连接。
pub struct Probe {
    t: Box<dyn Transport>,
    packet_size: usize,
}

impl Probe {
    pub fn new(t: Box<dyn Transport>) -> Self {
        Self {
            t,
            packet_size: 64,
        }
    }

    pub fn describe(&self) -> String {
        self.t.describe()
    }

    /// 探针报告的 DAP 包长（查询过 [`Probe::query_packet_size`] 之后才准确）。
    pub fn packet_size(&self) -> usize {
        self.packet_size
    }

    /// 发一个原始 DAP 包。
    pub fn xfer(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        self.t.xfer(request)
    }

    /// 发一个 DAP 包并记下请求前后的本机时间（用来把探针时间对到电脑时间）。
    pub fn xfer_timed(&mut self, request: &[u8]) -> Result<(Vec<u8>, Instant, Instant)> {
        let before = Instant::now();
        let resp = self.t.xfer(request)?;
        let after = Instant::now();
        Ok((resp, before, after))
    }

    // -------------------------------------------------------------- DAP_Info

    /// DAP_Info 原始数据（去掉命令字节和长度）。
    pub fn dap_info(&mut self, id: u8) -> Result<Vec<u8>> {
        let resp = self.xfer(&[0x00, id])?;
        if resp.len() < 2 || resp[0] != 0x00 {
            return Err(Error::protocol(format!(
                "DAP_Info(0x{id:02X}) 响应格式不对：{}",
                vendor::hex(&resp)
            )));
        }
        let len = resp[1] as usize;
        if resp.len() < 2 + len {
            return Err(Error::protocol(format!(
                "DAP_Info(0x{id:02X}) 响应太短：{}",
                vendor::hex(&resp)
            )));
        }
        Ok(resp[2..2 + len].to_vec())
    }

    /// DAP_Info 的字符串项；探针没有这一项时返回 None。
    pub fn dap_info_string(&mut self, id: u8) -> Result<Option<String>> {
        let data = self.dap_info(id)?;
        if data.is_empty() {
            return Ok(None);
        }
        let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
        Ok(Some(String::from_utf8_lossy(&data[..end]).into_owned()))
    }

    /// 查询 DAP 包长，并告诉传输层（USB 的 IN 传输长度依赖它）。
    pub fn query_packet_size(&mut self) -> Result<usize> {
        let data = self.dap_info(ID_PACKET_SIZE)?;
        if data.len() < 2 {
            return Err(Error::protocol("DAP_Info(包长) 响应太短"));
        }
        let size = u16::from_le_bytes([data[0], data[1]]) as usize;
        if size < 64 {
            return Err(Error::protocol(format!("探针报告的包长 {size} 不合理")));
        }
        self.packet_size = size;
        self.t.set_packet_size(size);
        Ok(size)
    }

    // -------------------------------------------------------------- SWD

    fn simple(&mut self, request: &[u8], what: &str) -> Result<Vec<u8>> {
        let resp = self.xfer(request)?;
        if resp.first() != Some(&request[0]) {
            return Err(Error::protocol(format!(
                "{what} 的响应格式不对：{}",
                vendor::hex(&resp)
            )));
        }
        Ok(resp)
    }

    fn expect_ok(&mut self, request: &[u8], what: &str) -> Result<()> {
        let resp = self.simple(request, what)?;
        if resp.get(1) != Some(&0x00) {
            return Err(Error::protocol(format!(
                "{what} 失败：{}",
                vendor::hex(&resp)
            )));
        }
        Ok(())
    }

    /// 一次 DAP_Transfer，访问一个 DP 寄存器；读时返回值。
    fn dp_transfer(&mut self, request: u8, write: Option<u32>, what: &str) -> Result<u32> {
        let mut req = vec![0x05, 0x00, 0x01, request];
        if let Some(v) = write {
            req.extend_from_slice(&v.to_le_bytes());
        }
        let resp = self.simple(&req, what)?;
        if resp.len() < 3 {
            return Err(Error::protocol(format!(
                "{what} 的响应太短：{}",
                vendor::hex(&resp)
            )));
        }
        let ack = resp[2] & 0x07;
        if resp[1] != 1 || ack != 1 {
            let reason = match ack {
                2 => "WAIT",
                4 => "FAULT",
                7 => "没有应答（检查接线、目标供电）",
                _ => "错误",
            };
            return Err(Error::Io(format!(
                "{what} 失败：SWD 应答 {reason}（响应 {}）",
                vendor::hex(&resp)
            )));
        }
        if write.is_some() {
            return Ok(0);
        }
        if resp.len() < 7 {
            return Err(Error::protocol(format!(
                "{what} 的响应太短：{}",
                vendor::hex(&resp)
            )));
        }
        Ok(u32::from_le_bytes(resp[3..7].try_into().unwrap()))
    }

    /// 让探针停止采样。采样期间探针只接受 DAP_Info、DAP_HostStatus 和厂商命令。
    pub fn stop_sampling(&mut self) -> Result<()> {
        let resp = self.xfer(&vendor::sub_ctrl_request(vendor::SUB_STOP))?;
        match vendor::parse_sub_ctrl(&resp)? {
            vendor::STATUS_OK => Ok(()),
            status => Err(Error::Status {
                cmd: vendor::CMD_SUB_CTRL,
                status,
            }),
        }
    }

    /// SWD 连接和 ADIv5 初始化：DAP_Connect(SWD)、设时钟、线复位加 JTAG 转 SWD、
    /// 读 DPIDR、清粘滞错误、调试域和系统域上电。之后厂商命令才能访问目标。
    pub fn connect_swd(&mut self, clock_hz: u32) -> Result<TargetInfo> {
        self.query_packet_size()?;
        let resp = self.xfer(&[0x02, 0x01])?;
        let resp = if resp == [0xFF] {
            // 上一个会话留下的采样还在运行
            self.stop_sampling()?;
            self.xfer(&[0x02, 0x01])?
        } else {
            resp
        };
        if resp.len() < 2 || resp[0] != 0x02 || resp[1] != 0x01 {
            return Err(Error::protocol(format!(
                "DAP_Connect(SWD) 失败：{}",
                vendor::hex(&resp)
            )));
        }
        let mut req = vec![0x11];
        req.extend_from_slice(&clock_hz.to_le_bytes());
        self.expect_ok(&req, "DAP_SWJ_Clock")?;
        // 空闲 0 周期，WAIT 重试 100 次，匹配重试 0 次
        self.expect_ok(&[0x04, 0x00, 100, 0x00, 0x00, 0x00], "DAP_TransferConfigure")?;
        self.expect_ok(&[0x13, 0x00], "DAP_SWD_Configure")?;
        let mut seq = vec![0x12, 128];
        seq.extend_from_slice(&JTAG_TO_SWD);
        self.expect_ok(&seq, "DAP_SWJ_Sequence")?;
        self.expect_ok(&[0x12, 8, 0x00], "DAP_SWJ_Sequence")?;
        // 线复位后第一个 DP 访问必须是读 DPIDR
        let dpidr = self.dp_transfer(DP_READ_DPIDR, None, "读 DPIDR")?;
        let mut abort = vec![0x08, 0x00];
        abort.extend_from_slice(&0x1Eu32.to_le_bytes());
        self.expect_ok(&abort, "DAP_WriteABORT")?;
        self.dp_transfer(DP_WRITE_SELECT, Some(0), "写 DP SELECT")?;
        self.dp_transfer(
            DP_WRITE_CTRL_STAT,
            Some(CSYSPWRUPREQ | CDBGPWRUPREQ),
            "写 CTRL/STAT",
        )?;
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let cs = self.dp_transfer(DP_READ_CTRL_STAT, None, "读 CTRL/STAT")?;
            if cs & (CSYSPWRUPACK | CDBGPWRUPACK) == CSYSPWRUPACK | CDBGPWRUPACK {
                break;
            }
            if Instant::now() > deadline {
                return Err(Error::Io(format!(
                    "调试域上电没有应答（CTRL/STAT 0x{cs:08X}）"
                )));
            }
        }
        Ok(TargetInfo { dpidr })
    }

    // -------------------------------------------------------------- 厂商命令

    pub fn vendor_info(&mut self) -> Result<VendorInfo> {
        let resp = self.xfer(&vendor::info_request())?;
        vendor::parse_info(&resp)
    }

    /// 把条目分成多个 READ：每个最多 32 个条目，响应放得进一个包。
    fn chunks<T>(&self, items: &[T], resp_len: impl Fn(&[T]) -> usize) -> Vec<std::ops::Range<usize>> {
        let mut out = Vec::new();
        let mut start = 0;
        while start < items.len() {
            let mut end = start + 1;
            while end < items.len()
                && end - start < 32
                && resp_len(&items[start..end + 1]) <= self.packet_size
            {
                end += 1;
            }
            out.push(start..end);
            start = end;
        }
        out
    }

    /// 读一组条目。超过一个 READ 的容量时自动分成多次。
    pub fn read(&mut self, items: &[Item]) -> Result<Vec<ItemResult>> {
        let mut out = Vec::with_capacity(items.len());
        let len = |p: &[Item]| {
            let req = 2 + p.iter().map(Item::encoded_len).sum::<usize>();
            req.max(vendor::read_response_len(p))
        };
        for r in self.chunks(items, len) {
            let part = &items[r];
            let resp = self.xfer(&vendor::read_request(part))?;
            out.extend(vendor::parse_read(&resp, part)?);
        }
        Ok(out)
    }

    /// 写一组条目，value 为 size 字节的小端值。
    pub fn write(&mut self, items: &[(Item, Vec<u8>)]) -> Result<Vec<ItemResult>> {
        let mut out = Vec::with_capacity(items.len());
        let packet = self.packet_size;
        let req_len = |p: &[(Item, Vec<u8>)]| {
            2 + p.iter().map(|(i, v)| i.encoded_len() + v.len()).sum::<usize>()
        };
        for r in self.chunks(items, |p| req_len(p).max(3 + 3 * p.len())) {
            let part = &items[r];
            if req_len(part) > packet {
                return Err(Error::usage("一个写入条目放不进一个 DAP 包"));
            }
            let resp = self.xfer(&vendor::write_request(part))?;
            out.extend(vendor::parse_write(&resp, part)?);
        }
        Ok(out)
    }

    pub fn sub_set(&mut self, period_us: u32, flags: u8, items: &[SubItem]) -> Result<SubSetResult> {
        let resp = self.xfer(&vendor::sub_set_request(period_us, flags, items))?;
        vendor::parse_sub_set(&resp)
    }

    pub fn sub_ctrl(&mut self, op: u8) -> Result<u8> {
        let resp = self.xfer(&vendor::sub_ctrl_request(op))?;
        vendor::parse_sub_ctrl(&resp)
    }

    /// SUB_READ，同时返回请求前后的本机时间。
    pub fn sub_read(&mut self, max_bytes: u16) -> Result<(SubRead, Instant, Instant)> {
        let (resp, before, after) = self.xfer_timed(&vendor::sub_read_request(max_bytes))?;
        Ok((vendor::parse_sub_read(&resp)?, before, after))
    }
}
