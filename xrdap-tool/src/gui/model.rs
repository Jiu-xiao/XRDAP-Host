//! 界面用到的数据类型：找到的探针、监视项、变量表的行、CSV 录制、共享状态。

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::thread::JoinHandle;
use std::time::Instant;

use eframe::egui::Color32;
use xrdap::discover::{ProbeEntry, SerialPortInfo};
use xrdap::expr::Target;

use super::series::Series;

/// 曲线最多保留的时长和点数（每条）
pub(super) const KEEP_S: f64 = 120.0;
pub(super) const MAX_POINTS: usize = 500_000;

/// 后台查找的结果：找到的探针，和没有打开检查的其他串口。
#[derive(Default)]
pub(super) struct Found {
    pub(super) probes: Vec<ProbeEntry>,
    pub(super) others: Vec<SerialPortInfo>,
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

/// 正在录制的 CSV。一行一行交给写文件的线程，界面线程不碰文件。
pub(super) struct Csv {
    pub(super) path: PathBuf,
    pub(super) rows: u64,
    /// 从这个电脑时间（Unix 微秒）起的帧才写。选文件时对话框挡住界面，期间积压的帧不算
    pub(super) start_unix_us: i64,
    lines: Sender<String>,
    writer: JoinHandle<io::Result<()>>,
}

impl Csv {
    /// 开始录制：写文件的线程创建文件、先写表头。
    pub(super) fn start(path: &Path, header: String, start_unix_us: i64) -> Self {
        let (lines, rx) = channel::<String>();
        let file = path.to_path_buf();
        let writer = std::thread::spawn(move || {
            let mut out = BufWriter::new(File::create(&file)?);
            writeln!(out, "{header}")?;
            for line in rx {
                writeln!(out, "{line}")?;
            }
            out.flush()
        });
        Self {
            path: path.to_path_buf(),
            rows: 0,
            start_unix_us,
            lines,
            writer,
        }
    }

    /// 交一行出去；写文件的线程已经因为出错退出时返回 false。
    pub(super) fn push(&mut self, line: String) -> bool {
        self.rows += 1;
        self.lines.send(line).is_ok()
    }

    /// 结束录制，等写文件的线程写完，返回写了多少行或出错的原因。
    pub(super) fn finish(self) -> io::Result<u64> {
        drop(self.lines);
        match self.writer.join() {
            Ok(r) => r.map(|()| self.rows),
            Err(_) => Err(io::Error::other("写文件的线程异常退出")),
        }
    }
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
