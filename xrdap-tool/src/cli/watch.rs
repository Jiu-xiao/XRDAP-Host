//! watch：让探针按固定周期采样，打印或写 CSV。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use xrdap::dap::Probe;
use xrdap::expr::Target;

use xrdap::watch::{HostFrame, Lag, Sampler, Var};

use super::text::{display_width, rjust};
use super::CliResult;

pub struct WatchOpts {
    pub period_us: u32,
    pub csv: Option<PathBuf>,
    pub duration: Option<Duration>,
    pub stop_on_full: bool,
    pub print_all: bool,
}

struct Output<'a> {
    targets: Vec<Target>,
    csv: Option<std::io::BufWriter<std::fs::File>>,
    out: &'a mut dyn Write,
    print_all: bool,
    widths: Vec<usize>,
    first_probe_us: Option<u64>,
    last_print: Option<Instant>,
    latest: Vec<Option<String>>,
}

impl Output<'_> {
    fn header(&mut self) -> CliResult {
        if let Some(csv) = &mut self.csv {
            let line = xrdap::csv::header(self.targets.iter().map(|t| t.name.as_str()));
            writeln!(csv, "{line}")?;
            return Ok(());
        }
        let mut line = format!("{} {}", rjust("时间(s)", 12), rjust("节拍", 10));
        for (t, w) in self.targets.iter().zip(&self.widths) {
            line.push(' ');
            line.push_str(&rjust(&t.name, *w));
        }
        writeln!(self.out, "{line}")?;
        Ok(())
    }

    fn frames(&mut self, frames: &[HostFrame]) -> CliResult {
        for f in frames {
            let first = *self.first_probe_us.get_or_insert(f.probe_us);
            let t = (f.probe_us as f64 - first as f64) / 1e6;
            let cells: Vec<Option<String>> = f
                .values
                .iter()
                .zip(&self.targets)
                .map(|(v, t)| xrdap::csv::cell(&t.ty, v))
                .collect();
            if let Some(csv) = &mut self.csv {
                writeln!(csv, "{}", xrdap::csv::row(f, t, &cells))?;
            }
            for (slot, c) in self.latest.iter_mut().zip(&cells) {
                if c.is_some() {
                    *slot = c.clone();
                }
            }
            let now = Instant::now();
            let due = self.print_all
                || self
                    .last_print
                    .is_none_or(|p| now.duration_since(p) >= Duration::from_millis(100));
            if due && self.csv.is_none() {
                self.last_print = Some(now);
                let mut line = format!("{t:>12.6} {:>10}", f.tick);
                for (c, w) in self.latest.iter().zip(&self.widths) {
                    line.push(' ');
                    line.push_str(&rjust(c.as_deref().unwrap_or("-"), *w));
                }
                writeln!(self.out, "{line}")?;
            }
        }
        Ok(())
    }
}

pub fn cmd_watch(
    probe: &mut Probe,
    specs: Vec<(Target, u16, Option<u16>)>,
    opts: &WatchOpts,
    out: &mut dyn Write,
    err: &mut dyn Write,
    stop: &AtomicBool,
) -> CliResult {
    let vars = specs
        .iter()
        .enumerate()
        .map(|(i, (t, div, phase))| Var {
            item: t.item(i as u16),
            div: *div,
            phase: *phase,
        })
        .collect();
    let targets: Vec<Target> = specs.into_iter().map(|(t, _, _)| t).collect();

    let mut sampler = Sampler::start(probe, vars, opts.period_us, opts.stop_on_full)?;

    writeln!(
        out,
        "采样 {} 个变量，周期 {} µs",
        targets.len(),
        opts.period_us
    )?;
    for (t, s) in targets.iter().zip(sampler.items()) {
        if s.div > 1 {
            writeln!(out, "  {} 每 {} 拍采一次，相位 {}", t.name, s.div, s.phase)?;
        }
    }
    let csv = match &opts.csv {
        Some(path) => {
            writeln!(out, "写入 {}，按 Ctrl-C 停止", path.display())?;
            Some(std::io::BufWriter::new(std::fs::File::create(path)?))
        }
        None => {
            writeln!(out, "按 Ctrl-C 停止")?;
            None
        }
    };
    let widths = targets
        .iter()
        .map(|t| display_width(&t.name).max(12))
        .collect();
    let latest = vec![None; targets.len()];
    let mut output = Output {
        targets,
        csv,
        out,
        print_all: opts.print_all,
        widths,
        first_probe_us: None,
        last_print: None,
        latest,
    };
    output.header()?;

    let started = Instant::now();
    let interval = sampler.poll_interval();
    let mut last_status = Instant::now();
    // 跟不上时只提示一次（协议文档 3.5 节：按跳拍、丢帧和缓冲余量判断）
    let mut warned: Option<Lag> = None;
    let result: CliResult = (|| {
        while !stop.load(Ordering::SeqCst) {
            if opts.duration.is_some_and(|d| started.elapsed() >= d) {
                break;
            }
            let (frames, remaining) = sampler.poll()?;
            output.frames(&frames)?;
            let lag = sampler.stats().lag();
            if lag.is_some() && lag != warned {
                warned = lag;
                writeln!(err, "\n提示：{}", lag.map_or("", Lag::advice))?;
            }
            if !sampler.sampling_on_probe() && remaining == 0 {
                writeln!(err, "探针上的采样已经停止（会话结束）")?;
                break;
            }
            if output.csv.is_some() && last_status.elapsed() >= Duration::from_secs(1) {
                last_status = Instant::now();
                let st = sampler.stats();
                write!(
                    err,
                    "\r已采 {} 帧，跳过节拍 {}，缓冲丢帧 {}   ",
                    st.frames, st.skipped, st.dropped
                )?;
                err.flush()?;
            }
            if remaining == 0 {
                let mut wait = interval;
                if let Some(d) = opts.duration {
                    wait = wait.min(d.saturating_sub(started.elapsed()));
                }
                std::thread::sleep(wait);
            }
        }
        Ok(())
    })();
    // 出错时也要停止采样；stop() 顺便读完缓冲里剩下的帧
    let rest = sampler.stop();
    result?;
    output.frames(&rest?)?;
    if let Some(csv) = &mut output.csv {
        csv.flush()?;
        writeln!(err)?;
    }
    let st = sampler.stats();
    let secs = started.elapsed().as_secs_f64();
    writeln!(
        output.out,
        "已停止：{} 帧，用时 {secs:.2} s；探针跳过节拍 {}，缓冲丢帧 {}，帧序号缺口 {}，读取失败的值 {}",
        st.frames, st.skipped, st.dropped, st.missing, st.failed_values
    )?;
    if let (Some((_, half_rtt)), Some(path)) = (sampler.clock_offset(), &opts.csv) {
        writeln!(
            output.out,
            "{} 的 host_time 列是对到电脑时钟的 Unix 时间（秒），按 SUB_READ 的往返时间估计，误差约 ±{half_rtt} µs",
            path.display()
        )?;
    }
    if let Some(lag) = st.lag() {
        writeln!(output.out, "提示：{}", lag.advice())?;
    }
    Ok(())
}
