//! USB 传输：CMSIS-DAP v2 的 Bulk 端点（Windows 上走 WinUSB，Linux 上走 usbfs）。
//!
//! USB transport: the CMSIS-DAP v2 bulk endpoints (WinUSB on Windows, usbfs on Linux).

use std::time::Duration;

use nusb::descriptors::TransferType;
use nusb::transfer::{
    Buffer, Bulk, ControlIn, ControlOut, ControlType, Direction, In, Out, Recipient,
};
use nusb::{DeviceInfo, Endpoint, Interface, MaybeFuture};

use super::Transport;
use crate::lang::tr;
use crate::{trf, Error, Result};

/// 探针的 USB 厂商 ID。
///
/// USB vendor ID of the probe.
pub const VID: u16 = 0x1D50;
/// 探针的 USB 产品 ID。
///
/// USB product ID of the probe.
pub const PID: u16 = 0x6199;

/// 发一个 OUT 包的最长时间：探针收不下时（端点一直 NAK）算链路出错
const SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// 一个找到的探针。
///
/// A probe that was found.
#[derive(Debug, Clone)]
pub struct UsbProbeInfo {
    /// USB 描述符里的序列号。
    ///
    /// Serial number from the USB descriptor.
    pub serial: Option<String>,
    /// USB 描述符里的产品名。
    ///
    /// Product name from the USB descriptor.
    pub product: Option<String>,
    /// USB 描述符里的厂商名。
    ///
    /// Manufacturer name from the USB descriptor.
    pub manufacturer: Option<String>,
    /// 设备所在的总线。
    ///
    /// The bus the device is on.
    pub bus: String,
    /// 设备在总线上的地址。
    ///
    /// The device address on the bus.
    pub address: u8,
}

fn usb_err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Io(trf!("{what}: {e}", "{what}：{e}"))
}

/// 名字表明设备是 CMSIS-DAP 探针：产品名或某个接口名里带 "CMSIS-DAP"（CMSIS-DAP 规范对
/// v1 的产品名、对 v2 的接口名的要求）。用同一对 VID:PID 的别的设备两处都不带，不算探针。
///
/// The names say the device is a CMSIS-DAP probe: the product string or an interface string
/// contains "CMSIS-DAP", as the CMSIS-DAP specification asks of v1 product strings and v2
/// interface strings. Other devices with the same VID:PID have neither and are not probes.
fn named_cmsis_dap<'a>(
    product: Option<&str>,
    mut interfaces: impl Iterator<Item = Option<&'a str>>,
) -> bool {
    const NAME: &str = "CMSIS-DAP";
    product.is_some_and(|s| s.contains(NAME))
        || interfaces.any(|s| s.is_some_and(|s| s.contains(NAME)))
}

fn probes() -> Result<Vec<DeviceInfo>> {
    let list = nusb::list_devices()
        .wait()
        .map_err(|e| usb_err(tr("Listing USB devices failed", "列 USB 设备失败"), e))?;
    Ok(list
        .filter(|d| d.vendor_id() == VID && d.product_id() == PID)
        .filter(|d| {
            named_cmsis_dap(
                d.product_string(),
                d.interfaces().map(|i| i.interface_string()),
            )
        })
        .collect())
}

/// 列出 VID:PID 为 1D50:6199、产品名或接口名带 "CMSIS-DAP" 的设备。
///
/// List the devices with VID:PID 1D50:6199 whose product or interface string contains
/// "CMSIS-DAP".
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

/// 一个打开的 USB 探针（CMSIS-DAP v2 的 Bulk 端点）。
///
/// An open USB probe (the CMSIS-DAP v2 bulk endpoints).
pub struct UsbTransport {
    _interface: Interface,
    ep_out: Endpoint<Bulk, Out>,
    ep_in: Endpoint<Bulk, In>,
    in_len: usize,
    serial: String,
    interface_number: u8,
}

