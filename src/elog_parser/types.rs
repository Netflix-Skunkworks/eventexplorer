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

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

/// Everything a full parse yields: the per-session map plus the global
/// provider streams (`hpts` STARS aggregate samples, per-connection
/// STARS_PACE samples flattened out of `Sessions`, `cpu` profile, `if`
/// profile, `tcp` periodic samples, `fanout_node` channel profile), the
/// file header, and the provider table. Returned by `parse_elog_core`
/// and the `parse_elog*` loaders.
pub type ParsedCapture = (
    Sessions,
    Vec<StarsAggSample>,
    Vec<StarsPaceSample>,
    CpuProfile,
    IfProfile,
    TcpSampleProfile,
    EllaProfile,
    NginxProfile,
    FileInfo,
    Vec<Provider>,
    SchemaCapture,
);

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// One file's contribution to a capture, in the order the files sit on
/// the merged time axis. A single-file load carries one entry; a rolled
/// set loaded through [`merge_captures`](crate::elog_parser::merge_captures)
/// carries one per file, so the GUI and CLI can show which file covers
/// which stretch of the timeline.
#[derive(Debug, Clone)]
pub struct CaptureSource {
    /// Display label — the file's basename, or the `host:path` spec for
    /// a remote load.
    pub name: String,
    /// This file's own header origin and last event, on the shared
    /// monotonic clock (not rebased onto the merged origin).
    pub capture_start: u64,
    pub capture_end: u64,
    /// Header event count for this file alone.
    pub event_count: u64,
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub version: u32,
    /// Monotonic origin every capture-relative timestamp is measured
    /// from, and the instant [`Self::start_utc_us`] names in wall-clock
    /// terms. Usually the header's `capture_start` verbatim, but a
    /// capture that opens with drained ring backlog moves it forward to
    /// the first live event — see [`Self::header_capture_start`].
    pub capture_start: u64,
    /// The header's `capture_start` field as written by `elog(1)`: the
    /// timestamp of the oldest event it drained. A user-space provider
    /// whose ring still holds registration records from before `elog`
    /// attached backdates this by however long that provider had been
    /// running, while the header's `start_utc_us` is a `gettimeofday()`
    /// taken at drain time — so the two disagree by exactly the backlog
    /// span. Equal to [`Self::capture_start`] on a capture with no
    /// backlog.
    pub header_capture_start: u64,
    /// Events that predate [`Self::capture_start`], i.e. the drained
    /// backlog the effective origin skipped over. `0` when
    /// [`Self::capture_start`] is the header value verbatim.
    pub backlog_events: usize,
    pub start_utc_us: u64,
    /// Raw monotonic timestamp of the last event seen in the capture
    /// (the max across every event header, not just decoded ones).
    /// Paired with `capture_start`/`start_utc_us` the same way
    /// `capture_start` is, to derive a wall-clock "capture end".
    pub capture_end: u64,
    pub event_count: u64,
    /// Kernel per-CPU buffer-full drops (`ELOG_COUNTER_KERNEL_DROPPED`).
    /// The only drop counter that exists in v1/v2 files (as the header's
    /// single `dropped_events` field); 0 for the three fields below on
    /// those versions.
    pub dropped: u64,
    /// User-space producer (`libeventlog`) drop counters, v3+ only. See
    /// `ELOG_COUNTER_USER_DROPPED_*` in elog.c / "Header Counters" in
    /// docs/file-format.md.
    pub user_dropped_full: u64,
    pub user_dropped_prejoin: u64,
    pub user_dropped_late: u64,
    pub parsed: usize,
    pub skipped_other_providers: usize,
    /// The files this capture was built from, earliest first. Filled in
    /// by the `parse_elog*` loaders, which know the paths; empty when a
    /// caller parses raw bytes through `parse_elog_core` directly.
    pub sources: Vec<CaptureSource>,
    /// Host/build metadata from an elog-synthesised `machine.MachineInfo`
    /// event (first sighting wins across merged files).
    pub machine: Option<MachineInfo>,
}

/// Host and build identity from the virtual `machine` provider's
/// `MachineInfo` event, synthesised by `elog(1)` at capture start.
#[derive(Debug, Clone, Default)]
pub struct MachineInfo {
    pub hostuuid: String,
    pub hostname: String,
    pub osrelease: String,
    pub kernel_version: String,
    pub firmware_version: String,
    pub platform: String,
    pub cpu_model: String,
    pub ncpu: u32,
    pub physmem: u64,
    pub boottime_sec: i64,
    /// Optional key/value pairs from `machine.MetadataKV` events.
    pub metadata_kv: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Provider {
    pub id: u16,
    pub name: String,
    /// Total number of events seen in the capture whose header tagged
    /// them with this provider id, regardless of whether they were
    /// further decoded (some `tcp` events fall back to
    /// `EventPayload::Other`, and most `hpts` events besides
    /// `STARS_AGG_SAMPLE` are not consumed). Useful for the aggregate
    /// info popup so the user can see which providers contributed
    /// data to the file.
    pub event_count: u64,
    /// First and last event timestamps for this provider (header clock).
    /// `None` when the provider contributed no events.
    pub first_ts: Option<u64>,
    pub last_ts: Option<u64>,
    /// Number of distinct `session_id` values seen across this
    /// provider's events (regardless of whether they were decoded).
    pub session_count: usize,
    /// Total uncompressed byte size (header + payload) in the file
    /// across this provider's events, regardless of whether they were
    /// decoded.
    pub byte_size: u64,
    /// Provider name from this provider's resolved+verified `EDB1`
    /// decode blob (v2 captures only), if the file carried one, its
    /// `schema_id` matched `SHA256(blob)`, and the blob parsed
    /// successfully. `None` for v1 captures and for v2 providers whose
    /// blob was missing/corrupt/hash-mismatched (in which case this
    /// provider's events decode exactly as they would in a v1 file —
    /// see the "Version 2" section of `docs/file-format.md`).
    pub schema_name: Option<String>,
    /// Number of events this schema defines, or 0 when `schema_name`
    /// is `None`.
    pub schema_event_count: usize,
    /// One human-readable line per event id where the hardcoded
    /// decoder's assumed field offsets/sizes disagree with the
    /// resolved schema (an *unsafe* drift — insertion, reorder, or
    /// retype of an existing field). Empty unless real drift was
    /// detected; events listed here are decoded generically from the
    /// schema instead of via the hardcoded decoder. See
    /// `schema_validate::validate_hardcoded_decoders`.
    pub schema_divergent_events: Vec<String>,
    /// One human-readable "notice" line per event id where the
    /// hardcoded decoder is still safe to trust but the schema has
    /// grown fields beyond what it decodes (*additive*, safe drift —
    /// e.g. `"IN: 5 undecoded fields (state, th_off, th_sum, th_urp,
    /// rcv_time_us)"`). These fields are NOT automatically captured
    /// into `EventPayload` (see the plan's "additive fields are
    /// flagged, not auto-decoded" scope note) — this is a developer
    /// signal to extend the decoder deliberately, computed once per
    /// file rather than per event.
    pub schema_new_fields: Vec<String>,
    /// Per-event record counts and sizes. Includes schema events the
    /// capture never recorded (`EventStat::defined_only`).
    pub events: Vec<EventStat>,
}

/// One event id's on-disk footprint inside a provider.
#[derive(Debug, Clone)]
pub struct EventStat {
    pub event_id: u32,
    pub name: String,
    pub count: u64,
    /// Sum of whole records (32-byte header + payload).
    pub bytes: u64,
    pub payload_bytes: u64,
    /// Smallest and largest record, header included. Zero when `count` is 0.
    pub min_len: u32,
    pub max_len: u32,
    /// Header timestamps. Set once a record is seen.
    pub first_ts: Option<u64>,
    pub last_ts: Option<u64>,
    pub level: Option<u8>,
    pub keywords: Vec<String>,
    /// Payload fields from the decode schema, in offset order.
    pub fields: Vec<EventFieldStat>,
    pub format: String,
    /// The schema lists this id and the capture recorded none.
    pub defined_only: bool,
}

/// One payload field from the provider's decode schema.
#[derive(Debug, Clone)]
pub struct EventFieldStat {
    pub name: String,
    pub offset: u16,
    /// Element size in bytes. Zero when `varlen` is set.
    pub size: u16,
    pub varlen: bool,
    pub varlen_max: u16,
}

/// Machine-wide snapshot of the STARS adaptive egress-rate-squeeze
/// controller, from an HPTS `STARS_AGG_SAMPLE` event. Not tied to a TCP
/// session — one aggregate stream per capture. Fixed-width 16×`uint64_t`
/// payload; see `hpts_eventlog_schema.src`'s `STRUCT STARS_AGG_SAMPLE`.
///
/// `pressure`, `slot_bytes_peak`, and `slot_gate_budget_bytes` are
/// instantaneous gauges (the latter two reset/refreshed every sample);
/// everything else ending in a counter-ish name is cumulative — diff
/// successive samples to get per-period rates. `health_limit_bps` /
/// `min_rate_bps` / `slot_gate_enable` are config values that rarely
/// change within a capture.
#[derive(Debug, Clone, Copy)]
pub struct StarsAggSample {
    pub ts: u64,
    /// Throttle gauge, 0..1024 (`STARS_PRESSURE_SCALE`); higher means
    /// more throttling.
    pub pressure: u64,
    /// Token-bucket level. `int64_t` on the kernel side (can go
    /// negative); wired as a `uint64_t` bit pattern, so this is cast
    /// back to signed after reading.
    pub tokens: i64,
    pub per_slot_bytes: u64,
    /// Configured per-slot byte budget. Instantaneous gauge.
    pub slot_gate_budget_bytes: u64,
    /// Since-last-sample peak slot burst size. Instantaneous gauge,
    /// NOT cumulative.
    pub slot_bytes_peak: u64,
    pub slot_gate_denies: u64,
    pub slot_gate_admits: u64,
    pub slot_gate_denied_bytes: u64,
    pub slot_gate_admitted_bytes: u64,
    pub meter_obytes: u64,
    pub pressure_sum: u64,
    pub pressure_ticks: u64,
    pub synthetic_norate_hits: u64,
    /// `0` or `1`.
    pub slot_gate_enable: u64,
    pub health_limit_bps: u64,
    pub min_rate_bps: u64,
    /// Current pacing-rollover backlog, summed across all HPTS.
    /// Instantaneous gauge, NOT cumulative. `0` for captures predating
    /// this field (older files simply don't carry it).
    pub rollover_queue_depth: u64,
    pub rollover_diverts: u64,
    pub rollover_admits: u64,
    /// Self-tuning slot-gate headroom (1..100), applied as a percentage
    /// of the auto (baud-derived) budget; 100 = no reduction. `100` for
    /// captures predating this field. Instantaneous gauge, NOT
    /// cumulative.
    pub slot_gate_headroom_pct: u64,
    /// Cumulative sends handed back for inline transmission during RTO
    /// recovery, i.e. how often the `stars_rxt_no_pace` bypass actually
    /// fired. `0` for captures predating this field, which is
    /// indistinguishable from the knob being off -- check the knob's own
    /// state rather than inferring it from a zero here.
    pub rxt_no_pace_hits: u64,
}

/// One `STARS_PACE` event (tcp provider, event 604), flattened out of the
/// per-session `Sessions` map into its own file-wide stream so whole-capture
/// natural-vs-scaled-rate analysis (`starspace` CLI command) doesn't need to
/// walk every session's event list. Emitted at both `hpts_stars_pace_rate()`
/// call sites in `rack_get_pacing_delay()`, only when the connection's
/// eventlog session is enabled (e.g. via `auto_ratio`). `natural_bps` is the
/// pre-squeeze rate estimate; `natural_bps == 1 << 24` (134.217728 Mbps)
/// marks the synthetic no-rate-estimate placeholder rather than a real
/// goodput-based estimate — see `synthetic_norate_hits` on `StarsAggSample`.
#[derive(Debug, Clone, Copy)]
pub struct StarsPaceSample {
    pub ts: u64,
    pub session_id: u64,
    pub natural_bps: u64,
    pub scaled_bps: u64,
    /// Throttle gauge, 0..1024 (`STARS_PRESSURE_SCALE`), in effect for this
    /// send — same scale as `StarsAggSample::pressure`.
    pub pressure: u32,
    pub len: u32,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub ts: u64,
    pub cpu: u16,
    pub tid: i32,
    pub event_id: u32,
    pub payload: EventPayload,
}

// ---------------------------------------------------------------------------
// TCP periodic-sample profile (collected from the "tcp" provider's
// SAMPLE / SAMPLE_HIST events, 900-903).
//
// These are global per-stack snapshots, not per-connection events. A
// FreeBSD box registers one "tcp" provider per loaded TCP stack (so a
// capture can contain several providers all named "tcp", each with a
// distinct id), and each stack emits its samples on its own "global"
// eventlog session. TCP_STACK_INFO maps a session_id to a stack name +
// sample period; the SAMPLE/RPS/GP events correlate via session_id.
// ---------------------------------------------------------------------------

/// Identity + sample cadence for one TCP stack's global session, from a
/// `TCP_STACK_INFO` (id 902) event.
#[derive(Debug, Clone)]
pub struct TcpStackInfo {
    /// Stack name, e.g. `"rack"`, `"bbr"`, `"freebsd"`.
    pub stack_name: String,
    /// Configured sample period in microseconds (0 if no sampler armed).
    pub sample_period_us: u32,
}

/// One `TCP_SAMPLE` (id 900): aggregate PCB-state gauges across every
/// VNET at a single tick. One `u64` count per TCP state, matching the
/// kernel `tcp_eventlog_schema.src` layout (11 contiguous `uint64_t`
/// fields, host byte order — 88 bytes). Total PCB count = sum of all
/// fields.
#[derive(Debug, Clone, Copy)]
pub struct TcpStateSample {
    pub ts: u64,
    /// Owning "tcp" provider id. Session ids are only unique *within* a
    /// provider, so `(provider_id, session_id)` is the real stack key.
    pub provider_id: u16,
    /// Subsystem-wide aggregate session id (`TCP_SESSION_ID_AGGREGATE`);
    /// these gauges are cross-stack, not tied to any one stack.
    pub session_id: u64,
    pub closed: u64,
    pub listen: u64,
    pub syn_sent: u64,
    pub syn_rcvd: u64,
    pub estab: u64,
    pub close_wait: u64,
    pub fin_wait_1: u64,
    pub closing: u64,
    pub last_ack: u64,
    pub fin_wait_2: u64,
    pub time_wait: u64,
}

impl TcpStateSample {
    /// Sum of every per-state count — the total number of PCBs the
    /// kernel counted at this tick.
    pub fn total(&self) -> u64 {
        self.closed
            + self.listen
            + self.syn_sent
            + self.syn_rcvd
            + self.estab
            + self.close_wait
            + self.fin_wait_1
            + self.closing
            + self.last_ack
            + self.fin_wait_2
            + self.time_wait
    }
}

/// One `TCP_RPS_SAMPLE` (id 903): per-stack cumulative request count.
/// Diff successive samples (by session_id) to get requests/sec.
#[derive(Debug, Clone, Copy)]
pub struct TcpRpsSample {
    pub ts: u64,
    /// Owning "tcp" provider id; part of the `(provider_id, session_id)`
    /// stack key.
    pub provider_id: u16,
    pub session_id: u64,
    pub req_count: u64,
}

/// One `TCP_GP_SAMPLE` (id 901): per-stack cumulative goodput histogram.
/// `hist` holds `gput_n` cumulative per-bucket counters; diff successive
/// samples (by session_id) for per-period bucket occupancy.
#[derive(Debug, Clone)]
pub struct TcpGpSample {
    pub ts: u64,
    /// Owning "tcp" provider id; part of the `(provider_id, session_id)`
    /// stack key.
    pub provider_id: u16,
    pub session_id: u64,
    pub hist: Vec<u64>,
}

/// All TCP periodic-sample data collected from a capture. `is_empty()`
/// gates the TCP-stacks view in the GUI.
///
/// A box registers one "tcp" provider per loaded TCP stack, and every
/// one numbers its stack-global session identically
/// (`TCP_SESSION_ID_STACK_GLOBAL`). So a bare `session_id` is *not*
/// unique across stacks — the base "freebsd" provider and a loadable
/// `ngen` stack both use `0x8000000000000001`. All per-stack data is
/// therefore keyed by `(provider_id, session_id)`.
#[derive(Debug, Clone, Default)]
pub struct TcpSampleProfile {
    /// (provider_id, session_id) -> stack identity / period (from
    /// TCP_STACK_INFO).
    pub stacks: FxHashMap<(u16, u64), TcpStackInfo>,
    pub state_samples: Vec<TcpStateSample>,
    pub rps_samples: Vec<TcpRpsSample>,
    pub gp_samples: Vec<TcpGpSample>,
    /// `TCP_PACING_RATE_SAMPLE` (id 904): per-stack cumulative histogram
    /// of the commanded STARS pacing rate. Same bucket layout as
    /// `gp_samples`, meant to overlay on the same axis.
    pub pacing_rate_samples: Vec<TcpGpSample>,
    /// `TCP_REALIZED_RATE_SAMPLE` (id 905): per-stack cumulative
    /// histogram of the realized per-connection egress rate. Same
    /// bucket layout as `gp_samples`.
    pub realized_rate_samples: Vec<TcpGpSample>,
    /// `TCP_SEND_SAMPLE` (id 906): per-stack cumulative histogram of
    /// send sizes (byte-weighted segments-per-burst). Same on-wire
    /// shape as `gp_samples` but 64 linear buckets.
    pub send_samples: Vec<TcpGpSample>,
    /// `TCP_LRO_FLUSH_SAMPLE` (id 907): subsystem-wide cumulative
    /// histogram of packets coalesced per LRO flush. Same on-wire
    /// shape as `send_samples`; rides the aggregate session.
    pub lro_flush_samples: Vec<TcpGpSample>,
    /// `TCP_SRTT_SAMPLE` (id 908): per-stack cumulative histogram of
    /// smoothed RTT in usec. Same on-wire shape as `gp_samples` but
    /// 14 custom buckets.
    pub srtt_samples: Vec<TcpGpSample>,
}

impl TcpSampleProfile {
    pub fn is_empty(&self) -> bool {
        self.stacks.is_empty()
            && self.state_samples.is_empty()
            && self.rps_samples.is_empty()
            && self.gp_samples.is_empty()
            && self.pacing_rate_samples.is_empty()
            && self.realized_rate_samples.is_empty()
            && self.send_samples.is_empty()
            && self.lro_flush_samples.is_empty()
            && self.srtt_samples.is_empty()
    }

