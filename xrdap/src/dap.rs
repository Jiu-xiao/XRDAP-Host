//! CMSIS-DAP 标准命令、SWD/ADIv5 的最小初始化，以及厂商命令的封装。
//!
//! CMSIS-DAP standard commands, minimal SWD/ADIv5 setup, and wrappers for the vendor commands.

use std::time::{Duration, Instant};

use crate::bytes::le_u32;
use crate::transport::Transport;
use crate::vendor::wifi::{self, WifiStatus};
use crate::vendor::{self, Item, ItemResult, SubItem, SubRead, SubSetResult, VendorInfo};
use crate::{Error, Result};

/// DAP_Info 项：厂商名。
///
/// DAP_Info item: vendor name.
pub const ID_VENDOR: u8 = 0x01;
/// DAP_Info 项：产品名。
///
/// DAP_Info item: product name.
pub const ID_PRODUCT: u8 = 0x02;
/// DAP_Info 项：序列号。
///
/// DAP_Info item: serial number.
pub const ID_SERIAL: u8 = 0x03;
/// DAP_Info 项：CMSIS-DAP 协议版本。
///
/// DAP_Info item: CMSIS-DAP protocol version.
pub const ID_PROTOCOL: u8 = 0x04;
/// DAP_Info 项：探针固件版本。
///
/// DAP_Info item: probe firmware version.
pub const ID_FIRMWARE: u8 = 0x09;
/// DAP_Info 项：探针可缓存的 DAP 包个数。
///
/// DAP_Info item: number of DAP packets the probe can buffer.
pub const ID_PACKET_COUNT: u8 = 0xFE;
/// DAP_Info 项：DAP 包长。
///
/// DAP_Info item: DAP packet size.
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
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x9E, 0xE7, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
];

/// 解析 SWD 时钟：`4000000`、`4M`、`500k`、`1.5MHz`。范围 1 Hz 到 100 MHz。
///
/// Parse an SWD clock: `4000000`, `4M`, `500k` or `1.5MHz`. The range is 1 Hz to 100 MHz.
pub fn parse_clock(s: &str) -> Result<u32> {
    let t = s.trim().to_ascii_lowercase();
    let t = t.strip_suffix("hz").unwrap_or(&t);
    let (num, scale) = if let Some(n) = t.strip_suffix('m') {
        (n, 1e6)
    } else if let Some(n) = t.strip_suffix('k') {
        (n, 1e3)
    } else {
        (t, 1.0)
    };
    let v: f64 = num
        .trim()
        .parse()
        .map_err(|_| Error::usage(format!("时钟 {s} 写得不对，例如 4M、500k")))?;
    let hz = (v * scale).round();
    if !(1.0..=100e6).contains(&hz) {
        return Err(Error::usage(format!(
            "时钟 {s} 超出范围（1 Hz 到 100 MHz）"
        )));
    }
    Ok(hz as u32)
}

/// 连接目标后得到的信息。
///
/// Information obtained after connecting to the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetInfo {
    /// 目标 DP 的 DPIDR 寄存器值。
    ///
    /// The DPIDR register value of the target's DP.
    pub dpidr: u32,
}

impl TargetInfo {
    /// DPIDR 的可读说明：设计者、DP 版本、部件号。
    ///
    /// A readable description of the DPIDR: designer, DP version and part number.
    pub fn describe(&self) -> String {
        let d = self.dpidr;
        let designer = (d >> 1) & 0x7FF;
        let version = (d >> 12) & 0xF;
        let partno = (d >> 20) & 0xFF;
        let who = if designer == 0x23B {
            "ARM"
        } else {
            "其他厂商"
        };
        format!(
            "DPIDR 0x{d:08X}（{who}，设计者代码 0x{designer:03X}，DPv{version}，部件号 0x{partno:02X}）"
        )
    }
}

/// 探针在 DAP_Info 里报的身份；探针没有报的项为 None。
///
/// The identity a probe reports through DAP_Info; items the probe does not report are None.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    /// 厂商名。
    ///
    /// Vendor name.
    pub vendor: Option<String>,
    /// 产品名。
    ///
    /// Product name.
    pub product: Option<String>,
    /// 序列号，例如 `XRobot-XRDAP-AABBCCDDEEFF`；同一块探针在 USB、串口、TCP 上相同。
    ///
    /// Serial number, e.g. `XRobot-XRDAP-AABBCCDDEEFF`; the same probe reports the same value
    /// over USB, serial and TCP.
    pub serial: Option<String>,
    /// 探针固件版本。
    ///
    /// Probe firmware version.
    pub firmware: Option<String>,
    /// CMSIS-DAP 协议版本。
    ///
    /// CMSIS-DAP protocol version.
    pub protocol: Option<String>,
}

