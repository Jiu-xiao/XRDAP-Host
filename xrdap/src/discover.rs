//! 找探针：USB（VID:PID 1D50:6199）、串口、局域网（mDNS）。
//!
//! - USB：列出 VID:PID 为 1D50:6199 的设备，序列号来自 USB 描述符。
//! - 串口：只看常见 USB 转串口芯片（WCH CH34x、Silicon Labs CP210x、FTDI）和乐鑫芯片自带的
//!   USB Serial/JTAG，打开后发 DAP_Info，按时回答的就是探针，同时读出序列号。别的串口不打开。
//!   S3 板上的 USB 转串口打开时可能让探针复位一次，所以第一次找串口最长要等 `serial_wait`。
//! - 局域网：浏览 mDNS 服务 `_xrdap._tcp`，TXT 里带序列号。
//!
//! 同一块探针可能同时经 USB、串口、Wi-Fi 被找到，序列号相同；[`group`] 把它们合成一项。
//!
//! ```no_run
//! use xrdap::discover::{find, group, table, Options};
//! let probes = group(find(&Options::default()));
//! print!("{}", table(&probes));
//! ```

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::dap::Probe;
use crate::open::Addr;
use crate::transport::serial::{SerialTransport, DEFAULT_BAUD, SAFE_BAUD};
use crate::Result;

/// 找到的一个连接：一块探针经某种方式可达。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// 探针序列号，例如 XRobot-XRDAP-E8069093EF1C
    pub serial: Option<String>,
    /// 产品名，例如 XR-DAP-S3
    pub product: Option<String>,
    pub addr: Addr,
    /// 补充说明，例如 mDNS 主机名
    pub note: Option<String>,
}

/// 一块探针和它所有可达的连接。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeEntry {
    pub serial: Option<String>,
    pub product: Option<String>,
    /// 按 USB、串口、TCP 排好序
    pub links: Vec<Found>,
}

impl ProbeEntry {
    /// 推荐用的连接：USB 优先，其次串口，再次 TCP。
    pub fn best(&self) -> &Addr {
        &self.links[0].addr
    }

    /// 序列号是否和 `text` 相同，或以它结尾（不分大小写）。
    pub fn matches(&self, text: &str) -> bool {
        let Some(sn) = &self.serial else {
            return false;
        };
        let (sn, t) = (sn.to_ascii_uppercase(), text.trim().to_ascii_uppercase());
        !t.is_empty() && (sn == t || sn.ends_with(&t))
    }
}

/// 找哪些连接、找多久。
#[derive(Debug, Clone)]
pub struct Options {
    pub usb: bool,
    pub serial: bool,
    pub mdns: bool,
    /// 串口应答、mDNS 浏览的等待时间
    pub time: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            usb: true,
            serial: true,
            mdns: true,
            time: Duration::from_millis(1500),
        }
    }
}

/// USB 上的探针。
pub fn usb() -> Result<Vec<Found>> {
    Ok(crate::transport::usb::list()?
        .into_iter()
        .map(|p| Found {
            addr: Addr::Usb {
                serial: p.serial.clone(),
            },
            serial: p.serial,
            product: p.product,
            note: None,
        })
        .collect())
}

const VID_WCH: u16 = 0x1A86;
const VID_SILABS: u16 = 0x10C4;
const VID_FTDI: u16 = 0x0403;
const VID_ESPRESSIF: u16 = 0x303A;

/// WCH 里最高只到 2 Mbaud 的型号：CH340、CH341
const PID_WCH_SLOW: [u16; 2] = [0x7523, 0x5523];

/// 一个可能接着探针的串口，以及按它的 USB 转串口芯片该怎么打开。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialPortProfile {
    pub port: String,
    /// 依次尝试的波特率。S3 板上的 CH343 跑 4 Mbaud；CP2102、CH340 到不了 4 Mbaud，
    /// 只试传统 ESP32 固件用的 921600
    pub bauds: &'static [u32],
}

const BAUDS_FAST: &[u32] = &[DEFAULT_BAUD, SAFE_BAUD];
const BAUDS_SAFE: &[u32] = &[SAFE_BAUD];
/// USB Serial/JTAG 不看波特率
const BAUDS_ANY: &[u32] = &[DEFAULT_BAUD];

