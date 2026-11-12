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
// Binary format constants (from elog.c / goodput_analysis.py)
// ---------------------------------------------------------------------------

// FILE_HDR_FMT = '<4sIQQQQ'  → 4 + 4 + 8 + 8 + 8 + 8 = 40. Versions 1-2 only:
// the last field is a single `dropped_events:u64`.
pub const FILE_HDR_SIZE: usize = 40;
// Version 3 shrinks the fixed header to '<4sIQQQI' (4+4+8+8+8+4 = 36) by
// replacing the trailing `dropped_events:u64` with `counter_count:u32`,
// followed immediately by `counter_count` packed `uint64_t` counters (see
// "Header Counters" in elog.5 / docs/file-format.md). The old single
// counter becomes index `COUNTER_KERNEL_DROPPED` in that array.
pub const FILE_HDR_SIZE_V3: usize = 36;
/// Mirrors the producer's `ELOG_HEADER_COUNTER_SANE_MAX` (elog.c): a
/// `counter_count` above this is rejected outright rather than trusted for
/// a read size, so a corrupt/malicious file can't force an unbounded
/// allocation/read.
pub const HEADER_COUNTER_SANE_MAX: usize = 1024;
/// Indices into the v3 header's trailing counters array (`ELOG_COUNTER_*`
/// in elog.c). Append-only — an index's meaning never changes, so a
/// counter_count shorter than expected just reads as 0 for the missing
/// trailing indices.
pub const COUNTER_KERNEL_DROPPED: usize = 0;
pub const COUNTER_USER_DROPPED_FULL: usize = 1;
pub const COUNTER_USER_DROPPED_PREJOIN: usize = 2;
pub const COUNTER_USER_DROPPED_LATE: usize = 3;
// PROVIDER_FMT = '<H32s'  → 2 + 32 = 34. Version 1 only (no schema_id).
pub const PROVIDER_SIZE: usize = 34;
/// Version 2 provider row: `struct eventlog_provider_info` — `id:u16,
/// name:char[32], schema_id:u8[16]` (see
/// `sys/share/man/man5/elog.5`). 16 bytes larger than [`PROVIDER_SIZE`]
/// to carry the provider's decode-blob content hash.
pub const PROVIDER_SIZE_V2: usize = 50;
/// Byte length of a v2 provider row's `schema_id` (`SHA256(blob)`
/// truncated), and of `EVENTLOG_DECODE_SCHEMA_ID_LEN` in
/// `sys/sys/eventlog_decode.h`.
pub const SCHEMA_ID_LEN: usize = 16;
// EVENT_HDR_FMT = '<HHHHQQIi'  → 2+2+2+2+8+8+4+4 = 32
pub const EVENT_HDR_SIZE: usize = 32;

pub const USEC_IN_SEC: f64 = 1_000_000.0;

/// RTT values above this threshold (in microseconds) are treated as unsigned
/// underflow artifacts from the kernel and discarded.  The RACK stack computes
/// `t = cts - send_ts` as an unsigned u32 subtraction; when the receive
/// timestamp is earlier than the stored send timestamp (e.g. forced-ACK probe
/// timing issue) the result wraps to near UINT32_MAX.  Using i32::MAX as the
/// cutoff mirrors the kernel's own `if ((int)t <= 0) t = 1;` guard that is
/// present on the main data path but missing on several secondary paths.
pub const RTT_UNDERFLOW_THRESHOLD: u32 = i32::MAX as u32;

