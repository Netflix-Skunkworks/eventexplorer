// Copyright 2026 Netflix, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[allow(unused_imports)]
use super::*;

// ---------------------------------------------------------------------------
// cpu provider: sampler rate as observed in the samples.
//
// `CPU_INFO.pmchz` is the rate when the session was created. A later
// `kern.eventlog.cpu.pmchz` write re-arms the sampler at the new rate
// without a fresh CPU_INFO, so the header can be wrong for part or all
// of a capture. Samples are delivered on the sampler's own period, so
// the most common short gap between consecutive samples on one CPU is
// that period.
// ---------------------------------------------------------------------------

/// The cpu sampler rate in effect from `start_us` until the next
/// segment (or the end of the capture).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleRateSegment {
    pub start_us: u64,
    pub hz: u32,
    /// True when `hz` came from the samples rather than `CPU_INFO`.
    pub detected: bool,
}

impl SampleRateSegment {
    /// One whole-capture segment at `hz`, or none when `hz` is unknown.
    pub fn uniform(hz: Option<u32>) -> Vec<SampleRateSegment> {
        hz.filter(|&hz| hz > 0)
            .map(|hz| SampleRateSegment {
                start_us: 0,
                hz,
                detected: false,
            })
            .into_iter()
            .collect()
    }
}

const RATE_WINDOW_US: u64 = 10_000_000;
const MAX_GAP_US: u64 = 1_100_000;
const MIN_PERIOD_GAPS: u32 = 20;
const MIN_PERIOD_SHARE: f64 = 0.005;
const NOMINAL_SNAP: f64 = 0.05;
const SLOWER_WINDOWS: usize = 3;

/// Log-spaced gap bucket, 2% wide.
fn gap_bucket(gap_us: u64) -> i32 {
    ((gap_us as f64).ln() / 1.02f64.ln()).round() as i32
}

/// Gap histogram for one window: `bucket -> (count, sum_us)`.
type GapHist = FxHashMap<i32, (u32, u64)>;

/// The sampler period in us for one window: the mean gap of the
/// cluster around the shortest bucket that holds enough of the
/// window's `n` gaps to be the period rather than noise. The cluster
/// absorbs tick jitter (a 100 Hz sampler on a 1000 Hz clock lands at
/// 9, 10 and 11 ms). `None` for an idle window.
fn window_period(hist: &GapHist, n: u32) -> Option<f64> {
    let min_count = MIN_PERIOD_GAPS.max((n as f64 * MIN_PERIOD_SHARE).ceil() as u32);
    let first = hist
        .iter()
        .filter(|(_, &(c, _))| c >= min_count)
        .map(|(&b, _)| b)
        .min()?;
    let peak = (first..=first + 9)
        .filter_map(|b| hist.get(&b).map(|&(c, _)| (b, c)))
        .max_by_key(|&(_, c)| c)
        .map_or(first, |(b, _)| b);
    let (c, sum) = (peak - 5..=peak + 5)
        .filter_map(|b| hist.get(&b))
        .fold((0u32, 0u64), |(c, s), &(bc, bs)| (c + bc, s + bs));
    Some(sum as f64 / c as f64)
}

/// Gaps in `hist` within one bucket of `period_us`.
fn gaps_near(hist: &GapHist, period_us: f64) -> u32 {
    let b = gap_bucket(period_us.round() as u64);
    (b - 1..=b + 1)
        .filter_map(|k| hist.get(&k))
        .map(|&(c, _)| c)
        .sum()
}