/// 可能接着探针的串口：常见 USB 转串口芯片和乐鑫 USB Serial/JTAG。
pub fn serial_candidates() -> Vec<SerialPortProfile> {
    let Ok(ports) = serialport::available_ports() else {
        return Vec::new();
    };
    ports
        .into_iter()
        .filter_map(|p| {
            let serialport::SerialPortType::UsbPort(u) = p.port_type else {
                return None;
            };
            let bauds = match u.vid {
                VID_ESPRESSIF => BAUDS_ANY,
                VID_SILABS => BAUDS_SAFE,
                VID_WCH if PID_WCH_SLOW.contains(&u.pid) => BAUDS_SAFE,
                VID_WCH | VID_FTDI => BAUDS_FAST,
                _ => return None,
            };
            Some(SerialPortProfile {
                port: p.port_name,
                bauds,
            })
        })
        .collect()
}

/// 一个串口按芯片该怎么打开；不是已知的 USB 转串口芯片时先试 4 Mbaud 再试 921600。
pub fn serial_profile(port: &str) -> SerialPortProfile {
    serial_candidates()
        .into_iter()
        .find(|p| p.port.eq_ignore_ascii_case(port))
        .unwrap_or(SerialPortProfile {
            port: port.to_string(),
            bauds: BAUDS_FAST,
        })
}

/// 问一个串口是不是探针：按 `bauds` 依次试，每个波特率在 `wait` 内回答 DAP_Info 就是。
pub fn probe_serial(profile: &SerialPortProfile, wait: Duration) -> Option<Found> {
    let mut probe = profile.bauds.iter().find_map(|&baud| {
        let t = SerialTransport::open(&profile.port, baud).ok()?;
        let mut probe = Probe::new(Box::new(t), wait);
        probe.wait_ready(wait).ok()?;
        Some(probe)
    })?;
    let id = probe.identity().unwrap_or_default();
    Some(Found {
        serial: id.serial,
        product: id.product,
        addr: Addr::Serial {
            port: profile.port.clone(),
        },
        note: None,
    })
}

/// 串口上的探针。各个候选口同时问，每个口每种波特率约 `wait`。
pub fn serial(wait: Duration) -> Vec<Found> {
    let handles: Vec<_> = serial_candidates()
        .into_iter()
        .map(|p| std::thread::spawn(move || probe_serial(&p, wait)))
        .collect();
    handles
        .into_iter()
        .filter_map(|h| h.join().ok().flatten())
        .collect()
}

/// mDNS 服务类型。
pub const MDNS_SERVICE: &str = "_xrdap._tcp.local.";

/// 局域网里经 mDNS 广播的探针，浏览 `time` 这么久。
pub fn mdns(time: Duration) -> Result<Vec<Found>> {
    let err = |e: mdns_sd::Error| crate::Error::Io(format!("mDNS：{e}"));
    let daemon = mdns_sd::ServiceDaemon::new().map_err(err)?;
    let rx = daemon.browse(MDNS_SERVICE).map_err(err)?;
    let deadline = Instant::now() + time;
    let mut out: Vec<Found> = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = rx.recv_timeout(left) else {
            break;
        };
        let mdns_sd::ServiceEvent::ServiceResolved(info) = event else {
            continue;
        };
        let Some(ip) = info.get_addresses_v4().into_iter().next() else {
            continue;
        };
        let props = &info.txt_properties;
        let found = Found {
            serial: props.get_property_val_str("serial").map(str::to_string),
            product: props
                .get_property_val_str("chip")
                .map(|c| product_from_chip(c).to_string()),
            addr: Addr::Tcp {
                host: format!("{ip}:{}", info.get_port()),
            },
            note: Some(info.get_hostname().trim_end_matches('.').to_string()),
        };
        if !out.contains(&found) {
            out.push(found);
        }
    }
    let _ = daemon.shutdown();
    Ok(out)
}

fn product_from_chip(chip: &str) -> &str {
    match chip {
        "esp32s3" => "XR-DAP-S3",
        "esp32c3" => "XR-DAP-C3",
        other => other,
    }
}

