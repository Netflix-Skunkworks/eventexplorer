# Changelog

All notable changes to EventExplorer are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.6.3] - 2026-11-12

### Added

- Pacing views work on default captures where the kernel no longer
  collects `PACING_CALC` (current firmware logs it only at TRACE).
  The pacing-rate line, the session-list Pacing column, and the
  Overview paced/unpaced split fall back to the new `GOODPUT.pacing_bw`
  field. The limiter's pacing gate falls back to the new log-only
  `PACED` (0x2000) flag on `OUT`. Captures that carry `PACING_CALC`
  keep using it, and older captures without the new field or flag
  decode as before.
- A `.gz` that concatenates several compressed `.elog` files (one
  capture per gzip member) opens as one rolled set, in the GUI, the
  CLI, and the browser build. The trace's file list names each member
  from its gzip filename when the member header carries one.
- Share captures by upload to a site-configured share server. The
  tab-bar 🔗 button now works on desktop too: when the loaded capture
  has no URL (any desktop load, or the browser's file picker), the
  Share dialog uploads it (presigned object-store PUT, bytes never
  proxied through the server) and builds a view-state
  permalink on the upload's long-lived download URL. Desktop copies
  the link straight to the clipboard; the web overlay swaps to it
  and updates the page URL in place. New `elog_cli upload
  [--cache NAME] [--note TEXT] [--open]` subcommand for the same
  flow from scripts and remote hosts. Upload, symbol-catalog and
  firmware-repo endpoints come from the site config
  ([docs/site-config.md](docs/site-config.md)): `ELOG_*` environment
  variables or a `site.toml` (user config, next to the binary, or
  next to the web page's `index.html`). `elog_cli site install` and
  `site show` manage it. Unset keys turn those features off.
- Support new TCP eventlog flags from the firmware (`FLAG tcp_flags`):
  `TCP_FLAG_FIN`, `SYN`, `RST`, `PUSH`, `ACK`, `URG`, `ECE`, `CWR`, `AE`,
  and the log-only `TLP` (0x4000) and `RETRANSMIT` (0x8000) flags.
- `OUT` sends with `TCP_FLAG_RETRANSMIT` are directly recognized as
  retransmits on the Sizes panel and in CLI session summaries.
- `format_tcp_flags` helper in `elog_parser::names` for rendering TCP
  flags bitmasks.

### Removed

- `EVT_RETRANSMIT_REASON` (id 401) decoding and the heuristic that
  reclassified the preceding `OUT` as a retransmit when one arrived.
  Retransmits are now detected solely from `TCP_FLAG_RETRANSMIT` on
  `EVT_OUT.flags`.

### Fixed

- Fanout traffic and join plots work on captures from ELLA's reworked
  `fanout_node` schema, which moves the `CHANNEL_STATS` counters into new
  per-side `TRAFFIC_STATS` and `DELIVERED_STATS` events. Both fold into the
  preceding `CHANNEL_STATS` sample, and the Joins plot adds downstream
  leaves, KEEPALIVE expiries and rejects. `OBJECT_TIMING` decodes its new
  `first_packet`, and the new `SUBSCRIBER_IDENTITY` event (client address
  and PBCID) appears in the channel event list and `elog_cli ellaevents`.
  Older captures decode as before.
- Fanout `CHANNEL_STATS` from current ELLA decodes its 64-bit
  `bytes_up` / `bytes_dn` / `bytes_dlvr`, so the Fanout traffic
  plot keeps working on relays above 4 GiB per sample. Captures with the
  earlier 32-bit layout decode as before. The schema-less fallback reads the
  packed 104-byte layout.
- `elog_cli ellastats` prints `CHANNEL_STATS` unique clients (`cli_up`,
  `cli_dn`) and relay subtree subscriptions (`subs_dlvr`).
- Current captures no longer report `IN`/`OUT` as schema-divergent.
  The decoder's expected layout still listed `th_sum`, which the kernel
  schema dropped in August, so every capture since
  then decoded IN/OUT on the generic path. `th_sum` is now optional, as
  are `GOODPUT.pacing_bw`, the IF `other_*` sample counters and IF caps2,
  so their presence or absence no longer raises a drift warning or an
  "undecoded field" notice.
- The desktop Share Capture dialog stays inside the window. Its width
  tracks the app window (capped at 560px), the permalink wraps, and a
  short window scrolls instead of running off the bottom.
- Switching to the Process or Flame tab on large captures in the browser
  version no longer blocks the UI thread. Initial tab builds and filter
  rollups run through background tasks that yield to the browser before
  computing, displaying the tab and its progress spinner immediately.
- Browser loads of large captures no longer abort mid-parse and leave
  the bar stuck on "Parsing events…". The CPU-sample buffer is reserved
  from the capture's own sample count, session lists from its connection
  fan-out, and a gzip load drops each decompressed chunk once the parser
  has passed it so decoded events reuse that space. If a capture still
  exhausts the tab, the page reports that instead of spinning.

## [1.5.0] - 2026-10-08

### Added

- Session List **Timeline** column and overview minimap. Each row draws
  the session's lifetime on the full trace span, with a brighter segment
  for the byte-weighted active window. Drag the minimap to keep sessions
  that overlap a time window.
- Flame **Compare…** differential flame graph. Each capture keeps its
  own time window.
- System **Origin Segment Rate**, loaded from one or more CSVs and
  aligned to the capture. A file with no rows in the window stays
  listed. **HPTS Slot Gate** plots (hidden by default). **TCP Smoothed
  RTT** from `TCP_SRTT_SAMPLE`. A Time buckets slider sets how many
  equal-time buckets the time-series plots use.
- nginx diagnostic events: connections, TLS, cache, upstream, sendfile,
  file-open failures, and worker stalls. New `nginxconns` and
  `nginxdiag` commands, extra `nginx` columns, a System **nginx Problem
  Events** plot, and request detail in the session info popup.
- Processes: merged top-function ranking, resizable panes, and stack
  display settings (module prefixes, wrapping) that persist across runs.
  CPU selection is shared with Flame and Top functions.

### Changed

- Memory is a flat stack of page states (Wired / Active / Inactive /
  Laundry / Other / Free) and starts hidden, with Memory Pressure and
  Swap. Network TX and Network RX each keep their own interface
  selector. Origin Segment Rate sits above Network TX.
- Channel Object Rate uses `CHANNEL_SAMPLE` when the title has no
  `OBJECT_STATE`.
- Smoothed RTT and Goodput color keys keep a bucket that reaches 1% of
  any visible time column, listed from the smallest range to the
  largest.
- Light-theme flame canvas matches the window background. Closing a
  flame function's source tab returns to the tab that opened it.

### Fixed

- The web build compiles again. Origin-segment CSV paths stay on the
  desktop path; the browser build re-aligns rows already in memory.
- nginx `REQUEST_START` from current firmware (packed schema,
  variable-length `uri`) is no longer flagged schema-divergent.
- Dragging the Time buckets slider no longer starts a box zoom.

## [1.4.0] - 2026-09-29

### Added

- System **Memory** and **Swap** panels from the kernel `mem` provider.
  `elog_cli mem` dumps `MEM_INFO` and `SYS_SAMPLE`.
- `PROC_FORK(child, parent)`: fork-without-exec children inherit the
  parent's `PROC_MAP` set. Captures without the event keep the
  same-`comm` ASLR heuristic.
- Web build opens a rolled capture set as one capture, like desktop.

### Changed

- Processes tab: the selected process fills the right side, stack rows
  scroll, and the CPU strip shares its time window with Flame and
  System.
- Group-by-process keeps a kernel thread under `kernel` when a stack
  walk inserts a junk PC.
- Blue user-space flame frames are on by default.

## [1.3.1] - 2026-09-25

### Added

- Shift+left-drag slides a zoomed time window (or the Flame CPU Usage
  filter) without changing its width. Shift+right-drag slides a Δt
  measurement band the same way.

- Web splash lists `.elog` / `.elog.gz` files from a sibling `../logs/`
  directory listing (a dev host's `~/public_html/logs`) so a capture opens
  with one click. One subdirectory level is flattened in, the list is
  centered under the splash card, and rows sort newest-first. The
  list is omitted when that path is missing or is not an autoindex.
  Opening a listed capture (or a splash URL) uses `history.pushState`
  so Back returns to the landing page.
- System **TCP Connection Rate** — established vs torn-down conn/s from
  the same `TCP_SAMPLE` PCB gauges as TCP Connections by State, 250
  bins over the visible window (same as HTTP RPS).

### Changed

- Stacked histogram color keys and hover lists (TCP Send Size, LRO
  Flush, Goodput over time, HPTS) keep buckets that are at least 1%
  of the visible window / hovered column, ordered by share.

- Plot catalog omits rows with no capture data unless **Show plots
  with no data** is checked (footer, off by default). Dimmed empty
  rows remain available when that box is on.

- Plot catalog sidebar scrolls when the row list is taller than the
  panel; Apply / Reset stay pinned at the bottom.

- System Fanout plots split into one catalog row per plot: **Fanout
  Server TX / RX / Frame Send / Frame Duration / Frame Ratio / Errors**
  and **Fanout Relay Subscribers / RX / TX / Frame Send / Frame
  Duration / Frame Ratio / Channels / Errors**. Server TX, Relay
  Subscribers, and Relay RX are shown by default; the rest start
  hidden. The Server no longer has Subscribers or Channels plots. The
  Fanout line plots (Subscribers aggregate, Frame Send / Duration /
  Ratio, Channels, Errors) now bin into 250 equal-time buckets over
  the visible window, the same model the channel tabs use.

- System catalog hides **Network RX** and **TCP Connection Rate** by
  default.

- Fanout channel-tab line plots (Subscribers, Object Rate, Frame
  Rate / Duration / Ratio, Object Duration / Size, Errors) use 250
  equal-time bins over the visible window, matching System HTTP RPS
  and TCP Connection Rate, instead of mean-downsampling up to 4000
  raw points.
- Fanout **Frame Timeline** lays Server Actual at
  `[FRAME_SENT timestamp − actual_send_us, timestamp]` when the object's
  frame events are spread in time (per-frame emit). A GOP-end burst
  (all timestamps within 10 ms) still packs end-to-end from object
  STARTED, so older captures keep the previous layout.
- Fanout **Object Timeline** and **Frame Timeline** stripe GOPs by the
  object slot (`boundary_index`, rotating 0-3 per object) and shade frames
  by the FDACE `toggle` bit — both read off the wire on every event — instead
  of the time-ranked index and a renumbered position. Dropped events no
  longer flip a GOP's teal/purple or shift the within-GOP frame shading, and
  the two lanes stay in lockstep.

### Fixed

- Share dialog (web) uses a real HTML text field and Copy button so
  clipboard, Ctrl+C, and the browser context menu work (including on
  HTTP). Flame no longer treats Ctrl as stack multi-select while that
  dialog is open.

- Opening a Fanout title with many envelope sessions (e.g.
  `health_channel` reincarnated hundreds of times) indexes that title's
  events and samples in one pass instead of scanning the whole Fanout
  stream once per session, so the channel tab spinner finishes.

- System **HTTP Requests Per Second** skips `TCP_RPS_SAMPLE` zero
  priming ticks (same as `elog_cli tcpsamples`) and un-hides when those
  samples have a non-zero count, so a TCP-only capture plots a rate.
  A lone startup sample (count > 0, no later ticks) no longer marks
  the plot as having data.

- Fanout channel-tab line plots keep a frozen occupancy hold across
  the 250 visible-window bins. A long unchanged `CHANNEL_SAMPLE`
  gauge (e.g. ~96k subscribers with no vertex for ~220s) no longer
  drops to 0 in the middle of the window.

- Fanout channel-tab plots compute on a worker thread, so opening a
  busy title no longer freezes the UI. A spinner shows until they land.
- Share URLs (web) can deep-link to a tab (`tab=system|sessions|flame|processes`),
  a Fanout channel (`channel=<provider_id>,<session_id>`), a time range
  (`t=` on System / session / channel / Flame highlight), and a catalog
  row (`plot=`, scrolled into view).
- Session and Fanout channel tabs keep their own time zoom; switching
  among those tabs (or to System) does not copy a window. System and
  Flame still share one. CPU-usage strips and Fanout channel plots use
  per-tab widget ids so cached plot bounds cannot leak across session
  or channel tabs.
- Right-click Δt highlight stays inside the plot scroll region (below
  the channel-tab header) and maps each plot through its own x transform,
  so the band stays put when the pointer moves onto the Object/Frame
  Timelines (wider y-gutter).
- Object/Frame Timeline hover tooltips list lanes top-to-bottom and
  include size and duration.

- A failed file open (truncated gzip, parse error, empty capture) stays
  on the splash and shows the error instead of bouncing back with no
  message.
- Flame graphs copy `PROC_MAP` from the last same-`comm` donor onto
  forked pids that share one ASLR layout (`inherit_fork_proc_maps`),
  so nginx reload workers without their own maps still symbolize.

## [1.3.0] - 2026-09-23

### Added

- Fanout **Frame Timeline** joins frames to Object Timeline GOPs
  (`FRAME_SENT` id, same teal/purple). Server Actual is laid from
  object STARTED; Relay Recv stays first-to-last packet.

### Changed

- Fanout **Object Duration** (and Object Timeline) caption: STARTED
  until the next object's first packet, so a skip reads as ~4s / ~6s.

- Fanout **Object Timeline** / **Frame Timeline** use three lanes:
  Server, Relay Internal, Relay External. External Median / P75 / P95 /
  Max picks one worker; Internal is the ingest instance. **Object Rate**,
  **Object Duration**, and **Object Size** split the same way (External
  Duration/Size stay worker percentiles).

- Session **HTTP Response Size** and **HTTP Latency** are per-request
  scatters (status-class colors), not binned means.
- System **HTTP Latency** is two 250-bin means: **Small Requests**
  (< 5K bytes) vs **Large Requests**.
- CPU Usage **Scale by freq** uses `CPU_INFO.freq_max_mhz` as the
  100 % reference (sample `freq_mhz` / rated max). Quiet windows
  that stay in a low P-state now scale down instead of treating
  their observed peak as 100 %. Older captures without the field
  still use the histogram stand-in.

### Fixed

- Fanout per-title stacks and the channel list ignore `CHANNEL_SAMPLE`
  on EventLog sessions that never got `SESSION_CREATE` (drained-ring
  leftovers such as `#752` / `#753` on `ella-12k.elog`). Occupancy
  was holding those one-tick gauges for the whole capture, so Per
  channel no longer matched Aggregate.
- Fanout channel-tab **Subscribers** uses `CHANNEL_SAMPLE` gauges
  when the title has them (same occupancy as System Per channel /
  Peak). Join events are unique `(instance, address, port)` only
  when samples are absent, so a title with 54 instances no longer
  climbs to 100k+ while Peak stays ~6k.
- Fanout **Object Timeline** / **Duration** / **Size** / **Rate**
  share one typical completed GOP copy per node. Duration matches
  bar width (a long copy is clipped when the next GOP starts).
  Rate is that copy's bytes/duration, not the sum of every
  instance's raw STARTED→COMPLETED box.

## [1.2.3] - 2026-09-21

### Added

- System **HTTP Latency**: mean `REQUEST_START` → `REQUEST_DONE` in
  milliseconds, 250 bins over the visible window (same count as HTTP
  RPS and Completion Bytes). Header shows the mean of completions in
  view.
- Session **HTTP Response Size** and **HTTP Latency**: mean
  `bytes_sent` / START→DONE ms in 250 bins over the visible session
  window, sharing the TCP session time axis.

### Changed

- Session HTTP request table is gone; request lanes stay on the
  session view (`elog_cli nginx` still lists requests).
- Fanout per-title stacks (Subscribers / RX / TX) omit the plot
  legend; the color key and cursor tooltip still name each band.
- Channel-tab **Object Size** and **Object Duration** y-axes follow
  the visible series instead of pinning 0 (idle Duration zeros do not
  set the floor).

### Fixed

- Session HTTP Response Size / Latency unzoom with the shared time
  axis (double-click undo and middle-click reset).
- Fanout Object Rate / Timeline / Duration / Size no longer draw one
  object across a capture hole (missing roll or idle between files).
  STARTED→COMPLETED longer than a GOP is dropped; Object Size drops
  to 0 between lives the same way Duration already did.
- Trace **i** popup no longer rebuilds NIC rate series and the Fanout
  session list on every frame (those stats are computed at load).

## [1.2.2] - 2026-09-18

### Added

- Flame graph: mouse **Forward** side-button redoes an undone zoom
  step (browser-style forward history alongside Back).
- Flame graph stack filters: **CTRL + click** bars to build a
  keep-only set (cyan outline; applied when CTRL is released),
  **ALT + click** a bar to filter its stack out. Active filters show
  as removable chips above the canvas; excludes win over includes.
- Flame graph: **Escape** and **middle-click** clear all zoom,
  history, and stack filters (including a pending CTRL selection).
- Flame graph: **?** button next to **i** lists mouse and keyboard
  gestures (zoom, history, stack filters, CPU time window).
- Tab bar **?** (left of **i**) lists shared time-plot mouse and
  keyboard gestures.
- Splash screen shows the crate version (`v` plus `Cargo.toml`); the
  empty window / browser-tab title is `EventExplorer v<version>`.
- Fanout **Frame Duration** plots (mean per-frame ms): System
  Server / Relay Internal–External, and per-channel Server / Relay.

### Changed

- HTTP Requests per Second omits the TCP line when nginx data is
  present. HTTP Completion Bytes stays catalog-hidden by default.
  nginx requests live on the TCP session view (table + status-class
  lanes); there is no separate request list or per-request tab.
- TCP session plots (**RTT**, **Rate**, **Send State**, **Sizes**,
  **Size CDF**, **HTTP Requests**) have a title row above the plot,
  matching System / Fanout. Units stay on the y-axis.
- Fanout channel tabs use the full capture time range and plot every
  reincarnation of the title, so opening several files together is not
  cropped to the last file's session. Object Timeline colors flip each
  GOP in time (Server and Relay stay aligned). Skip-red is an id jump
  while objects were still running; the first GOP after a quiet gap is
  not red. Object Duration drops to 0 when the node has no live object.

## [1.2.1] - 2026-09-17

### Added

- Independent height sizer under every catalog plot (grows only that
  plot) and a maximize/restore control on each plot.
- Tab-bar **gear** (next to the info button) for Dark, UTC, and
  global plot **Legends**.

### Changed

- Session **HTTP Requests** sits before RTT. Its **Complete** legend
  is the former Freed series. Y-axis is labeled Slot.
- Middle-drag no longer pans plots (the window may still scroll).
  A middle-click still resets zoom.
- Plot legends sit at the bottom-right so they stay clear of the
  cursor tooltip.

### Fixed

- Enabling System **HTTP Requests per Second** always draws the panel.
  Idle `TCP_RPS_SAMPLE` streams (all-zero counters) no longer skip it.
- Catalog height sizers appear only under plots that drew (hidden or
  empty catalog rows no longer leave stray bars).
- Maximizing a plot with a title row (Network TX/RX, CPU Usage, …)
  sizes the plot to the space *below* that chrome so the tab does not
  scroll. CPU Usage also folds its 8px y-tick inset into that leftover
  so the strip does not overflow.
- Kernel vs User / Per-Process CPU tooltips include a **Total** row
  (full stack sum, including Flame and Session).

## [1.2.0] - 2026-09-17

### Added

- Plot catalog **lock**: a lock control on each sidebar row pins a
  leading run of plots above that tab's scroll. Session and System
  support it; nothing is locked by default. Locking a row also locks
  every plot above it.
- Session catalog includes System **CPU Usage**, **Network TX**,
  **Network RX**, and **Errors & drops** at the top of the list
  (hidden by default).
- System **TCP Average RTT** and **TCP Average Goodput** (renamed from
  Smoothed RTT / Kernel Goodput CDF) each have a Distribution / Over
  time toggle. Over time steps the mean per-session average across
  sessions active at each moment, paced vs not paced, on the shared
  time axis.

### Changed

- Session **CPU Usage** and **HTTP Requests** are ordinary catalog
  rows (show/hide/reorder/lock) instead of always-on sticky chrome.
  HTTP Requests follows Size CDF, is shown by default, and uses the
  same shared time-axis gestures and tooltip as the other session
  plots.
- HTTP aggregate RPS / completion-bytes plots prefer `NGINX_SAMPLE`
  intervals when present (`REQUEST_DONE` remains the fallback).

### Fixed

- System **TCP Goodput** (and the other stacked-bucket plots: send
  size, LRO flush, HPTS lateness/batch) pin x-bounds to the shared
  zoom window so box-zoom matches CPU / Network TX.

### Parsing

- `NGINX_SAMPLE` (id 100).

## [1.1.0] - 2026-09-16

### Added

- Fanout **Subscribers** per-channel stack and channel-tab occupancy
  use `CHANNEL_SAMPLE` when the capture has it (live `subscribers` per
  title / instance). Object rate (channel tab, Fanout RX/TX, Session
  List Rate) uses `object_bytes` over each tick, spreading from the
  previous non-zero sample when a tick is empty. Older
  captures still replay `SUBSCRIBED` / `UNSUBSCRIBED` and `OBJECT_STATE`.
  `ellastats` dumps the samples.
- Session List **Stretch** and **Frame** columns from `FRAME_SAMPLE`
  (stretched-frame fraction and byte-weighted frame Mbps; TX/actual on
  Server, RX/recv on Relay). System **Frame send** plots from the same
  samples.
- Plot catalog sidebar: hide/show and reorder System plots; former
  Network plots live on System until enabled.
- nginx EventLog decode, HTTP Requests plot, request list, and
  `elog_cli nginx` / `nginxstats` / `nginxevents`.
- Machine metadata in the info popup; window title includes hostname.
- Web tab favicon: pixel-aligned navy **E** with cyan and gold bars
  (same palette as the desktop window icon; 16×16 source so the letter
  stays an E at tab size).
- Web splash **Open from a URL** field (same fetch path as `?file=`;
  Enter or Open rewrites the query so a refresh reloads that capture).
- Flame search shows matching-frame sample cost beside next/prev.
- A rolled capture set (`elog --roll-time` / `--roll-size`) opens as
  one continuous timeline.

### Changed

- nginx decode matches connection-scoped firmware sessions (one EventLog
  session per TCP connection; `request` in the payload).
- HTTP Requests per Second has **By class** / **By code**; completion
  bytes plot as Gb/s over the visible window.
- System-tab **Fanout Server** and **Fanout Relay** are separate catalog
  entries; Relay is on by default, Server starts hidden.
- **TCP Connections by State** is on by default.
- CPU Usage plots open in **Kernel vs User** mode (gear still has
  Aggregate / Per-CPU / Per-Process).
- CPU Usage uses the same box-zoom / pan / Δt-measurement gestures as
  the other time-series plots (Flame still drag-selects a time filter).
- The Δt measurement readout stays at the top of the visible plot area
  while scrolling.
- UTC x-axis ticks skip the first neighbor of the `UTC+` left-edge
  label so `UTC+HH:MM:SS` and the next clock don't overlap.
- Box zoom (and Δt measurement) clamp to the visible time window when
  the pointer is released off the plot or off the window.
- Time axis origin is the live capture start, not a drained-ring
  backlog.
- Stacked-area plots use 250 bins over the visible window.
- Web `?file=` loads report download progress via XHR `onprogress`
  (byte counts when the length is known) and yield during gzip so
  the status line moves past Fetching; the event scan still occupies
  one JS turn after "Parsing events…".
- **Dark** checkbox next to UTC (and on the splash) uses the egui
  dark palette on desktop and web (on by default). Desktop stores it
  in `~/.config/event_explorer/dark_mode`; web uses `localStorage`.
  Neither follows the OS/browser light/dark preference. The web splash
  also has a navy/cyan/gold mark and a framed card.

### Parsing

- `CHANNEL_SAMPLE` (id 152) and `FRAME_SAMPLE` (id 153).
- Packed `CONN_SET_IP_V4` / `CONN_SET_IP_V6`.
- nginx `REQUEST_START` `tcp_session_id` for TCP correlation.

## [1.0.0] - 2026-09-08

### Added

- Fanout **Server vs Relay** split when a capture has two `fanout_node`
  providers: System-tab plots, channel-list Node column, and CLI
  `ella`/`ellastats`/`ellaevents` prefixes. Role comes from session-0
  `NODE_INFO` when present, else INSTANCE_ATTACH inference.
- One channel tab per title spanning both nodes, with RX and TX object
  rate on a single plot.
- Fanout object-Mbps System stacks and a sortable channel list (Name / IDs
  / Instances) that stays visible next to TCP sessions.
- `OBJECT_STATE` decode and occupancy plots.
- Packed EventLog payload decode from the embedded EDB1 schema for every
  typed provider.
- Per-failure-site `NODE_ERROR` codes, message types, and binary peer addresses.

### Changed

- Fanout channel-tab series read `<node> - <side>` (`Relay - External`,
  `Server - TX`); a side with a lone instance on a node drops its `i<N>`
  suffix.
- The tab bar's `+` sits after System / Processes / Flame; tabs it opens
  render to its right.
- Aggregate Fanout side series step only where the value moves, so a
  1-instance side is no longer downsampled into a ramp by another side's
  timestamps.
- Per-channel Fanout stacks draw per node again (own palette and plot ids).
- CPU stack samples take `cpu` from the event header instead of casting the
  per-CPU session id.

### Fixed

- Fanout per-channel occupancy on newer ELLA captures, which insert
  `UNSUBSCRIBING` at 2 and move terminal `UNSUBSCRIBED` to 3, so every
  leave printed as `?` and reconstructed stacks never dropped. Ingest now
  resolves state through the blob's `subscriber_state` enum table.
- CI fmt/clippy/test stay green when the runner cannot save the cargo cache.

## [0.3.3] - 2026-05-01

### Added

- Web build auto-loads a capture from a `?file=<url>` query parameter.

### Changed

- Web bundle emits relative asset URLs so it can be hosted under any subpath.
- `release.sh` refuses to build while a `trunk serve` dev server is running
  (it would otherwise corrupt the release zip).

### Fixed

- Web build rejects `?file=file://` URLs with a console-visible error.

## [0.3.2] - 2026-04-30

### Added

- Aggregate page info popup with provider / event / session statistics.

### Changed

- GUI: recovery shading is now toggleable, `rwnd`/`sndbuf` lines default to
  hidden, and retransmit dots were recolored.

## [0.3.1] - 2026-04-30

### Changed

- Updated the HPTS `NIC_RATE_SAMPLE` parser for the dual-EWMA format.

## [0.3.0] - 2026-04-30

### Added

- WebAssembly target: the full GUI runs in a browser tab, packaged as a web
  bundle release artifact.
- Support for the CONN_PARAMS_V2 additions and new TCP stack / congestion
  control change events.

## [0.2.3] - 2026-04-29

### Added

- `CONN_PARAMS_V2` decoding, send-buffer tracking, and a limiter-classification
  overhaul.
- SSH remote-file support with a phase-aware loading screen.

## [0.2.2] - 2026-04-24

### Added

- Limiter classifier plus loss / retransmit signals.
- Gear-menu overlay toggles for the session plots.

### Changed

- Session view defaults to equal plot sizes; the session list scrolls
  horizontally.

## [0.2.1] - 2026-04-24

### Added

- `PACING_CALC_V2` cap visualization and the HPTS system-wide NIC TX-rate plot.
- `AGENTS.md` and a screenshot-led documentation revamp (`docs/` split).

### Changed

- Time x-axis ticks now render on every per-session time-series plot.

## [0.2.0] - 2026-04-13

### Added

- `elog_cli` command-line analysis tools.
- In-flight byte tracking on sends and a recovery-episode count in the session
  list.
- Release packaging (`release.sh`).

## [0.1.0] - 2026-04-09

### Added

- Initial release: egui desktop GUI and shared `.elog` parser.
- Per-session tabs with linked RTT / rate / send-state / size panels, a size
  CDF, goodput-span overlays, right-click Y-axis editing, window-state
  persistence, an application icon, and Linux desktop integration.
