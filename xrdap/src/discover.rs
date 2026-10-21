//! 找探针：USB（VID:PID 1D50:6199）、串口、局域网（mDNS）。
//!
//! - USB：列出 VID:PID 为 1D50:6199 的设备，序列号来自 USB 描述符。
//! - 串口：只看系统列出的端口和它们的 USB 描述符，默认**不打开**任何串口——打开串口可能复位
//!   接在上面的别的开发板。乐鑫芯片自带的 USB Serial/JTAG（VID:PID 303A:1001）按描述符认作
//!   探针：描述符里的序列号是芯片的 MAC，探针序列号由它得出。别的串口只有
//!   [`Options::ask_serial`] 打开时才逐个打开、发 DAP_Info 确认。
//! - 局域网：浏览 mDNS 服务 `_xrdap._tcp`，TXT 里带序列号。
//!
//! 同一块探针可能同时经 USB、串口、Wi-Fi 被找到，序列号相同；[`group`] 把它们合成一项。
//!
//! Find probes on USB (1D50:6199), serial ports and the LAN (mDNS). Serial ports are only
//! listed with their USB descriptors and are not opened by default, since opening a port can
//! reset another board on it. Espressif's built-in USB Serial/JTAG (303A:1001) is recognised
//! from its descriptor, whose serial number is the chip MAC. Other ports are opened and asked
//! for DAP_Info only with [`Options::ask_serial`].
//!
//! ```no_run
//! use xrdap::discover::{find, group, Options};
//! for p in group(find(&Options::default())) {
//!     println!("{:?} {:?}", p.serial, p.best());
//! }
//! ```

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::addr::Addr;
use crate::dap::Probe;
use crate::raw::transport::serial::{SerialTransport, DEFAULT_BAUD, SAFE_BAUD};
use crate::raw::transport::usb;
use crate::Result;

/// 探针序列号的前缀，后面是芯片唯一 ID 的大写十六进制（协议文档 4.1 节）。
///
/// Prefix of probe serial numbers, followed by the chip's unique ID in upper-case hex.
pub(crate) const SERIAL_PREFIX: &str = "XRobot-XRDAP-";

/// 找到的一个连接：一块探针经某种方式可达。
///
/// One way to reach a probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// 探针序列号，例如 `XRobot-XRDAP-AABBCCDDEEFF`
    ///
    /// Probe serial number
    pub serial: Option<String>,
    /// 产品名，例如 `XR-DAP-S3`
    ///
    /// Product name
    pub product: Option<String>,
    /// 连接方式。
    ///
    /// How to reach the probe.
    pub addr: Addr,
    /// 补充说明，例如 mDNS 主机名
    ///
    /// Extra note, e.g. the mDNS host name
    pub note: Option<String>,
}

/// 一块探针和它所有可达的连接。
///
/// One probe and every way to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeEntry {
    /// 探针序列号。
    ///
    /// Probe serial number.
    pub serial: Option<String>,
    /// 产品名。
    ///
    /// Product name.
    pub product: Option<String>,
    /// 按 USB、串口、TCP 排好序
    ///
    /// Sorted USB, serial, TCP
    pub links: Vec<Found>,
}

impl ProbeEntry {
    /// 推荐用的连接：USB 优先，其次串口，再次 TCP。
    ///
    /// The preferred address: USB, then serial, then TCP.
    pub fn best(&self) -> &Addr {
        &self.links[0].addr
    }

    /// 序列号是否和 `text` 相同，或以它结尾（不分大小写）。
    ///
    /// Whether the serial number equals `text` or ends with it (case-insensitive).
    pub fn matches(&self, text: &str) -> bool {
        let Some(sn) = &self.serial else {
            return false;
        };
        let (sn, t) = (sn.to_ascii_uppercase(), text.trim().to_ascii_uppercase());
        !t.is_empty() && (sn == t || sn.ends_with(&t))
    }
}

