//! 对照模拟探针的端到端测试。判据来自 XRDAP-Core 的协议文档、CMSIS-DAP 规范、ARMv7-M 架构
//! 手册和 tests/fixtures/types.c 里的初值，不来自本库自己的实现。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use xrdap::dap::Probe;
use xrdap::expr::{Resolver, Target};
use xrdap::raw::vendor::{self, Item, SubItem};
use xrdap::types::SymbolTable;
use xrdap::watch::{Lag, Sampler, Var};
use xrdap_sim::Sim;

const TIMEOUT: Duration = Duration::from_secs(2);

fn elf_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/types.elf")
}

/// 装好 types.elf 初值的模拟探针、连着它的 Probe 和符号表。
fn setup() -> (Sim, Probe, SymbolTable) {
    let sim = Sim::new();
    sim.load_elf(&std::fs::read(elf_path()).unwrap());
    let probe = Probe::new(Box::new(sim.link()), TIMEOUT);
    (sim, probe, xrdap::elf::load(&elf_path()).unwrap())
}

fn targets(syms: &mut SymbolTable, exprs: &[&str]) -> Vec<Target> {
    let mut r = Resolver::new(syms);
    exprs.iter().map(|e| r.resolve(e).unwrap()).collect()
}

fn items(t: &[Target]) -> Vec<Item> {
    t.iter()
        .enumerate()
        .map(|(i, t)| t.item(i as u16))
        .collect()
}

fn addr(syms: &mut SymbolTable, name: &str) -> u32 {
    targets(syms, &[name])[0].direct_addr().unwrap()
}

/// 协议文档 3.1：DAP_Connect 之前 READ 回 0x04。
#[test]
fn vendor_commands_need_connect() {
    let (_sim, mut probe, mut syms) = setup();
    let counter = Item::direct(1, 4, addr(&mut syms, "counter"));
    let resp = probe
        .xfer(&vendor::read_request(std::slice::from_ref(&counter)))
        .unwrap();
    assert_eq!(resp, [0x81, 0x04]);
    assert!(probe.read(std::slice::from_ref(&counter)).is_err());
    let target = probe.connect(4_000_000).unwrap();
    assert_eq!(target.dpidr, xrdap_sim::DPIDR);
    assert!(target.describe().contains("ARM"));
    let r = probe.read(&[counter]).unwrap();
    assert_eq!(r[0].value, 12345u32.to_le_bytes());
    let info = probe.vendor_info().unwrap();
    assert_eq!((info.version, info.max_items, info.max_depth), (2, 32, 8));
}

/// 协议文档 3.2、3.3 的条目状态：空指针 0x30+k、读指针出错 0x10+k、指针地址不对齐 0x20+k、
/// 最终地址不对齐 0x2F、最后的读写出错 0x1F。
#[test]
fn read_item_status_per_protocol() {
    let (_sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    let t = targets(
        &mut syms,
        &[
            "s8",
            "big",
            "***(uint32_t***)chain_p1",
            "*null_p1",
            "*bad_p1",
            "motor[1].pid->out",
        ],
    );
    let r = probe.read(&items(&t)).unwrap();
    let text: Vec<String> = t
        .iter()
        .zip(&r)
        .map(|(t, r)| t.ty.format(&r.value))
        .collect();
    assert_eq!(text[0], "-7 (0xF9)");
    assert_eq!(text[1], "1234605616436508552 (0x1122334455667788)");
    assert_eq!(text[2], "3405691582 (0xCAFEBABE)");
    assert_eq!((r[3].status, r[4].status), (0x30, 0x1F));
    assert_eq!(text[5], "-3.75");
    assert!(vendor::item_status_text(r[3].status, &t[3].chain).contains("null_p1"));

    let null_p1 = addr(&mut syms, "null_p1");
    let bad_p1 = addr(&mut syms, "bad_p1");
    let counter = addr(&mut syms, "counter");
    let via = |id, base, derefs: Vec<i16>| Item {
        derefs,
        ..Item::direct(id, 4, base)
    };
    let cases = [
        (via(0x10, null_p1, vec![0]), 0x30),
        (via(0x11, 0x6000_0000, vec![0]), 0x10),
        (via(0x12, bad_p1, vec![1]), 0x20),
        (
            Item {
                final_off: 1,
                ..Item::direct(0x13, 4, counter)
            },
            0x2F,
        ),
        (via(0x15, bad_p1, vec![0, 0]), 0x11),
        (via(0x16, bad_p1, vec![0]), 0x1F),
        (Item::direct(0x14, 4, counter), 0x00),
    ];
    let list: Vec<Item> = cases.iter().map(|(i, _)| i.clone()).collect();
    for ((_, want), got) in cases.iter().zip(probe.read(&list).unwrap()) {
        assert_eq!(got.status, *want, "id {:#x}", got.id);
    }
}

#[test]
fn write_then_read_back() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    let t = targets(
        &mut syms,
        &["s8", "state.pair.hi", "big", "block[0]", "*chain_p3"],
    );
    let values = ["0x5A", "0xBEAF", "0x8877665544332211", "0xDEADBEEF", "7"];
    let list: Vec<(Item, Vec<u8>)> = t
        .iter()
        .zip(values)
        .enumerate()
        .map(|(i, (t, v))| (t.item(i as u16), t.ty.parse(v).unwrap()))
        .collect();
    let r = probe.write(&list).unwrap();
    assert!(r.iter().all(|r| r.status == 0), "{r:?}");
    assert_eq!(sim.peek(addr(&mut syms, "s8"), 1), [0x5A]);
    assert_eq!(
        sim.peek(addr(&mut syms, "chain_final"), 4),
        7u32.to_le_bytes()
    );
    let back = probe.read(&items(&t)).unwrap();
    assert_eq!(back[2].value, 0x8877_6655_4433_2211u64.to_le_bytes());
    // Flash 只读：最后的写入出错，0x1F
    let flash = Item::direct(9, 4, 0x0800_0000);
    assert_eq!(probe.write(&[(flash, vec![0; 4])]).unwrap()[0].status, 0x1F);
}

