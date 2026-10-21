//! 界面用到的数据类型：连接方式、监视项、变量表的行、CSV 录制、共享状态。

use std::fs::File;
use std::io::BufWriter;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui::Color32;
use xrdap::expr::Target;

use super::series::Series;

/// 曲线最多保留的时长和点数（每条）
pub(super) const KEEP_S: f64 = 120.0;
pub(super) const MAX_POINTS: usize = 500_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    Usb,
    Serial,
    Tcp,
}

impl Kind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Kind::Usb => "USB",
            Kind::Serial => "串口",
            Kind::Tcp => "TCP",
        }
    }
    pub(super) fn key(self) -> &'static str {
        match self {
            Kind::Usb => "usb",
            Kind::Serial => "serial",
            Kind::Tcp => "tcp",
        }
    }
    pub(super) fn from_key(s: &str) -> Self {
        match s {
            "serial" => Kind::Serial,
            "tcp" => Kind::Tcp,
            _ => Kind::Usb,
        }
    }
}

pub(super) struct Watch {
    pub(super) id: u64,
    pub(super) target: Target,
    pub(super) div: u16,
    pub(super) show: bool,
    pub(super) latest: Option<Vec<u8>>,
    pub(super) failed: bool,
    pub(super) write_text: String,
    pub(super) note: String,
    pub(super) series: Series,
    pub(super) color: Color32,
    /// 画在第几张图（1 起）
    pub(super) chart: u8,
    /// 当前值最近一次变化的时刻，用来高亮
    pub(super) changed_at: Option<Instant>,
}

pub(super) struct VarRow {
    pub(super) name: String,
    pub(super) lower: String,
    pub(super) addr: u64,
    pub(super) ty: String,
    pub(super) size: Option<u64>,
    /// 定义在 HAL、CMSIS、RTOS、C 库等第三方代码里
    pub(super) system: bool,
}

/// 按编译单元路径和变量名粗略判断是不是第三方代码（HAL、CMSIS、RTOS、中间件、C 库）的变量。
pub(super) fn is_system_var(name: &str, unit: &str) -> bool {
    let u = unit.replace('\\', "/").to_ascii_lowercase();
    const UNIT_MARKS: &[&str] = &[
        "drivers/",
        "middlewares/",
        "cmsis",
        "_hal",
        "hal_",
        "freertos",
        "/rtos",
        "rt-thread",
        "lwip",
        "usb_device",
        "usbd_",
        "fatfs",
        "newlib",
        "picolibc",
        "libc",
        "libgcc",
        "/arm-none-eabi/",
        "startup_",
        "syscalls",
        "sysmem",
        "system_stm32",
        "segger",
    ];
    name.starts_with("__")
        || matches!(name, "errno" | "_impure_ptr" | "environ")
        || UNIT_MARKS.iter().any(|m| u.contains(m))
}

pub(super) struct Csv {
    pub(super) path: PathBuf,
    pub(super) out: BufWriter<File>,
    pub(super) rows: u64,
    /// 从这个电脑时间（Unix 微秒）起的帧才写。选文件时对话框挡住界面，期间积压的帧不算
    pub(super) start_unix_us: i64,
}

/// 共享为 USB DAP 的界面状态。
pub(super) struct ShareState {
    pub(super) attach: String,
    pub(super) device: String,
    pub(super) client: Option<SocketAddr>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_var_guess() {
        assert!(is_system_var(
            "hspi1",
            "../Drivers/STM32G4xx_HAL_Driver/Src/stm32g4xx_hal_spi.c"
        ));
        assert!(is_system_var(
            "pxCurrentTCB",
            r"Middlewares\Third_Party\FreeRTOS\Source\tasks.c"
        ));
        assert!(is_system_var(
            "__malloc_av_",
            "newlib/libc/stdlib/mallocr.c"
        ));
        assert!(!is_system_var("motor", "Core/Src/main.c"));
        assert!(!is_system_var("pid_cfg", "User/app/pid.cpp"));
    }
}
