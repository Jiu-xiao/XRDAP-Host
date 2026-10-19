//! 串口传输：libxr Topic 帧，请求 topic `dap_req`，响应 topic `dap_resp`。
//!
//! 打开串口时，ESP32 开发板的 USB 转串口芯片会通过 DTR/RTS 让探针复位一次，
//! 所以打开后先用 [`SerialTransport::handshake`] 等探针启动完。

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

pub struct SerialTransport {
    port: SerialPort,
    rx: Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    name: String,
    /// 一个请求等待响应的超时。
    pub timeout: Duration,
}

impl SerialTransport {
    /// 打开串口。`keep_lines` 为 false 时置 DTR=1、RTS=0（ESP32 开发板自动下载电路的运行状态）。
    pub fn open(name: &str, baud: u32, keep_lines: bool, timeout: Duration) -> Result<Self> {
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
            timeout,
        })
    }

    /// 发一个请求，等它的响应。之前残留的响应（比如超时后才到的）先丢掉。
    pub fn transaction(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        while self.rx.try_recv().is_ok() {}
        let frame = serial_request_frame(request);
        self.port
            .write_all(&frame)
            .map_err(|e| Error::Io(format!("串口写失败：{e}")))?;
        self.port
            .flush()
            .map_err(|e| Error::Io(format!("串口写失败：{e}")))?;

        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout("等待串口响应超时".into()));
            }
            match self.rx.recv_timeout(remaining) {
                Ok(payload) => match dap_from_payload(&payload) {
                    Some(packet) => return Ok(packet.to_vec()),
                    None => continue,
                },
                Err(RecvTimeoutError::Timeout) => {
                    return Err(Error::Timeout("等待串口响应超时".into()))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::Io("串口读线程已退出".into()))
                }
            }
        }
    }

    /// 反复发 DAP_Info(包长) 直到探针应答（最长 `wait`），返回 DAP_Info 的响应。
    /// 打开串口引起的复位要等探针启动完。
    pub fn handshake(&mut self, wait: Duration) -> Result<Vec<u8>> {
        let normal = self.timeout;
        self.timeout = Duration::from_millis(200);
        let deadline = Instant::now() + wait;
        let result = loop {
            match self.transaction(&[0x00, 0xFF]) {
                Ok(response) => break Ok(response),
                Err(e) if Instant::now() >= deadline => {
                    break Err(Error::Timeout(format!("探针没有应答：{e}")))
                }
                Err(_) => {}
            }
        };
        self.timeout = normal;
        result
    }
}

impl Transport for SerialTransport {
    fn xfer(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        self.transaction(request)
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
