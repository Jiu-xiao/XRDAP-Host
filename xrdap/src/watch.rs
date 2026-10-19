//! 探针端采样：SUB_SET、开始、循环 SUB_READ、解码、停止。
//! 探针时间按 docs/protocol.md 3.5 节对到电脑时间：每次 SUB_READ 记下请求前后的本机时间，
//! 取中点与响应里的探针时间配对；取最近若干次里往返最短的一次作为偏移量。

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::dap::Probe;
use crate::vendor::{self, SubItem, SubSetResult};
use crate::{Error, Result};

/// 解析周期：`1000`（µs）、`500us`、`500µs`、`2ms`、`2.5ms`、`1s`。
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
pub fn parse_spec(text: &str) -> Result<(String, u16, Option<u16>)> {
    let Some((expr, rest)) = text.rsplit_once('@') else {
        return Ok((text.trim().to_string(), 1, None));
    };
    let bad = || Error::usage(format!("{text}：分频要写成 变量@分频 或 变量@分频:相位，例如 speed@10"));
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

    pub fn unix_us(&self, t: Instant) -> i64 {
        let d = t.saturating_duration_since(self.instant0).as_micros() as i64;
        self.unix0_us + d
    }
}

/// 探针时间到电脑时间的偏移量。
#[derive(Debug, Clone)]
pub struct ClockSync {
    clock: HostClock,
    recent: VecDeque<(i64, i64)>, // (往返 µs, 偏移 µs)
    window: usize,
}

impl ClockSync {
    pub fn new(clock: HostClock) -> Self {
        Self {
            clock,
            recent: VecDeque::new(),
            window: 32,
        }
    }

    /// 记一次测量：请求前后的本机时间和响应里的探针时间。
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
    pub fn offset(&self) -> Option<(i64, i64)> {
        self.recent
            .iter()
            .min_by_key(|(rtt, _)| *rtt)
            .map(|&(rtt, off)| (off, rtt / 2))
    }

    pub fn to_host(&self, probe_us: u64) -> Option<i64> {
        self.offset().map(|(off, _)| probe_us as i64 + off)
    }
}

/// 一帧采样，带电脑时间。
#[derive(Debug, Clone, PartialEq)]
pub struct HostFrame {
    /// 节拍序号（展开成 64 位，不回绕）
    pub tick: u64,
    pub probe_us: u64,
    /// 对到电脑时钟的 Unix 微秒
    pub host_unix_us: i64,
    /// 每个条目一个：None 表示这一拍没有到期；Some(None) 表示读取失败
    pub values: Vec<Option<Option<Vec<u8>>>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub frames: u64,
    /// 探针报告的累计缓冲丢帧数
    pub dropped: u32,
    /// 探针报告的累计跳过节拍数
    pub skipped: u32,
    /// 电脑这边发现的帧序号缺口：应有帧（有条目到期）却没收到的节拍数
    pub missing: u64,
    /// 读取失败的条目值个数
    pub failed_values: u64,
}

/// SUB_SET 没有成功。
#[derive(Debug)]
pub enum StartError {
    /// 0x03：估计的单帧耗时大于周期，或条目太多
    Rejected(SubSetResult),
    Other(Error),
}

impl From<Error> for StartError {
    fn from(e: Error) -> Self {
        StartError::Other(e)
    }
}

/// 根据 est_frame_us 建议一个周期：留 25% 余量，取整到 50 µs。
pub fn suggest_period(est_frame_us: u32) -> u32 {
    let p = est_frame_us as u64 * 5 / 4;
    (p.div_ceil(50) * 50).max(50) as u32
}

pub struct Sampler<'a> {
    probe: &'a mut Probe,
    items: Vec<SubItem>,
    pub period_us: u32,
    pub setup: SubSetResult,
    sync: ClockSync,
    last_tick: Option<u64>,
    stats: Stats,
    running: bool,
    max_read: u16,
}

