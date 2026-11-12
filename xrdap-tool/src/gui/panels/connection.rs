//! 左上：找到的探针、探针说明、内核状态和按钮、共享开关。

use std::sync::mpsc::channel;

use eframe::egui::{self, RichText};
use xrdap::cortex::{AfterReset, CoreState};
use xrdap::discover::{self, Chip};
use xrdap::lang::tr;
use xrdap::trf;

use crate::gui::app::App;
use crate::gui::model::Found;
use crate::gui::theme;
use crate::gui::widgets::{fill_width, short_serial};
use crate::gui::worker::{Cmd, Conn, CoreCmd};

impl App {
    fn conn(&self) -> Result<Conn, String> {
        Ok(Conn {
            spec: self.probe_spec.trim().to_string(),
            baud: match self.baud.trim() {
                "" => None,
                b => Some(b.parse().map_err(|_| {
                    tr("The baud rate is not an integer", "波特率不是整数").to_string()
                })?),
            },
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
                let which = if conn.spec.is_empty() {
                    tr("the automatically selected probe", "自动选择的探针")
                } else {
                    &conn.spec
                };
                self.log(trf!(
                    "Connecting {which}, SWD {clock} Hz...",
                    "连接 {which}，SWD {clock} Hz……"
                ));
                self.connecting = true;
                self.send(Cmd::Connect(conn, clock));
            }
            Err(e) => self.log(e),
        }
    }

