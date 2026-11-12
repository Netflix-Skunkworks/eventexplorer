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

//! nginx request-layer analytics over the parsed `NginxProfile`. Everything
//! here is egui-free: it turns the per-request lifecycle events into list
//! rows, per-session lane spans, and the System-tab aggregate series.

use crate::elog_parser::*;

/// One row in the nginx request table (`elog_cli nginx` and the session view).
pub struct NginxRequestListRow {
    pub provider_id: u16,
    pub session_id: u64,
    pub request: u32,
    pub method: String,
    pub uri: String,
    pub status: Option<u16>,
    pub bytes_sent: Option<u64>,
    pub content_length: Option<i64>,
    /// REQUEST_START → REQUEST_DONE, seconds. `None` while still open.
    pub duration_s: Option<f64>,
    /// REQUEST_START → RESPONSE_HEADERS (time to first byte), seconds.
    pub ttfb_s: Option<f64>,
    pub start_s: f64,
    pub event_count: usize,
    /// Cache status name, "" when no cache lookup was logged.
    pub cache: String,
    pub upstream_tries: u8,
    /// `REQUEST_DONE` flags as `A|B`.
    pub flags: String,
    pub err_status: Option<u16>,
    pub messages: Vec<String>,
}

/// Status-class bucket for completed nginx requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NginxStatusClass {
    Informational,
    Success,
    Redirect,
    ClientError,
    ServerError,
}

impl NginxStatusClass {
    pub fn of(status: u16) -> Option<Self> {
        match status {
            100..=199 => Some(Self::Informational),
            200..=299 => Some(Self::Success),
            300..=399 => Some(Self::Redirect),
            400..=499 => Some(Self::ClientError),
            500..=599 => Some(Self::ServerError),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Informational => "1xx",
            Self::Success => "2xx",
            Self::Redirect => "3xx",
            Self::ClientError => "4xx",
            Self::ServerError => "5xx",
        }
    }
}

/// Cache status from the main-request CACHE_LOOKUP, else REQUEST_DONE.
fn request_cache_name(r: &NginxRequestInfo) -> &'static str {
    let v = r
        .cache
        .as_ref()
        .map(|c| c.cache_status)
        .or(r.detail.map(|d| d.cache_status))
        .unwrap_or(0);
    nginx_cache_status_name(v)
}

/// Upstream attempts from REQUEST_DONE, else the attempts logged.
fn request_upstream_tries(r: &NginxRequestInfo) -> u8 {
    r.detail
        .map(|d| d.upstream_tries)
        .filter(|&t| t != 0)
        .unwrap_or_else(|| r.upstream.iter().filter(|a| !a.subrequest).count().min(255) as u8)
}

/// Build the request-list rows, one per HTTP request, sorted by start.
pub fn build_nginx_request_list(prof: &NginxProfile, t0_us: u64) -> Vec<NginxRequestListRow> {
    let mut rows: Vec<NginxRequestListRow> = prof
        .requests
        .values()
        .map(|r| NginxRequestListRow {
            provider_id: r.provider_id,
            session_id: r.session_id,
            request: r.request,
            method: r.method.clone(),
            uri: r.uri.clone(),
            status: r.status,
            bytes_sent: r.bytes_sent,
            content_length: r.content_length,
            duration_s: r.duration_us().map(|us| us as f64 / 1_000_000.0),
            ttfb_s: r
                .headers_ts
                .map(|h| h.saturating_sub(r.start_ts) as f64 / 1_000_000.0),
            start_s: r.start_ts.saturating_sub(t0_us) as f64 / 1_000_000.0,
            event_count: r.event_count,
            cache: request_cache_name(r).to_string(),
            upstream_tries: request_upstream_tries(r),
            flags: r
                .detail
                .map(|d| nginx_done_flag_names(d.flags))
                .unwrap_or_default(),
            err_status: r.detail.map(|d| d.err_status).filter(|&e| e != 0),
            messages: r.messages.clone(),
        })
        .collect();
    rows.sort_by(|a, b| {
        a.start_s
            .partial_cmp(&b.start_s)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.session_id.cmp(&b.session_id))
            .then(a.request.cmp(&b.request))
    });
    rows
}

// ---------------------------------------------------------------------------
// Per-session rollup (shared session id with the `tcp` provider)
// ---------------------------------------------------------------------------

/// nginx activity rolled up by transport session id (the TCP connection's
/// EventLog session id), for merging into the TCP session list.
#[derive(Debug, Clone, Copy, Default)]
pub struct NginxSessionCounts {
    pub requests: u32,
    pub event_count: usize,
    pub bytes_sent: u64,
    pub first_ts: u64,
    pub last_ts: u64,
}

pub fn nginx_session_counts(prof: &NginxProfile) -> FxHashMap<u64, NginxSessionCounts> {
    let mut map: FxHashMap<u64, NginxSessionCounts> = FxHashMap::default();
    for r in prof.requests.values() {
        let c = map.entry(r.session_id).or_default();
        c.requests += 1;
        c.event_count += r.event_count;
        c.bytes_sent += r.bytes_sent.unwrap_or(0);
        let end = r.done_ts.unwrap_or(r.start_ts);
        if c.requests == 1 {
            c.first_ts = r.start_ts;
            c.last_ts = end;
        } else {
            c.first_ts = c.first_ts.min(r.start_ts);
            c.last_ts = c.last_ts.max(end);
        }
    }
    // Connections with no logged request (TLS failures, idle closes).
    for conn in prof.conns.values() {
        let (Some(first), Some(last)) =
            (conn.start_ts.or(conn.end_ts), conn.end_ts.or(conn.start_ts))
        else {
            continue;
        };
        map.entry(conn.session_id)
            .or_insert_with(|| NginxSessionCounts {
                first_ts: first,
                last_ts: last,
                ..Default::default()
            });
    }
    map
}

