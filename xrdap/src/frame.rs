//! 链路帧：串口上的 libxr Topic 帧和 TCP 上的 `"DAP\0"` 帧。格式见 docs/protocol.md 第 1、2 节。

use crate::bytes::le_u32;

pub const TCP_MAGIC: &[u8; 4] = b"DAP\0";
pub const TCP_HEADER_LEN: usize = 8;
pub const TCP_TYPE_REQUEST: u8 = 1;
pub const TCP_TYPE_RESPONSE: u8 = 2;

pub const REQ_TOPIC: &[u8] = b"dap_req";
pub const RESP_TOPIC: &[u8] = b"dap_resp";

pub const PACKET_PREFIX: u8 = 0x5A;
pub const PACKET_VERSION: u8 = 1;
pub const HEADER_LEN: usize = 16;
/// 帧头 16 字节加尾 CRC8 1 字节。
pub const FRAME_OVERHEAD: usize = 17;
pub const MAX_TOPIC_PAYLOAD: usize = 2 + 4096;

/// libxr 的 CRC8（多项式 0x8C 反射，初值 0xFF）。
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0xFFu8;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8C
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// libxr 的 CRC32（反射，初值 0xFFFFFFFF，最后不取反），用来算 topic 名的键。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// 组一个 Topic 帧：16 字节头 + 载荷 + 1 字节尾 CRC8。
pub fn build_frame(topic_crc: u32, payload: &[u8], timestamp_us: u64) -> Vec<u8> {
    let mut frame = vec![0u8; HEADER_LEN + payload.len() + 1];
    frame[0] = PACKET_PREFIX;
    frame[1..4].copy_from_slice(&(payload.len() as u32).to_le_bytes()[..3]);
    frame[4..8].copy_from_slice(&topic_crc.to_le_bytes());
    frame[8..14].copy_from_slice(&timestamp_us.to_le_bytes()[..6]);
    frame[14] = PACKET_VERSION;
    frame[15] = crc8(&frame[..15]);
    frame[HEADER_LEN..HEADER_LEN + payload.len()].copy_from_slice(payload);
    frame[HEADER_LEN + payload.len()] = crc8(&frame[..HEADER_LEN + payload.len()]);
    frame
}

/// 串口上的 Topic 载荷是 `u16 长度 + DAP 包`。
pub fn serial_payload(request: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(2 + request.len());
    payload.extend_from_slice(&(request.len() as u16).to_le_bytes());
    payload.extend_from_slice(request);
    payload
}

/// 一个完整的串口请求帧（topic `dap_req`）。
pub fn serial_request_frame(request: &[u8]) -> Vec<u8> {
    build_frame(crc32(REQ_TOPIC), &serial_payload(request), 0)
}

/// 从 Topic 载荷里取出 DAP 包；长度字段不合理时返回 None。
pub fn dap_from_payload(payload: &[u8]) -> Option<&[u8]> {
    if payload.len() < 2 {
        return None;
    }
    let len = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    if len > payload.len() - 2 {
        return None;
    }
    Some(&payload[2..2 + len])
}

/// 从字节流里切出 Topic 帧；头或尾 CRC 错时丢一个字节重新找前缀。
/// 只返回 topic 与 `topic_crc` 相同的帧的载荷。
pub struct FrameParser {
    buf: Vec<u8>,
    topic_crc: u32,
}

impl FrameParser {
    pub fn new(topic_crc: u32) -> Self {
        Self {
            buf: Vec::new(),
            topic_crc,
        }
    }

    /// 解析探针发来的响应帧（topic `dap_resp`）。
    pub fn responses() -> Self {
        Self::new(crc32(RESP_TOPIC))
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            let Some(start) = self.buf.iter().position(|&b| b == PACKET_PREFIX) else {
                self.buf.clear();
                break;
            };
            if start > 0 {
                self.buf.drain(..start);
            }
            if self.buf.len() < HEADER_LEN {
                break;
            }
            let data_len =
                self.buf[1] as usize | (self.buf[2] as usize) << 8 | (self.buf[3] as usize) << 16;
            if self.buf[14] != PACKET_VERSION
                || data_len > MAX_TOPIC_PAYLOAD
                || crc8(&self.buf[..HEADER_LEN - 1]) != self.buf[HEADER_LEN - 1]
            {
                self.buf.drain(..1);
                continue;
            }
            let total = HEADER_LEN + data_len + 1;
            if self.buf.len() < total {
                break;
            }
            if crc8(&self.buf[..HEADER_LEN + data_len]) != self.buf[HEADER_LEN + data_len] {
                self.buf.drain(..1);
                continue;
            }
            let topic_crc = le_u32(&self.buf, 4);
            let payload = self.buf[HEADER_LEN..HEADER_LEN + data_len].to_vec();
            self.buf.drain(..total);
            if topic_crc == self.topic_crc {
                out.push(payload);
            }
        }
        if self.buf.len() > 65536 {
            self.buf.clear();
        }
        out
    }
}

