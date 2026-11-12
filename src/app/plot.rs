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

pub fn plot_ids(session_id: u64) -> [String; 5] {
    [
        format!("rtt_plot_{session_id}"),
        format!("rate_plot_{session_id}"),
        format!("cwnd_plot_{session_id}"),
        format!("sizes_plot_{session_id}"),
        format!("size_cdf_plot_{session_id}"),
    ]
}

// ---------------------------------------------------------------------------
// Display downsampling
// ---------------------------------------------------------------------------

pub const MAX_DISPLAY_POINTS: usize = 4000;
/// Default equal-time bucket count. The tab-bar gear overrides it.
pub const STACK_BINS: usize = 250;
pub const PLOT_BINS_MIN: usize = 20;
pub const PLOT_BINS_MAX: usize = 2000;

/// Fraction above the drawn peak so a series isn't flush with the frame.
const Y_AXIS_HEADROOM: f64 = 1.08;

/// Y-axis upper bound: 8% above `peak`, or 1 when the window is empty.
/// Keeps 0 on the bottom edge of the frame.
pub fn y_axis_top(peak: f64) -> f64 {
    if peak > 0.0 {
        peak * Y_AXIS_HEADROOM
    } else {
        1.0
    }
}

/// Y-axis range with 8% padding on both sides. Used when 0 is not pinned
/// (duration-ratio plots). A flat series pads by 8% of its magnitude.
pub fn y_axis_span(lo: f64, hi: f64) -> (f64, f64) {
    if !lo.is_finite() || !hi.is_finite() {
        return (0.0, 1.0);
    }
    let (lo, hi) = if hi < lo { (hi, lo) } else { (lo, hi) };
    if hi <= lo {
        let mag = lo.abs().max(1.0);
        let pad = mag * (Y_AXIS_HEADROOM - 1.0);
        return (lo - pad, lo + pad);
    }
    let pad = (hi - lo) * (Y_AXIS_HEADROOM - 1.0);
    (lo - pad, hi + pad)
}

/// Highest step-post y in `[view[0], view[1]]`, including the value held
/// at the left edge of the window.
pub fn step_peak_in_view(data: &[[f64; 2]], view: [f64; 2]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut peak = lookup_step_value(data, view[0]).unwrap_or(0.0);
    for p in data {
        if p[0] > view[0] && p[0] <= view[1] {
            peak = peak.max(p[1]);
        }
    }
    peak
}

/// X-window to downsample or bin against. Pass the shared time-axis
/// seed (`TimeAxisController::seed`), not `PlotUi::plot_bounds()`.
///
/// `plot_bounds()` is last frame's transform. On the first paint —
/// and after a file load, before PlotMemory has this capture's
/// `include_x` — egui_plot 0.27 reports an origin-centered dummy of
/// ±1, so a minutes-long series would only draw two seconds until a
/// hover forced another frame.
pub fn seed_view_x(seed_lo: f64, seed_hi: f64, data_lo: f64, data_hi: f64) -> [f64; 2] {
    let lo = seed_lo.max(data_lo);
    let hi = seed_hi.min(data_hi);
    if hi > lo {
        [lo, hi]
    } else if data_hi > data_lo {
        [data_lo, data_hi]
    } else {
        [seed_lo, seed_hi.max(seed_lo + 1.0)]
    }
}

/// Downsample raw data to `max_points` using min-max bucketing, filtered to
/// the visible x-range.  Data must be sorted by x.
pub fn downsample_raw(data: &[[f64; 2]], view_range: [f64; 2], max_points: usize) -> Vec<[f64; 2]> {
    if data.is_empty() {
        return Vec::new();
    }

    let margin = (view_range[1] - view_range[0]) * 0.02;
    let x_lo = view_range[0] - margin;
    let x_hi = view_range[1] + margin;

    let start = data.partition_point(|p| p[0] < x_lo).saturating_sub(1);
    let end = (data.partition_point(|p| p[0] <= x_hi) + 1).min(data.len());
    let visible = &data[start..end];

    if visible.len() <= max_points {
        return visible.to_vec();
    }

    let num_buckets = max_points / 2;
    let bucket_size = visible.len() as f64 / num_buckets as f64;
    let mut result = Vec::with_capacity(max_points + 2);

    for i in 0..num_buckets {
        let b_start = (i as f64 * bucket_size) as usize;
        let b_end = (((i + 1) as f64 * bucket_size) as usize).min(visible.len());
        if b_start >= b_end {
            continue;
        }

        let mut min_idx = b_start;
        let mut max_idx = b_start;
        for j in (b_start + 1)..b_end {
            if visible[j][1] < visible[min_idx][1] {
                min_idx = j;
            }
            if visible[j][1] > visible[max_idx][1] {
                max_idx = j;
            }
        }

        if min_idx <= max_idx {
            result.push(visible[min_idx]);
            if min_idx != max_idx {
                result.push(visible[max_idx]);
            }
        } else {
            result.push(visible[max_idx]);
            if min_idx != max_idx {
                result.push(visible[min_idx]);
            }
        }
    }

    result
}

/// Downsample by averaging within each bucket, emitting one point per
/// bucket — a clean representative line rather than a min/max envelope.
/// Suited to overview rate plots where several series share one axis and
/// legibility (comparing sustained levels) matters more than catching
/// every single-sample spike; the min/max fill of [`downsample_raw`]
/// turns overlapping series into opaque bands. Data must be sorted by x.
pub fn downsample_mean(
    data: &[[f64; 2]],
    view_range: [f64; 2],
    max_points: usize,
) -> Vec<[f64; 2]> {
    if data.is_empty() {
        return Vec::new();
    }

    let margin = (view_range[1] - view_range[0]) * 0.02;
    let x_lo = view_range[0] - margin;
    let x_hi = view_range[1] + margin;

    let start = data.partition_point(|p| p[0] < x_lo).saturating_sub(1);
    let end = (data.partition_point(|p| p[0] <= x_hi) + 1).min(data.len());
    let visible = &data[start..end];

    if visible.len() <= max_points {
        return visible.to_vec();
    }

    let num_buckets = max_points.max(1);
    let bucket_size = visible.len() as f64 / num_buckets as f64;
    let mut result = Vec::with_capacity(num_buckets);

    for i in 0..num_buckets {
        let b_start = (i as f64 * bucket_size) as usize;
        let b_end = (((i + 1) as f64 * bucket_size) as usize).min(visible.len());
        if b_start >= b_end {
            continue;
        }
        let n = (b_end - b_start) as f64;
        let (mut sx, mut sy) = (0.0, 0.0);
        for p in &visible[b_start..b_end] {
            sx += p[0];
            sy += p[1];
        }
        result.push([sx / n, sy / n]);
    }

    result
}