    /// Whether the profile has per-stack series worth a dedicated TCP
    /// Stack tab (requests/sec, goodput, send size, or smoothed RTT). The
    /// subsystem-wide PCB-state gauges (`state_samples`) and LRO flush
    /// histogram are shown on the Aggregate tab instead, so they don't
    /// count here.
    pub fn has_stack_series(&self) -> bool {
        !self.rps_samples.is_empty()
            || !self.gp_samples.is_empty()
            || !self.pacing_rate_samples.is_empty()
            || !self.realized_rate_samples.is_empty()
            || !self.send_samples.is_empty()
            || !self.srtt_samples.is_empty()
    }

    /// Display label for a per-stack global session. Uses the
    /// `TCP_STACK_INFO` name when the stack emitted one; otherwise falls
    /// back to a provider-qualified id. The latter happens when a stack
    /// streams samples but its `TCP_STACK_INFO` never landed — e.g. a
    /// stack loaded *after* the collector subscribed to the `tcp`
    /// provider name, so no dump-on-subscribe replayed its identity.
    pub fn stack_label(&self, provider_id: u16, session_id: u64) -> String {
        self.stacks
            .get(&(provider_id, session_id))
            .map(|s| s.stack_name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("tcp#{provider_id}"))
    }
}

// ---------------------------------------------------------------------------
// ELLA `fanout_node` channel-layer profile (corp/ella PR #456)
// ---------------------------------------------------------------------------

/// Per-channel identity accumulated from SESSION_CREATE / ID_ASSIGNED /
/// STATE / DESTROYED. Keyed by `(provider_id, session_id)`. Session 0 is
/// the global session and is not stored here.
#[derive(Debug, Clone)]
pub struct EllaChannelInfo {
    pub provider_id: u16,
    pub session_id: u64,
    pub channel_name: String,
    pub version: u8,
    pub first_ts: u64,
    pub last_ts: u64,
    pub event_count: usize,
    /// Last `channel_id` learned per Fanout `instance_id`.
    pub channel_ids: FxHashMap<u32, u32>,
    pub last_subscribers: u32,
    /// Max reconstructed occupancy (SUBSCRIBED − UNSUBSCRIBED, summed
    /// across instances) while the session was live.
    pub peak_subscribers: u32,
    /// Live occupancy by instance; updated while ingesting.
    pub occ_by_inst: FxHashMap<u32, u32>,
    pub last_state: Option<u8>,
    pub destroy_reason: Option<u8>,
}

impl EllaChannelInfo {
    /// CHANNEL_DESTROYED when logged, otherwise the last CHANNEL_STATE.
    pub fn end_state_name(&self) -> &'static str {
        if self.destroy_reason.is_some() {
            "DESTROYED"
        } else {
            self.last_state
                .map(ella_lifecycle_state_name)
                .unwrap_or("-")
        }
    }
}

/// One `CHANNEL_COUNT` sample (id 150), on the global session.
/// Five u32s on current `fanout_node`; a sixth `sessions` field is
/// present on older `ella_relay` captures.
#[derive(Debug, Clone, Copy)]
pub struct EllaChannelCount {
    pub ts: u64,
    pub provider_id: u16,
    pub instance_id: u32,
    pub channels: u32,
    pub channels_pending_leave: u32,
    pub channels_pending_retire: u32,
    pub subscribers: u32,
    pub sessions: u32,
}

/// One `CHANNEL_SAMPLE` (id 152): one channel's occupancy and object
/// traffic on one instance, on that channel's session. `packets` is
/// DATA packets noted this interval (same datagrams as `object_bytes`);
/// older captures leave it 0. Later captures also carry
/// `inproc_attached`, `publish_no_ring`, and `drain_skipped_pending`.
#[derive(Debug, Clone, Copy)]
pub struct EllaChannelSample {
    pub ts: u64,
    pub provider_id: u16,
    pub session_id: u64,
    pub instance_id: u32,
    pub subscribers: u32,
    pub subscribers_pending: u32,
    pub subscribers_leaving: u32,
    pub objects_completed: u32,
    pub object_bytes: u32,
    pub packets: u32,
    pub inproc_attached: u32,
    pub publish_no_ring: u32,
    pub drain_skipped_pending: u32,
}

/// One `FRAME_SAMPLE` (id 153): one channel's pacing-frame totals for a
/// sample interval, on that channel's session. Origin sums are planned
/// vs actual send durations; relay sums are wire/recv/fwd durations.
/// `sum_packets` is DATA packets across those frames; older captures
/// leave it 0.
#[derive(Debug, Clone, Copy)]
pub struct EllaFrameSample {
    pub ts: u64,
    pub provider_id: u16,
    pub session_id: u64,
    pub instance_id: u32,
    pub frames: u32,
    pub frames_stretched: u32,
    pub sum_frame_len: u32,
    pub sum_planned_us: u32,
    pub sum_actual_us: u32,
    pub sum_fwd_us: u32,
    pub max_overshoot_us: u32,
    pub sum_packets: u32,
}

/// One `CHANNEL_COUNT_TOTAL` / `CHANNEL_STATS` sample (id 151),
/// process-wide occupancy on the global session (`STATS` keyword).
/// Gauge fields are levels; traffic/join counters are deltas since the
/// previous sample. Current captures carry the counters in the
/// `TRAFFIC_STATS` / `DELIVERED_STATS` events that follow, folded in
/// here. Older captures stop after the subscriber gauges (or earlier);
/// the counter fields then stay 0.
#[derive(Debug, Clone, Copy)]
pub struct EllaChannelCountTotal {
    pub ts: u64,
    pub provider_id: u16,
    pub channels_up: u32,
    pub channels_dn: u32,
    pub channels_pending_leave_up: u32,
    pub channels_pending_leave_dn: u32,
    pub channels_pending_retire_up: u32,
    pub channels_pending_retire_dn: u32,
    pub subscribers_up: u32,
    pub subscribers_dn: u32,
    pub objects_up: u32,
    pub objects_dn: u32,
    pub packets_up: u32,
    pub packets_dn: u32,
    pub bytes_up: u64,
    pub bytes_dn: u64,
    pub joins_up: u32,
    pub joins_dn: u32,
    pub switches_up: u32,
    pub switches_dn: u32,
    pub leaves_up: u32,
    pub leaves_dn: u32,
    pub expiries_up: u32,
    pub expiries_dn: u32,
    pub rejects_up: u32,
    pub rejects_dn: u32,
    pub packets_dlvr: u32,
    pub bytes_dlvr: u64,
    pub joins_dlvr: u32,
    pub switches_dlvr: u32,
    pub new_clients: u32,
    pub clients_up: u32,
    pub clients_dn: u32,
    pub subscribers_dlvr: u32,
}

/// Lifecycle / subscriber / object / error payload. Periodic
/// `CHANNEL_COUNT` and `CHANNEL_COUNT_TOTAL` are stored on [`EllaProfile`].
#[derive(Debug, Clone)]
pub enum EllaPayload {
    InstanceAttach {
        instance_id: u32,
        side: u8,
    },
    NodeInfo {
        role: u8,
    },
    SessionCreate {
        channel_name: String,
        version: u8,
        instance_id: u32,
    },
    SessionDestroy,
    ChannelState {
        state: u8,
        reason: u8,
        instance_id: u32,
        subscribers: u32,
    },
    ChannelDestroyed {
        reason: u8,
        instance_id: u32,
        subscribers: u32,
    },
    ChannelIdAssigned {
        instance_id: u32,
        channel_id: u32,
    },
    ChannelSubscriber {
        state: u8,
        reason: u8,
        instance_id: u32,
        subscriber_address: String,
        subscriber_port: u16,
        subscribers: u32,
    },
    /// A subscription's PBCID, just before its SUBSCRIBING
    /// `ChannelSubscriber`.
    SubscriberIdentity {
        instance_id: u32,
        request_id: u32,
        subscriber_address: String,
        subscriber_port: u16,
        pbcid: String,
    },
    ObjectState {
        state: u8,
        instance_id: u32,
        boundary_index: u8,
        first_packet: u32,
        packets: u32,
        bytes: u32,
    },
    /// Origin object encode/enqueue/pacer timing (`OBJECT` keyword).
    ObjectTiming {
        instance_id: u32,
        object_id: u32,
        first_packet: u32,
        packets_sent: u32,
        packets_total: u32,
        packets_unsent: u32,
        start_lateness_us: i32,
        signature_duration_us: u32,
        encode_duration_us: u32,
        enqueue_duration_us: u32,
    },
    FrameSent {
        instance_id: u32,
        object_id: u32,
        frame_index: u32,
        first_packet: u32,
        packets: u32,
        is_probe: bool,
        toggle: bool,
        frame_len: u32,
        planned_send_us: u32,
        actual_send_us: u32,
        start_lateness_us: i32,
    },
    FrameRelay {
        instance_id: u32,
        first_packet: u32,
        packets: u32,
        toggle: bool,
        boundary_index: u8,
        frame_len: u32,
        wire_send_us: u32,
        recv_us: u32,
        fwd_us: u32,
        residency_us: u32,
    },
    ChannelError {
        code: u8,
        instance_id: u32,
        detail: u32,
    },
    RelayError {
        code: u8,
        instance_id: u32,
        channel_id: u32,
        channel_id_known: bool,
        detail: u32,
        message_type: u8,
        subscriber_address: String,
        subscriber_port: u16,
    },
}

/// One decoded `fanout_node` lifecycle, subscriber, or object event.
#[derive(Debug, Clone)]
pub struct EllaEvent {
    pub ts: u64,
    pub cpu: u16,
    pub tid: i32,
    pub provider_id: u16,
    pub session_id: u64,
    pub event_id: u32,
    pub payload: EllaPayload,
}

/// Role of one `fanout_node` provider, inferred from its `INSTANCE_ATTACH`
/// set. Internal / External remain faces *within* a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EllaNodeRole {
    Server,
    Relay,
    Other,
}

impl EllaNodeRole {
    /// Display order wherever nodes are listed: Server, Relay, then
    /// anything whose attach set didn't match either shape.
    pub fn rank(self) -> u8 {
        match self {
            Self::Server => 0,
            Self::Relay => 1,
            Self::Other => 2,
        }
    }

    pub fn label(self, provider_id: u16) -> String {
        match self {
            Self::Server => "Server".into(),
            Self::Relay => "Relay".into(),
            Self::Other => format!("fanout_node#{provider_id}"),
        }
    }

    pub fn cli_prefix(self, provider_id: u16) -> String {
        match self {
            Self::Server => "server".into(),
            Self::Relay => "relay".into(),
            Self::Other => format!("fanout_node#{provider_id}"),
        }
    }
}

/// One `OFFLOAD_STATS` sample (id 154): process-wide send-seq and
/// rxring deltas for the stats interval, on the global session.
/// `rxring_result` is the open attempt: 0 not attempted, 1 opened,
/// 2 no socket, 3 port unbound, 4 catch-all refused, 5 region too big,
/// 6 socket failed, 7 init failed, 8 evloop failed.
#[derive(Debug, Clone, Copy, Default)]
pub struct EllaOffloadSample {
    pub ts: u64,
    pub provider_id: u16,
    pub send_seq_ok: u32,
    pub send_seq_stale: u32,
    pub rxring_routed: u32,
    pub rxring_else_gate: u32,
    pub rxring_else_unreg: u32,
    pub rxring_else_malformed: u32,
    pub rxring_overwrites: u32,
    pub rxring_dropped_unreg: u32,
    pub rxring_result: u32,
    pub rxring_port: u32,
    pub rxring_errno: u32,
    pub rxring_region_bytes: u32,
    pub rxring_else_nocls: u32,
}

/// All `fanout_node` / `ella_relay` data from a capture. `is_empty()`
/// gates the ELLA System-tab panels and the channel list.
#[derive(Debug, Clone, Default)]
pub struct EllaProfile {
    pub channels: FxHashMap<(u16, u64), EllaChannelInfo>,
    pub events: Vec<EllaEvent>,
    pub counts: Vec<EllaChannelCount>,
    pub counts_total: Vec<EllaChannelCountTotal>,
    pub channel_samples: Vec<EllaChannelSample>,
    pub frame_samples: Vec<EllaFrameSample>,
    pub offload_samples: Vec<EllaOffloadSample>,
    /// `(provider_id, instance_id) → side` from `INSTANCE_ATTACH`.
    pub instances: FxHashMap<(u16, u32), u8>,
    /// `provider_id → role` from `NODE_INFO`, the producer's own
    /// declaration of what it is.
    pub roles: FxHashMap<u16, u8>,
}

impl EllaProfile {
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
            && self.events.is_empty()
            && self.counts.is_empty()
            && self.counts_total.is_empty()
            && self.channel_samples.is_empty()
            && self.frame_samples.is_empty()
            && self.offload_samples.is_empty()
    }

    pub fn has_channel_counts(&self) -> bool {
        !self.counts.is_empty() || !self.counts_total.is_empty()
    }

    /// Detailed or aggregate frame-duration data for System timing-ratio plots.
    pub fn has_frame_timing(&self) -> bool {
        !self.frame_samples.is_empty()
            || self.events.iter().any(|event| {
                matches!(
                    &event.payload,
                    EllaPayload::FrameSent { .. } | EllaPayload::FrameRelay { .. }
                )
            })
    }

    /// Origin `OBJECT_TIMING` records for Ella Trace Metrics plots.
    pub fn has_object_timing(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(&event.payload, EllaPayload::ObjectTiming { .. }))
    }

    /// Process-wide traffic / join counter samples (CDN metrics fields).
    pub fn has_cdn_counters(&self) -> bool {
        self.counts_total.iter().any(|s| {
            u64::from(
                s.packets_up
                    | s.packets_dn
                    | s.joins_up
                    | s.joins_dn
                    | s.switches_up
                    | s.switches_dn
                    | s.leaves_up
                    | s.leaves_dn
                    | s.expiries_up
                    | s.expiries_dn
                    | s.rejects_up
                    | s.rejects_dn
                    | s.packets_dlvr
                    | s.joins_dlvr
                    | s.switches_dlvr
                    | s.new_clients,
            ) | s.bytes_up
                | s.bytes_dn
                | s.bytes_dlvr
                != 0
        })
    }

    /// Distinct `fanout_node` provider ids in this capture.
    pub fn fanout_provider_ids(&self) -> Vec<u16> {
        let mut ids = FxHashSet::default();
        for (pid, _) in self.instances.keys() {
            ids.insert(*pid);
        }
        for (pid, _) in self.channels.keys() {
            ids.insert(*pid);
        }
        for s in &self.counts {
            ids.insert(s.provider_id);
        }
        for s in &self.counts_total {
            ids.insert(s.provider_id);
        }
        for s in &self.channel_samples {
            ids.insert(s.provider_id);
        }
        for s in &self.frame_samples {
            ids.insert(s.provider_id);
        }
        for e in &self.events {
            ids.insert(e.provider_id);
        }
        let mut v: Vec<u16> = ids.into_iter().collect();
        v.sort_unstable();
        v
    }

    /// `NODE_INFO` is authoritative when the producer emitted it. Older
    /// producers, and captures that lost the event to a prejoin drop,
    /// fall back to the shape of the `INSTANCE_ATTACH` set: a server
    /// fans out from i0 alone, a relay pulls from an upstream i0 and/or
    /// spreads work across instances above i0.
    pub fn node_role(&self, provider_id: u16) -> EllaNodeRole {
        match self.roles.get(&provider_id) {
            Some(&ELLA_ROLE_ORIGIN) => return EllaNodeRole::Server,
            Some(&ELLA_ROLE_RELAY) => return EllaNodeRole::Relay,
            _ => {}
        }
        let mut up_i0 = false;
        let mut dn_i0 = false;
        let mut dn_worker = false;
        for ((pid, iid), side) in &self.instances {
            if *pid != provider_id {
                continue;
            }
            match (*iid, *side) {
                (0, ELLA_SIDE_UPSTREAM) => up_i0 = true,
                (0, ELLA_SIDE_DOWNSTREAM) => dn_i0 = true,
                (_, ELLA_SIDE_DOWNSTREAM) => dn_worker = true,
                _ => {}
            }
        }
        if up_i0 || dn_worker {
            EllaNodeRole::Relay
        } else if dn_i0 {
            EllaNodeRole::Server
        } else {
            EllaNodeRole::Other
        }
    }

    pub fn node_label(&self, provider_id: u16) -> String {
        self.node_role(provider_id).label(provider_id)
    }

    pub fn node_cli_prefix(&self, provider_id: u16) -> String {
        self.node_role(provider_id).cli_prefix(provider_id)
    }

    pub fn instance_side(&self, provider_id: u16, instance_id: u32) -> u8 {
        self.instances
            .get(&(provider_id, instance_id))
            .copied()
            .unwrap_or(ELLA_SIDE_DOWNSTREAM)
    }

    pub fn instance_label(&self, provider_id: u16, instance_id: u32) -> String {
        let side = self.instances.get(&(provider_id, instance_id)).copied();
        match self.node_role(provider_id) {
            EllaNodeRole::Server => format!("server i{instance_id}"),
            EllaNodeRole::Relay => match side {
                Some(ELLA_SIDE_UPSTREAM) => format!("relay internal i{instance_id}"),
                _ => format!("relay i{instance_id}"),
            },
            EllaNodeRole::Other => format!("fanout_node#{provider_id} i{instance_id}"),
        }
    }

    pub fn channel_label(&self, provider_id: u16, session_id: u64) -> String {
        self.channels
            .get(&(provider_id, session_id))
            .map(|c| {
                if c.channel_name.is_empty() {
                    format!("ch#{session_id}")
                } else {
                    c.channel_name.clone()
                }
            })
            .unwrap_or_else(|| format!("ch#{session_id}"))
    }
}

