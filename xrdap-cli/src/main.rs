use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::Parser;

fn main() {
    let cli = xrdap_cli::Cli::parse();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let watching = matches!(cli.cmd, xrdap_cli::Cmd::Watch { .. });
    // 采样时第一次 Ctrl-C 让采样正常停止（停止探针端采样、读完缓冲）；第二次直接退出。
    // 其他命令直接退出。
    let _ = ctrlc::set_handler(move || {
        if !watching || flag.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
    });
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    if let Err(e) = xrdap_cli::run(cli, &mut out, &mut err, &stop) {
        eprintln!("错误：{e}");
        std::process::exit(1);
    }
}
