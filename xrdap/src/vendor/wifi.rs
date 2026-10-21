//! 厂商命令 0x86：探针 Wi-Fi 配置的查询、保存、清除（协议文档 3.7 节）。只有 XRDAP-ESP 支持，
//! INFO 功能位 bit4 为 1 时才有。执行见 [`Probe::wifi_status`](crate::dap::Probe::wifi_status)
//! 等方法。
//!
//! Vendor command 0x86: query, save and clear the probe's Wi-Fi credentials (protocol 3.7).
//! XRDAP-ESP only, present when INFO feature bit 4 is set.

use std::net::Ipv4Addr;

use super::{hex, STATUS_ARG, STATUS_OK, STATUS_RESOURCE};
use crate::{Error, Result};

/// 命令字节：Wi-Fi 配置。
///
/// Command byte: Wi-Fi configuration.
pub const CMD_WIFI: u8 = 0x86;
/// op：查询状态。
///
/// op: query the state.
pub const OP_STATUS: u8 = 0x00;
/// op：保存热点参数。
///
/// op: save the credentials.
pub const OP_SET: u8 = 0x01;
/// op：清除热点参数。
///
/// op: clear the credentials.
pub const OP_CLEAR: u8 = 0x02;
/// 状态标志 bit0：Wi-Fi 已启动。
///
/// Status flag bit 0: Wi-Fi is running.
pub const FLAG_STARTED: u8 = 0x01;
/// 状态标志 bit1：已连上热点并拿到 IP。
///
/// Status flag bit 1: connected to the hotspot with an IP address.
pub const FLAG_CONNECTED: u8 = 0x02;
/// 状态标志 bit2：本次启动时 NVS 损坏，已擦除重建。
///
/// Status flag bit2: NVS was damaged at this boot and was erased and rebuilt.
pub const FLAG_NVS_REBUILT: u8 = 0x04;
/// 清除请求的确认字节（协议文档 3.7 节）：没有或不对时探针不清除。
///
/// Confirm byte of the clear request (protocol section 3.7); the probe clears nothing without it.
pub const CLEAR_CONFIRM: u8 = 0xA5;

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
    /// 探针的 IPv4 地址；没连上时为 0.0.0.0
    ///
    /// The probe's IPv4 address; 0.0.0.0 when not connected
    pub ip: Ipv4Addr,
    /// 信号强度，dBm；没连上时为 0
    ///
    /// Signal strength in dBm; 0 when not connected
    pub rssi: i8,
    /// 本次启动时 NVS 损坏，已擦除重建。
    ///
    /// NVS was damaged at this boot and was erased and rebuilt.
    pub nvs_rebuilt: bool,
    /// 最近一次清除热点参数的来源，编码见 [`WifiStatus::last_clear_text`]。
    ///
    /// What last cleared the credentials; see [`WifiStatus::last_clear_text`].
    pub last_clear: u8,
    /// 本次启动的复位原因，编码见 [`WifiStatus::reset_reason_text`]。
    ///
    /// Reset reason of this boot; see [`WifiStatus::reset_reason_text`].
    pub reset_reason: u8,
}

impl WifiStatus {
    /// 一行说明，例如 `热点 lab，已连接，IP 192.0.2.10，信号 -40 dBm`。
    ///
    /// One-line description, e.g. `热点 lab，已连接，IP 192.0.2.10，信号 -40 dBm`.
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

    /// 最近一次清除热点参数的来源（协议文档 3.7 节）。
    ///
    /// What last cleared the credentials (protocol section 3.7).
    pub fn last_clear_text(&self) -> &'static str {
        match self.last_clear {
            0 => "没有清除过或不知道",
            1 => "板上按键",
            2 => "串口命令行 wifi clear",
            3 => "0x86 清除命令",
            4 => "NVS 修复",
            _ => "未定义",
        }
    }

    /// 本次启动的复位原因（协议文档 3.7 节）。
    ///
    /// Reset reason of this boot (protocol section 3.7).
    pub fn reset_reason_text(&self) -> &'static str {
        match self.reset_reason {
            1 => "上电",
            2 => "软件重启",
            3 => "看门狗",
            4 => "程序崩溃",
            5 => "欠压",
            6 => "其他",
            _ => "不知道",
        }
    }

    /// 一行历史说明：最近一次清除的来源、本次启动的原因，以及 NVS 是否重建过。
    ///
    /// One line of history: what last cleared the credentials, why this boot happened, and
    /// whether NVS was rebuilt.
    pub fn history(&self) -> String {
        let nvs = if self.nvs_rebuilt {
            "；本次启动时 NVS 损坏，已擦除重建"
        } else {
            ""
        };
        format!(
            "最近一次清除热点参数：{}；本次启动原因：{}{nvs}",
            self.last_clear_text(),
            self.reset_reason_text()
        )
    }
}

/// 查询请求包。
///
/// The query request packet.
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

/// 清除请求包。
///
/// The clear request packet.
pub fn clear_request() -> Vec<u8> {
    vec![CMD_WIFI, OP_CLEAR, CLEAR_CONFIRM]
}

/// 解码查询响应。
///
/// Decode a query response.
pub fn parse_status(resp: &[u8]) -> Result<WifiStatus> {
    parse_reply(resp)?;
    let n = resp.get(8).copied().unwrap_or(0) as usize;
    if resp.len() < 11 + n {
        return Err(Error::protocol(format!(
            "Wi-Fi 状态响应太短：{}",
            hex(resp)
        )));
    }
    let flags = resp[2];
    Ok(WifiStatus {
        ssid: String::from_utf8_lossy(&resp[9..9 + n]).into_owned(),
        started: flags & FLAG_STARTED != 0,
        connected: flags & FLAG_CONNECTED != 0,
        ip: Ipv4Addr::new(resp[3], resp[4], resp[5], resp[6]),
        rssi: resp[7] as i8,
        nvs_rebuilt: flags & FLAG_NVS_REBUILT != 0,
        last_clear: resp[9 + n],
        reset_reason: resp[10 + n],
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
        assert_eq!(clear_request(), [0x86, 0x02, 0xA5]);
        let st = parse_status(&[
            0x86, 0, 3, 0xC0, 0xA8, 0x01, 0x14, 0xCC, 3, b'l', b'a', b'b', 0, 1,
        ])
        .unwrap();
        assert_eq!(st.ssid, "lab");
        assert!(st.started && st.connected && !st.nvs_rebuilt);
        assert_eq!(st.ip, Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(st.rssi, -52);
        assert_eq!(st.reset_reason_text(), "上电");
        // 0x86 清除并软件重启之后
        let after = parse_status(&[0x86, 0, 0, 0, 0, 0, 0, 0, 0, 3, 2]).unwrap();
        assert!(after.ssid.is_empty() && !after.started);
        assert_eq!(after.last_clear_text(), "0x86 清除命令");
        assert_eq!(after.reset_reason_text(), "软件重启");
        let rebuilt = parse_status(&[0x86, 0, 4, 0, 0, 0, 0, 0, 0, 4, 1]).unwrap();
        assert!(rebuilt.nvs_rebuilt && rebuilt.history().contains("重建"));
        // 版本 1 的查询响应没有后两个字节
        assert!(parse_status(&[0x86, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
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
