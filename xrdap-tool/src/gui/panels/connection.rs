//! 左上：找到的探针、连接表单、内核状态和按钮、共享开关。

use std::path::PathBuf;
use std::sync::mpsc::channel;

use eframe::egui::{self, Color32, RichText};
use xrdap::cortex::{AfterReset, CoreState};
use xrdap::discover;
use xrdap::open::Addr;

use crate::gui::app::App;
use crate::gui::model::Kind;
use crate::gui::widgets::{fill_width, short_serial};
use crate::gui::worker::{Cmd, Conn, CoreCmd};

impl App {
    pub(crate) fn refresh_usb(&mut self) {
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

    pub(crate) fn refresh_serial(&mut self) {
        match xrdap::transport::serial::available_ports() {
            Ok(ports) => self.serial_ports = ports,
            Err(e) => self.log(e.to_string()),
        }
    }

    fn conn(&self) -> Result<Conn, String> {
        Ok(match self.kind {
            Kind::Usb => Conn::Probe {
                addr: Addr::Usb {
                    serial: Some(self.usb_serial.trim().to_string()).filter(|s| !s.is_empty()),
                },
                baud: None,
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
                    baud: match self.baud.trim() {
                        "" => None,
                        b => Some(b.parse().map_err(|_| "波特率不是整数".to_string())?),
                    },
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
                    baud: None,
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

    /// 在后台找 USB、串口、局域网上的探针，找完唤醒界面。
    pub(crate) fn start_finding(&mut self, ctx: &egui::Context) {
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

    pub(crate) fn ui_connection(&mut self, ui: &mut egui::Ui) {
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
                        ui.add(
                            egui::TextEdit::singleline(&mut self.baud)
                                .hint_text("自动")
                                .desired_width(90.0),
                        )
                        .on_hover_text(
                            "不填时按串口芯片自动选：CH343 等 4000000，CP2102、CH340 921600",
                        );
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
                    egui::Checkbox::new(&mut on, "共享为 USB DAP"),
                )
                .on_hover_text(
                    "在本机 3240 端口提供 USB/IP 设备。用 usbip 挂载后，Keil、pyOCD、probe-rs、
                     OpenOCD 把它当 USB CMSIS-DAP 探针用。共享期间这里不能读写变量和采样。
                     Windows 需要先安装 usbip-win2。",
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
}
