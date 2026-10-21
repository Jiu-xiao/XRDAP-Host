//! 串口传输：libxr Topic 帧，请求 topic `dap_req`，响应 topic `dap_resp`。
//!
//! 打开串口时 DTR、RTS 都不置位，和乐鑫 idf_monitor 一样：ESP32 开发板的自动下载电路用这两根线
//! 控制 EN 和 GPIO0，置位任何一根都可能让芯片复位或进入下载模式。只有一个端口句柄，读在调用
//! 线程里进行，传输丢掉时端口立即关闭。
//!
//! Serial transport: libxr topic frames, request topic `dap_req`, response topic `dap_resp`.
//! The port is opened with DTR and RTS both deasserted, as Espressif's idf_monitor does: the
//! auto-download circuit of ESP32 boards drives EN and GPIO0 from these lines, and asserting
//! either can reset the chip or enter the bootloader. There is a single port handle and reads
//! happen on the calling thread, so dropping the transport closes the port at once.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

use serialport::{ClearBuffer, FlowControl, SerialPort};

use super::Transport;
use crate::frame::{dap_from_payload, serial_request_frame, FrameParser};
use crate::{Error, Result};

/// 串口探针固件的波特率。
///
/// Baud rate of the serial probe firmware.
pub const DEFAULT_BAUD: u32 = 4_000_000;
/// 传统 ESP32 版固件的波特率：常见 USB 转串口芯片（CP2102、CH340）都支持的最高速率附近。
///
/// Baud rate of the classic ESP32 firmware, near the maximum of common USB-serial chips.
pub const SAFE_BAUD: u32 = 921_600;

/// 系统里的串口名，排好序（Windows 上 COMx，Linux 上 /dev/ttyXXX）。
///
/// Serial port names on this system, sorted (COMx on Windows, /dev/ttyXXX on Linux).
pub fn available_ports() -> Result<Vec<String>> {
    let mut ports: Vec<String> = serialport::available_ports()
        .map_err(|e| Error::Io(format!("列串口失败：{e}")))?
        .into_iter()
        .map(|p| p.port_name)
        .collect();
    ports.sort();
    Ok(ports)
}

/// 一个打开的串口。
///
/// An open serial port.
pub struct SerialTransport {
    port: Box<dyn SerialPort>,
    parser: FrameParser,
    /// 已经解析出来、还没取走的帧载荷
    ready: VecDeque<Vec<u8>>,
    name: String,
}

impl SerialTransport {
    /// 打开串口，DTR、RTS 都不置位。
    ///
    /// Open a serial port with DTR and RTS both deasserted.
    pub fn open(name: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(name, baud)
            .flow_control(FlowControl::None)
            .dtr_on_open(false)
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|e| Error::Io(format!("打不开 {name}：{e}")))?;
        Self::with_port(port, name)
    }

    /// 在已经打开的端口上建立传输：先把 DTR、RTS 都撤销（Linux 打开 tty 时内核会置位它们），
    /// 再丢掉接收缓冲里的旧数据。
    fn with_port(mut port: Box<dyn SerialPort>, name: &str) -> Result<Self> {
        let err = |e: serialport::Error| Error::Io(format!("设置串口 {name} 失败：{e}"));
        port.write_data_terminal_ready(false).map_err(err)?;
        port.write_request_to_send(false).map_err(err)?;
        port.clear(ClearBuffer::Input).map_err(err)?;
        Ok(Self {
            port,
            parser: FrameParser::responses(),
            ready: VecDeque::new(),
            name: name.to_string(),
        })
    }
}