/// Fold nginx per-connection activity into the TCP session list: set
/// `nginx_requests` on matching rows and append stub rows for connections
/// seen only in the nginx provider (e.g. a `-c nginx` capture).
pub fn merge_nginx_session_summaries(
    summaries: &mut Vec<super::SessionSummary>,
    sessions: &Sessions,
    prof: &NginxProfile,
) {
    let counts = nginx_session_counts(prof);
    if counts.is_empty() {
        return;
    }
    for s in summaries.iter_mut() {
        if let Some(c) = counts.get(&s.session_id) {
            s.nginx_requests = c.requests;
        }
    }
    for (sid, c) in &counts {
        if !sessions.contains_key(sid) {
            summaries.push(super::nginx_stub_summary(*sid, c));
        }
    }
}

/// One nginx request as a lane span on the TCP session's HTTP Requests
/// plot. `t_end` is `None` while the request is still open.
#[derive(Debug, Clone, Copy)]
pub struct NginxReqSpan {
    pub provider_id: u16,
    pub request: u32,
    pub lane: u32,
    pub t_start: f64,
    pub t_end: Option<f64>,
    pub status: Option<u16>,
    pub bytes_sent: Option<u64>,
    /// `NGX_HTTP_CACHE_*`, 0 when none.
    pub cache_status: u8,
    pub upstream_tries: u8,
    pub done_flags: u16,
    pub busy_defer_ms: u32,
    pub aio_wait_ms: u32,
    pub messages: u16,
}

impl NginxReqSpan {
    /// Extra hover detail after the status/duration, e.g.
    /// `cache MISS · 2 tries · busy 12 ms · TIMEDOUT`. Empty when none.
    pub fn detail(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let cache = nginx_cache_status_name(self.cache_status);
        if !cache.is_empty() {
            parts.push(format!("cache {cache}"));
        }
        if self.upstream_tries > 0 {
            let noun = if self.upstream_tries == 1 {
                "try"
            } else {
                "tries"
            };
            parts.push(format!("{} upstream {noun}", self.upstream_tries));
        }
        if self.busy_defer_ms > 0 {
            parts.push(format!("disk busy {} ms", self.busy_defer_ms));
        }
        if self.aio_wait_ms > 0 {
            parts.push(format!("aio {} ms", self.aio_wait_ms));
        }
        let abnormal =
            self.done_flags & (NGINX_DONE_TERMINATED | NGINX_DONE_TIMEDOUT | NGINX_DONE_CONN_ERROR);
        if abnormal != 0 {
            parts.push(nginx_done_flag_names(abnormal));
        }
        if self.messages > 0 {
            let s = if self.messages == 1 { "" } else { "s" };
            parts.push(format!("{} message{s}", self.messages));
        }
        parts.join(" · ")
    }
}

