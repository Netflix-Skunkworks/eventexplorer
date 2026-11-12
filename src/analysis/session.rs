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

use crate::elog_parser::*;
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;

/// Stable per-connection parameters extracted from the session's first
/// `EVT_CONN_PARAMS_V2` event. Captured once on retention and reused
/// by every analysis pass that needs to surface the negotiated window
/// scales / options / cc-algo. The presence of this struct (vs.
/// `None`) is also used as a "this is a modern, well-formed capture"
/// gate for enabling peer-rwnd UI and the `Rwnd` limiter
/// classification.
#[derive(Debug, Clone)]
pub struct ConnParams {
    pub iss: u32,
    pub irs: u32,
    /// Negotiated window scale for peer-advertised windows. See
    /// `EventPayload::ConnParamsV2::snd_wscale` — the kernel
    /// already shifts `EVT_IN.win` by this value before logging, so
    /// EventExplorer keeps the field around for header-display
    /// purposes only.
    pub snd_wscale: u8,
    /// Negotiated window scale for our own advertised windows (the
    /// peer applies this to `EVT_OUT.win`).
    pub rcv_wscale: u8,
    /// `CP_FLAG_*` bitmask of negotiated options.
    pub flags: u8,
    /// Initial congestion-control algorithm name.
    pub cc_algo: String,
    /// Initial TCP stack name. Empty on older captures lacking the field.
    pub tcp_stack: String,
}

/// Per-session side data precomputed at retain-time.
///
/// `EventPayload::In` events are ≈20% of the event stream but only
/// contribute a single field (`win`) to the analysis. Keeping them
/// inflates event RAM by ~1.6×. Instead, we scan them once during
/// `retain_plot_events`, run-length-encode on `win` (typical captures
/// collapse to a few tens of distinct values per session), and store
/// the resulting `(ts, win)` pairs on the side. The event vector is
/// then free to drop `In` entirely.
///
/// `conn_params` is similarly snapshotted from the first
/// `EVT_CONN_PARAMS_V2` (kernels that emit it; pre-`_V2` captures
/// leave this `None`) so callers can reach `snd_wscale` etc. without
/// rescanning the event vector.
#[derive(Default, Clone)]
pub struct SessionAuxData {
    /// RLE'd `(ts_us, win)` samples from the session's `EVT_IN`
    /// stream. Sorted by `ts_us`. The series is interpreted as
    /// piecewise-constant: `win` applies from its sample's `ts`
    /// until the next sample's `ts` (or end-of-session). The kernel
    /// stores the already-scaled `tiwin = th_win << snd_scale` in
    /// `EVT_IN.win` (see tcp_input.c / ngen.c), so the values here
    /// are bytes ready to plot — no further shifting needed.
    pub peer_rwnd: Vec<(u64, u32)>,
    /// Stable per-connection parameters from the first
    /// `EVT_CONN_PARAMS_V2`, or `None` for older kernels that don't
    /// emit it.
    pub conn_params: Option<ConnParams>,
}

