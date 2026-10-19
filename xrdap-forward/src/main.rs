//! XR-DAP 转发：把一块用串口或 TCP 连着的探针转成别的工具能直接用的形式。
//!
//! - 默认（TCP 模式）：在 3241 端口提供 OpenOCD `cmsis-dap backend tcp` 的协议。
//!   帧格式：`"DAP\0" + 长度(u16 小端) + 类型(1 请求 2 响应) + 保留(1) + DAP 包`。
//! - `--usbip`：在 3240 端口提供 USB/IP 设备。电脑用 `usbip attach` 挂上后出现一个
//!   USB CMSIS-DAP v2 探针，Keil、pyOCD、probe-rs、OpenOCD 都能用。见 xrdap 库的 `usbip` 模块。
//!
//! 请求原样交给探针，不解析 DAP 内容。同一时间只服务一个客户端；客户端断开时替它停掉采样，
//! 否则探针会一直占着 SWD。

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use clap::Parser;
use xrdap::dap::{Probe, ID_PRODUCT, ID_SERIAL};
use xrdap::frame::{
    parse_tcp_header, tcp_frame, TCP_HEADER_LEN, TCP_TYPE_REQUEST, TCP_TYPE_RESPONSE,
};
use xrdap::open::{open, OpenOptions};
use xrdap::usbip::{self, DeviceInfo};

/// SUB_CTRL 停止采样
const STOP_SAMPLING: [u8; 2] = [0x84, 0x00];
/// 探针没有应答时回的响应：0xFF 表示命令不支持，调试工具会报错而不是一直等
const NO_RESPONSE: [u8; 1] = [0xFF];

#[derive(Parser, Debug)]
#[command(
    name = "xrdap-forward",
    about = "把串口或 Wi-Fi 连着的 XR-DAP 转成 OpenOCD 的 TCP 后端，或用 --usbip 转成标准 USB CMSIS-DAP",
    after_help = "例子：
          xrdap-forward -p COM7                 # OpenOCD：cmsis-dap backend tcp，端口 3241
          xrdap-forward -p COM7 --usbip         # 然后 usbip attach -r 127.0.0.1 -b 1-1
          xrdap-forward -p COM7 --usbip --listen 0.0.0.0:3240   # 给局域网里别的电脑用"
)]
struct Args {
    /// 探针：COM7、/dev/ttyACM0、192.168.0.108、xrdap-721384.local、序列号或末尾几位；不写则自动选
    #[arg(short, long)]
    probe: Option<String>,
    /// 同 --probe COMx（旧写法）
    #[arg(long, conflicts_with = "probe", hide = true)]
    serial: Option<String>,
    /// 串口波特率（S3 的串口链路是 4 Mbaud；C3 的 USB 口不看波特率）
    #[arg(long, default_value_t = 4_000_000)]
    baud: u32,
    /// 提供 USB/IP 设备，而不是 OpenOCD 的 TCP 协议
    #[arg(long)]
    usbip: bool,
    /// 监听地址；默认 127.0.0.1:3241，--usbip 时 127.0.0.1:3240
    #[arg(long)]
    listen: Option<String>,
    /// 一个请求等待响应的超时（毫秒）
    #[arg(long, default_value_t = 2000)]
    timeout_ms: u64,
    /// 打印每个转发的请求和响应
    #[arg(short, long)]
    verbose: bool,
}

/// 把一个 DAP 包交给探针；失败时打印原因并回 [`NO_RESPONSE`]。
fn exec(probe: &mut Probe, request: &[u8], verbose: bool) -> Vec<u8> {
    let response = probe.xfer(request).unwrap_or_else(|e| {
        eprintln!("探针没有应答：{e}，回 0xFF");
        NO_RESPONSE.to_vec()
    });
    if verbose {
        println!("{:02X?} -> {:02X?}", request, response);
    }
    response
}

fn stop_sampling(probe: &mut Probe) {
    if let Err(e) = probe.xfer(&STOP_SAMPLING) {
        eprintln!("停止采样失败：{e}");
    }
}

