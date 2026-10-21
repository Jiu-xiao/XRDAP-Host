//! 后台线程：独占探针，执行界面发来的命令，把结果和采样帧送回界面。
//! 界面线程不直接碰探针，USB/串口/TCP 的阻塞读写都在这里。

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use xrdap::addr::{Addr, Spec};
use xrdap::cortex::{self, AfterReset, CoreStatus};
use xrdap::dap::Probe;
use xrdap::expr::Target;
use xrdap::open::{self, OpenOptions};
use xrdap::vendor::wifi::WifiStatus;
use xrdap::vendor::{self, Item, ItemResult, SubItem};
use xrdap::watch::{HostFrame, Sampler, StartError, Stats};

use xrdap::share::{Event as ShareEvent, Protocol, Share};

/// 共享为 USB DAP 时的监听地址：只给本机用
const SHARE_LISTEN: &str = "127.0.0.1:3240";

/// 连哪块探针：USB、串口或 TCP；`baud` 只对串口有用
#[derive(Debug, Clone)]
pub struct Conn {
    pub addr: Addr,
    pub baud: Option<u32>,
}

pub enum Cmd {
    Connect(Conn, u32),
    Disconnect,
    /// 读一次，tag 原样带回
    Read(u64, Vec<Target>),
    Write(Target, Vec<u8>),
    Start {
        targets: Vec<Target>,
        divs: Vec<u16>,
        period_us: u32,
        stop_on_full: bool,
    },
    Stop,
    /// 读内核状态；界面每隔一段时间发一次
    PollCore,
    Core(CoreCmd),
    /// 开始或停止共享为 USB DAP（USB/IP）。共享期间别的命令都不执行
    Share(bool),
    /// 查询探针的 Wi-Fi 状态
    WifiStatus,
    /// 保存或清除热点参数；成功后探针约 0.5 秒重启，连接随之断开
    WifiApply(WifiChange),
}

#[derive(Debug, Clone)]
pub enum WifiChange {
    Set { ssid: String, password: String },
    Clear,
}

#[derive(Debug, Clone, Copy)]
pub enum CoreCmd {
    Halt,
    Resume,
    Reset(AfterReset),
}

pub enum Event {
    Connected {
        probe: String,
        target: String,
    },
    Disconnected,
    Error(String),
    ReadDone(u64, Vec<ItemResult>),
    WriteDone {
        name: String,
        ok: bool,
        text: String,
    },
    Started {
        est_frame_us: u32,
        phases: Vec<(u16, u16)>,
    },
    Rejected {
        est_frame_us: u32,
    },
    Frames(Vec<HostFrame>),
    Stats(Stats),
    Stopped,
    /// 内核型号（连接后读一次）
    CoreInfo(String),
    /// 内核状态；cmd 为 Some 时是停住、继续、复位的结果
    Core {
        cmd: Option<CoreCmd>,
        status: Result<CoreStatus, String>,
    },
    /// 开始共享：挂载命令和虚拟 USB 设备的名字
    Sharing {
        attach: String,
        device: String,
    },
    /// 共享中客户端连上或断开
    Share(ShareEvent),
    /// 共享结束，探针交回界面
    ShareStopped,
    /// Wi-Fi 查询的结果；Err 是探针不支持或读取失败的原因
    WifiStatus(Result<WifiStatus, String>),
    /// 保存或清除热点参数成功，探针正在重启（随后还有一个 Disconnected）
    WifiApplied {
        cleared: bool,
    },
}

const TIMEOUT: Duration = Duration::from_millis(2000);

fn open(conn: &Conn) -> Result<Probe, String> {
    let opts = OpenOptions {
        timeout: TIMEOUT,
        baud: conn.baud,
        ..OpenOptions::default()
    };
    open::open(&Spec::Addr(conn.addr.clone()), &opts).map_err(|e| e.to_string())
}

fn items_of(targets: &[Target]) -> Vec<Item> {
    targets
        .iter()
        .enumerate()
        .map(|(i, t)| t.item(i as u16))
        .collect()
}

/// 写一个变量并读回一次。
fn write_target(probe: &mut Probe, target: &Target, bytes: &[u8]) -> Event {
    let (ok, text) = match target.write(probe, bytes) {
        Ok(()) => match target.read(probe) {
            Ok(v) => (true, format!("读回 {}", target.ty.format(&v))),
            Err(e) => (true, format!("读回失败：{e}")),
        },
        Err(xrdap::Error::Item { status, .. }) => {
            (false, vendor::item_status_text(status, &target.chain))
        }
        Err(e) => (false, e.to_string()),
    };
    Event::WriteDone {
        name: target.name.clone(),
        ok,
        text,
    }
}

