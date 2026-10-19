//! 界面：左边连接、ELF、变量搜索；中间采样控制、监视表、曲线；底部日志。

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use xrdap::expr::{Resolver, Target};
use xrdap::types::{ScalarKind, SymbolTable};
use xrdap::watch::{HostFrame, Stats};

use crate::plot::{Curve, PlotView, MAX_CHARTS};
use crate::series::Series;
use crate::share::ShareEvent;
use crate::worker::{self, Cmd, Conn, CoreCmd, Event};
use xrdap::cortex::{AfterReset, CoreState, CoreStatus};
use xrdap::discover::{self, ProbeEntry};
use xrdap::open::Addr;
use xrdap::transport::serial::DEFAULT_BAUD;

/// 曲线最多保留的时长和点数（每条）
const KEEP_S: f64 = 120.0;
const MAX_POINTS: usize = 500_000;
const WINDOWS: &[f64] = &[0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0];
const LOG_LINES: usize = 300;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Usb,
    Serial,
    Tcp,
    Fake,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Usb => "USB",
            Kind::Serial => "串口",
            Kind::Tcp => "TCP",
            Kind::Fake => "模拟",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Kind::Usb => "usb",
            Kind::Serial => "serial",
            Kind::Tcp => "tcp",
            Kind::Fake => "fake",
        }
    }
    fn from_key(s: &str) -> Self {
        match s {
            "serial" => Kind::Serial,
            "tcp" => Kind::Tcp,
            "fake" => Kind::Fake,
            _ => Kind::Usb,
        }
    }
}

struct Watch {
    id: u64,
    target: Target,
    div: u16,
    show: bool,
    latest: Option<Vec<u8>>,
    failed: bool,
    write_text: String,
    note: String,
    series: Series,
    color: Color32,
    /// 画在第几张图（1 起）
    chart: u8,
    /// 当前值最近一次变化的时刻，用来高亮
    changed_at: Option<Instant>,
}

struct VarRow {
    name: String,
    lower: String,
    addr: u64,
    ty: String,
    size: Option<u64>,
    /// 定义在 HAL、CMSIS、RTOS、C 库等第三方代码里
    system: bool,
}

/// 按编译单元路径和变量名粗略判断是不是第三方代码（HAL、CMSIS、RTOS、中间件、C 库）的变量。
fn is_system_var(name: &str, unit: &str) -> bool {
    let u = unit.replace('\\', "/").to_ascii_lowercase();
    const UNIT_MARKS: &[&str] = &[
        "drivers/",
        "middlewares/",
        "cmsis",
        "_hal",
        "hal_",
        "freertos",
        "/rtos",
        "rt-thread",
        "lwip",
        "usb_device",
        "usbd_",
        "fatfs",
        "newlib",
        "picolibc",
        "libc",
        "libgcc",
        "/arm-none-eabi/",
        "startup_",
        "syscalls",
        "sysmem",
        "system_stm32",
        "segger",
    ];
    name.starts_with("__")
        || matches!(name, "errno" | "_impure_ptr" | "environ")
        || UNIT_MARKS.iter().any(|m| u.contains(m))
}

/// 输入框占满剩下的宽度：留出边框和内边距，否则每帧把面板撑宽一点。
fn fill_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() - 12.0).max(40.0)
}

struct Csv {
    path: PathBuf,
    out: BufWriter<File>,
    rows: u64,
    /// 从这个电脑时间（Unix 微秒）起的帧才写。选文件时对话框挡住界面，期间积压的帧不算
    start_unix_us: i64,
}

fn unix_us_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

/// 共享为 USB DAP 的界面状态。
struct ShareState {
    attach: String,
    device: String,
    client: Option<SocketAddr>,
}

pub struct App {
    tx: Option<Sender<Cmd>>,
    rx: Receiver<Event>,
    worker: Option<JoinHandle<()>>,
    started: Instant,

    kind: Kind,
    usb_list: Vec<(Option<String>, String)>,
    /// 找到的探针（USB、串口、局域网）；`finding` 不为空时正在后台找
    found: Vec<ProbeEntry>,
    finding: Option<Receiver<Vec<ProbeEntry>>>,
    usb_serial: String,
    serial_ports: Vec<String>,
    serial_port: String,
    baud: String,
    keep_lines: bool,
    tcp_addr: String,
    clock: String,
    connecting: bool,
    connected: bool,
    probe_desc: String,
    target_desc: String,
    core_info: String,
    core: Option<Result<CoreStatus, String>>,
    core_busy: bool,
    last_core_poll: Instant,
    /// 正在共享为 USB DAP 时的状态；None 为没有共享
    share: Option<ShareState>,
    share_busy: bool,

