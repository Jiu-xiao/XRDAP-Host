//! XR 厂商扩展命令 0x80 到 0x85 的请求编码和响应解码。字节格式见 docs/protocol.md 第 3 节。

use crate::{Error, Result};

pub const CMD_INFO: u8 = 0x80;
pub const CMD_READ: u8 = 0x81;
pub const CMD_WRITE: u8 = 0x82;
pub const CMD_SUB_SET: u8 = 0x83;
pub const CMD_SUB_CTRL: u8 = 0x84;
pub const CMD_SUB_READ: u8 = 0x85;

pub const STATUS_OK: u8 = 0x00;
pub const STATUS_ARG: u8 = 0x01;
pub const STATUS_STATE: u8 = 0x02;
pub const STATUS_RESOURCE: u8 = 0x03;
pub const STATUS_NO_TARGET: u8 = 0x04;

pub const SUB_STOP: u8 = 0;
pub const SUB_START: u8 = 1;
pub const SUB_CLEAR: u8 = 2;

pub const MAX_DEPTH: usize = 8;
/// SUB_READ 响应头的字节数。
pub const SUB_READ_HEADER: usize = 24;
/// 帧头：节拍序号 4 字节 + 时间戳 6 字节。
pub const FRAME_HEADER: usize = 10;

/// 整体状态的说明。
pub fn status_text(status: u8) -> &'static str {
    match status {
        STATUS_OK => "成功",
        STATUS_ARG => "参数非法",
        STATUS_STATE => "当前状态不允许（采样中、没有成功的 SUB_SET，或 SWD 链路忙）",
        STATUS_RESOURCE => "资源不足（条目太多、响应放不进一个包，或周期太短）",
        STATUS_NO_TARGET => "没有连接目标（没有 DAP_Connect，或读 AP 的 CSW 失败）",
        _ => "未定义的状态",
    }
}

/// 条目状态的说明。`chain` 是各级指针的表达式（第 k 个对应第 k 级），可以为空。
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub ap: u8,
    pub id: u16,
    pub size: u8,
    pub base: u32,
    pub derefs: Vec<i16>,
    pub final_off: i16,
}

impl Item {
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

    pub fn encoded_len(&self) -> usize {
        11 + 2 * self.derefs.len()
    }

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

