//! 连接选项：用哪块探针、怎么连、目标程序的 ELF。

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use xrdap::dap::{self, Probe};
use xrdap::fake::FakeProbe;
use xrdap::open::{self, Addr, OpenOptions, Spec};
use xrdap::types::SymbolTable;

use super::CliResult;

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "连接选项")]
pub struct ConnOpts {
    /// 用哪块探针：序列号（全称或末尾几位）、usb、COM35、192.168.0.108、xrdap-93ef1c.local。
    /// 不写时自动选唯一的那块；xrdap list 列出所有探针和对应的写法
    #[arg(short = 'p', long, global = true, value_name = "探针")]
    pub probe: Option<String>,

    /// 串口波特率，要和探针固件一致。不写时按串口芯片自动选：CH343 等先试 4000000，
    /// CP2102、CH340 用 921600（传统 ESP32），其他芯片两个都试
    #[arg(long, global = true, value_name = "波特率")]
    pub baud: Option<u32>,

    /// 打开串口后不改 DTR/RTS（默认置为 DTR=1、RTS=0）。乐鑫芯片自带的 USB 口自动这样处理
    #[arg(long, global = true)]
    pub keep_lines: bool,

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

    /// 使用内置的模拟探针，不需要硬件（试用和测试）。配合 --elf 时按 ELF 填模拟内存
    #[arg(long, global = true)]
    pub fake: bool,
}

fn parse_hz(s: &str) -> Result<u32, String> {
    dap::parse_clock(s).map_err(|e| e.to_string())
}

pub fn load_symbols(conn: &ConnOpts) -> CliResult<SymbolTable> {
    match &conn.elf {
        Some(path) => Ok(xrdap::elf::load(path)?),
        None => Ok(SymbolTable::new()),
    }
}

/// 按连接选项打开探针（不连接目标）。
pub fn open_probe(conn: &ConnOpts, syms: &SymbolTable, err: &mut dyn Write) -> CliResult<Probe> {
    if conn.fake {
        if conn.probe.is_some() {
            return Err("--fake 和 --probe 只能写一个".into());
        }
        let fake = FakeProbe::new();
        if let Some(path) = &conn.elf {
            fake.load_elf_image(&std::fs::read(path)?)?;
            fake.apply_g431_fixture(syms);
        }
        return Ok(Probe::new(
            Box::new(fake),
            Duration::from_millis(conn.timeout_ms),
        ));
    }
    let opts = OpenOptions {
        timeout: Duration::from_millis(conn.timeout_ms),
        baud: conn.baud,
        ..OpenOptions::default()
    };
    match open::parse(conn.probe.as_deref()) {
        Spec::Addr(Addr::Serial { port, keep_lines }) => {
            if !keep_lines && !conn.keep_lines {
                writeln!(err, "打开串口 {port}，等探针应答……")?;
            }
            let addr = Addr::Serial {
                port,
                keep_lines: keep_lines || conn.keep_lines,
            };
            Ok(open::open_addr(&addr, &opts)?)
        }
        Spec::Addr(addr) => Ok(open::open_addr(&addr, &opts)?),
        // 自动选择或按序列号找：先找，再按找到的连接打开
        Spec::Auto | Spec::Serial(_) => Ok(open::open(conn.probe.as_deref(), &opts)?),
    }
}

pub fn connect_target(probe: &mut Probe, conn: &ConnOpts) -> CliResult<dap::TargetInfo> {
    probe
        .connect_swd(conn.clock)
        .map_err(|e| format!("连接目标失败：{e}").into())
}

pub fn print_notes(notes: &mut Vec<String>, out: &mut dyn Write) -> CliResult {
    for n in notes.drain(..) {
        writeln!(out, "提示：{n}")?;
    }
    Ok(())
}
