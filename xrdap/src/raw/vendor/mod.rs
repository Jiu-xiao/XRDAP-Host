//! XR 厂商扩展命令的请求编码和响应解码：0x80 到 0x85 在这里，0x86（Wi-Fi 配置）在
//! [`wifi`]。字节格式见 XRDAP-Core 的协议文档第 3 节：
//! <https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.md>
//!
//! Request encoding and response decoding of the XR vendor commands: 0x80 to 0x85 here,
//! 0x86 (Wi-Fi configuration) in [`wifi`]. Byte formats: section 3 of the XRDAP-Core protocol
//! document: <https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.md>

pub mod wifi;

use crate::bytes::{le_u16, le_u32, le_u48, le_u64};
use crate::{Error, Result};

/// 命令字节：INFO。
///
/// Command byte: INFO.
pub const CMD_INFO: u8 = 0x80;
/// 命令字节：READ。
///
/// Command byte: READ.
pub const CMD_READ: u8 = 0x81;
/// 命令字节：WRITE。
///
/// Command byte: WRITE.
pub const CMD_WRITE: u8 = 0x82;
/// 命令字节：SUB_START（协议版本 2，一条命令带全部配置并开始采样）。
///
/// Command byte: SUB_START (protocol version 2; carries the whole configuration and starts).
pub const CMD_SUB_START: u8 = 0x83;
/// 命令字节：SUB_CTRL。
///
/// Command byte: SUB_CTRL.
pub const CMD_SUB_CTRL: u8 = 0x84;
/// 命令字节：SUB_READ。
///
/// Command byte: SUB_READ.
pub const CMD_SUB_READ: u8 = 0x85;

/// 整体状态：成功。
///
/// Overall status: success.
pub const STATUS_OK: u8 = 0x00;
/// 整体状态：参数非法。
///
/// Overall status: invalid argument.
pub const STATUS_ARG: u8 = 0x01;
/// 整体状态：当前状态不允许该命令。
///
/// Overall status: the command is not allowed in the current state.
pub const STATUS_STATE: u8 = 0x02;
/// 整体状态：资源不足。
///
/// Overall status: insufficient resources.
pub const STATUS_RESOURCE: u8 = 0x03;
/// 整体状态：没有连接目标。
///
/// Overall status: no target connected.
pub const STATUS_NO_TARGET: u8 = 0x04;
/// 整体状态：另一个传输是会话主人（协议文档 3.1 节）。
///
/// Overall status: another transport owns the probe (protocol section 3.1).
pub const STATUS_BUSY: u8 = 0x05;

/// 本库实现的厂商扩展协议版本；INFO 报告别的版本时不使用扩展。
///
/// The vendor extension protocol version this library implements; with any other version
/// reported by INFO the extension is not used.
pub const PROTOCOL_VERSION: u8 = 2;

/// 探针被另一条连接占用时给用户看的说明。
///
/// The message shown when another connection owns the probe.
pub const BUSY_TEXT: &str = "探针正被另一条连接使用";

/// SUB_CTRL 的 op：停止采样。
///
/// SUB_CTRL op: stop sampling.
pub const SUB_STOP: u8 = 0;

/// 一次请求、一个订阅最多的条目数（协议文档 3.4 节 INFO）。
///
/// Maximum number of items in one request or subscription (protocol document, section 3.4,
/// INFO).
pub const MAX_ITEMS: usize = 32;
/// 一个条目最多经过的指针级数（协议文档 3.3 节）。
///
/// Maximum number of pointer levels one item may pass through (protocol document, section 3.3).
pub const MAX_DEPTH: usize = 8;
/// SUB_READ 响应头的字节数。
///
/// Size in bytes of the SUB_READ response header.
pub const SUB_READ_HEADER: usize = 24;
/// 帧头：节拍序号 4 字节 + 时间戳 6 字节。
///
/// Frame header: 4 bytes of tick number plus 6 bytes of timestamp.
pub const FRAME_HEADER: usize = 10;

/// 0x80 到 0x85 的整体状态的说明。
///
/// Meaning of an overall status of commands 0x80 to 0x85.
pub fn status_text(status: u8) -> &'static str {
    match status {
        STATUS_OK => "成功",
        STATUS_ARG => "参数非法",
        STATUS_STATE => "当前状态不允许（已经在采样）",
        STATUS_RESOURCE => {
            "资源不足（条目超过 32 个、回复放不进剩余空间、用到超过 8 个 AP，或所有条目都到期的一帧放不进采样缓冲）"
        }
        STATUS_NO_TARGET => "没有连接目标（这次会话还没有 DAP_Connect，或读 AP 的 CSW 失败）",
        STATUS_BUSY => BUSY_TEXT,
        _ => "未定义的状态",
    }
}