// ---------------------------------------------------------------------------
// nginx user-space provider (`nflx_eventlog`). One eventlog session per
// transport connection; request events on that session are tagged with the
// HTTP/1 request serial or HTTP/2 stream id. See `nginx_eventlog_schema.src`.
// ---------------------------------------------------------------------------

/// Request-layer payload for one `nginx` event. Envelope `session_id` is
/// the TCP connection; `request` distinguishes keepalive / HTTP/2 streams.
/// Worker-wide events (`Sample`, `WorkerStall`, `AioSyncFallback`) arrive
/// on special sessions (top bit set) instead of a connection.
#[derive(Debug, Clone)]
pub enum NginxPayload {
    RequestStart {
        request: u32,
        range_start: i64,
        range_end: i64,
        method: String,
        uri: String,
    },
    ResponseHeaders {
        request: u32,
        status: u16,
        content_length: i64,
        /// Served file's `st_dev`; `None` on captures predating the field,
        /// `Some(0)` when the response wasn't a static/cached file.
        dev: Option<u64>,
    },
    RequestDone {
        request: u32,
        status: u16,
        bytes_sent: u64,
        /// Appended completion detail; `None` on older captures.
        detail: Option<NginxDoneDetail>,
    },
    RequestMessage {
        request: u32,
        msg: String,
    },
    /// Interval sample (NGINX_SAMPLE): completions by status class plus
    /// response bytes since the previous sample.
    Sample {
        resp_1xx: u64,
        resp_2xx: u64,
        resp_3xx: u64,
        resp_4xx: u64,
        resp_5xx: u64,
        resp_other: u64,
        bytes_sent: u64,
    },
    ConnStart {
        server_port: u16,
        flags: u8,
    },
    ConnEnd {
        reason: String,
        requests: u32,
        bytes_sent: u64,
        duration_ms: u32,
    },
    TlsHandshakeDone(NginxTlsInfo),
    TlsHandshakeFail(NginxTlsFail),
    SendfileBusyDefer {
        delay_ms: u16,
        file_pos: i64,
        dev: u64,
        ktls: bool,
    },
    SendfileError {
        kind: String,
        sys_errno: i32,
        file_pos: i64,
        dev: u64,
        ktls: bool,
    },
    FileOpenFail {
        request: u32,
        status: u16,
        sys_errno: i32,
        syscall: String,
    },
    CacheLookup(NginxCacheLookup),
    UpstreamAttemptStart {
        request: u32,
        subrequest: bool,
        attempt: u16,
        connect_rc: String,
        local_port: u16,
        dns_ms: i32,
        upstream_session: u64,
        target_type: String,
        peer: String,
    },
    UpstreamAttemptEnd {
        request: u32,
        subrequest: bool,
        attempt: u16,
        /// `NGX_HTTP_UPSTREAM_FT_*` bits; 0 for the final attempt.
        ft: u32,
        status: u16,
        connect_ms: i32,
        header_ms: i32,
        elapsed_ms: u32,
        bytes_received: u64,
    },
    WorkerStall {
        stall_ms: u32,
        file: String,
        line: u32,
        prev_file: String,
        prev_line: u32,
        func: String,
    },
    AioSyncFallback {
        offset: i64,
        size: u64,
        read_us: u32,
        dev: u64,
    },
}

/// `REQUEST_DONE` fields appended in NCD-10222.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NginxDoneDetail {
    /// `NGINX_DONE_*` bits.
    pub flags: u16,
    pub err_status: u16,
    pub content_length: i64,
    pub request_time_ms: u32,
    /// `NGX_HTTP_CACHE_*` (0 = no cache involved).
    pub cache_status: u8,
    pub upstream_tries: u8,
    /// Sendfile EBUSY on the connection during the request (HTTP/2:
    /// includes other streams).
    pub busy_events: u32,
    pub busy_defer_ms: u32,
    pub aio_reads: u32,
    pub aio_wait_ms: u32,
    pub aio_max_ms: u32,
}

/// `TLS_HANDSHAKE_DONE`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NginxTlsInfo {
    /// `SSL_version()` (0x0304 = TLS 1.3).
    pub version: u16,
    /// `NGINX_TLS_*` bits.
    pub flags: u8,
    /// Accept → handshake done.
    pub handshake_ms: u32,
    pub cipher: String,
    pub alpn: String,
}

/// `TLS_HANDSHAKE_FAIL`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NginxTlsFail {
    pub kind: String,
    pub ssl_error: i32,
    /// `ERR_GET_REASON()` of the oldest queued OpenSSL error.
    pub reason: u32,
    pub sys_errno: i32,
    pub handshake_ms: u32,
}

/// `CACHE_LOOKUP`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NginxCacheLookup {
    pub request: u32,
    pub subrequest: bool,
    pub cache_status: u8,
    pub fas_state: String,
    pub lock_wait_ms: u32,
}

/// One upstream (fill) attempt, joined from START/END.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NginxUpstreamAttempt {
    pub attempt: u16,
    pub subrequest: bool,
    pub peer: String,
    pub target_type: String,
    pub connect_rc: String,
    /// The upstream socket's TCP EventLog session id (0 = unknown).
    pub upstream_session: u64,
    pub start_ts: u64,
    pub end_ts: Option<u64>,
    pub ft: u32,
    pub status: u16,
    pub connect_ms: i32,
    pub header_ms: i32,
    pub bytes_received: u64,
}

pub const NGINX_DONE_TERMINATED: u16 = 0x01;
pub const NGINX_DONE_TIMEDOUT: u16 = 0x02;
pub const NGINX_DONE_CONN_ERROR: u16 = 0x04;
pub const NGINX_DONE_HEADER_SENT: u16 = 0x08;
pub const NGINX_DONE_KEEPALIVE: u16 = 0x10;
pub const NGINX_DONE_NO_START: u16 = 0x20;

pub const NGINX_TLS_REUSED: u8 = 0x01;
pub const NGINX_TLS_KTLS_TX: u8 = 0x02;
pub const NGINX_TLS_EARLY_DATA: u8 = 0x04;

pub const NGINX_CONN_SSL: u8 = 0x01;
pub const NGINX_CONN_PROXY_PROTOCOL: u8 = 0x02;

/// Top bit marks a non-connection nginx session (sample, worker).
pub const NGINX_SPECIAL_SESSION: u64 = 1 << 63;

/// One decoded `nginx` event.
#[derive(Debug, Clone)]
pub struct NginxEvent {
    pub ts: u64,
    pub cpu: u16,
    pub tid: i32,
    pub provider_id: u16,
    pub session_id: u64,
    pub event_id: u32,
    pub payload: NginxPayload,
}

impl NginxPayload {
    /// HTTP request (serial / stream id) the event belongs to. Cache and
    /// upstream events carry the main request's id even for subrequests.
    pub fn request(&self) -> Option<u32> {
        match self {
            NginxPayload::RequestStart { request, .. }
            | NginxPayload::ResponseHeaders { request, .. }
            | NginxPayload::RequestDone { request, .. }
            | NginxPayload::RequestMessage { request, .. }
            | NginxPayload::FileOpenFail { request, .. }
            | NginxPayload::CacheLookup(NginxCacheLookup { request, .. })
            | NginxPayload::UpstreamAttemptStart { request, .. }
            | NginxPayload::UpstreamAttemptEnd { request, .. } => Some(*request),
            _ => None,
        }
    }

    /// Connection-level (not request-level, not worker-wide) event.
    pub fn is_conn_event(&self) -> bool {
        matches!(
            self,
            NginxPayload::ConnStart { .. }
                | NginxPayload::ConnEnd { .. }
                | NginxPayload::TlsHandshakeDone(_)
                | NginxPayload::TlsHandshakeFail(_)
                | NginxPayload::SendfileBusyDefer { .. }
                | NginxPayload::SendfileError { .. }
        )
    }
}

/// Per-request identity accumulated from REQUEST_START / RESPONSE_HEADERS /
/// REQUEST_DONE and the request-tagged cache/upstream/message events. Keyed
/// by `(provider_id, session_id, request)`. A request still open at capture
/// end has `done_ts == None`.
#[derive(Debug, Clone)]
pub struct NginxRequestInfo {
    pub provider_id: u16,
    pub session_id: u64,
    pub request: u32,
    pub method: String,
    pub uri: String,
    pub range_start: i64,
    pub range_end: i64,
    pub start_ts: u64,
    pub headers_ts: Option<u64>,
    pub done_ts: Option<u64>,
    pub status: Option<u16>,
    pub content_length: Option<i64>,
    pub bytes_sent: Option<u64>,
    pub event_count: usize,
    pub dev: Option<u64>,
    pub detail: Option<NginxDoneDetail>,
    pub messages: Vec<String>,
    /// Main-request cache lookup (subrequest lookups are not kept here).
    pub cache: Option<NginxCacheLookup>,
    pub upstream: Vec<NginxUpstreamAttempt>,
    /// `(status, errno, syscall)` from FILE_OPEN_FAIL.
    pub open_fail: Option<(u16, i32, String)>,
}

impl NginxRequestInfo {
    pub fn new(provider_id: u16, session_id: u64, request: u32, ts: u64) -> Self {
        Self {
            provider_id,
            session_id,
            request,
            method: String::new(),
            uri: String::new(),
            range_start: -1,
            range_end: -1,
            start_ts: ts,
            headers_ts: None,
            done_ts: None,
            status: None,
            content_length: None,
            bytes_sent: None,
            event_count: 0,
            dev: None,
            detail: None,
            messages: Vec::new(),
            cache: None,
            upstream: Vec::new(),
            open_fail: None,
        }
    }

    /// Wall-clock duration from REQUEST_START to REQUEST_DONE, when the
    /// request completed within the capture. `None` while still open.
    pub fn duration_us(&self) -> Option<u64> {
        self.done_ts.map(|d| d.saturating_sub(self.start_ts))
    }

    /// Status from REQUEST_DONE when present, else RESPONSE_HEADERS.
    pub fn status(&self) -> Option<u16> {
        self.status
    }
}

/// Per-connection state from CONN_START / CONN_END / TLS / disk events.
/// Keyed by `(provider_id, session_id)`.
#[derive(Debug, Clone, Default)]
pub struct NginxConnInfo {
    pub provider_id: u16,
    pub session_id: u64,
    pub start_ts: Option<u64>,
    pub end_ts: Option<u64>,
    pub server_port: u16,
    /// `NGINX_CONN_*` bits.
    pub flags: u8,
    pub close_reason: Option<String>,
    /// From CONN_END: requests and response bytes nginx counted.
    pub requests: u32,
    pub bytes_sent: u64,
    pub duration_ms: u32,
    pub tls: Option<NginxTlsInfo>,
    pub tls_fail: Option<NginxTlsFail>,
    pub busy_defers: u32,
    pub busy_defer_ms: u64,
    pub sendfile_errors: u32,
}

/// All `nginx` data from a capture. `is_empty()` gates the nginx System-tab
/// panels and the request list.
#[derive(Debug, Clone, Default)]
pub struct NginxProfile {
    pub requests: FxHashMap<(u16, u64, u32), NginxRequestInfo>,
    pub conns: FxHashMap<(u16, u64), NginxConnInfo>,
    pub events: Vec<NginxEvent>,
}

impl NginxProfile {
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty() && self.conns.is_empty() && self.events.is_empty()
    }
}

// ---------------------------------------------------------------------------
// "if" provider profile (interface lifecycle + periodic counter samples).
//
// ifindex is the primary key. Interface metadata (name, driver, mtu, …)
// is folded into `interfaces` from IF_ATTACH and mutated by the rename /
// link / flags / mtu / caps change events; periodic counter samples
// (IF_SAMPLE, or its IF_SAMPLE_BYTES/IF_SAMPLE_PACKETS successors) land
// in `samples`. All counter fields are CUMULATIVE (since attach / last
// reset) — consumers diff successive samples per ifindex to recover
// per-period rates.
// ---------------------------------------------------------------------------

/// Per-interface metadata, seeded by `IF_ATTACH` and updated in place by
/// the rename / link / flags / mtu / caps change events.
#[derive(Debug, Clone, Default)]
pub struct IfInterface {
    pub ifindex: u16,
    pub name: String,
    pub if_type: u8,
    pub driver: String,
    pub mtu: u32,
    pub baudrate: u64,
    /// Hardware (link-layer) address bytes, if carried by IF_ATTACH.
    pub hwaddr: Vec<u8>,
    /// Most recent link state seen (`link_state` enum: 0 UNKNOWN/1 DOWN/2 UP).
    pub link_state: u8,
    /// Most recent interface flags (from IF_FLAGS_CHANGE), if seen.
    pub flags: u32,
    /// True once an IF_DETACH for this ifindex has been observed.
    pub detached: bool,
}

/// One periodic per-interface counter sample. All fields are cumulative
/// byte/packet/error counters.
///
/// Populated from either the deprecated `IF_SAMPLE` (id 20, one event per
/// tick) or the pair `IF_SAMPLE_BYTES` + `IF_SAMPLE_PACKETS` (ids 21/22,
/// merged into one record by `core.rs` -- see the merge logic there) that
/// replaced it. `tcp_*` / `udp_*` / `other_*` are the protocol split
/// carried only by the new pair; captures with just the legacy event read
/// as 0, which callers should treat as "no protocol data", not "no
/// traffic".
///
/// `other_*` counts every non-TCP/UDP IP packet (ICMP, etc). On a schema
/// new enough to carry it (see `if_eventlog_schema.src`'s `other_ibytes`
/// field), it's a direct kernel counter bumped at the same IP-dispatch
/// call site as tcp/udp, so `tcp_* + udp_* + other_*` is self-consistent
/// by construction -- unlike `ibytes`/`obytes`, which on a multi-port
/// aggregate interface (lagg) are a live sum of member-port driver
/// counters and can diverge from that sum per-tick (NCD-10070). On an
/// older schema that predates the field, `core.rs` falls back to
/// deriving it as `ibytes - tcp_ibytes - udp_ibytes`, matching this
/// struct's pre-NCD-10070 behavior.
#[derive(Debug, Clone, Copy, Default)]
pub struct IfSample {
    pub ts: u64,
    pub ifindex: u16,
    pub ibytes: u64,
    pub obytes: u64,
    pub ipackets: u64,
    pub opackets: u64,
    pub ierrors: u64,
    pub oerrors: u64,
    pub iqdrops: u64,
    pub oqdrops: u64,
    pub imcasts: u64,
    pub omcasts: u64,
    pub noproto: u64,
    pub tcp_ibytes: u64,
    pub tcp_obytes: u64,
    pub udp_ibytes: u64,
    pub udp_obytes: u64,
    pub tcp_ipackets: u64,
    pub tcp_opackets: u64,
    pub udp_ipackets: u64,
    pub udp_opackets: u64,
    pub other_ibytes: u64,
    pub other_obytes: u64,
    pub other_ipackets: u64,
    pub other_opackets: u64,
}

