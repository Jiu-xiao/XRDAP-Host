//! 对照模拟探针的端到端测试。判据来自 XRDAP-Core 的协议文档、CMSIS-DAP 规范、ARMv7-M 架构
//! 手册和 tests/fixtures/types.c 里的初值，不来自本库自己的实现。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use xrdap::dap::Probe;
use xrdap::expr::{Resolver, Target};
use xrdap::types::SymbolTable;
use xrdap::vendor::{self, Item, SubItem};
use xrdap::watch::{Sampler, StartError};
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
    let target = probe.connect_swd(4_000_000).unwrap();
    assert_eq!(target.dpidr, xrdap_sim::DPIDR);
    assert!(target.describe().contains("ARM"));
    let r = probe.read(&[counter]).unwrap();
    assert_eq!(r[0].value, 12345u32.to_le_bytes());
    let info = probe.vendor_info().unwrap();
    assert_eq!((info.version, info.max_items, info.max_depth), (1, 32, 8));
}

/// 协议文档 3.2、3.3 的条目状态：空指针 0x30+k、读指针出错 0x10+k、指针地址不对齐 0x20+k、
/// 最终地址不对齐 0x2F、最后的读写出错 0x1F。
#[test]
fn read_item_status_per_protocol() {
    let (_sim, mut probe, mut syms) = setup();
    probe.connect_swd(4_000_000).unwrap();
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
    probe.connect_swd(4_000_000).unwrap();
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
    probe.connect_swd(4_000_000).unwrap();
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

/// 协议文档 3.5：第 k 拍的目标时间是开始时间 + k × 周期；`(k + phase) % div == 0` 的条目到期。
#[test]
fn sampling_with_divided_items() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect_swd(4_000_000).unwrap();
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
    let mut s = Sampler::start(&mut probe, list.clone(), 1000, false).unwrap();
    // 所有条目都到期时一帧的字节数：帧头 10 + 位图 1 + 各条目的值
    assert_eq!(s.setup.max_frame_bytes, Some(10 + 1 + 4 + 4 + 8 + 1 + 4));
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

/// 协议文档 3.4 SUB_SET：单帧估计大于周期时回 0x03，max_frame_bytes 和 est_frame_us 照样给出。
#[test]
fn sampling_rejected_reports_estimate() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect_swd(4_000_000).unwrap();
    sim.set_access_us(100);
    let t = targets(&mut syms, &["counter", "big", "***(uint32_t***)chain_p1"]);
    let list = vec![
        sub(&t[0], 0, 1, 0),
        sub(&t[1], 1, 1, 0),
        sub(&t[2], 2, 1, 0),
    ];
    match Sampler::start(&mut probe, list, 20, false) {
        Err(StartError::Rejected(r)) => {
            assert_eq!(r.status, vendor::STATUS_RESOURCE);
            let est = r.est_frame_us.unwrap();
            assert!(est > 20);
            assert!(xrdap::watch::suggest_period(est) >= est);
        }
        Err(StartError::Other(e)) => panic!("{e}"),
        Ok(_) => panic!("应该被拒绝"),
    };
}

/// 协议文档 3.5：跳拍计数、丢帧计数，以及读取失败的条目在位图里置 1。
#[test]
fn sampling_counters_and_gaps() {
    let (sim, mut probe, mut syms) = setup();
    probe.connect_swd(4_000_000).unwrap();
    sim.skip_every(Some(10));
    sim.set_ring_bytes(400);
    let t = targets(&mut syms, &["counter", "*null_p1"]);
    let list = vec![sub(&t[0], 0, 1, 0), sub(&t[1], 1, 1, 0)];
    let mut s = Sampler::start(&mut probe, list, 500, false).unwrap();
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
    // Sampler 销毁时停止采样
    drop(s);
    assert!(!sim.is_sampling());
}

/// 协议文档第 2 节：TCP 帧；没有响应的命令（DAP_TransferAbort）不回帧。
#[test]
fn tcp_transport_round_trip() {
    let (sim, _, mut syms) = setup();
    let host = sim.serve_tcp().to_string();
    let t = xrdap::transport::tcp::TcpTransport::connect(&host, TIMEOUT).unwrap();
    let mut probe = Probe::new(Box::new(t), TIMEOUT);
    assert!(probe.describe().starts_with("TCP 127.0.0.1:"));
    probe.connect_swd(4_000_000).unwrap();
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
    probe.connect_swd(4_000_000).unwrap();
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

/// 协议文档 3.7：0x86 的查询、保存、清除。
#[test]
fn wifi_status_set_clear() {
    let (sim, mut probe, _) = setup();
    assert!(xrdap::wifi::status(&mut probe).unwrap().ssid.is_empty());
    xrdap::wifi::set(&mut probe, "lab", "12345678").unwrap();
    assert_eq!(sim.wifi_ssid(), "lab");
    assert_eq!(xrdap::wifi::status(&mut probe).unwrap().ssid, "lab");
    xrdap::wifi::clear(&mut probe).unwrap();
    assert!(sim.wifi_ssid().is_empty());
}

/// 复位只临时改 DEMCR.VC_CORERESET：复位成功、失败，复位后运行还是停住，DEMCR 都恢复原值。
#[test]
fn reset_always_restores_demcr() {
    use xrdap::cortex::{self, AfterReset, DEMCR, DHCSR};
    let (sim, mut probe, _) = setup();
    probe.connect_swd(4_000_000).unwrap();
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
