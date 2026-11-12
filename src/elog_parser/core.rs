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
// Binary .elog parser
// ---------------------------------------------------------------------------

/// Shared implementation for parsing an elog byte slice.
///
/// When `progress` is provided, reports permille values:
///   0-50   file open/mmap (handled by caller)
///   50-800 event parsing (payload decoding)
///   800-900 per-session sort (parallel)
///   900+   returned to caller for summary computation
/// Which decoder family a provider id routes to. Several providers can
/// share a name (e.g. one "tcp" provider per loaded TCP stack), so the
/// event loop dispatches on kind, resolved from a `provider_id -> kind`
/// map built during the provider-table scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Tcp,
    Hpts,
    Cpu,
    If,
    /// Split-schema "symbols" provider: kernel/user symbolisation metadata
    /// that older captures carried inside the "cpu" provider.
    Symbols,
    /// Split-schema "process" provider: thread/process lifecycle and proc
    /// maps that older captures carried inside the "cpu" provider.
    Process,
    EllaRelay,
    Nginx,
    Machine,
    Mem,
    Other,
}

/// Maps a provider table row's `name` to the decoder family that routes
/// its events. Shared by the header-time provider table scan and the
/// in-band `EVT_PROVIDER_INFO` announcement handling in the main event
/// loop below.
pub(crate) fn provider_kind_for_name(name: &str) -> ProviderKind {
    match name {
        "tcp" => ProviderKind::Tcp,
        "hpts" => ProviderKind::Hpts,
        "cpu" => ProviderKind::Cpu,
        "if" => ProviderKind::If,
        "symbols" => ProviderKind::Symbols,
        "process" => ProviderKind::Process,
        "ella_relay" | "fanout_node" => ProviderKind::EllaRelay,
        "nginx" => ProviderKind::Nginx,
        "machine" => ProviderKind::Machine,
        "mem" => ProviderKind::Mem,
        _ => ProviderKind::Other,
    }
}

/// Records a resolved `ProviderSchema` on `provider` and re-validates the
/// hardcoded decoders for `kind` against it (`schema_validate::
/// validate_hardcoded_decoders`), filling in `provider.schema_*` and, on
/// divergence, `divergent[provider.id]`. Shared by the header-time blob
/// table pass and the in-band `EVT_PROVIDER_BLOB` announcement handling in
/// the main event loop below, so a late-registering provider's schema is
/// validated exactly the same way as one present at file-open time.
fn apply_resolved_schema(
    provider: &mut Provider,
    kind: ProviderKind,
    schema: &ProviderSchema,
    divergent: &mut FxHashMap<u16, FxHashSet<u32>>,
) {
    provider.schema_name = Some(schema.name.clone());
    provider.schema_event_count = schema.events.len();
    let expected: &[ExpectedEvent] = match kind {
        ProviderKind::Tcp => TCP_EXPECTED,
        ProviderKind::If => IF_EXPECTED,
        ProviderKind::Cpu => CPU_EXPECTED,
        ProviderKind::Hpts => HPTS_EXPECTED,
        ProviderKind::EllaRelay => ELLA_RELAY_EXPECTED,
        ProviderKind::Nginx => NGINX_EXPECTED,
        ProviderKind::Symbols
        | ProviderKind::Process
        | ProviderKind::Machine
        | ProviderKind::Mem
        | ProviderKind::Other => &[],
    };
    let result = validate_hardcoded_decoders(schema, expected);
    provider.schema_divergent_events = result.divergent_reasons;
    provider.schema_new_fields = result.new_fields;
    if !result.divergent.is_empty() {
        divergent.insert(provider.id, result.divergent);
    }
}

/// Leading event timestamps retained for backlog detection. Sized well
/// past any plausible registration-record backlog while staying a fixed,
/// trivial allocation on multi-hundred-million-event captures.
const BACKLOG_PROBE_EVENTS: usize = 4096;
/// Minimum length of the idle gap that separates drained backlog from the
/// live capture. Every provider in a live capture goes silent together
/// for this long only when nothing was being captured yet: the periodic
/// samplers (`cpu`, `if`, `tcp`) tick far faster.
const BACKLOG_GAP_MIN_US: u64 = 60_000_000;
/// Share of the capture's events the backlog may account for. Keeps a
/// genuinely sparse opening *phase* of a capture from being mistaken for
/// backlog, which would move the origin into the middle of real data.
const BACKLOG_MAX_SHARE: f64 = 0.001;
/// Backlog size allowed regardless of [`BACKLOG_MAX_SHARE`], so the
/// detection still works on a capture too short for the share test to
/// admit anything.
const BACKLOG_MIN_ALLOWANCE: usize = 8;

/// The monotonic instant a capture's relative time axis should start at,
/// given the header's `capture_start` and the timestamps of the capture's
/// first [`BACKLOG_PROBE_EVENTS`] events.
///
/// `elog(1)` writes the file header when it drains its first event, using
/// that event's timestamp as `capture_start` and a `gettimeofday()` taken
/// right then as `start_utc_us`. The two only name the same instant when
/// the first drained event is also a live one. A user-space provider
/// registers with `libeventlog` and writes an identity record
/// immediately, so a provider that had been running for a while before
/// `elog` attached leaves that record sitting in its ring — and draining
/// it backdates `capture_start` (but not `start_utc_us`) by the
/// provider's whole uptime. That shifts every rendered wall-clock label
/// forward by the same amount and pads the time axis with an interval
/// that was never captured.
///
/// Recover the live start by looking for the idle gap between the two:
/// backlog is a handful of records at the very front, then nothing at all
/// until `elog` starts streaming.
fn effective_capture_start(header_start: u64, prefix: &[u64], total_events: usize) -> (u64, usize) {
    let mut sorted = prefix.to_vec();
    sorted.sort_unstable();
    let Some((gap, live_idx)) = (1..sorted.len())
        .map(|i| (sorted[i] - sorted[i - 1], i))
        .max_by_key(|&(gap, _)| gap)
    else {
        return (header_start, 0);
    };
    let allowance = ((total_events as f64 * BACKLOG_MAX_SHARE) as usize).max(BACKLOG_MIN_ALLOWANCE);
    if gap < BACKLOG_GAP_MIN_US || live_idx > allowance {
        return (header_start, 0);
    }
    (sorted[live_idx], live_idx)
}

/// Initial `Vec` capacity for one TCP session: the capture's average
/// events per connection, clamped so a one-session file still grows by
/// doubling and a tiny average still starts with a handful of slots.
fn session_vec_cap(tcp_events: usize, tcp_sessions: usize) -> usize {
    (tcp_events / tcp_sessions.max(1)).clamp(32, 4096)
}

pub fn parse_elog_core(data: &[u8], progress: Option<&LoadProgress>) -> Result<ParsedCapture> {
    parse_elog_from(&mut SliceSrc(data), progress)
}

pub fn parse_elog_chunked(
    data: &mut ChunkedSrc,
    progress: Option<&LoadProgress>,
) -> Result<ParsedCapture> {
    parse_elog_from(data, progress)
}

/// Shared shape of kernel [`SymbolRange`] and user [`UserSymbolRange`]
/// for the deterministic alias-dedup sort in `parse_elog_from`. Several
/// names can share one address (libc aliases like `_select` /
/// `__sys_select`); within a same-offset group we prefer the name with
/// the fewest leading underscores, which is the libc public name.
trait HasSymbolRange {
    fn sym_offset(&self) -> u64;
    fn sym_name(&self) -> &str;
    fn leading_underscores(&self) -> usize {
        let n = self.sym_name();
        n.len() - n.trim_start_matches('_').len()
    }
}

impl HasSymbolRange for crate::elog_parser::types::SymbolRange {
    fn sym_offset(&self) -> u64 {
        self.offset
    }
    fn sym_name(&self) -> &str {
        &self.name
    }
}

impl HasSymbolRange for crate::elog_parser::types::UserSymbolRange {
    fn sym_offset(&self) -> u64 {
        self.offset
    }
    fn sym_name(&self) -> &str {
        &self.name
    }
}

/// Sort a symbol-range vector and drop duplicate offsets, keeping the
/// public-facing alias. Stable sort on (offset, fewest leading
/// underscores), preserving stream order on a full tie, then dedup on
/// offset. Deterministic across builds and reproducible by the Python
/// `sde_elog_flame` fold — unlike `sort_unstable_by_key(offset)`, which
/// let the surviving alias depend on Rust's sort internals.
fn sort_symbol_ranges<S: HasSymbolRange>(syms: &mut Vec<S>) {
    syms.sort_by_key(|s| (s.sym_offset(), s.leading_underscores()));
    syms.dedup_by(|a, b| a.sym_offset() == b.sym_offset());
}

