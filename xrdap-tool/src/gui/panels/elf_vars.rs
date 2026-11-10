//! 左下：ELF 文件和变量搜索、添加到监视表。ELF 在后台线程里读，界面不卡。

use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;

use eframe::egui::{self, RichText};
use xrdap::expr::Resolver;
use xrdap::types::{Member, ScalarKind, SymbolTable, Type, TypeId};

use crate::gui::app::App;
use crate::gui::model::{is_system_var, TreeMatches, VarRow, VarTree, Watch, KEEP_S, MAX_POINTS};
use crate::gui::plot::MAX_CHARTS;
use crate::gui::series::Series;
use crate::gui::theme;
use crate::gui::widgets::{fill_width, right_to_left_row};

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
            ty_id: v.ty,
            size: syms.size_of(v.ty),
            system: is_system_var(&v.name, &v.unit),
        })
        .collect();
    vars.sort_by(|a, b| a.name.cmp(&b.name).then(a.addr.cmp(&b.addr)));
    vars
}

/// 成员树里的一个节点类型：一个真的 TypeId，或者还没取完下标的数组。
///
/// A node type in the member tree: a real TypeId, or an array with dimensions left to index.
#[derive(Clone)]
enum NodeTy {
    Ty(TypeId),
    /// 逐维取下标：`dims` 是还没用掉的那几维
    Arr {
        elem: TypeId,
        dims: Vec<Option<u64>>,
    },
}

/// 展开 typedef、const/volatile 之类不改变访问的包装，得到真正的类型；只有声明的结构体换成
/// 别的编译单元里的完整定义，和读写变量时一样。
fn effective(syms: &SymbolTable, id: TypeId) -> TypeId {
    syms.complete(id)
}

/// 一个变量或成员的类型，数组转成 [`NodeTy::Arr`]。
fn node_ty(syms: &SymbolTable, id: TypeId) -> NodeTy {
    match syms.get(effective(syms, id)) {
        Type::Array { elem, dims } => NodeTy::Arr {
            elem: *elem,
            dims: dims.clone(),
        },
        _ => NodeTy::Ty(id),
    }
}

fn node_name(syms: &SymbolTable, ty: &NodeTy) -> String {
    match ty {
        NodeTy::Ty(id) => syms.name_of(*id),
        NodeTy::Arr { elem, dims } => {
            let mut s = syms.name_of(*elem);
            for d in dims {
                match d {
                    Some(n) => s.push_str(&format!("[{n}]")),
                    None => s.push_str("[]"),
                }
            }
            s
        }
    }
}

/// 数组最多展开这么多个元素，再多的只提示数量。
const MAX_ARRAY_ROWS: u64 = 64;
/// 筛选结果最多列出这么多项。
const MAX_MATCHES: usize = 300;
/// 成员树的深度上限，防止递归回到自己（指针不展开，理论上不会出现）。
const MAX_DEPTH: usize = 24;

/// 匿名 struct/union 成员没有名字，它的字段可以直接访问，所以把字段平铺到 inv 这一层。
fn push_members(
    syms: &SymbolTable,
    members: &[Member],
    out: &mut Vec<(String, NodeTy, bool)>,
    depth: usize,
) {
    for m in members {
        if let Some(n) = &m.name {
            // 编译器生成的成员（虚函数表指针 _vptr.Port）名字里有点，写不成表达式
            if n.contains('.') {
                continue;
            }
            out.push((format!(".{n}"), node_ty(syms, m.ty), m.bit_size.is_some()));
        } else if depth < 8 {
            if let Type::Struct {
                members,
                declaration: false,
                ..
            } = syms.get(effective(syms, m.ty))
            {
                push_members(syms, members, out, depth + 1);
            }
        }
    }
}

