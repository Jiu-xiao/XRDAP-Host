//! `xrdap` 命令行工具的实现。命令的输出写到传入的 Write，测试里用模拟探针跑整个命令。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::{ArgAction, Args, Parser, Subcommand};
use xrdap::cortex::{self, AfterReset};
use xrdap::dap::{self, Probe};
use xrdap::discover;
use xrdap::expr::{Resolver, Target};
use xrdap::fake::FakeProbe;
use xrdap::open::{self, Addr, OpenOptions, Spec};
use xrdap::transport::serial::DEFAULT_BAUD;
use xrdap::types::SymbolTable;
use xrdap::vendor::{self, Item, SubItem};
use xrdap::watch::{self, HostFrame, Sampler, StartError};
use xrdap::wifi;

pub type CliResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const TEMPLATE: &str = "{about-with-newline}\n用法：{usage}\n\n{all-args}{after-help}";

#[derive(Parser, Debug)]
#[command(
    name = "xrdap",
    bin_name = "xrdap",
    version,
    about = "XR-DAP 探针的命令行工具：按变量名读写目标内存，在探针上周期采样变量",
    help_template = TEMPLATE,
    subcommand_help_heading = "命令",
    subcommand_value_name = "命令",
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    after_help = "\
用 --probe 指定探针（序列号、usb、COM35、192.168.0.108 等），不写时自动选唯一的那块；
xrdap list 列出所有探针。
变量的写法：counter、state.speed、motor[1].pid->out、*ptr、p->q->x、
  (uint32_t***)p 这样的类型转换，或原始地址 0x20000010:u32（类型可以是 u8/u16/u32/u64、
  i8/i16/i32/i64、f32/f64，以及 ELF 里的类型名，可以带 *）。"
)]
pub struct Cli {
    #[command(flatten)]
    pub conn: ConnOpts,

    #[command(subcommand)]
    pub cmd: Cmd,

    /// 显示帮助
    #[arg(short = 'h', long, action = ArgAction::Help, global = true, help_heading = "选项")]
    help: Option<bool>,

    /// 显示版本
    #[arg(short = 'V', long, action = ArgAction::Version, help_heading = "选项")]
    version: Option<bool>,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "连接选项")]
pub struct ConnOpts {
    /// 用哪块探针：序列号（全称或末尾几位）、usb、COM35、192.168.0.108、xrdap-93ef1c.local。
    /// 不写时自动选唯一的那块；xrdap list 列出所有探针和对应的写法
    #[arg(short = 'p', long, global = true, value_name = "探针")]
    pub probe: Option<String>,

    /// 用 USB 连接，等同于 --probe usb；插着多个探针时写成 --usb=序列号
    #[arg(long, global = true, num_args = 0..=1, require_equals = true, default_missing_value = "", value_name = "序列号")]
    pub usb: Option<String>,

    /// 用串口连接，等同于 --probe COM35
    #[arg(long, global = true, value_name = "串口")]
    pub serial: Option<String>,

    /// 串口波特率，要和探针固件一致（默认 4000000）
    #[arg(long, global = true, default_value_t = DEFAULT_BAUD, hide_default_value = true, value_name = "波特率")]
    pub baud: u32,

    /// 打开串口后不改 DTR/RTS（默认置为 DTR=1、RTS=0）。乐鑫芯片自带的 USB 口自动这样处理
    #[arg(long, global = true)]
    pub keep_lines: bool,

    /// 用 TCP 连接，等同于 --probe 地址：探针的 IP 或主机名、xrdap-forward 的监听地址，端口默认 3241
    #[arg(long, global = true, value_name = "地址[:端口]")]
    pub tcp: Option<String>,

    /// 目标程序的 ELF（带 -g 调试信息），用来按名字找变量和类型
    #[arg(long, global = true, value_name = "文件")]
    pub elf: Option<PathBuf>,

    /// SWD 时钟，例如 4000000、4M、500k（默认 4M）
    #[arg(long, global = true, default_value = "4M", hide_default_value = true, value_parser = parse_hz, value_name = "Hz")]
    pub clock: u32,

