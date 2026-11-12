//! 曲线区：按变量分到 1 到 4 张图（时间轴联动），可以归一化，悬停读数，A/B 游标测时间差。

use eframe::egui::{self, Color32, RichText};
use egui_plot::{Line, Plot, PlotPoints, VLine};
use xrdap::lang::tr;
use xrdap::trf;

use super::series::Series;

pub const MAX_CHARTS: u8 = 4;

/// 一条要画的曲线。
pub struct Curve<'a> {
    /// 监视项的编号，图例里点击切换显示时用
    pub id: u64,
    pub name: &'a str,
    pub color: Color32,
    /// 画在第几张图（1 起）
    pub chart: u8,
    pub series: &'a Series,
    /// 浮点变量按小数显示，其余按整数显示
    pub float: bool,
    /// 是否画这条曲线；图例里隐藏的曲线仍然列出，点击可以再显示
    pub show: bool,
}

pub struct PlotView {
    /// 每条曲线按当前窗口里的最小、最大值缩放到 0 到 1
    pub normalize: bool,
    pub cursor_a: Option<f64>,
    pub cursor_b: Option<f64>,
    /// 浮动的图例窗口是否显示；窗口的位置、大小和折叠状态由 egui 记忆保存
    pub legend_open: bool,
}

impl Default for PlotView {
    fn default() -> Self {
        Self {
            normalize: false,
            cursor_a: None,
            cursor_b: None,
            legend_open: true,
        }
    }
}

