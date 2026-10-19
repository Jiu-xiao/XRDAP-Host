//! 终端表格的对齐：中文等宽字符算 2 格。

use xrdap::expr::Target;

/// 终端里的显示宽度：中文等宽字符算 2 格。
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if (c as u32) >= 0x1100 { 2 } else { 1 })
        .sum()
}

pub fn rjust(s: &str, w: usize) -> String {
    let pad = w.saturating_sub(display_width(s));
    format!("{}{s}", " ".repeat(pad))
}

pub fn ljust(s: &str, w: usize) -> String {
    let pad = w.saturating_sub(display_width(s));
    format!("{s}{}", " ".repeat(pad))
}

pub fn name_width(targets: &[&Target]) -> usize {
    targets
        .iter()
        .map(|t| display_width(&t.name))
        .max()
        .unwrap_or(0)
}