    /// 本地检查：size 和解引用次数。
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubItem {
    pub item: Item,
    pub div: u16,
    pub phase: u16,
}

impl SubItem {
    /// 第 k 拍是否采这个条目。
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

/// 整体状态不为 0 时转成错误。
fn require_ok(resp: &[u8], cmd: u8) -> Result<()> {
    match check_header(resp, cmd)? {
        STATUS_OK => Ok(()),
        status => Err(Error::Status { cmd, status }),
    }
}

pub fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn le_u16(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn le_u32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

fn le_u48(d: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b[..6].copy_from_slice(&d[at..at + 6]);
    u64::from_le_bytes(b)
}

fn le_u64(d: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(d[at..at + 8].try_into().unwrap())
}

// ------------------------------------------------------------------ INFO

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorInfo {
    pub version: u8,
    pub features: u32,
    pub max_items: u16,
    pub max_depth: u8,
    pub ring_bytes: u32,
    pub probe_time_us: u64,
    pub access_us: u32,
}

impl VendorInfo {
    pub fn feature_names(&self) -> Vec<&'static str> {
        let names = ["READ", "WRITE", "采样", "8 字节访问"];
        (0..4)
            .filter(|i| self.features & (1 << i) != 0)
            .map(|i| names[i])
            .collect()
    }
}

pub fn info_request() -> Vec<u8> {
    vec![CMD_INFO]
}

pub fn parse_info(resp: &[u8]) -> Result<VendorInfo> {
    require_ok(resp, CMD_INFO)?;
    if resp.len() < 26 {
        return Err(Error::protocol(format!("INFO 响应太短：{}", hex(resp))));
    }
    Ok(VendorInfo {
        version: resp[2],
        features: le_u32(resp, 3),
        max_items: le_u16(resp, 7),
        max_depth: resp[9],
        ring_bytes: le_u32(resp, 10),
        probe_time_us: le_u64(resp, 14),
        access_us: le_u32(resp, 22),
    })
}

// ------------------------------------------------------------------ READ / WRITE

/// READ、WRITE 每个条目的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemResult {
    pub id: u16,
    pub status: u8,
    /// READ 读到的值（size 字节）；WRITE 为空。
    pub value: Vec<u8>,
}

pub fn read_request(items: &[Item]) -> Vec<u8> {
    let mut out = vec![CMD_READ, items.len() as u8];
    for it in items {
        it.encode(&mut out);
    }
    out
}

/// READ 响应的字节数。
pub fn read_response_len(items: &[Item]) -> usize {
    3 + items.iter().map(|i| 3 + i.size as usize).sum::<usize>()
}

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

pub fn write_request(items: &[(Item, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![CMD_WRITE, items.len() as u8];
    for (it, value) in items {
        it.encode(&mut out);
        out.extend_from_slice(value);
    }
    out
}

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

pub fn sub_set_request(period_us: u32, flags: u8, items: &[SubItem]) -> Vec<u8> {
    let mut out = vec![CMD_SUB_SET];
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

/// SUB_SET 的结果。status 为 0x03 时 max_frame_bytes、est_frame_us 照样有效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubSetResult {
    pub status: u8,
    pub max_frame_bytes: Option<u16>,
    pub est_frame_us: Option<u32>,
}

pub fn parse_sub_set(resp: &[u8]) -> Result<SubSetResult> {
    let status = check_header(resp, CMD_SUB_SET)?;
    if resp.len() >= 8 {
        Ok(SubSetResult {
            status,
            max_frame_bytes: Some(le_u16(resp, 2)),
            est_frame_us: Some(le_u32(resp, 4)),
        })
    } else {
        Ok(SubSetResult {
            status,
            max_frame_bytes: None,
            est_frame_us: None,
        })
    }
}

pub fn sub_ctrl_request(op: u8) -> Vec<u8> {
    vec![CMD_SUB_CTRL, op]
}

/// SUB_CTRL 返回整体状态。
pub fn parse_sub_ctrl(resp: &[u8]) -> Result<u8> {
    check_header(resp, CMD_SUB_CTRL)
}

pub fn sub_read_request(max_bytes: u16) -> Vec<u8> {
    let mut out = vec![CMD_SUB_READ];
    out.extend_from_slice(&max_bytes.to_le_bytes());
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubRead {
    pub probe_time_us: u64,
    pub remaining: u32,
    pub dropped: u32,
    pub skipped: u32,
    pub frame_count: u16,
    pub frames: Vec<u8>,
}

pub fn parse_sub_read(resp: &[u8]) -> Result<SubRead> {
    require_ok(resp, CMD_SUB_READ)?;
    if resp.len() < SUB_READ_HEADER {
        return Err(Error::protocol(format!("SUB_READ 响应太短：{}", hex(resp))));
    }
    Ok(SubRead {
        probe_time_us: le_u64(resp, 2),
        remaining: le_u32(resp, 10),
        dropped: le_u32(resp, 14),
        skipped: le_u32(resp, 18),
        frame_count: le_u16(resp, 22),
        frames: resp[SUB_READ_HEADER..].to_vec(),
    })
}

/// 一帧采样。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// 节拍序号（帧里的 32 位值）。
    pub tick: u32,
    /// 探针时间，µs（48 位）。
    pub timestamp_us: u64,
    /// 到期的条目：(在 SUB_SET 里的下标, 值)。失败时值为 None。
    pub values: Vec<(usize, Option<Vec<u8>>)>,
}

/// 按 SUB_SET 的条目表解码 SUB_READ 返回的帧。到期的条目由节拍序号和 div、phase 推算。
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

/// 所有条目都到期时一帧的字节数（与探针 SUB_SET 返回的 max_frame_bytes 相同）。
pub fn max_frame_bytes(items: &[SubItem]) -> usize {
    FRAME_HEADER
        + items.len().div_ceil(8)
        + items.iter().map(|s| s.item.size as usize).sum::<usize>()
}

/// 平均每拍写进环形缓冲的字节数（含每帧 2 字节长度），按 div 折算。
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

    #[test]
    fn info_example() {
        let info = parse_info(&unhex(
            "80 00 01 0F000000 2000 08 00400000 40420F0000000000 32000000",
        ))
        .unwrap();
        assert_eq!(
            info,
            VendorInfo {
                version: 1,
                features: 0x0F,
                max_items: 32,
                max_depth: 8,
                ring_bytes: 16384,
                probe_time_us: 1_000_000,
                access_us: 50,
            }
        );
        assert_eq!(info.feature_names().len(), 4);
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

    #[test]
    fn sub_set_example() {
        let items = doc_sub_items();
        let mut expect = unhex("83 E8030000 00 0200");
        expect.extend(unhex("00 1000 04 00010020 00 0000 0100 0000"));
        expect.extend(unhex("00 1100 02 04010020 00 0000 0A00 0300"));
        assert_eq!(sub_set_request(1000, 0, &items), expect);
        assert_eq!(max_frame_bytes(&items), 17);
        let ok = parse_sub_set(&unhex("83 00 1100 78000000")).unwrap();
        assert_eq!(
            ok,
            SubSetResult {
                status: 0,
                max_frame_bytes: Some(17),
                est_frame_us: Some(120)
            }
        );
        let rejected = parse_sub_set(&unhex("83 03 1100 78000000")).unwrap();
        assert_eq!(rejected.status, STATUS_RESOURCE);
        assert_eq!(rejected.est_frame_us, Some(120));
        let no_target = parse_sub_set(&unhex("83 04")).unwrap();
        assert_eq!(no_target.status, STATUS_NO_TARGET);
        assert_eq!(no_target.est_frame_us, None);
        let due: Vec<u64> = (0..30).filter(|&k| items[1].due(k)).collect();
        assert_eq!(due, vec![7, 17, 27]);
    }

    #[test]
    fn sub_ctrl_and_ff() {
        assert_eq!(sub_ctrl_request(SUB_START), unhex("84 01"));
        assert_eq!(parse_sub_ctrl(&unhex("84 00")).unwrap(), 0);
        assert!(parse_sub_ctrl(&[0xFF]).is_err());
        assert_eq!(sub_read_request(1024), unhex("85 0004"));
    }

    #[test]
    fn sub_read_example_decodes() {
        let resp = unhex(
            "85 00 80610F0000000000 00000000 00000000 00000000 0200
             06000000 B0590F000000 00 07000000
             07000000 985D0F000000 00 08000000 3412",
        );
        let r = parse_sub_read(&resp).unwrap();
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