/// A discrete (non-sample) interface event, retained as a timeline of
/// lifecycle / link / config / address changes for the GUI + CLI.
#[derive(Debug, Clone)]
pub struct IfEvent {
    pub ts: u64,
    pub ifindex: u16,
    pub event_id: u32,
    pub detail: String,
}

/// All `if`-provider data collected from a capture. `is_empty()` gates
/// the Interfaces view in the GUI.
#[derive(Debug, Clone, Default)]
pub struct IfProfile {
    /// ifindex -> interface metadata.
    pub interfaces: FxHashMap<u16, IfInterface>,
    pub samples: Vec<IfSample>,
    pub events: Vec<IfEvent>,
    /// Configured sample period in microseconds (from IF_INFO; 0 if no
    /// sampler armed). Last value seen wins.
    pub period_us: u32,
    /// Number of interfaces reported by the most recent IF_INFO.
    pub nifs: u16,
}

impl IfProfile {
    pub fn is_empty(&self) -> bool {
        self.interfaces.is_empty() && self.samples.is_empty() && self.events.is_empty()
    }

    /// Best-effort interface label for an ifindex: the attached name if
    /// known, else `ifN`.
    pub fn name_for(&self, ifindex: u16) -> String {
        self.interfaces
            .get(&ifindex)
            .filter(|i| !i.name.is_empty())
            .map(|i| i.name.clone())
            .unwrap_or_else(|| format!("if{ifindex}"))
    }
}

// ---------------------------------------------------------------------------
// CPU profile (collected from the "cpu" provider — flame-graph input)
// ---------------------------------------------------------------------------

/// One per-CPU stack sample as emitted by `EVT_CPU_SAMPLE` (event id 3).
///
/// `frames` is in **leaf-first** order, matching the kernel schema. The
/// flame-graph builder reverses this so the synthetic root sits at the
/// bottom of the stack, which is the standard flame-graph orientation.
/// Idle samples carry an empty `frames` vector and `CPU_SAMPLE_FLAG_IDLE`
/// in `flags`; truncated samples carry `CPU_SAMPLE_FLAG_TRUNC`.
#[derive(Debug, Clone)]
pub struct CpuStackSample {
    pub ts: u64,
    /// Sampling CPU. Equal to the per-event header's `session_id` (one
    /// eventlog session per CPU) but duplicated here so post-parse
    /// consumers don't need the session map.
    pub cpu: u16,
    pub tid: u32,
    pub pid: u32,
    pub sample_type: u8,
    pub flags: u8,
    /// Effective CPU frequency (MHz) at sample time, derived kernel-side
    /// from APERF/MPERF. `None` on captures predating this field or when
    /// `CPU_SAMPLE_FLAG_FREQ_VALID` is clear (e.g. no APERF/MPERF).
    pub freq_mhz: Option<u16>,
    pub frames: Vec<u64>,
}

/// Thread metadata captured from `EVT_CPU_THREAD_CREATE` (event id 1).
/// One entry per unique tid observed during the capture; used by the
/// flame-graph view to annotate per-thread / per-process aggregates.
/// `comm` is the process command name (`p_comm`) at fork / dump time;
/// after `execve` the image name lives in [`CpuProfile::proc_execs`].
/// `tdname` is the per-thread name (`td_name`, e.g. `"swi5: clock"`).
#[derive(Debug, Clone)]
pub struct ThreadInfo {
    pub tid: u32,
    pub pid: u32,
    pub comm: String,
    pub tdname: String,
}

/// One `do_execve` for a pid, from `PROC_EXEC`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcExec {
    pub ts: u64,
    pub comm: String,
    /// Set when `PROC_EXIT` closes this image.
    pub end_ts: Option<u64>,
}

/// Kernel metadata captured from `EVT_CPU_KERNEL_INFO` (event id 0).
/// Re-emitted on every subscriber attach via `dump_state`, so we keep
/// the *first* one we see (subsequent dumps are byte-identical).
#[derive(Debug, Clone, Default)]
pub struct KernelInfo {
    /// Address of `btext` (kernel text segment start). Stable for the
    /// lifetime of the booted kernel; consumers can subtract this from
    /// each sample frame to get a kernel-relative offset suitable for
    /// `addr2line` / `kgdb` / `nm`.
    pub text_base: u64,
    /// Size of the kernel text segment (`etext - btext`, bytes).
    pub text_size: u64,
    pub ncpus: u32,
    /// Kernel tick rate in Hz (`kern.hz`) at the time the session was
    /// created. NOT the sampler rate -- see [`Self::pmchz`] and
    /// [`Self::sample_hz`].
    pub hz: u32,
    /// hwpmc sampler frequency in Hz (`kern.eventlog.cpu.pmchz`) at the
    /// time the session was created. Split out from `hz` (which used to
    /// double as the sampler rate before `pmchz` became independently
    /// configurable) on split-schema captures; 0 on legacy captures that
    /// predate the field, where `hz` was in fact the sampler rate.
    pub pmchz: u32,
    /// Rated max CPU frequency in MHz from the cpu provider's
    /// `CPU_INFO.freq_max_mhz`. 0 when the capture predates that field.
    pub freq_max_mhz: u16,
    /// 40-character lowercase hex `.note.gnu.build-id` of the running
    /// kernel (matches `kern.build_id`). Empty when the kernel was not
    /// linked with `--build-id` or when the capture pre-dates the
    /// schema field (legacy 88-byte payload). `elog --embed-symbols`
    /// uses this to verify it opened the right `/boot/kernel/kernel`.
    pub build_id: String,
    /// Kernel ident (FreeBSD `ostype`, e.g. `"FreeBSD"`). 64 bytes max.
    pub ident: String,
}

impl KernelInfo {
    /// The rate `cpu` provider samples actually arrive at: `pmchz` when
    /// present (current captures, where it's independently configurable
    /// via `kern.eventlog.cpu.pmchz`), else `hz` (legacy captures, where
    /// the sampler rode the kernel tick).
    pub fn sample_hz(&self) -> u32 {
        if self.pmchz > 0 {
            self.pmchz
        } else {
            self.hz
        }
    }

    /// Rated max frequency from `CPU_INFO.freq_max_mhz`, if present.
    pub fn rated_freq_mhz(&self) -> Option<u16> {
        (self.freq_max_mhz > 0).then_some(self.freq_max_mhz)
    }
}

/// One loaded linker file (the kernel itself or a `.ko` module),
/// captured from `EVT_CPU_LINKER_FILE` (event id 5). The kernel emits
/// one of these per file on subscriber attach (`-D`).
///
/// `(base..base+size)` is the runtime VA range; `find_for_pc` uses it
/// to route a sample frame to its owning file. `pathname` is the
/// absolute on-disk location elog(1) opened to resolve symbols;
/// EventExplorer doesn't open it directly (symbols arrive in-band as
/// `SYMBOL_RANGE` events) but surfaces it in the GUI for context.
#[derive(Debug, Clone)]
pub struct LinkerFile {
    pub file_id: u32,
    pub base: u64,
    pub size: u64,
    /// Bitmask of `LINKER_FILE_FLAG_*`. The KERNEL bit distinguishes
    /// the kernel image from loadable modules.
    pub flags: u32,
    /// 40-char lowercase hex build-id, or empty for modules in v1
    /// (per-module build-id parsing is a future enhancement noted in
    /// `kern_cpu_eventlog.c`).
    pub build_id: String,
    /// Short filename (`linker_file::filename`), e.g. `"kernel"` or
    /// `"if_em.ko"`. 32 bytes max on the wire.
    pub filename: String,
    /// Absolute pathname (`linker_file::pathname`), e.g.
    /// `"/boot/kernel/kernel"`. 128 bytes max on the wire.
    pub pathname: String,
}

impl LinkerFile {
    pub fn is_kernel(&self) -> bool {
        (self.flags & LINKER_FILE_FLAG_KERNEL) != 0
    }
}

/// One resolved function symbol synthesised by `elog --embed-symbols`,
/// captured from `EVT_CPU_SYMBOL_RANGE` (event id 6). `offset` is the
/// function start *relative to its owning [`LinkerFile`]'s `base`*
/// (matching how the kernel exposes addresses for the kernel image —
/// `st_value - lf->base`); `size` is `st_size` from the on-disk ELF.
/// Stored in a per-file vector that the parser sorts by `offset` so
/// runtime lookup can binary-search.
#[derive(Debug, Clone)]
pub struct SymbolRange {
    pub offset: u64,
    pub size: u32,
    pub name: String,
}

/// One backing ELF referenced by any [`ProcMap`], captured from
/// `EVT_CPU_USER_FILE` (event id 7). `user_file_id` is a synthetic
/// counter minted by `elog(1)` (not stable across captures) — a
/// single shared library mapped by N processes is emitted *once*
/// and N [`ProcMap`]s reference it, so the per-file
/// [`UserSymbolRange`] table is shared across processes that share
/// the DSO.
#[derive(Debug, Clone)]
pub struct UserFile {
    pub user_file_id: u32,
    /// 40-char lowercase hex `.note.gnu.build-id`, or empty when
    /// the ELF was not linked with `--build-id`.
    pub build_id: String,
    /// Absolute on-disk path elog(1) opened to resolve symbols
    /// (e.g. `/lib/libc.so.7`). May be truncated to 255 chars.
    pub pathname: String,
}

impl UserFile {
    /// `basename(pathname)` — short label for tooltips and
    /// "show module" GUI mode. Falls back to `?` for empty paths.
    pub fn short_name(&self) -> &str {
        if self.pathname.is_empty() {
            return "?";
        }
        match self.pathname.rsplit_once('/') {
            Some((_, base)) => base,
            None => self.pathname.as_str(),
        }
    }
}

/// One `SYMBOL_PACK_REF` (event id 7 on the `symbols` provider): binds
/// a capture-local file id to the content-addressed pack that carries
/// that binary's function ranges. `kind` selects the namespace
/// (`0` = [`LinkerFile::file_id`], `1` = [`UserFile::user_file_id`]);
/// `id_kind` 0 means `pack_id` is the binary's GNU build-id (lowercase
/// hex, matching `default/sde-elog/v1/symbols/<pack_id>`).
#[derive(Debug, Clone)]
pub struct SymbolPackRef {
    pub kind: u8,
    pub id_kind: u8,
    pub file_id: u32,
    pub pack_id: String,
}

impl SymbolPackRef {
    pub const KIND_LINKER: u8 = 0;
    pub const KIND_USER: u8 = 1;
    pub const ID_GNU_BUILD_ID: u8 = 0;
}

/// One file-backed `vm_map_entry` for a pid, captured from
/// `EVT_CPU_PROC_MAP` (event id 8) / the `process` provider's `PROC_MAP`.
/// The consumer-side resolver formula is:
///
/// ```text
/// pc_file_offset = pc - pm.base + pm.offset
/// ```
///
/// Both sides of this formula live in on-disk **file-offset**
/// coordinates, not ELF virtual addresses: `pm.offset` is the raw
/// `vm_map_entry::offset` the kernel captured at mmap() time (no
/// ELF-parsing needed, so it's always exact), and `elog -U` computes
/// each [`UserSymbolRange::offset`] as `st_value - p_vaddr + p_offset`
/// of the covering `PT_LOAD` — i.e. the same file-offset space. This
/// sidesteps having to reconstruct a `p_vaddr`/load-bias from either
/// side. Consumers binary search the per-`user_file_id` symbol table
/// (sorted by `offset`) for a [`UserSymbolRange`] whose
/// `[offset, offset+size)` covers `pc_file_offset`.
///
/// The per-pid `Vec<ProcMap>` is sorted by `base` at parse time so
/// `find_for_pc` can short-circuit, but typical pids only have a
/// handful of file-backed mappings (executable + a few DSOs) so
/// linear scan stays competitive.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcMap {
    pub pid: u32,
    pub user_file_id: u32,
    /// Runtime VA where the mapping begins.
    pub base: u64,
    /// Mapping length in bytes.
    pub size: u64,
    /// On-disk file offset (`vm_map_entry::offset`) backing this
    /// mapping — required to translate a runtime VA to the matching
    /// file-offset coordinate when looking up symbols.
    pub offset: u64,
}

/// One resolved function symbol from a user-space ELF, captured from
/// `EVT_CPU_USER_SYMBOL_RANGE` (event id 9). `offset` is the symbol's
/// on-disk file offset (`st_value - p_vaddr + p_offset` of the
/// covering `PT_LOAD`, computed by `elog -U` so it shares
/// [`ProcMap::offset`]'s coordinate space — see the comment there).
/// The table is per-file, *not* per-pid — processes that share a DSO
/// share the same symbols. `size` is the raw `st_size`; user-space asm
/// helpers (pthread stubs, hand-written `memcpy` variants, ifunc
/// resolvers) frequently arrive with `size == 0`. The same post-sort
/// fill-in pass used for kernel [`SymbolRange`] is applied here.
#[derive(Debug, Clone)]
pub struct UserSymbolRange {
    pub user_file_id: u32,
    pub offset: u64,
    pub size: u32,
    pub name: String,
}

/// One source file interned by `elog -L`, captured from symbols-provider
/// `SOURCE_FILE`. `source_file_id` is capture-local. `pathname` is the
/// path DWARF recorded (a build-machine path, often absolute).
/// `commit_id` is a 40-character hex commit when the capture recorded
/// one, and empty when it did not.
#[derive(Debug, Clone)]
pub struct EmbeddedSourceFile {
    pub source_file_id: u32,
    pub binary_kind: u8,
    pub binary_id: u32,
    pub commit_id: String,
    pub pathname: String,
}

/// One instruction-to-line mapping from symbols-provider `SOURCE_LINE`.
/// `file_offset` shares the `SYMBOL_RANGE` / `USER_SYMBOL_RANGE`
/// coordinate space. Stored per `(binary_kind, binary_id)`, sorted by
/// `file_offset`.
#[derive(Debug, Clone)]
pub struct EmbeddedSourceLine {
    pub source_file_id: u32,
    pub file_offset: u64,
    pub line: u32,
}

/// Result of resolving a single program counter against a
/// [`CpuProfile`].
///
/// The two variants share the same [`label`](Self::label) /
/// [`label_with_file`](Self::label_with_file) shape so the renderer
/// can format any frame uniformly without caring whether it landed
/// in kernel or user space; callers that want richer info (e.g.
/// surfacing the owning DSO in a tooltip) can match on the variant.
/// `inner_offset` is the displacement from the function's start, in
/// bytes.
#[derive(Debug, Clone)]
pub enum SymbolHit<'a> {
    /// Kernel resolution: `pc` matched some [`LinkerFile`] and a
    /// [`SymbolRange`] in that file's table covered the offset.
    Kernel {
        file: &'a LinkerFile,
        symbol: Cow<'a, SymbolRange>,
        inner_offset: u64,
    },
    /// User-space resolution: `(pid, pc)` matched some [`ProcMap`]
    /// and a [`UserSymbolRange`] in the corresponding [`UserFile`]'s
    /// table covered the resulting file offset.
    User {
        file: &'a UserFile,
        map: &'a ProcMap,
        symbol: Cow<'a, UserSymbolRange>,
        inner_offset: u64,
    },
    /// Module-only fallback: `(pid, pc)` matched a [`ProcMap`] (we
    /// know which DSO the PC fell in) but no [`UserSymbolRange`]
    /// covered the resulting file offset. Surfacing the binary's
    /// basename + the offset within its runtime mapping is far
    /// more useful than a bare `0xADDR` — the user can at least
    /// see which library is hot.
    ///
    /// In practice this fires for two reasons:
    ///
    /// 1. The owning DSO's symbol table on the wire doesn't cover the
    ///    PC — e.g. a stripped region the DWARF walk in `elog -U`
    ///    couldn't attribute to any function.
    /// 2. Less commonly, the PC is in a legitimate non-symbolized
    ///    region of the binary (PLT stubs, padding, etc.).
    ///
    /// `runtime_offset` is `pc - map.base` — the displacement from
    /// the runtime mapping's start, not the file offset. This is the
    /// most stable thing to surface because it doesn't depend on the
    /// symbol-table lookup that just missed.
    UserModuleOnly {
        file: &'a UserFile,
        map: &'a ProcMap,
        runtime_offset: u64,
    },
}

impl<'a> SymbolHit<'a> {
    /// Resolved function name. Cheap accessor used by both label
    /// builders and by callers (e.g. flame-graph search) that just
    /// want to compare names. For [`Self::UserModuleOnly`] there's
    /// no function symbol, so we return the DSO basename — keeps
    /// substring searches like `"libssl"` working uniformly.
    pub fn name(&self) -> &str {
        match self {
            SymbolHit::Kernel { symbol, .. } => &symbol.name,
            SymbolHit::User { symbol, .. } => &symbol.name,
            SymbolHit::UserModuleOnly { file, .. } => file.short_name(),
        }
    }

    /// Displacement from the symbol's start address, in bytes. For
    /// [`Self::UserModuleOnly`] there's no function symbol, so this
    /// returns the offset within the runtime mapping instead — the
    /// label cascade treats both uniformly so callers don't need a
    /// special case.
    pub fn inner_offset(&self) -> u64 {
        match self {
            SymbolHit::Kernel { inner_offset, .. } => *inner_offset,
            SymbolHit::User { inner_offset, .. } => *inner_offset,
            SymbolHit::UserModuleOnly { runtime_offset, .. } => *runtime_offset,
        }
    }

