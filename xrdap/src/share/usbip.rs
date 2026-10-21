//! USB/IP 服务端：把探针模拟成一个 USB CMSIS-DAP v2 设备。
//!
//! 虚拟设备：VID:PID 1D50:6199、bcdDevice 0x0200（与 S3 真 USB 设备的 0x0100 区分，Windows 按
//! 这三项缓存描述符），一个厂商类接口（接口名 `CMSIS-DAP v2`），Bulk OUT 0x01、Bulk IN 0x81，
//! 高速 512 字节；BOS 里带 MS OS 2.0 描述符，Windows 自动绑定 WinUSB。与固件
//! `firmware/common/net/usbip_device.hpp` 是同一个设备模型。
//!
//! 协议：`OP_REQ_DEVLIST`、`OP_REQ_IMPORT` 之后是 `USBIP_CMD_SUBMIT`/`RET_SUBMIT`、
//! `CMD_UNLINK`/`RET_UNLINK`，字段大端。见 Linux 内核文档 `usb/usbip_protocol.rst`。

use std::collections::VecDeque;
use std::io;

use super::link::{bad, Link};

const VERSION: u16 = 0x0111;
const OP_REQ_DEVLIST: u16 = 0x8005;
const OP_REP_DEVLIST: u16 = 0x0005;
const OP_REQ_IMPORT: u16 = 0x8003;
const OP_REP_IMPORT: u16 = 0x0003;
const CMD_SUBMIT: u32 = 1;
const CMD_UNLINK: u32 = 2;
const RET_SUBMIT: u32 = 3;
const RET_UNLINK: u32 = 4;
const DIR_IN: u32 = 1;
const SPEED_HIGH: u32 = 3;
const BUS_ID: &str = "1-1";
const BUS_NUM: u32 = 1;
const DEV_NUM: u32 = 1;
const EPIPE: i32 = -32;
const ECONNRESET: i32 = -104;

const VID: u16 = 0x1D50;
const PID: u16 = 0x6199;
const BCD_DEVICE: u16 = 0x0200;
const EP_SIZE: u16 = 512;
const MS_VENDOR_CODE: u8 = 0x20;
const DAP_EP: u32 = 1;

/// 虚拟设备的产品名和序列号。
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// USB 产品名，例如 `XR-DAP-C3 CMSIS-DAP`（带 "CMSIS-DAP"，工具按它识别）
    pub product: String,
    pub serial: String,
}

impl DeviceInfo {
    /// `product` 是探针的产品名（例如 XR-DAP-C3），后面自动加上 " CMSIS-DAP"。
    pub(crate) fn new(product: &str, serial: &str) -> Self {
        Self {
            product: format!("{product} CMSIS-DAP"),
            serial: serial.to_string(),
        }
    }
}

// ------------------------------------------------------------------ 描述符

fn device_descriptor() -> [u8; 18] {
    let [vl, vh] = VID.to_le_bytes();
    let [pl, ph] = PID.to_le_bytes();
    let [bl, bh] = BCD_DEVICE.to_le_bytes();
    [
        18, 1, 0x10, 0x02, // bcdUSB 2.10：有 BOS
        0, 0, 0, 64, vl, vh, pl, ph, bl, bh, 1, 2, 3, 1,
    ]
}

fn config_descriptor() -> Vec<u8> {
    let [ml, mh] = EP_SIZE.to_le_bytes();
    vec![
        9, 2, 32, 0, 1, 1, 0, 0x80,
        50, // 配置：总长 32，一个接口，总线供电 100 mA
        9, 4, 0, 0, 2, 0xFF, 0, 0,
        4, // 接口：厂商类，两个端点，接口名是字符串 4
        7, 5, 0x01, 2, ml, mh, 0, // Bulk OUT
        7, 5, 0x81, 2, ml, mh, 0, // Bulk IN
    ]
}