/// Sampler-rate timeline for `samples` (sorted by `ts`). Each 10 s
/// window's period is the shortest well-populated same-CPU sample gap.
/// A longer period replaces the current one only after three windows
/// with none of the current period's gaps, because a window can lack
/// back-to-back samples without the rate changing. A rate within 5% of
/// `nominal_hz` is reported as `nominal_hz`. Falls back to
/// [`SampleRateSegment::uniform`] when nothing can be measured.
pub fn detect_sample_rates(
    samples: &[CpuStackSample],
    nominal_hz: Option<u32>,
) -> Vec<SampleRateSegment> {
    let nominal = nominal_hz.filter(|&hz| hz > 0);
    let Some(first) = samples.first() else {
        return SampleRateSegment::uniform(nominal);
    };
    let t0 = first.ts;
    let max_cpu = samples.iter().map(|s| s.cpu).max().unwrap_or(0) as usize;
    let mut last = vec![u64::MAX; max_cpu + 1];
    let mut windows: Vec<GapHist> = vec![GapHist::default()];
    let mut counts: Vec<u32> = vec![0];
    for s in samples {
        let w = (s.ts.saturating_sub(t0) / RATE_WINDOW_US) as usize;
        if w >= windows.len() {
            windows.resize_with(w + 1, GapHist::default);
            counts.resize(w + 1, 0);
        }
        let prev = std::mem::replace(&mut last[s.cpu as usize], s.ts);
        if prev == u64::MAX || s.ts <= prev || s.ts - prev > MAX_GAP_US {
            continue;
        }
        let gap = s.ts - prev;
        let e = windows[w].entry(gap_bucket(gap)).or_insert((0, 0));
        e.0 += 1;
        e.1 += gap;
        counts[w] += 1;
    }

    let periods: Vec<Option<f64>> = windows
        .iter()
        .zip(&counts)
        .map(|(h, &n)| window_period(h, n))
        .collect();
    let Some(mut period) = periods.iter().flatten().copied().reduce(f64::min) else {
        return SampleRateSegment::uniform(nominal);
    };
    let to_hz = |period: f64| {
        let hz = 1e6 / period;
        match nominal {
            Some(nom) if ((hz - nom as f64).abs() / nom as f64) <= NOMINAL_SNAP => nom,
            _ => hz.round().max(1.0) as u32,
        }
    };
    let mut window_hz = Vec::with_capacity(windows.len());
    let mut slower = 0usize;
    for (i, (hist, p)) in windows.iter().zip(&periods).enumerate() {
        match *p {
            Some(p) if p < period / 1.1 => {
                period = p;
                slower = 0;
            }
            Some(p) if p > period * 1.1 && gaps_near(hist, period) == 0 => {
                slower += 1;
                if slower == SLOWER_WINDOWS {
                    period = p;
                    let from = i + 1 - SLOWER_WINDOWS;
                    let hz = to_hz(period);
                    window_hz[from..].fill(hz);
                    slower = 0;
                }
            }
            _ => slower = 0,
        }
        window_hz.push(to_hz(period));
    }
    let mut out: Vec<SampleRateSegment> = Vec::new();
    for (i, &hz) in window_hz.iter().enumerate() {
        if out.last().is_none_or(|seg| seg.hz != hz) {
            out.push(SampleRateSegment {
                start_us: if i == 0 {
                    0
                } else {
                    t0 + i as u64 * RATE_WINDOW_US
                },
                hz,
                detected: Some(hz) != nominal,
            });
        }
    }
    out
}

/// Expected samples from one fully busy CPU over `[lo_us, hi_us)`.
pub fn expected_samples(rates: &[SampleRateSegment], lo_us: u64, hi_us: u64) -> f64 {
    let mut total = 0.0;
    for (i, seg) in rates.iter().enumerate() {
        let start = if i == 0 { 0 } else { seg.start_us };
        let end = rates.get(i + 1).map_or(u64::MAX, |n| n.start_us);
        let a = lo_us.max(start);
        let b = hi_us.min(end);
        if b > a {
            total += (b - a) as f64 * seg.hz as f64 / 1e6;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy_cpu(cpu: u16, start_us: u64, end_us: u64, period_us: u64) -> Vec<CpuStackSample> {
        (start_us..end_us)
            .step_by(period_us as usize)
            .map(|ts| CpuStackSample {
                ts,
                cpu,
                tid: 0,
                pid: 0,
                sample_type: 0,
                flags: 0,
                freq_mhz: None,
                frames: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn detects_rate_change_against_stale_header() {
        let mut samples = busy_cpu(0, 0, 30_000_000, 100_000);
        samples.extend(busy_cpu(0, 30_000_000, 60_000_000, 50_000));
        samples.extend(busy_cpu(1, 0, 60_000_000, 400_000));
        samples.sort_by_key(|s| s.ts);
        let rates = detect_sample_rates(&samples, Some(10));
        assert_eq!(rates.len(), 2);
        assert_eq!((rates[0].hz, rates[0].detected), (10, false));
        assert_eq!(
            (rates[1].start_us, rates[1].hz, rates[1].detected),
            (30_000_000, 20, true)
        );
        let busy = expected_samples(&rates, 25_000_000, 35_000_000);
        assert!((busy - 150.0).abs() < 1e-9);
    }

    #[test]
    fn idle_capture_keeps_header_rate() {
        let samples = busy_cpu(0, 0, 5_000_000, 700_000);
        assert_eq!(
            detect_sample_rates(&samples, Some(10)),
            SampleRateSegment::uniform(Some(10))
        );
        assert!(detect_sample_rates(&[], None).is_empty());
    }
}