    /// True when this hit carries an actual function symbol (kernel
    /// or user). [`Self::UserModuleOnly`] returns false. The GUI
    /// uses this to suppress the function-name prefix in
    /// [`Self::label_with_file`] (no point showing
    /// `libfoo.so:libfoo.so+0x123`).
    pub fn has_symbol(&self) -> bool {
        !matches!(self, SymbolHit::UserModuleOnly { .. })
    }

    /// Short name of the owning binary — `LinkerFile.filename` for
    /// kernel hits (e.g. `"kernel"`, `"if_em.ko"`), `basename` of
    /// the user file's `pathname` for user hits (e.g. `"libc.so.7"`,
    /// `"a.out"`). Used by [`Self::label_with_file`] and by GUI
    /// tooltips.
    pub fn file_short(&self) -> &str {
        match self {
            SymbolHit::Kernel { file, .. } => {
                if file.filename.is_empty() {
                    "?"
                } else {
                    file.filename.as_str()
                }
            }
            SymbolHit::User { file, .. } => file.short_name(),
            SymbolHit::UserModuleOnly { file, .. } => file.short_name(),
        }
    }

    /// Default short label: `name+0xINNER` (or just `name` for
    /// inner_offset == 0). The owning file is intentionally elided
    /// to keep the bottom-row labels short — flame-graph bars for
    /// kernel functions get crowded fast. Callers that want a file
    /// prefix can use [`Self::label_with_file`].
    ///
    /// For [`Self::UserModuleOnly`] the "name" *is* the DSO basename
    /// (we have no function symbol), so the result already reads
    /// like the kernel-text fallback (`libcksum32.so.0+0x43d9`,
    /// matching `kernel+0x1234`).
    pub fn label(&self) -> String {
        let inner = self.inner_offset();
        let name = self.name();
        if inner == 0 {
            name.to_string()
        } else {
            format!("{name}+{inner:#x}")
        }
    }

    /// Labelled with the owning file's short name as a prefix:
    /// `kernel:tcp_input+0x1234`, `if_em.ko:em_isr+0x40`,
    /// `libc.so.7:recvfrom+0x12`, or `a.out:main+0x40`. Used by the
    /// GUI when the user explicitly asks to show the module origin
    /// (e.g. to disambiguate same-named symbols across modules /
    /// DSOs, or to see "is this in libc or in libssl?").
    ///
    /// For [`Self::UserModuleOnly`] the file basename is already in
    /// the default label, so we don't double it up — same string as
    /// [`Self::label`].
    pub fn label_with_file(&self) -> String {
        if !self.has_symbol() {
            return self.label();
        }
        let inner = self.inner_offset();
        let name = self.name();
        let file = self.file_short();
        if inner == 0 {
            format!("{file}:{name}")
        } else {
            format!("{file}:{name}+{inner:#x}")
        }
    }
}

/// Symbol table shared by every clone of a [`CpuProfile`].
///
/// `Arc<CpuProfile>` clones (the CPU strips, a flame rebuild) alias the
/// same allocation, and this table aliases again so a pack install is
/// visible through all of them. Cloning the profile struct shares the
/// table too.
#[derive(Debug, Clone)]
pub struct SharedSymbolTable<T> {
    inner: Arc<Mutex<FxHashMap<u32, Vec<T>>>>,
}

impl<T> Default for SharedSymbolTable<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FxHashMap::default())),
        }
    }
}

impl<T> SharedSymbolTable<T> {
    fn lock(&self) -> std::sync::MutexGuard<'_, FxHashMap<u32, Vec<T>>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn insert(&self, file_id: u32, ranges: Vec<T>) {
        self.lock().insert(file_id, ranges);
    }

    fn len(&self) -> usize {
        self.lock().values().map(|v| v.len()).sum()
    }

    pub(crate) fn contains(&self, file_id: u32) -> bool {
        self.lock().get(&file_id).is_some_and(|v| !v.is_empty())
    }

    pub(crate) fn append(&self, other: &Self) {
        if Arc::ptr_eq(&self.inner, &other.inner) {
            return;
        }
        let drained = std::mem::take(&mut *other.lock());
        self.lock().extend(drained);
    }
}

impl<T: Clone> SharedSymbolTable<T> {
    fn lookup(
        &self,
        file_id: u32,
        offset: u64,
        sym_off: impl Fn(&T) -> u64,
        sym_size: impl Fn(&T) -> u32,
    ) -> Option<(T, u64)> {
        let guard = self.lock();
        let (sym, inner) = covering(guard.get(&file_id)?, offset, sym_off, sym_size)?;
        Some((sym.clone(), inner))
    }
}

fn covering<T>(
    syms: &[T],
    offset: u64,
    sym_off: impl Fn(&T) -> u64,
    sym_size: impl Fn(&T) -> u32,
) -> Option<(&T, u64)> {
    let idx = syms.partition_point(|s| sym_off(s) <= offset);
    if idx == 0 {
        return None;
    }
    let sym = &syms[idx - 1];
    let start = sym_off(sym);
    if offset >= start.saturating_add(sym_size(sym) as u64) {
        return None;
    }
    Some((sym, offset - start))
}

/// Aggregate of everything the `cpu` provider contributes to a capture.
/// Empty when the file has no `cpu` provider events (the common case
/// for tcp-only / hpts-only captures).
#[derive(Debug, Clone, Default)]
pub struct CpuProfile {
    /// All `CPU_SAMPLE` events, sorted by timestamp.
    pub samples: Vec<CpuStackSample>,
    /// `tid -> ThreadInfo` lookup populated from `THREAD_CREATE`
    /// events. Misses are common (the kernel only emits THREAD_CREATE
    /// the first time it sees a tid on each CPU's session) so callers
    /// must be tolerant of tids missing from this map. Upsert: see the
    /// identity-maps note in `parse_capture`
    /// (`elog_parser/core.rs`) on `EVENTLOG_IOCTL_TRIGGER_DUMP_STATE`.
    /// `comm` is the name at fork / dump time; use
    /// [`Self::process_comm_at`] for the image name at a sample.
    pub threads: FxHashMap<u32, ThreadInfo>,
    /// `pid -> PROC_EXEC` history in capture-time order. `end_ts` is
    /// set on `PROC_EXIT`. Sorted by `ts` at parse finalize.
    pub proc_execs: FxHashMap<u32, Vec<ProcExec>>,
    /// Most recent `KERNEL_INFO` seen in the capture (upsert: see the
    /// identity-maps note in `elog_parser/core.rs`). `None` when the
    /// capture is malformed or pre-dates the event.
    pub kernel: Option<KernelInfo>,
    /// `file_id -> LinkerFile` lookup populated from `LINKER_FILE`
    /// events. Upsert (see the identity-maps note in
    /// `elog_parser/core.rs`): a repeat sighting of `file_id` replaces
    /// the entry and clears `symbols[file_id]`, which is then
    /// repopulated by the `SYMBOL_RANGE`s that accompany the same
    /// re-dump. Empty for captures that pre-date the event (falls back
    /// to KERNEL_INFO-only resolution).
    pub linker_files: FxHashMap<u32, LinkerFile>,
    /// `file_id -> sorted-by-offset SymbolRange` lookup populated from
    /// `SYMBOL_RANGE` events synthesised by `elog --embed-symbols`. The
    /// per-file vector is sorted at parse time so runtime lookup can
    /// `binary_search_by`. Cleared per-`file_id` on a `LINKER_FILE`
    /// re-sighting (see `linker_files`) before being rebuilt.
    pub symbols: FxHashMap<u32, Vec<SymbolRange>>,
    /// `user_file_id -> UserFile` lookup populated from `USER_FILE`
    /// events synthesised by `elog -U`. Upsert (see the identity-maps
    /// note in `elog_parser/core.rs`): a repeat sighting of
    /// `user_file_id` replaces the entry and clears
    /// `user_symbols[user_file_id]`, mirroring `linker_files`/
    /// `symbols`. Empty for captures made without `-U`.
    pub user_files: FxHashMap<u32, UserFile>,
    /// `pid -> sorted-by-base ProcMap` lookup populated from
    /// `PROC_MAP` events, plus `PROC_FORK` inheritance (see
    /// `apply_proc_fork`). One entry per file-backed executable
    /// mapping the pid had at the time `elog` last dumped its state
    /// for that pid. **Lazy snapshot**: libraries `dlopen`d after that
    /// point are not reflected until the next dump (either the next
    /// `--roll-time` re-trigger, or a future revision of `elog` that
    /// re-snapshots on unknown PCs). Re-dumps are deduplicated
    /// per-entry on push (see the identity-maps note in
    /// `elog_parser/core.rs`) rather than upserted by key, since a
    /// live pid's map set is legitimately multi-valued.
    pub proc_maps: FxHashMap<u32, Vec<ProcMap>>,
    /// `user_file_id -> sorted-by-offset UserSymbolRange` lookup
    /// populated from `USER_SYMBOL_RANGE` events. Stored in on-disk
    /// file-offset coordinates (see [`ProcMap::offset`]), so the table
    /// is shared across every process that maps the same DSO. The
    /// per-file vector is
    /// sorted by `offset` at parse time and zero-sized entries are
    /// extended to span up to the next entry's offset (mirroring
    /// the kernel-side fill-in pass) so `symbolize` can binary
    /// search without special cases.
    pub user_symbols: FxHashMap<u32, Vec<UserSymbolRange>>,
    /// Pack ranges for linker files. Shared by every `Arc` clone of
    /// this profile, including the CPU strips that keep a clone for
    /// the life of the capture.
    pub pack_symbols: SharedSymbolTable<SymbolRange>,
    /// Pack ranges for user files. Same sharing rule as
    /// [`Self::pack_symbols`].
    pub pack_user_symbols: SharedSymbolTable<UserSymbolRange>,
    /// `SYMBOL_PACK_REF` events seen at dump-state, in stream order.
    /// Resolution order for a file is: in-file ranges, then the pack
    /// named here (filled by `elog_parser::packs::install_packs`),
    /// then the raw `kernel+0xOFF` fallback.
    pub pack_refs: Vec<SymbolPackRef>,
    /// `source_file_id -> EmbeddedSourceFile` from `elog -L`.
    pub source_files: FxHashMap<u32, EmbeddedSourceFile>,
    /// `(binary_kind, binary_id) -> SOURCE_LINE` rows sorted by
    /// `file_offset`. Kind 0 is a linker file id; kind 1 is a user file id.
    pub source_lines: FxHashMap<(u8, u32), Vec<EmbeddedSourceLine>>,
    /// Sum of `OVERFLOW.dropped` counters across CPUs. Currently always
    /// zero — the kernel reserves the event but the v1 sampler does
    /// not emit it. Surfaced for future use.
    pub overflow_dropped: u64,
}

/// Path basename for a main executable. Shared libraries and empty
/// names are rejected so flame bars do not collapse onto `libc.so.7`.
fn exe_basename(pathname: &str) -> Option<&str> {
    let base = pathname.rsplit('/').next().unwrap_or(pathname);
    if base.is_empty() || base.contains(".so") {
        return None;
    }
    Some(base)
}

/// Parent shells, and the interpreter's own comm. Any other thread
/// comm is the process name from dump time.
fn generic_fork_comm(comm: &str) -> bool {
    matches!(
        comm,
        "sh" | "csh" | "tcsh" | "bash" | "dash" | "ksh" | "perl"
    )
}

/// `PROC_MAP` names the interpreter inode `perl` for a hardlinked
/// exec. The dump-time thread comm is that process's name. A shell
/// parent still yields the mapped interpreter.
fn comm_from_exe_or_thread<'a>(
    exe: Option<&'a str>,
    fork_comm: Option<&'a str>,
) -> Option<&'a str> {
    let fork_comm = fork_comm.filter(|c| !c.is_empty());
    if exe == Some("perl") {
        if let Some(fc) = fork_comm {
            if !generic_fork_comm(fc) {
                return Some(fc);
            }
        }
    }
    exe.or(fork_comm)
}

impl CpuProfile {
    /// The pack id referenced for a linker `file_id`, when one was
    /// emitted and the file has no in-file ranges (in-file wins).
    pub fn pack_ref_for_linker(&self, file_id: u32) -> Option<&str> {
        if self.symbols.get(&file_id).is_some_and(|v| !v.is_empty()) {
            return None;
        }
        self.pack_refs
            .iter()
            .rev()
            .find(|r| r.kind == SymbolPackRef::KIND_LINKER && r.file_id == file_id)
            .map(|r| r.pack_id.as_str())
    }

    /// The pack id referenced for a `user_file_id`, when one was
    /// emitted and the file has no in-file ranges (in-file wins).
    pub fn pack_ref_for_user(&self, user_file_id: u32) -> Option<&str> {
        if self
            .user_symbols
            .get(&user_file_id)
            .is_some_and(|v| !v.is_empty())
        {
            return None;
        }
        self.pack_refs
            .iter()
            .rev()
            .find(|r| r.kind == SymbolPackRef::KIND_USER && r.file_id == user_file_id)
            .map(|r| r.pack_id.as_str())
    }

