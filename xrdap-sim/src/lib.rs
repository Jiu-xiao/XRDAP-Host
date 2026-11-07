//! 测试用的 XR-DAP 探针模拟器。只给本仓库的测试用，不进发布的程序。
//!
//! 行为的依据只有两份文档，不照抄主机或固件的代码：
//! - CMSIS-DAP 规范：DAP_Info、DAP_Connect、DAP_Transfer 等标准命令，响应回显命令号，
//!   DAP_TransferAbort 没有响应，不认识的命令回 `FF`；
//! - XRDAP-Core 的协议文档（版本 2）<https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.md>：
//!   TCP 帧（第 2 节）、一次只使用一条链路的规则和采样（第 3.1 节）、厂商命令 0x80 到 0x86
//!   （第 3 节）、控制口（第 3.7.1 节）、探针身份（第 4.1 节）。
//!
//! 每条链路（[`Sim::link`] 的每一个、TCP 的每个客户端）是一个传输。探针不仲裁，所有命令谁发都
//! 执行。DAP_Connect 记下它来自哪个传输，这个连接随 DAP_Disconnect（任何传输发的）或那个传输
//! 断开而作废，采样随之停止（协议文档 3.1 节）。
//!
//! 目标是一个 Cortex-M：Flash 0x08000000 起 128 KiB（只读），RAM 0x20000000 起 64 KiB，
//! 别的地址访问出错（SWD FAULT）；DHCSR、DEMCR、AIRCR、CPUID 按 ARMv7-M 架构手册 C1.6 的
//! 规则响应。采样按电脑的单调时钟走，收到命令时补齐到期的帧。
//!
//! Test-only XR-DAP probe simulator. Its behaviour follows the CMSIS-DAP specification and the
//! XRDAP-Core protocol document only, not host or firmware code. The target is a Cortex-M with
//! read-only flash at 0x08000000 and RAM at 0x20000000; other addresses fault.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use xrdap::raw::frame::{
    tcp_frame, TCP_HEADER_LEN, TCP_MAGIC, TCP_TYPE_CONTROL_REQUEST, TCP_TYPE_CONTROL_RESPONSE,
    TCP_TYPE_REQUEST, TCP_TYPE_RESPONSE,
};
use xrdap::raw::transport::Transport;

/// 模拟探针的序列号（协议文档 4.1 节的格式，占位值）。
pub const SERIAL: &str = "XRobot-XRDAP-AABBCCDDEEFF";
/// DAP 包长：1024（协议文档 3.2 节，XRDAP-ESP 的值）。
pub const PACKET_SIZE: usize = 1024;
/// 每次请求和每个订阅最多的条目数（协议文档 3.4 节 INFO）。
pub const MAX_ITEMS: usize = 32;
/// 默认的功能位（协议文档 3.4 节 INFO）：XRDAP-ESP 的值，有 Wi-Fi 配置、控制口和紧凑帧。
pub const FEATURES: u32 = 0x7F;
/// DPIDR：ARM 的 SW-DP v1，部件号 0xBA。
pub const DPIDR: u32 = 0x2BA0_1477;
/// CPUID：Cortex-M4 r0p1。
pub const CPUID: u32 = 0x410F_C241;

const FEATURE_WIFI: u32 = 1 << 4;
const FEATURE_CONTROL: u32 = 1 << 5;
const FEATURE_COMPACT: u32 = 1 << 6;

/// 控制口自己的状态：没有这个操作（协议文档 3.7.1 节）。
const CONTROL_UNSUPPORTED: u8 = 0xF2;

/// 时间戳 48 位。
const STAMP_MASK: u64 = (1 << 48) - 1;

const FLASH: u32 = 0x0800_0000;
const RAM: u32 = 0x2000_0000;
const SCS_CPUID: u32 = 0xE000_ED00;
const SCS_AIRCR: u32 = 0xE000_ED0C;
const SCS_DHCSR: u32 = 0xE000_EDF0;
const SCS_DEMCR: u32 = 0xE000_EDFC;

/// 探针时间轴的起点：开机后过了 1 秒，避免时间戳为 0 的特例。
const TIME_ORIGIN_US: u64 = 1_000_000;

struct Region {
    start: u32,
    data: Vec<u8>,
    writable: bool,
}

/// 随内核运行时间递增的变量：值 = 内核运行的微秒数 / period_us。内核停住时不走。
struct Counter {
    addr: u32,
    size: u8,
    period_us: u64,
}

#[derive(Clone)]
struct Item {
    ap: u8,
    id: u16,
    size: u8,
    base: u32,
    derefs: Vec<i16>,
    final_off: i16,
}

#[derive(Clone)]
struct SubItem {
    item: Item,
    div: u16,
    phase: u16,
}

struct Subscription {
    items: Vec<SubItem>,
    period_us: u32,
    drop_new: bool,
    /// SUB_READ 用紧凑帧，直到下一次 SUB_START（协议文档 3.5 节）
    compact: bool,
}

/// Cortex-M 的调试状态（ARMv7-M C1.6）。
#[derive(Default)]
struct Core {
    debugen: bool,
    halted: bool,
    demcr: u32,
    /// DHCSR.S_RESET_ST：复位后第一次读 DHCSR 时为 1，读后清零
    reset_seen: bool,
}

