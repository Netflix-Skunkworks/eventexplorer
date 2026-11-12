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

mod recent;
pub use recent::*;
mod hosted_logs;
pub use hosted_logs::*;
mod window;
pub use window::*;
mod share;
pub use share::*;
#[cfg(not(target_arch = "wasm32"))]
mod file_dialog;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use file_dialog::*;
mod colors;
pub use colors::*;
mod plot;
pub use plot::*;
mod plot_layout;
pub(crate) use plot_layout::*;
mod sidebar;
mod timeaxis;
pub(crate) use timeaxis::*;
mod format;
pub use format::*;
mod flame;
pub use flame::*;
mod cpustrip;
pub use cpustrip::*;
mod cpu_filter;
use cpu_filter::CpuFilterUi;
mod aggregate;
pub use aggregate::*;
mod processes;
pub use processes::*;
mod events;
pub(crate) use events::*;
mod functions;
pub use functions::*;
mod source;
pub use source::*;
mod github_source;
mod samples;
pub use samples::*;
mod ella;
pub use ella::*;
mod nginx;
pub use nginx::*;
mod memory;
mod origin;
pub(crate) use origin::*;
// `state` and `update` only add `impl` blocks (no re-exportable items), so
// they are declared without a glob re-export.
mod state;
use state::{PackFetchJob, SymbolPackProgress};
mod update;

pub(crate) use eframe::egui;
pub(crate) use egui::{Align2, Color32, RichText, Stroke, Vec2b};
pub(crate) use egui_extras::{Column, TableBuilder};
pub(crate) use egui_plot::{
    log_grid_spacer, AxisHints, Corner, GridInput, GridMark, Legend, Line, MarkerShape, Plot,
    PlotBounds, PlotMemory, PlotPoint, PlotPoints, Points, Polygon,
};

pub(crate) use crate::analysis::{
    self, build_flame_tree_with_base, FlameAggregation, FlameFilter, FlameNode, HttpReqOutcome,
    Limiter, LimiterSpan, LimiterSummary, PacingCapSpan, ProfileBinaryId, ProfileFunctionId,
    SessionSummary, StackSelector,
};
pub(crate) use crate::elog_parser::{
    self, CpuProfile, EllaNodeRole, EllaProfile, Event, FileInfo, HptsWheelEvent, HptsWheelPayload,
    IfProfile, LoadProgress, NginxProfile, Provider, Sessions, StarsAggSample, TcpSampleProfile,
    TCP_SESSION_ID_STACK_GLOBAL,
};

pub(crate) use std::path::PathBuf;
pub(crate) use std::sync::atomic::{AtomicU32, Ordering};
pub(crate) use std::sync::{mpsc, Arc};
// `web_time::Instant` is a drop-in replacement for `std::time::Instant`
// that works on both native and wasm32-unknown-unknown (where the std
// version panics).
pub(crate) use web_time::Instant;

// ---------------------------------------------------------------------------
// Tab enum
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Aggregate,
    Session(u64),
    SessionList,
    /// Interactive flame-graph view backed by the `cpu` provider's
    /// stack samples. Shown only when the loaded capture contains
    /// cpu-provider events; auto-focused when it's the only data
    /// source (i.e. a cpu-only capture has no TCP sessions or NIC
    /// samples to anchor the Aggregate tab).
    CpuFlame,
    /// Per-process rollup table (pid / comm / samples / % CPU /
    /// threads / modules / first / last) with a per-process detail
    /// panel below the table. Shown only when the loaded capture
    /// contains cpu-provider data. Rows match the flame's Group-by-
    /// process bars by default (user `comm` + one `kernel` row);
    /// the gear can split by thread name or by pid. The detail
    /// panel hosts the "Open flame" button that zooms the Flame
    /// tab to that bar.
    Processes,
    /// Per-event record census (id, name, count, bytes). Opened from
    /// the trace-info popup's Providers section.
    Events,
    /// One ELLA channel (`fanout_node`). Keyed by
    /// `(provider_id, session_id)` the same way the parser stores
    /// channels; relay session ids pack the Fanout instance index in
    /// the high byte.
    EllaChannel {
        provider_id: u16,
        session_id: u64,
    },
}

