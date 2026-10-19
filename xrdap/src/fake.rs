//! 进程内的模拟探针：按固件（firmware/main/dap_vendor.hpp）的规则实现厂商命令和
//! 初始化需要的几条 CMSIS-DAP 命令，目标内存是一块模拟的 Flash 和 RAM。
//! 用于在没有硬件时测试 read、write、watch 的整个流程。
//!
//! 和真探针的区别：时间取电脑的单调时钟；采样在收到请求时按经过的时间补齐帧；
//! SWD 访问耗时按时钟频率估算。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::transport::Transport;
use crate::types::SymbolTable;
use crate::vendor::{Item, SubItem};
use crate::Result;

const PACKET_SIZE: usize = 1024;
const MAX_ITEMS: usize = 32;
const MAX_APS: usize = 8;
const DPIDR: u32 = 0x2BA0_1477;

struct Region {
    start: u32,
    data: Vec<u8>,
    writable: bool,
}

/// 随时间递增的变量：值 = 经过的微秒数 / period_us。
struct Counter {
    addr: u32,
    size: u8,
    period_us: u64,
}

struct SubConfig {
    items: Vec<SubItem>,
    period_us: u32,
    stop_on_full: bool,
}

struct State {
    regions: Vec<Region>,
    counters: Vec<Counter>,
    t0: Instant,
    connected: bool,
    clock_hz: u32,
    ctrl_stat: u32,
    access_us: u32,
    config: Option<SubConfig>,
    active: bool,
    start_us: u64,
    next_k: u64,
    ring: VecDeque<Vec<u8>>,
    ring_used: usize,
    ring_payload: usize,
    ring_bytes: usize,
    dropped: u32,
    skipped: u32,
    skip_every: Option<u64>,
    requests: Vec<Vec<u8>>,
}

/// 模拟探针。可以克隆：克隆出的句柄共享同一个探针，测试里用来检查内存。
#[derive(Clone)]
pub struct FakeProbe(Arc<Mutex<State>>);