/// OpenOCD TCP 协议：一个请求帧对应一个响应帧。
fn serve_tcp_client(stream: &mut TcpStream, probe: &mut Probe, verbose: bool) {
    let mut header = [0u8; TCP_HEADER_LEN];
    loop {
        if let Err(e) = stream.read_exact(&mut header) {
            if e.kind() != ErrorKind::UnexpectedEof {
                eprintln!("TCP 读失败：{e}");
            }
            return;
        }
        let Some(len) = parse_tcp_header(&header, TCP_TYPE_REQUEST) else {
            eprintln!("帧头无效，关闭连接");
            return;
        };
        let mut request = vec![0u8; len];
        if let Err(e) = stream.read_exact(&mut request) {
            eprintln!("TCP 读失败：{e}");
            return;
        }
        let response = exec(probe, &request, verbose);
        let out = tcp_frame(&response, TCP_TYPE_RESPONSE);
        if stream.write_all(&out).is_err() {
            return;
        }
    }
}

fn run_tcp(listener: &TcpListener, probe: &mut Probe, verbose: bool) -> std::io::Result<()> {
    println!(
        "OpenOCD 配置：adapter driver cmsis-dap; cmsis-dap backend tcp; cmsis-dap tcp port {}",
        listener.local_addr()?.port()
    );
    loop {
        let (mut stream, peer) = listener.accept()?;
        let _ = stream.set_nodelay(true);
        println!("[{peer}] 已连接");
        serve_tcp_client(&mut stream, probe, verbose);
        println!("[{peer}] 已断开");
        stop_sampling(probe);
    }
}

fn run_usbip(listener: &TcpListener, probe: &mut Probe, verbose: bool) -> std::io::Result<()> {
    let product = probe.dap_info_string(ID_PRODUCT).ok().flatten();
    let serial = probe.dap_info_string(ID_SERIAL).ok().flatten();
    let info = DeviceInfo::new(
        product.as_deref().unwrap_or("XR-DAP"),
        serial.as_deref().unwrap_or("XRDAP"),
    );
    let addr = listener.local_addr()?;
    println!("USB 设备：{}，序列号 {}", info.product, info.serial);
    println!(
        "挂载：usbip attach -r {} -b 1-1",
        if addr.ip().is_unspecified() {
            "<本机 IP>".to_string()
        } else {
            addr.ip().to_string()
        }
    );
    // 采样的停止要在 exec 之外做，exec 闭包借着 probe
    let mut stop_needed = false;
    loop {
        let (stream, peer) = listener.accept()?;
        println!("[{peer}] 已连接");
        let r = usbip::serve_connection(stream, &info, &mut |req| {
            stop_needed = true;
            exec(probe, req, verbose)
        });
        match r {
            Ok(()) => println!("[{peer}] 已断开"),
            Err(e) => println!("[{peer}] 连接断开：{e}"),
        }
        if std::mem::take(&mut stop_needed) {
            stop_sampling(probe);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let opts = OpenOptions {
        timeout: Duration::from_millis(args.timeout_ms),
        baud: args.baud,
        ..OpenOptions::default()
    };
    let spec = args.probe.as_deref().or(args.serial.as_deref());
    let mut probe = open(spec, &opts)?;
    println!("探针：{}", probe.describe());

    let default_port = if args.usbip { usbip::PORT } else { 3241 };
    let listen = args
        .listen
        .unwrap_or_else(|| format!("127.0.0.1:{default_port}"));
    let listener = TcpListener::bind(&listen)?;
    println!("在 {} 等待连接", listener.local_addr()?);
    if args.usbip {
        run_usbip(&listener, &mut probe, args.verbose)?;
    } else {
        run_tcp(&listener, &mut probe, args.verbose)?;
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
        assert_eq!(
            tcp_frame(&[0x84, 0x00], TCP_TYPE_RESPONSE),
            b"DAP\0\x02\x00\x02\x00\x84\x00"
        );
    }
}
