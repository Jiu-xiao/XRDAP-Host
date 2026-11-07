//! 图形界面：按变量名选变量，在探针上采样，实时画曲线，可以边采边改变量，或把探针共享给
//! 别的调试工具。

mod app;
mod fonts;
mod model;
mod panels;
mod plot;
mod series;
mod theme;
mod widgets;
mod worker;

/// 打开主窗口，直到用户关闭。
pub fn run() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 760.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("XR-DAP"),
        ..Default::default()
    };
    // 应用名决定界面设置的保存位置
    eframe::run_native(
        "xrdap-gui",
        opts,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