// TCP event IDs (from tcp_eventlog_schema.src)
// Connection (0-99)
pub const EVT_CONN_SET_IP_V4: u32 = 0;
pub const EVT_CONN_SET_IP_V6: u32 = 1;
pub const EVT_LOG_ID: u32 = 2;
pub const EVT_CONN_PARAMS: u32 = 3;
pub const EVT_MSS: u32 = 4;
/// CONN_OWNER (id 5): the pid that owns the connection's socket.
/// Emitted once when the owning process is first known. `pid` is an
/// `int32_t` (negative or 0 means "no owning userland process",
/// e.g. kernel-internal connections).
pub const EVT_CONN_OWNER: u32 = 5;
/// CONN_PARAMS_V2 (id 6).  Replaces the deprecated `CONN_PARAMS` (id 3)
/// and adds the connection-stable parameters we previously had no way
/// to recover from a capture: the negotiated MSS, window scales, the
/// negotiated-options bitmask, the initial congestion-control
/// algorithm name, and the initial TCP stack name. Emitted once per
/// session (immediately after the SYN/SYN-ACK exchange completes);
/// also re-emitted by `tcp_eventlog_dump_session` when the stack is
/// switched mid-session.
///
/// NOTE: the event id moved from 5 to 6 in the merged kernel schema
/// (`tcp_eventlog_schema.src` on `master`), which inserted CONN_OWNER
/// at id 5. The pre-merge PR #5538 draft used id 5; captures from that
/// short-lived draft will mis-decode here, but no released kernel emits
/// the draft numbering.
pub const EVT_CONN_PARAMS_V2: u32 = 6;
/// TCP_STACK_CHANGE (id 7): the TCP stack was swapped mid-connection.
/// `tcp_stack` is a 32-byte NUL-terminated string (e.g. `"rack"`, `"bbr"`).
pub const EVT_TCP_STACK_CHANGE: u32 = 7;
/// CC_ALGO_CHANGE (id 8): the congestion-control algorithm was swapped
/// mid-connection via `setsockopt(TCP_CONGESTION)`.
/// `cc_algo` is a 16-byte NUL-terminated string (e.g. `"cubic"`).
pub const EVT_CC_ALGO_CHANGE: u32 = 8;
// Data plane (100-199)
pub const EVT_IN: u32 = 100;
pub const EVT_OUT: u32 = 101;
pub const EVT_ACK: u32 = 102;
pub const EVT_SACK: u32 = 103;
pub const EVT_USER_SEND: u32 = 104;
pub const EVT_SENDFILE: u32 = 105;
// Congestion control / Window (200-299)
pub const EVT_CWND: u32 = 200;
pub const EVT_CC_PARAMS: u32 = 201;
pub const EVT_ENTER_RECOVERY: u32 = 202;
pub const EVT_EXIT_RECOVERY: u32 = 203;
pub const EVT_PRR_STATE: u32 = 204;
pub const EVT_WINDOW_COLLAPSE: u32 = 205;
pub const EVT_HYSTART: u32 = 206;
pub const EVT_GOODPUT: u32 = 207;
/// NOT IN THE CURRENT SCHEMA TREE: audited against `tcp_eventlog_schema.src`
/// on `master` and the `NCD-9706-cpu-samples-integration` branch (2026-08)
/// and id 208 is unassigned in both — GOODPUT (207) is immediately followed
/// by RTT (300). Kept defensively in case some other deployed kernel branch
/// still emits it; harmless if not (the id simply never appears on the wire).
pub const EVT_GP_CHECK: u32 = 208;
// RTT (300-399)
pub const EVT_RTT: u32 = 300;
pub const EVT_RTT_SAMPLE: u32 = 301;
/// NOT IN THE CURRENT SCHEMA TREE: see the `EVT_GP_CHECK` note above — id
/// 302 is unassigned in the schemas audited 2026-08; RTT_SAMPLE (301) is
/// the last id before LOSS/RETRANSMIT (400+).
pub const EVT_RTT_UNDERFLOW: u32 = 302;
// Loss / Retransmission (400-499)
pub const EVT_RTO: u32 = 400;
pub const EVT_DSACK: u32 = 402;
// Timers (500-599)
pub const EVT_TIMER_START: u32 = 500;
pub const EVT_TIMER_CANCEL: u32 = 501;
pub const EVT_TIMER_FIRED: u32 = 502;
pub const EVT_SB_WAKE: u32 = 503;
// Pacing (600-699)
pub const EVT_PACING_CALC: u32 = 600;
pub const EVT_PACE_STATE: u32 = 601;
pub const EVT_HW_PACE_RATE: u32 = 602;
/// NOT IN THE CURRENT SCHEMA TREE: same status as `EVT_GP_CHECK` — the
/// schemas audited 2026-08 have a single PACING_CALC (600) with no `_V2`
/// and no `cap_source` field. Kept defensively for the reasons above.
pub const EVT_PACING_CALC_V2: u32 = 603;
/// Per-connection STARS squeeze diagnostic (NCD-9761): logs the natural
/// (pre-squeeze) pacing rate alongside the result of
/// `hpts_stars_pace_rate()` and the global pressure that produced it.
/// Only emitted for connections with eventlog enabled (see
/// `net.inet.tcp.eventlog.auto_ratio`), from both call sites in
/// `rack_get_pacing_delay()` (ngen.c).
pub const EVT_STARS_PACE: u32 = 604;

// Pacing cap source enum values (see PACING_CAP_* in tcp_ngen.h and ENUM
// pacing_cap_source in tcp_eventlog_schema.src).  A value other than NONE
// means the pacing rate reported by a PACING_CALC_V2 event was clamped by
// the named software cap.
pub const PACING_CAP_NONE: u8 = 0;
pub const PACING_CAP_STARS: u8 = 1;
pub const PACING_CAP_BW_RATE_CAP: u8 = 2;
pub const PACING_CAP_FIXED: u8 = 3;
pub const PACING_CAP_UNKNOWN: u8 = 255;
// CONN_PARAMS_V2 flags bitmask (FLAG conn_params_flags in
// tcp_eventlog_schema.src).  Each bit reflects a TCP option that was
// negotiated for the lifetime of the connection.
pub const CP_FLAG_SACK_PERMIT: u8 = 0x01;
pub const CP_FLAG_TIMESTAMPS: u8 = 0x02;
pub const CP_FLAG_ECN: u8 = 0x04;
pub const CP_FLAG_ACE: u8 = 0x08;
pub const CP_FLAG_FASTOPEN: u8 = 0x10;
pub const CP_FLAG_SIGNATURE: u8 = 0x20;
/// Window scaling was successfully negotiated (both sides sent the option).
/// When this bit is clear, `snd_wscale`/`rcv_wscale` are 0 (no scaling),
/// not "unknown".
pub const CP_FLAG_WINDOW_SCALE: u8 = 0x40;

// TCP flags bitmask (FLAG tcp_flags in tcp_eventlog_schema.src,
// carried by EVT_IN and EVT_OUT).
pub const TCP_FLAG_FIN: u16 = 0x01;
pub const TCP_FLAG_SYN: u16 = 0x02;
pub const TCP_FLAG_RST: u16 = 0x04;
pub const TCP_FLAG_PUSH: u16 = 0x08;
pub const TCP_FLAG_ACK: u16 = 0x10;
pub const TCP_FLAG_URG: u16 = 0x20;
pub const TCP_FLAG_ECE: u16 = 0x40;
pub const TCP_FLAG_CWR: u16 = 0x80;
pub const TCP_FLAG_AE: u16 = 0x100;
/// Log-only flags sitting above the 12 wire header flag bits:
/// PACED (0x2000): ngen was rate pacing when it sent this segment.
pub const TCP_FLAG_PACED: u16 = 0x2000;
/// TLP (0x4000): Tail Loss Probe retransmit.
pub const TCP_FLAG_TLP: u16 = 0x4000;
/// RETRANSMIT (0x8000): output was a retransmit.
pub const TCP_FLAG_RETRANSMIT: u16 = 0x8000;