fn qualifier_descriptor() -> [u8; 10] {
    [10, 6, 0x00, 0x02, 0, 0, 0, 64, 1, 0]
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn string_descriptor(s: &str) -> Vec<u8> {
    let body = utf16(s);
    let mut d = vec![(2 + body.len()) as u8, 3];
    d.extend(body);
    d
}

/// MS OS 2.0 描述符集：兼容 ID WINUSB，设备接口 GUID 为 CMSIS-DAP v2 约定的值。
fn ms_os_20_set() -> Vec<u8> {
    let name = utf16("DeviceInterfaceGUIDs\0");
    let data = utf16("{CDB3B5AD-293B-4663-AA36-1AAE46463776}\0\0");
    let prop_len = 10 + name.len() + data.len();
    let total = 10 + 20 + prop_len;
    let mut d = Vec::with_capacity(total);
    // 描述符集头：Windows 8.1（0x06030000）起
    d.extend_from_slice(&[10, 0, 0, 0, 0x00, 0x00, 0x03, 0x06]);
    d.extend_from_slice(&(total as u16).to_le_bytes());
    // 兼容 ID
    d.extend_from_slice(&[20, 0, 3, 0]);
    d.extend_from_slice(b"WINUSB\0\0");
    d.extend_from_slice(&[0; 8]);
    // 注册表属性：REG_MULTI_SZ
    d.extend_from_slice(&(prop_len as u16).to_le_bytes());
    d.extend_from_slice(&[4, 0, 7, 0]);
    d.extend_from_slice(&(name.len() as u16).to_le_bytes());
    d.extend(name);
    d.extend_from_slice(&(data.len() as u16).to_le_bytes());
    d.extend(data);
    d
}

fn bos_descriptor() -> Vec<u8> {
    let set_len = ms_os_20_set().len() as u16;
    let mut d = vec![5, 0x0F, 33, 0, 1];
    // 平台能力描述符，UUID D8DD60DF-4589-4CC7-9CD2-659D9E648A9F（MS OS 2.0）
    d.extend_from_slice(&[28, 0x10, 5, 0]);
    d.extend_from_slice(&[
        0xDF, 0x60, 0xDD, 0xD8, 0x89, 0x45, 0xC7, 0x4C, 0x9C, 0xD2, 0x65, 0x9D, 0x9E, 0x64, 0x8A,
        0x9F,
    ]);
    d.extend_from_slice(&[0x00, 0x00, 0x03, 0x06]);
    d.extend_from_slice(&set_len.to_le_bytes());
    d.extend_from_slice(&[MS_VENDOR_CODE, 0]);
    d
}

/// 控制传输：返回数据（IN）或空（OUT）；不支持的请求返回 None（STALL）。
fn control(info: &DeviceInfo, setup: &[u8; 8]) -> Option<Vec<u8>> {
    let (req_type, req) = (setup[0], setup[1]);
    let value = u16::from_le_bytes([setup[2], setup[3]]);
    let index = u16::from_le_bytes([setup[4], setup[5]]);
    let data = match (req_type, req) {
        (0x80, 0x06) => match (value >> 8, value & 0xFF) {
            (1, _) => device_descriptor().to_vec(),
            (2, _) => config_descriptor(),
            (6, _) => qualifier_descriptor().to_vec(),
            (0x0F, _) => bos_descriptor(),
            (3, 0) => vec![4, 3, 0x09, 0x04],
            (3, 1) => string_descriptor("XRobot"),
            (3, 2) => string_descriptor(&info.product),
            (3, 3) => string_descriptor(&info.serial),
            (3, 4) => string_descriptor("CMSIS-DAP v2"),
            _ => return None,
        },
        (0xC0, MS_VENDOR_CODE) if index == 7 => ms_os_20_set(),
        (0x80..=0x82, 0x00) => vec![0, 0], // GET_STATUS
        (0x80, 0x08) => vec![1],           // GET_CONFIGURATION
        (0x81, 0x0A) => vec![0],           // GET_INTERFACE
        (0x00, 0x09) | (0x01, 0x0B) | (0x02, 0x01) | (0x02, 0x03) | (0x00, 0x05) => Vec::new(),
        _ => return None,
    };
    Some(data)
}

// ------------------------------------------------------------------ 协议

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn be_u32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// `usbip_usb_device`：312 字节。
fn usb_device(out: &mut Vec<u8>) {
    let mut path = format!("/sys/devices/xrdap/usb{BUS_NUM}/{BUS_ID}").into_bytes();
    path.resize(256, 0);
    out.extend(path);
    let mut busid = BUS_ID.as_bytes().to_vec();
    busid.resize(32, 0);
    out.extend(busid);
    put_u32(out, BUS_NUM);
    put_u32(out, DEV_NUM);
    put_u32(out, SPEED_HIGH);
    put_u16(out, VID);
    put_u16(out, PID);
    put_u16(out, BCD_DEVICE);
    // 设备类、子类、协议、当前配置、配置数、接口数
    out.extend_from_slice(&[0, 0, 0, 1, 1, 1]);
}

fn op_header(out: &mut Vec<u8>, code: u16, status: u32) {
    put_u16(out, VERSION);
    put_u16(out, code);
    put_u32(out, status);
}

fn ret_header(cmd: u32, seqnum: u32, status: i32, actual: usize) -> Vec<u8> {
    let mut h = Vec::with_capacity(48);
    put_u32(&mut h, cmd);
    put_u32(&mut h, seqnum);
    // RET 里的 devid、direction、ep 按协议填 0
    h.extend_from_slice(&[0; 12]);
    put_u32(&mut h, status as u32);
    put_u32(&mut h, actual as u32);
    h.extend_from_slice(&[0; 20]); // start_frame、number_of_packets、error_count、setup
    h
}

/// 一个挂载会话：处理一个客户端从 DEVLIST/IMPORT 到断开。
struct Session<'a> {
    info: &'a DeviceInfo,
    /// 等数据的 Bulk IN 请求：(seqnum, 长度)
    pending_in: VecDeque<(u32, usize)>,
    /// 还没被 IN 取走的 DAP 响应
    responses: VecDeque<Vec<u8>>,
    /// 待发的回复
    tx: Vec<u8>,
}