/// 按 `opts` 找全部连接。串口和 mDNS 同时进行，总共约 `opts.time`；出错的那一类跳过。
pub fn find(opts: &Options) -> Vec<Found> {
    let time = opts.time;
    let serial = opts
        .serial
        .then(|| std::thread::spawn(move || serial(time)));
    let mdns = opts.mdns.then(|| std::thread::spawn(move || mdns(time)));
    let mut out = Vec::new();
    if opts.usb {
        out.extend(usb().unwrap_or_default());
    }
    if let Some(h) = serial {
        out.extend(h.join().unwrap_or_default());
    }
    if let Some(h) = mdns {
        out.extend(h.join().ok().and_then(|r| r.ok()).unwrap_or_default());
    }
    out
}

fn link_rank(a: &Addr) -> u8 {
    match a {
        Addr::Usb { .. } => 0,
        Addr::Serial { .. } => 1,
        Addr::Tcp { .. } => 2,
    }
}

/// 把同一序列号的连接合成一块探针；没有序列号的各算一块。按序列号排序。
pub fn group(found: Vec<Found>) -> Vec<ProbeEntry> {
    let mut by_serial: BTreeMap<String, ProbeEntry> = BTreeMap::new();
    let mut anonymous = Vec::new();
    for f in found {
        match f.serial.clone() {
            Some(sn) => {
                let e = by_serial.entry(sn).or_insert_with(|| ProbeEntry {
                    serial: f.serial.clone(),
                    product: None,
                    links: Vec::new(),
                });
                if e.product.is_none() {
                    e.product = f.product.clone();
                }
                e.links.push(f);
            }
            None => anonymous.push(ProbeEntry {
                serial: None,
                product: f.product.clone(),
                links: vec![f],
            }),
        }
    }
    let mut out: Vec<ProbeEntry> = by_serial.into_values().chain(anonymous).collect();
    for e in &mut out {
        e.links.sort_by_key(|f| link_rank(&f.addr));
    }
    out
}

/// 给人看的表：每块探针一行序列号和产品，下面每行一个连接，后面是 `--probe` 的写法。
pub fn table(probes: &[ProbeEntry]) -> String {
    let mut s = String::new();
    for p in probes {
        s.push_str(&format!(
            "{}  {}\n",
            p.serial.as_deref().unwrap_or("（没有序列号）"),
            p.product.as_deref().unwrap_or("")
        ));
        for l in &p.links {
            let note = l
                .note
                .as_deref()
                .map(|n| format!("（{n}）"))
                .unwrap_or_default();
            s.push_str(&format!(
                "    {}{note}    --probe {}\n",
                l.addr,
                spec_of(&l.addr)
            ));
        }
    }
    s
}

/// 连接对应的 `--probe` 写法。
pub fn spec_of(addr: &Addr) -> String {
    match addr {
        Addr::Usb { serial: Some(s) } => format!("usb:{s}"),
        Addr::Usb { serial: None } => "usb".into(),
        Addr::Serial { port, .. } => port.clone(),
        Addr::Tcp { host } => host.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(sn: &str, addr: Addr) -> Found {
        Found {
            serial: Some(sn.into()),
            product: Some("XR-DAP-S3".into()),
            addr,
            note: None,
        }
    }

    #[test]
    fn unknown_serial_port_tries_both_firmware_bauds() {
        let p = serial_profile("XRDAP-NO-SUCH-PORT");
        assert_eq!(p.bauds, &[DEFAULT_BAUD, SAFE_BAUD]);
    }

    #[test]
    fn group_and_match() {
        let tcp = Addr::Tcp {
            host: "10.0.0.2:3241".into(),
        };
        let usb = Addr::Usb {
            serial: Some("XRobot-XRDAP-AABBCC".into()),
        };
        let ser = Addr::Serial {
            port: "COM9".into(),
        };
        let g = group(vec![
            found("XRobot-XRDAP-AABBCC", tcp.clone()),
            found("XRobot-XRDAP-AABBCC", usb.clone()),
            found("XRobot-XRDAP-112233", ser.clone()),
        ]);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].serial.as_deref(), Some("XRobot-XRDAP-112233"));
        assert_eq!(g[1].best(), &usb);
        assert_eq!(g[1].links[1].addr, tcp);
        assert!(g[1].matches("aabbcc"));
        assert!(g[1].matches("XRobot-XRDAP-AABBCC"));
        assert!(!g[1].matches("1122"));
        assert!(!g[1].matches(""));
        let t = table(&g);
        assert!(t.contains("--probe COM9"));
        assert!(t.contains("--probe usb:XRobot-XRDAP-AABBCC"));
    }
}