    elf_path: String,
    syms: SymbolTable,
    vars: Vec<VarRow>,
    search: String,
    expr: String,
    user_only: bool,

    watches: Vec<Watch>,
    next_id: u64,
    read_ids: HashMap<u64, Vec<u64>>,
    next_tag: u64,

    period: String,
    stop_on_full: bool,
    starting: bool,
    sampling: bool,
    run_ids: Vec<u64>,
    est_frame_us: u32,
    stats: Stats,
    rate: f64,
    rate_mark: Option<(Instant, u64)>,
    t0_probe: Option<u64>,
    t_last: Option<f64>,
    window_s: f64,
    paused: bool,
    plot: PlotView,
    csv: Option<Csv>,

    log: VecDeque<String>,
}

fn color_for(id: u64) -> Color32 {
    let h = (id as f32 * 0.618_034).fract();
    egui::ecolor::Hsva::new(h, 0.75, 0.85, 1.0).into()
}

/// 序列号的末尾 6 位，例如 …93EF1C；短的原样返回。
fn short_serial(sn: &str) -> String {
    let n = sn.chars().count();
    if n > 8 {
        format!("…{}", sn.chars().skip(n - 6).collect::<String>())
    } else {
        sn.to_string()
    }
}

/// 一行、从右往左排：先放的按钮在最右边，最后放的输入框占剩下的宽度。
fn right_to_left_row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), add)
        .inner
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let font = crate::fonts::install(&cc.egui_ctx);
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
            baud: get("xrdap.baud").unwrap_or_else(|| DEFAULT_BAUD.to_string()),
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

    fn log(&mut self, msg: String) {
        let t = self.started.elapsed().as_secs_f64();
        self.log.push_back(format!("[{t:8.2}] {msg}"));
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }

    fn send(&mut self, cmd: Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }

    /// 能不能读写、采样、控制内核：已连接，且探针没有共享给外部工具。
    fn usable(&self) -> bool {
        self.connected && self.share.is_none()
    }

    fn refresh_usb(&mut self) {
        match xrdap::transport::usb::list() {
            Ok(list) => {
                self.usb_list = list
                    .into_iter()
                    .map(|p| {
                        let label = format!(
                            "{}  {}",
                            p.serial.as_deref().unwrap_or("（无序列号）"),
                            p.product.as_deref().unwrap_or("")
                        );
                        (p.serial, label)
                    })
                    .collect();
            }
            Err(e) => self.log(format!("列 USB 设备失败：{e}")),
        }
    }

    fn refresh_serial(&mut self) {
        match serial2::SerialPort::available_ports() {
            Ok(list) => {
                self.serial_ports = list.iter().map(|p| p.display().to_string()).collect();
                self.serial_ports.sort();
            }
            Err(e) => self.log(format!("列串口失败：{e}")),
        }
    }

    fn load_elf(&mut self) {
        let path = PathBuf::from(self.elf_path.trim());
        match xrdap::elf::load(&path) {
            Ok(syms) => {
                let mut vars: Vec<VarRow> = syms
                    .vars
                    .values()
                    .flatten()
                    .map(|v| VarRow {
                        name: v.name.clone(),
                        lower: v.name.to_lowercase(),
                        addr: v.addr,
                        ty: syms.name_of(v.ty),
                        size: syms.size_of(v.ty),
                        system: is_system_var(&v.name, &v.unit),
                    })
                    .collect();
                vars.sort_by(|a, b| a.name.cmp(&b.name).then(a.addr.cmp(&b.addr)));
                self.log(format!(
                    "读入 {}：{} 个全局变量",
                    path.display(),
                    vars.len()
                ));
                self.syms = syms;
                self.vars = vars;
                // 已有的监视项按名字重新解析，地址和类型以新 ELF 为准
                let old: Vec<(String, u16, bool, u8)> = self
                    .watches
                    .drain(..)
                    .map(|w| (w.target.name, w.div, w.show, w.chart))
                    .collect();
                for (name, div, show, chart) in old {
                    self.add_expr(&name, div, show, chart, true);
                }
            }
            Err(e) => self.log(format!("读 ELF 失败：{e}")),
        }
    }

    /// 解析表达式并加进监视表。结构体、数组展开成各个成员；已有同名项时跳过。
    fn add_expr(&mut self, text: &str, div: u16, show: bool, chart: u8, quiet: bool) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let mut r = Resolver::new(&mut self.syms);
        let res = r.resolve_all(text);
        let notes = std::mem::take(&mut r.notes);
        for n in notes {
            self.log(format!("提示：{n}"));
        }
        match res {
            Ok(targets) => {
                let mut added = 0;
                for t in targets {
                    if self.watches.iter().any(|w| w.target.name == t.name) {
                        continue;
                    }
                    let id = self.next_id;
                    self.next_id += 1;
                    // 新加的指针默认不画曲线；恢复的监视项按保存的设置
                    let show = show && (quiet || !matches!(t.ty.kind, ScalarKind::Pointer));
                    self.watches.push(Watch {
                        id,
                        target: t,
                        div: div.max(1),
                        show,
                        latest: None,
                        failed: false,
                        write_text: String::new(),
                        note: String::new(),
                        series: Series::new(KEEP_S, MAX_POINTS),
                        color: color_for(id),
                        chart: chart.clamp(1, MAX_CHARTS),
                        changed_at: None,
                    });
                    added += 1;
                }
                if !quiet && added == 0 {
                    self.log(format!("{text} 已经在监视表里"));
                }
            }
            Err(e) => self.log(format!("{text}：{e}")),
        }
    }

    fn conn(&self) -> Result<Conn, String> {
        Ok(match self.kind {
            Kind::Usb => Conn::Probe {
                addr: Addr::Usb {
                    serial: Some(self.usb_serial.trim().to_string()).filter(|s| !s.is_empty()),
                },
                baud: DEFAULT_BAUD,
            },
            Kind::Serial => {
                if self.serial_port.trim().is_empty() {
                    return Err("先选一个串口".into());
                }
                Conn::Probe {
                    addr: Addr::Serial {
                        port: self.serial_port.trim().to_string(),
                        keep_lines: self.keep_lines,
                    },
                    baud: self
                        .baud
                        .trim()
                        .parse()
                        .map_err(|_| "波特率不是整数".to_string())?,
                }
            }
            Kind::Tcp => {
                if self.tcp_addr.trim().is_empty() {
                    return Err("先填探针或转发程序的地址".into());
                }
                Conn::Probe {
                    addr: Addr::Tcp {
                        host: self.tcp_addr.trim().to_string(),
                    },
                    baud: DEFAULT_BAUD,
                }
            }
            Kind::Fake => Conn::Fake(
                Some(PathBuf::from(self.elf_path.trim())).filter(|p| !p.as_os_str().is_empty()),
            ),
        })
    }

    fn connect(&mut self) {
        let clock = match xrdap::dap::parse_clock(&self.clock) {
            Ok(c) => c,
            Err(e) => {
                self.log(format!("SWD {e}"));
                return;
            }
        };
        match self.conn() {
            Ok(conn) => {
                self.log(format!("连接 {conn:?}，SWD {clock} Hz……"));
                self.connecting = true;
                self.send(Cmd::Connect(conn, clock));
            }
            Err(e) => self.log(e),
        }
    }

    fn start(&mut self) {
        let period_us = match xrdap::watch::parse_period(&self.period) {
            Ok(p) => p,
            Err(e) => {
                self.log(format!("周期：{e}"));
                return;
            }
        };
        if self.watches.is_empty() {
            self.log("监视表是空的".into());
            return;
        }
        self.run_ids = self.watches.iter().map(|w| w.id).collect();
        let targets = self.watches.iter().map(|w| w.target.clone()).collect();
        let divs = self.watches.iter().map(|w| w.div).collect();
        self.starting = true;
        let stop_on_full = self.stop_on_full;
        self.send(Cmd::Start {
            targets,
            divs,
            period_us,
            stop_on_full,
        });
    }

    fn read_once(&mut self) {
        if self.watches.is_empty() {
            return;
        }
        let tag = self.next_tag;
        self.next_tag += 1;
        self.read_ids
            .insert(tag, self.watches.iter().map(|w| w.id).collect());
        let targets = self.watches.iter().map(|w| w.target.clone()).collect();
        self.send(Cmd::Read(tag, targets));
    }

    fn start_csv(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name("xrdap.csv")
            .save_file()
        else {
            return;
        };
        let start_unix_us = unix_us_now();
        let line = xrdap::csv::header(
            self.run_ids
                .iter()
                .filter_map(|id| self.watches.iter().find(|w| w.id == *id))
                .map(|w| w.target.name.as_str()),
        );
        let res = File::create(&path).and_then(|f| {
            let mut out = BufWriter::new(f);
            writeln!(out, "{line}")?;
            Ok(out)
        });
        match res {
            Ok(out) => {
                self.log(format!("开始录制 {}", path.display()));
                self.csv = Some(Csv {
                    path,
                    out,
                    rows: 0,
                    start_unix_us,
                });
            }
            Err(e) => self.log(format!("打开 {} 失败：{e}", path.display())),
        }
    }

    fn stop_csv(&mut self) {
        if let Some(mut c) = self.csv.take() {
            match c.out.flush() {
                Ok(()) => self.log(format!("录制结束：{}，{} 行", c.path.display(), c.rows)),
                Err(e) => self.log(format!("写 {} 失败：{e}", c.path.display())),
            }
        }
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
            Event::Core { cmd, status } => {
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
                        ShareEvent::Idle => None,
                    };
                }
            }
            Event::ShareStopped => {
                self.share_busy = false;
                self.share = None;
                self.log("已停止共享".into());
            }
            Event::Disconnected => {
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
                self.starting = false;
                self.log(e);
            }
            Event::ReadDone(tag, results) => {
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
            Event::WriteDone { name, ok, text } => {
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
            Event::Started {
                est_frame_us,
                phases,
            } => {
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
            Event::Rejected { est_frame_us } => {
                self.starting = false;
                let p = xrdap::watch::suggest_period(est_frame_us);
                self.log(format!(
                    "探针拒绝：单帧估计 {est_frame_us} µs，比周期长。周期已改成建议值 {p} µs，或者给变量加分频"
                ));
                self.period = format!("{p}us");
            }
            Event::Frames(frames) => self.on_frames(frames),
            Event::Stats(s) => {
                let now = Instant::now();
                match self.rate_mark {
                    Some((t, n)) if now.duration_since(t) >= Duration::from_millis(900) => {
                        self.rate =
                            s.frames.saturating_sub(n) as f64 / now.duration_since(t).as_secs_f64();
                        self.rate_mark = Some((now, s.frames));
                    }
                    None => self.rate_mark = Some((now, s.frames)),
                    _ => {}
                }
                self.stats = s;
            }
            Event::Stopped => {
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
    }

    /// 在后台找 USB、串口、局域网上的探针，找完唤醒界面。
    fn start_finding(&mut self, ctx: &egui::Context) {
        if self.finding.is_some() || self.connected {
            return;
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let probes = discover::group(discover::find(&discover::Options::default()));
            let _ = tx.send(probes);
            ctx.request_repaint();
        });
        self.finding = Some(rx);
    }

    /// 选中一个找到的连接：切到对应的连接方式并填好参数。
    fn use_found(&mut self, addr: &Addr) {
        match addr {
            Addr::Usb { serial } => {
                self.kind = Kind::Usb;
                self.usb_serial = serial.clone().unwrap_or_default();
            }
            Addr::Serial { port, keep_lines } => {
                self.kind = Kind::Serial;
                self.serial_port = port.clone();
                self.keep_lines = *keep_lines;
                if !self.serial_ports.contains(port) {
                    self.serial_ports.push(port.clone());
                }
            }
            Addr::Tcp { host } => {
                self.kind = Kind::Tcp;
                self.tcp_addr = host.clone();
            }
        }
    }

    /// 找到的探针：每个连接一行，点一下填进下面的连接参数。
    fn ui_found(&mut self, ui: &mut egui::Ui) {
        if let Some(rx) = &self.finding {
            if let Ok(found) = rx.try_recv() {
                self.found = found;
                self.finding = None;
            }
        }
        ui.horizontal(|ui| {
            ui.label("找到的探针");
            if self.finding.is_some() {
                ui.add(egui::Spinner::new());
            } else if ui.small_button("重新查找").clicked() {
                self.start_finding(ui.ctx());
            }
        });
        if self.found.is_empty() && self.finding.is_none() {
            ui.label(RichText::new("没有找到，可以在下面手动填").weak().small());
        }
        let mut chosen = None;
        for p in &self.found {
            let name = p.serial.as_deref().unwrap_or("（没有序列号）");
            let product = p.product.as_deref().unwrap_or("");
            for l in &p.links {
                let text = format!("{product}  {}  ·  {}", short_serial(name), l.addr);
                if ui
                    .selectable_label(false, RichText::new(text).small())
                    .on_hover_text(format!("{name}\n{}", l.note.as_deref().unwrap_or("")))
                    .clicked()
                {
                    chosen = Some(l.addr.clone());
                }
            }
        }
        if let Some(addr) = chosen {
            self.use_found(&addr);
        }
    }

    // ------------------------------------------------------------------ 界面

    fn ui_connection(&mut self, ui: &mut egui::Ui) {
        ui.heading("连接");
        if !self.connected && !self.connecting {
            self.ui_found(ui);
            ui.separator();
        }
        let editable = !self.connected && !self.connecting;
        let mut refresh_usb = false;
        let mut refresh_serial = false;
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                for k in [Kind::Usb, Kind::Serial, Kind::Tcp, Kind::Fake] {
                    ui.selectable_value(&mut self.kind, k, k.label());
                }
            });
            match self.kind {
                Kind::Usb => {
                    ui.horizontal(|ui| {
                        let shown = if self.usb_serial.is_empty() {
                            "第一个".to_string()
                        } else {
                            self.usb_serial.clone()
                        };
                        egui::ComboBox::from_id_salt("usb")
                            .selected_text(shown)
                            .width(180.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.usb_serial, String::new(), "第一个");
                                for (serial, label) in &self.usb_list {
                                    if let Some(s) = serial {
                                        ui.selectable_value(&mut self.usb_serial, s.clone(), label);
                                    }
                                }
                            });
                        refresh_usb = ui.button("刷新").clicked();
                    });
                    ui.label(format!("找到 {} 个探针", self.usb_list.len()));
                }
                Kind::Serial => {
                    ui.horizontal(|ui| {
                        let shown = if self.serial_port.is_empty() {
                            "选择串口".to_string()
                        } else {
                            self.serial_port.clone()
                        };
                        egui::ComboBox::from_id_salt("serial")
                            .selected_text(shown)
                            .width(180.0)
                            .show_ui(ui, |ui| {
                                for p in &self.serial_ports {
                                    ui.selectable_value(&mut self.serial_port, p.clone(), p);
                                }
                            });
                        refresh_serial = ui.button("刷新").clicked();
                    });
                    ui.horizontal(|ui| {
                        ui.label("波特率");
                        ui.add(egui::TextEdit::singleline(&mut self.baud).desired_width(90.0));
                    });
                    ui.checkbox(&mut self.keep_lines, "不改 DTR/RTS（不复位探针）");
                }
                Kind::Tcp => {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.tcp_addr)
                            .hint_text("IP 或 IP:端口，默认端口 3241")
                            .desired_width(fill_width(ui)),
                    );
                }
                Kind::Fake => {
                    ui.label("进程内的模拟探针，不需要硬件；有 ELF 时按 ELF 填内存");
                }
            }
            ui.horizontal(|ui| {
                ui.label("SWD 时钟");
                ui.add(egui::TextEdit::singleline(&mut self.clock).desired_width(70.0));
            });
        });
        if refresh_usb {
            self.refresh_usb();
        }
        if refresh_serial {
            self.refresh_serial();
        }
        ui.horizontal(|ui| {
            if self.connected {
                if ui.button("断开").clicked() {
                    self.send(Cmd::Disconnect);
                }
            } else if self.connecting {
                ui.add(egui::Spinner::new());
                ui.label("连接中……");
            } else if ui.button("连接").clicked() {
                self.connect();
            }
        });
        if self.connected {
            ui.label(RichText::new(&self.probe_desc).small());
            ui.label(RichText::new(&self.target_desc).small());
            if self.share.is_none() {
                self.ui_core(ui);
            }
            ui.separator();
            self.ui_share(ui);
        }
    }

    /// 共享为 USB DAP：开关、挂载命令、谁在用。
    fn ui_share(&mut self, ui: &mut egui::Ui) {
        let mut on = self.share.is_some();
        ui.horizontal(|ui| {
            let changed = ui
                .add_enabled(!self.share_busy, egui::Checkbox::new(&mut on, "共享为 USB DAP"))
                .on_hover_text(
                    "在本机 3240 端口提供 USB/IP 设备。用 usbip 挂载后，Keil、pyOCD、probe-rs、                     OpenOCD 把它当 USB CMSIS-DAP 探针用。共享期间这里不能读写变量和采样。                     Windows 需要先安装 usbip-win2。",
                )
                .changed();
            if self.share_busy {
                ui.add(egui::Spinner::new());
            }
            if changed {
                self.share_busy = true;
                self.send(Cmd::Share(on));
            }
        });
        let Some(st) = &self.share else {
            return;
        };
        ui.label(RichText::new(&st.device).small());
        let mut copy = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(&st.attach).monospace().small());
            copy = ui.small_button("复制").clicked();
        });
        if copy {
            ui.ctx().copy_text(st.attach.clone());
        }
        match st.client {
            Some(peer) => ui.label(
                RichText::new(format!("● 正被 {peer} 使用")).color(Color32::from_rgb(90, 200, 120)),
            ),
            None => ui.label(RichText::new("等待挂载（Linux 用 sudo 执行上面的命令）").weak()),
        };
    }

    /// 内核型号、运行状态和停住、继续、复位按钮。
    fn ui_core(&mut self, ui: &mut egui::Ui) {
        let (text, color) = match &self.core {
            None => ("读取中".to_string(), ui.visuals().weak_text_color()),
            Some(Err(e)) => (format!("读不到状态：{e}"), ui.visuals().error_fg_color),
            Some(Ok(s)) => {
                let c = match s.state {
                    CoreState::Running | CoreState::Sleeping => Color32::from_rgb(90, 200, 120),
                    CoreState::Halted => ui.visuals().warn_fg_color,
                    CoreState::Lockup => ui.visuals().error_fg_color,
                };
                (s.state.text().to_string(), c)
            }
        };
        ui.horizontal(|ui| {
            ui.label(format!("内核：{}", self.core_info));
            ui.label(RichText::new(format!("● {text}")).color(color));
        });
        let halted = matches!(&self.core, Some(Ok(s)) if s.state == CoreState::Halted);
        let mut cmd = None;
        ui.add_enabled_ui(!self.core_busy, |ui| {
            ui.horizontal(|ui| {
                if halted {
                    if ui.button("▶ 继续").clicked() {
                        cmd = Some(CoreCmd::Resume);
                    }
                } else if ui.button("⏸ 停住").clicked() {
                    cmd = Some(CoreCmd::Halt);
                }
                if ui
                    .button("↻ 复位")
                    .on_hover_text("系统复位后运行")
                    .clicked()
                {
                    cmd = Some(CoreCmd::Reset(AfterReset::Run));
                }
                if ui
                    .button("复位并停住")
                    .on_hover_text("系统复位，停在复位向量，第一条指令之前")
                    .clicked()
                {
                    cmd = Some(CoreCmd::Reset(AfterReset::Halt));
                }
                if self.core_busy {
                    ui.add(egui::Spinner::new());
                }
            });
        });
        if let Some(c) = cmd {
            self.core_busy = true;
            self.send(Cmd::Core(c));
        }
    }

    fn ui_elf(&mut self, ui: &mut egui::Ui) {
        ui.heading("ELF");
        let mut load = false;
        // 从右往左排：先放按钮，输入框占剩下的宽度，不会把面板越撑越宽
        right_to_left_row(ui, |ui| {
            load |= ui.button("重新读").clicked();
            if ui.button("…").on_hover_text("选择文件").clicked() {
                let mut d = rfd::FileDialog::new()
                    .add_filter("ELF", &["elf", "axf", "out"])
                    .add_filter("所有文件", &["*"]);
                if let Some(dir) = Path::new(self.elf_path.trim())
                    .parent()
                    .filter(|p| p.is_dir())
                {
                    d = d.set_directory(dir);
                }
                if let Some(p) = d.pick_file() {
                    self.elf_path = p.display().to_string();
                    load = true;
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.elf_path)
                    .hint_text("带 -g 调试信息的 ELF")
                    .desired_width(fill_width(ui)),
            );
        });
        if load {
            self.load_elf();
        }
    }

    fn ui_vars(&mut self, ui: &mut egui::Ui) {
        ui.heading("添加变量");
        let editable = !self.sampling && !self.starting;
        let mut add: Option<String> = None;
        ui.add_enabled_ui(editable, |ui| {
            right_to_left_row(ui, |ui| {
                let clicked = ui.button("添加").clicked();
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.expr)
                        .hint_text("表达式，例如 motor[1].pid->out")
                        .desired_width(fill_width(ui)),
                );
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if clicked || enter {
                    add = Some(std::mem::take(&mut self.expr));
                }
            });
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text(format!("在 {} 个全局变量里搜索", self.vars.len()))
                    .desired_width(fill_width(ui)),
            );
            ui.checkbox(&mut self.user_only, "只看自己的变量")
                .on_hover_text("隐藏 HAL、CMSIS、RTOS、中间件和 C 库里的变量（按源文件路径猜测）");
            let needle = self.search.trim().to_lowercase();
            let user_only = self.user_only;
            let hits: Vec<usize> = self
                .vars
                .iter()
                .enumerate()
                .filter(|(_, v)| !(user_only && v.system))
                .filter(|(_, v)| needle.is_empty() || v.lower.contains(&needle))
                .map(|(i, _)| i)
                .collect();
            let row_h = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical()
                .id_salt("vars")
                .auto_shrink([false, false])
                .show_rows(ui, row_h, hits.len(), |ui, range| {
                    for &i in &hits[range] {
                        let v = &self.vars[i];
                        ui.horizontal(|ui| {
                            if ui.small_button("+").clicked() {
                                add = Some(v.name.clone());
                            }
                            let size = v
                                .size
                                .map(|s| format!("{s} B"))
                                .unwrap_or_else(|| "?".into());
                            let kind = if v.system { "，第三方代码" } else { "" };
                            ui.label(&v.name)
                                .on_hover_text(format!("0x{:08X}  {}  {size}{kind}", v.addr, v.ty));
                            ui.label(RichText::new(&v.ty).weak().small());
                        });
                    }
                });
        });
        if let Some(name) = add {
            self.add_expr(&name, 1, true, 1, false);
        }
    }

    fn ui_controls(&mut self, ui: &mut egui::Ui) {
        let mut start = false;
        let mut read = false;
        let mut csv_on = false;
        let mut csv_off = false;
        let mut clear = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("周期");
            ui.add_enabled(
                !self.sampling && !self.starting,
                egui::TextEdit::singleline(&mut self.period).desired_width(70.0),
            )
            .on_hover_text("例如 1000（µs）、500us、2ms");
            if self.sampling {
                if ui.button("■ 停止").clicked() {
                    self.send(Cmd::Stop);
                }
            } else {
                let can = self.usable() && !self.starting && !self.watches.is_empty();
                start = ui
                    .add_enabled(can, egui::Button::new("▶ 开始采样"))
                    .clicked();
            }
            if self.starting {
                ui.add(egui::Spinner::new());
            }
            ui.add_enabled_ui(!self.sampling, |ui| {
                ui.checkbox(&mut self.stop_on_full, "缓冲满时丢新帧")
                    .on_hover_text("默认覆盖最旧的帧");
            });
            read = ui
                .add_enabled(self.usable(), egui::Button::new("读一次"))
                .clicked();
            ui.separator();
            if let Some(c) = &self.csv {
                csv_off = ui.button("停止录制").clicked();
                ui.label(format!("录制中 {} 行", c.rows));
            } else {
                csv_on = ui
                    .add_enabled(self.sampling, egui::Button::new("录制 CSV…"))
                    .clicked();
            }
            ui.separator();
            ui.label("窗口");
            egui::ComboBox::from_id_salt("window")
                .selected_text(format!("{} s", self.window_s))
                .width(70.0)
                .show_ui(ui, |ui| {
                    for &w in WINDOWS {
                        ui.selectable_value(&mut self.window_s, w, format!("{w} s"));
                    }
                });
            ui.toggle_value(&mut self.plot.normalize, "归一化")
                .on_hover_text(
                    "每条曲线按窗口里的最小、最大值缩放到同一高度，方便比较量级差很多的变量",
                );
            ui.toggle_value(&mut self.paused, "暂停跟随")
                .on_hover_text("暂停后可以拖动、缩放；数据照常接收");
            clear = ui.button("清空曲线").clicked();
        });
        if start {
            self.start();
        }
        if read {
            self.read_once();
        }
        if csv_on {
            self.start_csv();
        }
        if csv_off {
            self.stop_csv();
        }
        if clear {
            for w in &mut self.watches {
                w.series.clear();
            }
        }
        if self.sampling {
            let s = self.stats;
            let warn = s.dropped > 0 || s.skipped > 0 || s.missing > 0 || s.failed_values > 0;
            let text = format!(
                "{:.0} 帧/s，共 {} 帧，单帧估计 {} µs；探针丢帧 {}，跳拍 {}，缺帧 {}，失败值 {}",
                self.rate,
                s.frames,
                self.est_frame_us,
                s.dropped,
                s.skipped,
                s.missing,
                s.failed_values
            );
            let text = if warn {
                RichText::new(text).color(ui.visuals().warn_fg_color)
            } else {
                RichText::new(text)
            };
            ui.label(text);
            if let Some(Ok(c)) = &self.core {
                if matches!(c.state, CoreState::Halted | CoreState::Lockup) {
                    ui.label(
                        RichText::new(format!("目标内核{}，变量不会变化", c.state.text()))
                            .color(ui.visuals().warn_fg_color),
                    );
                }
            }
        }
    }

    fn ui_watches(&mut self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};
        let frozen = self.sampling || self.starting;
        let mut remove = None;
        let mut writes = Vec::new();
        let row_h = ui.spacing().interact_size.y + 2.0;
        let watches = &mut self.watches;
        let max_h = ui.available_height();
        TableBuilder::new(ui)
            .id_salt("watches")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, true])
            .max_scroll_height(max_h)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(40.0))
            .column(Column::initial(150.0).at_least(60.0).clip(true))
            .column(Column::initial(70.0).at_least(40.0).clip(true))
            .column(Column::exact(50.0))
            .column(Column::exact(44.0))
            .column(Column::initial(170.0).at_least(60.0).clip(true))
            .column(Column::exact(130.0))
            .column(Column::remainder().at_least(40.0).clip(true))
            .header(row_h, |mut h| {
                for title in ["", "变量", "类型", "分频", "图", "当前值", "写入", ""] {
                    h.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, watches.len(), |mut row| {
                    let i = row.index();
                    let w = &mut watches[i];
                    row.col(|ui| {
                        ui.checkbox(&mut w.show, "").on_hover_text("画曲线");
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 2.0, w.color);
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(&w.target.name)
                                .selectable(false)
                                .truncate(),
                        )
                        .on_hover_text(w.target.location());
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&w.target.ty.name).weak())
                                .selectable(false)
                                .truncate(),
                        );
                    });
                    row.col(|ui| {
                        ui.add_enabled(
                            !frozen,
                            egui::DragValue::new(&mut w.div).range(1..=65535).speed(0.2),
                        )
                        .on_hover_text("每几拍采一次");
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut w.chart)
                                .range(1..=MAX_CHARTS)
                                .speed(0.05),
                        )
                        .on_hover_text("画在第几张图，几张图的时间轴联动");
                    });
                    row.col(|ui| {
                        // 值刚变过时短暂高亮
                        if let Some(t) = w.changed_at {
                            let age = t.elapsed().as_secs_f32();
                            if age < 0.4 {
                                let a = ((0.4 - age) / 0.4 * 36.0) as u8;
                                let rect = ui.max_rect();
                                ui.painter().rect_filled(
                                    rect,
                                    2.0,
                                    Color32::from_rgba_unmultiplied(255, 200, 80, a),
                                );
                            }
                        }
                        let value = match (&w.latest, w.failed) {
                            (_, true) => {
                                RichText::new("读取失败").color(ui.visuals().error_fg_color)
                            }
                            (Some(b), _) => RichText::new(w.target.ty.format(b)).monospace(),
                            (None, _) => RichText::new("-").weak(),
                        };
                        ui.add(egui::Label::new(value).selectable(false).truncate());
                    });
                    row.col(|ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut w.write_text)
                                .id_salt(("write", w.id))
                                .desired_width(90.0),
                        );
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.small_button("写").clicked() || enter {
                            writes.push(i);
                        }
                    });
                    row.col(|ui| {
                        if ui
                            .add_enabled(!frozen, egui::Button::new("删").small())
                            .on_hover_text("移除")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                        ui.add(
                            egui::Label::new(RichText::new(&w.note).small())
                                .selectable(false)
                                .truncate(),
                        )
                        .on_hover_text(&w.note);
                    });
                });
            });
        if let Some(i) = remove {
            self.watches.remove(i);
        }
        for i in writes {
            if !self.usable() {
                self.watches[i].note = if self.connected {
                    "探针正在共享为 USB DAP".into()
                } else {
                    "还没有连接".into()
                };
                continue;
            }
            let w = &mut self.watches[i];
            match w.target.ty.parse(&w.write_text) {
                Ok(bytes) => {
                    w.note = "写入中……".into();
                    let t = w.target.clone();
                    self.send(Cmd::Write(t, bytes));
                }
                Err(e) => w.note = e.to_string(),
            }
        }
    }

    fn ui_plot(&mut self, ui: &mut egui::Ui) {
        let curves: Vec<Curve> = self
            .watches
            .iter()
            .filter(|w| w.show)
            .map(|w| Curve {
                name: &w.target.name,
                color: w.color,
                chart: w.chart,
                series: &w.series,
                float: matches!(w.target.ty.kind, ScalarKind::Float),
            })
            .collect();
        self.plot.ui_cursors(ui, &curves);
        let t_last = self.t_last.unwrap_or(0.0);
        self.plot
            .ui_plots(ui, &curves, !self.paused, t_last, self.window_s);
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
        storage.set_string("xrdap.baud", self.baud.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_var_guess() {
        assert!(is_system_var(
            "hspi1",
            "../Drivers/STM32G4xx_HAL_Driver/Src/stm32g4xx_hal_spi.c"
        ));
        assert!(is_system_var(
            "pxCurrentTCB",
            r"Middlewares\Third_Party\FreeRTOS\Source\tasks.c"
        ));
        assert!(is_system_var(
            "__malloc_av_",
            "newlib/libc/stdlib/mallocr.c"
        ));
        assert!(!is_system_var("motor", "Core/Src/main.c"));
        assert!(!is_system_var("pid_cfg", "User/app/pid.cpp"));
    }
}