    /// 在后台找 USB、局域网上的探针和系统列出的串口（不打开串口），找完唤醒界面。
    pub(crate) fn start_finding(&mut self) {
        if self.finding.is_some() || self.connected {
            return;
        }
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let probes = discover::group(discover::find(&discover::Options::default()));
            let others = discover::serial_ports()
                .into_iter()
                .filter(|p| p.chip != Chip::EspressifUsb)
                .collect();
            let _ = tx.send(Found { probes, others });
            ctx.request_repaint();
        });
        self.finding = Some(rx);
    }

    /// 找到的探针：每个连接一行，点一下把它的说明填进“探针”。下面是没有打开检查的串口。
    fn ui_found(&mut self, ui: &mut egui::Ui) {
        if let Some(rx) = &self.finding {
            if let Ok(found) = rx.try_recv() {
                self.found = found;
                self.finding = None;
            }
        }
        ui.horizontal(|ui| {
            ui.label(tr("Found probes", "找到的探针"));
            if self.finding.is_some() {
                ui.add(egui::Spinner::new());
            } else if ui.small_button(tr("Search again", "重新查找")).clicked() {
                self.start_finding();
            }
        });
        let mut chosen = None;
        if self.found.probes.is_empty() && self.finding.is_none() {
            ui.label(
                RichText::new(tr("None found on USB or the LAN", "USB 和局域网上没有找到"))
                    .weak()
                    .small(),
            );
        }
        for p in &self.found.probes {
            let name = p
                .serial
                .as_deref()
                .unwrap_or(tr("(no serial number)", "（没有序列号）"));
            let product = p.product.as_deref().unwrap_or("");
            for l in &p.links {
                let text = format!("{product}  {}  ·  {}", short_serial(name), l.addr);
                if ui
                    .selectable_label(false, RichText::new(text).small())
                    .on_hover_text(format!("{name}\n{}", l.note.as_deref().unwrap_or("")))
                    .clicked()
                {
                    chosen = Some(l.addr.spec());
                }
            }
        }
        if !self.found.others.is_empty() {
            ui.label(
                RichText::new(tr(
                    "Other serial ports (not opened to check; click one to use it)",
                    "其他串口（没有打开检查，点一下选用）",
                ))
                .weak()
                .small(),
            );
            ui.horizontal_wrapped(|ui| {
                for s in &self.found.others {
                    if ui
                        .selectable_label(false, RichText::new(&s.port).small())
                        .on_hover_text(s.chip.name())
                        .clicked()
                    {
                        chosen = Some(s.port.clone());
                    }
                }
            });
        }
        if let Some(spec) = chosen {
            self.probe_spec = spec;
        }
    }

    pub(crate) fn ui_connection(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "XRDAP / PROBE", tr("Connection", "连接"));
        if !self.connected && !self.connecting {
            self.ui_found(ui);
            ui.separator();
        }
        let editable = !self.connected && !self.connecting;
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                ui.label(tr("Probe", "探针"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.probe_spec)
                        .hint_text(tr("Empty: the only probe", "不填时自动选唯一的探针"))
                        .desired_width(fill_width(ui)),
                )
                .on_hover_text(tr(
                    "Serial number (in full or its last digits), usb, COM3, /dev/ttyUSB0, 192.0.2.10, \
                     xrdap-aabbcc.local; the same forms as -p on the command line",
                    "序列号（全称或末尾几位）、usb、COM3、/dev/ttyUSB0、192.0.2.10、\
                     xrdap-aabbcc.local，和命令行的 -p 写法相同",
                ));
            });
            ui.horizontal(|ui| {
                ui.label(tr("Serial baud rate", "串口波特率"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.baud)
                        .hint_text(tr("Auto", "自动"))
                        .desired_width(80.0),
                )
                .on_hover_text(tr(
                    "Empty: chosen by serial chip: CH343 and similar 4000000, CP2102 and CH340 921600, other chips try both",
                    "不填时按串口芯片自动选：CH343 等 4000000，CP2102、CH340 921600，其他芯片两个都试",
                ));
                ui.label(tr("SWD clock", "SWD 时钟"));
                ui.add(egui::TextEdit::singleline(&mut self.clock).desired_width(60.0));
            });
        });
        ui.horizontal(|ui| {
            if self.connected {
                if ui.button(tr("Disconnect", "断开")).clicked() {
                    self.send(Cmd::Disconnect);
                }
            } else if self.connecting {
                ui.add(egui::Spinner::new());
                ui.label(tr("Connecting...", "连接中……"));
            } else if theme::primary_button(ui, tr("Connect", "连接")).clicked() {
                // 没连上时，“连接”是这个窗口唯一的主按钮
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
            if self.share.is_none() {
                ui.separator();
                self.ui_wifi(ui);
            }
        }
    }

    /// 共享为 USB DAP：开关、挂载命令、谁在用。
    fn ui_share(&mut self, ui: &mut egui::Ui) {
        let mut on = self.share.is_some();
        ui.horizontal(|ui| {
            let changed = ui
                .add_enabled(
                    !self.share_busy,
                    egui::Checkbox::new(&mut on, tr("Share as USB DAP", "共享为 USB DAP")),
                )
                .on_hover_text(tr(
                    "Serves a USB/IP device on port 3240 of this computer. Once attached with usbip, \
                     Keil, pyOCD, probe-rs and OpenOCD use it as a USB CMSIS-DAP probe. While \
                     sharing, this window cannot read or write variables or sample. Windows needs \
                     usbip-win2 installed first.",
                    "在本机 3240 端口提供 USB/IP 设备。用 usbip 挂载后，Keil、pyOCD、probe-rs、\
                     OpenOCD 把它当 USB CMSIS-DAP 探针用。共享期间这里不能读写变量和采样。\
                     Windows 需要先安装 usbip-win2。",
                ))
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
            copy = ui.small_button(tr("Copy", "复制")).clicked();
        });
        if copy {
            ui.ctx().copy_text(st.attach.clone());
        }
        match st.client {
            Some(peer) => {
                ui.horizontal(|ui| {
                    theme::tag(ui, "IN USE", true);
                    ui.label(trf!("In use by {peer}", "正被 {peer} 使用"));
                });
            }
            None => {
                ui.label(
                    RichText::new(tr(
                        "Waiting for attach (on Linux run the command above with sudo)",
                        "等待挂载（Linux 用 sudo 执行上面的命令）",
                    ))
                    .weak(),
                );
            }
        }
    }

    /// 内核型号、运行状态和停住、继续、复位按钮。
    fn ui_core(&mut self, ui: &mut egui::Ui) {
        ui.label(trf!("Core: {}", "内核：{}", self.core_info));
        match &self.core {
            None => {
                ui.label(RichText::new(tr("Reading", "读取中")).weak());
            }
            Some(Err(e)) => theme::fail(ui, trf!("Cannot read the state: {e}", "读不到状态：{e}")),
            // 锁死是一个结论，用状态色；其余状态用墨色记号，停住时反相强调
            Some(Ok(s)) if s.state == CoreState::Lockup => theme::fail(ui, s.state.text()),
            Some(Ok(s)) => {
                theme::tag(ui, s.state.text(), s.state == CoreState::Halted);
            }
        }
        let halted = matches!(&self.core, Some(Ok(s)) if s.state == CoreState::Halted);
        let mut cmd = None;
        ui.add_enabled_ui(!self.core_busy, |ui| {
            ui.horizontal(|ui| {
                if halted {
                    if ui.button(tr("Resume", "继续")).clicked() {
                        cmd = Some(CoreCmd::Resume);
                    }
                } else if ui.button(tr("Halt", "停住")).clicked() {
                    cmd = Some(CoreCmd::Halt);
                }
                if ui
                    .button(tr("Reset", "复位"))
                    .on_hover_text(tr("System reset, then run", "系统复位后运行"))
                    .clicked()
                {
                    cmd = Some(CoreCmd::Reset(AfterReset::Run));
                }
                if ui
                    .button(tr("Reset and halt", "复位并停住"))
                    .on_hover_text(tr(
                        "System reset, stop at the reset vector before the first instruction",
                        "系统复位，停在复位向量，第一条指令之前",
                    ))
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
}
