//! 探针端采样：SUB_START、循环 SUB_READ、解码、停止，以及按探针的计数器判断跟不跟得上。
//!
//! 探针时间按协议文档 3.5 节对到电脑时间：每次 SUB_READ 记下请求前后的本机时间，
//! 取中点与响应里的探针时间配对；取最近若干次里往返最短的一次作为偏移量。
//! 协议文档：<https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.md>
//!
//! Probe-side sampling: SUB_START, a SUB_READ loop, decoding, stop, and judging from the
//! probe's counters whether sampling keeps up.
//!
//! Probe time is aligned to host time as in section 3.5 of the protocol document: each
//! SUB_READ records the local time before and after the request and pairs the midpoint with
//! the probe time in the response; the offset is taken from the round trip that was shortest
//! among the most recent ones.
//! Protocol document: <https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.md>

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::dap::Probe;
use crate::vendor::{self, Item, SubItem};
use crate::{Error, Result};

/// 让探针停止采样（SUB_CTRL stop，协议文档 3.4）。没在采样时也成功。只用来停自己开始的采样。
///
/// Stop sampling on the probe (SUB_CTRL stop); succeeds when not sampling too. Used only for
/// sampling this host started.
pub fn stop(probe: &mut Probe) -> Result<()> {
    probe.sub_ctrl(vendor::SUB_STOP)
}

/// 解析周期：`1000`（µs）、`500us`、`500µs`、`2ms`、`2.5ms`、`1s`。
///
/// Parse a period: `1000` (µs), `500us`, `500µs`, `2ms`, `2.5ms` or `1s`.
pub fn parse_period(text: &str) -> Result<u32> {
    let t = text.trim().to_ascii_lowercase();
    let (num, scale) = if let Some(n) = t.strip_suffix("ms") {
        (n, 1000.0)
    } else if let Some(n) = t.strip_suffix("us").or_else(|| t.strip_suffix("µs")) {
        (n, 1.0)
    } else if let Some(n) = t.strip_suffix('s') {
        (n, 1_000_000.0)
    } else {
        (t.as_str(), 1.0)
    };
    let v: f64 = num
        .trim()
        .parse()
        .map_err(|_| Error::usage(format!("周期 {text} 写得不对，例如 1000、500us、2ms")))?;
    let us = (v * scale).round();
    if !(1.0..=u32::MAX as f64).contains(&us) {
        return Err(Error::usage(format!("周期 {text} 超出范围")));
    }
    Ok(us as u32)
}

/// 采样变量的写法 `expr[@div[:phase]]`，返回 (表达式, 分频, 相位)。
///
/// Parse the form of a sampled variable, `expr[@div[:phase]]`, and return (expression, divider,
/// phase).
pub fn parse_spec(text: &str) -> Result<(String, u16, Option<u16>)> {
    let Some((expr, rest)) = text.rsplit_once('@') else {
        return Ok((text.trim().to_string(), 1, None));
    };
    let bad = || {
        Error::usage(format!(
            "{text}：分频要写成 变量@分频 或 变量@分频:相位，例如 speed@10"
        ))
    };
    let (div, phase) = match rest.split_once(':') {
        Some((d, p)) => (d, Some(p)),
        None => (rest, None),
    };
    let div: u16 = div.trim().parse().map_err(|_| bad())?;
    if div == 0 {
        return Err(bad());
    }
    let phase = match phase {
        Some(p) => {
            let p: u16 = p.trim().parse().map_err(|_| bad())?;
            if p >= div {
                return Err(Error::usage(format!("{text}：相位要小于分频")));
            }
            Some(p)
        }
        None => None,
    };
    Ok((expr.trim().to_string(), div, phase))
}

/// 电脑的时钟：单调时钟加开始时的系统时间，得到 Unix 微秒。
///
/// The host clock: a monotonic clock plus the system time at its start, giving Unix
/// microseconds.
#[derive(Debug, Clone)]
pub struct HostClock {
    instant0: Instant,
    unix0_us: i64,
}