fn parse_elog_from(
    data: &mut impl ByteSrc,
    progress: Option<&LoadProgress>,
) -> Result<ParsedCapture> {
    if let Some(p) = progress {
        p.set_status("Parsing events…");
    }
    // Common prefix through event_count (magic+version+capture_start+
    // start_utc_us+event_count = 32 bytes) is unchanged across all
    // versions; what follows it at offset 32 is version-dependent.
    const COMMON_HDR_SIZE: usize = 32;
    if data.len() < COMMON_HDR_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("File too small ({} bytes)", data.len()),
        ));
    }

    let mut magic = [0u8; 4];
    magic.copy_from_slice(data.slice(0, 4));
    if &magic != b"ELOG" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Bad magic: {:?} (expected b\"ELOG\")", magic),
        ));
    }

    let version = data.u32(4);
    let capture_start = data.u64(8);
    let start_utc_us = data.u64(16);
    let event_count = data.u64(24);

    // v3 replaced the fixed `dropped_events:u64` at offset 32 with
    // `counter_count:u32` followed by that many trailing packed `u64`
    // counters (see "Header Counters" in docs/file-format.md); v1/v2 files
    // instead have that single u64 there and nothing after. Reading
    // `dropped` back out of index 0 of the v3 array preserves the same
    // meaning (`ELOG_COUNTER_KERNEL_DROPPED`) as the old scalar field.
    let (dropped, user_dropped_full, user_dropped_prejoin, user_dropped_late, mut off);
    if version >= 3 {
        if data.len() < FILE_HDR_SIZE_V3 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "File truncated in header (v3 counter_count)",
            ));
        }
        let counter_count = data.u32(COMMON_HDR_SIZE) as usize;
        if counter_count > HEADER_COUNTER_SANE_MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid header counter count {counter_count} in file"),
            ));
        }
        if data.len() < FILE_HDR_SIZE_V3 + counter_count * 8 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "File truncated in header counters",
            ));
        }
        let mut counter_at = |idx: usize| -> u64 {
            if idx < counter_count {
                data.u64(FILE_HDR_SIZE_V3 + idx * 8)
            } else {
                0
            }
        };
        dropped = counter_at(COUNTER_KERNEL_DROPPED);
        user_dropped_full = counter_at(COUNTER_USER_DROPPED_FULL);
        user_dropped_prejoin = counter_at(COUNTER_USER_DROPPED_PREJOIN);
        user_dropped_late = counter_at(COUNTER_USER_DROPPED_LATE);
        off = FILE_HDR_SIZE_V3 + counter_count * 8;
    } else {
        if data.len() < FILE_HDR_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("File too small ({} bytes)", data.len()),
            ));
        }
        dropped = data.u64(COMMON_HDR_SIZE);
        user_dropped_full = 0;
        user_dropped_prejoin = 0;
        user_dropped_late = 0;
        off = FILE_HDR_SIZE;
    }

    if off + 4 > data.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "File truncated in provider count",
        ));
    }
    let prov_count = data.u32(off) as usize;
    off += 4;

    let mut providers = Vec::with_capacity(prov_count);
    // A capture can register several providers that share a name — most
    // notably one "tcp" provider per loaded TCP stack, each with its own
    // id. Map every provider id to its kind (for event routing) and its
    // index into `providers` (for the per-provider event count below) so
    // the event loop never has to scan `providers` looking for a match —
    // that scan used to run once per event, i.e. once per hundred-million
    // events on a large capture.
    let mut provider_kind: FxHashMap<u16, (ProviderKind, usize)> = FxHashMap::default();

    // v2+ provider rows are 16 bytes wider than v1 (`schema_id:u8[16]`
    // trails the name) — see the "Version 2" section of
    // docs/file-format.md. Unchanged by the v3 header-counters bump.
    // `provider_schema_id` is resolved against the blob table read just
    // below the provider table.
    let is_v2 = version >= 2;
    let provider_row_size = if is_v2 {
        PROVIDER_SIZE_V2
    } else {
        PROVIDER_SIZE
    };
    let mut provider_schema_id: FxHashMap<u16, [u8; SCHEMA_ID_LEN]> = FxHashMap::default();

    for _ in 0..prov_count {
        if off + provider_row_size > data.len() {
            break;
        }
        let pid = data.u16(off);
        let pname_bytes = data.slice(off + 2, off + 34);
        let end = pname_bytes.iter().position(|&b| b == 0).unwrap_or(32);
        let name = String::from_utf8_lossy(&pname_bytes[..end]).to_string();
        let kind = provider_kind_for_name(&name);
        provider_kind.insert(pid, (kind, providers.len()));
        if is_v2 {
            let mut sid = [0u8; SCHEMA_ID_LEN];
            sid.copy_from_slice(data.slice(off + 34, off + 34 + SCHEMA_ID_LEN));
            provider_schema_id.insert(pid, sid);
        }
        providers.push(Provider {
            id: pid,
            name: name.clone(),
            event_count: 0,
            first_ts: None,
            last_ts: None,
            session_count: 0,
            byte_size: 0,
            schema_name: None,
            schema_event_count: 0,
            schema_divergent_events: Vec::new(),
            schema_new_fields: Vec::new(),
            events: Vec::new(),
        });
        off += provider_row_size;
    }

    // v2 only: `blob_count:u32` then `blob_count` × `{schema_id:u8[16],
    // size:u32, blob:[u8;size]}`. Each blob is a self-contained `EDB1`
    // decode description; `schema_id` content-addresses it
    // (`compute_schema_id` = `SHA256(blob)[..SCHEMA_ID_LEN]`). Blobs are
    // deduplicated by schema_id so identical schemas shared across
    // providers (e.g. multiple "tcp" stacks) are only parsed once.
    let mut schema_by_provider_id: FxHashMap<u16, ProviderSchema> = FxHashMap::default();
    let mut divergent: FxHashMap<u16, FxHashSet<u32>> = FxHashMap::default();
    // Kept in scope past the header's own blob table (unlike the rest of
    // this block's locals) so an in-band EVT_PROVIDER_BLOB announcement
    // discovered later, in the event loop, can add to the same map — see
    // the interception at the top of the main `while` loop below.
    let mut blob_by_schema_id: FxHashMap<[u8; SCHEMA_ID_LEN], ProviderSchema> =
        FxHashMap::default();
    if is_v2 && off + 4 <= data.len() {
        let blob_count = data.u32(off) as usize;
        off += 4;
        for _ in 0..blob_count {
            if off + SCHEMA_ID_LEN + 4 > data.len() {
                break;
            }
            let mut sid = [0u8; SCHEMA_ID_LEN];
            sid.copy_from_slice(data.slice(off, off + SCHEMA_ID_LEN));
            off += SCHEMA_ID_LEN;
            let blob_size = data.u32(off) as usize;
            off += 4;
            if blob_size > EVENTLOG_DECODE_BLOB_MAX || off + blob_size > data.len() {
                break;
            }
            let blob = data.slice(off, off + blob_size);
            off += blob_size;

            // Verify the content hash before trusting anything the blob
            // claims (mirrors kern_eventlog.c's registrar). A mismatch
            // means corruption or a hostile file; treat the blob as
            // unresolved rather than parse+trust unverified bytes.
            if compute_schema_id(blob) != sid {
                continue;
            }
            if let Some(schema) = ProviderSchema::parse(blob) {
                blob_by_schema_id.insert(sid, schema);
            }
        }
        for (pid, sid) in &provider_schema_id {
            if let Some(schema) = blob_by_schema_id.get(sid) {
                schema_by_provider_id.insert(*pid, schema.clone());
            }
        }

        // One-time, per-provider validation: does this hardcoded decoder
        // family still match the resolved schema's byte layout? See
        // `schema_validate::validate_hardcoded_decoders`.
        for provider in providers.iter_mut() {
            let Some(schema) = schema_by_provider_id.get(&provider.id).cloned() else {
                continue;
            };
            let kind = provider_kind
                .get(&provider.id)
                .map(|(k, _)| *k)
                .unwrap_or(ProviderKind::Other);
            apply_resolved_schema(provider, kind, &schema, &mut divergent);
        }
    }

    // A capture is "split-schema" when symbolisation / process metadata
    // lives in dedicated "symbols" / "process" providers (newer kernels)
    // rather than inside the "cpu" provider. The flag selects how the
    // cpu-provider event-ids are interpreted (samples move from id 3 to
    // id 1) and enables routing of the two extra providers.
    //
    // Provider presence alone isn't a reliable signal, though: some
    // kernels emit split-schema cpu-provider ids (INFO=0, SAMPLE=1)
    // unconditionally, even in a capture that only enabled the bare
    // "cpu" provider (e.g. a logging-only capture, which leaves
    // "symbols"/"process" off). Misdetecting this makes every real SAMPLE
    // (id 1) get decoded as a legacy THREAD_CREATE (also id 1 in that
    // scheme) instead, so no stack samples ever reach `cpu_profile` even
    // though the provider clearly has millions of cpu-provider events.
    // Disambiguate with a cheap header-only pre-pass over the cpu
    // provider's own events: stack samples are ~99% of cpu-provider
    // traffic under either scheme, so whichever of id 1 / id 3 is far
    // more common tells us which one is "sample" for this file,
    // independent of which other providers happen to be present.
    let events_start = off;
    let mut cpu_id1_count: u64 = 0;
    let mut cpu_id3_count: u64 = 0;
    let mut tcp_event_count: usize = 0;
    let mut tcp_sessions: FxHashSet<u64> = FxHashSet::default();
    {
        let mut scan_off = events_start;
        while scan_off + EVENT_HDR_SIZE <= data.len() {
            let ev_len = data.u16(scan_off) as usize;
            if ev_len < EVENT_HDR_SIZE || scan_off + ev_len > data.len() {
                break;
            }
            match provider_kind.get(&data.u16(scan_off + 4)) {
                Some((ProviderKind::Cpu, _)) => match data.u32(scan_off + 24) {
                    1 => cpu_id1_count += 1,
                    3 => cpu_id3_count += 1,
                    _ => {}
                },
                Some((ProviderKind::Tcp, _)) => {
                    tcp_event_count += 1;
                    tcp_sessions.insert(data.u64(scan_off + 16));
                }
                _ => {}
            }
            scan_off += ev_len;
        }
    }
    let tcp_session_count = tcp_sessions.len();
    drop(tcp_sessions);
    let split_schema = provider_kind
        .values()
        .any(|(k, _)| matches!(k, ProviderKind::Symbols | ProviderKind::Process))
        || cpu_id1_count > cpu_id3_count;

    let events_span = data.len().saturating_sub(events_start).max(1);

    // Average TCP events per connection, from the header pre-pass. Each
    // session Vec is reserved to that size. Wasm linear memory commits
    // the reserve, so a high-connection capture has to be sized from
    // its own fan-out.
    let estimated_sessions = tcp_session_count.clamp(64, 100_000);
    let initial_vec_cap = session_vec_cap(tcp_event_count, tcp_session_count);
    let mut sessions: Sessions =
        FxHashMap::with_capacity_and_hasher(estimated_sessions, Default::default());
    let mut stars_agg_samples: Vec<StarsAggSample> = Vec::new();
    let mut stars_pace_samples: Vec<StarsPaceSample> = Vec::new();
    // Identity maps below (kernel, threads, linker_files, user_files) key
    // on values the kernel can legitimately reuse (tid/file_id) and are
    // now also re-emitted by design: elog's --roll-time integration
    // re-triggers a dump-state replay at every rolled-file boundary
    // (EVENTLOG_IOCTL_TRIGGER_DUMP_STATE), so a repeat sighting of the
    // same key within one capture is expected, not an error. Every site
    // below upserts (last write wins) rather than keeping only the first
    // sighting, so a genuine reuse (not just a byte-identical re-dump)
    // is reflected correctly.
    let mut cpu_profile = CpuProfile::default();
    // CPU_SAMPLE dominates large captures (99%+ of events on a cpu-heavy
    // one), so growing this Vec via repeated doubling means memcpy-ing
    // tens of millions of elements over the life of the parse. The
    // header pre-pass counted cpu-provider ids 1 and 3; the larger is
    // the sample stream under either schema, and reserving it is one
    // allocation where it matters. Wasm commits the full reserve, so
    // the bound is that sample count.
    cpu_profile
        .samples
        .reserve(cpu_id1_count.max(cpu_id3_count) as usize);
    let mut tcp_samples = TcpSampleProfile::default();
    let mut if_profile = IfProfile::default();
    let mut ella_profile = EllaProfile::default();
    let mut nginx_profile = NginxProfile::default();
    // IF_SAMPLE_BYTES halves awaiting their IF_SAMPLE_PACKETS other half,
    // keyed by ifindex -- see the merge comment at the EVT_IF_SAMPLE_BYTES
    // arm below.
    let mut if_sample_pending: FxHashMap<u16, IfSample> = FxHashMap::default();
    // v2 only: hpts timing-wheel events and every event decoded purely
    // from a resolved schema (no hardcoded decoder, or a divergent one —
    // see the routing guard below). Always empty for v1 captures.
    let mut schema_capture = SchemaCapture::default();
    let mut total_parsed: usize = 0;
    let mut total_skipped: usize = 0;
    let mut last_progress: u32 = 50;
    // Per-provider event totals indexed by the provider's position in
    // `providers`. Built as a parallel Vec to avoid an inner HashMap
    // lookup on every event.
    let mut provider_counts: Vec<u64> = vec![0; providers.len()];
    let mut provider_first: Vec<Option<u64>> = vec![None; providers.len()];
    let mut provider_last: Vec<Option<u64>> = vec![None; providers.len()];
    let mut provider_bytes: Vec<u64> = vec![0; providers.len()];
    let mut provider_sessions: Vec<FxHashSet<u64>> =
        (0..providers.len()).map(|_| FxHashSet::default()).collect();
    // Parallel to `providers`. The last-seen id hits first; a miss scans
    // the vec. A provider's id set stays tiny, so the event loop never
    // hashes.
    let mut event_acc: Vec<ProviderEventAcc> = (0..providers.len())
        .map(|_| ProviderEventAcc::default())
        .collect();
    let mut unknown_events: FxHashMap<u16, ProviderEventAcc> = FxHashMap::default();
    let mut elog_events = ProviderEventAcc::default();
    // Max timestamp across every event header in the file (not just
    // decoded ones) — the aggregate info popup's "Capture end".
    let mut last_event_ts: u64 = 0;
    // Leading event timestamps, in file order, for `effective_capture_start`.
    let mut ts_probe: Vec<u64> = Vec::with_capacity(BACKLOG_PROBE_EVENTS);
    let mut total_events: usize = 0;
    let mut machine_info: Option<MachineInfo> = None;

    while off + EVENT_HDR_SIZE <= data.len() {
        let event_length = data.u16(off) as usize;
        if event_length < EVENT_HDR_SIZE || off + event_length > data.len() {
            break;
        }
        data.release_before(off);
        let raw = data.event(off, event_length);
        let event_id = read_u32(raw, 24);

        // In-band provider-table / decode-blob announcements: elog(1)
        // writes these for a provider that registered after the file's
        // header (and its provider table / decode-blob section) was
        // already committed, right before that provider's first event —
        // see elog.5's "Provider Announcement Records". Apply them to the
        // same maps the header-time provider table / blob table populate
        // above, then move past without counting either as a parsed or
        // skipped event.
        if event_id == EVT_PROVIDER_INFO || event_id == EVT_PROVIDER_BLOB {
            elog_events.bump(event_id, event_length as u32, read_u64(raw, 8));
            let payload_off = EVENT_HDR_SIZE;
            let payload_size = event_length - EVENT_HDR_SIZE;
            if payload_off + payload_size <= raw.len() {
                let payload = &raw[payload_off..payload_off + payload_size];
                if event_id == EVT_PROVIDER_INFO && payload_size == PROVIDER_SIZE_V2 {
                    let pid = read_u16(payload, 0);
                    let name = read_cstr(payload, 2, 32);
                    let mut sid = [0u8; SCHEMA_ID_LEN];
                    sid.copy_from_slice(&payload[34..34 + SCHEMA_ID_LEN]);
                    let kind = provider_kind_for_name(&name);
                    let idx = match provider_kind.get(&pid) {
                        Some((_, idx)) => *idx,
                        None => {
                            let idx = providers.len();
                            providers.push(Provider {
                                id: pid,
                                name,
                                event_count: 0,
                                first_ts: None,
                                last_ts: None,
                                session_count: 0,
                                byte_size: 0,
                                schema_name: None,
                                schema_event_count: 0,
                                schema_divergent_events: Vec::new(),
                                schema_new_fields: Vec::new(),
                                events: Vec::new(),
                            });
                            provider_counts.push(0);
                            provider_first.push(None);
                            provider_last.push(None);
                            provider_bytes.push(0);
                            provider_sessions.push(FxHashSet::default());
                            event_acc.push(ProviderEventAcc::default());
                            idx
                        }
                    };
                    provider_kind.insert(pid, (kind, idx));
                    if is_v2 && sid != [0u8; SCHEMA_ID_LEN] {
                        provider_schema_id.insert(pid, sid);
                        if let Some(schema) = blob_by_schema_id.get(&sid).cloned() {
                            schema_by_provider_id.insert(pid, schema.clone());
                            apply_resolved_schema(
                                &mut providers[idx],
                                kind,
                                &schema,
                                &mut divergent,
                            );
                        }
                    }
                } else if event_id == EVT_PROVIDER_BLOB && payload_size >= SCHEMA_ID_LEN + 4 {
                    let mut sid = [0u8; SCHEMA_ID_LEN];
                    sid.copy_from_slice(&payload[..SCHEMA_ID_LEN]);
                    let blob_size = read_u32(payload, SCHEMA_ID_LEN) as usize;
                    if blob_size <= EVENTLOG_DECODE_BLOB_MAX
                        && payload_size == SCHEMA_ID_LEN + 4 + blob_size
                    {
                        let blob = &payload[SCHEMA_ID_LEN + 4..];
                        // Verify before trusting, same as the header-time
                        // blob table above.
                        if compute_schema_id(blob) == sid {
                            if let Some(schema) = ProviderSchema::parse(blob) {
                                blob_by_schema_id.insert(sid, schema.clone());
                                for (&pid, &psid) in provider_schema_id.iter() {
                                    if psid != sid {
                                        continue;
                                    }
                                    schema_by_provider_id.insert(pid, schema.clone());
                                    if let Some((k, idx)) = provider_kind.get(&pid).copied() {
                                        if let Some(provider) = providers.get_mut(idx) {
                                            apply_resolved_schema(
                                                provider,
                                                k,
                                                &schema,
                                                &mut divergent,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            off += event_length;
            continue;
        }

        let provider_id = read_u16(raw, 4);
        let timestamp = read_u64(raw, 8);
        let session_id = read_u64(raw, 16);
        let (kind, provider_idx) = provider_kind
            .get(&provider_id)
            .copied()
            .unwrap_or((ProviderKind::Other, usize::MAX));
        let is_tcp = kind == ProviderKind::Tcp;
        let is_hpts = kind == ProviderKind::Hpts;
        let is_cpu = kind == ProviderKind::Cpu;
        let is_if = kind == ProviderKind::If;
        let is_symbols = kind == ProviderKind::Symbols;
        let is_process = kind == ProviderKind::Process;
        let is_ella_relay = kind == ProviderKind::EllaRelay;
        let is_nginx = kind == ProviderKind::Nginx;
        let is_machine = kind == ProviderKind::Machine;
        let is_mem = kind == ProviderKind::Mem;
        if let Some(count) = provider_counts.get_mut(provider_idx) {
            *count += 1;
        }
        if let Some(first) = provider_first.get_mut(provider_idx) {
            *first = Some(first.map_or(timestamp, |t| t.min(timestamp)));
        }
        if let Some(last) = provider_last.get_mut(provider_idx) {
            *last = Some(last.map_or(timestamp, |t| t.max(timestamp)));
        }
        if let Some(bytes) = provider_bytes.get_mut(provider_idx) {
            *bytes += event_length as u64;
        }
        let record_len = event_length as u32;
        if provider_idx == usize::MAX {
            unknown_events
                .entry(provider_id)
                .or_default()
                .bump(event_id, record_len, timestamp);
        } else if let Some(acc) = event_acc.get_mut(provider_idx) {
            acc.bump(event_id, record_len, timestamp);
        }
        if let Some(sessions_seen) = provider_sessions.get_mut(provider_idx) {
            sessions_seen.insert(session_id);
        }
        if timestamp > last_event_ts {
            last_event_ts = timestamp;
        }
        total_events += 1;
        if ts_probe.len() < BACKLOG_PROBE_EVENTS {
            ts_probe.push(timestamp);
        }
        if !is_tcp
            && !is_hpts
            && !is_cpu
            && !is_if
            && !is_symbols
            && !is_process
            && !is_ella_relay
            && !is_nginx
            && !is_machine
            && !is_mem
        {
            off += event_length;
            total_skipped += 1;
            if let Some(prog) = progress {
                let p = 50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
                if p >= last_progress + 5 {
                    prog.store_permille(p.min(800));
                    last_progress = p;
                }
            }
            continue;
        }

        let cpu = read_u16(raw, 2);
        let thread_id = read_i32(raw, 28);

        let payload_off = EVENT_HDR_SIZE;
        let payload_size = event_length - EVENT_HDR_SIZE;
        let schema = schema_by_provider_id.get(&provider_id);

        if is_machine {
            if payload_off + payload_size <= raw.len() {
                let payload = &raw[payload_off..payload_off + payload_size];
                match event_id {
                    EVT_MACHINE_INFO if machine_info.is_none() => {
                        machine_info = parse_machine_info(payload);
                    }
                    EVT_MACHINE_METADATA_KV => {
                        if let Some((k, v)) = parse_machine_kv(payload) {
                            let mi = machine_info.get_or_insert_with(MachineInfo::default);
                            mi.metadata_kv.push((k, v));
                        }
                    }
                    _ => {}
                }
            }
            total_parsed += 1;
            off += event_length;
            if let Some(prog) = progress {
                let p = 50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
                if p >= last_progress + 5 {
                    prog.store_permille(p.min(800));
                    last_progress = p;
                }
            }
            continue;
        }

        // Read by schema field name before the generic-schema routing
        // below, so mem never lands in generic_events.
        if is_mem {
            parse_mem_event(
                &mut schema_capture.mem,
                timestamp,
                event_id,
                raw,
                payload_off,
                payload_size,
                schema,
            );
            total_parsed += 1;
            off += event_length;
            if let Some(prog) = progress {
                let p = 50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
                if p >= last_progress + 5 {
                    prog.store_permille(p.min(800));
                    last_progress = p;
                }
            }
            continue;
        }

        // Schema-verified decoding routing (v2 only — `schema_by_provider_id`
        // is empty for v1 captures, so this is a no-op there). Route to the
        // generic schema decoder instead of the normal per-provider dispatch
        // below when either:
        //   - the hardcoded decoder for (provider_id, event_id) was found to
        //     diverge from the resolved schema at load time (unsafe drift:
        //     missing/renamed/resized fields), or
        //   - no hardcoded decoder exists for event_id at all, but the
        //     resolved schema defines it (a schema-only event).
        // Packing-only offset drift is *not* divergence: typed ingest
        // reads by schema field name. If the schema doesn't define this
        // event id either, fall through unchanged into the dispatch below.
        if let Some(schema) = schema {
            let is_divergent = divergent
                .get(&provider_id)
                .is_some_and(|set| set.contains(&event_id));
            let has_hardcoded = match kind {
                ProviderKind::Tcp => tcp_has_hardcoded_decoder(event_id),
                ProviderKind::If => if_has_hardcoded_decoder(event_id),
                ProviderKind::Cpu => cpu_has_hardcoded_decoder(event_id),
                ProviderKind::Hpts => hpts_has_hardcoded_decoder(event_id),
                ProviderKind::EllaRelay => ella_relay_has_hardcoded_decoder(event_id),
                ProviderKind::Nginx => nginx_has_hardcoded_decoder(event_id),
                ProviderKind::Symbols
                | ProviderKind::Process
                | ProviderKind::Machine
                | ProviderKind::Mem
                | ProviderKind::Other => true,
            };
            // `IN`/`OUT` carry the per-session plot data, so a divergent
            // decoder for them still rebuilds a typed payload by schema
            // name (missing fields read as 0) rather than dropping them
            // into generic_events.
            if is_divergent
                && is_tcp
                && matches!(event_id, EVT_IN | EVT_OUT)
                && payload_off + payload_size <= raw.len()
            {
                let payload = parse_payload(event_id, raw, payload_off, payload_size, Some(schema));
                sessions
                    .entry(session_id)
                    .or_insert_with(|| Vec::with_capacity(initial_vec_cap))
                    .push(Event {
                        ts: timestamp,
                        cpu,
                        tid: thread_id,
                        event_id,
                        payload,
                    });
                total_parsed += 1;
                off += event_length;
                if let Some(prog) = progress {
                    let p = 50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
                    if p >= last_progress + 5 {
                        prog.store_permille(p.min(800));
                        last_progress = p;
                    }
                }
                continue;
            }
            if (is_divergent || !has_hardcoded) && payload_off + payload_size <= raw.len() {
                if let Some(ev) = schema.find_event(event_id) {
                    let formatted = schema
                        .format_payload(event_id, &raw[payload_off..payload_off + payload_size])
                        .unwrap_or_default();
                    schema_capture.generic_events.push(GenericEvent {
                        ts: timestamp,
                        cpu,
                        tid: thread_id,
                        session_id,
                        provider_id,
                        event_id,
                        event_name: ev.name.clone(),
                        formatted,
                    });
                    total_parsed += 1;
                    off += event_length;
                    if let Some(prog) = progress {
                        let p =
                            50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
                        if p >= last_progress + 5 {
                            prog.store_permille(p.min(800));
                            last_progress = p;
                        }
                    }
                    continue;
                }
            }
        }
        // Split-schema info events whose wire layout differs from the
        // legacy cpu-provider KERNEL_INFO. `ncpus`/`hz` (cpu provider, per
        // CPU) and `text_base`/`build_id`/`ident` (symbols provider) each
        // fill their own half of one merged `KernelInfo`.
        if split_schema && is_cpu && event_id == EVT_CPUX_INFO {
            let r = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            if r.typed() || payload_size >= 8 {
                let k = cpu_profile.kernel.get_or_insert_with(KernelInfo::default);
                if k.ncpus == 0 {
                    k.ncpus = r.u32("ncpus", 0);
                    k.hz = r.u32("hz", 4);
                    k.pmchz = r.u32("pmchz", 8);
                    k.freq_max_mhz = r.u16("freq_max_mhz", 12);
                }
                total_parsed += 1;
            } else {
                total_skipped += 1;
            }
            off += event_length;
            continue;
        }
        if is_symbols && event_id == EVT_SYM_KERNEL_INFO {
            // Layout: text_base u64, text_size u64, build_id char[41],
            // ident char[]. (No ncpus/hz — those arrive on the cpu
            // provider's per-CPU info event.)
            let r = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            let kernel_text_unset = match &cpu_profile.kernel {
                Some(k) => k.text_base == 0,
                None => true,
            };
            if (r.typed() || payload_size >= 16) && kernel_text_unset {
                let k = cpu_profile.kernel.get_or_insert_with(KernelInfo::default);
                k.text_base = r.u64("text_base", 0);
                k.text_size = r.u64("text_size", 8);
                k.build_id = r.cstr("build_id", 16, 41);
                k.ident = r.cstr("ident", 57, payload_size.saturating_sub(57));
            }
            total_parsed += 1;
            off += event_length;
            continue;
        }

        // PROC_EXEC (process provider id 2): image name after execve.
        // THREAD_CREATE carries the fork-time comm (still the parent for
        // a shell script); flame group bars resolve the name at sample
        // time through CpuProfile::process_comm_at.
        if is_process && event_id == EVT_PROC_EXEC {
            let r = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            // pid@0, ppid@4, start_sec@8, comm@16 — 48 bytes packed.
            if r.typed() || payload_size >= 48 {
                cpu_profile.apply_proc_exec(timestamp, r.u32("pid", 0), r.cstr("comm", 16, 32));
                total_parsed += 1;
            } else {
                total_skipped += 1;
            }
            off += event_length;
            continue;
        }
        // PROC_EXIT (process provider id 3): close the open PROC_EXEC
        // so samples from before the exit keep that image name.
        if is_process && event_id == EVT_PROC_EXIT {
            let r = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            if r.typed() || payload_size >= 4 {
                cpu_profile.apply_proc_exit(timestamp, r.u32("pid", 0));
                total_parsed += 1;
            } else {
                total_skipped += 1;
            }
            off += event_length;
            continue;
        }

        // PROC_FORK (process provider id 5): fork-without-exec edge.
        // Copy the parent's current PROC_MAP set onto the child pid
        // here, at the edge's position in the stream, so mappings the
        // parent adds later (post-fork mmap) don't leak into the
        // child. Captures that predate the event fall back to
        // `inherit_fork_proc_maps` at finalize.
        if is_process && event_id == EVT_PROC_FORK {
            let r = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            if r.typed() || payload_size >= 8 {
                let pid = r.u32("pid", 0);
                let ppid = r.u32("ppid", 4);
                cpu_profile.apply_proc_fork(pid, ppid);
                cpu_profile.inherit_proc_title(timestamp, pid, ppid);
                total_parsed += 1;
            } else {
                total_skipped += 1;
            }
            off += event_length;
            continue;
        }
        // PROC_TITLE (process provider id 6): setproctitle / $0, and the
        // capture-start dump of p_args. pid@0, title@4 — 36 bytes packed.
        if is_process && event_id == EVT_PROC_TITLE {
            let r = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            if r.typed() || payload_size >= 36 {
                cpu_profile.apply_proc_title(timestamp, r.u32("pid", 0), r.cstr("title", 4, 32));
                total_parsed += 1;
            } else {
                total_skipped += 1;
            }
            off += event_length;
            continue;
        }

        // SOURCE_FILE / SOURCE_LINE live only in the symbols provider
        // (ids 5 and 6). Those numbers are LINKER_FILE / SYMBOL_RANGE
        // in the legacy cpu provider, so they are decoded here and not
        // translated onto the cpu id space.
        if is_symbols && event_id == EVT_SYM_SOURCE_FILE {
            let r = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            if r.typed() || payload_size >= 50 {
                let source_file_id = r.u32("source_file_id", 0);
                cpu_profile.source_files.insert(
                    source_file_id,
                    EmbeddedSourceFile {
                        source_file_id,
                        binary_kind: r.u8("binary_kind", 4),
                        binary_id: r.u32("binary_id", 5),
                        commit_id: r.cstr("commit_id", 9, 41),
                        pathname: r.cstr("pathname", 50, 256),
                    },
                );
                total_parsed += 1;
            } else {
                total_skipped += 1;
            }
            off += event_length;
            continue;
        }
        if is_symbols && event_id == EVT_SYM_SOURCE_LINE {
            let r = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            if r.typed() || payload_size >= 21 {
                let key = (r.u8("binary_kind", 4), r.u32("binary_id", 5));
                cpu_profile
                    .source_lines
                    .entry(key)
                    .or_default()
                    .push(EmbeddedSourceLine {
                        source_file_id: r.u32("source_file_id", 0),
                        file_offset: r.u64("file_offset", 9),
                        line: r.u32("line", 17),
                    });
                total_parsed += 1;
            } else {
                total_skipped += 1;
            }
            off += event_length;
            continue;
        }

        // Translate split-schema cpu/symbols/process event-ids onto the
        // legacy cpu-provider decoders below (the payloads are
        // byte-compatible). `None` means "not a cpu-decodable event".
        let cpu_eid: Option<u32> = if is_cpu {
            if split_schema {
                match event_id {
                    EVT_CPUX_SAMPLE => Some(EVT_CPU_SAMPLE),
                    _ => None,
                }
            } else {
                Some(event_id)
            }
        } else if is_symbols {
            match event_id {
                EVT_SYM_LINKER_FILE => Some(EVT_CPU_LINKER_FILE),
                EVT_SYM_SYMBOL_RANGE => Some(EVT_CPU_SYMBOL_RANGE),
                EVT_SYM_USER_FILE => Some(EVT_CPU_USER_FILE),
                EVT_SYM_USER_SYMBOL_RANGE => Some(EVT_CPU_USER_SYMBOL_RANGE),
                EVT_SYM_SYMBOL_PACK_REF => Some(EVT_CPU_SYMBOL_PACK_REF),
                _ => None,
            }
        } else if is_process {
            match event_id {
                EVT_PROC_THREAD_CREATE => Some(EVT_CPU_THREAD_CREATE),
                EVT_PROC_THREAD_EXIT => Some(EVT_CPU_THREAD_EXIT),
                EVT_PROC_MAP => Some(EVT_CPU_PROC_MAP),
                _ => None,
            }
        } else {
            None
        };

        if let Some(eff_eid) = cpu_eid {
            let cr = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            // The cpu provider's event-id namespace is independent of
            // tcp's; every layout below mirrors the STRUCT definitions
            // in `cpu_eventlog_schema.src`.
            match eff_eid {
                EVT_CPU_KERNEL_INFO if cr.typed() || payload_size >= 88 => {
                    let has_build_id = cr.has("build_id") || payload_size >= 24 + 41 + 64;
                    cpu_profile.kernel = Some(KernelInfo {
                        text_base: cr.u64("text_base", 0),
                        text_size: cr.u64("text_size", 8),
                        ncpus: cr.u32("ncpus", 16),
                        hz: cr.u32("hz", 20),
                        pmchz: 0,
                        freq_max_mhz: 0,
                        build_id: if has_build_id {
                            cr.cstr("build_id", 24, 41)
                        } else {
                            String::new()
                        },
                        ident: if has_build_id {
                            cr.cstr("ident", 65, 64)
                        } else {
                            cr.cstr("ident", 24, 64)
                        },
                    });
                    total_parsed += 1;
                }
                EVT_CPU_THREAD_CREATE if cr.typed() || payload_size >= 72 => {
                    let tid = cr.u32("tid", 0);
                    cpu_profile.threads.insert(
                        tid,
                        ThreadInfo {
                            tid,
                            pid: cr.u32("pid", 4),
                            comm: cr.cstr("comm", 8, 32),
                            tdname: cr.cstr("tdname", 40, 32),
                        },
                    );
                    total_parsed += 1;
                }
                EVT_CPU_THREAD_EXIT if cr.typed() || payload_size >= 4 => {
                    total_parsed += 1;
                }
                EVT_CPU_SAMPLE if cr.typed() || payload_size >= 16 => {
                    let flags = cr.u8("flags", 9);
                    let frames = cr.hist_u64s(CPU_SAMPLE_FRAMES_MAX, 10, 16);
                    let freq_mhz = if flags & CPU_SAMPLE_FLAG_FREQ_VALID != 0 {
                        Some(cr.u16("freq_mhz", 12))
                    } else {
                        None
                    };
                    cpu_profile.samples.push(CpuStackSample {
                        ts: timestamp,
                        cpu,
                        tid: cr.u32("tid", 0),
                        pid: cr.u32("pid", 4),
                        sample_type: cr.u8("type", 8),
                        flags,
                        freq_mhz,
                        frames,
                    });
                    total_parsed += 1;
                }
                EVT_CPU_OVERFLOW if cr.typed() || payload_size >= 4 => {
                    cpu_profile.overflow_dropped = cpu_profile
                        .overflow_dropped
                        .saturating_add(cr.u32("dropped", 0) as u64);
                    total_parsed += 1;
                }
                EVT_CPU_LINKER_FILE if cr.typed() || payload_size >= 257 => {
                    // Untyped layout fallback: the wire struct is
                    // __packed (257 B). Naturally-aligned 261 B events
                    // only exist in pre-packing captures.
                    let packed = cr.typed() || payload_size < 261;
                    let (base_o, size_o, flags_o, bid_o, fname_o, path_o) = if packed {
                        (4, 12, 20, 24, 65, 129)
                    } else {
                        (8, 16, 24, 28, 69, 133)
                    };
                    let file_id = cr.u32("file_id", 0);
                    cpu_profile.linker_files.insert(
                        file_id,
                        LinkerFile {
                            file_id,
                            base: cr.u64("base", base_o),
                            size: cr.u64("size", size_o),
                            flags: cr.u32("flags", flags_o),
                            build_id: cr.cstr("build_id", bid_o, 41),
                            filename: cr.cstr("filename", fname_o, 64),
                            pathname: cr.cstr("pathname", path_o, 128),
                        },
                    );
                    cpu_profile.symbols.remove(&file_id);
                    total_parsed += 1;
                }
                EVT_CPU_SYMBOL_RANGE if cr.typed() || payload_size >= 112 => {
                    // Untyped layout fallback: the wire struct is
                    // __packed (112 B: file_id@0, offset@4, size@12,
                    // name@16). Naturally-aligned 116 B events only
                    // exist in pre-packing captures and early PoC packs.
                    let packed = cr.typed() || payload_size < 116;
                    let file_id = cr.u32("file_id", 0);
                    cpu_profile
                        .symbols
                        .entry(file_id)
                        .or_default()
                        .push(SymbolRange {
                            offset: cr.u64("offset", if packed { 4 } else { 8 }),
                            size: cr.u32("size", if packed { 12 } else { 16 }),
                            name: cr.cstr("name", if packed { 16 } else { 20 }, 96),
                        });
                    total_parsed += 1;
                }
                EVT_CPU_USER_FILE if cr.typed() || payload_size >= 173 => {
                    let user_file_id = cr.u32("user_file_id", 0);
                    cpu_profile.user_files.insert(
                        user_file_id,
                        UserFile {
                            user_file_id,
                            build_id: cr.cstr("build_id", 4, 41),
                            pathname: cr.cstr("pathname", 45, 128),
                        },
                    );
                    cpu_profile.user_symbols.remove(&user_file_id);
                    total_parsed += 1;
                }
                EVT_CPU_PROC_MAP if cr.typed() || payload_size >= 32 => {
                    let pid = cr.u32("pid", 0);
                    let map = ProcMap {
                        pid,
                        user_file_id: cr.u32("user_file_id", 4),
                        base: cr.u64("base", 8),
                        size: cr.u64("size", 16),
                        offset: cr.u64("offset", 24),
                    };
                    let entries = cpu_profile.proc_maps.entry(pid).or_default();
                    if !entries.contains(&map) {
                        entries.push(map);
                    }
                    total_parsed += 1;
                }
                EVT_CPU_USER_SYMBOL_RANGE if cr.typed() || payload_size >= 112 => {
                    // Same packed/legacy split as EVT_CPU_SYMBOL_RANGE.
                    let packed = cr.typed() || payload_size < 116;
                    let user_file_id = cr.u32("user_file_id", 0);
                    cpu_profile
                        .user_symbols
                        .entry(user_file_id)
                        .or_default()
                        .push(UserSymbolRange {
                            user_file_id,
                            offset: cr.u64("offset", if packed { 4 } else { 8 }),
                            size: cr.u32("size", if packed { 12 } else { 16 }),
                            name: cr.cstr("name", if packed { 16 } else { 20 }, 96),
                        });
                    total_parsed += 1;
                }
                EVT_CPU_SYMBOL_PACK_REF if cr.typed() || payload_size >= 71 => {
                    cpu_profile.pack_refs.push(SymbolPackRef {
                        kind: cr.u8("kind", 0),
                        id_kind: cr.u8("id_kind", 1),
                        file_id: cr.u32("file_id", 2),
                        pack_id: cr.cstr("pack_id", 6, 65),
                    });
                    total_parsed += 1;
                }
                _ => {
                    total_skipped += 1;
                }
            }
            // Suppress unused-warning of `thread_id` / `cpu` for cpu
            // events: the per-event-header tid/cpu fields are wired to
            // generic event metadata that the cpu provider doesn't use
            // (its own tid/cpu live in the CPU_SAMPLE payload).
            let _ = (thread_id, cpu);
            off += event_length;
            if let Some(prog) = progress {
                let p = 50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
                if p >= last_progress + 5 {
                    prog.store_permille(p.min(800));
                    last_progress = p;
                }
            }
            continue;
        }

        // Cpu-family events that didn't map to a decoder above (unknown
        // cpu ids, or split-schema proc exec/exit) — count and advance.
        if is_cpu || is_symbols || is_process {
            total_skipped += 1;
            off += event_length;
            continue;
        }

        if is_hpts {
            // Two unrelated schemas share the `hpts` provider's id
            // namespace (see constants.rs's comment): STARS_AGG_SAMPLE
            // (id 16) and the timing-wheel scheduler trace (ids 0-14,
            // decoded by `hpts_wheel::parse_hpts_wheel_payload`).
            // Everything else is skipped without polluting the
            // per-session TCP map.
            //
            // STARS_AGG_SAMPLE is fixed-width, no variable-length part:
            // 21 × u64 = 168 bytes, in the exact field order of `STRUCT
            // STARS_AGG_SAMPLE` (hpts_eventlog_schema.src). `tokens` is
            // `int64_t` on the kernel side but wired as the same u64 bit
            // pattern. The trailing rollover_* fields (offsets 128-151),
            // slot_gate_headroom_pct (offset 152), and rxt_no_pace_hits
            // (offset 160) were each appended later, so captures from
            // before land with payload_size == 128, 152 or 160
            // respectively; treat missing fields as their "no-op" default
            // (0 for counters, 100 -- no reduction -- for headroom)
            // rather than rejecting the whole event.
            let hr = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            if event_id == EVT_HPTS_STARS_AGG_SAMPLE && (hr.typed() || payload_size >= 128) {
                let headroom = if hr.has("slot_gate_headroom_pct") || payload_size >= 160 {
                    hr.u64("slot_gate_headroom_pct", 152)
                } else {
                    100
                };
                stars_agg_samples.push(StarsAggSample {
                    ts: timestamp,
                    pressure: hr.u64("pressure", 0),
                    tokens: hr.i64("tokens", 8),
                    per_slot_bytes: hr.u64("per_slot_bytes", 16),
                    slot_gate_budget_bytes: hr.u64("slot_gate_budget_bytes", 24),
                    slot_bytes_peak: hr.u64("slot_bytes_peak", 32),
                    slot_gate_denies: hr.u64("slot_gate_denies", 40),
                    slot_gate_admits: hr.u64("slot_gate_admits", 48),
                    slot_gate_denied_bytes: hr.u64("slot_gate_denied_bytes", 56),
                    slot_gate_admitted_bytes: hr.u64("slot_gate_admitted_bytes", 64),
                    meter_obytes: hr.u64("meter_obytes", 72),
                    pressure_sum: hr.u64("pressure_sum", 80),
                    pressure_ticks: hr.u64("pressure_ticks", 88),
                    synthetic_norate_hits: hr.u64("synthetic_norate_hits", 96),
                    slot_gate_enable: hr.u64("slot_gate_enable", 104),
                    health_limit_bps: hr.u64("health_limit_bps", 112),
                    min_rate_bps: hr.u64("min_rate_bps", 120),
                    rollover_queue_depth: hr.u64("rollover_queue_depth", 128),
                    rollover_diverts: hr.u64("rollover_diverts", 136),
                    rollover_admits: hr.u64("rollover_admits", 144),
                    slot_gate_headroom_pct: headroom,
                    rxt_no_pace_hits: hr.u64("rxt_no_pace_hits", 160),
                });
                total_parsed += 1;
            } else if let Some(payload) =
                parse_hpts_wheel_payload(event_id, raw, payload_off, payload_size, schema)
            {
                schema_capture.hpts_wheel_events.push(HptsWheelEvent {
                    ts: timestamp,
                    cpu,
                    session_id,
                    payload,
                });
                total_parsed += 1;
            } else {
                total_skipped += 1;
            }
            off += event_length;
            if let Some(prog) = progress {
                let p = 50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
                if p >= last_progress + 5 {
                    prog.store_permille(p.min(800));
                    last_progress = p;
                }
            }
            continue;
        }

        if is_if {
            // "if" provider: interface lifecycle / link / config / addr
            // changes plus periodic IF_SAMPLE counter snapshots. All
            // payloads carry ifindex (the primary key) as the first u16.
            // STRUCT field offsets follow C natural alignment; trailing
            // variable-length arrays start at the (alignment-padded)
            // sizeof of the fixed head (see if_eventlog_schema.src).
            let ir = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            match event_id {
                EVT_IF_INFO if ir.typed() || payload_size >= 18 => {
                    if_profile.period_us = ir.u32("period_us", 0);
                    if_profile.nifs = ir.u16("nifs", 16);
                    total_parsed += 1;
                }
                EVT_IF_ATTACH if ir.typed() || payload_size >= 49 => {
                    let ifindex = ir.u16("ifindex", 0);
                    let name = ir.cstr("name", 2, 16);
                    let if_type = ir.u8("if_type", 18);
                    let driver = ir.cstr("driver", 19, 16);
                    let mtu = ir.u32("mtu", 36);
                    let baudrate = ir.u64("baudrate", 40);
                    let hwaddr_len = ir.u8("hwaddr_len", 48) as usize;
                    let hwaddr = ir.varlen_bytes("hwaddr", 49, hwaddr_len.min(32));
                    let entry = if_profile.interfaces.entry(ifindex).or_default();
                    entry.ifindex = ifindex;
                    entry.name = name.clone();
                    entry.if_type = if_type;
                    entry.driver = driver;
                    entry.mtu = mtu;
                    entry.baudrate = baudrate;
                    entry.hwaddr = hwaddr;
                    entry.detached = false;
                    if_profile.events.push(IfEvent {
                        ts: timestamp,
                        ifindex,
                        event_id,
                        detail: format!(
                            "attach {name} type={if_type} mtu={mtu} baudrate={baudrate}"
                        ),
                    });
                    total_parsed += 1;
                }
                EVT_IF_DETACH if ir.typed() || payload_size >= 18 => {
                    let ifindex = ir.u16("ifindex", 0);
                    let name = ir.cstr("name", 2, 16);
                    if let Some(e) = if_profile.interfaces.get_mut(&ifindex) {
                        e.detached = true;
                    }
                    if_profile.events.push(IfEvent {
                        ts: timestamp,
                        ifindex,
                        event_id,
                        detail: format!("detach {name}"),
                    });
                    total_parsed += 1;
                }
                EVT_IF_RENAME if ir.typed() || payload_size >= 34 => {
                    let ifindex = ir.u16("ifindex", 0);
                    let old_name = ir.cstr("old_name", 2, 16);
                    let new_name = ir.cstr("new_name", 18, 16);
                    if let Some(e) = if_profile.interfaces.get_mut(&ifindex) {
                        e.name = new_name.clone();
                    }
                    if_profile.events.push(IfEvent {
                        ts: timestamp,
                        ifindex,
                        event_id,
                        detail: format!("rename {old_name} -> {new_name}"),
                    });
                    total_parsed += 1;
                }
                EVT_IF_LINK_STATE if ir.typed() || payload_size >= 16 => {
                    let ifindex = ir.u16("ifindex", 0);
                    let state = ir.u8("state", 2);
                    let baudrate = ir.u64("baudrate", 8);
                    if let Some(e) = if_profile.interfaces.get_mut(&ifindex) {
                        e.link_state = state;
                        if baudrate != 0 {
                            e.baudrate = baudrate;
                        }
                    }
                    if_profile.events.push(IfEvent {
                        ts: timestamp,
                        ifindex,
                        event_id,
                        detail: format!("link {} baudrate={baudrate}", if_link_state_name(state)),
                    });
                    total_parsed += 1;
                }
                EVT_IF_FLAGS_CHANGE if ir.typed() || payload_size >= 12 => {
                    let ifindex = ir.u16("ifindex", 0);
                    let old_flags = ir.u32("old_flags", 4);
                    let new_flags = ir.u32("new_flags", 8);
                    if let Some(e) = if_profile.interfaces.get_mut(&ifindex) {
                        e.flags = new_flags;
                    }
                    if_profile.events.push(IfEvent {
                        ts: timestamp,
                        ifindex,
                        event_id,
                        detail: format!("flags {old_flags:#x} -> {new_flags:#x}"),
                    });
                    total_parsed += 1;
                }
                EVT_IF_MTU_CHANGE if ir.typed() || payload_size >= 12 => {
                    let ifindex = ir.u16("ifindex", 0);
                    let old_mtu = ir.u32("old_mtu", 4);
                    let new_mtu = ir.u32("new_mtu", 8);
                    if let Some(e) = if_profile.interfaces.get_mut(&ifindex) {
                        e.mtu = new_mtu;
                    }
                    if_profile.events.push(IfEvent {
                        ts: timestamp,
                        ifindex,
                        event_id,
                        detail: format!("mtu {old_mtu} -> {new_mtu}"),
                    });
                    total_parsed += 1;
                }
                EVT_IF_CAPABILITIES_CHANGE if ir.typed() || payload_size >= 20 => {
                    let ifindex = ir.u16("ifindex", 0);
                    let old_caps = ir.u32("old_caps", 4);
                    let new_caps = ir.u32("new_caps", 8);
                    let old_caps2 = ir.u32("old_caps2", 12);
                    let new_caps2 = ir.u32("new_caps2", 16);
                    if_profile.events.push(IfEvent {
                        ts: timestamp,
                        ifindex,
                        event_id,
                        detail: if ir.has("old_caps2") || !ir.typed() {
                            format!(
                                "caps {old_caps:#x} -> {new_caps:#x} (caps2 {old_caps2:#x} -> {new_caps2:#x})"
                            )
                        } else {
                            format!("caps {old_caps:#x} -> {new_caps:#x}")
                        },
                    });
                    total_parsed += 1;
                }
                EVT_IF_ADDR_ADD | EVT_IF_ADDR_REMOVE if ir.typed() || payload_size >= 6 => {
                    let ifindex = ir.u16("ifindex", 0);
                    let family = ir.u16("family", 2);
                    let prefixlen = ir.u8("prefixlen", 4);
                    let addr_len = ir.u8("addr_len", 5) as usize;
                    let addr = ir.varlen_bytes("addr", 6, addr_len.min(16));
                    let verb = if event_id == EVT_IF_ADDR_ADD {
                        "addr-add"
                    } else {
                        "addr-remove"
                    };
                    if_profile.events.push(IfEvent {
                        ts: timestamp,
                        ifindex,
                        event_id,
                        detail: format!(
                            "{verb} {} prefixlen={prefixlen}",
                            format_if_addr(family, &addr)
                        ),
                    });
                    total_parsed += 1;
                }
                // Deprecated: no protocol split, carried for old captures.
                // tcp/udp are 0 (no classification data at all), so the
                // "other" fallback below correctly attributes 100% of
                // traffic to other -- consistent with the behavior
                // before the appended other_* counters.
                EVT_IF_SAMPLE if ir.typed() || payload_size >= 96 => {
                    let ibytes = ir.u64("ibytes", 8);
                    let obytes = ir.u64("obytes", 16);
                    let ipackets = ir.u64("ipackets", 24);
                    let opackets = ir.u64("opackets", 32);
                    if_profile.samples.push(IfSample {
                        ts: timestamp,
                        ifindex: ir.u16("ifindex", 0),
                        ibytes,
                        obytes,
                        ipackets,
                        opackets,
                        ierrors: ir.u64("ierrors", 40),
                        oerrors: ir.u64("oerrors", 48),
                        iqdrops: ir.u64("iqdrops", 56),
                        oqdrops: ir.u64("oqdrops", 64),
                        imcasts: ir.u64("imcasts", 72),
                        omcasts: ir.u64("omcasts", 80),
                        noproto: ir.u64("noproto", 88),
                        tcp_ibytes: 0,
                        tcp_obytes: 0,
                        udp_ibytes: 0,
                        udp_obytes: 0,
                        tcp_ipackets: 0,
                        tcp_opackets: 0,
                        udp_ipackets: 0,
                        udp_opackets: 0,
                        other_ibytes: ibytes,
                        other_obytes: obytes,
                        other_ipackets: ipackets,
                        other_opackets: opackets,
                    });
                    total_parsed += 1;
                }
                // The kernel emits IF_SAMPLE_BYTES then IF_SAMPLE_PACKETS
                // back to back for the same ifindex, once per sample tick
                // (see if_eventlog_sample_tick()). Stash the bytes half
                // here and complete the record when its packets half
                // arrives, so the GUI/CLI see one merged IfSample per
                // tick, matching the pre-split IF_SAMPLE shape.
                EVT_IF_SAMPLE_BYTES if ir.typed() || payload_size >= 56 => {
                    let ifindex = ir.u16("ifindex", 0);
                    let ibytes = ir.u64("ibytes", 8);
                    let obytes = ir.u64("obytes", 16);
                    let tcp_ibytes = ir.u64("tcp_ibytes", 24);
                    let tcp_obytes = ir.u64("tcp_obytes", 32);
                    let udp_ibytes = ir.u64("udp_ibytes", 40);
                    let udp_obytes = ir.u64("udp_obytes", 48);
                    // other_ibytes/obytes are additive fields
                    // appended after udp_obytes: present (a direct,
                    // IP-dispatch-synchronous counter) on a schema new
                    // enough to carry them, else derived the old way.
                    let (other_ibytes, other_obytes) =
                        if ir.has("other_ibytes") || payload_size >= 72 {
                            (ir.u64("other_ibytes", 56), ir.u64("other_obytes", 64))
                        } else {
                            (
                                ibytes.saturating_sub(tcp_ibytes).saturating_sub(udp_ibytes),
                                obytes.saturating_sub(tcp_obytes).saturating_sub(udp_obytes),
                            )
                        };
                    if let Some(orphan) = if_sample_pending.remove(&ifindex) {
                        // Previous tick's bytes half never got its packets
                        // half (dropped/truncated event) -- flush it
                        // standalone rather than losing it.
                        if_profile.samples.push(orphan);
                    }
                    if_sample_pending.insert(
                        ifindex,
                        IfSample {
                            ts: timestamp,
                            ifindex,
                            ibytes,
                            obytes,
                            tcp_ibytes,
                            tcp_obytes,
                            udp_ibytes,
                            udp_obytes,
                            other_ibytes,
                            other_obytes,
                            ipackets: 0,
                            opackets: 0,
                            ierrors: 0,
                            oerrors: 0,
                            iqdrops: 0,
                            oqdrops: 0,
                            imcasts: 0,
                            omcasts: 0,
                            noproto: 0,
                            tcp_ipackets: 0,
                            tcp_opackets: 0,
                            udp_ipackets: 0,
                            udp_opackets: 0,
                            other_ipackets: 0,
                            other_opackets: 0,
                        },
                    );
                    total_parsed += 1;
                }
                EVT_IF_SAMPLE_PACKETS if ir.typed() || payload_size >= 112 => {
                    let ifindex = ir.u16("ifindex", 0);
                    let ipackets = ir.u64("ipackets", 8);
                    let opackets = ir.u64("opackets", 16);
                    let tcp_ipackets = ir.u64("tcp_ipackets", 24);
                    let tcp_opackets = ir.u64("tcp_opackets", 32);
                    let udp_ipackets = ir.u64("udp_ipackets", 40);
                    let udp_opackets = ir.u64("udp_opackets", 48);
                    let ierrors = ir.u64("ierrors", 56);
                    let oerrors = ir.u64("oerrors", 64);
                    let iqdrops = ir.u64("iqdrops", 72);
                    let oqdrops = ir.u64("oqdrops", 80);
                    let imcasts = ir.u64("imcasts", 88);
                    let omcasts = ir.u64("omcasts", 96);
                    let noproto = ir.u64("noproto", 104);
                    let (other_ipackets, other_opackets) =
                        if ir.has("other_ipackets") || payload_size >= 128 {
                            (ir.u64("other_ipackets", 112), ir.u64("other_opackets", 120))
                        } else {
                            (
                                ipackets
                                    .saturating_sub(tcp_ipackets)
                                    .saturating_sub(udp_ipackets),
                                opackets
                                    .saturating_sub(tcp_opackets)
                                    .saturating_sub(udp_opackets),
                            )
                        };
                    if let Some(mut s) = if_sample_pending.remove(&ifindex) {
                        s.ipackets = ipackets;
                        s.opackets = opackets;
                        s.tcp_ipackets = tcp_ipackets;
                        s.tcp_opackets = tcp_opackets;
                        s.udp_ipackets = udp_ipackets;
                        s.udp_opackets = udp_opackets;
                        s.other_ipackets = other_ipackets;
                        s.other_opackets = other_opackets;
                        s.ierrors = ierrors;
                        s.oerrors = oerrors;
                        s.iqdrops = iqdrops;
                        s.oqdrops = oqdrops;
                        s.imcasts = imcasts;
                        s.omcasts = omcasts;
                        s.noproto = noproto;
                        if_profile.samples.push(s);
                    } else {
                        // No matching bytes half seen (e.g. capture starts
                        // mid-tick) -- push a packets-only record; byte
                        // fields read as 0 for this one tick.
                        if_profile.samples.push(IfSample {
                            ts: timestamp,
                            ifindex,
                            ibytes: 0,
                            obytes: 0,
                            tcp_ibytes: 0,
                            tcp_obytes: 0,
                            udp_ibytes: 0,
                            udp_obytes: 0,
                            other_ibytes: 0,
                            other_obytes: 0,
                            ipackets,
                            opackets,
                            tcp_ipackets,
                            tcp_opackets,
                            udp_ipackets,
                            udp_opackets,
                            other_ipackets,
                            other_opackets,
                            ierrors,
                            oerrors,
                            iqdrops,
                            oqdrops,
                            imcasts,
                            omcasts,
                            noproto,
                        });
                    }
                    total_parsed += 1;
                }
                _ => {
                    total_skipped += 1;
                }
            }
            off += event_length;
            if let Some(prog) = progress {
                let p = 50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
                if p >= last_progress + 5 {
                    prog.store_permille(p.min(800));
                    last_progress = p;
                }
            }
            continue;
        }

        if is_ella_relay {
            let ok = ingest_ella_relay(
                &mut ella_profile,
                timestamp,
                cpu,
                thread_id,
                provider_id,
                session_id,
                event_id,
                raw,
                payload_off,
                payload_size,
                schema,
            );
            if ok {
                total_parsed += 1;
            } else {
                total_skipped += 1;
            }
            off += event_length;
            if let Some(prog) = progress {
                let p = 50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
                if p >= last_progress + 5 {
                    prog.store_permille(p.min(800));
                    last_progress = p;
                }
            }
            continue;
        }

        if is_nginx {
            let ok = ingest_nginx(
                &mut nginx_profile,
                timestamp,
                cpu,
                thread_id,
                provider_id,
                session_id,
                event_id,
                raw,
                payload_off,
                payload_size,
                schema,
            );
            if ok {
                total_parsed += 1;
            } else {
                total_skipped += 1;
            }
            off += event_length;
            if let Some(prog) = progress {
                let p = 50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
                if p >= last_progress + 5 {
                    prog.store_permille(p.min(800));
                    last_progress = p;
                }
            }
            continue;
        }

        // TCP periodic-sample events ride on a per-stack "global" session
        // (one session_id per registered TCP stack), not on a per-connection
        // session, so they're collected into `tcp_samples` rather than the
        // `Sessions` map. TCP_STACK_INFO maps a session_id to its stack name
        // + sample period.
        if is_tcp
            && matches!(
                event_id,
                EVT_TCP_SAMPLE
                    | EVT_TCP_GP_SAMPLE
                    | EVT_TCP_STACK_INFO
                    | EVT_TCP_RPS_SAMPLE
                    | EVT_TCP_PACING_RATE_SAMPLE
                    | EVT_TCP_REALIZED_RATE_SAMPLE
                    | EVT_TCP_SEND_SAMPLE
                    | EVT_TCP_LRO_FLUSH_SAMPLE
                    | EVT_TCP_SRTT_SAMPLE
            )
        {
            let tr = PayloadReader::new(schema, event_id, raw, payload_off, payload_size);
            match event_id {
                EVT_TCP_STACK_INFO if tr.typed() || payload_size >= 36 => {
                    let stack_name = tr.cstr("stack_name", 0, 32);
                    let sample_period_us = tr.u32("sample_period_us", 32);
                    // Key by (provider_id, session_id): every "tcp"
                    // provider numbers its stack-global session the same,
                    // so session_id alone collides across stacks.
                    tcp_samples.stacks.insert(
                        (provider_id, session_id),
                        TcpStackInfo {
                            stack_name,
                            sample_period_us,
                        },
                    );
                    total_parsed += 1;
                }
                EVT_TCP_SAMPLE if tr.typed() || payload_size >= 88 => {
                    tcp_samples.state_samples.push(TcpStateSample {
                        ts: timestamp,
                        provider_id,
                        session_id,
                        closed: tr.u64("closed", 0),
                        listen: tr.u64("listen", 8),
                        syn_sent: tr.u64("syn_sent", 16),
                        syn_rcvd: tr.u64("syn_rcvd", 24),
                        estab: tr.u64("estab", 32),
                        close_wait: tr.u64("close_wait", 40),
                        fin_wait_1: tr.u64("fin_wait_1", 48),
                        closing: tr.u64("closing", 56),
                        last_ack: tr.u64("last_ack", 64),
                        fin_wait_2: tr.u64("fin_wait_2", 72),
                        time_wait: tr.u64("time_wait", 80),
                    });
                    total_parsed += 1;
                }
                EVT_TCP_RPS_SAMPLE if tr.typed() || payload_size >= 8 => {
                    tcp_samples.rps_samples.push(TcpRpsSample {
                        ts: timestamp,
                        provider_id,
                        session_id,
                        req_count: tr.u64("req_count", 0),
                    });
                    total_parsed += 1;
                }
                EVT_TCP_GP_SAMPLE
                | EVT_TCP_PACING_RATE_SAMPLE
                | EVT_TCP_REALIZED_RATE_SAMPLE
                | EVT_TCP_SEND_SAMPLE
                | EVT_TCP_LRO_FLUSH_SAMPLE
                | EVT_TCP_SRTT_SAMPLE
                    if tr.typed() || payload_size >= 1 =>
                {
                    let max_n = match event_id {
                        EVT_TCP_SEND_SAMPLE | EVT_TCP_LRO_FLUSH_SAMPLE => 64,
                        EVT_TCP_SRTT_SAMPLE => 14,
                        _ => 16,
                    };
                    let hist = tr.hist_u64s(max_n, 0, 8);
                    let sample = TcpGpSample {
                        ts: timestamp,
                        provider_id,
                        session_id,
                        hist,
                    };
                    match event_id {
                        EVT_TCP_PACING_RATE_SAMPLE => tcp_samples.pacing_rate_samples.push(sample),
                        EVT_TCP_REALIZED_RATE_SAMPLE => {
                            tcp_samples.realized_rate_samples.push(sample)
                        }
                        EVT_TCP_SEND_SAMPLE => tcp_samples.send_samples.push(sample),
                        EVT_TCP_LRO_FLUSH_SAMPLE => tcp_samples.lro_flush_samples.push(sample),
                        EVT_TCP_SRTT_SAMPLE => tcp_samples.srtt_samples.push(sample),
                        _ => tcp_samples.gp_samples.push(sample),
                    }
                    total_parsed += 1;
                }
                _ => {
                    total_skipped += 1;
                }
            }
            off += event_length;
            if let Some(prog) = progress {
                let p = 50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
                if p >= last_progress + 5 {
                    prog.store_permille(p.min(800));
                    last_progress = p;
                }
            }
            continue;
        }

        let payload = parse_payload(event_id, raw, payload_off, payload_size, schema);

        if let EventPayload::StarsPace {
            natural_bps,
            scaled_bps,
            pressure,
            len,
        } = &payload
        {
            stars_pace_samples.push(StarsPaceSample {
                ts: timestamp,
                session_id,
                natural_bps: *natural_bps,
                scaled_bps: *scaled_bps,
                pressure: *pressure,
                len: *len,
            });
        }

        if !matches!(payload, EventPayload::Other) {
            sessions
                .entry(session_id)
                .or_insert_with(|| Vec::with_capacity(initial_vec_cap))
                .push(Event {
                    ts: timestamp,
                    cpu,
                    tid: thread_id,
                    event_id,
                    payload,
                });
        }
        total_parsed += 1;

        off += event_length;
        if let Some(prog) = progress {
            let p = 50 + ((off - events_start) as u64 * 750 / events_span as u64) as u32;
            if p >= last_progress + 5 {
                prog.store_permille(p.min(800));
                last_progress = p;
            }
        }
    }

    // Sort each session's events by timestamp (events may arrive out-of-order across CPUs).
    // Sessions are independent so we can sort them in parallel.
    let session_count = sessions.len().max(1);
    if let Some(prog) = progress {
        prog.store_permille(800);
        prog.set_status(format!("Sorting {session_count} sessions…"));
    }
    let sorted_count = std::sync::atomic::AtomicUsize::new(0);
    let sort_one = |events: &mut Vec<Event>| {
        events.sort_unstable_by_key(|e| e.ts);
        if let Some(prog) = progress {
            let done = sorted_count.fetch_add(1, Ordering::Relaxed) + 1;
            if done.is_multiple_of(20) || done == session_count {
                let p = 800 + (done as u64 * 100 / session_count as u64) as u32;
                prog.store_permille(p.min(900));
            }
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    sessions
        .par_iter_mut()
        .for_each(|(_, events)| sort_one(events));
    #[cfg(target_arch = "wasm32")]
    sessions.iter_mut().for_each(|(_, events)| sort_one(events));

    if let Some(prog) = progress {
        prog.store_permille(900);
        prog.set_status("Finalizing…");
    }

    stars_agg_samples.sort_unstable_by_key(|s| s.ts);
    stars_pace_samples.sort_unstable_by_key(|s| s.ts);
    // CPU samples can arrive slightly out of order across CPUs because
    // each per-CPU session writes to its own ring buffer; the merge into
    // the file's event stream isn't strictly time-monotonic. Sort once
    // here so flame-graph time-window filters can rely on monotonic ts.
    // Sessions are independent of this vec so it's safe to sort in
    // parallel too; this one vec can hold tens of millions of samples
    // on cpu-heavy captures, where a single-threaded sort is seconds
    // of dead time on the parse thread.
    #[cfg(not(target_arch = "wasm32"))]
    cpu_profile.samples.par_sort_unstable_by_key(|s| s.ts);
    #[cfg(target_arch = "wasm32")]
    cpu_profile.samples.sort_unstable_by_key(|s| s.ts);
    cpu_profile.sample_rates = detect_sample_rates(
        &cpu_profile.samples,
        cpu_profile.kernel.as_ref().map(|k| k.sample_hz()),
    );
    // SYMBOL_RANGE / USER_SYMBOL_RANGE dedup uses the deterministic
    // alias rule in `sort_symbol_ranges` (see its doc comment).
    // SYMBOL_RANGE events are emitted by elog(1) in the order it
    // discovers PCs, which is roughly by sample timestamp — *not*
    // sorted by symbol offset. Sort each per-file vector by offset
    // and drop any duplicate (offset, name) tuples (elog dedups by
    // (file_id, offset) but a buggy emitter could send dupes; cheap
    // insurance) so `CpuProfile::symbolize` can binary-search.
    //
    // After sorting, expand every zero-sized symbol so its range
    // runs up to the next symbol's offset (or the owning linker
    // file's end for the last entry). FreeBSD's hand-written kernel
    // assembly entry points (`Xtimerint`, `fork_trampoline`,
    // `Xfast_syscall`, `calltrap`, …) ship with `st_size == 0`
    // because the asm macros never emit `.size` directives, so the
    // ELF — and therefore the `SYMBOL_RANGE` events elog(1)
    // synthesises from it — record `size: 0`. Without a real range
    // those entries can't be matched by binary search: any PC in
    // their body falls *after* their offset and *before* the next
    // symbol's, the `partition_point` lookup lands on the next
    // entry, and the lookup misses. pmcstat(8), addr2line(1), and
    // every other consumer that walks an ELF symbol table fix this
    // the same way: extend zero-sized entries to the next entry's
    // offset. Doing it once here keeps `CpuProfile::symbolize` a
    // straight half-open-interval check (no special case for size
    // == 0) and resolves the asm entry points that previously
    // appeared in flame graphs as raw `kernel+0xOFFSET` labels.
    for (file_id, syms) in cpu_profile.symbols.iter_mut() {
        sort_symbol_ranges(syms);
        let file_end = cpu_profile
            .linker_files
            .get(file_id)
            .map(|lf| lf.size)
            .unwrap_or(u64::MAX);
        for i in 0..syms.len() {
            if syms[i].size != 0 {
                continue;
            }
            let next = syms.get(i + 1).map(|s| s.offset).unwrap_or(file_end);
            let span = next.saturating_sub(syms[i].offset);
            // Cap at u32::MAX so the u32 `size` field doesn't wrap
            // for pathological cases (e.g. a stray zero-sized
            // symbol near the end of a huge mapping). Real kernel
            // functions are nowhere near 4 GiB; this is purely
            // defensive against malformed inputs.
            syms[i].size = span.min(u32::MAX as u64) as u32;
        }
    }
    // PROC_MAP events arrive in elog's vm_map iteration order
    // (which is base-address order today, but isn't part of the
    // wire contract). Sort each per-pid vector by base so the
    // user-mode resolver can short-circuit on out-of-range PCs and
    // so a future binary-search version slots in without further
    // reshuffling. We deliberately don't dedup: two PROC_MAPs at
    // the same base for the same pid would mean an mmap the
    // emitter saw twice — vanishingly unlikely, and silently
    // dropping one would mask a real bug rather than fix it.
    for maps in cpu_profile.proc_maps.values_mut() {
        maps.sort_unstable_by_key(|pm| pm.base);
    }
    for execs in cpu_profile.proc_execs.values_mut() {
        execs.sort_unstable_by_key(|e| e.ts);
    }
    for titles in cpu_profile.proc_titles.values_mut() {
        titles.sort_unstable_by_key(|t| t.ts);
    }
    cpu_profile.inherit_fork_proc_maps();
    for lines in cpu_profile.source_lines.values_mut() {
        lines.sort_unstable_by_key(|line| line.file_offset);
        lines.dedup_by(|a, b| a.file_offset == b.file_offset);
    }
    // USER_SYMBOL_RANGE follows the same pattern as kernel
    // SYMBOL_RANGE: sort each per-user-file vector by offset,
    // dedup on (offset), then extend any zero-sized entry up to
    // the next entry's offset. The fallback cap when there is no
    // next entry is `u64::MAX` rather than the kernel's
    // `LinkerFile.size`: ELFs don't carry a single global file
    // size we can use here (different `PT_LOAD` segments cover
    // disjoint VA ranges), and any PROC_MAP that references this
    // file already bounds the lookup at resolution time. The
    // u32::MAX cap on the result keeps the field from wrapping if
    // a stray zero-sized symbol is the very last entry in a DSO
    //     whose top symbol sits near `u64::MAX`.
    for syms in cpu_profile.user_symbols.values_mut() {
        sort_symbol_ranges(syms);
        for i in 0..syms.len() {
            if syms[i].size != 0 {
                continue;
            }
            let next = syms.get(i + 1).map(|s| s.offset).unwrap_or(u64::MAX);
            let span = next.saturating_sub(syms[i].offset);
            syms[i].size = span.min(u32::MAX as u64) as u32;
        }
    }

    // Global per-stack / per-interface sample series can arrive slightly
    // out of order; sort by ts so consumers (rate diffs, plots) can rely
    // on monotonic time.
    tcp_samples.state_samples.sort_unstable_by_key(|s| s.ts);
    tcp_samples.rps_samples.sort_unstable_by_key(|s| s.ts);
    tcp_samples.gp_samples.sort_unstable_by_key(|s| s.ts);
    tcp_samples
        .pacing_rate_samples
        .sort_unstable_by_key(|s| s.ts);
    tcp_samples
        .realized_rate_samples
        .sort_unstable_by_key(|s| s.ts);
    tcp_samples.send_samples.sort_unstable_by_key(|s| s.ts);
    tcp_samples.lro_flush_samples.sort_unstable_by_key(|s| s.ts);
    tcp_samples.srtt_samples.sort_unstable_by_key(|s| s.ts);
    ella_profile.events.sort_unstable_by_key(|e| e.ts);
    ella_profile.counts.sort_unstable_by_key(|s| s.ts);
    ella_profile.counts_total.sort_unstable_by_key(|s| s.ts);
    ella_profile.channel_samples.sort_unstable_by_key(|s| s.ts);
    apply_channel_sample_peaks(&mut ella_profile);
    nginx_profile.events.sort_unstable_by_key(|e| e.ts);
    // Any IF_SAMPLE_BYTES halves whose IF_SAMPLE_PACKETS half never
    // arrived (capture ends mid-tick) -- flush them standalone.
    if_profile.samples.extend(if_sample_pending.into_values());
    if_profile.samples.sort_unstable_by_key(|s| s.ts);
    if_profile.events.sort_unstable_by_key(|e| e.ts);

    for i in 0..providers.len() {
        providers[i].event_count = provider_counts[i];
        providers[i].byte_size = provider_bytes[i];
        providers[i].session_count = provider_sessions[i].len();
        providers[i].first_ts = provider_first[i];
        providers[i].last_ts = provider_last[i];
    }
    attach_event_stats(
        &mut providers,
        event_acc,
        unknown_events,
        elog_events,
        &provider_kind,
        &schema_by_provider_id,
        split_schema,
    );

    let (effective_start, backlog_events) =
        effective_capture_start(capture_start, &ts_probe, total_events);

    let file_info = FileInfo {
        version,
        capture_start: effective_start,
        header_capture_start: capture_start,
        backlog_events,
        start_utc_us,
        capture_end: last_event_ts,
        event_count,
        dropped,
        user_dropped_full,
        user_dropped_prejoin,
        user_dropped_late,
        parsed: total_parsed,
        skipped_other_providers: total_skipped,
        sources: Vec::new(),
        machine: machine_info,
    };

    schema_capture
        .hpts_wheel_events
        .sort_unstable_by_key(|e| e.ts);
    schema_capture.generic_events.sort_unstable_by_key(|e| e.ts);
    schema_capture.mem.infos.sort_unstable_by_key(|e| e.ts);
    schema_capture.mem.samples.sort_unstable_by_key(|e| e.ts);

    Ok((
        sessions,
        stars_agg_samples,
        stars_pace_samples,
        cpu_profile,
        if_profile,
        tcp_samples,
        ella_profile,
        nginx_profile,
        file_info,
        providers,
        schema_capture,
    ))
}

fn parse_machine_info(payload: &[u8]) -> Option<MachineInfo> {
    if payload.len() < 729 {
        return None;
    }
    Some(MachineInfo {
        hostuuid: read_cstr(payload, 0, 37),
        hostname: read_cstr(payload, 37, 64),
        osrelease: read_cstr(payload, 101, 32),
        kernel_version: read_cstr(payload, 133, 256),
        firmware_version: read_cstr(payload, 389, 128),
        platform: read_cstr(payload, 517, 64),
        cpu_model: read_cstr(payload, 581, 128),
        ncpu: read_u32(payload, 709),
        physmem: read_u64(payload, 713),
        boottime_sec: read_u64(payload, 721) as i64,
        metadata_kv: Vec::new(),
    })
}

fn parse_machine_kv(payload: &[u8]) -> Option<(String, String)> {
    if payload.len() < 320 {
        return None;
    }
    let key = read_cstr(payload, 0, 64);
    let value = read_cstr(payload, 64, 256);
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

struct EventAcc {
    event_id: u32,
    count: u64,
    bytes: u64,
    payload_bytes: u64,
    min_len: u32,
    max_len: u32,
    first_ts: u64,
    last_ts: u64,
}

impl EventAcc {
    fn new(event_id: u32, record_len: u32, ts: u64) -> Self {
        let payload = record_len.saturating_sub(EVENT_HDR_SIZE as u32);
        Self {
            event_id,
            count: 1,
            bytes: record_len as u64,
            payload_bytes: payload as u64,
            min_len: record_len,
            max_len: record_len,
            first_ts: ts,
            last_ts: ts,
        }
    }

    fn add(&mut self, record_len: u32, ts: u64) {
        let payload = record_len.saturating_sub(EVENT_HDR_SIZE as u32);
        self.count += 1;
        self.bytes += record_len as u64;
        self.payload_bytes += payload as u64;
        self.min_len = self.min_len.min(record_len);
        self.max_len = self.max_len.max(record_len);
        self.first_ts = self.first_ts.min(ts);
        self.last_ts = self.last_ts.max(ts);
    }
}

#[derive(Default)]
struct ProviderEventAcc {
    last: usize,
    rows: Vec<EventAcc>,
}

impl ProviderEventAcc {
    fn bump(&mut self, event_id: u32, record_len: u32, ts: u64) {
        if let Some(row) = self.rows.get_mut(self.last) {
            if row.event_id == event_id {
                row.add(record_len, ts);
                return;
            }
        }
        if let Some(i) = self.rows.iter().position(|r| r.event_id == event_id) {
            self.last = i;
            self.rows[i].add(record_len, ts);
            return;
        }
        self.last = self.rows.len();
        self.rows.push(EventAcc::new(event_id, record_len, ts));
    }
}

fn builtin_event_name(kind: ProviderKind, event_id: u32, split_schema: bool) -> String {
    let name = match kind {
        ProviderKind::Tcp => event_name(event_id),
        ProviderKind::If => if_event_name(event_id),
        ProviderKind::Hpts => {
            if event_id == EVT_HPTS_STARS_AGG_SAMPLE {
                "STARS_AGG_SAMPLE"
            } else {
                hpts_wheel_event_name(event_id)
            }
        }
        ProviderKind::Cpu => {
            if split_schema && event_id == EVT_CPUX_SAMPLE {
                "CPU_SAMPLE"
            } else {
                cpu_event_name(event_id)
            }
        }
        ProviderKind::Symbols => match event_id {
            EVT_SYM_KERNEL_INFO => "KERNEL_INFO",
            EVT_SYM_LINKER_FILE => "LINKER_FILE",
            EVT_SYM_SYMBOL_RANGE => "SYMBOL_RANGE",
            EVT_SYM_USER_FILE => "USER_FILE",
            EVT_SYM_USER_SYMBOL_RANGE => "USER_SYMBOL_RANGE",
            EVT_SYM_SOURCE_FILE => "SOURCE_FILE",
            EVT_SYM_SOURCE_LINE => "SOURCE_LINE",
            EVT_SYM_SYMBOL_PACK_REF => "SYMBOL_PACK_REF",
            _ => "UNKNOWN",
        },
        ProviderKind::Process => match event_id {
            EVT_PROC_THREAD_CREATE => "THREAD_CREATE",
            EVT_PROC_THREAD_EXIT => "THREAD_EXIT",
            EVT_PROC_EXEC => "PROC_EXEC",
            EVT_PROC_EXIT => "PROC_EXIT",
            EVT_PROC_MAP => "PROC_MAP",
            EVT_PROC_FORK => "PROC_FORK",
            EVT_PROC_TITLE => "PROC_TITLE",
            _ => "UNKNOWN",
        },
        ProviderKind::EllaRelay => ella_relay_event_name(event_id),
        ProviderKind::Nginx => match event_id {
            EVT_NGINX_REQUEST_START => "REQUEST_START",
            EVT_NGINX_RESPONSE_HEADERS => "RESPONSE_HEADERS",
            EVT_NGINX_REQUEST_DONE => "REQUEST_DONE",
            EVT_NGINX_REQUEST_MESSAGE => "REQUEST_MESSAGE",
            EVT_NGINX_SAMPLE => "NGINX_SAMPLE",
            EVT_NGINX_CONN_START => "CONN_START",
            EVT_NGINX_CONN_END => "CONN_END",
            EVT_NGINX_TLS_HANDSHAKE_DONE => "TLS_HANDSHAKE_DONE",
            EVT_NGINX_TLS_HANDSHAKE_FAIL => "TLS_HANDSHAKE_FAIL",
            EVT_NGINX_SENDFILE_BUSY_DEFER => "SENDFILE_BUSY_DEFER",
            EVT_NGINX_SENDFILE_ERROR => "SENDFILE_ERROR",
            EVT_NGINX_FILE_OPEN_FAIL => "FILE_OPEN_FAIL",
            EVT_NGINX_CACHE_LOOKUP => "CACHE_LOOKUP",
            EVT_NGINX_UPSTREAM_ATTEMPT_START => "UPSTREAM_ATTEMPT_START",
            EVT_NGINX_UPSTREAM_ATTEMPT_END => "UPSTREAM_ATTEMPT_END",
            EVT_NGINX_WORKER_STALL => "WORKER_STALL",
            EVT_NGINX_AIO_SYNC_FALLBACK => "AIO_SYNC_FALLBACK",
            _ => "UNKNOWN",
        },
        ProviderKind::Machine => match event_id {
            EVT_MACHINE_INFO => "MACHINE_INFO",
            EVT_MACHINE_METADATA_KV => "METADATA_KV",
            _ => "UNKNOWN",
        },
        ProviderKind::Mem => match event_id {
            EVT_MEM_INFO => "MEM_INFO",
            EVT_MEM_SYS_SAMPLE => "SYS_SAMPLE",
            _ => "UNKNOWN",
        },
        ProviderKind::Other => "UNKNOWN",
    };
    if name == "UNKNOWN" {
        format!("id_{event_id}")
    } else {
        name.to_string()
    }
}

fn stat_from_schema(schema: &ProviderSchema, event_id: u32) -> Option<EventStat> {
    let ev = schema.find_event(event_id)?;
    let keywords = schema
        .keywords
        .iter()
        .filter(|k| ev.keywords & k.value != 0)
        .map(|k| k.name.clone())
        .collect();
    let mut fields = Vec::new();
    if let Some(st) = schema.struct_for_event(ev) {
        let mut src: Vec<&DecodeField> = st.fields.iter().collect();
        src.sort_by_key(|f| f.offset);
        for f in src {
            let varlen = f.flags & DECODE_FIELD_VARLEN != 0;
            fields.push(EventFieldStat {
                name: f.name.clone(),
                offset: f.offset,
                size: if varlen { 0 } else { field_byte_size(f) },
                varlen,
                varlen_max: f.varlen_max,
            });
        }
    }
    Some(EventStat {
        event_id,
        name: ev.name.clone(),
        count: 0,
        bytes: 0,
        payload_bytes: 0,
        min_len: 0,
        max_len: 0,
        first_ts: None,
        last_ts: None,
        level: Some(ev.level),
        keywords,
        fields,
        format: ev.format.clone(),
        defined_only: true,
    })
}

fn stat_from_acc(
    kind: ProviderKind,
    acc: EventAcc,
    schema: Option<&ProviderSchema>,
    split_schema: bool,
    name_override: Option<&str>,
) -> EventStat {
    let mut stat = schema
        .and_then(|s| stat_from_schema(s, acc.event_id))
        .unwrap_or_else(|| EventStat {
            event_id: acc.event_id,
            name: name_override
                .map(str::to_string)
                .unwrap_or_else(|| builtin_event_name(kind, acc.event_id, split_schema)),
            count: 0,
            bytes: 0,
            payload_bytes: 0,
            min_len: 0,
            max_len: 0,
            first_ts: None,
            last_ts: None,
            level: None,
            keywords: Vec::new(),
            fields: Vec::new(),
            format: String::new(),
            defined_only: false,
        });
    if let Some(name) = name_override {
        stat.name = name.to_string();
    }
    stat.count = acc.count;
    stat.bytes = acc.bytes;
    stat.payload_bytes = acc.payload_bytes;
    stat.min_len = acc.min_len;
    stat.max_len = acc.max_len;
    stat.first_ts = Some(acc.first_ts);
    stat.last_ts = Some(acc.last_ts);
    stat.defined_only = false;
    stat
}

fn fill_provider_events(
    provider: &mut Provider,
    kind: ProviderKind,
    acc: ProviderEventAcc,
    schema: Option<&ProviderSchema>,
    split_schema: bool,
    name_override: impl Fn(u32) -> Option<&'static str>,
) {
    let mut seen = FxHashSet::default();
    for row in acc.rows {
        let event_id = row.event_id;
        seen.insert(event_id);
        provider.events.push(stat_from_acc(
            kind,
            row,
            schema,
            split_schema,
            name_override(event_id),
        ));
    }
    if let Some(schema) = schema {
        for ev in &schema.events {
            if seen.contains(&ev.id) {
                continue;
            }
            if let Some(stat) = stat_from_schema(schema, ev.id) {
                provider.events.push(stat);
            }
        }
    }
    provider.events.sort_by_key(|e| e.event_id);
}

fn unused_provider_id(providers: &[Provider]) -> u16 {
    let used: FxHashSet<u16> = providers.iter().map(|p| p.id).collect();
    (0..=u16::MAX)
        .rev()
        .find(|id| !used.contains(id))
        .unwrap_or(0)
}

fn push_synthetic_provider(
    providers: &mut Vec<Provider>,
    name: String,
    kind: ProviderKind,
    acc: ProviderEventAcc,
    name_override: impl Fn(u32) -> Option<&'static str>,
) {
    if acc.rows.is_empty() {
        return;
    }
    let id = unused_provider_id(providers);
    let mut provider = Provider {
        id,
        name,
        event_count: 0,
        first_ts: None,
        last_ts: None,
        session_count: 0,
        byte_size: 0,
        schema_name: None,
        schema_event_count: 0,
        schema_divergent_events: Vec::new(),
        schema_new_fields: Vec::new(),
        events: Vec::new(),
    };
    fill_provider_events(&mut provider, kind, acc, None, false, name_override);
    provider.event_count = provider.events.iter().map(|e| e.count).sum();
    provider.byte_size = provider.events.iter().map(|e| e.bytes).sum();
    provider.first_ts = provider.events.iter().filter_map(|e| e.first_ts).min();
    provider.last_ts = provider.events.iter().filter_map(|e| e.last_ts).max();
    providers.push(provider);
}

fn attach_event_stats(
    providers: &mut Vec<Provider>,
    event_acc: Vec<ProviderEventAcc>,
    unknown_events: FxHashMap<u16, ProviderEventAcc>,
    elog_events: ProviderEventAcc,
    provider_kind: &FxHashMap<u16, (ProviderKind, usize)>,
    schemas: &FxHashMap<u16, ProviderSchema>,
    split_schema: bool,
) {
    debug_assert_eq!(providers.len(), event_acc.len());
    for (provider, acc) in providers.iter_mut().zip(event_acc) {
        let kind = provider_kind
            .get(&provider.id)
            .map(|(k, _)| *k)
            .unwrap_or(ProviderKind::Other);
        let schema = schemas.get(&provider.id);
        fill_provider_events(provider, kind, acc, schema, split_schema, |_| None);
    }
    let mut unknown: Vec<(u16, ProviderEventAcc)> = unknown_events.into_iter().collect();
    unknown.sort_by_key(|(id, _)| *id);
    for (id, acc) in unknown {
        push_synthetic_provider(
            providers,
            format!("provider_{id}"),
            ProviderKind::Other,
            acc,
            |_| None,
        );
    }
    push_synthetic_provider(
        providers,
        "elog".to_string(),
        ProviderKind::Other,
        elog_events,
        |id| {
            if id == EVT_PROVIDER_INFO {
                Some("PROVIDER_INFO")
            } else if id == EVT_PROVIDER_BLOB {
                Some("PROVIDER_BLOB")
            } else {
                None
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elog_parser::types::UserSymbolRange;

    fn usr(offset: u64, name: &str) -> UserSymbolRange {
        UserSymbolRange {
            user_file_id: 1,
            offset,
            size: 0,
            name: name.to_string(),
        }
    }

    #[test]
    fn symbol_alias_dedup_prefers_fewest_leading_underscores() {
        // Same address, libc aliases in a scrambled stream order. The
        // public name (one leading underscore) must win regardless of
        // input order — the unstable sort used to make this depend on
        // Rust's sort internals.
        let mut syms = vec![
            usr(0x100, "__sys_write"),
            usr(0x100, "_write"),
            usr(0x200, "__sysctl"),
            usr(0x200, "___sysctl"),
        ];
        sort_symbol_ranges(&mut syms);
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].offset, 0x100);
        assert_eq!(syms[0].name, "_write");
        assert_eq!(syms[1].offset, 0x200);
        assert_eq!(syms[1].name, "__sysctl"); // no fewer-underscore alias here
    }

    #[test]
    fn symbol_alias_dedup_is_input_order_independent() {
        let base = ["__sys_select", "_select", "select"];
        let mut a: Vec<_> = base.iter().map(|n| usr(0x40, n)).collect();
        let mut b: Vec<_> = base.iter().rev().map(|n| usr(0x40, n)).collect();
        sort_symbol_ranges(&mut a);
        sort_symbol_ranges(&mut b);
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].name, b[0].name);
        assert_eq!(a[0].name, "select");
    }

    #[test]
    fn session_vec_cap_follows_connection_fanout() {
        // 13.1M events across 50k connections averages 262 slots.
        assert_eq!(session_vec_cap(13_100_818, 50_000), 262);
        assert_eq!(session_vec_cap(0, 0), 32);
        assert_eq!(session_vec_cap(10_000_000, 1), 4096);
    }

    #[test]
    fn parse_machine_info_roundtrip() {
        let mut payload = vec![0u8; 729];
        fn write_cstr(buf: &mut [u8], off: usize, max: usize, s: &str) {
            let n = s.len().min(max - 1);
            buf[off..off + n].copy_from_slice(s.as_bytes());
        }
        write_cstr(&mut payload, 0, 37, "uuid-here");
        write_cstr(&mut payload, 37, 64, "c101.lab001.dev");
        write_cstr(&mut payload, 101, 32, "15.0-CURRENT");
        write_cstr(&mut payload, 133, 256, "FreeBSD 15.0");
        write_cstr(&mut payload, 389, 128, "2026Q2.1");
        write_cstr(&mut payload, 517, 64, "amd64");
        write_cstr(&mut payload, 581, 128, "Intel(R) Xeon(R)");
        payload[709..713].copy_from_slice(&4u32.to_le_bytes());
        payload[713..721].copy_from_slice(&((64u64) << 30).to_le_bytes());
        payload[721..729].copy_from_slice(&1_700_000_000i64.to_le_bytes());

        let mi = parse_machine_info(&payload).expect("parse");
        assert_eq!(mi.hostuuid, "uuid-here");
        assert_eq!(mi.hostname, "c101.lab001.dev");
        assert_eq!(mi.firmware_version, "2026Q2.1");
        assert_eq!(mi.ncpu, 4);
    }

    const S: u64 = 1_000_000;

    /// A body of live events, one per second starting at `start`.
    fn body(start: u64, n: u64) -> Vec<u64> {
        (0..n).map(|i| start + i * S).collect()
    }

    #[test]
    fn a_dense_capture_keeps_its_header_origin() {
        let prefix = body(500 * S, 200);
        assert_eq!(effective_capture_start(500 * S, &prefix, 200), (500 * S, 0));
    }

    #[test]
    fn a_leading_registration_record_moves_the_origin_past_the_gap() {
        let mut prefix = vec![10 * S];
        prefix.extend(body(1500 * S, 200));
        assert_eq!(
            effective_capture_start(10 * S, &prefix, 100_000),
            (1500 * S, 1)
        );
    }

    /// The probe sees events in file order, which `elog(1)`'s merge only
    /// approximately sorts, so the gap search has to sort first.
    #[test]
    fn out_of_order_probe_events_still_resolve_the_gap() {
        let mut prefix = body(1500 * S, 50);
        prefix.swap(0, 7);
        prefix.insert(3, 10 * S);
        assert_eq!(
            effective_capture_start(10 * S, &prefix, 100_000),
            (1500 * S, 1)
        );
    }

    #[test]
    fn a_gap_under_the_floor_is_left_alone() {
        let mut prefix = vec![10 * S];
        prefix.extend(body(59 * S, 200));
        assert_eq!(
            effective_capture_start(10 * S, &prefix, 100_000),
            (10 * S, 0)
        );
    }

    /// A quiet opening *phase* holding a real share of the capture is data,
    /// not backlog -- moving the origin past it would hide it.
    #[test]
    fn a_substantial_leading_phase_is_left_alone() {
        let mut prefix = body(10 * S, 100);
        prefix.extend(body(1500 * S, 100));
        assert_eq!(effective_capture_start(10 * S, &prefix, 1_000), (10 * S, 0));
    }

    #[test]
    fn a_single_event_capture_keeps_its_header_origin() {
        assert_eq!(effective_capture_start(10 * S, &[10 * S], 1), (10 * S, 0));
        assert_eq!(effective_capture_start(10 * S, &[], 0), (10 * S, 0));
    }

    fn push_u16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn push_u32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn push_u64(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    /// v1 capture: one `tcp` provider plus the given records
    /// `(provider_id, event_id, ts, payload_len)`.
    fn census_elog(events: &[(u16, u32, u64, usize)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"ELOG");
        push_u32(&mut out, 1);
        push_u64(&mut out, 1_000_000);
        push_u64(&mut out, 1_700_000_000_000_000);
        push_u64(&mut out, events.len() as u64);
        push_u64(&mut out, 0);
        push_u32(&mut out, 1);
        push_u16(&mut out, 1);
        let mut name = b"tcp".to_vec();
        name.resize(32, 0);
        out.extend_from_slice(&name);
        for &(pid, eid, ts, payload) in events {
            let len = (EVENT_HDR_SIZE + payload) as u16;
            push_u16(&mut out, len);
            push_u16(&mut out, 0);
            push_u16(&mut out, pid);
            push_u16(&mut out, 0);
            push_u64(&mut out, ts);
            push_u64(&mut out, 1);
            push_u32(&mut out, eid);
            push_u32(&mut out, 0);
            out.resize(out.len() + payload, 0);
        }
        out
    }

    fn provider_named<'a>(providers: &'a [Provider], name: &str) -> &'a Provider {
        providers.iter().find(|p| p.name == name).expect(name)
    }

    #[test]
    fn event_census_counts_records_and_merges() {
        let bytes = census_elog(&[
            (1, EVT_RTT, 1_000_000, 8),
            (1, EVT_RTT, 2_000_000, 16),
            (1, EVT_IN, 1_500_000, 0),
            (0, EVT_PROVIDER_INFO, 1_000_000, 0),
            (99, 7, 1_200_000, 4),
        ]);
        let cap = parse_elog_core(&bytes, None).unwrap();
        let tcp = provider_named(&cap.9, "tcp");
        assert_eq!(tcp.event_count, 3);
        assert_eq!(tcp.byte_size, 40 + 48 + 32);
        let rtt = tcp.events.iter().find(|e| e.event_id == EVT_RTT).unwrap();
        assert_eq!(rtt.name, "RTT");
        assert_eq!(rtt.count, 2);
        assert_eq!(rtt.bytes, 88);
        assert_eq!(rtt.payload_bytes, 24);
        assert_eq!(rtt.min_len, 40);
        assert_eq!(rtt.max_len, 48);
        assert_eq!(rtt.first_ts, Some(1_000_000));
        assert_eq!(rtt.last_ts, Some(2_000_000));
        assert!(!rtt.defined_only);
        let inn = tcp.events.iter().find(|e| e.event_id == EVT_IN).unwrap();
        assert_eq!(inn.name, "IN");
        assert_eq!(inn.count, 1);
        assert_eq!(inn.bytes, 32);
        let elog = provider_named(&cap.9, "elog");
        assert_eq!(elog.event_count, 1);
        assert_eq!(elog.events[0].name, "PROVIDER_INFO");
        assert_eq!(elog.events[0].bytes, 32);
        let unknown = provider_named(&cap.9, "provider_99");
        assert_eq!(unknown.events[0].name, "id_7");
        assert_eq!(unknown.events[0].bytes, 36);

        let a = parse_elog_core(&census_elog(&[(1, EVT_RTT, 1_000_000, 8)]), None).unwrap();
        let b = parse_elog_core(&census_elog(&[(1, EVT_RTT, 3_000_000, 24)]), None).unwrap();
        let merged = merge_captures(vec![a, b]).unwrap();
        let tcp = provider_named(&merged.9, "tcp");
        let rtt = tcp.events.iter().find(|e| e.event_id == EVT_RTT).unwrap();
        assert_eq!(rtt.count, 2);
        assert_eq!(rtt.bytes, 40 + 56);
        assert_eq!(rtt.min_len, 40);
        assert_eq!(rtt.max_len, 56);
        assert_eq!(rtt.first_ts, Some(1_000_000));
        assert_eq!(rtt.last_ts, Some(3_000_000));
    }
}
