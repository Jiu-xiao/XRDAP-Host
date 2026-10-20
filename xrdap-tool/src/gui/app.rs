//! 界面：左边连接、ELF、变量搜索；中间采样控制、监视表、曲线；底部日志。
//! `App` 是唯一的状态持有者；各块界面在 `panels/`，数据类型在 `model.rs`。

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use xrdap::cortex::{AfterReset, CoreStatus};
use xrdap::discover::ProbeEntry;
use xrdap::share::Event as ShareEvent;
use xrdap::types::SymbolTable;
use xrdap::vendor::ItemResult;
use xrdap::watch::{HostFrame, Stats};
use xrdap::wifi::WifiStatus;

use super::model::{Csv, Kind, ShareState, VarRow, Watch};
use super::plot::PlotView;
use super::worker::{self, Cmd, CoreCmd, Event};

const LOG_LINES: usize = 300;

pub struct App {
    pub(super) tx: Option<Sender<Cmd>>,
    pub(super) rx: Receiver<Event>,
    pub(super) worker: Option<JoinHandle<()>>,
    pub(super) started: Instant,

    pub(super) kind: Kind,
    pub(super) usb_list: Vec<(Option<String>, String)>,
    /// 找到的探针（USB、串口、局域网）；`finding` 不为空时正在后台找
    pub(super) found: Vec<ProbeEntry>,
    pub(super) finding: Option<Receiver<Vec<ProbeEntry>>>,
    pub(super) usb_serial: String,
    pub(super) serial_ports: Vec<String>,
    pub(super) serial_port: String,
    pub(super) baud: String,
    pub(super) keep_lines: bool,
    pub(super) tcp_addr: String,
    pub(super) clock: String,
    pub(super) connecting: bool,
    pub(super) connected: bool,
    pub(super) probe_desc: String,
    pub(super) target_desc: String,
    pub(super) core_info: String,
    pub(super) core: Option<Result<CoreStatus, String>>,
    pub(super) core_busy: bool,
    pub(super) last_core_poll: Instant,
    /// 正在共享为 USB DAP 时的状态；None 为没有共享
    pub(super) share: Option<ShareState>,
    pub(super) share_busy: bool,
    /// 探针报告的 Wi-Fi 状态；None 为还没有查询
    pub(super) wifi: Option<Result<WifiStatus, String>>,
    pub(super) wifi_ssid: String,
    pub(super) wifi_password: String,
    pub(super) wifi_busy: bool,

    pub(super) elf_path: String,
    pub(super) syms: SymbolTable,
    pub(super) vars: Vec<VarRow>,
    pub(super) search: String,
    pub(super) expr: String,
    pub(super) user_only: bool,

    pub(super) watches: Vec<Watch>,
    pub(super) next_id: u64,
    pub(super) read_ids: HashMap<u64, Vec<u64>>,
    pub(super) next_tag: u64,

    pub(super) period: String,
    pub(super) stop_on_full: bool,
    pub(super) starting: bool,
    pub(super) sampling: bool,
    pub(super) run_ids: Vec<u64>,
    pub(super) est_frame_us: u32,
    pub(super) stats: Stats,
    pub(super) rate: f64,
    pub(super) rate_mark: Option<(Instant, u64)>,
    pub(super) t0_probe: Option<u64>,
    pub(super) t_last: Option<f64>,
    pub(super) window_s: f64,
    pub(super) paused: bool,
    pub(super) plot: PlotView,
    pub(super) csv: Option<Csv>,