/// 协议文档 3.4：一次 READ 最多 32 个条目，多的分成几次。
#[test]
fn many_items_split_into_several_reads() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    let block = addr(&mut syms, "block[0]");
    let list: Vec<Item> = (0..40)
        .map(|i| Item::direct(i, 4, block + 4 * (i as u32 % 8)))
        .collect();
    let before = sim.requests().len();
    let r = probe.read(&list).unwrap();
    assert_eq!(r.len(), 40);
    assert_eq!(sim.requests().len() - before, 2);
    assert_eq!(r[3].value, 0x4444_4444u32.to_le_bytes());
}

fn sub(t: &Target, id: u16, div: u16, phase: u16) -> SubItem {
    SubItem {
        item: t.item(id),
        div,
        phase,
    }
}

/// 固定相位的采样条目。
fn var(s: &SubItem) -> Var {
    Var {
        item: s.item.clone(),
        div: s.div,
        phase: Some(s.phase),
    }
}

fn vars(list: &[SubItem]) -> Vec<Var> {
    list.iter().map(var).collect()
}

/// 协议文档 3.5：第 k 拍的目标时间是开始时间 + k × 周期；`(k + phase) % div == 0` 的条目到期。
#[test]
fn sampling_with_divided_items() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    sim.add_counter(addr(&mut syms, "counter"), 4, 1000);
    let t = targets(
        &mut syms,
        &[
            "counter",
            "state.count",
            "big",
            "s8",
            "***(uint32_t***)chain_p1",
        ],
    );
    let mut list = vec![
        sub(&t[0], 0, 1, 0),
        sub(&t[1], 1, 10, 0),
        sub(&t[2], 2, 5, 0),
        sub(&t[3], 3, 20, 0),
        sub(&t[4], 4, 20, 0),
    ];
    vendor::assign_phases(&mut list, &[false; 5]);
    assert_ne!(list[3].phase, list[4].phase);
    let mut s = Sampler::start(&mut probe, vars(&list), 1000, false).unwrap();
    let mut frames = Vec::new();
    let end = Instant::now() + Duration::from_millis(300);
    while Instant::now() < end {
        frames.extend(s.poll().unwrap().0);
        std::thread::sleep(Duration::from_millis(5));
    }
    frames.extend(s.stop().unwrap());
    let stats = s.stats();
    drop(s);
    assert!(frames.len() > 200, "{} 帧", frames.len());
    assert_eq!(
        (
            stats.missing,
            stats.dropped,
            stats.skipped,
            stats.failed_values
        ),
        (0, 0, 0, 0)
    );
    for w in frames.windows(2) {
        assert_eq!(w[1].tick, w[0].tick + 1);
        assert_eq!(w[1].probe_us - w[0].probe_us, 1000);
    }
    for f in &frames {
        for (i, item) in list.iter().enumerate() {
            assert_eq!(
                f.values[i].is_some(),
                item.due(f.tick),
                "k={} 条目 {i}",
                f.tick
            );
        }
        if let Some(Some(v)) = &f.values[1] {
            assert_eq!(v, &123456u32.to_le_bytes());
        }
        if let Some(Some(v)) = &f.values[4] {
            assert_eq!(v, &0xCAFE_BABEu32.to_le_bytes());
        }
    }
    // 计数器按探针时间递增
    let counts: Vec<u32> = frames
        .iter()
        .map(|f| {
            u32::from_le_bytes(
                f.values[0].clone().unwrap().unwrap()[..4]
                    .try_into()
                    .unwrap(),
            )
        })
        .collect();
    assert!(counts.windows(2).all(|w| w[1] == w[0] + 1), "{counts:?}");
    // 停止后标准命令又能用了（协议文档 3.1：采样期间标准命令回 FF）
    assert_eq!(probe.xfer(&[0x02, 0x01]).unwrap(), [0x02, 0x01]);
}

