//! 命令行。每个子命令只接受自己用得到的选项；命令的输出写到传入的 Write，测试里对着模拟探针
//! 跑整个命令。帮助文字按 [`xrdap::lang`] 选中文或英文。

mod connect;
mod memory;
mod probe;
mod share;
mod text;
mod watch;

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use xrdap::expr::Resolver;
use xrdap::lang::tr;
use xrdap::trf;

pub use connect::{ElfOpts, ProbeOpts, TargetOpts};
pub use watch::{cmd_watch, WatchOpts};

use connect::{open_probe, open_target, print_notes};

pub type CliResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// 所有命令的帮助模板。
fn template() -> &'static str {
    tr(
        "{about-with-newline}\nUsage: {usage}\n\n{all-args}{after-help}",
        "{about-with-newline}\n用法：{usage}\n\n{all-args}{after-help}",
    )
}

/// 选项的标题。
fn options() -> &'static str {
    tr("Options", "选项")
}

/// 位置参数的标题。
fn arguments() -> &'static str {
    tr("Arguments", "参数")
}

#[derive(Parser, Debug)]
#[command(
    name = "xrdap",
    bin_name = "xrdap",
    version,
    about = tr(
        "XR-DAP probe tool. Without a command it opens the GUI",
        "XR-DAP 探针工具。不带命令运行时打开图形界面",
    ),
    help_template = template(),
    subcommand_help_heading = tr("Commands", "命令"),
    subcommand_value_name = tr("COMMAND", "命令"),
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    after_help = tr(
        "\
The probe is given with -p (serial number or its last digits, usb, COM3, /dev/ttyUSB0,
192.0.2.10, xrdap-aabbcc.local); without -p the only probe is used; xrdap list lists all
probes.
Variables are written as counter, state.speed, motor[1].pid->out, *ptr, p->q->x,
  type casts such as (uint32_t***)p, or a raw address 0x20000010:u32 (types are u8/u16/u32/u64,
  i8/i16/i32/i64, f32/f64 and type names from the ELF, optionally followed by *).
xrdap <command> -h shows the options of each command.",
        "\
探针用 -p 指定（序列号或末尾几位、usb、COM3、/dev/ttyUSB0、192.0.2.10、
xrdap-aabbcc.local），不写时自动选唯一的那块；xrdap list 列出所有探针。
变量的写法：counter、state.speed、motor[1].pid->out、*ptr、p->q->x、
  (uint32_t***)p 这样的类型转换，或原始地址 0x20000010:u32（类型可以是 u8/u16/u32/u64、
  i8/i16/i32/i64、f32/f64，以及 ELF 里的类型名，可以带 *）。
xrdap 命令 -h 看每个命令的选项。",
    )
)]
pub struct Cli {
    /// 不写时打开图形界面
    #[command(subcommand)]
    pub cmd: Option<Cmd>,

    #[arg(short = 'h', long, action = ArgAction::Help, global = true, help_heading = options(),
        help = tr("Show help", "显示帮助"))]
    help: Option<bool>,

    #[arg(short = 'V', long, action = ArgAction::Version, help_heading = options(),
        help = tr("Show version", "显示版本"))]
    version: Option<bool>,
}