/// For step-post data sorted by x, find the y value at the given x.
/// Returns None if x is before the first data point or the series is empty.
pub fn lookup_step_value(raw: &[[f64; 2]], x: f64) -> Option<f64> {
    if raw.is_empty() {
        return None;
    }
    let idx = raw.partition_point(|p| p[0] <= x);
    if idx == 0 {
        None
    } else {
        Some(raw[idx - 1][1])
    }
}

pub fn lookup_nearest_value(raw: &[[f64; 2]], x: f64, max_dist: f64) -> Option<f64> {
    if raw.is_empty() {
        return None;
    }
    let idx = raw.partition_point(|p| p[0] <= x);
    let mut best: Option<(f64, f64)> = None;
    for &i in &[idx.saturating_sub(1), idx.min(raw.len() - 1)] {
        let dist = (raw[i][0] - x).abs();
        if dist <= max_dist && best.is_none_or(|(d, _)| dist < d) {
            best = Some((dist, raw[i][1]));
        }
    }
    best.map(|(_, v)| v)
}

/// Linear interpolation on sorted [x, y] data.
pub fn interpolate_line(data: &[[f64; 2]], x: f64) -> Option<f64> {
    if data.is_empty() {
        return None;
    }
    if x <= data[0][0] {
        return Some(data[0][1]);
    }
    let last = data.len() - 1;
    if x >= data[last][0] {
        return Some(data[last][1]);
    }
    let idx = data.partition_point(|p| p[0] <= x);
    if idx == 0 {
        return Some(data[0][1]);
    }
    let [x0, y0] = data[idx - 1];
    let [x1, y1] = data[idx];
    if (x1 - x0).abs() < 1e-12 {
        return Some(y1);
    }
    let t = (x - x0) / (x1 - x0);
    Some(y0 + t * (y1 - y0))
}

/// Extend the last sample to `x_hi` so a staircase stays flat through
/// the rest of the view window.
pub fn hold_last_to_edge(points: &mut Vec<[f64; 2]>, x_hi: f64) {
    if let Some(last) = points.last().copied() {
        if last[0] < x_hi {
            points.push([x_hi, last[1]]);
        }
    }
}

/// Downsample raw data, then apply step-post to get a correct staircase, and
/// extend the last value to the view edge so the line stays flat.
pub fn prepare_series(raw: &[[f64; 2]], view_range: [f64; 2], max_points: usize) -> Vec<[f64; 2]> {
    let ds = downsample_raw(raw, view_range, max_points);
    if ds.is_empty() {
        return Vec::new();
    }
    let mut stepped = analysis::to_step_post(&ds);
    hold_last_to_edge(&mut stepped, view_range[1]);
    stepped
}

/// Time-weight a step series into `nbins` equal buckets over `view_range`,
/// then draw it as a staircase held out to the right edge.
pub fn bucket_series(raw: &[[f64; 2]], view_range: [f64; 2], nbins: usize) -> Vec<[f64; 2]> {
    if raw.is_empty() || nbins == 0 || view_range[1] <= view_range[0] {
        return Vec::new();
    }
    let mut closed = Vec::with_capacity(raw.len() + 1);
    closed.extend_from_slice(raw);
    let last = raw[raw.len() - 1];
    if last[0] < view_range[1] {
        closed.push([view_range[1], last[1]]);
    }
    let mut bins = analysis::step_series_bins(&closed, view_range[0], view_range[1], nbins);
    let width = (view_range[1] - view_range[0]) / nbins as f64;
    let first = raw[0][0];
    bins.retain(|p| p[0] + width * 0.5 > first);
    if bins.is_empty() {
        return Vec::new();
    }
    let mut stepped = analysis::to_step_post(&bins);
    hold_last_to_edge(&mut stepped, view_range[1]);
    stepped
}

// ---------------------------------------------------------------------------
// Grid step helpers
// ---------------------------------------------------------------------------

/// Pick a "nice" step size that produces ~5 labeled ticks for the given range.
pub fn nice_step(range: f64) -> f64 {
    if range <= 0.0 {
        return 1.0;
    }
    let rough = range / 5.0;
    let mag = 10.0f64.powf(rough.log10().floor());
    let residual = rough / mag;
    let nice = if residual <= 1.5 {
        1.0
    } else if residual <= 3.5 {
        2.0
    } else if residual <= 7.5 {
        5.0
    } else {
        10.0
    };
    nice * mag
}

// ---------------------------------------------------------------------------
// "Nines" percentile scale: log-spaced X axis emphasising the tail
// ---------------------------------------------------------------------------

pub const NINES_TICKS: &[(f64, &str)] = &[
    (0.0, "0"),
    (50.0, "50"),
    (75.0, "75"),
    (90.0, "90"),
    (95.0, "95"),
    (99.0, "99"),
];

pub fn pct_to_nines(p: f64) -> f64 {
    if p <= 0.0 {
        return 0.0;
    }
    let p_clamped = p.min(99.0);
    -((1.0 - p_clamped / 100.0).log10())
}

pub fn nines_to_pct(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    (1.0 - 10.0_f64.powf(-x)) * 100.0
}