impl Default for HostClock {
    fn default() -> Self {
        Self::new()
    }
}

impl HostClock {
    /// 以当前时刻为起点建立时钟。
    ///
    /// Create a clock that starts at the present moment.
    pub fn new() -> Self {
        let unix0_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);
        Self {
            instant0: Instant::now(),
            unix0_us,
        }
    }

    /// 单调时刻 `t` 对应的 Unix 微秒。
    ///
    /// The Unix microseconds that correspond to the monotonic instant `t`.
    pub fn unix_us(&self, t: Instant) -> i64 {
        let d = t.saturating_duration_since(self.instant0).as_micros() as i64;
        self.unix0_us + d
    }
}

/// 探针时间到电脑时间的偏移量。
///
/// The offset from probe time to host time.
#[derive(Debug, Clone)]
pub struct ClockSync {
    clock: HostClock,
    recent: VecDeque<(i64, i64)>, // (往返 µs, 偏移 µs)
    window: usize,
}

impl ClockSync {
    /// 用 `clock` 作电脑时钟，建立空的同步状态。
    ///
    /// Create an empty sync state using `clock` as the host clock.
    pub fn new(clock: HostClock) -> Self {
        Self {
            clock,
            recent: VecDeque::new(),
            window: 32,
        }
    }

    /// 记一次测量：请求前后的本机时间和响应里的探针时间。
    ///
    /// Record one measurement: the local time before and after the request and the probe time
    /// in the response.
    pub fn update(&mut self, before: Instant, after: Instant, probe_us: u64) {
        let b = self.clock.unix_us(before);
        let a = self.clock.unix_us(after);
        let mid = b + (a - b) / 2;
        self.recent.push_back((a - b, mid - probe_us as i64));
        if self.recent.len() > self.window {
            self.recent.pop_front();
        }
    }

    /// 当前的偏移量（电脑 Unix µs = 探针 µs + 偏移）和它的误差（半个往返）。
    ///
    /// The current offset (host Unix µs = probe µs + offset) and its error (half a round trip).
    pub fn offset(&self) -> Option<(i64, i64)> {
        self.recent
            .iter()
            .min_by_key(|(rtt, _)| *rtt)
            .map(|&(rtt, off)| (off, rtt / 2))
    }

    /// 探针时间换算成电脑的 Unix 微秒；还没有测量时为 None。
    ///
    /// Convert a probe time to host Unix microseconds; None before the first measurement.
    pub fn to_host(&self, probe_us: u64) -> Option<i64> {
        self.offset().map(|(off, _)| probe_us as i64 + off)
    }
}

/// 一帧采样，带电脑时间。
///
/// One sampled frame with host time.
#[derive(Debug, Clone, PartialEq)]
pub struct HostFrame {
    /// 节拍序号（展开成 64 位，不回绕）。
    ///
    /// Tick number (widened to 64 bits, without wrap-around).
    pub tick: u64,
    /// 探针时间，µs。
    ///
    /// Probe time in µs.
    pub probe_us: u64,
    /// 对到电脑时钟的 Unix 微秒。
    ///
    /// Unix microseconds aligned to the host clock.
    pub host_unix_us: i64,
    /// 每个条目一个：None 表示这一拍没有到期；Some(None) 表示读取失败。
    ///
    /// One per item: None means the item was not due on this tick; Some(None) means the read
    /// failed.
    pub values: Vec<Option<Option<Vec<u8>>>>,
}

/// 采样的累计统计。
///
/// Cumulative sampling statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// 已收到的帧数。
    ///
    /// Number of frames received.
    pub frames: u64,
    /// 探针报告的累计缓冲丢帧数。
    ///
    /// Cumulative number of frames dropped by the buffer, as reported by the probe.
    pub dropped: u32,
    /// 探针报告的累计跳过节拍数。
    ///
    /// Cumulative number of skipped ticks, as reported by the probe.
    pub skipped: u32,
    /// 电脑这边发现的帧序号缺口：应有帧（有条目到期）却没收到的节拍数。
    ///
    /// Gaps in frame numbers found on the host: ticks with at least one due item whose frame
    /// was not received.
    pub missing: u64,
    /// 读取失败的条目值个数。
    ///
    /// Number of item values whose read failed.
    pub failed_values: u64,
    /// 最近一次 SUB_READ 时缓冲里还没读走的帧字节数。
    ///
    /// Frame bytes still unread in the buffer at the last SUB_READ.
    pub backlog: u32,
    /// 采样缓冲的字节数（INFO）。
    ///
    /// Size of the sampling buffer in bytes (INFO).
    pub ring_bytes: u32,
}

