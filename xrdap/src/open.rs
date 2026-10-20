//! 打开探针：用一个字符串说明是哪一块、怎么连，或者自动选唯一的那一块。
//!
//! | 写法 | 含义 |
//! |---|---|
//! | 不写 | 自动：只有一块探针时用它（先看 USB，再找串口和局域网） |
//! | `usb`、`usb:<序列号>` | USB（CMSIS-DAP v2） |
//! | `COM35`、`/dev/ttyUSB0`、`serial:<串口>` | 串口 |
//! | `192.168.0.108`、`192.168.0.108:3241`、`xrdap-93ef1c.local`、`tcp:<主机>` | TCP |
//! | `XRobot-XRDAP-E8069093EF1C`、`93EF1C` | 按序列号（全称或末尾几位）找，USB 优先，再串口、TCP |
//!
//! ```no_run
//! use xrdap::open::{open, OpenOptions};
//! # fn main() -> xrdap::Result<()> {
//! let mut probe = open(Some("COM35"), &OpenOptions::default())?;
//! let target = probe.connect_swd(4_000_000)?;
//! println!("{}", target.describe());
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::time::Duration;

use crate::dap::Probe;
use crate::transport::serial::SerialTransport;
use crate::transport::tcp::TcpTransport;
use crate::{Error, Result};

/// 连到一块探针的方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addr {
    /// USB；`serial` 为 None 时用找到的第一块
    Usb { serial: Option<String> },
    /// 串口；`keep_lines` 为 true 时打开后不改 DTR/RTS（芯片自带的 USB Serial/JTAG 要这样）
    Serial { port: String, keep_lines: bool },
    /// TCP，`host` 为 `主机[:端口]`，端口默认 3241
    Tcp { host: String },
}

impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Addr::Usb { serial: Some(s) } => write!(f, "USB {s}"),
            Addr::Usb { serial: None } => f.write_str("USB"),
            Addr::Serial { port, .. } => write!(f, "串口 {port}"),
            Addr::Tcp { host } => write!(f, "TCP {host}"),
        }
    }
}

/// 打开时的参数。
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// 等一个响应的超时
    pub timeout: Duration,
    /// 串口波特率；None 时按串口芯片依次试（见 [`discover::serial_profile`](crate::discover::serial_profile)）
    pub baud: Option<u32>,
    /// 串口打开后等探针应答的最长时间（S3 打开串口时可能复位一次，要等它启动）
    pub serial_wait: Duration,
    /// 自动选择或按序列号查找时，找串口和局域网探针的时间
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

/// 一个探针说明解析出来的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    /// 不写：自动选唯一的探针
    Auto,
    /// 直接给出了连接方式
    Addr(Addr),
    /// 按序列号（全称或末尾几位，不分大小写）查找
    Serial(String),
}

/// 解析探针说明，见模块文档的表。
pub fn parse(spec: Option<&str>) -> Spec {
    let Some(s) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
        return Spec::Auto;
    };
    let lower = s.to_ascii_lowercase();
    if lower == "usb" {
        return Spec::Addr(Addr::Usb { serial: None });
    }
    if let Some(sn) = strip_prefix_ci(s, "usb:") {
        return Spec::Addr(Addr::Usb {
            serial: Some(sn.to_string()),
        });
    }
    if let Some(port) = strip_prefix_ci(s, "serial:") {
        return Spec::Addr(serial_addr(port));
    }
    if let Some(host) = strip_prefix_ci(s, "tcp:") {
        return Spec::Addr(Addr::Tcp {
            host: host.to_string(),
        });
    }
    if looks_like_serial_port(s) {
        return Spec::Addr(serial_addr(s));
    }
    if looks_like_host(s) {
        return Spec::Addr(Addr::Tcp {
            host: s.to_string(),
        });
    }
    Spec::Serial(s.to_string())
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() > prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

fn serial_addr(port: &str) -> Addr {
    Addr::Serial {
        port: port.to_string(),
        keep_lines: false,
    }
}

