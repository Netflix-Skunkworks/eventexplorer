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
// Aggregate (cross-session) view state
// ---------------------------------------------------------------------------

pub struct PacedStats {
    pub(crate) p50: f64,
    pub(crate) p95: f64,
    pub(crate) p99: f64,
    pub(crate) mean: f64,
}

/// Which view the per-session-average panels (TCP Average RTT / TCP
/// Average Goodput) show.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AvgView {
    /// CDF of per-session averages on a nines percentile x-axis.
    Distribution,
    /// Mean of per-session averages over the active sessions, binned
    /// at render time into 250 buckets over the visible window.
    OverTime,
}

/// Gap-separated step-series segments of an over-time average (see
/// [`compute_avg_timeline`]). Binned at render time by
/// [`avg_timeline_bins`].
pub(crate) type AvgSegments = Vec<Vec<[f64; 2]>>;

pub struct AggregateViewState {
    pub(crate) srtt_paced_cdf: Vec<[f64; 2]>,
    pub(crate) srtt_unpaced_cdf: Vec<[f64; 2]>,
    pub(crate) srtt_paced_stats: PacedStats,
    pub(crate) srtt_unpaced_stats: PacedStats,
    /// Mean per-session average SRTT over the active sessions, split into
    /// maximal segments so spans with no active session draw nothing.
    pub(crate) srtt_paced_avg: AvgSegments,
    pub(crate) srtt_unpaced_avg: AvgSegments,

    pub(crate) goodput_paced_cdf: Vec<[f64; 2]>,
    pub(crate) goodput_unpaced_cdf: Vec<[f64; 2]>,
    pub(crate) goodput_paced_stats: PacedStats,
    pub(crate) goodput_unpaced_stats: PacedStats,
    pub(crate) goodput_paced_avg: AvgSegments,
    pub(crate) goodput_unpaced_avg: AvgSegments,

    pub(crate) session_count_paced: Vec<[f64; 2]>,
    pub(crate) session_count_unpaced: Vec<[f64; 2]>,
    pub(crate) time_max: f64,
    pub(crate) peak_paced: f64,
    pub(crate) peak_unpaced: f64,

    pub(crate) total_sessions: usize,
    pub(crate) paced_sessions: usize,
    pub(crate) unpaced_sessions: usize,

    /// Subsystem-wide PCB-state gauges (`TCP_SAMPLE`), spanning every
    /// TCP stack. This cross-stack view and the per-stack series
    /// (requests/sec, goodput) are all panels on the Aggregate tab.
    /// `estab` is the dominant line; the other ten kernel states live in
    /// `tcp_states.others`. Empty when the capture has no `TCP_SAMPLE`
    /// data. Populated by [`Self::with_tcp_state`].
    pub(crate) tcp_states: analysis::TcpStateSeries,
    /// Peak `established` count across the capture, shown in the panel
    /// header.
    pub(crate) tcp_states_peak: f64,
    /// Establish / teardown rates from the same `TCP_SAMPLE` ticks.
    pub(crate) tcp_conn_rate: analysis::TcpConnRateSeries,

    /// Optional unified CPU-usage plot rendered above the aggregate
    /// panels (Aggregate / Per-CPU / Per-Process / Kernel vs User,
    /// switched by its own gear menu). Populated by `with_cpu_strips`
    /// when the loaded capture has cpu-provider data.
    pub(crate) cpu_panel: Option<CpuUsagePanel>,
}

pub fn make_cdf(sorted_values: &[f64]) -> Vec<[f64; 2]> {
    let n = sorted_values.len();
    if n == 0 {
        return Vec::new();
    }
    let mut result = Vec::with_capacity(n + 1);
    result.push([0.0, sorted_values[0]]);
    for (i, &v) in sorted_values.iter().enumerate() {
        let pct = (i as f64 + 1.0) / n as f64 * 100.0;
        result.push([pct, v]);
    }
    result
}