/// 条目状态的说明。`chain` 是各级指针的表达式（第 k 个对应第 k 级），可以为空。
///
/// Meaning of an item status. `chain` holds the expression of each pointer level (the k-th
/// entry belongs to level k) and may be empty.
pub fn item_status_text(status: u8, chain: &[String]) -> String {
    let level = |k: u8| -> String {
        match chain.get(k as usize) {
            Some(name) => format!("第 {} 级指针 {name} ", k + 1),
            None => format!("第 {} 级指针", k + 1),
        }
    };
    match status {
        0x00 => "成功".into(),
        0x10..=0x17 => format!(
            "读{}时 SWD 出错（FAULT 或超时），地址可能无效",
            level(status - 0x10)
        ),
        0x1F => "读写变量时 SWD 出错（FAULT、超时或粘滞错误），地址可能无效".into(),
        0x20..=0x27 => format!("{}所在的地址没有 4 字节对齐", level(status - 0x20)),
        0x2F => "变量地址没有按访问宽度对齐".into(),
        0x30..=0x37 => format!("{}为空（NULL）", level(status - 0x30)),
        other => format!("未定义的条目状态 0x{other:02X}"),
    }
}

/// 一个条目：变量的位置，最多经过 8 级指针。
///
/// One item: the location of a variable, passing through at most 8 pointer levels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// MEM-AP 编号。
    ///
    /// MEM-AP number.
    pub ap: u8,
    /// 调用方自定的编号，响应里原样带回。
    ///
    /// Caller-defined identifier, returned unchanged in the response.
    pub id: u16,
    /// 访问宽度，字节：1、2、4 或 8。
    ///
    /// Access width in bytes: 1, 2, 4 or 8.
    pub size: u8,
    /// 起始地址。
    ///
    /// Start address.
    pub base: u32,
    /// 每一级指针的偏移：读出该级地址上的指针，作为下一级的起点。
    ///
    /// Offset of each pointer level: the pointer read at that address is the start of the next
    /// level.
    pub derefs: Vec<i16>,
    /// 最后一级指针之后的偏移，得到变量地址。
    ///
    /// Offset applied after the last pointer level, giving the variable address.
    pub final_off: i16,
}

impl Item {
    /// 不经过指针、直接访问 `addr` 的条目（AP 0）。
    ///
    /// An item that accesses `addr` directly, without pointers (AP 0).
    pub fn direct(id: u16, size: u8, addr: u32) -> Self {
        Self {
            ap: 0,
            id,
            size,
            base: addr,
            derefs: Vec::new(),
            final_off: 0,
        }
    }

    /// 条目编码后的字节数。
    ///
    /// Encoded length of the item in bytes.
    pub fn encoded_len(&self) -> usize {
        11 + 2 * self.derefs.len()
    }

    /// 把条目按协议格式追加到 `out`。
    ///
    /// Append the item to `out` in the protocol format.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.ap);
        out.extend_from_slice(&self.id.to_le_bytes());
        out.push(self.size);
        out.extend_from_slice(&self.base.to_le_bytes());
        out.push(self.derefs.len() as u8);
        for d in &self.derefs {
            out.extend_from_slice(&d.to_le_bytes());
        }
        out.extend_from_slice(&self.final_off.to_le_bytes());
    }

    /// 本地检查：size 和解引用次数（协议文档 3.3 节的取值范围）。
    ///
    /// Local check of size and dereference count against the ranges of the protocol document,
    /// section 3.3.
    pub fn check(&self) -> Result<()> {
        if !matches!(self.size, 1 | 2 | 4 | 8) {
            return Err(Error::usage(format!("访问宽度 {} 不是 1/2/4/8", self.size)));
        }
        if self.derefs.len() > MAX_DEPTH {
            return Err(Error::usage(format!(
                "要经过 {} 级指针，探针最多支持 {MAX_DEPTH} 级",
                self.derefs.len()
            )));
        }
        Ok(())
    }
}