fn core_cmd(probe: &mut Probe, cmd: CoreCmd) -> Event {
    let status = match cmd {
        CoreCmd::Halt => cortex::halt(probe),
        CoreCmd::Resume => cortex::resume(probe),
        CoreCmd::Reset(after) => cortex::reset(probe, after),
    };
    Event::Core {
        cmd: Some(cmd),
        status: status.map_err(|e| e.to_string()),
    }
}

fn wifi_status(probe: &mut Probe) -> Event {
    Event::WifiStatus(probe.wifi_status().map_err(|e| e.to_string()))
}

/// 保存或清除热点参数；成功时返回 true，探针随后重启。
fn wifi_apply(probe: &mut Probe, change: &WifiChange, send: &impl Fn(Event)) -> bool {
    let (result, cleared) = match change {
        WifiChange::Set { ssid, password } => (probe.wifi_set(ssid, password), false),
        WifiChange::Clear => (probe.wifi_clear(), true),
    };
    match result {
        Ok(()) => {
            send(Event::WifiApplied { cleared });
            true
        }
        Err(e) => {
            send(Event::Error(format!("配置 Wi-Fi 失败：{e}")));
            false
        }
    }
}

fn poll_core(probe: &mut Probe) -> Event {
    Event::Core {
        cmd: None,
        status: cortex::status(probe).map_err(|e| e.to_string()),
    }
}