// ---------------------------------------------------------------------------
// App view enum
// ---------------------------------------------------------------------------

type SessionAuxMap = rustc_hash::FxHashMap<u64, analysis::SessionAuxData>;
#[cfg(target_arch = "wasm32")]
type PendingFilePick = Option<Vec<(String, Vec<u8>, web_sys::File)>>;
type LoadResult = Result<
    (
        Sessions,
        Vec<SessionSummary>,
        Vec<StarsAggSample>,
        Arc<elog_parser::CpuProfile>,
        IfProfile,
        TcpSampleProfile,
        EllaProfile,
        NginxProfile,
        SessionAuxMap,
        FileInfo,
        Vec<Provider>,
        AggregateViewState,
        Vec<HptsWheelEvent>,
        analysis::MemSeries,
    ),
    String,
>;

/// The pieces of a baseline capture the flame-graph comparison needs
/// (everything else the parser produced is dropped).
pub(crate) struct BaselineCapture {
    pub(crate) cpu_profile: Arc<CpuProfile>,
    pub(crate) name: String,
    pub(crate) cpu_panel: Option<CpuUsagePanel>,
}

/// A baseline-capture parse in flight for flame comparison: progress
/// for the chip row, channel for the result.
pub(crate) struct BaselineLoad {
    pub(crate) name: String,
    pub(crate) progress: Arc<LoadProgress>,
    pub(crate) receiver: mpsc::Receiver<Result<BaselineCapture, String>>,
}

/// A share-upload in flight (or just finished), either target: the
/// worker (a native thread or a wasm future) reports permille +
/// stage text through the `Arc`s and the final records through the
/// channel, consumed by `outcome`.
pub(crate) struct ShareUpload {
    pub(crate) progress: Arc<AtomicU32>,
    pub(crate) status: Arc<std::sync::Mutex<String>>,
    pub(crate) receiver: Option<mpsc::Receiver<Result<Vec<elog_parser::UploadRecord>, String>>>,
    pub(crate) outcome: Option<Result<ShareUploadOutcome, String>>,
}

/// What a finished share-upload leaves behind: the upload records
/// and the permalink built from them. The desktop dialog reads both;
/// the web overlay keeps only the error text.
pub(crate) struct ShareUploadOutcome {
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) records: Vec<elog_parser::UploadRecord>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) link: String,
}

/// Editable fields of the desktop Share dialog, kept across frames.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct ShareForm {
    pub(crate) cache_name: String,
    pub(crate) note: String,
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
#[derive(Default)]
struct UnixTextSelectionState {
    drag_origin: Option<egui::Pos2>,
    dragged: bool,
    drag_started_on_text: bool,
    selectable_text_under_cursor: bool,
    mutable_text_under_cursor: bool,
    primary_selection: Arc<std::sync::Mutex<Option<String>>>,
    clipboard: Option<arboard::Clipboard>,
    hook_installed: bool,
}

enum View {
    NoFile,
    Loading {
        file_name: String,
        progress: Arc<LoadProgress>,
        receiver: mpsc::Receiver<LoadResult>,
    },
    Loaded,
}

