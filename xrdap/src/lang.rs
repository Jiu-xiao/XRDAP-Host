//! 输出语言：中文环境输出中文，其他环境输出英文。规则和 xrobot、libxr 两个命令行工具相同。
//!
//! 语言依次由环境变量 `XR_LANG`、`LANGUAGE`、`LC_ALL`、`LC_MESSAGES`、`LANG` 决定，第一个非空的
//! 值以 `zh` 开头时输出中文；都没有设置时，Windows 上看系统界面语言，其他系统输出英文。每个进程
//! 只判断一次。
//!
//! Output language: Chinese in a Chinese environment, English elsewhere, by the same rule as
//! the xrobot and libxr command-line tools.
//!
//! The first non-empty of the environment variables `XR_LANG`, `LANGUAGE`, `LC_ALL`,
//! `LC_MESSAGES` and `LANG` decides: a value starting with `zh` selects Chinese. When none is
//! set, Windows follows the system display language and other systems use English. The
//! language is decided once per process.

use std::sync::atomic::{AtomicU8, Ordering};

const VARIABLES: [&str; 5] = ["XR_LANG", "LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"];

/// 0：还没判断；1：中文；2：英文。
///
/// 0: not decided yet; 1: Chinese; 2: English.
static LANG: AtomicU8 = AtomicU8::new(0);

/// 当前是否输出中文。
///
/// Whether output is in Chinese.
pub fn chinese() -> bool {
    match LANG.load(Ordering::Relaxed) {
        0 => {
            let v = if detect() { 1 } else { 2 };
            // 别的线程先判断或先设置了语言时以它为准
            // Another thread that decided or set the language first wins
            match LANG.compare_exchange(0, v, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => v == 1,
                Err(now) => now == 1,
            }
        }
        v => v == 1,
    }
}

/// 不看环境，直接选定语言。给测试和自己选语言的程序用，在输出任何文字之前调用。
///
/// Selects the language regardless of the environment. For tests and for programs that
/// choose the language themselves; call it before any text is produced.
pub fn set_chinese(chinese: bool) {
    LANG.store(if chinese { 1 } else { 2 }, Ordering::Relaxed);
}

/// 按当前语言选择一条文字的英文或中文。
///
/// Chooses the English or the Chinese form of a text for the current language.
pub fn tr(en: &'static str, zh: &'static str) -> &'static str {
    if chinese() {
        zh
    } else {
        en
    }
}

/// 按当前语言格式化一条文字的英文或中文，参数和 `format!` 相同，返回 `String`。
///
/// Formats the English or the Chinese form of a text for the current language; the arguments
/// are those of `format!`, and the result is a `String`.
///
/// ```
/// let n = 3;
/// let s = xrdap::trf!("{n} probes", "{n} 块探针");
/// assert!(s == "3 probes" || s == "3 块探针");
/// ```
#[macro_export]
macro_rules! trf {
    ($en:literal, $zh:literal $(, $($arg:tt)*)?) => {
        if $crate::lang::chinese() {
            ::std::format!($zh $(, $($arg)*)?)
        } else {
            ::std::format!($en $(, $($arg)*)?)
        }
    };
}

fn detect() -> bool {
    for name in VARIABLES {
        let value = std::env::var(name).unwrap_or_default();
        let value = value.trim();
        if !value.is_empty() {
            return value.to_lowercase().starts_with("zh");
        }
    }
    system_chinese()
}

/// Windows 的系统界面语言是否为中文：LANGID 的低 10 位是主语言，0x04 为中文。
///
/// Whether the Windows display language is Chinese: the low 10 bits of a LANGID are the
/// primary language, and 0x04 is Chinese.
#[cfg(windows)]
fn system_chinese() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // SAFETY: 没有参数，只返回一个数
    // SAFETY: no arguments, returns a number
    let lang = unsafe { GetUserDefaultUILanguage() };
    lang & 0x3FF == 0x04
}

#[cfg(not(windows))]
fn system_chinese() -> bool {
    false
}
