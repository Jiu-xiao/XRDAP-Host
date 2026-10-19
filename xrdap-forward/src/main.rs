//! XR-DAP 串口转 TCP：让 OpenOCD 通过 `cmsis-dap backend tcp` 使用串口连接的探针。
//!
//! 串口一侧：libxr Topic 帧，请求 topic 为 `dap_req`，响应为 `dap_resp`，
//! 载荷是 `长度(u16 小端) + DAP 包`。
//! TCP 一侧：`"DAP\0" + 长度(u16 小端) + 类型(1 请求 2 响应) + 保留(1) + DAP 包`。
//! 两种帧的编码在 xrdap 库的 `frame` 模块。
//!
//! 请求和响应原样转发，不解析 DAP 内容。同一时间只服务一个 TCP 客户端。

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use clap::Parser;
use xrdap::frame::{parse_tcp_header, tcp_frame, TCP_HEADER_LEN, TCP_TYPE_REQUEST, TCP_TYPE_RESPONSE};
use xrdap::transport::serial::SerialTransport;

/// SUB_CTRL 停止采样
const STOP_SAMPLING: [u8; 2] = [0x84, 0x00];

#[derive(Parser, Debug)]
#[command(name = "xrdap-forward", about = "XR-DAP 串口转 TCP，供 OpenOCD 的 cmsis-dap tcp 后端使用")]
struct Args {
    /// 探针的串口，例如 COM35 或 /dev/ttyUSB0
    #[arg(long)]
    serial: String,
    /// 串口波特率，要和探针固件一致
    #[arg(long, default_value_t = 4_000_000)]
    baud: u32,
    /// TCP 监听地址
    #[arg(long, default_value = "127.0.0.1:3241")]
    listen: String,
    /// 一个请求等待响应的超时（毫秒）
    #[arg(long, default_value_t = 2000)]
    timeout_ms: u64,
    /// 打开串口后不改 DTR/RTS（默认置为 DTR=1、RTS=0，即 ESP32 开发板自动下载电路的运行状态）
    #[arg(long)]
    keep_lines: bool,
    /// 打印每个转发的请求和响应
    #[arg(long)]
    verbose: bool,
}

/// 反复发 DAP_Info(包长) 直到探针应答：打开串口引起的复位要等探针启动完。
fn handshake(link: &mut SerialTransport) -> Result<(), String> {
    let response = link
        .handshake(Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    if response.len() >= 4 && response[0] == 0x00 && response[1] == 0x02 {
        let size = u16::from_le_bytes([response[2], response[3]]);
        println!("探针已应答，DAP 包长 {size} 字节");
    } else {
        println!("探针已应答：{response:02X?}");
    }
    Ok(())
}

fn handle_client(stream: &mut TcpStream, link: &mut SerialTransport, verbose: bool) {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let mut header = [0u8; TCP_HEADER_LEN];
    loop {
        if let Err(e) = stream.read_exact(&mut header) {
            if e.kind() != ErrorKind::UnexpectedEof {
                eprintln!("[{peer}] TCP 读失败：{e}");
            }
            return;
        }
        let Some(len) = parse_tcp_header(&header, TCP_TYPE_REQUEST) else {
            eprintln!("[{peer}] 帧头无效，关闭连接");
            return;
        };
        let mut request = vec![0u8; len];
        if let Err(e) = stream.read_exact(&mut request) {
            eprintln!("[{peer}] TCP 读失败：{e}");
            return;
        }
        if verbose {
            println!("[{peer}] 请求 {} 字节：{:02X?}", request.len(), request);
        }

        // 串口没有应答时回 0xFF（命令不支持），OpenOCD 会报错而不是一直等。
        let response = link.transaction(&request).unwrap_or_else(|e| {
            eprintln!("[{peer}] {e}，回 0xFF");
            vec![0xFF]
        });
        if verbose {
            println!("[{peer}] 响应 {} 字节：{:02X?}", response.len(), response);
        }

        let out = tcp_frame(&response, TCP_TYPE_RESPONSE);
        if stream.write_all(&out).is_err() || stream.flush().is_err() {
            return;
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mut link = SerialTransport::open(
        &args.serial,
        args.baud,
        args.keep_lines,
        Duration::from_millis(args.timeout_ms),
    )?;
    println!("串口 {}，{} baud", args.serial, args.baud);
    handshake(&mut link)?;

    let listener = TcpListener::bind(&args.listen)?;
    println!("在 {} 等待 OpenOCD 连接", listener.local_addr()?);
    for connection in listener.incoming() {
        let mut stream = match connection {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("接受连接失败：{e}");
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
        println!("[{peer}] 已连接");
        handle_client(&mut stream, &mut link, args.verbose);
        // 客户端开过采样的话替它停掉，否则探针会一直独占 SWD，直到串口租期过期。
        println!("[{peer}] 已断开");
        if let Err(e) = link.transaction(&STOP_SAMPLING) {
            eprintln!("停止采样失败：{e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use xrdap::frame::*;

    #[test]
    fn crc32_reference_value() {
        // 反射 CRC-32，最后不取反
        assert_eq!(crc32(b"123456789"), 0x340B_C6D9);
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
    fn parser_resyncs_after_garbage() {
        let topic = crc32(RESP_TOPIC);
        let payload = vec![0x00, 0x02, 0x80, 0x00];
        let mut stream = vec![0x5A, 0x00]; // 被截断的残帧
        stream.extend_from_slice(&build_frame(topic, &payload, 42));
        let mut parser = FrameParser::new(topic);
        assert_eq!(parser.feed(&stream), vec![payload]);
    }

    #[test]
    fn request_payload_has_length_prefix() {
        let payload = serial_payload(&[0x00, 0xFF]);
        assert_eq!(payload, vec![0x02, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn parser_skips_other_topic() {
        let other = build_frame(crc32(REQ_TOPIC), &[0x00, 0x01, 0xAA], 0);
        let mut parser = FrameParser::new(crc32(RESP_TOPIC));
        assert!(parser.feed(&other).is_empty());
    }

    #[test]
    fn tcp_response_frame() {
        assert_eq!(tcp_frame(&[0x84, 0x00], TCP_TYPE_RESPONSE), b"DAP\0\x02\x00\x02\x00\x84\x00");
    }
}
