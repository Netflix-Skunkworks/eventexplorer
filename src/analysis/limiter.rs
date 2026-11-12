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

/// Classification of what was limiting the sender during a time bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limiter {
    /// Inflight stayed pressed against the congestion window during
    /// the bucket: the peak came within `CWND_SATURATION_MARGIN_BYTES`
    /// (~1 MSS) of `cwnd` *and* the trough never fell below
    /// `cwnd × SUSTAINED_PIN_RATIO`. The two-sided check separates
    /// "truly cwnd-bound" (instant-refill flows whose post-ACK dips
    /// stay near cwnd) from "tangentially grazing cwnd at the top of
    /// a paced ramp" (sawtoothed inflight that drops to a fraction
    /// of cwnd between cycles -- attributed to `Pacing` instead).
    Cwnd,
    /// Peer-advertised RWND was the tighter of the two windows:
    /// inflight was pinned against `peer_rwnd < cwnd`, meaning the
    /// receiver's buffer — not the congestion window — capped throughput.
    Rwnd,
    /// The send rate was gated by the stack rather than the congestion
    /// or flow-control window. Two signals trigger this:
    ///
    /// 1. **Explicit pacing**: at least one `EVT_PACING_CALC[_V2]` event
    ///    carried a non-zero `slot` or an explicit cap (`cap_source != NONE`),
    ///    *and* there was a non-trivial unsent backlog. This covers rack/bbr
    ///    pacing decisions where the kernel computed a wait interval.
    ///
    /// 2. **Burst mitigation / ACK-clocking**: the kernel had a backlog and
    ///    was actively sending (`had_send`), but inflight never approached the
    ///    binding window (neither cwnd nor rwnd was saturated). Without rack
    ///    pacing events the stack is still rate-limiting sends through its
    ///    burst-mitigation logic — one TSO burst per ACK, spaced by the RTT
    ///    rather than by an explicit pacing delay. Behaviourally identical to
    ///    pacing from the bottleneck perspective: the sending rate is governed
    ///    by the stack, not by window exhaustion or application starvation.
    Pacing,
    /// The application isn't queuing bytes fast enough to keep the
    /// kernel's send buffer non-empty: at every event in the bucket
    /// we observed `sndbuf_used - inflight ≤ APP_LIMIT_BACKLOG_BYTES`
    /// (no meaningful unsent backlog). Two sub-shapes both land
    /// here:
    ///
    ///   * Bucket has sends but inflight isn't pinned to the
    ///     window — the kernel was draining what little the app
    ///     gave it but couldn't keep cwnd full.
    ///
    ///   * Bucket has *no* sends but `max_inflight` is non-trivial
    ///     (typically the post-`TH_PUSH` drain after a sendfile
    ///     chunk completed; see `tcp_output.c:1129–1131`). ACKs
    ///     are clocking inflight down while the app hasn't issued
    ///     the next `send()`/`sendfile()` — the canonical
    ///     "between-chunks pause" pattern on CDN flows.
    ///
    /// Sessions without USER_SEND events stay in the legacy
    /// "App = unexplained non-pinned non-paced bucket" mode (no
    /// backlog signal to disambiguate), preserving the pre-buffer-
    /// tracking behaviour for older captures.
    App,
    /// Bucket had no sends *and* nothing meaningful on the wire
    /// No sends in the bucket, and the kernel had a non-trivial
    /// unsent backlog (`sndbuf_used - inflight > APP_LIMIT_BACKLOG_BYTES`)
    /// at some point. The connection is NOT idle — data was queued
    /// and the kernel could have sent it — but no OUT event fired.
    /// The most common cause is an event-free 10 ms bucket that falls
    /// between two ACK-triggered sends (the previous ACK fired at the
    /// end of the last bucket; the next fires at the start of the
    /// next). We cannot name the bottleneck, so we label it
    /// `Unknown` and render it with a subtle visual mark to
    /// distinguish it from true idle.
    Unknown,
    /// No sends, no backlog, and inflight is negligible: the
    /// connection is genuinely parked. Pipe carried no data, no
    /// data was waiting, and nothing was in flight. These spans
    /// are left un-shaded (invisible background) in the GUI to
    /// reduce noise; the separate `idle_spans` overlay captures
    /// them when they exceed 100 ms.
    Idle,
}