/// Strip events not needed for plotting, releasing ~60% of event memory.
/// Call this after session summaries have been computed.
///
/// `EventPayload::In` events are precomputed into `SessionAuxData`
/// (RLE'd on `win`) and then dropped from the event vector. Downstream
/// analysis (`extract_overview_data`, `build_limiter_spans`) consumes
/// the aux data alongside the retained event stream.
pub fn retain_plot_events(sessions: &mut Sessions) -> FxHashMap<u64, SessionAuxData> {
    let build_aux = |sid: &u64, events: &Vec<Event>| {
        let mut peer_rwnd: Vec<(u64, u32)> = Vec::new();
        let mut last_win: Option<u32> = None;
        let mut conn_params: Option<ConnParams> = None;
        for e in events.iter() {
            match &e.payload {
                EventPayload::In { win, .. } => {
                    if last_win != Some(*win) {
                        peer_rwnd.push((e.ts, *win));
                        last_win = Some(*win);
                    }
                }
                EventPayload::ConnParamsV2 {
                    iss,
                    irs,
                    snd_wscale,
                    rcv_wscale,
                    flags,
                    cc_algo,
                    tcp_stack,
                    ..
                } if conn_params.is_none() => {
                    // Capture the first event only. Subsequent
                    // events from `tcp_eventlog_dump_session`
                    // (e.g. on stack switches) carry the same
                    // negotiated parameters, so they're redundant.
                    conn_params = Some(ConnParams {
                        iss: *iss,
                        irs: *irs,
                        snd_wscale: *snd_wscale,
                        rcv_wscale: *rcv_wscale,
                        flags: *flags,
                        cc_algo: cc_algo.clone(),
                        tcp_stack: tcp_stack.clone(),
                    });
                }
                _ => {}
            }
        }
        peer_rwnd.shrink_to_fit();
        (
            *sid,
            SessionAuxData {
                peer_rwnd,
                conn_params,
            },
        )
    };

    #[cfg(not(target_arch = "wasm32"))]
    let aux: FxHashMap<u64, SessionAuxData> = sessions
        .par_iter()
        .map(|(sid, events)| build_aux(sid, events))
        .collect();
    #[cfg(target_arch = "wasm32")]
    let aux: FxHashMap<u64, SessionAuxData> = sessions
        .iter()
        .map(|(sid, events)| build_aux(sid, events))
        .collect();

    let retain_one = |events: &mut Vec<Event>| {
        events.retain(|e| {
            matches!(
                e.payload,
                EventPayload::Rtt { .. }
                    | EventPayload::Goodput { .. }
                    | EventPayload::GpCheck { .. }
                    | EventPayload::PacingCalc { .. }
                    | EventPayload::Cwnd { .. }
                    | EventPayload::Ack { .. }
                    | EventPayload::Out { .. }
                    | EventPayload::UserSend { .. }
                    | EventPayload::SendFile { .. }
                    | EventPayload::EnterRecovery { .. }
                    | EventPayload::ExitRecovery { .. }
                    | EventPayload::Dsack { .. }
                    | EventPayload::Rto { .. }
                    | EventPayload::WindowCollapse { .. }
                    | EventPayload::HttpReqNew { .. }
                    | EventPayload::HttpReqFreed { .. }
                    | EventPayload::HttpReqStale { .. }
                    | EventPayload::HttpReqForcefree { .. }
                    | EventPayload::HttpReqAllocfail { .. }
            )
        });
        events.shrink_to_fit();
    };
    #[cfg(not(target_arch = "wasm32"))]
    sessions
        .par_iter_mut()
        .for_each(|(_, events)| retain_one(events));
    #[cfg(target_arch = "wasm32")]
    sessions
        .iter_mut()
        .for_each(|(_, events)| retain_one(events));

    aux
}

// ---------------------------------------------------------------------------
// Session summary helpers
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SessionSummary {
    pub session_id: u64,
    pub event_count: usize,
    pub duration_sec: f64,
    pub total_bytes_sent: u64,
    pub total_bytes_acked: u64,
    pub avg_goodput_mbps: f64,
    pub avg_pacing_rate_mbps: f64,
    pub avg_kernel_goodput_mbps: f64,
    pub avg_srtt_ms: f64,
    pub min_rtt_ms: f64,
    pub remote: String,
    pub log_id: String,
    pub start_ts_us: u64,
    pub end_ts_us: u64,
    pub recovery_count: u32,
    /// Owning process id for this TCP session (the pid of the
    /// process that owned the socket when the session was
    /// captured), or `None` when the kernel did not report it.
    ///
    /// Populated from a future `EVT_CONN_OWNER_PID` event the
    /// `tcp` provider does not yet emit. The hook lives here so
    /// the GUI can light up cross-links to the Processes tab and
    /// the per-session info popup the moment the kernel side
    /// lands. Until then this field stays `None` for every
    /// session and the linked UI affordances stay hidden.
    ///
    /// The producer-side change (FreeBSD kernel) needs to:
    ///   1. Add `EVT_CONN_OWNER_PID` (or similar) to
    ///      `tcp_eventlog_schema.src` carrying a single u32 pid.
    ///   2. Emit it once at session creation time (or whenever
    ///      `so_pgid` is otherwise resolved).
    ///
    /// Consumer-side here: parse the event into a new
    /// `EventPayload::ConnOwnerPid { pid }`, retain it via
    /// `retain_plot_events`, and populate `owner_pid` from the
    /// first such event seen for each session inside
    /// `session_summary_for`.
    pub owner_pid: Option<u32>,
    /// HTTP requests the `nginx` provider logged on this connection
    /// (nginx sessions share the TCP connection's session id). Filled
    /// in at load time; 0 when the capture has no nginx events.
    pub nginx_requests: u32,
}

