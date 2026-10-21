//! 左下：ELF 文件和全局变量搜索、添加到监视表。ELF 在后台线程里读，界面不卡。

use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;

use eframe::egui::{self, RichText};
use xrdap::expr::Resolver;
use xrdap::types::{ScalarKind, SymbolTable};

use crate::gui::app::App;
use crate::gui::model::{is_system_var, VarRow, Watch, KEEP_S, MAX_POINTS};
use crate::gui::plot::MAX_CHARTS;
use crate::gui::series::Series;
use crate::gui::widgets::{color_for, fill_width, right_to_left_row};

/// 后台读完的 ELF：路径，和符号表加变量表，或读失败的原因。
pub(crate) type ElfLoaded = (PathBuf, Result<(SymbolTable, Vec<VarRow>), String>);

/// 保存在设置里的一个监视项：名字、分频、是否画曲线、画在第几张图。
pub(crate) struct SavedWatch {
    pub(crate) name: String,
    pub(crate) div: u16,
    pub(crate) show: bool,
    pub(crate) chart: u8,
}

impl SavedWatch {
    pub(crate) fn of(w: &Watch) -> Self {
        Self {
            name: w.target.name.clone(),
            div: w.div,
            show: w.show,
            chart: w.chart,
        }
    }

    /// 设置里的一行：名字、分频、是否画、第几张图，用制表符隔开。
    pub(crate) fn line(&self) -> String {
        let show = u8::from(self.show);
        format!("{}\t{}\t{show}\t{}", self.name, self.div, self.chart)
    }

    pub(crate) fn parse(line: &str) -> Option<Self> {
        let mut parts = line.split('\t');
        Some(Self {
            name: parts.next().filter(|n| !n.is_empty())?.to_string(),
            div: parts.next().and_then(|d| d.parse().ok()).unwrap_or(1),
            show: parts.next() != Some("0"),
            chart: parts.next().and_then(|c| c.parse().ok()).unwrap_or(1),
        })
    }
}

fn var_rows(syms: &SymbolTable) -> Vec<VarRow> {
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
    vars
}

impl App {
    /// 在后台读 ELF。采样时不读：监视项按新 ELF 重新解析后，正在采样的条目就对不上了。
    pub(crate) fn load_elf(&mut self) {
        if self.sampling || self.starting {
            self.log("采样中不能重新读 ELF，先停止采样".into());
            return;
        }
        if self.elf_loading.is_some() {
            return;
        }
        let path = PathBuf::from(self.elf_path.trim());
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let loaded = xrdap::elf::load(&path)
                .map(|syms| {
                    let vars = var_rows(&syms);
                    (syms, vars)
                })
                .map_err(|e| e.to_string());
            let _ = tx.send((path, loaded));
            ctx.request_repaint();
        });
        self.elf_loading = Some(rx);
    }

    /// 后台读完 ELF 时换上新的符号表，已有的监视项按名字重新解析。
    pub(crate) fn poll_elf(&mut self) {
        let Some(rx) = &self.elf_loading else {
            return;
        };
        let Ok((path, loaded)) = rx.try_recv() else {
            return;
        };
        self.elf_loading = None;
        let mut keep: Vec<SavedWatch> =
            self.watches.drain(..).map(|w| SavedWatch::of(&w)).collect();
        keep.append(&mut self.restore);
        match loaded {
            Ok((syms, vars)) => {
                self.log(format!(
                    "读入 {}：{} 个全局变量",
                    path.display(),
                    vars.len()
                ));
                self.syms = syms;
                self.vars = vars;
            }
            Err(e) => self.log(format!("读 ELF 失败：{e}")),
        }
        // 地址和类型以新 ELF 为准
        for w in keep {
            self.add_expr(&w.name, w.div, w.show, w.chart, true);
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
        // 采样时不能换 ELF，读的过程中也不能再读
        let idle = !self.sampling && !self.starting && self.elf_loading.is_none();
        right_to_left_row(ui, |ui| {
            if self.elf_loading.is_some() {
                ui.add(egui::Spinner::new());
            }
            load |= ui
                .add_enabled(idle, egui::Button::new("重新读"))
                .on_disabled_hover_text("采样中不能重新读 ELF")
                .clicked();
            if ui
                .add_enabled(idle, egui::Button::new("…"))
                .on_hover_text("选择文件")
                .clicked()
            {
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