/// 协议文档 3.5：跳拍计数、丢帧计数，以及读取失败的条目在位图里置 1。
#[test]
fn sampling_counters_and_gaps() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    sim.skip_every(Some(10));
    sim.set_ring_bytes(400);
    let t = targets(&mut syms, &["counter", "*null_p1"]);
    let list = vec![sub(&t[0], 0, 1, 0), sub(&t[1], 1, 1, 0)];
    let mut s = Sampler::start(&mut probe, vars(&list), 500, false).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let mut frames = Vec::new();
    loop {
        let (f, remaining) = s.poll().unwrap();
        frames.extend(f);
        if remaining == 0 {
            break;
        }
    }
    let st = s.stats();
    assert!(st.skipped > 0, "{st:?}");
    assert!(st.dropped > 0, "{st:?}");
    assert!(st.missing > 0, "{st:?}");
    assert_eq!(st.failed_values, frames.len() as u64);
    assert!(frames.iter().all(|f| matches!(f.values[1], Some(None))));
    // 跳拍说明 SWD 跟不上（协议文档 3.5 节的建议表）
    assert_eq!(st.lag(), Some(Lag::Swd));
    // Sampler 销毁时停止采样
    drop(s);
    assert!(!sim.is_sampling());
}

/// 协议文档 3.4、3.5：探针不估计耗时、不拒绝短周期；跟不跟得上只看运行时的计数器。
/// 只丢帧不跳拍时是读取跟不上。
#[test]
fn keeping_up_is_judged_from_counters() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    sim.set_ring_bytes(100);
    let t = targets(&mut syms, &["counter", "big"]);
    let list = vec![sub(&t[0], 0, 1, 0), sub(&t[1], 1, 1, 0)];
    let mut s = Sampler::start(&mut probe, vars(&list), 1, false).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    s.poll().unwrap();
    let st = s.stats();
    assert_eq!(st.skipped, 0, "{st:?}");
    assert!(st.dropped > 0, "{st:?}");
    assert_eq!(st.lag(), Some(Lag::Read));
}

/// 协议文档第 2 节：TCP 帧；没有响应的命令（DAP_TransferAbort）不回帧。
#[test]
fn tcp_transport_round_trip() {
    let (sim, _, mut syms) = setup();
    let host = sim.serve_tcp().to_string();
    let t = xrdap::raw::transport::tcp::TcpTransport::connect(&host, TIMEOUT).unwrap();
    let mut probe = Probe::new(Box::new(t), TIMEOUT);
    assert!(probe.describe().starts_with("TCP 127.0.0.1:"));
    probe.connect(4_000_000).unwrap();
    let start = Instant::now();
    assert!(probe.xfer(&[0x07]).unwrap().is_empty());
    assert!(start.elapsed() < Duration::from_millis(500));
    let r = probe
        .read(&[Item::direct(1, 2, addr(&mut syms, "state.pair.hi"))])
        .unwrap();
    assert_eq!(r[0].value, 0x2222u16.to_le_bytes());
}

