//! 左下：ELF 文件和全局变量搜索、添加到监视表。

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use xrdap::expr::Resolver;
use xrdap::types::ScalarKind;

use crate::gui::app::App;
use crate::gui::model::{is_system_var, VarRow, Watch, KEEP_S, MAX_POINTS};
use crate::gui::plot::MAX_CHARTS;
use crate::gui::series::Series;
use crate::gui::widgets::{color_for, fill_width, right_to_left_row};

impl App {
    pub(crate) fn load_elf(&mut self) {
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
    pub(crate) fn add_expr(&mut self, text: &str, div: u16, show: bool, chart: u8, quiet: bool) {
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

    pub(crate) fn ui_elf(&mut self, ui: &mut egui::Ui) {
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

    pub(crate) fn ui_vars(&mut self, ui: &mut egui::Ui) {
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
}