// Aliases matching the kernel's generated TCP_EVENTLOG_FLAG_* macros.
pub const TCP_EVENTLOG_FLAG_FIN: u16 = TCP_FLAG_FIN;
pub const TCP_EVENTLOG_FLAG_SYN: u16 = TCP_FLAG_SYN;
pub const TCP_EVENTLOG_FLAG_RST: u16 = TCP_FLAG_RST;
pub const TCP_EVENTLOG_FLAG_PUSH: u16 = TCP_FLAG_PUSH;
pub const TCP_EVENTLOG_FLAG_ACK: u16 = TCP_FLAG_ACK;
pub const TCP_EVENTLOG_FLAG_URG: u16 = TCP_FLAG_URG;
pub const TCP_EVENTLOG_FLAG_ECE: u16 = TCP_FLAG_ECE;
pub const TCP_EVENTLOG_FLAG_CWR: u16 = TCP_FLAG_CWR;
pub const TCP_EVENTLOG_FLAG_AE: u16 = TCP_FLAG_AE;
pub const TCP_EVENTLOG_FLAG_PACED: u16 = TCP_FLAG_PACED;
pub const TCP_EVENTLOG_FLAG_TLP: u16 = TCP_FLAG_TLP;
pub const TCP_EVENTLOG_FLAG_RETRANSMIT: u16 = TCP_FLAG_RETRANSMIT;

// Configuration (700-799)
pub const EVT_SOCKET_OPT_UINT32: u32 = 700;
pub const EVT_SOCKET_OPT_ERR: u32 = 701;
// Output (800-899)
pub const EVT_OUTPUT_DECISION: u32 = 800;
// Periodic samples (900-999) — subscriber-gated per-stack snapshots.
// These ride on a per-stack "global" eventlog session (one session_id
// per registered TCP stack), not on a per-connection session. The stack
// name and sample period for a given session_id are carried out-of-band
// by TCP_STACK_INFO; sample events correlate to it via session_id.
/// TCP_SAMPLE (id 900): aggregate TCP PCB-state gauges across every VNET.
pub const EVT_TCP_SAMPLE: u32 = 900;
/// TCP_GP_SAMPLE (id 901): per-stack cumulative goodput histogram
/// (`gput_n` buckets of `uint64_t`). Consumer diffs successive samples.
pub const EVT_TCP_GP_SAMPLE: u32 = 901;
/// TCP_STACK_INFO (id 902): stack identity + provider-wide sample period.
/// Emitted once per stack on subscriber attach and again when the period
/// sysctl changes.
pub const EVT_TCP_STACK_INFO: u32 = 902;
/// TCP_RPS_SAMPLE (id 903): per-stack cumulative request count.
/// Consumer diffs successive samples to get requests/sec.
pub const EVT_TCP_RPS_SAMPLE: u32 = 903;
/// TCP_PACING_RATE_SAMPLE (id 904): per-stack cumulative histogram of
/// the commanded STARS pacing rate, same log2-Mbps buckets as
/// TCP_GP_SAMPLE so it overlays on the same axis. Consumer diffs
/// successive samples.
pub const EVT_TCP_PACING_RATE_SAMPLE: u32 = 904;
/// TCP_REALIZED_RATE_SAMPLE (id 905): per-stack cumulative histogram
/// of the realized per-connection egress rate (bytes sent / wall-clock
/// elapsed), same log2-Mbps buckets as TCP_GP_SAMPLE. Consumer diffs
/// successive samples.
pub const EVT_TCP_REALIZED_RATE_SAMPLE: u32 = 905;
/// TCP_SEND_SAMPLE (id 906): per-stack cumulative histogram of TCP
/// send sizes (byte-weighted segments-per-burst, 64 linear buckets).
/// Consumer diffs successive samples.
pub const EVT_TCP_SEND_SAMPLE: u32 = 906;
/// TCP_LRO_FLUSH_SAMPLE (id 907): subsystem-wide cumulative histogram
/// of packets coalesced per `tcp_lro_flush()` (64 linear buckets).
/// Consumer diffs successive samples.
pub const EVT_TCP_LRO_FLUSH_SAMPLE: u32 = 907;
/// TCP_SRTT_SAMPLE (id 908): per-stack cumulative histogram of smoothed
/// RTT in usec (14 custom buckets: 0, <512us, then log2 from 512us).
/// Consumer diffs successive samples.
pub const EVT_TCP_SRTT_SAMPLE: u32 = 908;

// TCP EventLog "special" (non-per-connection) session ids
// (`TCP_EVENTLOG_SESSION_ID_*` in the kernel). The top bit marks a
// special session; the aggregate carries subsystem-wide PCB-state gauges
// (independent of any one stack), while each stack's global session
// carries that stack's identity / RPS / goodput. These ids are only
// unique *within* a provider — every "tcp" provider (one per loaded
// stack) numbers its stack-global session identically — so per-stack
// sample data must be keyed by (provider_id, session_id).
/// Subsystem-wide aggregate session: machine-level TCP_SAMPLE gauges
/// and TCP_LRO_FLUSH_SAMPLE.
pub const TCP_SESSION_ID_AGGREGATE: u64 = 1 << 63;
/// Per-stack global session: stack-level RPS / goodput / send-size / SRTT / STACK_INFO.
pub const TCP_SESSION_ID_STACK_GLOBAL: u64 = (1 << 63) | 1;
// HTTP request tracking (1000-1099) — TCP_REQUEST_TRK slot lifecycle.
pub const EVT_HTTP_REQ_NEW: u32 = 1000;
pub const EVT_HTTP_REQ_FREED: u32 = 1001;
pub const EVT_HTTP_REQ_STALE: u32 = 1002;
pub const EVT_HTTP_REQ_ALLOCFAIL: u32 = 1003;
/// HTTP_REQ_FORCEFREE (id 1004): slot forcibly reclaimed (e.g. socket
/// closed with the slot still marked in-flight). Same fields as
/// HTTP_REQ_FREED minus `last_ack_us`.
pub const EVT_HTTP_REQ_FORCEFREE: u32 = 1004;
// Session
pub const EVT_SESSION_CREATE: u32 = 0xFFFFFFFE;
pub const EVT_SESSION_END: u32 = 0xFFFFFFFF;

