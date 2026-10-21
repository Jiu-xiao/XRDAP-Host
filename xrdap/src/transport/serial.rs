//! 串口传输：libxr Topic 帧，请求 topic `dap_req`，响应 topic `dap_resp`。
//!
//! 打开串口时，ESP32 开发板的 USB 转串口芯片会通过 DTR/RTS 让探针复位一次，
//! 所以打开后先用 [`Probe::wait_ready`](crate::dap::Probe::wait_ready) 等探针启动完。

use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serial2::SerialPort;

use super::Transport;
use crate::frame::{dap_from_payload, serial_request_frame, FrameParser};
use crate::{Error, Result};

pub const DEFAULT_BAUD: u32 = 4_000_000;
/// 传统 ESP32 版固件的波特率：常见 USB 转串口芯片（CP2102、CH340）都支持的最高速率附近
pub const SAFE_BAUD: u32 = 921_600;

/// 系统里的串口名，排好序（Windows 上 COMx，Linux 上 /dev/ttyXXX）。
pub fn available_ports() -> Result<Vec<String>> {
    let mut ports: Vec<String> = SerialPort::available_ports()
        .map_err(|e| Error::Io(format!("列串口失败：{e}")))?
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    ports.sort();
    Ok(ports)
}

pub struct SerialTransport {
    port: SerialPort,
    rx: Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    name: String,
}

impl SerialTransport {
    /// 打开串口。`keep_lines` 为 false 时置 DTR=1、RTS=0（ESP32 开发板自动下载电路的运行状态）。
    pub fn open(name: &str, baud: u32, keep_lines: bool) -> Result<Self> {
        let mut port =
            SerialPort::open(name, baud).map_err(|e| Error::Io(format!("打不开 {name}：{e}")))?;
        port.set_read_timeout(Duration::from_millis(100))
            .map_err(|e| Error::Io(format!("设置串口超时失败：{e}")))?;
        port.set_write_timeout(Duration::from_millis(1000))
            .map_err(|e| Error::Io(format!("设置串口超时失败：{e}")))?;
        // ESP32 开发板的 USB 转串口芯片用 DTR/RTS 控制 EN 和 BOOT，运行状态是 DTR=1、RTS=0。
        // 打开串口会让探针复位一次，后面的握手会等它启动完。
        if !keep_lines {
            let _ = port.set_dtr(true);
            let _ = port.set_rts(false);
        }
        let mut reader = port
            .try_clone()
            .map_err(|e| Error::Io(format!("复制串口句柄失败：{e}")))?;
        reader
            .set_read_timeout(Duration::from_millis(100))
            .map_err(|e| Error::Io(format!("设置串口超时失败：{e}")))?;
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_reader = stop.clone();
        std::thread::spawn(move || reader_thread(reader, tx, stop_reader));
        Ok(Self {
            port,
            rx,
            stop,
            name: name.to_string(),
        })
    }
}

impl Transport for SerialTransport {
    fn send(&mut self, request: &[u8]) -> Result<()> {
        let frame = serial_request_frame(request);
        self.port
            .write_all(&frame)
            .and_then(|()| self.port.flush())
            .map_err(|e| Error::Io(format!("串口写失败：{e}")))
    }

    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(payload) => {
                    if let Some(packet) = dap_from_payload(&payload) {
                        return Ok(packet.to_vec());
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(Error::Timeout("等待串口响应超时".into()))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::Io("串口读线程已退出".into()))
                }
            }
        }
    }

    fn describe(&self) -> String {
        format!("串口 {}", self.name)
    }
}

impl Drop for SerialTransport {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn reader_thread(port: SerialPort, tx: Sender<Vec<u8>>, stop: Arc<AtomicBool>) {
    let mut parser = FrameParser::responses();
    let mut buf = [0u8; 1024];
    while !stop.load(Ordering::Relaxed) {
        match port.read(&mut buf) {
            Ok(0) => continue,
            Ok(n) => {
                for payload in parser.feed(&buf[..n]) {
                    if tx.send(payload).is_err() {
                        return;
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::TimedOut => continue,
            Err(_) => return,
        }
    }
}