/// 连上目标的命令都有的选项：探针和 SWD 时钟。
#[derive(Args, Debug, Clone)]
pub struct Conn {
    #[command(flatten)]
    pub probe: ProbeOpts,
    #[command(flatten)]
    pub target: TargetOpts,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    #[command(
        about = tr(
            "List the probes on USB and the LAN and the Espressif USB serial ports recognized by \
             their descriptors, with the -p form that selects each. Opens no serial port by default",
            "列出 USB、局域网上的探针和按描述符认出的乐鑫 USB 串口，以及选它们时 -p 的写法。 \
             默认不打开任何串口",
        ),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    List {
        #[arg(long, help = tr(
            "Open each serial port that may have a probe on it and send DAP_Info (opening a port \
             may reset the development board connected to it)",
            "逐个打开可能接着探针的串口，发 DAP_Info 询问（打开串口可能让接在上面的开发板复位）",
        ))]
        ask_serial: bool,
    },

    #[command(
        about = tr(
            "Show probe information: DAP_Info, the vendor extension's INFO, and connect the target \
             to read DPIDR",
            "显示探针信息：DAP_Info、厂商扩展 INFO，并连接目标读 DPIDR",
        ),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Info {
        #[command(flatten)]
        conn: Conn,
        #[arg(long, help = tr(
            "Query the probe only, do not connect the target",
            "不连接目标，只查询探针",
        ))]
        no_target: bool,
    },

    #[command(
        about = tr("List the variables in the ELF", "列出 ELF 里的变量"),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Vars {
        #[arg(long, value_name = tr("FILE", "文件"), help = tr(
            "The target program's ELF (built with -g)",
            "目标程序的 ELF（带 -g 调试信息）",
        ))]
        elf: PathBuf,
        #[arg(help_heading = arguments(), value_name = tr("TEXT", "文字"), help = tr(
            "List only the variables whose names contain this text",
            "只列出名字里带这段文字的变量",
        ))]
        pattern: Option<String>,
    },

    #[command(
        about = tr(
            "Read variables. Structs and arrays are expanded into their members",
            "读变量。结构体和数组会展开成各个成员",
        ),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Read {
        #[command(flatten)]
        conn: Conn,
        #[command(flatten)]
        elf: ElfOpts,
        #[arg(required = true, help_heading = arguments(), value_name = tr("VAR", "变量"),
            help = tr("Variables to read, one or more", "要读的变量，可以写多个"))]
        exprs: Vec<String>,
    },

    #[command(
        about = tr(
            "Write variables and read each one back",
            "写变量，写完读回一次",
        ),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Write {
        #[command(flatten)]
        conn: Conn,
        #[command(flatten)]
        elf: ElfOpts,
        #[arg(required = true, help_heading = arguments(), value_name = tr("VAR=VALUE", "变量=值"),
            help = tr(
                "VAR=VALUE, one or more. Integers in decimal or 0x hexadecimal, floating-point \
                 numbers as decimals, enumerations by name",
                "变量=值，可以写多个。整数可以写十进制或 0x 十六进制，浮点写小数，枚举可以写名字",
            ))]
        assigns: Vec<String>,
    },

    #[command(
        about = tr(
            "Sample variables on the probe at a fixed period, print them or write CSV. Ctrl-C stops",
            "在探针上按固定周期采样变量，打印或写成 CSV。按 Ctrl-C 停止",
        ),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Watch {
        #[command(flatten)]
        conn: Conn,
        #[command(flatten)]
        elf: ElfOpts,
        #[arg(required = true, help_heading = arguments(), value_name = tr("VAR[@DIV]", "变量[@分频]"),
            help = tr(
                "Variables to sample; VAR@DIV samples every DIV ticks, VAR@DIV:PHASE also sets \
                 the phase",
                "要采样的变量，变量@分频 表示每几拍采一次，变量@分频:相位 可以指定相位",
            ))]
        exprs: Vec<String>,

        #[arg(
            long,
            default_value = "1ms",
            hide_default_value = true,
            value_name = tr("PERIOD", "周期"),
            help = tr(
                "Sampling period, e.g. 1000 (µs), 500us, 2ms (default 1ms)",
                "采样周期，例如 1000（µs）、500us、2ms（默认 1ms）",
            )
        )]
        period: String,

        #[arg(long, value_name = tr("FILE", "文件"), help = tr(
            "Write every frame to a CSV file. Cells not due are empty, failed reads are ERR",
            "把每一帧写进 CSV 文件。不到期的格为空，读取失败的格为 ERR",
        ))]
        csv: Option<PathBuf>,

        #[arg(long, value_name = tr("SECONDS", "秒"), help = tr(
            "Stop after this many seconds; without it sampling runs until Ctrl-C",
            "采样多少秒后停止；不写时一直采到 Ctrl-C",
        ))]
        duration: Option<f64>,

        #[arg(long, help = tr(
            "Drop new frames when the buffer is full (default: overwrite the oldest frame)",
            "缓冲满后丢弃新帧（默认覆盖最旧的帧）",
        ))]
        no_overwrite: bool,

        #[arg(long, help = tr(
            "Print every frame in the terminal (default: at most one line per 0.1 s)",
            "终端里打印每一帧（默认最多每 0.1 秒打印一行）",
        ))]
        all: bool,
    },

    #[command(
        about = tr("Halt the target core", "停住目标内核"),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Halt {
        #[command(flatten)]
        conn: Conn,
    },

    #[command(
        about = tr("Resume the halted target core", "让停住的目标内核继续运行"),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Run {
        #[command(flatten)]
        conn: Conn,
    },

    #[command(
        about = tr(
            "Reset the target (AIRCR.SYSRESETREQ); it runs after the reset by default",
            "复位目标（AIRCR.SYSRESETREQ），默认复位后运行",
        ),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Reset {
        #[command(flatten)]
        conn: Conn,
        #[arg(long, help = tr(
            "Stop at the reset vector after the reset instead of running",
            "复位后停在复位向量，不运行",
        ))]
        halt: bool,
    },

    #[command(
        about = tr(
            "Query, set or clear the probe's Wi-Fi credentials (over DAP itself, no separate \
             serial terminal needed)",
            "查询、设置、清除探针的 Wi-Fi 热点参数（经 DAP 本身，不用另开串口终端）",
        ),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Wifi {
        #[command(subcommand)]
        op: WifiCmd,
    },

    #[command(
        about = tr(
            "Share the probe with other debug tools (Keil, pyOCD, probe-rs, OpenOCD); Ctrl-C stops",
            "把探针共享给别的调试工具（Keil、pyOCD、probe-rs、OpenOCD），按 Ctrl-C 停止",
        ),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Share {
        #[command(flatten)]
        probe: ProbeOpts,
        #[arg(value_enum, help_heading = arguments(), value_name = tr("MODE", "方式"), help = tr(
            "usbip: appear as a standard USB CMSIS-DAP probe, attached with usbip attach; \
             openocd: OpenOCD's cmsis-dap TCP backend",
            "usbip：变成标准 USB CMSIS-DAP 探针，用 usbip attach 挂载； \
             openocd：OpenOCD 的 cmsis-dap TCP 后端",
        ))]
        protocol: ShareKind,

        #[arg(long, value_name = tr("ADDR:PORT", "地址:端口"), help = tr(
            "Listening address, 127.0.0.1:3240 (usbip) or 127.0.0.1:3241 (openocd) by default; \
             write 0.0.0.0:PORT to serve other computers on the LAN",
            "监听地址，默认 127.0.0.1:3240（usbip）或 127.0.0.1:3241（openocd）； \
             给局域网里别的电脑用时写 0.0.0.0:端口",
        ))]
        listen: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum WifiCmd {
    #[command(
        about = tr(
            "Show the SSID, whether the probe is connected, its IP and the signal strength \
             (not the password)",
            "显示热点名、是否连接、IP、信号强度（不显示密码）",
        ),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Status {
        #[command(flatten)]
        probe: ProbeOpts,
    },

    #[command(
        about = tr(
            "Save the credentials; the probe then restarts and joins this hotspot",
            "保存热点参数，探针随后重启并连接这个热点",
        ),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Set {
        #[command(flatten)]
        probe: ProbeOpts,
        #[arg(help_heading = arguments(), value_name = tr("SSID", "热点"),
            help = tr("Hotspot name (SSID)", "热点名"))]
        ssid: String,
        #[arg(help_heading = arguments(), value_name = tr("PASSWORD", "密码"), help = tr(
            "Password, 8 to 63 bytes; omitted for an open network. A password on the command line \
             stays in the shell history and the process list; --password-stdin avoids that",
            "密码，8 到 63 个字节；开放网络不写。写在命令行上的密码会留在 shell 历史和进程列表里， \
             可以改用 --password-stdin",
        ))]
        password: Option<String>,
        #[arg(long, conflicts_with = "password", help = tr(
            "Read one line from standard input as the password (not through a command-line \
             argument). Over TCP the password crosses the network in plaintext",
            "从标准输入读一行作为密码（不经过命令行参数）。经 TCP 连接探针时密码以明文经过网络",
        ))]
        password_stdin: bool,
    },

    #[command(
        about = tr(
            "Clear the credentials; the probe then restarts and no longer joins Wi-Fi",
            "清除热点参数，探针随后重启，不再连 Wi-Fi",
        ),
        help_template = template(),
        disable_help_flag = true,
        next_help_heading = options()
    )]
    Clear {
        #[command(flatten)]
        probe: ProbeOpts,
    },
}