// elog(1)-internal bookkeeping events, never emitted by the kernel: a
// provider that registers after a binary capture's header (and its
// provider table / decode-blob section) was already written gets
// embedded in-band instead, the moment its first event is about to be
// written. See elog.5's "Provider Announcement Records" and
// `write_provider_announcement()` in elog.c. `EVT_PROVIDER_INFO`'s
// payload is a `PROVIDER_SIZE_V2`-byte provider-table row (same layout
// as a normal provider row); `EVT_PROVIDER_BLOB`'s is `schema_id:u8[16]`
// + `size:u32` + that many raw EDB1 decode-blob bytes.
pub const EVT_PROVIDER_INFO: u32 = 0xFFFFFFFC;
pub const EVT_PROVIDER_BLOB: u32 = 0xFFFFFFFB;

/// HTTP request-tracking slot flags (`FLAG http_req_track_flags` in
/// `tcp_eventlog_schema.src`; mirrors `TCP_TRK_TRACK_FLG_*`).
pub const HTTP_REQ_FLG_USED: u32 = 0x01;
pub const HTTP_REQ_FLG_OPEN: u32 = 0x02;
pub const HTTP_REQ_FLG_SEQV: u32 = 0x04;
pub const HTTP_REQ_FLG_COMP: u32 = 0x08;
pub const HTTP_REQ_FLG_FSND: u32 = 0x10;
pub const HTTP_REQ_FLG_LSND: u32 = 0x20;

// ---------------------------------------------------------------------------
// "if" provider (distinct provider namespace from "tcp" / "hpts" / "cpu").
//
// The if provider streams interface lifecycle / link / config / address
// changes plus periodic per-interface counter samples. One eventlog
// session per VNET; the per-event header `session_id` is the VNET-scoped
// session. ifindex (carried in every payload) is the primary key for an
// interface; the (ifindex -> name) binding is established by IF_ATTACH,
// mutated by IF_RENAME, torn down by IF_DETACH.
// See `if_eventlog_schema.src` in the FreeBSD source tree.
// ---------------------------------------------------------------------------
pub const EVT_IF_INFO: u32 = 0;
pub const EVT_IF_ATTACH: u32 = 1;
pub const EVT_IF_DETACH: u32 = 2;
pub const EVT_IF_RENAME: u32 = 3;
pub const EVT_IF_LINK_STATE: u32 = 4;
pub const EVT_IF_FLAGS_CHANGE: u32 = 5;
pub const EVT_IF_MTU_CHANGE: u32 = 6;
pub const EVT_IF_CAPABILITIES_CHANGE: u32 = 7;
pub const EVT_IF_ADDR_ADD: u32 = 8;
pub const EVT_IF_ADDR_REMOVE: u32 = 9;
/// Deprecated: superseded by EVT_IF_SAMPLE_BYTES / EVT_IF_SAMPLE_PACKETS.
/// The kernel no longer emits this event; the decoder is kept so older
/// captures still decode.
pub const EVT_IF_SAMPLE: u32 = 20;
/// Replaces EVT_IF_SAMPLE's ibytes/obytes with a TCP/UDP protocol split
/// (plus the same totals). Paired with EVT_IF_SAMPLE_PACKETS at the same
/// sample tick -- see the merge logic in `core.rs`.
pub const EVT_IF_SAMPLE_BYTES: u32 = 21;
/// Replaces EVT_IF_SAMPLE's ipackets/opackets/ierrors/.../noproto with a
/// TCP/UDP packet-count split (plus the same totals and link-level
/// counters). Paired with EVT_IF_SAMPLE_BYTES at the same sample tick.
pub const EVT_IF_SAMPLE_PACKETS: u32 = 22;

// ---------------------------------------------------------------------------
// HPTS provider (distinct provider namespace from "tcp").
//
// Three unrelated schemas share this provider's id namespace:
//   - STARS_AGG_SAMPLE (id 16): a machine-wide snapshot of the STARS
//     adaptive egress-rate-squeeze controller (slot-gate admission
//     counters and the pressure gauge).
//   - HPTS_LATENESS_SAMPLE / HPTS_TP_BATCH_SAMPLE (ids 13-14): periodic
//     per-CPU snapshots of the hist_lateness_usec/hist_tp_batch_size
//     cumulative histograms each per-CPU hpts already maintains. Ids 13
//     and 14 previously had guessed/superseded meanings (NIC_RATE_SAMPLE,
//     SLOT_CALC/INSERT_ARC_VIOLATION from an earlier CLI draft) that were
//     never emitted by any real kernel; these are the current ground
//     truth per `hpts_eventlog_schema.src`.
//   - The timing-wheel scheduler trace (ids 0-14): per-callout tracing
//     of the hpts callout wheel itself (init, slot processing, per-TCP
//     insert/remove/move, wheel-wrap, and lateness diagnostics). See
//     `hpts_eventlog_schema.src` — offsets below are verified against
//     both that source file and a real v2 capture's embedded schema.
// ---------------------------------------------------------------------------
pub const EVT_HPTS_STARS_AGG_SAMPLE: u32 = 16;

pub const EVT_HPTS_INIT: u32 = 0;
pub const EVT_HPTS_INIT_HPTS_ENTRY: u32 = 1;
pub const EVT_HPTS_INIT_TIMING_PARAMS: u32 = 2;
pub const EVT_HPTS_STATE_ACTIVE: u32 = 3;
pub const EVT_HPTS_STATE_INACTIVE: u32 = 4;
pub const EVT_HPTS_SLEEP: u32 = 5;
pub const EVT_HPTS_CONN_INSERT: u32 = 6;
pub const EVT_HPTS_CONN_REMOVE: u32 = 7;
pub const EVT_HPTS_CONN_MOVE: u32 = 8;
pub const EVT_HPTS_WHEEL_WRAP: u32 = 9;
pub const EVT_HPTS_PROCESSING: u32 = 10;
pub const EVT_HPTS_PERF_PROCESSING: u32 = 11;
pub const EVT_HPTS_PERF_LATENESS: u32 = 12;
pub const EVT_HPTS_LATENESS_SAMPLE: u32 = 13;
pub const EVT_HPTS_TP_BATCH_SAMPLE: u32 = 14;
/// Reserved id referenced by an earlier draft of the CLI (`src/cli/hpts.rs`)
/// but **not present** in `hpts_eventlog_schema.src` nor in any embedded
/// schema seen so far — there is no ground truth for its byte layout, so
/// no decoder exists for it (see the doc comment on
/// [`HptsWheelPayload::NoRunSlotSkip`](crate::elog_parser::HptsWheelPayload::NoRunSlotSkip)).
pub const EVT_HPTS_NO_RUN_SLOT_SKIP: u32 = 15;
pub const EVT_HPTS_OPPOR_SAMPLE: u32 = 17;
/// `HPTS_SLOT_GATE_SAMPLE` (id 18): per-CPU interval counters for the
/// HPTS slot gate. Fixed layout, naturally aligned: fires/diverts u32,
/// bytes u64, then rollover_max/walks/closed u32 (28 bytes).
pub const EVT_HPTS_SLOT_GATE_SAMPLE: u32 = 18;