/// 采样条目：条目加分频和相位。
///
/// A sampled item: an item plus its divider and phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubItem {
    /// 被采样的条目。
    ///
    /// The sampled item.
    pub item: Item,
    /// 分频：每 `div` 拍采一次，1 表示每拍都采。
    ///
    /// Divider: sampled once every `div` ticks; 1 samples on every tick.
    pub div: u16,
    /// 相位：第 k 拍满足 `(k + phase) % div == 0` 时采样。
    ///
    /// Phase: tick k is sampled when `(k + phase) % div == 0`.
    pub phase: u16,
}

impl SubItem {
    /// 第 k 拍是否采这个条目。
    ///
    /// Whether this item is sampled on tick k.
    pub fn due(&self, k: u64) -> bool {
        (k + self.phase as u64).is_multiple_of(self.div as u64)
    }
}

fn check_header(resp: &[u8], cmd: u8) -> Result<u8> {
    if resp.len() == 1 && resp[0] == 0xFF {
        return Err(Error::protocol(format!(
            "探针不支持命令 0x{cmd:02X}（回了 FF）：固件没有厂商扩展，或正在采样"
        )));
    }
    if resp.len() < 2 || resp[0] != cmd {
        return Err(Error::protocol(format!(
            "命令 0x{cmd:02X} 的响应格式不对：{}",
            hex(resp)
        )));
    }
    Ok(resp[1])
}

/// 非 0 的整体状态对应的错误；另一条连接占用探针时是 [`Error::Busy`]。
fn status_error(cmd: u8, status: u8) -> Error {
    match status {
        STATUS_BUSY => Error::Busy(BUSY_TEXT.into()),
        status => Error::Status {
            cmd,
            status,
            text: status_text(status),
        },
    }
}

/// 整体状态不为 0 时转成错误。
fn require_ok(resp: &[u8], cmd: u8) -> Result<()> {
    match check_header(resp, cmd)? {
        STATUS_OK => Ok(()),
        status => Err(status_error(cmd, status)),
    }
}

/// 把字节格式化成以空格分隔的大写十六进制，例如 `80 01`。
///
/// Format bytes as space-separated upper-case hexadecimal, e.g. `80 01`.
pub fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// ------------------------------------------------------------------ INFO

/// 0x80 INFO 的响应：探针的厂商扩展能力。
///
/// The 0x80 INFO response: the probe's vendor-extension capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorInfo {
    /// 厂商扩展的协议版本。
    ///
    /// Protocol version of the vendor extension.
    pub version: u8,
    /// 功能位：bit0 READ，bit1 WRITE，bit2 采样，bit3 8 字节访问，bit4 Wi-Fi 配置。
    ///
    /// Feature bits: bit0 READ, bit1 WRITE, bit2 sampling, bit3 8-byte access, bit4 Wi-Fi
    /// configuration.
    pub features: u32,
    /// 一个请求或订阅最多的条目数。
    ///
    /// Maximum number of items in one request or subscription.
    pub max_items: u16,
    /// 最大解引用深度。
    ///
    /// Maximum dereference depth.
    pub max_depth: u8,
    /// 采样环形缓冲的字节数。
    ///
    /// Size of the sampling ring buffer in bytes.
    pub ring_bytes: u32,
    /// 探针当前时间，µs。
    ///
    /// The probe's current time in µs.
    pub probe_time_us: u64,
}

impl VendorInfo {
    /// `features` 中已置位的功能名称（只列 bit0 到 bit3）。
    ///
    /// Names of the feature bits set in `features` (bit0 to bit3 only).
    pub fn feature_names(&self) -> Vec<&'static str> {
        let names = ["READ", "WRITE", "采样", "8 字节访问"];
        (0..4)
            .filter(|i| self.features & (1 << i) != 0)
            .map(|i| names[i])
            .collect()
    }
}

/// INFO 请求包。
///
/// The INFO request packet.
pub fn info_request() -> Vec<u8> {
    vec![CMD_INFO]
}

/// 解码 INFO 响应（协议版本 2 为 22 字节）。版本不是 [`PROTOCOL_VERSION`] 时是错误，
/// 不做兼容。
///
/// Decode an INFO response (22 bytes in protocol version 2). Any version other than
/// [`PROTOCOL_VERSION`] is an error; there is no compatibility.
pub fn parse_info(resp: &[u8]) -> Result<VendorInfo> {
    require_ok(resp, CMD_INFO)?;
    match resp.get(2) {
        Some(&PROTOCOL_VERSION) => {}
        Some(v) => {
            return Err(Error::protocol(format!(
                "探针的厂商扩展是协议版本 {v}，这个版本的 xrdap 需要版本 {PROTOCOL_VERSION}，请升级探针固件"
            )))
        }
        None => return Err(Error::protocol(format!("INFO 响应太短：{}", hex(resp)))),
    }
    if resp.len() < 22 {
        return Err(Error::protocol(format!("INFO 响应太短：{}", hex(resp))));
    }
    Ok(VendorInfo {
        version: resp[2],
        features: le_u32(resp, 3),
        max_items: le_u16(resp, 7),
        max_depth: resp[9],
        ring_bytes: le_u32(resp, 10),
        probe_time_us: le_u64(resp, 14),
    })
}

