//! 命令的选项组：用哪块探针、目标的 SWD 时钟、目标程序的 ELF。每个子命令只挂它用得到的组。

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use xrdap::addr::{Addr, Spec};
use xrdap::dap::{self, Probe};
use xrdap::expr::{Resolver, Target};
use xrdap::open::{self, OpenOptions};
use xrdap::types::SymbolTable;
use xrdap::Error;

use super::CliResult;

/// 用哪块探针、怎么连。
#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "探针选项")]
pub struct ProbeOpts {
    /// 用哪块探针：序列号（全称或末尾几位）、usb、COM3、/dev/ttyUSB0、192.0.2.10、xrdap-aabbcc.local。不写时自动选唯一的那块；xrdap list 列出所有探针和对应的写法
    #[arg(short = 'p', long, value_name = "探针")]
    pub probe: Option<String>,

    /// 串口波特率，要和探针固件一致。不写时按串口芯片自动选：CH343 等先试 4000000，CP2102、CH340 用 921600（传统 ESP32），其他芯片两个都试
    #[arg(long, value_name = "波特率")]
    pub baud: Option<u32>,

    /// 等待一个响应的超时，毫秒（默认 2000）
    #[arg(
        long,
        default_value_t = 2000,
        hide_default_value = true,
        value_name = "毫秒"
    )]
    pub timeout_ms: u64,
}

/// 连接目标的参数。
#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "目标选项")]
pub struct TargetOpts {
    /// SWD 时钟，例如 4000000、4M、500k（默认 4M）
    #[arg(long, default_value = "4M", hide_default_value = true, value_parser = parse_hz, value_name = "Hz")]
    pub clock: u32,
}

/// 按名字找变量用的 ELF。
#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "目标选项")]
pub struct ElfOpts {
    /// 目标程序的 ELF（带 -g 调试信息），用来按名字找变量和类型；不写时只能写原始地址，例如 0x20000000:u32
    #[arg(long, value_name = "文件")]
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
            (None, Error::Usage(msg)) => format!("{msg}（按名字找变量要用 --elf 指定 ELF）").into(),
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
        writeln!(err, "打开{addr}，等探针应答……")?;
    }
    open::open(&spec, &open_opts).map_err(|e| match e {
        Error::Select(msg) => format!("{msg}。用 -p 指定一块探针，xrdap list 列出全部").into(),
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
    xrdap::watch::connect(&mut p, target.clock).map_err(|e| format!("连接目标失败：{e}"))?;
    Ok(p)
}

pub fn print_notes(notes: &mut Vec<String>, out: &mut dyn Write) -> CliResult {
    for n in notes.drain(..) {
        writeln!(out, "提示：{n}")?;
    }
    Ok(())
}
