//! 后台线程：独占探针，执行界面发来的命令，把结果和采样帧送回界面。
//! 界面线程不直接碰探针，USB/串口/TCP 的阻塞读写都在这里。

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use xrdap::cortex::{self, AfterReset, CoreStatus};
use xrdap::dap::Probe;
use xrdap::expr::Target;
use xrdap::fake::FakeProbe;
use xrdap::transport::serial::SerialTransport;
use xrdap::transport::tcp::TcpTransport;
use xrdap::types::SymbolTable;
use xrdap::vendor::{self, Item, ItemResult, SubItem};
use xrdap::watch::{HostFrame, Sampler, StartError, Stats};

#[derive(Debug, Clone)]
pub enum Conn {
    Usb(Option<String>),
    Serial {
        port: String,
        baud: u32,
        keep_lines: bool,
    },
    Tcp(String),
    /// 进程内的模拟探针；有 ELF 时按 ELF 填内存
    Fake(Option<PathBuf>),
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
}

const TIMEOUT: Duration = Duration::from_millis(2000);

fn open(conn: &Conn) -> Result<Probe, String> {
    let e = |x: xrdap::Error| x.to_string();
    Ok(match conn {
        Conn::Usb(serial) => Probe::new(Box::new(
            xrdap::transport::usb::UsbTransport::open(serial.as_deref(), TIMEOUT).map_err(e)?,
        )),
        Conn::Serial {
            port,
            baud,
            keep_lines,
        } => {
            let mut t = SerialTransport::open(port, *baud, *keep_lines, TIMEOUT).map_err(e)?;
            t.handshake(Duration::from_secs(10)).map_err(e)?;
            Probe::new(Box::new(t))
        }
        Conn::Tcp(addr) => Probe::new(Box::new(TcpTransport::connect(addr, TIMEOUT).map_err(e)?)),
        Conn::Fake(elf) => {
            let fake = FakeProbe::new();
            if let Some(path) = elf {
                let data = std::fs::read(path).map_err(|x| x.to_string())?;
                fake.load_elf_image(&data).map_err(e)?;
                let syms: SymbolTable = xrdap::elf::load_bytes(&data).map_err(e)?;
                fake.apply_g431_fixture(&syms);
            }
            Probe::new(Box::new(fake))
        }
    })
}

fn items_of(targets: &[Target]) -> Vec<Item> {
    targets
        .iter()
        .enumerate()
        .map(|(i, t)| t.item(i as u16))
        .collect()
}

fn write_one(probe: &mut Probe, target: &Target, bytes: &[u8]) -> Event {
    let item = target.item(0);
    let res = probe.write(&[(item.clone(), bytes.to_vec())]);
    match res {
        Ok(r) if r.first().is_some_and(|r| r.status == 0) => {
            let back = probe.read(&[item]).ok().and_then(|v| v.into_iter().next());
            let text = match back {
                Some(b) if b.status == 0 => format!("读回 {}", target.ty.format(&b.value)),
                Some(b) => format!(
                    "读回失败：{}",
                    vendor::item_status_text(b.status, &target.chain)
                ),
                None => "读回失败".into(),
            };
            Event::WriteDone {
                name: target.name.clone(),
                ok: true,
                text,
            }
        }
        Ok(r) => Event::WriteDone {
            name: target.name.clone(),
            ok: false,
            text: vendor::item_status_text(r.first().map_or(0xFF, |r| r.status), &target.chain),
        },
        Err(e) => Event::WriteDone {
            name: target.name.clone(),
            ok: false,
            text: e.to_string(),
        },
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

fn poll_core(probe: &mut Probe) -> Event {
    Event::Core {
        cmd: None,
        status: cortex::status(probe).map_err(|e| e.to_string()),
    }
}

pub fn run(rx: Receiver<Cmd>, tx: Sender<Event>, wake: impl Fn() + Send + 'static) {
    // 采样帧和统计不唤醒界面：界面采样时自己按固定帧率刷新
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
    let mut pending: Option<Cmd> = None;
    loop {
        let cmd = match pending.take() {
            Some(c) => c,
            None => match rx.recv() {
                Ok(c) => c,
                Err(_) => return,
            },
        };
        match cmd {
            Cmd::Connect(conn, clock) => {
                probe = None;
                match open(&conn).and_then(|mut p| {
                    let t = p
                        .connect_swd(clock)
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
                Some(p) => send(write_one(p, &target, &bytes)),
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
                        Ok(Cmd::Write(t, b)) => send(write_one(sampler.probe_mut(), &t, &b)),
                        Ok(Cmd::Read(tag, targets)) => {
                            match sampler.probe_mut().read(&items_of(&targets)) {
                                Ok(r) => send(Event::ReadDone(tag, r)),
                                Err(e) => send(Event::Error(format!("读取失败：{e}"))),
                            }
                        }
                        Ok(Cmd::PollCore) => send(poll_core(sampler.probe_mut())),
                        Ok(Cmd::Core(c)) => send(core_cmd(sampler.probe_mut(), c)),
                        Ok(Cmd::Stop) => break None,
                        Ok(other) => {
                            // 连接、断开、重新开始：先停掉这次采样再处理
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