/// ARMv7-M C1.6：C_HALT 停住内核、清零后继续；SYSRESETREQ 系统复位，DEMCR.VC_CORERESET 为 1
/// 时停在复位向量；S_RESET_ST 在复位后第一次读 DHCSR 时为 1。
#[test]
fn halt_resume_and_reset() {
    use xrdap::cortex::{self, AfterReset, CoreState};
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    let counter = addr(&mut syms, "counter");
    sim.add_counter(counter, 4, 1000);
    let read = |probe: &mut Probe| -> u32 { cortex::read_u32(probe, counter).unwrap() };

    assert_eq!(
        cortex::info(&mut probe).unwrap().describe(),
        "Cortex-M4 r0p1"
    );
    assert_eq!(
        cortex::status(&mut probe).unwrap().state,
        CoreState::Running
    );

    // 停住后计数器不走
    assert_eq!(cortex::halt(&mut probe).unwrap().state, CoreState::Halted);
    assert!(sim.is_halted());
    let a = read(&mut probe);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(read(&mut probe), a);

    // 继续后计数器接着走
    assert_ne!(cortex::resume(&mut probe).unwrap().state, CoreState::Halted);
    std::thread::sleep(Duration::from_millis(20));
    assert!(read(&mut probe) > a);

    // 复位后停在复位向量：计数器回到 0 且不走，DEMCR 恢复原值
    let s = cortex::reset(&mut probe, AfterReset::Halt).unwrap();
    assert_eq!(s.state, CoreState::Halted);
    assert_eq!(read(&mut probe), 0);
    assert_eq!(sim.demcr() & 1, 0);

    // 复位后运行：计数器从 0 重新开始
    std::thread::sleep(Duration::from_millis(30));
    assert_ne!(
        cortex::reset(&mut probe, AfterReset::Run).unwrap().state,
        CoreState::Halted
    );
    let t = read(&mut probe);
    assert!(t < 25, "复位后计数应从 0 开始，读到 {t}");
}

/// 协议文档 3.7.1：探针报了控制口时，Wi-Fi 的查询、保存、清除都走控制口，不发 0x86；不需要
/// 连接，采样期间也能用。
#[test]
fn wifi_goes_through_the_control_port() {
    let (sim, mut probe, mut syms) = setup();
    assert!(probe.wifi_status().unwrap().ssid.is_empty());
    probe.wifi_set("lab", "12345678").unwrap();
    assert_eq!(sim.wifi_ssid(), "lab");
    assert_eq!(
        sim.control_requests()[1],
        b"\x01\x03lab\x0812345678".to_vec()
    );
    probe.connect(4_000_000).unwrap();
    let t = targets(&mut syms, &["counter"]);
    probe.sub_start(1000, 0, &[sub(&t[0], 0, 1, 0)]).unwrap();
    assert_eq!(probe.wifi_status().unwrap().ssid, "lab");
    assert!(sim.is_sampling());
    probe.wifi_clear().unwrap();
    assert!(sim.wifi_ssid().is_empty());
    assert_eq!(sim.control_requests().last().unwrap(), &[0x02, 0xA5]);
    assert!(sim.requests().iter().all(|r| r[0] != 0x86));
}

fn tcp_probe(sim: &Sim) -> Probe {
    let host = sim.serve_tcp().to_string();
    let t = xrdap::raw::transport::tcp::TcpTransport::connect(&host, TIMEOUT).unwrap();
    Probe::new(Box::new(t), TIMEOUT)
}

/// 协议文档第 2 节、3.7.1：TCP 上控制请求是帧类型 3，应答是类型 4，和 DAP 帧在同一条连接上。
#[test]
fn wifi_over_tcp_uses_control_frames() {
    let (sim, _, _) = setup();
    let mut probe = tcp_probe(&sim);
    probe.wifi_set("lab", "12345678").unwrap();
    assert_eq!(probe.wifi_status().unwrap().ssid, "lab");
    probe.connect(4_000_000).unwrap();
    probe.wifi_clear().unwrap();
    assert!(sim.wifi_ssid().is_empty());
    assert_eq!(sim.control_requests().len(), 3);
    assert!(sim.requests().iter().all(|r| r[0] != 0x86));
}