    /// True when the capture contains no cpu-provider data. Used by the
    /// GUI to decide whether to surface the flame-graph tab.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
            && self.threads.is_empty()
            && self.kernel.is_none()
            && self.linker_files.is_empty()
            && self.symbols.is_empty()
            && self.user_files.is_empty()
            && self.proc_maps.is_empty()
            && self.user_symbols.is_empty()
            && self.pack_refs.is_empty()
            && self.source_files.is_empty()
            && self.source_lines.is_empty()
            && self.overflow_dropped == 0
    }

    /// Total number of resolved kernel symbols across all linker
    /// files. Used by the GUI metadata strip and `elog_cli` file
    /// header.
    pub fn symbol_count(&self) -> usize {
        self.symbols.values().map(|v| v.len()).sum::<usize>() + self.pack_symbols.len()
    }

    /// Total number of resolved user-space symbols across all
    /// [`UserFile`] tables. The same DSO mapped by N processes is
    /// counted *once* (the per-`user_file_id` symbol table is
    /// shared), so this is "unique symbols on disk" not "symbols ×
    /// pids".
    pub fn user_symbol_count(&self) -> usize {
        self.user_symbols.values().map(|v| v.len()).sum::<usize>() + self.pack_user_symbols.len()
    }

    /// `SOURCE_LINE` rows across every binary. A re-dump of the same
    /// offset is collapsed at parse time, so this is unique mappings.
    pub fn source_line_count(&self) -> usize {
        self.source_lines.values().map(|v| v.len()).sum()
    }

    /// Exact embedded line for a sampled file offset. `file_offset` is
    /// the same coordinate `symbolize` reports as `sampled_offset`.
    pub fn embedded_source_line(
        &self,
        binary_kind: u8,
        binary_id: u32,
        file_offset: u64,
    ) -> Option<(&EmbeddedSourceFile, u32)> {
        let lines = self.source_lines.get(&(binary_kind, binary_id))?;
        let index = lines
            .binary_search_by_key(&file_offset, |line| line.file_offset)
            .ok()?;
        let hit = &lines[index];
        let file = self.source_files.get(&hit.source_file_id)?;
        Some((file, hit.line))
    }

    /// Total number of distinct file-backed mappings across all
    /// pids. Useful for a one-line summary header alongside
    /// [`Self::user_symbol_count`].
    pub fn proc_map_count(&self) -> usize {
        self.proc_maps.values().map(|v| v.len()).sum()
    }

    /// First `THREAD_CREATE` `comm` for `pid`. Prefer
    /// [`Self::process_comm_at`] / [`Self::process_comm_latest`] when
    /// the image name after `execve` matters.
    pub fn comm_for_pid(&self, pid: u32) -> Option<&str> {
        self.threads
            .values()
            .find(|t| t.pid == pid)
            .map(|t| t.comm.as_str())
    }

    /// Pid whose comm and mappings match this sample's stack.
    ///
    /// `THREAD_CREATE` records the thread's pid. A sample taken inside
    /// `fork` can carry the new child's pid on the parent's tid. The
    /// child's address space still contains the parent's perl mapping
    /// (`libperl.so` or a `/usr/local/ccsc/libexec/perl/` hardlink), so
    /// the parent's `Perl_pp_fork` stack is filed under the child
    /// (`elog`). When a frame resolves to that perl image and the
    /// child's comm is a different name, the stack belongs to the
    /// thread's pid. Samples in the child's own image (`ld-elf.so.1`,
    /// `/usr/bin/elog`) keep the stamped pid.
    pub fn sample_owner_pid(&self, tid: u32, pid: u32, ts: u64, frames: &[u64]) -> u32 {
        let Some(ti) = self.threads.get(&tid) else {
            return pid;
        };
        if ti.pid == 0 || ti.pid == pid {
            return pid;
        }
        let inherited = frames
            .iter()
            .any(|pc| self.inherited_perl_frame(pid, ts, *pc));
        if inherited {
            ti.pid
        } else {
            pid
        }
    }

    /// `pc` falls in perl inside a pid whose comm is some other executable.
    ///
    /// A perl script's own `libperl.so` frames stay put. `elog`'s
    /// address space still contains the parent's perl mapping for a
    /// few milliseconds around `exec`, and those frames belong to the
    /// thread's pid.
    fn inherited_perl_frame(&self, pid: u32, ts: u64, pc: u64) -> bool {
        let path = match self.symbolize(pc, Some(pid)) {
            Some(SymbolHit::User { file, .. } | SymbolHit::UserModuleOnly { file, .. }) => {
                file.pathname.as_str()
            }
            _ => return false,
        };
        let base = path.rsplit('/').next().unwrap_or(path);
        if base != "perl" && !base.starts_with("libperl.so") && !path.contains("/libexec/perl/") {
            return false;
        }
        self.comm_names_other_exe(pid, ts)
    }

    /// Comm in force at `ts` is a mapped executable other than perl.
    fn comm_names_other_exe(&self, pid: u32, ts: u64) -> bool {
        let Some(comm) = self.process_comm_at(pid, None, ts) else {
            return false;
        };
        let Some(maps) = self.proc_maps.get(&pid) else {
            return false;
        };
        maps.iter().any(|m| {
            let Some(f) = self.user_files.get(&m.user_file_id) else {
                return false;
            };
            let path = f.pathname.as_str();
            let base = path.rsplit('/').next().unwrap_or(path);
            base == comm
                && !base.contains(".so")
                && base != "perl"
                && !path.contains("/libexec/perl/")
        })
    }

    /// Process command name in force at `ts`: the latest `PROC_EXEC`
    /// at or before `ts` that is still open, else the main executable
    /// basename from `PROC_MAP`/`USER_FILE` (covers captures that miss
    /// live `PROC_EXEC`), else the `THREAD_CREATE` `comm`. A mapped
    /// `perl` path yields the thread comm when that comm is a process
    /// name.
    pub fn process_comm_at(&self, pid: u32, tid: Option<u32>, ts: u64) -> Option<&str> {
        if let Some(execs) = self.proc_execs.get(&pid) {
            let i = execs.partition_point(|e| e.ts <= ts);
            if i > 0 {
                let e = &execs[i - 1];
                let closed = e.end_ts.is_some_and(|end| ts >= end);
                if !closed && !e.comm.is_empty() {
                    return Some(e.comm.as_str());
                }
            }
        }
        let fork_comm = tid
            .and_then(|t| self.threads.get(&t))
            .map(|ti| ti.comm.as_str())
            .or_else(|| self.comm_for_pid(pid));
        comm_from_exe_or_thread(self.exe_comm_for_pid(pid, fork_comm), fork_comm)
    }

    /// Latest known process name for `pid` (open `PROC_EXEC`, else
    /// executable basename from maps, else `THREAD_CREATE`). A mapped
    /// `perl` path yields the thread comm when that comm is a process
    /// name.
    pub fn process_comm_latest(&self, pid: u32) -> Option<&str> {
        if let Some(execs) = self.proc_execs.get(&pid) {
            if let Some(e) = execs.last() {
                if e.end_ts.is_none() && !e.comm.is_empty() {
                    return Some(e.comm.as_str());
                }
            }
        }
        let fork_comm = self.comm_for_pid(pid);
        comm_from_exe_or_thread(self.exe_comm_for_pid(pid, fork_comm), fork_comm)
    }

    /// Basename of the pid's main executable from `PROC_MAP` /
    /// `USER_FILE`. Shared libraries (`.so`) are skipped. When fork
    /// inheritance left the parent's binary alongside a post-`exec`
    /// binary, prefer the name that differs from `fork_comm`.
    pub(crate) fn exe_comm_for_pid(&self, pid: u32, fork_comm: Option<&str>) -> Option<&str> {
        let maps = self.proc_maps.get(&pid)?;
        let mut names: Vec<(u64, &str)> = Vec::new();
        for m in maps {
            let Some(f) = self.user_files.get(&m.user_file_id) else {
                continue;
            };
            let Some(base) = exe_basename(&f.pathname) else {
                continue;
            };
            if names.iter().any(|(_, n)| *n == base) {
                continue;
            }
            names.push((m.base, base));
        }
        if names.is_empty() {
            return None;
        }
        if names.len() == 1 {
            return Some(names[0].1);
        }
        if let Some(fc) = fork_comm {
            let mut others: Vec<(u64, &str)> =
                names.iter().copied().filter(|(_, n)| *n != fc).collect();
            if others.len() == 1 {
                return Some(others[0].1);
            }
            if !others.is_empty() {
                others.sort_unstable_by_key(|(b, _)| *b);
                return Some(others[0].1);
            }
        }
        names.sort_unstable_by_key(|(b, _)| *b);
        Some(names[0].1)
    }

    /// Append a `PROC_EXEC` for `pid`. Sorted by `ts` at parse finalize.
    pub fn apply_proc_exec(&mut self, ts: u64, pid: u32, comm: String) {
        self.proc_execs.entry(pid).or_default().push(ProcExec {
            ts,
            comm,
            end_ts: None,
        });
    }

    /// Close the open `PROC_EXEC` for `pid` at `ts`.
    pub fn apply_proc_exit(&mut self, ts: u64, pid: u32) {
        let Some(execs) = self.proc_execs.get_mut(&pid) else {
            return;
        };
        if let Some(e) = execs
            .iter_mut()
            .rev()
            .find(|e| e.ts <= ts && e.end_ts.is_none())
        {
            e.end_ts = Some(ts);
        }
    }

    /// Apply a `PROC_FORK(child, parent)` edge: fork-without-exec
    /// inherits the parent's address space, so copy the parent's
    /// current `PROC_MAP` set onto the child pid. Called at the edge's
    /// position in the event stream, so mappings the parent adds later
    /// (post-fork `mmap`) do not leak into the child. No-op when the
    /// parent has no maps; `inherit_fork_proc_maps` is the fallback
    /// for captures that predate `PROC_FORK`.
    pub fn apply_proc_fork(&mut self, child: u32, parent: u32) {
        let Some(maps) = self.proc_maps.get(&parent) else {
            return;
        };
        let maps = maps.clone();
        let entries = self.proc_maps.entry(child).or_default();
        for mut m in maps {
            m.pid = child;
            if !entries.contains(&m) {
                entries.push(m);
            }
        }
    }

    /// Copy `PROC_MAP`s onto same-`comm` pids that have none, when every
    /// pid that already has maps for that comm shares one layout
    /// (`user_file_id`/`base`/`size`/`offset`). Forked workers (nginx
    /// reload) inherit ASLR from the master but the kernel only emits
    /// `THREAD_CREATE` for the child, so dump_state maps stay on the
    /// pre-reload pids. Donors that disagree on layout have distinct
    /// slides; leave those pids unmapped rather than guess. Fallback
    /// for captures without `PROC_FORK` edges — a child covered by
    /// `apply_proc_fork` already has maps and is skipped here.
    pub fn inherit_fork_proc_maps(&mut self) {
        let mut pids_by_comm: FxHashMap<String, Vec<u32>> = FxHashMap::default();
        for t in self.threads.values() {
            if t.pid == 0 || t.comm.is_empty() {
                continue;
            }
            pids_by_comm.entry(t.comm.clone()).or_default().push(t.pid);
        }
        for pids in pids_by_comm.values_mut() {
            pids.sort_unstable();
            pids.dedup();
        }
        for pids in pids_by_comm.into_values() {
            let donors: Vec<u32> = pids
                .iter()
                .copied()
                .filter(|p| self.proc_maps.get(p).is_some_and(|m| !m.is_empty()))
                .collect();
            if donors.is_empty() {
                continue;
            }
            let layout = |pid: u32| -> Vec<(u32, u64, u64, u64)> {
                let mut v: Vec<_> = self.proc_maps[&pid]
                    .iter()
                    .map(|m| (m.user_file_id, m.base, m.size, m.offset))
                    .collect();
                v.sort_unstable();
                v
            };
            let canon = layout(donors[0]);
            if donors.iter().any(|&p| layout(p) != canon) {
                continue;
            }
            let template = self.proc_maps[&donors[0]].clone();
            for pid in pids {
                if self.proc_maps.get(&pid).is_some_and(|m| !m.is_empty()) {
                    continue;
                }
                self.proc_maps.insert(
                    pid,
                    template
                        .iter()
                        .map(|m| ProcMap { pid, ..m.clone() })
                        .collect(),
                );
            }
        }
    }

    /// Resolve a runtime PC against the captured symbol metadata,
    /// trying the kernel path first and the per-pid user-space path
    /// second:
    ///
    /// 1. **Kernel** — find a [`LinkerFile`] whose `[base, base+size)`
    ///    range covers `pc`, then binary-search its
    ///    [`SymbolRange`] table → [`SymbolHit::Kernel`].
    /// 2. **User (full)** — when `pid` is provided and the kernel
    ///    path missed, find a [`ProcMap`] for that pid whose
    ///    `[base, base+size)` covers `pc`, translate to a file offset
    ///    via `pc - pm.base + pm.offset`, then binary-search the
    ///    corresponding [`UserFile`]'s [`UserSymbolRange`] table →
    ///    [`SymbolHit::User`].
    /// 3. **User (module only)** — same as 2 but the symbol search
    ///    missed; we still know which DSO the PC fell in →
    ///    [`SymbolHit::UserModuleOnly`].
    ///
    /// Returning `None` means callers should fall back to
    /// `kernel+0xOFFSET` (when [`KernelInfo`] is present and the PC
    /// is in the kernel text segment) or to the raw hex address.
    ///
    /// Looking the kernel up first keeps the common case
    /// (kernel-mode samples — by far the majority on a typical
    /// busy box) on the cheaper code path; the user lookup only
    /// runs for PCs that don't fall in any loaded `.ko` /
    /// `kernel`. Both halves are O(files) for the mapping pick
    /// (typically a few dozen / handful) plus O(log nsyms) for the
    /// symbol binary search.
    pub fn symbolize(&self, pc: u64, pid: Option<u32>) -> Option<SymbolHit<'_>> {
        if let Some(hit) = self.symbolize_kernel(pc) {
            return Some(hit);
        }
        let pid = pid?;
        self.symbolize_user(pc, pid)
    }

    /// Kernel half of [`Self::symbolize`]. Split out so callers that
    /// know the PC came from a specific source (e.g. the GUI's
    /// "show only kernel" filter) can opt out of the user fallback,
    /// and so the unit tests can assert each half independently.
    pub fn symbolize_kernel(&self, pc: u64) -> Option<SymbolHit<'_>> {
        let file = self
            .linker_files
            .values()
            .find(|lf| pc >= lf.base && pc < lf.base.saturating_add(lf.size))?;
        let offset = pc - file.base;
        let file_id = file.file_id;
        if let Some(syms) = self.symbols.get(&file_id) {
            if let Some((sym, inner_offset)) = covering(syms, offset, |s| s.offset, |s| s.size) {
                return Some(SymbolHit::Kernel {
                    file,
                    symbol: Cow::Borrowed(sym),
                    inner_offset,
                });
            }
        }
        self.pack_symbols
            .lookup(file_id, offset, |s| s.offset, |s| s.size)
            .map(|(symbol, inner_offset)| SymbolHit::Kernel {
                file,
                symbol: Cow::Owned(symbol),
                inner_offset,
            })
    }

    /// User half of [`Self::symbolize`]. The pid is required because
    /// runtime VAs are per-address-space — pid 100's `0x4000_0000`
    /// is unrelated to pid 200's `0x4000_0000` (different `vm_map`s,
    /// different ASLR slides).
    ///
    /// Three-tier outcome:
    ///
    /// 1. Some [`ProcMap`] covers the PC AND its owning
    ///    [`UserSymbolRange`] table contains a symbol for the
    ///    resulting file offset → [`SymbolHit::User`].
    /// 2. Some [`ProcMap`] covers the PC but no symbol matches →
    ///    [`SymbolHit::UserModuleOnly`] (we still know which DSO
    ///    the PC fell in, just not the function).
    /// 3. Otherwise → `None`. Callers should fall through to the
    ///    raw-hex tier; we couldn't even tell which binary this PC
    ///    came from, usually because the pid wasn't in `elog`'s
    ///    vm_map snapshot.
    pub fn symbolize_user(&self, pc: u64, pid: u32) -> Option<SymbolHit<'_>> {
        let maps = self.proc_maps.get(&pid)?;
        let pm = maps
            .iter()
            .find(|pm| pc >= pm.base && pc < pm.base.saturating_add(pm.size))?;
        let file = self.user_files.get(&pm.user_file_id)?;
        // Compute the runtime offset upfront — both the symbol-resolved
        // hit and the module-only fallback want it (the latter as the
        // displayed offset; the former is asserted to match what the
        // file-offset path computes).
        let runtime_offset = pc - pm.base;
        // `wrapping_sub`/`wrapping_add` because pm.offset can be
        // smaller than pm.base (it's a file offset, unrelated in
        // magnitude to the runtime VA) — the subtraction overflows
        // briefly before the addition cancels it. Both operands are
        // well-defined unsigned arithmetic so wrapping is the correct
        // semantic here, not saturating.
        let file_offset = pc.wrapping_sub(pm.base).wrapping_add(pm.offset);
        let user_file_id = pm.user_file_id;
        if let Some(syms) = self.user_symbols.get(&user_file_id) {
            if let Some((sym, inner_offset)) = covering(syms, file_offset, |s| s.offset, |s| s.size)
            {
                return Some(SymbolHit::User {
                    file,
                    map: pm,
                    symbol: Cow::Borrowed(sym),
                    inner_offset,
                });
            }
        }
        if let Some((symbol, inner_offset)) =
            self.pack_user_symbols
                .lookup(user_file_id, file_offset, |s| s.offset, |s| s.size)
        {
            return Some(SymbolHit::User {
                file,
                map: pm,
                symbol: Cow::Owned(symbol),
                inner_offset,
            });
        }
        // Fallback: we know the DSO but not the function. Surface the
        // runtime offset so the user can at least see *which binary*
        // is hot. See the docstring on [`SymbolHit::UserModuleOnly`]
        // for why this case dominates real-world `elog -U` captures
        // today.
        Some(SymbolHit::UserModuleOnly {
            file,
            map: pm,
            runtime_offset,
        })
    }

    /// Format a PC for display, applying the standard cascade:
    ///   1. kernel symbol -> `name+0xINNER` (or `file:name+0xINNER`
    ///      when `with_file` is true).
    ///   2. user-space symbol (when `pid` is provided and the PC
    ///      falls in some [`ProcMap`] for that pid AND a symbol in
    ///      that DSO's table covers it) -> same shape.
    ///   3. user-space module-only fallback (PC fell in a known
    ///      [`ProcMap`] but no symbol matched) -> `libfoo.so+0xN`,
    ///      where `N` is the offset from the runtime mapping's start.
    ///      Strictly more useful than `0xADDR` because the user can
    ///      see *which library* is hot even when symbol resolution
    ///      failed for it.
    ///   4. otherwise, when [`KernelInfo`] is present and the PC is in
    ///      the kernel text segment -> `kernel+0xOFFSET`.
    ///   5. otherwise -> raw `0xADDR`.
    ///
    /// The synthetic root (`pc == 0`) is reported as `"<root>"` to
    /// avoid the meaningless `"0x0"`.
    pub fn format_pc(&self, pc: u64, pid: Option<u32>, with_file: bool) -> String {
        if pc == 0 {
            return "<root>".to_string();
        }
        if let Some(hit) = self.symbolize(pc, pid) {
            return if with_file {
                hit.label_with_file()
            } else {
                hit.label()
            };
        }
        if let Some(k) = &self.kernel {
            if pc >= k.text_base && pc < k.text_base.saturating_add(k.text_size) {
                return format!("kernel+{:#x}", pc - k.text_base);
            }
        }
        format!("{pc:#x}")
    }
}