/// Round `value` UP to the next "nice" multiple in the {1, 2, 5} ×
/// 10ⁿ family — the same kind of human-friendly tick endpoints
/// `nice_y_grid_spacer` produces internally. Used by the CPU usage
/// strip to snap `y_max` to a value the auto tick spacer is
/// guaranteed to land a labeled tick on.
///
/// Examples:
///   `nice_ceiling(0.7)`  → 1.0
///   `nice_ceiling(3.4)`  → 5.0
///   `nice_ceiling(7.1)`  → 10.0
///   `nice_ceiling(12.0)` → 20.0
///   `nice_ceiling(46.0)` → 50.0
///   `nice_ceiling(80.0)` → 100.0
pub fn nice_ceiling(value: f64) -> f64 {
    if value.is_nan() || value <= 0.0 {
        return 1.0;
    }
    let magnitude = 10.0_f64.powf(value.log10().floor());
    let normalized = value / magnitude;
    let nice_norm = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice_norm * magnitude
}

/// Y-axis top for a 0–100% series: nice ceiling of `peak`, never above 100.
pub fn percent_y_axis_top(peak: f64) -> f64 {
    nice_ceiling(peak.max(1.0)).min(100.0)
}

pub fn nines_grid_spacer(input: egui_plot::GridInput) -> Vec<GridMark> {
    let (lo, hi) = input.bounds;
    NINES_TICKS
        .iter()
        .map(|&(pct, _)| pct_to_nines(pct))
        .filter(|&x| x >= lo - 0.01 && x <= hi + 0.01)
        .map(|x| GridMark {
            value: x,
            step_size: 1.0,
        })
        .collect()
}

