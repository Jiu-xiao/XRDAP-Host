//! 对着模拟探针（经 TCP，和真探针的 3241 端口一样）跑 xrdap 的各个命令，检查输出。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use clap::Parser;
use xrdap::addr::Spec;
use xrdap::open::{open, OpenOptions};
use xrdap_sim::Sim;
use xrdap_tool::cli::{cmd_watch, run, Cli, WatchOpts};

fn elf() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../xrdap/tests/fixtures/types.elf")
        .to_string_lossy()
        .into_owned()
}

/// 装好 types.elf 初值、在 TCP 上等连接的模拟探针，和它的地址。
fn sim() -> (Sim, String) {
    let sim = Sim::new();
    sim.load_elf(&std::fs::read(elf()).unwrap());
    let host = sim.serve_tcp().to_string();
    (sim, host)
}

fn addr_of(name: &str) -> u32 {
    let mut syms = xrdap::elf::load(std::path::Path::new(&elf())).unwrap();
    xrdap::expr::Resolver::new(&mut syms)
        .resolve(name)
        .unwrap()
        .direct_addr()
        .unwrap()
}

/// 跑一个命令，返回它的输出；失败时错误里带上已经输出的文字。
fn xrdap(host: &str, args: &[&str]) -> Result<String, String> {
    let mut out = Vec::new();
    let r = xrdap_to(host, args, &mut out, &AtomicBool::new(false));
    let text = String::from_utf8(out).unwrap();
    r.map(|()| text.clone()).map_err(|e| format!("{text}{e}"))
}

fn xrdap_to(
    host: &str,
    args: &[&str],
    out: &mut dyn Write,
    stop: &AtomicBool,
) -> Result<(), String> {
    // 探针选项挂在子命令上：wifi 再下一级
    let at = if args.first() == Some(&"wifi") { 2 } else { 1 };
    let mut argv = vec!["xrdap"];
    argv.extend_from_slice(&args[..at]);
    argv.extend_from_slice(&["-p", host]);
    argv.extend_from_slice(&args[at..]);
    let cli = Cli::try_parse_from(argv).map_err(|e| e.to_string())?;
    let mut err = Vec::new();
    run(cli, out, &mut err, stop).map_err(|e| format!("错误：{e}"))
}

#[test]
fn info_shows_probe_and_target() {
    let (_sim, host) = sim();
    let out = xrdap(&host, &["info"]).unwrap();
    assert!(out.contains("厂商扩展：版本 2"), "{out}");
    assert!(out.contains("DPIDR 0x2BA01477"), "{out}");
    assert!(out.contains(xrdap_sim::SERIAL), "{out}");
}

#[test]
fn read_values_and_item_errors() {
    let (_sim, host) = sim();
    let out = xrdap(
        &host,
        &[
            "read",
            "--elf",
            &elf(),
            "s8",
            "big",
            "***(uint32_t***)chain_p1",
            "*null_p1",
            "*bad_p1",
        ],
    )
    .unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].starts_with("s8") && lines[0].ends_with("= -7 (0xF9)"),
        "{out}"
    );
    assert!(
        lines[1].ends_with("= 1234605616436508552 (0x1122334455667788)"),
        "{out}"
    );
    assert!(lines[2].ends_with("= 3405691582 (0xCAFEBABE)"), "{out}");
    assert!(lines[3].contains("第 1 级指针 null_p1 为空"), "{out}");
    assert!(lines[4].contains("SWD 出错"), "{out}");
}

#[test]
fn read_types_and_raw() {
    let (_sim, host) = sim();
    let elf = elf();
    let out = xrdap(
        &host,
        &[
            "read",
            "--elf",
            &elf,
            "motor[1].pid->out",
            "state.voltage",
            "motor[0].mode",
            "s8",
            "0x2000007c:u32",
        ],
    )
    .unwrap();
    assert!(out.contains("motor[1].pid->out = -3.75"), "{out}");
    assert!(out.contains("= 3.3"), "{out}");
    assert!(out.contains("MODE_RUN (1)"), "{out}");
    assert!(out.contains("= -7 (0xF9)"), "{out}");
    assert!(out.contains("0x2000007c:u32    = 12345"), "{out}");
    let e = xrdap(&host, &["read", "--elf", &elf, "motor[0].flags.error"]).unwrap_err();
    assert!(e.contains("位域"), "{e}");
    let e = xrdap(&host, &["read", "counter"]).unwrap_err();
    assert!(e.contains("--elf"), "{e}");
}