struct State {
    /// 功能位（INFO 偏移 3）
    features: u32,
    /// 控制口 SEND 能带的最长请求
    control_limit: usize,
    control_requests: Vec<Vec<u8>>,
    regions: Vec<Region>,
    counters: Vec<Counter>,
    /// 计数器的零点（探针时间）；系统复位时重新开始
    counter_origin_us: u64,
    /// 内核停住的累计时长，和这次停住的起点
    frozen_us: u64,
    halt_start_us: Option<u64>,
    core: Core,
    /// 地址 -> 还要失败几次读
    failing_reads: Vec<(u32, u32)>,
    t0: Instant,
    /// 连接所在的传输；None 时没有连接，厂商命令回 0x04
    connected: Option<u32>,
    /// 开始这次采样的传输
    sampling_link: u32,
    ctrl_stat: u32,
    next_transport: u32,
    sub: Option<Subscription>,
    active: bool,
    start_us: u64,
    next_tick: u64,
    ring: VecDeque<Vec<u8>>,
    ring_bytes: usize,
    dropped: u32,
    skipped: u32,
    skip_every: Option<u64>,
    wifi_ssid: Vec<u8>,
    /// 最近一次清除热点参数的来源（协议文档 3.7 节的编码）
    wifi_cleared_by: u8,
    requests: Vec<Vec<u8>>,
}

/// 模拟探针。克隆出的句柄共享同一个探针。
#[derive(Clone)]
pub struct Sim(Arc<Mutex<State>>);

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}

impl Sim {
    /// Flash 和 RAM 内容全为 0，内核在运行。
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(State {
            features: FEATURES,
            control_limit: usize::MAX,
            control_requests: Vec::new(),
            regions: vec![
                Region {
                    start: FLASH,
                    data: vec![0; 128 * 1024],
                    writable: false,
                },
                Region {
                    start: RAM,
                    data: vec![0; 64 * 1024],
                    writable: true,
                },
            ],
            counters: Vec::new(),
            counter_origin_us: TIME_ORIGIN_US,
            frozen_us: 0,
            halt_start_us: None,
            core: Core::default(),
            failing_reads: Vec::new(),
            t0: Instant::now(),
            connected: None,
            sampling_link: 0,
            ctrl_stat: 0,
            next_transport: 0,
            sub: None,
            active: false,
            start_us: 0,
            next_tick: 0,
            ring: VecDeque::new(),
            ring_bytes: 16384,
            dropped: 0,
            skipped: 0,
            skip_every: None,
            wifi_ssid: Vec::new(),
            wifi_cleared_by: 0,
            requests: Vec::new(),
        })))
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 把数据放进目标内存（Flash 也可以），不经过 SWD。
    pub fn load(&self, addr: u32, data: &[u8]) {
        let mut st = self.st();
        for (i, &b) in data.iter().enumerate() {
            assert!(
                st.poke(addr + i as u32, b, true),
                "0x{addr:08X} 不在模拟内存里"
            );
        }
    }

    /// 按 ELF 的各段把初值放进目标内存：段的运行地址落在 Flash 或 RAM 里的都放。
    pub fn load_elf(&self, elf: &[u8]) {
        use object::{Object, ObjectSection, SectionKind};
        let file = object::File::parse(elf).expect("ELF 格式不对");
        for s in file.sections() {
            if matches!(
                s.kind(),
                SectionKind::UninitializedData | SectionKind::Other
            ) {
                continue;
            }
            let (Ok(data), addr) = (s.data(), s.address() as u32) else {
                continue;
            };
            if !data.is_empty() && self.st().region(addr).is_some() {
                self.load(addr, data);
            }
        }
    }

    /// 读目标内存，不经过 SWD。
    pub fn peek(&self, addr: u32, len: usize) -> Vec<u8> {
        let st = self.st();
        (0..len)
            .map(|i| st.byte(addr + i as u32).expect("地址不在模拟内存里"))
            .collect()
    }

    /// 让一个变量随内核运行时间递增：值 = 运行的微秒数 / `period_us`（`size` 字节，小端）。
    /// 内核停住时不走，系统复位后从 0 开始。
    pub fn add_counter(&self, addr: u32, size: u8, period_us: u64) {
        self.st().counters.push(Counter {
            addr,
            size,
            period_us,
        });
    }

    /// 功能位（INFO 偏移 3），默认 [`FEATURES`]。例如 0x6F 是没有 Wi-Fi 的探针，0x1F 是没有控制口
    /// 和紧凑帧的旧固件（协议文档 3.4 节）。
    pub fn set_features(&self, bits: u32) {
        self.st().features = bits;
    }

    /// [`Sim::link`] 的控制口 SEND 能带的最长请求，更长的被拒（协议文档 3.7.1 节，USB 的 EP0 OUT
    /// 缓冲）。默认不限。
    pub fn set_control_limit(&self, bytes: usize) {
        self.st().control_limit = bytes;
    }

    /// 接下来 `times` 次读 `addr` 都失败（SWD FAULT）。
    pub fn fail_reads(&self, addr: u32, times: u32) {
        self.st().failing_reads.push((addr, times));
    }

    /// 采样环形缓冲的字节数（INFO 偏移 10），默认 16384。
    pub fn set_ring_bytes(&self, n: usize) {
        self.st().ring_bytes = n;
    }

    /// 每 `n` 拍跳过一拍，模拟 SWD 跟不上（协议文档 3.5 节）。
    pub fn skip_every(&self, n: Option<u64>) {
        self.st().skip_every = n;
    }

    /// 探针是否正在采样。
    pub fn is_sampling(&self) -> bool {
        self.st().active
    }

    /// 内核是否停住。
    pub fn is_halted(&self) -> bool {
        self.st().core.halted
    }

    /// DEMCR 的当前值。
    pub fn demcr(&self) -> u32 {
        self.st().core.demcr
    }

    /// 保存的 Wi-Fi 热点名。
    pub fn wifi_ssid(&self) -> String {
        String::from_utf8_lossy(&self.st().wifi_ssid).into_owned()
    }

    /// 到现在为止收到的全部请求。
    pub fn requests(&self) -> Vec<Vec<u8>> {
        self.st().requests.clone()
    }

    /// 到现在为止经控制口收到的全部请求。
    pub fn control_requests(&self) -> Vec<Vec<u8>> {
        self.st().control_requests.clone()
    }

    /// 一条直接连着这个模拟探针的链路，是一个新的传输。
    pub fn link(&self) -> Link {
        Link {
            sim: self.clone(),
            transport: self.st().new_transport(),
            out: VecDeque::new(),
        }
    }

    /// 传输 `transport` 执行一个请求；没有响应的命令（DAP_TransferAbort）返回 None。
    fn execute(&self, transport: u32, request: &[u8]) -> Option<Vec<u8>> {
        let mut st = self.st();
        st.requests.push(request.to_vec());
        st.handle(transport, request)
    }

    /// 在 `127.0.0.1` 的一个空闲端口上提供探针的 TCP DAP 服务（协议文档第 2 节），返回地址。
    /// 服务线程在后台一直运行。
    pub fn serve_tcp(&self) -> SocketAddr {
        self.serve_tcp_on("127.0.0.1:0").expect("监听本机端口")
    }

    /// 在 `listen` 上提供 TCP DAP 服务，返回实际监听的地址。服务线程在后台一直运行。
    pub fn serve_tcp_on(&self, listen: &str) -> std::io::Result<SocketAddr> {
        let listener = TcpListener::bind(listen)?;
        let addr = listener.local_addr()?;
        let sim = self.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let transport = sim.st().new_transport();
                let _ = serve_client(&sim, transport, stream);
                // 客户端断开，这个传输丢了（协议文档 3.1 节）
                sim.st().link_lost(transport);
            }
        });
        Ok(addr)
    }
}

