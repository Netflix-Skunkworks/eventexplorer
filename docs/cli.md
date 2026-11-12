# elog_cli Reference

`elog_cli` is the scriptable counterpart to the `event_explorer` GUI. It
shares the same parser and exposes two dozen focused subcommands for
inspecting, summarizing, and debugging FreeBSD EventLog captures. Most
subcommands target the `tcp` provider (plus the file-info header, which
lists every provider in the capture); `flame` targets the `cpu` provider,
`interfaces` / `ifsamples` / `ifevents` target the `if` provider,
`tcpsamples` dumps the `tcp` provider's periodic per-stack samples, `ella` /
`ellastats` / `ellaevents` dump the `fanout_node` channel-layer
provider (older captures: `ella_relay`), and
`hptsevents` / `hptslateness` / `hptsgaps` target the `hpts` provider's
timing-wheel scheduler trace (version 2 captures only — see
[file-format.md → Version 2](file-format.md#version-2-schema_id-and-embedded-decode-blobs)).
New subcommands can be added as coverage grows to other kernel EventLog
providers.

```
elog_cli <file.elog[.gz]>... [command] [args...]
```

If `command` is omitted, `summary` runs. Every command accepts either
uncompressed `.elog` or gzip-compressed `.elog.gz` files.

## Rolled capture sets

`elog --roll-time` / `--roll-size` splits a long capture into a numbered
set of files. Pass as many as you like and they are analyzed as one
continuous capture, so a window of interest that straddles a boundary
needs no stitching by hand. A single `.gz` that is several `.elog.gz`
files concatenated (`cat a.elog.gz b.elog.gz > bundle.elog.gz`) is the
same thing: each gzip member that inflates to its own capture joins the
set, and the file list names the member's gzip filename when the member
header has one.

```bash
elog_cli ~/logs/c555.eventlog-*.elog summary
```

Leading arguments are taken as paths while they name a readable file or a
`host:path` remote spec, so the command still follows them positionally
(`elog_cli a.elog b.elog ifsamples 12 --window-s 600`). Order doesn't
matter — the files are sorted onto the time axis by the timestamps of
their own events — and a repeated path is loaded once.

All timestamps are relative to the *earliest* file's capture start, and
the derived series (interface rates, CPU bins, per-session timelines) run
straight through the boundaries. The `=== File Info ===` header lists the
contributing files with the span each covers:

```
=== File Info ===
  version:         3
  merged files:    3
    c555.eventlog-2026-09-09T000000Z-00000.elog (0.000s..3599.999s, 4439630 events)
    c555.eventlog-2026-09-09T010000Z-00000.elog (3600.000s..7199.998s, 6462587 events)
    c555.eventlog-2026-09-09T020000Z-00000.elog (7200.067s..10799.999s, 4634577 events)
```

Header counters (`events`, `dropped`, …) are summed across the set.

Files must come from one `elog` run. Two checks refuse anything else
rather than splicing an unrelated file into the timeline, because event
timestamps are raw kernel-monotonic values that only mean something
within a single boot:

- **Different boots.** Each header's `start_utc_us - capture_start`
  offset maps monotonic time to wall clock; a rolled set agrees on it to
  the microsecond, and a reboot shifts it by the whole uptime. Both the
  raw and the backlog-trimmed origin are tried, so a first file that
  opened on drained ring backlog (see
  [file-format.md](file-format.md#drained-ring-backlog)) still passes.
  The refusal asks you to open each file individually.
- **Different provider numbering.** Provider ids are assigned per file
  and the sample streams key on them, so an id present in more than one
  file must name the same provider.

The provider check names the conflicting id. Open the files separately.

Note that merging holds every file's parse result in memory at once, so
peak RSS is roughly the sum of the individual loads.

## Subcommand index

| Command | Purpose |
| --- | --- |
| [`summary`](#summary-default) (default) | One row per TCP session: remote, duration, bytes acked, avg RTT, goodput, pacing rate. `--limiter` adds the cwnd / rwnd / pacing / app / unknown / idle breakdown. |
| [`limiter`](#limiter-session) | Per-session limiter breakdown only — the same classifier behind the GUI overlay. |
| [`stats`](#stats) | Event-type distribution and memory breakdown. |
| [`sizes`](#sizes) | On-disk bytes per (provider, event), with optional per-field breakdown. |
| [`memtest`](#memtest) | Peak-memory probe for a capture. |
| [`rtt`](#rtt-session) | RTT time series with every variant (srtt, path_rtt, gp_srtt, lowest, gp filter min). |
| [`rttanalysis`](#rttanalysis) | Cross-variant RTT correlation and collapsibility verdicts. |
| [`underflow`](#underflow-session) | Sessions carrying `i32::MAX`-scale RTT underflow artifacts. |
| [`goodput`](#goodput-session) | Goodput samples with expected-span ratio analysis. |
| [`ackdump`](#ackdump-session-start_s-end_s) | Raw ACK-processing dump over a time window. |
| [`gpsim`](#gpsim-session-start_s-end_s) | Replay the kernel goodput-window state machine. |
| [`gpcheck`](#gpcheck-session-start_s-end_s) | Inspect GP_CHECK decisions in a window. |
| [`events`](#events-session) | Raw decoded event stream, with `--since` / `--until`. |
| [`hexdump`](#hexdump-offset-count) | Hex dump of the file at a byte offset. |
| [`flame`](#flame-options) | `cpu` provider stack samples as a Brendan Gregg folded-stack file. |
| [`interfaces`](#interfaces) | Per-interface metadata and TX/RX byte/packet totals (`if` provider). |
| [`ifsamples`](#ifsamples-ifindex) | Periodic per-interface counter samples, with `--window-s`. |
| [`ifevents`](#ifevents) | Interface lifecycle events. |
| [`mem`](#mem) | `mem` provider context and system page-state samples. |
| [`tcpsamples`](#tcpsamples) | Periodic per-stack TCP samples, including send-size and LRO-flush histograms. |
| [`ella`](#ella) | Fanout channel sessions (`fanout_node`; older captures `ella_relay`). |
| [`ellastats`](#ellastats) | Fanout occupancy samples. |
| [`ellaevents`](#ellaevents-session) | Fanout channel lifecycle events. |
| [`nginx`](#nginx) | nginx HTTP requests: status, timing, bytes, cache status, upstream tries, abnormal-completion flags, `REQUEST_MESSAGE` lines. |
| [`nginxstats`](#nginxstats) | nginx completion Gb/s and status mix (250 bins). |
| [`nginxevents`](#nginxevents-session) | Raw nginx lifecycle and diagnostic events. |
| [`nginxconns`](#nginxconns) | nginx connections: close reason, TLS handshake, disk counters. |
| [`nginxdiag`](#nginxdiag) | nginx problem summary: TLS, cache, upstream, disk and worker stalls. |
| [`origin`](#origin-segmentscsv-morecsv---raw) | Origin video segments ground truth bitrate alignment. |
| [`hptsevents`](#hptsevents-filter) | HPTS timing-wheel scheduler trace (version 2 captures). |
| [`hptslateness`](#hptslateness) | Callout-lateness and batch-size histograms, with `--start-s` / `--end-s`. |
| [`hptsgaps`](#hptsgaps-exclude_last_secs) | PROCESSING-slot continuity cross-check. |
| [`upload`](#upload---cache-name---note-text---open) | Share a capture via the site's share server (presigned PUT) and print a shareable EventExplorer link. |

Running `elog_cli <file>` with an unrecognized command prints the binary's
own usage text, which also covers the STARS commands (`starsagg`,
`starspace`, `starsdiag`, `ratecollapse`), `httpreqs`, and `percore`.

## File-info header

Every command first prints a `=== File Info ===` block with parse statistics
before its own output:

```
=== File Info ===
  version:         1
  capture_start:   1234567890123 us
  start_utc_us:    1700000000000000 us
  events (header): 4218993
  events (parsed): 4185220
  skipped:         33773
  dropped:         0
  providers:
    id=1 name="tcp"  events=4179180 first=0.000s last=120.000s
    id=2 name="hpts" events=5044 first=0.010s last=119.800s
    id=4 name="cpu"  events=315264 first=0.000s last=120.000s
  sessions:        312
  stars_agg_samples:5044
  cpu_samples:     315264 (574 threads, 0 dropped)
  cpu_symbols:     34218 symbols across 47 linker files
  cpu_user_syms:   68024 symbols across 131 user files / 1153 proc maps across 43 pids
  cpu_kernel:      FreeBSD build_id=2bfd5a91d3c0e7af1c2a3b…
```

- `events (header)` comes from the file header; `events (parsed)` is what
  `elog_cli` actually decoded after filtering.
- `skipped` counts events from providers other than `tcp`/`hpts`/`cpu`
  (and HPTS events that weren't STARS agg samples, etc.).
- `dropped` is what the kernel reported as dropped at capture time.
- Each provider line's `first` / `last` are that provider's first and last
  event timestamps, in seconds relative to `capture_start`.
- The `cpu_samples:` line appears only when the capture has cpu-provider
  data.
- `cpu_symbols:` and `cpu_kernel:` appear only for captures produced
  by `elog --embed-symbols` (`-S`). The kernel emits `LINKER_FILE`
  events on subscriber attach (with `-D`) carrying `file_id`,
  `base`, `size`, `flags`, `build_id`, `filename` and `pathname`;
  `elog --embed-symbols` then opens each ELF and synthesises
  `SYMBOL_RANGE` events into the stream so EventExplorer can resolve
  every kernel PC offline.
- `cpu_user_syms:` appears only for captures produced by `elog -U`
  (default for profiling captures). `elog` queries `libprocstat`
  for each sampled pid's `vm_map`, emits one `USER_FILE` per unique
  backing ELF and one `PROC_MAP` per `(pid, file)`, then opens each
  ELF and synthesises `USER_SYMBOL_RANGE` events covering only the
  functions referenced by captured samples. Symbol coverage is
  per-DSO (shared across processes that map the same library), so
  the count is "unique symbols on disk", not "symbols × pids". The
  resolver is pid-scoped at lookup time. See
  [file-format.md → CPU symbolisation pipeline](file-format.md#cpu-symbolisation-pipeline)
  and [→ User-mode symbolisation pipeline](file-format.md#user-mode-symbolisation-pipeline).

On a **version 2** capture, each provider's line is followed by a schema
summary, and two more lines can appear after the provider list:

```
=== File Info ===
  version:         2
  ...
  providers:
    id=1 name="hpts" events=5044
      schema: "hpts" (15 events defined)
      schema: hardcoded decoders verified, no drift detected
    id=3 name="tcp"  events=4179180
      schema: "tcp" (49 events defined)
      schema: hardcoded decoders verified, no drift detected
  hpts_wheel_events: 812 (see `hptsevents`/`hptslateness`/`hptsgaps`)
  generic_events:  312 (schema-decoded, no hardcoded decoder)
  sessions:        312
```

If a future kernel change moves a field's offset/size, or renames/removes a
field the hardcoded decoder expects, that provider's summary switches to
something like:

```
    id=3 name="tcp"  events=4179180
      schema: "tcp" (49 events defined)
      schema DIVERGENT (generic decode used): DSACK: field 'round' not present in schema
      schema notice: IN: 1 undecoded field (future_flag)
```

- `schema:` appears only for providers with a resolved+verified embedded
  schema (`EDB1` decode blob); providers with no schema, or an unresolved
  one (hash mismatch, missing blob), print no schema lines at all and
  behave exactly like a version 1 capture.
- `schema DIVERGENT` lines mean a field the hardcoded decoder reads is
  missing, renamed, or changed size in the live schema — EventExplorer
  fell back to decoding it generically from the schema
  (`generic_events`) rather than trusting a stale assumption. Packing
  (same names and sizes, different offsets) is not divergence.
- `schema notice` lines mean the schema has grown fields beyond what the
  (still-trusted) hardcoded decoder reads — a heads-up to extend the
  decoder deliberately, not an error.
- `hpts_wheel_events:` / `generic_events:` only appear when non-zero. See
  [file-format.md → Schema-verified decoding](file-format.md#schema-verified-decoding).

## Session identifiers

Every subcommand that takes `<session>` accepts a 64-bit session id as hex,
with or without a `0x` prefix:

```bash
elog_cli capture.elog rtt 0xdeadbeef12345678
elog_cli capture.elog rtt deadbeef12345678
```

Run `elog_cli capture.elog summary` first if you don't know the session ids.

---

## `summary` (default)

Per-session rollup: one row per TCP connection in the file.

**Synopsis**

```bash
elog_cli capture.elog.gz
elog_cli capture.elog.gz summary
elog_cli capture.elog.gz summary --limiter
```

**Output**

```
=== Session Summaries (312 sessions) ===
session_id         remote                 events  duration  bytes_ack  avg_rtt  goodput  avg_pacing
0xdead...          10.0.0.1:443           12483   45.2 s    128.3 MB   18.4 ms  22.3 Mb  24.1 Mb
...
```

Columns: session id, remote `IP:port`, event count, duration (seconds), bytes
acked, average SRTT (ms), average kernel goodput (Mb/s), average pacing rate
(Mb/s). `(unknown)` appears when no `EVT_CONN_SET_IP_V4` / `EVT_CONN_SET_IP_V6`
was seen.

With `--limiter`, the RTT and pacing-rate columns are replaced with a
time-weighted breakdown of what was limiting the sender, plus a retransmit
count (`EVT_OUT` sends flagged `TCP_FLAG_RETRANSMIT`):

```
session_id  remote           events  duration  bytes_ack  goodput   %pacing  %cwnd  %rwnd  %app  %idle  retx
0xdead...   10.0.0.1:443     12483    45.2s    128.3 MB  22.3 Mb/s   62.4%   18.2%   4.7% 11.3%   3.6%   37
```

Percentages are derived from 10 ms buckets (the same classification the
GUI's "Limiter overlay" renders, and the `limiter` subcommand below
describes). `%idle` combines "true idle" (no sends, no backlog) *and*
"unknown" (no sends but backlog present) for table brevity — the `limiter`
subcommand shows them separately. `%rwnd` is reported for sessions that
have a `CONN_PARAMS_V2` event (kernels with `snd_wscale`); legacy captures
without it print `n/a` and a footer note explains why — see the
"Known kernel-side gaps" section in [`gui.md`](gui.md) or
[`file-format.md`](file-format.md).

**Use case.** Quick file triage — see what's in the capture and pick a session
to investigate. `--limiter` is the fastest way to spot which sessions were
send-rate limited vs window-limited without opening the GUI.

---

## `limiter [session]`

Per-session limiter summary and run-length-encoded timeline. Without a
`session`, one block per session is printed.

**Synopsis**

```bash
elog_cli capture.elog limiter
elog_cli capture.elog limiter 0xdead...
```

**Output**

```
=== Limiter summary for session 0xdead... ===
  cc=cubic  snd_wscale=8  rcv_wscale=14  opts=SACK+TS+ECN
  pacing=62.4%  cwnd=18.2%  rwnd=4.7%  app=8.6%  unknown=2.3%  idle=3.8%
  compact: Pacing 62% · CWND 18% · RWND 5% · App 9% · Unknown 2% · Idle 4%

  312 spans (RLE, bucket = 10ms)
     t_start_s    t_end_s     dur_s     class
    ---------------------------------------------
         0.000      0.350      0.350    pacing
         0.350      0.400      0.050    cwnd
         ...

  5 idle spans (gap > 100ms)
    total idle time: 0.410s

  markers: RTO=0  WindowCollapse=0  DSACK=2
```

The `cc=` line is printed when the session has a `CONN_PARAMS_V2`
event; older captures omit it and report `rwnd=n/a` with an
explanatory note instead.

The `compact:` line is identical to the string shown in the GUI's
per-session header.

Classifier rules (same as the GUI overlay, applied per 10 ms bucket):

- **cwnd** — `max_inflight ≥ cwnd − 2000 bytes` (roughly 1.3 × MSS)
  *and* `min_inflight ≥ cwnd × 0.75` during the bucket. Both
  conditions are required: a brief peak at cwnd in an otherwise
  unconstrained interval doesn't classify as cwnd-limited. An
  *absolute* byte margin is used rather than a percentage because
  TCP's constraint is per-segment, not per-percentage.
- **rwnd** — same dual-threshold logic as cwnd but against the
  peer-advertised receive window (`EVT_IN.win`, already bytes as
  logged by the kernel), *and* `peer_rwnd < cwnd` (the receiver
  window is the binding constraint). Only produced when
  `CONN_PARAMS_V2` is present; otherwise `%rwnd = n/a`.
- **pacing** — `PACING_CALC_V2.slot > 0` *or* `cap_source ≠ NONE`
  was seen in the bucket *and* send-buffer backlog exceeded
  1500 bytes. Also covers "burst-mitigation / ACK-clocking" —
  buckets where there is backlog and sends occur but inflight never
  reaches the cwnd/rwnd window, because the TCP stack's own spacing
  logic limits the send rate even without explicit software pacing.
- **app** — bucket had sends and backlog but hit none of the above
  constraints. Also used when a backlog that existed at the start of
  a burst drains to near-zero during the burst ("chunk boundary"
  drain). For no-send buckets: inflight is positive and draining but
  no new backlog has arrived — the application has paused queuing.
- **unknown** — bucket had *no* sends but a meaningful backlog was
  present throughout. Possible causes: kernel scheduling gap, pacing
  hold-off between bursts, brief send-queue congestion. Rendered as
  a subtle gray fill in the GUI to distinguish from true idle.
- **idle** — bucket had no sends and no meaningful backlog or inflight.
  True connection inactivity — between data bursts or at session
  boundaries. Left unshaded in the GUI; reported separately from
  `unknown` in the per-session limiter output.

Precedence when multiple signals fire in the same bucket:
`cwnd` → `rwnd` → `pacing` → `app`.

**Use case.** Script-friendly answer to "what was limiting this session?"
without running the GUI. Feeds directly into regression tests or
per-release headline numbers.

---

## `stats`

Event-type distribution and memory breakdown.

**Synopsis**

```bash
elog_cli capture.elog stats
```

**Output**

```
=== Event Type Distribution (4185220 events, 56 bytes/event) ===
type                   count          %      MB
RTT                    812443      19.4    43.4
ACK                   1822010      43.5    97.3
...
-------------------------------------------------
PLOT-ESSENTIAL        2712311      64.8   144.9
DISCARDABLE           1472909      35.2    78.7
```

Shows how the capture is composed by decoded event kind, and splits the total
into "plot-essential" (variants kept for GUI plots) vs "discardable"
(retained during parsing but dropped by `retain_plot_events` before the GUI
renders). Event types include `ConnParamsV2`, `UserSend`, and `SendFile`
alongside the older TCP events.

**Use case.** Understanding capture composition and memory footprint; sanity
check before loading into the GUI.

---

## `sizes`

On-disk size breakdown by (provider, event): which record shapes dominate the
capture file. Unlike `stats` (which measures decoded in-memory events), `sizes`
walks event record headers only — no session building — so it answers in
seconds even on multi-GB captures, and its bytes are the actual on-disk record
bytes (32-byte record header + payload), i.e. what a capture upload pays for.

**Synopsis**

```bash
elog_cli capture.elog sizes [--fields N]
```

**Output**

```
provider     event          level    keywords       count        bytes    %    avg  pay
cpu          CPU_SAMPLE     VERBOSE  KSTACK        2656708    332.1 MiB 83.0    131   99
symbols      USER_SYMBOL... INFO     SYMBOL_META    150542     20.7 MiB  5.2    144  112
...
```

Level and keyword columns come from the capture's embedded decode schema.
`--fields N` appends a per-field byte table (fixed fields exact, trailing
variable-length arrays from the payload remainder, inter-field padding called
out as `[pad]`) for the top N shapes.

**Use case.** Deciding what to shrink or stop collecting: which events (or
which fields of an event) cost the most bytes on disk, and whether a record's
cost is payload or fixed header overhead.

---

## `memtest`

Measure resident-set size (RSS) at each stage of the load pipeline. Linux
only — reads `/proc/self/status`.

**Synopsis**

```bash
elog_cli capture.elog memtest
```

**Output (stderr)**

```
After parse:    1842 MB  (4185220 events, ~234 MB of event data)
After summary:  1844 MB  ...
After retain:    728 MB  ...
```

**Use case.** Validating parser memory behavior and the impact of
`retain_plot_events` on large files.

---

## `rtt [session]`

Raw RTT event stream with all RTT variants and a min/avg/max footer.

**Synopsis**

```bash
elog_cli capture.elog rtt                    # all sessions
elog_cli capture.elog rtt 0xdeadbeef...       # one session
```

**Output**

```
=== RTT events for session 0xdead... (812 samples) ===
   time_s     rtt_us     srtt_us     path_us   lowest_us   gpmin_us   gpsrtt_us
    0.012       17234        18012       17890       14020      14100        17888
...
raw (us):  min=14020  avg=19384  max=128440
as ms:     min=14.02  avg=19.38  max=128.44
```

Only samples with `rtt > 0` are printed. Filter applies per session.

**Use case.** Spot path-vs-SRTT divergence, outliers, or unusually high
goodput-filtered minima.

---

## `rttanalysis`

Whole-file study of how SRTT, goodput SRTT, and path RTT relate.

**Synopsis**

```bash
elog_cli capture.elog rttanalysis
```

Filters to samples where `srtt`, `gp_srtt`, and `path_rtt` are all positive
and all below the RTT underflow threshold (`i32::MAX`). Produces, in order:

- An intro block explaining goal and filter rules.
- **Per-session tables** (top 20 by sample count): per-metric min / median /
  mean / p95 / max / stddev, plus pairwise mean/median/p95/max absolute and
  percentage differences and Pearson correlation for (srtt, gp_srtt),
  (srtt, path_rtt), and (gp_srtt, path_rtt).
- **Aggregate tables** pooled across all valid samples.
- **Per-session divergence distribution** — p25/p50/p75/p95/max of each
  session's mean percentage difference.
- **Per-session correlation distribution** — min/p5/p25/p50/p75 of each
  session's Pearson r.
- **Collapsibility verdict** — a heuristic "can these three metrics be
  treated as one?" result per pair, based on both aggregate and per-session
  thresholds.

**Use case.** Decide whether plots/analyses can safely conflate RTT variants,
backed by quantitative evidence from the capture.

---

## `underflow [session]`

Diagnose `EVT_RTT_UNDERFLOW` events — unsigned-arithmetic wrap-around in the
kernel's RTT code path.

**Synopsis**

```bash
elog_cli capture.elog underflow
elog_cli capture.elog underflow 0xdead...
```

**Output**

```
=== RTT_UNDERFLOW events (142 total) ===
session       hdr_ts_us     time_s  cpu  us_rtt  minuend(a)  subtra(b)  where  overshoot
0xdead...     1700...         0.42    4   ...     0xABCD      0xFFFF    loc17    0x0033
...
=== Summary by 'where' location ===
loc17   44  overshoot min/avg/max ...
...
=== Operand Analysis ===   (only when operands are non-zero)
...
=== Summary by session (top 20) ===
...
=== Totals ===
```

If non-zero operand metadata is present, an additional operand-range analysis
section is printed. A detail section can follow with the first few events
from the noisiest sessions.

**Use case.** Root-cause RTT-underflow log spam: where in the stack it
fires, how bad the unsigned wrap is, and whether logged operands explain it.

---

## `goodput [session]`

Kernel goodput samples with an expected-span ratio analysis.

**Synopsis**

```bash
elog_cli capture.elog goodput
elog_cli capture.elog goodput 0xdead...
```

**Output**

```
=== Goodput events for session 0xdead... ===
 #   time_s   gp_bw(Mbps)  raw_bw(Mbps)   utim_ms   span_ms   ratio   min_bytes
 1    0.180        22.34         25.01     33.20     30.10    1.10      128000
 2    0.410        21.80         24.90     41.00     18.50    2.22 <--   98304
```

`span_ms` is an expected active-time computed from the previous sample's
goodput and SRTT; `ratio = utim / span`. Lines with `ratio > 2` get a
trailing ` <--` marker to flag suspicious active-time accounting.

**Use case.** Check whether goodput "utim" (active time) looks consistent
with how much data the kernel claims it sent over that window.

---

## `ackdump <session> [start_s] [end_s]`

Streamed dump of ACK, Goodput, GpCheck, and Out events in a time window,
with inter-event gap tracking.

**Synopsis**

```bash
elog_cli capture.elog ackdump 0xdead...                  # full trace
elog_cli capture.elog ackdump 0xdead... 10.0 10.5        # 500 ms slice
```

- `session` is required; start and end are optional (default `0.0` and
  `f64::MAX`). If you specify start you must also specify end.

**Output**

```
time_s    type     detail                               bytes_ack  delta_ms  srtt_us  cwnd
10.012    OUT      seq=... ack=... len=1448 flags=...         0      0.00    18012   131072
10.013    ACK      acked=11584 una=... inflt=... ...         11584     1.00    18012   131072
10.015    ACK      ...                                       11584   *2.30    17980   131072
10.020    GP       ...                                        0      5.00    17980   131072
10.071  ***ACK     ...                                        2896  *51.20    17980   131072
...
ACKs in range: 184
ACK gaps > 50ms: 2
```

`*` on `delta_ms` means >20 ms; `***` prefix on the type column means
>50 ms.

**Use case.** Forensic correlation between ACK arrivals, sends, goodput
sampling, and GpCheck decisions for a specific time window (e.g. suspected
bursts or idle stalls).

---

## `gpsim <session> [start_s] [end_s]`

Replay a simplified goodput-window state machine per ACK and compare its
samples to the kernel's actual `EVT_GOODPUT` events.

**Synopsis**

```bash
elog_cli capture.elog gpsim 0xdead...
elog_cli capture.elog gpsim 0xdead... 5.0 10.0
```

- `session` is required; start and end filter the *verbose* per-event
  output to that window. The simulator's `SAMPLE #` lines print for the
  whole trace regardless.

**Output**

```
=== GP SIM for session 0xdead... t=[5.00..10.00] ===
5.012  WINDOW-OPEN   ...
5.030  IDLE-GAP      ...
5.041  SAMPLE #14    bytes=... utim=... rate=... min_bytes=... th_ack=...
...
Total samples: 214
```

Lines can be `WINDOW-OPEN`, `RE-ANCHOR`, `IDLE-GAP`, `FLUSH-SAMPLE`,
`SAMPLE-REJECT`, or `SAMPLE #N`.

**Use case.** Debug discrepancies between EventExplorer's understanding of
kernel goodput windowing and what the kernel logged — useful when adjusting
the anchoring/idle-gap heuristics.

---

## `gpcheck <session> [start_s] [end_s]`

Dump `EVT_GP_CHECK` diagnostic events in a time window with decoded
`reason` codes.

**Synopsis**

```bash
elog_cli capture.elog gpcheck 0xdead...
elog_cli capture.elog gpcheck 0xdead... 5.0 10.0
```

- `session` is required; start and end default to the full trace.

**Output**

```
=== GP_CHECK events for session 0xdead... t=[5.00..10.00] ===
time_s   reason   elapsed_ms  utim_ms   now_us       start_us      last_ack_us   srtt_us   bytes   min_bytes
5.013    TIME          12.34    0.00    1700...       1700...        1700...     18012       0      128000
5.028    BYTES         15.12    5.40    ...                                      17980   94720     98304
5.041    SAMPLE        13.00    5.40    ...                                      17980   94720     98304
...
Total: 512    OFF=104  OFF2=12  RECOV=3  TIME=181  BYTES=68  SAMPLE=144
```

**Use case.** Understand *why* the kernel decided to take (or skip) a goodput
sample at each check — which threshold dominated (time, bytes, off-period,
recovery, …) in a given window.

---

## `events [session]`

Raw decoded event stream for one or all sessions.

**Synopsis**

```bash
elog_cli capture.elog events
elog_cli capture.elog events 0xdead...
```

**Output**

```
=== Events for session 0xdead... (12483 events) ===
 0.012  cpu=4 tid=1234  RTT        Rtt { rtt: 17234, srtt: 18012, path_rtt: ..., ... }
 0.013  cpu=4 tid=1234  ACK        Ack { acked: 11584, snd_una: ..., inflight: ..., ... }
 0.047  cpu=4 tid=1234  DSACK      round=1 num_dsack=1 persist=0 (round_seen=1 round_end=414248 mod=0)
 0.052  cpu=4 tid=1234  RTO        timeout#1 rtt=210000us min_rtt=48000us seg=1448B inflight=28960B pkts_out=20
 0.055  cpu=4 tid=1234  WINDOW_COLLAPSE  1 segments, direction=COLLAPSED inflight=28960B has_collapsed=1
...
```

`RTT`, `ACK`, and other bulk variants use Rust `Debug` formatting for the
payload — exhaustive but not aligned. `DSACK`, `RTO`,
and `WINDOW_COLLAPSE` get a compact, script-friendly line instead.
`CONN_PARAMS_V2`, `USER_SEND`, and `SENDFILE` are pretty-printed with their
key fields (cc algo, window scales, flags; sndbuf_used, sndbuf_avail,
send_len; offset, length, flags respectively). Useful for `grep`/`awk`
pipelines when you need a field that no higher-level subcommand surfaces.

---

## `hexdump <offset> [count]`

Raw hex + ASCII dump of any slice of the file. `count` defaults to 64 bytes.

**Synopsis**

```bash
elog_cli capture.elog hexdump 0 128
elog_cli capture.elog hexdump 40
```

**Output**

```
=== Hex dump: offset 0x0 (0), 128 bytes ===
00000000  45 4c 4f 47 01 00 00 00  a1 b2 c3 d4 ...  |ELOG............|
00000010  ...
```

**Use case.** Forensics on a capture with an unexpected header, truncation,
or corruption; reading structures at known offsets while developing the
parser.

Note: `hexdump` re-reads the file from disk (not via mmap) so it works even
if the parser bailed early.

---

## `flame [options]`

Emit cpu-provider stack samples in **folded-stack** format on stdout —
the input format consumed by Brendan Gregg's
[`flamegraph.pl`](https://github.com/brendangregg/FlameGraph). One line
per leaf frame, with frames separated by `;` and a sample count after
the last frame:

```
kernel+0x4a91d2;kernel+0x4b2034;kernel+0x4baccc 142
kernel+0x4a91d2;kernel+0x4b2034;kernel+0x4bad7e 17
```

Lines are sorted lexically by stack so successive runs over the same
filter are diff-friendly. The synthetic root's "no-stack" samples
are emitted as a single `IDLE <count>` line. On fresh captures this
line is always absent — current kernels skip idle-thread ticks before
emitting — but archival captures can still carry empty-stack idle
samples, in which case they fold into this one line instead of
littering the graph with depth-0 towers. Pipe `| grep -v ^IDLE` if
you want to drop the line from a folded-stack file.

Frame labels follow a five-tier cascade (kernel resolver tried
first because most samples on a busy box are kernel-mode):

1. **Resolved kernel symbol** — when the capture carries
   `LINKER_FILE` plus `SYMBOL_RANGE` events (synthesised by
   `elog --embed-symbols` at capture time), the frame is rendered
   as `name+0xINNER` (or `kernel:name+0xINNER` /
   `if_em.ko:name+0xINNER` with `--show-module`).
2. **Resolved user symbol** — when the kernel resolver missed and
   the sample's pid has a `PROC_MAP` covering the PC (emitted live
   by the kernel's process provider) with a matching
   `USER_SYMBOL_RANGE` (synthesised by `elog -U`), the frame is
   rendered as `name+0xINNER` (or `libc.so.7:name+0xINNER` /
   `a.out:name+0xINNER` with `--show-module`). The DSO short name
   is the basename of the `USER_FILE` pathname.
3. **DSO module-only fallback** — when the PC is in a known
   `PROC_MAP` for the sample's pid but no `USER_SYMBOL_RANGE`
   covers the resulting file offset (e.g. a stripped binary that
   `elog -U` couldn't extract any symbols from), the frame is
   rendered as `libfoo.so+0xRUNTIME_OFFSET` (offset from the
   runtime mapping's start, not the file offset). In folded-stack
   output this still collapses identical (pid, runtime-offset)
   pairs so heavy hot spots within an unsymbolised DSO show up
   clearly.
4. **Kernel-relative** — when no symbol matches but `KERNEL_INFO`
   is present and the frame falls inside the kernel text segment,
   `kernel+0xOFFSET`. Kept on by default; pass `--raw-addrs` to
   disable this fallback.
5. **Raw hex** — otherwise the frame's `0xADDRESS`. User-mode PCs
   in pids whose `vm_map` `elog -U` didn't snapshot in time fall
   here (lazy-snapshot limitation in v1; see file-format docs).

**Synopsis**

```bash
# Default: render to SVG via flamegraph.pl. With `elog --embed-symbols`
# captures, frames render as `name+0xINNER`; otherwise as kernel+0xOFF.
elog_cli capture-cpu.elog flame \
  | flamegraph.pl --title "OCA c101 idle profile" > c101.svg

# Per-process slice
elog_cli capture-cpu.elog flame --comm nginx > nginx.folded

# Single CPU, with module prefix on every symbol so cross-module
# functions disambiguate clearly
elog_cli capture-cpu.elog flame --cpu 0 --show-module > cpu0.folded
```

**Options**

- `--cpu N[,M,...]` — restrict to listed cpu ids (decimal or `0xN` hex).
- `--pid N[,M,...]` — restrict to listed pids.
- `--tid N[,M,...]` — restrict to listed kernel-thread tids.
- `--comm S[,T,...]` — restrict to samples whose `comm` *or* `tdname`
  starts with one of the listed prefixes (matches `THREAD_CREATE`
  metadata; samples whose tid was never seen in a `THREAD_CREATE`
  are dropped).
- `--show-module` — prefix each symbolised frame with its owning
  linker file or DSO basename (e.g. `kernel:tcp_input+0x40`,
  `if_em.ko:em_isr+0x18`, `libc.so.7:recvfrom+0x12`,
  `a.out:main+0x40`). No effect on frames that fall through to
  `kernel+0xOFFSET` / raw hex. Useful when symbols collide across
  modules / DSOs; off by default to keep stacks short.
- `--by-symbol` — collapse PCs that resolve to the same function
  (kernel or user-space) into a single folded-stack leaf (classic
  `stackcollapse-perf.pl` style). **This is the default whenever
  the capture has `SYMBOL_RANGE` *or* `USER_SYMBOL_RANGE` events**
  (i.e. was made with `elog --embed-symbols` or `elog -U`); pass
  the flag explicitly only if you want a `# note:` line on stderr
  when you expected symbols and there were none, or as
  documentation in a pipeline. With `--by-symbol`, a hot function
  emits one folded leaf — `…;tcp_lro_flush <sum>` — instead of
  many adjacent per-instruction leaves. User-mode bars **merge
  across pids by default** (N forks of the same binary collapse
  into one `_start;main;…` tower), keyed on the stable
  `(UserFile::user_file_id, UserSymbolRange::offset)` identity so
  ASLR slides don't matter. Pass `--split-user-by-pid` to keep the
  legacy per-pid scope. The CLI omits the per-PC offset overlay
  that the GUI paints in the matching mode (an extra level of
  nested folded leaves doesn't render usefully in
  `flamegraph.pl`).
- `--split-user-by-pid` — keep resolved user-mode frames scoped to
  the pid that ran them (the pre-0.4 behaviour: one `_start;…`
  tower per pid). The default is the opposite — fold matching
  user-mode symbols across pids so a daemon with many worker forks
  reads as one flame graph. Effective only with `--by-symbol`; the
  `--by-pc` output is always pid-scoped for user frames because
  raw PCs can't be safely merged across address spaces. Unresolved
  PCs always fall back to being keyed by their raw PC (pid-scoped
  for user frames) either way.
- `--group-by-process` — prefix every folded stack with the process
  `comm` (or `kernel;<thread comm>` for kernel threads: idle,
  ithreads, clock, pid 0). Same-`comm` pids share one process
  prefix, so `nginx` workers collapse together. Off by default so
  the output stays `stackcollapse-perf.pl` shaped; the GUI has this
  on by default (gear-menu **Group by process**).
- `--by-pc` — opt out of `--by-symbol` and emit one folded-stack
  leaf per unique PC, even on captures that have symbols. Useful
  when you specifically want to spot tight loops as multiple
  adjacent leaves of the same function (`…;tcp_lro_flush+0x25 142`
  / `…;tcp_lro_flush+0x40 17` / …) rather than a single collapsed
  bar. On captures without symbols this flag is a no-op (per-PC is
  the only aggregation possible).
- `--raw-addrs` — disable the `kernel+0xOFFSET` fallback. Symbols
  still render normally; unsymbolised frames print as raw `0xADDR`.
  Useful when piping into a tool that already does its own
  symbolisation.
- `--rel-text` — accepted as a no-op alias for backward compat
  (kernel-relative formatting is now the default; pass `--raw-addrs`
  to opt out).

**Use case.** Drive standard flame-graph tooling
(`flamegraph.pl`, `stackcollapse-*`, `inferno-flamegraph`,
[Speedscope](https://www.speedscope.app/) — accepts folded format
via `--paste`) against profiling captures (`cpu` provider with
stack sampling on). For interactive exploration without a script
pipeline, use the GUI's `🔥 Flame` tab — same data, with click-to-
zoom and search built in.

A short summary line is printed on stderr —
`# N samples after filter (M included as IDLE) — K kernel syms / L linker files — U user syms / F user files / P proc maps — aggregated by symbol`
— so callers can sanity-check that filters aren't accidentally
rejecting everything *and* that both kernel and user-space symbol
metadata was actually captured. The line picks up
`aggregated by symbol` when `--by-symbol` had something to collapse
on. When `LINKER_FILE` events are present but `SYMBOL_RANGE` events
are not (capture was made without `--embed-symbols`), a second line
suggests re-capturing with `-S`; analogously for `USER_FILE` without
`USER_SYMBOL_RANGE`, a line suggests re-capturing with `-U`.

---

## `interfaces`

Lists every interface seen by the `if` provider, keyed by `ifindex`, with
the latest metadata (driver, MTU, link baud rate, link state, hardware
address, attach/detach state). The header reports the provider-wide sample
period and interface count from `IF_INFO`.

```
=== Network Interfaces (8, sample period 10000000 us, nifs=8) ===
 ifidx  name         driver        mtu       baudrate link     hwaddr             state
--------------------------------------------------------------------------------------------
     1  igb0         igb          1500     1000000000 UP       a0:42:3f:51:71:e9  attached
     6  mce0         mce          1500   100000000000 UP       ...                attached
     8  lagg0        lagg         1500   200000000000 UP       ...                attached
```

## `ifsamples [ifindex]`

Dumps periodic per-interface counter samples, deriving per-period rates
(the kernel emits **cumulative** counters, so consecutive samples are
diffed). With no argument, every interface is dumped; pass an `ifindex` to
restrict to one. Columns: `t(s)`, `rx_Bps`, `tx_Bps`, `tcp_rx_Bps`,
`tcp_tx_Bps`, `udp_rx_Bps`, `udp_tx_Bps`, `other_rx_Bps`, `other_tx_Bps`,
`rx_pps`, `tx_pps`, `ierrors`, `idrops`.

`rx_Bps`/`tx_Bps` are link-level totals; `tcp_*`/`udp_*`/`other_*` break
them down by transport protocol (see
[file-format.md](file-format.md#if-provider-event-id-catalog)).
`other_*` covers every non-TCP/UDP packet (ICMP, etc). On a schema new
enough to carry it (appended `other_*` fields), it's a direct kernel counter bumped at
the same IP-dispatch call site as `tcp_*`/`udp_*` — NOT `rx_Bps -
tcp_rx_Bps - udp_rx_Bps`. `tcp + udp + other` is self-consistent by
construction, but on a multi-port aggregate interface (`lagg`) it is
**not** guaranteed to sum to `rx_Bps`/`tx_Bps`, which are a live sum of
member-port driver counters on their own asynchronous update schedule —
see file-format.md for detail. On an older schema, `other_*` falls back
to that subtraction. Captures with only the deprecated `IF_SAMPLE` event
have no protocol data at all, so `tcp_*`/`udp_*` read 0 and `other_*`
equals the total.

```
=== IF_SAMPLE for lagg0 (ifindex 8, 360 samples) ===
      t(s)          rx_Bps         tx_Bps     tcp_rx_Bps     tcp_tx_Bps     udp_rx_Bps     udp_tx_Bps   other_rx_Bps   other_tx_Bps       rx_pps       tx_pps    ierrors     idrops
------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------
    20.000       155172177    16736625932      154998421    16735021177         120404          31840          53352        1572915      1703205     11213843          0          0
```

## `ifevents`

Dumps the `if` provider's discrete lifecycle / link / config / address
timeline (attach, detach, rename, link-state, flags, MTU, capabilities, and
address add/remove), one line per change with its relative timestamp.

```
=== if provider event timeline (16 events) ===
     0.000s  IF_ATTACH              ifidx=8    lagg0      attach lagg0 type=6 mtu=1500 baudrate=200000000000
     0.000s  IF_LINK_STATE          ifidx=8    lagg0      link UP baudrate=200000000000
```

## `mem`

Dumps the `mem` provider: each `MEM_INFO` (page size, NUMA domains,
sample period, physmem / usable / swap total), then one row per
`SYS_SAMPLE` with every page state, the free thresholds, and swap in
use, all in GiB.

```
MEM_INFO t=0.000s page_size=16384 domains=2 period_us=1000000 physmem=256.00GiB usable=250.00GiB swap=8.00GiB
      t(s)       free  inactive   laundry    active     wired     target       min    severe  swap_used
     0.000     30.134   117.866     2.000    60.000    40.000      8.000     3.000     1.500      0.000
```

## `tcpsamples`

Dumps the `tcp` provider's periodic per-stack aggregate samples. A box
registers one `tcp` provider per loaded TCP stack, and every stack numbers
its "global" session identically, so samples are demuxed by
`(provider_id, session_id)` — a bare `session_id` collides across stacks.
Output sections: a stack-identity table (from `TCP_STACK_INFO`), PCB-state
gauges (`TCP_SAMPLE`), requests/sec derived from the cumulative
`TCP_RPS_SAMPLE` counter, and — when present — the goodput histogram
(`TCP_GP_SAMPLE`), send-size histogram (`TCP_SEND_SAMPLE`), smoothed
RTT histogram (`TCP_SRTT_SAMPLE`), and LRO flush-size histogram
(`TCP_LRO_FLUSH_SAMPLE`).

```
=== TCP stacks (1) ===
  provider=3   session=0x8000000000000001  stack=freebsd          sample_period_us=100000

=== TCP_SAMPLE state gauges (35633 samples) ===
      t(s)  stack           closed  listen syn_sent syn_rcvd   estab  close_w  fin_w1 closing last_ack  fin_w2  time_w     total
     0.102  (all)                3     693        0        8    7069       10      17       5        9     228     952      8994
     0.202  (all)                3     693        0       12    7066       10      17       4        8     229     956      8998

=== TCP_RPS_SAMPLE (requests/sec, 71266 samples) ===
      t(s)  stack              cum_requests        req/s
     0.102  freebsd                       0            -
     0.102  tcp#7                   1733951            -
     0.202  freebsd                       0          0.0
     0.202  tcp#7                   1734061       1090.3

=== TCP_GP_SAMPLE goodput histogram (36532 samples) ===
     0.001s  tcp#7          [239327, 59637, 187455, 607434, 639062, 522862, 319128, 110842, 9857, 2781, 975, 190, 0, 0, 92, 15640]
```

The state-gauge columns list all eleven PCB states in canonical TCP FSM
order (`closed` … `time_wait`), so `total` reconciles as their sum.
`TCP_SAMPLE` gauges are the subsystem-wide aggregate across every stack and
VNET, so they ride the aggregate session rather than any one stack — they're
labelled `(all)`.

Per-stack rows are labelled by the stack's `TCP_STACK_INFO` name, or
`tcp#<provider>` when the stack emitted no `TCP_STACK_INFO` (it was loaded
after the collector subscribed, so no dump-on-subscribe replayed its
identity). Above, the base built-in `freebsd` stack carries no request
traffic while the loadable stack on provider 7 (`tcp#7`) does — the two
would be indistinguishable if keyed by `session_id` alone, since both use
`0x8000000000000001`. `req/s` is diffed against the last monotonic sample of
the *same* stack (and shows `-` when a gap is too small to yield a
meaningful rate).

## `ella`

Lists ELLA channel sessions seen in the capture (`fanout_node` /
`ella_relay`), grouped
by channel name. One row per title (reincarnated sessions fold together)
with **node** (Server / Relay), short channel name, full channel name,
event count, `CHANNEL_ERROR` count, peak reconstructed occupancy,
typical object rate (time-weighted mode of object Mbps: TX on Server,
RX otherwise; `-` when the matching `OBJECT_STATE` was absent), end state (`DESTROYED` after
`CHANNEL_DESTROYED`), compact channel id, instance count, and duration.
A title carried by both nodes is one row reading `Server + Relay`, with
each column summed or peaked across them.

```
=== ELLA channels (1 titles, 1 sessions) ===
node            channel            name                                              dur_s   peak       rate state            ids           inst errors     events
Relay           1                  1                                                 0.110      1          - DESTROYED        7               1      0          8
```

## `ellastats`

Dumps occupancy samples. `CHANNEL_COUNT` is per Fanout instance
(channels, live clients, pending leave/retire; older captures also
print live sessions). `CHANNEL_COUNT_TOTAL` is the process-wide
up/dn occupancy (leave/retire per side; older captures print the
summed pair on the downstream columns). Each line is prefixed with
the node (`server` / `relay` / `fanout_node#<id>`). JOIN_NEXT
subscribers appear on `CHANNEL_COUNT` after the join boundary;
per-channel `CHANNEL_SUBSCRIBER` counts include pending join
immediately.

## `ellaevents [session]`

Dumps ELLA lifecycle/subscriber/object events, optionally filtered to one envelope `session_id`. Periodic
`CHANNEL_COUNT` / `CHANNEL_COUNT_TOTAL` are not included — use `ellastats`
for those. Pass `0` to see global-session events (`INSTANCE_ATTACH`,
`NODE_INFO`, `NODE_ERROR`) from every `fanout_node` provider; each line
is prefixed with the node. New `NODE_ERROR` records print their distinct
CAT/JOIN failure code, message type, and binary-decoded peer address when
those fields apply.

## `nginx`

One row per nginx request (`REQUEST_START` … `REQUEST_DONE`), keyed by
worker pid, TCP session and request number. Columns: start, session,
request, method, status, duration, TTFB, bytes, `cache` (proxy-cache
status), `tries` (upstream attempts), `flags` (abnormal `REQUEST_DONE`
flags such as `TIMEDOUT`, `CLIENT_ABORT`, `WRITE_ERROR`; an
`err_status` is appended when set) and URI. Each `REQUEST_MESSAGE`
for the request prints on its own `message:` line under the row.
Cache/tries/flags are `-` on captures from firmware older than the
nginx diagnostic events.

## `nginxstats`

Completion throughput (Gb/s of `REQUEST_DONE.bytes_sent`) and status
mix in 250 bins over the capture.

## `nginxevents [session]`

Every decoded nginx event in time order, optionally limited to one TCP
session. Besides the request lifecycle this covers `CONN_START` /
`CONN_END`, `TLS_HANDSHAKE_DONE` / `TLS_HANDSHAKE_FAIL`,
`CACHE_LOOKUP`, `UPSTREAM_ATTEMPT_START` / `UPSTREAM_ATTEMPT_END`,
`REQUEST_MESSAGE`, `SENDFILE_BUSY_DEFER`, `SENDFILE_ERROR`,
`FILE_OPEN_FAIL`, `WORKER_STALL` and `AIO_SYNC_FALLBACK`. Worker-scoped
events (stalls, aio fallbacks) carry the special session id
`0x8000000000000000 | tid`.

```
  0.000800  0xaaaa  TLS_HANDSHAKE_DONE  TLSv1.3 TLS_AES_128_GCM_SHA256 alpn=http/1.1 flags=REUSED|KTLS_TX handshake=7ms
  0.001300  0xaaaa  UPSTREAM_ATTEMPT_END  request=1 attempt=1 ft=TIMEOUT_CONN status=0 connect=-1ms header=-1ms elapsed=3000ms rx=0 subrequest=false
  0.003000  0xaaaa  CONN_END  reason=CLIENT_CLOSED requests=2 bytes_sent=2000 duration=1500ms
```

## `nginxconns`

One row per nginx connection seen via `CONN_START`, `CONN_END` or a
TLS event: port, close reason, request count, bytes, duration, sendfile
EBUSY deferrals, sendfile errors, and the TLS outcome (version, cipher,
ALPN, handshake ms and `REUSED` / `KTLS_TX` / `KTLS_RX`, or the
handshake failure kind and errno).

## `nginxdiag`

Capture-wide nginx problem summary: close-reason mix, TLS handshake
counts / latency percentiles / version mix / failure kinds, abnormal
request completions, cache status mix and lock waits, upstream attempt
failures (`ft` reasons), `REQUEST_MESSAGE` text counts, sendfile EBUSY
deferrals and errors, file-open failures, blocking aio reads, and
every `WORKER_STALL` with its source location.

---

## `origin <segments.csv> [more.csv ...] [--raw]`

Aligns one or more origin video segments CSV files (recording segment
publish timestamps and byte sizes for `enc1` and `enc2`) to the capture's
UTC anchor (`start_utc_us`). Rows outside the capture are skipped. A file
that only partly overlaps still contributes the rows that fall inside.

**Synopsis**

```bash
elog_cli capture.elog origin origin-segments.csv
elog_cli capture.elog origin day1.csv day2.csv --raw
```

**Output fields**

- `Total segments` — total segment rows in the CSV.
- `Capture duration` — capture span in seconds.
- `Overlap segments` — segments falling within the capture time window.
- `Nominal duration` — inferred segment media duration (e.g. 2.0s).
- `Enc1 / Enc2 bitrate` — average, min, and max bitrate in Mbps across the overlapping window.
- `--raw` dumps one line per segment with `t_s`, `enc1_mbps`, `enc2_mbps`, and `size1_bytes`.

---

## `hptsevents [filter]`

Dumps the `hpts` provider's timing-wheel scheduler trace (ids 0–12 — a
separate schema from `STARS_AGG_SAMPLE`; see
[file-format.md → HPTS timing-wheel scheduler trace](file-format.md#hpts-timing-wheel-scheduler-trace-ids-012)).
**Version 2 captures only** — the trace has no analogue in version 1.

**Synopsis**

```bash
elog_cli capture.elog hptsevents
elog_cli capture.elog hptsevents PERF_LATENESS   # one event type, case-insensitive
```

**Output**

```
=== hpts timing-wheel scheduler trace (812 events) ===
      t(s)  cpu             session_id event            detail
----------------------------------------------------------------------------------------------------
     0.000  4                 0x1       INIT             num_hptss=4 bind_type=CPU num_slots=4096 usecs_per_slot=100 total_slot_time=409600
     0.012  4                 0x1       CONN_INSERT      gencnt=1 slot=12 usecs=1200 tp_cur_count=1
     1.412  4                 0x1       PERF_LATENESS    gencnt=555 lateness_slots=3 lateness_usec=250 requested_time=1000 actual_time=1250
...
```

**Use case.** Raw trace inspection — grep/awk pipelines, or spot-checking
a specific wheel instance's behavior around a known timestamp.

---

## `hptslateness`

Summarizes `PERF_LATENESS` events — how late the hpts callout wheel fired
relative to its requested slot — the primary "hpts delay" diagnostic.

**Synopsis**

```bash
elog_cli capture.elog hptslateness
```

**Output**

```
=== hpts PERF_LATENESS summary (812 events) ===
  on-time (0 usec): 640 (78.8%)
  mean:          142.3 usec
  p50:             0.0 usec
  p90:           310.0 usec
  p99:          1840.0 usec
  p99.9:        4200.0 usec
  max:          8100.0 usec

=== by hpts instance (cpu, session_id) -- 4 instances ===
   cpu         session_id       count      mean_us       p99_us       max_us
--------------------------------------------------------------------------------
     4                0x1         203        180.2       2100.0       8100.0
...

=== worst 20 PERF_LATENESS events ===
      t(s)     cpu         session_id       slots         usec
----------------------------------------------------------------------
     3.412       4                0x1           4          8100
...
```

The "by hpts instance" section only prints when the capture has more than
one distinct `(cpu, session_id)` wheel instance.

**Use case.** Quantify hpts callout scheduling jitter/delay and identify
whether it's uniform or concentrated on a specific CPU/instance.

---

## `hptsgaps [exclude_last_secs]`

Cross-checks the `PROCESSING` (id 10) event stream for silently-skipped
wheel slots: for consecutive `PROCESSING` runs on the same wheel instance,
does the next run's `prev_slot` exactly equal the previous run's
`cur_slot`? A mismatch means slots were never covered by any processing
run. Cross-references each mismatch against `NO_RUN_SLOT_SKIP` /
`INSERT_ARC_VIOLATION` events on the same session to help attribute the
cause (both are reserved ids with no current decoder — see
[file-format.md](file-format.md#hpts-timing-wheel-scheduler-trace-ids-012)
— so this cross-reference currently always reports 0 matches until a
kernel schema defines them).

**Synopsis**

```bash
elog_cli capture.elog hptsgaps
elog_cli capture.elog hptsgaps 5.0     # ignore the last 5s (in-flight wheel at EOF)
```

**Output**

```
=== PROCESSING slot continuity (4218 PROCESSING events) ===
  grouped by session_id (hpts instance): 3/4214 consecutive pairs do NOT line up (next.prev_slot != prev.cur_slot)
  grouped by cpu:                        3/4214 consecutive pairs do NOT line up

  0 match a NO_RUN_SLOT_SKIP event on the same session in the same window (the known insert-race fast-path bug)
  2 are small (<=3 slots) but unmatched (no NO_RUN_SLOT_SKIP recorded -- likely the same bug on a code path not covered by that instrumentation, or a benign 1-slot boundary race)
  1 are large (>3 slots) and unmatched -- see the burst below

=== gaps per second of capture ===
  t=   3s: 2
  t=   7s: 1

=== continuity gaps (grouped by session_id), worst first ===
        session_id   t_before    t_after   cpuB   cpuA     cur(A)    prev(B)  gap_slots  no_run?
--------------------------------------------------------------------------------------------------------------
               0x1      3.412      3.512      4      4         12         20          8       no
...
```

If every consecutive pair lines up, prints a one-line confirmation and
exits early instead of the full report.

**Use case.** Root-cause "hpts is dropping work" hypotheses by directly
measuring slot continuity, independent of whether the kernel's own
skip-detection instrumentation happened to be armed for the code path
that caused it.

---

## `site show` / `site install <file|url>`

Run without a capture: `elog_cli site show` prints the site files in
lookup order and the effective value and source of every key.
`elog_cli site install <file|url>` validates a site file and writes it
as the user config (`<config dir>/event_explorer/site.toml`). See
[site-config.md](site-config.md).

## `upload [--cache NAME] [--note TEXT] [--open]`

Shares a local capture by uploading it to the share server named by
`uploads_url` in the [site config](site-config.md) and printing a link
anyone with access to that server can open in the hosted
EventExplorer. The capture is **not parsed** — the command checks the
magic, asks the server who you are (`GET <base>/whoami`), has it sign
an upload, and PUTs the bytes straight to object storage on a
presigned URL (the server never proxies them). Retention and deletion
are up to the server. One upload is capped at 2 GiB. With no
`uploads_url` the command fails with "no upload server is configured".

API requests run through the site's `http_command` (default
`curl -sS`), which is where a site adds authentication; the server's
`whoami` answer is what puts your username on the upload. The PUT
itself is plain `curl`. The cache name defaults to the
appliance-style prefix of the file name
(`c101.lab001.dev-eventlog-…` → `c101.lab001.dev`); pass `--cache`
when the name doesn't lead with one. Given several files (a rolled
set), each is uploaded in turn and one link loads the whole set.

**Synopsis**

```bash
elog_cli c101.lab001.dev-20260918-085841-nginx.elog upload \
    --note "stall repro"
elog_cli capture-*.elog.gz upload --cache c102.lab002.dev --open
```

**Output**

```
Uploading c101.lab001.dev-20260918-085841-nginx.elog (128.6 KiB) for c101.lab001.dev…
Uploaded as alice@example.com:
  https://share.example.com/api/uploads/20261018-214836-3aece328/download#c101.lab001.dev-20260918-085841-nginx.elog

Open in EventExplorer:
  https://share.example.com/eventexplorer/?file=https%3A%2F%2F…
```

`--open` additionally opens the EventExplorer link in a browser
(`open` on macOS, `xdg-open` elsewhere). The `download` URL 307s to a
freshly-signed GET on every fetch, so both links live as long as
the upload itself; the `#name` fragment is a display hint for the
viewer, never sent to the server. The GUI's 🔗 Share dialog is the
same flow with the view state (tab, session, time window, toggles)
folded into the link — see [gui.md → Sharing a view](gui.md#sharing-a-view).

---

## Exit codes

- `0` on success.
- `1` when:
  - The command line is missing the file argument.
  - The file can't be opened or parsed.
  - A subcommand requires `<session>` and it wasn't supplied.
  - An unknown subcommand was given.

See also: [`gui.md`](gui.md) for the interactive GUI, and
[`file-format.md`](file-format.md) for the on-disk layout of `.elog` files.