pub fn percentile_value(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = p / 100.0 * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = (rank.ceil() as usize).min(sorted.len() - 1);
    let frac = rank - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

pub fn compute_session_timeline(
    summaries: &[&SessionSummary],
    t0_us: u64,
) -> (Vec<[f64; 2]>, f64, f64) {
    let valid: Vec<&&SessionSummary> = summaries
        .iter()
        .filter(|s| s.start_ts_us > 0 && s.end_ts_us >= s.start_ts_us)
        .collect();
    if valid.is_empty() {
        return (Vec::new(), 0.0, 0.0);
    }

    let rel = |t: u64| -> f64 { (t.saturating_sub(t0_us)) as f64 / 1_000_000.0 };

    let mut events: Vec<(f64, i32)> = Vec::with_capacity(valid.len() * 2);
    for s in &valid {
        events.push((rel(s.start_ts_us), 1));
        events.push((rel(s.end_ts_us), -1));
    }
    events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(b.1.cmp(&a.1)));

    let mut count = 0i32;
    let mut result = vec![[0.0, 0.0]];
    let mut peak = 0.0_f64;
    for (t, delta) in &events {
        count += delta;
        let c = count.max(0) as f64;
        result.push([*t, c]);
        peak = peak.max(c);
    }
    let time_max = events.last().map(|(t, _)| *t).unwrap_or(0.0);
    (result, time_max, peak)
}

/// Mean of a per-session value (`f`) over the sessions active at each
/// moment, as a step series: the value only moves at session start/end
/// boundaries. Sessions with a non-positive value (no samples) are
/// skipped, matching the CDFs. Returns one series per maximal span with
/// at least one active session, so a gap with no active sessions draws
/// nothing rather than a connecting line.
pub fn compute_avg_timeline(
    summaries: &[&SessionSummary],
    t0_us: u64,
    f: fn(&SessionSummary) -> f64,
) -> AvgSegments {
    let rel = |t: u64| -> f64 { (t.saturating_sub(t0_us)) as f64 / 1_000_000.0 };

    let mut events: Vec<(f64, f64, i32)> = Vec::with_capacity(summaries.len() * 2);
    for s in summaries {
        let v = f(s);
        if v <= 0.0 || s.start_ts_us == 0 || s.end_ts_us < s.start_ts_us {
            continue;
        }
        events.push((rel(s.start_ts_us), v, 1));
        events.push((rel(s.end_ts_us), -v, -1));
    }
    events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    let mut segments: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut sum = 0.0;
    let mut count = 0i64;
    let mut i = 0;
    while i < events.len() {
        let t = events[i].0;
        let was_open = count > 0;
        let held = if was_open { sum / count as f64 } else { 0.0 };
        while i < events.len() && events[i].0 == t {
            sum += events[i].1;
            count += events[i].2 as i64;
            i += 1;
        }
        if count > 0 {
            if !was_open {
                segments.push(Vec::new());
            }
            segments.last_mut().unwrap().push([t, sum / count as f64]);
        } else if was_open {
            // The mean held until this boundary; close the segment there.
            segments.last_mut().unwrap().push([t, held]);
        }
    }
    segments
}

/// Time-weighted mean of [`compute_avg_timeline`] steps, `nbins` bins
/// over `[vx0, vx1]`. Empty bins are 0. Each step is painted by bin
/// index; walking a float cursor stalls when `(t - vx0) / w` lands
/// just under an integer and drops the rest of a long hold.
pub fn avg_timeline_bins(segs: &AvgSegments, vx0: f64, vx1: f64, nbins: usize) -> Vec<[f64; 2]> {
    if nbins == 0 || vx1 <= vx0 {
        return Vec::new();
    }
    let w = (vx1 - vx0) / nbins as f64;
    let mut sums = vec![0.0; nbins];
    let mut durs = vec![0.0; nbins];
    for seg in segs {
        for pair in seg.windows(2) {
            let (t0, t1, y) = (pair[0][0], pair[1][0], pair[0][1]);
            if t1 <= t0 {
                continue;
            }
            let lo = t0.max(vx0);
            let hi = t1.min(vx1);
            if hi <= lo {
                continue;
            }
            let mut i = ((lo - vx0) / w).floor() as usize;
            if i >= nbins {
                continue;
            }
            let mut end = ((hi - vx0) / w).ceil() as usize;
            if end > nbins {
                end = nbins;
            }
            while i < end {
                let bin_lo = vx0 + i as f64 * w;
                let bin_hi = bin_lo + w;
                let overlap = hi.min(bin_hi) - lo.max(bin_lo);
                if overlap > 0.0 {
                    sums[i] += y * overlap;
                    durs[i] += overlap;
                }
                i += 1;
            }
        }
    }
    (0..nbins)
        .map(|i| {
            [
                vx0 + (i as f64 + 0.5) * w,
                if durs[i] > 0.0 {
                    sums[i] / durs[i]
                } else {
                    0.0
                },
            ]
        })
        .collect()
}