/// DAP_TransferAbort：CMSIS-DAP 规定它没有响应。
///
/// DAP_TransferAbort: the CMSIS-DAP specification defines no response for it.
pub const CMD_TRANSFER_ABORT: u8 = 0x07;
/// 探针不认识的命令回一个字节 `0xFF`（CMSIS-DAP 的 DAP_Invalid）。
///
/// A probe answers a command it does not know with the single byte `0xFF` (DAP_Invalid).
pub const DAP_INVALID: u8 = 0xFF;

/// 这个请求有没有响应。CMSIS-DAP 里只有 DAP_TransferAbort 没有响应。
///
/// Whether a request gets a response. In CMSIS-DAP only DAP_TransferAbort has none.
pub fn expects_response(request: &[u8]) -> bool {
    request.first() != Some(&CMD_TRANSFER_ABORT)
}

/// 超时之后，下一个请求发出前丢弃迟到响应的等待窗口：这段时间里没有再收到包就算清干净了。
const STALE_QUIET: Duration = Duration::from_millis(100);

/// 一个探针连接。请求和响应一一对应：响应的第一个字节回显命令号（CMSIS-DAP 规定），
/// 对不上的包是之前超时的请求迟到的响应，丢掉。
///
/// One probe connection. Requests and responses pair one to one: a response echoes the
/// command ID in its first byte (CMSIS-DAP), so a packet that does not match is a late
/// response to an earlier request that timed out, and is dropped.
pub struct Probe {
    t: Box<dyn Transport>,
    packet_size: usize,
    timeout: Duration,
    /// 上一个请求超时了，链路上可能还有它迟到的响应
    stale: bool,
}

impl Probe {
    /// 在一条传输上建立探针连接，`timeout` 是等一个响应的最长时间。
    ///
    /// Wrap a transport; `timeout` bounds the wait for one response.
    pub fn new(t: Box<dyn Transport>, timeout: Duration) -> Self {
        Self {
            t,
            packet_size: 64,
            timeout,
            stale: false,
        }
    }

    /// 连接描述，例如 `USB XRobot-XRDAP-xxxx`。
    ///
    /// Connection description, e.g. `USB XRobot-XRDAP-xxxx`.
    pub fn describe(&self) -> String {
        self.t.describe()
    }

    /// 探针报告的 DAP 包长（查询过 [`Probe::query_packet_size`] 之后才准确）。
    ///
    /// The probe's DAP packet size (accurate after [`Probe::query_packet_size`]).
    pub fn packet_size(&self) -> usize {
        self.packet_size
    }