impl Session<'_> {
    /// 把排队的响应交给等着的 IN 请求。
    fn pair(&mut self) {
        while !self.pending_in.is_empty() && !self.responses.is_empty() {
            if let (Some((seq, len)), Some(mut data)) =
                (self.pending_in.pop_front(), self.responses.pop_front())
            {
                data.truncate(len);
                self.reply(RET_SUBMIT, seq, 0, data.len(), &data);
            }
        }
    }

    fn reply(&mut self, cmd: u32, seq: u32, status: i32, actual: usize, data: &[u8]) {
        self.tx.extend(ret_header(cmd, seq, status, actual));
        self.tx.extend_from_slice(data);
    }

    /// 一个命令引起的回复（OUT 的回复和配上的 IN 响应）合成一次写。分两次写时第二段要等
    /// 对方的延迟 ACK，每个来回多出约 40 ms。
    fn flush(&mut self, s: &mut Link) -> io::Result<()> {
        if self.tx.is_empty() {
            return Ok(());
        }
        let r = s.write_all(&self.tx);
        self.tx.clear();
        r
    }

    fn submit(&mut self, s: &mut Link, hdr: &[u8; 48], exec: &mut super::Exec) -> io::Result<()> {
        let (seq, dir, ep) = (be_u32(hdr, 4), be_u32(hdr, 12), be_u32(hdr, 16));
        let len = be_u32(hdr, 24) as usize;
        let mut setup = [0u8; 8];
        setup.copy_from_slice(&hdr[40..48]);
        let mut out_data = vec![0u8; if dir == DIR_IN { 0 } else { len }];
        s.read_full(&mut out_data)?;
        match (ep, dir) {
            (0, _) => match control(self.info, &setup) {
                Some(mut data) => {
                    if dir != DIR_IN {
                        data.clear();
                    }
                    data.truncate(len);
                    self.reply(RET_SUBMIT, seq, 0, data.len(), &data);
                }
                None => self.reply(RET_SUBMIT, seq, EPIPE, 0, &[]),
            },
            (DAP_EP, DIR_IN) => {
                self.pending_in.push_back((seq, len));
                self.pair();
            }
            (DAP_EP, _) => {
                let resp = exec(&out_data)?;
                // OUT 的回复只带写入的字节数，不带数据
                self.reply(RET_SUBMIT, seq, 0, out_data.len(), &[]);
                // 没有应答的命令（DAP_TransferAbort）不占 IN
                if !resp.is_empty() {
                    self.responses.push_back(resp);
                }
                self.pair();
            }
            _ => self.reply(RET_SUBMIT, seq, EPIPE, 0, &[]),
        }
        Ok(())
    }

    fn unlink(&mut self, hdr: &[u8; 48]) {
        let (seq, victim) = (be_u32(hdr, 4), be_u32(hdr, 20));
        let before = self.pending_in.len();
        self.pending_in.retain(|(q, _)| *q != victim);
        let status = if self.pending_in.len() < before {
            ECONNRESET
        } else {
            0
        };
        self.reply(RET_UNLINK, seq, status, 0, &[]);
    }
}

