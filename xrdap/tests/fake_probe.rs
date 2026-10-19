//! 用模拟探针把初始化、READ、WRITE、采样整个走一遍。
//! 期望值与硬件测试脚本 run_vendor_usb.py、sampling_acceptance.py 一致。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use xrdap::dap::Probe;
use xrdap::expr::Resolver;
use xrdap::fake::FakeProbe;
use xrdap::types::SymbolTable;
use xrdap::vendor::{self, Item, SubItem};
use xrdap::watch::{Sampler, StartError};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn g431() -> (FakeProbe, Probe, SymbolTable) {
    let path = fixture("g431-test.elf");
    let syms = xrdap::elf::load(&path).unwrap();
    let fake = FakeProbe::new();
    fake.load_elf_image(&std::fs::read(&path).unwrap()).unwrap();
    assert!(fake.apply_g431_fixture(&syms));
    let probe = Probe::new(Box::new(fake.clone()));
    (fake, probe, syms)
}

fn targets(syms: &mut SymbolTable, exprs: &[&str]) -> Vec<xrdap::expr::Target> {
    let mut r = Resolver::new(syms);
    exprs.iter().map(|e| r.resolve(e).unwrap()).collect()
}

fn items(t: &[xrdap::expr::Target]) -> Vec<Item> {
    t.iter().enumerate().map(|(i, t)| t.item(i as u16)).collect()
}

#[test]
fn vendor_commands_need_connect() {
    let (_fake, mut probe, _) = g431();
    let resp = probe.xfer(&vendor::read_request(&[Item::direct(1, 4, 0x2000_001C)])).unwrap();
    assert_eq!(resp, vec![0x81, 0x04]);
    let err = probe.read(&[Item::direct(1, 4, 0x2000_001C)]).unwrap_err();
    assert!(err.to_string().contains("没有连接目标"), "{err}");
    let target = probe.connect_swd(4_000_000).unwrap();
    assert_eq!(target.dpidr, 0x2BA0_1477);
    assert!(target.describe().contains("ARM"));
    let r = probe.read(&[Item::direct(1, 4, 0x2000_001C)]).unwrap();
    assert_eq!(r[0].value, 0x89AB_CDEFu32.to_le_bytes());
    let info = probe.vendor_info().unwrap();
    assert_eq!((info.version, info.features, info.max_items), (1, 0x0F, 32));
}

#[test]
fn read_status_matrix_like_hardware() {
    let (_fake, mut probe, mut syms) = g431();
    probe.connect_swd(4_000_000).unwrap();
    let t = targets(
        &mut syms,
        &["v_u8", "v_u16", "v_u32", "v_u64", "***(uint32_t***)chain_p1", "*null_p1", "*bad_p1"],
    );
    let r = probe.read(&items(&t)).unwrap();
    let vals: Vec<String> = t.iter().zip(&r).map(|(t, r)| t.ty.format(&r.value)).collect();
    assert_eq!(vals[0], "165 (0xA5)");
    assert_eq!(vals[1], "4660 (0x1234)");
    assert_eq!(vals[2], "2309737967 (0x89ABCDEF)");
    assert_eq!(vals[3], "81985529216486895 (0x0123456789ABCDEF)");
    assert_eq!(vals[4], "3405691582 (0xCAFEBABE)");
    assert_eq!(r[5].status, 0x30);
    assert_eq!(r[6].status, 0x1F);
    assert!(vendor::item_status_text(r[5].status, &t[5].chain).contains("null_p1"));

    // 与 run_vendor_usb.py 的状态表相同
    let bad = [
        (Item { derefs: vec![0], ..Item::direct(0x10, 4, 0x2000_0038) }, 0x30),
        (Item { derefs: vec![0], ..Item::direct(0x11, 4, 0xFFFF_F000) }, 0x10),
        (Item { derefs: vec![1], ..Item::direct(0x12, 4, 0x2000_003C) }, 0x20),
        (Item { final_off: 1, ..Item::direct(0x13, 4, 0x2000_001C) }, 0x2F),
        (Item { derefs: vec![0, 0], ..Item::direct(0x15, 4, 0x2000_003C) }, 0x11),
        (Item { derefs: vec![0], ..Item::direct(0x16, 4, 0x2000_003C) }, 0x1F),
        (Item::direct(0x14, 4, 0x2000_001C), 0x00),
    ];
    let list: Vec<Item> = bad.iter().map(|(i, _)| i.clone()).collect();
    let r = probe.read(&list).unwrap();
    for ((_, want), got) in bad.iter().zip(&r) {
        assert_eq!(got.status, *want, "id {:#x}", got.id);
    }
}