/// 找哪些连接、找多久。
///
/// What to search and for how long.
#[derive(Debug, Clone)]
pub struct Options {
    /// 是否找 USB 上的探针。
    ///
    /// Whether to look for probes on USB.
    pub usb: bool,
    /// 是否浏览局域网里的 mDNS 广播。
    ///
    /// Whether to browse mDNS advertisements on the LAN.
    pub mdns: bool,
    /// 逐个打开可能接着探针的串口，发 DAP_Info 确认（会让打开时就复位的开发板复位）
    ///
    /// Open each candidate serial port and ask DAP_Info (resets boards that reset on open)
    pub ask_serial: bool,
    /// mDNS 浏览、串口应答的等待时间
    ///
    /// Time to browse mDNS and to wait for serial answers
    pub time: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            usb: true,
            mdns: true,
            ask_serial: false,
            time: Duration::from_millis(1500),
        }
    }
}

/// USB 上的探针。
///
/// Probes on USB.
pub fn usb() -> Result<Vec<Found>> {
    Ok(usb::list()?
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

/// 串口的 USB 转串口芯片，按 USB VID/PID 判断。
///
/// The USB-serial chip behind a port, judged from its USB VID/PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    /// 乐鑫芯片自带的 USB Serial/JTAG（303A:1001），不看波特率
    ///
    /// Espressif built-in USB Serial/JTAG; the baud rate has no effect
    EspressifUsb,
    /// WCH CH343 等，能跑 4 Mbaud
    ///
    /// WCH CH343 and similar, 4 Mbaud capable
    WchFast,
    /// WCH CH340、CH341，最高 2 Mbaud
    ///
    /// WCH CH340/CH341, at most 2 Mbaud
    WchSlow,
    /// Silicon Labs CP210x。
    ///
    /// Silicon Labs CP210x.
    SiliconLabs,
    /// FTDI。
    ///
    /// FTDI.
    Ftdi,
    /// 不认识或不是 USB 串口
    ///
    /// Unknown, or not a USB serial port
    Other,
}

impl Chip {
    fn from_usb(vid: u16, pid: u16) -> Chip {
        match (vid, pid) {
            (0x303A, 0x1001) => Chip::EspressifUsb,
            (0x1A86, 0x7523 | 0x5523) => Chip::WchSlow,
            (0x1A86, _) => Chip::WchFast,
            (0x10C4, _) => Chip::SiliconLabs,
            (0x0403, _) => Chip::Ftdi,
            _ => Chip::Other,
        }
    }

    /// 依次尝试的波特率。CH343 跑探针固件的 4 Mbaud；CP2102、CH340 到不了，只试传统 ESP32
    /// 固件的 921600；不认识的芯片两个都试。
    ///
    /// Baud rates to try, in order: 4 Mbaud firmware on capable chips, 921600 (classic ESP32
    /// firmware) on slow ones, both on unknown chips.
    pub(crate) fn bauds(self) -> &'static [u32] {
        match self {
            Chip::EspressifUsb => &[DEFAULT_BAUD],
            Chip::WchSlow | Chip::SiliconLabs => &[SAFE_BAUD],
            Chip::WchFast | Chip::Ftdi | Chip::Other => &[DEFAULT_BAUD, SAFE_BAUD],
        }
    }

    /// 芯片名，给人看。
    ///
    /// Chip name for people.
    pub fn name(self) -> &'static str {
        match self {
            Chip::EspressifUsb => "乐鑫 USB Serial/JTAG",
            Chip::WchFast => "WCH CH34x",
            Chip::WchSlow => "WCH CH340/CH341",
            Chip::SiliconLabs => "Silicon Labs CP210x",
            Chip::Ftdi => "FTDI",
            Chip::Other => "其他",
        }
    }
}

/// 系统列出的一个串口和它的 USB 描述符。
///
/// One serial port as listed by the system, with its USB descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialPortInfo {
    /// 串口名，例如 `COM3`。
    ///
    /// Serial port name, e.g. `COM3`.
    pub port: String,
    /// 串口背后的 USB 转串口芯片。
    ///
    /// The USB-serial chip behind the port.
    pub chip: Chip,
    /// USB 描述符里的序列号
    ///
    /// Serial number from the USB descriptor
    pub usb_serial: Option<String>,
}