/// 处理一个客户端连接，直到对方断开（返回 Ok）或出错。设备列表查询（`usbip list -r`）
/// 回完列表就返回。
pub(super) fn serve(s: &mut Link, info: &DeviceInfo, exec: &mut super::Exec) -> io::Result<()> {
    let mut op = [0u8; 8];
    s.read_full(&mut op)?;
    let code = u16::from_be_bytes([op[2], op[3]]);
    let mut reply = Vec::new();
    match code {
        OP_REQ_DEVLIST => {
            op_header(&mut reply, OP_REP_DEVLIST, 0);
            put_u32(&mut reply, 1);
            usb_device(&mut reply);
            reply.extend_from_slice(&[0xFF, 0, 0, 0]); // 接口：厂商类
            return s.write_all(&reply);
        }
        OP_REQ_IMPORT => {
            let mut busid = [0u8; 32];
            s.read_full(&mut busid)?;
            let wanted = busid.split(|&b| b == 0).next().unwrap_or(&[]);
            if wanted != BUS_ID.as_bytes() {
                op_header(&mut reply, OP_REP_IMPORT, 1);
                return s.write_all(&reply);
            }
            op_header(&mut reply, OP_REP_IMPORT, 0);
            usb_device(&mut reply);
            s.write_all(&reply)?;
        }
        other => return Err(bad(format!("USB/IP：未知操作 0x{other:04X}"))),
    }
    let mut session = Session {
        info,
        pending_in: VecDeque::new(),
        responses: VecDeque::new(),
        tx: Vec::new(),
    };
    let mut hdr = [0u8; 48];
    loop {
        match s.read_full(&mut hdr) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        }
        match be_u32(&hdr, 0) {
            CMD_SUBMIT => session.submit(s, &hdr, exec)?,
            CMD_UNLINK => session.unlink(&hdr),
            other => return Err(bad(format!("USB/IP：未知命令 {other}"))),
        }
        session.flush(s)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::AtomicBool;
    use std::thread;

    fn submit(
        s: &mut TcpStream,
        seq: u32,
        dir: u32,
        ep: u32,
        len: u32,
        setup: [u8; 8],
        data: &[u8],
    ) {
        let mut h = Vec::new();
        for v in [CMD_SUBMIT, seq, 0x0001_0001, dir, ep, 0, len, 0, 0, 0] {
            put_u32(&mut h, v);
        }
        h.extend_from_slice(&setup);
        h.extend_from_slice(data);
        s.write_all(&h).unwrap();
    }

    /// 读一个 RET；OUT 和 UNLINK 的回复不带数据（OUT 的 actual_length 是写入的字节数）。
    fn ret_with(s: &mut TcpStream, has_data: bool) -> (u32, u32, i32, Vec<u8>) {
        let mut h = [0u8; 48];
        s.read_exact(&mut h).unwrap();
        let actual = be_u32(&h, 24) as usize;
        let mut data = vec![0; if has_data { actual } else { 0 }];
        s.read_exact(&mut data).unwrap();
        (be_u32(&h, 0), be_u32(&h, 4), be_u32(&h, 20) as i32, data)
    }

    fn ret(s: &mut TcpStream) -> (u32, u32, i32, Vec<u8>) {
        ret_with(s, true)
    }

    fn get_desc(ty: u8, idx: u8, len: u16) -> [u8; 8] {
        let [l, h] = len.to_le_bytes();
        [0x80, 6, idx, ty, 0, 0, l, h]
    }

    #[test]
    fn devlist_import_control_and_bulk() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let info = DeviceInfo::new("XR-DAP-C3", "XRobot-XRDAP-1");
            // 两个连接：一次 DEVLIST，一次 IMPORT
            for _ in 0..2 {
                let (s, _) = listener.accept().unwrap();
                let stop = AtomicBool::new(false);
                let mut link = Link::new(s, &stop).unwrap();
                serve(&mut link, &info, &mut |req| {
                    let mut r = req.to_vec();
                    r.push(0xAA);
                    Ok(r)
                })
                .unwrap();
            }
        });

        let mut c = TcpStream::connect(addr).unwrap();
        c.write_all(&[0x01, 0x11, 0x80, 0x05, 0, 0, 0, 0]).unwrap();
        let mut rep = vec![0u8; 8 + 4 + 312 + 4];
        c.read_exact(&mut rep).unwrap();
        assert_eq!(&rep[2..4], &[0x00, 0x05]);
        assert_eq!(be_u32(&rep, 8), 1);
        assert_eq!(&rep[12 + 256..12 + 259], b"1-1");

        let mut c = TcpStream::connect(addr).unwrap();
        let mut req = vec![0x01, 0x11, 0x80, 0x03, 0, 0, 0, 0];
        let mut busid = b"1-1".to_vec();
        busid.resize(32, 0);
        req.extend(busid);
        c.write_all(&req).unwrap();
        let mut rep = vec![0u8; 8 + 312];
        c.read_exact(&mut rep).unwrap();
        assert_eq!(be_u32(&rep, 4), 0);

        // 设备描述符、BOS、MS OS 2.0 描述符集、产品名
        submit(&mut c, 1, DIR_IN, 0, 18, get_desc(1, 0, 18), &[]);
        let (_, seq, st, d) = ret(&mut c);
        assert_eq!((seq, st), (1, 0));
        assert_eq!(&d[8..12], &[0x50, 0x1D, 0x99, 0x61]);
        submit(&mut c, 2, DIR_IN, 0, 255, get_desc(0x0F, 0, 255), &[]);
        let (_, _, _, bos) = ret(&mut c);
        assert_eq!(bos.len(), 33);
        let set_len = u16::from_le_bytes([bos[29], bos[30]]);
        submit(
            &mut c,
            3,
            DIR_IN,
            0,
            set_len as u32,
            [0xC0, MS_VENDOR_CODE, 0, 0, 7, 0, 0xFF, 0],
            &[],
        );
        let (_, _, _, set) = ret(&mut c);
        assert_eq!(set.len(), set_len as usize);
        assert_eq!(&set[14..20], b"WINUSB");
        submit(&mut c, 4, DIR_IN, 0, 255, get_desc(3, 2, 255), &[]);
        let (_, _, _, name) = ret(&mut c);
        assert_eq!(name[2..].len(), "XR-DAP-C3 CMSIS-DAP".len() * 2);
        // 不支持的请求：STALL
        submit(&mut c, 5, DIR_IN, 0, 8, [0xA1, 0x01, 0, 0, 0, 0, 8, 0], &[]);
        assert_eq!(ret(&mut c).2, EPIPE);

        // Bulk：IN 先到、OUT 后到，响应配给先到的 IN
        submit(&mut c, 10, DIR_IN, 1, 512, [0; 8], &[]);
        submit(&mut c, 11, DIR_IN, 1, 512, [0; 8], &[]);
        submit(&mut c, 12, 0, 1, 2, [0; 8], &[0x00, 0x04]);
        let out = ret_with(&mut c, false);
        let inn = ret(&mut c);
        assert_eq!((out.1, out.2), (12, 0));
        assert_eq!((inn.1, inn.3), (10, vec![0x00, 0x04, 0xAA]));
        // 取消还在等的 IN 11
        let mut h = Vec::new();
        for v in [CMD_UNLINK, 13, 0x0001_0001, 0, 1, 11, 0, 0, 0, 0] {
            put_u32(&mut h, v);
        }
        h.extend_from_slice(&[0; 8]);
        c.write_all(&h).unwrap();
        let (cmd, seq, st, _) = ret_with(&mut c, false);
        assert_eq!((cmd, seq, st), (RET_UNLINK, 13, ECONNRESET));
        drop(c);
        server.join().unwrap();
    }

    #[test]
    fn ms_os_lengths_consistent() {
        let set = ms_os_20_set();
        assert_eq!(u16::from_le_bytes([set[8], set[9]]) as usize, set.len());
        assert_eq!(set.len(), 162);
        assert_eq!(config_descriptor().len(), 32);
    }
}