/// 协议文档第 2 节、3.7.1：TCP 传输的控制口。探针没有的操作回状态 0xF2；之后 DAP 帧照常配对；
/// 空请求是帧头无效，连接被关闭。
#[test]
fn tcp_transport_control_frames() {
    use xrdap::raw::transport::{tcp::TcpTransport, Transport};
    let (sim, _, _) = setup();
    let host = sim.serve_tcp().to_string();
    let mut t = TcpTransport::connect(&host, TIMEOUT).unwrap();
    assert_eq!(t.control(&[0x7E]).unwrap(), Some(vec![0x7E, 0xF2]));
    t.send(&[0x00, 0xFF]).unwrap();
    assert_eq!(t.recv(TIMEOUT).unwrap()[..2], [0x00, 0x02]);
    assert!(matches!(t.control(&[]), Err(xrdap::Error::Io(_))));
}

/// 协议文档 3.7.1：探针没有 Wi-Fi 操作时（功能位 0x6F）应答状态 0xF2，不再试 0x86。
#[test]
fn wifi_on_a_probe_without_wifi_reports_no_such_operation() {
    let (sim, link, _) = setup();
    sim.set_features(0x6F);
    for mut probe in [link, tcp_probe(&sim)] {
        match probe.wifi_status() {
            Err(xrdap::Error::Status {
                cmd: 0x86,
                status: 0xF2,
                text,
            }) => assert!(text.contains("探针没有这个设置操作"), "{text}"),
            other => panic!("{other:?}"),
        }
    }
    assert!(sim.requests().iter().all(|r| r[0] != 0x86));
}

/// 协议文档 3.4：INFO 的 bit5 为 0 的固件不发控制请求，走 0x86；TCP 上它收到类型 3 的帧会关连接。
#[test]
fn wifi_without_the_control_bit_uses_the_dap_command() {
    let (sim, _, _) = setup();
    sim.set_features(0x1F);
    let mut probe = tcp_probe(&sim);
    probe.wifi_set("lab", "12345678").unwrap();
    assert_eq!(probe.wifi_status().unwrap().ssid, "lab");
    assert!(sim.control_requests().is_empty());
    assert_eq!(sim.requests().iter().filter(|r| r[0] == 0x86).count(), 2);
}

/// 协议文档 3.7.1：请求超过 EP0 的 OUT 缓冲时 SEND 被拒，Wi-Fi 保存回退到 0x86；放得下的照走
/// 控制口。
#[test]
fn wifi_falls_back_to_the_dap_command_when_send_is_refused() {
    let (sim, mut probe, _) = setup();
    sim.set_control_limit(64);
    let (ssid, password) = ("s".repeat(32), "p".repeat(63));
    probe.wifi_set(&ssid, &password).unwrap();
    assert_eq!(sim.wifi_ssid(), ssid);
    assert!(sim.control_requests().is_empty());
    assert_eq!(sim.requests().last().unwrap()[0], 0x86);
    assert_eq!(probe.wifi_status().unwrap().ssid, ssid);
    assert_eq!(sim.control_requests(), [[0x00]]);
}

/// 协议文档 3.1：没有 0x86 的固件回 `FF`，报告不支持。
#[test]
fn wifi_on_old_firmware_without_wifi_is_reported() {
    let (sim, mut probe, _) = setup();
    sim.set_features(0x0F);
    let e = probe.wifi_status().unwrap_err().to_string();
    assert!(e.contains("探针不支持 Wi-Fi 配置命令"), "{e}");
}

/// 最近一次 SUB_START 的 flags 字节。
fn start_flags(sim: &Sim) -> u8 {
    sim.requests()
        .iter()
        .rfind(|r| r[0] == vendor::CMD_SUB_START)
        .unwrap()[5]
}

