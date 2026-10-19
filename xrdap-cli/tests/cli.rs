//! 用模拟探针跑 xrdap 的各个命令，检查输出。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use xrdap::dap::Probe;
use xrdap::expr::Resolver;
use xrdap::fake::FakeProbe;
use xrdap_cli::{cmd_watch, run, Cli, WatchOpts};

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../xrdap/tests/fixtures")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn xrdap(args: &[&str]) -> Result<String, String> {
    let mut argv = vec!["xrdap", "--fake"];
    argv.extend_from_slice(args);
    let cli = Cli::try_parse_from(argv).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let stop = AtomicBool::new(false);
    let r = run(cli, &mut out, &mut err, &stop);
    let text = String::from_utf8(out).unwrap();
    match r {
        Ok(()) => Ok(text),
        Err(e) => Err(format!("{text}错误：{e}")),
    }
}

#[test]
fn info_shows_probe_and_target() {
    let out = xrdap(&["info"]).unwrap();
    assert!(out.contains("厂商扩展：版本 1"), "{out}");
    assert!(out.contains("DPIDR 0x2BA01477"), "{out}");
}

#[test]
fn read_values_and_item_errors() {
    let elf = fixture("g431-test.elf");
    let out = xrdap(&[
        "--elf", &elf, "read", "v_u8", "v_u64", "***(uint32_t***)chain_p1", "*null_p1", "*bad_p1",
    ])
    .unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("v_u8") && lines[0].ends_with("= 165 (0xA5)"), "{out}");
    assert!(lines[1].ends_with("= 81985529216486895 (0x0123456789ABCDEF)"), "{out}");
    assert!(lines[2].ends_with("= 3405691582 (0xCAFEBABE)"), "{out}");
    assert!(lines[3].contains("第 1 级指针 null_p1 为空"), "{out}");
    assert!(lines[4].contains("SWD 出错"), "{out}");
}

#[test]
fn read_types_and_raw() {
    let elf = fixture("types.elf");
    let out = xrdap(&[
        "--elf", &elf, "read", "motor[1].pid->out", "state.voltage", "motor[0].mode", "s8",
        "0x2000007c:u32",
    ])
    .unwrap();
    assert!(out.contains("motor[1].pid->out = -3.75"), "{out}");
    assert!(out.contains("= 3.3"), "{out}");
    assert!(out.contains("MODE_RUN (1)"), "{out}");
    assert!(out.contains("= -7 (0xF9)"), "{out}");
    assert!(out.contains("0x2000007c:u32    = 12345"), "{out}");
    let e = xrdap(&["--elf", &elf, "read", "motor[0].flags.error"]).unwrap_err();
    assert!(e.contains("位域"), "{e}");
    let e = xrdap(&["read", "counter"]).unwrap_err();
    assert!(e.contains("--elf"), "{e}");
}

#[test]
fn write_and_read_back() {
    let elf = fixture("types.elf");
    let out = xrdap(&[
        "--elf", &elf, "write", "motor[1].pid->out=2.5", "motor[0].speed=-3", "counter=0x10",
    ])
    .unwrap();
    assert!(out.contains("motor[1].pid->out ← 2.5，读回 2.5"), "{out}");
    assert!(out.contains("读回 -3 (0xFFFD)"), "{out}");
    assert!(out.contains("读回 16 (0x00000010)"), "{out}");
    let e = xrdap(&["--elf", &elf, "write", "s8=200"]).unwrap_err();
    assert!(e.contains("超出 1 字节的范围"), "{e}");
    let e = xrdap(&["--elf", &elf, "write", "null_pid->kp=1"]).unwrap_err();
    assert!(e.contains("null_pid 为空"), "{e}");
    let e = xrdap(&["--elf", &elf, "write", "cv=1"]).unwrap_err();
    assert!(e.contains("写入失败"), "{e}");
}

