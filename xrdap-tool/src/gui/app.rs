//! 界面：左边连接、ELF、变量搜索；中间采样控制、监视表、曲线；底部日志。
//! `App` 是唯一的状态持有者；各块界面在 `panels/`，数据类型在 `model.rs`。

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use xrdap::cortex::{AfterReset, CoreStatus};
use xrdap::share::Event as ShareEvent;
use xrdap::types::SymbolTable;
use xrdap::watch::{HostFrame, Stats};
use xrdap::ItemResult;
use xrdap::WifiStatus;

use super::model::{Csv, Found, ShareState, VarRow, Watch};
use super::panels::{ElfLoaded, SavedWatch};
use super::plot::PlotView;
use super::theme::{self, Choice};
use super::worker::{self, Cmd, CoreCmd, Event, Op};

const LOG_LINES: usize = 300;

pub struct App {
    pub(super) ctx: egui::Context,
    pub(super) tx: Option<Sender<Cmd>>,
    pub(super) rx: Receiver<Event>,
    pub(super) worker: Option<JoinHandle<()>>,
    pub(super) started: Instant,

    /// 找到的探针和其他串口；`finding` 不为空时正在后台找
    pub(super) found: Found,
    pub(super) finding: Option<Receiver<Found>>,
    /// 探针说明，和命令行的 -p 写法相同；空为自动选择
    pub(super) probe_spec: String,
    pub(super) baud: String,
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
    /// 后台正在读的 ELF；读完前不能开始采样
    pub(super) elf_loading: Option<Receiver<ElfLoaded>>,
    /// 等 ELF 读完再恢复的监视项（启动时从设置里读出来的）
    pub(super) restore: Vec<SavedWatch>,
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
    /// 亮暗主题的选择
    pub(super) theme: Choice,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let fonts = super::fonts::install(&cc.egui_ctx);
        let mut app = Self::with_settings(cc.egui_ctx.clone(), worker::SHARE_LISTEN, |k| {
            cc.storage.and_then(|s| s.get_string(k))
        });
        theme::install(&cc.egui_ctx, &fonts.families, app.theme);
        app.log(fonts.summary);
        app.start_finding();
        app
    }

    /// 按保存的设置建立界面状态，启动后台线程，在后台开始读 ELF。
    /// `share_listen` 是共享为 USB DAP 时的监听地址。
    pub(super) fn with_settings(
        ctx: egui::Context,
        share_listen: &'static str,
        get: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let (tx, worker_rx) = channel();
        let (worker_tx, rx) = channel();
        let repaint = ctx.clone();
        let worker = std::thread::Builder::new()
            .name("xrdap-worker".into())
            .spawn(move || {
                worker::run(
                    worker_rx,
                    worker_tx,
                    move || repaint.request_repaint(),
                    share_listen,
                )
            })
            .expect("启动后台线程");
        let mut app = App {
            ctx,
            tx: Some(tx),
            rx,
            worker: Some(worker),
            started: Instant::now(),
            found: Found::default(),
            finding: None,
            probe_spec: get("xrdap.probe").unwrap_or_default(),
            baud: get("xrdap.serial_baud").unwrap_or_default(),
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
            elf_loading: None,
            restore: Vec::new(),
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
            theme: Choice::from_key(&get("xrdap.theme").unwrap_or_default()),
        };
        let saved: Vec<SavedWatch> = get("xrdap.watches")
            .unwrap_or_default()
            .lines()
            .filter_map(SavedWatch::parse)
            .collect();
        if app.elf_path.trim().is_empty() {
            for w in saved {
                app.add_expr(&w.name, w.div, w.show, w.chart, true);
            }
        } else {
            // 按名字的监视项要等符号表
            app.restore = saved;
            app.load_elf();
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
        let mut csv_lost = false;
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
                if !c.push(line) {
                    csv_lost = true;
                    break;
                }
            }
        }
        if csv_lost {
            // 写文件的线程出错退出了，stop_csv 报告原因
            self.stop_csv();
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
            Event::Failed { op, msg } => {
                match op {
                    Op::Connect => {
                        self.connecting = false;
                        self.connected = false;
                    }
                    Op::Share => self.share_busy = false,
                    Op::Wifi => self.wifi_busy = false,
                    Op::Sample => self.starting = false,
                    Op::Read => {}
                }
                self.log(msg);
            }
            Event::ReadDone(tag, results) => self.on_read_done(tag, results),
            Event::WriteDone { name, ok, text } => self.on_write_done(name, ok, text),
            Event::Started { phases } => self.on_started(phases),
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
                    w.note = xrdap::item_status_text(r.status, &w.target.chain);
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

    fn on_started(&mut self, phases: Vec<(u16, u16)>) {
        self.starting = false;
        self.sampling = true;
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
        self.log(format!("开始采样：{} 个变量{extra}", self.run_ids.len()));
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

impl App {
    /// 顶栏：产品名和主题选择。
    fn ui_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                theme::path_label(ui, "XRDAP / HOST");
                ui.label(RichText::new("XR-DAP").text_style(egui::TextStyle::Heading));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for c in Choice::ALL.iter().rev() {
                    if ui.selectable_label(self.theme == *c, c.label()).clicked() {
                        self.theme = *c;
                        theme::choose(ui.ctx(), *c);
                    }
                }
                theme::path_label(ui, "主题");
            });
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(ev) = self.rx.try_recv() {
            self.handle(ev);
        }
        self.poll_elf();
        let p = theme::Palette::of(ctx.theme());
        // 导航栏：paper-raised 加底部一条 line
        egui::TopBottomPanel::top("bar")
            .frame(
                egui::Frame::new()
                    .fill(p.raised)
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ctx, |ui| self.ui_bar(ui));
        // 页脚的位置放日志：paper-sunken 加顶部一条 line
        egui::TopBottomPanel::bottom("log")
            .resizable(true)
            .default_height(110.0)
            .frame(
                egui::Frame::new()
                    .fill(p.sunken)
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
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
                ui.add_space(8.0);
                theme::card(ui, |ui| self.ui_connection(ui));
                ui.add_space(8.0);
                theme::card(ui, |ui| self.ui_elf(ui));
                ui.add_space(8.0);
                theme::card(ui, |ui| self.ui_vars(ui));
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
        storage.set_string("xrdap.theme", self.theme.key().into());
        storage.set_string("xrdap.probe", self.probe_spec.clone());
        storage.set_string("xrdap.serial_baud", self.baud.clone());
        storage.set_string("xrdap.clock", self.clock.clone());
        storage.set_string("xrdap.elf", self.elf_path.clone());
        storage.set_string("xrdap.period", self.period.clone());
        storage.set_string("xrdap.window", self.window_s.to_string());
        storage.set_string("xrdap.normalize", u8::from(self.plot.normalize).to_string());
        storage.set_string("xrdap.user_only", u8::from(self.user_only).to_string());
        let watches: Vec<String> = self
            .watches
            .iter()
            .map(|w| SavedWatch::of(w).line())
            .collect();
        storage.set_string("xrdap.watches", watches.join("\n"));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // 让后台线程停掉采样、断开探针。它可能正卡在打开探针里（串口最多等十几秒），
        // 窗口最多等 EXIT_WAIT，之后进程退出时线程随之结束
        const EXIT_WAIT: Duration = Duration::from_secs(3);
        self.send(Cmd::Disconnect);
        self.tx = None;
        let deadline = Instant::now() + EXIT_WAIT;
        if let Some(h) = self.worker.take() {
            while !h.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        self.stop_csv();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::with_settings(egui::Context::default(), "127.0.0.1:0", |_| None)
    }

    fn wait_elf(a: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while a.elf_loading.is_some() && Instant::now() < deadline {
            a.poll_elf();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(a.elf_loading.is_none(), "ELF 没有读完");
    }

    /// 一个操作失败只结束它自己的“进行中”状态，别的操作照常等结果。
    #[test]
    fn failure_ends_only_its_own_operation() {
        let mut a = app();
        (a.connected, a.wifi_busy, a.share_busy, a.starting) = (true, true, true, true);
        a.handle(Event::Failed {
            op: Op::Read,
            msg: "读取失败".into(),
        });
        assert!(a.connected && a.wifi_busy && a.share_busy && a.starting);
        a.handle(Event::Failed {
            op: Op::Wifi,
            msg: "配置 Wi-Fi 失败".into(),
        });
        assert!(!a.wifi_busy && a.share_busy && a.starting && a.connected);
    }

    /// ELF 在后台读；采样时不重新读，正在采样的监视项不会被换掉。
    #[test]
    fn elf_loads_in_background_and_not_while_sampling() {
        let mut a = app();
        a.elf_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../xrdap/tests/fixtures/types.elf"
        )
        .into();
        a.load_elf();
        wait_elf(&mut a);
        assert!(a.vars.iter().any(|v| v.name == "counter"));
        a.add_expr("counter", 1, true, 1, false);
        let ids: Vec<u64> = a.watches.iter().map(|w| w.id).collect();
        a.run_ids = ids.clone();
        a.sampling = true;
        a.load_elf();
        assert!(a.elf_loading.is_none());
        assert_eq!(a.watches.iter().map(|w| w.id).collect::<Vec<_>>(), ids);
    }

    /// 保存 Wi-Fi 时密码离开界面状态，失败也不留在界面里。
    #[test]
    fn wifi_password_leaves_gui_state() {
        let mut a = app();
        a.wifi_ssid = "lab".into();
        a.wifi_password = "12345678".into();
        let cmd = a.wifi_set_cmd();
        assert!(a.wifi_password.is_empty());
        assert!(matches!(
            cmd,
            Cmd::WifiApply(super::worker::WifiChange::Set { ref password, .. }) if password == "12345678"
        ));
        a.handle(Event::Failed {
            op: Op::Wifi,
            msg: "配置 Wi-Fi 失败".into(),
        });
        assert!(a.wifi_password.is_empty());
    }

    /// 启动时按名字的监视项等 ELF 读完再恢复。
    #[test]
    fn saved_watches_wait_for_symbols() {
        let mut a = App::with_settings(egui::Context::default(), "127.0.0.1:0", |k| match k {
            "xrdap.elf" => Some(
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../xrdap/tests/fixtures/types.elf"
                )
                .into(),
            ),
            "xrdap.watches" => Some("state.count	10	1	2".into()),
            _ => None,
        });
        assert!(a.watches.is_empty());
        wait_elf(&mut a);
        assert_eq!(a.watches.len(), 1);
        assert_eq!((a.watches[0].div, a.watches[0].chart), (10, 2));
    }
}
