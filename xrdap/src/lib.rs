//! XR-DAP 探针的上位机库 / Host library for XR-DAP probes.
//!
//! 打开探针、连接 SWD 目标、按变量名读写内存、让探针按固定周期采样、配置探针的 Wi-Fi。
//! USB、串口、TCP 三种连接用法相同。
//!
//! Open a probe, connect to the SWD target, read and write memory by variable name, let
//! the probe sample variables at a fixed period, and configure the probe's Wi-Fi. USB,
//! serial and TCP links behave the same.
//!
//! ```no_run
//! use xrdap::expr::Resolver;
//! use xrdap::open::{open, OpenOptions};
//!
//! # fn main() -> xrdap::Result<()> {
//! // 自动选唯一的探针；也可以写 Some("COM35")、Some("192.168.0.108")、Some("93EF1C")
//! // Auto-select the only probe; Some("COM35"), Some("192.168.0.108"), Some("93EF1C") also work
//! let mut probe = open(None, &OpenOptions::default())?;
//! probe.connect_swd(4_000_000)?;
//!
//! let mut syms = xrdap::elf::load(std::path::Path::new("app.elf"))?;
//! let speed = Resolver::new(&mut syms).resolve("motor.speed")?;
//! println!("{} = {}", speed.name, speed.read_text(&mut probe)?);
//! # Ok(())
//! # }
//! ```
//!
//! | 模块 / Module | 内容 / Content |
//! |---|---|
//! | [`open`] | 按一个字符串打开探针，或自动选择 / Open a probe from one string, or auto-select |
//! | [`discover`] | 找 USB、串口、局域网（mDNS）上的探针 / Find probes on USB, serial ports, LAN (mDNS) |
//! | [`dap`] | CMSIS-DAP 命令、SWD/ADIv5 初始化、厂商命令 / CMSIS-DAP commands, SWD/ADIv5 setup, vendor commands |
//! | [`expr`]、[`types`]、[`elf`] | 变量表达式、类型、DWARF / Variable expressions, types, DWARF |
//! | [`watch`]、[`csv`] | 探针端采样、CSV 记录 / Probe-side sampling, CSV records |
//! | [`cortex`] | 内核状态和停住、继续、复位 / Core state, halt, resume, reset |
//! | [`wifi`] | 探针 Wi-Fi 的查询、设置、清除 / Query, set and clear the probe's Wi-Fi |
//! | [`vendor`]、[`frame`]、[`transport`] | 厂商命令编码、串口/TCP 帧、三种传输 / Vendor encoding, serial/TCP framing, transports |
//! | [`fake`] | 进程内的模拟探针，测试用 / In-process fake probe for tests |
//!
//! 字节格式见 XRDAP-ESP 仓库的 `docs/protocol.md`。完整示例在 `examples/`。
//! Byte formats: `docs/protocol.md` in the XRDAP-ESP repository. Full examples: `examples/`.

pub mod cortex;
pub mod csv;
pub mod dap;
#[cfg(feature = "discover")]
pub mod discover;
#[cfg(feature = "elf")]
pub mod elf;
pub mod error;
pub mod expr;
pub mod fake;
pub mod frame;
pub mod open;
pub mod transport;
pub mod types;
pub mod usbip;
pub mod vendor;
pub mod watch;
pub mod wifi;

pub use error::{Error, Result};