// ------------------------------------------------------------------ READ / WRITE

/// READ、WRITE 每个条目的结果。
///
/// The result of one item of READ or WRITE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemResult {
    /// 条目的 id。
    ///
    /// The item's id.
    pub id: u16,
    /// 条目状态，0 为成功。
    ///
    /// Item status; 0 is success.
    pub status: u8,
    /// READ 读到的值（size 字节）；WRITE 为空。
    ///
    /// The value read by READ (`size` bytes); empty for WRITE.
    pub value: Vec<u8>,
}

/// READ 请求包。
///
/// The READ request packet.
pub fn read_request(items: &[Item]) -> Vec<u8> {
    let mut out = vec![CMD_READ, items.len() as u8];
    for it in items {
        it.encode(&mut out);
    }
    out
}

/// READ 响应的字节数。
///
/// Length of the READ response in bytes.
pub fn read_response_len(items: &[Item]) -> usize {
    3 + items.iter().map(|i| 3 + i.size as usize).sum::<usize>()
}

/// 按请求的条目表解码 READ 响应。
///
/// Decode a READ response against the items of the request.
pub fn parse_read(resp: &[u8], items: &[Item]) -> Result<Vec<ItemResult>> {
    require_ok(resp, CMD_READ)?;
    if resp.len() < 3 || resp[2] as usize != items.len() {
        return Err(Error::protocol(format!(
            "READ 响应的条目数不对：{}",
            hex(resp)
        )));
    }
    let mut pos = 3;
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let size = it.size as usize;
        if resp.len() < pos + 3 + size {
            return Err(Error::protocol(format!("READ 响应太短：{}", hex(resp))));
        }
        let id = le_u16(resp, pos);
        if id != it.id {
            return Err(Error::protocol(format!(
                "READ 响应的条目 id 是 {id}，应为 {}",
                it.id
            )));
        }
        out.push(ItemResult {
            id,
            status: resp[pos + 2],
            value: resp[pos + 3..pos + 3 + size].to_vec(),
        });
        pos += 3 + size;
    }
    Ok(out)
}