pub fn nice_y_grid_spacer(input: egui_plot::GridInput) -> Vec<GridMark> {
    let (min, max) = input.bounds;
    let range = max - min;
    if range <= 0.0 {
        return vec![];
    }
    let rough_step = range / 4.0;
    let magnitude = 10.0_f64.powf(rough_step.log10().floor());
    let normalized = rough_step / magnitude;
    let nice_step = if normalized <= 1.5 {
        magnitude
    } else if normalized <= 3.5 {
        2.0 * magnitude
    } else if normalized <= 7.5 {
        5.0 * magnitude
    } else {
        10.0 * magnitude
    };
    let start = (min / nice_step).ceil() as i64;
    let end = (max / nice_step).floor() as i64;
    (start..=end)
        .map(|i| GridMark {
            value: i as f64 * nice_step,
            step_size: nice_step,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Sort
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    StartTime,
    SessionId,
    Remote,
    LogId,
    Duration,
    BytesSent,
    Goodput,
    PacingRate,
    Rtt,
    MinRtt,
    EventCount,
    RecoveryCount,
    NginxRequests,
}

pub fn sort_summaries(summaries: &mut [SessionSummary], col: SortColumn, ascending: bool) {
    summaries.sort_by(|a, b| {
        let ord = match col {
            SortColumn::StartTime => a.start_ts_us.cmp(&b.start_ts_us),
            SortColumn::SessionId => a.session_id.cmp(&b.session_id),
            SortColumn::Remote => a.remote.cmp(&b.remote),
            SortColumn::LogId => a.log_id.cmp(&b.log_id),
            SortColumn::Duration => a
                .duration_sec
                .partial_cmp(&b.duration_sec)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortColumn::BytesSent => a.total_bytes_sent.cmp(&b.total_bytes_sent),
            SortColumn::Goodput => a
                .avg_kernel_goodput_mbps
                .partial_cmp(&b.avg_kernel_goodput_mbps)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortColumn::PacingRate => a
                .avg_pacing_rate_mbps
                .partial_cmp(&b.avg_pacing_rate_mbps)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortColumn::Rtt => a
                .avg_srtt_ms
                .partial_cmp(&b.avg_srtt_ms)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortColumn::MinRtt => a
                .min_rtt_ms
                .partial_cmp(&b.min_rtt_ms)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortColumn::EventCount => a.event_count.cmp(&b.event_count),
            SortColumn::RecoveryCount => a.recovery_count.cmp(&b.recovery_count),
            SortColumn::NginxRequests => a.nginx_requests.cmp(&b.nginx_requests),
        };
        if ascending {
            ord
        } else {
            ord.reverse()
        }
    });
}

// ---------------------------------------------------------------------------
// PlotView state (the data needed to render the three-panel plot)
// ---------------------------------------------------------------------------

/// Right-click Y axis → edit maximum; anchor is where the window opens.
pub struct YAxisMaxEditor {
    pub(crate) plot_idx: usize,
    pub(crate) draft: String,
    pub(crate) anchor: egui::Pos2,
}

pub struct PlotViewState {
    pub(crate) summary: SessionSummary,
    pub(crate) recovery_spans: Vec<[f64; 2]>,
    /// Default-on overlay: pink shading for every
    /// `EVT_ENTER_RECOVERY` → `EVT_EXIT_RECOVERY` pair. Toggle lives in
    /// the gear menu so users can suppress noisy recovery patterns
    /// (e.g. wifi captures) when reading other overlays.
    pub(crate) show_recovery_spans: bool,
    pub(crate) goodput_spans: Vec<[f64; 2]>,
    pub(crate) show_goodput_spans: bool,
    pub(crate) pacing_cap_spans: Vec<PacingCapSpan>,
    pub(crate) show_pacing_cap: bool,
    /// Time-weighted breakdown of limiter classes — rendered as a one-liner
    /// in the per-session header.
    pub(crate) limiter_summary: LimiterSummary,
    /// RLE'd limiter spans covering the full active range of the session.
    pub(crate) limiter_spans: Vec<LimiterSpan>,
    pub(crate) show_limiter: bool,
    /// Discrete-event marker times (time_s, reason text), drawn as thin
    /// semi-transparent vertical lines across every time-linked panel
    /// and surfaced in the shared cursor tooltip. Each event type has
    /// its own independent toggle — they intentionally are not grouped,
    /// because they have very different base rates (RTO and Window
    /// Collapse are rare, DSACK can fire hundreds of times per session
    /// on loss-heavy paths) and users may want to suppress the chatty
    /// one while keeping the rare ones visible.
    pub(crate) rto_markers: Vec<(f64, String)>,
    pub(crate) show_rto: bool,
    pub(crate) window_collapse_markers: Vec<(f64, String)>,
    pub(crate) show_window_collapse: bool,
    pub(crate) dsack_markers: Vec<(f64, String)>,
    pub(crate) show_dsack: bool,
    /// `HTTP_REQ_TRK` slot lifetimes, rendered as a small Gantt strip
    /// above the time-linked panels. Empty on captures without the
    /// `HTTP_REQ` keyword (the panel is hidden in that case). When
    /// nginx requests exist on the same session id, the kernel spans
    /// are cleared — nginx is the authoritative request timeline.
    pub(crate) http_req_spans: Vec<analysis::HttpReqSpan>,
    /// nginx request lifetimes on this connection (same session id),
    /// drawn as lanes; legend/color is HTTP status class (`2xx`, `4xx`, …).
    pub(crate) nginx_req_spans: Vec<analysis::NginxReqSpan>,
    pub(crate) http_req_allocfails: Vec<f64>,
    pub(crate) show_http_req: bool,
    /// Whether to render the per-plot legends. Default on; primarily
    /// useful for presentations / screenshots where panel real estate
    /// matters or when overlapping legend boxes obscure data.
    pub(crate) show_legends: bool,
    /// Stable per-connection parameters (`CONN_PARAMS_V2`), surfaced
    /// in the session header as a small subtitle line. `None` for
    /// pre-`_V2` captures — and an empty `raw_peer_rwnd` is the
    /// signal that the Peer RWND line / tooltip should stay hidden.
    pub(crate) conn_params: Option<analysis::ConnParams>,

    // Raw (non-staircase) data — step-post is applied after downsampling
    pub(crate) raw_rtt: Vec<[f64; 2]>,
    pub(crate) raw_srtt: Vec<[f64; 2]>,
    pub(crate) raw_path_rtt: Vec<[f64; 2]>,
    pub(crate) raw_min_rtt: Vec<[f64; 2]>,
    pub(crate) raw_gp_filter_min: Vec<[f64; 2]>,
    pub(crate) raw_gp_srtt: Vec<[f64; 2]>,
    pub(crate) raw_goodput: Vec<[f64; 2]>,
    pub(crate) raw_pacing: Vec<[f64; 2]>,
    pub(crate) raw_ack_rate: Vec<[f64; 2]>,
    pub(crate) raw_cwnd: Vec<[f64; 2]>,
    pub(crate) raw_ssthresh: Vec<[f64; 2]>,
    pub(crate) raw_inflight: Vec<[f64; 2]>,
    pub(crate) raw_peer_rwnd: Vec<[f64; 2]>,
    /// Estimated bytes resident in the send socket buffer
    /// (sent-unacked + unsent), KB. Built by `extract_overview_data`
    /// from `EVT_USER_SEND.sndbuf_used` / `EVT_SENDFILE.length` /
    /// `EVT_ACK.acked` — see the doc on `OverviewPlotData::sndbuf_kb`.
    /// Empty for sessions that emit none of those events; the
    /// renderer skips the series in that case.
    pub(crate) raw_sndbuf: Vec<[f64; 2]>,
    pub(crate) raw_sacked: Vec<[f64; 2]>,
    pub(crate) raw_send_size: Vec<[f64; 2]>,
    pub(crate) raw_retx_size: Vec<[f64; 2]>,
    pub(crate) raw_ack_size: Vec<[f64; 2]>,

    pub(crate) x_min: f64,
    pub(crate) x_max: f64,

    /// Shared time-axis state (zoom window, undo history, and the transient
    /// box-zoom / Δt-measurement drag) for this session's stacked panels.
    /// The same controller drives the System tab.
    pub(crate) time_axis: TimeAxisController,

    pub(crate) plot_layout: PlotLayout,
    pub(crate) plot_draft: PlotLayout,

    pub(crate) needs_legend_init: bool,
    pub(crate) needs_reset: bool,
    pub(crate) custom_name: Option<String>,

    /// Per-plot fixed Y maximum (`None` = automatic).
    pub(crate) y_max_override: [Option<f64>; 5],
    pub(crate) y_max_editor: Option<YAxisMaxEditor>,

    /// Optional unified CPU-usage plot rendered above the
    /// time-linked panels. Built by [`Self::with_cpu_strips`] when
    /// the loaded capture has cpu-provider data; `None` for
    /// tcp-only captures (the plot is then hidden).
    pub(crate) cpu_panel: Option<CpuUsagePanel>,
    pub(crate) open_gear_menu: bool,
}

impl PlotViewState {
    pub(crate) fn from_events(
        events: &[Event],
        summary: SessionSummary,
        aux: Option<&analysis::SessionAuxData>,
        trace_t0_us: u64,
    ) -> Self {
        let plot_data = analysis::extract_overview_data(events, aux, Some(trace_t0_us));

        let all_raw: [&Vec<[f64; 2]>; 18] = [
            &plot_data.rtt_samples,
            &plot_data.srtt,
            &plot_data.path_rtt,
            &plot_data.min_rtt,
            &plot_data.gp_filter_min,
            &plot_data.gp_srtt,
            &plot_data.kernel_goodput_mbps,
            &plot_data.pacing_rate_mbps,
            &plot_data.ack_rate_mbps,
            &plot_data.cwnd_kb,
            &plot_data.ssthresh_kb,
            &plot_data.inflight_kb,
            &plot_data.sndbuf_kb,
            &plot_data.peer_rwnd_kb,
            &plot_data.sacked_bytes,
            &plot_data.send_size_bytes,
            &plot_data.retx_size_bytes,
            &plot_data.ack_size_bytes,
        ];
        let data_x_min = all_raw
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s[0][0])
            .fold(f64::INFINITY, f64::min);
        let data_x_max = all_raw
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.last().unwrap()[0])
            .fold(f64::NEG_INFINITY, f64::max);
        // x_min is the session’s offset from trace start (seconds).
        // For session-relative output (CLI / legacy callers passing
        // trace_t0_us = 0) this is 0; for trace-relative output it
        // is `(summary.start_ts_us - trace_t0_us) / 1e6`. Either
        // way the panels’ default zoom covers exactly the session’s
        // lifetime (`x_min .. x_max`).
        let session_offset_s =
            (summary.start_ts_us.saturating_sub(trace_t0_us)) as f64 / 1_000_000.0;
        let x_min = if data_x_min.is_finite() {
            data_x_min
        } else {
            session_offset_s
        };
        let x_max = if summary.duration_sec > 0.0 {
            session_offset_s + summary.duration_sec
        } else if data_x_max.is_finite() {
            data_x_max
        } else {
            session_offset_s
        };

        Self {
            summary,
            recovery_spans: plot_data.recovery_spans,
            show_recovery_spans: true,
            goodput_spans: plot_data.goodput_spans,
            show_goodput_spans: false,
            pacing_cap_spans: plot_data.pacing_cap_spans,
            show_pacing_cap: false,
            limiter_summary: plot_data.limiter_summary,
            limiter_spans: plot_data.limiter_spans,
            show_limiter: false,
            rto_markers: plot_data.rto_markers,
            show_rto: false,
            window_collapse_markers: plot_data.window_collapse_markers,
            show_window_collapse: false,
            dsack_markers: plot_data.dsack_markers,
            show_dsack: false,
            http_req_spans: plot_data.http_req_spans,
            nginx_req_spans: Vec::new(),
            http_req_allocfails: plot_data.http_req_allocfails,
            show_http_req: true,
            show_legends: true,
            conn_params: plot_data.conn_params,
            raw_rtt: plot_data.rtt_samples,
            raw_srtt: plot_data.srtt,
            raw_path_rtt: plot_data.path_rtt,
            raw_min_rtt: plot_data.min_rtt,
            raw_gp_filter_min: plot_data.gp_filter_min,
            raw_gp_srtt: plot_data.gp_srtt,
            raw_goodput: plot_data.kernel_goodput_mbps,
            raw_pacing: plot_data.pacing_rate_mbps,
            raw_ack_rate: plot_data.ack_rate_mbps,
            raw_cwnd: plot_data.cwnd_kb,
            raw_ssthresh: plot_data.ssthresh_kb,
            raw_inflight: plot_data.inflight_kb,
            raw_peer_rwnd: plot_data.peer_rwnd_kb,
            raw_sndbuf: plot_data.sndbuf_kb,
            raw_sacked: plot_data.sacked_bytes,
            raw_send_size: plot_data.send_size_bytes,
            raw_retx_size: plot_data.retx_size_bytes,
            raw_ack_size: plot_data.ack_size_bytes,
            x_min,
            x_max,
            time_axis: TimeAxisController::default(),
            plot_layout: default_layout(PlotCatalogKind::Session),
            plot_draft: default_layout(PlotCatalogKind::Session),
            cpu_panel: None,
            needs_legend_init: true,
            needs_reset: true,
            custom_name: None,
            y_max_override: [None; 5],
            y_max_editor: None,
            open_gear_menu: false,
        }
    }

    /// Which of the 5 stacked plots [RTT, Rate, Send State, Sizes, Size
    /// CDF] have any underlying data to draw. Captures that only log
    /// STARS_PACE/PACING_CALC (no per-packet IN/OUT/ACK/RTO) leave RTT,
    /// Send State, and Sizes empty; the caller hides those panels
    /// instead of rendering blank axes. Size CDF mirrors Sizes since
    /// it's derived from the same send/ack/retx byte series.
    pub(crate) fn plot_has_data(&self) -> [bool; 5] {
        let rtt = !self.raw_rtt.is_empty()
            || !self.raw_srtt.is_empty()
            || !self.raw_path_rtt.is_empty()
            || !self.raw_min_rtt.is_empty()
            || !self.raw_gp_filter_min.is_empty()
            || !self.raw_gp_srtt.is_empty();
        let rate = !self.raw_pacing.is_empty()
            || !self.raw_goodput.is_empty()
            || !self.raw_ack_rate.is_empty();
        let send_state = !self.raw_cwnd.is_empty()
            || !self.raw_ssthresh.is_empty()
            || !self.raw_inflight.is_empty()
            || !self.raw_sndbuf.is_empty()
            || !self.raw_peer_rwnd.is_empty();
        let sizes = !self.raw_send_size.is_empty()
            || !self.raw_retx_size.is_empty()
            || !self.raw_ack_size.is_empty()
            || !self.raw_sacked.is_empty();
        [rtt, rate, send_state, sizes, sizes]
    }

    /// Whether the HTTP request lane strip has anything to draw. Hidden
    /// (rather than rendered empty) on captures without the `HTTP_REQ`
    /// keyword enabled.
    pub(crate) fn has_http_req_data(&self) -> bool {
        !self.http_req_spans.is_empty()
            || !self.nginx_req_spans.is_empty()
            || !self.http_req_allocfails.is_empty()
    }

    /// Whether any nginx request on this session completed with a
    /// `bytes_sent` (the HTTP Response Size plot's input).
    pub(crate) fn has_http_resp_size_data(&self) -> bool {
        self.nginx_req_spans
            .iter()
            .any(|s| s.bytes_sent.is_some() && s.t_end.is_some())
    }

    /// Whether any nginx request on this session completed (the HTTP
    /// Latency plot's input).
    pub(crate) fn has_http_latency_data(&self) -> bool {
        self.nginx_req_spans.iter().any(|s| s.t_end.is_some())
    }

    /// Highest `HTTP_REQ_TRK` slot number seen, used to size the lane
    /// strip's y-axis (slots 0..=max).
    pub(crate) fn http_req_max_slot(&self) -> u8 {
        self.http_req_spans
            .iter()
            .map(|s| s.slot)
            .max()
            .unwrap_or(0)
    }

    /// First y-lane available for nginx requests: above the kernel slots,
    /// with a one-lane gap when both kinds are present.
    pub(crate) fn nginx_req_lane_base(&self) -> f64 {
        if self.http_req_spans.is_empty() {
            0.0
        } else {
            self.http_req_max_slot() as f64 + 2.0
        }
    }

    /// Top of the lane strip's y-axis, covering kernel slots and nginx
    /// lanes.
    pub(crate) fn http_req_lanes_y_hi(&self) -> f64 {
        let nginx_top = self
            .nginx_req_spans
            .iter()
            .map(|s| s.lane as f64 + self.nginx_req_lane_base())
            .fold(f64::NEG_INFINITY, f64::max);
        if nginx_top.is_finite() {
            nginx_top + 0.5
        } else {
            self.http_req_max_slot() as f64 + 0.5
        }
    }

    /// Attach nginx request lanes for this session (nginx sessions share
    /// the TCP connection's session id). No-op when the capture has no
    /// nginx requests on this connection. When nginx data exists, the
    /// kernel `HTTP_REQ_TRK` spans are dropped — nginx is the
    /// authoritative request timeline. Returns `self` so callers can
    /// chain onto [`Self::from_events`].
    pub(crate) fn with_nginx_spans(
        mut self,
        profile: &crate::elog_parser::NginxProfile,
        session_id: u64,
        trace_t0_us: u64,
    ) -> Self {
        if !profile.is_empty() {
            self.nginx_req_spans = analysis::nginx_session_spans(profile, session_id, trace_t0_us);
            if !self.nginx_req_spans.is_empty() {
                self.http_req_spans.clear();
            }
        }
        self
    }

    /// Attach context strips backed by `profile`. No-op when the
    /// profile is empty; otherwise builds a `CpuBusyStripView`
    /// and a `TopProcessesStripView` sharing the same bin layout
    /// so they stack pixel-aligned (same as the Flame tab).
    ///
    /// Returns `self` so callers can chain the call onto
    /// [`Self::from_events`]. Idempotent: calling twice replaces
    /// the prior strips, dropping any in-progress drag state.
    pub(crate) fn with_cpu_strips(mut self, profile: &Arc<CpuProfile>) -> Self {
        self.cpu_panel = CpuUsagePanel::build(
            profile,
            CpuUsagePanelStyle {
                plot_height: 110.0,
                x_axis_title: Some("Time (seconds)".to_string()),
                // Matches make_plot's/the Size-CDF axis's y-axis label
                // size so the CPU strip's plot area lines up with the
                // RTT/Rate/Send State/Sizes panels stacked below it.
                y_axis_label_size: 14.0,
            },
        );
        self
    }

    pub(crate) fn apply_view_x(&self, ctx: &egui::Context, id: &str) {
        self.time_axis.apply(ctx, id);
    }

    pub(crate) fn reset_all_plots(&self, ctx: &egui::Context, reset_y: bool) {
        let ids = plot_ids(self.summary.session_id);
        let sid = self.summary.session_id;
        let extra = [
            format!("http_req_plot_{sid}"),
            format!("http_resp_size_plot_{sid}"),
            format!("http_latency_plot_{sid}"),
            format!("s{sid}_if_tx_plot"),
            format!("s{sid}_if_rx_plot"),
            format!("s{sid}_if_single_tx_plot"),
            format!("s{sid}_if_single_rx_plot"),
        ];
        for id_str in ids.iter().chain(extra.iter()) {
            let plot_id = egui::Id::new(id_str.as_str());
            if let Some(mut mem) = PlotMemory::load(ctx, plot_id) {
                mem.auto_bounds = Vec2b::new(true, if reset_y { true } else { mem.auto_bounds.y });
                mem.store(ctx, plot_id);
            }
        }
    }

    /// Lock Y range so the top edge is `y_max` (and keep current lower bound when possible).
    pub(crate) fn apply_y_max_bounds(&self, ctx: &egui::Context, plot_id_str: &str, y_max: f64) {
        let plot_id = egui::Id::new(plot_id_str);
        if let Some(mut mem) = PlotMemory::load(ctx, plot_id) {
            let mut bounds = *mem.bounds();
            let y_lo = bounds.min()[1];
            let y_lo = if y_lo.is_finite() { y_lo } else { 0.0 };
            let y_hi = y_max.max(y_lo + 1e-9);
            bounds.set_y(&PlotBounds::from_min_max([0.0, y_lo], [0.0, y_hi]));
            mem.set_bounds(bounds);
            mem.auto_bounds = Vec2b::new(mem.auto_bounds.x, false);
            mem.store(ctx, plot_id);
        }
    }

    pub(crate) fn make_plot(
        &self,
        id: &str,
        label: &str,
        height: f32,
        show_x_axis: bool,
        y_min: Option<f64>,
        y_max: Option<f64>,
    ) -> Plot {
        let y_axis = AxisHints::new_y()
            .label(RichText::new(label).size(14.0))
            .max_digits(6);
        let (x_lo, x_hi, x_auto) = self.time_axis.seed(self.x_min, self.x_max);
        let mut plot = Plot::new(id)
            .id(egui::Id::new(id))
            .height(height)
            .y_axis_width(6)
            // No x margin: the default (unzoomed) view must land on
            // exactly [x_lo, x_hi] so its transform matches the CPU strip,
            // which forces its bounds to the same range with no margin.
            .set_margin_fraction(egui::vec2(0.0, 0.05))
            .include_x(x_lo)
            .include_x(x_hi)
            .auto_bounds(Vec2b::new(x_auto, true));
        if let Some(y) = y_min {
            plot = plot.include_y(y);
        }
        if let Some(y) = y_max {
            plot = plot.include_y(y);
        }
        plot = plot
            .y_grid_spacer(|input: egui_plot::GridInput| {
                let (lo, hi) = input.bounds;
                let range = (hi - lo).abs().max(1e-9);
                let step = nice_step(range);
                let start = (lo / step).ceil() as i64;
                let end = (hi / step).floor() as i64;
                (start..=end)
                    .map(|i| GridMark {
                        value: i as f64 * step,
                        step_size: step,
                    })
                    .collect()
            })
            .custom_y_axes(vec![y_axis])
            .show_axes([show_x_axis, true])
            .show_x(false)
            .show_y(false)
            .allow_scroll(false)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .sense(egui::Sense::hover());
        if self.show_legends {
            plot = plot.legend(Legend::default().position(Corner::RightBottom));
        }
        plot
    }
}