pub struct EventExplorerApp {
    sessions: Sessions,
    session_aux: SessionAuxMap,
    summaries: Vec<SessionSummary>,
    /// STARS controller aggregate samples (`hpts` provider). Empty when
    /// the capture has no `STARS_AGG_SAMPLE` events. Drives the STARS
    /// panels on the System tab (hidden by default).
    stars_agg_samples: Vec<StarsAggSample>,
    /// STARS-section view state (System tab). Built lazily on first
    /// render of the section; cleared on file load.
    stars_agg_view: Option<StarsAggViewState>,
    /// `hpts` provider timing-wheel events (ids 0-14 — see
    /// `hpts_wheel::parse_hpts_wheel_payload`). Only the
    /// `HPTS_LATENESS_SAMPLE` / `HPTS_TP_BATCH_SAMPLE` (ids 13/14)
    /// variants are currently surfaced in the GUI; empty when the
    /// capture predates them. Drives the HPTS histogram panels on the
    /// System tab (hidden by default).
    hpts_wheel_events: Vec<HptsWheelEvent>,
    /// HPTS-histogram-section view state (System tab). Built lazily
    /// on first render of the section; cleared on file load.
    hpts_hist_view: Option<HptsHistViewState>,
    /// True when the capture has `HPTS_SLOT_GATE_SAMPLE` events. The
    /// Slot Gate catalog rows stay hidden until enabled.
    hpts_has_slot_gate: bool,
    /// Slot-gate view state (System tab). Built lazily on first render
    /// of the section; cleared on file load.
    hpts_slot_gate_view: Option<HptsSlotGateViewState>,
    /// CPU stack samples and thread/kernel metadata from the `cpu`
    /// provider. Empty (`is_empty() == true`) when the loaded capture
    /// has no cpu-provider events. The flame-graph tab is hidden in
    /// that case; otherwise the tab is shown and `flame` is rebuilt
    /// lazily whenever the filter changes.
    ///
    /// Wrapped in an `Arc` so the (potentially multi-gigabyte) profile
    /// can be shared with the background flame-build thread without a
    /// deep copy — see `flame_build`.
    cpu_profile: Arc<CpuProfile>,
    /// `if`-provider profile: interface metadata + periodic counter
    /// samples. Empty when the capture has no `if` data; gates the
    /// Interfaces section of the System tab.
    if_profile: IfProfile,
    /// `tcp`-provider periodic sample profile: PCB-state gauges,
    /// requests/sec, goodput histograms. Empty when the capture has no
    /// TCP sample data. Drives TCP Connections by State and the
    /// per-stack TCP panels on the System tab.
    tcp_samples: TcpSampleProfile,
    /// Interfaces-section view state. Built lazily on first render of
    /// the System interface panels; cleared on file load.
    interfaces_view: Option<InterfacesViewState>,
    /// Per-stack TCP-section view state (System tab). Built lazily on
    /// first render of the section; cleared on file load.
    tcp_samples_view: Option<TcpSamplesViewState>,
    /// `fanout_node` channel-layer profile: per-channel identity,
    /// lifecycle/subscriber events, and `CHANNEL_COUNT` /
    /// `CHANNEL_COUNT_TOTAL`. Empty when the
    /// capture has no ELLA data. Drives the System-tab Fanout panels
    /// and the per-channel session list / plots.
    ella_profile: Arc<EllaProfile>,
    /// Cached `EllaProfile::has_frame_timing` / `fanout_provider_ids`,
    /// computed once at load: the sidebar catalog and the Fanout section
    /// gate on these every frame, and both scans walk every ELLA event.
    ella_has_frame_timing: bool,
    ella_fanout_ids: Vec<u16>,
    /// `nginx` request-layer profile: per-request identity and lifecycle
    /// events. Empty when the capture has no nginx data. Drives the nginx
    /// HTTP RPS plot on System, bytes plot (hidden by default), session-list
    /// HTTP Reqs, and the per-session request table / lanes.
    nginx_profile: NginxProfile,
    /// Capture has nginx problem events (stalls, disk, TLS, upstream).
    nginx_has_diag: bool,
    /// `mem` provider page-state series, built at load. Empty when the
    /// capture has no `mem` data; drives the System-tab Memory / Swap
    /// panels.
    mem_series: analysis::MemSeries,
    /// Grouped Fanout channel table (Session List `+` tab). Built at
    /// load so the RX-rate mode walk does not rerun every frame.
    ella_channel_list: Vec<analysis::EllaChannelListRow>,
    /// Trace-info popup interface byte totals and Fanout summary, filled
    /// at load so opening **i** only paints stored rows.
    info_if_bytes: Vec<analysis::IfByteTotals>,
    ella_info: analysis::EllaRelaySummary,
    ella_sort_column: EllaSortColumn,
    ella_sort_ascending: bool,
    /// ELLA System-tab view state. Built lazily on first render of the
    /// section; cleared on file load.
    ella_samples_view: Option<EllaSamplesViewState>,
    /// Open ELLA channel tabs (sibling of `open_sessions`).
    open_ella_channels: Vec<EllaChannelViewState>,
    /// nginx System-tab bytes view state. Built lazily on first render; cleared
    /// on file load.
    nginx_samples_view: Option<NginxSamplesViewState>,
    /// Aligned origin segment rate and size profile (System tab).
    pub(crate) origin_segments: Option<analysis::OriginSegmentsProfile>,
    /// Loaded origin CSVs (path, region, rows), kept so a new capture re-aligns.
    pub(crate) raw_origin_segments: Vec<(String, String, Vec<analysis::OriginSegmentRow>)>,
    pub(crate) origin_segments_view: OriginSegmentViewMode,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) pending_origin_csv: Vec<PathBuf>,
    /// In-flight background flame-tree build. `Some` while aggregating
    /// the flame tree off the UI thread; the canvas shows a spinner
    /// until the result lands. See `request_flame_rebuild` / `poll_flame_build`.
    flame_build: Option<mpsc::Receiver<FlameBuild>>,
    /// Bumped at the start of each capture load. An in-flight pack
    /// fetch carries the generation it started with and is dropped
    /// when a newer load has begun.
    pack_fetch_gen: u64,
    /// Background catalog fetch for `SYMBOL_PACK_REF` rows. Hits are
    /// installed as they arrive; [`EventExplorerApp::poll_pack_fetch`]
    /// rebuilds an open flame.
    pack_fetch: Option<PackFetchJob>,
    /// Catalog lookup counts for the flame info popup. Kept after
    /// [`Self::pack_fetch`] is cleared.
    pack_progress: SymbolPackProgress,
    /// Flame-graph view state. Built lazily on first switch to
    /// `Tab::CpuFlame` and rebuilt whenever the user changes a
    /// filter; cleared when a new file is loaded.
    flame: Option<FlameViewState>,
    /// Capture-local CPU selector shared by Flame and Top functions.
    cpu_filter_ui: CpuFilterUi,
    /// In-flight baseline-capture parse for flame-graph comparison
    /// (the Flame tab's Compare… button). Polled by
    /// `show_cpu_flame_view`; on success the profile moves into
    /// `FlameViewState.compare`. Cleared when a new primary capture
    /// starts loading.
    baseline_load: Option<BaselineLoad>,
    /// Last failed baseline load, surfaced in the flame compare chip
    /// row until dismissed (or a new compare starts).
    baseline_error: Option<String>,
    /// Process-rollup view state for `Tab::Processes`. Built
    /// lazily on first switch to the tab; cleared on file load.
    processes_view: Option<ProcessesViewState>,
    /// Cross-capture Processes layout/display choices, persisted across runs.
    processes_preferences: ProcessesPreferences,
    /// Function rankings and stack drill-down for the expanded Processes row.
    process_functions_view: Option<FunctionsViewState>,
    process_functions_owner: Option<ProcessSelection>,
    /// In-flight aggregation for the expanded Processes row.
    process_functions_build: Option<mpsc::Receiver<FunctionsViewState>>,
    /// In-flight process-list rollup for a Processes time window.
    processes_rollup_build: Option<mpsc::Receiver<ProcessRollup>>,
    /// Capture-local source-prefix rewrites, scoped by executable image.
    /// Each executable may accumulate multiple non-overlapping DWARF roots.
    #[cfg(not(target_arch = "wasm32"))]
    source_path_rewrites: rustc_hash::FxHashMap<ProfileBinaryId, Vec<SourcePathRewrite>>,
    /// Local ELF/debug paths already chosen for each executable image.
    #[cfg(not(target_arch = "wasm32"))]
    source_binary_paths: rustc_hash::FxHashMap<ProfileBinaryId, PathBuf>,
    /// Cross-session source lookup choices, matched by build id first and
    /// captured executable path second.
    #[cfg(not(target_arch = "wasm32"))]
    source_lookup_preferences: Vec<SourceLookupPreference>,
    #[cfg(not(target_arch = "wasm32"))]
    native_file_dialog: Option<NativeFileDialog>,
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
    ))]
    unix_text_selection: UnixTextSelectionState,
    /// Keep macOS Control-click mapped to secondary through mouse-up,
    /// even if Control is released before the mouse button.
    #[cfg(target_os = "macos")]
    macos_control_click: bool,
    next_source_id: u64,
    /// Time-window filter stashed by tab-switch sync when the
    /// Flame view has not been built yet. Consumed on the next
    /// entry into `Tab::CpuFlame`.
    pending_flame_filter: Option<analysis::FlameFilter>,
    /// Group bar the next Flame-tab paint should zoom to (Processes
    /// "Open flame", after a rebuild or first visit).
    pending_flame_zoom: Option<FlameZoomTarget>,
    /// File header + parser stats from the most recently loaded
    /// capture, surfaced through the aggregate-page info popup.
    /// `None` until a file has finished loading.
    file_info: Option<FileInfo>,
    /// Provider table from the most recently loaded capture, with
    /// per-provider event counts populated by the parser.
    providers: Vec<Provider>,
    /// Events tab (trace-info "More details..."). Closed on a new file.
    events_tab: EventsTabState,
    sort_column: SortColumn,
    sort_ascending: bool,
    filter_low_events: bool,
    /// Session-list time-window filter (absolute µs on the trace
    /// clock), set by dragging the overview minimap. `None` shows
    /// every session.
    session_time_filter: Option<(u64, u64)>,
    /// Anchor timestamp of an in-progress minimap drag.
    session_filter_drag: Option<u64>,
    /// Distribution vs over-time toggle on the TCP Average RTT / TCP
    /// Average Goodput panels. App-level (like `agg_time_axis`) so the
    /// choice survives the aggregate rebuild on "Hide short sessions".
    srtt_avg_view: AvgView,
    goodput_avg_view: AvgView,
    /// When true, every time axis and cursor tooltip shows absolute UTC
    /// clock time instead of seconds-since-capture. Global (not
    /// per-session) — toggled from the tab bar.
    utc_time_labels: bool,
    /// Per-plot legend boxes. Global; toggled from the tab-bar gear.
    show_legends: bool,
    /// Equal-time buckets each time-series plot uses over the visible
    /// window. Global; the tab-bar gear slider. Default [`STACK_BINS`].
    plot_bins: usize,
    /// egui dark visuals (on by default). Remembered in
    /// `localStorage` on web and `~/.config/event_explorer/dark_mode`
    /// on desktop.
    dark_mode: bool,
    /// Web splash: URL typed into the "Open from URL" field.
    #[cfg(target_arch = "wasm32")]
    splash_url: String,
    /// Web splash: captures listed from `../logs/` when that sibling
    /// directory is served as an autoindex (ocadev `~/public_html/logs`).
    #[cfg(target_arch = "wasm32")]
    hosted_logs: HostedLogs,
    #[cfg(target_arch = "wasm32")]
    pending_hosted_logs: Option<mpsc::Receiver<HostedLogs>>,
    /// Web: `location.search` from Back/Forward (`popstate`).
    #[cfg(target_arch = "wasm32")]
    pending_history_nav: mpsc::Receiver<String>,
    /// When true, CPU-busy plots (Aggregate, per-session, Flame) show
    /// busy% multiplied by each bin's frequency-scale factor instead of
    /// the raw sample count. On by default, for comparison against
    /// frequency-scaled external telemetry (e.g. OC Admin's `Cpu`
    /// metric). No-op on captures that predate per-sample frequency
    /// capture.
    scale_cpu_by_freq: bool,
    /// CPU Usage on System follows the shared time zoom (box zoom,
    /// re-binned window). Off shows the full capture with that window
    /// highlighted.
    cpu_zoom_system: bool,
    /// Same switch for the Flame tab. Off by default: full capture
    /// plus a drag-to-select highlight.
    cpu_zoom_flame: bool,
    /// Same switch for the Processes tab. Off by default.
    cpu_zoom_processes: bool,
    /// Same switch for per-session CPU Usage. On by default: the plot
    /// follows that session's time zoom.
    cpu_zoom_session: bool,
    view: View,
    /// Most-recently-opened captures, newest first. Each entry is the
    /// file set it was opened as, so a rolled capture reopens whole.
    recent_files: Vec<Vec<PathBuf>>,
    /// Files of the in-flight load, promoted into `recent_files` once it
    /// succeeds.
    loading_paths: Vec<PathBuf>,
    /// Last failed open, painted on the splash until the next load starts.
    open_error: Option<String>,
    active_tab: Tab,
    /// Applied System-tab plot order / visibility.
    system_layout: PlotLayout,
    /// Sidebar draft for the System tab; Apply copies this onto
    /// `system_layout`.
    system_draft: PlotLayout,
    flame_layout: PlotLayout,
    flame_draft: PlotLayout,
    processes_layout: PlotLayout,
    processes_draft: PlotLayout,
    plot_sidebar_open: bool,
    /// When false, catalog rows with no capture data are omitted.
    plot_catalog_show_empty: bool,
    plot_sidebar_drag: Option<PlotId>,
    aggregate: Option<AggregateViewState>,
    /// Shared time-axis zoom/pan/measurement state for the System tab's
    /// stacked time panels. Lives on the app (not `AggregateViewState`,
    /// which is rebuilt on every filter toggle) so the zoom window
    /// survives "Hide short sessions" and per-process rebuilds; reset only
    /// on new-file load.
    agg_time_axis: TimeAxisController,
    open_sessions: Vec<PlotViewState>,
    renaming_tab: Option<(u64, String)>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    startup_monitor: Option<(f32, f32)>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    current_window_state: Option<WindowState>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    last_window_save: Option<Instant>,
    loaded_file_name: Option<String>,
    /// URLs the active capture was fetched from, when it came in via
    /// `?file=…` (web; one per file of a rolled set). Empty when any file
    /// came in via the file picker, the SSH dialog, or the desktop CLI —
    /// those have no URL a Share link recipient could re-fetch. The Share
    /// popup emits these as `file=` and warns when it can't.
    loaded_file_urls: Vec<String>,
    current_title: Option<String>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    remote_input: Option<String>,
    /// Share params parsed from the page's query string at startup
    /// (web). The `file_urls` field has already been consumed by the
    /// auto-load path; the remaining post-load fields (`session`,
    /// `t_range`, the toggle overrides) are applied once when the
    /// first capture finishes loading. `take()`-ed in
    /// `apply_pending_share_params` so subsequent file loads in the
    /// same browser tab don't re-apply old state.
    pending_share_params: Option<ShareParams>,
    /// Whether the Share popup is currently displayed. Toggled by the
    /// 🔗 button in the tab bar: a DOM overlay on web, an egui
    /// dialog on desktop.
    share_popup_open: bool,
    /// Share-upload job driven from the popup/dialog, either target.
    share_upload: Option<ShareUpload>,
    /// Paths of the loaded capture (desktop), kept so the Share
    /// dialog can upload them. Cleared on the next load.
    #[cfg(not(target_arch = "wasm32"))]
    loaded_paths: Vec<PathBuf>,
    /// Share dialog form drafts (desktop): cache name and note.
    #[cfg(not(target_arch = "wasm32"))]
    share_form: Option<ShareForm>,
    /// Clipboard for the Share dialog's copies (desktop). Kept alive
    /// for the app's lifetime: an X11 selection dies with its owner.
    #[cfg(not(target_arch = "wasm32"))]
    share_clipboard: Option<arboard::Clipboard>,
    /// Browser file handles of the loaded capture (web; picker loads
    /// only), kept so the Share overlay can re-read them for upload
    /// without holding the bytes.
    #[cfg(target_arch = "wasm32")]
    loaded_local_files: Vec<(String, web_sys::File)>,
    /// Validation error to show in the Share overlay's upload card
    /// (web) before a job starts.
    #[cfg(target_arch = "wasm32")]
    share_upload_error: Option<String>,
    /// One-shot scroll target from a share URL's `plot=` param: the
    /// catalog id to scroll into view on the next frame for that tab.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pending_scroll_plot: Option<PlotId>,
    // Web-only: receiver for the async browser file picker. Native uses
    // the egui filesystem browser in `file_dialog.rs`. The web task
    // sends `Some((name, bytes))` on success or `None` if the user
    // cancelled. Polled each frame in `update`.
    #[cfg(target_arch = "wasm32")]
    pending_file_pick: Option<mpsc::Receiver<PendingFilePick>>,
    // Web-only: same picker channel for the flame-compare baseline
    // (single file).
    #[cfg(target_arch = "wasm32")]
    pending_baseline_pick: Option<mpsc::Receiver<Option<(String, Vec<u8>)>>>,
}