/// `ENUM bind_type` in `hpts_eventlog_schema.src` — how the hpts
/// timing-wheel threads are bound to CPUs.
pub const HPTS_BIND_TYPE_NONE: u8 = 0;
pub const HPTS_BIND_TYPE_CPU: u8 = 1;
pub const HPTS_BIND_TYPE_NUMA: u8 = 2;

// ---------------------------------------------------------------------------
// CPU provider (distinct provider namespace from "tcp" / "hpts").
//
// The kernel's `cpu_eventlog` provider emits per-CPU kernel-thread stack
// samples for flame-graph reconstruction. One eventlog session is created
// per CPU; the framework's per-event header `session_id` is the cpu id.
// See `cpu_eventlog_schema.src` in the FreeBSD source tree for the
// authoritative wire layout.
// ---------------------------------------------------------------------------
pub const EVT_CPU_KERNEL_INFO: u32 = 0;
pub const EVT_CPU_THREAD_CREATE: u32 = 1;
pub const EVT_CPU_THREAD_EXIT: u32 = 2;
pub const EVT_CPU_SAMPLE: u32 = 3;
pub const EVT_CPU_OVERFLOW: u32 = 4;
/// `LINKER_FILE` describes one loaded linker file (the kernel itself or
/// a `.ko` module). Emitted by the kernel on subscribe (`-D`) and on
/// future MOD_LOAD events; carries the `file_id` later referenced by
/// `SYMBOL_RANGE`. See `cpu_eventlog_schema.src` for the wire layout.
pub const EVT_CPU_LINKER_FILE: u32 = 5;
/// `SYMBOL_RANGE` is **not** emitted by the kernel. `elog(1)` with
/// `--embed-symbols` (a.k.a. `-S`) opens each linker file's on-disk
/// ELF, parses `.symtab`, and synthesises one event per unique
/// `(file_id, symbol)` tuple referenced by any `CPU_SAMPLE`. We fold
/// these into a per-file sorted symbol table so the renderer / CLI can
/// turn raw PCs into `name+0xINNER` labels offline.
pub const EVT_CPU_SYMBOL_RANGE: u32 = 6;

/// `USER_FILE`, `PROC_MAP`, `USER_SYMBOL_RANGE` are the user-space
/// counterparts of `LINKER_FILE` / `SYMBOL_RANGE`, also synthesised
/// by `elog(1)` (the `-U` flag, default in `oca.py get profile`).
/// When `elog` sees a `CPU_SAMPLE` with the [`CPU_SAMPLE_FLAG_USER_MODE`]
/// bit set (or, in practice, any sample at all when `-U` is on), it
/// queries libprocstat for the sampled pid's `vm_map`, canonicalises
/// each file-backed mapping to a stable `user_file_id`, opens the
/// backing ELF via libelf, and emits:
///
/// * one `USER_FILE` per unique backing ELF across the whole capture
///   (content-addressed by the ELF's build-id when available;
///   different file paths with identical contents collapse to one
///   entry).
/// * one `PROC_MAP` per `(pid, file)` mapping the pid's runtime VA
///   range to a `user_file_id` plus the on-disk file offset
///   (`vm_map_entry::offset`) of the mapping (so
///   `pc - pm.base + pm.offset` yields the file offset for
///   binary-searching the symbol table — works uniformly for
///   `ET_EXEC` and `ET_DYN`/PIE).
/// * one `USER_SYMBOL_RANGE` per unique `(user_file_id, function)`
///   tuple referenced by any sample. Stored in file-offset
///   coordinates so processes that share a DSO share the symbol table.
///
/// The wire format deliberately mirrors `LINKER_FILE` /
/// `SYMBOL_RANGE`; consumer-side, [`CpuProfile::symbolize`] tries
/// the kernel resolver first, then falls back to the user path
/// when the sample's pid is known and a `PROC_MAP` covers the PC.
pub const EVT_CPU_USER_FILE: u32 = 7;
pub const EVT_CPU_PROC_MAP: u32 = 8;
pub const EVT_CPU_USER_SYMBOL_RANGE: u32 = 9;

/// `EVT_CPU_PROC_EXEC` (event id 10) and `EVT_CPU_PROC_EXEC_MAP`
/// (event id 11) are emitted by the kernel synchronously with each
/// `execve()` so the user-space resolver in elog(1) can prime its
/// per-process executable-mapping cache before the process returns
/// to userspace. `EVT_CPU_PROC_MMAP` (event id 12) is emitted for
/// every successful `mmap(PROT_EXEC)` of a file-backed region that
/// happens after `execve()` — RTLD library loads, later `dlopen()`s.
///
/// All three are consumed in place by elog(1) -U and NOT written to
/// the output stream, so a well-formed capture should contain zero
/// of them. If they appear here they were produced by a capture
/// without `-U` and we record counts for diagnostic purposes only.
pub const EVT_CPU_PROC_EXEC: u32 = 10;
pub const EVT_CPU_PROC_EXEC_MAP: u32 = 11;
pub const EVT_CPU_PROC_MMAP: u32 = 12;
/// `SYMBOL_PACK_REF` binds a `LINKER_FILE.file_id` (kind 0) or
/// `USER_FILE.user_file_id` (kind 1) to a content-addressed symbol
/// pack (`default/sde-elog/v1/symbols/<build_id>`). The capture keeps
/// identity rows only; the pack's ranges fill in at read time (see
/// `elog_parser/packs.rs`). In-file ranges always win over the pack.
pub const EVT_CPU_SYMBOL_PACK_REF: u32 = 13;