pub fn maybe_legend(mut plot: Plot, show: bool) -> Plot {
    if show {
        plot = plot.legend(Legend::default().position(Corner::RightBottom));
    }
    plot
}

pub(crate) fn plot_section_title(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.strong(RichText::new(title).size(14.0));
}

/// Drag handle under a plot. Returns the vertical delta to apply to
/// that plot's height only (the plot below is not resized).
/// `slot` must be unique among plots on the same tab (the plot widget
/// id is the usual choice).
pub(crate) fn draw_plot_sizer(ui: &mut egui::Ui, slot: &str) -> f32 {
    let (rect, resp) = ui
        .push_id(slot, |ui| {
            ui.allocate_exact_size(
                egui::vec2(ui.available_width(), PLOT_SIZER_H),
                egui::Sense::drag(),
            )
        })
        .inner;
    let color = if resp.hovered() || resp.dragged() {
        Color32::from_gray(120)
    } else {
        Color32::from_gray(60)
    };
    ui.painter().rect_filled(
        egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width() * 0.3, 2.0)),
        1.0,
        color,
    );
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    if resp.dragged() {
        resp.drag_delta().y
    } else {
        0.0
    }
}

/// Maximize/restore control in the top-right of `plot_rect`. Returns
/// true when the button was clicked.
pub(crate) fn paint_maximize_button(
    ui: &mut egui::Ui,
    plot_rect: egui::Rect,
    slot: &str,
    is_maximized: bool,
) -> bool {
    let btn_size = 18.0_f32;
    let btn_margin = 8.0_f32;
    let btn_rect = egui::Rect::from_min_size(
        egui::pos2(
            plot_rect.right() - btn_size - btn_margin,
            plot_rect.top() + btn_margin,
        ),
        egui::vec2(btn_size, btn_size),
    );
    let resp = ui.interact(
        btn_rect,
        ui.id().with(("maximize_btn", slot)),
        egui::Sense::click(),
    );
    let hovered = resp.hovered();
    let alpha = if hovered { 220u8 } else { 80 };
    let bg = Color32::from_rgba_premultiplied(40, 40, 45, alpha);
    let icon_color = Color32::from_rgba_premultiplied(200, 200, 200, alpha);
    let stroke = Stroke::new(1.5_f32, icon_color);
    ui.painter().rect_filled(btn_rect, 3.0, bg);
    if is_maximized {
        let inset = 4.0_f32;
        let s = btn_size - inset * 2.0;
        let offset = s * 0.3;
        let back = egui::Rect::from_min_size(
            egui::pos2(btn_rect.left() + inset + offset, btn_rect.top() + inset),
            egui::vec2(s - offset, s - offset),
        );
        ui.painter()
            .rect_stroke(back, 0.0, Stroke::new(1.0_f32, icon_color));
        let front = egui::Rect::from_min_size(
            egui::pos2(btn_rect.left() + inset, btn_rect.top() + inset + offset),
            egui::vec2(s - offset, s - offset),
        );
        ui.painter().rect_filled(front, 0.0, bg);
        ui.painter().rect_stroke(front, 0.0, stroke);
    } else {
        ui.painter().rect_stroke(btn_rect.shrink(4.0), 0.0, stroke);
    }
    if hovered {
        resp.clone()
            .on_hover_text(if is_maximized { "Restore" } else { "Maximize" });
    }
    resp.clicked()
}