/// A summary for a session seen only in the `nginx` provider (a capture
/// without `tcp` events for this connection, e.g. `-c nginx`). TCP
/// counters stay zeroed; the row exists so the session list covers every
/// connection that served HTTP requests.
pub fn nginx_stub_summary(
    sid: u64,
    counts: &crate::analysis::NginxSessionCounts,
) -> SessionSummary {
    SessionSummary {
        session_id: sid,
        event_count: counts.event_count,
        duration_sec: counts.last_ts.saturating_sub(counts.first_ts) as f64 / USEC_IN_SEC,
        total_bytes_sent: counts.bytes_sent,
        total_bytes_acked: 0,
        avg_goodput_mbps: 0.0,
        avg_pacing_rate_mbps: 0.0,
        avg_kernel_goodput_mbps: 0.0,
        avg_srtt_ms: 0.0,
        min_rtt_ms: 0.0,
        remote: String::new(),
        log_id: String::new(),
        start_ts_us: counts.first_ts,
        end_ts_us: counts.last_ts,
        recovery_count: 0,
        owner_pid: None,
        nginx_requests: counts.requests,
    }
}

pub fn session_summary_for(sid: u64, events: &[Event]) -> SessionSummary {
    let t0 = events.first().map(|e| e.ts).unwrap_or(0);
    let mut total_bytes_sent: u64 = 0;
    let mut total_bytes_acked: u64 = 0;
    let mut srtt_sum: f64 = 0.0;
    let mut srtt_count: u64 = 0;
    let mut min_rtt: u32 = u32::MAX;
    let mut pacing_sum_mbps: f64 = 0.0;
    let mut pacing_count: u64 = 0;
    let mut gp_pacing_sum_mbps: f64 = 0.0;
    let mut gp_pacing_count: u64 = 0;
    let mut remote = String::new();
    let mut log_id = String::new();
    let mut data_points: Vec<(u64, u64)> = Vec::new();
    let mut kernel_gp_sum_mbps: f64 = 0.0;
    let mut kernel_gp_count: u64 = 0;
    let mut recovery_count: u32 = 0;

    for e in events {
        match &e.payload {
            EventPayload::Out { len, .. } => {
                total_bytes_sent += *len as u64;
                if *len > 0 {
                    data_points.push((e.ts, *len as u64));
                }
            }
            EventPayload::Ack { acked, .. } => {
                total_bytes_acked += *acked as u64;
                if *acked > 0 {
                    data_points.push((e.ts, *acked as u64));
                }
            }
            EventPayload::Rtt {
                srtt, lowest_rtt, ..
            } => {
                if *srtt > 0 && *srtt < RTT_UNDERFLOW_THRESHOLD {
                    srtt_sum += *srtt as f64;
                    srtt_count += 1;
                }
                if *lowest_rtt > 0 && *lowest_rtt < RTT_UNDERFLOW_THRESHOLD {
                    min_rtt = min_rtt.min(*lowest_rtt);
                }
            }
            EventPayload::PacingCalc { bw_est, method, .. } if *method == 2 && *bw_est > 0 => {
                pacing_sum_mbps += *bw_est as f64 * 8.0 / 1e6;
                pacing_count += 1;
            }
            EventPayload::ConnSetIpV4 {
                dst_addr, dst_port, ..
            } => {
                remote = format!("{dst_addr}:{dst_port}");
            }
            EventPayload::ConnSetIpV6 {
                dst_addr, dst_port, ..
            } => {
                remote = format!("[{dst_addr}]:{dst_port}");
            }
            EventPayload::LogId { log_id: lid } => {
                log_id = lid.clone();
            }
            EventPayload::Goodput {
                goodput, pacing_bw, ..
            } => {
                if *goodput > 0 {
                    kernel_gp_sum_mbps += *goodput as f64 * 8.0 / 1e6;
                    kernel_gp_count += 1;
                }
                if *pacing_bw > 0 {
                    gp_pacing_sum_mbps += *pacing_bw as f64 * 8.0 / 1e6;
                    gp_pacing_count += 1;
                }
            }
            EventPayload::EnterRecovery { .. } => {
                recovery_count += 1;
            }
            _ => {}
        }
    }

    let avg_srtt_ms = if srtt_count > 0 {
        srtt_sum / srtt_count as f64 / 1000.0
    } else {
        0.0
    };

    let min_rtt_ms = if min_rtt < u32::MAX {
        min_rtt as f64 / 1000.0
    } else {
        0.0
    };

    // PACING_CALC wins when present; GOODPUT.pacing_bw covers default
    // captures where PACING_CALC is TRACE.
    let avg_pacing_rate_mbps = if pacing_count > 0 {
        pacing_sum_mbps / pacing_count as f64
    } else if gp_pacing_count > 0 {
        gp_pacing_sum_mbps / gp_pacing_count as f64
    } else {
        0.0
    };

    let avg_kernel_goodput_mbps = if kernel_gp_count > 0 {
        kernel_gp_sum_mbps / kernel_gp_count as f64
    } else {
        0.0
    };

    // Compute active duration using byte-weighted analysis: find the point
    // where 99% of data bytes have been transferred, then extend through any
    // contiguous non-idle activity that immediately follows.  This avoids
    // inflating duration with long tails of low-volume periodic events
    // (keepalives, probes, stale goodput reports) that the kernel may emit
    // well after the meaningful data transfer has finished.
    let dur = if data_points.len() < 2 {
        if data_points.is_empty() {
            0.0
        } else {
            (data_points[0].0 - t0) as f64 / USEC_IN_SEC
        }
    } else {
        data_points.sort_unstable_by_key(|p| p.0);

        let total_data_bytes: u64 = data_points.iter().map(|p| p.1).sum();
        let target_bytes = (total_data_bytes as f64 * 0.99) as u64;

        let mut cumulative: u64 = 0;
        let mut byte_end_idx = data_points.len() - 1;
        for (i, &(_, bytes)) in data_points.iter().enumerate() {
            cumulative += bytes;
            if cumulative >= target_bytes {
                byte_end_idx = i;
                break;
            }
        }

        let mut gaps: Vec<u64> = Vec::with_capacity(data_points.len() - 1);
        for i in 1..data_points.len() {
            gaps.push(data_points[i].0 - data_points[i - 1].0);
        }
        let median_idx = gaps.len() / 2;
        gaps.select_nth_unstable(median_idx);
        let median_gap = gaps[median_idx];
        let idle_threshold = median_gap.saturating_mul(100).clamp(2_000_000, 30_000_000);

        let mut end_idx = byte_end_idx;
        for i in (byte_end_idx + 1)..data_points.len() {
            if data_points[i].0 - data_points[i - 1].0 > idle_threshold {
                break;
            }
            end_idx = i;
        }

        (data_points[end_idx].0 - t0) as f64 / USEC_IN_SEC
    };

    let avg_goodput_mbps = if dur > 0.0 {
        total_bytes_acked as f64 * 8.0 / dur / 1e6
    } else {
        0.0
    };

    SessionSummary {
        session_id: sid,
        event_count: events.len(),
        duration_sec: dur,
        total_bytes_sent,
        total_bytes_acked,
        avg_goodput_mbps,
        avg_pacing_rate_mbps,
        avg_kernel_goodput_mbps,
        avg_srtt_ms,
        min_rtt_ms,
        remote,
        log_id,
        start_ts_us: t0,
        end_ts_us: events.last().map(|e| e.ts).unwrap_or(t0),
        recovery_count,
        // Populated from a future EVT_CONN_OWNER_PID event the
        // tcp provider does not yet emit. See SessionSummary
        // doc for the full plumbing recipe.
        owner_pid: None,
        // Merged from the nginx profile at load time.
        nginx_requests: 0,
    }
}

