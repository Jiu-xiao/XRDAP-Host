//! 几个小的界面工具函数。

use eframe::egui::{self, Color32};

/// 输入框占满剩下的宽度：留出边框和内边距，否则每帧把面板撑宽一点。
pub(super) fn fill_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() - 12.0).max(40.0)
}

pub(super) fn unix_us_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

pub(super) fn color_for(id: u64) -> Color32 {
    let h = (id as f32 * 0.618_034).fract();
    egui::ecolor::Hsva::new(h, 0.75, 0.85, 1.0).into()
}

/// 序列号的末尾 6 位，例如 …DDEEFF；短的原样返回。
pub(super) fn short_serial(sn: &str) -> String {
    let n = sn.chars().count();
    if n > 8 {
        format!("…{}", sn.chars().skip(n - 6).collect::<String>())
    } else {
        sn.to_string()
    }
}

/// 一行、从右往左排：先放的按钮在最右边，最后放的输入框占剩下的宽度。
pub(super) fn right_to_left_row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), add)
        .inner
}