// ---------------------------------------------------------------------------
// Split-provider schema (newer captures)
//
// Newer kernels emit a redesigned eventlog layout that moves symbol and
// process metadata out of the "cpu" provider into dedicated "symbols" and
// "process" providers, each with its own event-id namespace starting at 0.
// The "cpu" provider then carries only per-CPU info (id 0) and stack
// samples (id 1). Payload layouts are byte-compatible with the legacy
// cpu-provider events of the same kind (apart from KERNEL_INFO, which no
// longer carries ncpus/hz inline), so the parser translates these ids onto
// the EVT_CPU_* decoders. A capture is treated as split-schema when it
// lists a "symbols" or "process" provider.
// ---------------------------------------------------------------------------
// cpu provider, split schema:
//   id 0 = per-CPU info (ncpus, hz)
//   id 1 = stack sample (legacy EVT_CPU_SAMPLE layout)
pub const EVT_CPUX_INFO: u32 = 0;
pub const EVT_CPUX_SAMPLE: u32 = 1;
// symbols provider
pub const EVT_SYM_KERNEL_INFO: u32 = 0;
pub const EVT_SYM_LINKER_FILE: u32 = 1;
pub const EVT_SYM_SYMBOL_RANGE: u32 = 2;
pub const EVT_SYM_USER_FILE: u32 = 3;
pub const EVT_SYM_USER_SYMBOL_RANGE: u32 = 4;
/// `SOURCE_FILE` / `SOURCE_LINE` are synthesised by `elog -L`. They stay
/// in the symbols provider's own id space (5 and 6); the legacy cpu
/// provider used those ids for `LINKER_FILE` / `SYMBOL_RANGE`.
pub const EVT_SYM_SOURCE_FILE: u32 = 5;
pub const EVT_SYM_SOURCE_LINE: u32 = 6;
/// `SYMBOL_PACK_REF` names a binary's symbol pack by GNU build-id: the
/// pack (not the capture) carries the function ranges. Emitted at
/// dump-state next to `LINKER_FILE` (kernel) and synthesised by
/// `elog -U` (user). Routed onto `EVT_CPU_SYMBOL_PACK_REF` below.
pub const EVT_SYM_SYMBOL_PACK_REF: u32 = 7;
/// `SOURCE_FILE.binary_kind`: the id is a `LINKER_FILE.file_id`.
pub const SOURCE_BINARY_KIND_LINKER: u8 = 0;
/// `SOURCE_FILE.binary_kind`: the id is a `USER_FILE.user_file_id`.
pub const SOURCE_BINARY_KIND_USER: u8 = 1;
// process provider
pub const EVT_PROC_THREAD_CREATE: u32 = 0;
pub const EVT_PROC_THREAD_EXIT: u32 = 1;
/// PROC_EXEC (id 2): `do_execve()` completed an image switch. Parsed into
/// [`CpuProfile::proc_execs`] so flame group bars use the image name at
/// sample time rather than the fork-time `THREAD_CREATE` `comm`.
pub const EVT_PROC_EXEC: u32 = 2;
/// PROC_EXIT (id 3): a process exited. Clears that pid's
/// [`CpuProfile::proc_execs`] so a reused pid does not keep the old name.
pub const EVT_PROC_EXIT: u32 = 3;
pub const EVT_PROC_MAP: u32 = 4;
/// PROC_FORK (id 5): `fork()` created `pid` as a copy of `ppid`.
/// Fork-without-exec inherits the parent's address space, so the parser
/// copies the parent's current `PROC_MAP` set onto the child pid — see
/// `CpuProfile::apply_proc_fork`.
pub const EVT_PROC_FORK: u32 = 5;

pub const EVT_MACHINE_INFO: u32 = 1;
pub const EVT_MACHINE_METADATA_KV: u32 = 2;

/// `mem` provider (`mem_eventlog_schema.src`): static context, then the
/// periodic system-wide page-state sample. All sizes are bytes.
pub const EVT_MEM_INFO: u32 = 0;
pub const EVT_MEM_SYS_SAMPLE: u32 = 1;

/// `LINKER_FILE.flags` bits (FLAG linker_file_flags in the schema).
/// Mirror of FreeBSD's `linker_file::flags` (the LINKED bit means
/// "linked into the kernel address space, vs. unloaded").
pub const LINKER_FILE_FLAG_LINKED: u32 = 0x01;
/// This linker file is the kernel itself, not a loadable module. Set
/// by the kernel sampler when `lf == linker_kernel_file`. The CPU
/// symbolizer trusts the on-disk symbol table for KERNEL files but
/// not for modules (ET_REL relocations aren't reconstructible from
/// the on-disk ELF alone — a v1 limitation flagged in elog_symbols.c).
pub const LINKER_FILE_FLAG_KERNEL: u32 = 0x02;

/// Schema-side upper bound on `frames[]` length per `CPU_SAMPLE`
/// event (matches `frames:uint64_t[depth:64]` in the schema). This
/// is the **wire-format ceiling**, not what the kernel sampler can
/// actually emit today: FreeBSD's per-sample stack buffer is
/// `STACK_MAX` (currently `18`, defined in `sys/sys/_stack.h`), so
/// the kernel will never deliver `depth > 18` regardless of what
/// the schema would allow. Used here as a defensive bound when
/// decoding payloads — anything larger than 64 must be a corrupt
/// header and is clamped down rather than allocated.
pub const CPU_SAMPLE_FRAMES_MAX: usize = 64;