    pub(super) log: VecDeque<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let font = super::fonts::install(&cc.egui_ctx);
        let (tx, worker_rx) = channel();
        let (worker_tx, rx) = channel();
        let ctx = cc.egui_ctx.clone();
        let worker = std::thread::Builder::new()
            .name("xrdap-worker".into())
            .spawn(move || worker::run(worker_rx, worker_tx, move || ctx.request_repaint()))
            .expect("启动后台线程");
        let get = |k: &str| cc.storage.and_then(|s| s.get_string(k));
        let mut app = App {
            tx: Some(tx),
            rx,
            worker: Some(worker),
            started: Instant::now(),
            kind: Kind::from_key(&get("xrdap.kind").unwrap_or_default()),
            usb_list: Vec::new(),
            found: Vec::new(),
            finding: None,
            usb_serial: get("xrdap.usb").unwrap_or_default(),
            serial_ports: Vec::new(),
            serial_port: get("xrdap.serial").unwrap_or_default(),
            baud: get("xrdap.serial_baud").unwrap_or_default(),
            keep_lines: get("xrdap.keep_lines").is_some_and(|v| v == "1"),
            tcp_addr: get("xrdap.tcp").unwrap_or_default(),
            clock: get("xrdap.clock").unwrap_or_else(|| "4M".into()),
            connecting: false,
            connected: false,
            probe_desc: String::new(),
            target_desc: String::new(),
            core_info: String::new(),
            core: None,
            core_busy: false,
            last_core_poll: Instant::now(),
            share: None,
            share_busy: false,
            wifi: None,
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            wifi_busy: false,
            elf_path: get("xrdap.elf").unwrap_or_default(),
            syms: SymbolTable::new(),
            vars: Vec::new(),
            search: String::new(),
            expr: String::new(),
            user_only: get("xrdap.user_only").is_some_and(|v| v == "1"),
            watches: Vec::new(),
            next_id: 0,
            read_ids: HashMap::new(),
            next_tag: 0,
            period: get("xrdap.period").unwrap_or_else(|| "1ms".into()),
            stop_on_full: false,
            starting: false,
            sampling: false,
            run_ids: Vec::new(),
            est_frame_us: 0,
            stats: Stats::default(),
            rate: 0.0,
            rate_mark: None,
            t0_probe: None,
            t_last: None,
            window_s: get("xrdap.window")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10.0),
            paused: false,
            plot: PlotView {
                normalize: get("xrdap.normalize").is_some_and(|v| v == "1"),
                ..Default::default()
            },
            csv: None,
            log: VecDeque::new(),
        };
        match font {
            Some(p) => app.log(format!("中文字体：{p}")),
            None => app.log("没有找到中文字体，中文会显示成方框".into()),
        }
        app.refresh_usb();
        app.refresh_serial();
        app.start_finding(&cc.egui_ctx);
        if !app.elf_path.trim().is_empty() {
            app.load_elf();
        }
        if let Some(saved) = get("xrdap.watches") {
            for line in saved.lines() {
                let mut parts = line.split('\t');
                let Some(name) = parts.next() else { continue };
                let div = parts.next().and_then(|d| d.parse().ok()).unwrap_or(1);
                let show = parts.next() != Some("0");
                let chart = parts.next().and_then(|c| c.parse().ok()).unwrap_or(1);
                app.add_expr(name, div, show, chart, true);
            }
        }
        app
    }

    pub(super) fn log(&mut self, msg: String) {
        let t = self.started.elapsed().as_secs_f64();
        self.log.push_back(format!("[{t:8.2}] {msg}"));
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }

    pub(super) fn send(&mut self, cmd: Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }

    /// 能不能读写、采样、控制内核：已连接，且探针没有共享给外部工具。
    pub(super) fn usable(&self) -> bool {
        self.connected && self.share.is_none()
    }

    fn on_frames(&mut self, frames: Vec<HostFrame>) {
        let idx: Vec<Option<usize>> = self
            .run_ids
            .iter()
            .map(|id| self.watches.iter().position(|w| w.id == *id))
            .collect();
        let mut csv_err = None;
        let now = Instant::now();
        for f in &frames {
            let t0 = *self.t0_probe.get_or_insert(f.probe_us);
            let t = (f.probe_us as f64 - t0 as f64) / 1e6;
            self.t_last = Some(t);
            let record = self
                .csv
                .as_ref()
                .is_some_and(|c| f.host_unix_us >= c.start_unix_us);
            let mut cells = Vec::with_capacity(if record { f.values.len() } else { 0 });
            for (k, v) in f.values.iter().enumerate() {
                let Some(Some(i)) = idx.get(k) else {
                    if record {
                        cells.push(None);
                    }
                    continue;
                };
                let w = &mut self.watches[*i];
                if record {
                    cells.push(xrdap::csv::cell(&w.target.ty, v));
                }
                match v {
                    Some(Some(b)) => {
                        w.series.push(t, w.target.ty.to_f64(b));
                        w.failed = false;
                        if w.latest.as_ref() != Some(b) {
                            if w.latest.is_some() {
                                w.changed_at = Some(now);
                            }
                            w.latest = Some(b.clone());
                        }
                    }
                    Some(None) => w.failed = true,
                    None => {}
                }
            }
            let line = record.then(|| xrdap::csv::row(f, t, &cells));
            if let (Some(line), Some(c)) = (line, &mut self.csv) {
                if let Err(e) = writeln!(c.out, "{line}") {
                    csv_err = Some(e);
                    break;
                }
                c.rows += 1;
            }
        }
        if let Some(e) = csv_err {
            self.log(format!("写 CSV 失败：{e}"));
            self.csv = None;
        }
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Connected { probe, target } => {
                self.connecting = false;
                self.connected = true;
                self.log(format!("已连接：{probe}；{target}"));
                self.probe_desc = probe;
                self.target_desc = target;
            }
            Event::CoreInfo(text) => self.core_info = text,
            Event::Core { cmd, status } => self.on_core(cmd, status),
            Event::Sharing { attach, device } => {
                self.share_busy = false;
                self.sampling = false;
                self.log(format!("已共享为 USB DAP：{device}；挂载：{attach}"));
                self.share = Some(ShareState {
                    attach,
                    device,
                    client: None,
                });
            }
            Event::Share(ev) => {
                if let Some(st) = self.share.as_mut() {
                    st.client = match ev {
                        ShareEvent::Client(peer) => Some(peer),
                        ShareEvent::Idle(_) => None,
                    };
                }
            }
            Event::ShareStopped => {
                self.share_busy = false;
                self.share = None;
                self.log("已停止共享".into());
            }
            Event::WifiStatus(status) => {
                self.wifi_busy = false;
                self.wifi = Some(status);
            }
            Event::WifiApplied { cleared } => {
                self.wifi_busy = false;
                self.wifi_password.clear();
                self.log(if cleared {
                    "已清除热点参数，探针重启后不再连 Wi-Fi".into()
                } else {
                    format!("已保存热点 {}，探针重启后连接", self.wifi_ssid)
                });
            }
            Event::Disconnected => {
                self.wifi = None;
                self.core = None;
                self.core_info.clear();
                self.connected = false;
                self.sampling = false;
                self.log("已断开".into());
            }
            Event::Error(e) => {
                if self.connecting {
                    self.connected = false;
                }
                self.connecting = false;
                self.share_busy = false;
                self.wifi_busy = false;
                self.starting = false;
                self.log(e);
            }
            Event::ReadDone(tag, results) => self.on_read_done(tag, results),
            Event::WriteDone { name, ok, text } => self.on_write_done(name, ok, text),
            Event::Started {
                est_frame_us,
                phases,
            } => self.on_started(est_frame_us, phases),
            Event::Rejected { est_frame_us } => self.on_rejected(est_frame_us),
            Event::Frames(frames) => self.on_frames(frames),
            Event::Stats(s) => self.on_stats(s),
            Event::Stopped => self.on_stopped(),
        }
    }

    fn on_core(&mut self, cmd: Option<CoreCmd>, status: Result<CoreStatus, String>) {
        if let Some(cmd) = cmd {
            self.core_busy = false;
            let what = match cmd {
                CoreCmd::Halt => "停住",
                CoreCmd::Resume => "继续",
                CoreCmd::Reset(AfterReset::Run) => "复位",
                CoreCmd::Reset(AfterReset::Halt) => "复位并停住",
            };
            match &status {
                Ok(s) => self.log(format!("{what}：内核{}", s.state.text())),
                Err(e) => self.log(format!("{what}失败：{e}")),
            }
        }
        self.core = Some(status);
    }

    fn on_read_done(&mut self, tag: u64, results: Vec<ItemResult>) {
        let ids = self.read_ids.remove(&tag).unwrap_or_default();
        for (id, r) in ids.iter().zip(results) {
            if let Some(w) = self.watches.iter_mut().find(|w| w.id == *id) {
                if r.status == 0 {
                    w.latest = Some(r.value);
                    w.failed = false;
                } else {
                    w.failed = true;
                    w.note = xrdap::vendor::item_status_text(r.status, &w.target.chain);
                }
            }
        }
    }

    fn on_write_done(&mut self, name: String, ok: bool, text: String) {
        if let Some(w) = self.watches.iter_mut().find(|w| w.target.name == name) {
            w.note = if ok {
                text.clone()
            } else {
                format!("写入失败：{text}")
            };
        }
        self.log(format!(
            "写 {name}：{}",
            if ok { text } else { format!("失败，{text}") }
        ));
    }

    fn on_started(&mut self, est_frame_us: u32, phases: Vec<(u16, u16)>) {
        self.starting = false;
        self.sampling = true;
        self.est_frame_us = est_frame_us;
        self.stats = Stats::default();
        self.rate = 0.0;
        self.rate_mark = None;
        self.t0_probe = None;
        self.t_last = None;
        for w in &mut self.watches {
            w.series.clear();
        }
        let spread = phases.iter().filter(|(d, _)| *d > 1).count();
        let extra = if spread > 0 {
            format!("，{spread} 个分频项已错开相位")
        } else {
            String::new()
        };
        self.log(format!(
            "开始采样：{} 个变量，单帧估计 {est_frame_us} µs{extra}",
            self.run_ids.len()
        ));
    }

    fn on_rejected(&mut self, est_frame_us: u32) {
        self.starting = false;
        let p = xrdap::watch::suggest_period(est_frame_us);
        self.log(format!(
            "探针拒绝：单帧估计 {est_frame_us} µs，比周期长。周期已改成建议值 {p} µs，或者给变量加分频"
        ));
        self.period = format!("{p}us");
    }

    fn on_stats(&mut self, s: Stats) {
        let now = Instant::now();
        match self.rate_mark {
            Some((t, n)) if now.duration_since(t) >= Duration::from_millis(900) => {
                self.rate = s.frames.saturating_sub(n) as f64 / now.duration_since(t).as_secs_f64();
                self.rate_mark = Some((now, s.frames));
            }
            None => self.rate_mark = Some((now, s.frames)),
            _ => {}
        }
        self.stats = s;
    }

    fn on_stopped(&mut self) {
        if self.sampling {
            let s = self.stats;
            self.log(format!(
                "停止采样：{} 帧，探针丢帧 {}，跳拍 {}，缺帧 {}，失败值 {}",
                s.frames, s.dropped, s.skipped, s.missing, s.failed_values
            ));
        }
        self.sampling = false;
        self.starting = false;
        self.stop_csv();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(ev) = self.rx.try_recv() {
            self.handle(ev);
        }
        egui::TopBottomPanel::bottom("log")
            .resizable(true)
            .default_height(110.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for l in &self.log {
                            ui.label(RichText::new(l).monospace().small());
                        }
                    });
            });
        egui::SidePanel::left("left")
            .resizable(true)
            .default_width(320.0)
            .width_range(260.0..=520.0)
            .show(ctx, |ui| {
                self.ui_connection(ui);
                ui.separator();
                self.ui_elf(ui);
                ui.separator();
                self.ui_vars(ui);
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_controls(ui);
            ui.separator();
            let h = (ui.available_height() * 0.38).max(80.0);
            ui.allocate_ui(egui::vec2(ui.available_width(), h), |ui| {
                ui.set_max_height(h);
                self.ui_watches(ui);
            });
            ui.separator();
            self.ui_plot(ui);
        });
        if self.sampling || self.starting || self.connecting {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
        // 连接期间每 0.5 秒读一次内核状态（读 DHCSR 是一次厂商 READ，采样期间也可以）
        if self.usable() && !self.core_busy {
            const POLL: Duration = Duration::from_millis(500);
            if self.last_core_poll.elapsed() >= POLL {
                self.last_core_poll = Instant::now();
                self.send(Cmd::PollCore);
            }
            ctx.request_repaint_after(POLL);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // eframe 自己用 "window"、"egui" 这些键，这里的键都加 xrdap. 前缀
        storage.set_string("xrdap.kind", self.kind.key().into());
        storage.set_string("xrdap.usb", self.usb_serial.clone());
        storage.set_string("xrdap.serial", self.serial_port.clone());
        storage.set_string("xrdap.serial_baud", self.baud.clone());
        storage.set_string(
            "xrdap.keep_lines",
            if self.keep_lines { "1" } else { "0" }.into(),
        );
        storage.set_string("xrdap.tcp", self.tcp_addr.clone());
        storage.set_string("xrdap.clock", self.clock.clone());
        storage.set_string("xrdap.elf", self.elf_path.clone());
        storage.set_string("xrdap.period", self.period.clone());
        storage.set_string("xrdap.window", self.window_s.to_string());
        storage.set_string("xrdap.normalize", u8::from(self.plot.normalize).to_string());
        storage.set_string("xrdap.user_only", u8::from(self.user_only).to_string());
        let watches: Vec<String> = self
            .watches
            .iter()
            .map(|w| {
                format!(
                    "{}\t{}\t{}\t{}",
                    w.target.name,
                    w.div,
                    u8::from(w.show),
                    w.chart
                )
            })
            .collect();
        storage.set_string("xrdap.watches", watches.join("\n"));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // 先让后台线程停掉采样、断开探针，再退出
        self.send(Cmd::Disconnect);
        self.tx = None;
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
        self.stop_csv();
    }
}
