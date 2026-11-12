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

use super::*;
use crate::elog_parser::*;

// ---------------------------------------------------------------------------
// Rate computation
// ---------------------------------------------------------------------------

/// Sliding window rate over (timestamp_us, byte_count) pairs.
pub fn sliding_window_rate(
    pairs: &[(u64, u32)],
    window_us: u64,
    step_us: u64,
    t0: u64,
) -> Vec<(f64, f64)> {
    if pairs.is_empty() {
        return Vec::new();
    }

    let mut sorted: Vec<(u64, u32)> = pairs.to_vec();
    sorted.sort_by_key(|x| x.0);

    let n = sorted.len();
    let half_w = window_us / 2;
    let t_start = sorted[0].0;
    let t_end = sorted[n - 1].0;

    let mut points = Vec::new();
    let mut left = 0usize;
    let mut right = 0usize;
    let mut window_bytes: u64 = 0;
    let mut t = t_start;

    while t <= t_end + half_w {
        let w_start = t.saturating_sub(half_w);
        let w_end = t + half_w;

        while right < n && sorted[right].0 < w_end {
            window_bytes += sorted[right].1 as u64;
            right += 1;
        }
        while left < n && sorted[left].0 < w_start {
            window_bytes -= sorted[left].1 as u64;
            left += 1;
        }

        let rate = window_bytes as f64 * USEC_IN_SEC / window_us as f64;
        points.push(((t - t0) as f64 / USEC_IN_SEC, rate));
        t += step_us;
    }

    points
}

/// Compute send rate from OUT events using a sliding window.
pub fn compute_send_rate(
    events: &[Event],
    window_us: u64,
    step_us: u64,
    t0: u64,
) -> Vec<(f64, f64)> {
    let out_pairs: Vec<(u64, u32)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::Out { len, .. } => Some((e.ts, *len)),
            _ => None,
        })
        .collect();
    sliding_window_rate(&out_pairs, window_us, step_us, t0)
}

/// Compute ACK rate from ACK events using a sliding window.
pub fn compute_ack_rate(
    events: &[Event],
    window_us: u64,
    step_us: u64,
    t0: u64,
) -> Vec<(f64, f64)> {
    let ack_pairs: Vec<(u64, u32)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::Ack { acked, .. } if *acked > 0 => Some((e.ts, *acked)),
            _ => None,
        })
        .collect();
    sliding_window_rate(&ack_pairs, window_us, step_us, t0)
}

/// Rolling ACK delivery rate excluding idle gaps.
pub fn compute_ack_delivery_rate(
    events: &[Event],
    window_us: u64,
    idle_thresh_us: u64,
    step_us: u64,
    t0: u64,
    min_active_us: u64,
) -> Vec<(f64, f64)> {
    let mut ack_pairs: Vec<(u64, u32)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::Ack { acked, .. } if *acked > 0 => Some((e.ts, *acked)),
            _ => None,
        })
        .collect();
    ack_pairs.sort_by_key(|x| x.0);

    let n = ack_pairs.len();
    if n < 2 {
        return Vec::new();
    }

    let mut points = Vec::new();
    let mut left = 0usize;
    let mut last_emit_us = ack_pairs[0].0.saturating_sub(step_us + 1);

    for right in 1..n {
        let ts = ack_pairs[right].0;
        let w_start = ts.saturating_sub(window_us);
        while left < right && ack_pairs[left].0 < w_start {
            left += 1;
        }
        if ts.saturating_sub(last_emit_us) < step_us {
            continue;
        }

        let mut total_bytes: u64 = 0;
        let mut active_time: u64 = 0;
        for k in left..=right {
            total_bytes += ack_pairs[k].1 as u64;
            if k > left {
                let gap = ack_pairs[k].0 - ack_pairs[k - 1].0;
                if gap < idle_thresh_us {
                    active_time += gap;
                }
            }
        }

        if active_time >= min_active_us {
            let rate = total_bytes as f64 * USEC_IN_SEC / active_time as f64;
            points.push(((ts - t0) as f64 / USEC_IN_SEC, rate));
            last_emit_us = ts;
        }
    }

    points
}

// ---------------------------------------------------------------------------
// Plot data extraction (direct-from-event series only)
// ---------------------------------------------------------------------------

pub struct OverviewPlotData {
    pub rtt_samples: Vec<[f64; 2]>,
    pub srtt: Vec<[f64; 2]>,
    pub path_rtt: Vec<[f64; 2]>,
    pub min_rtt: Vec<[f64; 2]>,
    pub gp_filter_min: Vec<[f64; 2]>,
    pub gp_srtt: Vec<[f64; 2]>,

