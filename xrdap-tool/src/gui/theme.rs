//! 界面主题：XRobot Style 的墨与纸。颜色、字号、间距取自规范的 tokens.json，亮暗两套随系统或
//! 用户选择切换；直角、无阴影，强调靠反相和字重；通道色 ch0–ch3 只给曲线数据，
//! pass / fail / blocked 只和文字结论一起出现。另有几个规范里的小部件：主按钮（左上、右下切角）、
//! 卡片（右上切角）、PathLabel、Tag 和带文字的状态。

use eframe::egui::{
    self, epaint, Color32, CornerRadius, FontFamily, FontId, Margin, Response, RichText, Sense,
    Shadow, Stroke, TextStyle, Theme, Vec2, WidgetText,
};

/// 一套主题的颜色（tokens.json `color.tokens`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Palette {
    pub paper: Color32,
    pub raised: Color32,
    pub sunken: Color32,
    pub line: Color32,
    pub line_strong: Color32,
    pub ink: Color32,
    pub ink_muted: Color32,
    pub on_ink: Color32,
    /// 数据通道 ch0（黄）、ch1（青）、ch2（品红）、ch3（绿），只用于曲线
    pub ch: [Color32; 4],
    pub pass: Color32,
    pub fail: Color32,
    pub blocked: Color32,
}

fn stroke(width: f32, color: Color32) -> Stroke {
    Stroke { width, color }
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// 亮色：数据手册，冷灰纸面、近黑墨。
pub(super) const LIGHT: Palette = Palette {
    paper: hex(0xf2f3f2),
    raised: hex(0xffffff),
    sunken: hex(0xe6e8e6),
    line: hex(0xd3d6d3),
    line_strong: hex(0x7c827e),
    ink: hex(0x111312),
    ink_muted: hex(0x4d524f),
    on_ink: hex(0xf2f3f2),
    ch: [hex(0x7a5c00), hex(0x006573), hex(0xa1206f), hex(0x2a6e1b)],
    pass: hex(0x16693a),
    fail: hex(0xb42318),
    blocked: hex(0x7d5100),
};

/// 暗色：电路板，近黑阻焊层、丝印白。
pub(super) const DARK: Palette = Palette {
    paper: hex(0x0c0e0d),
    raised: hex(0x151816),
    sunken: hex(0x070908),
    line: hex(0x2a2e2c),
    line_strong: hex(0x6f7672),
    ink: hex(0xe6e8e6),
    ink_muted: hex(0xa3a9a5),
    on_ink: hex(0x0c0e0d),
    ch: [hex(0xffd84d), hex(0x4fdded), hex(0xff85cb), hex(0x8be36a)],
    pass: hex(0x5bd68a),
    fail: hex(0xff8a7a),
    blocked: hex(0xf0b54a),
};

/// 45° 切角的边长。
pub(super) const CHAMFER: f32 = 8.0;
/// 按钮高度（size-control）。
const CONTROL: f32 = 36.0;
/// 可点击区域的最小边长（size-target）。
const TARGET: f32 = 32.0;
/// 4px 网格的几档间距。
const SPACE_1: f32 = 4.0;
const SPACE_2: f32 = 8.0;
const SPACE_4: f32 = 16.0;

/// 粗体的字体族名（由 [`fonts`](super::fonts) 装入系统字体）。
pub(super) const BOLD: &str = "bold";
pub(super) const MONO_BOLD: &str = "mono-bold";

impl Palette {
    pub(super) fn of(theme: Theme) -> &'static Palette {
        match theme {
            Theme::Light => &LIGHT,
            Theme::Dark => &DARK,
        }
    }

    /// 当前界面的颜色。
    pub(super) fn current(ui: &egui::Ui) -> &'static Palette {
        Palette::of(if ui.visuals().dark_mode {
            Theme::Dark
        } else {
            Theme::Light
        })
    }

    /// 第 i 条曲线的颜色：四个数据通道依次循环。
    pub(super) fn channel(&self, i: usize) -> Color32 {
        self.ch[i % self.ch.len()]
    }
}

fn bold(size: f32, families: &[&str]) -> FontId {
    FontId::new(size, family(families, BOLD))
}

/// 有粗体字体时用它，否则退回常规字体（找不到系统粗体时界面照常可用）。
fn family(available: &[&str], name: &str) -> FontFamily {
    if available.contains(&name) {
        FontFamily::Name(name.into())
    } else if name == MONO_BOLD {
        FontFamily::Monospace
    } else {
        FontFamily::Proportional
    }
}