pub fn fmt_value(v: f64, float: bool) -> String {
    if !float {
        return format!("{v:.0}");
    }
    let a = v.abs();
    if a != 0.0 && !(1e-3..1e6).contains(&a) {
        format!("{v:.4e}")
    } else {
        format!("{v:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

fn fmt_dt(dt: f64) -> String {
    let a = dt.abs();
    let (v, unit) = if a >= 1.0 {
        (dt, "s")
    } else if a >= 1e-3 {
        (dt * 1e3, "ms")
    } else {
        (dt * 1e6, "µs")
    };
    let mut s = format!("{v:.3} {unit}");
    if a > 0.0 {
        s.push_str(&trf!(" ({:.3} Hz)", "（{:.3} Hz）", 1.0 / a));
    }
    s
}

impl PlotView {
    /// 游标栏：A、B 的时间和 Δt，以及两处的数值表。
    pub fn ui_cursors(&mut self, ui: &mut egui::Ui, curves: &[Curve]) {
        if self.cursor_a.is_none() && self.cursor_b.is_none() {
            ui.label(
                RichText::new(tr(
                    "Click a curve to place cursor A, Shift+click for cursor B; hover shows the values of all curves",
                    "单击曲线放游标 A，Shift+单击放游标 B；悬停显示各曲线的值",
                ))
                    .weak()
                    .small(),
            );
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if let Some(a) = self.cursor_a {
                ui.label(format!("A {a:.4} s"));
            }
            if let Some(b) = self.cursor_b {
                ui.label(format!("B {b:.4} s"));
            }
            if let (Some(a), Some(b)) = (self.cursor_a, self.cursor_b) {
                ui.strong(format!("Δt = {}", fmt_dt(b - a)));
            }
            if ui.small_button(tr("Clear cursors", "清除游标")).clicked() {
                self.cursor_a = None;
                self.cursor_b = None;
            }
        });
        egui::CollapsingHeader::new(tr("Values at the cursors", "游标处的值"))
            .id_salt("cursor_values")
            .default_open(false)
            .show(ui, |ui| {
                egui::Grid::new("cursor_grid").striped(true).show(ui, |ui| {
                    ui.strong(tr("Variable", "变量"));
                    ui.strong("A");
                    ui.strong("B");
                    ui.strong("B − A");
                    ui.end_row();
                    for c in curves.iter().filter(|c| c.show) {
                        let va = self.cursor_a.and_then(|t| c.series.value_at(t));
                        let vb = self.cursor_b.and_then(|t| c.series.value_at(t));
                        let show = |v: Option<f64>| v.map_or("-".into(), |v| fmt_value(v, c.float));
                        ui.label(RichText::new(c.name).color(c.color));
                        ui.label(show(va));
                        ui.label(show(vb));
                        let d = match (va, vb) {
                            (Some(a), Some(b)) => fmt_value(b - a, c.float),
                            _ => "-".into(),
                        };
                        ui.label(d);
                        ui.end_row();
                    }
                });
            });
    }

    /// 画图。follow 为 true 时 X 轴跟着最新数据走，显示 [t_last - window, t_last]。
    /// 返回图例里点击的监视项编号，调用方用它切换这条曲线画不画。
    pub fn ui_plots(
        &mut self,
        ui: &mut egui::Ui,
        curves: &[Curve],
        follow: bool,
        t_last: f64,
        window: f64,
    ) -> Option<u64> {
        let mut charts: Vec<u8> = curves.iter().filter(|c| c.show).map(|c| c.chart).collect();
        charts.sort_unstable();
        charts.dedup();
        if charts.is_empty() {
            charts.push(1);
        }
        // 图例默认放在曲线区右上角
        let area = ui.available_rect_before_wrap();
        let buckets = (ui.available_width() as usize).max(100);
        let gap = ui.spacing().item_spacing.y;
        let height = ((ui.available_height() - gap * (charts.len() as f32 - 1.0))
            / charts.len() as f32)
            .max(80.0);
        let shift = ui.input(|i| i.modifiers.shift);
        let palette = super::theme::Palette::current(ui);
        let (ink, ink_muted) = (palette.ink, palette.ink_muted);
        let mut hover_x = None;
        let mut clicked_x = None;
        let link = egui::Id::new("xrdap_plot_x");
        let last = charts.len() - 1;
        for (k, &chart) in charts.iter().enumerate() {
            let mut plot = Plot::new(("plot", chart))
                .height(height)
                .link_axis(link, egui::Vec2b::new(true, false))
                .link_cursor(link, egui::Vec2b::new(true, false))
                .allow_drag(!follow)
                .allow_zoom(!follow)
                .allow_scroll(!follow)
                .show_axes(egui::Vec2b::new(true, !self.normalize))
                // 几张图的 Y 轴标签宽度固定，时间轴才对得齐
                .y_axis_min_width(64.0)
                // 读数由下面统一的悬停提示显示，不要 egui_plot 自己的坐标提示
                .show_x(false)
                .show_y(false);
            if k == last {
                plot = plot.x_axis_label(tr("Time (s)", "时间 (s)"));
            }
            let normalize = self.normalize;
            let (cursor_a, cursor_b) = (self.cursor_a, self.cursor_b);
            plot.show(ui, |p| {
                let (x0, x1) = if follow {
                    let x0 = (t_last - window).max(0.0);
                    let x1 = x0 + window;
                    p.set_plot_bounds_x(x0..=x1);
                    p.set_auto_bounds(egui::Vec2b::new(false, true));
                    (x0, x1)
                } else {
                    let b = p.plot_bounds();
                    (b.min()[0], b.max()[0])
                };
                for c in curves.iter().filter(|c| c.show && c.chart == chart) {
                    let mut pts = c.series.decimated(x0, x1, buckets);
                    if normalize {
                        let (lo, hi) = pts
                            .iter()
                            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                                (lo.min(p[1]), hi.max(p[1]))
                            });
                        let span = hi - lo;
                        for p in &mut pts {
                            p[1] = if span > 0.0 { (p[1] - lo) / span } else { 0.5 };
                        }
                    }
                    p.line(Line::new(c.name, PlotPoints::from(pts)).color(c.color));
                }
                // 游标是界面，不是数据：A 用墨色实线，B 用次要墨色
                if let Some(a) = cursor_a {
                    p.vline(VLine::new("A", a).color(ink));
                }
                if let Some(b) = cursor_b {
                    p.vline(VLine::new("B", b).color(ink_muted));
                }
                if let Some(pos) = p.pointer_coordinate() {
                    if p.response().hovered() {
                        hover_x = Some(pos.x);
                    }
                    if p.response().clicked() {
                        clicked_x = Some(pos.x);
                    }
                }
            });
        }
        if let Some(x) = clicked_x {
            if shift {
                self.cursor_b = Some(x);
            } else {
                self.cursor_a = Some(x);
            }
        }
        let toggle = self.ui_legend(ui.ctx(), curves, area);
        if let Some(x) = hover_x {
            egui::Tooltip::always_open(
                ui.ctx().clone(),
                ui.layer_id(),
                egui::Id::new("plot_hover"),
                egui::PopupAnchor::Pointer,
            )
            .gap(16.0)
            .show(|ui| {
                ui.strong(format!("t = {x:.4} s"));
                egui::Grid::new("hover_grid").show(ui, |ui| {
                    for c in curves.iter().filter(|c| c.show) {
                        ui.label(RichText::new(c.name).color(c.color));
                        let v = c.series.value_at(x);
                        ui.monospace(v.map_or("-".into(), |v| fmt_value(v, c.float)));
                        ui.end_row();
                    }
                });
            });
        }
        toggle
    }

    /// 浮动的图例窗口：默认在曲线区右上角，可以拖动、收起、调大小，曲线多时用滚轮查看；
    /// 点击条目切换这条曲线画不画，关闭后可以在控制栏的“图例”按钮重新打开。
    fn ui_legend(
        &mut self,
        ctx: &egui::Context,
        curves: &[Curve],
        area: egui::Rect,
    ) -> Option<u64> {
        if !self.legend_open || curves.is_empty() {
            return None;
        }
        let mut charts: Vec<u8> = curves.iter().map(|c| c.chart).collect();
        charts.sort_unstable();
        charts.dedup();
        let p = super::theme::Palette::of(ctx.theme());
        let fold = egui::Id::new("xrdap_legend_collapsed");
        let mut collapsed = ctx
            .data_mut(|d| d.get_persisted::<bool>(fold))
            .unwrap_or(false);
        let mut open = self.legend_open;
        let mut toggle = None;
        const WIDTH: f32 = 260.0;
        egui::Window::new(tr("Legend", "图例"))
            .id(egui::Id::new("xrdap_plot_legend"))
            .title_bar(false)
            .frame(super::theme::floating_frame(ctx))
            .constrain_to(ctx.content_rect())
            .default_pos(egui::pos2(area.right() - WIDTH - 24.0, area.top() + 8.0))
            .default_width(WIDTH)
            .default_height((area.height() - 16.0).clamp(120.0, 200.0))
            .max_height(ctx.content_rect().height() - 48.0)
            .resizable(!collapsed)
            .show(ctx, |ui| {
                super::theme::floating_header(
                    ui,
                    "XRDAP / WATCH / LEGEND",
                    tr("Legend", "图例"),
                    &mut collapsed,
                    &mut open,
                );
                if collapsed {
                    return;
                }
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("legend_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let multi = charts.len() > 1;
                        for &chart in &charts {
                            if multi {
                                ui.label(
                                    RichText::new(trf!("Chart {chart}", "图 {chart}")).strong(),
                                );
                            }
                            for c in curves.iter().filter(|c| c.chart == chart) {
                                // 整行（色块、名字、右侧空白）都要能点。文字直接画 galley，
                                // 不用 Label：可选的 Label 会自己抢走点击，整行就点不动。
                                let row_h = ui.spacing().interact_size.y.max(18.0);
                                let (rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h),
                                    egui::Sense::click(),
                                );
                                if resp.hovered() {
                                    ui.painter().rect_filled(rect, 0.0, p.sunken);
                                }
                                let color = if c.show { c.color } else { p.ink_muted };
                                let sq = egui::Rect::from_center_size(
                                    egui::pos2(rect.left() + 7.0, rect.center().y),
                                    egui::vec2(10.0, 10.0),
                                );
                                if c.show {
                                    ui.painter().rect_filled(sq, 1.0, c.color);
                                } else {
                                    ui.painter().rect_stroke(
                                        sq,
                                        1.0,
                                        egui::Stroke::new(1.0_f32, p.ink_muted),
                                        egui::StrokeKind::Inside,
                                    );
                                }
                                let font = egui::TextStyle::Body.resolve(ui.style());
                                let wrap = egui::text::TextWrapping {
                                    max_width: (rect.width() - 22.0).max(1.0),
                                    max_rows: 1,
                                    break_anywhere: true,
                                    overflow_character: Some('…'),
                                };
                                let galley = ui.fonts_mut(|f| {
                                    let mut job = egui::text::LayoutJob::simple(
                                        c.name.to_string(),
                                        font.clone(),
                                        color,
                                        wrap.max_width,
                                    );
                                    job.wrap = wrap;
                                    f.layout_job(job)
                                });
                                let pos = egui::pos2(
                                    rect.left() + 16.0,
                                    rect.center().y - galley.size().y / 2.0,
                                );
                                ui.painter().galley(pos, galley, color);
                                if resp
                                    .on_hover_text(trf!(
                                        "{}\nClick to {} this curve",
                                        "{}\n点击{}这条曲线",
                                        c.name,
                                        if c.show {
                                            tr("hide", "隐藏")
                                        } else {
                                            tr("show", "显示")
                                        }
                                    ))
                                    .clicked()
                                {
                                    toggle = Some(c.id);
                                }
                            }
                            if multi {
                                ui.add_space(4.0);
                            }
                        }
                    });
            });
        ctx.data_mut(|d| d.insert_persisted(fold, collapsed));
        self.legend_open = open;
        toggle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_and_time_formats() {
        xrdap::lang::set_chinese(true);
        assert_eq!(fmt_value(1406.0, false), "1406");
        assert_eq!(fmt_value(0.25, true), "0.25");
        assert_eq!(fmt_value(3.0, true), "3");
        assert_eq!(fmt_value(1.5e-6, true), "1.5000e-6");
        assert_eq!(fmt_dt(0.0015), "1.500 ms（666.667 Hz）");
    }
}