    pub kernel_goodput_mbps: Vec<[f64; 2]>,
    pub pacing_rate_mbps: Vec<[f64; 2]>,
    pub ack_rate_mbps: Vec<[f64; 2]>,

    pub cwnd_kb: Vec<[f64; 2]>,
    pub ssthresh_kb: Vec<[f64; 2]>,
    pub inflight_kb: Vec<[f64; 2]>,
    /// Estimated bytes resident in the send socket buffer (KB),
    /// piecewise-constant between events that change the estimate.
    /// Computed by tracking `EVT_USER_SEND.sndbuf_used` (authoritative
    /// reading), adding `EVT_SENDFILE.length` (sendfile fires
    /// post-iodone), and subtracting `EVT_ACK.acked` (ACKs free
    /// acked bytes from the buffer). Includes both sent-but-unacked
    /// bytes (≈ `inflight`) and unsent bytes the kernel hasn't placed
    /// on the wire — so `sndbuf_kb − inflight_kb` is the "unsent
    /// backlog" the kernel still has queued from the app. Empty on
    /// sessions that emit no USER_SEND or SENDFILE events.
    pub sndbuf_kb: Vec<[f64; 2]>,
    /// Peer's advertised receive window in KB, piecewise-constant
    /// between EVT_IN events. The kernel logs
    /// `tiwin = th_win << snd_scale` directly in `EVT_IN.win`, so
    /// these are already byte-valued and need no further shifting.
    /// Empty on captures that lack a `CONN_PARAMS_V2` event —
    /// presence of that event is our gate for "this kernel logs
    /// peer-rwnd in a form we trust".
    pub peer_rwnd_kb: Vec<[f64; 2]>,
    /// `true` iff the session has no `CONN_PARAMS_V2` event and the
    /// peer-rwnd-derived signals (`peer_rwnd_kb`, `Limiter::Rwnd`)
    /// were therefore suppressed. Lets CLI / GUI explain why
    /// `%rwnd` is `n/a` for legacy captures without re-deriving the
    /// "do we have conn params?" check.
    pub peer_rwnd_unscaled: bool,
    /// Stable per-connection parameters from the session's first
    /// `CONN_PARAMS_V2`, when present. Lets the GUI / CLI surface
    /// negotiated wscales, options, and the initial cc-algo.
    pub conn_params: Option<ConnParams>,
    /// Instantaneous outstanding sacked bytes (raw bytes — same unit as
    /// the Sizes panel). Piecewise-constant between EVT_ACK events;
    /// sourced from `EVT_ACK.sacked`.
    pub sacked_bytes: Vec<[f64; 2]>,

    pub recovery_spans: Vec<[f64; 2]>,
    pub goodput_spans: Vec<[f64; 2]>,
    /// Contiguous runs of PACING_CALC_V2 reports sharing a single non-NONE
    /// cap_source.  Each entry is `(t_start, t_end, cap_source)` with times
    /// in seconds relative to the session start.  Adjacent events with the
    /// same cap are merged; NONE runs are elided.
    pub pacing_cap_spans: Vec<PacingCapSpan>,
    /// Classified "what was limiting the sender" spans, covering the full
    /// active range of the session.  Adjacent same-class buckets are
    /// run-length encoded into a single span.
    pub limiter_spans: Vec<LimiterSpan>,
    /// Session-level breakdown (percentages summing to ~100 %) of how
    /// much time was spent in each limiter class.
    pub limiter_summary: LimiterSummary,
    /// Inter-event data-plane gaps exceeding the idle threshold
    /// (default 50 ms) rendered as shaded spans; each entry is
    /// `[t_start, t_end]` in seconds relative to the session start.
    pub idle_spans: Vec<[f64; 2]>,

    /// Scatter of every fresh-send `EVT_OUT.len` (bytes) vs. its
    /// timestamp. Retransmits are split out into `retx_size_bytes`
    /// so the Size panel can colour them distinctly — use
    /// `send_size_bytes ∪ retx_size_bytes` when you want "all sends"
    /// (e.g. for the Size CDF).
    pub send_size_bytes: Vec<[f64; 2]>,
    /// Subset of OUT events that the kernel flagged as retransmits
    /// (`TCP_FLAG_RETRANSMIT` on `EVT_OUT.flags`). Each entry is
    /// `[t_seconds, bytes]`. These are *removed* from
    /// `send_size_bytes` (disjoint sets) so a single point renders as
    /// exactly one colour.
    pub retx_size_bytes: Vec<[f64; 2]>,
    pub ack_size_bytes: Vec<[f64; 2]>,