impl SerialPortInfo {
    /// 乐鑫 USB Serial/JTAG 上的探针序列号：描述符里的 MAC（`AA:BB:CC:DD:EE:FF`）去掉分隔符、
    /// 转大写，加上 [`SERIAL_PREFIX`]。别的芯片返回 None（要打开串口问才知道）。
    ///
    /// The probe serial number behind an Espressif USB Serial/JTAG port, derived from the MAC in
    /// its descriptor; None for other chips (the port must be opened to know).
    pub(crate) fn probe_serial(&self) -> Option<String> {
        if self.chip != Chip::EspressifUsb {
            return None;
        }
        let hex: String = self
            .usb_serial
            .as_deref()?
            .chars()
            .filter(char::is_ascii_hexdigit)
            .collect();
        (hex.len() == 12).then(|| format!("{SERIAL_PREFIX}{}", hex.to_ascii_uppercase()))
    }
}

/// 可能接着探针的串口：认识芯片的 USB 串口。只读系统的端口列表，不打开。
///
/// Serial ports that may lead to a probe: USB ports with a known chip. Reads the system's
/// port list only; nothing is opened.
pub fn serial_ports() -> Vec<SerialPortInfo> {
    let Ok(ports) = serialport::available_ports() else {
        return Vec::new();
    };
    let mut out: Vec<SerialPortInfo> = ports
        .into_iter()
        .filter_map(|p| {
            let serialport::SerialPortType::UsbPort(u) = p.port_type else {
                return None;
            };
            let chip = Chip::from_usb(u.vid, u.pid);
            (chip != Chip::Other).then_some(SerialPortInfo {
                port: p.port_name,
                chip,
                usb_serial: u.serial_number,
            })
        })
        .collect();
    out.sort_by(|a, b| a.port.cmp(&b.port));
    out
}

/// 一个串口的信息；不在 [`serial_ports`] 里时芯片记为 [`Chip::Other`]。
///
/// Information on one port; [`Chip::Other`] when it is not in [`serial_ports`].
pub(crate) fn serial_port(port: &str) -> SerialPortInfo {
    serial_ports()
        .into_iter()
        .find(|p| p.port.eq_ignore_ascii_case(port))
        .unwrap_or(SerialPortInfo {
            port: port.to_string(),
            chip: Chip::Other,
            usb_serial: None,
        })
}

/// 打开一个串口问它是不是探针：按芯片的波特率依次试，每个波特率在 `wait` 内回答 DAP_Info
/// 就是。会打开串口。
///
/// Open a port and ask whether a probe is there: try the chip's baud rates in turn; a DAP_Info
/// answer within `wait` means yes. This opens the port.
pub fn ask_serial(info: &SerialPortInfo, wait: Duration) -> Option<Found> {
    let mut probe = info.chip.bauds().iter().find_map(|&baud| {
        let t = SerialTransport::open(&info.port, baud).ok()?;
        let mut probe = Probe::new(Box::new(t), wait);
        probe.wait_ready(wait).ok()?;
        Some(probe)
    })?;
    let id = probe.identity().unwrap_or_default();
    Some(Found {
        serial: id.serial,
        product: id.product,
        addr: Addr::Serial {
            port: info.port.clone(),
        },
        note: None,
    })
}

type Ask = fn(&SerialPortInfo, Duration) -> Option<Found>;

/// 按描述符认出的串口探针（乐鑫 USB Serial/JTAG），不打开串口。
fn serial_by_descriptor(ports: &[SerialPortInfo]) -> Vec<Found> {
    ports
        .iter()
        .filter_map(|p| {
            Some(Found {
                serial: Some(p.probe_serial()?),
                product: None,
                addr: Addr::Serial {
                    port: p.port.clone(),
                },
                note: Some(format!("{}，按 USB 描述符识别", p.chip.name())),
            })
        })
        .collect()
}

/// mDNS 服务类型。
///
/// The mDNS service type.
pub(crate) const MDNS_SERVICE: &str = "_xrdap._tcp.local.";

