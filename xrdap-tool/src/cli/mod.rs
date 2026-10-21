//! 命令行。命令的输出写到传入的 Write，测试里用模拟探针跑整个命令。

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

use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use xrdap::expr::Resolver;
use xrdap::types::SymbolTable;

pub use connect::ConnOpts;
pub use watch::{cmd_watch, WatchOpts};

use connect::{connect_target, load_symbols, open_probe, print_notes};

pub type CliResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const TEMPLATE: &str = "{about-with-newline}\n用法：{usage}\n\n{all-args}{after-help}";

#[derive(Parser, Debug)]
#[command(
    name = "xrdap",
    bin_name = "xrdap",
    version,
    about = "XR-DAP 探针工具。不带命令运行时打开图形界面",
    help_template = TEMPLATE,
    subcommand_help_heading = "命令",
    subcommand_value_name = "命令",
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    after_help = "\
用 --probe 指定探针（序列号或末尾几位、usb、COM7、/dev/ttyACM0、192.168.0.108、
xrdap-93ef1c.local），不写时自动选唯一的那块；xrdap list 列出所有探针。
变量的写法：counter、state.speed、motor[1].pid->out、*ptr、p->q->x、
  (uint32_t***)p 这样的类型转换，或原始地址 0x20000010:u32（类型可以是 u8/u16/u32/u64、
  i8/i16/i32/i64、f32/f64，以及 ELF 里的类型名，可以带 *）。"
)]
pub struct Cli {
    #[command(flatten)]
    pub conn: ConnOpts,

    /// 不写时打开图形界面
    #[command(subcommand)]
    pub cmd: Option<Cmd>,

    /// 显示帮助
    #[arg(short = 'h', long, action = ArgAction::Help, global = true, help_heading = "选项")]
    help: Option<bool>,

    /// 显示版本
    #[arg(short = 'V', long, action = ArgAction::Version, help_heading = "选项")]
    version: Option<bool>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// 列出 USB、串口、局域网上的探针，以及选它们时 -p 的写法。默认不打开串口
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    List {
        /// 逐个打开可能接着探针的串口，发 DAP_Info 询问（打开串口可能让接在上面的开发板复位）
        #[arg(long)]
        ask_serial: bool,
    },

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

    /// 查询、设置、清除探针的 Wi-Fi 热点参数（经 DAP 本身，不用另开串口终端）
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    Wifi {
        #[command(subcommand)]
        op: WifiCmd,
    },

    /// 把探针共享给别的调试工具（Keil、pyOCD、probe-rs、OpenOCD），按 Ctrl-C 停止
    #[command(help_template = TEMPLATE, disable_help_flag = true, next_help_heading = "选项")]
    Share {
        /// usbip：变成标准 USB CMSIS-DAP 探针，用 usbip attach 挂载；
        /// openocd：OpenOCD 的 cmsis-dap TCP 后端
        #[arg(value_enum, help_heading = "参数", value_name = "方式")]
        protocol: ShareKind,

        /// 监听地址，默认 127.0.0.1:3240（usbip）或 127.0.0.1:3241（openocd）；
        /// 给局域网里别的电脑用时写 0.0.0.0:端口
        #[arg(long, value_name = "地址:端口")]
        listen: Option<String>,
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
    let conn = &cli.conn;
    let Some(cmd) = cli.cmd else {
        return Ok(());
    };
    match cmd {
        Cmd::List { ask_serial } => probe::cmd_list(ask_serial, out),
        Cmd::Vars { pattern } => memory::cmd_vars(conn, pattern.as_deref(), out),
        Cmd::Info { no_target } => {
            let syms = load_symbols(conn)?;
            let mut probe = open_probe(conn, &syms, err)?;
            probe::cmd_info(&mut probe, conn, !no_target, out)
        }
        Cmd::Read { exprs } => {
            let mut syms = load_symbols(conn)?;
            let mut r = Resolver::new(&mut syms);
            let mut targets = Vec::new();
            for e in &exprs {
                targets.extend(r.resolve_all(e)?);
            }
            print_notes(&mut r.notes, out)?;
            let mut probe = open_probe(conn, r.syms, err)?;
            connect_target(&mut probe, conn)?;
            memory::cmd_read(&mut probe, &targets, out)
        }
        Cmd::Write { assigns } => {
            let mut syms = load_symbols(conn)?;
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
            let mut probe = open_probe(conn, r.syms, err)?;
            connect_target(&mut probe, conn)?;
            memory::cmd_write(&mut probe, &list, out)
        }
        Cmd::Watch {
            exprs,
            period,
            csv,
            duration,
            no_overwrite,
            all,
        } => {
            let period_us = xrdap::watch::parse_period(&period)?;
            let mut syms = load_symbols(conn)?;
            let mut r = Resolver::new(&mut syms);
            let mut specs = Vec::new();
            for e in &exprs {
                let (expr, div, phase) = xrdap::watch::parse_spec(e)?;
                for t in r.resolve_all(&expr)? {
                    specs.push((t, div, phase));
                }
            }
            print_notes(&mut r.notes, out)?;
            let mut probe = open_probe(conn, r.syms, err)?;
            connect_target(&mut probe, conn)?;
            let opts = WatchOpts {
                period_us,
                csv,
                duration: duration.map(Duration::from_secs_f64),
                stop_on_full: no_overwrite,
                print_all: all,
            };
            cmd_watch(&mut probe, specs, &opts, out, err, stop)
        }
        Cmd::Halt => core(conn, CoreOp::Halt, out, err),
        Cmd::Run => core(conn, CoreOp::Run, out, err),
        Cmd::Reset { halt } => core(conn, CoreOp::Reset { halt }, out, err),
        Cmd::Wifi { op } => {
            let mut probe = open_probe(conn, &SymbolTable::new(), err)?;
            probe::cmd_wifi(&mut probe, &op, out)
        }
        Cmd::Share { protocol, listen } => {
            let probe = open_probe(conn, &SymbolTable::new(), err)?;
            share::cmd_share(probe, protocol, listen.as_deref(), out, stop)
        }
    }
}

fn core(conn: &ConnOpts, op: CoreOp, out: &mut dyn Write, err: &mut dyn Write) -> CliResult {
    let syms = load_symbols(conn)?;
    let mut probe = open_probe(conn, &syms, err)?;
    probe::cmd_core(&mut probe, conn, op, out)
}