    /// 等待一个响应的超时，毫秒（默认 2000）
    #[arg(
        long,
        global = true,
        default_value_t = 2000,
        hide_default_value = true,
        value_name = "毫秒"
    )]
    pub timeout_ms: u64,

    /// 使用内置的模拟探针，不需要硬件（测试用）。配合 --elf 时按 ELF 填模拟内存
    #[arg(long, global = true)]
    pub fake: bool,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// 列出 USB、串口、局域网上的探针，以及选它们时 --probe 的写法
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    List,

    /// 显示探针信息：DAP_Info、厂商扩展 INFO，并连接目标读 DPIDR
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    Info {
        /// 不连接目标，只查询探针
        #[arg(long)]
        no_target: bool,
    },

    /// 列出 ELF 里的全局变量
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    Vars {
        /// 只列出名字里带这段文字的变量
        #[arg(help_heading = "参数", value_name = "文字")]
        pattern: Option<String>,
    },

    /// 读变量。结构体和数组会展开成各个成员
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    Read {
        /// 要读的变量，可以写多个
        #[arg(required = true, help_heading = "参数", value_name = "变量")]
        exprs: Vec<String>,
    },

    /// 写变量，写完读回一次
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    Write {
        /// 变量=值，可以写多个。整数可以写十进制或 0x 十六进制，浮点写小数，枚举可以写名字
        #[arg(required = true, help_heading = "参数", value_name = "变量=值")]
        assigns: Vec<String>,
    },

    /// 查询、设置、清除探针的 Wi-Fi 热点参数（经 DAP 本身，不用另开串口终端）
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    Wifi {
        #[command(subcommand)]
        op: WifiCmd,
    },

    /// 停住目标内核
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    Halt,

    /// 让停住的目标内核继续运行
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    Run,

    /// 复位目标（AIRCR.SYSRESETREQ），默认复位后运行
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    Reset {
        /// 复位后停在复位向量，不运行
        #[arg(long)]
        halt: bool,
    },

    /// 在探针上按固定周期采样变量，打印或写成 CSV。按 Ctrl-C 停止
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    Watch {
        /// 要采样的变量，变量@分频 表示每几拍采一次，变量@分频:相位 可以指定相位
        #[arg(required = true, help_heading = "参数", value_name = "变量[@分频]")]
        exprs: Vec<String>,

        /// 采样周期，例如 1000（µs）、500us、2ms（默认 1ms）
        #[arg(
            long,
            default_value = "1ms",
            hide_default_value = true,
            value_name = "周期"
        )]
        period: String,

        /// 把每一帧写进 CSV 文件。不到期的格为空，读取失败的格为 ERR
        #[arg(long, value_name = "文件")]
        csv: Option<PathBuf>,

        /// 采样多少秒后停止；不写时一直采到 Ctrl-C
        #[arg(long, value_name = "秒")]
        duration: Option<f64>,

        /// 缓冲满后丢弃新帧（默认覆盖最旧的帧）
        #[arg(long)]
        no_overwrite: bool,

        /// 终端里打印每一帧（默认最多每 0.1 秒打印一行）
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum WifiCmd {
    /// 显示热点名、是否连接、IP、信号强度（不显示密码）
    #[command(help_template = TEMPLATE, disable_help_flag = true)]
    Status,

    /// 保存热点参数，探针随后重启并连接这个热点
    #[command(help_template = TEMPLATE, disable_help_flag = true)]
    Set {
        /// 热点名
        #[arg(help_heading = "参数", value_name = "热点")]
        ssid: String,
        /// 密码，8 到 63 个字节；开放网络不写
        #[arg(help_heading = "参数", value_name = "密码")]
        password: Option<String>,
    },

    /// 清除热点参数，探针随后重启，不再连 Wi-Fi
    #[command(help_template = TEMPLATE, disable_help_flag = true)]
    Clear,
}