impl Transport for SerialTransport {
    fn send(&mut self, request: &[u8]) -> Result<()> {
        self.port
            .write_all(&serial_request_frame(request))
            .and_then(|()| self.port.flush())
            .map_err(|e| Error::Io(format!("串口写失败：{e}")))
    }

    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut chunk = [0u8; 1024];
        loop {
            while let Some(payload) = self.ready.pop_front() {
                if let Some(packet) = dap_from_payload(&payload) {
                    return Ok(packet.to_vec());
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Timeout("等待串口响应超时".into()));
            }
            self.port
                .set_timeout(left)
                .map_err(|e| Error::Io(format!("设置串口超时失败：{e}")))?;
            match self.port.read(&mut chunk) {
                Ok(n) => self.ready.extend(self.parser.feed(&chunk[..n])),
                Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
                Err(e) => return Err(Error::Io(format!("串口读失败：{e}"))),
            }
        }
    }

    fn describe(&self) -> String {
        format!("串口 {}", self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{build_frame, crc32, serial_payload, RESP_TOPIC};
    use serialport::{DataBits, Parity, StopBits};
    use std::sync::{Arc, Mutex};

    /// 记录 DTR、RTS 每次被写成什么，按脚本吐出接收的字节。
    #[derive(Default)]
    struct Lines {
        dtr: Vec<bool>,
        rts: Vec<bool>,
        rx: VecDeque<Vec<u8>>,
        tx: Vec<u8>,
    }

    struct MockPort(Arc<Mutex<Lines>>);

    impl Read for MockPort {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.lock().unwrap().rx.pop_front() {
                Some(d) => {
                    buf[..d.len()].copy_from_slice(&d);
                    Ok(d.len())
                }
                None => Err(ErrorKind::TimedOut.into()),
            }
        }
    }

    impl Write for MockPort {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().tx.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl SerialPort for MockPort {
        fn name(&self) -> Option<String> {
            None
        }
        fn baud_rate(&self) -> serialport::Result<u32> {
            Ok(DEFAULT_BAUD)
        }
        fn data_bits(&self) -> serialport::Result<DataBits> {
            Ok(DataBits::Eight)
        }
        fn flow_control(&self) -> serialport::Result<FlowControl> {
            Ok(FlowControl::None)
        }
        fn parity(&self) -> serialport::Result<Parity> {
            Ok(Parity::None)
        }
        fn stop_bits(&self) -> serialport::Result<StopBits> {
            Ok(StopBits::One)
        }
        fn timeout(&self) -> Duration {
            Duration::ZERO
        }
        fn set_baud_rate(&mut self, _: u32) -> serialport::Result<()> {
            Ok(())
        }
        fn set_data_bits(&mut self, _: DataBits) -> serialport::Result<()> {
            Ok(())
        }
        fn set_flow_control(&mut self, _: FlowControl) -> serialport::Result<()> {
            Ok(())
        }
        fn set_parity(&mut self, _: Parity) -> serialport::Result<()> {
            Ok(())
        }
        fn set_stop_bits(&mut self, _: StopBits) -> serialport::Result<()> {
            Ok(())
        }
        fn set_timeout(&mut self, _: Duration) -> serialport::Result<()> {
            Ok(())
        }
        fn write_request_to_send(&mut self, level: bool) -> serialport::Result<()> {
            self.0.lock().unwrap().rts.push(level);
            Ok(())
        }
        fn write_data_terminal_ready(&mut self, level: bool) -> serialport::Result<()> {
            self.0.lock().unwrap().dtr.push(level);
            Ok(())
        }
        fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
            Ok(false)
        }
        fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
            Ok(false)
        }
        fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
            Ok(false)
        }
        fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
            Ok(false)
        }
        fn bytes_to_read(&self) -> serialport::Result<u32> {
            Ok(0)
        }
        fn bytes_to_write(&self) -> serialport::Result<u32> {
            Ok(0)
        }
        fn clear(&self, _: ClearBuffer) -> serialport::Result<()> {
            Ok(())
        }
        fn try_clone(&self) -> serialport::Result<Box<dyn SerialPort>> {
            Ok(Box::new(MockPort(self.0.clone())))
        }
        fn set_break(&self) -> serialport::Result<()> {
            Ok(())
        }
        fn clear_break(&self) -> serialport::Result<()> {
            Ok(())
        }
    }

    fn transport() -> (SerialTransport, Arc<Mutex<Lines>>) {
        let lines = Arc::new(Mutex::new(Lines::default()));
        let t = SerialTransport::with_port(Box::new(MockPort(lines.clone())), "MOCK").unwrap();
        (t, lines)
    }

    /// 和 idf_monitor 一样，DTR、RTS 都保持不置位。
    #[test]
    fn lines_stay_deasserted() {
        let (mut t, lines) = transport();
        t.send(&[0x00, 0xFF]).unwrap();
        let _ = t.recv(Duration::from_millis(1));
        let l = lines.lock().unwrap();
        assert_eq!(l.dtr, [false]);
        assert_eq!(l.rts, [false]);
    }

    /// 端口归传输所有：传输丢掉时没有别的句柄还开着端口。
    #[test]
    fn dropping_transport_closes_port() {
        let (t, lines) = transport();
        assert_eq!(Arc::strong_count(&lines), 2);
        drop(t);
        assert_eq!(Arc::strong_count(&lines), 1);
    }

    /// 协议文档第 1 节：请求是 dap_req 帧；响应帧拆成几次读到也能拼起来。
    #[test]
    fn frames_requests_and_reassembles_responses() {
        let (mut t, lines) = transport();
        t.send(&[0x00, 0xFF]).unwrap();
        let frame = build_frame(
            crc32(RESP_TOPIC),
            &serial_payload(&[0x00, 0x02, 0x00, 0x04]),
            7,
        );
        {
            let mut l = lines.lock().unwrap();
            assert_eq!(l.tx, serial_request_frame(&[0x00, 0xFF]));
            let (a, b) = frame.split_at(5);
            l.rx.push_back(a.to_vec());
            l.rx.push_back(b.to_vec());
        }
        assert_eq!(
            t.recv(Duration::from_secs(1)).unwrap(),
            [0x00, 0x02, 0x00, 0x04]
        );
    }
}
