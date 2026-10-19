//! share：把探针共享成 USB/IP 设备或 OpenOCD 的 TCP 后端，直到 Ctrl-C。

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use xrdap::dap::Probe;
use xrdap::share::{Event, Protocol, Share};

use super::{CliResult, ShareKind};

/// 多久看一次 Ctrl-C
const STOP_POLL: Duration = Duration::from_millis(100);

pub fn cmd_share(
    probe: Probe,
    kind: ShareKind,
    listen: Option<&str>,
    out: &mut dyn Write,
    stop: &AtomicBool,
) -> CliResult {
    let protocol = match kind {
        ShareKind::Usbip => Protocol::Usbip,
        ShareKind::Openocd => Protocol::OpenOcd,
    };
    let listen = listen
        .map(str::to_string)
        .unwrap_or_else(|| format!("127.0.0.1:{}", protocol.default_port()));
    let (tx, rx) = mpsc::channel();
    let share = Share::start(probe, protocol, &listen, move |e| {
        let _ = tx.send(e);
    })
    .map_err(|(_, e)| e)?;

    let device = share.device();
    writeln!(out, "探针：{}，序列号 {}", device.product, device.serial)?;
    match protocol {
        Protocol::Usbip => writeln!(out, "挂载：{}", share.attach_hint())?,
        Protocol::OpenOcd => writeln!(out, "OpenOCD：{}", share.attach_hint())?,
    }
    writeln!(out, "按 Ctrl-C 停止")?;
    while !stop.load(Ordering::SeqCst) {
        match rx.recv_timeout(STOP_POLL) {
            Ok(Event::Client(peer)) => writeln!(out, "{peer} 已连接")?,
            Ok(Event::Idle(None)) => writeln!(out, "已断开")?,
            Ok(Event::Idle(Some(why))) => writeln!(out, "连接异常断开：{why}")?,
            Err(_) => {}
        }
    }
    let _probe = share.stop();
    writeln!(out, "已停止")?;
    Ok(())
}