/// 协议文档 3.4、3.5：探针报了 bit6 时 Sampler 要紧凑帧；到期条目、失败、跳拍、时间戳和值都解得对。
#[test]
fn sampler_uses_compact_frames_when_offered() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    sim.add_counter(addr(&mut syms, "counter"), 4, 1000);
    sim.skip_every(Some(5));
    let t = targets(&mut syms, &["counter", "state.count", "*null_p1"]);
    let list = vec![
        sub(&t[0], 0, 1, 0),
        sub(&t[1], 1, 3, 0),
        sub(&t[2], 2, 2, 1),
    ];
    let mut s = Sampler::start(&mut probe, vars(&list), 1000, false).unwrap();
    assert_eq!(start_flags(&sim), vendor::SUB_FLAG_COMPACT);
    let mut frames = Vec::new();
    let end = Instant::now() + Duration::from_millis(60);
    while Instant::now() < end {
        frames.extend(s.poll().unwrap().0);
        std::thread::sleep(Duration::from_millis(5));
    }
    frames.extend(s.stop().unwrap());
    let stats = s.stats();
    assert!(frames.len() > 20, "{} 帧", frames.len());
    assert!(stats.skipped > 0, "{stats:?}");
    // 最后几拍跳过时后面没有帧，主机看不出缺口
    assert!(
        stats.missing > 0 && stats.missing <= stats.skipped as u64,
        "{stats:?}"
    );
    for w in frames.windows(2) {
        let dk = w[1].tick - w[0].tick;
        assert!(dk == 1 || dk == 2, "{dk}");
        assert_eq!(w[1].probe_us - w[0].probe_us, 1000 * dk);
    }
    let value = |f: &xrdap::watch::HostFrame| {
        let v = f.values[0].clone().unwrap().unwrap();
        u32::from_le_bytes(v[..4].try_into().unwrap()) as u64
    };
    for w in frames.windows(2) {
        assert_eq!(value(&w[1]) - value(&w[0]), w[1].tick - w[0].tick);
    }
    for f in &frames {
        for (i, item) in list.iter().enumerate() {
            assert_eq!(
                f.values[i].is_some(),
                item.due(f.tick),
                "k={} 条目 {i}",
                f.tick
            );
        }
        if let Some(v) = &f.values[1] {
            assert_eq!(v.as_deref(), Some(&123456u32.to_le_bytes()[..]));
        }
        if let Some(v) = &f.values[2] {
            assert!(v.is_none(), "*null_p1 应该读失败");
        }
    }
}

/// 协议文档 3.5：每拍采一次、全部成功、周期 1 ms 时，1 个 4 字节条目的帧从 15 字节降到 7 字节，
/// 每个回复的第 1 帧是完整帧。
#[test]
fn compact_frames_on_the_wire() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    let t = targets(&mut syms, &["counter"]);
    let list = [sub(&t[0], 0, 1, 0)];
    probe
        .sub_start(1000, vendor::SUB_FLAG_COMPACT, &list)
        .unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let (r, _, _) = probe.sub_read(1000).unwrap();
    assert!(r.frame_count > 3, "{} 帧", r.frame_count);
    assert_eq!(r.frames.len(), 15 + 7 * (r.frame_count as usize - 1));
    assert!(vendor::decode_frames(&r.frames, &list, r.frame_count, false).is_err());
    let frames = vendor::decode_frames(&r.frames, &list, r.frame_count, true).unwrap();
    assert_eq!(frames[1].tick, frames[0].tick + 1);
    assert_eq!(frames[1].timestamp_us, frames[0].timestamp_us + 1000);
    assert!(sim.is_sampling());
}

/// 协议文档 3.4：功能位没有 bit6 的探针不要紧凑帧，否则回 0x01；`--no-overwrite` 的 bit0 不受影响。
#[test]
fn sampler_uses_whole_frames_without_the_feature_bit() {
    let (sim, mut probe, mut syms) = setup();
    sim.set_features(0x1F);
    probe.connect(4_000_000).unwrap();
    let t = targets(&mut syms, &["counter"]);
    let list = [sub(&t[0], 0, 1, 0)];
    assert!(matches!(
        probe.sub_start(1000, vendor::SUB_FLAG_COMPACT, &list),
        Err(xrdap::Error::Status {
            status: vendor::STATUS_ARG,
            ..
        })
    ));
    let mut s = Sampler::start(&mut probe, vars(&list), 1000, true).unwrap();
    assert_eq!(start_flags(&sim), 0x01);
    std::thread::sleep(Duration::from_millis(10));
    let (frames, _) = s.poll().unwrap();
    assert!(frames.len() > 3, "{} 帧", frames.len());
    assert_eq!(frames[1].tick, frames[0].tick + 1);
}

