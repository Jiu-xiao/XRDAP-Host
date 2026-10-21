//! 打开探针：按一个[探针说明](crate::addr)打开，或者自动选唯一的那一块。命令行和图形界面都
//! 经这里打开探针。
//!
//! 自动选择和按序列号查找只看 USB、局域网和按描述符认出的乐鑫 USB 串口；
//! 接在 USB 转串口芯片上的探针要直接写串口名。
//!
//! Open a probe from a [probe spec](crate::addr), or pick the only one. Both the command line
//! and the GUI open probes here. Automatic selection and serial-number lookup consider USB,
//! the LAN and Espressif USB ports recognised by descriptor; a probe behind a USB-serial chip
//! is opened by naming its port.
//!
//! ```no_run
//! use xrdap::addr::Spec;
//! use xrdap::open::{open, OpenOptions};
//! # fn main() -> xrdap::Result<()> {
//! let mut probe = open(&Spec::parse("COM3"), &OpenOptions::default())?;
//! let target = probe.connect_swd(4_000_000)?;
//! println!("{}", target.describe());
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use crate::addr::{Addr, Spec};
use crate::dap::Probe;
use crate::discover::{self, ProbeEntry};
use crate::transport::serial::SerialTransport;
use crate::transport::tcp::TcpTransport;
use crate::transport::usb::UsbTransport;
use crate::{Error, Result};

/// 打开时的参数。
///
/// Options for opening a probe.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// 等一个响应的超时
    ///
    /// Timeout for one response
    pub timeout: Duration,
    /// 串口波特率；None 时按串口芯片依次试（见 [`Chip::bauds`](crate::discover::Chip::bauds)）
    ///
    /// Serial baud rate; None tries the rates of the serial chip in turn
    pub baud: Option<u32>,
    /// 串口打开后等探针应答的最长时间
    ///
    /// How long to wait for the probe to answer after opening a serial port
    pub serial_wait: Duration,
    /// 自动选择或按序列号查找时浏览局域网的时间
    ///
    /// LAN browsing time for automatic selection and serial-number lookup
    pub discover_time: Duration,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            baud: None,
            serial_wait: Duration::from_secs(10),
            discover_time: Duration::from_millis(1500),
        }
    }
}

/// 按说明打开探针（不连接目标）。
///
/// Open a probe as the spec says (without connecting the target).
pub fn open(spec: &Spec, opts: &OpenOptions) -> Result<Probe> {
    match spec {
        Spec::Addr(addr) => open_addr(addr, opts),
        Spec::Auto => open_addr(&auto_addr(opts)?, opts),
        Spec::Serial(sn) => open_addr(&addr_by_serial(sn, opts)?, opts),
    }
}

fn open_addr(addr: &Addr, opts: &OpenOptions) -> Result<Probe> {
    Ok(match addr {
        Addr::Usb { serial } => Probe::new(
            Box::new(UsbTransport::open(serial.as_deref())?),
            opts.timeout,
        ),
        Addr::Serial { port } => {
            let chip_bauds = discover::serial_port(port).chip.bauds();
            let fixed = opts.baud.map(|b| [b]);
            open_serial(port, fixed.as_ref().map_or(chip_bauds, |b| b), opts)?
        }
        Addr::Tcp { host } => Probe::new(
            Box::new(TcpTransport::connect(host, opts.timeout)?),
            opts.timeout,
        ),
    })
}

/// 换一个波特率重试前，前一个波特率等探针应答的时间
const FALLBACK_WAIT: Duration = Duration::from_secs(2);

/// 按 `bauds` 依次打开串口、等探针应答；最后一个波特率等满 `serial_wait`。
fn open_serial(port: &str, bauds: &[u32], opts: &OpenOptions) -> Result<Probe> {
    let mut last = Error::usage(format!("串口 {port} 没有可试的波特率"));
    for (i, &baud) in bauds.iter().enumerate() {
        let wait = if i + 1 == bauds.len() {
            opts.serial_wait
        } else {
            FALLBACK_WAIT
        };
        match SerialTransport::open(port, baud).and_then(|t| {
            let mut probe = Probe::new(Box::new(t), opts.timeout);
            probe.wait_ready(wait)?;
            Ok(probe)
        }) {
            Ok(probe) => return Ok(probe),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// 不打开串口能找到的探针。
fn probes(opts: &OpenOptions) -> Vec<ProbeEntry> {
    discover::group(discover::find(&discover::Options {
        time: opts.discover_time,
        ..Default::default()
    }))
}

/// 自动选择和按序列号查找不打开的那部分串口
const UNCHECKED: &str = "接在 USB 转串口芯片上的探针要直接指定串口";

fn auto_addr(opts: &OpenOptions) -> Result<Addr> {
    // 只有一块 USB 探针时直接用，不用等局域网
    let usb = discover::usb().unwrap_or_default();
    if let [one] = usb.as_slice() {
        return Ok(one.addr.clone());
    }
    match probes(opts).as_slice() {
        [one] => Ok(one.best().clone()),
        [] => Err(Error::Select(format!(
            "没有找到探针（USB、局域网、乐鑫 USB 串口都没有）；{UNCHECKED}"
        ))),
        many => Err(Error::Select(format!(
            "找到 {} 块探针：{}",
            many.len(),
            names(many)
        ))),
    }
}

fn addr_by_serial(sn: &str, opts: &OpenOptions) -> Result<Addr> {
    let probes = probes(opts);
    let hits: Vec<_> = probes.iter().filter(|p| p.matches(sn)).cloned().collect();
    match hits.as_slice() {
        [one] => Ok(one.best().clone()),
        [] => Err(Error::Select(format!(
            "没有找到序列号为 {sn} 的探针；{UNCHECKED}"
        ))),
        many => Err(Error::Select(format!(
            "序列号 {sn} 对应 {} 块探针：{}",
            many.len(),
            names(many)
        ))),
    }
}

fn names(probes: &[ProbeEntry]) -> String {
    probes
        .iter()
        .map(|p| format!("{}（{}）", p.serial.as_deref().unwrap_or("?"), p.best()))
        .collect::<Vec<_>>()
        .join("，")
}