#[derive(Debug, Clone)]
pub enum EventPayload {
    ConnSetIpV4 {
        src_addr: Ipv4Addr,
        src_port: u16,
        dst_addr: Ipv4Addr,
        dst_port: u16,
    },
    ConnSetIpV6 {
        src_addr: Ipv6Addr,
        src_port: u16,
        dst_addr: Ipv6Addr,
        dst_port: u16,
    },
    LogId {
        log_id: String,
    },
    /// `EVT_IN` (id 100). `state`/`th_off`/`th_sum`/`th_urp`/`rcv_time_us`
    /// verified against `/tmp/hpts-delay.elog`'s embedded `tcp` schema
    /// (`STRUCT IN`); not currently surfaced in any CLI/GUI view.
    In {
        seq: u32,
        ack: u32,
        win: u32,
        len: u32,
        flags: u16,
        /// TCP state at receive time (`ENUM tcp_state`: 0 CLOSED / 1
        /// LISTEN / 2 SYN_SENT / 3 SYN_RECEIVED / 4 ESTABLISHED / 5
        /// CLOSE_WAIT / 6 FIN_WAIT_1 / 7 CLOSING / 8 LAST_ACK / 9
        /// FIN_WAIT_2 / 10 TIME_WAIT). See [`names::tcp_state_name`].
        state: u8,
        th_off: u8,
        /// TCP checksum, displayed in hex by the schema's own annotation.
        th_sum: u16,
        th_urp: u16,
        /// Kernel receive timestamp (usec), same clock as `Event::ts`.
        rcv_time_us: u64,
    },
    /// `EVT_OUT` (id 101). `flags` carries `FLAG tcp_flags` (`TCP_FLAG_*`,
    /// including the log-only `TCP_FLAG_TLP` and `TCP_FLAG_RETRANSMIT` bits).
    /// `th_off`/`th_sum`/`th_urp` verified against `/tmp/hpts-delay.elog`'s
    /// embedded `tcp` schema (`STRUCT OUT`); not currently surfaced in any
    /// CLI/GUI view.
    Out {
        seq: u32,
        ack: u32,
        win: u32,
        len: u32,
        flags: u16,
        cwnd: u32,
        th_off: u8,
        th_sum: u16,
        th_urp: u16,
    },
    /// `EVT_ACK` (id 102) and `EVT_SACK` (id 103) share this variant —
    /// `STRUCT SACK` starts with the same 5 fields at the same offsets as
    /// `STRUCT ACK`, then adds the block fields below (always zero for a
    /// plain id-102 `ACK`, which carries no block data). `rcv_time_us`
    /// and the block fields verified against `/tmp/hpts-delay.elog`'s
    /// embedded `tcp` schema.
    Ack {
        acked: u32,
        snd_una: u32,
        inflight: u32,
        sacked: u32,
        nsegs: u32,
        /// Kernel receive timestamp (usec), same clock as `Event::ts`.
        rcv_time_us: u64,
        /// Number of `blk*_start`/`blk*_end` pairs below that are valid
        /// (0-4). Always 0 for a plain `ACK` (id 102).
        num_blocks: u8,
        blk1_start: u32,
        blk1_end: u32,
        blk2_start: u32,
        blk2_end: u32,
        blk3_start: u32,
        blk3_end: u32,
        blk4_start: u32,
        blk4_end: u32,
    },
    Cwnd {
        cwnd: u32,
        ssthresh: u32,
    },
    EnterRecovery {
        cwnd: u32,
        ssthresh: u32,
    },
    ExitRecovery {
        cwnd: u32,
        ssthresh: u32,
    },
    /// `EVT_GOODPUT` (id 207). `gp_srtt`/`lowest_rtt` verified against
    /// `/tmp/hpts-delay.elog`'s embedded `tcp` schema (`STRUCT GOODPUT`);
    /// not currently surfaced in any CLI/GUI view (the plots derive their
    /// own srtt/lowest_rtt from `EVT_RTT` instead).
    Goodput {
        goodput: u64,
        raw_bw: u64,
        utim: u64,
        gp_srtt: u32,
        lowest_rtt: u32,
        /// Rate-based pacing rate (bytes/s) when the sample was taken,
        /// 0 when not rate pacing or on kernels without the field.
        pacing_bw: u64,
    },
    GpCheck {
        elapsed_us: u64,
        utim_us: u64,
        now_us: u64,
        start_ts_us: u64,
        last_ack_us: u64,
        srtt_us: u32,
        th_ack: u32,
        bytes: u32,
        min_bytes: u32,
        reason: u8,
    },
    Rtt {
        rtt: u32,
        srtt: u32,
        path_rtt: u32,
        lowest_rtt: u32,
        gp_filter_min: u32,
        gp_srtt: u32,
    },
    PacingCalc {
        slot: u32,
        bw_est: u64,
        method: u8,
        quality: u8,
        in_slow_start: u8,
        /// `PACING_CAP_*` value naming the software clamp that reduced
        /// `bw_est`, or `PACING_CAP_NONE` (0) when unclamped / unavailable.
        /// Populated from PACING_CALC_V2 events; legacy PACING_CALC (id 600)
        /// events don't carry this and report `PACING_CAP_NONE`.
        cap_source: u8,
        inflight: u32,
    },
    RttUnderflow {
        us_rtt: u32,
        minuend: u32,
        subtrahend: u32,
        location: u8,
    },
    PaceState {
        rc_pace_min_segs: u32,
        rc_pace_max_segs: u32,
    },
    /// STARS squeeze diagnostic (NCD-9761): natural (pre-squeeze) rate vs.
    /// the result of `hpts_stars_pace_rate()`, plus the global pressure
    /// that produced it. Emitted only when this connection's eventlog
    /// session is enabled.
    StarsPace {
        natural_bps: u64,
        scaled_bps: u64,
        pressure: u32,
        len: u32,
    },
    SessionCreate {
        tp: u64,
    },
    /// Stable per-connection parameters, emitted once at handshake
    /// completion (or again on stack switch). Carries the negotiated
    /// window scales, the negotiated-options bitmask, and the initial
    /// congestion-control algorithm name. The original `CONN_PARAMS`
    /// (id 3) carried only `iss`/`irs` and is no longer emitted by the
    /// kernel; older captures that contain it decode to
    /// `EventPayload::Other`.
    ConnParamsV2 {
        iss: u32,
        irs: u32,
        /// Negotiated maximum segment size (bytes). Added in the merged
        /// kernel schema; `0` on captures from kernels predating the field.
        mss: u32,
        /// Negotiated window scale for peer-advertised windows. The
        /// production kernel (tcp_input.c / ngen.c) already shifts
        /// the raw 16-bit `th_win` left by this value before logging
        /// it as `EVT_IN.win`, so EventExplorer treats `EVT_IN.win`
        /// as bytes directly and surfaces `snd_wscale` only in the
        /// session header for context.
        snd_wscale: u8,
        /// Negotiated window scale for our own advertised windows
        /// (i.e. the value the peer applies to `EVT_OUT.win`).
        rcv_wscale: u8,
        /// `CP_FLAG_*` bitmask of negotiated options (SACK, timestamps,
        /// ECN, ACE, TFO, signature, WSCALE).
        flags: u8,
        /// Initial congestion-control algorithm name. Subsequent
        /// changes arrive via `EVT_CC_ALGO_CHANGE` (id 7).
        cc_algo: String,
        /// Initial TCP stack name (e.g. `"rack"`, `"bbr"`, `"default"`).
        /// Empty on captures from kernels predating the `tcp_stack` field.
        /// Mid-connection stack swaps arrive via `EVT_TCP_STACK_CHANGE` (id 7).
        tcp_stack: String,
    },
    /// CONN_OWNER (id 5): pid that owns the connection's socket. `<= 0`
    /// means no owning userland process (kernel-internal connection).
    ConnOwner {
        pid: i32,
    },
    /// TCP_STACK_CHANGE (id 7): the TCP stack was swapped mid-connection.
    TcpStackChange {
        tcp_stack: String,
    },
    /// CC_ALGO_CHANGE (id 8): the CC algorithm was swapped mid-connection
    /// via `setsockopt(TCP_CONGESTION)`.
    CcAlgoChange {
        cc_algo: String,
    },
    /// HTTP_REQ_NEW (id 1000): a TCP_REQUEST_TRK slot was allocated.
    HttpReqNew {
        slot: u8,
        track_flags: u32,
        start: u64,
        end: u64,
        start_seq: u32,
        end_seq: u32,
        timestamp: u64,
        localtime: u64,
        first_rcv_us: u64,
        playout_ms: u32,
    },
    /// HTTP_REQ_FREED (id 1001): a TCP_REQUEST_TRK slot was freed.
    HttpReqFreed {
        slot: u8,
        track_flags: u32,
        start_seq: u32,
        end_seq: u32,
        start: u64,
        end: u64,
        timestamp: u64,
        localtime: u64,
        first_rcv_us: u64,
        first_send: u64,
        last_ack_us: u64,
        sent_at_fs: u64,
        sent_at_ls: u64,
    },
    /// HTTP_REQ_STALE (id 1002): a slot was reaped while still open.
    HttpReqStale {
        slot: u8,
        track_flags: u32,
        is_oldest: u8,
        start_seq: u32,
        end_seq: u32,
        start: u64,
        end: u64,
        localtime: u64,
        time_delta: u64,
    },
    /// HTTP_REQ_ALLOCFAIL (id 1003): no free TCP_REQUEST_TRK slot.
    HttpReqAllocfail {
        slot: u8,
        track_flags: u32,
        start_seq: u32,
        end_seq: u32,
        start: u64,
        end: u64,
    },
    /// HTTP_REQ_FORCEFREE (id 1004): a slot was forcibly reclaimed while
    /// still marked in-flight. Same fields as `HttpReqFreed` minus
    /// `last_ack_us`.
    HttpReqForcefree {
        slot: u8,
        track_flags: u32,
        start_seq: u32,
        end_seq: u32,
        start: u64,
        end: u64,
        timestamp: u64,
        localtime: u64,
        first_rcv_us: u64,
        first_send: u64,
        sent_at_fs: u64,
        sent_at_ls: u64,
    },
    /// Receiver reported one or more DSACK (duplicate SACK) blocks in a
    /// round — i.e. we retransmitted unnecessarily. Fields verified
    /// against `/tmp/hpts-delay.elog`'s embedded `tcp` schema (`STRUCT
    /// DSACK`) — an earlier version of this variant guessed at
    /// `start_seq`/`end_seq` fields that never matched any real kernel
    /// schema (see `git log` on this file): DSACK accounting here is
    /// round-based (a count within `round`), not a single byte range.
    Dsack {
        /// Whether a DSACK was seen in the (previous) round being
        /// reported on.
        dsack_round_seen: u8,
        dsack_round_end: u32,
        /// Number of DSACKs seen in this round.
        num_dsack: u32,
        dsack_persist: u32,
        /// Named `mod` in the schema (a reserved word in Rust).
        round_mod: u8,
        round: u32,
    },
    /// `EVT_USER_SEND` (id 104): the application called
    /// `send`/`sendto`/`write` and the kernel just appended bytes to
    /// the socket send buffer. Carries the buffer fill snapshot
    /// after the append.
    ///
    /// `sndbuf_used` is `sbused(&so->so_snd)`, i.e. *all* bytes in
    /// the send buffer including those already on the wire but not
    /// yet ACKed. Therefore `sndbuf_used - inflight` is "bytes the
    /// app has supplied but the kernel has not yet placed on the
    /// wire" — the signal the limiter classifier uses to distinguish
    /// "pacing is gating data the app supplied" from "pacing is
    /// vacuously gating because the app supplied nothing more"
    /// (i.e. genuine app-limited).
    UserSend {
        /// `0` on success, otherwise an `errno` from the send path.
        error: i32,
        /// `flag_prus_flags` bitmask (PRUS_OOB / EOR / etc.).
        flags: u32,
        /// TCP state at the time of the send.
        state: u8,
        /// Bytes currently in the send buffer (sent-unacked + unsent).
        sndbuf_used: u32,
        /// Free space remaining in the send buffer.
        sndbuf_avail: u32,
        /// Bytes the app just appended on this call.
        send_len: u32,
    },
    /// `EVT_SENDFILE` (id 105): fires once per `vn_sendfile()` loop
    /// iteration in `kern_sendfile.c`, immediately after the
    /// chunk's `pr->pr_send()` call (which itself routes through
    /// `tcp_usr_send` and emits an `EVT_USER_SEND` with the
    /// authoritative `sbused()` snapshot). Carries the *syscall
    /// arguments* — the `offset`/`nbytes`/`flags` the app passed in
    /// — not the per-iteration chunk size. For non-blocking sockets
    /// (the common Netflix CDN path) `sendfile_wait_generic` returns
    /// `EAGAIN` after the first chunk, so the loop typically
    /// iterates once and `length` ends up being the app's
    /// *lookahead window* (often megabytes) rather than the bytes
    /// actually queued into the socket buffer (typically a few
    /// hundred KB, capped by `sbspace`). Therefore SENDFILE is
    /// useful for understanding the app's read-ahead pattern but
    /// NOT for tracking buffer occupancy — use the paired
    /// USER_SEND.sndbuf_used for that.
    SendFile {
        offset: u64,
        length: u64,
        flags: u32,
    },
    /// Retransmission-timeout fired. The variant still exists mainly so
    /// the event survives `retain_plot_events` and renders as a
    /// full-height vertical marker across the time-linked panels; fields
    /// verified against `/tmp/hpts-delay.elog`'s embedded `tcp` schema
    /// (`STRUCT RTO`) and now included in that marker's label.
    Rto {
        timeout_num: i32,
        min_rtt: u32,
        rtt: u32,
        segment_len: u32,
        inflight: u32,
        pkts_out: u32,
    },
    /// The peer advertised a shrinking receive window (window collapse).
    /// The variant still exists mainly so the event survives
    /// `retain_plot_events` and renders as a full-height vertical marker
    /// across the time-linked panels; fields verified against
    /// `/tmp/hpts-delay.elog`'s embedded `tcp` schema (`STRUCT
    /// WINDOW_COLLAPSE`) and now included in that marker's label.
    WindowCollapse {
        count: u32,
        /// `ENUM collapse_dir`: 0 UNCOLLAPSED / 1 COLLAPSED / 2
        /// RSM_MARKED / 3 SPLIT. See [`names::collapse_dir_name`].
        direction: u8,
        inflight: u32,
        has_collapsed: u8,
    },
    Other,
}

impl EventPayload {
    /// True if this event is an OUT event flagged as a retransmit via the
    /// log-only `TCP_FLAG_RETRANSMIT` bit on `flags`.
    pub fn is_retransmit(&self) -> bool {
        match self {
            EventPayload::Out { flags, .. } => (*flags & TCP_FLAG_RETRANSMIT) != 0,
            _ => false,
        }
    }

    /// True if this event is an OUT event flagged as a Tail Loss Probe (`TCP_FLAG_TLP`).
    pub fn is_tlp(&self) -> bool {
        match self {
            EventPayload::Out { flags, .. } => (*flags & TCP_FLAG_TLP) != 0,
            _ => false,
        }
    }
}

pub type Sessions = FxHashMap<u64, Vec<Event>>;

// ---------------------------------------------------------------------------
// Schema-driven decoding (EventLog v2) — events with no hardcoded decoder
// (or whose hardcoded decoder was found to diverge from the resolved
// schema) but that the schema itself defines. See
// `schema_validate::validate_hardcoded_decoders` and the routing guard in
// `core.rs`'s event loop.
// ---------------------------------------------------------------------------

/// One event decoded purely from an embedded v2 schema, with no typed
/// [`EventPayload`]/[`HptsWheelPayload`] variant of its own. `formatted`
/// is the schema's own format string rendered against this event's
/// payload bytes (see `schema::ProviderSchema::format_payload`) — the
/// same text `elog(1)` itself would print.
#[derive(Debug, Clone)]
pub struct GenericEvent {
    pub ts: u64,
    pub cpu: u16,
    pub tid: i32,
    pub session_id: u64,
    pub provider_id: u16,
    pub event_id: u32,
    pub event_name: String,
    pub formatted: String,
}

/// Schema-driven decode results not tied to the per-session `tcp` event
/// map: the `hpts` timing-wheel scheduler trace (a separate schema from
/// `STARS_AGG_SAMPLE`, see [`HptsWheelPayload`]) and every event decoded
/// generically because no hardcoded decoder claims it (or the hardcoded
/// one was found divergent) but a resolved v2 schema defines it. The
/// wheel trace and generic events are always empty for v1 captures.
#[derive(Debug, Clone, Default)]
pub struct SchemaCapture {
    pub hpts_wheel_events: Vec<HptsWheelEvent>,
    pub generic_events: Vec<GenericEvent>,
    /// `mem` provider time series. Lives here rather than in its own
    /// `ParsedCapture` slot because, like the hpts wheel trace, it is a
    /// global stream read by schema field name.
    pub mem: MemProfile,
}

/// `mem` provider `MEM_INFO`: context re-emitted on dump_state and on a
/// sample-period change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemInfo {
    pub ts: u64,
    pub page_size: u32,
    pub ndomains: u16,
    pub sample_period_us: u32,
    pub physmem: u64,
    pub usable: u64,
    pub swap_total: u64,
}

/// `mem` provider `SYS_SAMPLE`: one system-wide tick, all in bytes.
/// Wired pages can briefly also sit on a page queue, so the five states
/// may sum to slightly more than `MemInfo::usable`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemSample {
    pub ts: u64,
    pub free: u64,
    pub active: u64,
    pub inactive: u64,
    pub laundry: u64,
    pub wired: u64,
    pub free_target: u64,
    pub free_min: u64,
    pub free_severe: u64,
    pub swap_used: u64,
}

#[derive(Debug, Clone, Default)]
pub struct MemProfile {
    pub infos: Vec<MemInfo>,
    pub samples: Vec<MemSample>,
}

impl MemProfile {
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Most recent `MEM_INFO`, if any.
    pub fn info(&self) -> Option<&MemInfo> {
        self.infos.last()
    }
}

/// One event from the `hpts` provider's timing-wheel scheduler trace
/// (ids 0-14 — a separate schema from `STARS_AGG_SAMPLE`, id 16). See
/// `hpts_eventlog_schema.src` and `src/elog_parser/hpts_wheel.rs`.
#[derive(Debug, Clone)]
pub struct HptsWheelEvent {
    pub ts: u64,
    pub cpu: u16,
    pub session_id: u64,
    pub payload: HptsWheelPayload,
}