/// TCP 帧头：`"DAP\0" + 长度(2) + 类型(1) + 保留(1)`。
pub fn tcp_header(len: usize, kind: u8) -> [u8; TCP_HEADER_LEN] {
    let mut h = [0u8; TCP_HEADER_LEN];
    h[..4].copy_from_slice(TCP_MAGIC);
    h[4..6].copy_from_slice(&(len as u16).to_le_bytes());
    h[6] = kind;
    h
}

/// 组一个完整的 TCP 帧。
pub fn tcp_frame(packet: &[u8], kind: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(TCP_HEADER_LEN + packet.len());
    out.extend_from_slice(&tcp_header(packet.len(), kind));
    out.extend_from_slice(packet);
    out
}

/// 检查 TCP 帧头，返回后面 DAP 包的长度。
pub fn parse_tcp_header(h: &[u8; TCP_HEADER_LEN], kind: u8) -> Option<usize> {
    if &h[..4] != TCP_MAGIC || h[6] != kind {
        return None;
    }
    Some(u16::from_le_bytes([h[4], h[5]]) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_reference_value() {
        assert_eq!(crc32(b"123456789"), 0x340B_C6D9);
        assert_eq!(crc32(REQ_TOPIC), 0xDC83_09C5);
        assert_eq!(crc32(RESP_TOPIC), 0x89E1_A688);
    }

    #[test]
    fn dap_info_request_matches_doc() {
        // docs/protocol.md 第 1 节的例子
        let frame = serial_request_frame(&[0x00, 0xFF]);
        let expect = [
            0x5A, 0x04, 0x00, 0x00, 0xC5, 0x09, 0x83, 0xDC, 0, 0, 0, 0, 0, 0, 0x01, 0xB1, 0x02,
            0x00, 0x00, 0xFF, 0x32,
        ];
        assert_eq!(frame, expect);
    }

    #[test]
    fn frame_round_trip() {
        let topic = crc32(RESP_TOPIC);
        let payload: Vec<u8> = (0..=255u8).cycle().take(2 + 1024).collect();
        let frame = build_frame(topic, &payload, 1_000_000);
        assert_eq!(frame.len(), FRAME_OVERHEAD + payload.len());
        let mut parser = FrameParser::new(topic);
        assert_eq!(parser.feed(&frame), vec![payload]);
    }

    #[test]
    fn parser_resyncs_and_handles_split_input() {
        let topic = crc32(RESP_TOPIC);
        let payload = vec![0x02, 0x00, 0x80, 0x00];
        let mut stream = vec![0x5A, 0x00, 0x13];
        stream.extend_from_slice(&build_frame(topic, &payload, 42));
        stream.extend_from_slice(&build_frame(topic, &payload, 43));
        let mut parser = FrameParser::new(topic);
        let mut got = Vec::new();
        for chunk in stream.chunks(5) {
            got.extend(parser.feed(chunk));
        }
        assert_eq!(got, vec![payload.clone(), payload]);
        assert_eq!(dap_from_payload(&got[0]), Some(&[0x80, 0x00][..]));
    }

    #[test]
    fn tcp_frame_layout() {
        let f = tcp_frame(&[0x80], TCP_TYPE_REQUEST);
        assert_eq!(f, b"DAP\0\x01\x00\x01\x00\x80");
        let h: [u8; 8] = f[..8].try_into().unwrap();
        assert_eq!(parse_tcp_header(&h, TCP_TYPE_REQUEST), Some(1));
        assert_eq!(parse_tcp_header(&h, TCP_TYPE_RESPONSE), None);
    }
}