    /// Rare-event vertical markers.  Each entry is `(time_s, reason_text)`.
    pub rto_markers: Vec<(f64, String)>,
    pub window_collapse_markers: Vec<(f64, String)>,
    pub dsack_markers: Vec<(f64, String)>,

    /// `HTTP_REQ_TRK` slot lifetimes, paired from `HTTP_REQ_NEW` to
    /// whichever terminal event closed the slot.
    pub http_req_spans: Vec<HttpReqSpan>,
    /// `HTTP_REQ_ALLOCFAIL` times (seconds) — no free slot was available.
    pub http_req_allocfails: Vec<f64>,
}

/// How an `HTTP_REQ_TRK` slot's lifecycle ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpReqOutcome {
    /// `HTTP_REQ_FREED` — normal completion.
    Freed,
    /// `HTTP_REQ_STALE` — reaped while still open (client likely stalled).
    Stale,
    /// `HTTP_REQ_FORCEFREE` — evicted while still in-flight.
    Forcefree,
}

/// One `HTTP_REQ_NEW` → `HTTP_REQ_FREED`/`STALE`/`FORCEFREE` slot lifetime,
/// paired by matching `slot` numbers within a session (see
/// `extract_overview_data`'s `http_req_open` tracker).
#[derive(Debug, Clone, Copy)]
pub struct HttpReqSpan {
    pub slot: u8,
    pub t_start: f64,
    pub t_end: f64,
    pub outcome: HttpReqOutcome,
}

/// Convert a dense time-series into "post" step data for staircase rendering.
pub fn to_step_post(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    if points.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::with_capacity(points.len() * 2);
    for window in points.windows(2) {
        result.push(window[0]);
        result.push([window[1][0], window[0][1]]);
    }
    if let Some(last) = points.last() {
        result.push(*last);
    }
    result
}