/// Payload variants for the `hpts` provider's timing-wheel scheduler
/// trace. Ids 0-14 are decoded by `hpts_wheel::parse_hpts_wheel_payload`
/// against offsets verified against both `hpts_eventlog_schema.src` and
/// a real v2 capture's embedded schema.
///
/// [`Self::NoRunSlotSkip`] (would-be id 15) is **never constructed** —
/// kept only so `src/cli/hpts.rs`'s exhaustive matches keep compiling.
/// An earlier draft of that CLI referenced this event name, but it
/// exists in neither `hpts_eventlog_schema.src` nor any embedded schema
/// seen so far, so there is no ground truth for its byte layout
/// anywhere in the tree. Per "always verify, never guess", no decoder
/// is implemented for it; `cmd_hpts_gaps`'s `NoRunSlotSkip`
/// cross-reference degrades gracefully (always 0 matches) instead of
/// being deleted. Remove this variant entirely if a future kernel
/// schema ever defines it for real (at which point it'd get real field
/// types from that schema, not this guessed one).
///
/// Not `Copy`: [`Self::LatenessSample`] and [`Self::TpBatchSample`] own
/// a `Vec<u64>`.
#[derive(Debug, Clone)]
pub enum HptsWheelPayload {
    /// `INIT` (id 0): wheel construction parameters, emitted once.
    Init {
        num_hptss: u32,
        /// `ENUM bind_type`: 0 NONE / 1 CPU / 2 NUMA.
        bind_type: u8,
        num_slots: u32,
        usecs_per_slot: u32,
        total_slot_time: u32,
    },
    /// `INIT_HPTS_ENTRY` (id 1): per-hpts-instance creation marker, no
    /// payload fields (`STRUCT INIT_HPTS_ENTRY` is empty).
    InitHptsEntry,
    /// `INIT_TIMING_PARAMS` (id 2): timing parameters finalized.
    InitTimingParams {
        usecs_per_slot: u32,
        total_slot_time: u32,
        num_slots: u32,
        wheel_mask: u32,
    },
    /// `STATE_ACTIVE` (id 3): wheel transitioned to actively processing.
    StateActive {
        from_callout: u8,
        cur_slot: u32,
        tp_cur_count: u32,
    },
    /// `STATE_INACTIVE` (id 4): wheel finished a processing run and went
    /// idle.
    StateInactive { slots_ran: u32 },
    /// `SLEEP` (id 5): wheel is about to sleep for `sleep_time` usec.
    Sleep { sleep_time: u32 },
    /// `CONN_INSERT` (id 6): a TCP connection was inserted into a slot.
    ConnInsert {
        gencnt: u32,
        slot: u32,
        usecs: u32,
        tp_cur_count: u32,
    },
    /// `CONN_REMOVE` (id 7): a TCP connection was removed from a slot.
    ConnRemove {
        gencnt: u32,
        slot: u32,
        tp_cur_count: u32,
    },
    /// `CONN_MOVE` (id 8): a TCP connection was moved off a slot due to a
    /// race condition.
    ConnMove { gencnt: u32, old_slot: u32 },
    /// `WHEEL_WRAP` (id 9): the wheel wrapped around; a burst of slots
    /// needs processing.
    WheelWrap {
        wrap_loop_cnt: u32,
        slots_to_run: u32,
        prev_slot: u32,
        cur_slot: u32,
    },
    /// `PROCESSING` (id 10): one processing run covering `[prev_slot,
    /// cur_slot)`. `cmd_hpts_gaps` cross-checks continuity between
    /// consecutive runs on the same wheel via these two fields.
    Processing {
        from_callout: u8,
        prev_slot: u32,
        cur_slot: u32,
        slots_to_run: u32,
        tp_cur_count: u32,
    },
    /// `PERF_PROCESSING` (id 11): performance summary for one processing
    /// run.
    PerfProcessing {
        from_callout: u8,
        slots_processed: u32,
        tps_processed: u32,
        runtime_usec: u32,
        loop_cnt: u32,
    },
    /// `PERF_LATENESS` (id 12): a TCP connection's callout fired later
    /// than its requested slot. The primary "hpts delay" diagnostic —
    /// see `cmd_hpts_lateness`.
    PerfLateness {
        gencnt: u32,
        lateness_slots: u32,
        lateness_usec: u32,
        requested_time: u32,
        actual_time: u32,
    },
    /// `HPTS_LATENESS_SAMPLE` (id 13): periodic per-CPU snapshot of the
    /// cumulative `hist_lateness_usec` histogram (exponential/log2
    /// buckets, `flsll(lateness_usec)` — see `sys/histogram.h`). Same
    /// on-wire shape as `TCP_GP_SAMPLE`: `nbuckets:u8` (padded to 8
    /// bytes) + `nbuckets` cumulative `u64` buckets. Consumers diff
    /// successive samples (by cpu) — see `cmd_hpts_lateness`.
    LatenessSample { buckets: Vec<u64> },
    /// `HPTS_TP_BATCH_SAMPLE` (id 14): periodic per-CPU snapshot of the
    /// cumulative `hist_tp_batch_size` histogram (linear buckets, one
    /// per connections-processed-per-batch). Same shape/contract as
    /// [`Self::LatenessSample`].
    TpBatchSample { buckets: Vec<u64> },
    /// `HPTS_SLOT_GATE_SAMPLE` (id 18): per-CPU counts since the previous
    /// sample on that CPU. Cleared in the kernel after each snapshot, so
    /// consumers sum across CPUs rather than diffing.
    SlotGateSample {
        fires: u32,
        diverts: u32,
        bytes: u64,
        rollover_max: u32,
        walks: u32,
        closed: u32,
        /// Bytes `tcp_default_output` handed to ip_output since the previous sample.
        default_bytes: u64,
        rack_bytes: u64,
        bbr_bytes: u64,
        /// TCP bytes `ip_output` accepted while the sample was closed.
        closed_tcp_bytes: u64,
        /// Return address of an `ip_output` caller that submitted those bytes.
        caller0: u64,
        caller0_bytes: u64,
        caller1: u64,
        caller1_bytes: u64,
        caller_other_bytes: u64,
    },
    /// `HPTS_OPPOR_SAMPLE` (id 17): the [`Self::TpBatchSample`] histogram
    /// restricted to opportunistic (lro-driven) runs, plus the cumulative
    /// counts of those runs and of the attempts that found nothing to do.
    /// Compare its tail against `TpBatchSample`'s to tell whether
    /// `tcp_choose_hpts_to_run()`'s most-backlogged-wheel policy is what
    /// concentrates connections onto a tick.
    OpporSample {
        oppor_runs: u64,
        oppor_noop: u64,
        buckets: Vec<u64>,
    },
    /// Never constructed — see the enum doc comment.
    NoRunSlotSkip {
        prev_slot: u32,
        new_prev_slot: u32,
        skipped_slots: u32,
        tp_cur_count: u32,
    },
}

#[cfg(test)]
mod inherit_fork_proc_maps_tests {
    use super::*;

    fn thread(tid: u32, pid: u32, comm: &str) -> (u32, ThreadInfo) {
        (
            tid,
            ThreadInfo {
                tid,
                pid,
                comm: comm.to_string(),
                tdname: String::new(),
            },
        )
    }

    fn map(pid: u32, base: u64) -> ProcMap {
        ProcMap {
            pid,
            user_file_id: 58,
            base,
            size: 0x1a8000,
            offset: 0x90000,
        }
    }

    #[test]
    fn copies_shared_layout_onto_same_comm_pids_without_maps() {
        let mut p = CpuProfile::default();
        p.threads
            .extend([thread(1, 100, "nginx"), thread(2, 200, "nginx")]);
        p.proc_maps.insert(100, vec![map(100, 0x37dd_f7a9_0000)]);
        p.inherit_fork_proc_maps();
        let got = p.proc_maps.get(&200).expect("inherited");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].pid, 200);
        assert_eq!(got[0].base, 0x37dd_f7a9_0000);
        assert_eq!(got[0].user_file_id, 58);
    }

    #[test]
    fn skips_comm_when_donors_disagree_on_layout() {
        let mut p = CpuProfile::default();
        p.threads.extend([
            thread(1, 100, "nginx"),
            thread(2, 200, "nginx"),
            thread(3, 300, "nginx"),
        ]);
        p.proc_maps.insert(100, vec![map(100, 0x1000)]);
        p.proc_maps.insert(200, vec![map(200, 0x2000)]);
        p.inherit_fork_proc_maps();
        assert!(!p.proc_maps.contains_key(&300));
    }

    #[test]
    fn leaves_other_comms_alone() {
        let mut p = CpuProfile::default();
        p.threads
            .extend([thread(1, 100, "nginx"), thread(2, 200, "bird")]);
        p.proc_maps.insert(100, vec![map(100, 0x1000)]);
        p.inherit_fork_proc_maps();
        assert!(!p.proc_maps.contains_key(&200));
    }

    #[test]
    fn proc_fork_copies_parent_maps_onto_child() {
        let mut p = CpuProfile::default();
        p.proc_maps.insert(100, vec![map(100, 0x1000)]);
        p.apply_proc_fork(200, 100);
        let got = p.proc_maps.get(&200).expect("inherited");
        assert_eq!(got, &vec![map(200, 0x1000)]);
        assert_eq!(p.proc_maps.get(&100), Some(&vec![map(100, 0x1000)]));
    }

    #[test]
    fn proc_fork_without_parent_maps_is_a_no_op() {
        let mut p = CpuProfile::default();
        p.apply_proc_fork(200, 100);
        assert!(!p.proc_maps.contains_key(&200));
    }

    #[test]
    fn proc_fork_dedups_against_childs_existing_maps() {
        let mut p = CpuProfile::default();
        p.proc_maps.insert(100, vec![map(100, 0x1000)]);
        p.proc_maps.insert(200, vec![map(200, 0x1000)]);
        p.apply_proc_fork(200, 100);
        assert_eq!(p.proc_maps.get(&200), Some(&vec![map(200, 0x1000)]));
    }

    #[test]
    fn fork_edge_then_heuristic_covers_edgeless_sibling() {
        let mut p = CpuProfile::default();
        p.threads.extend([
            thread(1, 100, "nginx"),
            thread(2, 200, "nginx"),
            thread(3, 300, "nginx"),
        ]);
        p.proc_maps.insert(100, vec![map(100, 0x1000)]);
        p.apply_proc_fork(200, 100);
        p.inherit_fork_proc_maps();
        assert_eq!(p.proc_maps.get(&300), Some(&vec![map(300, 0x1000)]));
    }

    #[test]
    fn process_comm_at_follows_proc_exec_and_falls_back_to_thread() {
        let mut p = CpuProfile::default();
        p.threads.extend([thread(1, 100, "sh")]);
        assert_eq!(p.process_comm_at(100, Some(1), 50), Some("sh"));
        p.apply_proc_exec(100, 100, "perl".to_string());
        p.apply_proc_exec(200, 100, "python3.11".to_string());
        p.proc_execs
            .get_mut(&100)
            .unwrap()
            .sort_unstable_by_key(|e| e.ts);
        assert_eq!(p.process_comm_at(100, Some(1), 50), Some("sh"));
        assert_eq!(p.process_comm_at(100, Some(1), 100), Some("perl"));
        assert_eq!(p.process_comm_at(100, Some(1), 150), Some("perl"));
        assert_eq!(p.process_comm_at(100, Some(1), 200), Some("python3.11"));
        assert_eq!(p.process_comm_latest(100), Some("python3.11"));
    }

    #[test]
    fn process_comm_exit_keeps_the_name_until_exit() {
        let mut p = CpuProfile::default();
        p.threads.extend([thread(1, 100, "sh")]);
        p.apply_proc_exec(100, 100, "get_cfg_value".to_string());
        p.apply_proc_exit(150, 100);
        assert_eq!(p.process_comm_at(100, Some(1), 120), Some("get_cfg_value"));
        assert_eq!(p.process_comm_at(100, Some(1), 150), Some("sh"));
        assert_eq!(p.process_comm_at(100, Some(1), 200), Some("sh"));
        assert_eq!(p.process_comm_latest(100), Some("sh"));
        p.apply_proc_exec(200, 100, "nginx".to_string());
        p.proc_execs
            .get_mut(&100)
            .unwrap()
            .sort_unstable_by_key(|e| e.ts);
        assert_eq!(p.process_comm_at(100, Some(1), 120), Some("get_cfg_value"));
        assert_eq!(p.process_comm_at(100, Some(1), 180), Some("sh"));
        assert_eq!(p.process_comm_at(100, Some(1), 210), Some("nginx"));
        assert_eq!(p.process_comm_latest(100), Some("nginx"));
    }

    #[test]
    fn process_comm_uses_exe_basename_when_proc_exec_is_missing() {
        // Live PROC_EXEC is often absent (filtered); PROC_MAP is
        // LOG_ALWAYS and still names the image after exec.
        let mut p = CpuProfile::default();
        p.threads.extend([thread(1, 100, "sh")]);
        p.user_files.insert(
            1,
            UserFile {
                user_file_id: 1,
                build_id: String::new(),
                pathname: "/usr/local/bin/perl".to_string(),
            },
        );
        p.user_files.insert(
            2,
            UserFile {
                user_file_id: 2,
                build_id: String::new(),
                pathname: "/lib/libperl.so.5".to_string(),
            },
        );
        p.proc_maps.insert(
            100,
            vec![
                ProcMap {
                    pid: 100,
                    user_file_id: 1,
                    base: 0x4000_0000,
                    size: 0x1000,
                    offset: 0,
                },
                ProcMap {
                    pid: 100,
                    user_file_id: 2,
                    base: 0x8000_0000,
                    size: 0x1000,
                    offset: 0,
                },
            ],
        );
        assert_eq!(p.process_comm_at(100, Some(1), 50), Some("perl"));
        assert_eq!(p.process_comm_latest(100), Some("perl"));
    }

    #[test]
    fn process_comm_prefers_dump_comm_over_perl_path() {
        let mut p = CpuProfile::default();
        p.threads.extend([thread(1, 100, "bg_scan")]);
        p.user_files.insert(
            1,
            UserFile {
                user_file_id: 1,
                build_id: String::new(),
                pathname: "/usr/local/bin/perl".to_string(),
            },
        );
        p.proc_maps.insert(
            100,
            vec![ProcMap {
                pid: 100,
                user_file_id: 1,
                base: 0x4000_0000,
                size: 0x1000,
                offset: 0,
            }],
        );
        assert_eq!(p.process_comm_at(100, Some(1), 50), Some("bg_scan"));
        assert_eq!(p.process_comm_latest(100), Some("bg_scan"));
    }

    #[test]
    fn process_comm_prefers_post_exec_exe_over_inherited_parent_map() {
        let mut p = CpuProfile::default();
        p.threads.extend([thread(1, 100, "sh")]);
        p.user_files.insert(
            1,
            UserFile {
                user_file_id: 1,
                build_id: String::new(),
                pathname: "/bin/sh".to_string(),
            },
        );
        p.user_files.insert(
            2,
            UserFile {
                user_file_id: 2,
                build_id: String::new(),
                pathname: "/usr/local/bin/perl".to_string(),
            },
        );
        p.proc_maps.insert(
            100,
            vec![
                ProcMap {
                    pid: 100,
                    user_file_id: 1,
                    base: 0x1000_0000,
                    size: 0x1000,
                    offset: 0,
                },
                ProcMap {
                    pid: 100,
                    user_file_id: 2,
                    base: 0x4000_0000,
                    size: 0x1000,
                    offset: 0,
                },
            ],
        );
        assert_eq!(p.process_comm_at(100, Some(1), 50), Some("perl"));
    }

    #[test]
    fn out_retransmit_and_tlp_predicates() {
        use crate::elog_parser::{TCP_FLAG_ACK, TCP_FLAG_RETRANSMIT, TCP_FLAG_TLP};

        let fresh = EventPayload::Out {
            seq: 1000,
            ack: 2000,
            win: 65535,
            len: 1460,
            flags: TCP_FLAG_ACK,
            cwnd: 14600,
            th_off: 5,
            th_sum: 0,
            th_urp: 0,
        };
        assert!(!fresh.is_retransmit());
        assert!(!fresh.is_tlp());

        let retx = EventPayload::Out {
            seq: 1000,
            ack: 2000,
            win: 65535,
            len: 1460,
            flags: TCP_FLAG_ACK | TCP_FLAG_RETRANSMIT,
            cwnd: 14600,
            th_off: 5,
            th_sum: 0,
            th_urp: 0,
        };
        assert!(retx.is_retransmit());
        assert!(!retx.is_tlp());

        let tlp = EventPayload::Out {
            seq: 1000,
            ack: 2000,
            win: 65535,
            len: 1460,
            flags: TCP_FLAG_ACK | TCP_FLAG_RETRANSMIT | TCP_FLAG_TLP,
            cwnd: 14600,
            th_off: 5,
            th_sum: 0,
            th_urp: 0,
        };
        assert!(tlp.is_retransmit());
        assert!(tlp.is_tlp());
    }
}