/// 处理一个 TCP 客户端：一个请求帧对应一个响应帧，没有响应的命令不回帧，帧头无效时断开。
/// 帧类型 3 是控制请求，应答是类型 4；没有控制口的固件把它当帧头无效。
fn serve_client(sim: &Sim, transport: u32, mut s: std::net::TcpStream) -> std::io::Result<()> {
    let _ = s.set_nodelay(true);
    let mut h = [0u8; TCP_HEADER_LEN];
    loop {
        s.read_exact(&mut h)?;
        let len = u16::from_le_bytes([h[4], h[5]]) as usize;
        let control = h[6] == TCP_TYPE_CONTROL_REQUEST && sim.st().has_control();
        if &h[..4] != TCP_MAGIC
            || !(h[6] == TCP_TYPE_REQUEST || control)
            || len == 0
            || len > PACKET_SIZE
        {
            return Ok(());
        }
        let mut req = vec![0u8; len];
        s.read_exact(&mut req)?;
        if control {
            let answer = sim.st().control(&req);
            s.write_all(&tcp_frame(&answer, TCP_TYPE_CONTROL_RESPONSE))?;
        } else if let Some(resp) = sim.execute(transport, &req) {
            s.write_all(&tcp_frame(&resp, TCP_TYPE_RESPONSE))?;
        }
    }
}

/// 直接连着模拟探针的链路，实现 [`Transport`]。
pub struct Link {
    sim: Sim,
    transport: u32,
    out: VecDeque<Vec<u8>>,
}

impl Drop for Link {
    /// 链路断开，这个传输丢了。
    fn drop(&mut self) {
        self.sim.st().link_lost(self.transport);
    }
}

impl Transport for Link {
    fn send(&mut self, request: &[u8]) -> xrdap::Result<()> {
        self.out.extend(self.sim.execute(self.transport, request));
        Ok(())
    }

    fn recv(&mut self, _timeout: Duration) -> xrdap::Result<Vec<u8>> {
        self.out
            .pop_front()
            .ok_or_else(|| xrdap::Error::Timeout("模拟探针没有响应".into()))
    }

    fn describe(&self) -> String {
        "模拟探针".into()
    }

    /// 控制口，按 USB 的规则（协议文档 3.7.1 节）：固件没有控制口、请求为空或超过 EP0 的 OUT 缓冲
    /// 时 SEND 被拒。
    fn control(&mut self, request: &[u8]) -> xrdap::Result<Option<Vec<u8>>> {
        let mut st = self.sim.st();
        let refused = !st.has_control() || request.is_empty() || request.len() > st.control_limit;
        Ok((!refused).then(|| st.control(request)))
    }
}

fn u16_at(p: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([p[at], p[at + 1]])
}

fn u32_at(p: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([p[at], p[at + 1], p[at + 2], p[at + 3]])
}

/// 完整帧开头的节拍序号和时间戳（协议文档 3.5 节）。
fn frame_head(frame: &[u8]) -> (u32, u64) {
    let mut stamp = [0u8; 8];
    stamp[..6].copy_from_slice(&frame[4..10]);
    (u32_at(frame, 0), u64::from_le_bytes(stamp))
}

/// 无符号 LEB128（协议文档 3.5 节）：每字节低 7 位，从低位起；最高位为 1 表示后面还有字节。
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// 从 `p` 开头解析一个条目（协议文档 3.3 节）并前移 `p`。长度不够返回 None；字段不合法时
/// 仍然解析出来，由调用方检查。
fn take_item(p: &mut &[u8]) -> Option<Item> {
    if p.len() < 11 {
        return None;
    }
    let n = p[8] as usize;
    if p.len() < 11 + 2 * n {
        return None;
    }
    let derefs = (0..n).map(|i| u16_at(p, 9 + 2 * i) as i16).collect();
    let item = Item {
        ap: p[0],
        id: u16_at(p, 1),
        size: p[3],
        base: u32_at(p, 4),
        derefs,
        final_off: u16_at(p, 9 + 2 * n) as i16,
    };
    *p = &p[11 + 2 * n..];
    Some(item)
}

fn item_valid(it: &Item) -> bool {
    matches!(it.size, 1 | 2 | 4 | 8) && it.derefs.len() <= 8
}