/// 一套主题的 egui 样式。`families` 是装上的命名字体族（粗体）。
pub(super) fn style(theme: Theme, families: &[&str]) -> egui::Style {
    let p = Palette::of(theme);
    // 字号：heading 20 / 700，正文和按钮 14（按钮 700），代码 14，标签 12
    let mut s = egui::Style {
        text_styles: [
            (TextStyle::Heading, bold(20.0, families)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, bold(14.0, families)),
            (
                TextStyle::Small,
                FontId::new(12.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(14.0, FontFamily::Monospace),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let sp = &mut s.spacing;
    sp.item_spacing = Vec2::new(SPACE_2, SPACE_2);
    sp.button_padding = Vec2::new(SPACE_4 - SPACE_2, SPACE_1);
    sp.interact_size = Vec2::new(TARGET, TARGET);
    sp.window_margin = Margin::same(SPACE_4 as i8);
    sp.menu_margin = Margin::same(SPACE_2 as i8);
    sp.indent = SPACE_4;
    s.visuals = visuals(theme, p);
    s
}

fn visuals(theme: Theme, p: &Palette) -> egui::Visuals {
    let mut v = match theme {
        Theme::Light => egui::Visuals::light(),
        Theme::Dark => egui::Visuals::dark(),
    };
    let square = CornerRadius::ZERO;
    let line = stroke(1.0, p.line);
    let strong = stroke(1.0, p.line_strong);
    v.weak_text_color = Some(p.ink_muted);
    v.panel_fill = p.paper;
    v.window_fill = p.raised;
    v.window_stroke = line;
    v.window_corner_radius = square;
    v.menu_corner_radius = square;
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.faint_bg_color = p.sunken;
    v.extreme_bg_color = p.raised;
    v.text_edit_bg_color = Some(p.raised);
    v.code_bg_color = p.sunken;
    v.hyperlink_color = p.ink;
    v.warn_fg_color = p.blocked;
    v.error_fg_color = p.fail;
    // 选中：反相
    v.selection.bg_fill = p.ink;
    v.selection.stroke = stroke(1.0, p.on_ink);
    v.text_cursor.stroke = stroke(2.0, p.ink);
    let w = &mut v.widgets;
    // 静态的线和标签：装饰线用 line
    w.noninteractive.bg_fill = p.paper;
    w.noninteractive.weak_bg_fill = p.paper;
    w.noninteractive.bg_stroke = line;
    w.noninteractive.fg_stroke = stroke(1.0, p.ink);
    // 可交互的控件：承载含义的边框用 line-strong；hover、按下、展开时反相
    w.inactive.bg_fill = p.raised;
    w.inactive.weak_bg_fill = p.raised;
    w.inactive.bg_stroke = strong;
    w.inactive.fg_stroke = stroke(1.0, p.ink);
    for state in [&mut w.hovered, &mut w.open] {
        state.bg_fill = p.ink;
        state.weak_bg_fill = p.ink;
        state.bg_stroke = stroke(1.0, p.ink);
        state.fg_stroke = stroke(1.0, p.on_ink);
        state.expansion = 0.0;
    }
    // 按下：下沉的底色加 2px 墨线。egui 的粗体文字取这一态的前景色，所以它保持 ink
    w.active.bg_fill = p.sunken;
    w.active.weak_bg_fill = p.sunken;
    w.active.bg_stroke = stroke(2.0, p.ink);
    w.active.fg_stroke = stroke(1.0, p.ink);
    w.active.expansion = 0.0;
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.corner_radius = square;
    }
    v
}

/// 主题选择：跟随系统，或固定亮、暗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Choice {
    System,
    Light,
    Dark,
}

impl Choice {
    pub(super) const ALL: [Choice; 3] = [Choice::System, Choice::Light, Choice::Dark];

    pub(super) fn label(self) -> &'static str {
        match self {
            Choice::System => "跟随系统",
            Choice::Light => "亮",
            Choice::Dark => "暗",
        }
    }

    pub(super) fn key(self) -> &'static str {
        match self {
            Choice::System => "system",
            Choice::Light => "light",
            Choice::Dark => "dark",
        }
    }

    pub(super) fn from_key(key: &str) -> Choice {
        match key {
            "light" => Choice::Light,
            "dark" => Choice::Dark,
            _ => Choice::System,
        }
    }

    fn preference(self) -> egui::ThemePreference {
        match self {
            Choice::System => egui::ThemePreference::System,
            Choice::Light => egui::ThemePreference::Light,
            Choice::Dark => egui::ThemePreference::Dark,
        }
    }
}

