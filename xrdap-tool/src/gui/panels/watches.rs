//! 中间的监视表：开关、分频、所在图、当前值、写入、删除。

use eframe::egui::{self, RichText};

use crate::gui::app::App;
use crate::gui::plot::MAX_CHARTS;
use crate::gui::theme::Palette;
use crate::gui::worker::Cmd;

impl App {
    pub(crate) fn ui_watches(&mut self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};
        let frozen = self.sampling || self.starting;
        let palette = Palette::current(ui);
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
                        ui.painter()
                            .rect_filled(rect, 0.0, palette.channel(w.channel));
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
                                // 刚变过的值底色短暂加深（墨色，不用彩色）
                                let fade = (0.4 - age) / 0.4 * 0.15;
                                ui.painter().rect_filled(
                                    ui.max_rect(),
                                    0.0,
                                    palette.ink.gamma_multiply(fade),
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
                        if ui.small_button("写入").clicked() || enter {
                            writes.push(i);
                        }
                    });
                    row.col(|ui| {
                        if ui
                            .add_enabled(!frozen, egui::Button::new("删除").small())
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
}