/// `xrdap share` 的方式
#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum ShareKind {
    Usbip,
    Openocd,
}

/// 内核操作
#[derive(Debug, Clone, Copy)]
enum CoreOp {
    Halt,
    Run,
    Reset { halt: bool },
}

/// 执行一个命令。`cmd` 为 None（图形界面）时什么都不做；`stop` 置位时 watch 和 share 正常结束。
pub fn run(cli: Cli, out: &mut dyn Write, err: &mut dyn Write, stop: &AtomicBool) -> CliResult {
    let Some(cmd) = cli.cmd else {
        return Ok(());
    };
    match cmd {
        Cmd::List { ask_serial } => probe::cmd_list(ask_serial, out),
        Cmd::Vars { elf, pattern } => memory::cmd_vars(&elf, pattern.as_deref(), out),
        Cmd::Info { conn, no_target } => {
            let mut probe = open_probe(&conn.probe, err)?;
            let clock = (!no_target).then_some(conn.target.clock);
            probe::cmd_info(&mut probe, clock, out)
        }
        Cmd::Read { conn, elf, exprs } => {
            let mut syms = elf.load()?;
            let mut r = Resolver::new(&mut syms);
            let mut targets = Vec::new();
            for e in &exprs {
                targets.extend(elf.resolve_all(&mut r, e)?);
            }
            print_notes(&mut r.notes, out)?;
            let mut probe = open_target(&conn.probe, &conn.target, err)?;
            memory::cmd_read(&mut probe, &targets, out)
        }
        Cmd::Write { conn, elf, assigns } => {
            let mut syms = elf.load()?;
            let mut r = Resolver::new(&mut syms);
            let mut list = Vec::new();
            for a in &assigns {
                let Some((e, v)) = a.split_once('=') else {
                    return Err(trf!("{a}: write it as VAR=VALUE", "{a}：要写成 变量=值").into());
                };
                let t = elf.resolve(&mut r, e)?;
                let bytes = t.ty.parse(v)?;
                list.push((t, v.trim().to_string(), bytes));
            }
            print_notes(&mut r.notes, out)?;
            let mut probe = open_target(&conn.probe, &conn.target, err)?;
            memory::cmd_write(&mut probe, &list, out)
        }
        Cmd::Watch {
            conn,
            elf,
            exprs,
            period,
            csv,
            duration,
            no_overwrite,
            all,
        } => {
            let period_us = xrdap::watch::parse_period(&period)?;
            let mut syms = elf.load()?;
            let mut r = Resolver::new(&mut syms);
            let mut specs = Vec::new();
            for e in &exprs {
                let (expr, div, phase) = xrdap::watch::parse_spec(e)?;
                for t in elf.resolve_all(&mut r, &expr)? {
                    specs.push((t, div, phase));
                }
            }
            print_notes(&mut r.notes, out)?;
            let mut probe = open_target(&conn.probe, &conn.target, err)?;
            let opts = WatchOpts {
                period_us,
                csv,
                duration: duration.map(Duration::from_secs_f64),
                stop_on_full: no_overwrite,
                print_all: all,
            };
            cmd_watch(&mut probe, specs, &opts, out, err, stop)
        }
        Cmd::Halt { conn } => core(&conn, CoreOp::Halt, out, err),
        Cmd::Run { conn } => core(&conn, CoreOp::Run, out, err),
        Cmd::Reset { conn, halt } => core(&conn, CoreOp::Reset { halt }, out, err),
        Cmd::Wifi { op } => {
            let (WifiCmd::Status { probe } | WifiCmd::Set { probe, .. } | WifiCmd::Clear { probe }) =
                &op;
            let mut p = open_probe(probe, err)?;
            probe::cmd_wifi(&mut p, &op, &mut std::io::stdin().lock(), out)
        }
        Cmd::Share {
            probe,
            protocol,
            listen,
        } => {
            let p = open_probe(&probe, err)?;
            share::cmd_share(p, protocol, listen.as_deref(), out, stop)
        }
    }
}

fn core(conn: &Conn, op: CoreOp, out: &mut dyn Write, err: &mut dyn Write) -> CliResult {
    let mut probe = open_target(&conn.probe, &conn.target, err)?;
    probe::cmd_core(&mut probe, op, out)
}
