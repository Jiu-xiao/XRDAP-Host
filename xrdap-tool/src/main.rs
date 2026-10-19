//! `xrdap`：不带命令运行时打开图形界面，带命令时在终端里执行。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::Parser;
use xrdap_tool::cli::{self, Cli, Cmd};

fn main() {
    let cli = Cli::parse();
    if cli.cmd.is_none() {
        hide_own_console();
        if let Err(e) = xrdap_tool::gui::run() {
            eprintln!("错误：{e}");
            std::process::exit(1);
        }
        return;
    }

    // watch 和 share 的第一次 Ctrl-C 让它们正常结束（停止采样、断开客户端）；第二次直接退出。
    // 其他命令直接退出。
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let graceful = matches!(cli.cmd, Some(Cmd::Watch { .. } | Cmd::Share { .. }));
    let _ = ctrlc::set_handler(move || {
        if !graceful || flag.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
    });
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    if let Err(e) = cli::run(cli, &mut out, &mut err, &stop) {
        eprintln!("错误：{e}");
        std::process::exit(1);
    }
}

/// 双击启动时 Windows 会为程序新开一个控制台窗口；图形界面用不到它，关掉。从终端里启动时
/// 控制台属于终端，保留。
#[cfg(windows)]
fn hide_own_console() {
    use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    let mut pids = [0u32; 2];
    // 只有本进程挂在这个控制台上，说明控制台是为本进程新开的
    if unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) } == 1 {
        unsafe { FreeConsole() };
    }
}

#[cfg(not(windows))]
fn hide_own_console() {}
