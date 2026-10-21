//! USB 传输：CMSIS-DAP v2 的 Bulk 端点（Windows 上走 WinUSB，Linux 上走 usbfs）。

use std::time::Duration;

use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, Direction, In, Out};
use nusb::{DeviceInfo, Endpoint, Interface, MaybeFuture};

use super::Transport;
use crate::{Error, Result};

pub const VID: u16 = 0x1D50;
pub const PID: u16 = 0x6199;

/// 发一个 OUT 包的最长时间：探针收不下时（端点一直 NAK）算链路出错
const SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// 一个找到的探针。
#[derive(Debug, Clone)]
pub struct UsbProbeInfo {
    pub serial: Option<String>,
    pub product: Option<String>,
    pub manufacturer: Option<String>,
    pub bus: String,
    pub address: u8,
}

fn usb_err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Io(format!("{what}：{e}"))
}

fn probes() -> Result<Vec<DeviceInfo>> {
    let list = nusb::list_devices()
        .wait()
        .map_err(|e| usb_err("列 USB 设备失败", e))?;
    Ok(list
        .filter(|d| d.vendor_id() == VID && d.product_id() == PID)
        .collect())
}

/// 列出 VID:PID 为 1D50:6199 的设备。
pub fn list() -> Result<Vec<UsbProbeInfo>> {
    Ok(probes()?
        .iter()
        .map(|d| UsbProbeInfo {
            serial: d.serial_number().map(str::to_string),
            product: d.product_string().map(str::to_string),
            manufacturer: d.manufacturer_string().map(str::to_string),
            bus: d.bus_id().to_string(),
            address: d.device_address(),
        })
        .collect())
}

pub struct UsbTransport {
    _interface: Interface,
    ep_out: Endpoint<Bulk, Out>,
    ep_in: Endpoint<Bulk, In>,
    in_len: usize,
    serial: String,
}

impl UsbTransport {
    /// 打开探针。`serial` 为 None 时要求只插着一个探针。
    pub fn open(serial: Option<&str>) -> Result<Self> {
        let found = probes()?;
        let info = match serial {
            Some(s) => found
                .iter()
                .find(|d| d.serial_number() == Some(s))
                .ok_or_else(|| {
                    Error::Io(format!(
                        "没有找到序列号为 {s} 的探针（找到 {} 个：{}）",
                        found.len(),
                        serial_list(&found)
                    ))
                })?,
            None => match found.len() {
                0 => {
                    return Err(Error::Io(format!(
                        "没有找到 USB 探针（VID:PID {VID:04X}:{PID:04X}）"
                    )))
                }
                1 => &found[0],
                n => {
                    return Err(Error::usage(format!(
                        "插着 {n} 个探针，用 -p usb:<序列号> 选一个：{}",
                        serial_list(&found)
                    )))
                }
            },
        };
        let serial_text = info.serial_number().unwrap_or("?").to_string();
        let device = info
            .open()
            .wait()
            .map_err(|e| usb_err("打开 USB 设备失败", e))?;

        // 找 CMSIS-DAP v2 接口：厂商类、两个 Bulk 端点，接口名带 "CMSIS-DAP" 的优先。
        let named: Vec<u8> = info
            .interfaces()
            .filter(|i| {
                i.interface_string()
                    .is_some_and(|s| s.contains("CMSIS-DAP"))
            })
            .map(|i| i.interface_number())
            .collect();
        let config = device
            .active_configuration()
            .map_err(|e| usb_err("读 USB 配置描述符失败", e))?;
        let mut candidates = Vec::new();
        for iface in config.interfaces() {
            let alt = iface.first_alt_setting();
            if alt.class() != 0xFF {
                continue;
            }
            let mut ep_in = None;
            let mut ep_out = None;
            for ep in alt.endpoints() {
                if ep.transfer_type() != TransferType::Bulk {
                    continue;
                }
                match ep.direction() {
                    Direction::In => ep_in = ep_in.or(Some(ep.address())),
                    Direction::Out => ep_out = ep_out.or(Some(ep.address())),
                }
            }
            if let (Some(i), Some(o)) = (ep_in, ep_out) {
                candidates.push((iface.interface_number(), i, o));
            }
        }
        candidates.sort_by_key(|(n, _, _)| !named.contains(n));
        let &(number, in_addr, out_addr) = candidates
            .first()
            .ok_or_else(|| Error::Io("设备上没有 CMSIS-DAP v2 的 Bulk 接口".into()))?;

        let interface = device
            .detach_and_claim_interface(number)
            .wait()
            .map_err(|e| {
                usb_err(
                    &format!("占用接口 {number} 失败（被别的程序占用，或没有装 WinUSB 驱动）"),
                    e,
                )
            })?;
        let ep_out = interface
            .endpoint::<Bulk, Out>(out_addr)
            .map_err(|e| usb_err("打开 OUT 端点失败", e))?;
        let ep_in = interface
            .endpoint::<Bulk, In>(in_addr)
            .map_err(|e| usb_err("打开 IN 端点失败", e))?;
        let mut t = Self {
            _interface: interface,
            ep_out,
            ep_in,
            in_len: 0,
            serial: serial_text,
        };
        t.set_packet_size(1024);
        Ok(t)
    }
}

fn serial_list(found: &[DeviceInfo]) -> String {
    found
        .iter()
        .map(|d| d.serial_number().unwrap_or("?").to_string())
        .collect::<Vec<_>>()
        .join("，")
}

impl Transport for UsbTransport {
    fn send(&mut self, request: &[u8]) -> Result<()> {
        let done = self
            .ep_out
            .transfer_blocking(Buffer::from(request.to_vec()), SEND_TIMEOUT);
        done.status.map_err(|e| usb_err("USB 发送失败", e))?;
        // 请求长度正好是端点最大包长的整数倍时，没有短包标志传输结束，探针会一直等；补一个零长度包。
        let mps = self.ep_out.max_packet_size().max(1);
        if !request.is_empty() && request.len().is_multiple_of(mps) {
            let zlp = self
                .ep_out
                .transfer_blocking(Buffer::from(Vec::<u8>::new()), SEND_TIMEOUT);
            zlp.status.map_err(|e| usb_err("USB 发送失败", e))?;
        }
        Ok(())
    }

    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let done = self
            .ep_in
            .transfer_blocking(Buffer::new(self.in_len), timeout);
        match done.status {
            Ok(()) => Ok(done.buffer.into_vec()),
            Err(nusb::transfer::TransferError::Cancelled) => {
                Err(Error::Timeout("等待 USB 响应超时".into()))
            }
            Err(e) => {
                let _ = self.ep_in.clear_halt().wait();
                Err(usb_err("USB 接收失败", e))
            }
        }
    }

    fn describe(&self) -> String {
        format!("USB {}", self.serial)
    }

    /// IN 传输的请求长度要是端点最大包长的整数倍；取不小于 DAP 包长的最小倍数，
    /// 这样响应正好等于包长时传输也能结束。
    fn set_packet_size(&mut self, size: usize) {
        let mps = self.ep_in.max_packet_size().max(1);
        self.in_len = size.max(1).div_ceil(mps) * mps;
    }
}