/// 一个节点的子节点：结构体成员、数组元素。空表示这是叶子。
fn node_children(syms: &SymbolTable, ty: &NodeTy) -> Vec<(String, NodeTy, bool)> {
    let mut out = Vec::new();
    match ty {
        NodeTy::Arr { elem, dims } => {
            let Some(n) = dims.first().copied().flatten() else {
                return out;
            };
            let rest: Vec<Option<u64>> = dims.iter().skip(1).copied().collect();
            for i in 0..n.min(MAX_ARRAY_ROWS) {
                let child = if rest.is_empty() {
                    NodeTy::Ty(*elem)
                } else {
                    NodeTy::Arr {
                        elem: *elem,
                        dims: rest.clone(),
                    }
                };
                out.push((format!("[{i}]"), child, false));
            }
        }
        NodeTy::Ty(id) => match syms.get(effective(syms, *id)) {
            Type::Struct {
                members,
                declaration: false,
                ..
            } => push_members(syms, members, &mut out, 0),
            Type::Array { elem, dims } => {
                return node_children(
                    syms,
                    &NodeTy::Arr {
                        elem: *elem,
                        dims: dims.clone(),
                    },
                )
            }
            _ => {}
        },
    }
    out
}

/// 画树时的上下文：符号表、能否添加（采样时不能），以及这一帧点中的表达式。
struct TreeCtx<'a> {
    syms: &'a SymbolTable,
    editable: bool,
    add: Option<String>,
}

impl TreeCtx<'_> {
    /// 画成员树的这一层。`path` 是从根变量算起的完整表达式。
    fn node(&mut self, ui: &mut egui::Ui, path: &str, label: &str, ty: NodeTy, depth: usize) {
        let children = node_children(self.syms, &ty);
        if children.is_empty() || depth >= MAX_DEPTH {
            ui.horizontal(|ui| {
                if self.editable && ui.small_button("添加").clicked() {
                    self.add = Some(path.to_string());
                }
                ui.add(egui::Label::new(label).truncate())
                    .on_hover_text(format!("{path}  {}", node_name(self.syms, &ty)));
                ui.label(RichText::new(node_name(self.syms, &ty)).weak().small());
            });
            return;
        }
        egui::CollapsingHeader::new(label)
            .id_salt(("var_tree_node", path))
            .default_open(depth == 0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if self.editable && ui.small_button("添加全部").clicked() {
                        self.add = Some(path.to_string());
                    }
                    ui.label(RichText::new(node_name(self.syms, &ty)).weak().small());
                    ui.label(RichText::new(path).weak().small())
                        .on_hover_text("添加时用的表达式");
                });
                if let NodeTy::Arr { dims, .. } = &ty {
                    if let Some(n) = dims.first().copied().flatten() {
                        if n > MAX_ARRAY_ROWS {
                            ui.label(
                                RichText::new(format!(
                                    "只列出前 {MAX_ARRAY_ROWS} 个元素，共 {n} 个"
                                ))
                                .weak()
                                .small(),
                            );
                        }
                    }
                }
                for (suffix, child, bit) in children {
                    let child_label = suffix.trim_start_matches('.').to_string();
                    if bit {
                        ui.horizontal(|ui| {
                            ui.label(&child_label);
                            ui.label(RichText::new("位域，不能单独读").weak().small());
                        });
                        continue;
                    }
                    let child_path = format!("{path}{suffix}");
                    self.node(ui, &child_path, &child_label, child, depth + 1);
                }
            });
    }
}

/// 筛选时把匹配的节点和它的后代平铺成列表，路径里含 `needle` 的才收。数组只搜前
/// [`MAX_ARRAY_ROWS`] 个元素，遇到更长的数组时把 `clipped` 置为 true。
fn collect_matches(
    syms: &SymbolTable,
    path: &str,
    ty: &NodeTy,
    needle: &str,
    depth: usize,
    out: &mut Vec<(String, String, bool)>,
    clipped: &mut bool,
) {
    if out.len() >= MAX_MATCHES || depth >= MAX_DEPTH {
        return;
    }
    if let NodeTy::Arr { dims, .. } = ty {
        if dims
            .first()
            .copied()
            .flatten()
            .is_some_and(|n| n > MAX_ARRAY_ROWS)
        {
            *clipped = true;
        }
    }
    let children = node_children(syms, ty);
    let bit_leaf = children.is_empty();
    if path.to_lowercase().contains(needle) {
        out.push((path.to_string(), node_name(syms, ty), bit_leaf));
        if out.len() >= MAX_MATCHES {
            return;
        }
    }
    for (suffix, child, bit) in children {
        if bit {
            continue;
        }
        collect_matches(
            syms,
            &format!("{path}{suffix}"),
            &child,
            needle,
            depth + 1,
            out,
            clipped,
        );
    }
}

