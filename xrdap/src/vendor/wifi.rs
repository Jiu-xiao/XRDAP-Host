//! 厂商命令 0x86：探针 Wi-Fi 配置的查询、保存、清除（协议文档 3.7 节）。只有 XRDAP-ESP 支持，
//! INFO 功能位 bit4 为 1 时才有。执行见 [`Probe::wifi_status`](crate::dap::Probe::wifi_status)
//! 等方法。
//!
//! Vendor command 0x86: query, save and clear the probe's Wi-Fi credentials (protocol 3.7).
//! XRDAP-ESP only, present when INFO feature bit 4 is set.

use std::net::Ipv4Addr;

use super::{hex, STATUS_ARG, STATUS_OK, STATUS_RESOURCE};
use crate::{Error, Result};

pub const CMD_WIFI: u8 = 0x86;
pub const OP_STATUS: u8 = 0x00;
pub const OP_SET: u8 = 0x01;
pub const OP_CLEAR: u8 = 0x02;
pub const FLAG_STARTED: u8 = 0x01;
pub const FLAG_CONNECTED: u8 = 0x02;

/// 0x86 的状态说明。
///
/// Meaning of a 0x86 status.
pub fn status_text(status: u8) -> &'static str {
    match status {
        STATUS_OK => "成功",
        STATUS_ARG => "参数非法（热点名或密码长度不对）",
        STATUS_RESOURCE => "探针保存设置失败",
        _ => "未定义的状态",
    }
}

/// 探针报告的 Wi-Fi 状态。不含密码。
///
/// The Wi-Fi state reported by the probe, without the password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiStatus {
    /// 已保存的热点名；没有保存时为空
    ///
    /// Saved SSID; empty when none is saved
    pub ssid: String,
    /// Wi-Fi 协议栈已启动（有热点参数才会启动）
    ///
    /// The Wi-Fi stack is running (it starts only with saved credentials)
    pub started: bool,
    /// 已连上热点并拿到 IP
    ///
    /// Connected and has an IP address
    pub connected: bool,
    pub ip: Ipv4Addr,
    /// 信号强度，dBm；没连上时为 0
    ///
    /// Signal strength in dBm; 0 when not connected
    pub rssi: i8,
}

impl WifiStatus {
    /// 一行说明，例如 `热点 lab，已连接，IP 192.0.2.10，信号 -40 dBm`。
    ///
    /// One-line description.
    pub fn describe(&self) -> String {
        if self.ssid.is_empty() {
            return "没有保存热点参数".into();
        }
        if !self.started {
            return format!(
                "热点 {}，Wi-Fi 没有启动（刚保存的参数重启后生效）",
                self.ssid
            );
        }
        if !self.connected {
            return format!("热点 {}，还没有连上，正在重连", self.ssid);
        }
        format!(
            "热点 {}，已连接，IP {}，信号 {} dBm",
            self.ssid, self.ip, self.rssi
        )
    }
}

pub fn status_request() -> Vec<u8> {
    vec![CMD_WIFI, OP_STATUS]
}

/// 保存请求。热点名 1 到 32 字节；密码为空（开放网络）或 8 到 63 字节。
///
/// Save request. SSID 1 to 32 bytes; password empty (open network) or 8 to 63 bytes.
pub fn set_request(ssid: &str, password: &str) -> Result<Vec<u8>> {
    let (s, p) = (ssid.as_bytes(), password.as_bytes());
    if s.is_empty() || s.len() > 32 {
        return Err(Error::usage("热点名要 1 到 32 个字节"));
    }
    if !p.is_empty() && !(8..=63).contains(&p.len()) {
        return Err(Error::usage("密码要 8 到 63 个字节，开放网络不填"));
    }
    let mut req = vec![CMD_WIFI, OP_SET, s.len() as u8];
    req.extend_from_slice(s);
    req.push(p.len() as u8);
    req.extend_from_slice(p);
    Ok(req)
}

pub fn clear_request() -> Vec<u8> {
    vec![CMD_WIFI, OP_CLEAR]
}

pub fn parse_status(resp: &[u8]) -> Result<WifiStatus> {
    parse_reply(resp)?;
    if resp.len() < 9 || resp.len() < 9 + resp[8] as usize {
        return Err(Error::protocol(format!(
            "Wi-Fi 状态响应太短：{}",
            hex(resp)
        )));
    }
    let flags = resp[2];
    Ok(WifiStatus {
        ssid: String::from_utf8_lossy(&resp[9..9 + resp[8] as usize]).into_owned(),
        started: flags & FLAG_STARTED != 0,
        connected: flags & FLAG_CONNECTED != 0,
        ip: Ipv4Addr::new(resp[3], resp[4], resp[5], resp[6]),
        rssi: resp[7] as i8,
    })
}

/// 保存、清除的响应只有状态字节；状态不为 0 时是错误。
///
/// The save and clear responses carry only the status; non-zero is an error.
pub fn parse_reply(resp: &[u8]) -> Result<()> {
    match resp {
        [CMD_WIFI, STATUS_OK, ..] => Ok(()),
        [CMD_WIFI, status, ..] => Err(Error::Status {
            cmd: CMD_WIFI,
            status: *status,
            text: status_text(*status),
        }),
        // 没有 0x86 的探针按 CMSIS-DAP 回 FF（协议文档 3.1 节）
        _ => Err(Error::protocol("探针不支持 Wi-Fi 配置命令（0x86）")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 协议文档 3.8 的 0x86 例子。
    #[test]
    fn protocol_examples() {
        assert_eq!(status_request(), [0x86, 0x00]);
        assert_eq!(
            set_request("lab", "12345678").unwrap(),
            [0x86, 1, 3, b'l', b'a', b'b', 8, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8']
        );
        assert_eq!(clear_request(), [0x86, 0x02]);
        let st = parse_status(&[
            0x86, 0, 3, 0xC0, 0xA8, 0x01, 0x14, 0xCC, 3, b'l', b'a', b'b',
        ])
        .unwrap();
        assert_eq!(st.ssid, "lab");
        assert!(st.started && st.connected);
        assert_eq!(st.ip, Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(st.rssi, -52);
        let none = parse_status(&[0x86, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert!(none.ssid.is_empty() && !none.started);
    }

    #[test]
    fn bad_input_and_errors() {
        assert_eq!(
            set_request("open", "").unwrap(),
            [0x86, 1, 4, b'o', b'p', b'e', b'n', 0]
        );
        assert!(set_request("", "").is_err());
        assert!(set_request("x", "short").is_err());
        assert!(set_request(&"s".repeat(33), "").is_err());
        assert!(matches!(
            parse_status(&[0x86, 0x01]),
            Err(Error::Status { status: 1, .. })
        ));
        assert!(parse_status(&[0xFF]).is_err());
    }
}
