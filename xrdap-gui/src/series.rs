//! 一条曲线的数据：按时间追加，超出保留时长的旧点丢掉；画图时按像素做最大最小值抽稀。

use std::collections::VecDeque;

pub struct Series {
    pts: VecDeque<[f64; 2]>,
    keep_s: f64,
    max_points: usize,
}

impl Series {
    pub fn new(keep_s: f64, max_points: usize) -> Self {
        Self {
            pts: VecDeque::new(),
            keep_s,
            max_points,
        }
    }

    pub fn clear(&mut self) {
        self.pts.clear();
    }

    pub fn push(&mut self, t: f64, v: f64) {
        self.pts.push_back([t, v]);
        while self.pts.len() > self.max_points
            || self.pts.front().is_some_and(|p| t - p[0] > self.keep_s)
        {
            self.pts.pop_front();
        }
    }

    /// t 时刻的值：t 之前（含）最后一个点的值；t 早于第一个点时为 None。
    pub fn value_at(&self, t: f64) -> Option<f64> {
        let i = self.pts.partition_point(|p| p[0] <= t);
        (i > 0).then(|| self.pts[i - 1][1])
    }

    /// [t0, t1] 里的点，抽成最多 2 × buckets 个：每个桶保留最小值和最大值（按出现顺序），
    /// 尖峰不会被抽掉。点数本来就少时原样返回。
    pub fn decimated(&self, t0: f64, t1: f64, buckets: usize) -> Vec<[f64; 2]> {
        let start = self.pts.partition_point(|p| p[0] < t0).saturating_sub(1);
        let end = (self.pts.partition_point(|p| p[0] <= t1) + 1).min(self.pts.len());
        if start >= end {
            return Vec::new();
        }
        let n = end - start;
        let buckets = buckets.max(1);
        if n <= buckets * 2 {
            return self.pts.range(start..end).copied().collect();
        }
        let width = (t1 - t0) / buckets as f64;
        let mut out = Vec::with_capacity(buckets * 2 + 2);
        let mut bucket = f64::NAN;
        let (mut lo, mut hi) = ([0.0; 2], [0.0; 2]);
        let flush = |out: &mut Vec<[f64; 2]>, lo: [f64; 2], hi: [f64; 2]| {
            if lo[0] <= hi[0] {
                out.push(lo);
                if hi != lo {
                    out.push(hi);
                }
            } else {
                out.push(hi);
                out.push(lo);
            }
        };
        for &p in self.pts.range(start..end) {
            let b = ((p[0] - t0) / width).floor();
            if b != bucket {
                if !bucket.is_nan() {
                    flush(&mut out, lo, hi);
                }
                bucket = b;
                lo = p;
                hi = p;
            } else {
                if p[1] < lo[1] {
                    lo = p;
                }
                if p[1] > hi[1] {
                    hi = p;
                }
            }
        }
        if !bucket.is_nan() {
            flush(&mut out, lo, hi);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_spikes_and_window() {
        let mut s = Series::new(10.0, 1_000_000);
        for i in 0..10_000 {
            let t = i as f64 / 1000.0;
            s.push(t, if i == 5_000 { 100.0 } else { 0.0 });
        }
        let d = s.decimated(0.0, 10.0, 100);
        assert!(d.len() <= 202);
        assert!(d.iter().any(|p| p[1] == 100.0));
        assert!(d.windows(2).all(|w| w[0][0] <= w[1][0]));
    }

    #[test]
    fn drops_old_points() {
        let mut s = Series::new(1.0, 1_000_000);
        for i in 0..3000 {
            s.push(i as f64 / 1000.0, 0.0);
        }
        assert!(s.pts.front().unwrap()[0] >= 1.999 - 1.0);
        let mut s = Series::new(100.0, 10);
        for i in 0..100 {
            s.push(i as f64, 0.0);
        }
        assert_eq!(s.pts.len(), 10);
    }
}
