# .elog File Format

This document describes the on-disk layout of FreeBSD EventLog `.elog` files
as understood by EventExplorer's parser
([`src/elog_parser.rs`](../src/elog_parser.rs)). The format is produced by
the FreeBSD kernel's EventLog framework; EventExplorer reads, but does not
write, these files.

The envelope (file header, provider table, event-record framing) is the
same for every EventLog capture regardless of which kernel subsystems
produced it. The per-event payload layout is defined by the *provider* that
emitted the event. The provider catalogs below describe the providers
EventExplorer decodes today — `tcp` in depth, `cpu` for flame graphs, `if`
for interface counters, and `hpts` minimally — and are expected to expand
over time as more kernel components adopt EventLog.

> **Multiple providers can share a name.** A capture may contain more than
> one provider row named `tcp` (e.g. one carrying per-connection events and
> a second carrying the periodic per-stack `*_SAMPLE` snapshots). The parser
> therefore routes events by a `provider_id → kind` map built from the
> provider table, not by a single remembered id — see the
> [parsing pipeline](#parsing-pipeline).

All multi-byte integer fields are **little-endian** unless noted otherwise.
Field layout mirrors the Python `struct` format strings in the comments of
`src/elog_parser.rs`.

## Top-level layout

```
+----------------------------+
| File header       (40 B, 36 B for version 3)
+----------------------------+
| [version 3 only]           |
| Header counters   (counter_count * 8 B)
+----------------------------+
| Provider count    (4 B)    |
| Provider table    (34 B each, 50 B each for version 2+)
+----------------------------+
| [version 2+ only]          |
| Blob count        (4 B)    |
| Blob table        (variable — schema_id + size + EDB1 blob, repeated)
+----------------------------+
| Event stream      (variable)
+----------------------------+
```

See [Version 2: `schema_id` and embedded decode blobs](#version-2-schema_id-and-embedded-decode-blobs)
below for the wider provider row and the blob table's own layout, and
[Version 3: header counter array](#version-3-header-counter-array) for the
header change.

Uncompressed files are read via `mmap` (`memmap2`). Files ending in `.gz`
are streamed through `flate2`. A gzip that concatenates several compressed
captures — one `.elog` per member, as in `cat a.elog.gz b.elog.gz` — loads
as a [rolled set](cli.md#rolled-capture-sets). Parallel-gzip chunks of a
single capture stay one file.

## File header (40 bytes; 36 for version 3+)

`struct.pack('<4sIQQQQ', ...)` — 4 + 4 + 8 + 8 + 8 + 8 = 40 bytes. This is
the version 1/2 layout; version 3 replaces the last field — see
[Version 3](#version-3-header-counter-array) below.

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | `magic` | ASCII `ELOG`. Parser rejects anything else. |
| 4 | 4 | `version` | File format version (`u32`). `1` (legacy), `2` (adds per-provider `schema_id` + an optional embedded decode-blob section), or `3` (also replaces the header's single `dropped_events` with a counter array; see below). EventExplorer reads all three. |
| 8 | 8 | `capture_start` | Capture start timestamp (`u64`, microseconds; kernel monotonic clock). All event timestamps are on the same clock. `elog(1)` writes the timestamp of the first event it drained — which is not always a live one, see [Drained ring backlog](#drained-ring-backlog). |
| 16 | 8 | `start_utc_us` | Capture start in UTC microseconds since epoch (`u64`). A `gettimeofday()` taken when `elog(1)` wrote the header, which is what makes it the wall clock for `capture_start`. |
| 24 | 8 | `event_count` | Total events written by the kernel (`u64`). |
| 32 | 8 (v1/v2) or 4 (v3) | `dropped` (v1/v2) / `counter_count` (v3) | v1/v2: events dropped by the ring buffer at capture time (`u64`). v3: number of trailing `u64` counters that follow the header (`u32`) — see below. |

## Drained ring backlog

The header's two time fields are written together — `capture_start` from
the first event `elog(1)` drained, `start_utc_us` from a `gettimeofday()`
taken right then — and name the same instant only when that first event
is a live one.

A user-space provider registers with `libeventlog` and writes an identity
record (`NODE_INFO`, `INSTANCE_ATTACH`) immediately. A provider that had
been running for a while before `elog` attached therefore leaves that
record sitting in its ring, and draining it sets `capture_start` to the
provider's start rather than the capture's. The pair then disagrees by
the provider's whole uptime, which shifts every wall-clock label the
reader derives from it into the future and pads the front of the time
axis with an interval that was never captured.

EventExplorer recovers the live start by looking for the idle gap that
separates the backlog from the streamed events — nothing at all is
captured in between — and uses it as the origin
(`effective_capture_start` in
[`src/elog_parser/core.rs`](../src/elog_parser/core.rs)). `FileInfo`
keeps the header value in `header_capture_start` and reports the skipped
span in `backlog_events`; `elog_cli info` prints a `live start` line and
the GUI a "Backlog skipped" row when they differ. Backlog events are
still decoded — only the origin moves, so the identity records they carry
apply as usual.

The recovery needs the backlog to be visible in the file. On a rolled
capture only the first file holds it: the rings are empty by the second
roll, and each later header advances both fields together, carrying the
first file's `start_utc_us - capture_start` mapping — backlog skew and
all — forward across the set. Their headers are therefore internally
consistent and uniformly wrong. Fixing that requires anchoring
`start_utc_us` to a monotonic reading taken alongside it in `elog(1)`.

Loading the set as one capture (see
[cli.md → Rolled capture sets](cli.md#rolled-capture-sets)) is unaffected
either way: the merge orders files by their events and accepts either the
raw or the trimmed origin when it checks that they share a boot.

## Version 3: header counter array

Version 3 (`version == 3`) shrinks the fixed header to 36 bytes by
replacing the single `dropped_events:u64` at offset 32 with
`counter_count:u32`, followed immediately by `counter_count` packed
`u64` values. The provider table and everything after it is unchanged
from version 2 (same 50-byte rows, same optional blob section).

The array is positional and append-only — a given index's meaning never
changes, new counters are only ever added at the end, and a reader
treats any index at or beyond the file's own `counter_count` as `0`
rather than an error. This lets an older EventExplorer build read a
newer file with more counters (ignoring the extra ones) and a newer
build read an older v3 file with fewer (reading the missing trailing
ones as 0), without a version bump either way.

Currently defined indices (`COUNTER_*` in
[`src/elog_parser/constants.rs`](../src/elog_parser/constants.rs), mirroring
`ELOG_COUNTER_*` in `elog.c`):

| Index | Name | Meaning |
|-------|------|---------|
| 0 | `COUNTER_KERNEL_DROPPED` | Kernel per-CPU buffer-full drops. Same meaning as v1/v2's `dropped_events`; surfaced as `FileInfo::dropped`. |
| 1 | `COUNTER_USER_DROPPED_FULL` | User-space producer (`libeventlog`) events dropped because a per-stream kernel buffer filled before the subscriber drained it. |
| 2 | `COUNTER_USER_DROPPED_PREJOIN` | User-space producer events older than the subscriber's join point (the producer's stream predates the subscription). |
| 3 | `COUNTER_USER_DROPPED_LATE` | User-space producer events whose timestamp is older than one already delivered for the same stream — an ordering violation, usually a producer bug. |

`FileInfo` carries these as `dropped` (index 0, same field name as v1/v2)
plus `user_dropped_full` / `user_dropped_prejoin` / `user_dropped_late`
(indices 1-3, always `0` on a v1/v2 file). `elog_cli`'s `info` output and
the GUI's file-info panel only print the user-dropped rows when non-zero,
matching `elog.c`'s own `-v` stats.

A `counter_count` above `HEADER_COUNTER_SANE_MAX` (1024, mirroring the
producer's `ELOG_HEADER_COUNTER_SANE_MAX`) is rejected outright rather
than trusted for a read size, so a corrupt or hostile file can't force an
unbounded allocation/read; a `counter_count` within that bound but still
larger than the file actually has room for fails with a "File truncated
in header counters" error rather than reading past EOF.

## Provider table

Immediately after the file header:

| Offset | Size | Field |
|--------|------|-------|
| 40 | 4 | `prov_count` (`u32`) |
| 44 | `prov_count × 34` (v1) or `prov_count × 50` (v2) | Provider rows |

Each provider row is 34 bytes in a v1 file:

| Offset (within row) | Size | Field |
|--------|------|-------|
| 0 | 2 | `id` (`u16`) — the provider id referenced by every event record |
| 2 | 32 | `name` (ASCII, NUL-padded) — e.g. `tcp`, `hpts` |

In a v2 file each row is 50 bytes — the same 34-byte head plus a trailing
`schema_id`:

| Offset (within row) | Size | Field |
|--------|------|-------|
| 0 | 2 | `id` (`u16`) |
| 2 | 32 | `name` (ASCII, NUL-padded) |
| 34 | 16 | `schema_id` — SHA-256 of the provider's runtime decode blob, truncated to 16 bytes; all-zero if the provider registered no blob. |

`schema_id` is a content-addressed key into the decode-blob section below
(used by `elog -r` to render text for providers it has no compiled-in
decoder for). EventExplorer decodes every provider it knows about with
static Rust decoders rather than the runtime blob interpreter, so it
never needs to look the blob bytes up — it only has to compute the right
provider-row stride from `version` and skip the section (next) to reach
the event stream at the correct offset.

### Decode-blob section (version 2 only)

Immediately after the provider table, before the event stream:

| Field | Size | Description |
|-------|------|--------------|
| `blob_count` | 4 (`u32`) | `0` when the capture was written with `--no-embed-decode`. |
| per blob × `blob_count`: `schema_id` | 16 | Content-addressed key matching a provider row's `schema_id`. |
| per blob: `blob_size` | 4 (`u32`) | Length of the following blob in bytes. |
| per blob: `blob` | `blob_size` | Binary decode blob; begins with the ASCII magic `EDB1`. Opaque to EventExplorer. |

Blobs are deduplicated by `schema_id` — e.g. two `tcp` providers running
the identical stack binary share one entry.

Every provider manages its own event-id namespace, so `event_id = 100`
from one provider means something different from `event_id = 100` from
another. EventExplorer decodes four providers today:

- **`tcp`** — TCP stack events (congestion control, RTT, pacing, goodput,
  ACK processing). Owns the TCP event-id catalog defined below. Per-connection
  events are grouped per session; the periodic per-stack `*_SAMPLE` events
  (ids 900–903) ride on a separate per-stack "global" session and are
  collected into a `TcpSampleProfile`.
- **`hpts`** — High-Precision Timer System. Only `STARS_AGG_SAMPLE`
  (event id `16`) is consumed; it carries a machine-wide snapshot of the
  STARS adaptive egress-rate-squeeze controller (pressure gauge,
  slot-gate admit/deny counters, meter/token state, config). HPTS events
  are not associated with a session.
- **`if`** — Network-interface lifecycle, link/config/address changes, and
  periodic per-interface counter samples. One eventlog session per VNET;
  `ifindex` (carried in every payload) is the primary key. Collected into an
  `IfProfile`. See the [if provider catalog](#if-provider-event-id-catalog).
- **`cpu`** — Per-CPU kernel-thread stack samples for flame-graph
  reconstruction. One eventlog session is created per CPU at module
  load (the per-event header `session_id` field is the cpu id). Each
  `CPU_SAMPLE` record carries a leaf-first `uint64_t[]` of program
  counters. Captures produced by `elog --embed-symbols` (`-S`) also
  carry inline symbolisation metadata (`KERNEL_INFO.build_id`,
  `LINKER_FILE`, `SYMBOL_RANGE`) so `EventExplorer` / `elog_cli flame`
  can resolve frames to `name+0xINNER` without access to
  `/boot/kernel`. See the catalog below and
  [CPU symbolisation pipeline](#cpu-symbolisation-pipeline) for the
  end-to-end story.

Events from any other provider are counted under `skipped` and ignored.
Extending EventExplorer to handle a new provider is a matter of adding its
event-id catalog and routing rules — see
[`architecture.md`](architecture.md#add-support-for-a-new-provider).

## Version 2: `schema_id` and embedded decode blobs

Version 2 captures (`version == 2` in the file header) carry a
self-describing schema for each provider alongside the fixed catalogs
above, so a v2 file can in principle be decoded without any
provider-specific code, and so EventExplorer can detect when its own
hardcoded decoders have drifted from what the kernel actually emits. The
provider table and event stream framing are otherwise unchanged.

### v2 provider rows (50 bytes)

Each provider row grows by 16 bytes to carry a `schema_id`:

| Offset (within row) | Size | Field |
|--------|------|-------|
| 0 | 2 | `id` (`u16`) |
| 2 | 32 | `name` (ASCII, NUL-padded) |
| 34 | 16 | `schema_id` — `SHA256(blob)[..16]` for this provider's decode blob (see below) |

`PROVIDER_SIZE_V2` (50) vs. `PROVIDER_SIZE` (34,
version 1) in [`src/elog_parser/constants.rs`](../src/elog_parser/constants.rs).
A provider whose `schema_id` doesn't resolve to any blob in the blob table
below (or resolves to a blob whose hash doesn't match) is treated exactly
like a version-1 provider — decoded by the hardcoded catalog only, no
schema-driven behavior.

### Blob table

Immediately after the provider table, version 2 adds:

```
+----------------------------------+
| blob_count            (4 B, u32) |
+----------------------------------+
| blob 0: schema_id     (16 B)     |
|         size           (4 B, u32)|
|         blob           (size B)  |
+----------------------------------+
| blob 1: ...                      |
+----------------------------------+
```

Each `blob` is a self-contained `EDB1`-magic *decode blob* — see
[EDB1 decode-blob wire format](#edb1-decode-blob-wire-format) below.
`schema_id` content-addresses it (`SHA256(blob)[..16]`), so identical
schemas shared across providers (e.g. two "tcp" stacks with the same
struct layout) can be — and typically are — stored once and referenced by
multiple provider rows. EventExplorer verifies `schema_id ==
SHA256(blob)[..16]` before trusting anything the blob claims (mirrors the
kernel's own registrar); on a mismatch the blob is treated as unresolved
for every provider row referencing it, exactly as if it were absent.

### EDB1 decode-blob wire format

Ported from `eventlog_decode.h` / `decode.c` in the FreeBSD source tree
(see [`src/elog_parser/schema.rs`](../src/elog_parser/schema.rs)). All
integers are little-endian; strings are `u16`-length-prefixed (not
NUL-terminated). At a high level, one blob describes one provider's full
schema:

```
magic:u8[4] = "EDB1"
version:u16, word_size:u8, endian:u8, reserved:u16
name:string
nkeywords:u16, nenums:u16, nflags:u16, nstructs:u16, nevents:u16
keywords[nkeywords], enums[nenums], flags[nflags]
structs[nstructs]:
    name:string, nfields:u16
    fields[nfields]:
        name:string, type:u8, annotation:u8, annot_index:u16,
        char_len:u16, offset:u16, flags:u8,
        varlen_count_index:u16, varlen_max:u16
events[nevents]:
    id:u32, name:string, level:u8, keywords:u32,
    struct_index:u16 (1-based; 0 = no payload), format:string
```

Field `type` codes (`DECODE_TYPE_*`) cover the fixed-width integers plus
`IN_ADDR` / `IN6_ADDR` / `CHAR_ARRAY`; `annotation` (`DECODE_ANNOT_*`)
drives rendering (hex, `ntohs`, enum, flag) for the generic formatter. A
field's `flags & DECODE_FIELD_VARLEN` marks a trailing variable-length
array, sized at runtime by the field named at `varlen_count_index`. See
the doc comments in `src/elog_parser/schema.rs` for the exact byte-level
struct definitions and the `format_payload` printf-style interpreter
(supporting both direct, e.g. `%u`/`%x`, and 1-based positional, e.g.
`%1`, placeholders).

Defensive bounds (`EVENTLOG_DECODE_BLOB_MAX`, `DECODE_MAX_FIELD_WIDTH`,
`DECODE_MAX_FORMAT_FIELDS` in `constants.rs`) cap the blob size and the
format-string interpreter's work, since a v2 file's blob table is
untrusted input.

### Schema-verified decoding

At load time, once per provider with a resolved schema, EventExplorer
compares the hardcoded decoder's assumed **field names and sizes** against
the resolved schema — see `schema_validate::validate_hardcoded_decoders`
and its `TCP_EXPECTED` / `IF_EXPECTED` / `HPTS_EXPECTED` / `CPU_EXPECTED`
tables. Offset differences (packing vs C-natural alignment, or a field
reorder) are not divergence: typed ingest reads each field by schema name
at the blob's offset. Each `(provider_id, event_id)` then routes one of
three ways while parsing the event stream:

1. **Verified** — names and sizes match (offsets may differ). Decode the
   typed model using schema field offsets when a blob is present, or the
   hardcoded natural-alignment path for schema-less v1 files.
2. **Divergent** — a field the decoder reads is missing, renamed, or
   changed size. Decode generically from the schema instead
   (`ProviderSchema::format_payload`), and record a one-line reason in
   `Provider.schema_divergent_events`. `IN`/`OUT` still rebuild a typed
   payload by name so per-session plots keep working.
3. **Schema-only** — no hardcoded decoder exists for this event id at all,
   but the schema defines it. Decode generically, same as (2).

If neither a hardcoded decoder nor the schema knows the event id, it's
skipped, matching today's behavior for unknown ids.

Fields the schema has grown **beyond** what a verified hardcoded decoder
reads (e.g. a kernel appending new trailing fields to an existing event)
are safe, additive drift — they don't cause divergence, but a one-line
notice is recorded in `Provider.schema_new_fields` (computed once per
file, not per event) so a developer can see what's available to decode
without silent, permanent data loss. See the `GenericEvent` /
`SchemaCapture` types in
[`src/elog_parser/types.rs`](../src/elog_parser/types.rs) and the
`schema:` lines under `elog_cli`'s `=== File Info ===` output.

## Event stream

From the byte immediately following the last provider row to EOF. Each
event record is:

```
+----------------------------+
| Event header     (32 B)    |
+----------------------------+
| Payload          (variable)
+----------------------------+
```

### Event header (32 bytes)

`struct.pack('<HHHHQQIi', ...)` — 2+2+2+2+8+8+4+4 = 32 bytes.

| Offset | Size | Field | Notes |
|--------|------|-------|-------|
| 0 | 2 | `event_length` | Total record size in bytes, header + payload (`u16`). Must be ≥ 32. |
| 2 | 2 | `cpu` | CPU id that produced the event (`u16`). |
| 4 | 2 | `provider_id` | Matches a `provider.id` from the provider table (`u16`). |
| 6 | 2 | *reserved* | Not decoded. |
| 8 | 8 | `timestamp` | Kernel monotonic microseconds; subtract `capture_start` for seconds-since-capture (`u64`). |
| 16 | 8 | `session_id` | Per-connection id within the `tcp` provider (`u64`). The `if` provider uses it as a per-VNET session id, the `cpu` provider as the sampling cpu id, and the `tcp` periodic-sample events as a "special" session id (aggregate or per-stack global). **Only unique within a provider** — with multiple `tcp` providers, pair it with `provider_id` to disambiguate. HPTS events use this field differently. |
| 24 | 4 | `event_id` | Identifies which payload variant follows (`u32`; namespaces differ per provider). |
| 28 | 4 | `thread_id` | Kernel thread id (`i32`). |

Payload bytes span `[header + 32, header + event_length)`. The payload
layout is selected by `(provider_id, event_id)`.

## Provider announcement records

The provider table and decode-blob section are written once, when
`elog(1)` opens an output file, from the set of providers the kernel
reports at that moment. A provider that registers with the kernel's
`eventlog(9)` framework later in the capture (e.g. a TCP stack module
loaded after `elog` attached) is not in that snapshot, so it's embedded
in-band instead: the first time `elog(1)` is about to write an event from
a provider not already covered by the file's provider table or by an
earlier announcement in the same file, it writes an `EVT_PROVIDER_INFO`
record (`event_id == 0xFFFFFFFC`) and, if decode-blob embedding is
enabled and the provider's `schema_id` isn't already covered, an
`EVT_PROVIDER_BLOB` record (`event_id == 0xFFFFFFFB`) right after it.
Both ride the ordinary 32-byte event header, so a reader that doesn't
know about them can skip them exactly like any other unrecognized
`event_id` — this is why the feature needed no `version` bump.

- `EVT_PROVIDER_INFO`'s payload is one `PROVIDER_SIZE_V2`-byte (50-byte)
  row — the same layout as a provider-table entry: `id:u16`,
  `name:char[32]`, `schema_id:u8[16]`.
- `EVT_PROVIDER_BLOB`'s payload is `schema_id:u8[16]` + `size:u32` + that
  many raw `EDB1` decode-blob bytes — the same layout as one decode-blob
  section entry.

`parse_elog_core` intercepts both at the top of the main event-stream
loop, before the normal provider-kind dispatch, and applies each to the
same maps the header-time provider table / blob table populate: an
`EVT_PROVIDER_INFO` upserts the provider into `providers`/`provider_kind`
(by `provider_id`, same as a header row) so its later events route to the
right decoder family, and an `EVT_PROVIDER_BLOB` verifies
`schema_id == SHA256(blob)[..SCHEMA_ID_LEN]`, parses it, and runs the
same one-time hardcoded-decoder validation (`apply_resolved_schema`,
shared with the header-time path) for every provider id currently
referencing that `schema_id`. Neither record is counted as a captured
event (`Provider.event_count`, `parsed`/`skipped_other_providers`).

A blob too large to fit in one event's `event_length` (a `u16`, so at
most 65535 bytes total) is left out of the file entirely rather than
embedded — a real fallback to live-kernel decode by `schema_id` on
replay, same as a file written with `elog -N`. EventExplorer never needs
that fallback since it decodes providers with static Rust decoders and a
schema is only used for validation/generic-decode, but the byte layout
still reserves the same headroom the kernel-side wire format does.

Both kinds of record are idempotent per file: `elog(1)` writes each
provider at most once and each schema at most once, and a repeat sighting
of the same `provider_id` or `schema_id` (e.g. across `--roll-time`
boundaries in a *different* file) replaces the earlier one rather than
erroring — the same "repeat sighting wins" convention `elog(1)` already
uses for other identity/metadata events (`KERNEL_INFO`, `LINKER_FILE`,
`USER_FILE`, …).

## Parsing pipeline

`parse_elog_core` ([`src/elog_parser.rs`](../src/elog_parser.rs)) performs:

1. Size and magic check.
2. Header decode.
3. Provider table scan, building a `provider_id → ProviderKind` map
   (`Tcp` / `Hpts` / `Cpu` / `If` / `Ella` / `EllaRelay` / `Other`) keyed by provider name. This
   tolerates a capture that lists the same name (e.g. `tcp`) on more than
   one provider id. For `version == 2`, rows are 50 bytes and each row's
   `schema_id` is recorded for the next step.
4. **Version 2 only:** read the blob table, `ProviderSchema::parse` +
   verify each blob's `schema_id`, build a `provider_id → ProviderSchema`
   map, then run `validate_hardcoded_decoders` once per resolved provider
   to compute the per-provider divergent-event set and fill in
   `Provider.schema_*`. See
   [Schema-verified decoding](#schema-verified-decoding) above.
5. Linear scan of the event stream:
   - If `event_length < 32` or runs past EOF, stop.
   - If `event_id` is `EVT_PROVIDER_INFO` or `EVT_PROVIDER_BLOB`, apply it
     to the same maps step 3/4 built (see
     [Provider announcement records](#provider-announcement-records)
     above) and move on — not counted as parsed or skipped.
   - Skip any record whose provider kind is `Other`.
   - **Version 2 only:** if this `(provider_id, event_id)` was found
     divergent, or has no hardcoded decoder but the resolved schema
     defines it, decode generically from the schema (`GenericEvent`) and
     skip the rest of this step.
   - HPTS path: if `event_id == STARS_AGG_SAMPLE`, append a `StarsAggSample`;
     ids 0–12 (the timing-wheel scheduler trace) decode via
     `hpts_wheel::parse_hpts_wheel_payload` into `SchemaCapture.hpts_wheel_events`.
   - CPU path: route into the `CpuProfile` builder.
   - If path: route into the `IfProfile` builder (interface metadata,
     counter samples, lifecycle/link/config timeline events).
   - ELLA path: `fanout_node` / `ella_relay` events feed an `EllaProfile`
     (per-channel identity, lifecycle/subscriber events, `CHANNEL_COUNT`
     / `CHANNEL_COUNT_TOTAL`).
     They are not stored in the TCP `Sessions` map.
   - TCP path: ids 900–903 feed the `TcpSampleProfile` (per-stack gauges,
     request counts, goodput histograms, stack identity); all other ids
     dispatch on `event_id` to a per-connection payload decoder. If the
     decoder returns `EventPayload::Other` (unknown id or short payload),
     the event is counted as parsed but **not stored** in the session map.
6. Parallel per-session sort of events by `timestamp` (`rayon`).
7. Sort of the global `StarsAggSample`, `CpuProfile`, `IfProfile`,
   `TcpSampleProfile`, `EllaProfile`, and `SchemaCapture` sample lists by `timestamp`.

Progress during parsing is reported as permille (0–1000) to the GUI via an
`AtomicU32`.

## TCP provider event-id catalog

Event ids are grouped by numeric range. Ids marked **decoded** produce a
typed `EventPayload` variant and are stored on the session; ids without an
explicit decoder resolve to `Other` and are dropped.

### Connection (0–99)

| id | Name | Decoded | Notes |
|----|------|---------|-------|
| 0 | `CONN_SET_IP_V4` | yes | `src_addr`, `src_port`, `dst_addr`, `dst_port`. Ports are **big-endian** (network byte order). Current kernels emit a packed 12-byte payload; older captures pad 2 bytes after `src_port` (14 bytes). |
| 1 | `CONN_SET_IP_V6` | yes | Same fields as V4 with `in6_addr` (packed 36 bytes). Remote is shown as `[addr]:port`. |
| 2 | `LOG_ID` | yes | 64-byte NUL-padded C string. |
| 3 | `CONN_PARAMS` | no | Deprecated; replaced by `CONN_PARAMS_V2` (id 6). Older captures that emit it decode to `Other`. |
| 4 | `MSS` | no | |
| 5 | `CONN_OWNER` | yes | `pid` (`i32`) — the process that owns the connection's socket, emitted once when first known. `pid ≤ 0` means no owning userland process (kernel-internal connection). |
| 6 | `CONN_PARAMS_V2` | yes | `iss, irs, mss, snd_wscale, rcv_wscale, flags, cc_algo[16], tcp_stack[32]`. Stable per-connection parameters: negotiated MSS, window scales (peer's = `snd_wscale`, ours = `rcv_wscale`), negotiated-options bitmask (`CP_FLAG_*`: SACK_PERMIT, TIMESTAMPS, ECN, ACE, FASTOPEN, SIGNATURE, WINDOW_SCALE), and the initial cc algo / TCP stack. Emitted once at handshake completion (and again by `tcp_eventlog_dump_session` on stack switch). Used to recover the byte-valued peer RWND from `EVT_IN.win` and to light up the `Rwnd` limiter classification. **The id moved from 5 to 6 in the merged kernel schema, which inserted `CONN_OWNER` at id 5**; no released kernel emits the short-lived draft numbering. |
| 7 | `TCP_STACK_CHANGE` | yes | `tcp_stack` (32-byte NUL-terminated string) — the connection's TCP stack was swapped mid-flight. |
| 8 | `CC_ALGO_CHANGE` | yes | `cc_algo` (16-byte NUL-terminated string) — the congestion-control algorithm was swapped via `setsockopt(TCP_CONGESTION)`. |

### Data plane (100–199)

| id | Name | Decoded | Notes |
|----|------|---------|-------|
| 100 | `IN` | yes | `seq, ack, win, len, flags, state, th_off, th_sum, th_urp, rcv_time_us`. `win` is the **already-scaled** peer-advertised window in bytes (`tiwin = ntohs(th->th_win) << snd_scale`). Plot directly; do not apply `snd_wscale` again. `state`/`th_off`/`th_sum`/`th_urp`/`rcv_time_us` decode a 32-byte-or-newer payload; a legacy 18-byte payload zeroes them. |
| 101 | `OUT` | yes | `seq, ack, win, len, flags, cwnd, th_off, th_sum, th_urp`. `flags` carries `FLAG tcp_flags` (`TCP_FLAG_*`, including the log-only `TLP` and `RETRANSMIT` bits). The last three decode a 30-byte-or-newer payload; a legacy 26-byte payload zeroes them. |
| 102 | `ACK` | yes | `acked, snd_una, inflight, sacked, nsegs, rcv_time_us, num_blocks, blk1_start..blk4_end`. The SACK-block fields decode a 32-byte-or-newer payload (a plain ACK carries `num_blocks = 0`); a legacy 20-byte payload zeroes them. |
| 103 | `SACK` | yes | same `Ack` variant as 102, but the SACK-block fields decode a 68-byte-or-newer payload. |
| 104 | `USER_SEND` | yes | `error, flags, state, sndbuf_used, sndbuf_avail, send_len`. `sndbuf_used` = `sbused(&so->so_snd)` — total bytes in the kernel send socket buffer (in-flight + unsent backlog) at the time of the `send()`/`write()` call. Used as the authoritative send-buffer snapshot for the `sndbuf` plot and the App/pacing limiter heuristics. |
| 105 | `SENDFILE` | yes | `offset, length, flags`. `length` is the *requested* byte count from the `sendfile(2)` syscall arguments — it is **not** the chunk actually queued in a single iteration and can be orders of magnitude larger than the socket buffer. Not used for send-buffer accounting. |

### Congestion control / window (200–299)

| id | Name | Decoded |
|----|------|---------|
| 200 | `CWND` | yes: `cwnd, ssthresh` |
| 201 | `CC_PARAMS` | no |
| 202 | `ENTER_RECOVERY` | yes: `cwnd, ssthresh` |
| 203 | `EXIT_RECOVERY` | yes: `cwnd, ssthresh` |
| 204 | `PRR_STATE` | no |
| 205 | `WINDOW_COLLAPSE` | yes: `count, direction, inflight, has_collapsed` on a 13-byte-or-newer payload; treated as a marker-only event (fields zeroed) on older, smaller payloads. |
| 206 | `HYSTART` | no |
| 207 | `GOODPUT` | yes: `goodput, raw_bw, utim, gp_srtt, lowest_rtt`. The latter two decode a 32-byte-or-newer payload; a legacy 24-byte payload zeroes them. |
| 208 | `GP_CHECK` | yes: `elapsed_us, utim_us, now_us, start_ts_us, last_ack_us, srtt_us, th_ack, bytes, min_bytes, reason` |

### RTT (300–399)

| id | Name | Decoded |
|----|------|---------|
| 300 | `RTT` | yes: `rtt, srtt, path_rtt, lowest_rtt, gp_filter_min, gp_srtt` |
| 301 | `RTT_SAMPLE` | no |
| 302 | `RTT_UNDERFLOW` | yes: `us_rtt, minuend, subtrahend, location` |

### Loss / retransmission (400–499)

| id | Name | Decoded |
|----|------|---------|
| 400 | `RTO` | yes: `timeout_num, min_rtt, rtt, segment_len, inflight, pkts_out` on a 24-byte-or-newer payload; treated as a marker-only event (fields zeroed) on older, smaller payloads. |
| 401 | `RETRANSMIT_REASON` | no | Superseded by `TCP_FLAG_RETRANSMIT` on `EVT_OUT.flags`, which marks the retransmitting send directly. Not decoded. |
| 402 | `DSACK` | yes: `dsack_round_seen (u8), dsack_round_end (u32), num_dsack (u32), dsack_persist (u32), mod (u8), round (u32)` on a 24-byte-or-newer payload; treated as a marker-only event (fields zeroed) on older, smaller payloads. |

### Timers (500–599)

`TIMER_START`, `TIMER_CANCEL`, `TIMER_FIRED`, `SB_WAKE`. Ids known; not
decoded.

### Pacing (600–699)

| id | Name | Decoded |
|----|------|---------|
| 600 | `PACING_CALC` | yes: `slot, bw_est, method, quality, in_slow_start, cap_source (forced to NONE), inflight` |
| 601 | `PACE_STATE` | yes: `rc_pace_min_segs, rc_pace_max_segs` |
| 602 | `HW_PACE_RATE` | no |
| 603 | `PACING_CALC_V2` | yes: same as v1 but with a real `cap_source` byte |
| 604 | `STARS_PACE` | yes: `natural_bps, scaled_bps, pressure, len`. Per-connection STARS squeeze diagnostic; only emitted for connections with eventlog enabled (see `net.inet.tcp.eventlog.auto_ratio`). |

`cap_source` values (see `pacing_cap_name` in `elog_parser.rs`):

| Value | Name | Meaning |
|-------|------|---------|
| 0 | `NONE` | Not capped. |
| 1 | `STARS` | STARS software cap. |
| 2 | `BW_RATE_CAP` | BW rate cap. |
| 3 | `FIXED` | Fixed-rate cap. |
| 255 | `UNKNOWN` | Cap source not reported. |

### Configuration (700–799)

`SOCKET_OPT_UINT32`, `SOCKET_OPT_ERR`. Ids known; not decoded.

### Output (800–899)

`OUTPUT_DECISION`. Id known; not decoded.

### Periodic samples (900–999)

These are **not** per-connection events. They ride "special"
(non-per-connection) eventlog sessions flagged by the top `session_id` bit,
and are collected into a `TcpSampleProfile` rather than the per-session map:

- **`0x8000000000000000`** (`TCP_EVENTLOG_SESSION_ID_AGGREGATE`) — the
  subsystem-wide aggregate. `TCP_SAMPLE` PCB-state gauges ride here; they
  span every stack and VNET, so they belong to no single stack.
- **`0x8000000000000001`** (`TCP_EVENTLOG_SESSION_ID_STACK_GLOBAL`) — a
  stack's global session. `TCP_STACK_INFO` / `TCP_RPS_SAMPLE` /
  `TCP_GP_SAMPLE` / `TCP_SEND_SAMPLE` / `TCP_SRTT_SAMPLE` ride here.
  `TCP_LRO_FLUSH_SAMPLE` rides the aggregate session with `TCP_SAMPLE`.

> **These ids are only unique *within* a provider.** A box registers one
> `tcp` provider per loaded stack, and every one numbers its stack-global
> session identically (`0x…01`) — the base built-in `freebsd` provider and a
> loadable stack collide on the same `session_id`. Consumers must therefore
> key per-stack sample data by **`(provider_id, session_id)`**, not
> `session_id` alone, or they will merge stacks and mislabel one with the
> other's `TCP_STACK_INFO` name. EventExplorer keys `TcpSampleProfile` this
> way and labels a stack that emitted no `TCP_STACK_INFO` as
> `tcp#<provider_id>` (this happens when a stack loads *after* the collector
> subscribed, so no dump-on-subscribe replays its identity).

EventExplorer surfaces these on the **TCP Stack** GUI tab and the
`tcpsamples` CLI subcommand.

| id | Name | Decoded | Notes |
|----|------|---------|-------|
| 900 | `TCP_SAMPLE` | yes | Aggregate PCB-state gauges across every VNET at one tick: eleven contiguous host-order `uint64_t` counts, one per TCP state, in schema order — `closed`, `listen`, `syn_sent`, `syn_rcvd`, `estab`, `close_wait`, `fin_wait_1`, `closing`, `last_ack`, `fin_wait_2`, `time_wait` (88 bytes; matches `STRUCT TCP_SAMPLE` in the kernel `tcp_eventlog_schema.src`). Total PCB count = sum of all fields. |
| 901 | `TCP_GP_SAMPLE` | yes | Per-stack **cumulative** goodput histogram: `gput_n` buckets of `uint64_t`. Diff successive samples (by `(provider_id, session_id)`) for per-period bucket occupancy. |
| 902 | `TCP_STACK_INFO` | yes | Stack identity + provider-wide sample period: `stack_name` (string), `sample_period_us` (`u32`, 0 if no sampler armed). Emitted once per stack on subscriber attach and again when the period sysctl changes. Keyed by `(provider_id, session_id)` into `TcpSampleProfile::stacks`. |
| 903 | `TCP_RPS_SAMPLE` | yes | Per-stack **cumulative** request count: `req_count` (`u64`). Diff successive samples (by `(provider_id, session_id)`) to get requests/sec. |
| 906 | `TCP_SEND_SAMPLE` | yes | Per-stack **cumulative** send-size histogram (byte-weighted segments-per-burst): `nbuckets:uint8_t` (padded to 8) + `nbuckets` `uint64_t` buckets, cap 64. Diff successive samples by `(provider_id, session_id)`. |
| 907 | `TCP_LRO_FLUSH_SAMPLE` | yes | Subsystem-wide **cumulative** LRO flush-size histogram (packets coalesced per `tcp_lro_flush()`): same on-wire shape as `TCP_SEND_SAMPLE`. Rides the aggregate session. |
| 908 | `TCP_SRTT_SAMPLE` | yes | Per-stack **cumulative** smoothed-RTT histogram (usec): `nbuckets:uint8_t` (padded to 8) + `nbuckets` `uint64_t` buckets, cap 14. Bucket 0 is 0, bucket 1 is `<512us`, bucket `b >= 2` starts at `1 << (b + 7)` usec, and the last bucket is the saturated tail. Diff successive samples by `(provider_id, session_id)`. |

### HTTP request tracking (1000–1099)

`TCP_REQUEST_TRK` slot-lifecycle events for HTTP request accounting. All
carry a `slot` (`u8`) and a `track_flags` (`u32`) bitmask (`HTTP_REQ_FLG_*`:
USED, OPEN, SEQV, COMP, FSND, LSND) plus sequence/timestamp fields. Decoded
to typed `EventPayload` variants and stored on the session; survive
`retain_plot_events` and feed the GUI's HTTP Requests plot (see
[`docs/gui.md`](gui.md#overlays)) — `extract_overview_data` pairs each
`HTTP_REQ_NEW` with the next terminal event sharing its `slot` number.

| id | Name | Decoded | Notes |
|----|------|---------|-------|
| 1000 | `HTTP_REQ_NEW` | yes | A request-tracking slot was allocated: `start`, `end`, `start_seq`, `end_seq`, `timestamp`, `localtime`, `first_rcv_us`, `playout_ms`. |
| 1001 | `HTTP_REQ_FREED` | yes | A slot was freed: adds `first_send`, `last_ack_us`, `sent_at_fs`, `sent_at_ls` to the new-slot fields. |
| 1002 | `HTTP_REQ_STALE` | yes | A slot went stale: `is_oldest` (`u8`), sequence range, `localtime`, `time_delta`. |
| 1003 | `HTTP_REQ_ALLOCFAIL` | yes | Slot allocation failed: `slot`, `track_flags`, sequence range, `start`, `end`. |

### Session pseudo-events

| id | Name | Notes |
|----|------|-------|
| `0xFFFFFFFE` | `SESSION_CREATE` | Decoded to `SessionCreate { tp }`. |
| `0xFFFFFFFF` | `SESSION_END` | Not decoded; not stored. |

## CPU provider event-id catalog

Schema source: `cpu_eventlog_schema.src`
in the FreeBSD source tree. The provider creates one eventlog session
per CPU at module load; the per-event `session_id` is the sampling
cpu id.

**Split-schema captures.** Newer kernels split what used to be one
monolithic `cpu` provider into three: `cpu` (`CPU_INFO`/`CPU_SAMPLE`/
`OVERFLOW` only), `symbols` (`KERNEL_INFO`/`LINKER_FILE`/
`SYMBOL_RANGE`/`USER_FILE`/`USER_SYMBOL_RANGE`), and `process`
(`THREAD_CREATE`/`THREAD_EXIT`/`PROC_EXEC`/`PROC_EXIT`/`PROC_MAP`),
each with its own event-id namespace starting at 0. `parse_elog_core`
detects a split-schema capture (any `symbols` or `process` provider
present) and translates each provider's ids back onto the legacy
unified id-space below, so the table and the rest of this section
describe one logical event catalog regardless of which layout a given
capture uses. `KERNEL_INFO`'s `ncpus`/`hz` move to the `cpu`
provider's `CPU_INFO` (id 0) in split-schema captures — EventExplorer
merges both halves into one `CpuProfile.kernel`. Split-schema
`CPU_INFO` fields are `ncpus`, `hz`, `pmchz`, and `freq_max_mhz`
(rated max, used as the 100 % reference when scaling CPU busy by
frequency). `PROC_EXEC` (process provider id 2) fills
`CpuProfile.proc_execs` so flame group bars use the image name at
sample time; `PROC_EXIT` (id 3) clears that pid's history. When a
capture misses live `PROC_EXEC`, the bar falls back to the main
executable basename from `PROC_MAP`/`USER_FILE` (then to
`THREAD_CREATE` `comm`).

| id | Name | Decoded | Notes |
|----|------|---------|-------|
| 0 | `KERNEL_INFO` | yes | One-shot per session creation. Fields (current schema, ≥ FreeBSD r2j8): `text_base` (`u64` — `btext`), `text_size` (`u64` — `etext - btext`), `ncpus` (`u32`), `hz` (`u32`), `build_id` (`char[41]` — 40-char lowercase hex of `.note.gnu.build-id`, empty if the kernel was not linked with `--build-id`), `ident` (`char[64]` — `ostype`). The legacy 88-byte payload (no `build_id`) is still accepted for older captures. In split-schema captures `ncpus`/`hz` arrive separately on the `cpu` provider's `CPU_INFO` and `text_base`/`text_size`/`build_id`/`ident` on the `symbols` provider's `KERNEL_INFO` — see the split-schema note above. EventExplorer keeps the *first* one seen and uses `text_base` to format frames as `kernel+0xOFFSET` when no `SYMBOL_RANGE` matches. **May be absent** when the subscriber attaches after module load — the kernel re-emits via `dump_state` on subscriber attach but only if the subscriber requests `DUMP_STATE`. |
| 1 | `THREAD_CREATE` | yes | Emitted the first time the sampler observes a new tid on a given CPU's session, plus on every subscriber attach via `dump_state`. Fields: `tid` (`u32`), `pid` (`u32`), `comm` (`char[32]`), `tdname` (`char[32]`). The same tid may be emitted multiple times across CPUs; EventExplorer keeps the first occurrence in its `tid → ThreadInfo` map. |
| 2 | `THREAD_EXIT` | counted | Field: `tid` (`u32`). Currently unused — EventExplorer's flame-graph view treats the `tid → ThreadInfo` map as additive. |
| 3 | `CPU_SAMPLE` | yes | The flame-graph payload. Fields: `tid` (`u32`), `pid` (`u32`), `type` (`u8` — see below), `flags` (`u8` — see below), `depth` (`u8`), then `frames:uint64_t[depth]`. The schema caps `depth` at 64 but the kernel sampler can never fill more than `STACK_MAX` (currently `18`, defined in `sys/sys/_stack.h`) — `stack_save_intr()` is hard-bounded by that, and the kernel sets `TRUNC` when it does fill all 18 slots. Raising the practical depth means bumping `STACK_MAX`, which also affects `ddb` / `kgdb` / `procstat -kk`. The fixed head is **16 bytes**, not 11 — `STRUCT cpu_sample` carries `__aligned(8)` because the trailing array is `uint64_t`, so the head is padded from 11 to 16 before the frames begin. `frames` is in **leaf-first** order (`frames[0]` is the instruction the CPU was executing). |
| 4 | `OVERFLOW` | yes | Field: `dropped` (`u32`). Reserved for the v1 sampler; not currently emitted. EventExplorer accumulates the count into `CpuProfile.overflow_dropped` for surfacing in the flame-graph header. |
| 5 | `LINKER_FILE` | yes | One per loaded linker file (kernel + each `.ko`), emitted by the kernel on subscriber attach via `dump_state` and (in a future revision) on `MOD_LOAD`. Fields: `file_id` (`u32`), `base` (`u64` — load VA), `size` (`u64` — text size), `flags` (`u32` — `LINKED=0x01`, `KERNEL=0x02`), `build_id` (`char[41]` — populated for the kernel image; empty for modules in v1), `filename` (`char[64]` — short name, e.g. `"kernel"`, `"if_em.ko"`), `pathname` (`char[128]` — absolute on-disk path elog(1) opens to resolve symbols). EventExplorer stores these in `CpuProfile.linker_files` keyed by `file_id`. |
| 6 | `SYMBOL_RANGE` | yes | **Synthesised by `elog --embed-symbols` (the new `-S` flag)**, *not* emitted by the kernel. One event per unique `(file_id, function)` tuple referenced by any captured `CPU_SAMPLE`. Fields: `file_id` (`u32` — references a `LINKER_FILE`), `offset` (`u64` — function start, *relative to the owning `LINKER_FILE.base`*), `size` (`u32` — `st_size` from the on-disk ELF, **may be 0**), `name` (`char[96]`). EventExplorer collects them into `CpuProfile.symbols[file_id]` (sorted by `offset` at parse time) so `CpuProfile::symbolize(pc)` can binary-search. **Zero-sized entries are expanded at parse time to span up to the next symbol's offset** (or the owning `LINKER_FILE.size` for the last entry), mirroring pmcstat(8) / addr2line(1): FreeBSD's hand-written kernel asm entry points (`Xtimerint`, `fork_trampoline`, `Xfast_syscall`, `calltrap`, …) ship with `st_size == 0` because the asm macros never emit `.size` directives, and without the fill-in any PC inside their body would miss the binary search and fall back to `kernel+0xOFFSET`. v1 of `elog --embed-symbols` only synthesises symbols for the kernel image — module frames (`flags & KERNEL == 0`) need section-relocation-aware offset math that isn't reconstructible from the on-disk `.ko` alone, so they fall back to `kernel+0xOFFSET` / raw hex. |
| 7 | `USER_FILE` | yes | **Emitted by the kernel** — the process provider's `PROC_MAP` hooks (execve and `mmap(PROT_EXEC)`) resolve the backing vnode to a stable `user_file_id` via `eventlog_symbols_intern_user_file()` and the symbols provider emits `USER_FILE` for any id not seen before. One per unique backing ELF (content-addressed by `build_id` where available, so a single shared library mapped by N processes is emitted once and shared across N `PROC_MAP`s). Fields: `user_file_id` (`u32` — synthetic counter, **not stable across captures**), `build_id` (`char[41]` — 40-char lowercase hex of `.note.gnu.build-id`, empty when unavailable), `pathname` (`char[128]` — absolute on-disk path). 173 bytes on the wire (no padding). EventExplorer stores these in `CpuProfile.user_files` keyed by `user_file_id`. Expect ~50–150 entries per OCA. |
| 8 | `PROC_MAP` | yes | **Emitted by the kernel** (process provider) — one per executable file-backed `vm_map_entry`, logged from `do_execve()` (one per mapping in the fresh `vm_map`) and from `vn_mmap()` (one per later `mmap(PROT_EXEC)`); `dump_state` backfills every existing process's mappings for a subscriber that attaches mid-capture. Fields: `pid` (`u32`), `user_file_id` (`u32` — references a `USER_FILE`, 0 if unresolved), `base` (`u64` — runtime VA where the mapping begins), `size` (`u64` — mapping length in bytes), `offset` (`u64` — the mapping's `vm_map_entry::offset`, i.e. its on-disk file offset; requires no ELF parsing so it's always exact). 32 bytes on the wire (no padding). EventExplorer collects them into `CpuProfile.proc_maps[pid]` (sorted by `base` at parse time). The resolver formula is `file_offset = pc - pm.base + pm.offset`, which lands in the same coordinate space as `USER_SYMBOL_RANGE.offset` below regardless of `ET_EXEC` vs. `ET_DYN`/PIE. **Not tracked:** a region made executable via `mprotect(2)` without an accompanying `mmap()` call has no hook and won't get a `PROC_MAP`. |
| 9 | `USER_SYMBOL_RANGE` | yes | **Synthesised by `elog -U` / `libelog_user_symbols`**, *not* the kernel — walks `.symtab` (falling back to `.dynsym`) for every `USER_FILE` referenced by a `PROC_MAP`. One event per unique `(user_file_id, function)` tuple. Fields: `user_file_id` (`u32` — references a `USER_FILE`), `offset` (`u64` — the symbol's on-disk file offset, computed as `st_value - p_vaddr + p_offset` of the covering `PT_LOAD` — the same coordinate space as `PROC_MAP.offset`, *not* the raw ELF `st_value`), `size` (`u32` — raw `st_size`, **may be 0**), `name` (`char[96]`). 116 bytes on the wire — there's a 4-byte hole between `user_file_id` and `offset` because `uint64_t` requires 8-byte alignment. EventExplorer collects them into `CpuProfile.user_symbols[user_file_id]` (sorted by `offset` at parse time, with the same zero-sized-entry expansion pass as kernel `SYMBOL_RANGE` to handle pthread stubs / hand-written `memcpy` variants / ifunc resolvers that arrive with `size == 0`). Stored in file-offset coordinates, so the table is shared across processes that map the same DSO. Symbols outside every cached `PT_LOAD` are skipped by the emitter (no matching coordinate). Expect ~500–5 000 entries per file, ~25 000–100 000 across the capture. |

`SOURCE_FILE` (symbols provider id 5) and `SOURCE_LINE` (symbols provider id 6) are synthesised by `elog -L` and decoded on the symbols provider. The legacy cpu ids 5 and 6 stay `LINKER_FILE` and `SYMBOL_RANGE` in the unified catalog above. `SOURCE_FILE` is 306 packed bytes: `source_file_id` (`u32`), `binary_kind` (`u8` — 0 = linker `file_id`, 1 = user `user_file_id`), `binary_id` (`u32`), `commit_id` (`char[41]`), `pathname` (`char[256]`, the DWARF path). `SOURCE_LINE` is 21 packed bytes: `source_file_id` (`u32`), `binary_kind` (`u8`), `binary_id` (`u32`), `file_offset` (`u64`, same coordinate space as `SYMBOL_RANGE.offset` / `USER_SYMBOL_RANGE.offset`), `line` (`u32`). EventExplorer stores files in `CpuProfile.source_files` and lines in `CpuProfile.source_lines` keyed by `(binary_kind, binary_id)`. Opening annotated source for a function whose sampled offsets hit that table fetches the file from GitHub instead of a local ELF.

`CPU_SAMPLE.type` enum values (`ENUM sample_type` in the schema):

| Value | Name | Meaning |
|-------|------|---------|
| 0 | `STATCLOCK` | Sampled by the periodic callout (default v1 sampler). |
| 1 | `HWPMC` | Sampled by hwpmc; reserved for future use. |

`CPU_SAMPLE.flags` bits (`FLAG sample_flags`):

| Bit | Name | Meaning |
|-----|------|---------|
| 0x01 | `USER_MODE` | Leaf was in user mode. The kernel sampler observes user PCs at the syscall entry trampoline. EventExplorer's `CpuProfile::symbolize(pc, pid)` tries the kernel resolver first (most samples are kernel-mode) and falls back to the per-pid `PROC_MAP` lookup when that misses, so this flag is informational rather than gating — the `pid` from the sample is enough to resolve user frames whether or not it's set. |
| 0x02 | `IDLE` | Sampled thread was the per-CPU idle thread; `frames` is empty. EventExplorer drops these by default in the flame view. |
| 0x04 | `INTR` | Sampled inside an interrupt handler (`td_intr_nesting_level > 0` or ithread). The captured stack reflects the handler, not the interrupted code. |
| 0x08 | `TRUNC` | The kernel-side `stack_save_intr()` buffer was completely filled before reaching the bottom of the call stack. The buffer is `STACK_MAX` (currently 18 frames; see [`sys/sys/_stack.h`](https://github.com/freebsd/freebsd-src/blob/main/sys/sys/_stack.h)). A sample tagged TRUNC therefore has *exactly* `STACK_MAX` captured frames today and the bottom-most frame is *not* the real root of the call stack — it's wherever the unwinder happened to stop. EventExplorer drops these by default to avoid biasing aggregates toward the stop point. |

The kernel sets the per-event header `tid`/`cpu` to the sampling
*context* (the callout thread / cpu), which is unrelated to the
*sampled* thread. The sampled tid/pid/cpu are in the `CPU_SAMPLE`
payload (cpu = `session_id`); EventExplorer ignores the header
fields for cpu events.

`SESSION_CREATE` (`0xFFFFFFFE`) and `SESSION_END` (`0xFFFFFFFF`) are
emitted by the eventlog framework — one per per-CPU session — and
counted as parsed by EventExplorer but not retained.

### CPU symbolisation pipeline

Putting the per-frame symbol metadata **inside** the `.elog` capture
(rather than expecting consumers to find a matching `/boot/kernel/`
on the local box) keeps offline analysis hermetic: an EventExplorer
GUI on a developer laptop, a `flamegraph.pl` pipeline on a build
host, and `elog -r` reading the binary file all see the same names.

The end-to-end pipeline is:

1. **Capture-time:** `elog --embed-symbols` (a.k.a. `-S`, implies
   `-D`, requires `-o <file>`) subscribes to the cpu provider with
   `DUMP_STATE` so the kernel emits `KERNEL_INFO` and one
   `LINKER_FILE` per loaded linker file at attach.
2. **Inline resolution:** for each `CPU_SAMPLE` it writes, `elog`
   walks the frame array, locates each PC's owning `LINKER_FILE`,
   opens the on-disk ELF (verifying `build_id` if non-empty),
   parses `.symtab` (falling back to `.dynsym`), and synthesises
   one `SYMBOL_RANGE` event per newly-seen `(file_id, function)`
   tuple. The synthesised events are appended to the same binary
   stream so consumers receive them in-band.
3. **Consumer-side:** EventExplorer's parser collects `LINKER_FILE`
   into a `file_id → LinkerFile` map and `SYMBOL_RANGE` into a
   per-file sorted `Vec<SymbolRange>`. After sorting it expands
   any `size == 0` entry to span up to the next entry's offset
   (capped by the owning `LINKER_FILE.size` for the last entry),
   matching pmcstat(8) / addr2line(1). This is the only way to
   resolve PCs inside hand-written kernel asm entry points
   (`Xtimerint`, `fork_trampoline`, `Xfast_syscall`, `calltrap`,
   …), whose ELF `st_size` is 0 because the asm macros never emit
   `.size` directives. `CpuProfile::symbolize(pc)` then does a
   linear scan over linker files (typically a few dozen) to pick
   the owning image, then a binary search + half-open
   `[offset, offset + size)` check on its symbol vector. Misses
   fall through to `KERNEL_INFO`-relative formatting
   (`kernel+0xOFFSET`), then to raw hex.

In v1, only the **kernel** linker file gets `SYMBOL_RANGE` events.
`.ko` modules are linked with `ET_REL` semantics; mapping a runtime
PC back to a section offset in the on-disk ELF requires the
in-kernel linker's relocation result, which `elog` can't
reconstruct from the on-disk image alone. Module frames therefore
fall through the cascade as if `--embed-symbols` were not used. The
kernel-side TODO in `kern_cpu_eventlog.c` calls out walking each
linker file's `PT_NOTE` headers as the next step toward per-module
build-id reporting; per-module symbol coverage requires more
plumbing on top of that.

### User-mode symbolisation pipeline

User-space symbolisation splits across three events (`USER_FILE`,
`PROC_MAP`, `USER_SYMBOL_RANGE`) whose wire format deliberately
mirrors `LINKER_FILE` / `SYMBOL_RANGE`, but — unlike the kernel
path above — the mapping half is now emitted live by the kernel's
process provider rather than snapshotted after the fact by `elog`:

1. **Capture-time (kernel):** the process provider hooks
   `do_execve()` (one `PROC_MAP` per executable mapping in the
   fresh `vm_map`, plus the interpreter) and `vn_mmap()` (one more
   per later `mmap(PROT_EXEC)`); `dump_state` backfills every
   existing process's mappings for a subscriber that attaches
   mid-capture. Each hook resolves the backing vnode to a stable
   `user_file_id` via `eventlog_symbols_intern_user_file()`, which
   emits a `USER_FILE` on the symbols provider's session the first
   time it sees a given backing ELF (content-addressed by
   `build_id` where available, so N processes mapping the same
   library share one `USER_FILE`).
2. **Post-hoc resolution (userspace):** `elog -U` (default for
   profiling captures) reads the `USER_FILE`s emitted above,
   opens each backing ELF, parses `.symtab` (falling back to
   `.dynsym`), and for each `STT_FUNC` / `STT_GNU_IFUNC` symbol
   translates `st_value` (a runtime VA) to an on-disk file offset
   via the covering `PT_LOAD` (`st_value - p_vaddr + p_offset`),
   writing one `USER_SYMBOL_RANGE` per `(user_file_id, function)`
   tuple back into the `.elog` stream. Symbols outside every
   cached `PT_LOAD` are skipped. Offsets are stored in **file-offset**
   coordinates, not ELF-VA — this is what lets a single shared
   library mapped by N processes (each with a different load bias
   under ASLR) share one symbol table.
3. **Consumer-side:** EventExplorer's parser collects `USER_FILE`
   into a `user_file_id → UserFile` map, `PROC_MAP` into a per-pid
   `Vec<ProcMap>` sorted by `base`, and `USER_SYMBOL_RANGE` into a
   per-`user_file_id` sorted `Vec<UserSymbolRange>` with the same
   zero-sized-entry fill-in pass as kernel `SYMBOL_RANGE`.
   `CpuProfile::symbolize(pc, pid)` tries the kernel path first
   (cheap and the common case on a typical box), then on a miss —
   if `pid` is known — finds the `ProcMap` for that pid that
   covers `pc`, computes `file_offset = pc - pm.base + pm.offset`
   (both sides are `vm_map_entry`-offset coordinates, requiring no
   ELF parsing on the consumer side), and binary searches the
   corresponding `UserFile`'s symbol table. The resolved label
   format is identical to the kernel form (`name+0xINNER`); the
   GUI's "show module prefix" toggle and the CLI's `--show-module`
   flag surface the DSO basename (`libc.so.7:recvfrom+0x12`,
   `a.out:main+0x40`).

The `pid` scoping is the only structural difference from the
kernel path: kernel runtime VAs are global (the same `tcp_input`
PC means the same thing in every sample), but user runtime VAs
are per-`vm_map` (pid 100's `0x4000_0000` is unrelated to pid
200's). Flame-tree nodes therefore key on `(addr, pid)` so two
processes that happen to map their text segment at colliding
addresses can't be conflated by ASLR collisions, and the renderer
threads `pid` through every `format_pc` / `symbolize` call site.

### Annotated source prerequisites

Function rankings and stack drill-downs use the symbol metadata already
embedded in the capture. Source annotations additionally need the exact local
ELF or separate debug file because `.elog` does not embed DWARF or source
text. EventExplorer validates the captured build id, maps leaf sample PCs to
DWARF file/line records, and reads the named source file. The desktop GUI
always asks for the local executable or debug file; captured target paths are
informational and are never searched automatically. The path can be entered
directly or chosen with the in-app filesystem browser. If the DWARF source
path is unavailable locally, the GUI asks for the corresponding source file
too. A DWARF-prefix rewrite can instead map a remote build root to a local
checkout. Rewrites are scoped to the executable
image, support multiple source roots per executable, and last for the loaded
capture. When the selected executable or DWARF source path contains a `.obj`
directory, the DWARF-prefix field is pre-filled with the matching build root
before `.obj`. The local prefix can be entered directly or selected with the
directory browser. The detected DWARF prefix is populated when DWARF resolution
finishes and can also be restored with **Use detected**; an explicit **Paste**
button is available alongside the text field. Choosing a matching local source
file derives both prefixes from the files' longest shared suffix and saves that
rule automatically. The rewrite captures the build root before `.obj`, then
lexically collapses `.` and `..` components before substituting the local
prefix; the object directory does not need to exist in the local checkout.
Once a local executable/debug file is chosen, it is reused for other functions
from the same executable image for the rest of the loaded capture. Source-path,
rewrite-prefix, and filesystem-browser text fields all provide explicit paste
controls in addition to normal keyboard paste.

Limitations:

- **`mprotect`-only exec transitions aren't tracked.** `PROC_MAP` is
  only emitted from `do_execve()` and `mmap(PROT_EXEC)`; a mapping
  made executable later via `mprotect(2)` without an accompanying
  `mmap()` call has no hook and its frames fall through to raw hex.
- **Stripped binaries.** `elog -U`'s symbol walk is libelf-only —
  binaries with empty `.symtab`/`.dynsym` produce zero
  `USER_SYMBOL_RANGE`s. A future DWARF `DW_TAG_subprogram` walker
  could close this gap; until then those frames fall back to the
  `libfoo.so+0xRUNTIME_OFFSET` module-only label (EventExplorer
  detects "PC is in a known `PROC_MAP` but no symbol matched" and
  surfaces the DSO name rather than raw hex).
- **No user-side stack unwinding.** The kernel sampler stops at
  the user/kernel boundary; only the leaf user PC is captured.
  User frames in the flame view will therefore always be a single
  bar above the kernel `Xfast_syscall` / syscall-trampoline
  entry, never a `main → handle_connection → read()` chain.
  Closing this gap requires AST-hook-time capture along the
  lines of pmcstat(8), which is out of scope for v1.

## HPTS provider event-id catalog

| id | Name | Decoded | Notes |
|----|------|---------|-------|
| 16 | `STARS_AGG_SAMPLE` | yes | Fixed-width 16×`u64` payload (128 bytes), no variable-length part: `pressure` (0..1024 throttle gauge), `tokens` (`int64_t` on the kernel side, wired as `u64`, cast on read), `per_slot_bytes`, `slot_gate_budget_bytes`, `slot_bytes_peak` (since-last-sample peak, instantaneous), `slot_gate_denies` / `slot_gate_admits` / `slot_gate_denied_bytes` / `slot_gate_admitted_bytes` / `meter_obytes` / `pressure_sum` / `pressure_ticks` / `synthetic_norate_hits` (cumulative counters), `slot_gate_enable` (0 or 1), `health_limit_bps`, `min_rate_bps` (config, not counters). |

EventExplorer derives `pressure_pct` and `deny_pct` (slot-gate deny rate)
from consecutive samples' deltas for the aggregate view's primary STARS
panel; `slot_bytes_peak` / `slot_gate_budget_bytes` are plotted directly
(instantaneous gauges, not diffed) on a secondary panel. The config
fields rarely change within a capture and are surfaced as header text
rather than a time series.

Other HPTS event ids are skipped (this includes the now-dead
`NIC_RATE_SAMPLE`, id `13`, superseded by `STARS_AGG_SAMPLE` and no
longer emitted by any current kernel).

### HPTS timing-wheel scheduler trace (ids 0–12)

A second, unrelated schema shares the `hpts` provider's id namespace:
per-callout tracing of the hpts callout wheel itself (init, slot
processing, per-TCP insert/remove/move, wheel-wrap, and lateness
diagnostics), defined in `hpts_eventlog_schema.src` and decoded by
[`src/elog_parser/hpts_wheel.rs`](../src/elog_parser/hpts_wheel.rs) into
`SchemaCapture.hpts_wheel_events`. Unlike `STARS_AGG_SAMPLE`, these events
are not per-session — they trace the wheel's own scheduling loop.

| id | Name | Fields |
|----|------|--------|
| 0 | `INIT` | `num_hptss` (`u32`), `bind_type` (`u8`: 0 NONE / 1 CPU / 2 NUMA), `num_slots` (`u32`), `usecs_per_slot` (`u32`), `total_slot_time` (`u32`) |
| 1 | `INIT_HPTS_ENTRY` | none (marker) |
| 2 | `INIT_TIMING_PARAMS` | `usecs_per_slot`, `total_slot_time`, `num_slots`, `wheel_mask` (all `u32`) |
| 3 | `STATE_ACTIVE` | `from_callout` (`u8`), `cur_slot` (`u32`), `tp_cur_count` (`u32`) |
| 4 | `STATE_INACTIVE` | `slots_ran` (`u32`) |
| 5 | `SLEEP` | `sleep_time` (`u32`) |
| 6 | `CONN_INSERT` | `gencnt`, `slot`, `usecs`, `tp_cur_count` (all `u32`) |
| 7 | `CONN_REMOVE` | `gencnt`, `slot`, `tp_cur_count` (all `u32`) |
| 8 | `CONN_MOVE` | `gencnt`, `old_slot` (both `u32`) |
| 9 | `WHEEL_WRAP` | `wrap_loop_cnt`, `slots_to_run`, `prev_slot`, `cur_slot` (all `u32`) |
| 10 | `PROCESSING` | `from_callout` (`u8`), `prev_slot`, `cur_slot`, `slots_to_run`, `tp_cur_count` (all `u32`) |
| 11 | `PERF_PROCESSING` | `from_callout` (`u8`), `slots_processed`, `tps_processed`, `runtime_usec`, `loop_cnt` (all `u32`) |
| 12 | `PERF_LATENESS` | `gencnt`, `lateness_slots`, `lateness_usec`, `requested_time`, `actual_time` (all `u32`) |

Ids 13–15 (`SLOT_CALC`, `INSERT_ARC_VIOLATION`, `NO_RUN_SLOT_SKIP`) are
reserved constants referenced by `elog_cli`'s `hptsgaps` cross-check but
have **no ground truth** for their byte layout in either
`hpts_eventlog_schema.src` or any embedded schema seen so far — no
decoder exists for them, by design (see the doc comment on
`HptsWheelPayload::SlotCalc`). If a future kernel schema defines one of
these ids, it will be picked up automatically by the schema-only routing
path described above.

Surfaced via `elog_cli`'s `hptsevents` / `hptslateness` / `hptsgaps`
subcommands — see [`docs/cli.md`](cli.md).

## if provider event-id catalog

Schema source: `if_eventlog_schema.src` in the FreeBSD source tree. The
provider creates one eventlog session per VNET; the per-event `session_id`
is the VNET-scoped session. `ifindex` (carried in every payload) is the
primary key for an interface — the `ifindex → name` binding is established
by `IF_ATTACH`, mutated by `IF_RENAME`, and torn down by `IF_DETACH`.

EventExplorer collects interface metadata, periodic counter samples, and a
lifecycle/link/config timeline into an `IfProfile`, surfaced on the
**Interfaces** GUI tab and the `interfaces` / `ifsamples` / `ifevents` CLI
subcommands.

| id | Name | Decoded | Notes |
|----|------|---------|-------|
| 0 | `IF_INFO` | yes | Provider-wide: `period_us` (`u32` — sample period, 0 if no sampler armed) and `nifs` (`u16` — interface count). Last value seen wins. |
| 1 | `IF_ATTACH` | yes | `ifindex` (`u16`), `name` (`char[16]`), `if_type` (`u8`), `driver` (`char[16]`), `mtu` (`u32`), `baudrate` (`u64`), then a **variable-length** `hwaddr:uint8_t[hwaddr_len]`. See [variable-length arrays](#variable-length-trailing-arrays) below. |
| 2 | `IF_DETACH` | yes | `ifindex`, `name`. Marks the interface `detached`. |
| 3 | `IF_RENAME` | yes | `ifindex`, old + new name. |
| 4 | `IF_LINK_STATE` | yes | `ifindex`, `link_state` (`u8`: 0 UNKNOWN / 1 DOWN / 2 UP), `baudrate`. |
| 5 | `IF_FLAGS_CHANGE` | yes | `ifindex`, `flags` (`u32`). |
| 6 | `IF_MTU_CHANGE` | yes | `ifindex`, `mtu` (`u32`). |
| 7 | `IF_CAPABILITIES_CHANGE` | yes | `ifindex`, capability bitmask. |
| 8 | `IF_ADDR_ADD` | yes | `ifindex`, address family + a variable-length `addr[]`. |
| 9 | `IF_ADDR_REMOVE` | yes | As `IF_ADDR_ADD`. |
| 20 | `IF_SAMPLE` | yes | **Deprecated** — no longer emitted by the kernel, superseded by `IF_SAMPLE_BYTES`/`IF_SAMPLE_PACKETS` below. Kept so older captures still decode. Periodic per-interface counter snapshot: `ifindex` plus twelve `u64` cumulative counters — `ibytes`, `obytes`, `ipackets`, `opackets`, `ierrors`, `oerrors`, `iqdrops`, `oqdrops`, `imcasts`, `omcasts`, `noproto`. |
| 21 | `IF_SAMPLE_BYTES` | yes | Periodic per-interface byte snapshot: `ifindex`, `ibytes`, `obytes` (same link-level totals as `IF_SAMPLE`), a transport-protocol split `tcp_ibytes`/`tcp_obytes`/`udp_ibytes`/`udp_obytes`, and (newer schemas) `other_ibytes`/`other_obytes` for every non-TCP/UDP IP packet. Emitted immediately before `IF_SAMPLE_PACKETS` for the same `ifindex` at each sample tick; `core.rs` merges the pair into one `IfSample` keyed by `ifindex`. |
| 22 | `IF_SAMPLE_PACKETS` | yes | Periodic per-interface packet snapshot: `ifindex`, `ipackets`, `opackets` plus `tcp_ipackets`/`tcp_opackets`/`udp_ipackets`/`udp_opackets`, then the link-level `ierrors`, `oerrors`, `iqdrops`, `oqdrops`, `imcasts`, `omcasts`, `noproto` carried over unsplit from `IF_SAMPLE` (these occur before a packet can be classified by L4 protocol), and (newer schemas) trailing `other_ipackets`/`other_opackets`. Completes the `IfSample` record started by `IF_SAMPLE_BYTES`. |

`other_ibytes`/`other_obytes`/`other_ipackets`/`other_opackets`, when present,
are bumped at the same IP-dispatch call site as the `tcp_*`/`udp_*`
counters, so `tcp + udp + other` is self-consistent by construction. That
sum is NOT guaranteed to equal `ibytes`/`obytes`/`ipackets`/`opackets`,
though: on a multi-port aggregate interface (`lagg`) those totals are a
live sum of member-port driver counters updated asynchronously from the
IP-dispatch split, and per-tick divergence of tens of percent has been
observed even though the two reconcile over multi-second windows. On an
older schema that predates these fields, EventExplorer falls back to
deriving them per-sample as `ibytes - tcp_ibytes - udp_ibytes` (and
similarly for `obytes`/packets) via `IfSample::other_ibytes` etc — the
behavior before these fields existed, kept for older captures. On captures that only
have the deprecated `IF_SAMPLE`, `tcp_*`/`udp_*` read as 0 and `other_*`
equals the total — i.e. no protocol data available, not "all other
traffic". EventExplorer diffs successive samples per `ifindex` to derive
TX/RX rates, for both the totals and each protocol slice.

`SESSION_CREATE` / `SESSION_END` (one per VNET session) are counted as
parsed but not retained.

### Variable-length trailing arrays

Some `if`-provider events end in an array whose length is given by a
preceding fixed field (`hwaddr:uint8_t[hwaddr_len]` on `IF_ATTACH`,
`addr[]` on `IF_ADDR_*`). `eventlog_gen.awk` only forces extra padding
before the trailing array when the array's *own element type* needs
stricter alignment than 1 byte (it emits `struct __aligned(alignof(elem))`
so a cast to the element type is never misaligned) — it does not pad to
match some other, earlier field's alignment. `IF_ATTACH`'s trailing array
is `uint8_t[]` (`hwaddr`), whose alignment is 1, so no padding is forced:
the head is exactly `sizeof` of the fixed fields with ordinary
field-by-field alignment (49 bytes) and `hwaddr` begins immediately at
payload offset 49, right after `hwaddr_len`. A varlen array of a wider
element type (e.g. `uint64_t[]`) *would* pad the head up to that
element's alignment first. EventExplorer clamps `hwaddr_len` to 32 and
bounds the read by `event_length` so a truncated tail decodes to an empty
array rather than panicking.

## Notable conventions

### RTT underflow filter

The kernel's RACK stack computes RTT as an unsigned `u32` subtraction. On
secondary code paths that are missing a signedness guard, the result can
wrap to near `UINT32_MAX`. EventExplorer treats any RTT value ≥ `i32::MAX`
(`0x7FFFFFFF`, roughly 35 minutes) as an underflow artifact and excludes it
from plots and aggregates.

The `EVT_RTT_UNDERFLOW` event (id `302`) is a deliberate kernel-side
diagnostic that logs the offending operands and call site — use
`elog_cli <file> underflow` to inspect.

### Session grouping

TCP events are grouped into an `FxHashMap<u64, Vec<Event>>` keyed by
`session_id`. The map starts with estimated-but-preallocated capacity based
on `event_count`. After scanning, each session's event list is sorted by
`timestamp` in parallel — events can arrive out of order across CPUs.

### Byte-order quirks

- Ports in `CONN_SET_IP_V4` / `CONN_SET_IP_V6` are big-endian (network byte
  order); all other multi-byte integers are little-endian.
- IPv4 addresses are 4 raw bytes (`std::net::Ipv4Addr`); IPv6 addresses are
  16 raw bytes (`std::net::Ipv6Addr`).

### Decoder tolerance

Payload decoders accept records larger than the minimum they need (extra
bytes are ignored) but return `Other` for records that are too short. This
means forward-compatibility on payload size additions is implicit, while
shrinking a payload is a breaking change.

### Known kernel-side gaps

These limitations are in the event schema itself, not in EventExplorer,
and affect how derived signals are interpreted:

- **`EVT_IN.win` scaling.** Current production kernels (`tcp_input.c`,
  `ngen.c`, `bbr.c`) log `tiwin = ntohs(th->th_win) << snd_scale`
  — the peer-advertised window already scaled to bytes. Consumers
  should treat `win` as bytes and plot it directly; applying
  `snd_wscale` a second time would over-count by up to ×2^14.
  EventExplorer surfaces the `rwnd` series and the `Rwnd`-limited
  classifier only on captures that emit `EVT_CONN_PARAMS_V2` (id 5),
  because kernels predating `CONN_PARAMS_V2` may have logged the raw
  unscaled `th_win` (notably older `bbr.c` paths), making the value
  ambiguous. Suppressing both signals together is safer than risking
  a mis-scaled plot.
- **`EVT_USER_SEND.sndbuf_used` reflects total `sbused()`, not just
  unsent bytes.** Subtract `inflight` to get the send-buffer backlog
  (bytes queued but not yet on the wire). EventExplorer does this
  internally for the `sndbuf` plot and the limiter heuristics.
- `EVT_RTO`, `EVT_WINDOW_COLLAPSE`, and `EVT_DSACK` payloads smaller than
  their current schema size are decoded with the new fields zeroed and
  treated as marker-only events; no payload fields are required for the
  GUI's vertical markers or the CLI's `limiter` / `events` output.

## mem provider event-id catalog

Kernel system-memory time series (`PROVIDER mem`,
`include/eventlog/mem_eventlog_schema.src`). One global session; a
subscriber-gated sampler emits `SYS_SAMPLE` every
`kern.eventlog.mem.sample_period_us` (default 1 s). Payloads are packed
and every size is bytes. Decoded into `SchemaCapture::mem` (fields read
by schema name when the capture embeds a blob).

| id | Name | Keyword | Fields |
|----|------|---------|--------|
| 0 | `MEM_INFO` | META | `page_size` u32, `ndomains` u16, `sample_period_us` u32, `physmem`, `usable`, `swap_total` u64 — emitted on dump_state and on a period change |
| 1 | `SYS_SAMPLE` | SYSTEM | `free`, `active`, `inactive`, `laundry`, `wired`, `free_target`, `free_min`, `free_severe`, `swap_used` u64 |

Wired pages can briefly also sit on a page queue, so the five states can
sum to slightly more than `usable`.

## fanout_node provider event-id catalog

User-space Fanout node-layer provider from ELLA (`PROVIDER fanout_node`;
older captures register as `ella_relay`). One EventLog session per
channel, shared by both node sides and keyed by `(channel name, protocol
version)`. `SESSION_CREATE` fires when the name is first requested
(JOIN / JOIN_NEXT / SWITCH), before any channel id is known.
The envelope `session_id` is a private monotonic counter, not
a channel id. Session id 0 is the process-wide global session
(`INSTANCE_ATTACH`, `NODE_INFO`, `NODE_ERROR`, `CHANNEL_COUNT`,
`CHANNEL_COUNT_TOTAL`).
Older ELLA captures use C natural alignment. Newer ELLA builds
emit packed payloads; EventExplorer accepts both layouts.
Periodic occupancy samples are stored on `EllaProfile` rather than the
TCP session map.

### `fanout_node`

| id | Name | Session | Notes |
|----|------|---------|-------|
| 0 | `INSTANCE_ATTACH` | global | `instance_id`, `side` (0 UPSTREAM, 1 DOWNSTREAM). Payload 5 bytes. |
| 1 | `NODE_ERROR` | global | `code`, `instance_id`, `channel_id`, `detail`, `subscriber_port`, `channel_id_known`, `message_type`, `subscriber_address_len`, trailing binary `subscriber_address` (4-byte IPv4 or 16-byte IPv6). Packed head 18 bytes. Older `ella_relay` captures name this `RELAY_ERROR` and use the original 20-byte payload (some intermediate captures append fixed-string peer fields). |
| 2 | `NODE_INFO` | global | `role` (0 ORIGIN, 1 RELAY). Payload 1 byte. The node's own declaration of what it is; the GUI/CLI call ORIGIN the Server. |
| 50 | `SESSION_CREATE` | channel | `channel_name[64]`, `version`, `instance_id`. Packed payload 69 bytes; legacy payload 72 bytes. Opens at first JOIN, not when the id arrives. |
| 51 | `SESSION_DESTROY` | channel | Empty payload. |
| 52 | `CHANNEL_STATE` | channel | `state`, `reason`, `instance_id`, `subscribers` (live + join_next + switch_pending). Packed payload 10 bytes; legacy payload 12 bytes. |
| 53 | `CHANNEL_ID_ASSIGNED` | channel | `instance_id`, `channel_id`. Payload 8 bytes. Channel id 0 is legal. |
| 54 | `CHANNEL_ERROR` | channel | `code`, `instance_id`, `detail`. Packed payload 9 bytes; legacy payload 12 bytes. |
| 55 | `CHANNEL_DESTROYED` | channel | `reason`, `instance_id`, `subscribers` (live + join_next + switch_pending). Packed payload 9 bytes; legacy payload 12 bytes. |
| 100 | `CHANNEL_SUBSCRIBER` | channel | `state`, `reason`, `instance_id`, `subscribers`, `subscriber_port`, `subscriber_address_len`, trailing binary `subscriber_address` (4-byte IPv4 or 16-byte IPv6). Packed head 13 bytes. Legacy payload uses `subscriber_address[48]` and is 64 bytes. See `subscriber_state` below for the state numbering. |
| 101 | `SUBSCRIBER_IDENTITY` | channel | `instance_id`, `request_id`, `subscriber_port`, `subscriber_address` (`in6_addr`; IPv4 is v4-mapped and shown dotted), `pbcid_len`, trailing ASCII `pbcid`. Packed head 27 bytes. Emitted just before the SUBSCRIBING `CHANNEL_SUBSCRIBER`, only for subscribers with a PBCID. |
| 150 | `CHANNEL_COUNT` | global | `STATS_DETAILED` (removed in newer ELLA). Per Fanout instance: `instance_id`, `channels`, `channels_pending_leave`, `channels_pending_retire`, `subscribers` (live `clients` only). Payload 20 bytes. Older `ella_relay` captures append `sessions` (24 bytes). |
| 151 | `CHANNEL_COUNT_TOTAL` | global | `STATS`. Process-wide: `channels_up`, `channels_dn`, `channels_pending_leave_up`/`_dn`, `channels_pending_retire_up`/`_dn`, `subscribers_up`, `subscribers_dn`, then `objects_up`/`_dn` and the CDN counter deltas (packets, bytes, joins, switches, delivered, `new_clients`). Packed (no padding). With 64-bit `bytes_up`/`bytes_dn`/`bytes_dlvr` the payload is 104 bytes, or 116 with `clients_up`, `clients_dn` (unique clients) and `subscribers_dlvr` (relay subtree subscriptions) appended; 92 bytes with 32-bit bytes; 32 bytes before the CDN counters. Older captures are 24 bytes (leave/retire summed onto the downstream fields) or 8 bytes (channels only). Current ELLA is back to 40 bytes (gauges and objects) and moves the counters to `TRAFFIC_STATS` / `DELIVERED_STATS`. |
| 152 | `CHANNEL_SAMPLE` | channel | `STATS_CHANNEL` (newer ELLA). Per channel × instance: `instance_id`, `subscribers` (live), `subscribers_pending`, `subscribers_leaving`, `objects_completed`, `object_bytes` (object payload noted since last tick). Payload 24 bytes. |
| 155 | `TRAFFIC_STATS` | global | `STATS`. One side's traffic, right after each `CHANNEL_COUNT_TOTAL`, once per side the node has (an origin has only DOWNSTREAM): `side`, `packets`, `bytes` (u64), `joins`, `switches`, `leaves`, `expiries`, `rejects`, `new_clients` (deltas), `clients` (level). Packed payload 41 bytes. Folded into the preceding `CHANNEL_COUNT_TOTAL` sample's `_up`/`_dn` fields. |
| 156 | `DELIVERED_STATS` | global | `STATS`. Subtree delivery reported in subscriber KEEPALIVEs: `packets`, `bytes` (u64), `joins`, `switches`, `subscriptions` (level). Packed payload 24 bytes; omitted when all zero. Folded into the preceding `CHANNEL_COUNT_TOTAL` sample's `_dlvr` fields. |
| 200 | `OBJECT_STATE` | channel | `state` (0 STARTED, 1 COMPLETED, 2 ABANDONED), `instance_id`, `boundary_index` (plaintext DATA message_id, 0–3), `first_packet`, `packets`, `bytes`. Packed payload 18 bytes; legacy payload 24 bytes. `packets`/`bytes` are 0 on STARTED. |
| 250 | `FRAME_SENT` | channel | `FRAME`. One FDACE pacing frame on the origin: `instance_id`, `object_id`, `frame_index`, `first_packet`, `packets`, `is_probe`, `toggle`, `frame_len`, `planned_send_us`, `actual_send_us`, `start_lateness_us`. Packed payload 38 bytes. Current Ella emits one event at frame-end (timestamp is the send end). Older captures burst the GOP at object-free, so the timestamp is the free time. |
| 251 | `FRAME_RELAY` | channel | `FRAME`. One marker-delimited frame at a relay, emitted when the marker packet is processed: `instance_id`, `first_packet`, `packets`, `toggle`, `boundary_index`, `frame_len`, `wire_send_us`, `recv_us`, `fwd_us`, `residency_us`. Packed payload 34 bytes. No `object_id`/`frame_index` — the GUI joins frames to objects by `first_packet`. |

The Object/Frame Timelines stripe GOPs by `boundary_index` (the object slot,
rotating 0–3 per object) and shade frames by `toggle` (alternating per frame).
Both ride the wire on every event, so dropped events don't shift the colors.

`lifecycle_state`: REQUESTED, CREATED, CREATE_FAILED, CREATE_ROLLED_BACK, CREATE_TIMED_OUT, CREATE_REJECTED, RETIRE_ARMED, LEAVE_DEFERRED, CANCELLED. Teardown is `CHANNEL_DESTROYED`, not a state.

`subscriber_state`: SUBSCRIBING 0, SUBSCRIBED 1, UNSUBSCRIBING 2,
UNSUBSCRIBED 3. Newer ELLA builds insert UNSUBSCRIBING, moving the terminal
UNSUBSCRIBED from 2 to 3, so the parser resolves the value through the
blob's `subscriber_state` enum table and normalizes it onto this
numbering; a capture with no table for the field predates the insert and
reads 2 as terminal. Occupancy drops on the terminal state only —
UNSUBSCRIBING is emitted for the node's own upstream subscription, always
ahead of an UNSUBSCRIBED.

Packed-layout ELLA replaces the coarse `JOIN_REJECTED` node error with one code per
failure site: general packet errors 30–32, CAT errors 33–48, and JOIN
wrapper errors 49–50. DATA message types may carry marker/toggle bits;
EventExplorer strips those bits when naming the base DATA type.

## Reference implementation

See [`src/elog_parser.rs`](../src/elog_parser.rs) for the full parser,
including `parse_payload`, `event_name`, `pacing_cap_name`, and the
`EventPayload` enum definition.
