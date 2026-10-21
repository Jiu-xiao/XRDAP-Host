//! 协议层：链路帧、三种传输、厂商命令的编码和解码。日常使用走上层的 [`open`](crate::open)、
//! [`dap::Probe`](crate::dap::Probe)、[`watch`](crate::watch) 等；这里给需要直接处理字节的场合用，
//! 例如实现新的传输或模拟探针。它跟着协议文档变化，不保证接口稳定。
//!
//! Protocol layer: link framing, the three transports, vendor command encoding and decoding.
//! Everyday use goes through [`open`](crate::open), [`dap::Probe`](crate::dap::Probe),
//! [`watch`](crate::watch) and the other upper modules; this layer serves code that handles bytes
//! directly, such as a new transport or a simulated probe. It follows the protocol document and
//! its interface is not kept stable.

pub mod frame;
pub mod transport;
pub mod vendor;