impl<'a> Sampler<'a> {
    /// 配置并开始采样。探针还在采样（上次没停）时先停掉再配置。
    pub fn start(
        probe: &'a mut Probe,
        items: Vec<SubItem>,
        period_us: u32,
        stop_on_full: bool,
    ) -> std::result::Result<Self, StartError> {
        let flags = u8::from(stop_on_full);
        let mut setup = probe.sub_set(period_us, flags, &items)?;
        if setup.status == vendor::STATUS_STATE {
            probe.stop_sampling()?;
            setup = probe.sub_set(period_us, flags, &items)?;
        }
        match setup.status {
            vendor::STATUS_OK => {}
            vendor::STATUS_RESOURCE if setup.est_frame_us.is_some() => {
                return Err(StartError::Rejected(setup))
            }
            status => {
                return Err(StartError::Other(Error::Status {
                    cmd: vendor::CMD_SUB_SET,
                    status,
                }))
            }
        }
        let max_read = (probe.packet_size().min(1024) - vendor::SUB_READ_HEADER) as u16;
        let mut s = Sampler {
            probe,
            items,
            period_us,
            setup,
            sync: ClockSync::new(HostClock::new()),
            last_tick: None,
            stats: Stats::default(),
            running: false,
            max_read,
        };
        match s.probe.sub_ctrl(vendor::SUB_START)? {
            vendor::STATUS_OK => {}
            status => {
                return Err(StartError::Other(Error::Status {
                    cmd: vendor::CMD_SUB_CTRL,
                    status,
                }))
            }
        }
        s.running = true;
        Ok(s)
    }

    pub fn items(&self) -> &[SubItem] {
        &self.items
    }

    /// 采样期间探针仍接受厂商的 READ、WRITE 命令（例如在线改参数），用这个拿到探针。
    pub fn probe_mut(&mut self) -> &mut Probe {
        self.probe
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn clock_offset(&self) -> Option<(i64, i64)> {
        self.sync.offset()
    }

    /// 读一次 SUB_READ，返回解码好的帧和缓冲里剩余的字节数。
    pub fn poll(&mut self) -> Result<(Vec<HostFrame>, u32)> {
        let (r, before, after) = self.probe.sub_read(self.max_read)?;
        self.sync.update(before, after, r.probe_time_us);
        self.stats.dropped = r.dropped;
        self.stats.skipped = r.skipped;
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

    /// 两次 SUB_READ 之间建议等多久：缓冲能存的时间的 1/4，不超过 50 ms。
    pub fn poll_interval(&self, ring_bytes: u32) -> Duration {
        let per_tick = vendor::average_bytes_per_tick(&self.items).max(1.0);
        let fill_us = ring_bytes as f64 / per_tick * self.period_us as f64;
        Duration::from_micros((fill_us / 4.0).clamp(1000.0, 50_000.0) as u64)
    }

    /// 停止采样，把缓冲里剩下的帧读完。
    pub fn stop(&mut self) -> Result<Vec<HostFrame>> {
        if !self.running {
            return Ok(Vec::new());
        }
        self.running = false;
        self.probe.stop_sampling()?;
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
            let _ = self.probe.stop_sampling();
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
        assert_eq!(parse_spec("0x20000000:u32@10:3").unwrap(), ("0x20000000:u32".into(), 10, Some(3)));
        assert!(parse_spec("x@0").is_err());
        assert!(parse_spec("x@10:10").is_err());
        assert_eq!(suggest_period(1234), 1550);
    }

    #[test]
    fn clock_sync_prefers_short_round_trip() {
        let clock = HostClock::new();
        let t0 = clock.instant0;
        let mut s = ClockSync::new(clock.clone());
        // 往返 10 ms：中点 5 ms，探针时间 1 000 000 -> 偏移 unix0 + 5000 - 1e6
        s.update(t0, t0 + Duration::from_millis(10), 1_000_000);
        // 往返 1 ms：中点 20.5 ms，探针时间 1 020 000
        s.update(t0 + Duration::from_millis(20), t0 + Duration::from_millis(21), 1_020_000);
        let (off, err) = s.offset().unwrap();
        assert_eq!(off, clock.unix0_us + 20_500 - 1_020_000);
        assert_eq!(err, 500);
        assert_eq!(s.to_host(1_020_000), Some(clock.unix0_us + 20_500));
    }
}