/// 采样跟不上的原因（协议文档 3.5 节）。
///
/// Why sampling does not keep up (protocol section 3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lag {
    /// 探针跳过了节拍：SWD 读不完一拍的条目。
    ///
    /// The probe skipped ticks: SWD cannot read one tick's items in time.
    Swd,
    /// 丢了帧或缓冲快满：电脑读得不够快，或链路带宽不够。
    ///
    /// Frames were dropped or the buffer is nearly full: the host reads too slowly or the link
    /// lacks bandwidth.
    Read,
}

impl Lag {
    /// 给用户的说明和建议。
    ///
    /// The explanation and advice for users.
    pub fn advice(self) -> &'static str {
        match self {
            Lag::Swd => "探针跳过了节拍，SWD 跟不上：加长周期、给慢变量加分频，或者减少变量",
            Lag::Read => "探针丢了帧或缓冲快满，读取跟不上：给变量加分频，或者减少变量",
        }
    }
}

impl Stats {
    /// 按探针的计数器判断采样跟不跟得上：跳拍说明 SWD 跟不上；丢帧、或缓冲里没读走的字节超过
    /// 缓冲的四分之三，说明读得不够快。跟得上时为 None。
    ///
    /// Judge from the probe's counters whether sampling keeps up: skipped ticks mean SWD cannot
    /// keep up; dropped frames, or unread bytes above three quarters of the buffer, mean the
    /// host reads too slowly. None when it keeps up.
    pub fn lag(&self) -> Option<Lag> {
        if self.skipped > 0 {
            Some(Lag::Swd)
        } else if self.dropped > 0 || self.backlog as u64 * 4 > self.ring_bytes as u64 * 3 {
            Some(Lag::Read)
        } else {
            None
        }
    }
}

/// 要采样的一个条目：每 `div` 拍采一次；`phase` 为 None 时由 [`Sampler::start`] 错开相位。
///
/// One item to sample every `div` ticks; with `phase` None, [`Sampler::start`] staggers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Var {
    /// 条目。
    ///
    /// The item.
    pub item: Item,
    /// 分频。
    ///
    /// Divider.
    pub div: u16,
    /// 相位；None 表示由 [`Sampler::start`] 分配。
    ///
    /// Phase; None lets [`Sampler::start`] assign it.
    pub phase: Option<u16>,
}

/// 一次进行中的探针端采样。丢弃时如果还在采样会停止采样。
///
/// A running probe-side sampling. Dropping it stops sampling if it is still running.
pub struct Sampler<'a> {
    probe: &'a mut Probe,
    items: Vec<SubItem>,
    /// 采样周期，µs。
    ///
    /// Sampling period in µs.
    pub period_us: u32,
    sync: ClockSync,
    last_tick: Option<u64>,
    stats: Stats,
    /// 这个 Sampler 开始的采样还没有停
    running: bool,
    /// 最近一次 SUB_READ 时探针还在采样
    on_probe: bool,
    max_read: u16,
}