/// Consecutive bins that have an active session. A lone bin is drawn as
/// a short hold across its width.
pub fn avg_timeline_polylines(bins: &[[f64; 2]], bin_w: f64) -> Vec<Vec<[f64; 2]>> {
    let mut out: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut cur: Vec<[f64; 2]> = Vec::new();
    for &p in bins {
        if p[1] > 0.0 {
            cur.push(p);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    let h = bin_w * 0.5;
    for seg in &mut out {
        if seg.len() == 1 {
            let [x, y] = seg[0];
            *seg = vec![[x - h, y], [x + h, y]];
        }
    }
    out
}

pub fn is_significant_session(s: &SessionSummary) -> bool {
    s.event_count >= 250 && s.duration_sec >= 1.0
}

pub fn is_paced(s: &SessionSummary) -> bool {
    s.avg_pacing_rate_mbps != 0.0
}

pub fn build_cdf_nines(sorted: &[f64]) -> Vec<[f64; 2]> {
    if sorted.is_empty() {
        return Vec::new();
    }
    let p99 = percentile_value(sorted, 99.0);
    let mut cdf: Vec<[f64; 2]> = make_cdf(sorted)
        .iter()
        .filter(|p| p[0] <= 99.0)
        .map(|&[pct, val]| [pct_to_nines(pct), val])
        .collect();
    cdf.push([pct_to_nines(99.0), p99]);
    cdf
}

impl AggregateViewState {
    /// Attach context strips backed by `profile`. Mirrors the
    /// per-session `PlotViewState::with_cpu_strips` so both
    /// surfaces render the same correlation context above their
    /// respective panels.
    pub(crate) fn with_cpu_strips(mut self, profile: &Arc<CpuProfile>, t0_us: u64) -> Self {
        // The Aggregate tab shows the CPU panel a little taller than the
        // compact flame-tab strip, and drops the "Time (seconds)" caption
        // to match the shared no-"0" tick style used by the other
        // Aggregate panels.
        const AGG_CPU_STRIP_H: f32 = 150.0;
        self.cpu_panel = CpuUsagePanel::build(
            profile,
            CpuUsagePanelStyle {
                plot_height: AGG_CPU_STRIP_H,
                x_axis_title: None,
                y_axis_label_size: 12.0,
            },
        );
        if let Some(panel) = self.cpu_panel.as_ref() {
            let bins = panel.cpu_bins();
            let last_us = bins
                .t0_us
                .saturating_add(bins.bin_us.saturating_mul(bins.bin_count as u64));
            self.time_max = self
                .time_max
                .max(last_us.saturating_sub(t0_us) as f64 / 1_000_000.0);
        }
        self
    }

    /// Attach the subsystem-wide PCB-state gauges built from `prof`.
    /// These `TCP_SAMPLE` counts span every TCP stack, so they render as
    /// their own cross-stack panel on the Aggregate tab. Extends the
    /// shared `time_max` to cover the state samples so the panel stays
    /// aligned with the session-count / STARS plots.
    pub(crate) fn with_tcp_state(mut self, prof: &TcpSampleProfile, t0_us: u64) -> Self {
        let states = analysis::build_tcp_state_series(prof, t0_us);
        self.tcp_states_peak = states.estab.iter().map(|p| p[1]).fold(0.0_f64, f64::max);
        if let Some(last) = states.estab.last() {
            self.time_max = self.time_max.max(last[0]);
        }
        self.tcp_states = states;
        self.tcp_conn_rate = analysis::build_tcp_conn_rate_series(prof, t0_us);
        self
    }

    /// Extend the shared `time_max` to cover the `if`-provider interface
    /// samples too, so a capture with little/no TCP session or STARS data
    /// (e.g. an interface-throughput-focused trace) still gets a shared
    /// time axis spanning the whole file instead of collapsing to the
    /// `time_max.max(1.0)` fallback.
    pub(crate) fn with_if_profile(mut self, profile: &IfProfile, t0_us: u64) -> Self {
        if let Some(last) = profile.samples.last() {
            self.time_max = self
                .time_max
                .max(last.ts.saturating_sub(t0_us) as f64 / 1_000_000.0);
        }
        self
    }

    /// Cover `fanout_node` occupancy and lifecycle timestamps so the
    /// System-tab axis spans an ELLA-only capture.
    pub(crate) fn with_ella_profile(mut self, profile: &EllaProfile, t0_us: u64) -> Self {
        if let Some(last) = profile.counts.last() {
            self.time_max = self
                .time_max
                .max(last.ts.saturating_sub(t0_us) as f64 / 1_000_000.0);
        }
        if let Some(last) = profile.counts_total.last() {
            self.time_max = self
                .time_max
                .max(last.ts.saturating_sub(t0_us) as f64 / 1_000_000.0);
        }
        for e in &profile.events {
            self.time_max = self
                .time_max
                .max(e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0);
        }
        self
    }

    /// Cover `nginx` request lifecycle timestamps so the System-tab axis
    /// spans an nginx-only capture.
    pub(crate) fn with_nginx_profile(mut self, profile: &NginxProfile, t0_us: u64) -> Self {
        for e in &profile.events {
            self.time_max = self
                .time_max
                .max(e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0);
        }
        self
    }

    /// Cover `mem` samples so the System-tab axis spans a mem-only
    /// capture.
    pub(crate) fn with_mem_series(mut self, series: &analysis::MemSeries) -> Self {
        if let Some(t) = series.last_t() {
            self.time_max = self.time_max.max(t);
        }
        self
    }
}

pub fn compute_aggregate(
    summaries: &[SessionSummary],
    stars_agg_samples: &[StarsAggSample],
    filter: bool,
    trace_t0_us: u64,
) -> AggregateViewState {
    let filtered: Vec<&SessionSummary> = if filter {
        summaries
            .iter()
            .filter(|s| is_significant_session(s))
            .collect()
    } else {
        summaries.iter().collect()
    };
    let total = filtered.len();

    let (paced, unpaced): (Vec<&&SessionSummary>, Vec<&&SessionSummary>) =
        filtered.iter().partition(|s| is_paced(s));
    let paced_count = paced.len();
    let unpaced_count = unpaced.len();

    let extract_sorted =
        |sessions: &[&&SessionSummary], f: fn(&SessionSummary) -> f64| -> Vec<f64> {
            let mut vals: Vec<f64> = sessions.iter().map(|s| f(s)).filter(|v| *v > 0.0).collect();
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
            vals
        };

    let paced_srtts = extract_sorted(&paced, |s| s.avg_srtt_ms);
    let unpaced_srtts = extract_sorted(&unpaced, |s| s.avg_srtt_ms);

    let stats_from = |sorted: &[f64]| PacedStats {
        p50: percentile_value(sorted, 50.0),
        p95: percentile_value(sorted, 95.0),
        p99: percentile_value(sorted, 99.0),
        mean: if sorted.is_empty() {
            0.0
        } else {
            sorted.iter().sum::<f64>() / sorted.len() as f64
        },
    };

    let srtt_paced_stats = stats_from(&paced_srtts);
    let srtt_unpaced_stats = stats_from(&unpaced_srtts);

    let paced_goodputs = extract_sorted(&paced, |s| s.avg_kernel_goodput_mbps);
    let unpaced_goodputs = extract_sorted(&unpaced, |s| s.avg_kernel_goodput_mbps);

    let goodput_paced_stats = stats_from(&paced_goodputs);
    let goodput_unpaced_stats = stats_from(&unpaced_goodputs);

    let paced_refs: Vec<&SessionSummary> = paced.iter().map(|s| **s).collect();
    let unpaced_refs: Vec<&SessionSummary> = unpaced.iter().map(|s| **s).collect();

    // Trace-relative: x = 0 always means trace start.
    let t0_us = trace_t0_us;

    let (session_count_paced, time_max_p, peak_paced) =
        compute_session_timeline(&paced_refs, t0_us);
    let (session_count_unpaced, time_max_u, peak_unpaced) =
        compute_session_timeline(&unpaced_refs, t0_us);
    let mut time_max = time_max_p.max(time_max_u);

    // Extend to cover the STARS samples too, so an hpts-only capture (no
    // sessions) still gets a sensible shared time axis for its panel.
    if let Some(last) = stars_agg_samples.last() {
        time_max = time_max.max(last.ts.saturating_sub(t0_us) as f64 / 1_000_000.0);
    }

    let srtt_paced_avg = compute_avg_timeline(&paced_refs, t0_us, |s| s.avg_srtt_ms);
    let srtt_unpaced_avg = compute_avg_timeline(&unpaced_refs, t0_us, |s| s.avg_srtt_ms);
    let goodput_paced_avg = compute_avg_timeline(&paced_refs, t0_us, |s| s.avg_kernel_goodput_mbps);
    let goodput_unpaced_avg =
        compute_avg_timeline(&unpaced_refs, t0_us, |s| s.avg_kernel_goodput_mbps);

    AggregateViewState {
        srtt_paced_cdf: build_cdf_nines(&paced_srtts),
        srtt_unpaced_cdf: build_cdf_nines(&unpaced_srtts),
        srtt_paced_stats,
        srtt_unpaced_stats,
        srtt_paced_avg,
        srtt_unpaced_avg,
        goodput_paced_cdf: build_cdf_nines(&paced_goodputs),
        goodput_unpaced_cdf: build_cdf_nines(&unpaced_goodputs),
        goodput_paced_stats,
        goodput_unpaced_stats,
        goodput_paced_avg,
        goodput_unpaced_avg,
        session_count_paced,
        session_count_unpaced,
        time_max,
        peak_paced,
        peak_unpaced,
        total_sessions: total,
        paced_sessions: paced_count,
        unpaced_sessions: unpaced_count,

        tcp_states: analysis::TcpStateSeries {
            estab: Vec::new(),
            others: Vec::new(),
        },
        tcp_states_peak: 0.0,
        tcp_conn_rate: analysis::TcpConnRateSeries {
            estab: Vec::new(),
            teardown: Vec::new(),
        },

        cpu_panel: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(start_us: u64, end_us: u64, avg_srtt_ms: f64) -> SessionSummary {
        SessionSummary {
            session_id: 0,
            event_count: 0,
            duration_sec: 0.0,
            total_bytes_sent: 0,
            total_bytes_acked: 0,
            avg_goodput_mbps: 0.0,
            avg_pacing_rate_mbps: 0.0,
            avg_kernel_goodput_mbps: 0.0,
            avg_srtt_ms,
            min_rtt_ms: 0.0,
            remote: String::new(),
            log_id: String::new(),
            start_ts_us: start_us,
            end_ts_us: end_us,
            recovery_count: 0,
            owner_pid: None,
            nginx_requests: 0,
        }
    }

    #[test]
    fn avg_timeline_means_overlapping_sessions() {
        let a = summary(1_000_000, 5_000_000, 10.0);
        let b = summary(3_000_000, 7_000_000, 20.0);
        let segs = compute_avg_timeline(&[&a, &b], 0, |s| s.avg_srtt_ms);
        assert_eq!(segs.len(), 1);
        assert_eq!(
            segs[0],
            vec![[1.0, 10.0], [3.0, 15.0], [5.0, 20.0], [7.0, 20.0]]
        );
    }

    #[test]
    fn avg_timeline_splits_segments_across_gaps() {
        let a = summary(1_000_000, 2_000_000, 10.0);
        let b = summary(5_000_000, 6_000_000, 30.0);
        let segs = compute_avg_timeline(&[&a, &b], 0, |s| s.avg_srtt_ms);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0], vec![[1.0, 10.0], [2.0, 10.0]]);
        assert_eq!(segs[1], vec![[5.0, 30.0], [6.0, 30.0]]);
    }

    #[test]
    fn avg_timeline_skips_sessions_without_samples() {
        let a = summary(1_000_000, 4_000_000, 0.0);
        let b = summary(2_000_000, 3_000_000, 8.0);
        let segs = compute_avg_timeline(&[&a, &b], 0, |s| s.avg_srtt_ms);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0], vec![[2.0, 8.0], [3.0, 8.0]]);
    }

    #[test]
    fn avg_timeline_bins_time_weights_and_zeros_idle() {
        let a = summary(1_000_000, 5_000_000, 10.0);
        let b = summary(3_000_000, 7_000_000, 20.0);
        let segs = compute_avg_timeline(&[&a, &b], 0, |s| s.avg_srtt_ms);
        let bins = avg_timeline_bins(&segs, 0.0, 10.0, 10);
        assert_eq!(bins.len(), 10);
        assert_eq!(bins[0][1], 0.0);
        assert!((bins[1][1] - 10.0).abs() < 1e-9);
        assert!((bins[2][1] - 10.0).abs() < 1e-9);
        assert!((bins[3][1] - 15.0).abs() < 1e-9);
        assert!((bins[4][1] - 15.0).abs() < 1e-9);
        assert!((bins[5][1] - 20.0).abs() < 1e-9);
        assert!((bins[6][1] - 20.0).abs() < 1e-9);
        assert_eq!(bins[7][1], 0.0);
        let lines = avg_timeline_polylines(&bins, 1.0);
        assert_eq!(lines.len(), 1);
        assert!((lines[0][0][1] - 10.0).abs() < 1e-9);
        assert!((lines[0].last().unwrap()[1] - 20.0).abs() < 1e-9);
    }

    #[test]
    fn avg_timeline_polylines_break_on_idle() {
        let bins = [
            [0.5, 0.0],
            [1.5, 10.0],
            [2.5, 10.0],
            [3.5, 0.0],
            [4.5, 30.0],
        ];
        let lines = avg_timeline_polylines(&bins, 1.0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], vec![[1.5, 10.0], [2.5, 10.0]]);
        assert_eq!(lines[1], vec![[4.0, 30.0], [5.0, 30.0]]);
    }

    #[test]
    fn avg_timeline_empty_when_nothing_active() {
        assert!(compute_avg_timeline(&[], 0, |s| s.avg_srtt_ms).is_empty());
        let a = summary(0, 0, 10.0);
        assert!(compute_avg_timeline(&[&a], 0, |s| s.avg_srtt_ms).is_empty());
    }

    #[test]
    fn avg_timeline_bins_cover_a_long_hold() {
        let a = summary(100_000_000, 1_550_000_000, 39.5);
        let segs = compute_avg_timeline(&[&a], 0, |s| s.avg_srtt_ms);
        let bins = avg_timeline_bins(&segs, 0.0, 1700.0, 250);
        let filled = bins.iter().filter(|p| p[1] > 0.0).count();
        assert!(filled > 200, "filled {filled}");
        assert_eq!(bins[0][1], 0.0);
        assert!((bins[filled / 2 + 10][1] - 39.5).abs() < 1e-6);
    }
}