impl UsbTransport {
    /// 打开探针。`serial` 为 None 时要求只插着一个探针。
    ///
    /// Open a probe. With `serial` None exactly one probe must be plugged in.
    pub fn open(serial: Option<&str>) -> Result<Self> {
        let found = probes()?;
        let info = match serial {
            Some(s) => found
                .iter()
                .find(|d| d.serial_number() == Some(s))
                .ok_or_else(|| {
                    Error::Select(trf!(
                        "No USB probe with serial number {s} found ({} found: {})",
                        "没有找到序列号为 {s} 的 USB 探针（找到 {} 块：{}）",
                        found.len(),
                        serial_list(&found)
                    ))
                })?,
            None => match found.len() {
                0 => {
                    return Err(Error::Select(trf!(
                        "No USB probe found (VID:PID {VID:04X}:{PID:04X})",
                        "没有找到 USB 探针（VID:PID {VID:04X}:{PID:04X}）"
                    )))
                }
                1 => &found[0],
                n => {
                    return Err(Error::Select(trf!(
                        "{n} USB probes are plugged in: {}",
                        "插着 {n} 块 USB 探针：{}",
                        serial_list(&found)
                    )))
                }
            },
        };
        let serial_text = info.serial_number().unwrap_or("?").to_string();
        let device = info
            .open()
            .wait()
            .map_err(|e| usb_err(tr("Opening the USB device failed", "打开 USB 设备失败"), e))?;

        // 找 CMSIS-DAP v2 接口：厂商类、两个 Bulk 端点，接口名带 "CMSIS-DAP" 的优先。
        let named: Vec<u8> = info
            .interfaces()
            .filter(|i| {
                i.interface_string()
                    .is_some_and(|s| s.contains("CMSIS-DAP"))
            })
            .map(|i| i.interface_number())
            .collect();
        let config = device.active_configuration().map_err(|e| {
            usb_err(
                tr(
                    "Reading the USB configuration descriptor failed",
                    "读 USB 配置描述符失败",
                ),
                e,
            )
        })?;
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
        let &(number, in_addr, out_addr) = candidates.first().ok_or_else(|| {
            Error::Io(
                tr(
                    "The device has no CMSIS-DAP v2 bulk interface",
                    "设备上没有 CMSIS-DAP v2 的 Bulk 接口",
                )
                .into(),
            )
        })?;

        let interface = device
            .detach_and_claim_interface(number)
            .wait()
            .map_err(|e| {
                usb_err(
                    &trf!(
                        "Claiming interface {number} failed (in use by another program, or the WinUSB driver is not installed)",
                        "占用接口 {number} 失败（被别的程序占用，或没有装 WinUSB 驱动）"
                    ),
                    e,
                )
            })?;
        let ep_out = interface.endpoint::<Bulk, Out>(out_addr).map_err(|e| {
            usb_err(
                tr("Opening the OUT endpoint failed", "打开 OUT 端点失败"),
                e,
            )
        })?;
        let ep_in = interface
            .endpoint::<Bulk, In>(in_addr)
            .map_err(|e| usb_err(tr("Opening the IN endpoint failed", "打开 IN 端点失败"), e))?;
        let mut t = Self {
            _interface: interface,
            ep_out,
            ep_in,
            in_len: 0,
            serial: serial_text,
            interface_number: number,
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
        .join(tr(", ", "，"))
}

impl Transport for UsbTransport {
    fn send(&mut self, request: &[u8]) -> Result<()> {
        let done = self
            .ep_out
            .transfer_blocking(Buffer::from(request.to_vec()), SEND_TIMEOUT);
        done.status
            .map_err(|e| usb_err(tr("USB send failed", "USB 发送失败"), e))?;
        // 请求长度正好是端点最大包长的整数倍时，没有短包标志传输结束，探针会一直等；补一个零长度包。
        let mps = self.ep_out.max_packet_size().max(1);
        if !request.is_empty() && request.len().is_multiple_of(mps) {
            let zlp = self
                .ep_out
                .transfer_blocking(Buffer::from(Vec::<u8>::new()), SEND_TIMEOUT);
            zlp.status
                .map_err(|e| usb_err(tr("USB send failed", "USB 发送失败"), e))?;
        }
        Ok(())
    }

    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let done = self
            .ep_in
            .transfer_blocking(Buffer::new(self.in_len), timeout);
        match done.status {
            Ok(()) => Ok(done.buffer.into_vec()),
            Err(nusb::transfer::TransferError::Cancelled) => Err(Error::Timeout(
                tr("Timed out waiting for a USB response", "等待 USB 响应超时").into(),
            )),
            Err(e) => {
                let _ = self.ep_in.clear_halt().wait();
                Err(usb_err(tr("USB receive failed", "USB 接收失败"), e))
            }
        }
    }

    fn describe(&self) -> String {
        format!("USB {}", self.serial)
    }

    // 控制口：EP0 厂商请求，发给 DAP 接口。SEND（0x01，OUT）送请求，RECV（0x02，IN）取应答；
    // 应答没好时 RECV 返回 0 字节，稍后重试。固件没有控制口、或请求超过 EP0 的 OUT 缓冲时 SEND
    // 被 STALL，返回 Ok(None)。
    // The control port: EP0 vendor requests to the DAP interface. SEND (0x01, OUT) carries the
    // request, RECV (0x02, IN) the answer; while it is not ready RECV returns 0 bytes and is
    // retried. A firmware without a control port, or a request longer than the EP0 OUT buffer,
    // gets the SEND STALLed: Ok(None).
    fn control(&mut self, request: &[u8]) -> Result<Option<Vec<u8>>> {
        const TIMEOUT: Duration = Duration::from_millis(500);
        let out = ControlOut {
            control_type: ControlType::Vendor,
            recipient: Recipient::Interface,
            request: 0x01,
            value: 0,
            index: u16::from(self.interface_number),
            data: request,
        };
        if self._interface.control_out(out, TIMEOUT).wait().is_err() {
            return Ok(None);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let recv = ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Interface,
                request: 0x02,
                value: 0,
                index: u16::from(self.interface_number),
                length: 96,
            };
            let answer = self
                ._interface
                .control_in(recv, TIMEOUT)
                .wait()
                .map_err(|e| {
                    usb_err(
                        tr("Reading the control port answer failed", "读控制口应答失败"),
                        e,
                    )
                })?;
            if !answer.is_empty() {
                return Ok(Some(answer));
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::Timeout(
                    tr("The control port did not answer", "控制口没有应答").into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// IN 传输的请求长度要是端点最大包长的整数倍；取不小于 DAP 包长的最小倍数，
    /// 这样响应正好等于包长时传输也能结束。
    fn set_packet_size(&mut self, size: usize) {
        let mps = self.ep_in.max_packet_size().max(1);
        self.in_len = size.max(1).div_ceil(mps) * mps;
    }
}

#[cfg(test)]
mod tests {
    use super::named_cmsis_dap;

    #[test]
    fn only_devices_named_cmsis_dap_are_probes() {
        // XR-DAP 探针：产品名带 " CMSIS-DAP"，DAP 接口名是 "CMSIS-DAP v2"
        assert!(named_cmsis_dap(
            Some("XR-DAP-S3 CMSIS-DAP"),
            [Some("CMSIS-DAP v2"), Some("XR-DAP Serial")].into_iter()
        ));
        // 只有接口名（单接口设备在 Windows 上读不到接口名时靠产品名，反之亦然）
        assert!(named_cmsis_dap(
            Some("XR-DAP-V307"),
            [None, Some("CMSIS-DAP v2")].into_iter()
        ));
        assert!(named_cmsis_dap(
            Some("XR-DAP-F103 CMSIS-DAP"),
            [None].into_iter()
        ));
        // 同一对 VID:PID 的别的设备（例如用 LibXR 的主控板）
        assert!(!named_cmsis_dap(
            Some("MainCtrl"),
            [Some("CDC ACM"), None].into_iter()
        ));
        assert!(!named_cmsis_dap(None, std::iter::empty()));
    }
}