#[test]
fn watch_prints_and_writes_csv() {
    let elf = fixture("g431-test.elf");
    let csv = std::env::temp_dir().join(format!("xrdap-test-{}.csv", std::process::id()));
    let csv_s = csv.to_string_lossy().into_owned();
    let out = xrdap(&[
        "--elf", &elf, "watch", "cnt_a", "cnt_b@10", "v_u16@5:2", "*null_p1@20", "--period", "1ms",
        "--duration", "0.3", "--csv", &csv_s,
    ])
    .unwrap();
    assert!(out.contains("采样 4 个变量，周期 1000 µs"), "{out}");
    assert!(out.contains("v_u16 每 5 拍采一次，相位 2"), "{out}");
    assert!(out.contains("帧序号缺口 0"), "{out}");
    let phase_b: u64 = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("cnt_b 每 10 拍采一次，相位 "))
        .unwrap()
        .parse()
        .unwrap();
    let text = std::fs::read_to_string(&csv).unwrap();
    let _ = std::fs::remove_file(&csv);
    let mut lines = text.lines();
    assert_eq!(lines.next().unwrap(), "host_time,t,sample_tick,cnt_a,cnt_b,v_u16,*null_p1");
    let rows: Vec<Vec<&str>> = lines.map(|l| l.split(',').collect()).collect();
    assert!(rows.len() > 250, "{} 行", rows.len());
    for r in &rows {
        let k: u64 = r[2].parse().unwrap();
        assert!(!r[3].is_empty());
        assert_eq!(r[4].is_empty(), (k + phase_b) % 10 != 0, "k={k}");
        assert_eq!(r[5].is_empty(), (k + 2) % 5 != 0, "k={k}");
        if !r[5].is_empty() {
            assert_eq!(r[5], "4660");
        }
        if !r[6].is_empty() {
            assert_eq!(r[6], "ERR");
        }
    }
    // 时间列：探针每拍 1 ms，主机时间按同一个偏移换算
    let t1: f64 = rows[1][1].parse().unwrap();
    assert!((t1 - 0.001).abs() < 1e-9);
    let h0: f64 = rows[0][0].parse().unwrap();
    let h1: f64 = rows[1][0].parse().unwrap();
    // 偏移量可能在两次 SUB_READ 之间更新，差值在半个往返时间之内
    assert!((h1 - h0 - 0.001).abs() < 5e-4);
}

#[test]
fn watch_rejected_suggests_period() {
    let elf = fixture("g431-test.elf");
    let e = xrdap(&["--elf", &elf, "watch", "cnt_a", "v_u64", "--period", "20us"]).unwrap_err();
    assert!(e.contains("探针拒绝了这个配置") && e.contains("--period"), "{e}");
}

#[test]
fn watch_stops_on_ctrl_c() {
    let elf = PathBuf::from(fixture("g431-test.elf"));
    let mut syms = xrdap::elf::load(&elf).unwrap();
    let fake = FakeProbe::new();
    fake.load_elf_image(&std::fs::read(&elf).unwrap()).unwrap();
    fake.apply_g431_fixture(&syms);
    let mut probe = Probe::new(Box::new(fake.clone()));
    probe.connect_swd(4_000_000).unwrap();
    let t = Resolver::new(&mut syms).resolve("tick").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let watcher = fake.clone();
    let h = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        assert!(watcher.is_sampling());
        flag.store(true, Ordering::SeqCst);
    });
    let opts = WatchOpts {
        period_us: 500,
        csv: None,
        duration: None,
        stop_on_full: false,
        print_all: true,
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    cmd_watch(&mut probe, vec![(t, 1, None)], &opts, &mut out, &mut err, &stop).unwrap();
    h.join().unwrap();
    assert!(!fake.is_sampling());
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("已停止"), "{text}");
    // --all 时每帧一行：帧数与行数一致
    let frames = text.lines().filter(|l| l.trim_start().starts_with("0.") || l.trim_start().starts_with("1.")).count();
    assert!(frames > 100, "{frames}");
}

#[test]
fn connection_options_are_exclusive() {
    let e = xrdap(&["--tcp", "127.0.0.1", "info"]).unwrap_err();
    assert!(e.contains("只能选一个"), "{e}");
}

#[test]
fn usb_flag_does_not_swallow_subcommand() {
    use clap::Parser;
    let cli = xrdap_cli::Cli::try_parse_from(["xrdap", "--usb", "info"]).unwrap();
    assert!(matches!(cli.cmd, xrdap_cli::Cmd::Info { .. }));
    assert_eq!(cli.conn.usb.as_deref(), Some(""));
    let cli = xrdap_cli::Cli::try_parse_from(["xrdap", "--usb=ABC", "list"]).unwrap();
    assert_eq!(cli.conn.usb.as_deref(), Some("ABC"));
}