fn looks_like_serial_port(s: &str) -> bool {
    let upper = s.to_ascii_uppercase();
    (upper.starts_with("COM") && upper[3..].chars().all(|c| c.is_ascii_digit()) && s.len() > 3)
        || s.starts_with("/dev/")
        || s.starts_with(r"\\.\")
}

fn looks_like_host(s: &str) -> bool {
    let host = match s.rsplit_once(':') {
        Some((h, port)) if port.parse::<u16>().is_ok() => h,
        _ => s,
    };
    host.parse::<std::net::IpAddr>().is_ok()
        || host.to_ascii_lowercase().ends_with(".local")
        || host.eq_ignore_ascii_case("localhost")
        || s.starts_with('[')
}

/// 按连接方式打开探针（不连接目标）。
pub fn open_addr(addr: &Addr, opts: &OpenOptions) -> Result<Probe> {
    Ok(match addr {
        #[cfg(feature = "usb")]
        Addr::Usb { serial } => Probe::new(Box::new(crate::transport::usb::UsbTransport::open(
            serial.as_deref(),
            opts.timeout,
        )?)),
        #[cfg(not(feature = "usb"))]
        Addr::Usb { .. } => return Err(Error::usage("这个版本没有编译 USB 支持")),
        Addr::Serial { port, keep_lines } => {
            let (keep, bauds) = serial_profile(port);
            open_serial(
                port,
                *keep_lines || keep,
                &opts.baud.map_or(bauds, |b| vec![b]),
                opts,
            )?
        }
        Addr::Tcp { host } => Probe::new(Box::new(TcpTransport::connect(host, opts.timeout)?)),
    })
}

/// 换一个波特率重试前，前一个波特率等探针应答的时间
const FALLBACK_WAIT: Duration = Duration::from_secs(2);

/// 按 `bauds` 依次打开串口、等探针应答；最后一个波特率等满 `serial_wait`。
fn open_serial(port: &str, keep: bool, bauds: &[u32], opts: &OpenOptions) -> Result<Probe> {
    let mut last = Error::usage(format!("串口 {port} 没有可试的波特率"));
    for (i, &baud) in bauds.iter().enumerate() {
        let wait = if i + 1 == bauds.len() {
            opts.serial_wait
        } else {
            FALLBACK_WAIT
        };
        match SerialTransport::open(port, baud, keep, opts.timeout).and_then(|mut t| {
            t.handshake(wait)?;
            Ok(t)
        }) {
            Ok(t) => return Ok(Probe::new(Box::new(t))),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// 串口芯片决定的 (keep_lines, 依次尝试的波特率)。
#[cfg(feature = "discover")]
fn serial_profile(port: &str) -> (bool, Vec<u32>) {
    let p = crate::discover::serial_profile(port);
    (p.keep_lines, p.bauds.to_vec())
}

#[cfg(not(feature = "discover"))]
fn serial_profile(_port: &str) -> (bool, Vec<u32>) {
    (false, vec![crate::transport::serial::DEFAULT_BAUD])
}

/// 按说明打开探针（不连接目标）。`spec` 的写法见模块文档；None 表示自动选择。
pub fn open(spec: Option<&str>, opts: &OpenOptions) -> Result<Probe> {
    match parse(spec) {
        Spec::Addr(addr) => open_addr(&addr, opts),
        Spec::Auto => open_addr(&auto_addr(opts)?, opts),
        Spec::Serial(sn) => open_addr(&addr_by_serial(&sn, opts)?, opts),
    }
}

#[cfg(feature = "discover")]
fn auto_addr(opts: &OpenOptions) -> Result<Addr> {
    use crate::discover;
    // 只有一块 USB 探针时直接用，不去碰串口
    let usb = discover::usb().unwrap_or_default();
    if usb.len() == 1 {
        return Ok(usb[0].addr.clone());
    }
    let probes = discover::group(discover::find(&discover::Options {
        time: opts.discover_time,
        ..Default::default()
    }));
    match probes.as_slice() {
        [one] => Ok(one.best().clone()),
        [] => Err(Error::Io("没有找到探针（USB、串口、局域网都没有）".into())),
        many => Err(Error::usage(format!(
            "找到 {} 块探针，用 --probe 指定一块：\n{}",
            many.len(),
            discover::table(many)
        ))),
    }
}

#[cfg(not(feature = "discover"))]
fn auto_addr(_opts: &OpenOptions) -> Result<Addr> {
    Ok(Addr::Usb { serial: None })
}

#[cfg(feature = "discover")]
fn addr_by_serial(sn: &str, opts: &OpenOptions) -> Result<Addr> {
    use crate::discover;
    let probes = discover::group(discover::find(&discover::Options {
        time: opts.discover_time,
        ..Default::default()
    }));
    let hits: Vec<_> = probes.iter().filter(|p| p.matches(sn)).collect();
    match hits.as_slice() {
        [one] => Ok(one.best().clone()),
        [] => Err(Error::Io(format!("没有找到序列号为 {sn} 的探针"))),
        _ => Err(Error::usage(format!("{sn} 对应不止一块探针，写全序列号"))),
    }
}

#[cfg(not(feature = "discover"))]
fn addr_by_serial(sn: &str, _opts: &OpenOptions) -> Result<Addr> {
    Ok(Addr::Usb {
        serial: Some(sn.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp(h: &str) -> Spec {
        Spec::Addr(Addr::Tcp { host: h.into() })
    }

    #[test]
    fn parse_specs() {
        assert_eq!(parse(None), Spec::Auto);
        assert_eq!(parse(Some("  ")), Spec::Auto);
        assert_eq!(parse(Some("USB")), Spec::Addr(Addr::Usb { serial: None }));
        assert_eq!(
            parse(Some("usb:XRobot-XRDAP-1")),
            Spec::Addr(Addr::Usb {
                serial: Some("XRobot-XRDAP-1".into())
            })
        );
        assert_eq!(parse(Some("COM35")), Spec::Addr(serial_addr("COM35")));
        assert_eq!(parse(Some("com7")), Spec::Addr(serial_addr("com7")));
        assert_eq!(
            parse(Some("/dev/ttyACM0")),
            Spec::Addr(serial_addr("/dev/ttyACM0"))
        );
        assert_eq!(parse(Some("serial:COMX")), Spec::Addr(serial_addr("COMX")));
        assert_eq!(parse(Some("192.168.0.108")), tcp("192.168.0.108"));
        assert_eq!(parse(Some("192.168.0.108:3241")), tcp("192.168.0.108:3241"));
        assert_eq!(parse(Some("xrdap-93ef1c.local")), tcp("xrdap-93ef1c.local"));
        assert_eq!(parse(Some("tcp:probe-host")), tcp("probe-host"));
        assert_eq!(parse(Some("93EF1C")), Spec::Serial("93EF1C".into()));
        assert_eq!(
            parse(Some("XRobot-XRDAP-E8069093EF1C")),
            Spec::Serial("XRobot-XRDAP-E8069093EF1C".into())
        );
    }
}