impl Default for FakeProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeProbe {
    /// Flash 0x08000000 起 128 KiB（只读），RAM 0x20000000 起 32 KiB，内容全为 0。
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(State {
            regions: vec![
                Region {
                    start: 0x0800_0000,
                    data: vec![0; 128 * 1024],
                    writable: false,
                },
                Region {
                    start: 0x2000_0000,
                    data: vec![0; 32 * 1024],
                    writable: true,
                },
            ],
            counters: Vec::new(),
            t0: Instant::now(),
            connected: false,
            clock_hz: 1_000_000,
            ctrl_stat: 0,
            access_us: 0,
            config: None,
            active: false,
            start_us: 0,
            next_k: 0,
            ring: VecDeque::new(),
            ring_used: 0,
            ring_payload: 0,
            ring_bytes: 16384,
            dropped: 0,
            skipped: 0,
            skip_every: None,
            requests: Vec::new(),
        })))
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    /// 环形缓冲的字节数（默认与固件一样 16384）。
    pub fn set_ring_bytes(&self, n: usize) {
        self.st().ring_bytes = n;
    }

    /// 每 n 拍跳过一拍（模拟 SWD 来不及），用来测试跳过节拍的统计。
    pub fn set_skip_every(&self, n: Option<u64>) {
        self.st().skip_every = n;
    }

    /// 把一段数据写进模拟内存；不在已有区域里时新建一个可写区域。
    pub fn load(&self, addr: u32, data: &[u8]) {
        let mut st = self.st();
        for (i, &b) in data.iter().enumerate() {
            let a = addr.wrapping_add(i as u32);
            if !st.poke(a, b, true) {
                st.regions.push(Region {
                    start: a,
                    data: data[i..].to_vec(),
                    writable: true,
                });
                return;
            }
        }
    }

    pub fn load_u32(&self, addr: u32, v: u32) {
        self.load(addr, &v.to_le_bytes());
    }

    /// 读模拟内存（不经过 SWD），测试用。
    pub fn peek(&self, addr: u32, len: usize) -> Option<Vec<u8>> {
        let st = self.st();
        let now = st.elapsed_us();
        st.read(addr, len, now)
    }

    /// 让 addr 处 size 字节的变量每 period_us 微秒加一。
    pub fn add_counter(&self, addr: u32, size: u8, period_us: u64) {
        self.st().counters.push(Counter {
            addr,
            size,
            period_us,
        });
    }

    /// 收到过的全部请求，测试用。
    pub fn requests(&self) -> Vec<Vec<u8>> {
        self.st().requests.clone()
    }

    pub fn is_sampling(&self) -> bool {
        self.st().active
    }

    /// 按 ELF 的段内容填内存。
    #[cfg(feature = "elf")]
    pub fn load_elf_image(&self, data: &[u8]) -> Result<()> {
        for (addr, bytes) in crate::elf::load_image(data)? {
            self.load(addr as u32, &bytes);
        }
        Ok(())
    }

    /// 按测试板程序（tests/fixtures/g431-test.c）main() 里的初始化填变量，
    /// 并让 tick、cnt_a、cnt64 每 1 ms 加一、cnt_b 每 10 ms 加一。
    /// ELF 里没有这些变量时什么都不做，返回 false。
    pub fn apply_g431_fixture(&self, syms: &SymbolTable) -> bool {
        let addr = |n: &str| {
            syms.lookup_var(n)
                .ok()
                .flatten()
                .map(|v| v.addr as u32)
        };
        let names = [
            "v_u8", "v_u16", "v_u32", "v_u64", "chain_final", "chain_p3", "chain_p2", "chain_p1",
            "null_p1", "bad_p1", "rw_block", "cnt_a", "cnt_b", "tick", "cnt64",
        ];
        let Some(a) = names.iter().map(|n| addr(n)).collect::<Option<Vec<u32>>>() else {
            return false;
        };
        self.load(a[0], &[0xA5]);
        self.load(a[1], &0x1234u16.to_le_bytes());
        self.load_u32(a[2], 0x89AB_CDEF);
        self.load(a[3], &0x0123_4567_89AB_CDEFu64.to_le_bytes());
        self.load_u32(a[4], 0xCAFE_BABE);
        self.load_u32(a[5], a[4]);
        self.load_u32(a[6], a[5]);
        self.load_u32(a[7], a[6]);
        self.load_u32(a[8], 0);
        self.load_u32(a[9], 0xFFFF_F000);
        for i in 0..8u32 {
            self.load_u32(a[10] + 4 * i, 0x1111_1111u32.wrapping_mul(i + 1));
        }
        self.add_counter(a[11], 4, 1000);
        self.add_counter(a[12], 4, 10_000);
        self.add_counter(a[13], 4, 1000);
        self.add_counter(a[14], 8, 1000);
        true
    }
}

impl Transport for FakeProbe {
    fn xfer(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        let mut st = self.st();
        st.requests.push(request.to_vec());
        Ok(st.handle(request))
    }

    fn describe(&self) -> String {
        "模拟探针".into()
    }
}

fn get16(p: &[u8]) -> u16 {
    u16::from_le_bytes([p[0], p[1]])
}

fn get32(p: &[u8]) -> u32 {
    u32::from_le_bytes(p[..4].try_into().unwrap())
}

/// 按固件的格式解析一个条目。
fn parse_item(p: &mut &[u8], sampling: bool) -> Option<(Item, u16, u16)> {
    if p.len() < 9 {
        return None;
    }
    let n = p[8] as usize;
    if n > 8 || p.len() < 9 + 2 * n + 2 {
        return None;
    }
    let item = Item {
        ap: p[0],
        id: get16(&p[1..]),
        size: p[3],
        base: get32(&p[4..]),
        derefs: (0..n).map(|i| get16(&p[9 + 2 * i..]) as i16).collect(),
        final_off: get16(&p[9 + 2 * n..]) as i16,
    };
    *p = &p[11 + 2 * n..];
    let (mut div, mut phase) = (0, 0);
    if sampling {
        if p.len() < 4 {
            return None;
        }
        div = get16(p);
        phase = get16(&p[2..]);
        *p = &p[4..];
    }
    Some((item, div, phase))
}