/// Extract all plot series that come directly from parsed events (no computed rates).
///
/// `aux` is the per-session side data produced by `retain_plot_events`:
/// the peer-rwnd `(ts_us, win)` RLE'd stream and the once-per-session
/// `ConnParams`. Both are honoured when present:
///   - `aux.peer_rwnd` (when `Some`) replaces the in-event `EVT_IN`
///     scan, since `retain_plot_events` drops `EVT_IN` from the event
///     vector.
///   - `aux.conn_params` (when `Some`) supplies `snd_wscale` so peer
///     rwnd values can be reported in real bytes and the limiter
///     classifier can produce `Rwnd`-limited spans.
///
/// Pre-retain callers (`elog_cli stats`, `events`) pass `None` and
/// the function reconstructs both signals from the still-present
/// `EVT_IN` and `EVT_CONN_PARAMS_V2` events.
pub fn extract_overview_data(
    events: &[Event],
    aux: Option<&SessionAuxData>,
    trace_t0_us: Option<u64>,
) -> OverviewPlotData {
    let peer_rwnd_aux: Option<&[(u64, u32)]> = aux.map(|a| a.peer_rwnd.as_slice());
    // Prefer the aux-cached conn params; for pre-retain callers, find
    // the first `CONN_PARAMS_V2` directly in the event vector.
    let conn_params: Option<ConnParams> = aux
        .and_then(|a| a.conn_params.clone())
        .or_else(|| extract_conn_params(events));
    // `peer_rwnd_unscaled = true` means we deliberately suppress the
    // peer-rwnd UI for this session (no `CONN_PARAMS_V2` was present
    // to confirm the values are coming from a kernel that does the
    // scaling we expect). When `CONN_PARAMS_V2` *is* present, the
    // production tcp_input.c / ngen.c paths log `tiwin` (already
    // shifted by `snd_scale`), so `EVT_IN.win` is in bytes and we
    // plot it directly without any further shifting.
    let peer_rwnd_unscaled = conn_params.is_none();
    // When the caller provides a trace-wide origin (typically the
    // capture’s file-header `capture_start`), use it instead of
    // the session’s first-event ts. The result is that every plot
    // x-axis is in "seconds since trace start" rather than
    // "seconds since this session started", which means strips
    // and panels share the same coordinate system across all tabs.
    // CLI callers that don’t care pass None and get the legacy
    // session-relative output.
    let t0 = trace_t0_us.unwrap_or_else(|| events.first().map(|e| e.ts).unwrap_or(0));

    let mut rtt_samples = Vec::new();
    let mut srtt = Vec::new();
    let mut path_rtt = Vec::new();
    let mut min_rtt = Vec::new();
    let mut gp_filter_min = Vec::new();
    let mut gp_srtt = Vec::new();
    let mut kernel_goodput_mbps = Vec::new();
    let mut pacing_rate_mbps = Vec::new();
    // Default captures carry the pacing rate on GOODPUT once PACING_CALC
    // is TRACE; prefer PACING_CALC whenever the capture has it.
    let has_pacing_calc = events
        .iter()
        .any(|e| matches!(e.payload, EventPayload::PacingCalc { .. }));
    let mut pacing_cap_samples: Vec<(f64, u8)> = Vec::new();
    let mut cwnd_kb = Vec::new();
    let mut ssthresh_kb = Vec::new();
    let mut inflight_kb = Vec::new();
    let mut peer_rwnd_kb: Vec<[f64; 2]> = Vec::new();
    let mut sndbuf_kb: Vec<[f64; 2]> = Vec::new();
    // Running `sb_used` estimate. Anchored authoritatively from
    // `EVT_USER_SEND.sndbuf_used` and shrunk by `EVT_ACK.acked` in
    // between snapshots. `None` until the first USER_SEND so that
    // legacy / control-only sessions render as "no series".
    //
    // We deliberately do NOT use `EVT_SENDFILE.length` to grow this
    // estimate. Inside `vn_sendfile()` the kernel calls
    // `pr->pr_send()` (which routes through `tcp_usr_send` and
    // emits USER_SEND with `sbused(&so->so_snd)`) before
    // `tcp_eventlog_sendfile()` fires, so every SENDFILE is
    // already paired with an authoritative USER_SEND snapshot at
    // the same timestamp. SENDFILE's `length` field is the syscall
    // lookahead window the app *requested* (e.g. 16 MB), not the
    // bytes that actually got queued (capped by `sbspace()` —
    // typically a few hundred KB), so adding it would over-count
    // by an order of magnitude.
    let mut sndbuf_used: Option<f64> = None;
    let mut sacked_bytes: Vec<[f64; 2]> = Vec::new();
    let mut inflight_tracker: u64 = 0;
    let mut recovery_spans = Vec::new();
    let mut goodput_spans = Vec::new();
    let mut send_size_bytes: Vec<[f64; 2]> = Vec::new();
    let mut retx_size_bytes: Vec<[f64; 2]> = Vec::new();
    let mut ack_size_bytes = Vec::new();
    let mut recovery_enter: Option<u64> = None;
    let mut srtt_sum_us: u64 = 0;
    let mut srtt_count: u64 = 0;

    // Markers
    let mut rto_markers: Vec<(f64, String)> = Vec::new();
    let mut window_collapse_markers: Vec<(f64, String)> = Vec::new();
    let mut dsack_markers: Vec<(f64, String)> = Vec::new();

    // `HTTP_REQ_TRK` slots: only one request occupies a given slot number
    // at a time, so pairing each terminal event with the most recent
    // `HTTP_REQ_NEW` on the same slot number reconstructs the lifetime.
    let mut http_req_open: FxHashMap<u8, f64> = FxHashMap::default();
    let mut http_req_spans: Vec<HttpReqSpan> = Vec::new();
    let mut http_req_allocfails: Vec<f64> = Vec::new();
    // Wall time of the last ACK. GOODPUT is emitted before the ACK
    // that triggered it, so this is the previous ACK when the sample
    // is logged. An idle flush is that log on an ACK more than
    // rack_gp_idle_thresh_us (default 100 ms) later, and utim ended
    // on this ACK.
    const GP_IDLE_FLUSH_US: u64 = 100_000;
    let mut last_ack_ts: Option<u64> = None;

    for e in events {
        let t = (e.ts - t0) as f64 / USEC_IN_SEC;

        match &e.payload {
            EventPayload::Rtt {
                rtt: r,
                srtt: sr,
                path_rtt: pr,
                lowest_rtt,
                gp_filter_min: gfm,
                gp_srtt: gs,
            } => {
                if *r > 0 && *r < RTT_UNDERFLOW_THRESHOLD {
                    rtt_samples.push([t, *r as f64 / 1000.0]);
                }
                if *sr > 0 && *sr < RTT_UNDERFLOW_THRESHOLD {
                    srtt.push([t, *sr as f64 / 1000.0]);
                    srtt_sum_us += *sr as u64;
                    srtt_count += 1;
                }
                if *pr > 0 && *pr < RTT_UNDERFLOW_THRESHOLD {
                    path_rtt.push([t, *pr as f64 / 1000.0]);
                }
                if *lowest_rtt > 0 && *lowest_rtt < RTT_UNDERFLOW_THRESHOLD {
                    min_rtt.push([t, *lowest_rtt as f64 / 1000.0]);
                }
                if *gfm > 0 && *gfm < RTT_UNDERFLOW_THRESHOLD {
                    gp_filter_min.push([t, *gfm as f64 / 1000.0]);
                }
                if *gs > 0 && *gs < RTT_UNDERFLOW_THRESHOLD {
                    gp_srtt.push([t, *gs as f64 / 1000.0]);
                }
            }
            EventPayload::Goodput {
                goodput,
                utim,
                pacing_bw,
                ..
            } => {
                kernel_goodput_mbps.push([t, *goodput as f64 * 8.0 / 1e6]);
                if !has_pacing_calc && *pacing_bw > 0 {
                    pacing_rate_mbps.push([t, *pacing_bw as f64 * 8.0 / 1e6]);
                }
                if *utim > 0 {
                    let end_ts = match last_ack_ts {
                        Some(ack_ts) if e.ts.saturating_sub(ack_ts) > GP_IDLE_FLUSH_US => ack_ts,
                        _ => e.ts,
                    };
                    let end = (end_ts - t0) as f64 / USEC_IN_SEC;
                    let start = end - *utim as f64 / USEC_IN_SEC;
                    goodput_spans.push([start, end]);
                }
            }
            EventPayload::PacingCalc {
                bw_est,
                method,
                cap_source,
                ..
            } if *method == 2 => {
                pacing_rate_mbps.push([t, *bw_est as f64 * 8.0 / 1e6]);
                pacing_cap_samples.push((t, *cap_source));
            }
            EventPayload::Cwnd { cwnd, ssthresh } => {
                cwnd_kb.push([t, *cwnd as f64 / 1024.0]);
                if (*ssthresh as u64) < 1_000_000_000 {
                    ssthresh_kb.push([t, *ssthresh as f64 / 1024.0]);
                }
            }
            EventPayload::In { win, .. } => {
                // Pre-retain path: `EVT_IN` events are still in the
                // stream. Post-retain, these are dropped and the
                // equivalent samples come from `peer_rwnd_aux`.
                // Skip entirely when we don't have `CONN_PARAMS_V2`
                // — without that we don't know we can trust the
                // kernel's window-scaling behaviour for this capture.
                if peer_rwnd_aux.is_none() && !peer_rwnd_unscaled {
                    peer_rwnd_kb.push([t, *win as f64 / 1024.0]);
                }
            }
            EventPayload::Out { len, flags, .. } => {
                if *len > 0 {
                    if (*flags & TCP_FLAG_RETRANSMIT) != 0 {
                        retx_size_bytes.push([t, *len as f64]);
                    } else {
                        send_size_bytes.push([t, *len as f64]);
                    }
                    inflight_tracker += *len as u64;
                    inflight_kb.push([t, inflight_tracker as f64 / 1024.0]);
                }
            }
            EventPayload::Ack {
                inflight,
                acked,
                sacked,
                ..
            } => {
                last_ack_ts = Some(e.ts);
                let post_ack = inflight.saturating_sub(*acked);
                inflight_tracker = post_ack as u64;
                inflight_kb.push([t, post_ack as f64 / 1024.0]);
                sacked_bytes.push([t, *sacked as f64]);
                if *acked > 0 {
                    ack_size_bytes.push([t, *acked as f64]);
                }
                // Acked bytes are freed from the send buffer.
                if let Some(s) = sndbuf_used.as_mut() {
                    *s = (*s - *acked as f64).max(0.0);
                    sndbuf_kb.push([t, *s / 1024.0]);
                }
            }
            EventPayload::UserSend { sndbuf_used: u, .. } => {
                let v = *u as f64;
                sndbuf_used = Some(v);
                sndbuf_kb.push([t, v / 1024.0]);
            }
            EventPayload::EnterRecovery { .. } => {
                recovery_enter = Some(e.ts);
            }
            EventPayload::ExitRecovery { .. } => {
                if let Some(enter_ts) = recovery_enter.take() {
                    recovery_spans.push([
                        (enter_ts - t0) as f64 / USEC_IN_SEC,
                        (e.ts - t0) as f64 / USEC_IN_SEC,
                    ]);
                }
            }
            EventPayload::Dsack {
                num_dsack,
                dsack_persist,
                round,
                ..
            } => {
                let label =
                    format!("DSACK round {round}: {num_dsack} total (persist {dsack_persist})");
                dsack_markers.push((t, label));
            }
            EventPayload::Rto {
                timeout_num, rtt, ..
            } => {
                rto_markers.push((
                    t,
                    format!("RTO fired (timeout #{timeout_num}, rtt {rtt}us)"),
                ));
            }
            EventPayload::WindowCollapse {
                count, direction, ..
            } => {
                window_collapse_markers.push((
                    t,
                    format!(
                        "Window collapse ({count} segs, {})",
                        collapse_dir_name(*direction)
                    ),
                ));
            }
            EventPayload::HttpReqNew { slot, .. } => {
                http_req_open.insert(*slot, t);
            }
            EventPayload::HttpReqFreed { slot, .. } => {
                if let Some(t_start) = http_req_open.remove(slot) {
                    http_req_spans.push(HttpReqSpan {
                        slot: *slot,
                        t_start,
                        t_end: t,
                        outcome: HttpReqOutcome::Freed,
                    });
                }
            }
            EventPayload::HttpReqStale { slot, .. } => {
                if let Some(t_start) = http_req_open.remove(slot) {
                    http_req_spans.push(HttpReqSpan {
                        slot: *slot,
                        t_start,
                        t_end: t,
                        outcome: HttpReqOutcome::Stale,
                    });
                }
            }
            EventPayload::HttpReqForcefree { slot, .. } => {
                if let Some(t_start) = http_req_open.remove(slot) {
                    http_req_spans.push(HttpReqSpan {
                        slot: *slot,
                        t_start,
                        t_end: t,
                        outcome: HttpReqOutcome::Forcefree,
                    });
                }
            }
            EventPayload::HttpReqAllocfail { .. } => {
                http_req_allocfails.push(t);
            }
            _ => {}
        }
    }

    let ack_pairs: Vec<(u64, u32)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::Ack { acked, .. } if *acked > 0 => Some((e.ts, *acked)),
            _ => None,
        })
        .collect();
    let avg_srtt_us = srtt_sum_us.checked_div(srtt_count).unwrap_or(50_000);
    let window_us = (avg_srtt_us * 5 / 2).clamp(100_000, 1_000_000);
    let step_us = (avg_srtt_us / 2).clamp(20_000, 200_000);
    let ack_rate_mbps: Vec<[f64; 2]> = if ack_pairs.len() >= 2 {
        sliding_window_rate(&ack_pairs, window_us, step_us, t0)
            .into_iter()
            .map(|(t, bps)| [t, bps * 8.0 / 1e6])
            .collect()
    } else {
        Vec::new()
    };

    let session_end_s = events
        .last()
        .map(|e| (e.ts - t0) as f64 / USEC_IN_SEC)
        .unwrap_or(0.0);
    let session_start_us = events.first().map(|e| e.ts).unwrap_or(t0);
    let session_end_us = events.last().map(|e| e.ts).unwrap_or(session_start_us);
    let pacing_cap_spans = build_pacing_cap_spans(&pacing_cap_samples, session_end_s);

    // Same suppression as the pre-retain branch above: hide the
    // peer-rwnd series entirely when no `CONN_PARAMS_V2` is present.
    if let (Some(aux), false) = (peer_rwnd_aux, peer_rwnd_unscaled) {
        peer_rwnd_kb.reserve(aux.len());
        for (ts, win) in aux {
            let t = (ts.saturating_sub(t0)) as f64 / USEC_IN_SEC;
            peer_rwnd_kb.push([t, *win as f64 / 1024.0]);
        }
    }

    let idle_spans = build_idle_spans(events, t0, IDLE_GAP_US);

    // --- Limiter classification (per ~50 ms bucket + RLE) -----------------
    // Build a `(ts, peer_rwnd_bytes)` series the classifier can walk
    // alongside the event stream. The kernel's tcp_input.c / ngen.c
    // already stores `tiwin = th_win << snd_scale` in `EVT_IN.win`
    // before logging, so the values are already in bytes. We gate on
    // `conn_params.is_some()` purely as a "trust this kernel" signal:
    // a session that emits CONN_PARAMS_V2 is from a recent kernel
    // where peer-rwnd interpretation is well-understood. Older
    // captures fall back to cwnd-only classification.
    let peer_rwnd_for_classifier: Vec<(u64, u64)> = if conn_params.is_some() {
        if let Some(aux) = peer_rwnd_aux {
            aux.iter().map(|&(ts, win)| (ts, win as u64)).collect()
        } else {
            events
                .iter()
                .filter_map(|e| match &e.payload {
                    EventPayload::In { win, .. } => Some((e.ts, *win as u64)),
                    _ => None,
                })
                .collect()
        }
    } else {
        Vec::new()
    };
    let (limiter_spans, limiter_summary) = build_limiter_spans(
        events,
        session_start_us,
        session_end_us,
        t0,
        LIMITER_BUCKET_US,
        &pacing_cap_samples,
        &peer_rwnd_for_classifier,
        MAX_LIMITER_SPANS,
    );

    OverviewPlotData {
        rtt_samples,
        srtt,
        path_rtt,
        min_rtt,
        gp_filter_min,
        gp_srtt,
        kernel_goodput_mbps,
        pacing_rate_mbps,
        ack_rate_mbps,
        cwnd_kb,
        ssthresh_kb,
        inflight_kb,
        sndbuf_kb,
        peer_rwnd_kb,
        peer_rwnd_unscaled,
        conn_params,
        sacked_bytes,
        recovery_spans,
        goodput_spans,
        pacing_cap_spans,
        limiter_spans,
        limiter_summary,
        idle_spans,
        send_size_bytes,
        retx_size_bytes,
        ack_size_bytes,
        rto_markers,
        window_collapse_markers,
        dsack_markers,
        http_req_spans,
        http_req_allocfails,
    }
}