pub fn session_summaries(sessions: &Sessions) -> Vec<SessionSummary> {
    #[cfg(not(target_arch = "wasm32"))]
    let mut summaries: Vec<SessionSummary> = sessions
        .par_iter()
        .map(|(&sid, events)| session_summary_for(sid, events))
        .collect();
    #[cfg(target_arch = "wasm32")]
    let mut summaries: Vec<SessionSummary> = sessions
        .iter()
        .map(|(&sid, events)| session_summary_for(sid, events))
        .collect();
    summaries.sort_unstable_by_key(|b| std::cmp::Reverse(b.total_bytes_acked));
    summaries
}

/// Walk an event vector and return the first `EVT_CONN_PARAMS_V2`
/// payload (or `None`). Used by callers that haven't run
/// `retain_plot_events` and therefore can't read `aux.conn_params`.
pub fn extract_conn_params(events: &[Event]) -> Option<ConnParams> {
    events.iter().find_map(|e| match &e.payload {
        EventPayload::ConnParamsV2 {
            iss,
            irs,
            snd_wscale,
            rcv_wscale,
            flags,
            cc_algo,
            tcp_stack,
            ..
        } => Some(ConnParams {
            iss: *iss,
            irs: *irs,
            snd_wscale: *snd_wscale,
            rcv_wscale: *rcv_wscale,
            flags: *flags,
            cc_algo: cc_algo.clone(),
            tcp_stack: tcp_stack.clone(),
        }),
        _ => None,
    })
}