/// 局域网里经 mDNS 广播的探针，浏览 `time` 这么久。
///
/// Probes advertised over mDNS on the LAN, browsing for `time`.
pub(crate) fn mdns(time: Duration) -> Result<Vec<Found>> {
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

/// 按 `opts` 找全部连接。mDNS 浏览和串口询问同时进行，总共约 `opts.time`；出错的那一类跳过。
///
/// Find every connection per `opts`. mDNS browsing and serial asking run in parallel, about
/// `opts.time` in total; a category that fails is skipped.
pub fn find(opts: &Options) -> Vec<Found> {
    collect(opts, &serial_ports(), ask_serial)
}

/// [`find`] 的主体；`ask` 是打开串口询问的那一步。
fn collect(opts: &Options, ports: &[SerialPortInfo], ask: Ask) -> Vec<Found> {
    let time = opts.time;
    let asked: Vec<_> = ports
        .iter()
        .filter(|p| opts.ask_serial && p.chip != Chip::EspressifUsb)
        .cloned()
        .map(|p| std::thread::spawn(move || ask(&p, time)))
        .collect();
    let mdns = opts.mdns.then(|| std::thread::spawn(move || mdns(time)));
    let mut out = Vec::new();
    if opts.usb {
        out.extend(usb().unwrap_or_default());
    }
    out.extend(serial_by_descriptor(ports));
    out.extend(asked.into_iter().filter_map(|h| h.join().ok().flatten()));
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
///
/// Merge connections with the same serial number into one probe; each one without a serial
/// number is a probe of its own. Sorted by serial number.
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
        let p = serial_port("XRDAP-NO-SUCH-PORT");
        assert_eq!(p.chip, Chip::Other);
        assert_eq!(p.chip.bauds(), &[DEFAULT_BAUD, SAFE_BAUD]);
    }

    /// 乐鑫自带 USB 口按描述符认，探针序列号是 MAC 的大写十六进制
    /// （协议文档 4.1 节）。
    #[test]
    fn espressif_usb_port_recognised_by_descriptor() {
        let port = |chip, sn: &str| SerialPortInfo {
            port: "COM9".into(),
            chip,
            usb_serial: Some(sn.into()),
        };
        let esp = port(Chip::EspressifUsb, "aa:bb:cc:dd:ee:ff");
        assert_eq!(
            esp.probe_serial().as_deref(),
            Some("XRobot-XRDAP-AABBCCDDEEFF")
        );
        assert_eq!(Chip::from_usb(0x303A, 0x1001), Chip::EspressifUsb);
        // 别的芯片只看描述符认不出来
        assert_eq!(port(Chip::WchFast, "5A7B012345").probe_serial(), None);
        assert_eq!(port(Chip::EspressifUsb, "not-a-mac").probe_serial(), None);
        let f = serial_by_descriptor(&[esp, port(Chip::SiliconLabs, "0001")]);
        assert_eq!(f.len(), 1);
        assert_eq!(
            f[0].addr,
            Addr::Serial {
                port: "COM9".into()
            }
        );
    }

    /// 默认只列出串口，不打开；要求询问时才打开，乐鑫 USB 口不用问。
    #[test]
    fn serial_ports_opened_only_when_asked() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static OPENED: AtomicUsize = AtomicUsize::new(0);
        fn ask(_: &SerialPortInfo, _: Duration) -> Option<Found> {
            OPENED.fetch_add(1, Ordering::SeqCst);
            None
        }
        let ports = [
            SerialPortInfo {
                port: "COM3".into(),
                chip: Chip::WchFast,
                usb_serial: None,
            },
            SerialPortInfo {
                port: "COM9".into(),
                chip: Chip::EspressifUsb,
                usb_serial: Some("AA:BB:CC:DD:EE:FF".into()),
            },
        ];
        let mut opts = Options {
            usb: false,
            mdns: false,
            ..Options::default()
        };
        let found = collect(&opts, &ports, ask);
        assert_eq!(OPENED.load(Ordering::SeqCst), 0);
        assert_eq!(found.len(), 1, "乐鑫 USB 口按描述符列出");
        opts.ask_serial = true;
        collect(&opts, &ports, ask);
        assert_eq!(OPENED.load(Ordering::SeqCst), 1, "只问 CH34x 那个口");
    }

    #[test]
    fn group_and_match() {
        let tcp = Addr::Tcp {
            host: "192.0.2.10:3241".into(),
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
    }
}