// 整体状态（协议文档 3.2 节）
const OK: u8 = 0x00;
const ARG: u8 = 0x01;
const STATE: u8 = 0x02;
const RESOURCE: u8 = 0x03;
const NO_TARGET: u8 = 0x04;

impl State {
    fn new_transport(&mut self) -> u32 {
        self.next_transport += 1;
        self.next_transport
    }

    fn has_control(&self) -> bool {
        self.features & FEATURE_CONTROL != 0
    }

    /// 一个控制请求，`op + 载荷`（协议文档 3.7.1 节）。Wi-Fi 操作的 op 和载荷与 0x86 相同，只是请求
    /// 没有 `86`，应答第 1 字节是 op；别的操作这个模拟探针没有。不影响连接和采样。
    fn control(&mut self, req: &[u8]) -> Vec<u8> {
        self.control_requests.push(req.to_vec());
        let op = req[0];
        if self.features & FEATURE_WIFI == 0 || op > 2 {
            return vec![op, CONTROL_UNSUPPORTED];
        }
        let mut dap = vec![0x86];
        dap.extend_from_slice(req);
        let mut answer = self.wifi(&dap);
        answer[0] = op;
        answer
    }

    /// 连接作废：扩展命令要求重新 DAP_Connect，正在进行的采样停止（协议文档 3.1 节）。
    fn drop_connection(&mut self) {
        self.stop_sampling();
        self.connected = None;
    }

    /// 传输 `transport` 丢了：连接在它上面才作废，否则什么也不发生（协议文档 3.1 节）。
    fn link_lost(&mut self, transport: u32) {
        if self.connected == Some(transport) {
            self.drop_connection();
        }
    }

    fn now_us(&self) -> u64 {
        TIME_ORIGIN_US + self.t0.elapsed().as_micros() as u64
    }

    fn region(&self, addr: u32) -> Option<&Region> {
        self.regions
            .iter()
            .find(|r| addr >= r.start && ((addr - r.start) as usize) < r.data.len())
    }

    fn poke(&mut self, addr: u32, b: u8, force: bool) -> bool {
        match self
            .regions
            .iter_mut()
            .find(|r| addr >= r.start && ((addr - r.start) as usize) < r.data.len())
        {
            Some(r) if r.writable || force => {
                r.data[(addr - r.start) as usize] = b;
                true
            }
            _ => false,
        }
    }

    fn byte(&self, addr: u32) -> Option<u8> {
        self.region(addr).map(|r| r.data[(addr - r.start) as usize])
    }

    /// 在探针时间 `at` 读 `len` 字节；访问出错时返回 None。
    fn read_mem(&mut self, addr: u32, len: usize, at: u64) -> Option<Vec<u8>> {
        if let Some(f) = self
            .failing_reads
            .iter_mut()
            .find(|(a, n)| *a == addr && *n > 0)
        {
            f.1 -= 1;
            return None;
        }
        if len == 4 {
            if let Some(v) = self.scs_read(addr) {
                return Some(v.to_le_bytes().to_vec());
            }
        }
        let mut out: Vec<u8> = (0..len)
            .map(|i| self.byte(addr + i as u32))
            .collect::<Option<_>>()?;
        for c in &self.counters {
            if addr <= c.addr && c.addr + c.size as u32 <= addr + len as u32 {
                let v = self.run_us(at) / c.period_us;
                let off = (c.addr - addr) as usize;
                out[off..off + c.size as usize]
                    .copy_from_slice(&v.to_le_bytes()[..c.size as usize]);
            }
        }
        Some(out)
    }

    fn write_mem(&mut self, addr: u32, data: &[u8]) -> bool {
        if data.len() == 4 && self.scs_write(addr, u32_at(data, 0)) {
            return true;
        }
        if (0..data.len()).any(|i| self.region(addr + i as u32).is_none_or(|r| !r.writable)) {
            return false;
        }
        for (i, &b) in data.iter().enumerate() {
            self.poke(addr + i as u32, b, false);
        }
        true
    }

    /// 到探针时间 `at` 为止内核运行了多久（不算停住的时间）。
    fn run_us(&self, at: u64) -> u64 {
        let frozen = self.frozen_us + self.halt_start_us.map_or(0, |h| at.saturating_sub(h));
        at.saturating_sub(self.counter_origin_us)
            .saturating_sub(frozen)
    }

    fn set_halted(&mut self, halted: bool) {
        let now = self.now_us();
        match (self.halt_start_us, halted) {
            (None, true) => self.halt_start_us = Some(now),
            (Some(h), false) => {
                self.frozen_us += now - h;
                self.halt_start_us = None;
            }
            _ => {}
        }
        self.core.halted = halted;
    }

    /// 系统控制空间里模拟的寄存器（ARMv7-M C1.6）。
    fn scs_read(&mut self, addr: u32) -> Option<u32> {
        Some(match addr {
            SCS_CPUID => CPUID,
            SCS_DEMCR => self.core.demcr,
            SCS_AIRCR => 0xFA05_0000,
            SCS_DHCSR => {
                let c = &mut self.core;
                let v = u32::from(c.debugen)
                    | u32::from(c.halted) << 1
                    | u32::from(c.halted) << 17
                    | u32::from(c.reset_seen) << 25;
                c.reset_seen = false;
                v
            }
            _ => return None,
        })
    }

