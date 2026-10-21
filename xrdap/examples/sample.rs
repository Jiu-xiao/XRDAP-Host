//! 让探针按固定周期采样几个变量，取回 1 秒的数据并打印。
//!
//! ```text
//! cargo run -p xrdap --example sample -- app.elf motor.speed motor.current
//! ```
//!
//! 探针用环境变量 `XRDAP_PROBE` 指定（写法同 `xrdap --probe`），不设时自动选唯一的那块。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use xrdap::addr::Spec;
use xrdap::expr::Resolver;
use xrdap::open::{open, OpenOptions};
use xrdap::vendor::SubItem;
use xrdap::watch::Sampler;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(elf) = args.next() else {
        eprintln!("用法：sample <ELF> <变量>...");
        std::process::exit(2);
    };
    let mut syms = xrdap::elf::load(&PathBuf::from(elf))?;
    let mut resolver = Resolver::new(&mut syms);
    let targets = args
        .map(|e| resolver.resolve(&e))
        .collect::<xrdap::Result<Vec<_>>>()?;

    let spec = Spec::parse(&std::env::var("XRDAP_PROBE").unwrap_or_default());
    let mut probe = open(&spec, &OpenOptions::default())?;
    probe.connect_swd(4_000_000)?;

    // 每拍都采（div 1），周期 1 ms；探针拒绝时会给出估计的单帧耗时
    let items = targets
        .iter()
        .enumerate()
        .map(|(i, t)| SubItem {
            item: t.item(i as u16),
            div: 1,
            phase: 0,
        })
        .collect();
    let mut sampler = Sampler::start(&mut probe, items, 1000, false)
        .map_err(|e| format!("开始采样失败：{e:?}"))?;

    let end = Instant::now() + Duration::from_secs(1);
    let mut frames = Vec::new();
    while Instant::now() < end {
        let (got, _) = sampler.poll()?;
        frames.extend(got);
        std::thread::sleep(Duration::from_millis(10));
    }
    frames.extend(sampler.stop()?);

    for f in frames.iter().step_by(100) {
        let cells: Vec<String> = f
            .values
            .iter()
            .zip(&targets)
            .map(|(v, t)| match v {
                Some(Some(b)) => format!("{}={}", t.name, t.ty.format_plain(b)),
                Some(None) => format!("{}=ERR", t.name),
                None => String::new(),
            })
            .collect();
        println!("t={:.3}s  {}", f.probe_us as f64 / 1e6, cells.join("  "));
    }
    let s = sampler.stats();
    println!(
        "{} 帧，跳拍 {}，丢帧 {}，缺帧 {}",
        frames.len(),
        s.skipped,
        s.dropped,
        s.missing
    );
    Ok(())
}