/// WRITE 请求包，每个条目后面跟 size 字节的值。
///
/// The WRITE request packet; each item is followed by `size` bytes of value.
pub fn write_request(items: &[(Item, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![CMD_WRITE, items.len() as u8];
    for (it, value) in items {
        it.encode(&mut out);
        out.extend_from_slice(value);
    }
    out
}

/// 按请求的条目表解码 WRITE 响应。
///
/// Decode a WRITE response against the items of the request.
pub fn parse_write(resp: &[u8], items: &[(Item, Vec<u8>)]) -> Result<Vec<ItemResult>> {
    require_ok(resp, CMD_WRITE)?;
    if resp.len() != 3 + 3 * items.len() || resp[2] as usize != items.len() {
        return Err(Error::protocol(format!(
            "WRITE 响应格式不对：{}",
            hex(resp)
        )));
    }
    Ok(items
        .iter()
        .enumerate()
        .map(|(i, _)| ItemResult {
            id: le_u16(resp, 3 + 3 * i),
            status: resp[5 + 3 * i],
            value: Vec::new(),
        })
        .collect())
}

// ------------------------------------------------------------------ 采样

/// SUB_START 请求包：采样周期、标志和采样条目。探针检查通过后立即开始采样。
///
/// The SUB_START request packet: period, flags and items. The probe starts sampling once the
/// checks pass.
pub fn sub_start_request(period_us: u32, flags: u8, items: &[SubItem]) -> Vec<u8> {
    let mut out = vec![CMD_SUB_START];
    out.extend_from_slice(&period_us.to_le_bytes());
    out.push(flags);
    out.extend_from_slice(&(items.len() as u16).to_le_bytes());
    for s in items {
        s.item.encode(&mut out);
        out.extend_from_slice(&s.div.to_le_bytes());
        out.extend_from_slice(&s.phase.to_le_bytes());
    }
    out
}

/// SUB_START 的响应：整体状态不为 0 时是错误。
///
/// The SUB_START response: a non-zero overall status is an error.
pub fn parse_sub_start(resp: &[u8]) -> Result<()> {
    require_ok(resp, CMD_SUB_START)
}

/// SUB_CTRL 请求包，`op` 见 [`SUB_STOP`]。
///
/// The SUB_CTRL request packet; see [`SUB_STOP`] for `op`.
pub fn sub_ctrl_request(op: u8) -> Vec<u8> {
    vec![CMD_SUB_CTRL, op]
}

/// SUB_CTRL 的响应：整体状态不为 0 时是错误。
///
/// The SUB_CTRL response: a non-zero overall status is an error.
pub fn parse_sub_ctrl(resp: &[u8]) -> Result<()> {
    require_ok(resp, CMD_SUB_CTRL)
}

/// SUB_READ 请求包，`max_bytes` 是帧部分最多返回的字节数。
///
/// The SUB_READ request packet; `max_bytes` bounds the frame bytes returned.
pub fn sub_read_request(max_bytes: u16) -> Vec<u8> {
    let mut out = vec![CMD_SUB_READ];
    out.extend_from_slice(&max_bytes.to_le_bytes());
    out
}

/// 0x85 SUB_READ 的响应。
///
/// The 0x85 SUB_READ response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubRead {
    /// 探针还在采样（整体状态 0x00）；为 false 时采样已经停止（状态 0x02），缓冲里的帧照样返回。
    ///
    /// The probe is still sampling (status 0x00); false when sampling has stopped (status 0x02),
    /// with the buffered frames still returned.
    pub sampling: bool,
    /// 探针当前时间，µs。
    ///
    /// The probe's current time in µs.
    pub probe_time_us: u64,
    /// 缓冲里剩余未取的帧字节数。
    ///
    /// Frame bytes still left in the buffer.
    pub remaining: u32,
    /// 累计缓冲丢帧数。
    ///
    /// Cumulative number of frames dropped by the buffer.
    pub dropped: u32,
    /// 累计跳过的节拍数。
    ///
    /// Cumulative number of skipped ticks.
    pub skipped: u32,
    /// 本次返回的帧数。
    ///
    /// Number of frames returned by this response.
    pub frame_count: u16,
    /// 帧的原始字节，用 [`decode_frames`] 解码。
    ///
    /// Raw frame bytes; decode them with [`decode_frames`].
    pub frames: Vec<u8>,
}

/// 解码 SUB_READ 响应。
///
/// Decode a SUB_READ response.
pub fn parse_sub_read(resp: &[u8]) -> Result<SubRead> {
    // 0x02 表示没在采样，响应照样带完整的头和帧（协议文档 3.4 节）
    let sampling = match check_header(resp, CMD_SUB_READ)? {
        STATUS_OK => true,
        STATUS_STATE => false,
        status => return Err(status_error(CMD_SUB_READ, status)),
    };
    if resp.len() < SUB_READ_HEADER {
        return Err(Error::protocol(format!("SUB_READ 响应太短：{}", hex(resp))));
    }
    Ok(SubRead {
        sampling,
        probe_time_us: le_u64(resp, 2),
        remaining: le_u32(resp, 10),
        dropped: le_u32(resp, 14),
        skipped: le_u32(resp, 18),
        frame_count: le_u16(resp, 22),
        frames: resp[SUB_READ_HEADER..].to_vec(),
    })
}

/// 一帧采样。
///
/// One sampled frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// 节拍序号（帧里的 32 位值）。
    ///
    /// Tick number (the 32-bit value in the frame).
    pub tick: u32,
    /// 探针时间，µs（48 位）。
    ///
    /// Probe time in µs (48 bits).
    pub timestamp_us: u64,
    /// 到期的条目：(在 SUB_START 里的下标, 值)。失败时值为 None。
    ///
    /// The items that were due: (index in SUB_START, value). The value is None on failure.
    pub values: Vec<(usize, Option<Vec<u8>>)>,
}