    fn scs_write(&mut self, addr: u32, v: u32) -> bool {
        match addr {
            // 写 DHCSR 要在高 16 位带 DBGKEY；C_HALT 只在 C_DEBUGEN 为 1 时有效
            SCS_DHCSR => {
                if v >> 16 == 0xA05F {
                    self.core.debugen = v & 1 != 0;
                    self.set_halted(self.core.debugen && v & 2 != 0);
                }
            }
            SCS_DEMCR => self.core.demcr = v,
            // 写 AIRCR 要在高 16 位带 VECTKEY；SYSRESETREQ 让系统复位。DEMCR、DHCSR 的
            // C_DEBUGEN 不受系统复位影响；DEMCR.VC_CORERESET 为 1 时停在复位向量
            SCS_AIRCR => {
                if v >> 16 == 0x05FA && v & 4 != 0 {
                    self.core.reset_seen = true;
                    self.counter_origin_us = self.now_us();
                    self.frozen_us = 0;
                    self.halt_start_us = None;
                    self.set_halted(self.core.debugen && self.core.demcr & 1 != 0);
                }
            }
            SCS_CPUID => {}
            _ => return false,
        }
        true
    }

    /// 按协议文档 3.3 节的伪代码求最终地址；失败时给出条目状态。
    fn resolve(&mut self, it: &Item, at: u64) -> Result<u32, u8> {
        let mut cur = it.base;
        for (i, &d) in it.derefs.iter().enumerate() {
            let addr = cur.wrapping_add(d as i32 as u32);
            if !addr.is_multiple_of(4) {
                return Err(0x20 + i as u8);
            }
            let p = self.read_mem(addr, 4, at).ok_or(0x10 + i as u8)?;
            let p = u32_at(&p, 0);
            if p == 0 {
                return Err(0x30 + i as u8);
            }
            cur = p;
        }
        let addr = cur.wrapping_add(it.final_off as i32 as u32);
        if !addr.is_multiple_of(u32::from(it.size.min(4))) {
            return Err(0x2F);
        }
        Ok(addr)
    }

    fn read_item(&mut self, it: &Item, at: u64) -> (u8, Vec<u8>) {
        let size = it.size as usize;
        match self.resolve(it, at) {
            Ok(addr) => match self.read_mem(addr, size, at) {
                Some(v) => (OK, v),
                None => (0x1F, vec![0; size]),
            },
            Err(s) => (s, vec![0; size]),
        }
    }

    fn handle(&mut self, transport: u32, req: &[u8]) -> Option<Vec<u8>> {
        let cmd = *req.first()?;
        // CMSIS-DAP：DAP_TransferAbort 没有响应
        if cmd == 0x07 {
            return None;
        }
        self.advance();
        // 开始采样的传输又发来 DAP_Connect：它的上位机回来了，结束旧的连接和采样，重新连接
        // （协议文档 3.1 节）
        if cmd == 0x02 && self.active && self.sampling_link == transport {
            self.drop_connection();
        }
        // 采样期间只接受 DAP_Info、DAP_HostStatus 和厂商命令（协议文档 3.1 节）
        if self.active && !matches!(cmd, 0x00 | 0x01 | 0x80..=0x86) {
            return Some(vec![0xFF]);
        }
        Some(match cmd {
            0x00 => self.dap_info(req),
            0x01 => vec![0x01, 0x00],
            0x02 => self.dap_connect(transport, req),
            0x03 => {
                self.drop_connection();
                vec![0x03, 0x00]
            }
            0x04 | 0x08 | 0x11 | 0x12 | 0x13 => vec![cmd, 0x00],
            0x05 => self.dap_transfer(req),
            0x80 => self.info(req),
            0x81 => self.cmd_read(req),
            0x82 => self.cmd_write(req),
            0x83 => self.sub_start(transport, req),
            0x84 => self.sub_ctrl(req),
            0x85 => self.sub_read(req),
            0x86 if self.features & FEATURE_WIFI != 0 => self.wifi(req),
            _ => vec![0xFF],
        })
    }

    // ------------------------------------------------------------- CMSIS-DAP

    fn dap_info(&self, req: &[u8]) -> Vec<u8> {
        let text = |s: &str| {
            let mut v = s.as_bytes().to_vec();
            v.push(0);
            v
        };
        let data = match req.get(1) {
            Some(0x01) => text("XRobot"),
            Some(0x02) => text("XR-DAP-SIM"),
            Some(0x03) => text(SERIAL),
            Some(0x04) => text("2.0.0"),
            Some(0x09) => text("0.0.0"),
            Some(0xFE) => vec![1],
            Some(0xFF) => (PACKET_SIZE as u16).to_le_bytes().to_vec(),
            _ => Vec::new(),
        };
        let mut out = vec![0x00, data.len() as u8];
        out.extend(data);
        out
    }

    fn dap_connect(&mut self, transport: u32, req: &[u8]) -> Vec<u8> {
        // 端口 0（默认）和 1 是 SWD；只支持 SWD
        match req.get(1) {
            Some(0 | 1) => {
                self.connected = Some(transport);
                vec![0x02, 0x01]
            }
            _ => vec![0x02, 0x00],
        }
    }

    /// DAP_Transfer 只模拟 DP 寄存器：DPIDR、CTRL/STAT（上电请求立即应答）、SELECT、RDBUFF。
    /// AP 访问回 FAULT。
    fn dap_transfer(&mut self, req: &[u8]) -> Vec<u8> {
        let count = req.get(2).copied().unwrap_or(0) as usize;
        let mut pos = 3;
        let mut data = Vec::new();
        let mut done = 0u8;
        let mut ack = 1u8;
        for _ in 0..count {
            let Some(&r) = req.get(pos) else {
                break;
            };
            pos += 1;
            let (ap, read, a) = (r & 1 != 0, r & 2 != 0, r & 0x0C);
            if ap {
                ack = 4;
                break;
            }
            if read {
                let v = match a {
                    0x00 => DPIDR,
                    0x04 => self.ctrl_stat,
                    _ => 0,
                };
                data.extend_from_slice(&v.to_le_bytes());
            } else {
                let Some(v) = req.get(pos..pos + 4) else {
                    break;
                };
                pos += 4;
                if a == 0x04 {
                    // CSYSPWRUPREQ、CDBGPWRUPREQ 立即得到 ACK
                    let v = u32_at(v, 0);
                    self.ctrl_stat = v | (v & 0x5000_0000) << 1;
                }
            }
            done += 1;
        }
        let mut out = vec![0x05, done, ack];
        out.extend(data);
        out
    }