/// Apply a maximize click and a sizer drag to both the live layout
/// and its sidebar draft so Apply stays clean.
///
/// Every `egui_plot::Plot` widget must call this (or
/// [`finish_plot_slot`]) after it draws. Catalog rows that contain
/// several plots use [`finish_plot_slot`] with that widget's plot id
/// as `slot`.
pub(crate) fn finish_catalog_plot(
    ui: &mut egui::Ui,
    applied: &mut PlotLayout,
    draft: &mut PlotLayout,
    id: PlotId,
    plot_rect: Option<egui::Rect>,
) {
    finish_plot_slot(ui, applied, draft, id, id.chrome_slot(), plot_rect);
}

/// Like [`finish_catalog_plot`], keyed by a unique `slot` so several
/// plots in one catalog row each get a maximize button and sizer.
pub(crate) fn finish_plot_slot(
    ui: &mut egui::Ui,
    applied: &mut PlotLayout,
    draft: &mut PlotLayout,
    id: PlotId,
    slot: &str,
    plot_rect: Option<egui::Rect>,
) {
    let Some(rect) = plot_rect else {
        return;
    };
    if paint_maximize_button(
        ui,
        rect,
        slot,
        applied.maximized_slot.as_deref() == Some(slot),
    ) {
        applied.toggle_slot_maximize(id, slot);
        draft.maximized = applied.maximized;
        draft.maximized_slot = applied.maximized_slot.clone();
    }
    if applied.maximized_slot.is_some() {
        return;
    }
    let dy = draw_plot_sizer(ui, slot);
    if dy != 0.0 {
        let h = applied.slot_height(slot, id) + dy;
        applied.set_slot_height(slot, h);
        if slot == id.chrome_slot() {
            applied.set_plot_height(id, applied.slot_height(slot, id));
        }
        draft.set_slot_height(slot, applied.slot_height(slot, id));
        if slot == id.chrome_slot() {
            draft.set_plot_height(id, applied.plot_height(id));
        }
    }
}