/// 给界面装上两套主题，按 `choice` 选用。
pub(super) fn install(ctx: &egui::Context, families: &[&str], choice: Choice) {
    for theme in [Theme::Light, Theme::Dark] {
        ctx.set_style_of(theme, style(theme, families));
    }
    ctx.set_theme(choice.preference());
}

/// 改主题选择（样式已经装好）。
pub(super) fn choose(ctx: &egui::Context, choice: Choice) {
    ctx.set_theme(choice.preference());
}

/// 主按钮：`ink` 底、`on-ink` 字，左上和右下切角；hover 换成 `ink-muted` 底。
/// 每个视图最多一个。
pub(super) fn primary_button(ui: &mut egui::Ui, text: &str) -> Response {
    let p = Palette::current(ui);
    let font = ui.style().text_styles[&TextStyle::Button].clone();
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, p.on_ink);
    let size = Vec2::new(galley.size().x + 2.0 * SPACE_4, CONTROL);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if resp.hovered() || resp.is_pointer_button_down_on() {
            p.ink_muted
        } else {
            p.ink
        };
        let c = CHAMFER;
        let pts = vec![
            rect.left_top() + Vec2::new(c, 0.0),
            rect.right_top(),
            rect.right_bottom() - Vec2::new(0.0, c),
            rect.right_bottom() - Vec2::new(c, 0.0),
            rect.left_bottom(),
            rect.left_top() + Vec2::new(0.0, c),
        ];
        ui.painter()
            .add(epaint::PathShape::convex_polygon(pts, fill, Stroke::NONE));
        let pos = rect.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, p.on_ink);
        if resp.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                CornerRadius::ZERO,
                stroke(2.0, p.ink),
                egui::StrokeKind::Outside,
            );
        }
    }
    resp
}

/// 卡片：`paper-raised` 底、1px `line` 边框，右上角一条 1px 斜线切角，角外露出 `paper`。
pub(super) fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = Palette::current(ui);
    let out = egui::Frame::new()
        .fill(p.raised)
        .stroke(stroke(1.0, p.line))
        .corner_radius(CornerRadius::ZERO)
        .inner_margin(Margin::same(SPACE_4 as i8 - 4))
        .show(ui, add);
    let r = out.response.rect;
    let c = CHAMFER;
    let corner = vec![
        r.right_top() - Vec2::new(c, 0.0),
        r.right_top(),
        r.right_top() + Vec2::new(0.0, c),
    ];
    let painter = ui.painter();
    painter.add(epaint::PathShape::convex_polygon(
        corner,
        p.paper,
        Stroke::NONE,
    ));
    painter.line_segment(
        [
            r.right_top() - Vec2::new(c, 0.0),
            r.right_top() + Vec2::new(0.0, c),
        ],
        stroke(1.0, p.line),
    );
    out.inner
}

/// PathLabel：内容块标题上方的位置标签，等宽 12px、粗体、全大写、字距 0.08em，`ink-muted`；
/// 分隔符 `/` 用 `line-strong`。
pub(super) fn path_label(ui: &mut egui::Ui, path: &str) {
    let p = Palette::current(ui);
    let fonts = mono_bold(ui, 12.0);
    let mut job = egui::text::LayoutJob::default();
    let format = |color| egui::TextFormat {
        font_id: fonts.clone(),
        color,
        extra_letter_spacing: 12.0 * 0.08,
        ..Default::default()
    };
    for (i, part) in path.split('/').map(str::trim).enumerate() {
        if i > 0 {
            job.append(" / ", 0.0, format(p.line_strong));
        }
        job.append(&part.to_uppercase(), 0.0, format(p.ink_muted));
    }
    ui.label(job);
}

/// 区块标题：上方一个 PathLabel，下面 heading。
pub(super) fn section(ui: &mut egui::Ui, path: &str, title: &str) {
    path_label(ui, path);
    ui.label(RichText::new(title).text_style(TextStyle::Heading));
}

fn mono_bold(ui: &egui::Ui, size: f32) -> FontId {
    let has = ui
        .fonts(|f| f.families())
        .contains(&FontFamily::Name(MONO_BOLD.into()));
    FontId::new(
        size,
        if has {
            FontFamily::Name(MONO_BOLD.into())
        } else {
            FontFamily::Monospace
        },
    )
}