pub fn run(rx: Receiver<Cmd>, tx: Sender<Event>, wake: impl Fn() + Clone + Send + 'static) {
    // 采样帧和统计不唤醒界面：界面采样时自己按固定帧率刷新
    let share_notify = {
        let (tx, wake) = (tx.clone(), wake.clone());
        move |ev: ShareEvent| {
            let _ = tx.send(Event::Share(ev));
            wake();
        }
    };
    let send = |ev: Event| {
        let quiet = matches!(
            ev,
            Event::Frames(_)
                | Event::Stats(_)
                | Event::Core {
                    cmd: None,
                    status: Ok(_)
                }
        );
        let _ = tx.send(ev);
        if !quiet {
            wake();
        }
    };
    let mut probe: Option<Probe> = None;
    let mut share: Option<Share> = None;
    let mut clock = 0u32;
    let mut pending: Option<Cmd> = None;
    loop {
        let cmd = match pending.take() {
            Some(c) => c,
            None => match rx.recv() {
                Ok(c) => c,
                Err(_) => return,
            },
        };
        if let Some(s) = share.take() {
            match cmd {
                Cmd::Share(true) => {
                    share = Some(s);
                    continue;
                }
                // 停止共享：探针交回来，重新连接目标（外部工具可能改过 SWD 时钟、断开过目标）
                Cmd::Share(false) => {
                    let mut p = s.stop();
                    send(Event::ShareStopped);
                    if let Err(e) = xrdap::watch::connect(&mut p, clock) {
                        send(Event::Error(format!("重新连接目标失败：{e}")));
                    }
                    send(poll_core(&mut p));
                    probe = Some(p);
                    continue;
                }
                // 连接别的探针或断开：先停掉共享，交回来的探针随之关闭
                Cmd::Connect(..) | Cmd::Disconnect => {
                    drop(s.stop());
                    send(Event::ShareStopped);
                }
                Cmd::PollCore => {
                    share = Some(s);
                    continue;
                }
                _ => {
                    share = Some(s);
                    send(Event::Error("正在共享为 USB DAP，先停止共享".into()));
                    continue;
                }
            }
        }
        match cmd {
            Cmd::Share(false) => {}
            Cmd::Share(true) => match probe.take() {
                Some(p) => {
                    match Share::start(p, Protocol::Usbip, SHARE_LISTEN, share_notify.clone()) {
                        Ok(s) => {
                            send(Event::Sharing {
                                attach: s.attach_hint(),
                                device: format!("{}（{}）", s.device().product, s.device().serial),
                            });
                            share = Some(s);
                        }
                        Err((p, e)) => {
                            probe = Some(p);
                            send(Event::Error(e.to_string()));
                        }
                    }
                }
                None => send(Event::Error("还没有连接".into())),
            },
            Cmd::Connect(conn, c) => {
                probe = None;
                clock = c;
                match open(&conn).and_then(|mut p| {
                    let t = xrdap::watch::connect(&mut p, c)
                        .map_err(|e| format!("连接目标失败：{e}"))?;
                    Ok((p, t))
                }) {
                    Ok((mut p, t)) => {
                        send(Event::Connected {
                            probe: p.describe(),
                            target: t.describe(),
                        });
                        match cortex::info(&mut p) {
                            Ok(i) => send(Event::CoreInfo(i.describe())),
                            Err(e) => send(Event::CoreInfo(format!("读不到 CPUID（{e}）"))),
                        }
                        send(poll_core(&mut p));
                        send(wifi_status(&mut p));
                        probe = Some(p);
                    }
                    Err(e) => send(Event::Error(e)),
                }
            }
            Cmd::Disconnect => {
                probe = None;
                send(Event::Disconnected);
            }
            Cmd::Read(tag, targets) => match probe.as_mut() {
                Some(p) => match p.read(&items_of(&targets)) {
                    Ok(r) => send(Event::ReadDone(tag, r)),
                    Err(e) => send(Event::Error(format!("读取失败：{e}"))),
                },
                None => send(Event::Error("还没有连接".into())),
            },
            Cmd::Write(target, bytes) => match probe.as_mut() {
                Some(p) => send(write_target(p, &target, &bytes)),
                None => send(Event::Error("还没有连接".into())),
            },
            Cmd::Stop => send(Event::Stopped),
            Cmd::PollCore => {
                if let Some(p) = probe.as_mut() {
                    send(poll_core(p));
                }
            }
            Cmd::Core(c) => match probe.as_mut() {
                Some(p) => send(core_cmd(p, c)),
                None => send(Event::Error("还没有连接".into())),
            },
            Cmd::WifiStatus => match probe.as_mut() {
                Some(p) => send(wifi_status(p)),
                None => send(Event::Error("还没有连接".into())),
            },
            Cmd::WifiApply(change) => match probe.as_mut() {
                Some(p) => {
                    // 探针马上重启，这个连接不能再用
                    if wifi_apply(p, &change, &send) {
                        probe = None;
                        send(Event::Disconnected);
                    }
                }
                None => send(Event::Error("还没有连接".into())),
            },
            Cmd::Start {
                targets,
                divs,
                period_us,
                stop_on_full,
            } => {
                let Some(p) = probe.as_mut() else {
                    send(Event::Error("还没有连接".into()));
                    continue;
                };
                let mut items: Vec<SubItem> = targets
                    .iter()
                    .zip(&divs)
                    .enumerate()
                    .map(|(i, (t, d))| SubItem {
                        item: t.item(i as u16),
                        div: *d,
                        phase: 0,
                    })
                    .collect();
                let fixed = vec![false; items.len()];
                vendor::assign_phases(&mut items, &fixed);
                let ring = p.vendor_info().map(|i| i.ring_bytes).unwrap_or(16384);
                let mut sampler = match Sampler::start(p, items, period_us, stop_on_full) {
                    Ok(s) => s,
                    Err(StartError::Rejected(r)) => {
                        send(Event::Rejected {
                            est_frame_us: r.est_frame_us.unwrap_or(0),
                        });
                        continue;
                    }
                    Err(StartError::Other(e)) => {
                        send(Event::Error(format!("开始采样失败：{e}")));
                        continue;
                    }
                };
                send(Event::Started {
                    est_frame_us: sampler.setup.est_frame_us.unwrap_or(0),
                    phases: sampler.items().iter().map(|s| (s.div, s.phase)).collect(),
                });
                let interval = sampler.poll_interval(ring).min(Duration::from_millis(20));
                let mut last_stats = std::time::Instant::now();
                let err = loop {
                    match rx.try_recv() {
                        Ok(Cmd::Write(t, b)) => send(write_target(sampler.probe_mut(), &t, &b)),
                        Ok(Cmd::Read(tag, targets)) => {
                            match sampler.probe_mut().read(&items_of(&targets)) {
                                Ok(r) => send(Event::ReadDone(tag, r)),
                                Err(e) => send(Event::Error(format!("读取失败：{e}"))),
                            }
                        }
                        Ok(Cmd::PollCore) => send(poll_core(sampler.probe_mut())),
                        Ok(Cmd::Core(c)) => send(core_cmd(sampler.probe_mut(), c)),
                        Ok(Cmd::WifiStatus) => send(wifi_status(sampler.probe_mut())),
                        Ok(Cmd::Stop) => break None,
                        Ok(other) => {
                            // 连接、断开、重新开始、改 Wi-Fi：先停掉这次采样再处理
                            pending = Some(other);
                            break None;
                        }
                        Err(TryRecvError::Empty) => {}
                        Err(TryRecvError::Disconnected) => return,
                    }
                    match sampler.poll() {
                        Ok((frames, remaining)) => {
                            if !frames.is_empty() {
                                send(Event::Frames(frames));
                            }
                            if last_stats.elapsed() >= Duration::from_millis(250) {
                                last_stats = std::time::Instant::now();
                                send(Event::Stats(sampler.stats()));
                            }
                            if remaining == 0 {
                                std::thread::sleep(interval);
                            }
                        }
                        Err(e) => break Some(format!("采样中断：{e}")),
                    }
                };
                match sampler.stop() {
                    Ok(rest) if !rest.is_empty() => send(Event::Frames(rest)),
                    _ => {}
                }
                send(Event::Stats(sampler.stats()));
                drop(sampler);
                if let Some(e) = err {
                    send(Event::Error(e));
                }
                send(Event::Stopped);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    /// 连到一个新的模拟探针（TCP）。
    fn sim_conn() -> Conn {
        Conn {
            addr: Addr::Tcp {
                host: xrdap_sim::Sim::new().serve_tcp().to_string(),
            },
            baud: None,
        }
    }

    fn next(rx: &Receiver<Event>, want: impl Fn(&Event) -> bool) -> Event {
        loop {
            let ev = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("没有等到事件");
            if want(&ev) {
                return ev;
            }
        }
    }

    #[test]
    fn share_blocks_other_commands_and_gives_probe_back() {
        let (cmd_tx, cmd_rx) = channel();
        let (ev_tx, ev_rx) = channel();
        let worker = std::thread::spawn(move || run(cmd_rx, ev_tx, || {}));

        cmd_tx.send(Cmd::Connect(sim_conn(), 4_000_000)).unwrap();
        next(&ev_rx, |e| matches!(e, Event::Connected { .. }));

        cmd_tx.send(Cmd::Share(true)).unwrap();
        // 3240 被别的程序占着时共享失败，探针留在后台线程，照样可以停
        let started = next(&ev_rx, |e| {
            matches!(e, Event::Sharing { .. } | Event::Error(_))
        });
        if let Event::Sharing { attach, .. } = &started {
            assert!(attach.starts_with("usbip attach -r 127.0.0.1"));
            cmd_tx.send(Cmd::PollCore).unwrap();
            cmd_tx.send(Cmd::Core(CoreCmd::Halt)).unwrap();
            next(
                &ev_rx,
                |e| matches!(e, Event::Error(m) if m.contains("共享")),
            );
            cmd_tx.send(Cmd::Share(false)).unwrap();
            next(&ev_rx, |e| matches!(e, Event::ShareStopped));
        }
        // 探针回到后台线程：内核命令能执行
        cmd_tx.send(Cmd::Core(CoreCmd::Halt)).unwrap();
        next(&ev_rx, |e| matches!(e, Event::Core { cmd: Some(_), .. }));

        drop(cmd_tx);
        worker.join().unwrap();
    }

    #[test]
    fn wifi_status_after_connect_and_failed_set_keeps_connection() {
        let (cmd_tx, cmd_rx) = channel();
        let (ev_tx, ev_rx) = channel();
        let worker = std::thread::spawn(move || run(cmd_rx, ev_tx, || {}));

        cmd_tx.send(Cmd::Connect(sim_conn(), 4_000_000)).unwrap();
        // 连接后自动查询一次 Wi-Fi 状态
        next(&ev_rx, |e| matches!(e, Event::WifiStatus(Ok(_))));
        // 密码不足 8 字节（协议文档 3.7），保存失败，连接还在
        cmd_tx
            .send(Cmd::WifiApply(WifiChange::Set {
                ssid: "lab".into(),
                password: "short".into(),
            }))
            .unwrap();
        next(
            &ev_rx,
            |e| matches!(e, Event::Error(m) if m.contains("Wi-Fi")),
        );
        cmd_tx.send(Cmd::Core(CoreCmd::Halt)).unwrap();
        next(&ev_rx, |e| matches!(e, Event::Core { cmd: Some(_), .. }));

        drop(cmd_tx);
        worker.join().unwrap();
    }
}
