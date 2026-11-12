//! 命令的选项组：用哪块探针、目标的 SWD 时钟、目标程序的 ELF。每个子命令只挂它用得到的组。

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use xrdap::addr::{Addr, Spec};
use xrdap::dap::{self, Probe};
use xrdap::expr::{Resolver, Target};
use xrdap::lang::tr;
use xrdap::open::{self, OpenOptions};
use xrdap::trf;
use xrdap::types::SymbolTable;
use xrdap::Error;

use super::CliResult;

/// 用哪块探针、怎么连。
#[derive(Args, Debug, Clone)]
#[command(next_help_heading = tr("Probe options", "探针选项"))]
pub struct ProbeOpts {
    #[arg(short = 'p', long, value_name = tr("PROBE", "探针"), help = tr(
        "The probe: serial number (in full or its last digits), usb, COM3, /dev/ttyUSB0, \
         192.0.2.10, xrdap-aabbcc.local. Without it the only probe is used; xrdap list lists all \
         probes and how to write each",
        "用哪块探针：序列号（全称或末尾几位）、usb、COM3、/dev/ttyUSB0、192.0.2.10、\
         xrdap-aabbcc.local。不写时自动选唯一的那块；xrdap list 列出所有探针和对应的写法",
    ))]
    pub probe: Option<String>,

    #[arg(long, value_name = tr("BAUD", "波特率"), help = tr(
        "Serial baud rate, the same as the probe firmware's. Without it the rate follows the \
         serial chip: CH343 and similar try 4000000 first, CP2102 and CH340 use 921600 (classic \
         ESP32), other chips try both",
        "串口波特率，要和探针固件一致。不写时按串口芯片自动选：CH343 等先试 4000000，CP2102、\
         CH340 用 921600（传统 ESP32），其他芯片两个都试",
    ))]
    pub baud: Option<u32>,

    #[arg(
        long,
        default_value_t = 2000,
        hide_default_value = true,
        value_name = tr("MS", "毫秒"),
        help = tr(
            "Timeout for one response, in milliseconds (default 2000)",
            "等待一个响应的超时，毫秒（默认 2000）",
        )
    )]
    pub timeout_ms: u64,
}

/// 连接目标的参数。
#[derive(Args, Debug, Clone)]
#[command(next_help_heading = tr("Target options", "目标选项"))]
pub struct TargetOpts {
    #[arg(long, default_value = "4M", hide_default_value = true, value_parser = parse_hz, value_name = "Hz",
        help = tr(
            "SWD clock, e.g. 4000000, 4M, 500k (default 4M)",
            "SWD 时钟，例如 4000000、4M、500k（默认 4M）",
        ))]
    pub clock: u32,
}

/// 按名字找变量用的 ELF。
#[derive(Args, Debug, Clone)]
#[command(next_help_heading = tr("Target options", "目标选项"))]
pub struct ElfOpts {
    #[arg(long, value_name = tr("FILE", "文件"), help = tr(
        "The target program's ELF (built with -g), to look up variables and types by name; \
         without it only raw addresses work, e.g. 0x20000000:u32",
        "目标程序的 ELF（带 -g 调试信息），用来按名字找变量和类型；不写时只能写原始地址，\
         例如 0x20000000:u32",
    ))]
    pub elf: Option<PathBuf>,
}

impl ElfOpts {
    /// 读 ELF 的符号表；没写 `--elf` 时是空表。
    pub fn load(&self) -> CliResult<SymbolTable> {
        match &self.elf {
            Some(path) => Ok(xrdap::elf::load(path)?),
            None => Ok(SymbolTable::new()),
        }
    }

    /// 解析一个表达式（结构体、数组展开成成员）；没写 `--elf` 又按名字找变量时提示用 `--elf`。
    pub fn resolve_all(&self, r: &mut Resolver, expr: &str) -> CliResult<Vec<Target>> {
        r.resolve_all(expr).map_err(|e| self.hint(e))
    }

    /// 解析一个表达式，结果必须是一个数值。
    pub fn resolve(&self, r: &mut Resolver, expr: &str) -> CliResult<Target> {
        r.resolve(expr).map_err(|e| self.hint(e))
    }

    fn hint(&self, e: Error) -> Box<dyn std::error::Error> {
        match (&self.elf, e) {
            (None, Error::Usage(msg)) => trf!(
                "{msg} (looking up variables by name needs the ELF given with --elf)",
                "{msg}（按名字找变量要用 --elf 指定 ELF）"
            )
            .into(),
            (_, e) => e.into(),
        }
    }
}

fn parse_hz(s: &str) -> Result<u32, String> {
    dap::parse_clock(s).map_err(|e| e.to_string())
}

/// 按探针选项打开探针（不连接目标）。
pub fn open_probe(opts: &ProbeOpts, err: &mut dyn Write) -> CliResult<Probe> {
    let open_opts = OpenOptions {
        timeout: Duration::from_millis(opts.timeout_ms),
        baud: opts.baud,
        ..OpenOptions::default()
    };
    let spec = Spec::parse(opts.probe.as_deref().unwrap_or(""));
    if let Spec::Addr(addr @ Addr::Serial { .. }) = &spec {
        writeln!(
            err,
            "{}",
            trf!(
                "Opening {addr}, waiting for the probe to answer...",
                "打开{addr}，等探针应答……"
            )
        )?;
    }
    open::open(&spec, &open_opts).map_err(|e| match e {
        Error::Select(msg) => trf!(
            "{msg}. Select a probe with -p; xrdap list lists them all",
            "{msg}。用 -p 指定一块探针，xrdap list 列出全部"
        )
        .into(),
        e => e.into(),
    })
}

/// 打开探针并连接目标。
pub fn open_target(
    probe: &ProbeOpts,
    target: &TargetOpts,
    err: &mut dyn Write,
) -> CliResult<Probe> {
    let mut p = open_probe(probe, err)?;
    p.connect(target.clock)
        .map_err(|e| trf!("Connecting the target failed: {e}", "连接目标失败：{e}"))?;
    Ok(p)
}

pub fn print_notes(notes: &mut Vec<String>, out: &mut dyn Write) -> CliResult {
    for n in notes.drain(..) {
        writeln!(out, "{}", trf!("Note: {n}", "提示：{n}"))?;
    }
    Ok(())
}
