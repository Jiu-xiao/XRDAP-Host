//! 只用系统字体（XRobot Style）：无衬线排正文和标题，等宽排数值、标签和路径，各有常规和粗体
//! 两种字重。每一类在候选里取第一个存在的文件；中文字体同时作为等宽字体的回退。
//! 找不到时那一类退回 egui 自带的字体，界面照常可用。

use std::sync::Arc;

use eframe::egui::{self, FontFamily};

use super::theme::{BOLD, MONO_BOLD};

const SANS: &[&str] = &[
    "C:/Windows/Fonts/msyh.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/System/Library/Fonts/PingFang.ttc",
];
const SANS_BOLD: &[&str] = &[
    "C:/Windows/Fonts/msyhbd.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Bold.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Bold.ttc",
];
const MONO: &[&str] = &[
    "C:/Windows/Fonts/CascadiaMono.ttf",
    "C:/Windows/Fonts/consola.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/System/Library/Fonts/SFNSMono.ttf",
];
const MONO_BOLD_FILES: &[&str] = &[
    "C:/Windows/Fonts/consolab.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
];

/// 装好的字体：说明（给日志用），以及装上的命名字体族（粗体）。
pub struct Installed {
    pub summary: String,
    pub families: Vec<&'static str>,
}

fn load(candidates: &[&'static str]) -> Option<(&'static str, Arc<egui::FontData>)> {
    candidates.iter().find_map(|p| {
        let data = std::fs::read(p).ok()?;
        Some((*p, Arc::new(egui::FontData::from_owned(data))))
    })
}

pub fn install(ctx: &egui::Context) -> Installed {
    let mut fonts = egui::FontDefinitions::default();
    let mut used = Vec::new();
    let mut families = Vec::new();
    let mut put = |key: &str, candidates, family: FontFamily, fallback: Option<&str>| {
        let Some((path, data)) = load(candidates) else {
            return false;
        };
        used.push(path);
        fonts.font_data.insert(key.into(), data);
        let list = fonts.families.entry(family).or_default();
        list.insert(0, key.into());
        if let Some(f) = fallback {
            list.push(f.into());
        }
        true
    };
    let sans = put("sans", SANS, FontFamily::Proportional, None);
    put("mono", MONO, FontFamily::Monospace, sans.then_some("sans"));
    let sans_bold = put(
        "sans-bold",
        SANS_BOLD,
        FontFamily::Name(BOLD.into()),
        sans.then_some("sans"),
    );
    if sans_bold {
        families.push(BOLD);
    }
    if put(
        "mono-bold",
        MONO_BOLD_FILES,
        FontFamily::Name(MONO_BOLD.into()),
        sans_bold.then_some("sans-bold"),
    ) {
        families.push(MONO_BOLD);
    }
    // 命名字体族里缺的字形最后退回 egui 自带字体
    for name in &families {
        let defaults = fonts.families[&FontFamily::Proportional].clone();
        fonts
            .families
            .entry(FontFamily::Name((*name).into()))
            .or_default()
            .extend(defaults);
    }
    ctx.set_fonts(fonts);
    let summary = if used.is_empty() {
        "没有找到系统字体，中文会显示成方框".into()
    } else {
        format!("字体：{}", used.join("，"))
    };
    Installed { summary, families }
}