fn parse_hz(s: &str) -> Result<u32, String> {
    dap::parse_clock(s).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ 连接

fn load_symbols(conn: &ConnOpts) -> CliResult<SymbolTable> {
    match &conn.elf {
        Some(path) => Ok(xrdap::elf::load(path)?),
        None => Ok(SymbolTable::new()),
    }
}

fn open_probe(conn: &ConnOpts, syms: &SymbolTable, err: &mut dyn Write) -> CliResult<Probe> {
    if conn.fake {
        if conn.probe.is_some() || conn.usb.is_some() || conn.serial.is_some() || conn.tcp.is_some()
        {
            return Err("--fake 和 --probe、--usb、--serial、--tcp 只能写一个".into());
        }
        let fake = FakeProbe::new();
        if let Some(path) = &conn.elf {
            fake.load_elf_image(&std::fs::read(path)?)?;
            fake.apply_g431_fixture(syms);
        }
        return Ok(Probe::new(Box::new(fake)));
    }
    let opts = OpenOptions {
        timeout: Duration::from_millis(conn.timeout_ms),
        baud: conn.baud,
        ..OpenOptions::default()
    };
    let addr = match probe_spec(conn)? {
        Spec::Addr(Addr::Serial { port, keep_lines }) => Addr::Serial {
            port,
            keep_lines: keep_lines || conn.keep_lines,
        },
        Spec::Addr(addr) => addr,
        Spec::Auto | Spec::Serial(_) => {
            // 自动选择或按序列号找：先找，再按找到的连接打开
            return Ok(open::open(conn.probe.as_deref(), &opts)?);
        }
    };
    if matches!(
        addr,
        Addr::Serial {
            keep_lines: false,
            ..
        }
    ) {
        writeln!(err, "打开 {addr}，等探针应答……")?;
    }
    Ok(open::open_addr(&addr, &opts)?)
}

/// 连接选项合成一个探针说明：--probe，或兼容的 --usb、--serial、--tcp，只能写一个。
fn probe_spec(conn: &ConnOpts) -> CliResult<Spec> {
    let mut specs = Vec::new();
    if let Some(p) = &conn.probe {
        specs.push(open::parse(Some(p)));
    }
    if let Some(sn) = &conn.usb {
        specs.push(Spec::Addr(Addr::Usb {
            serial: Some(sn.clone()).filter(|s| !s.is_empty()),
        }));
    }
    if let Some(port) = &conn.serial {
        specs.push(Spec::Addr(Addr::Serial {
            port: port.clone(),
            keep_lines: false,
        }));
    }
    if let Some(host) = &conn.tcp {
        specs.push(Spec::Addr(Addr::Tcp { host: host.clone() }));
    }
    match specs.len() {
        0 => Ok(Spec::Auto),
        1 => Ok(specs.remove(0)),
        _ => Err("--probe、--usb、--serial、--tcp 只能写一个".into()),
    }
}

fn connect_target(probe: &mut Probe, conn: &ConnOpts) -> CliResult<dap::TargetInfo> {
    probe
        .connect_swd(conn.clock)
        .map_err(|e| format!("连接目标失败：{e}").into())
}

fn print_notes(notes: &mut Vec<String>, out: &mut dyn Write) -> CliResult {
    for n in notes.drain(..) {
        writeln!(out, "提示：{n}")?;
    }
    Ok(())
}

// ------------------------------------------------------------------ 命令

pub fn run(cli: Cli, out: &mut dyn Write, err: &mut dyn Write, stop: &AtomicBool) -> CliResult {
    let conn = cli.conn.clone();
    match cli.cmd {
        Cmd::List => cmd_list(out),
        Cmd::Vars { pattern } => cmd_vars(&conn, pattern.as_deref(), out),
        Cmd::Info { no_target } => {
            let syms = load_symbols(&conn)?;
            let mut probe = open_probe(&conn, &syms, err)?;
            cmd_info(&mut probe, &conn, !no_target, out)
        }
        Cmd::Read { exprs } => {
            let mut syms = load_symbols(&conn)?;
            let mut r = Resolver::new(&mut syms);
            let mut targets = Vec::new();
            for e in &exprs {
                targets.extend(r.resolve_all(e)?);
            }
            print_notes(&mut r.notes, out)?;
            let mut probe = open_probe(&conn, r.syms, err)?;
            connect_target(&mut probe, &conn)?;
            cmd_read(&mut probe, &targets, out)
        }
        Cmd::Write { assigns } => {
            let mut syms = load_symbols(&conn)?;
            let mut r = Resolver::new(&mut syms);
            let mut list = Vec::new();
            for a in &assigns {
                let Some((e, v)) = a.split_once('=') else {
                    return Err(format!("{a}：要写成 变量=值").into());
                };
                let t = r.resolve(e)?;
                let bytes = t.ty.parse(v)?;
                list.push((t, v.trim().to_string(), bytes));
            }
            print_notes(&mut r.notes, out)?;
            let mut probe = open_probe(&conn, r.syms, err)?;
            connect_target(&mut probe, &conn)?;
            cmd_write(&mut probe, &list, out)
        }
        Cmd::Wifi { op } => {
            let mut probe = open_probe(&conn, &SymbolTable::new(), err)?;
            cmd_wifi(&mut probe, &op, out)
        }
        Cmd::Halt | Cmd::Run | Cmd::Reset { .. } => {
            let syms = load_symbols(&conn)?;
            let mut probe = open_probe(&conn, &syms, err)?;
            connect_target(&mut probe, &conn)?;
            let s = match cli.cmd {
                Cmd::Halt => cortex::halt(&mut probe)?,
                Cmd::Run => cortex::resume(&mut probe)?,
                Cmd::Reset { halt } => {
                    let after = if halt {
                        AfterReset::Halt
                    } else {
                        AfterReset::Run
                    };
                    cortex::reset(&mut probe, after)?
                }
                _ => unreachable!(),
            };
            writeln!(out, "内核：{}（DHCSR 0x{:08X}）", s.state.text(), s.dhcsr)?;
            Ok(())
        }
        Cmd::Watch {
            exprs,
            period,
            csv,
            duration,
            no_overwrite,
            all,
        } => {
            let period_us = watch::parse_period(&period)?;
            let mut syms = load_symbols(&conn)?;
            let mut r = Resolver::new(&mut syms);
            let mut specs = Vec::new();
            for e in &exprs {
                let (expr, div, phase) = watch::parse_spec(e)?;
                for t in r.resolve_all(&expr)? {
                    specs.push((t, div, phase));
                }
            }
            print_notes(&mut r.notes, out)?;
            let mut probe = open_probe(&conn, r.syms, err)?;
            connect_target(&mut probe, &conn)?;
            let opts = WatchOpts {
                period_us,
                csv,
                duration: duration.map(Duration::from_secs_f64),
                stop_on_full: no_overwrite,
                print_all: all,
            };
            cmd_watch(&mut probe, specs, &opts, out, err, stop)
        }
    }
}

fn cmd_list(out: &mut dyn Write) -> CliResult {
    let probes = discover::group(discover::find(&discover::Options::default()));
    if probes.is_empty() {
        writeln!(out, "没有找到探针（USB、串口、局域网都没有）")?;
        return Ok(());
    }
    write!(out, "{}", discover::table(&probes))?;
    Ok(())
}

fn cmd_wifi(probe: &mut Probe, op: &WifiCmd, out: &mut dyn Write) -> CliResult {
    match op {
        WifiCmd::Status => writeln!(out, "{}", wifi::status(probe)?.describe())?,
        WifiCmd::Set { ssid, password } => {
            wifi::set(probe, ssid, password.as_deref().unwrap_or(""))?;
            writeln!(out, "已保存，探针重启后连接热点 {ssid}")?;
        }
        WifiCmd::Clear => {
            wifi::clear(probe)?;
            writeln!(out, "已清除，探针重启后不再连 Wi-Fi")?;
        }
    }
    Ok(())
}

fn cmd_vars(conn: &ConnOpts, pattern: Option<&str>, out: &mut dyn Write) -> CliResult {
    if conn.elf.is_none() {
        return Err("要用 --elf 指定 ELF 文件".into());
    }
    let syms = load_symbols(conn)?;
    let mut vars: Vec<_> = syms
        .vars
        .values()
        .flatten()
        .filter(|v| pattern.is_none_or(|p| v.name.contains(p)))
        .collect();
    vars.sort_by_key(|v| (v.addr, v.name.clone()));
    for v in vars {
        let size = syms
            .size_of(v.ty)
            .map(|s| format!("{s} 字节"))
            .unwrap_or_else(|| "大小未知".into());
        writeln!(
            out,
            "0x{:08X}  {:<24} {}，{}",
            v.addr,
            v.name,
            syms.name_of(v.ty),
            size
        )?;
    }
    Ok(())
}

fn cmd_info(probe: &mut Probe, conn: &ConnOpts, target: bool, out: &mut dyn Write) -> CliResult {
    writeln!(out, "连接：{}", probe.describe())?;
    let packet = probe.query_packet_size()?;
    let s = |p: &mut Probe, id| p.dap_info_string(id).ok().flatten();
    let vendor_name = s(probe, dap::ID_VENDOR).unwrap_or_default();
    let product = s(probe, dap::ID_PRODUCT).unwrap_or_default();
    let serial = s(probe, dap::ID_SERIAL).unwrap_or_else(|| "?".into());
    let fw = s(probe, dap::ID_FIRMWARE).unwrap_or_else(|| "?".into());
    let proto = s(probe, dap::ID_PROTOCOL).unwrap_or_else(|| "?".into());
    let count = probe
        .dap_info(dap::ID_PACKET_COUNT)
        .ok()
        .and_then(|d| d.first().copied())
        .unwrap_or(1);
    writeln!(
        out,
        "探针：{vendor_name} {product}，序列号 {serial}，固件 {fw}"
    )?;
    writeln!(
        out,
        "CMSIS-DAP：{proto}，包长 {packet} 字节，最多 {count} 个包在途"
    )?;
    match probe.vendor_info() {
        Ok(info) => {
            writeln!(
                out,
                "厂商扩展：版本 {}，功能 {}（0x{:02X}）",
                info.version,
                info.feature_names().join("、"),
                info.features
            )?;
            writeln!(
                out,
                "  每次最多 {} 个条目，最多 {} 级指针，采样缓冲 {} 字节",
                info.max_items, info.max_depth, info.ring_bytes
            )?;
            let calib = if info.access_us == 0 {
                "单次 SWD 访问耗时还没有标定（每次 SUB_SET 时按当前时钟标定）".to_string()
            } else {
                format!(
                    "单次 SWD 访问耗时 {} µs（上次 SUB_SET 时标定）",
                    info.access_us
                )
            };
            writeln!(
                out,
                "  探针时间 {:.6} s，{calib}",
                info.probe_time_us as f64 / 1e6
            )?;
        }
        Err(e) => writeln!(out, "厂商扩展：不可用（{e}）")?,
    }
    if target {
        match probe.connect_swd(conn.clock) {
            Ok(t) => {
                writeln!(out, "目标：{}，SWD 时钟 {} Hz", t.describe(), conn.clock)?;
                let core = cortex::info(probe).map(|i| i.describe());
                let state = cortex::status(probe);
                match (core, state) {
                    (Ok(c), Ok(st)) => writeln!(
                        out,
                        "内核：{c}，{}（DHCSR 0x{:08X}）",
                        st.state.text(),
                        st.dhcsr
                    )?,
                    (Err(e), _) | (_, Err(e)) => writeln!(out, "内核：读不到（{e}）")?,
                }
            }
            Err(e) => writeln!(out, "目标：连接失败（{e}）")?,
        }
    }
    Ok(())
}

fn name_width(targets: &[&Target]) -> usize {
    targets
        .iter()
        .map(|t| display_width(&t.name))
        .max()
        .unwrap_or(0)
}

fn cmd_read(probe: &mut Probe, targets: &[Target], out: &mut dyn Write) -> CliResult {
    let items: Vec<Item> = targets
        .iter()
        .enumerate()
        .map(|(i, t)| t.item(i as u16))
        .collect();
    let results = probe.read(&items)?;
    let w = name_width(&targets.iter().collect::<Vec<_>>());
    for (t, r) in targets.iter().zip(results) {
        if r.status == 0 {
            writeln!(out, "{} = {}", ljust(&t.name, w), t.ty.format(&r.value))?;
        } else {
            writeln!(
                out,
                "{}   读取失败：{}（地址 {}）",
                ljust(&t.name, w),
                vendor::item_status_text(r.status, &t.chain),
                t.location()
            )?;
        }
    }
    Ok(())
}

fn cmd_write(
    probe: &mut Probe,
    list: &[(Target, String, Vec<u8>)],
    out: &mut dyn Write,
) -> CliResult {
    let items: Vec<(Item, Vec<u8>)> = list
        .iter()
        .enumerate()
        .map(|(i, (t, _, b))| (t.item(i as u16), b.clone()))
        .collect();
    let results = probe.write(&items)?;
    let back = probe.read(&items.iter().map(|(i, _)| i.clone()).collect::<Vec<_>>())?;
    let w = name_width(&list.iter().map(|(t, _, _)| t).collect::<Vec<_>>());
    let mut failed = 0;
    for (((t, text, _), r), b) in list.iter().zip(results).zip(back) {
        if r.status == 0 {
            let readback = if b.status == 0 {
                t.ty.format(&b.value)
            } else {
                format!("读回失败：{}", vendor::item_status_text(b.status, &t.chain))
            };
            writeln!(out, "{} ← {text}，读回 {readback}", ljust(&t.name, w))?;
        } else {
            failed += 1;
            writeln!(
                out,
                "{}   写入失败：{}（地址 {}）",
                ljust(&t.name, w),
                vendor::item_status_text(r.status, &t.chain),
                t.location()
            )?;
        }
    }
    if failed > 0 {
        return Err(format!("{failed} 个变量写入失败").into());
    }
    Ok(())
}

// ------------------------------------------------------------------ watch

pub struct WatchOpts {
    pub period_us: u32,
    pub csv: Option<PathBuf>,
    pub duration: Option<Duration>,
    pub stop_on_full: bool,
    pub print_all: bool,
}

/// 终端里的显示宽度：中文等宽字符算 2 格。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if (c as u32) >= 0x1100 { 2 } else { 1 })
        .sum()
}

