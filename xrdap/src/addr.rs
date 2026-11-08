//! 探针说明：用一个字符串说明是哪一块探针、怎么连。
//!
//! | 写法 | 含义 |
//! |---|---|
//! | 空 | 自动：只有一块探针时用它 |
//! | `usb`、`usb:<序列号>` | USB（CMSIS-DAP v2） |
//! | `COM3`、`/dev/ttyUSB0`、`serial:<串口>` | 串口 |
//! | `192.0.2.10`、`192.0.2.10:3241`、`xrdap-aabbcc.local`、`tcp:<主机>` | TCP |
//! | `XRobot-XRDAP-AABBCCDDEEFF`、`DDEEFF` | 按序列号（全称或末尾几位）找 |
//!
//! Probe spec: one string telling which probe and how to reach it — empty for automatic,
//! `usb[:<serial>]`, a serial port, a TCP host, or a serial number (or its tail).

use std::fmt;

/// 连到一块探针的方式。
///
/// How to reach one probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addr {
    /// USB；`serial` 为 None 时要求只插着一块。
    ///
    /// USB; with `serial` None exactly one probe must be plugged in.
    Usb {
        /// 探针序列号，None 表示不指定。
        ///
        /// Probe serial number; None leaves it unspecified.
        serial: Option<String>,
    },
    /// 串口。
    ///
    /// Serial port.
    Serial {
        /// 串口名，例如 `COM3` 或 `/dev/ttyUSB0`。
        ///
        /// Serial port name, e.g. `COM3` or `/dev/ttyUSB0`.
        port: String,
    },
    /// TCP；端口缺省时为 3241。
    ///
    /// TCP; the port defaults to 3241.
    Tcp {
        /// `主机[:端口]`，例如 `192.0.2.10:3241`。
        ///
        /// `host[:port]`, e.g. `192.0.2.10:3241`.
        host: String,
    },
}

impl Addr {
    /// 这个连接的探针说明，[`Spec::parse`] 能原样解析回来。
    ///
    /// The probe spec of this address; [`Spec::parse`] parses it back unchanged.
    pub fn spec(&self) -> String {
        match self {
            Addr::Usb { serial: Some(s) } => format!("usb:{s}"),
            Addr::Usb { serial: None } => "usb".into(),
            Addr::Serial { port } if looks_like_serial_port(port) => port.clone(),
            Addr::Serial { port } => format!("serial:{port}"),
            Addr::Tcp { host } if looks_like_host(host) => host.clone(),
            Addr::Tcp { host } => format!("tcp:{host}"),
        }
    }
}

impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Addr::Usb { serial: Some(s) } => write!(f, "USB {s}"),
            Addr::Usb { serial: None } => f.write_str("USB"),
            Addr::Serial { port } => write!(f, "串口 {port}"),
            Addr::Tcp { host } => write!(f, "TCP {host}"),
        }
    }
}

/// 解析好的探针说明。
///
/// A parsed probe spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    /// 空：自动选唯一的探针
    ///
    /// Empty: pick the only probe
    Auto,
    /// 直接给出了连接方式
    ///
    /// The address is given directly
    Addr(Addr),
    /// 按序列号（全称或末尾几位，不分大小写）查找
    ///
    /// Look up by serial number (full or its tail, case-insensitive)
    Serial(String),
}

impl Spec {
    /// 解析探针说明，写法见模块文档。
    ///
    /// Parse a probe spec; see the module documentation for the forms.
    pub fn parse(text: &str) -> Spec {
        let s = text.trim();
        if s.is_empty() {
            return Spec::Auto;
        }
        if s.eq_ignore_ascii_case("usb") {
            return Spec::Addr(Addr::Usb { serial: None });
        }
        if let Some(sn) = strip_prefix_ci(s, "usb:") {
            return Spec::Addr(Addr::Usb {
                serial: Some(sn.to_string()),
            });
        }
        let serial = |port: &str| {
            Spec::Addr(Addr::Serial {
                port: port.to_string(),
            })
        };
        let tcp = |host: &str| {
            Spec::Addr(Addr::Tcp {
                host: host.to_string(),
            })
        };
        if let Some(port) = strip_prefix_ci(s, "serial:") {
            return serial(port);
        }
        if let Some(host) = strip_prefix_ci(s, "tcp:") {
            return tcp(host);
        }
        if looks_like_serial_port(s) {
            return serial(s);
        }
        if looks_like_host(s) {
            return tcp(s);
        }
        Spec::Serial(s.to_string())
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() > prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

fn looks_like_serial_port(s: &str) -> bool {
    let upper = s.to_ascii_uppercase();
    (upper.starts_with("COM") && s.len() > 3 && upper[3..].chars().all(|c| c.is_ascii_digit()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp(h: &str) -> Spec {
        Spec::Addr(Addr::Tcp { host: h.into() })
    }

    fn serial(p: &str) -> Spec {
        Spec::Addr(Addr::Serial { port: p.into() })
    }

    #[test]
    fn parse_specs() {
        assert_eq!(Spec::parse(""), Spec::Auto);
        assert_eq!(Spec::parse("  "), Spec::Auto);
        assert_eq!(Spec::parse("USB"), Spec::Addr(Addr::Usb { serial: None }));
        assert_eq!(
            Spec::parse("usb:XRobot-XRDAP-1"),
            Spec::Addr(Addr::Usb {
                serial: Some("XRobot-XRDAP-1".into())
            })
        );
        assert_eq!(Spec::parse("COM3"), serial("COM3"));
        assert_eq!(Spec::parse("com7"), serial("com7"));
        assert_eq!(Spec::parse(r"\\.\COM12"), serial(r"\\.\COM12"));
        assert_eq!(Spec::parse("/dev/ttyACM0"), serial("/dev/ttyACM0"));
        assert_eq!(Spec::parse("serial:COMX"), serial("COMX"));
        assert_eq!(Spec::parse("192.0.2.10"), tcp("192.0.2.10"));
        assert_eq!(Spec::parse("192.0.2.10:3241"), tcp("192.0.2.10:3241"));
        assert_eq!(Spec::parse("xrdap-aabbcc.local"), tcp("xrdap-aabbcc.local"));
        assert_eq!(Spec::parse("tcp:probe-host"), tcp("probe-host"));
        assert_eq!(Spec::parse("DDEEFF"), Spec::Serial("DDEEFF".into()));
        assert_eq!(
            Spec::parse("XRobot-XRDAP-AABBCCDDEEFF"),
            Spec::Serial("XRobot-XRDAP-AABBCCDDEEFF".into())
        );
    }

    #[test]
    fn spec_round_trips() {
        for a in [
            Addr::Usb { serial: None },
            Addr::Usb {
                serial: Some("XRobot-XRDAP-1".into()),
            },
            Addr::Serial {
                port: "COM3".into(),
            },
            Addr::Serial {
                port: "ttyS-odd".into(),
            },
            Addr::Tcp {
                host: "192.0.2.10:3241".into(),
            },
            Addr::Tcp {
                host: "probe-host".into(),
            },
        ] {
            assert_eq!(Spec::parse(&a.spec()), Spec::Addr(a));
        }
    }
}