impl App {
    /// 成员树窗口：展开结构体和数组，点“添加”把节点写成表达式加进监视表。
    pub(crate) fn ui_tree(&mut self, ctx: &egui::Context) {
        let Some(mut tree) = self.tree.take() else {
            return;
        };
        let editable = !self.sampling && !self.starting;
        let mut open = tree.open;
        let mut add: Option<String> = None;
        let title = format!("成员：{}", tree.path);
        // 标题栏底色取窗口 frame 的 fill；不给 frame 时用 widgets.open.weak_bg_fill，
        // 本主题把展开态画成墨色，标题就看不清了。
        let p = theme::Palette::of(ctx.theme());
        let frame = egui::Frame::window(&ctx.style())
            .fill(p.raised)
            .stroke(egui::Stroke::new(1.0_f32, p.line))
            .corner_radius(egui::CornerRadius::ZERO)
            .shadow(egui::epaint::Shadow::NONE);
        egui::Window::new(title)
            .id(egui::Id::new("var_tree"))
            .frame(frame)
            .open(&mut open)
            .default_width(430.0)
            .default_height(460.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("筛选");
                    ui.add(
                        egui::TextEdit::singleline(&mut tree.filter)
                            .hint_text("路径里含这段文字的成员，例如 ins_info")
                            .desired_width(fill_width(ui)),
                    );
                });
                let head = format!("0x{:08X}  {}", tree.addr, self.syms.name_of(tree.ty));
                ui.label(RichText::new(head).weak().small());
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("var_tree_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let needle = tree.filter.trim().to_lowercase();
                        let root = node_ty(&self.syms, tree.ty);
                        if needle.is_empty() {
                            let mut cx = TreeCtx {
                                syms: &self.syms,
                                editable,
                                add: None,
                            };
                            cx.node(ui, &tree.path, &tree.path, root, 0);
                            add = cx.add;
                        } else {
                            if tree.matches.as_ref().is_none_or(|m| m.needle != needle) {
                                let mut rows = Vec::new();
                                let mut clipped = false;
                                collect_matches(
                                    &self.syms,
                                    &tree.path,
                                    &root,
                                    &needle,
                                    0,
                                    &mut rows,
                                    &mut clipped,
                                );
                                tree.matches = Some(TreeMatches {
                                    needle: needle.clone(),
                                    rows,
                                    clipped,
                                });
                            }
                            let Some(m) = &tree.matches else { return };
                            if m.rows.is_empty() {
                                ui.label(RichText::new("没有匹配的成员").weak());
                            }
                            let truncated = m.rows.len() >= MAX_MATCHES;
                            for (path, tyname, leaf) in &m.rows {
                                let leaf = *leaf;
                                ui.horizontal(|ui| {
                                    if editable && ui.small_button("添加").clicked() {
                                        add = Some(path.clone());
                                    }
                                    let hint = if leaf {
                                        ""
                                    } else {
                                        "（结构体，添加会展开全部成员）"
                                    };
                                    ui.add(egui::Label::new(path.as_str()).truncate())
                                        .on_hover_text(format!("{path}  {tyname}{hint}"));
                                    ui.label(RichText::new(tyname.as_str()).weak().small());
                                });
                            }
                            if truncated {
                                ui.label(
                                    RichText::new(format!(
                                        "只列出前 {MAX_MATCHES} 项，缩小筛选范围"
                                    ))
                                    .weak()
                                    .small(),
                                );
                            }
                            if m.clipped {
                                ui.label(
                                    RichText::new(format!(
                                        "超过 {MAX_ARRAY_ROWS} 个元素的数组只搜了前 {MAX_ARRAY_ROWS} 个"
                                    ))
                                    .weak()
                                    .small(),
                                );
                            }
                        }
                    });
            });
        if add.is_some() {
            open = true;
        }
        tree.open = open;
        if open {
            self.tree = Some(tree);
        }
        if let Some(expr) = add {
            self.add_expr(&expr, 1, true, 1, false);
        }
    }

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
                self.log(format!("读入 {}：{} 个变量", path.display(), vars.len()));
                self.syms = syms;
                self.vars = vars;
                // 类型编号跟着旧符号表，换 ELF 后不能再拿它展开
                self.tree = None;
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
                        channel: id as usize,
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
        theme::section(ui, "XRDAP / ELF", "ELF");
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
        theme::section(ui, "XRDAP / ELF / VARS", "添加变量");
        let editable = !self.sampling && !self.starting;
        let mut add: Option<String> = None;
        let mut open_tree: Option<(String, u64, TypeId)> = None;
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
                    .hint_text(format!("在 {} 个变量里搜索", self.vars.len()))
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
                            if ui.small_button("树").clicked() {
                                open_tree =
                                    Some((xrdap::expr::name_expr(&v.name), v.addr, v.ty_id));
                            }
                            if ui.small_button("添加").clicked() {
                                add = Some(xrdap::expr::name_expr(&v.name));
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
        if let Some((path, addr, ty)) = open_tree {
            self.tree = Some(VarTree {
                path,
                addr,
                ty,
                open: true,
                filter: String::new(),
                matches: None,
            });
        }
        if let Some(name) = add {
            self.add_expr(&name, 1, true, 1, false);
        }
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> SymbolTable {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../xrdap/tests/fixtures")
            .join(name);
        xrdap::elf::load(&p).unwrap()
    }

    /// 数组按维度展开成 [0]、[1]，结构体成员用 .名字。
    #[test]
    fn children_of_array_and_struct() {
        let syms = fixture("types.elf");
        let v = &syms.vars["motor"][0];
        let root = node_ty(&syms, v.ty);
        let kids = node_children(&syms, &root);
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0].0, "[0]");
        assert_eq!(kids[1].0, "[1]");
        let members = node_children(&syms, &kids[0].1);
        assert!(members.iter().any(|(s, _, _)| s == ".current"));
        assert!(members.iter().any(|(s, _, _)| s == ".pid"));
    }

    /// 筛选收的是完整路径，能按成员名找到深层节点。
    #[test]
    fn collect_matches_by_member_name() {
        let syms = fixture("types.elf");
        let v = &syms.vars["motor"][0];
        let root = node_ty(&syms, v.ty);
        let mut rows = Vec::new();
        let mut clipped = false;
        collect_matches(&syms, "motor", &root, "pid", 0, &mut rows, &mut clipped);
        assert!(!clipped);
        assert!(rows.iter().any(|(p, _, _)| p == "motor[1].pid"));
        assert!(rows
            .iter()
            .all(|(p, _, _)| p.to_lowercase().contains("pid")));
    }

    /// 变量的类型在它的编译单元里只有声明时，换成别的编译单元里的完整定义展开，和读写时一样。
    #[test]
    fn declaration_only_type_expands() {
        let mut syms = fixture("decl.elf");
        let v = &syms.vars["port"][0];
        let root = node_ty(&syms, v.ty);
        let kids: Vec<String> = node_children(&syms, &root)
            .into_iter()
            .map(|(s, _, _)| s)
            .collect();
        assert_eq!(kids, [".rx", ".tx", ".buf"]);
        let mut rows = Vec::new();
        let mut clipped = false;
        collect_matches(&syms, "port", &root, "buf", 0, &mut rows, &mut clipped);
        assert!(rows.iter().any(|(p, _, _)| p == "port.buf[3]"), "{rows:?}");
        for (p, _, _) in &rows {
            xrdap::expr::Resolver::new(&mut syms)
                .resolve_all(p)
                .unwrap_or_else(|e| panic!("{p}: {e}"));
        }
    }

    /// 超过展开上限的数组在筛选时只搜前面的元素，并且说明。
    #[test]
    fn long_array_is_clipped() {
        let syms = fixture("types.elf");
        let long = NodeTy::Arr {
            elem: syms.vars["motor"][0].ty,
            dims: vec![Some(MAX_ARRAY_ROWS + 1)],
        };
        let mut rows = Vec::new();
        let mut clipped = false;
        collect_matches(&syms, "x", &long, "zzz", 0, &mut rows, &mut clipped);
        assert!(clipped && rows.is_empty());
    }

    /// 函数作用域的 static 也能从根变量展开，路径带函数名。
    #[test]
    fn tree_of_function_static() {
        let syms = fixture("statics.elf");
        let v = &syms.vars["statics_touch::func_static"][0];
        let root = node_ty(&syms, v.ty);
        assert!(node_children(&syms, &root).is_empty());
    }
}