#[test]
fn write_and_read_back() {
    let (sim, host) = sim();
    let elf = elf();
    let out = xrdap(
        &host,
        &[
            "write",
            "--elf",
            &elf,
            "motor[1].pid->out=2.5",
            "motor[0].speed=-3",
            "counter=0x10",
        ],
    )
    .unwrap();
    assert!(out.contains("motor[1].pid->out ← 2.5，读回 2.5"), "{out}");
    assert!(out.contains("读回 -3 (0xFFFD)"), "{out}");
    assert!(out.contains("读回 16 (0x00000010)"), "{out}");
    assert_eq!(sim.peek(addr_of("counter"), 4), 16u32.to_le_bytes());
    let e = xrdap(&host, &["write", "--elf", &elf, "s8=200"]).unwrap_err();
    assert!(e.contains("超出 1 字节的范围"), "{e}");
    let e = xrdap(&host, &["write", "--elf", &elf, "null_pid->kp=1"]).unwrap_err();
    assert!(e.contains("null_pid 为空"), "{e}");
    // cv 是 const，在 Flash 里，写不进去
    let e = xrdap(&host, &["write", "--elf", &elf, "cv=1"]).unwrap_err();
    assert!(e.contains("写入失败"), "{e}");
}

#[test]
fn watch_prints_and_writes_csv() {
    let (sim, host) = sim();
    sim.add_counter(addr_of("counter"), 4, 1000);
    let csv = std::env::temp_dir().join(format!("xrdap-test-{}.csv", std::process::id()));
    let csv_s = csv.to_string_lossy().into_owned();
    let out = xrdap(
        &host,
        &[
            "watch",
            "--elf",
            &elf(),
            "counter",
            "state.count@10",
            "state.pair.lo@5:2",
            "*null_p1@20",
            "--period",
            "1ms",
            "--duration",
            "0.3",
            "--csv",
            &csv_s,
        ],
    )
    .unwrap();
    assert!(out.contains("采样 4 个变量，周期 1000 µs"), "{out}");
    assert!(out.contains("state.pair.lo 每 5 拍采一次，相位 2"), "{out}");
    assert!(out.contains("帧序号缺口 0"), "{out}");
    let phase_b: u64 = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("state.count 每 10 拍采一次，相位 "))
        .unwrap()
        .parse()
        .unwrap();
    let text = std::fs::read_to_string(&csv).unwrap();
    let _ = std::fs::remove_file(&csv);
    let mut lines = text.lines();
    assert_eq!(
        lines.next().unwrap(),
        "host_time,t,sample_tick,counter,state.count,state.pair.lo,*null_p1"
    );
    let rows: Vec<Vec<&str>> = lines.map(|l| l.split(',').collect()).collect();
    assert!(rows.len() > 250, "{} 行", rows.len());
    for r in &rows {
        let k: u64 = r[2].parse().unwrap();
        assert!(!r[3].is_empty());
        assert_eq!(r[4].is_empty(), !(k + phase_b).is_multiple_of(10), "k={k}");
        assert_eq!(r[5].is_empty(), !(k + 2).is_multiple_of(5), "k={k}");
        if !r[5].is_empty() {
            assert_eq!(r[5], "4369");
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

/// 协议文档 3.5：探针不估计耗时；跳拍时提示加长周期、加分频或减少变量。
#[test]
fn watch_warns_when_not_keeping_up() {
    let (sim, host) = sim();
    sim.skip_every(Some(5));
    let out = xrdap(
        &host,
        &[
            "watch",
            "--elf",
            &elf(),
            "counter",
            "--period",
            "1ms",
            "--duration",
            "0.1",
        ],
    )
    .unwrap();
    assert!(out.contains(xrdap::watch::Lag::Swd.advice()), "{out}");
}

/// 协议文档 3.1：另一条连接占着探针时显示“探针正被另一条连接使用”。
#[test]
fn second_connection_is_reported_busy() {
    let (sim, host) = sim();
    // 另一个传输（例如 USB）先占着探针
    let mut owner = xrdap::dap::Probe::new(Box::new(sim.link()), Duration::from_secs(2));
    owner.connect(4_000_000).unwrap();
    let e = xrdap(&host, &["read", "0x2000007c:u32"]).unwrap_err();
    assert!(e.contains("探针正被另一条连接使用"), "{e}");
}

/// 协议文档 3.7：wifi status 显示最近一次清除的来源和本次启动原因；清除带确认字节。
#[test]
fn wifi_status_shows_history() {
    let (sim, host) = sim();
    xrdap(&host, &["wifi", "set", "lab", "12345678"]).unwrap();
    xrdap(&host, &["wifi", "clear"]).unwrap();
    assert!(sim.wifi_ssid().is_empty());
    let out = xrdap(&host, &["wifi", "status"]).unwrap();
    assert!(out.contains("最近一次清除热点参数：0x86 清除命令"), "{out}");
    assert!(out.contains("本次启动原因：上电"), "{out}");
}

#[test]
fn watch_stops_on_ctrl_c() {
    let (sim, host) = sim();
    let mut probe = open(&Spec::parse(&host), &OpenOptions::default()).unwrap();
    probe.connect(4_000_000).unwrap();
    let mut syms = xrdap::elf::load(std::path::Path::new(&elf())).unwrap();
    let t = xrdap::expr::Resolver::new(&mut syms)
        .resolve("counter")
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let watcher = sim.clone();
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
    cmd_watch(
        &mut probe,
        vec![(t, 1, None)],
        &opts,
        &mut out,
        &mut err,
        &stop,
    )
    .unwrap();
    h.join().unwrap();
    assert!(!sim.is_sampling());
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("已停止"), "{text}");
    // --all 时每帧一行
    let frames = text
        .lines()
        .filter(|l| l.trim_start().starts_with("0.") || l.trim_start().starts_with("1."))
        .count();
    assert!(frames > 100, "{frames}");
}

#[test]
fn no_command_means_gui() {
    let cli = Cli::try_parse_from(["xrdap"]).unwrap();
    assert!(cli.cmd.is_none());
}

/// 用户决定（C4）：每个子命令只收自己的选项，用不到的选项是参数错误，不再悄悄忽略。
#[test]
fn options_belong_to_their_commands() {
    let parse = |args: &[&str]| {
        Cli::try_parse_from(args)
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    assert!(parse(&["xrdap", "list", "--clock", "1M"]).is_err());
    assert!(parse(&["xrdap", "list", "-p", "COM3"]).is_err());
    assert!(parse(&["xrdap", "info", "--elf", "app.elf"]).is_err());
    assert!(parse(&["xrdap", "halt", "--elf", "app.elf"]).is_err());
    assert!(parse(&["xrdap", "share", "openocd", "--clock", "1M"]).is_err());
    assert!(parse(&["xrdap", "wifi", "status", "--elf", "app.elf"]).is_err());
    assert!(parse(&["xrdap", "vars"]).is_err(), "vars 必须有 --elf");
    // 选项写在子命令前面也不行
    assert!(parse(&["xrdap", "-p", "COM3", "info"]).is_err());
    assert!(
        parse(&["xrdap", "read", "-p", "COM3", "--clock", "1M", "--elf", "a.elf", "x"]).is_ok()
    );
    assert!(parse(&["xrdap", "wifi", "set", "-p", "COM3", "lab", "12345678"]).is_ok());
    assert!(parse(&["xrdap", "wifi", "set", "lab", "--password-stdin"]).is_ok());
    assert!(parse(&[
        "xrdap",
        "wifi",
        "set",
        "lab",
        "12345678",
        "--password-stdin"
    ])
    .is_err());
    assert!(parse(&[
        "xrdap",
        "share",
        "usbip",
        "-p",
        "COM3",
        "--listen",
        "0.0.0.0:3240"
    ])
    .is_ok());
}

/// 用户决定（C4）：只在要用的命令加载 --elf；info、halt 这些命令不看 ELF。
#[test]
fn commands_without_symbols_ignore_no_elf() {
    let (sim, host) = sim();
    assert!(xrdap(&host, &["halt"]).unwrap().contains("已停住"));
    assert!(sim.is_halted());
    let out = xrdap(&host, &["wifi", "set", "lab", "12345678"]).unwrap();
    assert!(out.contains("lab"), "{out}");
    assert_eq!(sim.wifi_ssid(), "lab");
}

/// 把写进来的文字一行一行送到通道，测试边运行边看输出。
struct Lines(mpsc::Sender<String>, Vec<u8>);

impl Write for Lines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.1.extend_from_slice(buf);
        while let Some(i) = self.1.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.1.drain(..=i).collect();
            let _ = self.0.send(String::from_utf8_lossy(&line).into_owned());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn share_openocd_answers_dap_info_until_stopped() {
    use std::io::Read;
    use std::net::TcpStream;

    let (_sim, host) = sim();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let (tx, rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut out = Lines(tx, Vec::new());
        xrdap_to(
            &host,
            &["share", "openocd", "--listen", "127.0.0.1:0"],
            &mut out,
            &flag,
        )
    });
    let next = || {
        rx.recv_timeout(Duration::from_secs(5))
            .expect("share 没有输出")
    };
    let port = loop {
        let line = next();
        if let Some(p) = line.split("cmsis-dap tcp port ").nth(1) {
            break p.trim().to_string();
        }
    };
    let mut c = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    // DAP_Info(厂商)
    c.write_all(b"DAP\0\x02\x00\x01\x00\x00\x01").unwrap();
    let mut hdr = [0u8; 8];
    c.read_exact(&mut hdr).unwrap();
    let len = u16::from_le_bytes([hdr[4], hdr[5]]) as usize;
    let mut resp = vec![0u8; len];
    c.read_exact(&mut resp).unwrap();
    assert_eq!(&resp[..2], [0x00, 7]);
    assert_eq!(&resp[2..8], b"XRobot");
    while !next().contains("已连接") {}
    stop.store(true, Ordering::SeqCst);
    server.join().unwrap().unwrap();
}