/// 按 SUB_START 的条目表解码 SUB_READ 返回的帧。到期的条目由节拍序号和 div、phase 推算。
///
/// Decode the frames returned by SUB_READ against the item table of SUB_START. The due items
/// follow from the tick number and each item's div and phase.
pub fn decode_frames(raw: &[u8], items: &[SubItem], count: u16) -> Result<Vec<Frame>> {
    let mut pos = 0;
    let mut frames = Vec::with_capacity(count as usize);
    for n in 0..count {
        if raw.len() < pos + FRAME_HEADER {
            return Err(Error::protocol(format!("第 {n} 帧不完整")));
        }
        let tick = le_u32(raw, pos);
        let timestamp_us = le_u48(raw, pos + 4);
        pos += FRAME_HEADER;
        let due: Vec<usize> = (0..items.len())
            .filter(|&i| items[i].due(tick as u64))
            .collect();
        let bitmap_len = due.len().div_ceil(8);
        let values_len: usize = due.iter().map(|&i| items[i].item.size as usize).sum();
        if due.is_empty() || raw.len() < pos + bitmap_len + values_len {
            return Err(Error::protocol(format!(
                "第 {n} 帧（k={tick}）长度与条目表对不上"
            )));
        }
        let bitmap = &raw[pos..pos + bitmap_len];
        pos += bitmap_len;
        let mut values = Vec::with_capacity(due.len());
        for (j, &i) in due.iter().enumerate() {
            let size = items[i].item.size as usize;
            let failed = bitmap[j / 8] >> (j % 8) & 1 != 0;
            let v = raw[pos..pos + size].to_vec();
            pos += size;
            values.push((i, if failed { None } else { Some(v) }));
        }
        frames.push(Frame {
            tick,
            timestamp_us,
            values,
        });
    }
    if pos != raw.len() {
        return Err(Error::protocol(format!(
            "帧解码后多出 {} 字节",
            raw.len() - pos
        )));
    }
    Ok(frames)
}

/// 所有条目都到期时一帧的字节数（不含 2 字节长度）。
///
/// Bytes of one frame when every item is due (without the 2-byte length).
pub fn max_frame_bytes(items: &[SubItem]) -> usize {
    FRAME_HEADER
        + items.len().div_ceil(8)
        + items.iter().map(|s| s.item.size as usize).sum::<usize>()
}