/// Requests for one transport session as lane spans, in capture-relative
/// seconds. Lanes are assigned greedily (lowest lane whose previous request
/// finished), so serial keepalive requests share lane 0 and only concurrent
/// (e.g. HTTP/2) requests stack.
pub fn nginx_session_spans(prof: &NginxProfile, session_id: u64, t0_us: u64) -> Vec<NginxReqSpan> {
    let rel = |ts: u64| ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
    let mut reqs: Vec<&NginxRequestInfo> = prof
        .requests
        .values()
        .filter(|r| r.session_id == session_id)
        .collect();
    reqs.sort_by_key(|r| r.start_ts);

    // Lane occupancy: last end time per lane; None = open request.
    let mut lanes: Vec<Option<u64>> = Vec::new();
    reqs.iter()
        .map(|r| {
            let lane = lanes
                .iter()
                .position(|end| end.is_some_and(|e| e <= r.start_ts))
                .unwrap_or(lanes.len());
            if lane == lanes.len() {
                lanes.push(r.done_ts);
            } else {
                lanes[lane] = r.done_ts;
            }
            NginxReqSpan {
                provider_id: r.provider_id,
                request: r.request,
                lane: lane as u32,
                t_start: rel(r.start_ts),
                t_end: r.done_ts.map(rel),
                status: r.status,
                bytes_sent: r.bytes_sent,
                cache_status: r
                    .cache
                    .as_ref()
                    .map(|c| c.cache_status)
                    .or(r.detail.map(|d| d.cache_status))
                    .unwrap_or(0),
                upstream_tries: request_upstream_tries(r),
                done_flags: r.detail.map(|d| d.flags).unwrap_or(0),
                busy_defer_ms: r.detail.map(|d| d.busy_defer_ms).unwrap_or(0),
                aio_wait_ms: r.detail.map(|d| d.aio_wait_ms).unwrap_or(0),
                messages: r.messages.len().min(u16::MAX as usize) as u16,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// System-tab aggregate series
// ---------------------------------------------------------------------------

/// One completed `REQUEST_DONE`, capture-relative seconds.
#[derive(Debug, Clone, Copy)]
pub struct NginxCompletion {
    pub ts_s: f64,
    pub status: u16,
    pub bytes_sent: u64,
}

/// Named `[t, y]` series for one System-tab stack.
#[derive(Debug, Clone)]
pub struct NginxNamedSeries {
    pub label: String,
    pub data: Vec<[f64; 2]>,
}

/// One completed request's START→DONE interval, capture-relative seconds.
#[derive(Debug, Clone, Copy)]
pub struct NginxLatencySample {
    pub ts_s: f64,
    pub latency_s: f64,
    pub bytes_sent: Option<u64>,
}

/// Response bodies below this (bytes) are the small-object HTTP Duration
/// series; the rest share the other series.
pub const NGINX_SMALL_RESP_BYTES: u64 = 5_000;
pub const NGINX_LATENCY_SMALL_LABEL: &str = "Small";
pub const NGINX_LATENCY_LARGE_LABEL: &str = "Large";

/// Completions used by the System-tab HTTP plots. Rates and latency
/// are binned at render time into a fixed 250 bins over the visible
/// window (same count as the other stacked System plots). When the
/// capture carries `NGINX_SAMPLE` events, `samples` is the preferred
/// source for the aggregate rate plots and `completions` still feeds
/// the request-list views. `latencies` is START→DONE per completed
/// request (binned as a mean at render time).
#[derive(Debug, Clone, Default)]
pub struct NginxAggData {
    pub completions: Vec<NginxCompletion>,
    pub samples: Vec<NginxSamplePoint>,
    pub latencies: Vec<NginxLatencySample>,
    /// Problem events (stalls, disk, TLS, upstream failures), sorted.
    pub diag: Vec<NginxDiagEvent>,
}

/// Kinds of nginx problem event plotted on the System tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NginxDiagKind {
    WorkerStall,
    AioSyncRead,
    SendfileBusyDefer,
    SendfileError,
    FileOpenFail,
    TlsFail,
    UpstreamFail,
}

impl NginxDiagKind {
    pub const ALL: [NginxDiagKind; 7] = [
        NginxDiagKind::WorkerStall,
        NginxDiagKind::AioSyncRead,
        NginxDiagKind::SendfileBusyDefer,
        NginxDiagKind::SendfileError,
        NginxDiagKind::FileOpenFail,
        NginxDiagKind::TlsFail,
        NginxDiagKind::UpstreamFail,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NginxDiagKind::WorkerStall => "Worker stalls",
            NginxDiagKind::AioSyncRead => "Blocking aio reads",
            NginxDiagKind::SendfileBusyDefer => "Sendfile EBUSY deferrals",
            NginxDiagKind::SendfileError => "Sendfile errors",
            NginxDiagKind::FileOpenFail => "File open failures",
            NginxDiagKind::TlsFail => "TLS handshake failures",
            NginxDiagKind::UpstreamFail => "Upstream attempt failures",
        }
    }

    /// Classify one decoded event, `None` for non-problem events.
    pub fn of(p: &NginxPayload) -> Option<Self> {
        Some(match p {
            NginxPayload::WorkerStall { .. } => NginxDiagKind::WorkerStall,
            NginxPayload::AioSyncFallback { .. } => NginxDiagKind::AioSyncRead,
            NginxPayload::SendfileBusyDefer { .. } => NginxDiagKind::SendfileBusyDefer,
            NginxPayload::SendfileError { .. } => NginxDiagKind::SendfileError,
            NginxPayload::FileOpenFail { .. } => NginxDiagKind::FileOpenFail,
            NginxPayload::TlsHandshakeFail(_) => NginxDiagKind::TlsFail,
            NginxPayload::UpstreamAttemptEnd { ft, .. } if *ft != 0 => NginxDiagKind::UpstreamFail,
            _ => return None,
        })
    }
}

/// One problem event, capture-relative seconds.
#[derive(Debug, Clone, Copy)]
pub struct NginxDiagEvent {
    pub ts_s: f64,
    pub kind: NginxDiagKind,
}

/// One worker's `NGINX_SAMPLE` interval, capture-relative seconds. `resp`
/// is completions by status class (1xx, 2xx, 3xx, 4xx, 5xx, other) and
/// `bytes_sent` the response bytes served during the interval.
#[derive(Debug, Clone, Copy)]
pub struct NginxSamplePoint {
    pub ts_s: f64,
    pub resp: [u64; 6],
    pub bytes_sent: u64,
}

/// Build the aggregate series from `REQUEST_DONE` completions,
/// `NGINX_SAMPLE` intervals, and per-request START→DONE latencies.
pub fn build_nginx_agg(prof: &NginxProfile, t0_us: u64) -> NginxAggData {
    let rel = |ts: u64| ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
    let mut completions: Vec<NginxCompletion> = prof
        .events
        .iter()
        .filter_map(|e| match &e.payload {
            NginxPayload::RequestDone {
                status, bytes_sent, ..
            } => Some(NginxCompletion {
                ts_s: rel(e.ts),
                status: *status,
                bytes_sent: *bytes_sent,
            }),
            _ => None,
        })
        .collect();
    let mut samples: Vec<NginxSamplePoint> = prof
        .events
        .iter()
        .filter_map(|e| match &e.payload {
            NginxPayload::Sample {
                resp_1xx,
                resp_2xx,
                resp_3xx,
                resp_4xx,
                resp_5xx,
                resp_other,
                bytes_sent,
                ..
            } => Some(NginxSamplePoint {
                ts_s: rel(e.ts),
                resp: [
                    *resp_1xx,
                    *resp_2xx,
                    *resp_3xx,
                    *resp_4xx,
                    *resp_5xx,
                    *resp_other,
                ],
                bytes_sent: *bytes_sent,
            }),
            _ => None,
        })
        .collect();
    completions.sort_by(|a, b| a.ts_s.total_cmp(&b.ts_s));
    samples.sort_by(|a, b| a.ts_s.total_cmp(&b.ts_s));
    let mut latencies: Vec<NginxLatencySample> = prof
        .requests
        .values()
        .filter_map(|r| {
            let done = r.done_ts?;
            Some(NginxLatencySample {
                ts_s: rel(done),
                latency_s: done.saturating_sub(r.start_ts) as f64 / 1_000_000.0,
                bytes_sent: r.bytes_sent,
            })
        })
        .collect();
    latencies.sort_by(|a, b| a.ts_s.total_cmp(&b.ts_s));
    let diag: Vec<NginxDiagEvent> = prof
        .events
        .iter()
        .filter_map(|e| {
            NginxDiagKind::of(&e.payload).map(|kind| NginxDiagEvent {
                ts_s: rel(e.ts),
                kind,
            })
        })
        .collect();
    NginxAggData {
        completions,
        samples,
        latencies,
        diag,
    }
}

/// Problem events/sec per kind in `[vx0, vx1]`, `nbins` bins. Kinds with
/// no events in the window are omitted.
pub fn nginx_diag_bins(
    agg: &NginxAggData,
    vx0: f64,
    vx1: f64,
    nbins: usize,
) -> Vec<(NginxDiagKind, Vec<[f64; 2]>)> {
    if nbins == 0 || vx1 <= vx0 {
        return Vec::new();
    }
    let width = (vx1 - vx0) / nbins as f64;
    let centers = bin_centers(vx0, vx1, nbins);
    NginxDiagKind::ALL
        .iter()
        .filter_map(|&kind| {
            let mut counts = vec![0u64; nbins];
            let mut any = false;
            for e in agg.diag.iter().filter(|e| e.kind == kind) {
                if let Some(i) = bin_index_in_window(e.ts_s, vx0, vx1, nbins) {
                    counts[i] += 1;
                    any = true;
                }
            }
            any.then(|| {
                let data = centers
                    .iter()
                    .zip(counts)
                    .map(|(&x, n)| [x, n as f64 / width])
                    .collect();
                (kind, data)
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Diagnostic summary (`elog_cli nginxdiag`)
// ---------------------------------------------------------------------------

/// `(label, count)` rows, most frequent first.
pub type NginxCounts = Vec<(String, u64)>;

fn sorted_counts(map: FxHashMap<String, u64>) -> NginxCounts {
    let mut v: NginxCounts = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// One worker stall, capture-relative seconds.
#[derive(Debug, Clone)]
pub struct NginxStall {
    pub ts_s: f64,
    pub provider_id: u16,
    pub tid: i32,
    pub stall_ms: u32,
    /// `file:line (func)`.
    pub at: String,
    /// `file:line` of the previous checkpoint.
    pub since: String,
}

/// Capture-wide rollup of the connection, TLS, disk, cache, upstream and
/// worker events.
#[derive(Debug, Clone, Default)]
pub struct NginxDiagSummary {
    pub connections: u64,
    pub close_reasons: NginxCounts,
    pub tls_handshakes: u64,
    pub tls_versions: NginxCounts,
    pub tls_ktls: u64,
    pub tls_reused: u64,
    /// Handshake time (accept → done), ms: (p50, p90, max).
    pub tls_handshake_ms: Option<(u32, u32, u32)>,
    pub tls_failures: NginxCounts,
    pub requests: u64,
    pub abnormal_requests: NginxCounts,
    pub cache_status: NginxCounts,
    pub cache_lock_waits: u64,
    pub cache_lock_wait_ms_max: u32,
    pub upstream_attempts: u64,
    pub upstream_failures: NginxCounts,
    pub busy_defers: u64,
    pub busy_defer_ms: u64,
    pub sendfile_errors: NginxCounts,
    pub open_failures: NginxCounts,
    pub aio_sync_reads: u64,
    pub aio_sync_read_us: u64,
    pub aio_sync_read_us_max: u32,
    pub stalls: Vec<NginxStall>,
    pub messages: NginxCounts,
}

fn percentile(sorted: &[u32], p: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

/// Strip a leading `file(line) ` source location so the same message from
/// different requests counts together.
fn message_key(msg: &str) -> &str {
    msg.split_once(") ")
        .filter(|(loc, _)| loc.contains('('))
        .map(|(_, rest)| rest)
        .unwrap_or(msg)
}

pub fn build_nginx_diag_summary(prof: &NginxProfile, t0_us: u64) -> NginxDiagSummary {
    let rel = |ts: u64| ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
    let mut out = NginxDiagSummary::default();
    let mut close: FxHashMap<String, u64> = FxHashMap::default();
    let mut versions: FxHashMap<String, u64> = FxHashMap::default();
    let mut tls_fail: FxHashMap<String, u64> = FxHashMap::default();
    let mut hs: Vec<u32> = Vec::new();
    for c in prof.conns.values() {
        out.connections += 1;
        if let Some(r) = &c.close_reason {
            *close.entry(r.clone()).or_default() += 1;
        }
        if let Some(t) = &c.tls {
            out.tls_handshakes += 1;
            *versions
                .entry(nginx_tls_version_name(t.version))
                .or_default() += 1;
            if t.flags & NGINX_TLS_KTLS_TX != 0 {
                out.tls_ktls += 1;
            }
            if t.flags & NGINX_TLS_REUSED != 0 {
                out.tls_reused += 1;
            }
            hs.push(t.handshake_ms);
        }
        if let Some(f) = &c.tls_fail {
            *tls_fail.entry(f.kind.clone()).or_default() += 1;
        }
    }
    hs.sort_unstable();
    if !hs.is_empty() {
        out.tls_handshake_ms = Some((
            percentile(&hs, 0.5),
            percentile(&hs, 0.9),
            *hs.last().unwrap(),
        ));
    }

    let mut abnormal: FxHashMap<String, u64> = FxHashMap::default();
    let mut cache: FxHashMap<String, u64> = FxHashMap::default();
    let mut ups_fail: FxHashMap<String, u64> = FxHashMap::default();
    let mut open: FxHashMap<String, u64> = FxHashMap::default();
    let mut msgs: FxHashMap<String, u64> = FxHashMap::default();
    for r in prof.requests.values() {
        out.requests += 1;
        if let Some(d) = r.detail {
            let bad =
                d.flags & (NGINX_DONE_TERMINATED | NGINX_DONE_TIMEDOUT | NGINX_DONE_CONN_ERROR);
            if bad != 0 {
                *abnormal.entry(nginx_done_flag_names(bad)).or_default() += 1;
            }
        }
        let name = request_cache_name(r);
        if !name.is_empty() {
            *cache.entry(name.to_string()).or_default() += 1;
        }
        if let Some(c) = &r.cache {
            if c.lock_wait_ms > 0 {
                out.cache_lock_waits += 1;
                out.cache_lock_wait_ms_max = out.cache_lock_wait_ms_max.max(c.lock_wait_ms);
            }
        }
        for a in &r.upstream {
            out.upstream_attempts += 1;
            if a.ft != 0 {
                *ups_fail.entry(nginx_upstream_ft_names(a.ft)).or_default() += 1;
            }
        }
        if let Some((status, errno, syscall)) = &r.open_fail {
            *open
                .entry(format!("{syscall} errno={errno} -> {status}"))
                .or_default() += 1;
        }
        for m in &r.messages {
            *msgs.entry(message_key(m).to_string()).or_default() += 1;
        }
    }

    let mut sf_err: FxHashMap<String, u64> = FxHashMap::default();
    for e in &prof.events {
        match &e.payload {
            NginxPayload::SendfileBusyDefer { delay_ms, .. } => {
                out.busy_defers += 1;
                out.busy_defer_ms += *delay_ms as u64;
            }
            NginxPayload::SendfileError {
                kind, sys_errno, ..
            } => {
                *sf_err
                    .entry(format!("{kind} errno={sys_errno}"))
                    .or_default() += 1;
            }
            NginxPayload::AioSyncFallback { read_us, .. } => {
                out.aio_sync_reads += 1;
                out.aio_sync_read_us += *read_us as u64;
                out.aio_sync_read_us_max = out.aio_sync_read_us_max.max(*read_us);
            }
            NginxPayload::WorkerStall {
                stall_ms,
                file,
                line,
                prev_file,
                prev_line,
                func,
            } => out.stalls.push(NginxStall {
                ts_s: rel(e.ts),
                provider_id: e.provider_id,
                tid: e.tid,
                stall_ms: *stall_ms,
                at: format!("{file}:{line} ({func})"),
                since: format!("{prev_file}:{prev_line}"),
            }),
            _ => {}
        }
    }
    out.stalls.sort_by(|a, b| a.ts_s.total_cmp(&b.ts_s));

    out.close_reasons = sorted_counts(close);
    out.tls_versions = sorted_counts(versions);
    out.tls_failures = sorted_counts(tls_fail);
    out.abnormal_requests = sorted_counts(abnormal);
    out.cache_status = sorted_counts(cache);
    out.upstream_failures = sorted_counts(ups_fail);
    out.sendfile_errors = sorted_counts(sf_err);
    out.open_failures = sorted_counts(open);
    out.messages = sorted_counts(msgs);
    out
}

/// Connections sorted by start (or end, for ones that predate the capture).
pub fn nginx_conn_list(prof: &NginxProfile) -> Vec<&NginxConnInfo> {
    let mut v: Vec<&NginxConnInfo> = prof.conns.values().collect();
    v.sort_by_key(|c| (c.start_ts.or(c.end_ts).unwrap_or(0), c.session_id));
    v
}

fn bin_index_in_window(ts_s: f64, vx0: f64, vx1: f64, nbins: usize) -> Option<usize> {
    if nbins == 0 || vx1 <= vx0 || ts_s < vx0 {
        return None;
    }
    if ts_s >= vx1 {
        return (ts_s == vx1).then_some(nbins - 1);
    }
    let i = ((ts_s - vx0) / (vx1 - vx0) * nbins as f64).floor() as usize;
    Some(i.min(nbins - 1))
}

fn bin_centers(vx0: f64, vx1: f64, nbins: usize) -> Vec<f64> {
    let w = (vx1 - vx0) / nbins as f64;
    (0..nbins).map(|i| vx0 + (i as f64 + 0.5) * w).collect()
}

/// Mean of `y` in each bin of `[vx0, vx1]`. Empty bins are 0, matching
/// the other nginx stacked series.
pub fn nginx_mean_bins(
    samples: impl IntoIterator<Item = (f64, f64)>,
    vx0: f64,
    vx1: f64,
    nbins: usize,
) -> Vec<[f64; 2]> {
    if nbins == 0 || vx1 <= vx0 {
        return Vec::new();
    }
    let mut sums = vec![0.0; nbins];
    let mut counts = vec![0u64; nbins];
    for (ts, y) in samples {
        if let Some(i) = bin_index_in_window(ts, vx0, vx1, nbins) {
            sums[i] += y;
            counts[i] += 1;
        }
    }
    bin_centers(vx0, vx1, nbins)
        .into_iter()
        .zip(sums.into_iter().zip(counts))
        .map(|(x, (sum, n))| [x, if n == 0 { 0.0 } else { sum / n as f64 }])
        .collect()
}

/// Mean START→DONE latency in milliseconds, `nbins` bins over `[vx0, vx1]`.
/// Two series: bodies `< 5K` bytes vs the rest.
pub fn nginx_latency_mean_bins(
    agg: &NginxAggData,
    vx0: f64,
    vx1: f64,
    nbins: usize,
) -> Vec<NginxNamedSeries> {
    let ms = |s: &NginxLatencySample| (s.ts_s, s.latency_s * 1000.0);
    let small = nginx_mean_bins(
        agg.latencies
            .iter()
            .filter(|s| s.bytes_sent.is_some_and(|b| b < NGINX_SMALL_RESP_BYTES))
            .map(ms),
        vx0,
        vx1,
        nbins,
    );
    let rest = nginx_mean_bins(
        agg.latencies
            .iter()
            .filter(|s| !s.bytes_sent.is_some_and(|b| b < NGINX_SMALL_RESP_BYTES))
            .map(ms),
        vx0,
        vx1,
        nbins,
    );
    let mut out = Vec::new();
    if small.iter().any(|p| p[1] > 0.0) {
        out.push(NginxNamedSeries {
            label: NGINX_LATENCY_SMALL_LABEL.into(),
            data: small,
        });
    }
    if rest.iter().any(|p| p[1] > 0.0) {
        out.push(NginxNamedSeries {
            label: NGINX_LATENCY_LARGE_LABEL.into(),
            data: rest,
        });
    }
    out
}

/// Completed responses/sec in `[vx0, vx1]`, `nbins` bins, one series per
/// status class (`2xx`, `4xx`, …). `NGINX_SAMPLE` intervals, when present,
/// are the source (they already carry class counts).
pub fn nginx_status_bins(
    agg: &NginxAggData,
    vx0: f64,
    vx1: f64,
    nbins: usize,
) -> Vec<NginxNamedSeries> {
    if !agg.samples.is_empty() {
        return nginx_sample_status_bins(&agg.samples, vx0, vx1, nbins);
    }
    let completions = &agg.completions;
    if nbins == 0 || vx1 <= vx0 || completions.is_empty() {
        return Vec::new();
    }
    let width = (vx1 - vx0) / nbins as f64;
    if width <= 0.0 {
        return Vec::new();
    }
    let centers = bin_centers(vx0, vx1, nbins);
    let mut keys: Vec<NginxStatusClass> = completions
        .iter()
        .filter_map(|c| NginxStatusClass::of(c.status))
        .collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .map(|class| {
            let mut counts = vec![0u64; nbins];
            for c in completions {
                if NginxStatusClass::of(c.status) != Some(class) {
                    continue;
                }
                if let Some(i) = bin_index_in_window(c.ts_s, vx0, vx1, nbins) {
                    counts[i] += 1;
                }
            }
            NginxNamedSeries {
                label: class.label().to_string(),
                data: centers
                    .iter()
                    .zip(&counts)
                    .map(|(&x, &n)| [x, n as f64 / width])
                    .collect(),
            }
        })
        .collect()
}

/// Per-class requests/sec from `NGINX_SAMPLE` intervals, summing all
/// workers into each bin. Classes with no completions in the window are
/// omitted.
fn nginx_sample_status_bins(
    samples: &[NginxSamplePoint],
    vx0: f64,
    vx1: f64,
    nbins: usize,
) -> Vec<NginxNamedSeries> {
    if nbins == 0 || vx1 <= vx0 {
        return Vec::new();
    }
    let width = (vx1 - vx0) / nbins as f64;
    if width <= 0.0 {
        return Vec::new();
    }
    let centers = bin_centers(vx0, vx1, nbins);
    let labels = ["1xx", "2xx", "3xx", "4xx", "5xx", "other"];
    labels
        .iter()
        .enumerate()
        .filter_map(|(cls, label)| {
            let mut counts = vec![0u64; nbins];
            let mut any = 0u64;
            for s in samples {
                if let Some(i) = bin_index_in_window(s.ts_s, vx0, vx1, nbins) {
                    counts[i] += s.resp[cls];
                    any += s.resp[cls];
                }
            }
            if any == 0 {
                return None;
            }
            Some(NginxNamedSeries {
                label: label.to_string(),
                data: centers
                    .iter()
                    .zip(&counts)
                    .map(|(&x, &n)| [x, n as f64 / width])
                    .collect(),
            })
        })
        .collect()
}

/// Completion `bytes_sent` in `[vx0, vx1]` as Gb/s, `nbins` bins.
/// `NGINX_SAMPLE` intervals, when present, are the source.
pub fn nginx_bytes_gbps_bins(
    agg: &NginxAggData,
    vx0: f64,
    vx1: f64,
    nbins: usize,
) -> Vec<[f64; 2]> {
    if nbins == 0 || vx1 <= vx0 {
        return Vec::new();
    }
    let width = (vx1 - vx0) / nbins as f64;
    if width <= 0.0 {
        return Vec::new();
    }
    let mut bytes = vec![0u64; nbins];
    if !agg.samples.is_empty() {
        for s in &agg.samples {
            if let Some(i) = bin_index_in_window(s.ts_s, vx0, vx1, nbins) {
                bytes[i] = bytes[i].saturating_add(s.bytes_sent);
            }
        }
    } else {
        if agg.completions.is_empty() {
            return Vec::new();
        }
        for c in &agg.completions {
            if let Some(i) = bin_index_in_window(c.ts_s, vx0, vx1, nbins) {
                bytes[i] = bytes[i].saturating_add(c.bytes_sent);
            }
        }
    }
    bin_centers(vx0, vx1, nbins)
        .into_iter()
        .zip(bytes)
        .map(|(x, n)| [x, (n as f64 / width) * super::BYTES_PER_S_TO_GBPS])
        .collect()
}

/// Distinct `nginx` provider ids in this capture (one per worker process).
pub fn nginx_provider_ids(prof: &NginxProfile) -> Vec<u16> {
    let mut ids = FxHashSet::default();
    for e in &prof.events {
        ids.insert(e.provider_id);
    }
    for (pid, _, _) in prof.requests.keys() {
        ids.insert(*pid);
    }
    let mut v: Vec<u16> = ids.into_iter().collect();
    v.sort_unstable();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts_s: f64, resp: [u64; 6], bytes_sent: u64) -> NginxSamplePoint {
        NginxSamplePoint {
            ts_s,
            resp,
            bytes_sent,
        }
    }

    #[test]
    fn samples_bin_per_class_and_bytes() {
        let agg = NginxAggData {
            completions: vec![],
            samples: vec![
                sample(1.0, [0, 10, 0, 2, 0, 0], 1000),
                sample(1.0, [0, 5, 0, 1, 0, 0], 500), // second worker, same interval
                sample(2.0, [0, 20, 0, 0, 1, 0], 2000),
            ],
            latencies: vec![],
            diag: vec![],
        };
        // 10 bins over 0-10s: each sample lands in its own 1s bin.
        let series = nginx_status_bins(&agg, 0.0, 10.0, 10);
        let get = |label: &str| series.iter().find(|s| s.label == label).unwrap();
        assert_eq!(get("2xx").data[1][1], 15.0); // both workers summed
        assert_eq!(get("2xx").data[2][1], 20.0);
        assert_eq!(get("4xx").data[1][1], 3.0);
        assert_eq!(get("5xx").data[2][1], 1.0);
        assert!(series.iter().all(|s| s.label != "1xx"));

        let gbps = nginx_bytes_gbps_bins(&agg, 0.0, 10.0, 10);
        let expect = 1500.0 * super::super::BYTES_PER_S_TO_GBPS;
        assert!((gbps[1][1] - expect).abs() < 1e-12);
    }

    #[test]
    fn completions_used_when_no_samples() {
        let agg = NginxAggData {
            completions: vec![
                NginxCompletion {
                    ts_s: 1.0,
                    status: 200,
                    bytes_sent: 100,
                },
                NginxCompletion {
                    ts_s: 1.5,
                    status: 404,
                    bytes_sent: 50,
                },
            ],
            samples: vec![],
            latencies: vec![],
            diag: vec![],
        };
        let series = nginx_status_bins(&agg, 0.0, 10.0, 10);
        assert_eq!(series.len(), 2);
        assert!(series.iter().any(|s| s.label == "2xx"));
        assert!(series.iter().any(|s| s.label == "4xx"));
    }

    fn req(
        pid: u16,
        sid: u64,
        request: u32,
        start_us: u64,
        done_us: Option<u64>,
        status: Option<u16>,
    ) -> NginxRequestInfo {
        NginxRequestInfo {
            provider_id: pid,
            session_id: sid,
            request,
            method: "GET".into(),
            uri: "/".into(),
            range_start: -1,
            range_end: -1,
            start_ts: start_us,
            headers_ts: None,
            done_ts: done_us,
            status,
            content_length: None,
            bytes_sent: done_us.map(|_| 100),
            event_count: if done_us.is_some() { 3 } else { 1 },
            ..NginxRequestInfo::new(pid, sid, request, start_us)
        }
    }

    fn profile_with(reqs: Vec<NginxRequestInfo>) -> NginxProfile {
        NginxProfile {
            requests: reqs
                .into_iter()
                .map(|r| ((r.provider_id, r.session_id, r.request), r))
                .collect(),
            conns: Default::default(),
            events: vec![],
        }
    }

    #[test]
    fn counts_rollup_by_session() {
        let prof = profile_with(vec![
            req(1, 7, 1, 1_000_000, Some(1_100_000), Some(200)),
            req(1, 7, 2, 2_000_000, Some(2_100_000), Some(404)),
            req(2, 9, 1, 500_000, None, None),
        ]);
        let counts = nginx_session_counts(&prof);
        assert_eq!(counts.len(), 2);
        let c7 = counts[&7];
        assert_eq!(c7.requests, 2);
        assert_eq!(c7.event_count, 6);
        assert_eq!(c7.bytes_sent, 200);
        assert_eq!(c7.first_ts, 1_000_000);
        assert_eq!(c7.last_ts, 2_100_000);
        assert_eq!(counts[&9].requests, 1);
    }

    #[test]
    fn spans_share_lane_when_serial() {
        let prof = profile_with(vec![
            req(1, 7, 2, 2_000_000, Some(2_100_000), Some(200)),
            req(1, 7, 1, 1_000_000, Some(1_100_000), Some(200)),
        ]);
        let spans = nginx_session_spans(&prof, 7, 0);
        assert_eq!(spans.len(), 2);
        assert!(spans.iter().all(|s| s.lane == 0));
        assert_eq!(spans[0].request, 1);
        assert!((spans[0].t_start - 1.0).abs() < 1e-9);
        assert!((spans[1].t_end.unwrap() - 2.1).abs() < 1e-9);
    }

    #[test]
    fn spans_stack_when_concurrent() {
        let prof = profile_with(vec![
            req(1, 7, 1, 1_000_000, Some(3_000_000), Some(200)),
            req(1, 7, 2, 2_000_000, Some(2_500_000), Some(200)),
            req(1, 7, 3, 2_600_000, None, None),
        ]);
        let spans = nginx_session_spans(&prof, 7, 0);
        assert_eq!(spans.iter().map(|s| s.lane).collect::<Vec<_>>(), [0, 1, 1]);
        // The open request keeps its lane and has no end.
        assert_eq!(spans[2].t_end, None);
    }

    #[test]
    fn merge_counts_and_stub_rows() {
        let prof = profile_with(vec![
            req(1, 7, 1, 1_000_000, Some(1_100_000), Some(200)),
            req(1, 9, 1, 500_000, Some(600_000), Some(200)),
        ]);
        let mut sessions = Sessions::default();
        sessions.insert(9, vec![]);
        let mut summaries = vec![super::super::session_summary_for(9, &sessions[&9])];
        merge_nginx_session_summaries(&mut summaries, &sessions, &prof);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].nginx_requests, 1);
        let stub = summaries.iter().find(|s| s.session_id == 7).unwrap();
        assert_eq!(stub.nginx_requests, 1);
        assert_eq!(stub.total_bytes_sent, 100);
        assert!((stub.duration_sec - 0.1).abs() < 1e-9);
    }

    #[test]
    fn latency_mean_bins_averages_per_bucket() {
        let prof = profile_with(vec![
            req(1, 7, 1, 1_000_000, Some(1_100_000), Some(200)),
            req(1, 7, 2, 2_000_000, Some(2_300_000), Some(200)),
            req(1, 7, 3, 3_000_000, None, None),
        ]);
        let agg = build_nginx_agg(&prof, 0);
        assert_eq!(agg.latencies.len(), 2);
        let series = nginx_latency_mean_bins(&agg, 0.0, 10.0, 10);
        let small = series
            .iter()
            .find(|s| s.label == NGINX_LATENCY_SMALL_LABEL)
            .unwrap();
        assert_eq!(small.data.len(), 10);
        assert!((small.data[1][1] - 100.0).abs() < 1e-9);
        assert!((small.data[2][1] - 300.0).abs() < 1e-9);
        assert_eq!(small.data[0][1], 0.0);
        assert!(series.iter().all(|s| s.label != NGINX_LATENCY_LARGE_LABEL));
    }

    #[test]
    fn latency_mean_bins_split_small_and_rest() {
        let mut small = req(1, 7, 1, 1_000_000, Some(1_100_000), Some(200));
        small.bytes_sent = Some(100);
        let mut large = req(1, 7, 2, 2_000_000, Some(2_300_000), Some(200));
        large.bytes_sent = Some(50_000);
        let agg = build_nginx_agg(&profile_with(vec![small, large]), 0);
        let series = nginx_latency_mean_bins(&agg, 0.0, 10.0, 10);
        let get = |label: &str| series.iter().find(|s| s.label == label).unwrap();
        assert!((get(NGINX_LATENCY_SMALL_LABEL).data[1][1] - 100.0).abs() < 1e-9);
        assert_eq!(get(NGINX_LATENCY_SMALL_LABEL).data[2][1], 0.0);
        assert_eq!(get(NGINX_LATENCY_LARGE_LABEL).data[1][1], 0.0);
        assert!((get(NGINX_LATENCY_LARGE_LABEL).data[2][1] - 300.0).abs() < 1e-9);
    }
}