/// 复位只临时改 DEMCR.VC_CORERESET：复位成功、失败，复位后运行还是停住，DEMCR 都恢复原值。
#[test]
fn reset_always_restores_demcr() {
    use xrdap::cortex::{self, AfterReset, DEMCR, DHCSR};
    let (sim, mut probe, _) = setup();
    probe.connect(4_000_000).unwrap();
    // TRCENA 和 VC_CORERESET 都是调试器之前设好的
    let original = (1 << 24) | 1;
    cortex::write_u32(&mut probe, DEMCR, original).unwrap();

    cortex::reset(&mut probe, AfterReset::Run).unwrap();
    assert_eq!(sim.demcr(), original, "复位后运行");

    cortex::write_u32(&mut probe, DEMCR, 1 << 24).unwrap();
    sim.fail_reads(DHCSR, u32::MAX);
    assert!(cortex::reset(&mut probe, AfterReset::Halt).is_err());
    assert_eq!(sim.demcr(), 1 << 24, "复位失败");
}

/// 协议文档 3.1：探针不仲裁。连接在一条链路上时，别的链路的命令照样执行。
#[test]
fn commands_from_any_link_run() {
    let (sim, mut a, mut syms) = setup();
    a.connect(4_000_000).unwrap();
    let mut b = Probe::new(Box::new(sim.link()), TIMEOUT);
    assert!(b.identity().unwrap().serial.is_some());
    let counter = Item::direct(1, 4, addr(&mut syms, "counter"));
    let want = 12345u32.to_le_bytes();
    assert_eq!(
        b.read(std::slice::from_ref(&counter)).unwrap()[0].value,
        want
    );
    b.connect(4_000_000).unwrap();
    assert_eq!(a.read(&[counter]).unwrap()[0].value, want);
}

/// 协议文档 3.1：一条链路丢了，连接在它上面才作废；不在它上面时什么也不发生。
#[test]
fn losing_a_link_ends_only_a_connection_on_it() {
    let (sim, mut a, mut syms) = setup();
    let read = vendor::read_request(&[Item::direct(1, 4, addr(&mut syms, "counter"))]);
    // 直接发 DAP_Connect：Probe 没有记下连接，丢弃时不发 DAP_Disconnect，只是链路丢了
    assert_eq!(a.xfer(&[0x02, 0x01]).unwrap(), [0x02, 0x01]);
    let mut b = Probe::new(Box::new(sim.link()), TIMEOUT);
    assert_eq!(b.xfer(&read).unwrap()[1], 0x00);
    drop(b);
    assert_eq!(a.xfer(&read).unwrap()[1], 0x00);
    drop(a);
    let mut c = Probe::new(Box::new(sim.link()), TIMEOUT);
    assert_eq!(c.xfer(&read).unwrap(), [0x81, 0x04]);
}

/// 协议文档 3.1：DAP_Disconnect 结束任何链路上的连接，之后扩展命令要求重新 DAP_Connect。
#[test]
fn disconnect_from_any_link_ends_the_connection() {
    let (sim, mut a, mut syms) = setup();
    a.connect(4_000_000).unwrap();
    let read = vendor::read_request(&[Item::direct(1, 4, addr(&mut syms, "counter"))]);
    assert_eq!(a.xfer(&read).unwrap()[1], 0x00);
    let mut b = Probe::new(Box::new(sim.link()), TIMEOUT);
    assert_eq!(b.xfer(&[0x03]).unwrap(), [0x03, 0x00]);
    assert_eq!(a.xfer(&read).unwrap(), [0x81, 0x04]);
    a.connect(4_000_000).unwrap();
    assert_eq!(a.xfer(&read).unwrap()[1], 0x00);
}

/// 协议文档 3.1：采样期间，开始采样的那条链路发来 DAP_Connect 是它的上位机回来了，探针结束
/// 旧的连接和采样并重新连接；缓冲里的帧留着。
#[test]
fn connect_from_the_sampling_link_restarts() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    let t = targets(&mut syms, &["counter"]);
    probe.sub_start(1000, 0, &[sub(&t[0], 0, 1, 0)]).unwrap();
    probe.connect(4_000_000).unwrap();
    assert!(!sim.is_sampling());
    let (r, _, _) = probe.sub_read(512).unwrap();
    assert!(!r.sampling);
    assert!(r.frame_count > 0);
    // 连接是新的：厂商命令和新的采样都能用
    assert_eq!(probe.read(&[t[0].item(0)]).unwrap()[0].status, 0);
    probe.sub_start(1000, 0, &[sub(&t[0], 0, 1, 0)]).unwrap();
    assert!(sim.is_sampling());
}

