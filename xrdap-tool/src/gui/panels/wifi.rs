//! 探针的 Wi-Fi：当前状态，保存或清除热点参数（经 DAP 的 0x86 命令，不用另开串口终端）。

use eframe::egui::{self, Color32, RichText};

use super::super::app::App;
use super::super::worker::{Cmd, WifiChange};

impl App {
    /// Wi-Fi 状态和热点参数表单。保存、清除后探针重启，连接随之断开。
    pub(super) fn ui_wifi(&mut self, ui: &mut egui::Ui) {
        let mut cmd = None;
        egui::CollapsingHeader::new("Wi-Fi")
            .id_salt("wifi")
            .show(ui, |ui| {
                match &self.wifi {
                    None => ui.label(RichText::new("读取中").weak()),
                    Some(Err(e)) => ui.label(
                        RichText::new(format!("读不到（固件不支持或读取失败）：{e}"))
                            .color(ui.visuals().error_fg_color),
                    ),
                    Some(Ok(st)) => {
                        let color = if st.connected {
                            Color32::from_rgb(90, 200, 120)
                        } else {
                            ui.visuals().warn_fg_color
                        };
                        ui.label(RichText::new(format!("● {}", st.describe())).color(color))
                    }
                };
                egui::Grid::new("wifi_form").num_columns(2).show(ui, |ui| {
                    ui.label("热点");
                    ui.add(egui::TextEdit::singleline(&mut self.wifi_ssid).desired_width(160.0));
                    ui.end_row();
                    ui.label("密码");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.wifi_password)
                            .password(true)
                            .hint_text("开放网络不填")
                            .desired_width(160.0),
                    );
                    ui.end_row();
                });
                ui.add_enabled_ui(!self.wifi_busy, |ui| {
                    ui.horizontal(|ui| {
                        let ssid_ok = !self.wifi_ssid.trim().is_empty();
                        if ui
                            .add_enabled(ssid_ok, egui::Button::new("保存并重启"))
                            .on_hover_text("探针保存热点参数后约 0.5 秒重启，当前连接会断开")
                            .clicked()
                        {
                            cmd = Some(Cmd::WifiApply(WifiChange::Set {
                                ssid: self.wifi_ssid.trim().to_string(),
                                password: self.wifi_password.clone(),
                            }));
                        }
                        if ui
                            .button("清除")
                            .on_hover_text("清除热点参数，探针重启后不再连 Wi-Fi")
                            .clicked()
                        {
                            cmd = Some(Cmd::WifiApply(WifiChange::Clear));
                        }
                        if ui.button("刷新").clicked() {
                            cmd = Some(Cmd::WifiStatus);
                        }
                        if self.wifi_busy {
                            ui.add(egui::Spinner::new());
                        }
                    });
                });
            });
        if let Some(c) = cmd {
            self.wifi_busy = true;
            self.send(c);
        }
    }
}
