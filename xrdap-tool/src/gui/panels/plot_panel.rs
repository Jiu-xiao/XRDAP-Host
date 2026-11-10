//! 中间下方：曲线。

use eframe::egui;
use xrdap::types::ScalarKind;

use crate::gui::app::App;
use crate::gui::plot::Curve;
use crate::gui::theme::Palette;

impl App {
    pub(crate) fn ui_plot(&mut self, ui: &mut egui::Ui) {
        let p = Palette::current(ui);
        let curves: Vec<Curve> = self
            .watches
            .iter()
            .map(|w| Curve {
                id: w.id,
                name: &w.target.name,
                color: p.channel(w.channel),
                chart: w.chart,
                series: &w.series,
                float: matches!(w.target.ty.kind, ScalarKind::Float),
                show: w.show,
            })
            .collect();
        self.plot.ui_cursors(ui, &curves);
        let t_last = self.t_last.unwrap_or(0.0);
        let toggle = self
            .plot
            .ui_plots(ui, &curves, !self.paused, t_last, self.window_s);
        if let Some(id) = toggle {
            if let Some(w) = self.watches.iter_mut().find(|w| w.id == id) {
                w.show = !w.show;
            }
        }
    }
}