impl<'a> Sampler<'a> {
    /// 用一条 SUB_START 开始采样：条目数不超过探针 INFO 报告的上限，没给相位的分频条目错开
    /// 相位（[`vendor::assign_phases`]）。探针不估计耗时，跟不跟得上看运行时的计数器
    /// （[`Stats::lag`]）。探针已经在采样时返回错误，不停别人的采样。
    ///
    /// Start sampling with one SUB_START: at most the probe's INFO item limit, unphased divided
    /// items staggered. The probe does not estimate timing; whether it keeps up shows in the
    /// run-time counters ([`Stats::lag`]). When the probe is already sampling this is an error,
    /// and that sampling is left alone.
    pub fn start(
        probe: &'a mut Probe,
        vars: Vec<Var>,
        period_us: u32,
        stop_on_full: bool,
    ) -> Result<Self> {
        let info = probe.vendor_info()?;
        if vars.len() > info.max_items as usize {
            return Err(Error::usage(format!(
                "探针一次最多采样 {} 个变量，现在有 {} 个",
                info.max_items,
                vars.len()
            )));
        }
        let fixed: Vec<bool> = vars.iter().map(|v| v.phase.is_some()).collect();
        let mut items: Vec<SubItem> = vars
            .into_iter()
            .map(|v| SubItem {
                item: v.item,
                div: v.div,
                phase: v.phase.unwrap_or(0),
            })
            .collect();
        vendor::assign_phases(&mut items, &fixed);
        probe.sub_start(period_us, u8::from(stop_on_full), &items)?;
        let max_read = (probe.packet_size().min(1024) - vendor::SUB_READ_HEADER) as u16;
        Ok(Sampler {
            probe,
            items,
            period_us,
            sync: ClockSync::new(HostClock::new()),
            last_tick: None,
            stats: Stats {
                ring_bytes: info.ring_bytes,
                ..Stats::default()
            },
            running: true,
            on_probe: true,
            max_read,
        })
    }

    /// 最近一次 SUB_READ 时探针还在采样。为 false 时采样已在探针上停止（例如会话结束），
    /// 缓冲里剩下的帧照样可以读完。
    ///
    /// Whether the probe was still sampling at the last SUB_READ. When false, sampling stopped
    /// on the probe (for example because the session ended); the remaining frames can still be
    /// read.
    pub fn sampling_on_probe(&self) -> bool {
        self.on_probe
    }

    /// 采样条目表，帧里的下标对应这张表。
    ///
    /// The item table; indices in frames refer to it.
    pub fn items(&self) -> &[SubItem] {
        &self.items
    }

    /// 采样期间探针仍接受厂商的 READ、WRITE 命令（例如在线改参数），用这个拿到探针。
    ///
    /// The probe still accepts the vendor READ and WRITE commands while sampling (for example to
    /// change a parameter online); this gives access to the probe.
    pub fn probe_mut(&mut self) -> &mut Probe {
        self.probe
    }

    /// 目前的统计。
    ///
    /// The statistics so far.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// 探针时间到电脑时间的偏移量和误差，见 [`ClockSync::offset`]。
    ///
    /// The offset from probe time to host time and its error; see [`ClockSync::offset`].
    pub fn clock_offset(&self) -> Option<(i64, i64)> {
        self.sync.offset()
    }

    /// 读一次 SUB_READ，返回解码好的帧和缓冲里剩余的字节数。
    ///
    /// Do one SUB_READ and return the decoded frames and the bytes left in the buffer.
    pub fn poll(&mut self) -> Result<(Vec<HostFrame>, u32)> {
        let (r, before, after) = self.probe.sub_read(self.max_read)?;
        self.sync.update(before, after, r.probe_time_us);
        self.on_probe = r.sampling;
        self.stats.dropped = r.dropped;
        self.stats.skipped = r.skipped;
        self.stats.backlog = r.remaining;
        let frames = vendor::decode_frames(&r.frames, &self.items, r.frame_count)?;
        let mut out = Vec::with_capacity(frames.len());
        for f in frames {
            let tick = self.unwrap_tick(f.tick);
            if let Some(last) = self.last_tick {
                let gap = tick.saturating_sub(last + 1);
                self.stats.missing += if gap <= 100_000 {
                    (last + 1..tick)
                        .filter(|&k| self.items.iter().any(|s| s.due(k)))
                        .count() as u64
                } else {
                    gap
                };
            }
            self.last_tick = Some(tick);
            let mut values = vec![None; self.items.len()];
            for (i, v) in f.values {
                if v.is_none() {
                    self.stats.failed_values += 1;
                }
                values[i] = Some(v);
            }
            self.stats.frames += 1;
            out.push(HostFrame {
                tick,
                probe_us: f.timestamp_us,
                host_unix_us: self.sync.to_host(f.timestamp_us).unwrap_or(0),
                values,
            });
        }
        Ok((out, r.remaining))
    }

