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

//! Derivation of plottable time series from the periodic `if`, `tcp`,
//! `hpts`, and `ella`/`fanout_node` sample profiles. Everything here is
//! egui-free: it turns the raw cumulative counters / state gauges the
//! kernel (or user-space provider) emits into `[t_s, y]` point vectors
//! the GUI (and, in principle, the CLI) can render.

use crate::elog_parser::*;

/// Minimum spacing between two accepted samples of the same series
/// before we compute a rate from them, in microseconds. The samplers
/// run at ~10 Hz (100 ms), so anything closer than 1 ms is a
/// near-duplicate emission (the providers occasionally emit twice for
/// one logical tick) and would produce a divide-by-tiny-dt spike.
const MIN_RATE_DT_US: u64 = 1_000;

/// One accepted diff interval from [`cumulative_intervals`]:
/// `(t_seconds_since_t0, value_delta, dt_seconds)`, where `t` is the end
/// of the interval. `value_delta / dt_seconds` is that interval's rate;
/// keeping them separate (instead of pre-dividing) lets
/// [`smooth_weighted_rate`] weight by how much wall-clock time each
/// interval actually represents.
type RateInterval = (f64, f64, f64);

/// Diff a cumulative counter into accepted `(t, value_delta, dt)`
/// intervals.
///
/// `points` is `(timestamp_us, cumulative_value)` in timestamp order.
///
/// Robustness rules, in order:
///   * **Non-increasing samples are skipped without advancing the
///     baseline.** A cumulative counter only ever grows, so a value that
///     drops is either a spurious zero (some providers interleave a
///     `0`-valued sample from an idle sibling session that shares the
///     counter key) or a reset — drop it and keep the last good
///     baseline rather than emitting a huge false spike on the way back
///     up. A value that *repeats* means the driver hasn't refreshed its
///     stats yet: several NICs only update `obytes` every ~2–3 sample
///     ticks, so at 10 Hz most samples are stale copies. Emitting a `0`
///     rate for those (and then a 3× spike when the counter finally
///     jumps) turns a steady stream into a 0/spike sawtooth whose
///     min/max envelope badly misrepresents an aggregate like `lagg0`.
///     Instead we hold the baseline and let the eventual delta divide by
///     the *time since the counter last changed*, i.e. the true
///     sustained rate over the stale window.
///   * **Near-duplicate timestamps are absorbed** (`dt < MIN_RATE_DT_US`)
///     so a double-emitted tick doesn't divide a real delta by ~0; the
///     baseline still advances so a later sample isn't diffed against a
///     stale value.
///   * **A counter that never moves across the whole span still yields a
///     flat-zero interval, not an empty result.** The rule above holds the
///     baseline through *every* repeated value, so a counter that's
///     genuinely and permanently zero for this capture (e.g. `tcp_obytes`
///     on a lagg member port, which is only ever bumped on the lagg's own
///     ifp) never sees a `val > lval` and would otherwise diff down to
///     nothing. That's indistinguishable from "no data" once it reaches
///     the GUI, which drops empty series from the legend entirely --
///     silently hiding an interface instead of showing its true, flat
///     zero. Detecting that case here and emitting one explicit zero
///     interval keeps the two distinguishable.
fn cumulative_intervals(points: &[(u64, u64)], t0_us: u64) -> Vec<RateInterval> {
    let mut out = Vec::with_capacity(points.len());
    let mut last: Option<(u64, u64)> = None;
    for &(ts, val) in points {
        match last {
            None => last = Some((ts, val)),
            Some((lts, lval)) => {
                if val <= lval {
                    continue;
                }
                let dt = ts.saturating_sub(lts);
                if dt < MIN_RATE_DT_US {
                    last = Some((ts, val));
                    continue;
                }
                let t = ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
                out.push((t, (val - lval) as f64, dt as f64 / 1_000_000.0));
                last = Some((ts, val));
            }
        }
    }
    if out.is_empty() && points.len() >= 2 && points.iter().all(|&(_, v)| v == points[0].1) {
        let t_first = points[0].0.saturating_sub(t0_us) as f64 / 1_000_000.0;
        let t_last = points[points.len() - 1].0.saturating_sub(t0_us) as f64 / 1_000_000.0;
        out.push((t_first, 0.0, 1.0));
        if t_last > t_first {
            out.push((t_last, 0.0, 1.0));
        }
    }
    out
}

/// Repeated counter values shorter than this stay inside the next step.
/// A NIC refreshes stats every few ~100 ms samples. A repeat that lasts
/// this long is the counter sitting still, and the rate series returns
/// to zero until the next step.
const RATE_FLAT_HOLD_US: u64 = 1_000_000;

/// Per-second rate of a cumulative counter, one point per step.
///
/// A repeated value shorter than [`RATE_FLAT_HOLD_US`] stays part of the
/// next step, so a NIC that refreshes every few samples still reports the
/// rate across that refresh. Once a value has repeated for that long, the
/// series sits at zero from there through the last still sample, and the
/// next step is rated only across the samples that carried it.
fn cumulative_rate(points: &[(u64, u64)], t0_us: u64) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    let mut baseline: Option<(u64, u64)> = None;
    let mut prev_ts: Option<u64> = None;
    let mut zero_t: Option<f64> = None;
    let t_of = |ts: u64| ts.saturating_sub(t0_us) as f64 / 1_000_000.0;

    for &(ts, val) in points {
        let Some((bts, bval)) = baseline else {
            baseline = Some((ts, val));
            prev_ts = Some(ts);
            continue;
        };
        if val < bval {
            prev_ts = Some(ts);
            continue;
        }
        if val == bval {
            if zero_t.is_none() && ts.saturating_sub(bts) >= RATE_FLAT_HOLD_US {
                let t = t_of(ts);
                out.push([t, 0.0]);
                zero_t = Some(t);
            }
            prev_ts = Some(ts);
            continue;
        }
        let span = ts.saturating_sub(bts);
        if span < MIN_RATE_DT_US {
            baseline = Some((ts, val));
            prev_ts = Some(ts);
            continue;
        }
        let dt_us = if let Some(z) = zero_t {
            let prev = prev_ts.unwrap_or(bts);
            let t_prev = t_of(prev);
            if t_prev > z {
                out.push([t_prev, 0.0]);
            }
            zero_t = None;
            let dt_us = ts.saturating_sub(prev);
            if dt_us < MIN_RATE_DT_US {
                baseline = Some((ts, val));
                prev_ts = Some(ts);
                continue;
            }
            dt_us
        } else {
            span
        };
        out.push([t_of(ts), (val - bval) as f64 / (dt_us as f64 / 1_000_000.0)]);
        baseline = Some((ts, val));
        prev_ts = Some(ts);
    }
    if let (Some(z), Some(prev)) = (zero_t, prev_ts) {
        let t_prev = t_of(prev);
        if t_prev > z {
            out.push([t_prev, 0.0]);
        }
    }
    if out.is_empty() && points.len() >= 2 && points.iter().all(|&(_, v)| v == points[0].1) {
        let t_first = t_of(points[0].0);
        let t_last = t_of(points[points.len() - 1].0);
        out.push([t_first, 0.0]);
        if t_last > t_first {
            out.push([t_last, 0.0]);
        }
    }
    out
}

fn peak_y(series: &[[f64; 2]]) -> f64 {
    series.iter().fold(0.0_f64, |m, p| m.max(p[1]))
}

/// Window for [`smooth_weighted_rate`] on the tx/rx rate series. NICs
/// refresh their byte/packet counters every ~200-300ms, roughly 2-3x
/// the ~100ms sample cadence; the two periods aren't integer multiples
/// of each other, so the *observed* gap between counter changes aliases
/// between ~200ms and ~300ms (beat period ~0.5s) even though the true
/// underlying refresh cadence is steady. A window has to span several
/// beat periods to average that out — 0.5s (one beat) doesn't, and 1.5s
/// (three) does, without noticeably lagging real trend changes over a
/// multi-minute capture.
const RATE_SMOOTHING_WINDOW_SECS: f64 = 1.5;

/// Centered time-windowed average of [`cumulative_intervals`] output,
/// weighted by each interval's own `dt` rather than treating every
/// interval as one equally-weighted sample. Unweighted averaging is
/// biased here: a run of short, artificially-high-rate intervals and a
/// run of long, artificially-low-rate ones (see `RATE_SMOOTHING_WINDOW_SECS`)
/// don't actually represent equal amounts of traffic, so averaging the
/// rates directly over-counts the short ones. Weighting by `dt` is
/// equivalent to `sum(value_delta) / sum(dt)` over the window, i.e. the
/// true average rate over that span regardless of how unevenly the
/// samples happened to land within it.
///
/// Centered rather than trailing so it doesn't lag behind real trend
/// changes. `intervals` must be sorted by time; `lo`/`hi` then only
/// advance forward as the output cursor advances, making this O(n).
fn smooth_weighted_rate(intervals: &[RateInterval], window_secs: f64) -> Vec<[f64; 2]> {
    if intervals.is_empty() {
        return Vec::new();
    }
    if window_secs <= 0.0 {
        return intervals
            .iter()
            .map(|&(t, dval, dt)| [t, dval / dt])
            .collect();
    }
    let half = window_secs / 2.0;
    let mut out = Vec::with_capacity(intervals.len());
    let mut lo = 0;
    let mut hi = 0;
    let mut sum_val = 0.0_f64;
    let mut sum_dt = 0.0_f64;
    for &(t, _, _) in intervals {
        while intervals[lo].0 < t - half {
            sum_val -= intervals[lo].1;
            sum_dt -= intervals[lo].2;
            lo += 1;
        }
        while hi < intervals.len() && intervals[hi].0 <= t + half {
            sum_val += intervals[hi].1;
            sum_dt += intervals[hi].2;
            hi += 1;
        }
        out.push([t, sum_val / sum_dt]);
    }
    out
}

// ---------------------------------------------------------------------------
// Interface (`if` provider) rate series
// ---------------------------------------------------------------------------

/// Which transport-protocol slice of an [`IfRateSeries`] to read. `Other`
/// (non-TCP/UDP) comes from [`IfSample::other_ibytes`] etc, which on a
/// new-enough capture is a direct kernel counter (self-consistent with
/// tcp/udp by construction) and on an older one is derived per-sample as
/// `total - tcp - udp` -- see the schema comment on `IF_SAMPLE_BYTES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfProtocol {
    All,
    Tcp,
    Udp,
    Other,
}

/// Index into each [`IfProtocolCumulative`] array: `[tcp, udp, other]`,
/// matching the non-`All` variants of [`IfProtocol`] in declaration order.
pub const PROTO_STACK_LABELS: [&str; 3] = ["TCP", "UDP", "Other"];

/// Raw cumulative `(timestamp_us, value)` counters for one interface's
/// tcp/udp/other split, kept around (rather than only the smoothed
/// [`IfRateSeries`] rate series) so a stacked-area plot can recompute
/// per-bin deltas for all three protocols against a shared set of time
/// bins at render time -- the smoothed series are diffed/smoothed
/// independently and aren't guaranteed to land on the same timestamps,
/// so stacking them directly could produce a total that doesn't match
/// the interface's actual `All` total. Mirrors [`GoodputStack`]'s
/// "keep cumulative counters, diff into bins on demand" approach.
pub struct IfProtocolCumulative {
    /// `obytes`, one vec per `[tcp, udp, other]`.
    pub tx_bytes: [Vec<(u64, u64)>; 3],
    /// `ibytes`, one vec per `[tcp, udp, other]`.
    pub rx_bytes: [Vec<(u64, u64)>; 3],
    /// `opackets`, one vec per `[tcp, udp, other]`.
    pub tx_packets: [Vec<(u64, u64)>; 3],
    /// `ipackets`, one vec per `[tcp, udp, other]`.
    pub rx_packets: [Vec<(u64, u64)>; 3],
}

/// Per-interface derived rate series, x in seconds relative to `t0_us`.
/// Byte counters are converted to Gbit/s; packet counters to packets/s;
/// error / drop counters to events/s.
///
/// `tcp_*` / `udp_*` / `other_*` are only meaningful for captures whose
/// samples came from `IF_SAMPLE_BYTES`/`IF_SAMPLE_PACKETS` (ids 21/22);
/// on older captures (deprecated `IF_SAMPLE`, id 20) tcp/udp read as flat
/// zero and `other_*` equals the corresponding total series, since no
/// protocol split is available.
pub struct IfRateSeries {
    pub ifindex: u16,
    pub name: String,
    pub driver: String,
    pub tx_gbps: Vec<[f64; 2]>,
    pub rx_gbps: Vec<[f64; 2]>,
    pub tx_pps: Vec<[f64; 2]>,
    pub rx_pps: Vec<[f64; 2]>,
    pub tcp_tx_gbps: Vec<[f64; 2]>,
    pub tcp_rx_gbps: Vec<[f64; 2]>,
    pub tcp_tx_pps: Vec<[f64; 2]>,
    pub tcp_rx_pps: Vec<[f64; 2]>,
    pub udp_tx_gbps: Vec<[f64; 2]>,
    pub udp_rx_gbps: Vec<[f64; 2]>,
    pub udp_tx_pps: Vec<[f64; 2]>,
    pub udp_rx_pps: Vec<[f64; 2]>,
    pub other_tx_gbps: Vec<[f64; 2]>,
    pub other_rx_gbps: Vec<[f64; 2]>,
    pub other_tx_pps: Vec<[f64; 2]>,
    pub other_rx_pps: Vec<[f64; 2]>,
    /// (ierrors + oerrors) per second.
    pub err_rate: Vec<[f64; 2]>,
    /// (iqdrops + oqdrops) per second.
    pub drop_rate: Vec<[f64; 2]>,
    /// `oqdrops` per second, for an interface whose counter advanced.
    pub oqdrop_rate: Vec<[f64; 2]>,
    pub peak_tx_gbps: f64,
    pub peak_rx_gbps: f64,
    pub peak_tx_pps: f64,
    pub peak_rx_pps: f64,
    /// Total errors / drops observed over the capture (last − first).
    pub total_errs: u64,
    pub total_drops: u64,
    /// Total link-level bytes transmitted / received over the capture
    /// (last − first of the cumulative `obytes`/`ibytes` counters).
    pub total_tx_bytes: u64,
    pub total_rx_bytes: u64,
    /// True when this interface's TCP or UDP counters ever advanced
    /// during the capture -- i.e. the IP layer actually classified
    /// traffic on this ifp, as opposed to a carrier-only interface
    /// (lagg member, vlan's underlying physical NIC, bridge member)
    /// whose tcp/udp/other all read flat zero because classification
    /// happens on a different, logical ifp entirely (see
    /// `ip_proto_ifcount()`). Deliberately excludes `other_*` from the
    /// check: on a pre-split capture (deprecated `IF_SAMPLE`), `other_*`
    /// is *derived* as equal to the total series (see the struct doc
    /// above), so it reads nonzero even though no real classification
    /// exists -- tcp/udp are the only fields that are flat zero in both
    /// "no split available" and "carrier-only interface" cases, and
    /// only genuinely nonzero when classification is real.
    pub has_protocol_classification: bool,
    /// Raw cumulative tcp/udp/other counters for stacked-area
    /// rendering. `None` when `!has_protocol_classification`.
    pub proto_cum: Option<IfProtocolCumulative>,
}

impl IfRateSeries {
    /// TX series for the given protocol slice, in Gb/s.
    pub fn tx_gbps_for(&self, proto: IfProtocol) -> &[[f64; 2]] {
        match proto {
            IfProtocol::All => &self.tx_gbps,
            IfProtocol::Tcp => &self.tcp_tx_gbps,
            IfProtocol::Udp => &self.udp_tx_gbps,
            IfProtocol::Other => &self.other_tx_gbps,
        }
    }
    /// RX series for the given protocol slice, in Gb/s.
    pub fn rx_gbps_for(&self, proto: IfProtocol) -> &[[f64; 2]] {
        match proto {
            IfProtocol::All => &self.rx_gbps,
            IfProtocol::Tcp => &self.tcp_rx_gbps,
            IfProtocol::Udp => &self.udp_rx_gbps,
            IfProtocol::Other => &self.other_rx_gbps,
        }
    }
    /// TX series for the given protocol slice, in pkts/s.
    pub fn tx_pps_for(&self, proto: IfProtocol) -> &[[f64; 2]] {
        match proto {
            IfProtocol::All => &self.tx_pps,
            IfProtocol::Tcp => &self.tcp_tx_pps,
            IfProtocol::Udp => &self.udp_tx_pps,
            IfProtocol::Other => &self.other_tx_pps,
        }
    }
    /// RX series for the given protocol slice, in pkts/s.
    pub fn rx_pps_for(&self, proto: IfProtocol) -> &[[f64; 2]] {
        match proto {
            IfProtocol::All => &self.rx_pps,
            IfProtocol::Tcp => &self.tcp_rx_pps,
            IfProtocol::Udp => &self.udp_rx_pps,
            IfProtocol::Other => &self.other_rx_pps,
        }
    }
}

impl IfRateSeries {
    /// True when this interface moved no traffic at all — used by the
    /// GUI to hide the dozen idle management NICs and keep the plots
    /// legible.
    pub fn is_idle(&self) -> bool {
        self.peak_tx_gbps == 0.0
            && self.peak_rx_gbps == 0.0
            && self.peak_tx_pps == 0.0
            && self.peak_rx_pps == 0.0
    }
}

pub const BYTES_PER_S_TO_GBPS: f64 = 8.0 / 1_000_000_000.0;

/// Build one [`IfRateSeries`] per interface that emitted at least one
/// `IF_SAMPLE`, sorted by ifindex.
pub fn build_if_rate_series(prof: &IfProfile, t0_us: u64) -> Vec<IfRateSeries> {
    if prof.samples.is_empty() {
        return Vec::new();
    }
    // Samples are globally timestamp-sorted, so per-ifindex order is
    // preserved by a stable group-by.
    let mut by_if: FxHashMap<u16, Vec<&IfSample>> = FxHashMap::default();
    for s in &prof.samples {
        by_if.entry(s.ifindex).or_default().push(s);
    }

    let mut indices: Vec<u16> = by_if.keys().copied().collect();
    indices.sort_unstable();

    let mut out = Vec::with_capacity(indices.len());
    for idx in indices {
        let samples = &by_if[&idx];
        let collect = |f: &dyn Fn(&IfSample) -> u64| -> Vec<(u64, u64)> {
            samples.iter().map(|s| (s.ts, f(s))).collect()
        };

        // Smooth the plotted rate series against NIC counter-refresh
        // jitter (see `RATE_SMOOTHING_WINDOW_SECS`). err_rate/drop_rate
        // are deliberately left raw — for those a spike right when it
        // happens is more useful than a smoothed one, and they're
        // low-volume enough that jitter isn't the same problem.
        // Diffed/smoothed the same way as the totals below: tcp/udp/other
        // are each their own cumulative-per-sample counter (direct on a
        // new-enough capture, derived on an older one -- either way,
        // cumulative), so each gets its own baseline and jitter smoothing
        // rather than being computed by subtracting already-smoothed
        // rate series from each other.
        let gbps_series = |f: &dyn Fn(&IfSample) -> u64| -> Vec<[f64; 2]> {
            let mut s = smooth_weighted_rate(
                &cumulative_intervals(&collect(f), t0_us),
                RATE_SMOOTHING_WINDOW_SECS,
            );
            for p in s.iter_mut() {
                p[1] *= BYTES_PER_S_TO_GBPS;
            }
            s
        };
        let pps_series = |f: &dyn Fn(&IfSample) -> u64| -> Vec<[f64; 2]> {
            smooth_weighted_rate(
                &cumulative_intervals(&collect(f), t0_us),
                RATE_SMOOTHING_WINDOW_SECS,
            )
        };

        let tx_gbps = gbps_series(&|s| s.obytes);
        let rx_gbps = gbps_series(&|s| s.ibytes);
        let tx_pps = pps_series(&|s| s.opackets);
        let rx_pps = pps_series(&|s| s.ipackets);

        let tcp_tx_gbps = gbps_series(&|s| s.tcp_obytes);
        let tcp_rx_gbps = gbps_series(&|s| s.tcp_ibytes);
        let tcp_tx_pps = pps_series(&|s| s.tcp_opackets);
        let tcp_rx_pps = pps_series(&|s| s.tcp_ipackets);

        let udp_tx_gbps = gbps_series(&|s| s.udp_obytes);
        let udp_rx_gbps = gbps_series(&|s| s.udp_ibytes);
        let udp_tx_pps = pps_series(&|s| s.udp_opackets);
        let udp_rx_pps = pps_series(&|s| s.udp_ipackets);

        let other_tx_gbps = gbps_series(&|s| s.other_obytes);
        let other_rx_gbps = gbps_series(&|s| s.other_ibytes);
        let other_tx_pps = pps_series(&|s| s.other_opackets);
        let other_rx_pps = pps_series(&|s| s.other_ipackets);

        let err_rate = cumulative_rate(&collect(&|s| s.ierrors + s.oerrors), t0_us);
        let drop_rate = cumulative_rate(&collect(&|s| s.iqdrops + s.oqdrops), t0_us);
        let oqdrop_rate = cumulative_rate(&collect(&|s| s.oqdrops), t0_us);
        let oqdrop_rate = if peak_y(&oqdrop_rate) > 0.0 {
            oqdrop_rate
        } else {
            Vec::new()
        };

        let first = samples.first().unwrap();
        let last = samples.last().unwrap();
        let total_errs =
            (last.ierrors + last.oerrors).saturating_sub(first.ierrors + first.oerrors);
        let total_drops =
            (last.iqdrops + last.oqdrops).saturating_sub(first.iqdrops + first.oqdrops);
        let total_tx_bytes = last.obytes.saturating_sub(first.obytes);
        let total_rx_bytes = last.ibytes.saturating_sub(first.ibytes);

        let has_protocol_classification = last.tcp_obytes.saturating_sub(first.tcp_obytes) > 0
            || last.tcp_ibytes.saturating_sub(first.tcp_ibytes) > 0
            || last.udp_obytes.saturating_sub(first.udp_obytes) > 0
            || last.udp_ibytes.saturating_sub(first.udp_ibytes) > 0;
        let proto_cum = has_protocol_classification.then(|| IfProtocolCumulative {
            tx_bytes: [
                collect(&|s| s.tcp_obytes),
                collect(&|s| s.udp_obytes),
                collect(&|s| s.other_obytes),
            ],
            rx_bytes: [
                collect(&|s| s.tcp_ibytes),
                collect(&|s| s.udp_ibytes),
                collect(&|s| s.other_ibytes),
            ],
            tx_packets: [
                collect(&|s| s.tcp_opackets),
                collect(&|s| s.udp_opackets),
                collect(&|s| s.other_opackets),
            ],
            rx_packets: [
                collect(&|s| s.tcp_ipackets),
                collect(&|s| s.udp_ipackets),
                collect(&|s| s.other_ipackets),
            ],
        });

        out.push(IfRateSeries {
            ifindex: idx,
            name: prof.name_for(idx),
            driver: prof
                .interfaces
                .get(&idx)
                .map(|i| i.driver.clone())
                .unwrap_or_default(),
            peak_tx_gbps: peak_y(&tx_gbps),
            peak_rx_gbps: peak_y(&rx_gbps),
            peak_tx_pps: peak_y(&tx_pps),
            peak_rx_pps: peak_y(&rx_pps),
            tx_gbps,
            rx_gbps,
            tx_pps,
            rx_pps,
            tcp_tx_gbps,
            tcp_rx_gbps,
            tcp_tx_pps,
            tcp_rx_pps,
            udp_tx_gbps,
            udp_rx_gbps,
            udp_tx_pps,
            udp_rx_pps,
            other_tx_gbps,
            other_rx_gbps,
            other_tx_pps,
            other_rx_pps,
            err_rate,
            drop_rate,
            oqdrop_rate,
            total_errs,
            total_drops,
            total_tx_bytes,
            total_rx_bytes,
            has_protocol_classification,
            proto_cum,
        });
    }
    out
}

/// Per-interface capture TX/RX byte totals for the trace-info popup.
pub struct IfByteTotals {
    pub name: String,
    pub total_tx_bytes: u64,
    pub total_rx_bytes: u64,
}

/// First-to-last `obytes`/`ibytes` per interface, busiest TX first.
/// Skips the rate-series work [`build_if_rate_series`] does for plots.
pub fn if_byte_totals(prof: &IfProfile) -> Vec<IfByteTotals> {
    if prof.samples.is_empty() {
        return Vec::new();
    }
    let mut first_last: FxHashMap<u16, (&IfSample, &IfSample)> = FxHashMap::default();
    for s in &prof.samples {
        first_last
            .entry(s.ifindex)
            .and_modify(|(_, last)| *last = s)
            .or_insert((s, s));
    }
    let mut out: Vec<IfByteTotals> = first_last
        .into_iter()
        .map(|(idx, (first, last))| IfByteTotals {
            name: prof.name_for(idx),
            total_tx_bytes: last.obytes.saturating_sub(first.obytes),
            total_rx_bytes: last.ibytes.saturating_sub(first.ibytes),
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.total_tx_bytes));
    out
}

/// Compute stacked-area bin centers + cumulative-through-band tops for
/// one direction/metric slice of an interface's protocol split, over
/// the visible x-range `[vx0, vx1]` (seconds since `t0_us`), in `nbins`
/// even bins.
///
/// Each protocol is diffed and smoothed the same way as the All
/// Interfaces line ([`cumulative_intervals`] + [`smooth_weighted_rate`]),
/// then sampled at the bin centers so a tight zoom stays a connected
/// area at the sustained rate. `to_rate_per_sec` scales into the display
/// unit ([`BYTES_PER_S_TO_GBPS`] for Gb/s, `1.0` for pkts/s).
///
/// Returns `(centers, tops)`: `tops[k][i]` is the cumulative stacked
/// value through band `k` at `centers[i]` -- `tops[2]` is the full
/// stack (tcp+udp+other).
pub fn protocol_stack_bins(
    cum: &[Vec<(u64, u64)>; 3],
    t0_us: u64,
    vx0: f64,
    vx1: f64,
    nbins: usize,
    to_rate_per_sec: f64,
) -> (Vec<f64>, [Vec<f64>; 3]) {
    let mut centers = Vec::with_capacity(nbins);
    let mut tops: [Vec<f64>; 3] = [
        Vec::with_capacity(nbins),
        Vec::with_capacity(nbins),
        Vec::with_capacity(nbins),
    ];
    if nbins == 0 || vx1 <= vx0 {
        return (centers, tops);
    }
    let series: [Vec<[f64; 2]>; 3] = std::array::from_fn(|k| {
        let mut s = smooth_weighted_rate(
            &cumulative_intervals(&cum[k], t0_us),
            RATE_SMOOTHING_WINDOW_SECS,
        );
        for p in &mut s {
            p[1] *= to_rate_per_sec;
        }
        s
    });
    let bin_width = (vx1 - vx0) / nbins as f64;
    for i in 0..nbins {
        let t = vx0 + (i as f64 + 0.5) * bin_width;
        centers.push(t);
        let mut acc = 0.0;
        for k in 0..3 {
            acc += interpolate_rate(&series[k], t);
            tops[k].push(acc);
        }
    }
    (centers, tops)
}

fn interpolate_rate(series: &[[f64; 2]], t: f64) -> f64 {
    let Some(first) = series.first() else {
        return 0.0;
    };
    if t <= first[0] {
        return first[1];
    }
    let last = series[series.len() - 1];
    if t >= last[0] {
        return last[1];
    }
    let i = series.partition_point(|p| p[0] < t);
    let [t0, y0] = series[i - 1];
    let [t1, y1] = series[i];
    let dt = t1 - t0;
    if dt <= 0.0 {
        y1
    } else {
        y0 + (t - t0) / dt * (y1 - y0)
    }
}

// ---------------------------------------------------------------------------
// TCP-stack (`tcp` provider) sample series
// ---------------------------------------------------------------------------

/// One named PCB-state line over time (x in seconds relative to `t0_us`).
pub struct TcpStateLine {
    pub name: &'static str,
    pub data: Vec<[f64; 2]>,
}

/// PCB-state gauge lines over time. `estab` is the dominant state by ~1–3
/// orders of magnitude; every other kernel state is exposed individually
/// in `others` (TCP lifecycle order) rather than rolled into a lossy
/// "other" bucket. The view draws `estab` and `others` on one shared plot.
pub struct TcpStateSeries {
    pub estab: Vec<[f64; 2]>,
    /// The ten non-`estab` states, each as its own line.
    pub others: Vec<TcpStateLine>,
}

/// Name + gauge accessor for one PCB state.
type TcpStateAccessor = (&'static str, fn(&TcpStateSample) -> u64);

/// The ten non-`established` PCB states, in TCP lifecycle order, with an
/// accessor for each. `established` rides its own panel so it's excluded.
const TCP_OTHER_STATES: [TcpStateAccessor; 10] = [
    ("closed", |x| x.closed),
    ("listen", |x| x.listen),
    ("syn_sent", |x| x.syn_sent),
    ("syn_rcvd", |x| x.syn_rcvd),
    ("fin_wait_1", |x| x.fin_wait_1),
    ("fin_wait_2", |x| x.fin_wait_2),
    ("close_wait", |x| x.close_wait),
    ("closing", |x| x.closing),
    ("last_ack", |x| x.last_ack),
    ("time_wait", |x| x.time_wait),
];

/// Turn the raw `TCP_SAMPLE` state gauges into plottable lines. Samples
/// are plotted in timestamp order; a capture with a single sampling
/// session (the common case) yields clean lines.
pub fn build_tcp_state_series(prof: &TcpSampleProfile, t0_us: u64) -> TcpStateSeries {
    let n = prof.state_samples.len();
    let mut estab = Vec::with_capacity(n);
    let mut others: Vec<TcpStateLine> = TCP_OTHER_STATES
        .iter()
        .map(|(name, _)| TcpStateLine {
            name,
            data: Vec::with_capacity(n),
        })
        .collect();
    for sample in &prof.state_samples {
        let t = sample.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        estab.push([t, sample.estab as f64]);
        for (line, (_, f)) in others.iter_mut().zip(TCP_OTHER_STATES.iter()) {
            line.data.push([t, f(sample) as f64]);
        }
    }
    TcpStateSeries { estab, others }
}

fn tcp_closing_pcbs(s: &TcpStateSample) -> u64 {
    s.close_wait + s.fin_wait_1 + s.fin_wait_2 + s.closing + s.last_ack + s.time_wait
}

/// Cumulative establish / teardown counts from consecutive `TCP_SAMPLE`
/// PCB gauges (the same ticks as TCP Connections by State). Rates are
/// binned at render time into 250 buckets over the visible window.
///
/// Between ticks, connections that leave `estab` land in the closing
/// states. Pairing `Δestab` with `Δclosing` recovers both directions
/// when `estab` is flat but `time_wait` is growing.
pub struct TcpConnRateSeries {
    pub estab: Vec<(f64, u64)>,
    pub teardown: Vec<(f64, u64)>,
}

pub fn build_tcp_conn_rate_series(prof: &TcpSampleProfile, t0_us: u64) -> TcpConnRateSeries {
    let mut estab = Vec::new();
    let mut teardown = Vec::new();
    let mut cum_e = 0u64;
    let mut cum_d = 0u64;
    let mut prev: Option<&TcpStateSample> = None;
    for s in &prof.state_samples {
        let Some(p) = prev else {
            prev = Some(s);
            continue;
        };
        let dt_us = s.ts.saturating_sub(p.ts);
        prev = Some(s);
        if dt_us < MIN_RATE_DT_US {
            continue;
        }
        let d_estab = s.estab as f64 - p.estab as f64;
        let d_closing = tcp_closing_pcbs(s) as f64 - tcp_closing_pcbs(p) as f64;
        let left = d_closing.max(0.0) + (-(d_estab + d_closing)).max(0.0);
        let entered = (left + d_estab).max(0.0);
        cum_e = cum_e.saturating_add(entered.round() as u64);
        cum_d = cum_d.saturating_add(left.round() as u64);
        let t = s.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        estab.push((t, cum_e));
        teardown.push((t, cum_d));
    }
    TcpConnRateSeries { estab, teardown }
}

/// Conn/s in `nbins` bins over `[vx0, vx1]` from a cumulative count
/// series (same bucket count as HTTP RPS and the other stacked System
/// plots).
pub fn tcp_conn_rate_bins(
    samples: &[(f64, u64)],
    vx0: f64,
    vx1: f64,
    nbins: usize,
) -> Vec<[f64; 2]> {
    if nbins == 0 || vx1 <= vx0 || samples.is_empty() {
        return Vec::new();
    }
    let width = (vx1 - vx0) / nbins as f64;
    if width <= 0.0 {
        return Vec::new();
    }
    (0..nbins)
        .map(|i| {
            let e0 = vx0 + i as f64 * width;
            let e1 = vx0 + (i + 1) as f64 * width;
            let delta = cumulative_at(samples, e1).saturating_sub(cumulative_at(samples, e0));
            [vx0 + (i as f64 + 0.5) * width, delta as f64 / width]
        })
        .collect()
}

/// One stack's cumulative `TCP_RPS_SAMPLE` counter, capture-relative
/// seconds. Idle stacks (peak rate 0) are dropped at build time.
pub struct TcpRpsSeries {
    pub label: String,
    pub samples: Vec<(f64, u64)>,
    pub peak: f64,
}

/// Skip count drops so a zero priming tick does not become the baseline.
fn monotonic_req_counts(raw: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut out = Vec::with_capacity(raw.len());
    for &(ts, n) in raw {
        if out.last().is_some_and(|&(_, prev)| n < prev) {
            continue;
        }
        out.push((ts, n));
    }
    out
}

/// Build per-stack cumulative RPS counters, dropping stacks that never
/// registered a request (idle VNETs emit an all-zero counter). Grouped
/// by `(provider_id, session_id)` — one "tcp" provider per loaded stack,
/// all reusing the same stack-global session id — and labelled by stack
/// name (or `tcp#<provider>` when the stack emitted no `TCP_STACK_INFO`).
pub fn build_tcp_rps_series(prof: &TcpSampleProfile, t0_us: u64) -> Vec<TcpRpsSeries> {
    if prof.rps_samples.is_empty() {
        return Vec::new();
    }
    let mut by_stack: FxHashMap<(u16, u64), Vec<(u64, u64)>> = FxHashMap::default();
    for s in &prof.rps_samples {
        by_stack
            .entry((s.provider_id, s.session_id))
            .or_default()
            .push((s.ts, s.req_count));
    }
    let mut keys: Vec<(u16, u64)> = by_stack.keys().copied().collect();
    keys.sort_unstable();

    let mut out = Vec::new();
    for key in keys {
        let mut raw = by_stack.remove(&key).unwrap_or_default();
        raw.sort_by_key(|&(ts, _)| ts);
        let raw = monotonic_req_counts(&raw);
        if raw.is_empty() {
            continue;
        }
        let peak = peak_y(&cumulative_rate(&raw, t0_us));
        if peak <= 0.0 {
            continue;
        }
        let samples: Vec<(f64, u64)> = raw
            .iter()
            .map(|&(ts, n)| (ts.saturating_sub(t0_us) as f64 / 1_000_000.0, n))
            .collect();
        out.push(TcpRpsSeries {
            label: prof.stack_label(key.0, key.1),
            samples,
            peak,
        });
    }
    out
}

fn cumulative_at(samples: &[(f64, u64)], t: f64) -> u64 {
    let i = samples.partition_point(|p| p.0 <= t);
    if i == 0 {
        0
    } else {
        samples[i - 1].1
    }
}

/// Stacks summed into `nbins` req/s bins over `[vx0, vx1]` (same count
/// as the other stacked System plots).
pub fn tcp_rps_bins(series: &[TcpRpsSeries], vx0: f64, vx1: f64, nbins: usize) -> Vec<[f64; 2]> {
    if nbins == 0 || vx1 <= vx0 || series.is_empty() {
        return Vec::new();
    }
    let width = (vx1 - vx0) / nbins as f64;
    if width <= 0.0 {
        return Vec::new();
    }
    (0..nbins)
        .map(|i| {
            let e0 = vx0 + i as f64 * width;
            let e1 = vx0 + (i + 1) as f64 * width;
            let mut delta = 0u64;
            for s in series {
                delta = delta.saturating_add(
                    cumulative_at(&s.samples, e1).saturating_sub(cumulative_at(&s.samples, e0)),
                );
            }
            [vx0 + (i as f64 + 0.5) * width, delta as f64 / width]
        })
        .collect()
}

/// Aggregate goodput distribution over the whole capture: for each
/// sampling session, the per-bucket delta (last − first cumulative
/// snapshot), summed across sessions. Bucket `b` counts per-connection
/// goodput samples whose Mbps value has `flsll(mbps) == b` — i.e. a
/// log2 histogram (`[2^(b-1), 2^b)` Mbps for `b ≥ 1`; bucket 0 is
/// 0 Mbps; the top bucket is saturated).
pub struct GoodputHist {
    pub buckets: Vec<u64>,
    pub total: u64,
    /// Stack label(s) that contributed goodput samples (sorted, deduped).
    /// Goodput is a per-stack event, so this is who the distribution
    /// actually belongs to — typically a single loadable stack.
    pub labels: Vec<String>,
}

impl GoodputHist {
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }
}

pub fn build_goodput_hist(prof: &TcpSampleProfile) -> GoodputHist {
    let mut first: FxHashMap<(u16, u64), &Vec<u64>> = FxHashMap::default();
    let mut last: FxHashMap<(u16, u64), &Vec<u64>> = FxHashMap::default();
    for s in &prof.gp_samples {
        let key = (s.provider_id, s.session_id);
        first.entry(key).or_insert(&s.hist);
        last.insert(key, &s.hist);
    }
    let mut buckets = vec![0u64; 16];
    for (key, last_hist) in &last {
        let first_hist = first.get(key).copied().unwrap_or(last_hist);
        for (b, &lv) in last_hist.iter().enumerate() {
            if b >= buckets.len() {
                break;
            }
            let fv = first_hist.get(b).copied().unwrap_or(0);
            buckets[b] += lv.saturating_sub(fv);
        }
    }
    let total = buckets.iter().sum();
    let labels = stack_labels(prof, last.keys().copied());
    GoodputHist {
        buckets,
        total,
        labels,
    }
}

/// Whole-capture smoothed-RTT distribution: per-bucket delta (last − first)
/// of each `TCP_SRTT_SAMPLE` session, summed. 14 custom usec buckets; see
/// [`srtt_bucket_label`].
pub fn build_srtt_hist(prof: &TcpSampleProfile) -> GoodputHist {
    let mut first: FxHashMap<(u16, u64), &Vec<u64>> = FxHashMap::default();
    let mut last: FxHashMap<(u16, u64), &Vec<u64>> = FxHashMap::default();
    for s in &prof.srtt_samples {
        let key = (s.provider_id, s.session_id);
        first.entry(key).or_insert(&s.hist);
        last.insert(key, &s.hist);
    }
    let mut buckets = vec![0u64; 14];
    for (key, last_hist) in &last {
        let first_hist = first.get(key).copied().unwrap_or(last_hist);
        for (b, &lv) in last_hist.iter().enumerate() {
            if b >= buckets.len() {
                break;
            }
            let fv = first_hist.get(b).copied().unwrap_or(0);
            buckets[b] += lv.saturating_sub(fv);
        }
    }
    let total = buckets.iter().sum();
    let labels = stack_labels(prof, last.keys().copied());
    GoodputHist {
        buckets,
        total,
        labels,
    }
}

/// Sorted, deduped stack labels for a set of `(provider_id, session_id)`
/// keys, via [`TcpSampleProfile::stack_label`].
fn stack_labels(prof: &TcpSampleProfile, keys: impl Iterator<Item = (u16, u64)>) -> Vec<String> {
    let mut labels: Vec<String> = keys.map(|(p, s)| prof.stack_label(p, s)).collect();
    labels.sort();
    labels.dedup();
    labels
}

/// Merged cumulative goodput histogram over time, for the stacked
/// distribution-over-time plot. The kernel emits `TCP_GP_SAMPLE` as a
/// *cumulative* per-bucket snapshot every sample period; this merges all
/// emitting sessions into one global cumulative timeline (summing each
/// session's deltas) so the view can diff any two points to get the
/// per-window composition. `cum` is stored flat, row-major:
/// `cum[i * nbuckets + b]` is bucket `b`'s cumulative count at `times[i]`.
pub struct GoodputStack {
    pub nbuckets: usize,
    /// Seconds relative to `t0_us`, one per emitted snapshot, ascending.
    pub times: Vec<f64>,
    /// Flat row-major cumulative counts: `nbuckets` values per time.
    pub cum: Vec<u64>,
    /// Stack label(s) that contributed goodput samples (sorted, deduped).
    pub labels: Vec<String>,
}

impl GoodputStack {
    /// Need at least two snapshots to diff a window.
    pub fn is_empty(&self) -> bool {
        self.nbuckets == 0 || self.times.len() < 2
    }

    /// Cumulative count in bucket `b` at snapshot row `i`.
    #[inline]
    pub fn at(&self, i: usize, b: usize) -> u64 {
        self.cum[i * self.nbuckets + b]
    }
}

/// Build the merged cumulative goodput timeline. `gp_samples` is assumed
/// sorted by ts (the parser sorts it). Each session's per-bucket counter
/// is cumulative; we track each session's last snapshot and accumulate
/// deltas into a global running total so multi-session captures merge
/// cleanly (the common case is a single global session).
pub fn build_goodput_stack(prof: &TcpSampleProfile, t0_us: u64) -> GoodputStack {
    build_hist_stack(prof, &prof.gp_samples, t0_us, 16)
}

/// Same merge as [`build_goodput_stack`], for any cumulative `TcpGpSample`
/// series (send-size and LRO-flush hists cap at 64 buckets).
pub fn build_hist_stack(
    prof: &TcpSampleProfile,
    samples: &[TcpGpSample],
    t0_us: u64,
    max_buckets: usize,
) -> GoodputStack {
    let nbuckets = samples
        .iter()
        .map(|s| s.hist.len())
        .max()
        .unwrap_or(0)
        .min(max_buckets);
    if nbuckets == 0 || samples.len() < 2 {
        return GoodputStack {
            nbuckets,
            times: Vec::new(),
            cum: Vec::new(),
            labels: Vec::new(),
        };
    }
    let mut last: FxHashMap<(u16, u64), Vec<u64>> = FxHashMap::default();
    let mut global = vec![0u64; nbuckets];
    let mut times = Vec::with_capacity(samples.len());
    let mut cum = Vec::with_capacity(samples.len() * nbuckets);
    for s in samples {
        let prev = last
            .entry((s.provider_id, s.session_id))
            .or_insert_with(|| vec![0u64; nbuckets]);
        for b in 0..nbuckets {
            let v = s.hist.get(b).copied().unwrap_or(0);
            // Cumulative counters only grow; a drop is a reset, so treat
            // the new value itself as the delta rather than going negative.
            let delta = if v >= prev[b] { v - prev[b] } else { v };
            global[b] += delta;
            prev[b] = v;
        }
        times.push(s.ts.saturating_sub(t0_us) as f64 / 1_000_000.0);
        cum.extend_from_slice(&global);
    }
    let labels = stack_labels(prof, last.keys().copied());
    GoodputStack {
        nbuckets,
        times,
        cum,
        labels,
    }
}

/// Human-readable per-connection goodput range for log2 bucket `b`.
/// Matches the kernel's `flsll(gp_mbps)` bucketing (see
/// `rack_gput_hist`): bucket 0 is "0", bucket `b` covers
/// `[2^(b-1), 2^b)` Mbps, and the last bucket is the saturated tail.
pub fn goodput_bucket_label(b: usize, nbuckets: usize) -> String {
    if b == 0 {
        return "0".to_string();
    }
    let fmt = |mbps: u64| -> String {
        if mbps >= 1000 {
            let g = mbps as f64 / 1000.0;
            if (g.fract()).abs() < 1e-9 {
                format!("{g:.0}G")
            } else {
                format!("{g:.1}G")
            }
        } else {
            format!("{mbps}M")
        }
    };
    let lo = 1u64 << (b - 1);
    if b + 1 >= nbuckets {
        return format!("≥{}", fmt(lo));
    }
    let hi = 1u64 << b;
    format!("{}–{}", fmt(lo), fmt(hi))
}

/// Human-readable smoothed-RTT range for custom bucket `b`.
/// Matches `rack_srtt_hist_type_inc`: bucket 0 is 0, bucket 1 is
/// `<512us`, and bucket `b >= 2` starts at `1 << (b + 7)` usec. The
/// last bucket is the saturated tail.
pub fn srtt_bucket_label(b: usize, nbuckets: usize) -> String {
    if b == 0 {
        return "0".to_string();
    }
    if b == 1 {
        return "<512us".to_string();
    }
    let lo = 1u64 << (b + 7);
    if b + 1 >= nbuckets {
        return format!(">={}", fmt_srtt_edge(lo));
    }
    let hi = 1u64 << (b + 8);
    format!("{}-{}", fmt_srtt_edge(lo), fmt_srtt_edge(hi))
}

fn fmt_srtt_edge(us: u64) -> String {
    if us >= 1_000_000 {
        let tenths = (us + 50_000) / 100_000;
        if tenths.is_multiple_of(10) {
            format!("{}s", tenths / 10)
        } else {
            format!("{}.{}s", tenths / 10, tenths % 10)
        }
    } else if us >= 1_000 {
        format!("{}ms", (us + 500) / 1_000)
    } else {
        format!("{us}us")
    }
}

// ---------------------------------------------------------------------------
// hpts wheel per-instance histogram samples (LATENESS_SAMPLE / TP_BATCH_SAMPLE)
// ---------------------------------------------------------------------------

/// Human-readable usec range for exponential (log2, `flsll`) bucket `b`
/// of an `HPTS_LATENESS_SAMPLE` histogram. Bucket 0 is "0", bucket `b`
/// covers `[2^(b-1), 2^b)` usec, and the last bucket is the saturated
/// tail -- mirrors `HISTOGRAM_EXP_GENERATE`'s `flsll(value)` bucketing
/// in `sys/histogram.h`.
pub fn hpts_lateness_bucket_label(b: usize, nbuckets: usize) -> String {
    if b == 0 {
        return "0".to_string();
    }
    if b + 1 >= nbuckets {
        return format!(">={}us", 1u64 << (b - 1));
    }
    format!("{}-{}us", 1u64 << (b - 1), (1u64 << b) - 1)
}

/// Human-readable value for linear bucket `b` of an `HPTS_TP_BATCH_SAMPLE`
/// histogram: bucket `b` is exactly `b` connections processed per batch,
/// and the last bucket is the saturated tail -- mirrors
/// `HISTOGRAM_LIN_GENERATE`'s clamping in `sys/histogram.h`.
pub fn hpts_batch_bucket_label(b: usize, nbuckets: usize) -> String {
    lin_hist_bucket_label(b, nbuckets)
}

/// Linear histogram bucket `b`: the value is `b`, and the last bucket
/// is the saturated tail (`HISTOGRAM_LIN_GENERATE` in `sys/histogram.h`).
pub fn lin_hist_bucket_label(b: usize, nbuckets: usize) -> String {
    if b + 1 >= nbuckets {
        format!("{b}+")
    } else {
        format!("{b}")
    }
}

/// Build the merged cumulative timeline for one of the `hpts` provider's
/// per-instance wheel histogram samples (`HPTS_LATENESS_SAMPLE` /
/// `HPTS_TP_BATCH_SAMPLE`), for the stacked distribution-over-time plot.
/// Reuses [`GoodputStack`]'s shape (times + flat row-major cumulative
/// counts) even though this isn't goodput data -- the type is already a
/// generic "diffed cumulative histogram over time" container.
///
/// Keyed by `session_id`, not `cpu`: each `tcp_hpts_entry` logs to its
/// own fixed `p_eventlog_session` for the life of the machine, but the
/// *logging* cpu in the event header is whichever cpu happened to be
/// running `tcp_hptsi()` for that trip (wheels are commonly processed
/// opportunistically from a foreign send path) -- so `cpu` is not a
/// stable per-instance key. See `cli::hpts::diff_cumulative_hist` for
/// the capture-wide-total counterpart of this same reasoning.
///
/// Unlike per-connection goodput counters (which start close to the
/// beginning of the capture), `hist_lateness_usec` / `hist_tp_batch_size`
/// are machine-lifetime counters. An instance's first observed sample is
/// therefore recorded only as a baseline (zero contribution to the
/// running total) rather than counted at full value -- otherwise the
/// first plotted point for every hpts instance would dump hours/days of
/// pre-capture history into one time bucket.
fn build_hpts_hist_stack(
    events: &[HptsWheelEvent],
    t0_us: u64,
    extract: impl Fn(&HptsWheelPayload) -> Option<&[u64]>,
) -> GoodputStack {
    let mut samples: Vec<(u64, u64, &[u64])> = events
        .iter()
        .filter_map(|e| extract(&e.payload).map(|b| (e.ts, e.session_id, b)))
        .collect();
    samples.sort_by_key(|(ts, _, _)| *ts);

    let nbuckets = samples
        .iter()
        .map(|(_, _, b)| b.len())
        .max()
        .unwrap_or(0)
        .min(16);
    if nbuckets == 0 || samples.len() < 2 {
        return GoodputStack {
            nbuckets,
            times: Vec::new(),
            cum: Vec::new(),
            labels: Vec::new(),
        };
    }

    let mut last: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
    let mut global = vec![0u64; nbuckets];
    let mut times = Vec::with_capacity(samples.len());
    let mut cum = Vec::with_capacity(samples.len() * nbuckets);
    for (ts, session_id, buckets) in &samples {
        let first_sample_for_instance = !last.contains_key(session_id);
        let prev = last
            .entry(*session_id)
            .or_insert_with(|| vec![0u64; nbuckets]);
        if first_sample_for_instance {
            for (b, prev_b) in prev.iter_mut().enumerate() {
                *prev_b = buckets.get(b).copied().unwrap_or(0);
            }
        } else {
            for b in 0..nbuckets {
                let v = buckets.get(b).copied().unwrap_or(0);
                // Cumulative counters only grow; a drop is a reset, so
                // treat the new value itself as the delta.
                let delta = if v >= prev[b] { v - prev[b] } else { v };
                global[b] += delta;
                prev[b] = v;
            }
        }
        times.push(ts.saturating_sub(t0_us) as f64 / 1_000_000.0);
        cum.extend_from_slice(&global);
    }
    GoodputStack {
        nbuckets,
        times,
        cum,
        labels: Vec::new(),
    }
}

pub fn build_hpts_lateness_stack(events: &[HptsWheelEvent], t0_us: u64) -> GoodputStack {
    build_hpts_hist_stack(events, t0_us, |p| match p {
        HptsWheelPayload::LatenessSample { buckets } => Some(buckets.as_slice()),
        _ => None,
    })
}

pub fn build_hpts_batch_stack(events: &[HptsWheelEvent], t0_us: u64) -> GoodputStack {
    build_hpts_hist_stack(events, t0_us, |p| match p {
        HptsWheelPayload::TpBatchSample { buckets } => Some(buckets.as_slice()),
        _ => None,
    })
}

/// Machine-wide slot-gate series. Each point is one capture-relative
/// second. The kernel clears the counters after every sample, so a
/// sample's counts cover the span since that CPU's previous sample.
/// Rates use that span (1 s when an instance has no earlier sample).
/// Samples from the same instance in one second are averaged, then
/// summed across instances.
/// `rollover_max` is the deepest list on any CPU in that second.
/// `rollover_sum` adds each CPU's own peak from that second.
#[derive(Clone, Debug, Default)]
pub struct HptsSlotGateSeries {
    pub bytes_gbps: Vec<[f64; 2]>,
    pub fires: Vec<[f64; 2]>,
    pub diverts: Vec<[f64; 2]>,
    pub walks: Vec<[f64; 2]>,
    pub closed: Vec<[f64; 2]>,
    pub rollover_max: Vec<[f64; 2]>,
    pub rollover_sum: Vec<[f64; 2]>,
}

pub fn build_hpts_slot_gate_series(events: &[HptsWheelEvent], t0_us: u64) -> HptsSlotGateSeries {
    const DEFAULT_DT_US: u64 = 1_000_000;
    struct Acc {
        n: u32,
        fires: f64,
        diverts: f64,
        gbps: f64,
        walks: f64,
        closed: f64,
        rollover_max: f64,
    }
    let mut by_sid: std::collections::BTreeMap<u64, Vec<usize>> = std::collections::BTreeMap::new();
    for (i, e) in events.iter().enumerate() {
        if matches!(e.payload, HptsWheelPayload::SlotGateSample { .. }) {
            by_sid.entry(e.session_id).or_default().push(i);
        }
    }
    let mut by_sec: std::collections::BTreeMap<u64, std::collections::BTreeMap<u64, Acc>> =
        std::collections::BTreeMap::new();
    for (sid, mut idxs) in by_sid {
        idxs.sort_by_key(|&i| events[i].ts);
        let mut prev_ts: Option<u64> = None;
        for i in idxs {
            let e = &events[i];
            let HptsWheelPayload::SlotGateSample {
                fires,
                diverts,
                bytes,
                rollover_max,
                walks,
                closed,
                ..
            } = &e.payload
            else {
                continue;
            };
            let dt = prev_ts
                .map(|p| e.ts.saturating_sub(p))
                .filter(|d| *d > 0)
                .unwrap_or(DEFAULT_DT_US);
            prev_ts = Some(e.ts);
            let dt_s = dt as f64 / 1e6;
            let sec = e.ts.saturating_sub(t0_us) / 1_000_000;
            let acc = by_sec.entry(sec).or_default().entry(sid).or_insert(Acc {
                n: 0,
                fires: 0.0,
                diverts: 0.0,
                gbps: 0.0,
                walks: 0.0,
                closed: 0.0,
                rollover_max: 0.0,
            });
            acc.n += 1;
            acc.fires += f64::from(*fires) / dt_s;
            acc.diverts += f64::from(*diverts) / dt_s;
            acc.gbps += *bytes as f64 * 8.0 / dt as f64 / 1000.0;
            acc.walks += f64::from(*walks) / dt_s;
            acc.closed += f64::from(*closed) / dt_s;
            acc.rollover_max = acc.rollover_max.max(f64::from(*rollover_max));
        }
    }
    let mut out = HptsSlotGateSeries::default();
    for (sec, cpus) in by_sec {
        let t = sec as f64;
        let mut fires = 0.0;
        let mut diverts = 0.0;
        let mut gbps = 0.0;
        let mut walks = 0.0;
        let mut closed = 0.0;
        let mut rollover_max: f64 = 0.0;
        let mut rollover_sum: f64 = 0.0;
        for acc in cpus.values() {
            let n = f64::from(acc.n);
            fires += acc.fires / n;
            diverts += acc.diverts / n;
            gbps += acc.gbps / n;
            walks += acc.walks / n;
            closed += acc.closed / n;
            rollover_max = rollover_max.max(acc.rollover_max);
            rollover_sum += acc.rollover_max;
        }
        out.bytes_gbps.push([t, gbps]);
        out.fires.push([t, fires]);
        out.diverts.push([t, diverts]);
        out.walks.push([t, walks]);
        out.closed.push([t, closed]);
        out.rollover_max.push([t, rollover_max]);
        out.rollover_sum.push([t, rollover_sum]);
    }
    out
}

// ---------------------------------------------------------------------------
// STARS aggregate-sample (`hpts` provider) series
// ---------------------------------------------------------------------------

/// Derived STARS controller series, x in seconds relative to `t0_us`.
///
/// `pressure_pct` / `deny_pct` / `denied_mbps` are computed from the delta
/// between consecutive raw samples (`saturating_sub`, so a counter reset
/// clamps to a 0 delta instead of wrapping); an interval with a
/// zero-valued denominator is skipped rather than emitting a bogus point.
/// `slot_peak_kb` / `slot_budget_kb` are instantaneous gauges plotted
/// directly from each raw sample, not diffed.
pub struct StarsAggSeries {
    /// Mean throttle level the controller applied, 0..100%.
    pub pressure_pct: Vec<[f64; 2]>,
    /// Share of slot-gate admission checks that were denied, 0..100%.
    pub deny_pct: Vec<[f64; 2]>,
    /// Throughput denied by the slot gate, Mbps.
    pub denied_mbps: Vec<[f64; 2]>,
    /// Since-last-sample peak slot burst size, KB.
    pub slot_peak_kb: Vec<[f64; 2]>,
    /// Configured per-slot byte budget, KB.
    pub slot_budget_kb: Vec<[f64; 2]>,
    /// Config snapshot from the most recent sample (these rarely change
    /// within a capture).
    pub health_limit_bps: u64,
    pub min_rate_bps: u64,
    pub gate_enabled: bool,
}

pub fn build_stars_agg_series(samples: &[StarsAggSample], t0_us: u64) -> StarsAggSeries {
    let n = samples.len();
    let mut pressure_pct = Vec::with_capacity(n);
    let mut deny_pct = Vec::with_capacity(n);
    let mut denied_mbps = Vec::with_capacity(n);
    let mut slot_peak_kb = Vec::with_capacity(n);
    let mut slot_budget_kb = Vec::with_capacity(n);

    let mut prev: Option<&StarsAggSample> = None;
    for s in samples {
        let t = s.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        slot_peak_kb.push([t, s.slot_bytes_peak as f64 / 1024.0]);
        slot_budget_kb.push([t, s.slot_gate_budget_bytes as f64 / 1024.0]);

        if let Some(p) = prev {
            let d_pressure_sum = s.pressure_sum.saturating_sub(p.pressure_sum);
            let d_pressure_ticks = s.pressure_ticks.saturating_sub(p.pressure_ticks);
            if d_pressure_ticks > 0 {
                let pct = d_pressure_sum as f64 / d_pressure_ticks as f64 * (100.0 / 1024.0);
                pressure_pct.push([t, pct]);
            }

            let d_denies = s.slot_gate_denies.saturating_sub(p.slot_gate_denies);
            let d_admits = s.slot_gate_admits.saturating_sub(p.slot_gate_admits);
            let d_checks = d_denies + d_admits;
            if d_checks > 0 {
                deny_pct.push([t, d_denies as f64 / d_checks as f64 * 100.0]);
            }

            let dt_s = s.ts.saturating_sub(p.ts) as f64 / 1_000_000.0;
            if dt_s > 0.0 {
                let d_denied_bytes = s
                    .slot_gate_denied_bytes
                    .saturating_sub(p.slot_gate_denied_bytes);
                denied_mbps.push([t, d_denied_bytes as f64 * 8.0 / 1_000_000.0 / dt_s]);
            }
        }
        prev = Some(s);
    }

    let last = samples.last();
    StarsAggSeries {
        pressure_pct,
        deny_pct,
        denied_mbps,
        slot_peak_kb,
        slot_budget_kb,
        health_limit_bps: last.map(|s| s.health_limit_bps).unwrap_or(0),
        min_rate_bps: last.map(|s| s.min_rate_bps).unwrap_or(0),
        gate_enabled: last.map(|s| s.slot_gate_enable != 0).unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// ELLA `fanout_node` CHANNEL_COUNT / CHANNEL_COUNT_TOTAL series + per-channel occupancy
// ---------------------------------------------------------------------------

/// Above this many Fanout instances, System and channel plots aggregate
/// instead of drawing one line per instance.
pub const ELLA_PER_INSTANCE_PLOT_LIMIT: usize = 4;

/// One named time series (instance, side total, or process-wide gauge).
pub struct EllaNamedSeries {
    pub label: String,
    pub data: Vec<[f64; 2]>,
}

/// System-tab occupancy lines. Per-instance when `CHANNEL_COUNT` is
/// present and there are few Fanout instances; otherwise upstream vs
/// downstream. Downstream channels/pending keep both max-instance and
/// summed series so the GUI can toggle without rebuilding. When the
/// capture has only `CHANNEL_COUNT_TOTAL` (`STATS`), those process-wide
/// up/dn series are used instead.
#[derive(Default)]
pub struct EllaRelayPlotData {
    pub aggregated: bool,
    pub instance_count: usize,
    pub channels: Vec<EllaNamedSeries>,
    pub subscribers: Vec<EllaNamedSeries>,
    pub pending: Vec<EllaNamedSeries>,
    pub errors: Vec<EllaNamedSeries>,
    pub dn_channels_max: Vec<[f64; 2]>,
    pub dn_channels_sum: Vec<[f64; 2]>,
    pub dn_leave_max: Vec<[f64; 2]>,
    pub dn_leave_sum: Vec<[f64; 2]>,
    pub dn_retire_max: Vec<[f64; 2]>,
    pub dn_retire_sum: Vec<[f64; 2]>,
}

/// Process-wide CDN traffic / join rates from `CHANNEL_STATS` deltas.
#[derive(Default)]
pub struct EllaCdnMetricsPlotData {
    pub traffic: Vec<EllaNamedSeries>,
    pub joins: Vec<EllaNamedSeries>,
}

fn push_rate_point(series: &mut Vec<[f64; 2]>, t: f64, rate: f64) {
    series.push([t, rate]);
}

/// Sample-interval Mbps and join rates from `CHANNEL_STATS` for one node.
pub fn build_ella_cdn_metrics_plot_for(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
) -> EllaCdnMetricsPlotData {
    let mut up_mbps = Vec::new();
    let mut dn_mbps = Vec::new();
    let mut dlvr_mbps = Vec::new();
    let mut joins_up = Vec::new();
    let mut joins_dn = Vec::new();
    let mut joins_dlvr = Vec::new();
    let mut switches_dn = Vec::new();
    let mut leaves_dn = Vec::new();
    let mut expiries_dn = Vec::new();
    let mut rejects_dn = Vec::new();
    let mut new_clients = Vec::new();
    let mut prev_ts: Option<u64> = None;
    let mut saw_traffic = false;
    let mut saw_joins = false;
    for s in &prof.counts_total {
        if s.provider_id != provider_id {
            continue;
        }
        let t = s.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        let dt = match prev_ts {
            Some(p) => (s.ts.saturating_sub(p) as f64 / 1_000_000.0).max(1e-6),
            None => 1.0,
        };
        prev_ts = Some(s.ts);
        let up = s.bytes_up as f64 * 8.0 / 1_000_000.0 / dt;
        let dn = s.bytes_dn as f64 * 8.0 / 1_000_000.0 / dt;
        let dlvr = s.bytes_dlvr as f64 * 8.0 / 1_000_000.0 / dt;
        if s.bytes_up | s.bytes_dn | u64::from(s.packets_up | s.packets_dn) != 0 {
            saw_traffic = true;
        }
        // Keepalive delivered dumps sometimes carry control-sized
        // averages (~tens of bytes/packet); those are not DATA rates.
        let dlvr_avg = if s.packets_dlvr > 0 {
            s.bytes_dlvr as f64 / s.packets_dlvr as f64
        } else {
            0.0
        };
        if s.bytes_dlvr != 0 && dlvr_avg >= 200.0 {
            saw_traffic = true;
            push_rate_point(&mut dlvr_mbps, t, dlvr);
        } else if !dlvr_mbps.is_empty() {
            push_rate_point(&mut dlvr_mbps, t, 0.0);
        }
        push_rate_point(&mut up_mbps, t, up);
        push_rate_point(&mut dn_mbps, t, dn);
        let ju = s.joins_up as f64 / dt;
        let jd = s.joins_dn as f64 / dt;
        let jdl = s.joins_dlvr as f64 / dt;
        let sw = s.switches_dn as f64 / dt;
        let nc = s.new_clients as f64 / dt;
        if s.joins_up
            | s.joins_dn
            | s.joins_dlvr
            | s.switches_up
            | s.switches_dn
            | s.leaves_dn
            | s.expiries_dn
            | s.rejects_dn
            | s.new_clients
            != 0
        {
            saw_joins = true;
        }
        push_rate_point(&mut joins_up, t, ju);
        push_rate_point(&mut joins_dn, t, jd);
        push_rate_point(&mut joins_dlvr, t, jdl);
        push_rate_point(&mut switches_dn, t, sw);
        push_rate_point(&mut leaves_dn, t, s.leaves_dn as f64 / dt);
        push_rate_point(&mut expiries_dn, t, s.expiries_dn as f64 / dt);
        push_rate_point(&mut rejects_dn, t, s.rejects_dn as f64 / dt);
        push_rate_point(&mut new_clients, t, nc);
    }
    let mut traffic = Vec::new();
    if saw_traffic {
        if up_mbps.iter().any(|p| p[1] > 0.0) {
            traffic.push(EllaNamedSeries {
                label: "Upstream".into(),
                data: up_mbps,
            });
        }
        if dn_mbps.iter().any(|p| p[1] > 0.0) {
            traffic.push(EllaNamedSeries {
                label: "Downstream".into(),
                data: dn_mbps,
            });
        }
        if dlvr_mbps.iter().any(|p| p[1] > 0.0) {
            traffic.push(EllaNamedSeries {
                label: "Delivered".into(),
                data: dlvr_mbps,
            });
        }
    }
    let mut joins = Vec::new();
    if saw_joins {
        if joins_up.iter().any(|p| p[1] > 0.0) {
            joins.push(EllaNamedSeries {
                label: "Joins Up".into(),
                data: joins_up,
            });
        }
        if joins_dn.iter().any(|p| p[1] > 0.0) {
            joins.push(EllaNamedSeries {
                label: "Joins Down".into(),
                data: joins_dn,
            });
        }
        if joins_dlvr.iter().any(|p| p[1] > 0.0) {
            joins.push(EllaNamedSeries {
                label: "Joins Delivered".into(),
                data: joins_dlvr,
            });
        }
        if switches_dn.iter().any(|p| p[1] > 0.0) {
            joins.push(EllaNamedSeries {
                label: "Switches Down".into(),
                data: switches_dn,
            });
        }
        for (label, data) in [
            ("Leaves Down", leaves_dn),
            ("Expiries Down", expiries_dn),
            ("Rejects Down", rejects_dn),
        ] {
            if data.iter().any(|p| p[1] > 0.0) {
                joins.push(EllaNamedSeries {
                    label: label.into(),
                    data,
                });
            }
        }
        if new_clients.iter().any(|p| p[1] > 0.0) {
            joins.push(EllaNamedSeries {
                label: "New Clients".into(),
                data: new_clients,
            });
        }
    }
    EllaCdnMetricsPlotData { traffic, joins }
}

const OBJECT_TIMING_BIN_US: u64 = 1_000_000;

fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Origin `OBJECT_TIMING` medians (ms) in 1 s bins: start lateness and
/// signature / encode / enqueue durations.
pub fn build_ella_object_timing_plot(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
) -> Vec<EllaNamedSeries> {
    let mut late: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    let mut sign: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    let mut encode: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    let mut enqueue: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    for e in &prof.events {
        if e.provider_id != provider_id {
            continue;
        }
        let EllaPayload::ObjectTiming {
            start_lateness_us,
            signature_duration_us,
            encode_duration_us,
            enqueue_duration_us,
            ..
        } = &e.payload
        else {
            continue;
        };
        let bin = e.ts / OBJECT_TIMING_BIN_US;
        late.entry(bin)
            .or_default()
            .push(*start_lateness_us as f64 / 1000.0);
        if *signature_duration_us > 0 {
            sign.entry(bin)
                .or_default()
                .push(*signature_duration_us as f64 / 1000.0);
        }
        if *encode_duration_us > 0 {
            encode
                .entry(bin)
                .or_default()
                .push(*encode_duration_us as f64 / 1000.0);
        }
        if *enqueue_duration_us > 0 {
            enqueue
                .entry(bin)
                .or_default()
                .push(*enqueue_duration_us as f64 / 1000.0);
        }
    }
    fn median_series(
        bins: &std::collections::BTreeMap<u64, Vec<f64>>,
        t0_us: u64,
        label: &str,
    ) -> Option<EllaNamedSeries> {
        if bins.is_empty() {
            return None;
        }
        let mut data = Vec::new();
        for (&bin, vals) in bins {
            let mut sorted = vals.clone();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let t = (bin * OBJECT_TIMING_BIN_US).saturating_sub(t0_us) as f64 / 1_000_000.0;
            data.push([t, percentile_sorted(&sorted, 0.5)]);
        }
        Some(EllaNamedSeries {
            label: label.into(),
            data,
        })
    }
    let mut out = Vec::new();
    if let Some(s) = median_series(&late, t0_us, "Start late p50") {
        out.push(s);
    }
    // Also plot p95 start lateness when there is enough per-bin volume.
    if late.values().any(|v| v.len() >= 4) {
        let mut data = Vec::new();
        for (&bin, vals) in &late {
            let mut sorted = vals.clone();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let t = (bin * OBJECT_TIMING_BIN_US).saturating_sub(t0_us) as f64 / 1_000_000.0;
            data.push([t, percentile_sorted(&sorted, 0.95)]);
        }
        out.push(EllaNamedSeries {
            label: "Start late p95".into(),
            data,
        });
    }
    if let Some(s) = median_series(&sign, t0_us, "Signature p50") {
        out.push(s);
    }
    if let Some(s) = median_series(&encode, t0_us, "Encode p50") {
        out.push(s);
    }
    if let Some(s) = median_series(&enqueue, t0_us, "Enqueue p50") {
        out.push(s);
    }
    out
}

fn object_timing_channel_series(src: &EllaMemberSrc<'_>, t0_us: u64) -> Vec<EllaChannelSubSeries> {
    let named = {
        let mut late: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
        let mut encode: std::collections::BTreeMap<u64, Vec<f64>> =
            std::collections::BTreeMap::new();
        let mut enqueue: std::collections::BTreeMap<u64, Vec<f64>> =
            std::collections::BTreeMap::new();
        for e in &src.events {
            let EllaPayload::ObjectTiming {
                start_lateness_us,
                encode_duration_us,
                enqueue_duration_us,
                ..
            } = &e.payload
            else {
                continue;
            };
            let bin = e.ts / OBJECT_TIMING_BIN_US;
            late.entry(bin)
                .or_default()
                .push(*start_lateness_us as f64 / 1000.0);
            if *encode_duration_us > 0 {
                encode
                    .entry(bin)
                    .or_default()
                    .push(*encode_duration_us as f64 / 1000.0);
            }
            if *enqueue_duration_us > 0 {
                enqueue
                    .entry(bin)
                    .or_default()
                    .push(*enqueue_duration_us as f64 / 1000.0);
            }
        }
        let fold = |bins: &std::collections::BTreeMap<u64, Vec<f64>>, label: &str| {
            if bins.is_empty() {
                return None;
            }
            let mut data = Vec::new();
            for (&bin, vals) in bins {
                let mut sorted = vals.clone();
                sorted.sort_by(|a, b| a.total_cmp(b));
                let t = (bin * OBJECT_TIMING_BIN_US).saturating_sub(t0_us) as f64 / 1_000_000.0;
                data.push([t, percentile_sorted(&sorted, 0.5)]);
            }
            Some(EllaChannelSubSeries {
                label: label.into(),
                data,
            })
        };
        let mut out = Vec::new();
        if let Some(s) = fold(&late, "Start late p50") {
            out.push(s);
        }
        if let Some(s) = fold(&encode, "Encode p50") {
            out.push(s);
        }
        if let Some(s) = fold(&enqueue, "Enqueue p50") {
            out.push(s);
        }
        out
    };
    named
}

/// Capture-wide Fanout totals for the System-tab info popup.
#[derive(Default)]
pub struct EllaRelaySummary {
    pub upstream_instances: usize,
    pub downstream_instances: usize,
    pub titles: usize,
    pub channel_sessions: usize,
    pub peak_live_sessions: u32,
    pub peak_subscribers: u32,
    pub relay_errors: usize,
}

#[derive(Clone, Copy)]
struct EllaGauges {
    channels: u32,
    subscribers: u32,
    leave: u32,
    retire: u32,
}

fn ella_side(prof: &EllaProfile, provider_id: u16, instance_id: u32) -> u8 {
    prof.instance_side(provider_id, instance_id)
}

fn occupancy_side_label(prof: &EllaProfile, provider_id: u16, instance_id: u32) -> &'static str {
    if ella_side(prof, provider_id, instance_id) == ELLA_SIDE_UPSTREAM {
        "Internal"
    } else {
        "External"
    }
}

fn occupancy_instance_label(prof: &EllaProfile, provider_id: u16, instance_id: u32) -> String {
    format!(
        "{} i{instance_id}",
        occupancy_side_label(prof, provider_id, instance_id)
    )
}

fn pid_ok(filter: Option<u16>, provider_id: u16) -> bool {
    filter.map(|p| p == provider_id).unwrap_or(true)
}

struct OccSeries {
    inst_pts: FxHashMap<u32, Vec<[f64; 2]>>,
    total: Vec<[f64; 2]>,
    up: Vec<[f64; 2]>,
    dn: Vec<[f64; 2]>,
}

impl OccSeries {
    fn new() -> Self {
        Self {
            inst_pts: FxHashMap::default(),
            total: Vec::new(),
            up: Vec::new(),
            dn: Vec::new(),
        }
    }

    fn record(
        &mut self,
        t: f64,
        occupancy: &FxHashMap<u32, u32>,
        inst: u32,
        prof: &EllaProfile,
        provider_id: u16,
    ) {
        let v = occupancy.get(&inst).copied().unwrap_or(0) as f64;
        push_step_changed(self.inst_pts.entry(inst).or_default(), t, v);
        let tot: u32 = occupancy.values().copied().sum();
        push_step_changed(&mut self.total, t, tot as f64);
        let up: u32 = occupancy
            .iter()
            .filter(|(id, _)| ella_side(prof, provider_id, **id) == ELLA_SIDE_UPSTREAM)
            .map(|(_, n)| *n)
            .sum();
        let dn: u32 = occupancy
            .iter()
            .filter(|(id, _)| ella_side(prof, provider_id, **id) == ELLA_SIDE_DOWNSTREAM)
            .map(|(_, n)| *n)
            .sum();
        push_step_changed(&mut self.up, t, up as f64);
        push_step_changed(&mut self.dn, t, dn as f64);
    }

    fn extend_to(&mut self, t_last: f64) {
        for series in self.inst_pts.values_mut() {
            extend_step_series(series, t_last);
        }
        extend_step_series(&mut self.total, t_last);
        extend_step_series(&mut self.up, t_last);
        extend_step_series(&mut self.dn, t_last);
    }

    fn into_series(
        self,
        prof: &EllaProfile,
        provider_id: u16,
        combined_label: &'static str,
    ) -> Vec<EllaChannelSubSeries> {
        collect_occupancy_series(
            prof,
            provider_id,
            self.inst_pts,
            self.up,
            self.dn,
            self.total,
            combined_label,
        )
    }
}

fn collect_occupancy_series(
    prof: &EllaProfile,
    provider_id: u16,
    mut inst_pts: FxHashMap<u32, Vec<[f64; 2]>>,
    up_pts: Vec<[f64; 2]>,
    dn_pts: Vec<[f64; 2]>,
    total: Vec<[f64; 2]>,
    combined_label: &'static str,
) -> Vec<EllaChannelSubSeries> {
    let n_inst = inst_pts.len();
    if n_inst <= ELLA_PER_INSTANCE_PLOT_LIMIT {
        let mut keys: Vec<u32> = inst_pts.keys().copied().collect();
        keys.sort_unstable();
        // `i<N>` earns its place once a side runs more than one instance
        // on this node; a lone instance reads as just its side.
        let mut per_side: FxHashMap<&str, usize> = FxHashMap::default();
        for &id in &keys {
            *per_side
                .entry(occupancy_side_label(prof, provider_id, id))
                .or_insert(0) += 1;
        }
        keys.into_iter()
            .map(|id| {
                let side = occupancy_side_label(prof, provider_id, id);
                EllaChannelSubSeries {
                    label: if per_side[side] > 1 {
                        format!("{side} i{id}")
                    } else {
                        side.to_string()
                    },
                    data: inst_pts.remove(&id).unwrap_or_default(),
                }
            })
            .collect()
    } else {
        let mut out = Vec::new();
        let up_live = up_pts.iter().any(|p| p[1] > 0.0);
        let dn_live = dn_pts.iter().any(|p| p[1] > 0.0);
        if up_live {
            out.push(EllaChannelSubSeries {
                label: "Internal".into(),
                data: up_pts,
            });
        }
        if dn_live {
            out.push(EllaChannelSubSeries {
                label: "External".into(),
                data: dn_pts,
            });
        }
        if out.is_empty() && !total.is_empty() {
            out.push(EllaChannelSubSeries {
                label: combined_label.into(),
                data: total,
            });
        }
        out
    }
}

fn median_count_period_us(prof: &EllaProfile, provider_id: Option<u16>) -> u64 {
    let mut by_inst: FxHashMap<u32, Vec<u64>> = FxHashMap::default();
    for s in &prof.counts {
        if !pid_ok(provider_id, s.provider_id) {
            continue;
        }
        by_inst.entry(s.instance_id).or_default().push(s.ts);
    }
    let mut dts = Vec::new();
    for ts in by_inst.values_mut() {
        ts.sort_unstable();
        dts.extend(
            ts.windows(2)
                .map(|w| w[1].saturating_sub(w[0]))
                .filter(|d| *d > 0),
        );
    }
    if dts.is_empty() {
        return 100_000;
    }
    dts.sort_unstable();
    dts[dts.len() / 2].clamp(50_000, 1_000_000)
}

fn extend_step_series(series: &mut Vec<[f64; 2]>, t_last: f64) {
    if let Some(last) = series.last().copied() {
        if last[0] < t_last {
            series.push([t_last, last[1]]);
        }
    }
}

fn error_series_label(is_channel: bool, code: u8) -> String {
    if is_channel {
        match ella_error_code_name(code) {
            "?" => format!("channel {code}"),
            "UNKNOWN" => "CHANNEL UNKNOWN".into(),
            s => s.to_string(),
        }
    } else {
        match ella_relay_error_code_name(code) {
            "?" => format!("node {code}"),
            s => s.to_string(),
        }
    }
}

#[derive(Default)]
struct ErrorAcc {
    n: u32,
    pts: Vec<[f64; 2]>,
}

fn named_error_series(
    mut by_code: FxHashMap<u8, ErrorAcc>,
    is_channel: bool,
) -> Vec<EllaNamedSeries> {
    let mut codes: Vec<u8> = by_code.keys().copied().collect();
    codes.sort_unstable();
    codes
        .into_iter()
        .map(|code| EllaNamedSeries {
            label: error_series_label(is_channel, code),
            data: by_code.remove(&code).unwrap().pts,
        })
        .collect()
}

fn error_series(prof: &EllaProfile, provider_id: Option<u16>, t0_us: u64) -> Vec<EllaNamedSeries> {
    let mut relay: FxHashMap<u8, ErrorAcc> = FxHashMap::default();
    let mut channel: FxHashMap<u8, ErrorAcc> = FxHashMap::default();
    for e in &prof.events {
        if !pid_ok(provider_id, e.provider_id) {
            continue;
        }
        let (map, code) = match e.payload {
            EllaPayload::RelayError { code, .. } => (&mut relay, code),
            EllaPayload::ChannelError { code, .. } => (&mut channel, code),
            _ => continue,
        };
        let t = e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        let acc = map.entry(code).or_default();
        acc.n += 1;
        push_step(&mut acc.pts, t, acc.n as f64);
    }
    let mut out = named_error_series(relay, false);
    out.extend(named_error_series(channel, true));
    out
}

fn per_instance_relay_plot(
    prof: &EllaProfile,
    provider_id: Option<u16>,
    t0_us: u64,
) -> EllaRelayPlotData {
    let mut by_inst: FxHashMap<(u16, u32), Vec<&EllaChannelCount>> = FxHashMap::default();
    for s in &prof.counts {
        if !pid_ok(provider_id, s.provider_id) {
            continue;
        }
        by_inst
            .entry((s.provider_id, s.instance_id))
            .or_default()
            .push(s);
    }
    let mut keys: Vec<(u16, u32)> = by_inst.keys().copied().collect();
    keys.sort_unstable();
    let mut channels = Vec::with_capacity(keys.len());
    let mut subscribers = Vec::with_capacity(keys.len());
    let mut pending = Vec::with_capacity(keys.len() * 2);
    for key in keys {
        let samples = by_inst.get_mut(&key).unwrap();
        samples.sort_by_key(|s| s.ts);
        let first = samples[0];
        let face = if ella_side(prof, first.provider_id, first.instance_id) == ELLA_SIDE_UPSTREAM {
            "upstream"
        } else {
            "downstream"
        };
        let label = format!("{face} i{}", first.instance_id);
        let mut ch = Vec::with_capacity(samples.len());
        let mut sub = Vec::with_capacity(samples.len());
        let mut leave = Vec::with_capacity(samples.len());
        let mut retire = Vec::with_capacity(samples.len());
        for s in samples.iter() {
            let t = s.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
            ch.push([t, s.channels as f64]);
            sub.push([t, s.subscribers as f64]);
            leave.push([t, s.channels_pending_leave as f64]);
            retire.push([t, s.channels_pending_retire as f64]);
        }
        channels.push(EllaNamedSeries {
            label: format!("{label} Channels"),
            data: ch,
        });
        subscribers.push(EllaNamedSeries {
            label: format!("{label} Subscribers"),
            data: sub,
        });
        pending.push(EllaNamedSeries {
            label: format!("{label} Leave"),
            data: leave,
        });
        pending.push(EllaNamedSeries {
            label: format!("{label} Retire"),
            data: retire,
        });
    }
    let n = subscribers.len();
    EllaRelayPlotData {
        aggregated: false,
        instance_count: n,
        channels,
        subscribers,
        pending,
        errors: error_series(prof, provider_id, t0_us),
        ..Default::default()
    }
}

fn aggregated_relay_plot(
    prof: &EllaProfile,
    provider_id: Option<u16>,
    t0_us: u64,
) -> EllaRelayPlotData {
    let bin_us = median_count_period_us(prof, provider_id);
    let t_min = prof
        .counts
        .iter()
        .filter(|s| pid_ok(provider_id, s.provider_id))
        .map(|s| s.ts)
        .min()
        .unwrap_or(t0_us);
    let mut last_in_bin: FxHashMap<(u64, u16, u32), EllaGauges> = FxHashMap::default();
    let mut inst_ids: FxHashMap<(u16, u32), ()> = FxHashMap::default();
    for s in &prof.counts {
        if !pid_ok(provider_id, s.provider_id) {
            continue;
        }
        let bin = s.ts.saturating_sub(t_min) / bin_us;
        inst_ids.insert((s.provider_id, s.instance_id), ());
        last_in_bin.insert(
            (bin, s.provider_id, s.instance_id),
            EllaGauges {
                channels: s.channels,
                subscribers: s.subscribers,
                leave: s.channels_pending_leave,
                retire: s.channels_pending_retire,
            },
        );
    }
    let mut bins: Vec<u64> = last_in_bin.keys().map(|(b, _, _)| *b).collect();
    bins.sort_unstable();
    bins.dedup();
    let n_inst = inst_ids.len();
    let mut last: FxHashMap<(u16, u32), EllaGauges> = FxHashMap::default();
    let mut up_ch = Vec::new();
    let mut up_sub = Vec::new();
    let mut dn_sub = Vec::new();
    let mut up_leave = Vec::new();
    let mut up_retire = Vec::new();
    let mut dn_ch_max = Vec::new();
    let mut dn_ch_sum = Vec::new();
    let mut dn_leave_max = Vec::new();
    let mut dn_leave_sum = Vec::new();
    let mut dn_retire_max = Vec::new();
    let mut dn_retire_sum = Vec::new();
    for bin in bins {
        for key in inst_ids.keys() {
            if let Some(g) = last_in_bin.get(&(bin, key.0, key.1)) {
                last.insert(*key, *g);
            }
        }
        let t = t_min.saturating_add(bin.saturating_add(1).saturating_mul(bin_us));
        let t_s = t.saturating_sub(t0_us) as f64 / 1_000_000.0;
        let mut u_ch = 0u32;
        let mut u_sub = 0u32;
        let mut u_leave = 0u32;
        let mut u_retire = 0u32;
        let mut d_ch_max = 0u32;
        let mut d_ch_sum = 0u32;
        let mut d_sub = 0u32;
        let mut d_leave_max = 0u32;
        let mut d_leave_sum = 0u32;
        let mut d_retire_max = 0u32;
        let mut d_retire_sum = 0u32;
        let mut saw_up = false;
        let mut saw_dn = false;
        for ((pid, inst), g) in &last {
            if ella_side(prof, *pid, *inst) == ELLA_SIDE_UPSTREAM {
                saw_up = true;
                u_ch = u_ch.saturating_add(g.channels);
                u_sub = u_sub.saturating_add(g.subscribers);
                u_leave = u_leave.saturating_add(g.leave);
                u_retire = u_retire.saturating_add(g.retire);
            } else {
                saw_dn = true;
                d_ch_max = d_ch_max.max(g.channels);
                d_ch_sum = d_ch_sum.saturating_add(g.channels);
                d_sub = d_sub.saturating_add(g.subscribers);
                d_leave_max = d_leave_max.max(g.leave);
                d_leave_sum = d_leave_sum.saturating_add(g.leave);
                d_retire_max = d_retire_max.max(g.retire);
                d_retire_sum = d_retire_sum.saturating_add(g.retire);
            }
        }
        if saw_up {
            push_step(&mut up_ch, t_s, u_ch as f64);
            push_step(&mut up_sub, t_s, u_sub as f64);
            push_step(&mut up_leave, t_s, u_leave as f64);
            push_step(&mut up_retire, t_s, u_retire as f64);
        }
        if saw_dn {
            push_step(&mut dn_ch_max, t_s, d_ch_max as f64);
            push_step(&mut dn_ch_sum, t_s, d_ch_sum as f64);
            push_step(&mut dn_sub, t_s, d_sub as f64);
            push_step(&mut dn_leave_max, t_s, d_leave_max as f64);
            push_step(&mut dn_leave_sum, t_s, d_leave_sum as f64);
            push_step(&mut dn_retire_max, t_s, d_retire_max as f64);
            push_step(&mut dn_retire_sum, t_s, d_retire_sum as f64);
        }
    }
    let mut channels = Vec::new();
    let mut subscribers = Vec::new();
    let mut pending = Vec::new();
    if !up_ch.is_empty() {
        channels.push(EllaNamedSeries {
            label: "Upstream".into(),
            data: up_ch,
        });
        subscribers.push(EllaNamedSeries {
            label: "Internal".into(),
            data: up_sub,
        });
        pending.push(EllaNamedSeries {
            label: "Upstream Leave".into(),
            data: up_leave,
        });
        pending.push(EllaNamedSeries {
            label: "Upstream Retire".into(),
            data: up_retire,
        });
    }
    if !dn_sub.is_empty() {
        subscribers.push(EllaNamedSeries {
            label: "External".into(),
            data: dn_sub,
        });
    }
    EllaRelayPlotData {
        aggregated: true,
        instance_count: n_inst,
        channels,
        subscribers,
        pending,
        errors: error_series(prof, provider_id, t0_us),
        dn_channels_max: dn_ch_max,
        dn_channels_sum: dn_ch_sum,
        dn_leave_max,
        dn_leave_sum,
        dn_retire_max,
        dn_retire_sum,
    }
}

/// Occupancy series for one `fanout_node` provider.
pub fn build_ella_relay_plot_for(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
) -> EllaRelayPlotData {
    build_ella_relay_plot_filtered(prof, Some(provider_id), t0_us)
}

/// Group `CHANNEL_COUNT` into System-tab series. Prefer per-instance
/// `CHANNEL_COUNT` when any samples exist; otherwise plot
/// `CHANNEL_COUNT_TOTAL`. Single-provider captures pass through as
/// one plot; callers with two providers use [`build_ella_relay_plot_for`].
pub fn build_ella_relay_plot(prof: &EllaProfile, t0_us: u64) -> EllaRelayPlotData {
    let ids = prof.fanout_provider_ids();
    if ids.len() == 1 {
        build_ella_relay_plot_filtered(prof, Some(ids[0]), t0_us)
    } else {
        build_ella_relay_plot_filtered(prof, None, t0_us)
    }
}

fn build_ella_relay_plot_filtered(
    prof: &EllaProfile,
    provider_id: Option<u16>,
    t0_us: u64,
) -> EllaRelayPlotData {
    let n_count = prof
        .counts
        .iter()
        .filter(|c| pid_ok(provider_id, c.provider_id))
        .count();
    if n_count > 0 {
        let n_inst = {
            let mut s = FxHashMap::<(u16, u32), ()>::default();
            for c in &prof.counts {
                if pid_ok(provider_id, c.provider_id) {
                    s.insert((c.provider_id, c.instance_id), ());
                }
            }
            s.len()
        };
        if n_inst <= ELLA_PER_INSTANCE_PLOT_LIMIT {
            per_instance_relay_plot(prof, provider_id, t0_us)
        } else {
            aggregated_relay_plot(prof, provider_id, t0_us)
        }
    } else if prof
        .counts_total
        .iter()
        .any(|s| pid_ok(provider_id, s.provider_id))
    {
        process_total_relay_plot(prof, provider_id, t0_us)
    } else {
        EllaRelayPlotData {
            errors: error_series(prof, provider_id, t0_us),
            ..Default::default()
        }
    }
}

fn process_total_relay_plot(
    prof: &EllaProfile,
    provider_id: Option<u16>,
    t0_us: u64,
) -> EllaRelayPlotData {
    let mut up_ch = Vec::new();
    let mut up_sub = Vec::new();
    let mut up_leave = Vec::new();
    let mut up_retire = Vec::new();
    let mut dn_sub = Vec::new();
    let mut dn_leave = Vec::new();
    let mut dn_retire = Vec::new();
    let mut dn_ch = Vec::new();
    for s in &prof.counts_total {
        if !pid_ok(provider_id, s.provider_id) {
            continue;
        }
        let t = s.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        push_step(&mut up_ch, t, s.channels_up as f64);
        push_step(&mut up_sub, t, s.subscribers_up as f64);
        push_step(&mut up_leave, t, s.channels_pending_leave_up as f64);
        push_step(&mut up_retire, t, s.channels_pending_retire_up as f64);
        push_step(&mut dn_sub, t, s.subscribers_dn as f64);
        push_step(&mut dn_leave, t, s.channels_pending_leave_dn as f64);
        push_step(&mut dn_retire, t, s.channels_pending_retire_dn as f64);
        push_step(&mut dn_ch, t, s.channels_dn as f64);
    }
    let mut channels = Vec::new();
    let mut subscribers = Vec::new();
    let mut pending = Vec::new();
    if !up_ch.is_empty() {
        channels.push(EllaNamedSeries {
            label: "Upstream".into(),
            data: up_ch,
        });
        subscribers.push(EllaNamedSeries {
            label: "Internal".into(),
            data: up_sub,
        });
        pending.push(EllaNamedSeries {
            label: "Upstream Leave".into(),
            data: up_leave,
        });
        pending.push(EllaNamedSeries {
            label: "Upstream Retire".into(),
            data: up_retire,
        });
    }
    if !dn_sub.is_empty() || !dn_ch.is_empty() {
        subscribers.push(EllaNamedSeries {
            label: "External".into(),
            data: dn_sub,
        });
    }
    EllaRelayPlotData {
        aggregated: true,
        instance_count: prof
            .instances
            .keys()
            .filter(|(pid, _)| pid_ok(provider_id, *pid))
            .count(),
        channels,
        subscribers,
        pending,
        errors: error_series(prof, provider_id, t0_us),
        dn_channels_sum: dn_ch,
        dn_leave_sum: dn_leave,
        dn_retire_sum: dn_retire,
        ..Default::default()
    }
}

/// Capture-wide Fanout totals (instance counts, titles, peaks).
pub fn ella_relay_summary(prof: &EllaProfile) -> EllaRelaySummary {
    let mut upstream_instances = 0usize;
    let mut downstream_instances = 0usize;
    for side in prof.instances.values().copied() {
        if side == ELLA_SIDE_UPSTREAM {
            upstream_instances += 1;
        } else {
            downstream_instances += 1;
        }
    }
    let mut titles: FxHashSet<String> = FxHashSet::default();
    for c in prof.channels.values() {
        titles.insert(ella_channel_title(c));
    }
    EllaRelaySummary {
        upstream_instances,
        downstream_instances,
        titles: titles.len(),
        channel_sessions: prof.channels.len(),
        peak_live_sessions: prof.counts.iter().map(|s| s.sessions).max().unwrap_or(0),
        peak_subscribers: prof
            .channels
            .values()
            .map(|c| c.peak_subscribers)
            .max()
            .unwrap_or(0),
        relay_errors: prof
            .events
            .iter()
            .filter(|e| matches!(e.payload, EllaPayload::RelayError { .. }))
            .count(),
    }
}

/// One occupancy line on a channel tab.
#[derive(Clone)]
pub struct EllaChannelSubSeries {
    pub label: String,
    pub data: Vec<[f64; 2]>,
}

/// Lifecycle / error marker on the occupancy plot.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EllaChannelMarkerKind {
    Requested,
    Created,
    Destroyed,
    Error,
}

pub struct EllaChannelMarker {
    pub t: f64,
    pub y: f64,
    pub kind: EllaChannelMarkerKind,
}

/// How an object span ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EllaObjectSpanState {
    Completed,
    Abandoned,
    /// STARTED but no terminal event before the session's last event.
    Open,
}

/// One object's STARTED→COMPLETED span on one instance of one node.
/// `obj_idx` ranks `first_pn` across the tab so Server and Relay copies
/// of a GOP share a row. Duplicate spans on the same instance that
/// match to ~25 ms merge (`n` > 1).
#[derive(Clone)]
pub struct EllaObjectSpan {
    pub provider_id: u16,
    pub instance_id: u32,
    pub t0: f64,
    pub t1: f64,
    pub obj_idx: u32,
    pub first_pn: u32,
    /// Object slot (`OBJECT_STATE`), rotating 0-3 per GOP. Drop-robust
    /// stripe parity, unlike the time-ranked `obj_idx`.
    pub boundary_index: u8,
    /// From `FRAME_SENT` (`first_pn` match), on any node of the tab.
    pub object_id: Option<u32>,
    pub packets: u32,
    pub bytes: u32,
    pub n: u32,
    pub state: EllaObjectSpanState,
}

/// One FDACE frame on Frame Timeline (Actual send or Recv window).
#[derive(Clone)]
pub struct EllaFrameSpan {
    pub provider_id: u16,
    pub instance_id: u32,
    pub t0: f64,
    pub t1: f64,
    pub object_id: u32,
    pub obj_idx: u32,
    pub frame_index: u32,
    pub first_pn: u32,
    /// Object slot (`FRAME_RELAY`, or the joined object on the origin),
    /// rotating 0-3 per GOP. Drop-robust stripe parity.
    pub boundary_index: u8,
    /// FDACE toggle bit, alternating per frame. Drop-robust shade parity.
    pub toggle: bool,
    /// `frame_len` (FDACE-adjusted bytes).
    pub bytes: u32,
}

/// Per-channel subscriber occupancy, in-flight objects, object RX/TX rate,
/// errors, and lifecycle markers.
#[derive(Default)]
pub struct EllaChannelPlotData {
    pub subscribers: Vec<EllaChannelSubSeries>,
    pub objects: Vec<EllaChannelSubSeries>,
    /// Mbps of the same GOP as Object Timeline (bytes / STARTED→COMPLETED).
    /// `CHANNEL_SAMPLE` `object_bytes` per tick when `OBJECT_STATE` is absent.
    /// Upstream is RX, downstream TX.
    pub object_rate: Vec<EllaChannelSubSeries>,
    /// Origin `OBJECT_TIMING` medians (ms) when that keyword was captured.
    pub object_timing: Vec<EllaChannelSubSeries>,
    /// Mbps from `FRAME_SENT` / `FRAME_RELAY`, or `FRAME_SAMPLE` when
    /// those events are absent. Origin planned vs actual send; relay
    /// wire / recv / fwd. Upstream is RX, downstream TX.
    pub frame_rate: Vec<EllaChannelSubSeries>,
    /// Mean per-frame duration in milliseconds from the same sources as
    /// `frame_rate` (bytes unused; y is `sum_us / frames`).
    pub frame_duration: Vec<EllaChannelSubSeries>,
    /// Duration-ratio percentiles (Median/P75/P95/Max), RX/TX-prefixed
    /// per side: server `FRAME_SENT` actual/planned send, relay
    /// `FRAME_RELAY` recv/wire. Drawn in 250 visible-window bins.
    /// `FRAME_SAMPLE` 2 s aggregate when detailed events are absent.
    pub frame_ratio: Vec<EllaChannelSubSeries>,
    /// True when `frame_ratio` is the `FRAME_SAMPLE` aggregate fallback.
    pub frame_ratio_fallback: bool,
    /// `FRAME_SENT` object id of the live GOP on the node.
    pub object_id: Vec<EllaChannelSubSeries>,
    /// STARTED until the next object's first packet. Server and Internal
    /// are that face's copies; External is Median/P75/P95/Max of workers.
    pub object_duration: Vec<EllaChannelSubSeries>,
    /// Payload MB of that GOP. Same Server / Internal / External split.
    pub object_size: Vec<EllaChannelSubSeries>,
    /// Object Timeline spans: Server, Internal, and one External instance.
    pub object_spans: Vec<EllaObjectSpan>,
    /// Every instance's STARTED→COMPLETED copy, after GOP merge.
    pub object_span_copies: Vec<EllaObjectSpan>,
    /// `FRAME_SENT` / `FRAME_RELAY` bars for Frame Timeline.
    pub frame_spans: Vec<EllaFrameSpan>,
    pub errors: Vec<EllaNamedSeries>,
    pub markers: Vec<EllaChannelMarker>,
}

fn push_step(series: &mut Vec<[f64; 2]>, t: f64, y: f64) {
    if let Some(last) = series.last().copied() {
        if last[0] < t {
            series.push([t, last[1]]);
        }
    }
    series.push([t, y]);
}

/// Extend a step series only where its value moves. The total / up / dn
/// accumulators are offered a sample at every instance's event, so a
/// side with one instance would otherwise inherit the timestamps of
/// every other instance — enough points to push the series past
/// `MAX_DISPLAY_POINTS` and get it mean-downsampled into ramps.
fn push_step_changed(series: &mut Vec<[f64; 2]>, t: f64, y: f64) {
    if series.last().is_some_and(|last| last[1] == y) {
        return;
    }
    push_step(series, t, y);
}

fn marker_bucket(t: f64) -> i64 {
    (t * 1000.0).round() as i64
}

/// Events and samples for one envelope session, gathered in one pass
/// over the capture so a title with many reincarnations does not scan
/// `prof.events` once per member.
struct EllaMemberSrc<'a> {
    events: Vec<&'a EllaEvent>,
    samples: Vec<&'a EllaChannelSample>,
    frames: Vec<&'a EllaFrameSample>,
}

fn ella_member_sources<'a>(
    prof: &'a EllaProfile,
    members: &[(u16, u64)],
) -> FxHashMap<(u16, u64), EllaMemberSrc<'a>> {
    let mut out: FxHashMap<(u16, u64), EllaMemberSrc<'a>> = FxHashMap::default();
    for &k in members {
        out.entry(k).or_insert_with(|| EllaMemberSrc {
            events: Vec::new(),
            samples: Vec::new(),
            frames: Vec::new(),
        });
    }
    if out.is_empty() {
        return out;
    }
    for e in &prof.events {
        if let Some(m) = out.get_mut(&(e.provider_id, e.session_id)) {
            m.events.push(e);
        }
    }
    for s in &prof.channel_samples {
        if let Some(m) = out.get_mut(&(s.provider_id, s.session_id)) {
            m.samples.push(s);
        }
    }
    for s in &prof.frame_samples {
        if let Some(m) = out.get_mut(&(s.provider_id, s.session_id)) {
            m.frames.push(s);
        }
    }
    out
}

fn channel_error_series(events: &[&EllaEvent], t0_us: u64) -> Vec<EllaNamedSeries> {
    let mut by_code: FxHashMap<u8, ErrorAcc> = FxHashMap::default();
    for e in events {
        let EllaPayload::ChannelError { code, .. } = &e.payload else {
            continue;
        };
        let t = e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        let acc = by_code.entry(*code).or_default();
        acc.n += 1;
        push_step(&mut acc.pts, t, acc.n as f64);
    }
    named_error_series(by_code, true)
}

pub fn build_ella_channel_plot(
    prof: &EllaProfile,
    provider_id: u16,
    session_id: u64,
    t0_us: u64,
) -> EllaChannelPlotData {
    let members = [(provider_id, session_id)];
    let srcs = ella_member_sources(prof, &members);
    build_ella_channel_plot_from(
        prof,
        provider_id,
        t0_us,
        srcs.get(&(provider_id, session_id)).expect("member src"),
    )
}

fn build_ella_channel_plot_from(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
    src: &EllaMemberSrc<'_>,
) -> EllaChannelPlotData {
    // Occupancy from CHANNEL_SAMPLE when this session has samples
    // (same gauge as System Per channel / Peak). Otherwise unique
    // SUBSCRIBED−UNSUBSCRIBED per (instance, address, port).
    // SUBSCRIBING holds 0 until the first SUBSCRIBED. Objects are 1
    // from STARTED until COMPLETED / ABANDONED (or that instance's
    // CHANNEL_DESTROYED).
    let mut occupancy: FxHashMap<u32, u32> = FxHashMap::default();
    let mut subs = OccSeries::new();
    let mut obj_occupancy: FxHashMap<u32, u32> = FxHashMap::default();
    let mut objs = OccSeries::new();
    let mut markers = Vec::new();
    let mut seen_marker: FxHashMap<(u8, i64), ()> = FxHashMap::default();
    let mut t_last = 0.0_f64;
    let mut obj_open: FxHashMap<u32, (f64, u32, u8)> = FxHashMap::default();
    let mut object_spans: Vec<EllaObjectSpan> = Vec::new();
    let mut raw_frames: Vec<RawFrame> = Vec::new();
    let mut obj_ids: FxHashMap<u32, u32> = FxHashMap::default();
    let sampled = !src.samples.is_empty();
    let mut live_subs: FxHashSet<(u32, String, u16)> = FxHashSet::default();
    let mut live_joins: FxHashSet<(u32, u32)> = FxHashSet::default();

    let mut push_marker = |t: f64, y: f64, kind: EllaChannelMarkerKind| {
        let tag = match kind {
            EllaChannelMarkerKind::Requested => 0u8,
            EllaChannelMarkerKind::Created => 1,
            EllaChannelMarkerKind::Destroyed => 2,
            EllaChannelMarkerKind::Error => 3,
        };
        let b = marker_bucket(t);
        if seen_marker.insert((tag, b), ()).is_some() {
            return;
        }
        markers.push(EllaChannelMarker { t, y, kind });
    };

    if sampled {
        let mut ops: Vec<(u64, u32, u32)> = src
            .samples
            .iter()
            .map(|s| (s.ts, s.instance_id, s.subscribers))
            .collect();
        for e in &src.events {
            if let EllaPayload::ChannelDestroyed { instance_id, .. } = &e.payload {
                ops.push((e.ts, *instance_id, 0));
            }
        }
        ops.sort_unstable_by_key(|op| op.0);
        for (ts, inst, n) in ops {
            let t = ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
            t_last = t_last.max(t);
            occupancy.insert(inst, n);
            subs.record(t, &occupancy, inst, prof, provider_id);
        }
    }
    for e in &src.events {
        let t = e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        t_last = t_last.max(t);
        let tot_now: u32 = occupancy.values().copied().sum();
        match &e.payload {
            EllaPayload::ChannelState { state: st, .. } => {
                let kind = match *st {
                    ELLA_LIFECYCLE_REQUESTED => Some(EllaChannelMarkerKind::Requested),
                    ELLA_LIFECYCLE_CREATED => Some(EllaChannelMarkerKind::Created),
                    _ => None,
                };
                if let Some(kind) = kind {
                    push_marker(t, tot_now as f64, kind);
                }
            }
            EllaPayload::ChannelSubscriber {
                state: st,
                instance_id,
                subscriber_address,
                subscriber_port,
                ..
            } if !sampled => {
                let key = (*instance_id, subscriber_address.clone(), *subscriber_port);
                match *st {
                    ELLA_SUBSCRIBER_SUBSCRIBED => {
                        if !live_subs.insert(key) {
                            continue;
                        }
                        *occupancy.entry(*instance_id).or_insert(0) += 1;
                    }
                    ELLA_SUBSCRIBER_UNSUBSCRIBED => {
                        if !live_subs.remove(&key) {
                            continue;
                        }
                        let n = occupancy.entry(*instance_id).or_insert(0);
                        *n = n.saturating_sub(1);
                    }
                    ELLA_SUBSCRIBER_SUBSCRIBING => continue,
                    _ => continue,
                }
                subs.record(t, &occupancy, *instance_id, prof, provider_id);
            }
            EllaPayload::SubscriberJoin {
                instance_id,
                sub_id,
                ..
            } if !sampled => {
                if !live_joins.insert((*instance_id, *sub_id)) {
                    continue;
                }
                *occupancy.entry(*instance_id).or_insert(0) += 1;
                subs.record(t, &occupancy, *instance_id, prof, provider_id);
            }
            EllaPayload::SubscriberLeave {
                instance_id,
                sub_id,
                ..
            } if !sampled => {
                if !live_joins.remove(&(*instance_id, *sub_id)) {
                    continue;
                }
                let n = occupancy.entry(*instance_id).or_insert(0);
                *n = n.saturating_sub(1);
                subs.record(t, &occupancy, *instance_id, prof, provider_id);
            }
            EllaPayload::ObjectState {
                state: st,
                instance_id,
                boundary_index,
                first_packet,
                packets,
                bytes,
            } => {
                match *st {
                    ELLA_OBJECT_STARTED => {
                        if let Some((t0, first_pn, slot)) =
                            obj_open.insert(*instance_id, (t, *first_packet, *boundary_index))
                        {
                            push_object_span(
                                &mut object_spans,
                                EllaObjectSpan {
                                    provider_id,
                                    instance_id: *instance_id,
                                    t0,
                                    t1: t,
                                    obj_idx: 0,
                                    first_pn,
                                    boundary_index: slot,
                                    object_id: None,
                                    packets: 0,
                                    bytes: 0,
                                    n: 1,
                                    state: EllaObjectSpanState::Open,
                                },
                            );
                        }
                    }
                    ELLA_OBJECT_COMPLETED | ELLA_OBJECT_ABANDONED => {
                        if let Some((t0, first_pn, slot)) = obj_open.remove(instance_id) {
                            push_object_span(
                                &mut object_spans,
                                EllaObjectSpan {
                                    provider_id,
                                    instance_id: *instance_id,
                                    t0,
                                    t1: t,
                                    obj_idx: 0,
                                    first_pn,
                                    boundary_index: slot,
                                    object_id: None,
                                    packets: *packets,
                                    bytes: *bytes,
                                    n: 1,
                                    state: if *st == ELLA_OBJECT_COMPLETED {
                                        EllaObjectSpanState::Completed
                                    } else {
                                        EllaObjectSpanState::Abandoned
                                    },
                                },
                            );
                        }
                    }
                    _ => {}
                }
                let n = obj_occupancy.entry(*instance_id).or_insert(0);
                match *st {
                    ELLA_OBJECT_STARTED => *n = 1,
                    ELLA_OBJECT_COMPLETED | ELLA_OBJECT_ABANDONED => *n = 0,
                    _ => continue,
                }
                objs.record(t, &obj_occupancy, *instance_id, prof, provider_id);
            }
            EllaPayload::FrameSent {
                instance_id,
                object_id,
                frame_index,
                first_packet,
                actual_send_us,
                toggle,
                frame_len,
                ..
            } => {
                obj_ids.entry(*first_packet).or_insert(*object_id);
                raw_frames.push(RawFrame {
                    instance_id: *instance_id,
                    t,
                    object_id: *object_id,
                    frame_index: *frame_index,
                    first_pn: *first_packet,
                    dur_us: *actual_send_us,
                    origin: true,
                    boundary_index: FRAME_SLOT_UNKNOWN,
                    toggle: *toggle,
                    bytes: *frame_len,
                });
            }
            EllaPayload::FrameRelay {
                instance_id,
                first_packet,
                recv_us,
                toggle,
                boundary_index,
                frame_len,
                ..
            } => {
                raw_frames.push(RawFrame {
                    instance_id: *instance_id,
                    t,
                    object_id: 0,
                    frame_index: 0,
                    first_pn: *first_packet,
                    dur_us: *recv_us,
                    origin: false,
                    boundary_index: *boundary_index,
                    toggle: *toggle,
                    bytes: *frame_len,
                });
            }
            EllaPayload::ChannelDestroyed { instance_id, .. } => {
                if !sampled {
                    occupancy.insert(*instance_id, 0);
                    live_subs.retain(|(inst, _, _)| *inst != *instance_id);
                    live_joins.retain(|(inst, _)| *inst != *instance_id);
                    subs.record(t, &occupancy, *instance_id, prof, provider_id);
                }
                if obj_occupancy.contains_key(instance_id) {
                    obj_occupancy.insert(*instance_id, 0);
                    objs.record(t, &obj_occupancy, *instance_id, prof, provider_id);
                }
                let tot: u32 = occupancy.values().copied().sum();
                push_marker(t, tot as f64, EllaChannelMarkerKind::Destroyed);
            }
            EllaPayload::ChannelError { .. } => {
                push_marker(t, tot_now as f64, EllaChannelMarkerKind::Error);
            }
            _ => {}
        }
    }
    subs.extend_to(t_last);
    objs.extend_to(t_last);

    for (instance_id, (t0, first_pn, slot)) in obj_open {
        push_object_span(
            &mut object_spans,
            EllaObjectSpan {
                provider_id,
                instance_id,
                t0,
                t1: t_last,
                obj_idx: 0,
                first_pn,
                boundary_index: slot,
                object_id: None,
                packets: 0,
                bytes: 0,
                n: 1,
                state: EllaObjectSpanState::Open,
            },
        );
    }
    for s in &mut object_spans {
        s.object_id = obj_ids.get(&s.first_pn).copied();
    }
    object_spans.sort_by(|a, b| a.t0.total_cmp(&b.t0));
    let node = prof.node_label(provider_id);
    let (object_id, object_duration, object_size) =
        object_span_plots(&object_spans, |_| node.clone());

    let frame_rate = frame_rate_series(prof, provider_id, src, t0_us);
    let frame_duration = frame_duration_series(prof, provider_id, src, t0_us);
    let (frame_ratio, frame_ratio_fallback) =
        frame_ratio_channel_series(prof, provider_id, src, t0_us);
    EllaChannelPlotData {
        subscribers: subs.into_series(prof, provider_id, "Subscribers"),
        objects: objs.into_series(prof, provider_id, "Objects"),
        object_rate: object_rate_series(prof, provider_id, t0_us, t_last, src),
        object_timing: object_timing_channel_series(src, t0_us),
        frame_rate,
        frame_duration,
        frame_ratio,
        frame_ratio_fallback,
        object_id,
        object_duration,
        object_size,
        object_span_copies: object_spans.clone(),
        object_spans,
        frame_spans: layout_frame_spans(provider_id, &raw_frames),
        errors: channel_error_series(&src.events, t0_us),
        markers,
    }
}

/// COMPLETED and the next STARTED on one instance are two EventLog
/// records of the same GOP boundary. Hold rate across gaps shorter
/// than this so consecutive objects draw as a continuous Mbps box.
const OBJECT_RATE_HOLD_S: f64 = 0.05;

/// GOP length is ~2 s (stretched a few seconds). A STARTED→COMPLETED
/// or `CHANNEL_SAMPLE` byte interval longer than this is a capture
/// hole (rolled file missing, idle between files), not a live object.
const MAX_OBJECT_SPAN_S: f64 = 16.0;

fn object_interval_ok(t0: f64, t1: f64) -> bool {
    let dt = t1 - t0;
    dt.is_finite() && dt > 0.0 && dt <= MAX_OBJECT_SPAN_S
}

fn push_object_span(spans: &mut Vec<EllaObjectSpan>, span: EllaObjectSpan) {
    if object_interval_ok(span.t0, span.t1) {
        spans.push(span);
    }
}

struct SampleRateBox {
    provider_id: u16,
    session_id: u64,
    instance_id: u32,
    t0: f64,
    t1: f64,
    mbps: f64,
    dt: f64,
}

/// Object payload as a Mbps box from the previous non-zero sample
/// (previous tick on the first).
fn sample_object_rate_boxes(
    prof: &EllaProfile,
    mut include: impl FnMut(u16, u64) -> bool,
    t0_us: u64,
) -> Vec<SampleRateBox> {
    sample_object_rate_boxes_iter(
        prof.channel_samples
            .iter()
            .filter(|s| include(s.provider_id, s.session_id)),
        t0_us,
    )
}

fn sample_object_rate_boxes_iter<'a>(
    samples: impl Iterator<Item = &'a EllaChannelSample>,
    t0_us: u64,
) -> Vec<SampleRateBox> {
    let mut last_ts: FxHashMap<(u16, u64, u32), u64> = FxHashMap::default();
    let mut last_bytes_ts: FxHashMap<(u16, u64, u32), u64> = FxHashMap::default();
    let mut out = Vec::new();
    for s in samples {
        let key = (s.provider_id, s.session_id, s.instance_id);
        let prev = last_ts.insert(key, s.ts);
        let Some(t_prev) = prev else {
            continue;
        };
        if s.object_bytes == 0 {
            continue;
        }
        let t_start = last_bytes_ts.insert(key, s.ts).unwrap_or(t_prev);
        let dt_us = s.ts.saturating_sub(t_start);
        if dt_us < MIN_RATE_DT_US {
            continue;
        }
        let dt = dt_us as f64 / 1_000_000.0;
        if dt > MAX_OBJECT_SPAN_S {
            continue;
        }
        out.push(SampleRateBox {
            provider_id: s.provider_id,
            session_id: s.session_id,
            instance_id: s.instance_id,
            t0: t_start.saturating_sub(t0_us) as f64 / 1_000_000.0,
            t1: s.ts.saturating_sub(t0_us) as f64 / 1_000_000.0,
            mbps: s.object_bytes as f64 * 8.0 / 1_000_000.0 / dt,
            dt,
        });
    }
    out
}

fn sessions_with_object_events(
    prof: &EllaProfile,
    provider_id: Option<u16>,
) -> FxHashSet<(u16, u64)> {
    prof.events
        .iter()
        .filter(|e| pid_ok(provider_id, e.provider_id))
        .filter(|e| matches!(e.payload, EllaPayload::ObjectState { .. }))
        .map(|e| (e.provider_id, e.session_id))
        .collect()
}

fn provider_has_frame_events(prof: &EllaProfile, provider_id: u16) -> bool {
    prof.events.iter().any(|e| {
        e.provider_id == provider_id
            && matches!(
                e.payload,
                EllaPayload::FrameSent { .. } | EllaPayload::FrameRelay { .. }
            )
    })
}

/// Sessions whose object Mbps comes from `CHANNEL_SAMPLE` (no `OBJECT_STATE`).
fn object_rate_sample_sessions(
    prof: &EllaProfile,
    provider_id: Option<u16>,
) -> FxHashSet<(u16, u64)> {
    let detailed = sessions_with_object_events(prof, provider_id);
    prof.channel_samples
        .iter()
        .filter(|s| pid_ok(provider_id, s.provider_id))
        .filter(|s| s.session_id != ELLA_RELAY_GLOBAL_SESSION)
        .map(|s| (s.provider_id, s.session_id))
        .filter(|key| !detailed.contains(key))
        .collect()
}

/// One instance's Mbps boxes as a step series, holding across GOP gaps.
fn object_rate_instance_steps(mut boxes: Vec<(f64, f64, f64)>) -> Vec<[f64; 2]> {
    boxes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut s = Vec::new();
    for (i, &(t0, t1, mbps)) in boxes.iter().enumerate() {
        push_step(&mut s, t0, mbps);
        let hold_next = boxes
            .get(i + 1)
            .is_some_and(|&(n0, _, _)| n0 - t1 < OBJECT_RATE_HOLD_S);
        if !hold_next {
            push_step(&mut s, t1, 0.0);
        }
    }
    s
}

/// Spread each object's payload over STARTED→COMPLETED as a constant
/// Mbps box (one copy per Fanout instance). Upstream is RX, downstream TX.
/// Falls back to `CHANNEL_SAMPLE` `object_bytes` when no `OBJECT_STATE`.
fn object_rate_series(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
    t_last: f64,
    src: &EllaMemberSrc<'_>,
) -> Vec<EllaChannelSubSeries> {
    let has_obj_events = src
        .events
        .iter()
        .any(|e| matches!(e.payload, EllaPayload::ObjectState { .. }));
    let sampled = !has_obj_events && !src.samples.is_empty();
    let boxes = if sampled {
        sample_object_rate_boxes_iter(src.samples.iter().copied(), t0_us)
            .into_iter()
            .map(|b| (b.t0, b.t1, b.instance_id, b.mbps))
            .collect()
    } else {
        object_state_rate_boxes(&src.events, t0_us)
    };
    object_rate_from_boxes(prof, provider_id, t_last, boxes)
}

fn object_state_rate_boxes(events: &[&EllaEvent], t0_us: u64) -> Vec<(f64, f64, u32, f64)> {
    let mut open: FxHashMap<u32, f64> = FxHashMap::default();
    let mut boxes: Vec<(f64, f64, u32, f64)> = Vec::new();
    for e in events {
        let t = e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        let EllaPayload::ObjectState {
            state,
            instance_id,
            bytes,
            ..
        } = &e.payload
        else {
            continue;
        };
        match *state {
            ELLA_OBJECT_STARTED => {
                open.entry(*instance_id).or_insert(t);
            }
            ELLA_OBJECT_COMPLETED | ELLA_OBJECT_ABANDONED => {
                let Some(t_start) = open.remove(instance_id) else {
                    continue;
                };
                if *bytes == 0 {
                    continue;
                }
                if !object_interval_ok(t_start, t) {
                    continue;
                }
                let dt = (t - t_start).max(1e-6);
                let mbps = *bytes as f64 * 8.0 / 1_000_000.0 / dt;
                boxes.push((t_start, t, *instance_id, mbps));
            }
            _ => {}
        }
    }
    boxes
}

fn object_rate_from_boxes(
    prof: &EllaProfile,
    provider_id: u16,
    t_last: f64,
    boxes: Vec<(f64, f64, u32, f64)>,
) -> Vec<EllaChannelSubSeries> {
    if boxes.is_empty() {
        return Vec::new();
    }
    let mut by_inst: FxHashMap<u32, Vec<(f64, f64, f64)>> = FxHashMap::default();
    for (t0, t1, inst, mbps) in boxes {
        by_inst.entry(inst).or_default().push((t0, t1, mbps));
    }
    let mut inst_pts: FxHashMap<u32, Vec<[f64; 2]>> = FxHashMap::default();
    let mut edges: Vec<(f64, f64, u32)> = Vec::new();
    for (inst, mut inst_boxes) in by_inst {
        inst_boxes.sort_by(|a, b| a.0.total_cmp(&b.0));
        let s = inst_pts.entry(inst).or_default();
        for (i, &(t0, t1, mbps)) in inst_boxes.iter().enumerate() {
            push_step(s, t0, mbps);
            edges.push((t0, mbps, inst));
            let hold_next = inst_boxes
                .get(i + 1)
                .is_some_and(|&(n0, _, _)| n0 - t1 < OBJECT_RATE_HOLD_S);
            if hold_next {
                let n0 = inst_boxes[i + 1].0;
                edges.push((n0, -mbps, inst));
            } else {
                push_step(s, t1, 0.0);
                edges.push((t1, -mbps, inst));
            }
        }
    }
    edges.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut up = Vec::new();
    let mut dn = Vec::new();
    let mut total = Vec::new();
    let mut up_v = 0.0_f64;
    let mut dn_v = 0.0_f64;
    let mut i = 0;
    while i < edges.len() {
        let t = edges[i].0;
        while i < edges.len() && edges[i].0 == t {
            if ella_side(prof, provider_id, edges[i].2) == ELLA_SIDE_UPSTREAM {
                up_v += edges[i].1;
            } else {
                dn_v += edges[i].1;
            }
            i += 1;
        }
        push_step_changed(&mut up, t, up_v.max(0.0));
        push_step_changed(&mut dn, t, dn_v.max(0.0));
        push_step_changed(&mut total, t, (up_v + dn_v).max(0.0));
    }
    for s in inst_pts.values_mut() {
        extend_step_series(s, t_last);
    }
    extend_step_series(&mut up, t_last);
    extend_step_series(&mut dn, t_last);
    extend_step_series(&mut total, t_last);
    relabel_object_rate(collect_occupancy_series(
        prof,
        provider_id,
        inst_pts,
        up,
        dn,
        total,
        "Object rate",
    ))
}

fn relabel_object_rate(mut series: Vec<EllaChannelSubSeries>) -> Vec<EllaChannelSubSeries> {
    for s in &mut series {
        if let Some(rest) = s.label.strip_prefix("Internal").map(str::to_string) {
            s.label = format!("RX{rest}");
        } else if let Some(rest) = s.label.strip_prefix("External").map(str::to_string) {
            s.label = format!("TX{rest}");
        }
    }
    series
}

fn frame_mbps(len: u64, us: u64) -> Option<f64> {
    if us == 0 {
        None
    } else {
        Some(len as f64 * 8.0 / us as f64)
    }
}

#[derive(Default, Clone, Copy)]
struct FrameAcc {
    frames: u64,
    len: u64,
    planned: u64,
    actual: u64,
    fwd: u64,
}

impl FrameAcc {
    fn add(&mut self, s: &EllaFrameSample) {
        self.frames += u64::from(s.frames);
        self.len += u64::from(s.sum_frame_len);
        self.planned += u64::from(s.sum_planned_us);
        self.actual += u64::from(s.sum_actual_us);
        self.fwd += u64::from(s.sum_fwd_us);
    }

    fn add_event(&mut self, payload: &EllaPayload) {
        match payload {
            EllaPayload::FrameSent {
                frame_len,
                planned_send_us,
                actual_send_us,
                ..
            } => {
                self.frames += 1;
                self.len += u64::from(*frame_len);
                self.planned += u64::from(*planned_send_us);
                self.actual += u64::from(*actual_send_us);
            }
            EllaPayload::FrameRelay {
                frame_len,
                wire_send_us,
                recv_us,
                fwd_us,
                ..
            } => {
                self.frames += 1;
                self.len += u64::from(*frame_len);
                self.planned += u64::from(*wire_send_us);
                self.actual += u64::from(*recv_us);
                self.fwd += u64::from(*fwd_us);
            }
            _ => {}
        }
    }
}

fn push_frame_acc(pts: &mut Vec<[f64; 2]>, t: f64, len: u64, us: u64) {
    if let Some(mbps) = frame_mbps(len, us) {
        push_step(pts, t, mbps);
    }
}

fn push_frame_ms(pts: &mut Vec<[f64; 2]>, t: f64, frames: u64, us: u64) {
    if frames == 0 || us == 0 {
        return;
    }
    push_step(pts, t, us as f64 / frames as f64 / 1000.0);
}

/// Planned / Recv / Fwd stay in the legend but start hidden.
pub fn ella_frame_legend_hidden_by_default(label: &str) -> bool {
    matches!(
        label.rsplit(' ').next().unwrap_or(label),
        "Planned" | "Recv" | "Fwd"
    )
}

/// Duration-ratio Max stays in the legend but starts hidden.
pub fn ella_ratio_legend_hidden_by_default(label: &str) -> bool {
    label.rsplit(' ').next().unwrap_or(label) == "Max"
}

fn frame_metric_names(relay: bool) -> [&'static str; 3] {
    if relay {
        ["Wire", "Recv", "Fwd"]
    } else {
        ["Planned", "Actual", "Fwd"]
    }
}

fn frame_metric_side_series(
    relay: bool,
    names: [&'static str; 3],
    prefix: &str,
    by_ts: &std::collections::BTreeMap<u64, FrameAcc>,
    t0_us: u64,
    saw_fwd: bool,
    duration_ms: bool,
) -> Vec<EllaChannelSubSeries> {
    if by_ts.is_empty() {
        return Vec::new();
    }
    let mut planned = Vec::new();
    let mut actual = Vec::new();
    let mut fwd = Vec::new();
    for (&ts, acc) in by_ts {
        let t = ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        if duration_ms {
            push_frame_ms(&mut planned, t, acc.frames, acc.planned);
            push_frame_ms(&mut actual, t, acc.frames, acc.actual);
            if relay || saw_fwd {
                push_frame_ms(&mut fwd, t, acc.frames, acc.fwd);
            }
        } else {
            push_frame_acc(&mut planned, t, acc.len, acc.planned);
            push_frame_acc(&mut actual, t, acc.len, acc.actual);
            if relay || saw_fwd {
                push_frame_acc(&mut fwd, t, acc.len, acc.fwd);
            }
        }
    }
    let mut series = Vec::new();
    for (name, data) in [(names[0], planned), (names[1], actual), (names[2], fwd)] {
        if !data.is_empty() {
            series.push(EllaChannelSubSeries {
                label: format!("{prefix} {name}"),
                data,
            });
        }
    }
    series
}

fn member_has_frame_events(src: &EllaMemberSrc<'_>) -> bool {
    src.events.iter().any(|e| {
        matches!(
            e.payload,
            EllaPayload::FrameSent { .. } | EllaPayload::FrameRelay { .. }
        )
    })
}

fn accumulate_session_frames(
    prof: &EllaProfile,
    provider_id: u16,
    src: &EllaMemberSrc<'_>,
) -> (
    bool,
    bool,
    std::collections::BTreeMap<u64, FrameAcc>,
    std::collections::BTreeMap<u64, FrameAcc>,
) {
    let relay = prof.node_role(provider_id) != EllaNodeRole::Server;
    let use_samples = !member_has_frame_events(src) && !src.frames.is_empty();
    let mut up: std::collections::BTreeMap<u64, FrameAcc> = std::collections::BTreeMap::new();
    let mut dn: std::collections::BTreeMap<u64, FrameAcc> = std::collections::BTreeMap::new();
    let mut saw_fwd = false;
    if use_samples {
        for s in &src.frames {
            if s.sum_fwd_us > 0 {
                saw_fwd = true;
            }
            let dest = if ella_side(prof, provider_id, s.instance_id) == ELLA_SIDE_UPSTREAM {
                &mut up
            } else {
                &mut dn
            };
            dest.entry(s.ts).or_default().add(s);
        }
    } else {
        for e in &src.events {
            let instance_id = match &e.payload {
                EllaPayload::FrameSent { instance_id, .. } => *instance_id,
                EllaPayload::FrameRelay {
                    instance_id,
                    fwd_us,
                    ..
                } => {
                    if *fwd_us > 0 {
                        saw_fwd = true;
                    }
                    *instance_id
                }
                _ => continue,
            };
            let dest = if ella_side(prof, provider_id, instance_id) == ELLA_SIDE_UPSTREAM {
                &mut up
            } else {
                &mut dn
            };
            let bin = e.ts / FRAME_AGG_BIN_US * FRAME_AGG_BIN_US;
            dest.entry(bin).or_default().add_event(&e.payload);
        }
    }
    (relay, saw_fwd, up, dn)
}

const FRAME_AGG_BIN_US: u64 = 100_000;

/// Byte-weighted frame rates from `FRAME_SENT` / `FRAME_RELAY`, or
/// `FRAME_SAMPLE` when those events are absent. Origin is planned vs
/// actual send; relay is wire / recv / fwd. Upstream is RX, downstream TX.
fn frame_rate_series(
    prof: &EllaProfile,
    provider_id: u16,
    src: &EllaMemberSrc<'_>,
    t0_us: u64,
) -> Vec<EllaChannelSubSeries> {
    session_frame_metric_series(prof, provider_id, src, t0_us, false)
}

fn frame_duration_series(
    prof: &EllaProfile,
    provider_id: u16,
    src: &EllaMemberSrc<'_>,
    t0_us: u64,
) -> Vec<EllaChannelSubSeries> {
    session_frame_metric_series(prof, provider_id, src, t0_us, true)
}

fn session_frame_metric_series(
    prof: &EllaProfile,
    provider_id: u16,
    src: &EllaMemberSrc<'_>,
    t0_us: u64,
    duration_ms: bool,
) -> Vec<EllaChannelSubSeries> {
    let (relay, saw_fwd, up, dn) = accumulate_session_frames(prof, provider_id, src);
    let names = frame_metric_names(relay);
    let mut series = Vec::new();
    series.extend(frame_metric_side_series(
        relay,
        names,
        "RX",
        &up,
        t0_us,
        saw_fwd,
        duration_ms,
    ));
    series.extend(frame_metric_side_series(
        relay,
        names,
        "TX",
        &dn,
        t0_us,
        saw_fwd,
        duration_ms,
    ));
    series
}

/// Approximate one continuously sent object with a group of sampled intervals
/// when detailed per-frame EventLog records were not captured.
const FRAME_RATIO_FALLBACK_BIN_US: u64 = 2_000_000;

/// Node-wide byte-weighted frame rates from `FRAME_SENT` / `FRAME_RELAY`,
/// or `FRAME_SAMPLE` when those events are absent.
pub fn build_ella_frame_rate_aggregate(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
) -> Vec<EllaNamedSeries> {
    provider_frame_metric_series(prof, provider_id, t0_us, None, false)
}

/// Mean per-frame duration in milliseconds. `instance_side` limits a
/// relay plot to Internal or External; `None` is every instance.
pub fn build_ella_frame_duration_aggregate(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
    instance_side: Option<u8>,
) -> Vec<EllaNamedSeries> {
    provider_frame_metric_series(prof, provider_id, t0_us, instance_side, true)
}

fn provider_frame_metric_series(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
    instance_side: Option<u8>,
    duration_ms: bool,
) -> Vec<EllaNamedSeries> {
    let relay = prof.node_role(provider_id) != EllaNodeRole::Server;
    let names = frame_metric_names(relay);
    let use_samples = !provider_has_frame_events(prof, provider_id)
        && prof
            .frame_samples
            .iter()
            .any(|s| s.provider_id == provider_id);
    let mut by_bin: std::collections::BTreeMap<u64, FrameAcc> = std::collections::BTreeMap::new();
    let mut saw_fwd = false;
    if use_samples {
        for s in &prof.frame_samples {
            if s.provider_id != provider_id {
                continue;
            }
            if instance_side.is_some_and(|side| ella_side(prof, provider_id, s.instance_id) != side)
            {
                continue;
            }
            if s.sum_fwd_us > 0 {
                saw_fwd = true;
            }
            let bin = s.ts / FRAME_AGG_BIN_US;
            by_bin.entry(bin).or_default().add(s);
        }
    } else {
        for e in &prof.events {
            if e.provider_id != provider_id {
                continue;
            }
            let instance_id = match &e.payload {
                EllaPayload::FrameRelay {
                    instance_id,
                    fwd_us,
                    ..
                } => {
                    if *fwd_us > 0 {
                        saw_fwd = true;
                    }
                    *instance_id
                }
                EllaPayload::FrameSent { instance_id, .. } => *instance_id,
                _ => continue,
            };
            if instance_side.is_some_and(|side| ella_side(prof, provider_id, instance_id) != side) {
                continue;
            }
            let bin = e.ts / FRAME_AGG_BIN_US;
            by_bin.entry(bin).or_default().add_event(&e.payload);
        }
    }
    if by_bin.is_empty() {
        return Vec::new();
    }
    let mut planned = Vec::new();
    let mut actual = Vec::new();
    let mut fwd = Vec::new();
    for (&bin, acc) in &by_bin {
        let ts = bin * FRAME_AGG_BIN_US;
        let t = ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        if duration_ms {
            push_frame_ms(&mut planned, t, acc.frames, acc.planned);
            push_frame_ms(&mut actual, t, acc.frames, acc.actual);
            if relay || saw_fwd {
                push_frame_ms(&mut fwd, t, acc.frames, acc.fwd);
            }
        } else {
            push_frame_acc(&mut planned, t, acc.len, acc.planned);
            push_frame_acc(&mut actual, t, acc.len, acc.actual);
            if relay || saw_fwd {
                push_frame_acc(&mut fwd, t, acc.len, acc.fwd);
            }
        }
    }
    let mut out = Vec::new();
    for (name, data) in [(names[0], planned), (names[1], actual), (names[2], fwd)] {
        if !data.is_empty() {
            out.push(EllaNamedSeries {
                label: name.to_string(),
                data,
            });
        }
    }
    out
}

fn frame_ratio_percentile(sorted: &[f64], p: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    let pos = p * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

/// Per-frame timing-ratio percentiles in 100 ms EventLog-time bins.
///
/// Server nodes use `FRAME_SENT`'s actual/planned send duration. Relay and
/// unlabeled nodes use `FRAME_RELAY`'s receive/wire duration. These are
/// unweighted percentiles over detailed frame records; `FRAME_SAMPLE` cannot
/// provide equivalent tail statistics because it contains only interval sums.
fn frame_ratio_percentile_series(
    mut by_bin: std::collections::BTreeMap<u64, Vec<f64>>,
    bin_us: u64,
    t0_us: u64,
    capture_relative_bins: bool,
) -> Vec<EllaNamedSeries> {
    if by_bin.is_empty() {
        return Vec::new();
    }

    let mut median = Vec::with_capacity(by_bin.len());
    let mut p75 = Vec::with_capacity(by_bin.len());
    let mut p95 = Vec::with_capacity(by_bin.len());
    let mut max = Vec::with_capacity(by_bin.len());
    for (bin, ratios) in &mut by_bin {
        ratios.sort_by(f64::total_cmp);
        let ts = bin * bin_us;
        let t = if capture_relative_bins {
            ts as f64 / 1_000_000.0
        } else {
            ts.saturating_sub(t0_us) as f64 / 1_000_000.0
        };
        median.push([t, frame_ratio_percentile(ratios, 0.50)]);
        p75.push([t, frame_ratio_percentile(ratios, 0.75)]);
        p95.push([t, frame_ratio_percentile(ratios, 0.95)]);
        max.push([t, *ratios.last().expect("non-empty timing-ratio bin")]);
    }

    vec![
        EllaNamedSeries {
            label: "Median".into(),
            data: median,
        },
        EllaNamedSeries {
            label: "P75".into(),
            data: p75,
        },
        EllaNamedSeries {
            label: "P95".into(),
            data: p95,
        },
        EllaNamedSeries {
            label: "Max".into(),
            data: max,
        },
    ]
}

/// Per-frame timing-ratio percentiles in 100 ms EventLog-time bins.
///
/// Server nodes use `FRAME_SENT`'s actual/planned send duration. Relay and
/// unlabeled nodes use `FRAME_RELAY`'s receive/wire duration. These are
/// unweighted percentiles over detailed frame records; `FRAME_SAMPLE` cannot
/// provide equivalent tail statistics because it contains only interval sums.
pub fn build_ella_frame_ratio_percentiles(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
) -> Vec<EllaNamedSeries> {
    build_ella_frame_ratio_percentiles_for_side(prof, provider_id, t0_us, None)
}

/// Like [`build_ella_frame_ratio_percentiles`], limited to one Fanout side.
pub fn build_ella_frame_ratio_percentiles_for_side(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
    instance_side: Option<u8>,
) -> Vec<EllaNamedSeries> {
    let relay = prof.node_role(provider_id) != EllaNodeRole::Server;
    let mut by_bin: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();

    for event in &prof.events {
        if event.provider_id != provider_id {
            continue;
        }
        let instance_id = match &event.payload {
            EllaPayload::FrameSent { instance_id, .. }
            | EllaPayload::FrameRelay { instance_id, .. } => *instance_id,
            _ => continue,
        };
        if instance_side.is_some_and(|side| ella_side(prof, provider_id, instance_id) != side) {
            continue;
        }
        let ratio = match (relay, &event.payload) {
            (
                false,
                EllaPayload::FrameSent {
                    planned_send_us,
                    actual_send_us,
                    ..
                },
            ) if *planned_send_us > 0 => *actual_send_us as f64 / *planned_send_us as f64,
            (
                true,
                EllaPayload::FrameRelay {
                    wire_send_us,
                    recv_us,
                    ..
                },
            ) if *wire_send_us > 0 => *recv_us as f64 / *wire_send_us as f64,
            _ => continue,
        };
        by_bin
            .entry(event.ts / FRAME_AGG_BIN_US)
            .or_default()
            .push(ratio);
    }

    frame_ratio_percentile_series(by_bin, FRAME_AGG_BIN_US, t0_us, false)
}

/// Fallback timing-ratio percentiles from `FRAME_SAMPLE` aggregates.
///
/// Each observation is a channel/instance's duration ratio after summing its
/// 100 ms samples into a capture-relative 2 s window. This approximates a
/// continuously sent object, but is not a distribution of individual frames.
pub fn build_ella_frame_sample_ratio_percentiles(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
) -> Vec<EllaNamedSeries> {
    build_ella_frame_sample_ratio_percentiles_for_side(prof, provider_id, t0_us, None)
}

/// Like [`build_ella_frame_sample_ratio_percentiles`], limited to one Fanout
/// side.
pub fn build_ella_frame_sample_ratio_percentiles_for_side(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
    instance_side: Option<u8>,
) -> Vec<EllaNamedSeries> {
    let mut totals: std::collections::BTreeMap<(u64, u32, u64), (u64, u64)> =
        std::collections::BTreeMap::new();
    for sample in &prof.frame_samples {
        if sample.provider_id != provider_id {
            continue;
        }
        if instance_side
            .is_some_and(|side| ella_side(prof, provider_id, sample.instance_id) != side)
        {
            continue;
        }
        let bin = sample.ts.saturating_sub(t0_us) / FRAME_RATIO_FALLBACK_BIN_US;
        let entry = totals
            .entry((sample.session_id, sample.instance_id, bin))
            .or_default();
        entry.0 += u64::from(sample.sum_planned_us);
        entry.1 += u64::from(sample.sum_actual_us);
    }

    let mut by_bin: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    for ((_, _, bin), (reference_us, observed_us)) in totals {
        if reference_us > 0 {
            by_bin
                .entry(bin)
                .or_default()
                .push(observed_us as f64 / reference_us as f64);
        }
    }

    frame_ratio_percentile_series(by_bin, FRAME_RATIO_FALLBACK_BIN_US, t0_us, true)
}

/// One channel's duration-ratio percentiles, RX/TX-prefixed per side.
///
/// Detailed `FRAME_SENT` (server: actual/planned send) or `FRAME_RELAY`
/// (relay: recv/wire) records bin into 100 ms windows, matching the
/// System-tab ratio plots. When the channel has no detailed frame events
/// but does have `FRAME_SAMPLE`s, each instance's timing sums fold into
/// a capture-relative 2 s window and percentiles run across those
/// per-instance aggregate ratios; the returned flag marks that fallback.
fn frame_ratio_channel_series(
    prof: &EllaProfile,
    provider_id: u16,
    src: &EllaMemberSrc<'_>,
    t0_us: u64,
) -> (Vec<EllaChannelSubSeries>, bool) {
    let relay = prof.node_role(provider_id) != EllaNodeRole::Server;
    let use_samples = !member_has_frame_events(src) && !src.frames.is_empty();
    let (bin_us, capture_relative_bins) = if use_samples {
        (FRAME_RATIO_FALLBACK_BIN_US, true)
    } else {
        (FRAME_AGG_BIN_US, false)
    };
    let mut up: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    let mut dn: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    if use_samples {
        let mut totals: std::collections::BTreeMap<(u8, u32, u64), (u64, u64)> =
            std::collections::BTreeMap::new();
        for s in &src.frames {
            let side = ella_side(prof, provider_id, s.instance_id);
            let bin = s.ts.saturating_sub(t0_us) / FRAME_RATIO_FALLBACK_BIN_US;
            let entry = totals.entry((side, s.instance_id, bin)).or_default();
            entry.0 += u64::from(s.sum_planned_us);
            entry.1 += u64::from(s.sum_actual_us);
        }
        for ((side, _, bin), (reference_us, observed_us)) in totals {
            if reference_us > 0 {
                let dest = if side == ELLA_SIDE_UPSTREAM {
                    &mut up
                } else {
                    &mut dn
                };
                dest.entry(bin)
                    .or_default()
                    .push(observed_us as f64 / reference_us as f64);
            }
        }
    } else {
        for e in &src.events {
            let (instance_id, ratio) = match (relay, &e.payload) {
                (
                    false,
                    EllaPayload::FrameSent {
                        instance_id,
                        planned_send_us,
                        actual_send_us,
                        ..
                    },
                ) if *planned_send_us > 0 => (
                    *instance_id,
                    *actual_send_us as f64 / *planned_send_us as f64,
                ),
                (
                    true,
                    EllaPayload::FrameRelay {
                        instance_id,
                        wire_send_us,
                        recv_us,
                        ..
                    },
                ) if *wire_send_us > 0 => (*instance_id, *recv_us as f64 / *wire_send_us as f64),
                _ => continue,
            };
            let dest = if ella_side(prof, provider_id, instance_id) == ELLA_SIDE_UPSTREAM {
                &mut up
            } else {
                &mut dn
            };
            dest.entry(e.ts / FRAME_AGG_BIN_US).or_default().push(ratio);
        }
    }
    let mut series = Vec::new();
    for (prefix, by_bin) in [("RX", up), ("TX", dn)] {
        for s in frame_ratio_percentile_series(by_bin, bin_us, t0_us, capture_relative_bins) {
            series.push(EllaChannelSubSeries {
                label: format!("{prefix} {}", s.label),
                data: s.data,
            });
        }
    }
    (series, use_samples)
}

/// One lifecycle, subscriber, or object event for a channel session.
pub struct EllaChannelEventRow {
    pub t: f64,
    pub name: &'static str,
    pub instance_label: String,
    pub detail: String,
    pub is_join: bool,
    pub is_object: bool,
}

pub struct EllaChannelEventList {
    pub rows: Vec<EllaChannelEventRow>,
    pub n_joins: usize,
    pub n_lifecycle: usize,
    pub n_objects: usize,
    pub first_created_s: Option<f64>,
    pub last_destroyed_s: Option<f64>,
    pub n_created: usize,
    pub n_destroyed: usize,
}

fn payload_instance_id(p: &EllaPayload) -> Option<u32> {
    match p {
        EllaPayload::InstanceAttach { instance_id, .. }
        | EllaPayload::SessionCreate { instance_id, .. }
        | EllaPayload::ChannelState { instance_id, .. }
        | EllaPayload::ChannelDestroyed { instance_id, .. }
        | EllaPayload::ChannelIdAssigned { instance_id, .. }
        | EllaPayload::ChannelSubscriber { instance_id, .. }
        | EllaPayload::SubscriberIdentity { instance_id, .. }
        | EllaPayload::SubscriberJoin { instance_id, .. }
        | EllaPayload::SubscriberLeave { instance_id, .. }
        | EllaPayload::ObjectState { instance_id, .. }
        | EllaPayload::ObjectTiming { instance_id, .. }
        | EllaPayload::FrameSent { instance_id, .. }
        | EllaPayload::FrameRelay { instance_id, .. }
        | EllaPayload::ChannelError { instance_id, .. }
        | EllaPayload::RelayError { instance_id, .. } => Some(*instance_id),
        EllaPayload::SessionDestroy
        | EllaPayload::NodeInfo { .. }
        | EllaPayload::NodeIdentity { .. } => None,
    }
}

fn channel_event_detail(p: &EllaPayload) -> String {
    match p {
        EllaPayload::SessionCreate { version, .. } => format!("v{version}"),
        EllaPayload::SessionDestroy => String::new(),
        EllaPayload::NodeInfo { role } => ella_role_name(*role).to_string(),
        EllaPayload::NodeIdentity { relay_id } => format!("relay id {relay_id}"),
        EllaPayload::ChannelState {
            state,
            reason,
            subscribers,
            ..
        } => {
            let s = ella_lifecycle_state_name(*state);
            let r = ella_lifecycle_reason_name(*reason);
            if *subscribers > 0 {
                format!("{s}  {r}  subs {subscribers}")
            } else {
                format!("{s}  {r}")
            }
        }
        EllaPayload::ChannelDestroyed {
            reason,
            subscribers,
            ..
        } => {
            let r = ella_destroy_reason_name(*reason);
            if *subscribers > 0 {
                format!("{r}  subs {subscribers}")
            } else {
                r.to_string()
            }
        }
        EllaPayload::ChannelIdAssigned { channel_id, .. } => format!("id {channel_id}"),
        EllaPayload::ChannelSubscriber {
            state,
            reason,
            subscriber_address,
            subscriber_port,
            subscribers,
            ..
        } => {
            let st = ella_subscriber_state_name(*state);
            let addr = format!("{subscriber_address}:{subscriber_port}");
            if *reason == 0 {
                format!("{st}  {addr}  subs {subscribers}")
            } else {
                format!(
                    "{st}  {}  {addr}  subs {subscribers}",
                    ella_subscriber_reason_name(*reason)
                )
            }
        }
        EllaPayload::SubscriberIdentity {
            request_id,
            subscriber_address,
            subscriber_port,
            pbcid,
            ..
        } => format!("{subscriber_address}:{subscriber_port}  request {request_id}  pbcid {pbcid}"),
        EllaPayload::SubscriberJoin {
            sub_id,
            request_id,
            subscriber_address,
            subscriber_port,
            pending_ms,
            replay,
            pbcid,
            ..
        } => {
            let mut out = format!(
                "JOIN  sub {sub_id}  {subscriber_address}:{subscriber_port}  request {request_id}  pending {pending_ms} ms"
            );
            if !pbcid.is_empty() {
                out.push_str(&format!("  pbcid {pbcid}"));
            }
            if *replay {
                out.push_str("  replay");
            }
            out
        }
        EllaPayload::SubscriberLeave {
            sub_id,
            reason,
            duration_ms,
            packets,
            bytes,
            objects,
            ..
        } => format!(
            "LEAVE  sub {sub_id}  {}  {duration_ms} ms  {packets} pkts  {bytes} B  {objects} objs",
            ella_subscriber_reason_name(*reason)
        ),
        EllaPayload::ChannelError { code, detail, .. } => {
            format!("{}  detail {detail}", ella_error_code_name(*code))
        }
        EllaPayload::ObjectState {
            state,
            boundary_index,
            first_packet,
            packets,
            bytes,
            ..
        } => {
            let st = ella_object_state_name(*state);
            if *state == ELLA_OBJECT_STARTED {
                format!("{st}  slot {boundary_index}  first pn {first_packet}")
            } else {
                format!("{st}  slot {boundary_index}  {packets} pkts  {bytes} B  first pn {first_packet}")
            }
        }
        EllaPayload::ObjectTiming {
            object_id,
            first_packet,
            packets_sent,
            packets_total,
            packets_unsent,
            start_lateness_us,
            signature_duration_us,
            encode_duration_us,
            enqueue_duration_us,
            ..
        } => {
            format!(
                "object {object_id}  first pn {first_packet}  sent {packets_sent}/{packets_total}  unsent {packets_unsent}  late {start_lateness_us} us  sign {signature_duration_us} us  encode {encode_duration_us} us  enqueue {enqueue_duration_us} us"
            )
        }
        EllaPayload::FrameSent {
            object_id,
            frame_index,
            first_packet,
            packets,
            planned_send_us,
            actual_send_us,
            ..
        } => {
            format!("object {object_id}  frame {frame_index}  {packets} pkts  first pn {first_packet}  planned {planned_send_us} us  actual {actual_send_us} us")
        }
        EllaPayload::FrameRelay {
            first_packet,
            packets,
            boundary_index,
            wire_send_us,
            recv_us,
            fwd_us,
            ..
        } => {
            format!("slot {boundary_index}  {packets} pkts  first pn {first_packet}  wire {wire_send_us} us  recv {recv_us} us  fwd {fwd_us} us")
        }
        EllaPayload::RelayError {
            code,
            channel_id,
            channel_id_known,
            detail,
            message_type,
            subscriber_address,
            subscriber_port,
            ..
        } => {
            let mut out = if *channel_id_known {
                format!(
                    "{}  id {channel_id}  detail {detail}",
                    ella_relay_error_code_name(*code)
                )
            } else {
                format!("{}  detail {detail}", ella_relay_error_code_name(*code))
            };
            if *message_type != 0 {
                out.push_str(&format!(
                    "  msg {}",
                    ella_fanout_message_type_name(*message_type)
                ));
            }
            if !subscriber_address.is_empty() {
                out.push_str(&format!("  {subscriber_address}:{subscriber_port}"));
            }
            out
        }
        EllaPayload::InstanceAttach { side, .. } => ella_side_name(*side).to_string(),
    }
}

/// Lifecycle / subscriber / object events for one envelope session, in capture order.
pub fn build_ella_channel_events(
    prof: &EllaProfile,
    provider_id: u16,
    session_id: u64,
    t0_us: u64,
) -> EllaChannelEventList {
    let members = [(provider_id, session_id)];
    let srcs = ella_member_sources(prof, &members);
    build_ella_channel_events_from(
        prof,
        provider_id,
        t0_us,
        srcs.get(&(provider_id, session_id)).expect("member src"),
    )
}

fn build_ella_channel_events_from(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
    src: &EllaMemberSrc<'_>,
) -> EllaChannelEventList {
    let mut rows = Vec::new();
    let mut first_created_s = None;
    let mut last_destroyed_s = None;
    let mut n_created = 0usize;
    let mut n_destroyed = 0usize;
    for e in &src.events {
        let t = e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        let is_join = matches!(
            e.payload,
            EllaPayload::ChannelSubscriber { .. }
                | EllaPayload::SubscriberIdentity { .. }
                | EllaPayload::SubscriberJoin { .. }
                | EllaPayload::SubscriberLeave { .. }
        );
        let is_object = matches!(e.payload, EllaPayload::ObjectState { .. });
        if let EllaPayload::ChannelState {
            state: ELLA_LIFECYCLE_CREATED,
            ..
        } = e.payload
        {
            n_created += 1;
            if first_created_s.is_none() {
                first_created_s = Some(t);
            }
        }
        if matches!(e.payload, EllaPayload::ChannelDestroyed { .. }) {
            n_destroyed += 1;
            last_destroyed_s = Some(t);
        }
        let instance_label = payload_instance_id(&e.payload)
            .map(|id| occupancy_instance_label(prof, provider_id, id))
            .unwrap_or_default();
        rows.push(EllaChannelEventRow {
            t,
            name: ella_relay_event_name(e.event_id),
            instance_label,
            detail: channel_event_detail(&e.payload),
            is_join,
            is_object,
        });
    }
    let n_joins = rows.iter().filter(|r| r.is_join).count();
    let n_objects = rows.iter().filter(|r| r.is_object).count();
    let n_lifecycle = rows.len() - n_joins - n_objects;
    EllaChannelEventList {
        rows,
        n_joins,
        n_lifecycle,
        n_objects,
        first_created_s,
        last_destroyed_s,
        n_created,
        n_destroyed,
    }
}

/// Cap on named title bands in the occupancy stack; leftover titles
/// fold into an Other band.
const ELLA_STACK_TITLE_LIMIT: usize = 16;

/// Downstream occupancy by short title.
#[derive(Default)]
pub struct EllaSubStackData {
    pub series: Vec<EllaNamedSeries>,
}

fn is_loopback_addr(addr: &str) -> bool {
    addr == "127.0.0.1" || addr == "::1"
}

fn occupancy_at(data: &[[f64; 2]], t: f64) -> f64 {
    let i = data.partition_point(|p| p[0] <= t);
    if i == 0 {
        0.0
    } else {
        data[i - 1][1]
    }
}

fn series_peak(data: &[[f64; 2]]) -> f64 {
    data.iter().map(|p| p[1]).fold(0.0, f64::max)
}

fn sum_step_series(parts: &[Vec<[f64; 2]>]) -> Vec<[f64; 2]> {
    if parts.len() == 1 {
        return parts[0].clone();
    }
    let mut edges: Vec<(f64, f64)> = Vec::new();
    for s in parts {
        let mut last_y = 0.0_f64;
        let mut i = 0;
        while i < s.len() {
            let t = s[i][0];
            let mut y = s[i][1];
            i += 1;
            while i < s.len() && s[i][0] == t {
                y = s[i][1];
                i += 1;
            }
            let dy = y - last_y;
            if dy != 0.0 {
                edges.push((t, dy));
                last_y = y;
            }
        }
    }
    edges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    let mut y = 0.0_f64;
    let mut i = 0;
    while i < edges.len() {
        let t = edges[i].0;
        while i < edges.len() && edges[i].0 == t {
            y += edges[i].1;
            i += 1;
        }
        push_step_changed(&mut out, t, y.max(0.0));
    }
    out
}

fn occupancy_title_rank(pts: &FxHashMap<String, Vec<[f64; 2]>>) -> FxHashMap<String, u32> {
    pts.iter()
        .map(|(label, data)| (label.clone(), series_peak(data).round() as u32))
        .filter(|(_, peak)| *peak > 0)
        .collect()
}

/// Rank title series highest-peak first and fold leftovers into Other.
/// `rank` is peak external subscribers when provided (RX/TX follow
/// Subscribers); otherwise each series' own peak.
fn named_stack_from_pts(
    pts: FxHashMap<String, Vec<[f64; 2]>>,
    rank: Option<&FxHashMap<String, u32>>,
) -> EllaSubStackData {
    let mut items: Vec<(String, Vec<[f64; 2]>, f64)> = pts
        .into_iter()
        .map(|(label, data)| {
            let peak = series_peak(&data);
            (label, data, peak)
        })
        .filter(|(_, _, peak)| *peak > 0.0)
        .collect();
    items.sort_by(|a, b| {
        let ord = match rank {
            Some(m) => m
                .get(&b.0)
                .copied()
                .unwrap_or(0)
                .cmp(&m.get(&a.0).copied().unwrap_or(0)),
            None => b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal),
        };
        ord.then(a.0.cmp(&b.0))
    });
    let mut other: Option<Vec<[f64; 2]>> = None;
    if items.len() > ELLA_STACK_TITLE_LIMIT {
        let rest: Vec<Vec<[f64; 2]>> = items
            .split_off(ELLA_STACK_TITLE_LIMIT - 1)
            .into_iter()
            .map(|(_, data, _)| data)
            .collect();
        other = Some(sum_step_series(&rest));
    }
    let mut series: Vec<EllaNamedSeries> = items
        .into_iter()
        .map(|(label, data, _)| EllaNamedSeries { label, data })
        .collect();
    if let Some(data) = other {
        if series_peak(&data) > 0.0 {
            series.push(EllaNamedSeries {
                label: "Other".into(),
                data,
            });
        }
    }
    EllaSubStackData { series }
}

/// Last-value sample-and-hold of each title series onto `nbins` in
/// `[vx0, vx1]`. `tops[k][bin]` is the cumulative occupancy through
/// series `k` (bottom of the stack is `k = 0`).
pub fn occupancy_stack_bins(
    series: &[EllaNamedSeries],
    vx0: f64,
    vx1: f64,
    nbins: usize,
) -> (Vec<f64>, Vec<Vec<f64>>) {
    let mut centers = Vec::with_capacity(nbins);
    let mut tops: Vec<Vec<f64>> = (0..series.len())
        .map(|_| Vec::with_capacity(nbins))
        .collect();
    if nbins == 0 || vx1 <= vx0 || series.is_empty() {
        return (centers, tops);
    }
    let bin_width = (vx1 - vx0) / nbins as f64;
    for i in 0..nbins {
        let t = vx0 + (i as f64 + 0.5) * bin_width;
        centers.push(t);
        let mut acc = 0.0;
        for (k, s) in series.iter().enumerate() {
            acc += occupancy_at(&s.data, t);
            tops[k].push(acc);
        }
    }
    (centers, tops)
}

/// Time-weighted mean of a step series onto `nbins` equal bins in
/// `[vx0, vx1]` (same visible-window count as HTTP RPS / TCP conn
/// rate). Consecutive points are a hold of `y_i` on `[t_i, t_{i+1})`.
/// Empty bins are 0. Bins are walked by index so a long unchanged hold
/// covers every overlapping bucket.
pub fn step_series_bins(data: &[[f64; 2]], vx0: f64, vx1: f64, nbins: usize) -> Vec<[f64; 2]> {
    if nbins == 0 || vx1 <= vx0 {
        return Vec::new();
    }
    let w = (vx1 - vx0) / nbins as f64;
    if w <= 0.0 {
        return Vec::new();
    }
    let mut sums = vec![0.0; nbins];
    let mut durs = vec![0.0; nbins];
    for pair in data.windows(2) {
        let (t0, t1, y) = (pair[0][0], pair[1][0], pair[0][1]);
        if t1 <= t0 {
            continue;
        }
        let t = t0.max(vx0);
        let hi = t1.min(vx1);
        if t >= hi {
            continue;
        }
        let start = ((t - vx0) / w).floor().max(0.0) as usize;
        let end = ((hi - vx0) / w).ceil().min(nbins as f64) as usize;
        for i in start..end.min(nbins) {
            let bin_lo = vx0 + i as f64 * w;
            let bin_hi = vx0 + (i as f64 + 1.0) * w;
            let overlap = hi.min(bin_hi) - t.max(bin_lo);
            if overlap > 0.0 {
                sums[i] += y * overlap;
                durs[i] += overlap;
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

fn bump_title(
    pts: &mut FxHashMap<String, Vec<[f64; 2]>>,
    title_n: &mut FxHashMap<String, u32>,
    title: String,
    t: f64,
    delta: i32,
) {
    let n = title_n.entry(title.clone()).or_insert(0);
    if delta > 0 {
        *n = n.saturating_add(delta as u32);
    } else {
        *n = n.saturating_sub((-delta) as u32);
    }
    let y = *n as f64;
    push_step(pts.entry(title).or_default(), t, y);
}

fn drop_present<K: Eq + std::hash::Hash>(
    present: &mut FxHashSet<K>,
    pts: &mut FxHashMap<String, Vec<[f64; 2]>>,
    title_n: &mut FxHashMap<String, u32>,
    title: String,
    t: f64,
    keep: impl Fn(&K) -> bool,
) {
    let before = present.len();
    present.retain(keep);
    let gone = (before - present.len()) as i32;
    if gone > 0 {
        bump_title(pts, title_n, title, t, -gone);
    }
}

/// Reconstruct per-title occupancy from `CHANNEL_SUBSCRIBER` when
/// present, otherwise `CHANNEL_SAMPLE`. Reincarnated sessions of the
/// same short name share a band. Upstream / origin-loopback clients
/// are omitted on the subscriber-replay path.
pub fn build_ella_subscription_stack(prof: &EllaProfile, t0_us: u64) -> EllaSubStackData {
    build_ella_subscription_stack_for(prof, t0_us, None)
}

/// Per-title occupancy for one `fanout_node` provider (or all, when `None`).
pub fn build_ella_subscription_stack_for(
    prof: &EllaProfile,
    t0_us: u64,
    provider_id: Option<u16>,
) -> EllaSubStackData {
    let pts = ella_subscription_pts(prof, t0_us, provider_id);
    let rank = occupancy_title_rank(&pts);
    named_stack_from_pts(pts, Some(&rank))
}

fn ella_subscription_pts(
    prof: &EllaProfile,
    t0_us: u64,
    provider_id: Option<u16>,
) -> FxHashMap<String, Vec<[f64; 2]>> {
    let has_sub_events = prof.events.iter().any(|e| {
        pid_ok(provider_id, e.provider_id)
            && matches!(
                e.payload,
                EllaPayload::ChannelSubscriber { .. } | EllaPayload::SubscriberJoin { .. }
            )
    });
    if !has_sub_events
        && prof
            .channel_samples
            .iter()
            .any(|s| pid_ok(provider_id, s.provider_id))
    {
        return ella_subscription_pts_from_samples(prof, t0_us, provider_id);
    }
    ella_subscription_pts_from_subscribers(prof, t0_us, provider_id)
}

fn channel_short_name(prof: &EllaProfile, provider_id: u16, session_id: u64) -> String {
    prof.channels
        .get(&(provider_id, session_id))
        .map(|c| {
            if c.channel_name.is_empty() {
                format!("#{}", c.session_id)
            } else {
                ella_channel_short_label(&c.channel_name)
            }
        })
        .unwrap_or_else(|| format!("#{session_id}"))
}

fn apply_title_gauge(
    last: &mut FxHashMap<(u16, u64, u32), u32>,
    pts: &mut FxHashMap<String, Vec<[f64; 2]>>,
    title_n: &mut FxHashMap<String, u32>,
    title: String,
    t: f64,
    key: (u16, u64, u32),
    n: u32,
) {
    let old = last.get(&key).copied().unwrap_or(0);
    if n == old {
        return;
    }
    last.insert(key, n);
    let delta = n as i32 - old as i32;
    bump_title(pts, title_n, title, t, delta);
}

fn ella_subscription_pts_from_samples(
    prof: &EllaProfile,
    t0_us: u64,
    provider_id: Option<u16>,
) -> FxHashMap<String, Vec<[f64; 2]>> {
    let mut names: FxHashMap<(u16, u64), String> = FxHashMap::default();
    for c in prof.channels.values() {
        if !pid_ok(provider_id, c.provider_id) {
            continue;
        }
        names.insert(
            (c.provider_id, c.session_id),
            channel_short_name(prof, c.provider_id, c.session_id),
        );
    }
    #[derive(Clone)]
    enum OccOp {
        Create {
            ts: u64,
            provider_id: u16,
            session_id: u64,
            name: String,
        },
        Gauge {
            ts: u64,
            provider_id: u16,
            session_id: u64,
            instance_id: u32,
            n: u32,
        },
        ClearInst {
            ts: u64,
            provider_id: u16,
            session_id: u64,
            instance_id: u32,
        },
        ClearSess {
            ts: u64,
            provider_id: u16,
            session_id: u64,
        },
    }
    impl OccOp {
        fn ts(&self) -> u64 {
            match self {
                Self::Create { ts, .. }
                | Self::Gauge { ts, .. }
                | Self::ClearInst { ts, .. }
                | Self::ClearSess { ts, .. } => *ts,
            }
        }
        fn order(&self) -> u8 {
            match self {
                Self::Create { .. } => 0,
                Self::Gauge { .. } => 1,
                Self::ClearInst { .. } => 2,
                Self::ClearSess { .. } => 3,
            }
        }
    }
    let mut ops = Vec::new();
    for e in &prof.events {
        if !pid_ok(provider_id, e.provider_id) {
            continue;
        }
        match &e.payload {
            EllaPayload::SessionCreate { channel_name, .. } => {
                let name = if channel_name.is_empty() {
                    format!("#{}", e.session_id)
                } else {
                    ella_channel_short_label(channel_name)
                };
                ops.push(OccOp::Create {
                    ts: e.ts,
                    provider_id: e.provider_id,
                    session_id: e.session_id,
                    name,
                });
            }
            EllaPayload::ChannelDestroyed { instance_id, .. } => {
                ops.push(OccOp::ClearInst {
                    ts: e.ts,
                    provider_id: e.provider_id,
                    session_id: e.session_id,
                    instance_id: *instance_id,
                });
            }
            EllaPayload::SessionDestroy => {
                ops.push(OccOp::ClearSess {
                    ts: e.ts,
                    provider_id: e.provider_id,
                    session_id: e.session_id,
                });
            }
            _ => {}
        }
    }
    for s in &prof.channel_samples {
        if !pid_ok(provider_id, s.provider_id) {
            continue;
        }
        ops.push(OccOp::Gauge {
            ts: s.ts,
            provider_id: s.provider_id,
            session_id: s.session_id,
            instance_id: s.instance_id,
            n: s.subscribers,
        });
    }
    ops.sort_by_key(|op| (op.ts(), op.order()));
    let mut last: FxHashMap<(u16, u64, u32), u32> = FxHashMap::default();
    let mut title_n: FxHashMap<String, u32> = FxHashMap::default();
    let mut pts: FxHashMap<String, Vec<[f64; 2]>> = FxHashMap::default();
    for op in ops {
        let t = op.ts().saturating_sub(t0_us) as f64 / 1_000_000.0;
        match op {
            OccOp::Create {
                provider_id: pid,
                session_id: sid,
                name,
                ..
            } => {
                names.insert((pid, sid), name);
            }
            OccOp::Gauge {
                provider_id: pid,
                session_id: sid,
                instance_id,
                n,
                ..
            } => {
                if ella_side(prof, pid, instance_id) == ELLA_SIDE_UPSTREAM {
                    continue;
                }
                let Some(title) = names.get(&(pid, sid)).cloned() else {
                    continue;
                };
                apply_title_gauge(
                    &mut last,
                    &mut pts,
                    &mut title_n,
                    title,
                    t,
                    (pid, sid, instance_id),
                    n,
                );
            }
            OccOp::ClearInst {
                provider_id: pid,
                session_id: sid,
                instance_id,
                ..
            } => {
                if ella_side(prof, pid, instance_id) == ELLA_SIDE_UPSTREAM {
                    continue;
                }
                let Some(title) = names.get(&(pid, sid)).cloned() else {
                    continue;
                };
                apply_title_gauge(
                    &mut last,
                    &mut pts,
                    &mut title_n,
                    title,
                    t,
                    (pid, sid, instance_id),
                    0,
                );
            }
            OccOp::ClearSess {
                provider_id: pid,
                session_id: sid,
                ..
            } => {
                let keys: Vec<_> = last
                    .keys()
                    .copied()
                    .filter(|(p, s, _)| *p == pid && *s == sid)
                    .collect();
                let Some(title) = names.get(&(pid, sid)).cloned() else {
                    continue;
                };
                for key in keys {
                    apply_title_gauge(&mut last, &mut pts, &mut title_n, title.clone(), t, key, 0);
                }
            }
        }
    }
    pts
}

fn ella_subscription_pts_from_subscribers(
    prof: &EllaProfile,
    t0_us: u64,
    provider_id: Option<u16>,
) -> FxHashMap<String, Vec<[f64; 2]>> {
    let mut names: FxHashMap<(u16, u64), String> = FxHashMap::default();
    for c in prof.channels.values() {
        if !pid_ok(provider_id, c.provider_id) {
            continue;
        }
        let label = if c.channel_name.is_empty() {
            format!("#{}", c.session_id)
        } else {
            ella_channel_short_label(&c.channel_name)
        };
        names.insert((c.provider_id, c.session_id), label);
    }
    let mut present: FxHashSet<(String, u16, u16, u64, u32)> = FxHashSet::default();
    let mut joined: FxHashSet<(u16, u64, u32, u32)> = FxHashSet::default();
    let mut title_n: FxHashMap<String, u32> = FxHashMap::default();
    let mut pts: FxHashMap<String, Vec<[f64; 2]>> = FxHashMap::default();
    let title_of = |names: &FxHashMap<(u16, u64), String>, e: &EllaEvent| {
        names
            .get(&(e.provider_id, e.session_id))
            .cloned()
            .unwrap_or_else(|| format!("#{}", e.session_id))
    };
    for e in &prof.events {
        if !pid_ok(provider_id, e.provider_id) {
            continue;
        }
        match &e.payload {
            EllaPayload::SessionCreate { channel_name, .. } => {
                let label = if channel_name.is_empty() {
                    format!("#{}", e.session_id)
                } else {
                    ella_channel_short_label(channel_name)
                };
                names.insert((e.provider_id, e.session_id), label);
            }
            EllaPayload::ChannelSubscriber {
                state,
                instance_id,
                subscriber_address,
                subscriber_port,
                ..
            } => {
                if is_loopback_addr(subscriber_address)
                    || ella_side(prof, e.provider_id, *instance_id) == ELLA_SIDE_UPSTREAM
                {
                    continue;
                }
                let title = title_of(&names, e);
                let key = (
                    subscriber_address.clone(),
                    *subscriber_port,
                    e.provider_id,
                    e.session_id,
                    *instance_id,
                );
                let t = e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
                match *state {
                    ELLA_SUBSCRIBER_SUBSCRIBED => {
                        if present.insert(key) {
                            bump_title(&mut pts, &mut title_n, title, t, 1);
                        }
                    }
                    ELLA_SUBSCRIBER_UNSUBSCRIBED if present.remove(&key) => {
                        bump_title(&mut pts, &mut title_n, title, t, -1);
                    }
                    _ => {}
                }
            }
            EllaPayload::SubscriberJoin {
                instance_id,
                sub_id,
                subscriber_address,
                ..
            } => {
                if is_loopback_addr(subscriber_address)
                    || ella_side(prof, e.provider_id, *instance_id) == ELLA_SIDE_UPSTREAM
                {
                    continue;
                }
                if joined.insert((e.provider_id, e.session_id, *instance_id, *sub_id)) {
                    let t = e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
                    bump_title(&mut pts, &mut title_n, title_of(&names, e), t, 1);
                }
            }
            EllaPayload::SubscriberLeave {
                instance_id,
                sub_id,
                ..
            } => {
                if joined.remove(&(e.provider_id, e.session_id, *instance_id, *sub_id)) {
                    let t = e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
                    bump_title(&mut pts, &mut title_n, title_of(&names, e), t, -1);
                }
            }
            EllaPayload::ChannelDestroyed { instance_id, .. } => {
                if ella_side(prof, e.provider_id, *instance_id) == ELLA_SIDE_UPSTREAM {
                    continue;
                }
                let t = e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
                let pid = e.provider_id;
                let sid = e.session_id;
                let inst = *instance_id;
                drop_present(
                    &mut present,
                    &mut pts,
                    &mut title_n,
                    title_of(&names, e),
                    t,
                    |k| !(k.2 == pid && k.3 == sid && k.4 == inst),
                );
                drop_present(
                    &mut joined,
                    &mut pts,
                    &mut title_n,
                    title_of(&names, e),
                    t,
                    |k| !(k.0 == pid && k.1 == sid && k.2 == inst),
                );
            }
            EllaPayload::SessionDestroy => {
                let t = e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
                let pid = e.provider_id;
                let sid = e.session_id;
                drop_present(
                    &mut present,
                    &mut pts,
                    &mut title_n,
                    title_of(&names, e),
                    t,
                    |k| !(k.2 == pid && k.3 == sid),
                );
                drop_present(
                    &mut joined,
                    &mut pts,
                    &mut title_n,
                    title_of(&names, e),
                    t,
                    |k| !(k.0 == pid && k.1 == sid),
                );
            }
            _ => {}
        }
    }
    pts
}

type EllaObjectRateBoxes = FxHashMap<(String, u16, u64, u32), Vec<(f64, f64, f64)>>;

/// Stacked per-title object Mbps: upstream RX and downstream TX.
/// Same boxes as the channel-tab Object rate plots (`bytes` over
/// STARTED→COMPLETED, GOP-boundary hold). Reincarnations of a short
/// name share a band.
pub fn build_ella_object_rate_stacks(
    prof: &EllaProfile,
    t0_us: u64,
) -> (EllaSubStackData, EllaSubStackData) {
    build_ella_object_rate_stacks_for(prof, t0_us, None, None)
}

/// Stacked object Mbps for one `fanout_node` provider (or all, when `None`).
/// `title_rank` is peak external subscribers so named bands and Other
/// match Fanout Subscribers; `None` ranks each series by its own Mbps.
pub fn build_ella_object_rate_stacks_for(
    prof: &EllaProfile,
    t0_us: u64,
    provider_id: Option<u16>,
    title_rank: Option<&FxHashMap<String, u32>>,
) -> (EllaSubStackData, EllaSubStackData) {
    let mut names: FxHashMap<(u16, u64), String> = FxHashMap::default();
    for c in prof.channels.values() {
        if !pid_ok(provider_id, c.provider_id) {
            continue;
        }
        let label = if c.channel_name.is_empty() {
            format!("#{}", c.session_id)
        } else {
            ella_channel_short_label(&c.channel_name)
        };
        names.insert((c.provider_id, c.session_id), label);
    }
    for e in &prof.events {
        if !pid_ok(provider_id, e.provider_id) {
            continue;
        }
        if let EllaPayload::SessionCreate { channel_name, .. } = &e.payload {
            let label = if channel_name.is_empty() {
                format!("#{}", e.session_id)
            } else {
                ella_channel_short_label(channel_name)
            };
            names.insert((e.provider_id, e.session_id), label);
        }
    }
    let sampled = object_rate_sample_sessions(prof, provider_id);
    let mut open: FxHashMap<(u16, u64, u32), f64> = FxHashMap::default();
    let mut rx_boxes: EllaObjectRateBoxes = FxHashMap::default();
    let mut tx_boxes: EllaObjectRateBoxes = FxHashMap::default();
    for b in sample_object_rate_boxes(prof, |pid, sid| sampled.contains(&(pid, sid)), t0_us) {
        let Some(title) = names.get(&(b.provider_id, b.session_id)).cloned() else {
            continue;
        };
        let dest = if ella_side(prof, b.provider_id, b.instance_id) == ELLA_SIDE_UPSTREAM {
            &mut rx_boxes
        } else {
            &mut tx_boxes
        };
        dest.entry((title, b.provider_id, b.session_id, b.instance_id))
            .or_default()
            .push((b.t0, b.t1, b.mbps));
    }
    for e in &prof.events {
        if !pid_ok(provider_id, e.provider_id) {
            continue;
        }
        if sampled.contains(&(e.provider_id, e.session_id)) {
            continue;
        }
        match &e.payload {
            EllaPayload::SessionCreate { channel_name, .. } => {
                let label = if channel_name.is_empty() {
                    format!("#{}", e.session_id)
                } else {
                    ella_channel_short_label(channel_name)
                };
                names.insert((e.provider_id, e.session_id), label);
            }
            EllaPayload::ObjectState {
                state,
                instance_id,
                bytes,
                ..
            } => {
                let t = e.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
                let key = (e.provider_id, e.session_id, *instance_id);
                match *state {
                    ELLA_OBJECT_STARTED => {
                        open.entry(key).or_insert(t);
                    }
                    ELLA_OBJECT_COMPLETED | ELLA_OBJECT_ABANDONED => {
                        let Some(t_start) = open.remove(&key) else {
                            continue;
                        };
                        if *bytes == 0 {
                            continue;
                        }
                        if !object_interval_ok(t_start, t) {
                            continue;
                        }
                        let dt = (t - t_start).max(1e-6);
                        let mbps = *bytes as f64 * 8.0 / 1_000_000.0 / dt;
                        let title = names
                            .get(&(e.provider_id, e.session_id))
                            .cloned()
                            .unwrap_or_else(|| format!("#{}", e.session_id));
                        let dest =
                            if ella_side(prof, e.provider_id, *instance_id) == ELLA_SIDE_UPSTREAM {
                                &mut rx_boxes
                            } else {
                                &mut tx_boxes
                            };
                        dest.entry((title, e.provider_id, e.session_id, *instance_id))
                            .or_default()
                            .push((t_start, t, mbps));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    (
        rate_stack_from_boxes(rx_boxes, title_rank),
        rate_stack_from_boxes(tx_boxes, title_rank),
    )
}

/// Subscribers / RX / TX for one node, same peak-subscriber title order.
pub fn build_ella_node_title_stacks(
    prof: &EllaProfile,
    t0_us: u64,
    provider_id: u16,
) -> (EllaSubStackData, EllaSubStackData, EllaSubStackData) {
    let occ_pts = ella_subscription_pts(prof, t0_us, Some(provider_id));
    let rank = occupancy_title_rank(&occ_pts);
    let stack = named_stack_from_pts(occ_pts, Some(&rank));
    let (rx, tx) = build_ella_object_rate_stacks_for(prof, t0_us, Some(provider_id), Some(&rank));
    (stack, rx, tx)
}

fn rate_stack_from_boxes(
    boxes: EllaObjectRateBoxes,
    rank: Option<&FxHashMap<String, u32>>,
) -> EllaSubStackData {
    let mut title_parts: FxHashMap<String, Vec<Vec<[f64; 2]>>> = FxHashMap::default();
    for ((title, _, _, _), inst_boxes) in boxes {
        title_parts
            .entry(title)
            .or_default()
            .push(object_rate_instance_steps(inst_boxes));
    }
    let mut pts: FxHashMap<String, Vec<[f64; 2]>> = FxHashMap::default();
    for (title, parts) in title_parts {
        pts.insert(title, sum_step_series(&parts));
    }
    named_stack_from_pts(pts, rank)
}

/// Last path segment of a channel name, query string stripped.
pub fn ella_channel_short_label(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    let no_q = name.split('?').next().unwrap_or(name);
    no_q.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(no_q)
        .to_string()
}

/// Distinct `channel_id` values, without the instance count.
pub fn ella_channel_ids_label<K, S>(ids: &std::collections::HashMap<K, u32, S>) -> String {
    if ids.is_empty() {
        return "-".to_string();
    }
    let mut unique: Vec<u32> = ids.values().copied().collect();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() == 1 {
        format!("{}", unique[0])
    } else {
        format!("{} ids", unique.len())
    }
}

/// Channel-tab chip: ids label plus instance count when there is more
/// than one Fanout instance.
pub fn ella_compact_channel_ids<K, S>(ids: &std::collections::HashMap<K, u32, S>) -> String {
    if ids.is_empty() {
        return "-".to_string();
    }
    let n = ids.len();
    let label = ella_channel_ids_label(ids);
    if n == 1 {
        label
    } else {
        format!("{label} · {n} inst")
    }
}

/// One row in the grouped ELLA channel table (GUI + `elog_cli ella`).
pub struct EllaChannelListRow {
    pub provider_id: u16,
    pub session_id: u64,
    pub node: String,
    pub channel_name: String,
    pub short_label: String,
    pub version: u8,
    pub session_count: usize,
    pub event_count: usize,
    /// `CHANNEL_ERROR` count across reincarnations of this title.
    pub error_count: usize,
    pub peak_subscribers: u32,
    /// Time-weighted mode of object Mbps: TX on Server, RX otherwise.
    pub rx_rate_mbps: Option<f64>,
    /// Fraction of frames >1.25× reference: TX on Server, RX otherwise.
    pub stretch_frac: Option<f64>,
    /// Byte-weighted frame Mbps: actual send on Server, recv on Relay.
    pub frame_rate_mbps: Option<f64>,
    pub duration_s: f64,
    pub state: &'static str,
    pub ids_label: String,
    pub instance_count: usize,
}

/// Histogram grain for [`time_weighted_mode_mbps`]. Streaming encodes
/// sit on a coarse ladder; 0.1 Mbps keeps 3.6 vs 4.3 distinct while
/// still folding GOP-to-GOP jitter onto one bin.
const RX_RATE_BIN_MBPS: f64 = 0.1;

#[derive(Default, Clone, Copy)]
struct FrameListAcc {
    frames: u64,
    stretched: u64,
    len: u64,
    actual_us: u64,
}

impl FrameListAcc {
    fn add(&mut self, s: &EllaFrameSample) {
        self.frames += u64::from(s.frames);
        self.stretched += u64::from(s.frames_stretched);
        self.len += u64::from(s.sum_frame_len);
        self.actual_us += u64::from(s.sum_actual_us);
    }

    fn merge(&mut self, other: FrameListAcc) {
        self.frames += other.frames;
        self.stretched += other.stretched;
        self.len += other.len;
        self.actual_us += other.actual_us;
    }

    fn stretch_frac(self) -> Option<f64> {
        if self.frames == 0 {
            None
        } else {
            Some(self.stretched as f64 / self.frames as f64)
        }
    }

    fn frame_rate_mbps(self) -> Option<f64> {
        frame_mbps(self.len, self.actual_us)
    }
}

fn ella_list_frame_side(prof: &EllaProfile, provider_id: u16) -> u8 {
    if prof.node_role(provider_id) == EllaNodeRole::Server {
        ELLA_SIDE_DOWNSTREAM
    } else {
        ELLA_SIDE_UPSTREAM
    }
}

fn ella_channel_frame_list_acc(prof: &EllaProfile) -> FxHashMap<(u16, u64), FrameListAcc> {
    let mut out: FxHashMap<(u16, u64), FrameListAcc> = FxHashMap::default();
    let mut event_sessions: FxHashSet<(u16, u64)> = FxHashSet::default();
    for e in &prof.events {
        let instance_id = match &e.payload {
            EllaPayload::FrameSent { instance_id, .. }
            | EllaPayload::FrameRelay { instance_id, .. } => *instance_id,
            _ => continue,
        };
        if ella_side(prof, e.provider_id, instance_id) != ella_list_frame_side(prof, e.provider_id)
        {
            continue;
        }
        event_sessions.insert((e.provider_id, e.session_id));
        let acc = out.entry((e.provider_id, e.session_id)).or_default();
        acc.frames += 1;
        match &e.payload {
            EllaPayload::FrameSent {
                frame_len,
                actual_send_us,
                ..
            } => {
                acc.len += u64::from(*frame_len);
                acc.actual_us += u64::from(*actual_send_us);
            }
            EllaPayload::FrameRelay {
                frame_len, recv_us, ..
            } => {
                acc.len += u64::from(*frame_len);
                acc.actual_us += u64::from(*recv_us);
            }
            _ => {}
        }
    }
    for s in &prof.frame_samples {
        if event_sessions.contains(&(s.provider_id, s.session_id)) {
            continue;
        }
        if ella_side(prof, s.provider_id, s.instance_id)
            != ella_list_frame_side(prof, s.provider_id)
        {
            continue;
        }
        out.entry((s.provider_id, s.session_id)).or_default().add(s);
    }
    out
}

fn ella_channel_object_rate_bins(
    prof: &EllaProfile,
    side: u8,
) -> FxHashMap<(u16, u64), FxHashMap<i64, f64>> {
    let sampled = object_rate_sample_sessions(prof, None);
    let mut open: FxHashMap<(u16, u64, u32), u64> = FxHashMap::default();
    let mut bins: FxHashMap<(u16, u64), FxHashMap<i64, f64>> = FxHashMap::default();
    for b in sample_object_rate_boxes(prof, |pid, sid| sampled.contains(&(pid, sid)), 0) {
        if ella_side(prof, b.provider_id, b.instance_id) != side || b.mbps <= 0.0 {
            continue;
        }
        let bin = (b.mbps / RX_RATE_BIN_MBPS).round() as i64;
        *bins
            .entry((b.provider_id, b.session_id))
            .or_default()
            .entry(bin)
            .or_insert(0.0) += b.dt;
    }
    for e in &prof.events {
        if sampled.contains(&(e.provider_id, e.session_id)) {
            continue;
        }
        let EllaPayload::ObjectState {
            state,
            instance_id,
            bytes,
            ..
        } = &e.payload
        else {
            continue;
        };
        if ella_side(prof, e.provider_id, *instance_id) != side {
            continue;
        }
        let key = (e.provider_id, e.session_id, *instance_id);
        match *state {
            ELLA_OBJECT_STARTED => {
                open.entry(key).or_insert(e.ts);
            }
            ELLA_OBJECT_COMPLETED | ELLA_OBJECT_ABANDONED => {
                let Some(t0) = open.remove(&key) else {
                    continue;
                };
                if *bytes == 0 {
                    continue;
                }
                let dt_us = e.ts.saturating_sub(t0);
                if dt_us < MIN_RATE_DT_US {
                    continue;
                }
                let dt = dt_us as f64 / 1_000_000.0;
                if dt > MAX_OBJECT_SPAN_S {
                    continue;
                }
                let mbps = *bytes as f64 * 8.0 / 1_000_000.0 / dt;
                if mbps <= 0.0 {
                    continue;
                }
                let b = (mbps / RX_RATE_BIN_MBPS).round() as i64;
                *bins
                    .entry((e.provider_id, e.session_id))
                    .or_default()
                    .entry(b)
                    .or_insert(0.0) += dt;
            }
            _ => {}
        }
    }
    bins
}

fn mode_from_rate_bins(bins: &FxHashMap<i64, f64>) -> Option<f64> {
    let (&bin, _) = bins
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1).then(a.0.cmp(b.0)))?;
    Some(bin as f64 * RX_RATE_BIN_MBPS)
}

/// Format the channel-list Rate (`-` when no matching object bytes).
pub fn format_ella_rx_rate(mbps: Option<f64>) -> String {
    match mbps {
        None => "-".into(),
        Some(v) if v < 0.05 => "<0.1 Mbps".into(),
        Some(v) if (v - v.round()).abs() < 0.05 => format!("{} Mbps", v.round() as i64),
        Some(v) => format!("{v:.1} Mbps"),
    }
}

/// Format the channel-list Stretch (`-` when no `FRAME_SAMPLE`).
pub fn format_ella_stretch(frac: Option<f64>) -> String {
    match frac {
        None => "-".into(),
        Some(v) if v < 0.005 => "0%".into(),
        Some(v) => {
            let pct = v * 100.0;
            if (pct - pct.round()).abs() < 0.05 {
                format!("{}%", pct.round() as i64)
            } else {
                format!("{pct:.1}%")
            }
        }
    }
}

/// Peak and time-weighted mean of a last-value occupancy step series.
pub fn occupancy_peak_avg(data: &[[f64; 2]]) -> (f64, Option<f64>) {
    let peak = series_peak(data);
    let mut area = 0.0;
    let mut dur = 0.0;
    for w in data.windows(2) {
        let dt = w[1][0] - w[0][0];
        if dt <= 0.0 {
            continue;
        }
        area += w[0][1] * dt;
        dur += dt;
    }
    let avg = if dur > 0.0 { Some(area / dur) } else { None };
    (peak, avg)
}

fn format_ella_sub_avg(avg: Option<f64>) -> String {
    match avg {
        None => "-".into(),
        Some(v) if v < 0.05 => "0".into(),
        Some(v) if (v - v.round()).abs() < 0.05 => format!("{}", v.round() as i64),
        Some(v) => format!("{v:.1}"),
    }
}

/// Stats for a Fanout color-key hover (rate from the Session List,
/// occupancy from that node's External stack).
#[derive(Clone, Copy, Default)]
pub struct EllaStackLegendStats {
    pub rate_mbps: Option<f64>,
    pub peak_external: Option<u32>,
    pub avg_external: Option<f64>,
}

/// Color-key hover: Session List Rate, External peak/avg, click hint.
pub fn ella_stack_legend_hover(label: &str, stats: EllaStackLegendStats) -> String {
    let peak = stats
        .peak_external
        .map(|p| p.to_string())
        .unwrap_or_else(|| "-".into());
    format!(
        "Open {label}  ·  {}\npeak {peak}  ·  avg {} external",
        format_ella_rx_rate(stats.rate_mbps),
        format_ella_sub_avg(stats.avg_external),
    )
}

/// Title of a channel session, the key that folds reincarnations — and
/// the same title on another node — into one row.
fn ella_channel_title(c: &EllaChannelInfo) -> String {
    if c.channel_name.is_empty() {
        format!("#{}", c.session_id)
    } else {
        c.channel_name.clone()
    }
}

/// Longest reincarnation first, then busiest, then lowest session id.
/// The winner is the row's click target and stats source.
fn ella_longest_first(a: &EllaChannelInfo, b: &EllaChannelInfo) -> std::cmp::Ordering {
    let da = a.last_ts.saturating_sub(a.first_ts);
    let db = b.last_ts.saturating_sub(b.first_ts);
    db.cmp(&da)
        .then(b.event_count.cmp(&a.event_count))
        .then(a.session_id.cmp(&b.session_id))
}

/// Node label for a title carried by one or more nodes, e.g.
/// `Server + Relay`, Server first. Shared by the Session List cell and
/// the channel tab's Node chip so the two agree.
pub fn ella_nodes_label(prof: &EllaProfile, provider_ids: &[u16]) -> String {
    let mut pids: Vec<u16> = provider_ids.to_vec();
    pids.sort_unstable();
    pids.dedup();
    pids.sort_by_key(|pid| (prof.node_role(*pid).rank(), *pid));
    let mut parts: Vec<String> = Vec::new();
    for pid in pids {
        let label = prof.node_label(pid);
        if !parts.contains(&label) {
            parts.push(label);
        }
    }
    parts.join(" + ")
}

/// Group envelope sessions by channel name across every `fanout_node`
/// provider, so a title carried by both Server and Relay is one row.
/// Click target is the longest session of the title on any node.
pub fn build_ella_channel_list(prof: &EllaProfile) -> Vec<EllaChannelListRow> {
    let mut groups: FxHashMap<String, Vec<&EllaChannelInfo>> = FxHashMap::default();
    for c in prof.channels.values() {
        groups.entry(ella_channel_title(c)).or_default().push(c);
    }
    let rx_bins = ella_channel_object_rate_bins(prof, ELLA_SIDE_UPSTREAM);
    let tx_bins = ella_channel_object_rate_bins(prof, ELLA_SIDE_DOWNSTREAM);
    let frame_acc = ella_channel_frame_list_acc(prof);
    let mut errors: FxHashMap<(u16, u64), usize> = FxHashMap::default();
    for e in &prof.events {
        if matches!(e.payload, EllaPayload::ChannelError { .. }) {
            *errors.entry((e.provider_id, e.session_id)).or_insert(0) += 1;
        }
    }
    let mut rows: Vec<EllaChannelListRow> = groups
        .into_iter()
        .map(|(name, mut members)| {
            members.sort_by(|a, b| ella_longest_first(a, b));
            let longest = members[0];
            let first_ts = members.iter().map(|c| c.first_ts).min().unwrap_or(0);
            let last_ts = members.iter().map(|c| c.last_ts).max().unwrap_or(0);
            let event_count: usize = members.iter().map(|c| c.event_count).sum();
            let error_count: usize = members
                .iter()
                .map(|c| {
                    errors
                        .get(&(c.provider_id, c.session_id))
                        .copied()
                        .unwrap_or(0)
                })
                .sum();
            let peak = members
                .iter()
                .map(|c| c.peak_subscribers)
                .max()
                .unwrap_or(0);
            // Instance ids repeat across nodes (both have an i0), so the
            // union is keyed by node too.
            let mut ids: FxHashMap<(u16, u32), u32> = FxHashMap::default();
            for c in &members {
                for (inst, id) in &c.channel_ids {
                    ids.insert((c.provider_id, *inst), *id);
                }
            }
            let mut rate_bins: FxHashMap<i64, f64> = FxHashMap::default();
            let mut frames = FrameListAcc::default();
            for c in &members {
                // A Server's one instance is downstream, so its object
                // rate is TX; every other node ingests upstream as RX.
                let src = if prof.node_role(c.provider_id) == EllaNodeRole::Server {
                    &tx_bins
                } else {
                    &rx_bins
                };
                if let Some(b) = src.get(&(c.provider_id, c.session_id)) {
                    for (&bin, &dt) in b {
                        *rate_bins.entry(bin).or_insert(0.0) += dt;
                    }
                }
                if let Some(acc) = frame_acc.get(&(c.provider_id, c.session_id)) {
                    frames.merge(*acc);
                }
            }
            let short = if longest.channel_name.is_empty() {
                name.clone()
            } else {
                ella_channel_short_label(&name)
            };
            EllaChannelListRow {
                provider_id: longest.provider_id,
                session_id: longest.session_id,
                node: ella_nodes_label(
                    prof,
                    &members.iter().map(|c| c.provider_id).collect::<Vec<_>>(),
                ),
                channel_name: longest.channel_name.clone(),
                short_label: short,
                version: longest.version,
                session_count: members.len(),
                event_count,
                error_count,
                peak_subscribers: peak,
                rx_rate_mbps: mode_from_rate_bins(&rate_bins),
                stretch_frac: frames.stretch_frac(),
                frame_rate_mbps: frames.frame_rate_mbps(),
                duration_s: last_ts.saturating_sub(first_ts) as f64 / 1_000_000.0,
                state: longest.end_state_name(),
                ids_label: ella_channel_ids_label(&ids),
                instance_count: ids.len(),
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.peak_subscribers
            .cmp(&a.peak_subscribers)
            .then(a.short_label.cmp(&b.short_label))
    });
    rows
}

/// Envelope session a Fanout-by-Channel stack band should open, same
/// pick as the Session List (longest reincarnation of that short name).
pub fn ella_channel_for_stack_label(prof: &EllaProfile, label: &str) -> Option<(u16, u64)> {
    if label.is_empty() || label == "Other" {
        return None;
    }
    build_ella_channel_list(prof)
        .into_iter()
        .find(|r| r.short_label == label)
        .map(|r| (r.provider_id, r.session_id))
}

/// Sessions one channel tab draws together: every reincarnation of this
/// title on each `fanout_node` provider that carried it, Server first,
/// then by start time. `(provider_id, session_id)` is always among them.
pub fn ella_channel_tab_members(
    prof: &EllaProfile,
    provider_id: u16,
    session_id: u64,
) -> Vec<(u16, u64)> {
    let Some(seed) = prof.channels.get(&(provider_id, session_id)) else {
        return vec![(provider_id, session_id)];
    };
    let title = ella_channel_title(seed);
    let mut out: Vec<(u16, u64)> = prof
        .channels
        .values()
        .filter(|c| ella_channel_title(c) == title)
        .map(|c| (c.provider_id, c.session_id))
        .collect();
    if !out.contains(&(provider_id, session_id)) {
        out.push((provider_id, session_id));
    }
    out.sort_by(|&(pa, sa), &(pb, sb)| {
        let ta = prof
            .channels
            .get(&(pa, sa))
            .map(|c| c.first_ts)
            .unwrap_or(0);
        let tb = prof
            .channels
            .get(&(pb, sb))
            .map(|c| c.first_ts)
            .unwrap_or(0);
        prof.node_role(pa)
            .rank()
            .cmp(&prof.node_role(pb).rank())
            .then(pa.cmp(&pb))
            .then(ta.cmp(&tb))
            .then(sa.cmp(&sb))
    });
    out
}

fn ella_tab_multi_node(members: &[(u16, u64)]) -> bool {
    members
        .iter()
        .map(|(pid, _)| *pid)
        .collect::<FxHashSet<_>>()
        .len()
        > 1
}

/// Fold same-label series onto one line. A gap between reincarnations
/// drops to 0 so the plot does not draw a diagonal across idle time.
fn coalesce_channel_series(series: Vec<EllaChannelSubSeries>) -> Vec<EllaChannelSubSeries> {
    let mut order: Vec<String> = Vec::new();
    let mut by_label: FxHashMap<String, Vec<[f64; 2]>> = FxHashMap::default();
    for s in series {
        if !order.iter().any(|l| l == &s.label) {
            order.push(s.label.clone());
        }
        append_series_with_zero_gap(by_label.entry(s.label).or_default(), &s.data);
    }
    order
        .into_iter()
        .filter_map(|label| {
            let data = by_label.remove(&label)?;
            (!data.is_empty()).then_some(EllaChannelSubSeries { label, data })
        })
        .collect()
}

fn append_series_with_zero_gap(dst: &mut Vec<[f64; 2]>, src: &[[f64; 2]]) {
    if src.is_empty() {
        return;
    }
    if let Some(&[t_last, y_last]) = dst.last() {
        let t_next = src[0][0];
        if t_next > t_last {
            if y_last != 0.0 {
                dst.push([t_last, 0.0]);
            }
            if src[0][1] != 0.0 {
                dst.push([t_next, 0.0]);
            }
        }
    }
    dst.extend_from_slice(src);
}

/// Name a channel-tab series `<node> - <side>`, so a merged tab's
/// Server and Relay lines stay apart and read node-first.
fn ella_node_series_label(label: &str, node: &str) -> String {
    format!("{node} - {label}")
}

/// One channel tab's plots, with every node's view of the title on the
/// same axes. Series labels carry `· <node>` when the title spans more
/// than one provider.
pub fn build_ella_channel_plot_for(
    prof: &EllaProfile,
    members: &[(u16, u64)],
    t0_us: u64,
) -> EllaChannelPlotData {
    let mut out = EllaChannelPlotData {
        subscribers: Vec::new(),
        objects: Vec::new(),
        object_rate: Vec::new(),
        object_timing: Vec::new(),
        frame_rate: Vec::new(),
        frame_duration: Vec::new(),
        frame_ratio: Vec::new(),
        frame_ratio_fallback: false,
        object_id: Vec::new(),
        object_duration: Vec::new(),
        object_size: Vec::new(),
        object_spans: Vec::new(),
        object_span_copies: Vec::new(),
        frame_spans: Vec::new(),
        errors: Vec::new(),
        markers: Vec::new(),
    };
    let qualify = ella_tab_multi_node(members);
    let srcs = ella_member_sources(prof, members);
    for &(pid, sid) in members {
        let Some(src) = srcs.get(&(pid, sid)) else {
            continue;
        };
        let mut d = build_ella_channel_plot_from(prof, pid, t0_us, src);
        if qualify {
            let node = prof.node_label(pid);
            for s in d
                .subscribers
                .iter_mut()
                .chain(&mut d.objects)
                .chain(&mut d.object_rate)
                .chain(&mut d.object_timing)
                .chain(&mut d.frame_rate)
                .chain(&mut d.frame_duration)
                .chain(&mut d.frame_ratio)
            {
                s.label = ella_node_series_label(&s.label, &node);
            }
            for s in &mut d.errors {
                s.label = ella_node_series_label(&s.label, &node);
            }
        }
        out.subscribers.extend(d.subscribers);
        out.objects.extend(d.objects);
        out.object_rate.extend(d.object_rate);
        out.object_timing.extend(d.object_timing);
        out.frame_rate.extend(d.frame_rate);
        out.frame_duration.extend(d.frame_duration);
        out.frame_ratio.extend(d.frame_ratio);
        out.frame_ratio_fallback |= d.frame_ratio_fallback;
        out.object_spans.extend(d.object_spans);
        out.frame_spans.extend(d.frame_spans);
        out.errors.extend(d.errors);
        out.markers.extend(d.markers);
    }
    out.subscribers = coalesce_channel_series(out.subscribers);
    out.objects = coalesce_channel_series(out.objects);
    out.object_rate = coalesce_channel_series(out.object_rate);
    out.object_timing = coalesce_channel_series(out.object_timing);
    out.frame_rate = coalesce_channel_series(out.frame_rate);
    out.frame_duration = coalesce_channel_series(out.frame_duration);
    out.frame_ratio = coalesce_channel_series(out.frame_ratio);
    out.errors = {
        let folded = coalesce_channel_series(
            out.errors
                .into_iter()
                .map(|s| EllaChannelSubSeries {
                    label: s.label,
                    data: s.data,
                })
                .collect(),
        );
        folded
            .into_iter()
            .map(|s| EllaNamedSeries {
                label: s.label,
                data: s.data,
            })
            .collect()
    };
    out.markers.sort_by(|a, b| a.t.total_cmp(&b.t));
    merge_object_spans(&mut out.object_spans);
    out.object_span_copies = out.object_spans.clone();
    attach_frames_to_objects(&mut out.frame_spans, &out.object_span_copies);
    place_origin_frames(prof, &mut out.frame_spans, &out.object_span_copies);
    number_relay_frame_index(prof, &mut out.frame_spans);
    out.object_spans =
        object_spans_at_percentile(prof, &out.object_span_copies, EllaObjectPercentile::Median);
    let (object_id, _, _) = object_span_plots(&out.object_spans, |pid| prof.node_label(pid));
    let (object_duration, object_size) =
        object_copy_percentile_series(prof, &out.object_span_copies);
    out.object_id = object_id;
    out.object_duration = object_duration;
    out.object_size = object_size;
    if !out.object_span_copies.is_empty() {
        out.object_rate = object_rate_at_percentile(
            prof,
            &out.object_span_copies,
            EllaObjectPercentile::Median,
            qualify,
        );
    }
    out
}

fn object_span_plots(
    spans: &[EllaObjectSpan],
    mut node_of: impl FnMut(u16) -> String,
) -> (
    Vec<EllaChannelSubSeries>,
    Vec<EllaChannelSubSeries>,
    Vec<EllaChannelSubSeries>,
) {
    let mut clipped = spans.to_vec();
    clip_overlapping_spans(&mut clipped);
    let mut ids: FxHashMap<u16, (String, Vec<[f64; 2]>)> = FxHashMap::default();
    let mut durs: FxHashMap<u16, (String, Vec<[f64; 2]>)> = FxHashMap::default();
    let mut sizes: FxHashMap<u16, (String, Vec<[f64; 2]>)> = FxHashMap::default();
    let mut order: Vec<u16> = Vec::new();
    for s in &clipped {
        if !order.contains(&s.provider_id) {
            order.push(s.provider_id);
            let label = node_of(s.provider_id);
            ids.insert(s.provider_id, (label.clone(), Vec::new()));
            durs.insert(s.provider_id, (label.clone(), Vec::new()));
            sizes.insert(s.provider_id, (label, Vec::new()));
        }
    }
    sequential_span_series(&clipped, &mut ids, &mut durs, &mut sizes);
    for (_, pts) in durs.values_mut() {
        trim_trailing_collapse(pts);
    }
    for (_, pts) in sizes.values_mut() {
        trim_trailing_collapse(pts);
    }
    let to_series = |map: FxHashMap<u16, (String, Vec<[f64; 2]>)>| {
        order
            .iter()
            .filter_map(|pid| {
                let (label, data) = map.get(pid)?;
                if data.is_empty() {
                    None
                } else {
                    Some(EllaChannelSubSeries {
                        label: label.clone(),
                        data: data.clone(),
                    })
                }
            })
            .collect()
    };
    (to_series(ids), to_series(durs), to_series(sizes))
}

/// Duration/Size/id y follow the same bar as Object Timeline: one GOP
/// at a time, y = that bar's width / bytes / id.
fn sequential_span_series(
    spans: &[EllaObjectSpan],
    ids: &mut FxHashMap<u16, (String, Vec<[f64; 2]>)>,
    durs: &mut FxHashMap<u16, (String, Vec<[f64; 2]>)>,
    sizes: &mut FxHashMap<u16, (String, Vec<[f64; 2]>)>,
) {
    let mut by_pid: FxHashMap<u16, Vec<&EllaObjectSpan>> = FxHashMap::default();
    for s in spans {
        by_pid.entry(s.provider_id).or_default().push(s);
    }
    for (pid, mut row) in by_pid {
        row.sort_by(|a, b| {
            a.t0.total_cmp(&b.t0)
                .then(span_state_rank(b.state).cmp(&span_state_rank(a.state)))
                .then(span_duration(a).total_cmp(&span_duration(b)))
        });
        let mut i = 0usize;
        while i < row.len() {
            let s = row[i];
            while i + 1 < row.len() && row[i + 1].t0 == s.t0 {
                i += 1;
            }
            let dt = span_duration(s);
            if dt > 0.0 {
                if let Some((_, pts)) = durs.get_mut(&pid) {
                    push_step_changed(pts, s.t0, dt);
                }
                if let Some((_, pts)) = sizes.get_mut(&pid) {
                    push_step_changed(pts, s.t0, s.bytes as f64 / OBJECT_SIZE_MB);
                }
                if let Some(id) = s.object_id {
                    if let Some((_, pts)) = ids.get_mut(&pid) {
                        push_step_changed(pts, s.t0, id as f64);
                    }
                }
            }
            let next_t0 = row.get(i + 1).map(|n| n.t0);
            let hold = next_t0.is_some_and(|n0| n0 - s.t1 <= OBJECT_GOP_JOIN_S);
            if !hold {
                if let Some((_, pts)) = durs.get_mut(&pid) {
                    push_step_changed(pts, s.t1, 0.0);
                }
                if let Some((_, pts)) = sizes.get_mut(&pid) {
                    push_step_changed(pts, s.t1, 0.0);
                }
            }
            i += 1;
        }
    }
}

/// Payload bytes as plotted on Object Size.
const OBJECT_SIZE_MB: f64 = 1_000_000.0;

/// Drop end-of-capture GOPs whose y is far below the rest so Duration
/// and Size y-axes stay on the typical object, not the teardown cliff.
const OBJECT_TRAIL_FRAC: f64 = 0.5;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EllaObjectPercentile {
    Median,
    P75,
    P95,
    Max,
}

impl EllaObjectPercentile {
    pub const ALL: [Self; 4] = [Self::Median, Self::P75, Self::P95, Self::Max];

    pub fn label(self) -> &'static str {
        match self {
            Self::Median => "Median",
            Self::P75 => "P75",
            Self::P95 => "P95",
            Self::Max => "Max",
        }
    }

    pub fn as_p(self) -> f64 {
        match self {
            Self::Median => 0.50,
            Self::P75 => 0.75,
            Self::P95 => 0.95,
            Self::Max => 1.0,
        }
    }
}

/// Timeline y-axis: Server is one row; Relay splits Internal / External.
const OBJECT_LANE_SERVER: u8 = 0xFF;

pub fn object_timeline_lane(prof: &EllaProfile, provider_id: u16, instance_id: u32) -> (u16, u8) {
    if prof.node_role(provider_id) == EllaNodeRole::Server {
        (provider_id, OBJECT_LANE_SERVER)
    } else {
        (provider_id, ella_side(prof, provider_id, instance_id))
    }
}

pub fn object_timeline_lane_sort_key(pid: u16, side: u8) -> (u8, u16) {
    let face = if side == OBJECT_LANE_SERVER {
        2
    } else if side == ELLA_SIDE_UPSTREAM {
        1
    } else {
        0
    };
    (face, pid)
}

pub fn object_timeline_lane_label(prof: &EllaProfile, pid: u16, side: u8) -> String {
    match prof.node_role(pid) {
        EllaNodeRole::Server => prof.node_label(pid),
        EllaNodeRole::Relay => {
            if side == ELLA_SIDE_UPSTREAM {
                "Relay Internal".into()
            } else {
                "Relay External".into()
            }
        }
        EllaNodeRole::Other => {
            let node = prof.node_label(pid);
            if side == ELLA_SIDE_UPSTREAM {
                format!("{node} Internal")
            } else {
                format!("{node} External")
            }
        }
    }
}

fn object_lane_is_external(prof: &EllaProfile, pid: u16, side: u8) -> bool {
    prof.node_role(pid) != EllaNodeRole::Server && side == ELLA_SIDE_DOWNSTREAM
}

pub fn object_has_external_relay(prof: &EllaProfile, copies: &[EllaObjectSpan]) -> bool {
    copies.iter().any(|s| {
        let (_, side) = object_timeline_lane(prof, s.provider_id, s.instance_id);
        object_lane_is_external(prof, s.provider_id, side)
    })
}

fn object_face_series_label(prof: &EllaProfile, pid: u16, side: u8, qualify: bool) -> String {
    let face = if side == OBJECT_LANE_SERVER {
        "Server".to_string()
    } else if side == ELLA_SIDE_UPSTREAM {
        "Internal".to_string()
    } else {
        "External".to_string()
    };
    if qualify && prof.node_role(pid) != EllaNodeRole::Server {
        ella_node_series_label(&face, &prof.node_label(pid))
    } else {
        face
    }
}

/// Object Duration / Size: hide P75, P95, and Max until the user turns
/// them on. Median (and Server) start visible.
pub fn ella_object_legend_hidden_by_default(label: &str) -> bool {
    matches!(
        label.rsplit(" - ").next().unwrap_or(label),
        "P75" | "P95" | "Max"
    )
}

/// Server and Internal keep the median instance; External uses `kind`
/// among the N workers. Each face is its own Timeline lane.
pub fn object_spans_at_percentile(
    prof: &EllaProfile,
    copies: &[EllaObjectSpan],
    kind: EllaObjectPercentile,
) -> Vec<EllaObjectSpan> {
    if copies.is_empty() {
        return Vec::new();
    }
    let mut groups: FxHashMap<(u16, u8), Vec<&EllaObjectSpan>> = FxHashMap::default();
    let mut keys: Vec<(u16, u8)> = Vec::new();
    for s in copies {
        let k = object_timeline_lane(prof, s.provider_id, s.instance_id);
        let e = groups.entry(k).or_default();
        if e.is_empty() {
            keys.push(k);
        }
        e.push(s);
    }
    let mut out = Vec::new();
    for k in keys {
        let g = groups.remove(&k).unwrap_or_default();
        let p = if object_lane_is_external(prof, k.0, k.1) {
            kind.as_p()
        } else {
            0.50
        };
        let inst = pick_percentile_instance(&g, p);
        out.extend(
            g.iter()
                .filter(|s| s.instance_id == inst)
                .map(|s| (*s).clone()),
        );
    }
    out.sort_by(|a, b| {
        a.provider_id
            .cmp(&b.provider_id)
            .then(a.t0.total_cmp(&b.t0))
            .then(a.obj_idx.cmp(&b.obj_idx))
            .then(a.instance_id.cmp(&b.instance_id))
    });
    out
}

fn pick_percentile_instance(spans: &[&EllaObjectSpan], p: f64) -> u32 {
    let mut by_inst: FxHashMap<u32, Vec<&EllaObjectSpan>> = FxHashMap::default();
    let mut insts: Vec<u32> = Vec::new();
    for s in spans {
        let e = by_inst.entry(s.instance_id).or_default();
        if e.is_empty() {
            insts.push(s.instance_id);
        }
        e.push(*s);
    }
    if insts.len() == 1 {
        return insts[0];
    }
    let any_completed = spans
        .iter()
        .any(|s| s.state == EllaObjectSpanState::Completed);
    let mut scored: Vec<(f64, u32)> = Vec::new();
    for inst in &insts {
        let g = by_inst.get(inst).map(Vec::as_slice).unwrap_or(&[]);
        let completed: Vec<&EllaObjectSpan> = g
            .iter()
            .copied()
            .filter(|s| s.state == EllaObjectSpanState::Completed)
            .collect();
        if any_completed && completed.is_empty() {
            continue;
        }
        let pool: Vec<&EllaObjectSpan> = if completed.is_empty() {
            g.to_vec()
        } else {
            completed
        };
        let mut dts: Vec<f64> = pool
            .iter()
            .map(|s| span_duration(s))
            .filter(|d| *d > 0.0)
            .collect();
        if dts.is_empty() {
            continue;
        }
        dts.sort_by(|a, b| a.total_cmp(b));
        scored.push((copy_percentile(&dts, p), *inst));
    }
    if scored.is_empty() {
        return insts[0];
    }
    scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let n = scored.len();
    let k = if p >= 1.0 {
        n
    } else {
        ((p * n as f64).ceil() as usize).max(1).min(n)
    };
    scored[k - 1].1
}

/// Greedy interval partition per node: overlapping spans get distinct
/// sub-lanes so the Timeline can stack them.
pub fn object_span_sublanes(spans: &[EllaObjectSpan]) -> Vec<u32> {
    object_span_sublanes_by(spans, |s| s.provider_id as u64)
}

pub fn object_timeline_sublanes(prof: &EllaProfile, spans: &[EllaObjectSpan]) -> Vec<u32> {
    object_span_sublanes_by(spans, |s| {
        let (pid, side) = object_timeline_lane(prof, s.provider_id, s.instance_id);
        ((pid as u64) << 8) | u64::from(side)
    })
}

fn object_span_sublanes_by(
    spans: &[EllaObjectSpan],
    mut key_of: impl FnMut(&EllaObjectSpan) -> u64,
) -> Vec<u32> {
    let mut out = vec![0u32; spans.len()];
    let mut keys: Vec<u64> = Vec::new();
    let mut key_at: Vec<u64> = Vec::with_capacity(spans.len());
    for s in spans {
        let k = key_of(s);
        key_at.push(k);
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    for key in keys {
        let mut idxs: Vec<usize> = key_at
            .iter()
            .enumerate()
            .filter(|(_, k)| **k == key)
            .map(|(i, _)| i)
            .collect();
        idxs.sort_by(|&i, &j| {
            spans[i]
                .t0
                .total_cmp(&spans[j].t0)
                .then(spans[i].obj_idx.cmp(&spans[j].obj_idx))
                .then(spans[i].instance_id.cmp(&spans[j].instance_id))
        });
        let mut ends: Vec<f64> = Vec::new();
        for i in idxs {
            let t0 = spans[i].t0;
            let slot = ends.iter().position(|&end| end <= t0);
            let j = match slot {
                Some(j) => j,
                None => {
                    ends.push(0.0);
                    ends.len() - 1
                }
            };
            out[i] = j as u32;
            ends[j] = spans[i].t1;
        }
    }
    out
}

struct RawFrame {
    instance_id: u32,
    t: f64,
    object_id: u32,
    frame_index: u32,
    first_pn: u32,
    dur_us: u32,
    origin: bool,
    boundary_index: u8,
    toggle: bool,
    bytes: u32,
}

/// A single FDACE frame is tens of milliseconds; longer is a hole.
const MAX_FRAME_SPAN_S: f64 = 2.0;

/// Same-timestamp (or nearly) `FRAME_SENT` rows are a GOP-end burst
/// from older ELLA producers. Per-frame emit is tens of ms apart.
const ORIGIN_BURST_CLUSTER_S: f64 = 0.01;

/// `boundary_index` for an origin frame until `attach_frames_to_objects`
/// fills it from the joined object (relay frames carry their own).
const FRAME_SLOT_UNKNOWN: u8 = u8::MAX;

type OriginFrameRows = Vec<(u32, f64, u32, u32, bool, u32)>;
type ObjectRateBoxes = FxHashMap<(u16, u8), Vec<(f64, f64, f64)>>;

fn layout_frame_spans(provider_id: u16, raw: &[RawFrame]) -> Vec<EllaFrameSpan> {
    let mut origin: FxHashMap<(u32, u32), OriginFrameRows> = FxHashMap::default();
    let mut out = Vec::new();
    for r in raw {
        if r.origin {
            origin
                .entry((r.instance_id, r.object_id))
                .or_default()
                .push((r.frame_index, r.t, r.dur_us, r.first_pn, r.toggle, r.bytes));
            continue;
        }
        let dt = r.dur_us as f64 / 1_000_000.0;
        if dt > 0.0 && dt <= MAX_FRAME_SPAN_S {
            out.push(EllaFrameSpan {
                provider_id,
                instance_id: r.instance_id,
                t0: r.t - dt,
                t1: r.t,
                object_id: 0,
                obj_idx: 0,
                frame_index: 0,
                first_pn: r.first_pn,
                boundary_index: r.boundary_index,
                toggle: r.toggle,
                bytes: r.bytes,
            });
        }
    }
    for ((inst, oid), mut rows) in origin {
        rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        if origin_timestamps_clustered(&rows) {
            let mut t1 = rows.iter().map(|r| r.1).fold(f64::NEG_INFINITY, f64::max);
            for &(idx, _, dur_us, first_pn, toggle, bytes) in rows.iter().rev() {
                let dt = dur_us as f64 / 1_000_000.0;
                if dt <= 0.0 || dt > MAX_FRAME_SPAN_S || !t1.is_finite() {
                    continue;
                }
                let t0 = t1 - dt;
                out.push(EllaFrameSpan {
                    provider_id,
                    instance_id: inst,
                    t0,
                    t1,
                    object_id: oid,
                    obj_idx: 0,
                    frame_index: idx,
                    first_pn,
                    boundary_index: FRAME_SLOT_UNKNOWN,
                    toggle,
                    bytes,
                });
                t1 = t0;
            }
        } else {
            for &(idx, t, dur_us, first_pn, toggle, bytes) in &rows {
                let dt = dur_us as f64 / 1_000_000.0;
                if dt <= 0.0 || dt > MAX_FRAME_SPAN_S {
                    continue;
                }
                out.push(EllaFrameSpan {
                    provider_id,
                    instance_id: inst,
                    t0: t - dt,
                    t1: t,
                    object_id: oid,
                    obj_idx: 0,
                    frame_index: idx,
                    first_pn,
                    boundary_index: FRAME_SLOT_UNKNOWN,
                    toggle,
                    bytes,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.instance_id
            .cmp(&b.instance_id)
            .then(a.t0.total_cmp(&b.t0))
            .then(a.frame_index.cmp(&b.frame_index))
    });
    out
}

fn attach_frames_to_objects(frames: &mut [EllaFrameSpan], objects: &[EllaObjectSpan]) {
    if frames.is_empty() {
        return;
    }
    if objects.is_empty() {
        for f in frames {
            if f.object_id == 0 {
                f.object_id = f.first_pn;
            }
        }
        return;
    }
    let mut by_inst: FxHashMap<(u16, u32), Vec<&EllaObjectSpan>> = FxHashMap::default();
    for o in objects {
        by_inst
            .entry((o.provider_id, o.instance_id))
            .or_default()
            .push(o);
    }
    for list in by_inst.values_mut() {
        list.sort_by(|a, b| a.t0.total_cmp(&b.t0).then(a.first_pn.cmp(&b.first_pn)));
    }
    for f in frames {
        let Some(list) = by_inst.get(&(f.provider_id, f.instance_id)) else {
            continue;
        };
        let matched = if f.object_id != 0 {
            list.iter()
                .copied()
                .find(|o| o.object_id == Some(f.object_id) || o.first_pn == f.first_pn)
        } else {
            None
        };
        let matched = matched.or_else(|| {
            list.iter().copied().enumerate().find_map(|(i, o)| {
                let next_pn = list.get(i + 1).map(|n| n.first_pn).unwrap_or(u32::MAX);
                (f.first_pn >= o.first_pn && f.first_pn < next_pn).then_some(o)
            })
        });
        let matched = matched.or_else(|| {
            let mid = 0.5 * (f.t0 + f.t1);
            list.iter().copied().find(|o| mid >= o.t0 && mid < o.t1)
        });
        if let Some(o) = matched {
            f.obj_idx = o.obj_idx;
            if f.boundary_index == FRAME_SLOT_UNKNOWN {
                f.boundary_index = o.boundary_index;
            }
            if let Some(id) = o.object_id {
                f.object_id = id;
            } else if f.object_id == 0 {
                f.object_id = o.first_pn;
            }
        }
        if f.object_id == 0 {
            f.object_id = f.first_pn;
        }
    }
}

fn origin_timestamps_clustered(rows: &OriginFrameRows) -> bool {
    if rows.len() < 2 {
        return false;
    }
    let min_t = rows.iter().map(|r| r.1).fold(f64::INFINITY, f64::min);
    let max_t = rows.iter().map(|r| r.1).fold(f64::NEG_INFINITY, f64::max);
    max_t - min_t <= ORIGIN_BURST_CLUSTER_S
}

fn place_origin_frames(
    prof: &EllaProfile,
    frames: &mut [EllaFrameSpan],
    objects: &[EllaObjectSpan],
) {
    let mut span: FxHashMap<(u16, u32, u32), (f64, f64)> = FxHashMap::default();
    for o in objects {
        if prof.node_role(o.provider_id) != EllaNodeRole::Server {
            continue;
        }
        let e = span
            .entry((o.provider_id, o.instance_id, o.obj_idx))
            .or_insert((o.t0, o.t1));
        if o.t0 < e.0 {
            e.0 = o.t0;
        }
        if o.t1 > e.1 {
            e.1 = o.t1;
        }
    }
    if span.is_empty() {
        return;
    }
    let mut groups: FxHashMap<(u16, u32, u32), Vec<usize>> = FxHashMap::default();
    for (i, f) in frames.iter().enumerate() {
        if prof.node_role(f.provider_id) != EllaNodeRole::Server {
            continue;
        }
        groups
            .entry((f.provider_id, f.instance_id, f.obj_idx))
            .or_default()
            .push(i);
    }
    for (key, idxs) in &mut groups {
        let Some(&(t_obj, t_end)) = span.get(key) else {
            continue;
        };
        idxs.sort_by(|&i, &j| {
            frames[i]
                .frame_index
                .cmp(&frames[j].frame_index)
                .then(frames[i].t0.total_cmp(&frames[j].t0))
        });
        let min_t0 = idxs
            .iter()
            .map(|&i| frames[i].t0)
            .fold(f64::INFINITY, f64::min);
        let max_t1 = idxs
            .iter()
            .map(|&i| frames[i].t1)
            .fold(f64::NEG_INFINITY, f64::max);
        let packed_at_end = min_t0.is_finite()
            && t_end - max_t1 <= ORIGIN_BURST_CLUSTER_S
            && min_t0 - t_obj > t_end - min_t0;
        if !packed_at_end {
            continue;
        }
        let mut t = t_obj;
        for &i in idxs.iter() {
            let dt = frames[i].t1 - frames[i].t0;
            if dt <= 0.0 {
                continue;
            }
            frames[i].t0 = t;
            frames[i].t1 = t + dt;
            t = frames[i].t1;
        }
    }
}

fn number_relay_frame_index(prof: &EllaProfile, frames: &mut [EllaFrameSpan]) {
    let mut groups: FxHashMap<(u16, u32, u32), Vec<usize>> = FxHashMap::default();
    for (i, f) in frames.iter().enumerate() {
        if prof.node_role(f.provider_id) == EllaNodeRole::Server {
            continue;
        }
        groups
            .entry((f.provider_id, f.instance_id, f.obj_idx))
            .or_default()
            .push(i);
    }
    for idxs in groups.values_mut() {
        idxs.sort_by(|&i, &j| {
            frames[i]
                .first_pn
                .cmp(&frames[j].first_pn)
                .then(frames[i].t0.total_cmp(&frames[j].t0))
        });
        for (k, &i) in idxs.iter().enumerate() {
            frames[i].frame_index = k as u32;
        }
    }
}

/// Frames on the same instances Object Timeline selected.
pub fn frame_spans_at_percentile(
    prof: &EllaProfile,
    frames: &[EllaFrameSpan],
    copies: &[EllaObjectSpan],
    kind: EllaObjectPercentile,
) -> Vec<EllaFrameSpan> {
    if frames.is_empty() {
        return Vec::new();
    }
    let objs = object_spans_at_percentile(prof, copies, kind);
    let mut insts: FxHashSet<(u16, u32)> = FxHashSet::default();
    for s in &objs {
        insts.insert((s.provider_id, s.instance_id));
    }
    if insts.is_empty() {
        let mut counts: FxHashMap<(u16, u32), usize> = FxHashMap::default();
        for f in frames {
            *counts.entry((f.provider_id, f.instance_id)).or_insert(0) += 1;
        }
        let mut best: FxHashMap<u16, (u32, usize)> = FxHashMap::default();
        for ((pid, inst), n) in counts {
            let e = best.entry(pid).or_insert((inst, n));
            if n > e.1 || (n == e.1 && inst < e.0) {
                *e = (inst, n);
            }
        }
        for (pid, (inst, _)) in best {
            insts.insert((pid, inst));
        }
    }
    frames
        .iter()
        .filter(|f| insts.contains(&(f.provider_id, f.instance_id)))
        .cloned()
        .collect()
}

/// GOP stripe: `obj_idx` so Frame Timeline matches Object Timeline.
/// GOP stripe for the Frame Timeline: the object slot (`boundary_index`),
/// which rotates every GOP and survives drops, unlike the joined `obj_idx`.
/// Origin frames not yet matched to an object fall back to `obj_idx`.
pub fn frame_span_stripe(spans: &[EllaFrameSpan]) -> Vec<u32> {
    spans
        .iter()
        .map(|s| {
            if s.boundary_index == FRAME_SLOT_UNKNOWN {
                s.obj_idx
            } else {
                s.boundary_index as u32
            }
        })
        .collect()
}

/// Light/dark within a GOP so adjacent frames read as separate bars. The
/// FDACE toggle bit alternates per frame and survives drops, unlike a
/// renumbered position.
pub fn frame_span_shade(spans: &[EllaFrameSpan]) -> Vec<bool> {
    spans.iter().map(|s| s.toggle).collect()
}

pub fn frame_span_sublanes(prof: &EllaProfile, spans: &[EllaFrameSpan]) -> Vec<u32> {
    let tmp: Vec<EllaObjectSpan> = spans.iter().map(frame_as_object_span).collect();
    object_timeline_sublanes(prof, &tmp)
}

fn frame_as_object_span(f: &EllaFrameSpan) -> EllaObjectSpan {
    EllaObjectSpan {
        provider_id: f.provider_id,
        instance_id: f.instance_id,
        t0: f.t0,
        t1: f.t1,
        obj_idx: f.obj_idx,
        first_pn: f.first_pn,
        boundary_index: f.boundary_index,
        object_id: Some(f.object_id),
        packets: 0,
        bytes: 0,
        n: 1,
        state: EllaObjectSpanState::Completed,
    }
}

/// Mbps of the Timeline's percentile instance (bytes / STARTED→COMPLETED).
pub fn object_rate_at_percentile(
    prof: &EllaProfile,
    copies: &[EllaObjectSpan],
    kind: EllaObjectPercentile,
    qualify: bool,
) -> Vec<EllaChannelSubSeries> {
    let spans = object_spans_at_percentile(prof, copies, kind);
    let t_last = spans.iter().map(|s| s.t1).fold(0.0_f64, f64::max);
    object_rate_from_gop_spans(prof, &spans, t_last, qualify)
}

fn object_copy_percentile_series(
    prof: &EllaProfile,
    copies: &[EllaObjectSpan],
) -> (Vec<EllaChannelSubSeries>, Vec<EllaChannelSubSeries>) {
    let mut pids: Vec<u16> = Vec::new();
    for s in copies {
        if !pids.contains(&s.provider_id) {
            pids.push(s.provider_id);
        }
    }
    pids.sort_by_key(|pid| (prof.node_role(*pid).rank(), *pid));
    let mut durs = Vec::new();
    let mut sizes = Vec::new();
    for pid in pids {
        let relay = prof.node_role(pid) != EllaNodeRole::Server;
        if relay {
            let (d, z) = percentile_step_series(
                prof,
                copies,
                pid,
                Some(ELLA_SIDE_UPSTREAM),
                EllaObjectPercentile::Median,
            );
            if !d.is_empty() {
                durs.push(EllaChannelSubSeries {
                    label: "Internal".into(),
                    data: d,
                });
            }
            if !z.is_empty() {
                sizes.push(EllaChannelSubSeries {
                    label: "Internal".into(),
                    data: z,
                });
            }
            for kind in EllaObjectPercentile::ALL {
                let (d, z) =
                    percentile_step_series(prof, copies, pid, Some(ELLA_SIDE_DOWNSTREAM), kind);
                if !d.is_empty() {
                    durs.push(EllaChannelSubSeries {
                        label: kind.label().to_string(),
                        data: d,
                    });
                }
                if !z.is_empty() {
                    sizes.push(EllaChannelSubSeries {
                        label: kind.label().to_string(),
                        data: z,
                    });
                }
            }
        } else {
            let (d, z) =
                percentile_step_series(prof, copies, pid, None, EllaObjectPercentile::Median);
            let node = prof.node_label(pid);
            if !d.is_empty() {
                durs.push(EllaChannelSubSeries {
                    label: node.clone(),
                    data: d,
                });
            }
            if !z.is_empty() {
                sizes.push(EllaChannelSubSeries {
                    label: node,
                    data: z,
                });
            }
        }
    }
    (durs, sizes)
}

fn percentile_step_series(
    prof: &EllaProfile,
    copies: &[EllaObjectSpan],
    pid: u16,
    side: Option<u8>,
    kind: EllaObjectPercentile,
) -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
    let mut by_idx: FxHashMap<u32, Vec<&EllaObjectSpan>> = FxHashMap::default();
    let mut order: Vec<u32> = Vec::new();
    for s in copies {
        if s.provider_id != pid {
            continue;
        }
        if side.is_some_and(|want| ella_side(prof, s.provider_id, s.instance_id) != want) {
            continue;
        }
        let e = by_idx.entry(s.obj_idx).or_default();
        if e.is_empty() {
            order.push(s.obj_idx);
        }
        e.push(s);
    }
    order.sort();
    let mut gops: Vec<(f64, f64, f64, f64)> = Vec::new();
    for idx in order {
        let g = by_idx.remove(&idx).unwrap_or_default();
        let completed: Vec<&EllaObjectSpan> = g
            .iter()
            .copied()
            .filter(|s| s.state == EllaObjectSpanState::Completed)
            .collect();
        let pool = if completed.is_empty() { g } else { completed };
        if pool.is_empty() {
            continue;
        }
        let mut dts: Vec<f64> = pool.iter().map(|s| span_duration(s)).collect();
        let mut mbs: Vec<f64> = pool
            .iter()
            .map(|s| s.bytes as f64 / OBJECT_SIZE_MB)
            .collect();
        dts.sort_by(|a, b| a.total_cmp(b));
        mbs.sort_by(|a, b| a.total_cmp(b));
        let p = kind.as_p();
        let dt = copy_percentile(&dts, p);
        let mb = copy_percentile(&mbs, p);
        let t0 = pool.iter().map(|s| s.t0).fold(f64::INFINITY, f64::min);
        let t1 = t0 + dt;
        if t0.is_finite() && dt > 0.0 {
            gops.push((t0, t1, dt, mb));
        }
    }
    gops.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut dur = Vec::new();
    let mut size = Vec::new();
    for i in 0..gops.len() {
        let (t0, t1, dt, mb) = gops[i];
        push_step_changed(&mut dur, t0, dt);
        push_step_changed(&mut size, t0, mb);
        let hold = gops
            .get(i + 1)
            .is_some_and(|n| n.0 - t1 <= OBJECT_GOP_JOIN_S);
        if !hold {
            push_step_changed(&mut dur, t1, 0.0);
            push_step_changed(&mut size, t1, 0.0);
        }
    }
    trim_trailing_collapse(&mut dur);
    trim_trailing_collapse(&mut size);
    (dur, size)
}

fn copy_percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if p >= 1.0 {
        return *sorted.last().unwrap_or(&0.0);
    }
    let n = sorted.len();
    let k = ((p * n as f64).ceil() as usize).max(1).min(n);
    sorted[k - 1]
}

fn trim_trailing_collapse(pts: &mut Vec<[f64; 2]>) {
    let mut ys: Vec<f64> = pts.iter().map(|p| p[1]).filter(|y| *y > 0.0).collect();
    if ys.len() < 4 {
        return;
    }
    ys.sort_by(|a, b| a.total_cmp(b));
    let typical = ys[(ys.len() - 1) / 2];
    if typical <= 0.0 {
        return;
    }
    let floor = typical * OBJECT_TRAIL_FRAC;
    while let Some(&[_, y]) = pts.last() {
        if y >= floor {
            break;
        }
        pts.pop();
    }
    if let Some(&[t, y]) = pts.last() {
        if y == 0.0 {
            pts.pop();
            if pts.last().is_some_and(|p| p[0] == t) {
                pts.pop();
            }
        }
    }
}

/// End a GOP when the next one on that node starts so the Object Id
/// step series does not hold two ids at once.
fn clip_overlapping_spans(spans: &mut Vec<EllaObjectSpan>) {
    spans.sort_by(|a, b| {
        a.provider_id
            .cmp(&b.provider_id)
            .then(a.t0.total_cmp(&b.t0))
            .then(a.obj_idx.cmp(&b.obj_idx))
    });
    for i in 0..spans.len() {
        let pid = spans[i].provider_id;
        let t0 = spans[i].t0;
        if i > 0 && spans[i - 1].provider_id == pid && spans[i - 1].t0 < t0 && spans[i - 1].t1 > t0
        {
            spans[i - 1].t1 = t0;
        }
    }
    spans.retain(|s| object_interval_ok(s.t0, s.t1));
}

fn object_rate_from_gop_spans(
    prof: &EllaProfile,
    spans: &[EllaObjectSpan],
    t_last: f64,
    qualify: bool,
) -> Vec<EllaChannelSubSeries> {
    let mut order: Vec<(u16, u8)> = Vec::new();
    let mut boxes: ObjectRateBoxes = FxHashMap::default();
    for s in spans {
        let dt = span_duration(s);
        if dt <= 0.0 || s.bytes == 0 {
            continue;
        }
        let k = object_timeline_lane(prof, s.provider_id, s.instance_id);
        if !order.contains(&k) {
            order.push(k);
        }
        let mbps = s.bytes as f64 * 8.0 / 1_000_000.0 / dt;
        boxes.entry(k).or_default().push((s.t0, s.t1, mbps));
    }
    order.sort_by_key(|&(pid, side)| {
        let (face, pid) = object_timeline_lane_sort_key(pid, side);
        (std::cmp::Reverse(face), pid)
    });
    order
        .into_iter()
        .filter_map(|k| {
            let b = boxes.remove(&k)?;
            let mut data = object_rate_instance_steps(b);
            extend_step_series(&mut data, t_last);
            if data.is_empty() {
                return None;
            }
            let label = object_face_series_label(prof, k.0, k.1, qualify);
            Some(EllaChannelSubSeries { label, data })
        })
        .collect()
}

fn span_duration(s: &EllaObjectSpan) -> f64 {
    s.t1 - s.t0
}

fn span_state_rank(state: EllaObjectSpanState) -> u8 {
    match state {
        EllaObjectSpanState::Completed => 2,
        EllaObjectSpanState::Abandoned => 1,
        EllaObjectSpanState::Open => 0,
    }
}

/// Idle gap that starts a new `first_pn` / object-id map for the next
/// life of the title.
const OBJECT_LIFE_GAP_S: f64 = 10.0;

/// Starts within this window are the same GOP on Server and Relay.
const OBJECT_GOP_JOIN_S: f64 = 0.4;

/// Cross-label object ids by shared `first_pn` within one life, rank
/// rows in time, and merge instances whose spans match to ~25 ms.
fn merge_object_spans(spans: &mut Vec<EllaObjectSpan>) {
    if spans.is_empty() {
        return;
    }
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&i, &j| spans[i].t0.total_cmp(&spans[j].t0));
    let mut cluster = vec![0usize; spans.len()];
    let mut n_clusters = 1usize;
    let mut last_t = spans[order[0]].t0;
    for &i in &order[1..] {
        if spans[i].t0 - last_t > OBJECT_LIFE_GAP_S {
            n_clusters += 1;
        }
        cluster[i] = n_clusters - 1;
        last_t = spans[i].t0;
    }
    let mut offset = 0u32;
    for c in 0..n_clusters {
        let mut ids: FxHashMap<u32, u32> = FxHashMap::default();
        let mut pns: Vec<u32> = Vec::new();
        for (i, s) in spans.iter().enumerate() {
            if cluster[i] != c {
                continue;
            }
            if let Some(id) = s.object_id {
                ids.entry(s.first_pn).or_insert(id);
            }
            pns.push(s.first_pn);
        }
        pns.sort_unstable();
        pns.dedup();
        let rank: FxHashMap<u32, u32> = pns
            .iter()
            .enumerate()
            .map(|(i, &p)| (p, i as u32))
            .collect();
        for (i, s) in spans.iter_mut().enumerate() {
            if cluster[i] != c {
                continue;
            }
            if s.object_id.is_none() {
                s.object_id = ids.get(&s.first_pn).copied();
            }
            s.obj_idx = offset + rank[&s.first_pn];
        }
        offset += pns.len() as u32;
    }
    let bucket = |t: f64| (t * 40.0).round() as i64;
    spans.sort_by(|a, b| {
        (
            a.obj_idx,
            a.provider_id,
            a.instance_id,
            bucket(a.t0),
            bucket(a.t1),
            a.state as u8,
        )
            .cmp(&(
                b.obj_idx,
                b.provider_id,
                b.instance_id,
                bucket(b.t0),
                bucket(b.t1),
                b.state as u8,
            ))
    });
    let mut merged: Vec<EllaObjectSpan> = Vec::new();
    for s in spans.drain(..) {
        if let Some(last) = merged.last_mut() {
            if last.provider_id == s.provider_id
                && last.instance_id == s.instance_id
                && last.obj_idx == s.obj_idx
                && bucket(last.t0) == bucket(s.t0)
                && bucket(last.t1) == bucket(s.t1)
                && last.state == s.state
            {
                last.n += s.n;
                last.t0 = last.t0.min(s.t0);
                last.t1 = last.t1.max(s.t1);
                continue;
            }
        }
        merged.push(s);
    }
    *spans = merged;
}

/// Per span, whether this node's object id jumped by more than one
/// versus the previous span in time.
pub fn object_span_id_skipped(spans: &[EllaObjectSpan]) -> Vec<bool> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&i, &j| {
        spans[i]
            .provider_id
            .cmp(&spans[j].provider_id)
            .then(spans[i].t0.total_cmp(&spans[j].t0))
            .then(spans[i].first_pn.cmp(&spans[j].first_pn))
    });
    let mut last: FxHashMap<u16, (u32, f64)> = FxHashMap::default();
    let mut out = vec![false; spans.len()];
    for i in order {
        let s = &spans[i];
        let skipped = s.object_id.is_some_and(|id| {
            last.get(&s.provider_id).is_some_and(|&(prev, prev_t1)| {
                s.t0 - prev_t1 <= OBJECT_LIFE_GAP_S && id > prev.saturating_add(1)
            })
        });
        if let Some(id) = s.object_id {
            last.insert(s.provider_id, (id, s.t1));
        }
        out[i] = skipped;
    }
    out
}

/// GOP stripe for the Object Timeline: the object slot (`boundary_index`),
/// which rotates every GOP and survives drops, unlike the time-ranked
/// `obj_idx`.
pub fn object_span_stripe(spans: &[EllaObjectSpan]) -> Vec<u32> {
    spans.iter().map(|s| s.boundary_index as u32).collect()
}

/// Lifecycle counts and time bounds across every node's view of a
/// title, feeding one channel tab's caption and x range.
pub fn build_ella_channel_events_for(
    prof: &EllaProfile,
    members: &[(u16, u64)],
    t0_us: u64,
) -> EllaChannelEventList {
    let mut out = EllaChannelEventList {
        rows: Vec::new(),
        n_joins: 0,
        n_lifecycle: 0,
        n_objects: 0,
        first_created_s: None,
        last_destroyed_s: None,
        n_created: 0,
        n_destroyed: 0,
    };
    let qualify = ella_tab_multi_node(members);
    let srcs = ella_member_sources(prof, members);
    for &(pid, sid) in members {
        let Some(src) = srcs.get(&(pid, sid)) else {
            continue;
        };
        let mut ev = build_ella_channel_events_from(prof, pid, t0_us, src);
        if qualify {
            let node = prof.node_label(pid);
            for r in &mut ev.rows {
                if !r.instance_label.is_empty() {
                    r.instance_label = ella_node_series_label(&r.instance_label, &node);
                }
            }
        }
        out.rows.append(&mut ev.rows);
        out.n_joins += ev.n_joins;
        out.n_lifecycle += ev.n_lifecycle;
        out.n_objects += ev.n_objects;
        out.n_created += ev.n_created;
        out.n_destroyed += ev.n_destroyed;
        out.first_created_s = match (out.first_created_s, ev.first_created_s) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        out.last_destroyed_s = match (out.last_destroyed_s, ev.last_destroyed_s) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
    out.rows.sort_by(|a, b| a.t.total_cmp(&b.t));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srtt_bucket_labels_match_kernel_edges() {
        assert_eq!(srtt_bucket_label(0, 14), "0");
        assert_eq!(srtt_bucket_label(1, 14), "<512us");
        assert_eq!(srtt_bucket_label(2, 14), "512us-1ms");
        assert_eq!(srtt_bucket_label(3, 14), "1ms-2ms");
        assert_eq!(srtt_bucket_label(6, 14), "8ms-16ms");
        assert_eq!(srtt_bucket_label(12, 14), "524ms-1s");
        assert_eq!(srtt_bucket_label(13, 14), ">=1s");
    }

    #[test]
    fn slot_gate_series_sums_cpus_in_the_same_second() {
        let sample = |ts, cpu, fires, diverts, bytes, rollover_max, walks, closed| HptsWheelEvent {
            ts,
            cpu,
            session_id: u64::from(cpu),
            payload: HptsWheelPayload::SlotGateSample {
                fires,
                diverts,
                bytes,
                rollover_max,
                walks,
                closed,
                default_bytes: 0,
                rack_bytes: 0,
                bbr_bytes: 0,
                closed_tcp_bytes: 0,
                caller0: 0,
                caller0_bytes: 0,
                caller1: 0,
                caller1_bytes: 0,
                caller_other_bytes: 0,
            },
        };
        let events = vec![
            sample(1_500_000, 0, 10, 1, 1_000_000_000, 3, 4, 2),
            sample(1_600_000, 1, 5, 0, 250_000_000, 7, 1, 0),
            sample(2_100_000, 0, 1, 0, 0, 1, 0, 0),
        ];
        let s = build_hpts_slot_gate_series(&events, 0);
        assert_eq!(s.fires.len(), 2);
        assert!((s.fires[0][0] - 1.0).abs() < 1e-9);
        assert!((s.fires[0][1] - 15.0).abs() < 1e-9);
        assert!((s.diverts[0][1] - 1.0).abs() < 1e-9);
        assert!((s.bytes_gbps[0][1] - 10.0).abs() < 1e-6);
        assert!((s.walks[0][1] - 5.0).abs() < 1e-9);
        assert!((s.closed[0][1] - 2.0).abs() < 1e-9);
        assert!((s.rollover_max[0][1] - 7.0).abs() < 1e-9);
        assert!((s.rollover_sum[0][1] - 10.0).abs() < 1e-9);
        assert!((s.fires[1][0] - 2.0).abs() < 1e-9);
        // 1 fire over the 0.6 s since this CPU's previous sample.
        assert!((s.fires[1][1] - 1.0 / 0.6).abs() < 1e-6);
    }

    #[test]
    fn slot_gate_series_averages_two_samples_from_one_cpu() {
        let sample = |ts, bytes| HptsWheelEvent {
            ts,
            cpu: 0,
            session_id: 1,
            payload: HptsWheelPayload::SlotGateSample {
                fires: 0,
                diverts: 0,
                bytes,
                rollover_max: 0,
                walks: 0,
                closed: 0,
                default_bytes: 0,
                rack_bytes: 0,
                bbr_bytes: 0,
                closed_tcp_bytes: 0,
                caller0: 0,
                caller0_bytes: 0,
                caller1: 0,
                caller1_bytes: 0,
                caller_other_bytes: 0,
            },
        };
        // 125e6 bytes over the assumed first 1 s is 1 Gbps. The next
        // 50e6 bytes arrive 0.4 s later, also 1 Gbps. Both timestamps
        // fall in second 1, and the plotted rate stays 1 Gbps.
        let s = build_hpts_slot_gate_series(
            &[
                sample(1_000_000, 125_000_000),
                sample(1_400_000, 50_000_000),
            ],
            0,
        );
        assert_eq!(s.bytes_gbps.len(), 1);
        assert!((s.bytes_gbps[0][1] - 1.0).abs() < 1e-6);
    }

    fn tcp_state(ts: u64, estab: u64, time_wait: u64) -> TcpStateSample {
        TcpStateSample {
            ts,
            provider_id: 1,
            session_id: 0,
            closed: 0,
            listen: 0,
            syn_sent: 0,
            syn_rcvd: 0,
            estab,
            close_wait: 0,
            fin_wait_1: 0,
            closing: 0,
            last_ack: 0,
            fin_wait_2: 0,
            time_wait,
        }
    }

    #[test]
    fn tcp_conn_rate_estab_rise() {
        let prof = TcpSampleProfile {
            state_samples: vec![tcp_state(1_000_000, 100, 0), tcp_state(2_000_000, 200, 0)],
            ..Default::default()
        };
        let rate = build_tcp_conn_rate_series(&prof, 0);
        assert_eq!(rate.estab, vec![(2.0, 100)]);
        assert_eq!(rate.teardown, vec![(2.0, 0)]);
        let bins = tcp_conn_rate_bins(&rate.estab, 0.0, 2.0, 2);
        assert_eq!(bins.len(), 2);
        assert_eq!(bins[0][1], 0.0);
        assert!((bins[1][1] - 100.0).abs() < 1e-9);
    }

    #[test]
    fn tcp_conn_rate_estab_fall_into_time_wait() {
        let prof = TcpSampleProfile {
            state_samples: vec![tcp_state(1_000_000, 200, 0), tcp_state(2_000_000, 100, 100)],
            ..Default::default()
        };
        let rate = build_tcp_conn_rate_series(&prof, 0);
        assert_eq!(rate.estab, vec![(2.0, 0)]);
        assert_eq!(rate.teardown, vec![(2.0, 100)]);
    }

    #[test]
    fn tcp_conn_rate_churn_via_time_wait() {
        let prof = TcpSampleProfile {
            state_samples: vec![
                tcp_state(1_000_000, 1000, 50),
                tcp_state(2_000_000, 1000, 150),
            ],
            ..Default::default()
        };
        let rate = build_tcp_conn_rate_series(&prof, 0);
        assert_eq!(rate.estab, vec![(2.0, 100)]);
        assert_eq!(rate.teardown, vec![(2.0, 100)]);
    }

    fn tcp_rps(ts: u64, req_count: u64) -> TcpRpsSample {
        TcpRpsSample {
            ts,
            provider_id: 1,
            session_id: 0,
            req_count,
        }
    }

    #[test]
    fn tcp_rps_bins_skip_zero_priming_ticks() {
        let prof = TcpSampleProfile {
            rps_samples: vec![
                tcp_rps(1_000_000, 100),
                tcp_rps(1_001_000, 0),
                tcp_rps(2_000_000, 200),
                tcp_rps(2_001_000, 0),
            ],
            ..Default::default()
        };
        let series = build_tcp_rps_series(&prof, 0);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].samples, vec![(1.0, 100), (2.0, 200)]);
        let bins = tcp_rps_bins(&series, 0.0, 2.0, 2);
        assert_eq!(bins.len(), 2);
        assert!((bins[0][1] - 100.0).abs() < 1e-9);
        assert!((bins[1][1] - 100.0).abs() < 1e-9);
    }

    #[test]
    fn tcp_rps_series_drops_a_lone_startup_sample() {
        let prof = TcpSampleProfile {
            rps_samples: vec![tcp_rps(47_000, 36)],
            ..Default::default()
        };
        assert!(build_tcp_rps_series(&prof, 0).is_empty());
    }

    #[test]
    fn cumulative_intervals_permanently_zero_counter_yields_flat_zero_not_empty() {
        // A protocol-split counter (e.g. tcp_obytes on a lagg member port)
        // that never once increments across the whole capture used to diff
        // down to nothing, which is indistinguishable from "no samples at
        // all" once it reaches the GUI's empty-series check.
        let points: Vec<(u64, u64)> = (0..5).map(|i| (i * 100_000, 0)).collect();
        let out = cumulative_intervals(&points, 0);
        assert!(!out.is_empty());
        assert!(out.iter().all(|&(_, dval, _)| dval == 0.0));
    }

    #[test]
    fn cumulative_intervals_single_sample_stays_empty() {
        // One sample can't be diffed into a rate at all; this must stay
        // empty rather than fabricating a zero-width interval.
        let points = vec![(0u64, 0u64)];
        assert!(cumulative_intervals(&points, 0).is_empty());
    }

    #[test]
    fn cumulative_intervals_normal_counter_growth_is_unaffected() {
        let points = vec![(0u64, 0u64), (1_000_000, 1_000_000u64)];
        let out = cumulative_intervals(&points, 0);
        assert_eq!(out.len(), 1);
        let (t, dval, dt) = out[0];
        assert_eq!(t, 1.0);
        assert_eq!(dval, 1_000_000.0);
        assert_eq!(dt, 1.0);
    }

    #[test]
    fn cumulative_rate_keeps_a_short_refresh_inside_one_step() {
        let points = vec![(0u64, 0u64), (100_000, 0), (200_000, 0), (300_000, 300)];
        let out = cumulative_rate(&points, 0);
        assert_eq!(out.len(), 1);
        assert!((out[0][0] - 0.3).abs() < 1e-9);
        assert!((out[0][1] - 1000.0).abs() < 1e-6);
    }

    #[test]
    fn cumulative_rate_sits_at_zero_across_a_long_flat_stretch() {
        let mut points = vec![(0u64, 0u64), (100_000, 222u64)];
        let mut t = 200_000u64;
        while t < 200_000_000 {
            points.push((t, 222));
            t += 100_000;
        }
        points.push((200_000_000, 267));
        points.push((201_000_000, 267));
        points.push((202_000_000, 267));
        let out = cumulative_rate(&points, 0);
        let first = out.iter().find(|p| (p[0] - 0.1).abs() < 1e-6).unwrap();
        assert!((first[1] - 2220.0).abs() < 1e-3);
        assert!(out.iter().any(|p| (p[0] - 1.1).abs() < 1e-6 && p[1] == 0.0));
        assert!(out
            .iter()
            .any(|p| p[0] > 199.0 && p[0] < 200.0 && p[1] == 0.0));
        assert!(out
            .iter()
            .filter(|p| p[0] > 1.0 && p[0] < 200.0)
            .all(|p| p[1] == 0.0));
        let second = out.iter().find(|p| (p[0] - 200.0).abs() < 1e-6).unwrap();
        assert!((second[1] - 450.0).abs() < 1e-3);
        assert_eq!(out.last().unwrap()[1], 0.0);
    }

    /// Build one synthetic `IfSample` for ifindex 1, with `obytes`/`ibytes`
    /// (and their packet counterparts, set equal for simplicity) plus the
    /// tcp/udp/other split explicit -- everything else defaulted.
    fn mk_if_sample(
        ts: u64,
        obytes: u64,
        ibytes: u64,
        tcp: (u64, u64),
        udp: (u64, u64),
        other: (u64, u64),
    ) -> IfSample {
        IfSample {
            ts,
            ifindex: 1,
            obytes,
            ibytes,
            opackets: obytes,
            ipackets: ibytes,
            tcp_obytes: tcp.0,
            tcp_ibytes: tcp.1,
            tcp_opackets: tcp.0,
            tcp_ipackets: tcp.1,
            udp_obytes: udp.0,
            udp_ibytes: udp.1,
            udp_opackets: udp.0,
            udp_ipackets: udp.1,
            other_obytes: other.0,
            other_ibytes: other.1,
            other_opackets: other.0,
            other_ipackets: other.1,
            ..Default::default()
        }
    }

    fn one_if_profile(samples: Vec<IfSample>) -> IfProfile {
        IfProfile {
            interfaces: FxHashMap::default(),
            samples,
            events: Vec::new(),
            period_us: 0,
            nifs: 1,
        }
    }

    #[test]
    fn has_protocol_classification_false_when_tcp_and_udp_never_advance() {
        // Covers both a pre-split capture (deprecated `IF_SAMPLE`, where
        // tcp/udp are always 0 and `other` is derived as equal to the
        // total -- see the `IfRateSeries` struct doc) and a carrier-only
        // interface on a post-split capture (lagg member, vlan's
        // underlying NIC). Either way tcp/udp read flat zero; `other`
        // reading nonzero (mimicking the pre-split derivation) must NOT
        // flip this to `true`.
        let samples: Vec<IfSample> = (0u64..5)
            .map(|i| {
                let b = i * 1_000_000;
                mk_if_sample(i * 100_000, b, b, (0, 0), (0, 0), (b, b))
            })
            .collect();
        let prof = one_if_profile(samples);
        let series = build_if_rate_series(&prof, 0);
        assert_eq!(series.len(), 1);
        assert!(!series[0].has_protocol_classification);
        assert!(series[0].proto_cum.is_none());
    }

    #[test]
    fn has_protocol_classification_true_when_tcp_or_udp_advances() {
        let samples: Vec<IfSample> = (0u64..5)
            .map(|i| {
                let b = i * 1_000_000;
                mk_if_sample(i * 100_000, b, b, (b, b), (0, 0), (0, 0))
            })
            .collect();
        let prof = one_if_profile(samples);
        let series = build_if_rate_series(&prof, 0);
        assert_eq!(series.len(), 1);
        assert!(series[0].has_protocol_classification);
        assert!(series[0].proto_cum.is_some());
    }

    #[test]
    fn if_byte_totals_match_rate_series_first_last() {
        let samples: Vec<IfSample> = (0u64..5)
            .map(|i| {
                let b = i * 1_000_000;
                mk_if_sample(i * 100_000, b, b * 2, (0, 0), (0, 0), (0, 0))
            })
            .collect();
        let prof = one_if_profile(samples);
        let series = build_if_rate_series(&prof, 0);
        let totals = if_byte_totals(&prof);
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].total_tx_bytes, series[0].total_tx_bytes);
        assert_eq!(totals[0].total_rx_bytes, series[0].total_rx_bytes);
        assert_eq!(totals[0].total_tx_bytes, 4_000_000);
        assert_eq!(totals[0].total_rx_bytes, 8_000_000);
        assert!(series[0].oqdrop_rate.is_empty());
    }

    #[test]
    fn oqdrop_rate_counts_tx_queue_drops_only() {
        let samples: Vec<IfSample> = (0u64..3)
            .map(|i| {
                let mut s =
                    mk_if_sample(i * 1_000_000, i * 1_000, i * 1_000, (0, 0), (0, 0), (0, 0));
                s.oqdrops = i * 500;
                s.iqdrops = i * 9_000;
                s
            })
            .collect();
        let series = build_if_rate_series(&one_if_profile(samples), 0);
        assert_eq!(series[0].oqdrop_rate.len(), 2);
        for p in &series[0].oqdrop_rate {
            assert!((p[1] - 500.0).abs() < 1e-6);
        }
        for p in &series[0].drop_rate {
            assert!((p[1] - 9_500.0).abs() < 1e-6);
        }
    }

    #[test]
    fn step_series_bins_time_weights_holds_over_the_visible_window() {
        let data = [[0.0, 10.0], [1.0, 10.0], [1.0, 20.0], [2.0, 20.0]];
        let bins = step_series_bins(&data, 0.0, 2.0, 2);
        assert_eq!(bins.len(), 2);
        assert!((bins[0][0] - 0.5).abs() < 1e-9);
        assert!((bins[0][1] - 10.0).abs() < 1e-9);
        assert!((bins[1][0] - 1.5).abs() < 1e-9);
        assert!((bins[1][1] - 20.0).abs() < 1e-9);
        let zoomed = step_series_bins(&data, 0.0, 1.0, 1);
        assert_eq!(zoomed.len(), 1);
        assert!((zoomed[0][1] - 10.0).abs() < 1e-9);
    }

    #[test]
    fn step_series_bins_holds_a_long_unchanged_gauge_across_the_window() {
        // Frozen occupancy over ~220s, 250 bins on a 1700s window.
        let data = [
            [400.0, 96_000.0],
            [439.878, 96_000.0],
            [439.878, 96_062.0],
            [661.0, 96_062.0],
            [661.0, 96_061.0],
            [800.0, 96_061.0],
        ];
        let bins = step_series_bins(&data, 0.0, 1700.0, 250);
        assert_eq!(bins.len(), 250);
        let mid: Vec<_> = bins
            .iter()
            .filter(|p| p[0] > 440.0 && p[0] < 660.0)
            .collect();
        assert!(mid.len() > 20);
        for p in mid {
            assert!((p[1] - 96_062.0).abs() < 1e-6, "bin t={} y={}", p[0], p[1]);
        }
        assert!(bins.iter().any(|p| p[0] < 400.0 && p[1] == 0.0));
    }

    #[test]
    fn protocol_stack_bins_computes_expected_deltas_and_totals() {
        // tcp grows 100 bytes/s, udp 50 bytes/s, other flat zero; bin
        // edges land exactly on the sample timestamps so each bin's
        // delta is unambiguous.
        let tcp: Vec<(u64, u64)> = (0..=4).map(|t| (t * 1_000_000, t * 100)).collect();
        let udp: Vec<(u64, u64)> = (0..=4).map(|t| (t * 1_000_000, t * 50)).collect();
        let other: Vec<(u64, u64)> = (0..=4).map(|t| (t * 1_000_000, 0u64)).collect();
        let cum = [tcp, udp, other];

        let (centers, tops) = protocol_stack_bins(&cum, 0, 0.0, 4.0, 4, 1.0);
        assert_eq!(centers, vec![0.5, 1.5, 2.5, 3.5]);
        #[allow(clippy::needless_range_loop)]
        for i in 0..4 {
            assert!(
                (tops[0][i] - 100.0).abs() < 1e-9,
                "tcp band[{i}] = {}",
                tops[0][i]
            );
            assert!(
                (tops[1][i] - 150.0).abs() < 1e-9,
                "tcp+udp band[{i}] = {}",
                tops[1][i]
            );
            // other is flat zero, so the full stack equals tcp+udp.
            assert!(
                (tops[2][i] - 150.0).abs() < 1e-9,
                "full stack[{i}] = {}",
                tops[2][i]
            );
        }
    }

    #[test]
    fn protocol_stack_bins_holds_baseline_through_a_dip() {
        // A spurious zero/reset sample (see `cumulative_intervals`) must
        // not produce a negative-height band or a total below any of
        // its bands.
        let tcp: Vec<(u64, u64)> = vec![(0, 0), (1_000_000, 100), (2_000_000, 0), (3_000_000, 300)];
        let flat: Vec<(u64, u64)> = vec![(0, 0), (1_000_000, 0), (2_000_000, 0), (3_000_000, 0)];
        let cum = [tcp, flat.clone(), flat];

        let (_centers, tops) = protocol_stack_bins(&cum, 0, 0.0, 3.0, 3, 1.0);
        #[allow(clippy::needless_range_loop)]
        for i in 0..3 {
            assert!(tops[0][i] >= 0.0);
            assert!(tops[1][i] >= tops[0][i]);
            assert!(tops[2][i] >= tops[1][i]);
        }
    }

    #[test]
    fn protocol_stack_bins_empty_range_yields_no_bins() {
        let cum = [
            vec![(0u64, 0u64), (1_000_000, 10)],
            vec![(0u64, 0u64), (1_000_000, 5)],
            vec![(0u64, 0u64), (1_000_000, 0)],
        ];
        let (centers, tops) = protocol_stack_bins(&cum, 0, 1.0, 1.0, 10, 1.0);
        assert!(centers.is_empty());
        assert!(tops.iter().all(|t| t.is_empty()));
    }

    #[test]
    fn protocol_stack_bins_holds_rate_when_bins_are_finer_than_samples() {
        // 100 bytes/s, samples every 1s. A 0.5s window with 10 bins is
        // 50ms/bin — finer than the sample interval — and must stay at
        // the interval rate, not a bin-width spike.
        let tcp: Vec<(u64, u64)> = (0..=4).map(|t| (t * 1_000_000, t * 100)).collect();
        let flat: Vec<(u64, u64)> = (0..=4).map(|t| (t * 1_000_000, 0u64)).collect();
        let cum = [tcp, flat.clone(), flat];
        let (_centers, tops) = protocol_stack_bins(&cum, 0, 1.0, 1.5, 10, 1.0);
        assert_eq!(tops[0].len(), 10);
        for (i, &v) in tops[0].iter().enumerate() {
            assert!(
                (v - 100.0).abs() < 1e-6,
                "bin {i} = {v}, want held 100 bytes/s"
            );
        }
    }

    fn ella_event_id(payload: &EllaPayload) -> u32 {
        match payload {
            EllaPayload::InstanceAttach { .. } => EVT_ELLA_RELAY_INSTANCE_ATTACH,
            EllaPayload::NodeInfo { .. } => EVT_ELLA_RELAY_NODE_INFO,
            EllaPayload::NodeIdentity { .. } => EVT_ELLA_RELAY_NODE_IDENTITY,
            EllaPayload::SessionCreate { .. } => EVT_ELLA_RELAY_SESSION_CREATE,
            EllaPayload::SessionDestroy => EVT_ELLA_RELAY_SESSION_DESTROY,
            EllaPayload::ChannelState { .. } => EVT_ELLA_RELAY_CHANNEL_STATE,
            EllaPayload::ChannelIdAssigned { .. } => EVT_ELLA_RELAY_CHANNEL_ID_ASSIGNED,
            EllaPayload::ChannelError { .. } => EVT_ELLA_RELAY_CHANNEL_ERROR,
            EllaPayload::ChannelDestroyed { .. } => EVT_ELLA_RELAY_CHANNEL_DESTROYED,
            EllaPayload::ChannelSubscriber { .. } => EVT_ELLA_RELAY_CHANNEL_SUBSCRIBER,
            EllaPayload::SubscriberIdentity { .. } => EVT_ELLA_RELAY_SUBSCRIBER_IDENTITY,
            EllaPayload::SubscriberJoin { .. } => EVT_ELLA_RELAY_SUBSCRIBER_JOIN,
            EllaPayload::SubscriberLeave { .. } => EVT_ELLA_RELAY_SUBSCRIBER_LEAVE,
            EllaPayload::ObjectState { .. } => EVT_ELLA_RELAY_OBJECT_STATE,
            EllaPayload::ObjectTiming { .. } => EVT_ELLA_RELAY_OBJECT_TIMING,
            EllaPayload::FrameSent { .. } => EVT_ELLA_RELAY_FRAME_SENT,
            EllaPayload::FrameRelay { .. } => EVT_ELLA_RELAY_FRAME_RELAY,
            EllaPayload::RelayError { .. } => EVT_ELLA_RELAY_NODE_ERROR,
        }
    }

    fn ella_evt(ts: u64, session_id: u64, payload: EllaPayload) -> EllaEvent {
        ella_evt_on(1, ts, session_id, payload)
    }

    fn ella_evt_on(provider_id: u16, ts: u64, session_id: u64, payload: EllaPayload) -> EllaEvent {
        EllaEvent {
            ts,
            cpu: 0,
            tid: 0,
            provider_id,
            session_id,
            event_id: ella_event_id(&payload),
            payload,
        }
    }

    fn ella_sample(
        ts: u64,
        session_id: u64,
        instance_id: u32,
        subscribers: u32,
    ) -> EllaChannelSample {
        EllaChannelSample {
            ts,
            provider_id: 1,
            session_id,
            instance_id,
            subscribers,
            subscribers_pending: 0,
            subscribers_leaving: 0,
            objects_completed: 0,
            object_bytes: 0,
            packets: 0,
            inproc_attached: 0,
            publish_no_ring: 0,
            drain_skipped_pending: 0,
        }
    }

    fn ella_frame(
        ts: u64,
        session_id: u64,
        instance_id: u32,
        len: u32,
        planned_us: u32,
        actual_us: u32,
        fwd_us: u32,
    ) -> EllaFrameSample {
        EllaFrameSample {
            ts,
            provider_id: 1,
            session_id,
            instance_id,
            frames: 1,
            frames_stretched: 0,
            sum_frame_len: len,
            sum_planned_us: planned_us,
            sum_actual_us: actual_us,
            sum_fwd_us: fwd_us,
            max_overshoot_us: 0,
            sum_packets: 0,
        }
    }

    #[test]
    fn ella_channel_subs_plot_counts_subscribed_when_payload_is_zero() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::ChannelState {
                    state: ELLA_LIFECYCLE_CREATED,
                    reason: 0,
                    instance_id: 0,
                    subscribers: 0,
                },
            ),
            ella_evt(
                1_100_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 0,
                    subscriber_address: "127.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 0,
                },
            ),
            ella_evt(
                2_000_000,
                1,
                EllaPayload::ChannelDestroyed {
                    reason: 1,
                    instance_id: 0,
                    subscribers: 0,
                },
            ),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(plot.subscribers.len(), 1);
        assert_eq!(plot.subscribers[0].label, "Internal");
        assert_eq!(
            plot.subscribers[0].data,
            [[1.1, 1.0], [2.0, 1.0], [2.0, 0.0]]
        );
        assert!(plot
            .markers
            .iter()
            .any(|m| m.kind == EllaChannelMarkerKind::Created && (m.t - 1.0).abs() < 1e-9));
        assert!(plot
            .markers
            .iter()
            .any(|m| m.kind == EllaChannelMarkerKind::Destroyed && (m.t - 2.0).abs() < 1e-9));
    }

    #[test]
    fn ella_channel_plot_for_many_reincarnations_skips_other_sessions() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        let n = 40u64;
        for sid in 1..=n {
            prof.channels
                .insert((1, sid), mk_ch(1, sid, "health_channel", 1));
            prof.events.push(ella_evt(
                sid * 1_000_000,
                sid,
                EllaPayload::ChannelState {
                    state: ELLA_LIFECYCLE_CREATED,
                    reason: 0,
                    instance_id: 0,
                    subscribers: 0,
                },
            ));
        }
        for i in 0..2_000u64 {
            prof.events.push(ella_evt(
                100_000_000 + i,
                9_999,
                EllaPayload::ChannelState {
                    state: ELLA_LIFECYCLE_CREATED,
                    reason: 0,
                    instance_id: 0,
                    subscribers: 0,
                },
            ));
        }
        let members = ella_channel_tab_members(&prof, 1, 1);
        assert_eq!(members.len(), n as usize);
        let ev = build_ella_channel_events_for(&prof, &members, 0);
        assert_eq!(ev.n_created, n as usize);
        assert_eq!(ev.rows.len(), n as usize);
        let _ = build_ella_channel_plot_for(&prof, &members, 0);
    }

    fn ella_count(
        ts: u64,
        instance_id: u32,
        channels: u32,
        subscribers: u32,
        sessions: u32,
    ) -> EllaChannelCount {
        EllaChannelCount {
            ts,
            provider_id: 1,
            instance_id,
            channels,
            channels_pending_leave: 0,
            channels_pending_retire: 0,
            subscribers,
            sessions,
        }
    }

    #[test]
    fn ella_relay_plot_stays_per_instance_when_few() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.counts = vec![
            ella_count(1_000_000, 0, 1, 2, 1),
            ella_count(1_000_000, 2, 1, 3, 1),
            ella_count(2_000_000, 0, 1, 2, 1),
            ella_count(2_000_000, 2, 1, 4, 1),
        ];
        let plot = build_ella_relay_plot(&prof, 0);
        assert!(!plot.aggregated);
        assert_eq!(plot.instance_count, 2);
        assert_eq!(plot.subscribers.len(), 2);
        assert!(plot.dn_channels_max.is_empty());
    }

    fn series_peak(data: &[[f64; 2]]) -> f64 {
        data.iter().map(|p| p[1]).fold(0.0_f64, f64::max)
    }

    fn named_pending<'a>(plot: &'a EllaRelayPlotData, label: &str) -> &'a [[f64; 2]] {
        plot.pending
            .iter()
            .find(|s| s.label == label)
            .map(|s| s.data.as_slice())
            .unwrap_or(&[])
    }

    #[test]
    fn ella_relay_plot_aggregates_many_instances() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        let mut counts = vec![ella_count(1_000_000, 0, 3, 10, 3)];
        for i in 2u32..7 {
            prof.instances.insert((1, i), ELLA_SIDE_DOWNSTREAM);
            counts.push(ella_count(1_000_000, i, 1, 4, 3));
        }
        counts.push(ella_count(2_000_000, 0, 3, 12, 3));
        for i in 2u32..7 {
            counts.push(ella_count(2_000_000, i, 1, 5, 3));
        }
        for c in &mut counts {
            c.channels_pending_leave = if c.instance_id == 0 { 1 } else { 2 };
        }
        prof.counts = counts;
        let plot = build_ella_relay_plot(&prof, 0);
        assert!(plot.aggregated);
        assert_eq!(plot.instance_count, 6);
        assert_eq!(plot.subscribers.len(), 2);
        assert_eq!(plot.channels.len(), 1);
        assert_eq!(plot.pending.len(), 2);
        let dn = plot
            .subscribers
            .iter()
            .find(|s| s.label == "External")
            .unwrap();
        let peak = series_peak(&dn.data);
        assert!((peak - 25.0).abs() < 1e-6, "peak downstream {peak}");
        assert!(
            (series_peak(&plot.dn_leave_max) - 2.0).abs() < 1e-6,
            "downstream leave max {}",
            series_peak(&plot.dn_leave_max)
        );
        assert!(
            (series_peak(&plot.dn_leave_sum) - 10.0).abs() < 1e-6,
            "downstream leave sum {}",
            series_peak(&plot.dn_leave_sum)
        );
        assert!(
            (series_peak(&plot.dn_channels_max) - 1.0).abs() < 1e-6,
            "downstream channels max {}",
            series_peak(&plot.dn_channels_max)
        );
        assert!(
            (series_peak(&plot.dn_channels_sum) - 5.0).abs() < 1e-6,
            "downstream channels sum {}",
            series_peak(&plot.dn_channels_sum)
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn ella_total(
        ts: u64,
        channels_up: u32,
        channels_dn: u32,
        subscribers_up: u32,
        subscribers_dn: u32,
        leave_up: u32,
        leave_dn: u32,
        retire_up: u32,
        retire_dn: u32,
    ) -> EllaChannelCountTotal {
        EllaChannelCountTotal {
            ts,
            provider_id: 1,
            channels_up,
            channels_dn,
            channels_pending_leave_up: leave_up,
            channels_pending_leave_dn: leave_dn,
            channels_pending_retire_up: retire_up,
            channels_pending_retire_dn: retire_dn,
            subscribers_up,
            subscribers_dn,
            objects_up: 0,
            objects_dn: 0,
            packets_up: 0,
            packets_dn: 0,
            bytes_up: 0,
            bytes_dn: 0,
            joins_up: 0,
            joins_dn: 0,
            switches_up: 0,
            switches_dn: 0,
            leaves_up: 0,
            leaves_dn: 0,
            expiries_up: 0,
            expiries_dn: 0,
            rejects_up: 0,
            rejects_dn: 0,
            packets_dlvr: 0,
            bytes_dlvr: 0,
            joins_dlvr: 0,
            switches_dlvr: 0,
            new_clients: 0,
            clients_up: 0,
            clients_dn: 0,
            subscribers_dlvr: 0,
        }
    }

    #[test]
    fn ella_cdn_metrics_plot_rates_from_channel_stats() {
        let mut a = ella_total(1_000_000, 1, 2, 1, 2, 0, 0, 0, 0);
        a.bytes_up = 1_000_000;
        a.bytes_dn = 2_000_000;
        a.joins_dn = 10;
        a.new_clients = 5;
        let mut b = ella_total(2_000_000, 1, 2, 1, 2, 0, 0, 0, 0);
        b.bytes_up = 500_000;
        b.bytes_dn = 1_000_000;
        b.joins_dn = 4;
        b.new_clients = 1;
        let prof = EllaProfile {
            counts_total: vec![a, b],
            ..Default::default()
        };
        let plot = build_ella_cdn_metrics_plot_for(&prof, 1, 0);
        let up = plot
            .traffic
            .iter()
            .find(|s| s.label == "Upstream")
            .expect("upstream");
        assert!((up.data[1][1] - 4.0).abs() < 1e-9, "{:?}", up.data);
        let joins = plot
            .joins
            .iter()
            .find(|s| s.label == "Joins Down")
            .expect("joins");
        assert!((joins.data[1][1] - 4.0).abs() < 1e-9, "{:?}", joins.data);
        let clients = plot
            .joins
            .iter()
            .find(|s| s.label == "New Clients")
            .expect("clients");
        assert!(
            (clients.data[1][1] - 1.0).abs() < 1e-9,
            "{:?}",
            clients.data
        );
    }

    #[test]
    fn ella_relay_plot_uses_process_total_when_no_per_instance() {
        let prof = EllaProfile {
            counts_total: vec![
                ella_total(1_000_000, 2, 10, 4, 40, 1, 3, 0, 0),
                ella_total(2_000_000, 3, 20, 5, 80, 2, 4, 1, 5),
            ],
            ..Default::default()
        };
        let plot = build_ella_relay_plot(&prof, 0);
        assert!(plot.aggregated);
        assert!(plot.dn_channels_max.is_empty());
        assert!((series_peak(&plot.dn_channels_sum) - 20.0).abs() < 1e-6);
        let up = plot
            .subscribers
            .iter()
            .find(|s| s.label == "Internal")
            .unwrap();
        let dn = plot
            .subscribers
            .iter()
            .find(|s| s.label == "External")
            .unwrap();
        assert!((series_peak(&up.data) - 5.0).abs() < 1e-6);
        assert!((series_peak(&dn.data) - 80.0).abs() < 1e-6);
        assert!(
            (series_peak(named_pending(&plot, "Upstream Leave")) - 2.0).abs() < 1e-6,
            "upstream leave {}",
            series_peak(named_pending(&plot, "Upstream Leave"))
        );
        assert!(
            (series_peak(named_pending(&plot, "Upstream Retire")) - 1.0).abs() < 1e-6,
            "upstream retire {}",
            series_peak(named_pending(&plot, "Upstream Retire"))
        );
        assert!((series_peak(&plot.dn_leave_sum) - 4.0).abs() < 1e-6);
        assert!((series_peak(&plot.dn_retire_sum) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn ella_relay_plot_prefers_channel_count_over_total() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.counts = vec![
            ella_count(1_000_000, 0, 1, 2, 1),
            ella_count(1_000_000, 2, 1, 3, 1),
        ];
        prof.counts_total = vec![ella_total(1_000_000, 9, 99, 9, 99, 0, 0, 0, 0)];
        let plot = build_ella_relay_plot(&prof, 0);
        assert!(!plot.aggregated);
        assert_eq!(plot.instance_count, 2);
        assert!(plot.dn_channels_sum.is_empty());
    }

    fn relay_err(ts: u64, code: u8) -> EllaEvent {
        ella_evt(
            ts,
            0,
            EllaPayload::RelayError {
                code,
                instance_id: 0,
                channel_id: 0,
                channel_id_known: false,
                detail: 0,
                message_type: 0,
                subscriber_address: String::new(),
                subscriber_port: 0,
            },
        )
    }

    #[test]
    fn ella_relay_errors_split_by_code() {
        let prof = EllaProfile {
            events: vec![
                relay_err(1_000_000, 30),
                relay_err(2_000_000, 33),
                relay_err(3_000_000, 30),
                ella_evt(
                    4_000_000,
                    1,
                    EllaPayload::ChannelError {
                        code: 4,
                        instance_id: 0,
                        detail: 0,
                    },
                ),
            ],
            ..Default::default()
        };
        let plot = build_ella_relay_plot(&prof, 0);
        let labels: Vec<&str> = plot.errors.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "UNKNOWN_CHANNEL_DATA",
                "CAT_PACKET_TOO_SHORT",
                "DATA_QUEUE_FULL"
            ]
        );
        assert!((series_peak(&plot.errors[0].data) - 2.0).abs() < 1e-6);
        assert!((series_peak(&plot.errors[1].data) - 1.0).abs() < 1e-6);
        assert!((series_peak(&plot.errors[2].data) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ella_relay_cat_errors_split_by_code() {
        let prof = EllaProfile {
            events: vec![
                relay_err(1_000_000, 39),
                relay_err(2_000_000, 39),
                relay_err(3_000_000, 42),
            ],
            ..Default::default()
        };
        let plot = build_ella_relay_plot(&prof, 0);
        let labels: Vec<&str> = plot.errors.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["CAT_UNKNOWN_CLIENT", "CAT_DECRYPT_FAILED"]);
        assert!((series_peak(&plot.errors[0].data) - 2.0).abs() < 1e-6);
        assert!((series_peak(&plot.errors[1].data) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ella_channel_errors_are_scoped_to_provider_and_session() {
        let error = |provider_id, ts, session_id, code| {
            ella_evt_on(
                provider_id,
                ts,
                session_id,
                EllaPayload::ChannelError {
                    code,
                    instance_id: 0,
                    detail: 0,
                },
            )
        };
        let prof = EllaProfile {
            events: vec![
                error(1, 1_000_000, 7, 4),
                error(1, 2_000_000, 8, 2),
                error(2, 3_000_000, 7, 1),
                error(1, 4_000_000, 7, 4),
            ],
            ..Default::default()
        };

        let plot = build_ella_channel_plot(&prof, 1, 7, 0);
        assert_eq!(plot.errors.len(), 1);
        assert_eq!(plot.errors[0].label, "DATA_QUEUE_FULL");
        assert_eq!(plot.errors[0].data, [[1.0, 1.0], [4.0, 1.0], [4.0, 2.0]]);
        let mut ch7 = mk_ch(1, 7, "alpha", 1);
        ch7.event_count = 2;
        let mut ch8 = mk_ch(1, 8, "alpha", 1);
        ch8.event_count = 1;
        ch8.first_ts = 2_000_000;
        ch8.last_ts = 2_000_000;
        let mut ch9 = mk_ch(2, 7, "alpha", 1);
        ch9.event_count = 1;
        let mut with_channels = prof;
        with_channels.channels.insert((1, 7), ch7);
        with_channels.channels.insert((1, 8), ch8);
        with_channels.channels.insert((2, 7), ch9);
        // One row per title now, so its count is every node's errors
        // across every reincarnation.
        let rows = build_ella_channel_list(&with_channels);
        let alpha: Vec<_> = rows.iter().filter(|r| r.short_label == "alpha").collect();
        assert_eq!(alpha.len(), 1);
        assert_eq!(alpha[0].session_count, 3);
        assert_eq!(alpha[0].error_count, 4);
    }

    #[test]
    fn ella_channel_plot_sides_when_many_instances() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        for i in 2u32..7 {
            prof.instances.insert((1, i), ELLA_SIDE_DOWNSTREAM);
        }
        let mut events = vec![ella_evt(
            1_000_000,
            1,
            EllaPayload::ChannelSubscriber {
                state: ELLA_SUBSCRIBER_SUBSCRIBED,
                reason: 0,
                instance_id: 0,
                subscriber_address: "127.0.0.1".into(),
                subscriber_port: 1,
                subscribers: 1,
            },
        )];
        for i in 2u32..7 {
            events.push(ella_evt(
                1_100_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: i,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: i as u16,
                    subscribers: 1,
                },
            ));
        }
        prof.events = events;
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        let labels: Vec<&str> = plot.subscribers.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["Internal", "External"]);
        let dn = plot
            .subscribers
            .iter()
            .find(|s| s.label == "External")
            .unwrap();
        let peak = dn.data.iter().map(|p| p[1]).fold(0.0, f64::max);
        assert!((peak - 5.0).abs() < 1e-9, "external peak {peak}");
        let up = plot
            .subscribers
            .iter()
            .find(|s| s.label == "Internal")
            .unwrap();
        let up_peak = up.data.iter().map(|p| p[1]).fold(0.0, f64::max);
        assert!((up_peak - 1.0).abs() < 1e-9, "internal peak {up_peak}");
    }

    #[test]
    fn ella_channel_plot_stays_per_instance_when_few() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 0,
                    subscriber_address: "127.0.0.1".into(),
                    subscriber_port: 1,
                    subscribers: 1,
                },
            ),
            ella_evt(
                1_100_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 2,
                    subscribers: 1,
                },
            ),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        let labels: Vec<&str> = plot.subscribers.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["Internal", "External"]);
        assert!(plot.subscribers.iter().all(|s| s.label != "total"));
    }

    #[test]
    fn ella_channel_events_split_joins_from_lifecycle() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::ChannelState {
                    state: ELLA_LIFECYCLE_CREATED,
                    reason: 0,
                    instance_id: 0,
                    subscribers: 0,
                },
            ),
            ella_evt(
                1_100_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 0,
                    subscriber_address: "127.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 0,
                },
            ),
            ella_evt(
                2_000_000,
                1,
                EllaPayload::ChannelDestroyed {
                    reason: 1,
                    instance_id: 0,
                    subscribers: 0,
                },
            ),
        ];
        let ev = build_ella_channel_events(&prof, 1, 1, 0);
        assert_eq!(ev.n_lifecycle, 2);
        assert_eq!(ev.n_joins, 1);
        assert_eq!(ev.n_created, 1);
        assert_eq!(ev.n_destroyed, 1);
        assert!((ev.first_created_s.unwrap() - 1.0).abs() < 1e-9);
        assert!((ev.last_destroyed_s.unwrap() - 2.0).abs() < 1e-9);
        assert_eq!(ev.rows[0].name, "CHANNEL_STATE");
        assert_eq!(ev.rows[0].instance_label, "Internal i0");
        assert!(!ev.rows[0].is_join);
        assert!(!ev.rows[0].is_object);
        assert!(ev.rows[1].is_join);
        assert!(ev.rows[1].detail.contains("127.0.0.1:9"));
        assert_eq!(ev.n_objects, 0);
    }

    #[test]
    fn ella_channel_object_plot_started_until_completed() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_STARTED,
                    instance_id: 2,
                    boundary_index: 1,
                    first_packet: 10,
                    packets: 0,
                    bytes: 0,
                },
            ),
            ella_evt(
                2_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_COMPLETED,
                    instance_id: 2,
                    boundary_index: 1,
                    first_packet: 10,
                    packets: 8,
                    bytes: 1000,
                },
            ),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert!(plot.subscribers.is_empty());
        assert_eq!(plot.objects.len(), 1);
        assert_eq!(plot.objects[0].label, "External");
        assert_eq!(plot.objects[0].data, [[1.0, 1.0], [2.0, 1.0], [2.0, 0.0]]);
        assert_eq!(plot.object_rate.len(), 1);
        assert_eq!(plot.object_rate[0].label, "TX");
        assert!((plot.object_rate[0].data[0][1] - 0.008).abs() < 1e-12);
        let ev = build_ella_channel_events(&prof, 1, 1, 0);
        assert_eq!(ev.n_objects, 2);
        assert_eq!(ev.n_lifecycle, 0);
        assert!(ev.rows[0].is_object);
        assert!(ev.rows[0].detail.contains("slot 1"));
        assert!(ev.rows[1].detail.contains("8 pkts"));
    }

    #[test]
    fn ella_channel_object_rate_spreads_bytes_over_lifetime() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_STARTED,
                    instance_id: 2,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 0,
                    bytes: 0,
                },
            ),
            ella_evt(
                3_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_COMPLETED,
                    instance_id: 2,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 40,
                    bytes: 250_000,
                },
            ),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(plot.object_rate.len(), 1);
        assert_eq!(plot.object_rate[0].label, "TX");
        assert_eq!(
            plot.object_rate[0].data,
            [[1.0, 1.0], [3.0, 1.0], [3.0, 0.0]]
        );
    }

    #[test]
    fn ella_channel_object_rate_sides_only_step_where_they_change() {
        // One upstream instance ingesting while many downstream ones
        // fan out. The RX series used to gain a point at every
        // downstream edge too, which on a real relay pushed it past
        // MAX_DISPLAY_POINTS and got it mean-downsampled into ramps
        // while the same stream on the Server stayed a clean staircase.
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        for i in 1u32..=8 {
            prof.instances.insert((1, i), ELLA_SIDE_DOWNSTREAM);
        }
        let obj = |ts: u64, inst: u32, state: u8, bytes: u32| {
            ella_evt(
                ts,
                1,
                EllaPayload::ObjectState {
                    state,
                    instance_id: inst,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 10,
                    bytes,
                },
            )
        };
        // The upstream instance carries two objects; each downstream
        // instance carries the same two, staggered so their edges land
        // on distinct timestamps.
        for inst in 0u32..=8 {
            for obj_i in 0..2u64 {
                let base = 1_000_000 + obj_i * 4_000_000 + u64::from(inst) * 100_000;
                prof.events.push(obj(base, inst, ELLA_OBJECT_STARTED, 0));
                prof.events
                    .push(obj(base + 1_000_000, inst, ELLA_OBJECT_COMPLETED, 250_000));
            }
        }
        prof.events.sort_by_key(|e| e.ts);

        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        let rx = plot
            .object_rate
            .iter()
            .find(|s| s.label == "RX")
            .expect("aggregate RX");
        let tx = plot
            .object_rate
            .iter()
            .find(|s| s.label == "TX")
            .expect("aggregate TX");
        // Two objects on one instance: four square transitions plus the
        // hold out to the last event in the channel.
        assert_eq!(rx.data.len(), 8);
        assert!(rx.data.iter().all(|p| p[1] == 0.0 || p[1] == 2.0));
        // The downstream side really does change that often, so it
        // keeps its density.
        assert!(tx.data.len() > rx.data.len() * 3);
    }

    #[test]
    fn ella_channel_object_rate_includes_abandoned_bytes() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_STARTED,
                    instance_id: 0,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 0,
                    bytes: 0,
                },
            ),
            ella_evt(
                2_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_ABANDONED,
                    instance_id: 0,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 10,
                    bytes: 125_000,
                },
            ),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(plot.object_rate[0].label, "RX");
        assert_eq!(
            plot.object_rate[0].data,
            [[1.0, 1.0], [2.0, 1.0], [2.0, 0.0]]
        );
    }

    #[test]
    fn ella_channel_object_rate_holds_across_back_to_back_objects() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_STARTED,
                    instance_id: 2,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 0,
                    bytes: 0,
                },
            ),
            ella_evt(
                2_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_COMPLETED,
                    instance_id: 2,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 20,
                    bytes: 250_000,
                },
            ),
            ella_evt(
                2_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_STARTED,
                    instance_id: 2,
                    boundary_index: 1,
                    first_packet: 21,
                    packets: 0,
                    bytes: 0,
                },
            ),
            ella_evt(
                3_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_COMPLETED,
                    instance_id: 2,
                    boundary_index: 1,
                    first_packet: 21,
                    packets: 20,
                    bytes: 250_000,
                },
            ),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(plot.object_rate[0].label, "TX");
        assert!(
            plot.object_rate[0]
                .data
                .iter()
                .all(|p| p[1] == 0.0 || (p[1] - 2.0).abs() < 1e-12),
            "{:?}",
            plot.object_rate[0].data
        );
        assert!(
            !plot.object_rate[0]
                .data
                .windows(3)
                .any(|w| w[0][1] > 0.0 && w[1][1] == 0.0 && w[2][1] > 0.0),
            "rate spiked to 0 between back-to-back objects: {:?}",
            plot.object_rate[0].data
        );
    }

    #[test]
    fn ella_channel_object_rate_drops_across_idle_gap() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_STARTED,
                    instance_id: 2,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 0,
                    bytes: 0,
                },
            ),
            ella_evt(
                2_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_COMPLETED,
                    instance_id: 2,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 20,
                    bytes: 250_000,
                },
            ),
            ella_evt(
                2_100_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_STARTED,
                    instance_id: 2,
                    boundary_index: 1,
                    first_packet: 21,
                    packets: 0,
                    bytes: 0,
                },
            ),
            ella_evt(
                3_100_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_COMPLETED,
                    instance_id: 2,
                    boundary_index: 1,
                    first_packet: 21,
                    packets: 20,
                    bytes: 250_000,
                },
            ),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(plot.object_rate[0].label, "TX");
        assert!(
            plot.object_rate[0]
                .data
                .windows(3)
                .any(|w| (w[0][0] - 2.0).abs() < 1e-12
                    && w[0][1] == 2.0
                    && w[1][1] == 0.0
                    && w[2][0] >= 2.1),
            "idle gap ≥ 50 ms should drop to 0: {:?}",
            plot.object_rate[0].data
        );
    }

    #[test]
    fn ella_object_rate_stack_splits_rx_tx_and_titles() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((1, 3), ELLA_SIDE_DOWNSTREAM);
        let obj = |ts, sid, inst, state, bytes| {
            ella_evt(
                ts,
                sid,
                EllaPayload::ObjectState {
                    state,
                    instance_id: inst,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 0,
                    bytes,
                },
            )
        };
        let create = |ts, sid, name: &str, inst| {
            ella_evt(
                ts,
                sid,
                EllaPayload::SessionCreate {
                    channel_name: name.into(),
                    version: 2,
                    instance_id: inst,
                },
            )
        };
        // alpha: 10 Mbps RX, two downstream copies at 10 Mbps each → 20 Mbps TX.
        // beta: 5 Mbps RX only.
        prof.events = vec![
            create(900_000, 1, "alpha", 0),
            create(900_000, 2, "beta", 0),
            obj(1_000_000, 1, 0, ELLA_OBJECT_STARTED, 0),
            obj(2_000_000, 1, 0, ELLA_OBJECT_COMPLETED, 1_250_000),
            obj(1_000_000, 2, 0, ELLA_OBJECT_STARTED, 0),
            obj(2_000_000, 2, 0, ELLA_OBJECT_COMPLETED, 625_000),
            obj(1_000_000, 1, 2, ELLA_OBJECT_STARTED, 0),
            obj(2_000_000, 1, 2, ELLA_OBJECT_COMPLETED, 1_250_000),
            obj(1_000_000, 1, 3, ELLA_OBJECT_STARTED, 0),
            obj(2_000_000, 1, 3, ELLA_OBJECT_COMPLETED, 1_250_000),
        ];
        let (rx, tx) = build_ella_object_rate_stacks(&prof, 0);
        assert_eq!(
            rx.series
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        assert_eq!(tx.series.len(), 1);
        assert_eq!(tx.series[0].label, "alpha");
        assert!((occupancy_at(&rx.series[0].data, 1.5) - 10.0).abs() < 1e-9);
        assert!((occupancy_at(&rx.series[1].data, 1.5) - 5.0).abs() < 1e-9);
        assert!((occupancy_at(&tx.series[0].data, 1.5) - 20.0).abs() < 1e-9);
        assert!((occupancy_at(&rx.series[0].data, 2.1) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn ella_node_title_stacks_rank_by_peak_subscribers() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        let sub = |ts, sid, n| {
            ella_evt(
                ts,
                sid,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: n,
                    subscribers: 1,
                },
            )
        };
        let obj = |ts, sid, inst, state, bytes| {
            ella_evt(
                ts,
                sid,
                EllaPayload::ObjectState {
                    state,
                    instance_id: inst,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 0,
                    bytes,
                },
            )
        };
        let create = |ts, sid, name: &str, inst| {
            ella_evt(
                ts,
                sid,
                EllaPayload::SessionCreate {
                    channel_name: name.into(),
                    version: 2,
                    instance_id: inst,
                },
            )
        };
        // alpha: 10 Mbps RX, one viewer. beta: 5 Mbps RX, two viewers.
        prof.events = vec![
            create(900_000, 1, "alpha", 0),
            create(900_000, 2, "beta", 0),
            sub(1_000_000, 1, 1),
            sub(1_000_000, 2, 2),
            sub(1_000_000, 2, 3),
            obj(1_000_000, 1, 0, ELLA_OBJECT_STARTED, 0),
            obj(2_000_000, 1, 0, ELLA_OBJECT_COMPLETED, 1_250_000),
            obj(1_000_000, 2, 0, ELLA_OBJECT_STARTED, 0),
            obj(2_000_000, 2, 0, ELLA_OBJECT_COMPLETED, 625_000),
        ];
        let (stack, rx, _) = build_ella_node_title_stacks(&prof, 0, 1);
        assert_eq!(
            stack
                .series
                .iter()
                .map(|t| t.label.as_str())
                .collect::<Vec<_>>(),
            ["beta", "alpha"]
        );
        assert_eq!(
            rx.series
                .iter()
                .map(|t| t.label.as_str())
                .collect::<Vec<_>>(),
            ["beta", "alpha"]
        );
        let (unranked_rx, _) = build_ella_object_rate_stacks(&prof, 0);
        assert_eq!(
            unranked_rx
                .series
                .iter()
                .map(|t| t.label.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
    }

    #[test]
    fn ella_object_rate_stack_holds_gop_and_drops_idle() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        let obj = |ts, state, bytes| {
            ella_evt(
                ts,
                1,
                EllaPayload::ObjectState {
                    state,
                    instance_id: 0,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 0,
                    bytes,
                },
            )
        };
        prof.events = vec![
            ella_evt(
                0,
                1,
                EllaPayload::SessionCreate {
                    channel_name: "alpha".into(),
                    version: 2,
                    instance_id: 0,
                },
            ),
            obj(1_000_000, ELLA_OBJECT_STARTED, 0),
            obj(2_000_000, ELLA_OBJECT_COMPLETED, 1_250_000),
            obj(2_001_000, ELLA_OBJECT_STARTED, 0),
            obj(3_001_000, ELLA_OBJECT_COMPLETED, 1_250_000),
            obj(3_200_000, ELLA_OBJECT_STARTED, 0),
            obj(4_200_000, ELLA_OBJECT_COMPLETED, 1_250_000),
        ];
        let (rx, _) = build_ella_object_rate_stacks(&prof, 0);
        assert_eq!(rx.series.len(), 1);
        assert!((occupancy_at(&rx.series[0].data, 2.0005) - 10.0).abs() < 1e-9);
        assert!((occupancy_at(&rx.series[0].data, 3.1) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn ella_short_label_strips_path_and_query() {
        assert_eq!(
            ella_channel_short_label("/2557822/1263725932/2087032103?dutyCycle=0.91&fec=0.1"),
            "2087032103"
        );
        assert_eq!(ella_channel_short_label("health_channel"), "health_channel");
    }

    #[test]
    fn ella_channel_list_groups_by_name_and_uses_peak() {
        let mut a = EllaChannelInfo {
            provider_id: 1,
            session_id: 1,
            channel_name: "health_channel".into(),
            version: 2,
            first_ts: 0,
            last_ts: 1_000_000,
            event_count: 10,
            channel_ids: FxHashMap::default(),
            last_subscribers: 0,
            peak_subscribers: 1,
            occ_by_inst: FxHashMap::default(),
            last_state: Some(ELLA_LIFECYCLE_CREATED),
            destroy_reason: Some(1),
        };
        a.channel_ids.insert(0, 7);
        let mut b = a.clone();
        b.session_id = 2;
        b.first_ts = 2_000_000;
        b.last_ts = 3_000_000;
        b.peak_subscribers = 2;
        b.channel_ids.insert(2, 7);
        let mut busy = a.clone();
        busy.session_id = 3;
        busy.channel_name = "/x/y/title?q=1".into();
        busy.peak_subscribers = 50;
        busy.event_count = 100;
        busy.last_ts = 5_000_000;
        busy.channel_ids = FxHashMap::default();
        busy.channel_ids.insert(0, 9);
        let mut prof = EllaProfile::default();
        prof.channels.insert((1, 1), a);
        prof.channels.insert((1, 2), b);
        prof.channels.insert((1, 3), busy);
        let rows = build_ella_channel_list(&prof);
        assert_eq!(rows.len(), 2);
        let summary = ella_relay_summary(&prof);
        assert_eq!(summary.titles, 2);
        assert_eq!(summary.channel_sessions, 3);
        assert_eq!(rows[0].short_label, "title");
        assert_eq!(rows[0].peak_subscribers, 50);
        assert_eq!(rows[1].short_label, "health_channel");
        assert_eq!(rows[1].session_count, 2);
        assert_eq!(rows[1].peak_subscribers, 2);
        assert_eq!(rows[0].rx_rate_mbps, None);
        assert_eq!(rows[1].rx_rate_mbps, None);
        assert_eq!(rows[0].stretch_frac, None);
        assert_eq!(rows[1].stretch_frac, None);
        assert_eq!(rows[0].frame_rate_mbps, None);
        assert_eq!(rows[1].frame_rate_mbps, None);
        assert_eq!(rows[1].ids_label, "7");
        assert_eq!(rows[1].instance_count, 2);
        assert_eq!(rows[0].error_count, 0);
        assert_eq!(rows[1].error_count, 0);
        let mut two_inst = FxHashMap::default();
        two_inst.insert(0, 7);
        two_inst.insert(2, 7);
        assert_eq!(ella_compact_channel_ids(&two_inst), "7 · 2 inst");
        assert_eq!(ella_channel_for_stack_label(&prof, "title"), Some((1, 3)));
        assert_eq!(
            ella_channel_for_stack_label(&prof, "health_channel"),
            Some((1, 1))
        );
        assert_eq!(ella_channel_for_stack_label(&prof, "Other"), None);
    }

    #[test]
    fn ella_channel_list_rx_rate_is_time_weighted_mode_of_upstream() {
        let mut ch = EllaChannelInfo {
            provider_id: 1,
            session_id: 1,
            channel_name: "alpha".into(),
            version: 2,
            first_ts: 0,
            last_ts: 12_000_000,
            event_count: 10,
            channel_ids: FxHashMap::default(),
            last_subscribers: 0,
            peak_subscribers: 1,
            occ_by_inst: FxHashMap::default(),
            last_state: Some(ELLA_LIFECYCLE_CREATED),
            destroy_reason: Some(1),
        };
        ch.channel_ids.insert(0, 7);
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.channels.insert((1, 1), ch);
        let mut events = Vec::new();
        // Four 10 Mbps GOPs (2.5 MB / 2 s) plus one 1 Mbps GOP. Downstream
        // copies at 100 Mbps must not move the mode.
        for i in 0..4u32 {
            let t0 = 1_000_000 + u64::from(i) * 2_000_000;
            events.push(ella_evt(
                t0,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_STARTED,
                    instance_id: 0,
                    boundary_index: i as u8,
                    first_packet: 1,
                    packets: 0,
                    bytes: 0,
                },
            ));
            events.push(ella_evt(
                t0 + 2_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_COMPLETED,
                    instance_id: 0,
                    boundary_index: i as u8,
                    first_packet: 1,
                    packets: 40,
                    bytes: 2_500_000,
                },
            ));
        }
        events.push(ella_evt(
            9_000_000,
            1,
            EllaPayload::ObjectState {
                state: ELLA_OBJECT_STARTED,
                instance_id: 0,
                boundary_index: 4,
                first_packet: 1,
                packets: 0,
                bytes: 0,
            },
        ));
        events.push(ella_evt(
            11_000_000,
            1,
            EllaPayload::ObjectState {
                state: ELLA_OBJECT_COMPLETED,
                instance_id: 0,
                boundary_index: 4,
                first_packet: 1,
                packets: 4,
                bytes: 250_000,
            },
        ));
        events.push(ella_evt(
            1_000_000,
            1,
            EllaPayload::ObjectState {
                state: ELLA_OBJECT_STARTED,
                instance_id: 2,
                boundary_index: 0,
                first_packet: 1,
                packets: 0,
                bytes: 0,
            },
        ));
        events.push(ella_evt(
            3_000_000,
            1,
            EllaPayload::ObjectState {
                state: ELLA_OBJECT_COMPLETED,
                instance_id: 2,
                boundary_index: 0,
                first_packet: 1,
                packets: 400,
                bytes: 25_000_000,
            },
        ));
        prof.events = events;
        let rows = build_ella_channel_list(&prof);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rx_rate_mbps, Some(10.0));
        assert_eq!(format_ella_rx_rate(rows[0].rx_rate_mbps), "10 Mbps");
        assert_eq!(
            ella_stack_legend_hover(
                &rows[0].short_label,
                EllaStackLegendStats {
                    rate_mbps: rows[0].rx_rate_mbps,
                    peak_external: Some(12),
                    avg_external: Some(4.5),
                }
            ),
            format!(
                "Open {}  ·  10 Mbps\npeak 12  ·  avg 4.5 external",
                rows[0].short_label
            )
        );
    }

    #[test]
    fn ella_channel_list_server_rate_is_time_weighted_mode_of_tx() {
        let mut ch = EllaChannelInfo {
            provider_id: 9,
            session_id: 1,
            channel_name: "alpha".into(),
            version: 2,
            first_ts: 0,
            last_ts: 12_000_000,
            event_count: 10,
            channel_ids: FxHashMap::default(),
            last_subscribers: 0,
            peak_subscribers: 1,
            occ_by_inst: FxHashMap::default(),
            last_state: Some(ELLA_LIFECYCLE_CREATED),
            destroy_reason: Some(1),
        };
        ch.channel_ids.insert(0, 7);
        let mut prof = EllaProfile::default();
        prof.instances.insert((9, 0), ELLA_SIDE_DOWNSTREAM);
        prof.channels.insert((9, 1), ch);
        let mut events = Vec::new();
        for i in 0..4u32 {
            let t0 = 1_000_000 + u64::from(i) * 2_000_000;
            events.push(ella_evt_on(
                9,
                t0,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_STARTED,
                    instance_id: 0,
                    boundary_index: i as u8,
                    first_packet: 1,
                    packets: 0,
                    bytes: 0,
                },
            ));
            events.push(ella_evt_on(
                9,
                t0 + 2_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_COMPLETED,
                    instance_id: 0,
                    boundary_index: i as u8,
                    first_packet: 1,
                    packets: 40,
                    bytes: 2_500_000,
                },
            ));
        }
        events.push(ella_evt_on(
            9,
            9_000_000,
            1,
            EllaPayload::ObjectState {
                state: ELLA_OBJECT_STARTED,
                instance_id: 0,
                boundary_index: 4,
                first_packet: 1,
                packets: 0,
                bytes: 0,
            },
        ));
        events.push(ella_evt_on(
            9,
            11_000_000,
            1,
            EllaPayload::ObjectState {
                state: ELLA_OBJECT_COMPLETED,
                instance_id: 0,
                boundary_index: 4,
                first_packet: 1,
                packets: 4,
                bytes: 250_000,
            },
        ));
        prof.events = events;
        let rows = build_ella_channel_list(&prof);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].node, "Server");
        assert_eq!(rows[0].rx_rate_mbps, Some(10.0));
        assert_eq!(format_ella_rx_rate(rows[0].rx_rate_mbps), "10 Mbps");
        assert_eq!(rows[0].stretch_frac, None);
        assert_eq!(rows[0].frame_rate_mbps, None);
    }

    #[test]
    fn ella_channel_list_stretch_and_frame_rate_from_frame_sample() {
        let mut ch = EllaChannelInfo {
            provider_id: 9,
            session_id: 1,
            channel_name: "alpha".into(),
            version: 2,
            first_ts: 0,
            last_ts: 2_000_000,
            event_count: 4,
            channel_ids: FxHashMap::default(),
            last_subscribers: 0,
            peak_subscribers: 1,
            occ_by_inst: FxHashMap::default(),
            last_state: Some(ELLA_LIFECYCLE_CREATED),
            destroy_reason: Some(1),
        };
        ch.channel_ids.insert(0, 7);
        let mut prof = EllaProfile::default();
        prof.roles.insert(9, ELLA_ROLE_ORIGIN);
        prof.instances.insert((9, 0), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((9, 99), ELLA_SIDE_UPSTREAM);
        prof.channels.insert((9, 1), ch);
        prof.frame_samples = vec![
            EllaFrameSample {
                ts: 1_000_000,
                provider_id: 9,
                session_id: 1,
                instance_id: 0,
                frames: 10,
                frames_stretched: 2,
                sum_frame_len: 250_000,
                sum_planned_us: 400_000,
                sum_actual_us: 500_000,
                sum_fwd_us: 0,
                max_overshoot_us: 0,
                sum_packets: 0,
            },
            EllaFrameSample {
                ts: 1_000_000,
                provider_id: 9,
                session_id: 1,
                instance_id: 99,
                frames: 8,
                frames_stretched: 8,
                sum_frame_len: 1_000_000,
                sum_planned_us: 1,
                sum_actual_us: 1,
                sum_fwd_us: 0,
                max_overshoot_us: 0,
                sum_packets: 0,
            },
        ];
        let rows = build_ella_channel_list(&prof);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].stretch_frac, Some(0.2));
        assert_eq!(rows[0].frame_rate_mbps, Some(4.0));
        assert_eq!(format_ella_stretch(rows[0].stretch_frac), "20%");
        assert_eq!(format_ella_rx_rate(rows[0].frame_rate_mbps), "4 Mbps");

        let mut relay = EllaProfile::default();
        relay.roles.insert(1, ELLA_ROLE_RELAY);
        relay.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        relay.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        let mut rch = EllaChannelInfo {
            provider_id: 1,
            session_id: 3,
            channel_name: "beta".into(),
            version: 2,
            first_ts: 0,
            last_ts: 2_000_000,
            event_count: 2,
            channel_ids: FxHashMap::default(),
            last_subscribers: 0,
            peak_subscribers: 1,
            occ_by_inst: FxHashMap::default(),
            last_state: Some(ELLA_LIFECYCLE_CREATED),
            destroy_reason: Some(1),
        };
        rch.channel_ids.insert(0, 4);
        relay.channels.insert((1, 3), rch);
        relay.frame_samples = vec![
            EllaFrameSample {
                ts: 1_000_000,
                provider_id: 1,
                session_id: 3,
                instance_id: 0,
                frames: 4,
                frames_stretched: 1,
                sum_frame_len: 125_000,
                sum_planned_us: 250_000,
                sum_actual_us: 250_000,
                sum_fwd_us: 0,
                max_overshoot_us: 0,
                sum_packets: 0,
            },
            EllaFrameSample {
                ts: 1_000_000,
                provider_id: 1,
                session_id: 3,
                instance_id: 2,
                frames: 4,
                frames_stretched: 4,
                sum_frame_len: 999_000,
                sum_planned_us: 1,
                sum_actual_us: 1,
                sum_fwd_us: 1,
                max_overshoot_us: 0,
                sum_packets: 0,
            },
        ];
        let rrows = build_ella_channel_list(&relay);
        assert_eq!(rrows[0].stretch_frac, Some(0.25));
        assert_eq!(rrows[0].frame_rate_mbps, Some(4.0));
    }

    #[test]
    fn ella_subscription_stack_counts_live_subscribed() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::SessionCreate {
                    channel_name: "alpha".into(),
                    version: 2,
                    instance_id: 2,
                },
            ),
            ella_evt(
                1_100_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBING,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 1,
                },
            ),
            ella_evt(
                1_200_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 1,
                },
            ),
            ella_evt(
                2_000_000,
                2,
                EllaPayload::SessionCreate {
                    channel_name: "beta".into(),
                    version: 2,
                    instance_id: 2,
                },
            ),
            ella_evt(
                2_000_000,
                2,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBING,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 1,
                },
            ),
            ella_evt(
                2_500_000,
                2,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 1,
                },
            ),
            ella_evt(
                3_000_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_UNSUBSCRIBED,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 0,
                },
            ),
        ];
        let stack = build_ella_subscription_stack(&prof, 0);
        assert_eq!(stack.series.len(), 2);
        assert_eq!(stack.series[0].label, "alpha");
        assert_eq!(stack.series[1].label, "beta");
        assert!((occupancy_at(&stack.series[0].data, 1.15) - 0.0).abs() < 1e-9);
        assert!((occupancy_at(&stack.series[0].data, 1.5) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&stack.series[1].data, 2.2) - 0.0).abs() < 1e-9);
        assert!((occupancy_at(&stack.series[0].data, 2.7) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&stack.series[1].data, 2.7) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&stack.series[0].data, 3.5) - 0.0).abs() < 1e-9);
        assert!((occupancy_at(&stack.series[1].data, 3.5) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ella_subscription_stack_prefers_subscriber_events_over_channel_sample() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::SessionCreate {
                    channel_name: "alpha".into(),
                    version: 2,
                    instance_id: 2,
                },
            ),
            ella_evt(
                1_100_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 1,
                },
            ),
        ];
        prof.channel_samples = vec![
            ella_sample(1_200_000, 1, 2, 5),
            ella_sample(2_000_000, 1, 2, 3),
        ];
        let stack = build_ella_subscription_stack(&prof, 0);
        assert_eq!(stack.series.len(), 1);
        assert_eq!(stack.series[0].label, "alpha");
        assert!((occupancy_at(&stack.series[0].data, 1.2) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&stack.series[0].data, 2.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ella_subscription_stack_skips_sample_only_sessions() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![ella_evt(
            1_000_000,
            1,
            EllaPayload::SessionCreate {
                channel_name: "alpha".into(),
                version: 2,
                instance_id: 2,
            },
        )];
        prof.channels.insert((1, 1), mk_ch(1, 1, "alpha", 3));
        prof.channel_samples = vec![
            ella_sample(1_000_000, 1, 2, 3),
            ella_sample(1_000_000, 752, 2, 15_000),
            ella_sample(1_000_000, 753, 2, 15_000),
        ];
        let stack = build_ella_subscription_stack(&prof, 0);
        assert_eq!(stack.series.len(), 1);
        assert_eq!(stack.series[0].label, "alpha");
        assert!((occupancy_at(&stack.series[0].data, 1.5) - 3.0).abs() < 1e-9);
        let rows = build_ella_channel_list(&prof);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].short_label, "alpha");
    }

    #[test]
    fn ella_channel_plot_occupancy_prefers_channel_sample() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![ella_evt(
            1_100_000,
            1,
            EllaPayload::ChannelSubscriber {
                state: ELLA_SUBSCRIBER_SUBSCRIBED,
                reason: 0,
                instance_id: 2,
                subscriber_address: "10.0.0.1".into(),
                subscriber_port: 9,
                subscribers: 1,
            },
        )];
        prof.channel_samples = vec![
            ella_sample(1_000_000, 1, 2, 4),
            ella_sample(2_000_000, 1, 2, 2),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(plot.subscribers.len(), 1);
        assert!((occupancy_at(&plot.subscribers[0].data, 1.0) - 4.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.subscribers[0].data, 1.2) - 4.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.subscribers[0].data, 2.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn ella_channel_plot_occupancy_dedups_the_same_subscriber() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 1,
                },
            ),
            ella_evt(
                1_100_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 1,
                },
            ),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert!((occupancy_at(&plot.subscribers[0].data, 1.2) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ella_channel_object_rate_prefers_object_state_over_channel_sample() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        let s0 = ella_sample(1_000_000, 1, 2, 4);
        let mut s1 = ella_sample(2_000_000, 1, 2, 4);
        s1.object_bytes = 250_000;
        s1.objects_completed = 1;
        prof.channel_samples = vec![s0, s1];
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_STARTED,
                    instance_id: 2,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 0,
                    bytes: 0,
                },
            ),
            ella_evt(
                1_500_000,
                1,
                EllaPayload::ObjectState {
                    state: ELLA_OBJECT_COMPLETED,
                    instance_id: 2,
                    boundary_index: 0,
                    first_packet: 1,
                    packets: 1,
                    bytes: 500_000,
                },
            ),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(plot.object_rate.len(), 1);
        assert_eq!(plot.object_rate[0].label, "TX");
        assert!((occupancy_at(&plot.object_rate[0].data, 1.25) - 8.0).abs() < 1e-9);
        let tab = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        assert!((occupancy_at(&tab.object_rate[0].data, 1.25) - 8.0).abs() < 1e-9);
        let mut ch = mk_ch(1, 1, "alpha", 1);
        ch.event_count = 2;
        prof.channels.insert((1, 1), ch);
        let rows = build_ella_channel_list(&prof);
        assert_eq!(format_ella_rx_rate(rows[0].rx_rate_mbps), "8 Mbps");
        let (rx, tx) = build_ella_object_rate_stacks(&prof, 0);
        assert!(rx.series.is_empty());
        assert_eq!(tx.series.len(), 1);
        assert!((occupancy_at(&tx.series[0].data, 1.25) - 8.0).abs() < 1e-9);
    }

    #[test]
    fn ella_channel_object_rate_uses_channel_sample_bytes() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        let s0 = ella_sample(1_000_000, 1, 2, 4);
        let mut s1 = ella_sample(2_000_000, 1, 2, 4);
        s1.object_bytes = 250_000;
        s1.objects_completed = 1;
        prof.channel_samples = vec![s0, s1];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(plot.object_rate.len(), 1);
        assert_eq!(plot.object_rate[0].label, "TX");
        assert!((occupancy_at(&plot.object_rate[0].data, 1.5) - 2.0).abs() < 1e-9);
        let tab = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        assert_eq!(tab.object_rate.len(), 1);
        assert_eq!(tab.object_rate[0].label, "TX");
        assert!((occupancy_at(&tab.object_rate[0].data, 1.5) - 2.0).abs() < 1e-9);
        let mut ch = mk_ch(1, 1, "alpha", 1);
        ch.event_count = 1;
        prof.channels.insert((1, 1), ch);
        let rows = build_ella_channel_list(&prof);
        assert_eq!(format_ella_rx_rate(rows[0].rx_rate_mbps), "2 Mbps");
        let (rx, tx) = build_ella_object_rate_stacks(&prof, 0);
        assert!(rx.series.is_empty());
        assert_eq!(tx.series.len(), 1);
        assert!((occupancy_at(&tx.series[0].data, 1.5) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn ella_channel_frame_rate_uses_frame_sample() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.frame_samples = vec![ella_frame(1_000_000, 1, 2, 250_000, 1_000_000, 500_000, 0)];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(plot.frame_rate.len(), 2);
        assert_eq!(plot.frame_rate[0].label, "TX Planned");
        assert_eq!(plot.frame_rate[1].label, "TX Actual");
        assert!((occupancy_at(&plot.frame_rate[0].data, 1.0) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.frame_rate[1].data, 1.0) - 4.0).abs() < 1e-9);
        let agg = build_ella_frame_rate_aggregate(&prof, 1, 0);
        assert_eq!(agg.len(), 2);
        assert_eq!(agg[0].label, "Planned");
        assert!((occupancy_at(&agg[0].data, 1.0) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(&agg[1].data, 1.0) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn ella_channel_frame_rate_prefers_frame_events_over_frame_sample() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.frame_samples = vec![ella_frame(1_000_000, 1, 2, 250_000, 1_000_000, 500_000, 0)];
        prof.events = vec![ella_evt(
            1_000_000,
            1,
            EllaPayload::FrameSent {
                instance_id: 2,
                object_id: 1,
                frame_index: 0,
                first_packet: 1,
                packets: 10,
                is_probe: false,
                toggle: false,
                frame_len: 125_000,
                planned_send_us: 250_000,
                actual_send_us: 125_000,
                start_lateness_us: 0,
            },
        )];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(plot.frame_rate.len(), 2);
        assert!((occupancy_at(&plot.frame_rate[0].data, 1.0) - 4.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.frame_rate[1].data, 1.0) - 8.0).abs() < 1e-9);
        let agg = build_ella_frame_rate_aggregate(&prof, 1, 0);
        assert!((occupancy_at(&agg[0].data, 1.0) - 4.0).abs() < 1e-9);
        assert!((occupancy_at(&agg[1].data, 1.0) - 8.0).abs() < 1e-9);
    }

    #[test]
    fn ella_channel_frame_rate_relay_uses_wire_recv_fwd() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.frame_samples = vec![ella_frame(
            2_000_000, 1, 0, 125_000, 1_000_000, 500_000, 250_000,
        )];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(
            plot.frame_rate
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            ["RX Wire", "RX Recv", "RX Fwd"]
        );
        assert!((occupancy_at(&plot.frame_rate[0].data, 2.0) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.frame_rate[1].data, 2.0) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.frame_rate[2].data, 2.0) - 4.0).abs() < 1e-9);
        assert_eq!(
            plot.frame_duration
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            ["RX Wire", "RX Recv", "RX Fwd"]
        );
        assert!((occupancy_at(&plot.frame_duration[0].data, 2.0) - 1000.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.frame_duration[1].data, 2.0) - 500.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.frame_duration[2].data, 2.0) - 250.0).abs() < 1e-9);
    }

    #[test]
    fn ella_frame_duration_aggregate_splits_relay_sides() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((1, 3), ELLA_SIDE_DOWNSTREAM);
        let mut internal = ella_frame(1_000_000, 1, 0, 80_000, 300_000, 300_000, 322_000);
        internal.frames = 10;
        let mut external = ella_frame(1_000_000, 1, 3, 80_000, 300_000, 322_000, 322_000);
        external.frames = 10;
        prof.frame_samples = vec![internal, external];
        let up = build_ella_frame_duration_aggregate(&prof, 1, 0, Some(ELLA_SIDE_UPSTREAM));
        let dn = build_ella_frame_duration_aggregate(&prof, 1, 0, Some(ELLA_SIDE_DOWNSTREAM));
        assert_eq!(
            up.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(),
            ["Wire", "Recv", "Fwd"]
        );
        assert!((occupancy_at(&up[0].data, 1.0) - 30.0).abs() < 1e-9);
        assert!((occupancy_at(&up[1].data, 1.0) - 30.0).abs() < 1e-9);
        assert!((occupancy_at(&up[2].data, 1.0) - 32.2).abs() < 1e-9);
        assert!((occupancy_at(&dn[0].data, 1.0) - 30.0).abs() < 1e-9);
        assert!((occupancy_at(&dn[1].data, 1.0) - 32.2).abs() < 1e-9);
        assert!((occupancy_at(&dn[2].data, 1.0) - 32.2).abs() < 1e-9);
    }

    fn ella_frame_sent(ts: u64, instance_id: u32, frame_len: u32, us: u32) -> EllaEvent {
        ella_evt(
            ts,
            1,
            EllaPayload::FrameSent {
                instance_id,
                object_id: 1,
                frame_index: 0,
                first_packet: 1,
                packets: 10,
                is_probe: false,
                toggle: false,
                frame_len,
                planned_send_us: us,
                actual_send_us: us,
                start_lateness_us: 0,
            },
        )
    }

    #[test]
    fn ella_channel_frame_rate_holds_across_event_gap() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_frame_sent(1_000_000, 2, 125_000, 1_000_000),
            ella_frame_sent(4_000_000, 2, 125_000, 1_000_000),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert!((occupancy_at(&plot.frame_rate[1].data, 1.0) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.frame_rate[1].data, 2.0) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.frame_rate[1].data, 4.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ella_channel_frame_rate_bins_a_gop_burst() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        // Two frames 50 ms apart (same 100 ms bin) with different
        // instantaneous rates: the plot is one step, not a scribble.
        prof.events = vec![
            ella_frame_sent(1_000_000, 2, 125_000, 1_000_000),
            ella_frame_sent(1_050_000, 2, 125_000, 250_000),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        let actual = &plot.frame_rate[1].data;
        let mbps = 250_000.0 * 8.0 / 1_250_000.0;
        for p in actual {
            if p[1] > 0.0 {
                assert!(
                    (p[1] - mbps).abs() < 1e-9,
                    "expected one GOP rate {mbps}, got {actual:?}"
                );
            }
        }
        assert!(actual.iter().any(|p| (p[1] - mbps).abs() < 1e-9));
    }

    #[test]
    fn ella_channel_frame_rate_holds_across_sample_tick() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.frame_samples = vec![
            ella_frame(1_000_000, 1, 2, 125_000, 1_000_000, 1_000_000, 0),
            ella_frame(2_000_000, 1, 2, 125_000, 1_000_000, 1_000_000, 0),
            ella_frame(5_000_000, 1, 2, 125_000, 1_000_000, 1_000_000, 0),
        ];
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert!((occupancy_at(&plot.frame_rate[1].data, 1.5) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.frame_rate[1].data, 3.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ella_frame_legend_hides_secondary_metrics() {
        assert!(ella_frame_legend_hidden_by_default("TX Planned"));
        assert!(ella_frame_legend_hidden_by_default("RX Recv"));
        assert!(ella_frame_legend_hidden_by_default(
            "Fanout Server - TX Fwd"
        ));
        assert!(!ella_frame_legend_hidden_by_default("TX Actual"));
        assert!(!ella_frame_legend_hidden_by_default("RX Wire"));
        assert!(!ella_frame_legend_hidden_by_default("Planned extra"));
        assert!(ella_ratio_legend_hidden_by_default("Max"));
        assert!(ella_ratio_legend_hidden_by_default("TX Max"));
        assert!(!ella_ratio_legend_hidden_by_default("P95"));
        assert!(!ella_ratio_legend_hidden_by_default("Median"));
        assert!(!ella_ratio_legend_hidden_by_default("RX P95"));
        assert!(ella_object_legend_hidden_by_default("P75"));
        assert!(ella_object_legend_hidden_by_default("Max"));
        assert!(!ella_object_legend_hidden_by_default("Median"));
        assert!(!ella_object_legend_hidden_by_default("Server"));
    }

    fn obj_evt(
        pid: u16,
        ts: u64,
        session: u64,
        inst: u32,
        state: u8,
        first_pn: u32,
        payload: (u32, u32),
    ) -> EllaEvent {
        ella_evt_on(
            pid,
            ts,
            session,
            EllaPayload::ObjectState {
                state,
                instance_id: inst,
                boundary_index: 0,
                first_packet: first_pn,
                packets: payload.0,
                bytes: payload.1,
            },
        )
    }

    #[test]
    fn ella_channel_object_spans_completed_open_and_preempted() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            obj_evt(1, 1_000_000, 1, 2, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 2_000_000, 1, 2, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            // STARTED while the previous object is still open: the open
            // one closes as Open at the new start.
            obj_evt(1, 3_000_000, 1, 2, ELLA_OBJECT_STARTED, 200, (0, 0)),
            obj_evt(1, 4_000_000, 1, 2, ELLA_OBJECT_STARTED, 300, (0, 0)),
            sub_evt(1, 5_000_000, 1, 2, ELLA_SUBSCRIBER_SUBSCRIBED, 7),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        assert_eq!(plot.object_spans.len(), 3);
        let s = &plot.object_spans[0];
        assert_eq!(
            (s.t0, s.t1, s.state),
            (1.0, 2.0, EllaObjectSpanState::Completed)
        );
        assert_eq!(
            (s.first_pn, s.packets, s.bytes, s.obj_idx),
            (100, 10, 1000, 0)
        );
        let s = &plot.object_spans[1];
        assert_eq!((s.t0, s.t1, s.state), (3.0, 4.0, EllaObjectSpanState::Open));
        assert_eq!(s.obj_idx, 1);
        let s = &plot.object_spans[2];
        assert_eq!((s.t0, s.t1, s.state), (4.0, 5.0, EllaObjectSpanState::Open));
        assert_eq!(s.obj_idx, 2);
    }

    #[test]
    fn ella_channel_object_spans_share_rows_and_cross_label_ids() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(9, ELLA_ROLE_ORIGIN);
        prof.roles.insert(10, ELLA_ROLE_RELAY);
        prof.instances.insert((9, 0), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((10, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((10, 2), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((10, 3), ELLA_SIDE_DOWNSTREAM);
        prof.channels.insert((9, 1), mk_ch(9, 1, "/live/title", 1));
        prof.channels
            .insert((10, 1), mk_ch(10, 1, "/live/title", 4));
        prof.events = vec![
            obj_evt(9, 1_000_000, 1, 0, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(9, 2_000_000, 1, 0, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            ella_evt_on(
                9,
                1_500_000,
                1,
                EllaPayload::FrameSent {
                    instance_id: 0,
                    object_id: 42,
                    frame_index: 0,
                    first_packet: 100,
                    packets: 10,
                    is_probe: false,
                    toggle: false,
                    frame_len: 1000,
                    planned_send_us: 1000,
                    actual_send_us: 1000,
                    start_lateness_us: 0,
                },
            ),
            obj_evt(10, 1_010_000, 1, 2, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(10, 2_010_000, 1, 2, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            obj_evt(10, 1_012_000, 1, 3, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(10, 2_012_000, 1, 3, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
        ];
        let members = ella_channel_tab_members(&prof, 9, 1);
        let merged = build_ella_channel_plot_for(&prof, &members, 0);
        assert_eq!(merged.object_spans.len(), 2);
        let server = &merged.object_spans[0];
        assert_eq!(server.provider_id, 9);
        assert_eq!(
            (server.n, server.object_id, server.obj_idx),
            (1, Some(42), 0)
        );
        // One relay instance (lowest id among ties); object id via first_pn.
        let relay = &merged.object_spans[1];
        assert_eq!(relay.provider_id, 10);
        assert_eq!(relay.instance_id, 2);
        assert_eq!((relay.n, relay.object_id, relay.obj_idx), (1, Some(42), 0));
        assert!((relay.t0 - 1.010).abs() < 1e-9);
        assert!((relay.t1 - 2.010).abs() < 1e-9);
        assert_eq!(merged.object_id.len(), 2);
        assert!((occupancy_at(&merged.object_id[0].data, 1.5) - 42.0).abs() < 1e-9);
        assert!((occupancy_at(&merged.object_id[1].data, 1.5) - 42.0).abs() < 1e-9);
        assert!((occupancy_at(&merged.object_duration[0].data, 1.5) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&merged.object_duration[1].data, 1.5) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ella_channel_object_spans_one_instance_per_node() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((1, 3), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            obj_evt(1, 1_000_000, 1, 2, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 3_000_000, 1, 2, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            obj_evt(1, 3_000_000, 1, 2, ELLA_OBJECT_STARTED, 200, (0, 0)),
            obj_evt(1, 5_000_000, 1, 2, ELLA_OBJECT_COMPLETED, 200, (10, 1000)),
            obj_evt(1, 1_500_000, 1, 3, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 9_500_000, 1, 3, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        assert!(plot.object_spans.iter().all(|s| s.instance_id == 2));
        assert_eq!(plot.object_spans.len(), 2);
        let dur = &plot.object_duration[0].data;
        assert!((occupancy_at(dur, 2.0) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(dur, 4.0) - 2.0).abs() < 1e-9);
        let max = plot
            .object_duration
            .iter()
            .find(|s| s.label == "Max")
            .unwrap();
        assert!(occupancy_at(&max.data, 2.0) > 3.5);
    }

    #[test]
    fn ella_object_duration_ignores_early_abandons_on_the_same_gop() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        prof.instances.insert((1, 0), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            obj_evt(1, 1_000_000, 1, 0, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 3_000_000, 1, 0, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            obj_evt(1, 1_000_000, 1, 2, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 2_000_000, 1, 2, ELLA_OBJECT_ABANDONED, 100, (5, 500)),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        let dur = &plot.object_duration[0].data;
        assert!(
            dur.windows(2).all(|w| w[0][0] <= w[1][0]),
            "duration x must be non-decreasing: {dur:?}"
        );
        assert!((occupancy_at(dur, 1.5) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(dur, 2.5) - 2.0).abs() < 1e-9);
        let size = &plot.object_size[0].data;
        assert!((occupancy_at(size, 1.5) - 0.001).abs() < 1e-9);
        assert!((occupancy_at(size, 2.5) - 0.001).abs() < 1e-9);
    }

    #[test]
    fn ella_object_plots_drop_spans_across_capture_holes() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            obj_evt(1, 1_000_000, 1, 2, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(
                1,
                3_000_000,
                1,
                2,
                ELLA_OBJECT_COMPLETED,
                100,
                (10, 250_000),
            ),
            // Missing roll: STARTED in one file, COMPLETED minutes later.
            obj_evt(1, 4_000_000, 1, 2, ELLA_OBJECT_STARTED, 200, (0, 0)),
            obj_evt(
                1,
                304_000_000,
                1,
                2,
                ELLA_OBJECT_COMPLETED,
                200,
                (10, 250_000),
            ),
            obj_evt(1, 305_000_000, 1, 2, ELLA_OBJECT_STARTED, 300, (0, 0)),
            obj_evt(
                1,
                307_000_000,
                1,
                2,
                ELLA_OBJECT_COMPLETED,
                300,
                (10, 250_000),
            ),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        assert_eq!(plot.object_spans.len(), 2);
        assert!(plot.object_spans.iter().all(|s| s.t1 - s.t0 <= 3.0));
        let dur = &plot.object_duration[0].data;
        assert!((occupancy_at(dur, 2.0) - 2.0).abs() < 1e-9);
        assert_eq!(occupancy_at(dur, 150.0), 0.0);
        assert!((occupancy_at(dur, 306.0) - 2.0).abs() < 1e-9);
        let size = &plot.object_size[0].data;
        assert_eq!(occupancy_at(size, 150.0), 0.0);
        let rate = &plot.object_rate[0].data;
        assert_eq!(occupancy_at(rate, 150.0), 0.0);
        assert!(occupancy_at(rate, 2.0) > 0.0);
        assert!(occupancy_at(rate, 306.0) > 0.0);
    }

    #[test]
    fn ella_object_size_drops_to_zero_between_gops() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            obj_evt(1, 1_000_000, 1, 2, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 3_000_000, 1, 2, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            obj_evt(1, 8_000_000, 1, 2, ELLA_OBJECT_STARTED, 200, (0, 0)),
            obj_evt(1, 10_000_000, 1, 2, ELLA_OBJECT_COMPLETED, 200, (10, 2000)),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        let size = &plot.object_size[0].data;
        assert!((occupancy_at(size, 2.0) - 0.001).abs() < 1e-9);
        assert_eq!(occupancy_at(size, 5.0), 0.0);
        assert!((occupancy_at(size, 9.0) - 0.002).abs() < 1e-9);
    }

    #[test]
    fn ella_frame_ratio_percentiles_use_server_actual_over_planned() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        let sent = |ts, planned_send_us, actual_send_us| {
            ella_evt(
                ts,
                1,
                EllaPayload::FrameSent {
                    instance_id: 0,
                    object_id: 42,
                    frame_index: 0,
                    first_packet: 1,
                    packets: 4,
                    is_probe: true,
                    toggle: false,
                    frame_len: 1_200,
                    planned_send_us,
                    actual_send_us,
                    start_lateness_us: 0,
                },
            )
        };
        prof.events = vec![
            sent(1_000_000, 100, 100),
            sent(1_025_000, 100, 200),
            sent(1_050_000, 100, 300),
            sent(1_075_000, 100, 400),
            sent(1_075_000, 0, 999),
            sent(1_100_000, 100, 1_000),
            ella_evt_on(
                2,
                1_075_000,
                1,
                EllaPayload::FrameSent {
                    instance_id: 0,
                    object_id: 42,
                    frame_index: 0,
                    first_packet: 1,
                    packets: 4,
                    is_probe: true,
                    toggle: false,
                    frame_len: 1_200,
                    planned_send_us: 100,
                    actual_send_us: 999,
                    start_lateness_us: 0,
                },
            ),
        ];

        let series = build_ella_frame_ratio_percentiles(&prof, 1, 0);
        assert_eq!(
            series.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(),
            ["Median", "P75", "P95", "Max"]
        );
        assert_eq!(series[0].data, [[1.0, 2.5], [1.1, 10.0]]);
        assert_eq!(series[1].data, [[1.0, 3.25], [1.1, 10.0]]);
        assert!((series[2].data[0][1] - 3.85).abs() < 1e-9);
        assert_eq!(series[2].data[1], [1.1, 10.0]);
        assert_eq!(series[3].data, [[1.0, 4.0], [1.1, 10.0]]);
    }

    #[test]
    fn ella_frame_ratio_percentiles_use_relay_receive_over_wire() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        let relay = |ts, wire_send_us, recv_us| {
            ella_evt(
                ts,
                1,
                EllaPayload::FrameRelay {
                    instance_id: 0,
                    first_packet: 1,
                    packets: 4,
                    toggle: false,
                    boundary_index: 0,
                    frame_len: 1_200,
                    wire_send_us,
                    recv_us,
                    fwd_us: 0,
                    residency_us: 0,
                },
            )
        };
        prof.events = vec![
            relay(2_000_000, 100, 100),
            relay(2_025_000, 100, 200),
            relay(2_050_000, 100, 300),
            relay(2_075_000, 100, 400),
            relay(2_075_000, 0, 999),
            ella_evt(
                2_075_000,
                1,
                EllaPayload::FrameSent {
                    instance_id: 0,
                    object_id: 42,
                    frame_index: 0,
                    first_packet: 1,
                    packets: 4,
                    is_probe: true,
                    toggle: false,
                    frame_len: 1_200,
                    planned_send_us: 100,
                    actual_send_us: 999,
                    start_lateness_us: 0,
                },
            ),
        ];

        let series = build_ella_frame_ratio_percentiles(&prof, 1, 0);
        assert_eq!(series[0].data, [[2.0, 2.5]]);
        assert_eq!(series[1].data, [[2.0, 3.25]]);
        assert!((series[2].data[0][1] - 3.85).abs() < 1e-9);
        assert_eq!(series[3].data, [[2.0, 4.0]]);
    }

    #[test]
    fn ella_frame_sample_ratio_percentiles_aggregate_channel_instances_for_two_seconds() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.frame_samples = vec![
            // Two 100 ms samples on the same channel/instance form one 2 s
            // ratio: (100 + 300) / (100 + 100) = 2.
            ella_frame(1_000_000, 1, 0, 1_200, 100, 100, 0),
            ella_frame(1_100_000, 1, 0, 1_200, 100, 300, 0),
            ella_frame(1_200_000, 2, 0, 1_200, 100, 100, 0),
            ella_frame(1_300_000, 3, 0, 1_200, 100, 300, 0),
            ella_frame(1_400_000, 4, 0, 1_200, 100, 400, 0),
            ella_frame(1_500_000, 5, 0, 1_200, 0, 999, 0),
            ella_frame(3_000_000, 1, 0, 1_200, 100, 1_000, 0),
        ];

        let series = build_ella_frame_sample_ratio_percentiles(&prof, 1, 1_000_000);
        assert_eq!(series[0].data, [[0.0, 2.5], [2.0, 10.0]]);
        assert_eq!(series[1].data, [[0.0, 3.25], [2.0, 10.0]]);
        assert!((series[2].data[0][1] - 3.85).abs() < 1e-9);
        assert_eq!(series[2].data[1], [2.0, 10.0]);
        assert_eq!(series[3].data, [[0.0, 4.0], [2.0, 10.0]]);
    }

    #[test]
    fn ella_frame_sample_ratio_percentiles_filter_by_instance_side() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((1, 1), ELLA_SIDE_DOWNSTREAM);
        prof.frame_samples = vec![
            ella_frame(1_000_000, 1, 0, 1_200, 100, 100, 0),
            ella_frame(1_000_000, 2, 1, 1_200, 100, 400, 0),
        ];

        let internal = build_ella_frame_sample_ratio_percentiles_for_side(
            &prof,
            1,
            1_000_000,
            Some(ELLA_SIDE_UPSTREAM),
        );
        let external = build_ella_frame_sample_ratio_percentiles_for_side(
            &prof,
            1,
            1_000_000,
            Some(ELLA_SIDE_DOWNSTREAM),
        );
        assert_eq!(internal[0].data, [[0.0, 1.0]]);
        assert_eq!(internal[3].data, [[0.0, 1.0]]);
        assert_eq!(external[0].data, [[0.0, 4.0]]);
        assert_eq!(external[3].data, [[0.0, 4.0]]);
    }

    fn frame_sent(ts: u64, session: u64, inst: u32, planned: u32, actual: u32) -> EllaEvent {
        ella_evt(
            ts,
            session,
            EllaPayload::FrameSent {
                instance_id: inst,
                object_id: 42,
                frame_index: 0,
                first_packet: 1,
                packets: 4,
                is_probe: true,
                toggle: false,
                frame_len: 1_200,
                planned_send_us: planned,
                actual_send_us: actual,
                start_lateness_us: 0,
            },
        )
    }

    fn frame_relay(ts: u64, session: u64, inst: u32, wire: u32, recv: u32) -> EllaEvent {
        ella_evt(
            ts,
            session,
            EllaPayload::FrameRelay {
                instance_id: inst,
                first_packet: 1,
                packets: 4,
                toggle: false,
                boundary_index: 0,
                frame_len: 1_200,
                wire_send_us: wire,
                recv_us: recv,
                fwd_us: 0,
                residency_us: 0,
            },
        )
    }

    #[test]
    fn ella_channel_frame_ratio_prefers_events_over_frame_sample() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.instances.insert((1, 0), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            frame_sent(1_000_000, 1, 0, 100, 100),
            frame_sent(1_025_000, 1, 0, 100, 200),
            frame_sent(1_050_000, 1, 0, 100, 300),
            frame_sent(1_075_000, 1, 0, 100, 400),
            // Another channel's frames stay out of this channel's plot.
            frame_sent(1_050_000, 2, 0, 100, 9_900),
        ];
        // Samples that would draw a different ratio if they won.
        prof.frame_samples = vec![ella_frame(1_000_000, 1, 0, 1_200, 100, 900, 0)];

        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert!(!plot.frame_ratio_fallback);
        assert_eq!(
            plot.frame_ratio
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            ["TX Median", "TX P75", "TX P95", "TX Max"]
        );
        assert_eq!(plot.frame_ratio[0].data, [[1.0, 2.5]]);
        assert_eq!(plot.frame_ratio[3].data, [[1.0, 4.0]]);
    }

    #[test]
    fn ella_channel_frame_ratio_falls_back_to_frame_sample() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.instances.insert((1, 0), ELLA_SIDE_DOWNSTREAM);
        prof.frame_samples = vec![
            // One instance's two 100 ms samples form one 2 s ratio:
            // (100 + 300) / (100 + 100) = 2.
            ella_frame(1_000_000, 1, 0, 1_200, 100, 100, 0),
            ella_frame(1_100_000, 1, 0, 1_200, 100, 300, 0),
            // A second channel on the same node stays out.
            ella_frame(1_000_000, 2, 0, 1_200, 100, 9_900, 0),
        ];

        let plot = build_ella_channel_plot(&prof, 1, 1, 1_000_000);
        assert!(plot.frame_ratio_fallback);
        assert_eq!(
            plot.frame_ratio
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            ["TX Median", "TX P75", "TX P95", "TX Max"]
        );
        assert_eq!(plot.frame_ratio[0].data, [[0.0, 2.0]]);
        assert_eq!(plot.frame_ratio[3].data, [[0.0, 2.0]]);
    }

    #[test]
    fn ella_channel_frame_ratio_relay_splits_sides() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((1, 1), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            frame_relay(2_000_000, 1, 0, 100, 100),
            frame_relay(2_000_000, 1, 1, 100, 400),
        ];

        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert!(!plot.frame_ratio_fallback);
        assert_eq!(
            plot.frame_ratio
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            [
                "RX Median",
                "RX P75",
                "RX P95",
                "RX Max",
                "TX Median",
                "TX P75",
                "TX P95",
                "TX Max"
            ]
        );
        assert_eq!(plot.frame_ratio[0].data, [[2.0, 1.0]]);
        assert_eq!(plot.frame_ratio[4].data, [[2.0, 4.0]]);
    }

    #[test]
    fn ella_channel_frame_ratio_merged_tab_prefixes_node() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(9, ELLA_ROLE_ORIGIN);
        prof.roles.insert(10, ELLA_ROLE_RELAY);
        prof.instances.insert((9, 0), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((10, 0), ELLA_SIDE_UPSTREAM);
        prof.channels.insert((9, 1), mk_ch(9, 1, "/live/title", 1));
        prof.channels
            .insert((10, 1), mk_ch(10, 1, "/live/title", 4));
        prof.events = vec![
            ella_evt_on(
                9,
                1_000_000,
                1,
                EllaPayload::FrameSent {
                    instance_id: 0,
                    object_id: 42,
                    frame_index: 0,
                    first_packet: 1,
                    packets: 4,
                    is_probe: true,
                    toggle: false,
                    frame_len: 1_200,
                    planned_send_us: 100,
                    actual_send_us: 200,
                    start_lateness_us: 0,
                },
            ),
            ella_evt_on(
                10,
                1_000_000,
                1,
                EllaPayload::FrameRelay {
                    instance_id: 0,
                    first_packet: 1,
                    packets: 4,
                    toggle: false,
                    boundary_index: 0,
                    frame_len: 1_200,
                    wire_send_us: 100,
                    recv_us: 300,
                    fwd_us: 0,
                    residency_us: 0,
                },
            ),
        ];

        let plot = build_ella_channel_plot_for(&prof, &[(9, 1), (10, 1)], 0);
        assert_eq!(
            plot.frame_ratio
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            [
                "Server - TX Median",
                "Server - TX P75",
                "Server - TX P95",
                "Server - TX Max",
                "Relay - RX Median",
                "Relay - RX P75",
                "Relay - RX P95",
                "Relay - RX Max"
            ]
        );
    }

    #[test]
    fn ella_channel_object_rate_spreads_sample_bytes_across_gop() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        let mut samples = Vec::new();
        for (ts, bytes) in [
            (1_000_000, 0),
            (2_000_000, 250_000),
            (3_000_000, 0),
            (4_000_000, 250_000),
        ] {
            let mut s = ella_sample(ts, 1, 2, 4);
            s.object_bytes = bytes;
            if bytes > 0 {
                s.objects_completed = 1;
            }
            samples.push(s);
        }
        prof.channel_samples = samples;
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        assert_eq!(plot.object_rate.len(), 1);
        assert!((occupancy_at(&plot.object_rate[0].data, 1.5) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(&plot.object_rate[0].data, 3.0) - 1.0).abs() < 1e-9);
        prof.channels.insert((1, 1), mk_ch(1, 1, "alpha", 4));
        let (rx, tx) = build_ella_object_rate_stacks(&prof, 0);
        assert!(rx.series.is_empty());
        assert!((occupancy_at(&tx.series[0].data, 3.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ella_subscription_stack_skips_origin_and_sums_reincarnations() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::SessionCreate {
                    channel_name: "alpha".into(),
                    version: 2,
                    instance_id: 0,
                },
            ),
            ella_evt(
                1_100_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 0,
                    subscriber_address: "127.0.0.1".into(),
                    subscriber_port: 1,
                    subscribers: 1,
                },
            ),
            ella_evt(
                2_000_000,
                2,
                EllaPayload::SessionCreate {
                    channel_name: "/p/alpha".into(),
                    version: 2,
                    instance_id: 2,
                },
            ),
            ella_evt(
                2_100_000,
                2,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.2".into(),
                    subscriber_port: 4,
                    subscribers: 1,
                },
            ),
            ella_evt(
                3_000_000,
                3,
                EllaPayload::SessionCreate {
                    channel_name: "/q/alpha".into(),
                    version: 2,
                    instance_id: 2,
                },
            ),
            ella_evt(
                3_000_000,
                3,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBING,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.2".into(),
                    subscriber_port: 4,
                    subscribers: 1,
                },
            ),
        ];
        let stack = build_ella_subscription_stack(&prof, 0);
        assert_eq!(stack.series.len(), 1);
        assert_eq!(stack.series[0].label, "alpha");
        assert!((occupancy_at(&stack.series[0].data, 2.5) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&stack.series[0].data, 3.5) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ella_subscription_stack_clears_on_destroyed() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::SessionCreate {
                    channel_name: "alpha".into(),
                    version: 2,
                    instance_id: 2,
                },
            ),
            ella_evt(
                1_100_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 1,
                },
            ),
            ella_evt(
                2_000_000,
                1,
                EllaPayload::ChannelDestroyed {
                    reason: 1,
                    instance_id: 2,
                    subscribers: 1,
                },
            ),
        ];
        let stack = build_ella_subscription_stack(&prof, 0);
        assert_eq!(stack.series.len(), 1);
        assert!((occupancy_at(&stack.series[0].data, 1.5) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&stack.series[0].data, 2.5) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn ella_subscription_stack_unsubscribed_leave_decrements() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            ella_evt(
                1_000_000,
                1,
                EllaPayload::SessionCreate {
                    channel_name: "alpha".into(),
                    version: 2,
                    instance_id: 2,
                },
            ),
            ella_evt(
                1_100_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_SUBSCRIBED,
                    reason: 0,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 1,
                },
            ),
            ella_evt(
                2_000_000,
                1,
                EllaPayload::ChannelSubscriber {
                    state: ELLA_SUBSCRIBER_UNSUBSCRIBED,
                    reason: 1,
                    instance_id: 2,
                    subscriber_address: "10.0.0.1".into(),
                    subscriber_port: 9,
                    subscribers: 0,
                },
            ),
        ];
        let stack = build_ella_subscription_stack(&prof, 0);
        assert_eq!(stack.series.len(), 1);
        assert!((occupancy_at(&stack.series[0].data, 1.5) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(&stack.series[0].data, 2.5) - 0.0).abs() < 1e-9);
    }

    fn join_evt(ts: u64, inst: u32, sub_id: u32, replay: bool) -> EllaEvent {
        ella_evt(
            ts,
            1,
            EllaPayload::SubscriberJoin {
                instance_id: inst,
                sub_id,
                request_id: 0,
                subscriber_address: "10.0.0.1".into(),
                subscriber_port: 9,
                pending_ms: 0,
                replay,
                pbcid: String::new(),
            },
        )
    }

    fn leave_evt(ts: u64, inst: u32, sub_id: u32) -> EllaEvent {
        ella_evt(
            ts,
            1,
            EllaPayload::SubscriberLeave {
                instance_id: inst,
                sub_id,
                reason: 1,
                duration_ms: 0,
                packets: 0,
                bytes: 0,
                objects: 0,
            },
        )
    }

    /// Replay of a live key is a no-op; replay of an unseen key counts; a
    /// LEAVE for a key never joined is ignored, as UNSUBSCRIBED is.
    fn join_leave_events() -> Vec<EllaEvent> {
        vec![
            ella_evt(
                500_000,
                1,
                EllaPayload::SessionCreate {
                    channel_name: "alpha".into(),
                    version: 2,
                    instance_id: 2,
                },
            ),
            join_evt(1_000_000, 2, 1, false),
            join_evt(1_500_000, 2, 1, true),
            join_evt(2_000_000, 2, 2, true),
            leave_evt(3_000_000, 2, 1),
            leave_evt(3_500_000, 2, 9),
        ]
    }

    #[test]
    fn ella_channel_plot_occupancy_from_join_leave() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = join_leave_events();
        let plot = build_ella_channel_plot(&prof, 1, 1, 0);
        let d = &plot.subscribers[0].data;
        for (t, want) in [(1.2, 1.0), (1.7, 1.0), (2.5, 2.0), (3.2, 1.0), (3.7, 1.0)] {
            assert!((occupancy_at(d, t) - want).abs() < 1e-9, "t={t} {d:?}");
        }
    }

    #[test]
    fn ella_subscription_stack_from_join_leave() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = join_leave_events();
        prof.events.push(ella_evt(
            4_000_000,
            1,
            EllaPayload::ChannelDestroyed {
                reason: 1,
                instance_id: 2,
                subscribers: 1,
            },
        ));
        let stack = build_ella_subscription_stack(&prof, 0);
        assert_eq!(stack.series.len(), 1);
        assert_eq!(stack.series[0].label, "alpha");
        let d = &stack.series[0].data;
        for (t, want) in [
            (1.2, 1.0),
            (1.7, 1.0),
            (2.5, 2.0),
            (3.2, 1.0),
            (3.7, 1.0),
            (4.5, 0.0),
        ] {
            assert!((occupancy_at(d, t) - want).abs() < 1e-9, "t={t} {d:?}");
        }
    }

    #[test]
    fn occupancy_peak_avg_is_time_weighted() {
        let held = [[0.0, 4.0], [2.0, 4.0], [2.0, 0.0]];
        let (peak, avg) = occupancy_peak_avg(&held);
        assert!((peak - 4.0).abs() < 1e-9);
        assert!((avg.unwrap() - 4.0).abs() < 1e-9);
        let mixed = [[0.0, 10.0], [1.0, 10.0], [1.0, 0.0], [3.0, 0.0]];
        let (peak, avg) = occupancy_peak_avg(&mixed);
        assert!((peak - 10.0).abs() < 1e-9);
        assert!((avg.unwrap() - 10.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn occupancy_stack_bins_holds_last_value() {
        let series = vec![
            EllaNamedSeries {
                label: "alpha".into(),
                data: vec![[1.0, 1.0], [2.0, 1.0], [2.0, 0.0]],
            },
            EllaNamedSeries {
                label: "beta".into(),
                data: vec![[2.0, 2.0]],
            },
        ];
        let (centers, tops) = occupancy_stack_bins(&series, 0.0, 4.0, 4);
        assert_eq!(centers, vec![0.5, 1.5, 2.5, 3.5]);
        assert!((tops[0][0] - 0.0).abs() < 1e-9);
        assert!((tops[1][0] - 0.0).abs() < 1e-9);
        assert!((tops[0][1] - 1.0).abs() < 1e-9);
        assert!((tops[1][1] - 1.0).abs() < 1e-9);
        assert!((tops[0][2] - 0.0).abs() < 1e-9);
        assert!((tops[1][2] - 2.0).abs() < 1e-9);
        assert!((tops[0][3] - 0.0).abs() < 1e-9);
        assert!((tops[1][3] - 2.0).abs() < 1e-9);
    }

    fn mk_ch(provider_id: u16, session_id: u64, name: &str, peak: u32) -> EllaChannelInfo {
        EllaChannelInfo {
            provider_id,
            session_id,
            channel_name: name.into(),
            version: 2,
            first_ts: 0,
            last_ts: 1_000_000,
            event_count: 4,
            channel_ids: FxHashMap::default(),
            last_subscribers: peak,
            peak_subscribers: peak,
            occ_by_inst: FxHashMap::default(),
            last_state: Some(ELLA_LIFECYCLE_CREATED),
            destroy_reason: None,
        }
    }

    fn sub_evt(pid: u16, ts: u64, session: u64, inst: u32, state: u8, port: u16) -> EllaEvent {
        ella_evt_on(
            pid,
            ts,
            session,
            EllaPayload::ChannelSubscriber {
                state,
                reason: 0,
                instance_id: inst,
                subscriber_address: "10.0.0.1".into(),
                subscriber_port: port,
                subscribers: 1,
            },
        )
    }

    #[test]
    fn node_info_role_wins_over_the_attach_set() {
        let mut prof = EllaProfile::default();
        // An attach set that on its own would read as a Server.
        prof.instances.insert((9, 0), ELLA_SIDE_DOWNSTREAM);
        assert_eq!(prof.node_role(9), EllaNodeRole::Server);

        prof.roles.insert(9, ELLA_ROLE_RELAY);
        assert_eq!(prof.node_role(9), EllaNodeRole::Relay);

        prof.roles.insert(9, ELLA_ROLE_ORIGIN);
        assert_eq!(prof.node_role(9), EllaNodeRole::Server);
    }

    #[test]
    fn relay_survives_losing_its_upstream_attach() {
        // ella-10k-2026-09-03T211633Z: the relay's i0 UPSTREAM attach
        // landed in the capture's prejoin drop window, leaving only
        // DOWNSTREAM workers above i0.
        let mut prof = EllaProfile::default();
        prof.instances.insert((9, 0), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((10, 8), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((10, 39), ELLA_SIDE_DOWNSTREAM);

        assert_eq!(prof.node_role(9), EllaNodeRole::Server);
        assert_eq!(prof.node_role(10), EllaNodeRole::Relay);
    }

    #[test]
    fn dual_fanout_providers_split_server_vs_relay() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((9, 0), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((10, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((10, 2), ELLA_SIDE_DOWNSTREAM);

        assert_eq!(prof.node_role(9), EllaNodeRole::Server);
        assert_eq!(prof.node_role(10), EllaNodeRole::Relay);
        assert_eq!(prof.instance_label(9, 0), "server i0");
        assert_eq!(prof.instance_label(10, 0), "relay internal i0");
        assert_eq!(prof.instance_label(10, 2), "relay i2");

        let mut server_tot = ella_total(1_000_000, 0, 11, 0, 11, 0, 0, 0, 0);
        server_tot.provider_id = 9;
        let mut relay_tot = ella_total(1_000_000, 11, 414, 414, 10_000, 0, 0, 0, 0);
        relay_tot.provider_id = 10;
        prof.counts_total = vec![server_tot, relay_tot];

        let server = build_ella_relay_plot_for(&prof, 9, 0);
        let relay = build_ella_relay_plot_for(&prof, 10, 0);
        let s_ext = server
            .subscribers
            .iter()
            .find(|s| s.label == "External")
            .unwrap();
        let r_ext = relay
            .subscribers
            .iter()
            .find(|s| s.label == "External")
            .unwrap();
        assert!((series_peak(&s_ext.data) - 11.0).abs() < 1e-6);
        assert!((series_peak(&r_ext.data) - 10_000.0).abs() < 1e-6);
        assert!(server
            .subscribers
            .iter()
            .all(|s| series_peak(&s.data) < 100.0));

        let mut server_ch = mk_ch(9, 1, "/live/title", 11);
        server_ch.channel_ids.insert(0, 1);
        let mut relay_ch = mk_ch(10, 1, "/live/title", 10_000);
        relay_ch.channel_ids.insert(2, 1);
        prof.channels.insert((9, 1), server_ch);
        prof.channels.insert((10, 1), relay_ch);

        for i in 0..11u16 {
            prof.events.push(sub_evt(
                9,
                1_000_000,
                1,
                0,
                ELLA_SUBSCRIBER_SUBSCRIBED,
                1000 + i,
            ));
        }
        for i in 0..5u16 {
            prof.events.push(sub_evt(
                10,
                1_000_000,
                1,
                2,
                ELLA_SUBSCRIBER_SUBSCRIBED,
                2000 + i,
            ));
        }

        let s_stack = build_ella_subscription_stack_for(&prof, 0, Some(9));
        let r_stack = build_ella_subscription_stack_for(&prof, 0, Some(10));
        assert_eq!(s_stack.series.len(), 1);
        assert_eq!(r_stack.series.len(), 1);
        assert!((series_peak(&s_stack.series[0].data) - 11.0).abs() < 1e-6);
        assert!((series_peak(&r_stack.series[0].data) - 5.0).abs() < 1e-6);

        // One row for the title, naming both nodes, and one tab behind
        // it covering each node's session.
        let rows = build_ella_channel_list(&prof);
        let titled: Vec<_> = rows.iter().filter(|r| r.short_label == "title").collect();
        assert_eq!(titled.len(), 1);
        assert_eq!(titled[0].node, "Server + Relay");
        assert_eq!(titled[0].instance_count, 2);

        let open = ella_channel_for_stack_label(&prof, "title").unwrap();
        assert_eq!(
            ella_channel_tab_members(&prof, open.0, open.1),
            vec![(9, 1), (10, 1)]
        );
    }

    #[test]
    fn merged_channel_tab_labels_each_node_and_keeps_one_node_bare() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((9, 0), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((10, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((10, 2), ELLA_SIDE_DOWNSTREAM);
        prof.channels.insert((9, 1), mk_ch(9, 1, "/live/title", 1));
        prof.channels
            .insert((10, 1), mk_ch(10, 1, "/live/title", 4));
        prof.events
            .push(sub_evt(9, 1_000_000, 1, 0, ELLA_SUBSCRIBER_SUBSCRIBED, 1));
        prof.events
            .push(sub_evt(10, 1_000_000, 1, 2, ELLA_SUBSCRIBER_SUBSCRIBED, 2));

        let members = ella_channel_tab_members(&prof, 9, 1);
        let merged = build_ella_channel_plot_for(&prof, &members, 0);
        let labels: Vec<&str> = merged
            .subscribers
            .iter()
            .map(|s| s.label.as_str())
            .collect();
        assert_eq!(labels, vec!["Server - External", "Relay - External"]);

        // A title on one node only keeps its labels unqualified.
        let single = build_ella_channel_plot_for(&prof, &[(9, 1)], 0);
        assert_eq!(single.subscribers[0].label, "External");
    }

    #[test]
    fn channel_tab_draws_every_reincarnation_on_one_series() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_DOWNSTREAM);
        let mut early = mk_ch(1, 1, "/live/title", 1);
        early.first_ts = 1_000_000;
        early.last_ts = 2_000_000;
        early.event_count = 1;
        let mut late = mk_ch(1, 2, "/live/title", 1);
        late.first_ts = 100_000_000;
        late.last_ts = 101_000_000;
        late.event_count = 10;
        prof.channels.insert((1, 1), early);
        prof.channels.insert((1, 2), late);
        prof.events
            .push(sub_evt(1, 1_000_000, 1, 0, ELLA_SUBSCRIBER_SUBSCRIBED, 1));
        prof.events
            .push(sub_evt(1, 100_000_000, 2, 0, ELLA_SUBSCRIBER_SUBSCRIBED, 2));

        // Click target is still the longest life; the tab draws both.
        let open = ella_channel_for_stack_label(&prof, "title").unwrap();
        assert_eq!(open, (1, 2));
        assert_eq!(
            ella_channel_tab_members(&prof, open.0, open.1),
            vec![(1, 1), (1, 2)]
        );
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1), (1, 2)], 0);
        assert_eq!(plot.subscribers.len(), 1);
        assert_eq!(plot.subscribers[0].label, "External");
        let xs: Vec<f64> = plot.subscribers[0].data.iter().map(|p| p[0]).collect();
        assert!(xs.iter().any(|t| (*t - 1.0).abs() < 1e-9), "{xs:?}");
        assert!(xs.iter().any(|t| (*t - 100.0).abs() < 1e-9), "{xs:?}");
        // Idle gap between lives is a drop to 0, not a diagonal.
        let gap = plot.subscribers[0]
            .data
            .windows(2)
            .find(|w| w[0][0] < 50.0 && w[1][0] > 50.0);
        assert!(gap.is_some_and(|w| w[0][1] == 0.0 && w[1][1] == 0.0));
    }

    #[test]
    fn object_id_skip_is_time_order_and_ignores_id_restart() {
        let span = |t0: f64, id: u32, pn: u32| EllaObjectSpan {
            provider_id: 1,
            instance_id: 0,
            t0,
            t1: t0 + 2.0,
            obj_idx: pn,
            first_pn: pn,
            boundary_index: 0,
            object_id: Some(id),
            packets: 1,
            bytes: 1,
            n: 1,
            state: EllaObjectSpanState::Completed,
        };
        // Ranked by first_pn (1,2,100,101) would see 1→100 as a skip.
        let spans = vec![
            span(100.0, 1, 1),
            span(102.0, 2, 2),
            span(1.0, 1, 100),
            span(3.0, 3, 101),
        ];
        assert_eq!(
            object_span_id_skipped(&spans),
            vec![false, false, false, true]
        );
        // A later life that jumps ids after a quiet gap is not skip-red.
        let resume = vec![span(1.0, 10, 1), span(100.0, 50, 2)];
        assert_eq!(object_span_id_skipped(&resume), vec![false, false]);
    }

    #[test]
    fn object_span_stripe_flips_each_gop_and_joins_nodes() {
        // The object slot (boundary_index) drives the stripe, rotating per
        // GOP and shared by both nodes' copies of a GOP.
        let span = |pid: u16, t0: f64, pn: u32, idx: u32, slot: u8| EllaObjectSpan {
            provider_id: pid,
            instance_id: 0,
            t0,
            t1: t0 + 2.0,
            obj_idx: idx,
            first_pn: pn,
            boundary_index: slot,
            object_id: Some(pn),
            packets: 1,
            bytes: 1,
            n: 1,
            state: EllaObjectSpanState::Completed,
        };
        let spans = vec![
            span(9, 2.0, 20, 1, 3),
            span(9, 0.0, 10, 0, 2),
            span(10, 0.05, 10, 0, 2),
            span(10, 2.05, 20, 1, 3),
            span(9, 4.0, 30, 2, 0),
        ];
        assert_eq!(object_span_stripe(&spans), vec![3, 2, 2, 3, 0]);
    }

    #[test]
    fn merge_object_spans_keeps_ids_on_separate_lives() {
        let span = |t0: f64, id: u32| EllaObjectSpan {
            provider_id: 1,
            instance_id: 0,
            t0,
            t1: t0 + 2.0,
            obj_idx: 0,
            first_pn: 50,
            boundary_index: 0,
            object_id: Some(id),
            packets: 1,
            bytes: 1,
            n: 1,
            state: EllaObjectSpanState::Completed,
        };
        let mut spans = vec![span(1.0, 1), span(101.0, 9)];
        merge_object_spans(&mut spans);
        assert_eq!(spans[0].object_id, Some(1));
        assert_eq!(spans[1].object_id, Some(9));
        assert_eq!(spans[0].obj_idx, 0);
        assert_eq!(spans[1].obj_idx, 1);
    }

    #[test]
    fn object_duration_drops_to_zero_between_lives() {
        let span = |t0: f64, t1: f64, idx: u32| EllaObjectSpan {
            provider_id: 1,
            instance_id: 0,
            t0,
            t1,
            obj_idx: idx,
            first_pn: idx,
            boundary_index: 0,
            object_id: Some(idx),
            packets: 1,
            bytes: 1,
            n: 1,
            state: EllaObjectSpanState::Completed,
        };
        let spans = vec![span(1.0, 3.0, 0), span(20.0, 22.0, 1)];
        let (_, durs, _) = object_span_plots(&spans, |_| "n".into());
        let d = &durs[0].data;
        assert!((occupancy_at(d, 2.0) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(d, 10.0) - 0.0).abs() < 1e-9);
        assert!((occupancy_at(d, 21.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn object_duration_and_size_envelope_overlapping_spans() {
        let span = |t0: f64, t1: f64, idx: u32, bytes: u32| EllaObjectSpan {
            provider_id: 1,
            instance_id: 0,
            t0,
            t1,
            obj_idx: idx,
            first_pn: idx,
            boundary_index: 0,
            object_id: Some(idx),
            packets: 1,
            bytes,
            n: 1,
            state: EllaObjectSpanState::Completed,
        };
        let spans = vec![span(1.0, 3.0, 0, 100), span(2.0, 6.0, 1, 50)];
        let (ids, durs, sizes) = object_span_plots(&spans, |_| "n".into());
        let d = &durs[0].data;
        let sz = &sizes[0].data;
        assert!(
            d.windows(2).all(|w| w[0][0] <= w[1][0]),
            "duration x must be non-decreasing: {d:?}"
        );
        assert!(
            sz.windows(2).all(|w| w[0][0] <= w[1][0]),
            "size x must be non-decreasing: {sz:?}"
        );
        assert!((occupancy_at(d, 1.5) - 1.0).abs() < 1e-9);
        assert!((occupancy_at(d, 2.5) - 4.0).abs() < 1e-9);
        assert!((occupancy_at(d, 4.0) - 4.0).abs() < 1e-9);
        assert!((occupancy_at(sz, 1.5) - 100.0 / 1_000_000.0).abs() < 1e-12);
        assert!((occupancy_at(sz, 2.5) - 50.0 / 1_000_000.0).abs() < 1e-12);
        assert!((occupancy_at(sz, 4.0) - 50.0 / 1_000_000.0).abs() < 1e-12);
        assert!((occupancy_at(&ids[0].data, 1.5) - 0.0).abs() < 1e-9);
        assert!((occupancy_at(&ids[0].data, 2.5) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn object_duration_follows_latest_started_span() {
        let span = |t0: f64, t1: f64, idx: u32| EllaObjectSpan {
            provider_id: 1,
            instance_id: 0,
            t0,
            t1,
            obj_idx: idx,
            first_pn: idx,
            boundary_index: 0,
            object_id: Some(idx),
            packets: 1,
            bytes: 1,
            n: 1,
            state: EllaObjectSpanState::Completed,
        };
        let spans = vec![span(1.0, 13.0, 0), span(11.0, 13.0, 1)];
        let (_, durs, _) = object_span_plots(&spans, |_| "n".into());
        let d = &durs[0].data;
        assert!((occupancy_at(d, 2.0) - 10.0).abs() < 1e-9);
        assert!((occupancy_at(d, 12.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn object_duration_prefers_completed_instance_of_gop() {
        let span = |t0: f64, t1: f64, state: EllaObjectSpanState| EllaObjectSpan {
            provider_id: 1,
            instance_id: 0,
            t0,
            t1,
            obj_idx: 0,
            first_pn: 0,
            boundary_index: 0,
            object_id: Some(0),
            packets: 1,
            bytes: 1,
            n: 1,
            state,
        };
        let hung = span(1.0, 15.0, EllaObjectSpanState::Open);
        let done = span(1.0, 3.0, EllaObjectSpanState::Completed);
        let next = EllaObjectSpan {
            provider_id: 1,
            instance_id: 0,
            t0: 8.0,
            t1: 10.0,
            obj_idx: 1,
            first_pn: 1,
            boundary_index: 0,
            object_id: Some(1),
            packets: 1,
            bytes: 1,
            n: 1,
            state: EllaObjectSpanState::Completed,
        };
        let spans = vec![hung, done, next];
        let (_, durs, _) = object_span_plots(&spans, |_| "n".into());
        let d = &durs[0].data;
        assert!((occupancy_at(d, 2.0) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(d, 9.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn object_duration_uses_busiest_instance_not_hung_copy() {
        let span =
            |inst: u32, t0: f64, t1: f64, idx: u32, state: EllaObjectSpanState| EllaObjectSpan {
                provider_id: 1,
                instance_id: inst,
                t0,
                t1,
                obj_idx: idx,
                first_pn: idx,
                boundary_index: 0,
                object_id: Some(idx),
                packets: 1,
                bytes: 1,
                n: 1,
                state,
            };
        let mut spans = vec![
            span(1, 1.0, 3.0, 0, EllaObjectSpanState::Completed),
            span(1, 3.2, 5.2, 1, EllaObjectSpanState::Completed),
            span(9, 1.0, 16.0, 0, EllaObjectSpanState::Open),
        ];
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 1), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((1, 9), ELLA_SIDE_DOWNSTREAM);
        spans = object_spans_at_percentile(&prof, &spans, EllaObjectPercentile::Median);
        assert!(spans.iter().all(|s| s.instance_id == 1));
        let (_, durs, _) = object_span_plots(&spans, |_| "n".into());
        let d = &durs[0].data;
        assert!((occupancy_at(d, 2.0) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(d, 4.0) - 2.0).abs() < 1e-9);
        assert!(
            occupancy_at(d, 4.0) < 5.0,
            "hung 15s copy must not set duration: {d:?}"
        );
    }

    #[test]
    fn ella_channel_fills_gop_skipped_on_one_instance() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        prof.instances.insert((1, 0), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            obj_evt(1, 1_000_000, 1, 0, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 3_000_000, 1, 0, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            // Instance 0 never STARTED pn 200, then completed pn 300.
            obj_evt(1, 5_000_000, 1, 0, ELLA_OBJECT_STARTED, 300, (0, 0)),
            obj_evt(1, 7_000_000, 1, 0, ELLA_OBJECT_COMPLETED, 300, (10, 1000)),
            obj_evt(1, 3_000_000, 1, 2, ELLA_OBJECT_STARTED, 200, (0, 0)),
            obj_evt(1, 5_000_000, 1, 2, ELLA_OBJECT_COMPLETED, 200, (10, 1000)),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        assert_eq!(plot.object_spans.len(), 2);
        assert!(plot.object_spans.iter().all(|s| s.instance_id == 0));
        let dur = &plot.object_duration[0].data;
        assert!((occupancy_at(dur, 2.0) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(dur, 4.0) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(dur, 6.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn ella_channel_rejects_two_gops_glued_on_one_instance() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        prof.instances.insert((1, 0), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            obj_evt(1, 1_000_000, 1, 0, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 5_000_000, 1, 0, ELLA_OBJECT_COMPLETED, 200, (10, 1000)),
            obj_evt(1, 1_000_000, 1, 2, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 3_000_000, 1, 2, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            obj_evt(1, 3_000_000, 1, 2, ELLA_OBJECT_STARTED, 200, (0, 0)),
            obj_evt(1, 5_000_000, 1, 2, ELLA_OBJECT_COMPLETED, 200, (10, 1000)),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        assert!(plot.object_spans.iter().all(|s| s.t1 - s.t0 <= 2.1));
        let dur = &plot.object_duration[0].data;
        assert!((occupancy_at(dur, 2.0) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(dur, 4.0) - 2.0).abs() < 1e-9);
        assert!(
            occupancy_at(dur, 2.0) < 3.5,
            "glued 4s copy must not set duration: {dur:?}"
        );
    }

    #[test]
    fn ella_channel_clips_long_gop_under_the_next_bar() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        prof.instances.insert((1, 0), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            obj_evt(1, 1_000_000, 1, 0, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 5_000_000, 1, 0, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            obj_evt(1, 3_000_000, 1, 2, ELLA_OBJECT_STARTED, 200, (0, 0)),
            obj_evt(1, 5_000_000, 1, 2, ELLA_OBJECT_COMPLETED, 200, (10, 500)),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        assert!(plot.object_spans.iter().all(|s| s.instance_id == 2));
        assert!(plot.object_spans.iter().all(|s| s.t1 - s.t0 <= 2.1));
        let labels: Vec<&str> = plot
            .object_duration
            .iter()
            .map(|s| s.label.as_str())
            .collect();
        assert_eq!(labels, ["Median", "P75", "P95", "Max"]);
        let dur = &plot.object_duration[0].data;
        let max = &plot.object_duration[3].data;
        assert!((occupancy_at(dur, 2.0) - 4.0).abs() < 1e-9);
        assert!((occupancy_at(dur, 4.0) - 2.0).abs() < 1e-9);
        assert!((occupancy_at(max, 2.0) - 4.0).abs() < 1e-9);
        let rate = &plot.object_rate[0].data;
        let r4 = occupancy_at(rate, 4.0);
        assert!((r4 - 0.002).abs() < 1e-9, "rate at 4s: {r4} from {rate:?}");
        let max_spans =
            object_spans_at_percentile(&prof, &plot.object_span_copies, EllaObjectPercentile::Max);
        assert!(max_spans.iter().all(|s| s.instance_id == 0));
        assert!(max_spans.iter().any(|s| (s.t1 - s.t0 - 4.0).abs() < 1e-9));
    }

    #[test]
    fn object_percentile_keeps_one_instance_across_gops() {
        let span = |inst: u32, t0: f64, t1: f64, idx: u32| EllaObjectSpan {
            provider_id: 1,
            instance_id: inst,
            t0,
            t1,
            obj_idx: idx,
            first_pn: idx,
            boundary_index: 0,
            object_id: Some(idx),
            packets: 1,
            bytes: 1,
            n: 1,
            state: EllaObjectSpanState::Completed,
        };
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 1), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        let copies = vec![
            span(1, 0.0, 2.0, 0),
            span(1, 2.0, 4.0, 1),
            span(2, 0.0, 8.0, 0),
            span(2, 8.0, 10.0, 1),
        ];
        let med = object_spans_at_percentile(&prof, &copies, EllaObjectPercentile::Median);
        assert!(med.iter().all(|s| s.instance_id == 1));
        assert_eq!(med.len(), 2);
        let max = object_spans_at_percentile(&prof, &copies, EllaObjectPercentile::Max);
        assert!(max.iter().all(|s| s.instance_id == 2));
        assert!(max.iter().any(|s| (s.t1 - s.t0 - 8.0).abs() < 1e-9));
    }

    #[test]
    fn object_percentile_max_keeps_internal_instance() {
        let span = |inst: u32, t0: f64, t1: f64, idx: u32| EllaObjectSpan {
            provider_id: 1,
            instance_id: inst,
            t0,
            t1,
            obj_idx: idx,
            first_pn: idx,
            boundary_index: 0,
            object_id: Some(idx),
            packets: 1,
            bytes: 1,
            n: 1,
            state: EllaObjectSpanState::Completed,
        };
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        prof.instances.insert((1, 0), ELLA_SIDE_UPSTREAM);
        prof.instances.insert((1, 1), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        let copies = vec![
            span(0, 0.0, 2.0, 0),
            span(1, 0.0, 2.0, 0),
            span(2, 0.0, 8.0, 0),
        ];
        let max = object_spans_at_percentile(&prof, &copies, EllaObjectPercentile::Max);
        assert!(max.iter().any(|s| s.instance_id == 0));
        assert!(max.iter().any(|s| s.instance_id == 2));
        assert!(max.iter().all(|s| s.instance_id != 1));
        let (durs, _) = object_copy_percentile_series(&prof, &copies);
        let labels: Vec<&str> = durs.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["Internal", "Median", "P75", "P95", "Max"]);
        let rate = object_rate_at_percentile(&prof, &copies, EllaObjectPercentile::Max, false);
        let rate_labels: Vec<&str> = rate.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(rate_labels, ["Internal", "External"]);
    }

    #[test]
    fn object_span_sublanes_stack_overlaps_and_reuse_gaps() {
        let span = |t0: f64, t1: f64, idx: u32| EllaObjectSpan {
            provider_id: 1,
            instance_id: 0,
            t0,
            t1,
            obj_idx: idx,
            first_pn: idx,
            boundary_index: 0,
            object_id: Some(idx),
            packets: 1,
            bytes: 1,
            n: 1,
            state: EllaObjectSpanState::Completed,
        };
        let spans = vec![span(0.0, 4.0, 0), span(2.0, 4.0, 1), span(4.0, 6.0, 2)];
        let sub = object_span_sublanes(&spans);
        assert_eq!(sub, vec![0, 1, 0]);
    }

    fn frame_sent_evt(
        inst: u32,
        ts: u64,
        object_id: u32,
        frame_index: u32,
        actual_us: u32,
    ) -> EllaEvent {
        ella_evt_on(
            1,
            ts,
            1,
            EllaPayload::FrameSent {
                instance_id: inst,
                object_id,
                frame_index,
                first_packet: frame_index,
                packets: 1,
                is_probe: false,
                toggle: false,
                frame_len: 1000,
                planned_send_us: actual_us,
                actual_send_us: actual_us,
                start_lateness_us: 0,
            },
        )
    }

    #[test]
    fn frame_timeline_lays_origin_frames_end_to_end_from_burst() {
        let mut prof = EllaProfile::default();
        prof.instances.insert((1, 0), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            frame_sent_evt(0, 2_000_000, 9, 0, 10_000),
            frame_sent_evt(0, 2_000_000, 9, 1, 20_000),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        assert_eq!(plot.frame_spans.len(), 2);
        let mut rows = plot.frame_spans.clone();
        rows.sort_by_key(|a| a.frame_index);
        assert!((rows[1].t1 - 2.0).abs() < 1e-9);
        assert!((rows[1].t1 - rows[1].t0 - 0.02).abs() < 1e-9);
        assert!((rows[0].t1 - rows[1].t0).abs() < 1e-9);
        assert!((rows[0].t1 - rows[0].t0 - 0.01).abs() < 1e-9);
    }

    #[test]
    fn frame_timeline_relay_uses_recv_window() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_RELAY);
        prof.instances.insert((1, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![ella_evt_on(
            1,
            2_000_000,
            1,
            EllaPayload::FrameRelay {
                instance_id: 2,
                first_packet: 50,
                packets: 4,
                toggle: false,
                boundary_index: 0,
                frame_len: 800,
                wire_send_us: 10_000,
                recv_us: 15_000,
                fwd_us: 12_000,
                residency_us: 0,
            },
        )];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        assert_eq!(plot.frame_spans.len(), 1);
        let s = &plot.frame_spans[0];
        assert!((s.t1 - 2.0).abs() < 1e-9);
        assert!((s.t1 - s.t0 - 0.015).abs() < 1e-9);
        assert_eq!(s.object_id, 50);
    }

    #[test]
    fn frame_timeline_origin_starts_at_object_started() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.instances.insert((1, 0), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            obj_evt(1, 1_000_000, 1, 0, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 3_000_000, 1, 0, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            frame_sent_evt(0, 3_000_000, 42, 0, 10_000),
            frame_sent_evt(0, 3_000_000, 42, 1, 20_000),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        let mut rows = plot.frame_spans.clone();
        rows.sort_by_key(|a| a.frame_index);
        assert!((rows[0].t0 - 1.0).abs() < 1e-9);
        assert_eq!(rows[0].object_id, 42);
        assert_eq!(rows[0].obj_idx, plot.object_spans[0].obj_idx);
        assert_eq!(
            frame_span_stripe(&rows)[0],
            plot.object_spans[0].boundary_index as u32
        );
    }

    #[test]
    fn frame_timeline_origin_uses_event_time_when_timestamps_spread() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(1, ELLA_ROLE_ORIGIN);
        prof.instances.insert((1, 0), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            obj_evt(1, 1_000_000, 1, 0, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(1, 3_000_000, 1, 0, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            frame_sent_evt(0, 1_200_000, 42, 0, 10_000),
            frame_sent_evt(0, 1_250_000, 42, 1, 20_000),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(1, 1)], 0);
        let mut rows = plot.frame_spans.clone();
        rows.sort_by_key(|a| a.frame_index);
        assert_eq!(rows.len(), 2);
        assert!((rows[0].t1 - 1.2).abs() < 1e-9);
        assert!((rows[0].t1 - rows[0].t0 - 0.01).abs() < 1e-9);
        assert!((rows[1].t1 - 1.25).abs() < 1e-9);
        assert!((rows[1].t1 - rows[1].t0 - 0.02).abs() < 1e-9);
        assert!((rows[0].t0 - 1.0).abs() > 0.1);
    }

    #[test]
    fn frame_timeline_relay_takes_gop_object_id() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(9, ELLA_ROLE_ORIGIN);
        prof.roles.insert(10, ELLA_ROLE_RELAY);
        prof.instances.insert((9, 0), ELLA_SIDE_DOWNSTREAM);
        prof.instances.insert((10, 2), ELLA_SIDE_DOWNSTREAM);
        prof.events = vec![
            obj_evt(9, 1_000_000, 1, 0, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(9, 3_000_000, 1, 0, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            ella_evt_on(
                9,
                3_000_000,
                1,
                EllaPayload::FrameSent {
                    instance_id: 0,
                    object_id: 42,
                    frame_index: 0,
                    first_packet: 100,
                    packets: 10,
                    is_probe: false,
                    toggle: false,
                    frame_len: 1000,
                    planned_send_us: 10_000,
                    actual_send_us: 10_000,
                    start_lateness_us: 0,
                },
            ),
            obj_evt(10, 1_010_000, 1, 2, ELLA_OBJECT_STARTED, 100, (0, 0)),
            obj_evt(10, 3_010_000, 1, 2, ELLA_OBJECT_COMPLETED, 100, (10, 1000)),
            ella_evt_on(
                10,
                2_000_000,
                1,
                EllaPayload::FrameRelay {
                    instance_id: 2,
                    first_packet: 105,
                    packets: 4,
                    toggle: false,
                    boundary_index: 0,
                    frame_len: 800,
                    wire_send_us: 10_000,
                    recv_us: 15_000,
                    fwd_us: 12_000,
                    residency_us: 0,
                },
            ),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(9, 1), (10, 1)], 0);
        let relay = plot
            .frame_spans
            .iter()
            .find(|s| s.provider_id == 10)
            .unwrap();
        assert_eq!(relay.object_id, 42);
        let server_obj = plot
            .object_spans
            .iter()
            .find(|s| s.provider_id == 9)
            .unwrap();
        assert_eq!(relay.obj_idx, server_obj.obj_idx);
    }

    #[test]
    fn frame_span_shade_alternates_within_a_gop() {
        // The FDACE toggle bit drives the shade, so a dropped frame does not
        // shift the parity of the survivors.
        let f = |t0: f64, idx: u32| EllaFrameSpan {
            provider_id: 1,
            instance_id: 0,
            t0,
            t1: t0 + 0.02,
            object_id: 9,
            obj_idx: 0,
            frame_index: idx,
            first_pn: idx,
            boundary_index: 0,
            toggle: idx % 2 == 1,
            bytes: 100,
        };
        let spans = vec![f(0.0, 0), f(0.02, 1), f(0.04, 2)];
        assert_eq!(frame_span_shade(&spans), vec![false, true, false]);
        // A dropped middle frame leaves the survivors' parity untouched.
        let dropped = vec![f(0.0, 0), f(0.04, 2)];
        assert_eq!(frame_span_shade(&dropped), vec![false, false]);
    }

    #[test]
    fn frame_timeline_relay_stripe_follows_slot_under_drops() {
        let mut prof = EllaProfile::default();
        prof.roles.insert(10, ELLA_ROLE_RELAY);
        prof.instances.insert((10, 2), ELLA_SIDE_DOWNSTREAM);
        let obj = |ts: u64, state: u8, slot: u8, pn: u32| {
            ella_evt_on(
                10,
                ts,
                1,
                EllaPayload::ObjectState {
                    state,
                    instance_id: 2,
                    boundary_index: slot,
                    first_packet: pn,
                    packets: 0,
                    bytes: 0,
                },
            )
        };
        let frame = |ts: u64, slot: u8, pn: u32, toggle: bool| {
            ella_evt_on(
                10,
                ts,
                1,
                EllaPayload::FrameRelay {
                    instance_id: 2,
                    first_packet: pn,
                    packets: 1,
                    toggle,
                    boundary_index: slot,
                    frame_len: 100,
                    wire_send_us: 1000,
                    recv_us: 1000,
                    fwd_us: 1000,
                    residency_us: 0,
                },
            )
        };
        prof.events = vec![
            obj(1_000_000, ELLA_OBJECT_STARTED, 1, 100),
            frame(1_100_000, 1, 100, false),
            frame(1_200_000, 1, 110, true),
            obj(2_000_000, ELLA_OBJECT_COMPLETED, 1, 100),
            obj(2_000_000, ELLA_OBJECT_STARTED, 2, 200),
            // The second object's first frame event is dropped; only a later
            // frame survives.
            frame(2_500_000, 2, 230, true),
            obj(3_000_000, ELLA_OBJECT_COMPLETED, 2, 200),
        ];
        let plot = build_ella_channel_plot_for(&prof, &[(10, 1)], 0);
        // Object Timeline stripes by slot: 1 then 2.
        assert_eq!(object_span_stripe(&plot.object_spans), vec![1, 2]);
        // The frame lane follows each frame's own slot, so it matches the
        // object lane and does not flip when the second object's first frame
        // event is dropped.
        assert_eq!(frame_span_stripe(&plot.frame_spans), vec![1, 1, 2]);
    }
}
