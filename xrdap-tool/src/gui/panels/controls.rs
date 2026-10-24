//! 中间上方：采样周期、开始停止、读一次、CSV 录制、曲线窗口和采样统计。

use eframe::egui::{self, RichText};
use xrdap::cortex::CoreState;

use crate::gui::app::App;
use crate::gui::model::Csv;
use crate::gui::theme;
use crate::gui::widgets::unix_us_now;
use crate::gui::worker::Cmd;

const WINDOWS: &[f64] = &[0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0];

impl App {
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

    pub(crate) fn start_csv(&mut self) {
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
        self.log(format!("开始录制 {}", path.display()));
        self.csv = Some(Csv::start(&path, line, start_unix_us));
    }

    pub(crate) fn stop_csv(&mut self) {
        if let Some(c) = self.csv.take() {
            let path = c.path.clone();
            match c.finish() {
                Ok(rows) => self.log(format!("录制结束：{}，{rows} 行", path.display())),
                Err(e) => self.log(format!("写 {} 失败：{e}", path.display())),
            }
        }
    }

    /// 现在不能开始采样的原因（按钮保持可点，点了说明原因）。
    fn why_not_start(&self) -> &'static str {
        if !self.connected {
            "还没有连接探针"
        } else if self.share.is_some() {
            "探针正在共享给别的调试工具，先停止共享"
        } else if self.elf_loading.is_some() {
            "ELF 还在读取"
        } else if self.watches.is_empty() {
            "监视表是空的，先添加变量"
        } else {
            "正在开始采样"
        }
    }

    pub(crate) fn ui_controls(&mut self, ui: &mut egui::Ui) {
        theme::path_label(ui, "XRDAP / WATCH");
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
                if ui.button("停止采样").clicked() {
                    self.send(Cmd::Stop);
                }
            } else {
                // 读 ELF 期间监视表还没定下来
                let can = self.usable()
                    && !self.starting
                    && !self.watches.is_empty()
                    && self.elf_loading.is_none();
                // 连上探针以后，“开始采样”是窗口里唯一的主按钮；还不能开始时点了说明原因
                start = if self.usable() {
                    theme::primary_button(ui, "开始采样").clicked()
                } else {
                    ui.button("开始采样").clicked()
                };
                if start && !can {
                    start = false;
                    self.log(self.why_not_start().into());
                }
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
            let text = format!(
                "{:.0} 帧/s，共 {} 帧，缓冲 {}/{} 字节；探针丢帧 {}，跳拍 {}，缺帧 {}，失败值 {}",
                self.rate,
                s.frames,
                s.backlog,
                s.ring_bytes,
                s.dropped,
                s.skipped,
                s.missing,
                s.failed_values
            );
            ui.label(RichText::new(text).weak());
            // 跟不上是一个结论（协议文档 3.5 节），和建议一起出现
            if let Some(lag) = s.lag() {
                theme::fail(ui, lag.advice());
            }
            if let Some(Ok(c)) = &self.core {
                if matches!(c.state, CoreState::Halted | CoreState::Lockup) {
                    ui.label(
                        RichText::new(format!("目标内核{}，变量不会变化", c.state.text())).strong(),
                    );
                }
            }
        }
    }
}