    /// 节拍序号在帧里只有 32 位，按上一帧展开成 64 位。
    fn unwrap_tick(&self, t: u32) -> u64 {
        match self.last_tick {
            None => t as u64,
            Some(last) => {
                let hi = last & !0xFFFF_FFFF;
                let mut k = hi | t as u64;
                if k < last {
                    k += 1 << 32;
                }
                k
            }
        }
    }

    /// 两次 SUB_READ 之间建议等多久：缓冲能存的时间的 1/4，限制在 1 ms 到 50 ms 之间。
    ///
    /// Suggested wait between two SUB_READs: a quarter of the time the buffer can hold,
    /// limited to the range 1 ms to 50 ms.
    pub fn poll_interval(&self) -> Duration {
        let per_tick = vendor::average_bytes_per_tick(&self.items).max(1.0);
        let fill_us = self.stats.ring_bytes as f64 / per_tick * self.period_us as f64;
        Duration::from_micros((fill_us / 4.0).clamp(1000.0, 50_000.0) as u64)
    }

    /// 停止采样，把缓冲里剩下的帧读完。
    ///
    /// Stop sampling and read the frames left in the buffer.
    pub fn stop(&mut self) -> Result<Vec<HostFrame>> {
        if !self.running {
            return Ok(Vec::new());
        }
        self.running = false;
        stop(self.probe)?;
        let mut out = Vec::new();
        for _ in 0..64 {
            let (frames, remaining) = self.poll()?;
            let empty = frames.is_empty();
            out.extend(frames);
            if remaining == 0 || empty {
                break;
            }
        }
        Ok(out)
    }
}

impl Drop for Sampler<'_> {
    fn drop(&mut self) {
        if self.running {
            let _ = stop(self.probe);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_and_specs() {
        assert_eq!(parse_period("1000").unwrap(), 1000);
        assert_eq!(parse_period("500us").unwrap(), 500);
        assert_eq!(parse_period("500µs").unwrap(), 500);
        assert_eq!(parse_period("2.5ms").unwrap(), 2500);
        assert_eq!(parse_period("1s").unwrap(), 1_000_000);
        assert!(parse_period("0").is_err());
        assert!(parse_period("fast").is_err());
        assert_eq!(parse_spec("cnt_a").unwrap(), ("cnt_a".into(), 1, None));
        assert_eq!(parse_spec("a.b@10").unwrap(), ("a.b".into(), 10, None));
        assert_eq!(
            parse_spec("0x20000000:u32@10:3").unwrap(),
            ("0x20000000:u32".into(), 10, Some(3))
        );
        assert!(parse_spec("x@0").is_err());
        assert!(parse_spec("x@10:10").is_err());
    }

    #[test]
    fn clock_sync_prefers_short_round_trip() {
        let clock = HostClock::new();
        let t0 = clock.instant0;
        let mut s = ClockSync::new(clock.clone());
        // 往返 10 ms：中点 5 ms，探针时间 1 000 000 -> 偏移 unix0 + 5000 - 1e6
        s.update(t0, t0 + Duration::from_millis(10), 1_000_000);
        // 往返 1 ms：中点 20.5 ms，探针时间 1 020 000
        s.update(
            t0 + Duration::from_millis(20),
            t0 + Duration::from_millis(21),
            1_020_000,
        );
        let (off, err) = s.offset().unwrap();
        assert_eq!(off, clock.unix0_us + 20_500 - 1_020_000);
        assert_eq!(err, 500);
        assert_eq!(s.to_host(1_020_000), Some(clock.unix0_us + 20_500));
    }
}