/// Skip this widget when another slot is maximized; set height / fill.
pub(crate) fn prepare_plot_slot(
    layout: &PlotLayout,
    id: PlotId,
    slot: &str,
    cursor: &mut SysCursor,
    remaining: f32,
) -> bool {
    if !layout.slot_is_shown(slot) {
        return false;
    }
    let fill = layout.maximized_slot.as_deref() == Some(slot);
    cursor.fill_remaining = fill;
    cursor.panel_h = if fill {
        remaining.max(PLOT_H_MIN)
    } else {
        layout.slot_height(slot, id)
    };
    cursor.last_plot_rect = None;
    true
}

/// Paint a vertical band between two x values on a plot. `egui_plot`
/// doesn't ship a "filled vertical region" primitive in 0.27, so we
/// approximate one with a four-vertex polygon — cheap and visually
/// indistinguishable from a real fill at the resolutions we render
/// at.
///
/// Caller passes explicit `y_lo` / `y_hi` instead of reading them
/// from `plot_ui.plot_bounds()`. Reading bounds + including a
/// polygon at those bounds is a feedback loop with the plot's auto
/// margin — every frame the polygon's vertices push the bounds
/// outward by the y-margin %, and the y-axis grows exponentially
/// ("infinite y-axis when zooming"). Passing fixed bounds keeps the
/// overlay strictly inside the requested range.
pub fn draw_x_band(
    plot_ui: &mut egui_plot::PlotUi,
    x_lo: f64,
    x_hi: f64,
    y_lo: f64,
    y_hi: f64,
    fill: Color32,
) {
    if x_hi <= x_lo || y_hi <= y_lo {
        return;
    }
    let pts = PlotPoints::new(vec![[x_lo, y_lo], [x_hi, y_lo], [x_hi, y_hi], [x_lo, y_hi]]);
    plot_ui.polygon(
        egui_plot::Polygon::new(pts)
            .fill_color(fill)
            .stroke(Stroke::NONE),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn y_axis_top_pads_positive_peaks() {
        assert!((y_axis_top(100.0) - 108.0).abs() < 1e-9);
        assert!((y_axis_top(0.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn percent_y_axis_top_never_exceeds_100() {
        assert!((percent_y_axis_top(8.0) - 10.0).abs() < 1e-9);
        assert!((percent_y_axis_top(80.0) - 100.0).abs() < 1e-9);
        assert!((percent_y_axis_top(100.0) - 100.0).abs() < 1e-9);
        assert!((percent_y_axis_top(140.0) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn y_axis_span_pads_both_sides_without_pinning_zero() {
        let (lo, hi) = y_axis_span(0.9, 1.1);
        assert!((lo - 0.884).abs() < 1e-9);
        assert!((hi - 1.116).abs() < 1e-9);
        let (lo, hi) = y_axis_span(1.0, 1.0);
        assert!((lo - 0.92).abs() < 1e-9);
        assert!((hi - 1.08).abs() < 1e-9);
    }

    #[test]
    fn step_peak_in_view_includes_held_left_edge() {
        let data = [[1.0, 5.0], [2.0, 5.0], [2.0, 10.0], [3.0, 10.0]];
        assert!((step_peak_in_view(&data, [1.5, 1.9]) - 5.0).abs() < 1e-9);
        assert!((step_peak_in_view(&data, [1.5, 2.5]) - 10.0).abs() < 1e-9);
        assert!((step_peak_in_view(&data, [0.0, 0.5]) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn seed_view_x_clamps_seed_to_data() {
        assert_eq!(seed_view_x(0.0, 75.0, 0.0, 75.0), [0.0, 75.0]);
        assert_eq!(seed_view_x(10.0, 20.0, 0.0, 75.0), [10.0, 20.0]);
        assert_eq!(seed_view_x(0.0, 75.0, 5.0, 10.0), [5.0, 10.0]);
    }

    #[test]
    fn seed_view_x_falls_back_when_windows_miss() {
        // Zoom past the series falls back to the data extent.
        assert_eq!(seed_view_x(80.0, 90.0, 0.0, 75.0), [0.0, 75.0]);
        assert_eq!(seed_view_x(0.0, 0.0, 0.0, 0.0), [0.0, 1.0]);
    }

    #[test]
    fn bucket_series_follows_the_bucket_count() {
        let raw = [[0.0, 10.0], [1.0, 30.0]];
        let one = bucket_series(&raw, [0.0, 2.0], 1);
        assert_eq!(one.len(), 2);
        assert!((one[0][1] - 20.0).abs() < 1e-9);
        assert!((one[1][0] - 2.0).abs() < 1e-9);
        let four = bucket_series(&raw, [0.0, 2.0], 4);
        let values: Vec<f64> = four.iter().map(|p| p[1]).collect();
        assert!(values.iter().any(|y| (*y - 10.0).abs() < 1e-9));
        assert!(values.iter().any(|y| (*y - 30.0).abs() < 1e-9));
        assert!(four.len() > one.len());
    }

    #[test]
    fn bucket_series_skips_empty_time_before_the_first_sample() {
        let raw = [[1.0, 30.0]];
        let binned = bucket_series(&raw, [0.0, 2.0], 2);
        assert!(binned.iter().all(|p| (p[1] - 30.0).abs() < 1e-9));
    }

    #[test]
    fn hold_last_to_edge_extends_a_short_series() {
        let mut pts = vec![[0.0, 11.0], [423.0, 11.0]];
        hold_last_to_edge(&mut pts, 821.0);
        assert_eq!(pts, vec![[0.0, 11.0], [423.0, 11.0], [821.0, 11.0]]);
    }

    #[test]
    fn hold_last_to_edge_skips_when_already_at_edge() {
        let mut pts = vec![[0.0, 5.0], [10.0, 5.0]];
        hold_last_to_edge(&mut pts, 10.0);
        assert_eq!(pts, vec![[0.0, 5.0], [10.0, 5.0]]);
    }
}