/// `CPU_SAMPLE.flags` bits (FLAG sample_flags in cpu_eventlog_schema.src).
/// The leaf instruction was in user mode (currently always clear; kernel
/// stack walks unwind to the syscall trampoline instead).
pub const CPU_SAMPLE_FLAG_USER_MODE: u8 = 0x01;

/// The sampled thread was the per-CPU idle thread; `frames` will be empty.
/// Useful as a default-off filter — most idle samples are not informative
/// on a busy box but they are the only signal of "what fraction of CPU
/// time was spent doing nothing" on a quiet one.
pub const CPU_SAMPLE_FLAG_IDLE: u8 = 0x02;
/// The sample was taken with `td_intr_nesting_level > 0` (interrupt
/// context) or the thread is an `ithread`. The captured stack reflects
/// the interrupt handler, not the interrupted code.
pub const CPU_SAMPLE_FLAG_INTR: u8 = 0x04;
/// The kernel-side stack-walk buffer was completely filled before
/// the unwinder reached the bottom of the call stack. The buffer is
/// `STACK_MAX` entries (currently `18` on FreeBSD; see
/// `sys/sys/_stack.h`); when `stack_save_intr()` returns `stk.depth
/// == STACK_MAX`, `cpu_elog_statclock` sets this flag. A sample
/// tagged TRUNC therefore has *exactly* `STACK_MAX` captured frames
/// today, not 64 — the schema's `frames[depth:64]` is just the
/// wire-format ceiling. The bottom-most frame in `frames` is
/// therefore not the actual root of the stack and aggregating
/// across truncated samples can over-attribute time to the
/// unwinder's stop point.
pub const CPU_SAMPLE_FLAG_TRUNC: u8 = 0x08;
/// `freq_mhz` carries a meaningful effective-CPU-frequency reading
/// (derived kernel-side from APERF/MPERF). Unset on captures predating
/// this field and on hardware without a working APERF/MPERF pair.
pub const CPU_SAMPLE_FLAG_FREQ_VALID: u8 = 0x10;

/// `CPU_SAMPLE.sample_type` enum (ENUM sample_type in the schema).
pub const CPU_SAMPLE_TYPE_STATCLOCK: u8 = 0;
pub const CPU_SAMPLE_TYPE_HWPMC: u8 = 1;

// ---------------------------------------------------------------------------
// EventLog v2 "decode blob" (`EDB1`) wire format — a self-describing
// per-provider schema embedded in the file, letting a v2 capture be
// decoded without any provider-specific code. Mirrors
// `sys/sys/eventlog_decode.h`; see `src/elog_parser/schema.rs` for the
// parser and `docs/file-format.md` for the on-disk layout.
// ---------------------------------------------------------------------------
pub const EVENTLOG_DECODE_MAGIC: &[u8; 4] = b"EDB1";
pub const EVENTLOG_DECODE_VERSION: u16 = 1;
pub const EVENTLOG_DECODE_ENDIAN_LE: u8 = 0;

/// Upper bound on a single decode blob (`EVENTLOG_DECODE_BLOB_MAX`),
/// bounding allocations/copies for blobs read back from an untrusted
/// capture file.
pub const EVENTLOG_DECODE_BLOB_MAX: usize = 16 * 1024 * 1024;

// Field element type codes (`EVENTLOG_DECODE_TYPE_*`).
pub const DECODE_TYPE_U8: u8 = 1;
pub const DECODE_TYPE_U16: u8 = 2;
pub const DECODE_TYPE_U32: u8 = 3;
pub const DECODE_TYPE_U64: u8 = 4;
pub const DECODE_TYPE_I8: u8 = 5;
pub const DECODE_TYPE_I16: u8 = 6;
pub const DECODE_TYPE_I32: u8 = 7;
pub const DECODE_TYPE_I64: u8 = 8;
pub const DECODE_TYPE_PTR: u8 = 9;
pub const DECODE_TYPE_IN_ADDR: u8 = 10;
pub const DECODE_TYPE_IN6_ADDR: u8 = 11;
pub const DECODE_TYPE_CHAR_ARRAY: u8 = 12;

// Field display annotations (`EVENTLOG_DECODE_ANNOT_*`).
pub const DECODE_ANNOT_NONE: u8 = 0;
pub const DECODE_ANNOT_HEX: u8 = 1;
pub const DECODE_ANNOT_NTOHS: u8 = 2;
pub const DECODE_ANNOT_ENUM: u8 = 3;
pub const DECODE_ANNOT_FLAG: u8 = 4;

/// Field flag bits (`EVENTLOG_DECODE_FIELD_*`).
pub const DECODE_FIELD_VARLEN: u8 = 0x01;

/// Upper bound on a printf field width taken from a decode blob's format
/// string (`EVENTLOG_DECODE_MAX_FIELD_WIDTH`) — defensive against a
/// hostile blob's `"%999999999d"`.
pub const DECODE_MAX_FIELD_WIDTH: u32 = 256;
/// Upper bound on the number of `%`-placeholders formatted per event
/// (`EVENTLOG_DECODE_MAX_FORMAT_FIELDS`) — defensive against a hostile
/// blob pairing a huge format string with a huge struct/table.
pub const DECODE_MAX_FORMAT_FIELDS: usize = 256;

// ---------------------------------------------------------------------------
// ELLA `fanout_node` channel-layer events (corp/ella PR #456). Older
// captures register the same catalog as `ella_relay`. Session id 0 is the
// process-wide global session; every other session is one channel, shared
// by both node sides. Layouts are C natural alignment.
// ---------------------------------------------------------------------------

pub const ELLA_RELAY_GLOBAL_SESSION: u64 = 0;