    /// 发一个原始 DAP 包，返回它的响应。没有响应的命令（DAP_TransferAbort）发出后立即
    /// 返回空包，不等待。
    ///
    /// Send one raw DAP packet and return its response. A command without a response
    /// (DAP_TransferAbort) returns an empty packet right after sending, without waiting.
    pub fn xfer(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        let Some(&cmd) = request.first() else {
            return Err(Error::usage("DAP 请求不能是空的"));
        };
        if self.stale {
            self.drain();
        }
        self.t.send(request)?;
        if !expects_response(request) {
            return Ok(Vec::new());
        }
        let deadline = Instant::now() + self.timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let resp = match self.t.recv(left) {
                Ok(r) => r,
                Err(e) => {
                    self.stale = matches!(e, Error::Timeout(_));
                    return Err(e);
                }
            };
            if resp.first() == Some(&cmd) || resp == [DAP_INVALID] {
                return Ok(resp);
            }
        }
    }

    /// 丢掉超时请求迟到的响应：一直收，直到 [`STALE_QUIET`] 内没有新包。
    fn drain(&mut self) {
        while self.t.recv(STALE_QUIET).is_ok() {}
        self.stale = false;
    }

    /// 发一个 DAP 包并记下请求前后的本机时间（用来把探针时间对到电脑时间）。
    ///
    /// Send one DAP packet and record the local time before and after (for clock alignment).
    pub fn xfer_timed(&mut self, request: &[u8]) -> Result<(Vec<u8>, Instant, Instant)> {
        let before = Instant::now();
        let resp = self.xfer(request)?;
        let after = Instant::now();
        Ok((resp, before, after))
    }

    /// 反复问 DAP_Info(包长)，直到探针应答，最长 `wait`。刚上电或刚复位的探针要等它启动完。
    ///
    /// Ask DAP_Info(packet size) until the probe answers, for at most `wait`; a probe that
    /// just powered up or reset needs time to boot.
    pub fn wait_ready(&mut self, wait: Duration) -> Result<()> {
        const TRY: Duration = Duration::from_millis(200);
        let normal = std::mem::replace(&mut self.timeout, TRY);
        let deadline = Instant::now() + wait;
        let result = loop {
            match self.dap_info(ID_PACKET_SIZE) {
                Ok(_) => break Ok(()),
                Err(e) if Instant::now() >= deadline => {
                    break Err(Error::Timeout(format!("探针没有应答：{e}")))
                }
                Err(_) => {}
            }
        };
        self.timeout = normal;
        result
    }

    // -------------------------------------------------------------- DAP_Info

    /// DAP_Info 原始数据（去掉命令字节和长度）。
    ///
    /// Raw DAP_Info data, without the command byte and the length byte.
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
    ///
    /// A DAP_Info string item; None when the probe has no such item.
    pub fn dap_info_string(&mut self, id: u8) -> Result<Option<String>> {
        let data = self.dap_info(id)?;
        if data.is_empty() {
            return Ok(None);
        }
        let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
        Ok(Some(String::from_utf8_lossy(&data[..end]).into_owned()))
    }

    /// 读 DAP_Info 里的厂商、产品、序列号和版本。
    ///
    /// Read vendor, product, serial number and versions from DAP_Info.
    pub fn identity(&mut self) -> Result<Identity> {
        Ok(Identity {
            vendor: self.dap_info_string(ID_VENDOR)?,
            product: self.dap_info_string(ID_PRODUCT)?,
            serial: self.dap_info_string(ID_SERIAL)?,
            firmware: self.dap_info_string(ID_FIRMWARE)?,
            protocol: self.dap_info_string(ID_PROTOCOL)?,
        })
    }

    /// 查询 DAP 包长，并告诉传输层（USB 的 IN 传输长度依赖它）。
    ///
    /// Query the DAP packet size and pass it to the transport (USB IN transfers depend on it).
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

    /// 标准命令：响应要回显命令号。CMSIS-DAP 的必备命令回 `FF` 只有一个原因：探针正在采样，
    /// 只接受 DAP_Info 和厂商命令（协议文档 3.1 节）。
    fn simple(&mut self, request: &[u8], what: &str) -> Result<Vec<u8>> {
        let resp = self.xfer(request)?;
        if resp == [DAP_INVALID] {
            return Err(Error::Busy(format!(
                "{what}：探针正在采样，采样期间只接受 DAP_Info 和厂商命令"
            )));
        }
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
            return Err(Error::Target(format!(
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
        Ok(le_u32(&resp, 3))
    }

    /// SWD 连接和 ADIv5 初始化：DAP_Connect(SWD)、设时钟、线复位加 JTAG 转 SWD、
    /// 读 DPIDR、清粘滞错误、调试域和系统域上电。之后厂商命令才能访问目标。
    /// 探针正在采样时返回 [`Error::Busy`]。
    ///
    /// SWD connect and ADIv5 setup: DAP_Connect(SWD), clock, line reset with JTAG-to-SWD,
    /// DPIDR, sticky error clear, debug and system power-up. Vendor commands need it first.
    /// Returns [`Error::Busy`] while the probe is sampling.
    pub fn connect_swd(&mut self, clock_hz: u32) -> Result<TargetInfo> {
        self.query_packet_size()?;
        let resp = self.simple(&[0x02, 0x01], "DAP_Connect")?;
        if resp.get(1) != Some(&0x01) {
            return Err(Error::protocol(format!(
                "DAP_Connect(SWD) 失败：{}",
                vendor::hex(&resp)
            )));
        }
        let mut req = vec![0x11];
        req.extend_from_slice(&clock_hz.to_le_bytes());
        self.expect_ok(&req, "DAP_SWJ_Clock")?;
        // 空闲 0 周期，WAIT 重试 100 次，匹配重试 0 次
        self.expect_ok(
            &[0x04, 0x00, 100, 0x00, 0x00, 0x00],
            "DAP_TransferConfigure",
        )?;
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
                return Err(Error::Target(format!(
                    "调试域上电没有应答（CTRL/STAT 0x{cs:08X}）"
                )));
            }
        }
        Ok(TargetInfo { dpidr })
    }

    // -------------------------------------------------------------- 厂商命令

    /// 0x80 INFO：读探针的厂商命令能力。
    ///
    /// 0x80 INFO: read the probe's vendor command capabilities.
    pub fn vendor_info(&mut self) -> Result<VendorInfo> {
        let resp = self.xfer(&vendor::info_request())?;
        vendor::parse_info(&resp)
    }

    /// 把条目分成多个 READ：每个最多 32 个条目，响应放得进一个包。
    fn chunks<T>(
        &self,
        items: &[T],
        resp_len: impl Fn(&[T]) -> usize,
    ) -> Vec<std::ops::Range<usize>> {
        let mut out = Vec::new();
        let mut start = 0;
        while start < items.len() {
            let mut end = start + 1;
            while end < items.len()
                && end - start < vendor::MAX_ITEMS
                && resp_len(&items[start..end + 1]) <= self.packet_size
            {
                end += 1;
            }
            out.push(start..end);
            start = end;
        }
        out
    }

    /// 0x81 READ：读一组条目。超过一个 READ 的容量时自动分成多次。
    ///
    /// 0x81 READ of a list of items, split into several requests when needed.
    pub fn read(&mut self, items: &[Item]) -> Result<Vec<ItemResult>> {
        items.iter().try_for_each(Item::check)?;
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

    /// 0x82 WRITE：写一组条目，value 为 size 字节的小端值。
    ///
    /// 0x82 WRITE of a list of items; each value is `size` little-endian bytes.
    pub fn write(&mut self, items: &[(Item, Vec<u8>)]) -> Result<Vec<ItemResult>> {
        for (item, value) in items {
            item.check()?;
            if value.len() != item.size as usize {
                return Err(Error::usage(format!(
                    "条目 {} 的值有 {} 字节，访问宽度是 {}",
                    item.id,
                    value.len(),
                    item.size
                )));
            }
        }
        let mut out = Vec::with_capacity(items.len());
        let packet = self.packet_size;
        let req_len = |p: &[(Item, Vec<u8>)]| {
            2 + p
                .iter()
                .map(|(i, v)| i.encoded_len() + v.len())
                .sum::<usize>()
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

    /// 读一个条目的值；条目状态不为 0 时返回 [`Error::Item`]。
    ///
    /// Read one item; a non-zero item status is [`Error::Item`].
    pub fn read_one(&mut self, item: &Item) -> Result<Vec<u8>> {
        let r = self.read(std::slice::from_ref(item))?;
        let r = r
            .into_iter()
            .next()
            .ok_or_else(|| Error::protocol("READ 没有返回结果"))?;
        match r.status {
            vendor::STATUS_OK => Ok(r.value),
            status => Err(item_error(item, status)),
        }
    }

    /// 写一个条目；条目状态不为 0 时返回 [`Error::Item`]。
    ///
    /// Write one item; a non-zero item status is [`Error::Item`].
    pub fn write_one(&mut self, item: &Item, value: &[u8]) -> Result<()> {
        let r = self.write(&[(item.clone(), value.to_vec())])?;
        let r = r
            .into_iter()
            .next()
            .ok_or_else(|| Error::protocol("WRITE 没有返回结果"))?;
        match r.status {
            vendor::STATUS_OK => Ok(()),
            status => Err(item_error(item, status)),
        }
    }

    /// 0x83 SUB_SET：设置采样周期、标志和采样条目。
    ///
    /// 0x83 SUB_SET: set the sampling period, flags and sampled items.
    pub fn sub_set(
        &mut self,
        period_us: u32,
        flags: u8,
        items: &[SubItem],
    ) -> Result<SubSetResult> {
        let resp = self.xfer(&vendor::sub_set_request(period_us, flags, items))?;
        vendor::parse_sub_set(&resp)
    }

    /// 0x84 SUB_CTRL：控制采样；整体状态不为 0 时返回错误。
    ///
    /// 0x84 SUB_CTRL: control sampling; a non-zero overall status is an error.
    pub fn sub_ctrl(&mut self, op: u8) -> Result<()> {
        let resp = self.xfer(&vendor::sub_ctrl_request(op))?;
        vendor::parse_sub_ctrl(&resp)
    }

    /// 0x85 SUB_READ：取走已采样的帧，同时返回请求前后的本机时间。
    ///
    /// 0x85 SUB_READ: fetch sampled frames, together with the local time before and after the
    /// request.
    pub fn sub_read(&mut self, max_bytes: u16) -> Result<(SubRead, Instant, Instant)> {
        let (resp, before, after) = self.xfer_timed(&vendor::sub_read_request(max_bytes))?;
        Ok((vendor::parse_sub_read(&resp)?, before, after))
    }

    /// 0x86：查询 Wi-Fi 状态。
    ///
    /// 0x86: query the Wi-Fi state.
    pub fn wifi_status(&mut self) -> Result<WifiStatus> {
        wifi::parse_status(&self.xfer(&wifi::status_request())?)
    }

    /// 0x86：保存热点参数。成功后探针重启并连接新热点，这个连接随之断开。
    ///
    /// 0x86: save credentials. On success the probe restarts and connects to the new hotspot,
    /// and this connection is lost.
    pub fn wifi_set(&mut self, ssid: &str, password: &str) -> Result<()> {
        wifi::parse_reply(&self.xfer(&wifi::set_request(ssid, password)?)?)
    }

    /// 0x86：清除热点参数。成功后探针重启，不再连接 Wi-Fi。
    ///
    /// 0x86: clear credentials. On success the probe restarts and no longer joins a Wi-Fi network.
    pub fn wifi_clear(&mut self) -> Result<()> {
        wifi::parse_reply(&self.xfer(&wifi::clear_request())?)
    }
}

fn item_error(item: &Item, status: u8) -> Error {
    Error::Item {
        status,
        text: format!(
            "0x{:08X} 处的条目：{}",
            item.base,
            vendor::item_status_text(status, &[])
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::script::{Late, Script};

    fn probe(link: &Script) -> Probe {
        Probe::new(Box::new(link.clone()), Duration::from_secs(2))
    }

    /// CMSIS-DAP：DAP_TransferAbort 没有响应。发出去就返回，不等超时。
    #[test]
    fn transfer_abort_is_sent_without_waiting() {
        let link = Script::default();
        let mut p = probe(&link);
        let t0 = Instant::now();
        assert_eq!(p.xfer(&[CMD_TRANSFER_ABORT]).unwrap(), Vec::<u8>::new());
        assert!(t0.elapsed() < Duration::from_millis(500));
        assert_eq!(link.sent(), vec![vec![CMD_TRANSFER_ABORT]]);
        // 之后的请求照常配对
        assert_eq!(p.xfer(&[0x00, 0xFF]).unwrap(), vec![0x00, 1, 1]);
    }

    /// 超时的请求的响应在之后才到：下一个同样的请求拿到的是它自己的响应。
    #[test]
    fn late_response_after_timeout_is_drained() {
        let link = Script::default();
        let mut p = probe(&link);
        link.delay_next(Late::NextRecv);
        assert!(matches!(p.xfer(&[0x00, 0xFF]), Err(Error::Timeout(_))));
        assert_eq!(p.xfer(&[0x00, 0xFF]).unwrap(), vec![0x00, 1, 1]);
    }

    /// 迟到的响应夹在下一个请求和它的响应之间：命令号对不上，丢掉。
    #[test]
    fn late_response_of_other_command_is_skipped() {
        let link = Script::default();
        let mut p = probe(&link);
        link.delay_next(Late::NextSend);
        assert!(matches!(p.xfer(&[0x00, 0xFF]), Err(Error::Timeout(_))));
        assert_eq!(p.xfer(&[0x80]).unwrap(), vec![0x80, 1, 1]);
    }

    #[test]
    fn clock_strings() {
        assert_eq!(parse_clock("4M").unwrap(), 4_000_000);
        assert_eq!(parse_clock("500k").unwrap(), 500_000);
        assert_eq!(parse_clock("1.5MHz").unwrap(), 1_500_000);
        assert_eq!(parse_clock(" 1000000 ").unwrap(), 1_000_000);
        assert!(parse_clock("fast").is_err());
        assert!(parse_clock("200M").is_err());
    }
}