fn rjust(s: &str, w: usize) -> String {
    let pad = w.saturating_sub(display_width(s));
    format!("{}{s}", " ".repeat(pad))
}

fn ljust(s: &str, w: usize) -> String {
    let pad = w.saturating_sub(display_width(s));
    format!("{s}{}", " ".repeat(pad))
}

struct Output<'a> {
    targets: Vec<Target>,
    csv: Option<std::io::BufWriter<std::fs::File>>,
    out: &'a mut dyn Write,
    print_all: bool,
    widths: Vec<usize>,
    first_probe_us: Option<u64>,
    last_print: Option<Instant>,
    latest: Vec<Option<String>>,
}

impl Output<'_> {
    fn header(&mut self) -> CliResult {
        if let Some(csv) = &mut self.csv {
            let line = xrdap::csv::header(self.targets.iter().map(|t| t.name.as_str()));
            writeln!(csv, "{line}")?;
            return Ok(());
        }
        let mut line = format!("{} {}", rjust("时间(s)", 12), rjust("节拍", 10));
        for (t, w) in self.targets.iter().zip(&self.widths) {
            line.push(' ');
            line.push_str(&rjust(&t.name, *w));
        }
        writeln!(self.out, "{line}")?;
        Ok(())
    }

    fn frames(&mut self, frames: &[HostFrame]) -> CliResult {
        for f in frames {
            let first = *self.first_probe_us.get_or_insert(f.probe_us);
            let t = (f.probe_us as f64 - first as f64) / 1e6;
            let cells: Vec<Option<String>> = f
                .values
                .iter()
                .zip(&self.targets)
                .map(|(v, t)| xrdap::csv::cell(&t.ty, v))
                .collect();
            if let Some(csv) = &mut self.csv {
                writeln!(csv, "{}", xrdap::csv::row(f, t, &cells))?;
            }
            for (slot, c) in self.latest.iter_mut().zip(&cells) {
                if c.is_some() {
                    *slot = c.clone();
                }
            }
            let now = Instant::now();
            let due = self.print_all
                || self
                    .last_print
                    .is_none_or(|p| now.duration_since(p) >= Duration::from_millis(100));
            if due && self.csv.is_none() {
                self.last_print = Some(now);
                let mut line = format!("{t:>12.6} {:>10}", f.tick);
                for (c, w) in self.latest.iter().zip(&self.widths) {
                    line.push(' ');
                    line.push_str(&rjust(c.as_deref().unwrap_or("-"), *w));
                }
                writeln!(self.out, "{line}")?;
            }
        }
        Ok(())
    }
}

