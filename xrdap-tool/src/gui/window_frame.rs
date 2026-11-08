//! 窗口边框：去掉系统标题栏后，顶栏兼作标题栏（按住拖动窗口、双击最大化或还原，右侧最小化、
//! 最大化、关闭三个按钮）；窗口边缘按住改变大小；未最大化时沿窗口画一圈 1px `line-strong`。

use eframe::egui::emath::GuiRounding;
use eframe::egui::{
    self, CursorIcon, Id, LayerId, Order, Pos2, Rect, ResizeDirection, Sense, Stroke, StrokeKind,
    Vec2, ViewportCommand,
};

use super::theme::Palette;

/// 窗口边缘可以按住改变大小的宽度。
const EDGE: f32 = 5.0;
/// 角上两边一起改变大小的范围。
const CORNER: f32 = 12.0;
/// 窗口按钮的大小（与按钮高度 size-control 相同）。
const BUTTON: Vec2 = Vec2::new(40.0, 36.0);
/// 按钮图形的半边长。
const GLYPH: f32 = 5.0;

/// 窗口当前是否最大化。
pub(super) fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

/// 标题栏的拖动区。要在顶栏的其他控件之前调用，其他控件盖在它上面、各自响应点击。
/// 窗口边缘改变大小的区域留给 [`edges`]。
pub(super) fn title_bar(ui: &mut egui::Ui, rect: Rect) {
    let resp = ui.interact(rect, Id::new("xrdap.title_bar"), Sense::click_and_drag());
    let ctx = ui.ctx();
    if resizing_edge(ctx).is_some() {
        return;
    }
    if resp.double_clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized(ctx)));
    } else if resp.drag_started_by(egui::PointerButton::Primary) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// 指针所在的改变大小区域；最大化时没有。
fn resizing_edge(ctx: &egui::Context) -> Option<ResizeDirection> {
    if maximized(ctx) {
        return None;
    }
    let rect = ctx.content_rect();
    ctx.input(|i| i.pointer.hover_pos())
        .and_then(|pos| direction(rect, pos))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glyph {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// 最小化、最大化（或还原）、关闭三个按钮。在从右到左的布局中调用，关闭在最右。
pub(super) fn window_buttons(ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    if button(ui, Glyph::Close, "关闭").clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
    let max = maximized(&ctx);
    let (glyph, tip) = if max {
        (Glyph::Restore, "还原")
    } else {
        (Glyph::Maximize, "最大化")
    };
    if button(ui, glyph, tip).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!max));
    }
    if button(ui, Glyph::Minimize, "最小化").clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
    }
}

/// 窗口按钮：直角、无边框，图形是 1px 的墨线；hover 和按下时反相。
fn button(ui: &mut egui::Ui, glyph: Glyph, tip: &str) -> egui::Response {
    let p = Palette::current(ui);
    let (rect, resp) = ui.allocate_exact_size(BUTTON, Sense::click());
    let resp = resp.on_hover_text(tip);
    if ui.is_rect_visible(rect) {
        let hot = resp.hovered() || resp.is_pointer_button_down_on();
        let fg = if hot { p.on_ink } else { p.ink };
        let painter = ui.painter();
        if hot {
            painter.rect_filled(rect, 0.0, p.ink);
        }
        let s = Stroke::new(1.0_f32, fg);
        let c = rect
            .center()
            .round_to_pixel_center(painter.pixels_per_point());
        let g = GLYPH;
        match glyph {
            Glyph::Minimize => {
                painter.line_segment([c + Vec2::new(-g, 0.0), c + Vec2::new(g, 0.0)], s);
            }
            Glyph::Maximize => {
                let r = Rect::from_min_max(c - Vec2::splat(g), c + Vec2::splat(g));
                painter.rect_stroke(r, 0.0, s, StrokeKind::Middle);
            }
            Glyph::Restore => {
                // 后面一个方框只露出上边和右边，前面一个方框完整
                let d = 2.0;
                painter.line_segment([c + Vec2::new(-g + d, -g), c + Vec2::new(g, -g)], s);
                painter.line_segment([c + Vec2::new(g, -g), c + Vec2::new(g, g - d)], s);
                let front = Rect::from_min_max(c + Vec2::new(-g, -g + d), c + Vec2::new(g - d, g));
                painter.rect_stroke(front, 0.0, s, StrokeKind::Middle);
            }
            Glyph::Close => {
                painter.line_segment([c + Vec2::new(-g, -g), c + Vec2::new(g, g)], s);
                painter.line_segment([c + Vec2::new(-g, g), c + Vec2::new(g, -g)], s);
            }
        }
    }
    resp
}