/// Build the list of idle-gap spans from inter-event gaps between
/// consecutive data-plane events (EVT_OUT / EVT_ACK).  Each entry is
/// `[t_start_s, t_end_s]`, relative to `t0`.
pub fn build_idle_spans(events: &[Event], t0: u64, threshold_us: u64) -> Vec<[f64; 2]> {
    let mut spans = Vec::new();
    let mut prev: Option<u64> = None;
    for e in events {
        let is_data_plane = matches!(
            e.payload,
            EventPayload::Out { .. } | EventPayload::Ack { .. },
        );
        if !is_data_plane {
            continue;
        }
        if let Some(p) = prev {
            if e.ts.saturating_sub(p) > threshold_us {
                spans.push([
                    (p - t0) as f64 / USEC_IN_SEC,
                    (e.ts - t0) as f64 / USEC_IN_SEC,
                ]);
            }
        }
        prev = Some(e.ts);
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::extract_overview_data;
    use crate::elog_parser::{Event, EventPayload};

    fn ack(ts: u64) -> Event {
        Event {
            ts,
            cpu: 0,
            tid: 0,
            event_id: 102,
            payload: EventPayload::Ack {
                acked: 1,
                snd_una: 0,
                inflight: 0,
                sacked: 0,
                nsegs: 1,
                rcv_time_us: ts,
                num_blocks: 0,
                blk1_start: 0,
                blk1_end: 0,
                blk2_start: 0,
                blk2_end: 0,
                blk3_start: 0,
                blk3_end: 0,
                blk4_start: 0,
                blk4_end: 0,
            },
        }
    }

    fn goodput(ts: u64, utim: u64) -> Event {
        Event {
            ts,
            cpu: 0,
            tid: 0,
            event_id: 207,
            payload: EventPayload::Goodput {
                goodput: 1,
                raw_bw: 1,
                utim,
                gp_srtt: 0,
                lowest_rtt: 0,
                pacing_bw: 0,
            },
        }
    }

    fn goodput_paced(ts: u64, pacing_bw: u64) -> Event {
        let mut e = goodput(ts, 10_000);
        if let EventPayload::Goodput { pacing_bw: p, .. } = &mut e.payload {
            *p = pacing_bw;
        }
        e
    }

    fn pacing_calc(ts: u64, bw_est: u64) -> Event {
        Event {
            ts,
            cpu: 0,
            tid: 0,
            event_id: 600,
            payload: EventPayload::PacingCalc {
                slot: 1_000,
                bw_est,
                method: 2,
                quality: 0,
                in_slow_start: 0,
                cap_source: 0,
                inflight: 0,
            },
        }
    }

    #[test]
    fn goodput_pacing_bw_drives_pacing_line_without_pacing_calc() {
        let events = vec![
            goodput_paced(1_000_000, 1_250_000),
            goodput_paced(2_000_000, 0),
        ];
        let data = extract_overview_data(&events, None, Some(0));
        assert_eq!(data.pacing_rate_mbps, vec![[1.0, 10.0]]);
    }

    #[test]
    fn pacing_calc_wins_over_goodput_pacing_bw() {
        let events = vec![
            pacing_calc(1_000_000, 2_500_000),
            goodput_paced(1_500_000, 1_250_000),
        ];
        let data = extract_overview_data(&events, None, Some(0));
        assert_eq!(data.pacing_rate_mbps, vec![[1.0, 20.0]]);
    }

    #[test]
    fn summary_pacing_falls_back_to_goodput_pacing_bw() {
        use crate::analysis::session::session_summary_for;
        let s = session_summary_for(1, &[goodput_paced(1_000_000, 1_250_000)]);
        assert_eq!(s.avg_pacing_rate_mbps, 10.0);
        let s = session_summary_for(
            1,
            &[
                pacing_calc(1_000_000, 2_500_000),
                goodput_paced(1_500_000, 1_250_000),
            ],
        );
        assert_eq!(s.avg_pacing_rate_mbps, 20.0);
    }

    #[test]
    fn out_paced_flag_marks_pacing_without_pacing_calc() {
        use crate::elog_parser::{TCP_FLAG_ACK, TCP_FLAG_PACED};
        let sends = |flags: u16| -> Vec<Event> {
            (0..200)
                .flat_map(|i| {
                    let ts = 1_000_000 + i * 10_000;
                    [out(ts, 1448, flags), ack(ts + 5_000)]
                })
                .collect()
        };
        let plain = extract_overview_data(&sends(TCP_FLAG_ACK), None, Some(0));
        let paced = extract_overview_data(&sends(TCP_FLAG_ACK | TCP_FLAG_PACED), None, Some(0));
        assert_eq!(plain.limiter_summary.pacing_pct, 0.0);
        assert!(paced.limiter_summary.pacing_pct > 0.0);
    }

    #[test]
    fn in_window_span_ends_at_the_sample() {
        // GOODPUT is logged before the ACK that triggered it. The
        // previous ACK is 40 ms earlier, inside the idle threshold.
        let events = vec![ack(1_000_000), goodput(1_040_000, 30_000), ack(1_040_000)];
        let data = extract_overview_data(&events, None, Some(0));
        assert_eq!(data.goodput_spans, vec![[1.010, 1.040]]);
    }

    #[test]
    fn idle_flush_span_ends_at_the_previous_ack() {
        // The sample is logged 1.25 s later, on the ACK that found
        // the gap. utim ended on the ACK at 1.0 s.
        let events = vec![ack(1_000_000), goodput(2_250_000, 531_000), ack(2_250_000)];
        let data = extract_overview_data(&events, None, Some(0));
        let [start, end] = data.goodput_spans[0];
        assert!((end - 1.0).abs() < 1e-9);
        assert!((start - (1.0 - 0.531)).abs() < 1e-9);
    }

    fn out(ts: u64, len: u32, flags: u16) -> Event {
        Event {
            ts,
            cpu: 0,
            tid: 0,
            event_id: 101,
            payload: EventPayload::Out {
                seq: 1000,
                ack: 2000,
                win: 65535,
                len,
                flags,
                cwnd: 14600,
                th_off: 5,
                th_sum: 0,
                th_urp: 0,
            },
        }
    }

    #[test]
    fn out_retransmit_flag_populates_retx_size_bytes() {
        use crate::elog_parser::{TCP_FLAG_ACK, TCP_FLAG_RETRANSMIT};

        // Fresh OUT and retransmit OUT
        let events = vec![
            out(1_000_000, 1460, TCP_FLAG_ACK),
            out(1_010_000, 1460, TCP_FLAG_ACK | TCP_FLAG_RETRANSMIT),
        ];
        let plot_data = extract_overview_data(&events, None, None);
        assert_eq!(plot_data.send_size_bytes.len(), 1);
        assert_eq!(plot_data.send_size_bytes[0][1], 1460.0);
        assert_eq!(plot_data.retx_size_bytes.len(), 1);
        assert_eq!(plot_data.retx_size_bytes[0][1], 1460.0);
    }
}
