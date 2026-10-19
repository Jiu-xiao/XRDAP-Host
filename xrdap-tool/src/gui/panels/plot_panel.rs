//! 中间下方：曲线。

use eframe::egui;
use xrdap::types::ScalarKind;

use crate::gui::app::App;
use crate::gui::plot::Curve;

impl App {
    pub(crate) fn ui_plot(&mut self, ui: &mut egui::Ui) {
        let curves: Vec<Curve> = self
            .watches
            .iter()
            .filter(|w| w.show)
            .map(|w| Curve {
                name: &w.target.name,
                color: w.color,
                chart: w.chart,
                series: &w.series,
                float: matches!(w.target.ty.kind, ScalarKind::Float),
            })
            .collect();
        self.plot.ui_cursors(ui, &curves);
        let t_last = self.t_last.unwrap_or(0.0);
        self.plot
            .ui_plots(ui, &curves, !self.paused, t_last, self.window_s);
    }
}