/// 指针在窗口 `rect` 中的位置 `pos` 落在哪条边或哪个角的改变大小区域里。
fn direction(rect: Rect, pos: Pos2) -> Option<ResizeDirection> {
    if !rect.contains(pos) {
        return None;
    }
    let left = pos.x - rect.left();
    let right = rect.right() - pos.x;
    let top = pos.y - rect.top();
    let bottom = rect.bottom() - pos.y;
    let near = |d: f32| d < EDGE;
    let corner = |a: f32, b: f32| (near(a) && b < CORNER) || (near(b) && a < CORNER);
    Some(if corner(left, top) {
        ResizeDirection::NorthWest
    } else if corner(right, top) {
        ResizeDirection::NorthEast
    } else if corner(left, bottom) {
        ResizeDirection::SouthWest
    } else if corner(right, bottom) {
        ResizeDirection::SouthEast
    } else if near(left) {
        ResizeDirection::West
    } else if near(right) {
        ResizeDirection::East
    } else if near(top) {
        ResizeDirection::North
    } else if near(bottom) {
        ResizeDirection::South
    } else {
        return None;
    })
}

fn cursor(dir: ResizeDirection) -> CursorIcon {
    match dir {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::ResizeVertical,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::ResizeHorizontal,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::ResizeNwSe,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::ResizeNeSw,
    }
}

/// 窗口边缘：指针在边上时换成改变大小的光标，按下时交给系统改变窗口大小；并画外框。
/// 在每帧的最后调用，光标盖过边缘下面的控件。最大化时都不做。
pub(super) fn edges(ctx: &egui::Context) {
    if maximized(ctx) {
        return;
    }
    let rect = ctx.content_rect();
    let p = Palette::of(ctx.theme());
    ctx.layer_painter(LayerId::new(
        Order::Foreground,
        Id::new("xrdap.window_outline"),
    ))
    .rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0_f32, p.line_strong),
        StrokeKind::Inside,
    );
    if let Some(dir) = resizing_edge(ctx) {
        ctx.set_cursor_icon(cursor(dir));
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_and_corners() {
        let r = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(800.0, 500.0));
        let at = |x, y| direction(r, Pos2::new(x, y));
        assert_eq!(at(400.0, 250.0), None);
        assert_eq!(at(2.0, 250.0), Some(ResizeDirection::West));
        assert_eq!(at(798.0, 250.0), Some(ResizeDirection::East));
        assert_eq!(at(400.0, 1.0), Some(ResizeDirection::North));
        assert_eq!(at(400.0, 499.0), Some(ResizeDirection::South));
        // 角：一边贴边、另一边在角的范围内
        assert_eq!(at(2.0, 10.0), Some(ResizeDirection::NorthWest));
        assert_eq!(at(10.0, 2.0), Some(ResizeDirection::NorthWest));
        assert_eq!(at(797.0, 3.0), Some(ResizeDirection::NorthEast));
        assert_eq!(at(3.0, 495.0), Some(ResizeDirection::SouthWest));
        assert_eq!(at(799.0, 499.0), Some(ResizeDirection::SouthEast));
        // 贴边但离角远：只改一边
        assert_eq!(at(2.0, 40.0), Some(ResizeDirection::West));
        // 边缘以内、窗口以外都不算
        assert_eq!(at(6.0, 250.0), None);
        assert_eq!(at(-1.0, 250.0), None);
    }

    #[test]
    fn cursors_match_directions() {
        assert_eq!(cursor(ResizeDirection::North), CursorIcon::ResizeVertical);
        assert_eq!(cursor(ResizeDirection::West), CursorIcon::ResizeHorizontal);
        assert_eq!(cursor(ResizeDirection::SouthEast), CursorIcon::ResizeNwSe);
        assert_eq!(cursor(ResizeDirection::NorthEast), CursorIcon::ResizeNeSw);
    }
}
