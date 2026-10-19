//! 探针的 Wi-Fi 配置：厂商命令 0x86 的查询、设置、清除。
//!
//! 和其他厂商命令一样经 DAP 本身传输，USB、串口、TCP 都能用，不需要另开串口终端。
//! 设置和清除成功后探针约 0.5 秒后重启，正在用的连接会断开。格式见 XRDAP-ESP 的
//! `docs/protocol.md` 3.7 节。
//!
//! ```no_run
//! # fn main() -> xrdap::Result<()> {
//! let mut probe = xrdap::open::open(None, &Default::default())?;
//! let st = xrdap::wifi::status(&mut probe)?;
//! println!("{}", st.describe());
//! # Ok(())
//! # }
//! ```

use std::net::Ipv4Addr;

use crate::dap::Probe;
use crate::vendor;
use crate::{Error, Result};

pub const CMD_WIFI: u8 = 0x86;
const OP_STATUS: u8 = 0x00;
const OP_SET: u8 = 0x01;
const OP_CLEAR: u8 = 0x02;
const FLAG_STARTED: u8 = 0x01;
const FLAG_CONNECTED: u8 = 0x02;

/// 探针报告的 Wi-Fi 状态。不含密码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiStatus {
    /// 已保存的热点名；没有保存时为空
    pub ssid: String,
    /// Wi-Fi 协议栈已启动（有热点参数才会启动）
    pub started: bool,
    /// 已连上热点并拿到 IP
    pub connected: bool,
    pub ip: Ipv4Addr,
    /// 信号强度，dBm；没连上时为 0
    pub rssi: i8,
}

impl WifiStatus {
    /// 一行说明，例如 `热点 lab，已连接，IP 192.168.0.108，信号 -40 dBm`。
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

/// 设置请求。热点名 1 到 32 字节；密码为空（开放网络）或 8 到 63 字节。
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
    check(resp)?;
    if resp.len() < 9 || resp.len() < 9 + resp[8] as usize {
        return Err(Error::protocol(format!(
            "Wi-Fi 状态响应太短：{}",
            vendor::hex(resp)
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

/// 设置、清除的响应：只有状态字节。
pub fn parse_reply(resp: &[u8]) -> Result<()> {
    check(resp)
}

fn check(resp: &[u8]) -> Result<()> {
    match resp {
        [CMD_WIFI, vendor::STATUS_OK, ..] => Ok(()),
        [CMD_WIFI, status, ..] => Err(Error::Status {
            cmd: CMD_WIFI,
            status: *status,
        }),
        // 不支持 0x86 的固件（0.1.x）把它当成未知命令
        _ => Err(Error::protocol(
            "探针不支持 Wi-Fi 配置命令（固件 0.2.0 起才有）",
        )),
    }
}

/// 查询 Wi-Fi 状态。
pub fn status(probe: &mut Probe) -> Result<WifiStatus> {
    parse_status(&probe.xfer(&status_request())?)
}

/// 保存热点参数。成功后探针约 0.5 秒后重启并连接新热点，这个连接随之断开。
pub fn set(probe: &mut Probe, ssid: &str, password: &str) -> Result<()> {
    parse_reply(&probe.xfer(&set_request(ssid, password)?)?)
}

/// 清除热点参数。成功后探针约 0.5 秒后重启，不再连 Wi-Fi。
pub fn clear(probe: &mut Probe) -> Result<()> {
    parse_reply(&probe.xfer(&clear_request())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_set() {
        assert_eq!(
            set_request("ab", "12345678").unwrap(),
            [0x86, 1, 2, b'a', b'b', 8, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8']
        );
        assert_eq!(
            set_request("open", "").unwrap(),
            [0x86, 1, 4, b'o', b'p', b'e', b'n', 0]
        );
        assert!(set_request("", "").is_err());
        assert!(set_request("x", "short").is_err());
        assert!(set_request(&"s".repeat(33), "").is_err());
    }

    #[test]
    fn decode_status() {
        let mut r = vec![0x86, 0, 3, 192, 168, 0, 108, (-40i8) as u8, 3];
        r.extend_from_slice(b"lab");
        let st = parse_status(&r).unwrap();
        assert_eq!(st.ssid, "lab");
        assert!(st.started && st.connected);
        assert_eq!(st.ip, Ipv4Addr::new(192, 168, 0, 108));
        assert_eq!(st.rssi, -40);
        assert!(st.describe().contains("192.168.0.108"));
        assert!(matches!(
            parse_status(&[0x86, 0x01]),
            Err(Error::Status { status: 1, .. })
        ));
        assert!(parse_status(&[0xFF]).is_err());
    }
}