    // ------------------------------------------------------------- 厂商命令

    fn info(&self, req: &[u8]) -> Vec<u8> {
        if req.len() != 1 {
            return vec![0x80, ARG];
        }
        let mut out = vec![0x80, OK, 2];
        out.extend_from_slice(&self.features.to_le_bytes());
        out.extend_from_slice(&(MAX_ITEMS as u16).to_le_bytes());
        out.push(8);
        out.extend_from_slice(&(self.ring_bytes as u32).to_le_bytes());
        out.extend_from_slice(&self.now_us().to_le_bytes());
        out
    }

    /// READ、WRITE 共用的解析：`with_value` 为 true 时每个条目后面跟 size 字节的值。
    fn parse_items(req: &[u8], with_value: bool) -> Result<Vec<(Item, Vec<u8>)>, u8> {
        let count = *req.get(1).ok_or(ARG)? as usize;
        if count == 0 {
            return Err(ARG);
        }
        let mut p = &req[2..];
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            let it = take_item(&mut p).ok_or(ARG)?;
            if !item_valid(&it) {
                return Err(ARG);
            }
            let n = if with_value { it.size as usize } else { 0 };
            if p.len() < n {
                return Err(ARG);
            }
            let value = p[..n].to_vec();
            p = &p[n..];
            items.push((it, value));
        }
        if !p.is_empty() {
            return Err(ARG);
        }
        if count > MAX_ITEMS {
            return Err(RESOURCE);
        }
        Ok(items)
    }

    fn cmd_read(&mut self, req: &[u8]) -> Vec<u8> {
        let items = match Self::parse_items(req, false) {
            Ok(i) => i,
            Err(s) => return vec![0x81, s],
        };
        let resp_len = 3 + items
            .iter()
            .map(|(i, _)| 3 + i.size as usize)
            .sum::<usize>();
        if resp_len > PACKET_SIZE {
            return vec![0x81, RESOURCE];
        }
        if self.connected.is_none() {
            return vec![0x81, NO_TARGET];
        }
        let now = self.now_us();
        let mut out = vec![0x81, OK, items.len() as u8];
        for (it, _) in &items {
            let (status, value) = self.read_item(it, now);
            out.extend_from_slice(&it.id.to_le_bytes());
            out.push(status);
            out.extend(value);
        }
        out
    }

    fn cmd_write(&mut self, req: &[u8]) -> Vec<u8> {
        let items = match Self::parse_items(req, true) {
            Ok(i) => i,
            Err(s) => return vec![0x82, s],
        };
        if self.connected.is_none() {
            return vec![0x82, NO_TARGET];
        }
        let now = self.now_us();
        let mut out = vec![0x82, OK, items.len() as u8];
        for (it, value) in &items {
            let status = match self.resolve(it, now) {
                Ok(addr) if self.write_mem(addr, value) => OK,
                Ok(_) => 0x1F,
                Err(s) => s,
            };
            out.extend_from_slice(&it.id.to_le_bytes());
            out.push(status);
        }
        out
    }

    /// 0x83 SUB_START：一条命令带全部配置，检查通过就开始采样，第 0 拍立即采（协议文档 3.4 节）。
    fn sub_start(&mut self, transport: u32, req: &[u8]) -> Vec<u8> {
        if self.active {
            return vec![0x83, STATE];
        }
        if req.len() < 8 {
            return vec![0x83, ARG];
        }
        let period_us = u32_at(req, 1);
        let flags = req[5];
        let count = u16_at(req, 6) as usize;
        // flags：bit0 缓冲满时丢新帧，bit1 紧凑帧（功能位 bit6 为 1 时才能用），其余位必须为 0
        let compact = flags & 2 != 0;
        if period_us == 0
            || flags & !3 != 0
            || count == 0
            || (compact && self.features & FEATURE_COMPACT == 0)
        {
            return vec![0x83, ARG];
        }
        let mut p = &req[8..];
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(item) = take_item(&mut p) else {
                return vec![0x83, ARG];
            };
            if p.len() < 4 {
                return vec![0x83, ARG];
            }
            let (div, phase) = (u16_at(p, 0), u16_at(p, 2));
            p = &p[4..];
            if !item_valid(&item) || div == 0 || phase >= div {
                return vec![0x83, ARG];
            }
            items.push(SubItem { item, div, phase });
        }
        if !p.is_empty() {
            return vec![0x83, ARG];
        }
        let mut aps: Vec<u8> = items.iter().map(|s| s.item.ap).collect();
        aps.sort_unstable();
        aps.dedup();
        // 所有条目都到期的一帧加 2 字节长度放不进缓冲时资源不足
        let full_frame =
            10 + count.div_ceil(8) + items.iter().map(|s| s.item.size as usize).sum::<usize>();
        if count > MAX_ITEMS || aps.len() > 8 || full_frame + 2 > self.ring_bytes {
            return vec![0x83, RESOURCE];
        }
        if self.connected.is_none() {
            return vec![0x83, NO_TARGET];
        }
        self.sampling_link = transport;
        self.sub = Some(Subscription {
            items,
            period_us,
            drop_new: flags & 1 != 0,
            compact,
        });
        self.clear_ring();
        self.active = true;
        self.start_us = self.now_us();
        self.next_tick = 0;
        self.advance();
        vec![0x83, OK]
    }

    fn clear_ring(&mut self) {
        self.ring.clear();
        self.dropped = 0;
        self.skipped = 0;
    }

    fn stop_sampling(&mut self) {
        self.advance();
        self.active = false;
    }

    fn sub_ctrl(&mut self, req: &[u8]) -> Vec<u8> {
        if req.len() != 2 {
            return vec![0x84, ARG];
        }
        let status = match req[1] {
            0 => {
                self.stop_sampling();
                OK
            }
            2 => {
                self.clear_ring();
                OK
            }
            _ => ARG,
        };
        vec![0x84, status]
    }

    /// 补齐到现在为止到期的节拍（协议文档 3.5 节）。
    fn advance(&mut self) {
        if !self.active {
            return;
        }
        let Some(sub) = self.sub.take() else {
            return;
        };
        let now = self.now_us();
        loop {
            let k = self.next_tick;
            let at = self.start_us + k * sub.period_us as u64;
            if at > now {
                break;
            }
            self.next_tick += 1;
            if self
                .skip_every
                .is_some_and(|n| k > 0 && k.is_multiple_of(n))
            {
                self.skipped += 1;
                continue;
            }
            let due: Vec<&SubItem> = sub
                .items
                .iter()
                .filter(|s| (k + s.phase as u64).is_multiple_of(s.div as u64))
                .collect();
            if due.is_empty() {
                continue;
            }
            let mut frame = (k as u32).to_le_bytes().to_vec();
            frame.extend_from_slice(&at.to_le_bytes()[..6]);
            let mut bitmap = vec![0u8; due.len().div_ceil(8)];
            let mut values = Vec::new();
            for (j, s) in due.iter().enumerate() {
                let (status, v) = self.read_item(&s.item, at);
                if status != OK {
                    bitmap[j / 8] |= 1 << (j % 8);
                }
                values.extend(v);
            }
            frame.extend(bitmap);
            frame.extend(values);
            self.push_frame(frame, sub.drop_new);
        }
        self.sub = Some(sub);
    }

    /// 写进环形缓冲：每帧另占 2 字节长度；满了按 flags.bit0 覆盖最旧的帧或丢掉新帧。
    fn push_frame(&mut self, frame: Vec<u8>, drop_new: bool) {
        let cost = |f: &Vec<u8>| f.len() + 2;
        let used = |r: &VecDeque<Vec<u8>>| r.iter().map(cost).sum::<usize>();
        if cost(&frame) > self.ring_bytes {
            self.dropped += 1;
            return;
        }
        while used(&self.ring) + cost(&frame) > self.ring_bytes {
            if drop_new {
                self.dropped += 1;
                return;
            }
            self.ring.pop_front();
            self.dropped += 1;
        }
        self.ring.push_back(frame);
    }

    fn sub_read(&mut self, req: &[u8]) -> Vec<u8> {
        if req.len() != 3 {
            return vec![0x85, ARG];
        }
        let limit = (u16_at(req, 1) as usize).min(PACKET_SIZE - 24);
        let compact = self.sub.as_ref().is_some_and(|sub| sub.compact);
        let mut frames = Vec::new();
        let mut n = 0u16;
        // 回复里上一帧的节拍序号和时间戳：每个回复的第 1 帧是完整帧，单独解码
        let mut prev = None;
        while let Some(f) = self.ring.front() {
            let encoded = match prev {
                Some(prev) if compact => self.compact_frame(f, prev),
                _ => f.clone(),
            };
            if frames.len() + encoded.len() > limit {
                break;
            }
            prev = Some(frame_head(f));
            frames.extend(encoded);
            self.ring.pop_front();
            n += 1;
        }
        // 剩余字节数按完整帧计，紧凑帧也一样
        let remaining: usize = self.ring.iter().map(Vec::len).sum();
        let mut out = vec![0x85, if self.active { OK } else { STATE }];
        out.extend_from_slice(&self.now_us().to_le_bytes());
        out.extend_from_slice(&(remaining as u32).to_le_bytes());
        out.extend_from_slice(&self.dropped.to_le_bytes());
        out.extend_from_slice(&self.skipped.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend(frames);
        out
    }

    /// 按紧凑帧编码缓冲里的一帧（协议文档 3.5 节），`prev` 是回复里上一帧的节拍序号和时间戳：
    /// `h = (Δk << 1) | f`、Δt，有条目失败（f 为 1）时带位图，然后是值。
    fn compact_frame(&self, frame: &[u8], prev: (u32, u64)) -> Vec<u8> {
        let (k, stamp) = frame_head(frame);
        let due = self.sub.as_ref().map_or(0, |sub| {
            sub.items
                .iter()
                .filter(|s| (k as u64 + s.phase as u64).is_multiple_of(s.div as u64))
                .count()
        });
        let bitmap = &frame[10..10 + due.div_ceil(8)];
        let failed = bitmap.iter().any(|&b| b != 0);
        let mut out = Vec::new();
        put_varint(
            &mut out,
            u64::from(k.wrapping_sub(prev.0)) << 1 | u64::from(failed),
        );
        put_varint(&mut out, stamp.wrapping_sub(prev.1) & STAMP_MASK);
        if failed {
            out.extend_from_slice(bitmap);
        }
        out.extend_from_slice(&frame[10 + bitmap.len()..]);
        out
    }

    /// 0x86 Wi-Fi 配置（协议文档 3.7 节）。模拟探针没有网络，保存的热点不会连上。
    fn wifi(&mut self, req: &[u8]) -> Vec<u8> {
        match req.get(1) {
            Some(0) if req.len() == 2 => {
                let mut out = vec![0x86, OK, u8::from(!self.wifi_ssid.is_empty())];
                out.extend_from_slice(&[0, 0, 0, 0, 0]);
                out.push(self.wifi_ssid.len() as u8);
                out.extend_from_slice(&self.wifi_ssid);
                // 最近一次清除的来源；本次复位原因：上电
                out.extend_from_slice(&[self.wifi_cleared_by, 1]);
                out
            }
            Some(1) => {
                let ssid_len = req.get(2).copied().unwrap_or(0) as usize;
                let pw_len = req.get(3 + ssid_len).copied().unwrap_or(0) as usize;
                let valid = req.len() == 4 + ssid_len + pw_len
                    && (1..=32).contains(&ssid_len)
                    && (pw_len == 0 || (8..=63).contains(&pw_len));
                if !valid {
                    return vec![0x86, ARG];
                }
                self.wifi_ssid = req[3..3 + ssid_len].to_vec();
                vec![0x86, OK]
            }
            // 清除要带确认字节 0xA5
            Some(2) if req == [0x86, 2, 0xA5] => {
                self.wifi_ssid.clear();
                self.wifi_cleared_by = 3;
                vec![0x86, OK]
            }
            _ => vec![0x86, ARG],
        }
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

    /// 协议文档 3.8 节 SUB_START 例子的两个条目，打开紧凑帧；缓冲里放 `frames`（完整帧）。
    fn with_frames(frames: &[&str]) -> Sim {
        let sim = Sim::new();
        {
            let mut st = sim.st();
            let item = |id, size, base, div, phase| SubItem {
                item: Item {
                    ap: 0,
                    id,
                    size,
                    base,
                    derefs: Vec::new(),
                    final_off: 0,
                },
                div,
                phase,
            };
            st.sub = Some(Subscription {
                items: vec![
                    item(0x10, 4, 0x2000_0100, 1, 0),
                    item(0x11, 2, 0x2000_0104, 10, 3),
                ],
                period_us: 1000,
                drop_new: false,
                compact: true,
            });
            st.ring = frames.iter().map(|f| unhex(f)).collect();
        }
        sim
    }

    /// SUB_READ 回复的剩余字节数、帧数和帧。
    fn read(sim: &Sim, max_bytes: u16) -> (u32, u16, Vec<u8>) {
        let mut req = vec![0x85];
        req.extend_from_slice(&max_bytes.to_le_bytes());
        let r = sim.st().sub_read(&req);
        (u32_at(&r, 10), u16_at(&r, 22), r[24..].to_vec())
    }

    const K6: &str = "06000000 B0590F000000 00 07000000";
    const K7: &str = "07000000 985D0F000000 00 08000000 3412";

    /// 协议文档 3.8 节 SUB_READ 的紧凑帧例子，逐字节。
    #[test]
    fn compact_encoding_matches_the_document() {
        let (remaining, n, frames) = read(&with_frames(&[K6, K7]), 1024);
        assert_eq!((remaining, n), (0, 2));
        assert_eq!(
            frames,
            unhex("06000000 B0590F000000 00 07000000 02 E807 08000000 3412")
        );
    }

    /// 回复的第 1 帧总是完整帧，上一个回复的帧不算。
    #[test]
    fn every_reply_starts_with_a_whole_frame() {
        let sim = with_frames(&[K6, K7]);
        assert_eq!(read(&sim, 15), (17, 1, unhex(K6)));
        assert_eq!(read(&sim, 1024), (0, 1, unhex(K7)));
    }

    /// 帧的总长按编码后的长度算，不超过 max_bytes；剩余字节数按完整帧计。
    #[test]
    fn limit_counts_encoded_bytes_and_remaining_counts_whole_frames() {
        let (remaining, n, _) = read(&with_frames(&[K6, K7]), 23);
        assert_eq!((remaining, n), (17, 1));
        let (remaining, n, _) = read(&with_frames(&[K6, K7]), 24);
        assert_eq!((remaining, n), (0, 2));
    }

    /// f 为 1 时带位图；跳拍时 Δk 大于 1；时间戳按 48 位回绕。
    #[test]
    fn compact_failure_gap_and_wrap() {
        let sim = with_frames(&[
            "06000000 FBFFFFFFFFFF 00 07000000",
            // k=7，两个条目到期，第 2 个失败
            "07000000 030000000000 02 08000000 0000",
            // k=17，Δk 10，Δt 11000（F8 55）
            "11000000 FB2A00000000 00 09000000 3412",
        ]);
        let (_, n, frames) = read(&sim, 1024);
        assert_eq!(n, 3);
        assert_eq!(
            frames,
            unhex(
                "06000000 FBFFFFFFFFFF 00 07000000
                 03 08 02 08000000 0000
                 14 F855 09000000 3412"
            )
        );
    }

    /// 协议文档 3.4 节：功能位没有 bit6 时紧凑帧标志回 0x01，flags 的保留位也回 0x01；INFO 报的
    /// 功能位就是设的值。
    #[test]
    fn features_gate_compact_frames() {
        let sim = Sim::new();
        let mut link = sim.link();
        link.send(&[0x02, 0x01]).unwrap();
        link.recv(Duration::ZERO).unwrap();
        sim.set_features(0x1F);
        let mut start = unhex("83 E8030000 02 0100 00 1000 04 00010020 00 0000 0100 0000");
        link.send(&start).unwrap();
        assert_eq!(link.recv(Duration::ZERO).unwrap(), [0x83, ARG]);
        link.send(&[0x80]).unwrap();
        assert_eq!(u32_at(&link.recv(Duration::ZERO).unwrap(), 3), 0x1F);
        sim.set_features(FEATURES);
        start[5] = 4;
        link.send(&start).unwrap();
        assert_eq!(link.recv(Duration::ZERO).unwrap(), [0x83, ARG]);
        start[5] = 2;
        link.send(&start).unwrap();
        assert_eq!(link.recv(Duration::ZERO).unwrap(), [0x83, OK]);
    }
}