#[test]
fn write_then_read_back() {
    let (fake, mut probe, mut syms) = g431();
    probe.connect_swd(4_000_000).unwrap();
    let t = targets(&mut syms, &["v_u8", "v_u16", "v_u64", "rw_block[0]", "*chain_p3"]);
    let values = ["0x5A", "0xBEAF", "0x8877665544332211", "0xDEADBEEF", "7"];
    let list: Vec<(Item, Vec<u8>)> = t
        .iter()
        .zip(values)
        .enumerate()
        .map(|(i, (t, v))| (t.item(i as u16), t.ty.parse(v).unwrap()))
        .collect();
    let r = probe.write(&list).unwrap();
    assert!(r.iter().all(|r| r.status == 0));
    assert_eq!(fake.peek(0x2000_0018, 1).unwrap(), vec![0x5A]);
    assert_eq!(fake.peek(0x2000_0028, 4).unwrap(), 7u32.to_le_bytes());
    let back = probe.read(&items(&t)).unwrap();
    assert_eq!(back[2].value, 0x8877_6655_4433_2211u64.to_le_bytes());
    // 写 Flash 失败
    let flash = Item::direct(9, 4, 0x0800_0000);
    let r = probe.write(&[(flash, vec![0; 4])]).unwrap();
    assert_eq!(r[0].status, 0x1F);
}

#[test]
fn many_items_split_into_several_reads() {
    let (fake, mut probe, _) = g431();
    probe.connect_swd(4_000_000).unwrap();
    let list: Vec<Item> = (0..40).map(|i| Item::direct(i, 4, 0x2000_0040 + 4 * (i as u32 % 8))).collect();
    let before = fake.requests().len();
    let r = probe.read(&list).unwrap();
    assert_eq!(r.len(), 40);
    assert_eq!(fake.requests().len() - before, 2);
    assert_eq!(r[3].value, 0x4444_4444u32.to_le_bytes());
}

fn sub(t: &xrdap::expr::Target, id: u16, div: u16, phase: u16) -> SubItem {
    SubItem {
        item: t.item(id),
        div,
        phase,
    }
}

#[test]
fn sampling_with_divided_items() {
    let (_fake, mut probe, mut syms) = g431();
    probe.connect_swd(4_000_000).unwrap();
    let t = targets(&mut syms, &["cnt_a", "tick", "cnt_b", "cnt64", "v_u32", "***(uint32_t***)chain_p1"]);
    let mut list = vec![
        sub(&t[0], 0, 1, 0),
        sub(&t[1], 1, 1, 0),
        sub(&t[2], 2, 10, 0),
        sub(&t[3], 3, 5, 0),
        sub(&t[4], 4, 20, 0),
        sub(&t[5], 5, 20, 0),
    ];
    vendor::assign_phases(&mut list, &[false; 6]);
    assert_ne!(list[4].phase, list[5].phase);
    let mut s = Sampler::start(&mut probe, list.clone(), 1000, false).unwrap();
    assert_eq!(s.setup.max_frame_bytes, Some(10 + 1 + 4 + 4 + 4 + 8 + 4 + 4));
    let mut frames = Vec::new();
    let end = Instant::now() + Duration::from_millis(300);
    while Instant::now() < end {
        let (f, _) = s.poll().unwrap();
        frames.extend(f);
        std::thread::sleep(Duration::from_millis(5));
    }
    frames.extend(s.stop().unwrap());
    let stats = s.stats();
    drop(s);
    assert!(frames.len() > 200, "{} 帧", frames.len());
    assert_eq!((stats.missing, stats.dropped, stats.skipped, stats.failed_values), (0, 0, 0, 0));
    for w in frames.windows(2) {
        assert_eq!(w[1].tick, w[0].tick + 1);
        assert_eq!(w[1].probe_us - w[0].probe_us, 1000);
    }
    for f in &frames {
        for (i, item) in list.iter().enumerate() {
            assert_eq!(f.values[i].is_some(), item.due(f.tick), "k={} 条目 {i}", f.tick);
        }
        if let Some(Some(v)) = &f.values[4] {
            assert_eq!(v, &0x89AB_CDEFu32.to_le_bytes());
        }
        if let Some(Some(v)) = &f.values[5] {
            assert_eq!(v, &0xCAFE_BABEu32.to_le_bytes());
        }
        // 主机时间与探针时间之差就是偏移量，所有帧都一样
        assert!(f.host_unix_us > 1_600_000_000_000_000);
    }
    // 停止后标准命令又能用了
    assert_eq!(probe.xfer(&[0x02, 0x01]).unwrap(), vec![0x02, 0x01]);
}

#[test]
fn sampling_rejected_reports_estimate() {
    let (_fake, mut probe, mut syms) = g431();
    probe.connect_swd(4_000_000).unwrap();
    let t = targets(&mut syms, &["cnt_a", "v_u64", "***(uint32_t***)chain_p1"]);
    let list = vec![sub(&t[0], 0, 1, 0), sub(&t[1], 1, 1, 0), sub(&t[2], 2, 1, 0)];
    let res = Sampler::start(&mut probe, list, 20, false);
    match res {
        Err(StartError::Rejected(r)) => {
            assert_eq!(r.status, vendor::STATUS_RESOURCE);
            let est = r.est_frame_us.unwrap();
            assert!(est > 20);
            assert!(xrdap::watch::suggest_period(est) >= est);
        }
        Err(StartError::Other(e)) => panic!("{e}"),
        Ok(_) => panic!("应该被拒绝"),
    }
}

