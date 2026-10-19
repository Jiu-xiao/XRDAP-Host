//! XR-DAP 图形界面：按变量名选变量，在探针上采样，实时画曲线，可以边采边改变量。

// Windows 上发布版不弹控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod fonts;
mod plot;
mod series;
mod share;
mod worker;

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 760.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("XR-DAP"),
        ..Default::default()
    };
    eframe::run_native(
        "xrdap-gui",
        opts,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