impl Limiter {
    pub fn short(&self) -> &'static str {
        match self {
            Limiter::Cwnd => "cwnd",
            Limiter::Rwnd => "rwnd",
            Limiter::Pacing => "pacing",
            Limiter::App => "app",
            Limiter::Unknown => "unknown",
            Limiter::Idle => "idle",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LimiterSpan {
    pub t_start: f64,
    pub t_end: f64,
    pub class: Limiter,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LimiterSummary {
    pub pacing_pct: f64,
    pub cwnd_pct: f64,
    /// Time spent peer-rwnd-limited. Stays `0.0` for sessions without
    /// a `CONN_PARAMS_V2` event (the classifier needs `snd_wscale` to
    /// reconstruct the byte-valued peer window — see the comment on
    /// `EventPayload::ConnParamsV2`). Otherwise reports the fraction
    /// of buckets whose inflight was pinned against `peer_rwnd <
    /// cwnd`.
    pub rwnd_pct: f64,
    pub app_pct: f64,
    /// Buckets where data was queued but no send fired — connection
    /// not idle but bottleneck unknown. Rendered as "Unknown" to
    /// distinguish from truly idle time (`idle_pct`).
    pub unknown_pct: f64,
    /// Truly idle time: no sends, no backlog, negligible inflight.
    /// The pipe was genuinely parked.
    pub idle_pct: f64,
}

impl LimiterSummary {
    /// Compact one-liner used in the GUI header and the CLI summary.
    /// Elides classes whose percentage rounds to zero.
    pub fn compact(&self) -> String {
        let entries = [
            ("Pacing", self.pacing_pct),
            ("CWND", self.cwnd_pct),
            ("RWND", self.rwnd_pct),
            ("App", self.app_pct),
            ("Unknown", self.unknown_pct),
            ("Idle", self.idle_pct),
        ];
        let parts: Vec<String> = entries
            .iter()
            .filter(|&&(_, p)| p.round() as i32 > 0)
            .map(|&(name, p)| format!("{name} {}%", p.round() as i32))
            .collect();
        if parts.is_empty() {
            String::new()
        } else {
            parts.join(" · ")
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pacing_pct == 0.0
            && self.cwnd_pct == 0.0
            && self.rwnd_pct == 0.0
            && self.app_pct == 0.0
            && self.unknown_pct == 0.0
            && self.idle_pct == 0.0
    }
}

/// Default bucket width for limiter classification (10 ms).
///
/// The bucket width sets the smallest time interval we're willing to
/// attribute a limiting cause to. Smaller → better cursor↔overlay
/// alignment and tighter separation between a brief TSO burst grazing
/// cwnd and a subsequent pacing-capped idle, at the cost of ~5× more
/// buckets per session (RLE compresses most of this back out for
/// long steady-state stretches). 10 ms is roughly the granularity at
/// which rack re-evaluates pacing, so we don't gain information by
/// going finer.
pub const LIMITER_BUCKET_US: u64 = 10_000;
/// Default idle-gap threshold (100 ms). Chosen to be roughly "noticeable
/// on a plot at typical zoom" — below that, normal inter-ACK gaps at
/// low RTT can look idle without actually being limiting.
pub const IDLE_GAP_US: u64 = 100_000;
/// Byte margin below `cwnd` that still counts as "cwnd-saturated" for
/// the purpose of `Limiter::Cwnd` classification. Roughly 1.3 × MSS —
/// i.e. "if inflight is within about one segment of cwnd, the sender
/// cannot place another MSS and is effectively cwnd-bound".
/// See the extended rationale on the CWND branch of `classify_bucket`
/// for why this is an absolute byte threshold instead of a percentage.
pub const CWND_SATURATION_MARGIN_BYTES: f64 = 2000.0;

/// Bytes-of-unsent-backlog threshold above which a pacing decision
/// counts as "pacing meaningfully gated data". Used by the limiter
/// classifier to distinguish:
///
/// * **Pacing-bound:** rack/bbr deferred a send while the app had
///   queued > this many unsent bytes. Pacing was the gate.
///
/// * **App-bound:** rack/bbr deferred a send but the buffer held
///   only inflight bytes (`sndbuf_used ≈ inflight`). Pacing fired
///   vacuously — there was nothing to send anyway, the app is the
///   actual constraint.
///
/// Set to one Ethernet MSS (1500 B) on the principle that a backlog
/// smaller than a single segment can't be meaningfully gated by
/// pacing. Loosely coupled to the typical MSS on this fleet; if a
/// session's MSS is materially smaller (e.g. tunnels) the threshold
/// will admit slightly more events as pacing — false-positive direction,
/// not silently misclassify.
pub const APP_LIMIT_BACKLOG_BYTES: f64 = 1_500.0;

/// Minimum bucket-trough inflight, expressed as a fraction of the
/// binding window, required for a bucket to be classified as
/// `Limiter::Cwnd` / `Limiter::Rwnd`. The peak alone isn't enough:
/// a bucket whose peak briefly grazes `cwnd` at the top of a paced
/// ramp (e.g. rack sending one MSS every few ms until inflight just
/// touches the window, then idling until the next ACK) looks
/// momentarily window-pinned but is actually pacing-bound. Requiring
/// the trough to also stay close to the window distinguishes "truly
/// pinned" (instant-refill flows where post-ACK dips never go far
/// below cwnd) from "tangentially touching" (paced sawtooth that
/// drops to a fraction of cwnd between cycles).
///
/// 0.75 was chosen so that a bucket at steady-state cwnd-saturation,
/// where each ACK opens up one segment and the kernel refills it
/// immediately, still passes (post-ACK dip is roughly one TSO segment
/// — a few percent of a typical cwnd). The threshold is intentionally
/// loose: false negatives (mis-classifying genuine cwnd-pinned as
/// pacing/app) are worse than false positives, and any flow whose
/// trough drops below 75 % of the window has demonstrably had room
/// it didn't fill. Tune downward (toward 0.6) if a future stack
/// shows dips deeper than this in true cwnd-bound steady state.
pub const SUSTAINED_PIN_RATIO: f64 = 0.75;

/// Cap on the total number of RLE'd limiter spans we will keep, matching
/// the behaviour of `PacingCapSpan` for very long sessions.  When the
/// count exceeds this, adjacent small spans are coalesced into the
/// enclosing run.
///
/// Sized for `LIMITER_BUCKET_US = 10 ms`: a 600 s session that
/// oscillates class every single bucket would produce 60 000 spans,
/// so 8 000 gives enough headroom that realistic workloads never hit
/// the cap while still bounding worst-case memory.
pub const MAX_LIMITER_SPANS: usize = 8000;

#[derive(Debug, Clone, Copy)]
pub struct PacingCapSpan {
    pub t_start: f64,
    pub t_end: f64,
    pub cap: u8,
}

/// Classify each `bucket_us`-wide window as `Cwnd` / `Rwnd` / `Pacing` /
/// `App` / `Unknown`, then run-length-encode adjacent same-class buckets
/// into spans.  Returns both the span list and the time-weighted summary.
///
/// Heuristics:
/// - CwndLimited  — inflight ≥ cwnd-margin, and cwnd ≤ peer_rwnd
/// - RwndLimited  — inflight ≥ peer_rwnd-margin, and peer_rwnd < cwnd
///   (only when `peer_rwnd_samples` is non-empty — i.e. the session
///   has a `CONN_PARAMS_V2` and we could decode the wscale-corrected
///   window)
/// - PacingLimited — not window-limited; `PACING_CALC_V2.cap_source`
///   is non-NONE
/// - AppLimited    — none of the above; had sends but neither
///   window-limited nor pacing-capped
/// - Unknown       — bucket had no sends (rendered as "Idle")
///
/// `peer_rwnd_samples` is `(ts_us, peer_rwnd_bytes)` (the kernel
/// already shifted before logging — see the comment above where the
/// caller builds this slice). An empty slice disables RWND
/// classification entirely.
#[allow(clippy::too_many_arguments)]
pub fn build_limiter_spans(
    events: &[Event],
    start_us: u64,
    end_us: u64,
    t0: u64,
    bucket_us: u64,
    pacing_cap_samples: &[(f64, u8)],
    peer_rwnd_samples: &[(u64, u64)],
    max_spans: usize,
) -> (Vec<LimiterSpan>, LimiterSummary) {
    if end_us <= start_us || bucket_us == 0 {
        return (Vec::new(), LimiterSummary::default());
    }

    let num_buckets = ((end_us - start_us).div_ceil(bucket_us)) as usize;
    if num_buckets == 0 {
        return (Vec::new(), LimiterSummary::default());
    }
    let has_pacing_calc = events
        .iter()
        .any(|e| matches!(e.payload, EventPayload::PacingCalc { .. }));

    // State trackers carried forward between buckets. Peer RWND is
    // deliberately not tracked — see the comment in `classify_bucket`.
    // `bw_est` from PACING_CALC_V2 is likewise not consulted because
    // it's derived from recent observed goodput, which makes any
    // "bucket_bps ≈ bw_est ⇒ pacing-limited" heuristic circular.
    //
    // The "pacing was active in this bucket" flag intentionally does
    // *not* carry across bucket boundaries: a pacing decision is
    // meaningful only for the bucket in which it was made, and
    // letting it stick would cause long idle stretches that follow
    // an early pacing event to be mis-labeled pacing-limited.
    let mut cur_cwnd: f64 = 0.0; // bytes
    let mut cur_inflight: f64 = 0.0; // bytes
    let mut cur_pacing_cap: u8 = PACING_CAP_NONE;
    // Estimated bytes in the send socket buffer, in bytes. Includes
    // both sent-but-unacked and unsent — i.e. `sb_used` in FreeBSD
    // (`sbused(&so->so_snd)`), so `cur_sndbuf_used - cur_inflight`
    // is the unsent backlog the kernel still has to push.
    //
    // Updated authoritatively from `EVT_USER_SEND.sndbuf_used`,
    // grown by `EVT_SENDFILE.length` (sendfile fires post-iodone, so
    // the bytes are already queued by the event time), and shrunk
    // by `EVT_ACK.acked` (ACK frees acked bytes from the buffer).
    // OUT does NOT change it: data moves from "unsent" to
    // "sent-unacked" but stays in the buffer until ACKed.
    //
    // Stays `None` until the first USER_SEND or SENDFILE event,
    // since we have no anchor before then. The classifier treats
    // `None` as "no backlog signal available, trust pacing as-is",
    // so legacy captures and the pre-handshake portion of every
    // session keep the prior behaviour.
    let mut cur_sndbuf_used: Option<f64> = None;
    // Peer-advertised RWND in bytes (scaled by `snd_wscale`). Stays
    // `None` for sessions without a `CONN_PARAMS_V2`, in which case
    // `classify_bucket` ignores RWND and falls back to cwnd-only
    // classification — preserving backwards compatibility with
    // pre-`_V2` captures.
    let mut cur_peer_rwnd: Option<f64> = if peer_rwnd_samples.is_empty() {
        None
    } else {
        Some(f64::INFINITY)
    };
    let mut rwnd_idx: usize = 0;

    let mut classes: Vec<Limiter> = Vec::with_capacity(num_buckets);

    let mut ev_idx: usize = 0;
    // Accumulators for bucket-local signals. We track both the peak
    // (for has-sends buckets — "did a burst reach cwnd?") and the
    // trough (for event-less / ACK-only buckets — "did inflight stay
    // pinned at cwnd throughout, or did an ACK free space the sender
    // never reclaimed?"). Both are seeded at the carried-over
    // `cur_inflight` at each bucket boundary, so they reflect the
    // state at the start of the bucket and then track any updates.
    let mut bucket_max_inflight: f64 = cur_inflight;
    let mut bucket_min_inflight: f64 = cur_inflight;
    // "Pacing was the gate at some point during this bucket". Set on
    // any PACING_CALC_V2 event — the kernel only emits the event
    // when it computed a non-zero `pacing_delay` (rack: ngen.c
    // line 14668), i.e. it told itself "wait this many µs before
    // the next send". That includes both ordinary
    // bw_est × pacing_gain pacing and explicit STARS/BW_RATE_CAP/
    // FIXED clamps, so it covers the dominant case (paced ramp-ups
    // between ACKs) that the V2 cap_source flag alone misses.
    let mut bucket_pacing_active = false;
    let mut bucket_had_send = false;
    // "Kernel had a non-trivial unsent backlog at some point in this
    // bucket": `cur_sndbuf_used - cur_inflight > APP_LIMIT_BACKLOG_BYTES`,
    // i.e. there was data sitting in the send buffer that hadn't yet
    // been pushed onto the wire. Sampled at every event after we
    // update either side of that subtraction, so a brief moment of
    // backlog anywhere in the bucket flips this true.
    //
    // Stays `false` for sessions that never see USER_SEND
    // (`cur_sndbuf_used` stays `None`), preserving the legacy
    // "no buffer signal → fall back to App for non-pinned, non-paced
    // buckets" behaviour for older captures.
    let mut bucket_had_backlog = false;
    // "Backlog that was present has since been exhausted": set when
    // `bucket_had_backlog` was already true (a real unsent backlog
    // existed) AND the kernel was actively sending (`bucket_had_send`)
    // AND the backlog subsequently dropped to ≤ APP_LIMIT_BACKLOG_BYTES.
    //
    // This separates two distinct patterns that both involve a large
    // `sndbuf_used` early in the bucket:
    //
    //   * Sustained burst-mitigation: backlog remains large
    //     throughout; inflight never reaches the window because the
    //     stack is ACK-clocking at sub-window rate. `had_drain` stays
    //     false → classify as Pacing.
    //
    //   * CDN/sendfile chunk drain: the kernel fills cwnd with a
    //     fresh chunk and then drains it; partway through the chunk
    //     the send buffer catches up to inflight (sndbuf_used ≈
    //     inflight in the GUI plot). `had_drain` flips true →
    //     classify as App (the bottleneck was data supply, not
    //     deliberate rate-limiting by the stack).
    //
    // The guard on `bucket_had_backlog` prevents the natural
    // PSH-tail → USER_SEND(570) transition (where the buffer briefly
    // shows 570 bytes ≈ inflight just before the next sendfile chunk
    // is queued) from spuriously triggering the drain signal, because
    // at that instant `bucket_had_backlog` is still false.
    let mut bucket_had_drain = false;

    let mut bucket_end = start_us + bucket_us;
    let mut bucket_start = start_us;

    while ev_idx < events.len() || bucket_start < end_us {
        // Advance the peer-rwnd timeline up to the end of this bucket.
        // Each sample is "the peer's window from this ts onward, until
        // a later sample supersedes it" — so the value carried into
        // the bucket is the most recent sample with `ts < bucket_end`.
        while rwnd_idx < peer_rwnd_samples.len() && peer_rwnd_samples[rwnd_idx].0 < bucket_end {
            cur_peer_rwnd = Some(peer_rwnd_samples[rwnd_idx].1 as f64);
            rwnd_idx += 1;
        }

        // Consume all events up to the end of this bucket.
        while ev_idx < events.len() && events[ev_idx].ts < bucket_end {
            let e = &events[ev_idx];
            match &e.payload {
                EventPayload::Cwnd { cwnd, .. } => {
                    cur_cwnd = *cwnd as f64;
                }
                EventPayload::Out { len, flags, .. } => {
                    if *len > 0 {
                        cur_inflight += *len as f64;
                        bucket_had_send = true;
                        if cur_inflight > bucket_max_inflight {
                            bucket_max_inflight = cur_inflight;
                        }
                    }
                    // Without PACING_CALC (TRACE on default captures),
                    // the OUT PACED bit stands in for its pacing gate.
                    if !has_pacing_calc && (*flags & TCP_FLAG_PACED) != 0 {
                        let has_backlog = match cur_sndbuf_used {
                            Some(s) => (s - cur_inflight) > APP_LIMIT_BACKLOG_BYTES,
                            None => true,
                        };
                        if has_backlog {
                            bucket_pacing_active = true;
                        }
                    }
                }
                EventPayload::Ack {
                    inflight, acked, ..
                } => {
                    // `Ack.inflight` is the PRE-ACK value (what was
                    // outstanding when the ACK was received), not
                    // post-ACK. The kernel emits the ACK first, then
                    // processes it, then the next PACING_CALC_V2
                    // reports the post-ACK inflight. Using the raw
                    // `inflight` field here would make every ACK
                    // leak `acked` bytes into our running total and
                    // cause unbounded upward drift over the session.
                    // Post-ACK inflight = pre-ACK - cumulatively
                    // ACKed bytes.
                    cur_inflight = (*inflight as f64 - *acked as f64).max(0.0);
                    // ACK frees acked bytes from the send buffer
                    // (`sbdrop_locked` in tcp_input on cumulative
                    // advance). Mirror that here so our buffer
                    // estimate doesn't drift upward over the
                    // session.
                    if let Some(s) = cur_sndbuf_used.as_mut() {
                        *s = (*s - *acked as f64).max(0.0);
                    }
                    // An ACK never raises inflight, so don't touch
                    // the bucket max — just track the drop for the
                    // min (needed by the no-sends CWND-pinned check).
                    if cur_inflight < bucket_min_inflight {
                        bucket_min_inflight = cur_inflight;
                    }
                }
                EventPayload::UserSend { sndbuf_used, .. } => {
                    // Authoritative buffer snapshot from the kernel
                    // (`sbused(&so->so_snd)` at the end of
                    // `tcp_usr_send`). Every `pr_send` path emits
                    // this — including the one nested inside
                    // `vn_sendfile()`'s loop — so sendfile flows
                    // get an authoritative reading per syscall too,
                    // and we don't need to derive sb_used from
                    // SENDFILE's `length` field (which is the
                    // syscall's lookahead arg, not bytes queued).
                    cur_sndbuf_used = Some(*sndbuf_used as f64);
                }
                EventPayload::PacingCalc {
                    method,
                    cap_source,
                    inflight,
                    slot,
                    ..
                } => {
                    // Pacing-bound requires both:
                    //
                    //   (a) Pacing was the gate, signalled either by
                    //       a non-zero `slot` (the kernel told itself
                    //       to wait this long before the next send)
                    //       or an explicit STARS / BW_RATE_CAP /
                    //       FIXED clamp is in force.
                    //
                    //       We deliberately do NOT cap the slot
                    //       magnitude. Per-segment pacing slots scale
                    //       with `MSS / pacing_rate`, so a 32 Mbps
                    //       flow with 5.8 KB TSO segments naturally
                    //       sees `slot ≈ 1.5 ms` — well above any
                    //       sub-millisecond cut. The "post-TSO-burst
                    //       slack" case the cap was originally trying
                    //       to filter out (cwnd-bound flow with a
                    //       large slot that never actually defers a
                    //       send because the next ACK reopens cwnd
                    //       first) is already handled upstream: that
                    //       flow's inflight stays sustained-pinned at
                    //       cwnd, so the `pinned` check in
                    //       classify_bucket returns `Cwnd` before we
                    //       ever consult `pacing_active_in_bucket`.
                    //
                    //   (b) There is at least `APP_LIMIT_BACKLOG_BYTES`
                    //       worth of unsent data the kernel could push
                    //       if pacing weren't holding it back. If
                    //       `sndbuf_used - inflight ≈ 0` the buffer
                    //       holds only sent-unacked bytes, so pacing
                    //       is firing vacuously -- there's nothing
                    //       for it to gate, and the real bottleneck
                    //       is the application supply rate. We treat
                    //       that case as App, not Pacing.
                    //
                    // (b) is skipped when we don't yet have a
                    // sndbuf estimate (no USER_SEND/SENDFILE seen on
                    // this session): falling back to (a)-only
                    // matches the pre-buffer-tracking behaviour for
                    // the early portion of the session and for
                    // legacy captures.
                    let pacing_gate = *slot > 0 || *cap_source != PACING_CAP_NONE;
                    let has_backlog = match cur_sndbuf_used {
                        Some(s) => (s - cur_inflight) > APP_LIMIT_BACKLOG_BYTES,
                        None => true,
                    };
                    if pacing_gate && has_backlog {
                        bucket_pacing_active = true;
                    }
                    if *method == 2 {
                        cur_pacing_cap = *cap_source;
                        // Authoritative inflight snapshot. Method 2
                        // (regular paced send decision) fires after
                        // every OUT and after every ACK rack acts
                        // on, so this is the ground-truth value;
                        // adopting it corrects any drift accumulated
                        // by our own OUT/ACK bookkeeping. Other
                        // methods (7/8/12/14) carry pacing
                        // information but their `inflight` field
                        // semantics are not as well-defined for our
                        // purposes, so we leave `cur_inflight`
                        // alone for them.
                        cur_inflight = *inflight as f64;
                        if cur_inflight > bucket_max_inflight {
                            bucket_max_inflight = cur_inflight;
                        }
                        if cur_inflight < bucket_min_inflight {
                            bucket_min_inflight = cur_inflight;
                        }
                    }
                }
                _ => {}
            }
            // Sample the buffer-backlog state after every event. We
            // only need to flip it true once — any moment of backlog
            // in the bucket is enough to rule out App-limited.
            if !bucket_had_backlog {
                if let Some(s) = cur_sndbuf_used {
                    if (s - cur_inflight) > APP_LIMIT_BACKLOG_BYTES {
                        bucket_had_backlog = true;
                    }
                }
            }
            // Drain detection: the backlog that was present has now
            // been exhausted. Only meaningful once `had_backlog` is
            // true (i.e. a real backlog was established first) and
            // the kernel has been actively sending. If the backlog
            // has since dropped to ≤ threshold — including going
            // negative in our accounting, which represents
            // `sndbuf_used ≈ inflight` in the plot — the send buffer
            // ran dry. Flag as a drain so classify_bucket can return
            // App instead of Pacing for the backlog fallback.
            if bucket_had_backlog && bucket_had_send && !bucket_had_drain {
                if let Some(s) = cur_sndbuf_used {
                    if (s - cur_inflight) <= APP_LIMIT_BACKLOG_BYTES {
                        bucket_had_drain = true;
                    }
                }
            }
            ev_idx += 1;
        }

        // Classify this bucket.
        let class = classify_bucket(
            bucket_max_inflight,
            bucket_min_inflight,
            cur_cwnd,
            cur_peer_rwnd,
            cur_pacing_cap,
            bucket_pacing_active,
            bucket_had_send,
            bucket_had_backlog,
            bucket_had_drain,
        );
        classes.push(class);

        // Reset bucket-local accumulators. `bucket_pacing_active`
        // starts fresh each bucket — see the block comment above.
        // Both peak and trough are reseeded at the carried-over
        // inflight so the next bucket can detect either an OUT that
        // pushes past cwnd (max) or an ACK that drops below cwnd
        // (min) from the same starting point.
        bucket_max_inflight = cur_inflight;
        bucket_min_inflight = cur_inflight;
        bucket_pacing_active = false;
        bucket_had_send = false;
        bucket_had_drain = false; // drain state is per-bucket, not carried over
                                  // Seed from the carried-over state so that buckets with no
                                  // events still inherit the known backlog. Without this, an
                                  // event-free bucket between two sends (e.g. an ACK+OUT pair
                                  // at the boundary) would reset backlog to false and be
                                  // mis-classified as App even when sndbuf >> inflight.
        bucket_had_backlog = cur_sndbuf_used
            .map(|s| (s - cur_inflight) > APP_LIMIT_BACKLOG_BYTES)
            .unwrap_or(false);

        bucket_start = bucket_end;
        bucket_end = bucket_end.saturating_add(bucket_us);
        if bucket_start >= end_us {
            break;
        }
    }
    // Fold pacing_cap_samples into buckets we may have advanced past
    // (no-op for well-formed event streams; guards against events
    // arriving slightly out of bucket order).
    let _ = pacing_cap_samples; // currently only referenced via the live stream above

    // RLE adjacent same-class buckets.
    let mut spans: Vec<LimiterSpan> = Vec::new();
    if classes.is_empty() {
        return (spans, LimiterSummary::default());
    }
    let to_rel = |ts: u64| -> f64 { (ts.saturating_sub(t0)) as f64 / USEC_IN_SEC };
    let mut run_start = start_us;
    let mut run_class = classes[0];
    for (i, &c) in classes.iter().enumerate().skip(1) {
        if c != run_class {
            let run_end = start_us + (i as u64) * bucket_us;
            spans.push(LimiterSpan {
                t_start: to_rel(run_start),
                t_end: to_rel(run_end),
                class: run_class,
            });
            run_start = run_end;
            run_class = c;
        }
    }
    let run_end_final = (start_us + (classes.len() as u64) * bucket_us).min(end_us);
    spans.push(LimiterSpan {
        t_start: to_rel(run_start),
        t_end: to_rel(run_end_final),
        class: run_class,
    });

    // If we blew past `max_spans` (pathological: a capture that
    // oscillates between classes every bucket), coalesce the shortest
    // adjacent pair repeatedly until we fit.  The resulting
    // coarser-grained timeline preserves the overall proportions that
    // the summary is built from.
    while spans.len() > max_spans {
        let mut min_idx = 0usize;
        let mut min_dur = f64::INFINITY;
        for (i, s) in spans.iter().enumerate() {
            let d = s.t_end - s.t_start;
            if d < min_dur {
                min_dur = d;
                min_idx = i;
            }
        }
        // Merge into the longer neighbour.
        let left_dur = if min_idx > 0 {
            spans[min_idx - 1].t_end - spans[min_idx - 1].t_start
        } else {
            f64::NEG_INFINITY
        };
        let right_dur = if min_idx + 1 < spans.len() {
            spans[min_idx + 1].t_end - spans[min_idx + 1].t_start
        } else {
            f64::NEG_INFINITY
        };
        if left_dur >= right_dur && min_idx > 0 {
            let t_end = spans[min_idx].t_end;
            spans[min_idx - 1].t_end = t_end;
            spans.remove(min_idx);
        } else if min_idx + 1 < spans.len() {
            let t_start = spans[min_idx].t_start;
            spans[min_idx + 1].t_start = t_start;
            spans.remove(min_idx);
        } else {
            break;
        }
    }

    // Time-weighted summary.
    let total_duration: f64 = spans.iter().map(|s| (s.t_end - s.t_start).max(0.0)).sum();
    let mut summary = LimiterSummary::default();
    if total_duration > 0.0 {
        let mut pacing_s = 0.0;
        let mut cwnd_s = 0.0;
        let mut rwnd_s = 0.0;
        let mut app_s = 0.0;
        let mut unknown_s = 0.0;
        let mut idle_s = 0.0;
        for s in &spans {
            let d = (s.t_end - s.t_start).max(0.0);
            match s.class {
                Limiter::Pacing => pacing_s += d,
                Limiter::Cwnd => cwnd_s += d,
                Limiter::Rwnd => rwnd_s += d,
                Limiter::App => app_s += d,
                Limiter::Unknown => unknown_s += d,
                Limiter::Idle => idle_s += d,
            }
        }
        summary.pacing_pct = pacing_s / total_duration * 100.0;
        summary.cwnd_pct = cwnd_s / total_duration * 100.0;
        summary.rwnd_pct = rwnd_s / total_duration * 100.0;
        summary.app_pct = app_s / total_duration * 100.0;
        summary.unknown_pct = unknown_s / total_duration * 100.0;
        summary.idle_pct = idle_s / total_duration * 100.0;
    }

    (spans, summary)
}

#[allow(clippy::too_many_arguments)]
pub fn classify_bucket(
    max_inflight: f64,
    min_inflight: f64,
    cwnd: f64,
    peer_rwnd: Option<f64>,
    _cur_cap: u8,
    pacing_active_in_bucket: bool,
    had_send: bool,
    had_backlog: bool,
    had_drain: bool,
) -> Limiter {
    // Window-limited: the sender was up against the binding window in
    // this bucket. The binding window is `min(cwnd, peer_rwnd)` — the
    // smaller of the two is the one TCP actually can't push past.
    // When `peer_rwnd` is unknown (legacy captures without
    // `CONN_PARAMS_V2`) we fall back to cwnd-only classification.
    //
    // We require *two* conditions for window-limitation, not one:
    //
    //   1. Peak (`max_inflight`) reached within `CWND_SATURATION_MARGIN_BYTES`
    //      of the binding window — the sender literally couldn't fit
    //      another MSS at some moment in this bucket.
    //
    //   2. Trough (`min_inflight`) stayed at or above
    //      `window × SUSTAINED_PIN_RATIO` — i.e. the sender was
    //      *sustainedly* pressed against the window, not merely
    //      tangent to it.
    //
    // The peak threshold is an absolute byte margin (≈ 1.3 × MSS)
    // because TCP's real "can I send?" question is "does another MSS
    // fit?", which doesn't scale with the window. The trough
    // threshold is a fraction because the question there is
    // qualitative ("did inflight decisively drop away from the
    // window during this bucket?"), and a fraction lets a paced
    // sawtooth that drops to 30 % of cwnd between cycles fall
    // through to pacing classification while a true cwnd-pinned
    // flow whose post-ACK dips never go far below cwnd stays
    // labelled cwnd.
    //
    // We split on `had_send`:
    //
    // * With sends: peak + trough rule above.
    //
    // * Without sends: we need inflight to be pinned at the window
    //   *throughout* the bucket — only `min_inflight >= margin`
    //   matters. (No peak to consider, since nothing was placed.)
    //   Without this branch, a pure-ACK bucket would inherit the
    //   previous bucket's carried-over max and mis-classify as
    //   window-limited; with it, a steady-state cwnd-bound flow
    //   whose 10 ms bucket happens to fall between an OUT that
    //   fills cwnd and the ACK that opens room is still labelled
    //   window-limited (no events fire, inflight stays pinned, both
    //   max and min equal the carried-over value).
    let effective_window = match (cwnd, peer_rwnd) {
        (c, Some(r)) if c > 0.0 && r > 0.0 => Some(c.min(r)),
        (c, Some(r)) if c > 0.0 => Some(c.min(r)),
        (c, _) if c > 0.0 => Some(c),
        (_, Some(r)) if r > 0.0 => Some(r),
        _ => None,
    };
    if let Some(window) = effective_window {
        let peak_margin = window - CWND_SATURATION_MARGIN_BYTES;
        let trough_floor = window * SUSTAINED_PIN_RATIO;
        let pinned = if had_send {
            max_inflight >= peak_margin && min_inflight >= trough_floor
        } else {
            min_inflight >= peak_margin
        };
        if pinned {
            // Decide cwnd vs rwnd by which one is the binding
            // constraint. RWND wins when we have a known peer_rwnd
            // strictly tighter than cwnd; everything else (RWND
            // unknown, or cwnd ≤ peer_rwnd) attributes to cwnd.
            return match peer_rwnd {
                Some(r) if cwnd > 0.0 && r < cwnd => Limiter::Rwnd,
                _ => Limiter::Cwnd,
            };
        }
    }

    // No sends in this bucket and inflight is not pinned against
    // the window. Three situations to distinguish:
    //
    //   * App-limited drain: the socket send buffer was empty
    //     (`had_backlog` is false), and the bucket only carries
    //     ACKs draining a non-trivial inflight while the app
    //     hasn't issued the next `send()`/`sendfile()`. The kernel
    //     literally has nothing to push — the app is the
    //     bottleneck. Indicated by `max_inflight > threshold` (data
    //     was still on the wire) combined with no backlog.
    //
    //   * Unknown (mystery gap): `had_backlog` is true — data WAS
    //     queued — but no send fired. The connection is not idle;
    //     the kernel chose not to push in this window. Common
    //     cause: event-free 10 ms bucket between two ACK-triggered
    //     sends. We don't know the bottleneck, so label `Unknown`
    //     and render with a subtle shade to distinguish from true
    //     idle.
    //
    //   * Truly idle: no backlog, negligible inflight, connection
    //     genuinely parked. Rendered as invisible background;
    //     the `idle_spans` overlay handles 100ms+ idle periods.
    if !had_send {
        if max_inflight > APP_LIMIT_BACKLOG_BYTES && !had_backlog {
            return Limiter::App;
        }
        if had_backlog {
            return Limiter::Unknown;
        }
        return Limiter::Idle;
    }

    // Pacing-limited: at least one PACING_CALC[_V2] event fired
    // during this bucket. The kernel only emits these events when
    // it computed a non-zero `pacing_delay` (rack:
    // `pacing_delay != 0` gate in `rack_log_pacing_delay_calc`),
    // i.e. it told itself "wait this many µs before the next
    // send" — which IS the "pacing was the gate" signal. This
    // catches both the dominant `bw_est × pacing_gain` paced-ramp
    // case (which carries `cap_source = NONE` but a non-zero slot)
    // and explicit STARS / BW_RATE_CAP / FIXED clamps. We do NOT
    // infer pacing-limitation from "actual rate ≈ bw_est", because
    // `bw_est` is itself derived from observed goodput and the
    // comparison would be circular.
    if pacing_active_in_bucket {
        return Limiter::Pacing;
    }

    // Backlog override: the kernel had unsent bytes sitting in the
    // socket buffer at some point in this bucket, so the application
    // clearly was not the bottleneck — *unless* the backlog was
    // subsequently drained.
    //
    // * If the peak *reached* the binding window we attribute to that
    //   window (cwnd or rwnd, smaller wins) — the window was the
    //   actual ceiling, even if inflight temporarily dipped below it.
    //
    // * If the peak *fell short* of the window AND the backlog was
    //   drained during the bucket (`had_drain`): the kernel exhausted
    //   its supply — all queued bytes became in-flight and there was
    //   nothing left to push. That is App-limited: the bottleneck was
    //   the data-supply rate, not a deliberate hold-back by the stack.
    //   Typical shape: CDN sendfile chunk — 1 MB queued, kernel drains
    //   it over ~60–90 ms, buffer hits `sndbuf_used ≈ inflight`.
    //
    // * If the peak *fell short* AND the backlog was NOT drained:
    //   classify as Pacing. The TCP stack was metering sends through
    //   burst-mitigation / ACK-clocking (one TSO burst per ACK,
    //   spaced by RTT) without emitting explicit rack PACING_CALC
    //   events. Behaviourally this is the same bottleneck as rack
    //   pacing: the stack — not the window, not the app — was the
    //   rate gate.
    if had_backlog {
        if let Some(window) = effective_window {
            let peak_margin = window - CWND_SATURATION_MARGIN_BYTES;
            if max_inflight >= peak_margin {
                return match peer_rwnd {
                    Some(r) if cwnd > 0.0 && r < cwnd => Limiter::Rwnd,
                    _ => Limiter::Cwnd,
                };
            }
        }
        return if had_drain {
            Limiter::App
        } else {
            Limiter::Pacing
        };
    }

    // Remaining case: the sender had data and was neither window-
    // pinned nor pacing-gated. In practice the dominant cause is
    // the application / remote client not queuing more bytes (e.g.
    // the client hasn't requested another chunk yet). We lack a
    // positive kernel-side signal to prove this, so this class also
    // serves as the catch-all we can't otherwise explain.
    Limiter::App
}

/// Collapse a stream of `(time_s, cap_source)` samples (in chronological
/// order) into contiguous spans representing "the pacing cap that was active
/// at each point in time".
///
/// Semantics: a cap value, once stamped by a PACING_CALC_V2 event, is
/// considered active from that event's timestamp until the next event that
/// stamps a *different* non-NONE cap (or until `session_end_s`).  `NONE`
/// events (pacing was unclamped on that decision) are ignored for span
/// boundaries — the last explicitly-named cap persists — so neighbouring cap
/// regions butt up against each other without gaps and a final clamp runs
/// all the way to the end of the session.
///
/// Sessions that never see a non-NONE cap produce an empty vector.
pub fn build_pacing_cap_spans(samples: &[(f64, u8)], session_end_s: f64) -> Vec<PacingCapSpan> {
    let mut spans = Vec::new();

    // Discard NONE samples so they do not split spans; the caller will carry
    // the previous cap through them.
    let filtered: Vec<(f64, u8)> = samples
        .iter()
        .copied()
        .filter(|&(_, c)| c != PACING_CAP_NONE)
        .collect();
    if filtered.is_empty() {
        return spans;
    }

    let mut run_start = filtered[0].0;
    let mut run_cap = filtered[0].1;
    for &(t, c) in &filtered[1..] {
        if c != run_cap {
            if t > run_start {
                spans.push(PacingCapSpan {
                    t_start: run_start,
                    t_end: t,
                    cap: run_cap,
                });
            }
            run_start = t;
            run_cap = c;
        }
    }

    // Extend the final run to the end of the session so the active cap is
    // shown right up to the last event.
    let final_end = session_end_s.max(filtered.last().map(|s| s.0).unwrap_or(run_start));
    if final_end > run_start {
        spans.push(PacingCapSpan {
            t_start: run_start,
            t_end: final_end,
            cap: run_cap,
        });
    }

    spans
}