#[test]
fn sampling_counters_and_gaps() {
    let (fake, mut probe, mut syms) = g431();
    probe.connect_swd(4_000_000).unwrap();
    fake.set_skip_every(Some(10));
    fake.set_ring_bytes(400);
    let t = targets(&mut syms, &["cnt_a", "*null_p1"]);
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
    assert!(!fake.is_sampling());
}

/// TCP 传输：本机起一个用模拟探针应答的 TCP 服务，帧格式与探针的 3241 端口相同。
#[test]
fn tcp_transport_round_trip() {
    use std::io::{Read, Write};
    use xrdap::frame::{parse_tcp_header, tcp_frame, TCP_TYPE_REQUEST, TCP_TYPE_RESPONSE};
    use xrdap::transport::Transport;

    let (fake, _, _) = g431();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut server = fake.clone();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut h = [0u8; 8];
        while s.read_exact(&mut h).is_ok() {
            let len = parse_tcp_header(&h, TCP_TYPE_REQUEST).unwrap();
            let mut req = vec![0u8; len];
            s.read_exact(&mut req).unwrap();
            let resp = server.xfer(&req).unwrap();
            s.write_all(&tcp_frame(&resp, TCP_TYPE_RESPONSE)).unwrap();
        }
    });
    let t = xrdap::transport::tcp::TcpTransport::connect(
        &format!("127.0.0.1:{port}"),
        Duration::from_secs(2),
    )
    .unwrap();
    let mut probe = Probe::new(Box::new(t));
    assert!(probe.describe().starts_with("TCP 127.0.0.1:"));
    probe.connect_swd(4_000_000).unwrap();
    let r = probe.read(&[Item::direct(1, 2, 0x2000_001A)]).unwrap();
    assert_eq!(r[0].value, 0x1234u16.to_le_bytes());
    assert_eq!(xrdap::transport::tcp::with_default_port("192.168.0.108"), "192.168.0.108:3241");
    assert_eq!(xrdap::transport::tcp::with_default_port("localhost:5000"), "localhost:5000");
    assert_eq!(xrdap::transport::tcp::with_default_port("::1"), "[::1]:3241");
}

#[test]
fn connect_stops_leftover_sampling() {
    let (fake, mut probe, mut syms) = g431();
    probe.connect_swd(4_000_000).unwrap();
    let t = targets(&mut syms, &["cnt_a"]);
    probe.sub_set(1000, 0, &[sub(&t[0], 0, 1, 0)]).unwrap();
    assert_eq!(probe.sub_ctrl(vendor::SUB_START).unwrap(), 0);
    assert!(fake.is_sampling());
    // 新的会话：DAP_Connect 先回 FF，客户端停掉采样后重连
    let mut probe2 = Probe::new(Box::new(fake.clone()));
    probe2.connect_swd(4_000_000).unwrap();
    assert!(!fake.is_sampling());
}

#[test]
fn halt_resume_and_reset() {
    use xrdap::cortex::{self, AfterReset, CoreState};
    let (fake, mut probe, mut syms) = g431();
    probe.connect_swd(4_000_000).unwrap();
    let tick = targets(&mut syms, &["tick"]);
    let read_tick = |probe: &mut Probe| {
        let r = probe.read(&items(&tick)).unwrap();
        u32::from_le_bytes(r[0].value[..4].try_into().unwrap())
    };

    assert_eq!(cortex::info(&mut probe).unwrap().describe(), "Cortex-M4 r0p1");
    assert_eq!(cortex::status(&mut probe).unwrap().state, CoreState::Running);

    // 停住后计数器不走
    let s = cortex::halt(&mut probe).unwrap();
    assert_eq!(s.state, CoreState::Halted);
    assert!(fake.is_halted());
    let a = read_tick(&mut probe);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(read_tick(&mut probe), a);

    // 继续后计数器接着走
    assert_ne!(cortex::resume(&mut probe).unwrap().state, CoreState::Halted);
    std::thread::sleep(Duration::from_millis(20));
    assert!(read_tick(&mut probe) > a);

    // 复位后停在复位向量：计数器回到 0 且不走，DEMCR 恢复原值
    let s = cortex::reset(&mut probe, AfterReset::Halt).unwrap();
    assert_eq!(s.state, CoreState::Halted);
    assert_eq!(read_tick(&mut probe), 0);
    assert_eq!(cortex::read_u32(&mut probe, cortex::DEMCR).unwrap() & 1, 0);

    // 复位后运行：计数器从 0 重新开始
    std::thread::sleep(Duration::from_millis(30));
    let s = cortex::reset(&mut probe, AfterReset::Run).unwrap();
    assert_ne!(s.state, CoreState::Halted);
    let t = read_tick(&mut probe);
    assert!(t < 25, "复位后 tick 应从 0 开始，读到 {t}");
}
