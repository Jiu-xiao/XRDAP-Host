//! 探针的 Wi-Fi：当前状态，保存或清除热点参数（经 DAP 的 0x86 命令，不用另开串口终端）。

use eframe::egui::{self, RichText};

use super::super::app::App;
use super::super::theme;
use super::super::worker::{Cmd, WifiChange};

impl App {
    /// 保存热点参数的命令。密码从界面状态里取走，交给后台线程用完即丢，界面不再留着它
    /// （保存成功还是失败都一样）。
    pub(crate) fn wifi_set_cmd(&mut self) -> Cmd {
        Cmd::WifiApply(WifiChange::Set {
            ssid: self.wifi_ssid.trim().to_string(),
            password: std::mem::take(&mut self.wifi_password),
        })
    }

    /// Wi-Fi 状态和热点参数表单。保存、清除后探针重启，连接随之断开。
    pub(super) fn ui_wifi(&mut self, ui: &mut egui::Ui) {
        let mut cmd = None;
        egui::CollapsingHeader::new("Wi-Fi")
            .id_salt("wifi")
            .show(ui, |ui| {
                match &self.wifi {
                    None => ui.label(RichText::new("读取中").weak()),
                    Some(Err(e)) => {
                        theme::fail(ui, format!("读不到（固件不支持或读取失败）：{e}"));
                        ui.label("")
                    }
                    Some(Ok(st)) => {
                        ui.label(st.describe());
                        ui.label(RichText::new(st.history()).weak().small())
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
                            .on_hover_text(
                                "探针保存热点参数后约 0.5 秒重启，当前连接会断开。\n                                 经 TCP（Wi-Fi）连接时，密码以明文经过局域网",
                            )
                            .clicked()
                        {
                            cmd = Some(self.wifi_set_cmd());
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