fn size_valid(s: u8) -> bool {
    matches!(s, 1 | 2 | 4 | 8)
}

impl State {
    fn elapsed_us(&self) -> u64 {
        1_000_000 + self.t0.elapsed().as_micros() as u64
    }

    fn poke(&mut self, addr: u32, b: u8, force: bool) -> bool {
        for r in &mut self.regions {
            if addr >= r.start && ((addr - r.start) as usize) < r.data.len() {
                if !r.writable && !force {
                    return false;
                }
                r.data[(addr - r.start) as usize] = b;
                return true;
            }
        }
        false
    }

    fn byte(&self, addr: u32, now: u64) -> Option<u8> {
        for c in &self.counters {
            if addr >= c.addr && addr < c.addr + c.size as u32 {
                let v = (now - 1_000_000) / c.period_us;
                return Some(v.to_le_bytes()[(addr - c.addr) as usize]);
            }
        }
        self.regions.iter().find_map(|r| {
            (addr >= r.start && ((addr - r.start) as usize) < r.data.len())
                .then(|| r.data[(addr - r.start) as usize])
        })
    }

    fn read(&self, addr: u32, len: usize, now: u64) -> Option<Vec<u8>> {
        (0..len)
            .map(|i| self.byte(addr.wrapping_add(i as u32), now))
            .collect()
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> bool {
        // 先检查整段都可写，避免写一半
        let ok = (0..data.len()).all(|i| {
            let a = addr.wrapping_add(i as u32);
            self.regions.iter().any(|r| {
                r.writable && a >= r.start && ((a - r.start) as usize) < r.data.len()
            })
        });
        if ok {
            for (i, &b) in data.iter().enumerate() {
                self.poke(addr.wrapping_add(i as u32), b, false);
            }
        }
        ok
    }

    /// 沿指针链求最终地址，失败时返回条目状态。
    fn resolve(&self, it: &Item, now: u64) -> std::result::Result<u32, u8> {
        let mut cur = it.base;
        for (i, &d) in it.derefs.iter().enumerate() {
            let a = cur.wrapping_add(d as i32 as u32);
            if a & 3 != 0 {
                return Err(0x20 + i as u8);
            }
            let p = self.read(a, 4, now).ok_or(0x10 + i as u8)?;
            let p = get32(&p);
            if p == 0 {
                return Err(0x30 + i as u8);
            }
            cur = p;
        }
        let a = cur.wrapping_add(it.final_off as i32 as u32);
        let align = if it.size == 8 { 4 } else { it.size as u32 };
        if a & (align - 1) != 0 {
            return Err(0x2F);
        }
        Ok(a)
    }

    fn read_item(&self, it: &Item, now: u64) -> (u8, Vec<u8>) {
        let zero = vec![0u8; it.size as usize];
        match self.resolve(it, now) {
            Err(st) => (st, zero),
            Ok(a) => match self.read(a, it.size as usize, now) {
                Some(v) => (0, v),
                None => (0x1F, zero),
            },
        }
    }

    fn handle(&mut self, req: &[u8]) -> Vec<u8> {
        let Some(&cmd) = req.first() else {
            return vec![0xFF];
        };
        if self.active && !matches!(cmd, 0x00 | 0x01 | 0x80..=0x85) {
            return vec![0xFF];
        }
        match cmd {
            0x00 => self.dap_info(req),
            0x01 => vec![0x01, 0x00],
            0x02 => {
                let port = req.get(1).copied().unwrap_or(0);
                self.connected = port <= 1;
                vec![0x02, if self.connected { 1 } else { 0 }]
            }
            0x03 => {
                self.active = false;
                self.connected = false;
                vec![0x03, 0x00]
            }
            0x04 | 0x08 | 0x12 | 0x13 => vec![cmd, 0x00],
            0x11 => {
                if req.len() >= 5 {
                    self.clock_hz = get32(&req[1..]).max(1);
                }
                vec![0x11, 0x00]
            }
            0x05 => self.transfer(req),
            0x80 => self.info(req),
            0x81 => self.cmd_read(req),
            0x82 => self.cmd_write(req),
            0x83 => self.sub_set(req),
            0x84 => self.sub_ctrl(req),
            0x85 => self.sub_read(req),
            _ => vec![0xFF],
        }
    }

    fn dap_info(&self, req: &[u8]) -> Vec<u8> {
        let id = req.get(1).copied().unwrap_or(0);
        let data: Vec<u8> = match id {
            0x01 => b"XRobot\0".to_vec(),
            0x02 => b"XR-DAP fake\0".to_vec(),
            0x03 => b"FAKE-0001\0".to_vec(),
            0x04 => b"2.1.1\0".to_vec(),
            0x09 => b"fake\0".to_vec(),
            0xF0 => vec![0x01],
            0xFE => vec![1],
            0xFF => (PACKET_SIZE as u16).to_le_bytes().to_vec(),
            _ => Vec::new(),
        };
        let mut out = vec![0x00, data.len() as u8];
        out.extend(data);
        out
    }

    fn transfer(&mut self, req: &[u8]) -> Vec<u8> {
        if !self.connected || req.len() < 3 {
            return vec![0x05, 0x00, 0x00];
        }
        let count = req[2] as usize;
        let mut p = &req[3..];
        let mut data = Vec::new();
        let mut done = 0u8;
        for _ in 0..count {
            let Some((&r, rest)) = p.split_first() else { break };
            p = rest;
            let ap = r & 1 != 0;
            let read = r & 2 != 0;
            let a = r & 0x0C;
            if read {
                let v = match (ap, a) {
                    (false, 0x00) => DPIDR,
                    (false, 0x04) => {
                        let mut cs = self.ctrl_stat;
                        if cs & (1 << 28) != 0 {
                            cs |= 1 << 29;
                        }
                        if cs & (1 << 30) != 0 {
                            cs |= 1 << 31;
                        }
                        cs
                    }
                    _ => 0,
                };
                data.extend_from_slice(&v.to_le_bytes());
            } else {
                if p.len() < 4 {
                    break;
                }
                let v = get32(p);
                p = &p[4..];
                if !ap && a == 0x04 {
                    self.ctrl_stat = v;
                }
            }
            done += 1;
        }
        let mut out = vec![0x05, done, 0x01];
        out.extend(data);
        out
    }

    fn calibrated_access_us(&self) -> u32 {
        4 + 48_000_000 / self.clock_hz
    }

    fn info(&self, req: &[u8]) -> Vec<u8> {
        if req.len() != 1 {
            return vec![0x80, 0x01];
        }
        let mut out = vec![0x80, 0x00, 1];
        out.extend_from_slice(&0x0Fu32.to_le_bytes());
        out.extend_from_slice(&(MAX_ITEMS as u16).to_le_bytes());
        out.push(8);
        out.extend_from_slice(&(self.ring_bytes as u32).to_le_bytes());
        out.extend_from_slice(&self.elapsed_us().to_le_bytes());
        out.extend_from_slice(&self.access_us.to_le_bytes());
        out
    }

    fn cmd_read(&mut self, req: &[u8]) -> Vec<u8> {
        if req.len() < 2 || req[1] == 0 {
            return vec![0x81, 0x01];
        }
        let count = req[1] as usize;
        if count > MAX_ITEMS {
            return vec![0x81, 0x03];
        }
        let mut p = &req[2..];
        let mut items = Vec::new();
        let mut resp_size = 3;
        for _ in 0..count {
            match parse_item(&mut p, false) {
                Some((it, _, _)) if size_valid(it.size) => {
                    resp_size += 3 + it.size as usize;
                    items.push(it);
                }
                _ => return vec![0x81, 0x01],
            }
        }
        if !p.is_empty() {
            return vec![0x81, 0x01];
        }
        if resp_size > PACKET_SIZE {
            return vec![0x81, 0x03];
        }
        if !self.connected {
            return vec![0x81, 0x04];
        }
        let now = self.elapsed_us();
        let mut out = vec![0x81, 0x00, count as u8];
        for it in &items {
            let (st, v) = self.read_item(it, now);
            out.extend_from_slice(&it.id.to_le_bytes());
            out.push(st);
            out.extend(v);
        }
        out
    }

    fn cmd_write(&mut self, req: &[u8]) -> Vec<u8> {
        if req.len() < 2 || req[1] == 0 {
            return vec![0x82, 0x01];
        }
        let count = req[1] as usize;
        if count > MAX_ITEMS {
            return vec![0x82, 0x03];
        }
        let mut p = &req[2..];
        let mut items = Vec::new();
        for _ in 0..count {
            match parse_item(&mut p, false) {
                Some((it, _, _)) if size_valid(it.size) && p.len() >= it.size as usize => {
                    let v = p[..it.size as usize].to_vec();
                    p = &p[it.size as usize..];
                    items.push((it, v));
                }
                _ => return vec![0x82, 0x01],
            }
        }
        if !p.is_empty() {
            return vec![0x82, 0x01];
        }
        if !self.connected {
            return vec![0x82, 0x04];
        }
        let now = self.elapsed_us();
        let mut out = vec![0x82, 0x00, count as u8];
        for (it, v) in &items {
            let st = match self.resolve(it, now) {
                Err(st) => st,
                Ok(a) => {
                    if self.write(a, v) {
                        0
                    } else {
                        0x1F
                    }
                }
            };
            out.extend_from_slice(&it.id.to_le_bytes());
            out.push(st);
        }
        out
    }

    fn sub_set(&mut self, req: &[u8]) -> Vec<u8> {
        if self.active {
            return vec![0x83, 0x02];
        }
        if req.len() < 8 {
            return vec![0x83, 0x01];
        }
        let period = get32(&req[1..]);
        let flags = req[5];
        let count = get16(&req[6..]) as usize;
        if period == 0 || flags & 0xFE != 0 || count == 0 {
            return vec![0x83, 0x01];
        }
        if count > MAX_ITEMS {
            return vec![0x83, 0x03];
        }
        let mut p = &req[8..];
        let mut items = Vec::new();
        let mut aps: Vec<u8> = Vec::new();
        let mut max_frame = 10usize;
        for _ in 0..count {
            match parse_item(&mut p, true) {
                Some((it, div, phase)) if size_valid(it.size) && div != 0 && phase < div => {
                    if !aps.contains(&it.ap) {
                        if aps.len() >= MAX_APS {
                            return vec![0x83, 0x03];
                        }
                        aps.push(it.ap);
                    }
                    max_frame += it.size as usize;
                    items.push(SubItem {
                        item: it,
                        div,
                        phase,
                    });
                }
                _ => return vec![0x83, 0x01],
            }
        }
        if !p.is_empty() {
            return vec![0x83, 0x01];
        }
        max_frame += count.div_ceil(8);
        if !self.connected {
            return vec![0x83, 0x04];
        }
        self.access_us = self.calibrated_access_us();
        let t: u32 = items
            .iter()
            .map(|s| {
                let accesses = 1 + s.item.derefs.len() as u32 + u32::from(s.item.size == 8) + 1;
                accesses * self.access_us
            })
            .sum();
        let est = 10 + t * 8 / 5;
        let mut out = vec![0x83, 0x00];
        out.extend_from_slice(&(max_frame as u16).to_le_bytes());
        out.extend_from_slice(&est.to_le_bytes());
        if max_frame + 2 > self.ring_bytes || est > period {
            out[1] = 0x03;
            return out;
        }
        self.config = Some(SubConfig {
            items,
            period_us: period,
            stop_on_full: flags & 1 != 0,
        });
        self.reset_ring();
        out
    }

    fn reset_ring(&mut self) {
        self.ring.clear();
        self.ring_used = 0;
        self.ring_payload = 0;
        self.dropped = 0;
        self.skipped = 0;
    }

    fn sub_ctrl(&mut self, req: &[u8]) -> Vec<u8> {
        if req.len() != 2 {
            return vec![0x84, 0x01];
        }
        let status = match req[1] {
            0 => {
                self.advance();
                self.active = false;
                0x00
            }
            1 => {
                if self.active || self.config.is_none() {
                    0x02
                } else if !self.connected {
                    0x04
                } else {
                    self.reset_ring();
                    self.start_us = self.elapsed_us();
                    self.next_k = 0;
                    self.active = true;
                    0x00
                }
            }
            2 => {
                self.reset_ring();
                0x00
            }
            _ => 0x01,
        };
        vec![0x84, status]
    }

    fn push_frame(&mut self, frame: Vec<u8>, stop_on_full: bool) {
        while self.ring_used + frame.len() + 2 > self.ring_bytes {
            if stop_on_full || self.ring.is_empty() {
                self.dropped += 1;
                return;
            }
            let old = self.ring.pop_front().unwrap();
            self.ring_used -= old.len() + 2;
            self.ring_payload -= old.len();
            self.dropped += 1;
        }
        self.ring_used += frame.len() + 2;
        self.ring_payload += frame.len();
        self.ring.push_back(frame);
    }

    /// 生成从上次到现在所有到期节拍的帧。
    fn advance(&mut self) {
        if !self.active {
            return;
        }
        let Some(cfg) = self.config.as_ref() else { return };
        let period = cfg.period_us as u64;
        let items = cfg.items.clone();
        let stop_on_full = cfg.stop_on_full;
        let now = self.elapsed_us();
        let mut budget = 200_000;
        while self.start_us + self.next_k * period <= now && budget > 0 {
            budget -= 1;
            let k = self.next_k;
            self.next_k += 1;
            if let Some(n) = self.skip_every {
                if n > 0 && k % n == n - 1 {
                    self.skipped += 1;
                    continue;
                }
            }
            let due: Vec<&SubItem> = items.iter().filter(|s| s.due(k)).collect();
            if due.is_empty() {
                continue;
            }
            let ts = self.start_us + k * period;
            let mut frame = Vec::new();
            frame.extend_from_slice(&(k as u32).to_le_bytes());
            frame.extend_from_slice(&ts.to_le_bytes()[..6]);
            let bitmap_at = frame.len();
            frame.resize(bitmap_at + due.len().div_ceil(8), 0);
            for (j, s) in due.iter().enumerate() {
                let (st, v) = self.read_item(&s.item, ts);
                if st != 0 {
                    frame[bitmap_at + j / 8] |= 1 << (j % 8);
                }
                frame.extend(v);
            }
            self.push_frame(frame, stop_on_full);
        }
    }

    fn sub_read(&mut self, req: &[u8]) -> Vec<u8> {
        if req.len() != 3 {
            return vec![0x85, 0x01];
        }
        if self.config.is_none() {
            return vec![0x85, 0x02];
        }
        self.advance();
        let want = get16(&req[1..]) as usize;
        let limit = want.min(PACKET_SIZE - 24);
        let mut frames = Vec::new();
        let mut count = 0u16;
        while let Some(f) = self.ring.front() {
            if frames.len() + f.len() > limit {
                break;
            }
            let f = self.ring.pop_front().unwrap();
            self.ring_used -= f.len() + 2;
            self.ring_payload -= f.len();
            frames.extend(f);
            count += 1;
        }
        let mut out = vec![0x85, 0x00];
        out.extend_from_slice(&self.elapsed_us().to_le_bytes());
        out.extend_from_slice(&(self.ring_payload as u32).to_le_bytes());
        out.extend_from_slice(&self.dropped.to_le_bytes());
        out.extend_from_slice(&self.skipped.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend(frames);
        out
    }
}