/// Tag：等宽短记号。`solid` 为 `ink` 底 `on-ink` 字，只标一个结论；否则 1px `ink` 边框。
pub(super) fn tag(ui: &mut egui::Ui, text: &str, solid: bool) -> Response {
    let p = Palette::current(ui);
    let (fg, bg) = if solid {
        (p.on_ink, p.ink)
    } else {
        (p.ink, Color32::TRANSPARENT)
    };
    egui::Frame::new()
        .fill(bg)
        .stroke(stroke(1.0, p.ink))
        .inner_margin(Margin::symmetric(SPACE_2 as i8, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(mono_bold(ui, 12.0)).color(fg))
        })
        .inner
}

/// 失败的结论：`FAIL` 加说明文字，`fail` 色只用在 `FAIL` 这个词上。
pub(super) fn fail(ui: &mut egui::Ui, text: impl Into<WidgetText>) {
    let p = Palette::current(ui);
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new("FAIL")
                .font(mono_bold(ui, 12.0))
                .color(p.fail),
        );
        ui.label(text);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tokens.json 里的值（XRobot Style `color.tokens`），逐项对照。
    #[test]
    fn palettes_match_tokens() {
        let tokens: [(&str, Color32, Color32, u32, u32); 15] = [
            ("paper", LIGHT.paper, DARK.paper, 0xf2f3f2, 0x0c0e0d),
            (
                "paper-raised",
                LIGHT.raised,
                DARK.raised,
                0xffffff,
                0x151816,
            ),
            (
                "paper-sunken",
                LIGHT.sunken,
                DARK.sunken,
                0xe6e8e6,
                0x070908,
            ),
            ("line", LIGHT.line, DARK.line, 0xd3d6d3, 0x2a2e2c),
            (
                "line-strong",
                LIGHT.line_strong,
                DARK.line_strong,
                0x7c827e,
                0x6f7672,
            ),
            ("ink", LIGHT.ink, DARK.ink, 0x111312, 0xe6e8e6),
            (
                "ink-muted",
                LIGHT.ink_muted,
                DARK.ink_muted,
                0x4d524f,
                0xa3a9a5,
            ),
            ("on-ink", LIGHT.on_ink, DARK.on_ink, 0xf2f3f2, 0x0c0e0d),
            ("ch0", LIGHT.ch[0], DARK.ch[0], 0x7a5c00, 0xffd84d),
            ("ch1", LIGHT.ch[1], DARK.ch[1], 0x006573, 0x4fdded),
            ("ch2", LIGHT.ch[2], DARK.ch[2], 0xa1206f, 0xff85cb),
            ("ch3", LIGHT.ch[3], DARK.ch[3], 0x2a6e1b, 0x8be36a),
            ("pass", LIGHT.pass, DARK.pass, 0x16693a, 0x5bd68a),
            ("fail", LIGHT.fail, DARK.fail, 0xb42318, 0xff8a7a),
            ("blocked", LIGHT.blocked, DARK.blocked, 0x7d5100, 0xf0b54a),
        ];
        for (name, light, dark, l, d) in tokens {
            assert_eq!(light, hex(l), "{name} light");
            assert_eq!(dark, hex(d), "{name} dark");
        }
        assert_eq!(hex(0x123456), Color32::from_rgb(0x12, 0x34, 0x56));
    }

    /// 直角、无阴影；正文 14、标题 20，按钮高度不小于可点击区域。
    #[test]
    fn style_is_square_and_flat() {
        for theme in [Theme::Light, Theme::Dark] {
            let s = style(theme, &[BOLD]);
            let v = &s.visuals;
            assert_eq!(v.window_corner_radius, CornerRadius::ZERO);
            assert_eq!(v.widgets.inactive.corner_radius, CornerRadius::ZERO);
            assert_eq!(v.window_shadow, Shadow::NONE);
            assert_eq!(v.popup_shadow, Shadow::NONE);
            assert_eq!(v.panel_fill, Palette::of(theme).paper);
            // 强调靠反相：hover 是 ink 底 on-ink 字
            assert_eq!(v.widgets.hovered.bg_fill, Palette::of(theme).ink);
            assert_eq!(v.widgets.hovered.fg_stroke.color, Palette::of(theme).on_ink);
            assert_eq!(s.text_styles[&TextStyle::Heading].size, 20.0);
            assert_eq!(s.text_styles[&TextStyle::Body].size, 14.0);
            assert!(s.spacing.interact_size.y >= TARGET);
        }
    }

    #[test]
    fn channels_cycle_and_choice_round_trips() {
        assert_eq!(LIGHT.channel(5), LIGHT.ch[1]);
        for c in Choice::ALL {
            assert_eq!(Choice::from_key(c.key()), c);
        }
    }
}