pub const EVT_ELLA_RELAY_INSTANCE_ATTACH: u32 = 0;
pub const EVT_ELLA_RELAY_NODE_ERROR: u32 = 1;
pub const EVT_ELLA_RELAY_RELAY_ERROR: u32 = EVT_ELLA_RELAY_NODE_ERROR;
pub const EVT_ELLA_RELAY_NODE_INFO: u32 = 2;
pub const EVT_ELLA_RELAY_SESSION_CREATE: u32 = 50;
pub const EVT_ELLA_RELAY_SESSION_DESTROY: u32 = 51;
pub const EVT_ELLA_RELAY_CHANNEL_STATE: u32 = 52;
pub const EVT_ELLA_RELAY_CHANNEL_ID_ASSIGNED: u32 = 53;
pub const EVT_ELLA_RELAY_CHANNEL_ERROR: u32 = 54;
pub const EVT_ELLA_RELAY_CHANNEL_DESTROYED: u32 = 55;
pub const EVT_ELLA_RELAY_CHANNEL_SUBSCRIBER: u32 = 100;
pub const EVT_ELLA_RELAY_SUBSCRIBER_IDENTITY: u32 = 101;
pub const EVT_ELLA_RELAY_CHANNEL_COUNT: u32 = 150;
pub const EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL: u32 = 151;
pub const EVT_ELLA_RELAY_CHANNEL_SAMPLE: u32 = 152;
pub const EVT_ELLA_RELAY_FRAME_SAMPLE: u32 = 153;
pub const EVT_ELLA_RELAY_OFFLOAD_STATS: u32 = 154;
pub const EVT_ELLA_RELAY_TRAFFIC_STATS: u32 = 155;
pub const EVT_ELLA_RELAY_DELIVERED_STATS: u32 = 156;
pub const EVT_ELLA_RELAY_OBJECT_STATE: u32 = 200;
pub const EVT_ELLA_RELAY_OBJECT_TIMING: u32 = 201;
pub const EVT_ELLA_RELAY_FRAME_SENT: u32 = 250;
pub const EVT_ELLA_RELAY_FRAME_RELAY: u32 = 251;

pub const ELLA_SIDE_UPSTREAM: u8 = 0;
pub const ELLA_SIDE_DOWNSTREAM: u8 = 1;

/// `NODE_INFO.role`. The producer's ORIGIN is what the GUI/CLI call the
/// Server.
pub const ELLA_ROLE_ORIGIN: u8 = 0;
pub const ELLA_ROLE_RELAY: u8 = 1;

pub const ELLA_LIFECYCLE_REQUESTED: u8 = 0;
pub const ELLA_LIFECYCLE_CREATED: u8 = 1;
pub const ELLA_LIFECYCLE_CREATE_FAILED: u8 = 2;
pub const ELLA_LIFECYCLE_CREATE_ROLLED_BACK: u8 = 3;
pub const ELLA_LIFECYCLE_CREATE_TIMED_OUT: u8 = 4;
pub const ELLA_LIFECYCLE_CREATE_REJECTED: u8 = 5;
pub const ELLA_LIFECYCLE_RETIRE_ARMED: u8 = 6;
pub const ELLA_LIFECYCLE_LEAVE_DEFERRED: u8 = 7;
pub const ELLA_LIFECYCLE_CANCELLED: u8 = 8;

// Canonical `subscriber_state` numbering (ella#460). A capture whose
// blob numbers `UNSUBSCRIBED` 2 is mapped onto these on ingest.
pub const ELLA_SUBSCRIBER_SUBSCRIBING: u8 = 0;
pub const ELLA_SUBSCRIBER_SUBSCRIBED: u8 = 1;
pub const ELLA_SUBSCRIBER_UNSUBSCRIBING: u8 = 2;
pub const ELLA_SUBSCRIBER_UNSUBSCRIBED: u8 = 3;

pub const ELLA_OBJECT_STARTED: u8 = 0;
pub const ELLA_OBJECT_COMPLETED: u8 = 1;
pub const ELLA_OBJECT_ABANDONED: u8 = 2;

// ---------------------------------------------------------------------------
// nginx user-space provider (`nflx_eventlog`). One eventlog session per
// transport connection (TCP_EVENTLOG_SESSION_ID). REQUEST_START /
// RESPONSE_HEADERS / REQUEST_DONE are events on that session, tagged with
// the HTTP/1 request serial or HTTP/2 stream id. Pre-release captures used C
// natural alignment and a fixed `uri[256]`; shipped firmware packs every
// struct and carries `uri` as a varlen field. See
// `nginx_eventlog_schema.src` in the ocafirmware tree.
// ---------------------------------------------------------------------------

pub const EVT_NGINX_REQUEST_START: u32 = 0;
pub const EVT_NGINX_RESPONSE_HEADERS: u32 = 1;
pub const EVT_NGINX_REQUEST_DONE: u32 = 2;
// `$nflx_message` text attached to a request.
pub const EVT_NGINX_REQUEST_MESSAGE: u32 = 3;
// Per-worker sampled request stats, once per second per worker.
pub const EVT_NGINX_SAMPLE: u32 = 100;
// Connection lifecycle (NCD-10222).
pub const EVT_NGINX_CONN_START: u32 = 200;
pub const EVT_NGINX_CONN_END: u32 = 201;
pub const EVT_NGINX_TLS_HANDSHAKE_DONE: u32 = 300;
pub const EVT_NGINX_TLS_HANDSHAKE_FAIL: u32 = 301;
// Disk back-pressure and errors on the connection session.
pub const EVT_NGINX_SENDFILE_BUSY_DEFER: u32 = 400;
pub const EVT_NGINX_SENDFILE_ERROR: u32 = 401;
pub const EVT_NGINX_FILE_OPEN_FAIL: u32 = 402;
// Cache lookups and upstream (fill) attempts, tagged with the main request.
pub const EVT_NGINX_CACHE_LOOKUP: u32 = 500;
pub const EVT_NGINX_UPSTREAM_ATTEMPT_START: u32 = 501;
pub const EVT_NGINX_UPSTREAM_ATTEMPT_END: u32 = 502;
// Worker-wide events on the special worker session.
pub const EVT_NGINX_WORKER_STALL: u32 = 600;
pub const EVT_NGINX_AIO_SYNC_FALLBACK: u32 = 601;