pub fn cmd_watch(
    probe: &mut Probe,
    specs: Vec<(Target, u16, Option<u16>)>,
    opts: &WatchOpts,
    out: &mut dyn Write,
    err: &mut dyn Write,
    stop: &AtomicBool,
) -> CliResult {
    if specs.len() > 32 {
        return Err(format!("一次最多采样 32 个变量，现在有 {} 个", specs.len()).into());
    }
    let mut items: Vec<SubItem> = specs
        .iter()
        .enumerate()
        .map(|(i, (t, div, phase))| SubItem {
            item: t.item(i as u16),
            div: *div,
            phase: phase.unwrap_or(0),
        })
        .collect();
    let fixed: Vec<bool> = specs.iter().map(|(_, _, p)| p.is_some()).collect();
    vendor::assign_phases(&mut items, &fixed);
    let targets: Vec<Target> = specs.into_iter().map(|(t, _, _)| t).collect();
    let ring_bytes = probe.vendor_info().map(|i| i.ring_bytes).unwrap_or(16384);

    let mut sampler = match Sampler::start(probe, items, opts.period_us, opts.stop_on_full) {
        Ok(s) => s,
        Err(StartError::Rejected(r)) => {
            let est = r.est_frame_us.unwrap_or(0);
            return Err(format!(
                "探针拒绝了这个配置：估计每帧耗时 {est} µs，周期 {} µs 来不及。\
                 可以把周期改为 {} µs 以上（--period {}us），或者减少变量、给慢变量加分频（例如 变量@10）",
                opts.period_us,
                watch::suggest_period(est),
                watch::suggest_period(est)
            )
            .into());
        }
        Err(StartError::Other(e)) => return Err(e.into()),
    };

    writeln!(
        out,
        "采样 {} 个变量，周期 {} µs，估计每帧耗时 {} µs，一帧最多 {} 字节",
        targets.len(),
        opts.period_us,
        sampler.setup.est_frame_us.unwrap_or(0),
        sampler.setup.max_frame_bytes.unwrap_or(0)
    )?;
    for (t, s) in targets.iter().zip(sampler.items()) {
        if s.div > 1 {
            writeln!(out, "  {} 每 {} 拍采一次，相位 {}", t.name, s.div, s.phase)?;
        }
    }
    let csv = match &opts.csv {
        Some(path) => {
            writeln!(out, "写入 {}，按 Ctrl-C 停止", path.display())?;
            Some(std::io::BufWriter::new(std::fs::File::create(path)?))
        }
        None => {
            writeln!(out, "按 Ctrl-C 停止")?;
            None
        }
    };
    let widths = targets
        .iter()
        .map(|t| display_width(&t.name).max(12))
        .collect();
    let latest = vec![None; targets.len()];
    let mut output = Output {
        targets,
        csv,
        out,
        print_all: opts.print_all,
        widths,
        first_probe_us: None,
        last_print: None,
        latest,
    };
    output.header()?;

    let started = Instant::now();
    let interval = sampler.poll_interval(ring_bytes);
    let mut last_status = Instant::now();
    let result: CliResult = (|| {
        while !stop.load(Ordering::SeqCst) {
            if opts.duration.is_some_and(|d| started.elapsed() >= d) {
                break;
            }
            let (frames, remaining) = sampler.poll()?;
            output.frames(&frames)?;
            if output.csv.is_some() && last_status.elapsed() >= Duration::from_secs(1) {
                last_status = Instant::now();
                let st = sampler.stats();
                write!(
                    err,
                    "\r已采 {} 帧，跳过节拍 {}，缓冲丢帧 {}   ",
                    st.frames, st.skipped, st.dropped
                )?;
                err.flush()?;
            }
            if remaining == 0 {
                let mut wait = interval;
                if let Some(d) = opts.duration {
                    wait = wait.min(d.saturating_sub(started.elapsed()));
                }
                std::thread::sleep(wait);
            }
        }
        Ok(())
    })();
    // 出错时也要停止采样；stop() 顺便读完缓冲里剩下的帧
    let rest = sampler.stop();
    result?;
    output.frames(&rest?)?;
    if let Some(csv) = &mut output.csv {
        csv.flush()?;
        writeln!(err)?;
    }
    let st = sampler.stats();
    let secs = started.elapsed().as_secs_f64();
    writeln!(
        output.out,
        "已停止：{} 帧，用时 {secs:.2} s；探针跳过节拍 {}，缓冲丢帧 {}，帧序号缺口 {}，读取失败的值 {}",
        st.frames, st.skipped, st.dropped, st.missing, st.failed_values
    )?;
    if let (Some((_, half_rtt)), Some(path)) = (sampler.clock_offset(), &opts.csv) {
        writeln!(
            output.out,
            "{} 的 host_time 列是对到电脑时钟的 Unix 时间（秒），按 SUB_READ 的往返时间估计，误差约 ±{half_rtt} µs",
            path.display()
        )?;
    }
    if st.skipped > 0 {
        writeln!(
            output.out,
            "提示：探针来不及按这个周期采样，可以增大周期、加分频或减少变量"
        )?;
    }
    if st.dropped > 0 {
        writeln!(output.out, "提示：读得太慢，缓冲满了，可以加分频或减少变量")?;
    }
    Ok(())
}