/// 协议文档 3.1：采样期间别的链路的 DAP_Connect 和 DAP_Disconnect 回 `FF`，它的 SUB_START 回
/// 0x02，采样不受影响；DAP_Info 和厂商命令照常。开始采样的链路丢了，采样停下，别的链路才能连接。
#[test]
fn another_link_cannot_end_sampling_by_connecting() {
    let (sim, mut a, mut syms) = setup();
    a.connect(4_000_000).unwrap();
    let t = targets(&mut syms, &["counter"]);
    a.sub_start(1000, 0, &[sub(&t[0], 0, 1, 0)]).unwrap();
    let mut b = Probe::new(Box::new(sim.link()), TIMEOUT);
    assert!(b.identity().unwrap().serial.is_some());
    let e = b.connect(4_000_000).unwrap_err();
    assert!(
        matches!(&e, xrdap::Error::Busy(m) if m.contains("探针正在采样")),
        "{e}"
    );
    assert_eq!(b.xfer(&[0x03]).unwrap(), [0xFF]);
    assert_eq!(b.read(&[t[0].item(0)]).unwrap()[0].status, 0);
    assert!(Sampler::start(&mut b, vars(&[sub(&t[0], 0, 1, 0)]), 1000, false).is_err());
    assert!(sim.is_sampling());
    drop(a);
    assert!(!sim.is_sampling());
    b.connect(4_000_000).unwrap();
}

/// 协议文档 3.4：会话结束后采样停下，SUB_READ 回 0x02 但照样带缓冲里的帧。
#[test]
fn sampler_reads_frames_after_probe_stops() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    let t = targets(&mut syms, &["counter"]);
    let mut s = Sampler::start(&mut probe, vars(&[sub(&t[0], 0, 1, 0)]), 1000, false).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    // 探针那边的采样停了（例如会话结束）；用一条直接的 SUB_CTRL stop 模拟
    s.probe_mut().sub_ctrl(vendor::SUB_STOP).unwrap();
    let (frames, remaining) = s.poll().unwrap();
    assert!(!frames.is_empty());
    assert_eq!(remaining, 0);
    assert!(!s.sampling_on_probe());
    assert!(!sim.is_sampling());
}

/// 条目失败按类别报告；本地能查出的错（值的长度不对、访问宽度非法）不发请求。
#[test]
fn item_errors_are_typed_and_checked_locally() {
    use xrdap::Error;
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    let null_p1 = targets(&mut syms, &["*null_p1"]).remove(0);
    match null_p1.read(&mut probe) {
        Err(Error::Item { status: 0x30, text }) => assert!(text.contains("null_p1"), "{text}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        xrdap::cortex::read_u32(&mut probe, 0x6000_0000),
        Err(Error::Item { status: 0x1F, .. })
    ));
    let before = sim.requests().len();
    let counter = Item::direct(0, 4, addr(&mut syms, "counter"));
    assert!(matches!(
        probe.write(&[(counter, vec![0; 2])]),
        Err(Error::Usage(_))
    ));
    assert!(matches!(
        probe.read(&[Item::direct(0, 3, 0x2000_0000)]),
        Err(Error::Usage(_))
    ));
    assert_eq!(sim.requests().len(), before);
}

/// 条目数超过探针 INFO 报告的上限时在本地报错，不发 SUB_START；没给相位的分频条目被错开。
#[test]
fn sampler_checks_item_limit_and_spreads_phases() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect(4_000_000).unwrap();
    let counter = targets(&mut syms, &["counter"]).remove(0);
    let many: Vec<Var> = (0..=xrdap_sim::MAX_ITEMS as u16)
        .map(|i| Var {
            item: counter.item(i),
            div: 1,
            phase: None,
        })
        .collect();
    let before = sim.requests().len();
    assert!(matches!(
        Sampler::start(&mut probe, many, 1000, false),
        Err(xrdap::Error::Usage(_))
    ));
    assert!(sim.requests()[before..]
        .iter()
        .all(|r| r[0] != vendor::CMD_SUB_START));
    let two = (0..2)
        .map(|i| Var {
            item: counter.item(i),
            div: 10,
            phase: None,
        })
        .collect();
    let s = Sampler::start(&mut probe, two, 1000, false).unwrap();
    assert_ne!(s.items()[0].phase, s.items()[1].phase);
}