/// 平均每拍写进环形缓冲的字节数（含每帧 2 字节长度），按各条目到期的概率估算。
///
/// Estimated average bytes written to the ring buffer per tick (including the 2-byte length
/// of each frame), weighted by the probability that each item is due.
pub fn average_bytes_per_tick(items: &[SubItem]) -> f64 {
    // 按 lcm 周期精确统计代价太高，这里按到期概率估算：位图和帧头按"至少一个条目到期"的概率算。
    let mut p_none = 1.0;
    let mut values = 0.0;
    for s in items {
        let p = 1.0 / s.div as f64;
        p_none *= 1.0 - p;
        values += p * s.item.size as f64;
    }
    let p_frame = 1.0 - p_none;
    p_frame * (2.0 + FRAME_HEADER as f64 + 1.0) + values
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// 给没有指定相位的分频条目分配相位，让慢速条目尽量不落在同一拍。
///
/// 条目 i、j 同时到期的拍占比为 `[phase_i ≡ phase_j (mod gcd)] / lcm(div_i, div_j)`，
/// 依次给每个条目选使"与已放好的条目重叠的字节数"最小的相位。
/// `fixed[i]` 为 true 的条目保持原相位。
///
/// Assign phases to divided items that have no phase chosen, so that slow items avoid the
/// same ticks where possible. Items i and j are due together on a fraction
/// `[phase_i ≡ phase_j (mod gcd)] / lcm(div_i, div_j)` of the ticks; each item in turn gets
/// the phase with the smallest overlapping byte count against the items already placed.
/// Items with `fixed[i]` true keep their phase.
pub fn assign_phases(items: &mut [SubItem], fixed: &[bool]) {
    let mut placed: Vec<usize> = (0..items.len())
        .filter(|&i| fixed.get(i).copied().unwrap_or(false) || items[i].div <= 1)
        .collect();
    // 大的、慢的先放
    let mut order: Vec<usize> = (0..items.len()).filter(|i| !placed.contains(i)).collect();
    order.sort_by_key(|&i| std::cmp::Reverse((items[i].div, items[i].item.size)));
    for i in order {
        let d = items[i].div as u64;
        let mut best = (f64::INFINITY, 0u64);
        for p in 0..d {
            let mut cost = 0.0;
            for &j in &placed {
                let dj = items[j].div as u64;
                if dj <= 1 {
                    continue;
                }
                let g = gcd(d, dj);
                if (p + g * 1_000_000 - items[j].phase as u64 % g).is_multiple_of(g) {
                    let lcm = d / g * dj;
                    cost += items[j].item.size as f64 / lcm as f64;
                }
            }
            if cost < best.0 - 1e-12 {
                best = (cost, p);
            }
        }
        items[i].phase = best.1 as u16;
        placed.push(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// 协议文档 3.8 的 INFO 例子（版本 2）；版本 1 的探针不用。
    #[test]
    fn info_example() {
        let info = parse_info(&unhex(
            "80 00 02 1F000000 2000 08 00400000 40420F0000000000",
        ))
        .unwrap();
        assert_eq!(
            info,
            VendorInfo {
                version: 2,
                features: 0x1F,
                max_items: 32,
                max_depth: 8,
                ring_bytes: 16384,
                probe_time_us: 1_000_000,
            }
        );
        assert_eq!(info.feature_names().len(), 4);
        let v1 = parse_info(&unhex(
            "80 00 01 0F000000 2000 08 00400000 40420F0000000000 32000000",
        ))
        .unwrap_err();
        assert!(v1.to_string().contains("协议版本 1"), "{v1}");
    }

    /// 协议文档 3.2 和 3.8：另一个传输占着探针时厂商命令回 `命令 05`。
    #[test]
    fn busy_status() {
        let items = [Item::direct(1, 4, 0x2000_0000)];
        let e = parse_read(&unhex("81 05"), &items).unwrap_err();
        assert!(matches!(&e, Error::Busy(m) if m == BUSY_TEXT), "{e}");
        assert!(matches!(
            parse_read(&unhex("81 03"), &items),
            Err(Error::Status {
                status: STATUS_RESOURCE,
                ..
            })
        ));
    }

    #[test]
    fn read_example() {
        let item = Item {
            ap: 0,
            id: 1,
            size: 4,
            base: 0x2000_0000,
            derefs: vec![8],
            final_off: 4,
        };
        let mut enc = Vec::new();
        item.encode(&mut enc);
        assert_eq!(enc, unhex("00 0100 04 00000020 01 0800 0400"));
        assert_eq!(enc.len(), item.encoded_len());
        let items = [item];
        assert_eq!(
            read_request(&items),
            unhex("81 01 00 0100 04 00000020 01 0800 0400")
        );
        let r = parse_read(&unhex("81 00 01 0100 00 78563412"), &items).unwrap();
        assert_eq!(r[0].status, 0);
        assert_eq!(r[0].value, 0x1234_5678u32.to_le_bytes());
        let r = parse_read(&unhex("81 00 01 0100 30 00000000"), &items).unwrap();
        assert_eq!(r[0].status, 0x30);
        assert!(item_status_text(0x30, &["p".into()]).contains("为空"));
        assert_eq!(read_response_len(&items), 10);
    }

    #[test]
    fn write_example() {
        let item = Item::direct(2, 2, 0x2000_0010);
        let mut enc = Vec::new();
        item.encode(&mut enc);
        assert_eq!(enc, unhex("00 0200 02 10000020 00 0000"));
        let items = [(item, 0x1234u16.to_le_bytes().to_vec())];
        assert_eq!(
            write_request(&items),
            unhex("82 01 00 0200 02 10000020 00 0000 3412")
        );
        let r = parse_write(&unhex("82 00 01 0200 00"), &items).unwrap();
        assert_eq!((r[0].id, r[0].status), (2, 0));
    }

    fn doc_sub_items() -> Vec<SubItem> {
        vec![
            SubItem {
                item: Item::direct(0x10, 4, 0x2000_0100),
                div: 1,
                phase: 0,
            },
            SubItem {
                item: Item::direct(0x11, 2, 0x2000_0104),
                div: 10,
                phase: 3,
            },
        ]
    }

    /// 协议文档 3.8 的 SUB_START 例子。
    #[test]
    fn sub_start_example() {
        let items = doc_sub_items();
        let mut expect = unhex("83 E8030000 00 0200");
        expect.extend(unhex("00 1000 04 00010020 00 0000 0100 0000"));
        expect.extend(unhex("00 1100 02 04010020 00 0000 0A00 0300"));
        assert_eq!(sub_start_request(1000, 0, &items), expect);
        assert_eq!(max_frame_bytes(&items), 17);
        parse_sub_start(&unhex("83 00")).unwrap();
        assert!(matches!(
            parse_sub_start(&unhex("83 02")),
            Err(Error::Status {
                status: STATUS_STATE,
                ..
            })
        ));
        assert!(matches!(
            parse_sub_start(&unhex("83 04")),
            Err(Error::Status {
                status: STATUS_NO_TARGET,
                ..
            })
        ));
        let due: Vec<u64> = (0..30).filter(|&k| items[1].due(k)).collect();
        assert_eq!(due, vec![7, 17, 27]);
    }

    #[test]
    fn sub_ctrl_and_ff() {
        assert_eq!(sub_ctrl_request(SUB_STOP), unhex("84 00"));
        parse_sub_ctrl(&unhex("84 00")).unwrap();
        assert!(matches!(
            parse_sub_ctrl(&unhex("84 02")),
            Err(Error::Status {
                status: STATUS_STATE,
                ..
            })
        ));
        assert!(parse_sub_ctrl(&[0xFF]).is_err());
        assert_eq!(sub_read_request(1024), unhex("85 0004"));
    }

    /// 协议文档 3.4：SUB_READ 回 0x02 表示没在采样，头和帧照样带。
    #[test]
    fn sub_read_after_stop_still_carries_frames() {
        let resp = unhex(
            "85 02 80610F0000000000 00000000 00000000 00000000 0100
             06000000 B0590F000000 00 07000000",
        );
        let r = parse_sub_read(&resp).unwrap();
        assert!(!r.sampling);
        assert_eq!(r.frame_count, 1);
        assert!(parse_sub_read(&unhex("85 05")).is_err());
    }

    #[test]
    fn sub_read_example_decodes() {
        let resp = unhex(
            "85 00 80610F0000000000 00000000 00000000 00000000 0200
             06000000 B0590F000000 00 07000000
             07000000 985D0F000000 00 08000000 3412",
        );
        let r = parse_sub_read(&resp).unwrap();
        assert!(r.sampling);
        assert_eq!(r.probe_time_us, 1_008_000);
        assert_eq!(r.frame_count, 2);
        let items = doc_sub_items();
        let frames = decode_frames(&r.frames, &items, r.frame_count).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].tick, 6);
        assert_eq!(frames[0].timestamp_us, 1_006_000);
        assert_eq!(
            frames[0].values,
            vec![(0, Some(7u32.to_le_bytes().to_vec()))]
        );
        assert_eq!(frames[1].tick, 7);
        assert_eq!(frames[1].timestamp_us, 1_007_000);
        assert_eq!(
            frames[1].values,
            vec![
                (0, Some(8u32.to_le_bytes().to_vec())),
                (1, Some(0x1234u16.to_le_bytes().to_vec()))
            ]
        );
    }

    #[test]
    fn decode_failure_bitmap_and_many_items() {
        // 10 个条目都到期，位图 2 字节；第 0 和第 9 个失败。
        let items: Vec<SubItem> = (0..10)
            .map(|i| SubItem {
                item: Item::direct(i, 1, 0x2000_0000 + i as u32),
                div: 1,
                phase: 0,
            })
            .collect();
        let mut raw = vec![5, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0b0000_0001, 0b0000_0010];
        raw.extend(0u8..10);
        let f = &decode_frames(&raw, &items, 1).unwrap()[0];
        assert_eq!(f.values[0], (0, None));
        assert_eq!(f.values[9], (9, None));
        assert_eq!(f.values[1], (1, Some(vec![1])));
        // 多一个字节要报错
        raw.push(0);
        assert!(decode_frames(&raw, &items, 1).is_err());
    }

    #[test]
    fn phases_spread_divided_items() {
        let mk = |div| SubItem {
            item: Item::direct(0, 4, 0x2000_0000),
            div,
            phase: 0,
        };
        let mut items = vec![mk(1), mk(10), mk(10), mk(10), mk(5), mk(100)];
        assign_phases(&mut items, &[false; 6]);
        let p: Vec<u16> = items.iter().map(|s| s.phase).collect();
        assert_eq!(p[0], 0);
        // 三个 div 10 的条目相位互不相同，div 5 的不和它们同余
        assert!(p[1] != p[2] && p[2] != p[3] && p[1] != p[3]);
        for &q in &p[1..4] {
            assert_ne!(q % 5, p[4] % 5);
        }
        // 每拍最多两个分频条目到期
        for k in 0..100u64 {
            let n = items[1..].iter().filter(|s| s.due(k)).count();
            assert!(n <= 2, "k={k} 有 {n} 个");
        }
        // 固定的相位不变
        let mut items = vec![mk(10), mk(10)];
        items[0].phase = 3;
        assign_phases(&mut items, &[true, false]);
        assert_eq!(items[0].phase, 3);
        assert_ne!(items[1].phase, 3);
    }
}
