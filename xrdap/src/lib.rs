//! XR-DAP 探针的上位机库。
//!
//! - [`frame`]：串口 Topic 帧、TCP 帧、CRC。
//! - [`transport`]：USB（CMSIS-DAP v2 Bulk）、串口、TCP 三种传输。
//! - [`dap`]：CMSIS-DAP 标准命令和 SWD/ADIv5 初始化。
//! - [`vendor`]：XR 厂商扩展命令 0x80 到 0x85 的编码和解码。
//! - [`types`]、[`expr`]：变量类型和表达式解析，把 `motor[1].pid->out` 变成厂商条目。
//! - [`elf`]：从 ELF 的 DWARF 读变量和类型。
//! - [`watch`]：采样循环和探针时间到电脑时间的换算。
//! - [`fake`]：进程内的模拟探针，测试用。
//!
//! 协议见仓库的 `docs/protocol.md`。

pub mod dap;
#[cfg(feature = "elf")]
pub mod elf;
pub mod error;
pub mod expr;
pub mod fake;
pub mod frame;
pub mod transport;
pub mod types;
pub mod vendor;
pub mod watch;

pub use error::{Error, Result};
