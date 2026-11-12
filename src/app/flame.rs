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
// Flame-graph view state and renderer
// ---------------------------------------------------------------------------

/// How the flame's base rows are organized. The gear menu picks one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlameOrganize {
    /// `[all]` at the bottom, then a bar per process and one `kernel` bar.
    ByProcess,
    /// `kernel` and `user` are the bottom row.
    KernelUser,
    /// `[all]` at the bottom. User samples keep a process bar. Kernel
    /// stacks hang on `[all]`.
    NoKernelGroup,
}

impl FlameOrganize {
    /// `(group_by_process, kernel_user_base, kernel_group)`.
    fn flags(self) -> (bool, bool, bool) {
        match self {
            Self::ByProcess => (true, false, true),
            Self::KernelUser => (true, true, true),
            Self::NoKernelGroup => (true, false, false),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ByProcess => "by process",
            Self::KernelUser => "kernel / user",
            Self::NoKernelGroup => "no kernel group",
        }
    }
}

/// Cached flame-graph state for [`Tab::CpuFlame`]. Built lazily on the
/// first switch to the tab and rebuilt whenever the user changes a
/// filter checkbox / dropdown.
///
/// `tree` is the root of the aggregated `FlameNode`; `zoom_path` is the
/// chain of nested addresses the user has drilled into (empty = show
/// the full tree). Click a frame to push its address onto the zoom
/// stack; "Reset" clears it. `search` is a case-insensitive substring
/// of the visible label or hex address; `|` separates alternatives
/// (`ella|mrelay`). Matching frames render with a brighter hue.
pub struct FlameViewState {
    pub(crate) tree: FlameNode,
    /// Stack of `(addr, pid)` keys the user has zoomed into. Bottom
    /// of the stack is the topmost ancestor. The visible root for
    /// rendering is the deepest descendant matching this path, or
    /// the full tree if the path is empty. Frames under that root
    /// draw as one bar labelled `a > b > c`. We carry `pid` alongside
    /// `addr` because user-mode flame nodes are pid-scoped: pid 100's
    /// `0x4000_0000` and pid 200's `0x4000_0000` refer to different
    /// virtual addresses and must navigate to different children
    /// even if their raw addresses collide. `pid == 0` for kernel
    /// frames (whose runtime VAs are global) and for the synthetic
    /// root.
    pub(crate) zoom_path: Vec<(u64, u32)>,
    /// Browser-style back stack. Every navigation that changes
    /// `zoom_path` (click on a frame, click on a breadcrumb crumb)
    /// snapshots the *previous* `zoom_path` here in chronological
    /// order; "go back" pops the most recent snapshot and restores
    /// it verbatim. This intentionally restores the exact prior
    /// state rather than popping one address — clicking a deep
    /// frame in one gesture adds several addresses, and going back
    /// should undo that gesture, not the addresses one at a time.
    /// Cleared by Reset / middle-click / Escape.
    pub(crate) back_stack: Vec<Vec<(u64, u32)>>,
    /// Forward history for the mouse Forward side-button (Extra2).
    /// `go_back` pushes the state it leaves behind; any fresh
    /// `navigate` clears it (standard browser semantics).
    pub(crate) forward_stack: Vec<Vec<(u64, u32)>>,
    /// Include selectors being accumulated while CTRL is held.
    /// Seeded from `filter.include_stacks` when CTRL goes down;
    /// CTRL+click on a bar toggles its selector in/out; releasing
    /// CTRL applies the set as the new include filter. `None`
    /// whenever CTRL is up.
    pub(crate) ctrl_pending: Option<Vec<StackSelector>>,
    /// Set when Escape cancels a pending CTRL selection; blocks
    /// re-seeding until CTRL is fully released.
    pub(crate) ctrl_cancelled: bool,
    pub(crate) search: String,
    /// Index into the current search-match list (left-to-right,
    /// bottom-to-top). `None` until the user hits Next / Prev.
    pub(crate) search_match_idx: Option<usize>,
    pub(crate) filter: FlameFilter,
    /// User-facing toggle: when true (default), display frames as
    /// `kernel+0xOFFSET` if they fall inside the kernel text segment
    /// **and** no SYMBOL_RANGE matches. Symbolised frames always win
    /// regardless of this toggle. When false, unresolved frames
    /// render as raw hex addresses.
    pub(crate) rel_text: bool,
    /// User-facing toggle: when true, prefix symbolised frames with
    /// their owning linker file (e.g. `kernel:tcp_input+0x40`,
    /// `if_em.ko:em_isr+0x18`). Default is off because most kernel
    /// flame graphs are dominated by the kernel image and the prefix
    /// is just noise. Has no effect when no SYMBOL_RANGE events are
    /// present.
    pub(crate) show_module: bool,
    /// Aggregation strategy for the tree. `ByPc` is the historical
    /// default — every unique PC is its own bar, so a hot function
    /// "shatters" into one rectangle per hot instruction. `BySymbol`
    /// is the classic flame-graph view (one bar per function); when
    /// the GUI uses it, `with_offset_overlay` is always set so the
    /// hot-instruction detail is preserved as small `+0xINNER`
    /// rectangles painted above each function bar's self-time region.
    pub(crate) aggregation: FlameAggregation,
    /// When true, keep resolved user-mode bars scoped to the pid
    /// that ran them (the historical behaviour — N forks of nginx
    /// produce N `_start;main;…` towers). When false (the default),
    /// fold matching user-mode symbols across pids so a daemon
    /// with many worker forks reads as one flame graph. Applies
    /// only to `BySymbol` aggregation; in `ByPc` mode the setting
    /// has no effect (raw user PCs can't be safely merged across
    /// address spaces — see `build_flame_tree`). Forwarded into
    /// `build_flame_tree(..., split_user_by_pid)` on every rebuild.
    pub(crate) split_user_by_pid: bool,
    /// Base-row layout. Default is [`FlameOrganize::ByProcess`].
    pub(crate) organize: FlameOrganize,
    /// Render real user-space frames with a cool blue palette while kernel
    /// frames retain the classic warm flame palette. On by default; a saved
    /// preference overrides it. Render-only: does not rebuild the tree.
    pub(crate) blue_user_palette: bool,
    /// Last-rendered hover info (for the bottom status strip). Kept in
    /// view state so it survives the egui "moved off the rect" frame.
    pub(crate) hovered: Option<HoveredFrame>,
    /// Minimum bar width as a percent of the canvas width below
    /// which child bars are dropped from the rendered tree (the
    /// parent stops there, looking shorter). 0 = render everything
    /// down to ~1 px (the historical behaviour). 1.0 = drop any
    /// rectangle narrower than 1% of the canvas. Used to tame
    /// captures whose tower of one-pixel hex-address bars makes the
    /// canvas feel impossible to read; the user can move the slider
    /// up to get a high-level overview, then click a bar to zoom in
    /// (re-grants those filtered children real width).
    pub(crate) min_bar_width_pct: f32,
    /// Differential-flame state when comparing against a baseline
    /// capture (the Compare… button). While `Some`, the canvas renders
    /// `compare.diff.tree` (the label-keyed merged tree) with delta
    /// colouring instead of the normal per-capture `tree`.
    pub(crate) compare: Option<FlameCompareState>,
    /// Annotated source docked beside the flame, opened by
    /// right-clicking a resolved bar.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) source_view: Option<Box<SourceViewState>>,
    /// Pixel width of the docked source panel. 0 sizes it to 80 columns.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) source_width_px: f32,
    /// Canvas shows the current frame's callstack instead of the flame.
    /// Works for the root and for group bars such as `kernel`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) show_callstack: bool,
    /// Source panel is open beside the flame.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) show_source: bool,
    /// Middle-click reset the flame to all while the source panel stayed open.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) source_focus_suspended: bool,
    /// The zoom path changed. The open source panel follows that frame.
    #[cfg(not(target_arch = "wasm32"))]
    source_follow_zoom: bool,
    /// Last resolved frame. A left-click remembers it while the panel
    /// is closed.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) source_frame: Option<SourceJump>,
}

/// State for the differential flame view (Compare… against a baseline
/// capture). Owns the baseline's CPU profile so filter / time-window
/// changes can rebuild the merged tree.
pub(crate) struct FlameCompareState {
    pub(crate) baseline: Arc<CpuProfile>,
    pub(crate) baseline_name: String,
    /// Absolute µs window on the baseline capture's own clock.
    /// `(0, u64::MAX)` is the full capture. Independent of
    /// [`FlameViewState::filter`]'s window, which filters the
    /// current capture.
    pub(crate) base_ts_start_us: u64,
    pub(crate) base_ts_end_us: u64,
    /// Which capture's CPU Usage plot is on screen. The other plot
    /// stays built; only one is drawn.
    pub(crate) cpu_show_baseline: bool,
    /// CPU Usage plot for the baseline, so its time window can be
    /// dragged separately from the current capture's strip.
    pub(crate) cpu_panel: Option<CpuUsagePanel>,
    /// Swap A/B: when true, baseline counts present as "current" and
    /// vice versa (colours invert, tooltip order flips). Widths are
    /// symmetric under the swap, so this is a pure recolour — no
    /// rebuild.
    pub(crate) swapped: bool,
    /// Region compare within the primary capture: `baseline` is the
    /// same profile and `base_ts_*` is region A; the flame filter's
    /// own window is region B. Entered by Alt+drag on the CPU strip.
    pub(crate) same_file: bool,
    pub(crate) diff: analysis::FlameDiff,
}

/// Result of a background flame-tree build: either the normal
/// per-capture tree or a differential merge against the baseline.
pub(crate) enum FlameBuild {
    Single(FlameNode),
    Diff(analysis::FlameDiff),
}

pub struct HoveredFrame {
    pub(crate) label: String,
    /// Raw frame address (kept around for future "open in addr2line"
    /// or symbol-resolution paths). Currently only the formatted
    /// `label` is shown to the user.
    #[allow(dead_code)]
    pub(crate) addr: u64,
    /// Sampled pid for the hovered frame, or `0` for the synthetic
    /// root and kernel-resolved frames. Carried alongside `addr` so
    /// future tooltip enrichment (e.g. "process: nginx [pid 1234]")
    /// has the data it needs without re-walking the tree.
    #[allow(dead_code)]
    pub(crate) pid: u32,
    pub(crate) self_count: u64,
    pub(crate) total_count: u64,
    /// As a percentage of the visible (zoomed) root's total samples.
    pub(crate) pct_of_visible: f64,
    /// As a percentage of all post-filter samples.
    pub(crate) pct_of_total: f64,
    /// Raw per-capture counts when comparing (not swap-adjusted — the
    /// tooltip applies `FlameCompareState.swapped` at display time).
    pub(crate) diff: Option<analysis::DiffCounts>,
    /// The bar under a zoomed frame that names every frame below it.
    pub(crate) collapsed_below: bool,
}

impl FlameCompareState {
    /// (reference, comparison) inclusive counts for a node, honouring
    /// Swap A/B.
    pub(crate) fn ref_cur(&self, c: analysis::DiffCounts) -> (u64, u64) {
        if self.swapped {
            (c.target, c.base)
        } else {
            (c.base, c.target)
        }
    }

    /// (reference_total, comparison_total), honouring Swap A/B.
    pub(crate) fn totals(&self) -> (u64, u64) {
        if self.swapped {
            (self.diff.target_total, self.diff.base_total)
        } else {
            (self.diff.base_total, self.diff.target_total)
        }
    }

    /// Display names for the (reference, comparison) sides.
    pub(crate) fn side_names<'a>(&'a self, primary_name: &'a str) -> (&'a str, &'a str) {
        if self.same_file {
            return if self.swapped { ("B", "A") } else { ("A", "B") };
        }
        if self.swapped {
            (primary_name, self.baseline_name.as_str())
        } else {
            (self.baseline_name.as_str(), primary_name)
        }
    }
}

/// One-line summary of a hovered diff bar, shared by the tooltip and
/// the status strip: `Left: N (x%) -> Right: M (y%) | …`. Sides are
/// named Left/Right so a long capture filename can't push the tooltip
/// off screen. The delta is the change in *share of its capture's
/// samples*, so a busier capture doesn't read as across-the-board growth.
fn diff_hover_line(compare: &FlameCompareState, d: &analysis::DiffCounts) -> String {
    let (ref_n, cur_n) = compare.ref_cur(*d);
    let (ref_total, cur_total) = compare.totals();
    let ref_pct = ref_n as f64 * 100.0 / ref_total.max(1) as f64;
    let cur_pct = cur_n as f64 * 100.0 / cur_total.max(1) as f64;
    let delta = if ref_n == 0 && cur_n == 0 {
        "no samples".to_string()
    } else if ref_n == 0 {
        "new on the right".to_string()
    } else if cur_n == 0 {
        "gone on the right".to_string()
    } else {
        let rf = ref_n as f64 / ref_total.max(1) as f64;
        let cf = cur_n as f64 / cur_total.max(1) as f64;
        format!("delta {:+.1}% of share", (cf - rf) / rf * 100.0)
    };
    format!(
        "Left: {} ({:.2}%) -> Right: {} ({:.2}%) | {}",
        fmt_count(ref_n),
        ref_pct,
        fmt_count(cur_n),
        cur_pct,
        delta,
    )
}

/// Chip-row label for one side of a same-file region compare:
/// `A 12.3–45.6s` (capture-relative), or `A (full capture)`.
fn fmt_region_label(side: &str, lo_us: u64, hi_us: u64, capture_start_us: u64) -> String {
    if lo_us == 0 && hi_us == u64::MAX {
        return format!("{side} (full capture)");
    }
    let lo = (lo_us as i64 - capture_start_us as i64) as f64 / 1_000_000.0;
    let hi = (hi_us as i64 - capture_start_us as i64) as f64 / 1_000_000.0;
    format!("{side} {lo:.1}\u{2013}{hi:.1}s")
}

/// Parse the `EVENT_EXPLORER_FLAME_REGION_A` debug seed: "lo-hi" in
/// seconds since capture start. Installs a same-file region compare at
/// startup so the two-band compare UI is reachable without a drag.
fn parse_region_a_env(s: &str) -> Option<(f64, f64)> {
    let (lo, hi) = s.split_once('-')?;
    let lo = lo.trim().parse::<f64>().ok()?;
    let hi = hi.trim().parse::<f64>().ok()?;
    (lo >= 0.0 && hi > lo).then_some((lo, hi))
}

impl FlameViewState {
    /// Flame state with the same defaults the Flame tab uses, minus a
    /// built tree; the first render kicks the rebuild.
    pub(crate) fn new_empty() -> Self {
        Self {
            tree: FlameNode {
                addr: 0,
                pid: 0,
                total_count: 0,
                self_count: 0,
                children: Vec::new(),
                is_self_pc: false,
                group_label: None,
            },
            zoom_path: Vec::new(),
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            ctrl_pending: None,
            ctrl_cancelled: false,
            search: std::env::var("EVENT_EXPLORER_FLAME_SEARCH").unwrap_or_default(),
            search_match_idx: None,
            filter: FlameFilter::default(),
            rel_text: true,
            show_module: false,
            aggregation: FlameAggregation::DEFAULT_GUI,
            split_user_by_pid: false,
            organize: FlameOrganize::ByProcess,
            blue_user_palette: load_blue_user_flame(),
            hovered: None,
            min_bar_width_pct: 0.0,
            compare: None,
            #[cfg(not(target_arch = "wasm32"))]
            source_view: None,
            #[cfg(not(target_arch = "wasm32"))]
            source_width_px: 0.0,
            #[cfg(not(target_arch = "wasm32"))]
            show_callstack: false,
            #[cfg(not(target_arch = "wasm32"))]
            show_source: false,
            #[cfg(not(target_arch = "wasm32"))]
            source_focus_suspended: false,
            #[cfg(not(target_arch = "wasm32"))]
            source_follow_zoom: false,
            #[cfg(not(target_arch = "wasm32"))]
            source_frame: None,
        }
    }

    pub(crate) fn rebuild(&mut self, profile: &CpuProfile) {
        if self.compare.is_some() {
            self.rebuild_diff(profile);
            return;
        }
        let (group_by_process, kernel_user_base, kernel_group) = self.organize.flags();
        let tree = build_flame_tree_with_base(
            profile,
            &self.filter,
            self.aggregation,
            self.split_user_by_pid,
            group_by_process,
            kernel_user_base,
            kernel_group,
        );
        self.install_tree(tree);
    }

    /// Rebuild the merged diff tree against the installed baseline.
    /// Aggregation is fixed to by-symbol / merged-across-pids inside
    /// `build_flame_diff` (the merge is label-keyed), so the
    /// aggregation and split toggles don't apply here.
    fn rebuild_diff(&mut self, profile: &CpuProfile) {
        let Some(compare) = self.compare.as_mut() else {
            return;
        };
        let base_window = (compare.base_ts_start_us, compare.base_ts_end_us);
        let target_window = (self.filter.ts_start_us, self.filter.ts_end_us);
        let (group_by_process, kernel_user_base, kernel_group) = self.organize.flags();
        let diff = analysis::build_flame_diff(
            &compare.baseline,
            profile,
            &self.filter,
            base_window,
            target_window,
            group_by_process,
            kernel_user_base,
            kernel_group,
        );
        compare.diff = diff;
        let zoom_before = self.zoom_path.clone();
        truncate_zoom_for(&compare.diff.tree, &mut self.zoom_path);
        #[cfg(not(target_arch = "wasm32"))]
        if zoom_before != self.zoom_path {
            self.source_follow_zoom = true;
        }
        self.hovered = None;
        self.search_match_idx = None;
    }

    /// Install a freshly-built flame `tree` (from `rebuild` or from the
    /// background worker in `start_flame_rebuild`) and reconcile the
    /// zoom state against it.
    pub(crate) fn install_tree(&mut self, tree: FlameNode) {
        self.tree = tree;
        // The back and forward stacks are intentionally preserved so
        // the user's history isn't wiped by a filter toggle — entries
        // that are no longer reachable will themselves be truncated
        // when navigated back to.
        let zoom_before = self.zoom_path.clone();
        truncate_zoom_for(&self.tree, &mut self.zoom_path);
        #[cfg(not(target_arch = "wasm32"))]
        if zoom_before != self.zoom_path {
            self.source_follow_zoom = true;
        }
        self.hovered = None;
        self.search_match_idx = None;
    }

    /// Install a freshly-built merged diff tree (from `rebuild_diff`
    /// or the background worker). No-op when compare mode was cleared
    /// while the build was in flight.
    pub(crate) fn install_diff(&mut self, diff: analysis::FlameDiff) {
        let Some(compare) = self.compare.as_mut() else {
            return;
        };
        compare.diff = diff;
        let zoom_before = self.zoom_path.clone();
        truncate_zoom_for(&compare.diff.tree, &mut self.zoom_path);
        #[cfg(not(target_arch = "wasm32"))]
        if zoom_before != self.zoom_path {
            self.source_follow_zoom = true;
        }
        self.hovered = None;
        self.search_match_idx = None;
    }

    /// Snapshot the current zoom state onto the back stack and apply
    /// `mutate` to `self.zoom_path`. Used by every action that
    /// *forward-navigates* (click a frame, click a breadcrumb), so
    /// "go back" later restores the exact pre-action state. Skip the
    /// snapshot if `mutate` was a no-op so the back stack doesn't
    /// fill up with duplicate entries from idle clicks.
    pub(crate) fn navigate(&mut self, mutate: impl FnOnce(&mut Vec<(u64, u32)>)) {
        let snapshot = self.zoom_path.clone();
        mutate(&mut self.zoom_path);
        if snapshot != self.zoom_path {
            self.back_stack.push(snapshot);
            self.forward_stack.clear();
            self.hovered = None;
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.source_follow_zoom = true;
            }
        }
    }

    /// Browser-style back: pop one snapshot off the back stack and
    /// install it as the current zoom path. No-op when the stack is
    /// empty. Each "back" undoes one prior `navigate(...)` call as a
    /// single unit, regardless of how many addresses that call added
    /// to the path. The state it leaves behind goes onto the forward
    /// stack so the mouse Forward button can redo it.
    pub(crate) fn go_back(&mut self) -> bool {
        match self.back_stack.pop() {
            Some(prev) => {
                let cur = std::mem::replace(&mut self.zoom_path, prev);
                self.forward_stack.push(cur);
                self.hovered = None;
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.source_follow_zoom = true;
                }
                true
            }
            None => false,
        }
    }

    /// Browser-style forward: redo the state the most recent
    /// `go_back` undid. No-op when the forward stack is empty.
    pub(crate) fn go_forward(&mut self) -> bool {
        match self.forward_stack.pop() {
            Some(next) => {
                let cur = std::mem::replace(&mut self.zoom_path, next);
                self.back_stack.push(cur);
                self.hovered = None;
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.source_follow_zoom = true;
                }
                true
            }
            None => false,
        }
    }

    /// Reset to root, wipe the back/forward history, and drop every
    /// stack include/exclude filter. Triggered by a middle-click on
    /// the canvas or Escape. The source panel stays open. Returns
    /// true when a stack filter was live so the caller can rebuild
    /// the tree.
    pub(crate) fn reset_all(&mut self) -> bool {
        let rebuild = !self.filter.include_stacks.is_empty()
            || !self.filter.exclude_stacks.is_empty()
            || self.ctrl_pending.is_some();
        self.zoom_path.clear();
        self.back_stack.clear();
        self.forward_stack.clear();
        self.hovered = None;
        self.search_match_idx = None;
        self.filter.include_stacks.clear();
        self.filter.exclude_stacks.clear();
        self.ctrl_pending = None;
        self.ctrl_cancelled = true;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.source_focus_suspended = true;
            self.show_callstack = false;
        }
        rebuild
    }

    /// Step to the next (`delta > 0`) or previous (`delta < 0`)
    /// search match and zoom so that frame fills the canvas.
    /// First press from `None` lands on the first (or last) match.
    pub(crate) fn step_search_match(&mut self, matches: &[Vec<(u64, u32)>], delta: isize) {
        if matches.is_empty() {
            self.search_match_idx = None;
            return;
        }
        let n = matches.len() as isize;
        let next = match self.search_match_idx {
            None => {
                if delta >= 0 {
                    0
                } else {
                    n - 1
                }
            }
            Some(i) => (i as isize + delta).rem_euclid(n),
        } as usize;
        self.search_match_idx = Some(next);
        let path = matches[next].clone();
        self.navigate(|zp| *zp = path);
    }

    /// Zoom to a process bar. A kernel thread opens the merged `kernel` bar.
    pub(crate) fn zoom_to_target(&mut self, target: &FlameZoomTarget) {
        // In compare mode the diff tree holds the group bars (keyed by
        // label hash); the per-capture tree's keys wouldn't resolve.
        let tree = match &self.compare {
            Some(c) => &c.diff.tree,
            None => &self.tree,
        };
        if let Some(path) = zoom_path_for_target(tree, target) {
            self.navigate(|zp| *zp = path);
        }
    }
}

/// Drop any zoom path entries that don't survive a freshly-built tree
/// (e.g. if the user changed the cpu set the previously-zoomed
/// (addr, pid) pair may not be reachable any more).
fn truncate_zoom_for(tree: &FlameNode, zoom_path: &mut Vec<(u64, u32)>) {
    let mut node = tree;
    let mut keep: usize = 0;
    for &(addr, pid) in zoom_path.iter() {
        if let Some(child) = node
            .children
            .iter()
            .find(|c| c.addr == addr && c.pid == pid)
        {
            node = child;
            keep += 1;
        } else {
            break;
        }
    }
    zoom_path.truncate(keep);
}

/// Open-flame destination. `Process` is a root-level group bar
/// (`download_helper`, `kernel`, …). `Thread` is a thread `comm`;
/// a kernel thread opens the merged `kernel` bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FlameZoomTarget {
    Process(String),
    Thread(String),
}

pub(crate) fn flame_zoom_target_for_pid(profile: &CpuProfile, pid: u32) -> FlameZoomTarget {
    let process = analysis::flame_process_group_label(profile, pid);
    if process == analysis::KERNEL_GROUP_LABEL {
        if let Some(comm) = analysis::flame_thread_comm_for_pid(profile, pid) {
            return FlameZoomTarget::Thread(comm);
        }
    }
    FlameZoomTarget::Process(process)
}

fn group_zoom_path(tree: &FlameNode, label: &str) -> Option<Vec<(u64, u32)>> {
    if let Some(child) = tree
        .children
        .iter()
        .find(|c| c.group_label.as_deref() == Some(label))
    {
        return Some(vec![(child.addr, child.pid)]);
    }
    for child in &tree.children {
        if let Some(inner) = child
            .children
            .iter()
            .find(|n| n.group_label.as_deref() == Some(label))
        {
            return Some(vec![(child.addr, child.pid), (inner.addr, inner.pid)]);
        }
    }
    None
}

fn zoom_path_for_target(tree: &FlameNode, target: &FlameZoomTarget) -> Option<Vec<(u64, u32)>> {
    match target {
        FlameZoomTarget::Process(label) => group_zoom_path(tree, label),
        FlameZoomTarget::Thread(label) => {
            for c in &tree.children {
                if let Some(inner) = c
                    .children
                    .iter()
                    .find(|n| n.group_label.as_deref() == Some(label.as_str()))
                {
                    return Some(vec![(c.addr, c.pid), (inner.addr, inner.pid)]);
                }
            }
            if let Some(c) = tree
                .children
                .iter()
                .find(|c| c.group_label.as_deref() == Some(label.as_str()))
            {
                return Some(vec![(c.addr, c.pid)]);
            }
            // Kernel threads share the `kernel` bar.
            group_zoom_path(tree, analysis::KERNEL_GROUP_LABEL)
        }
    }
}

#[cfg(test)]
fn zoom_path_for_process(
    tree: &FlameNode,
    profile: &CpuProfile,
    pid: u32,
) -> Option<Vec<(u64, u32)>> {
    zoom_path_for_target(tree, &flame_zoom_target_for_pid(profile, pid))
}

fn flame_gear_popup_id() -> egui::Id {
    egui::Id::new("flame_gear_menu")
}

fn flame_info_popup_id() -> egui::Id {
    egui::Id::new("flame_info_popup")
}

fn flame_help_popup_id() -> egui::Id {
    egui::Id::new("flame_help_popup")
}

fn flame_chrome_popup_open(ui: &egui::Ui) -> bool {
    ui.memory(|m| {
        m.data
            .get_temp::<bool>(flame_gear_popup_id())
            .unwrap_or(false)
            || m.data
                .get_temp::<bool>(flame_info_popup_id())
                .unwrap_or(false)
            || m.data
                .get_temp::<bool>(flame_help_popup_id())
                .unwrap_or(false)
    })
}

fn flame_chrome_close_others(ui: &mut egui::Ui, keep: egui::Id) {
    for id in [
        flame_gear_popup_id(),
        flame_info_popup_id(),
        flame_help_popup_id(),
    ] {
        if id != keep {
            ui.memory_mut(|m| m.data.insert_temp(id, false));
        }
    }
}

fn flame_chrome_glyph_button(
    ui: &mut egui::Ui,
    glyph: &str,
    font_size: f32,
    open: bool,
    hover: &str,
) -> (egui::Rect, egui::Response) {
    let button_h = ui.spacing().interact_size.y;
    let button_size = egui::vec2(button_h, button_h);
    let (rect, response) = ui.allocate_exact_size(button_size, egui::Sense::click());
    let visuals = if open {
        ui.visuals().widgets.open
    } else {
        *ui.style().interact(&response)
    };
    ui.painter().rect(
        rect.expand(visuals.expansion),
        visuals.rounding,
        visuals.bg_fill,
        visuals.bg_stroke,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(font_size),
        visuals.text_color(),
    );
    let response = if open {
        response
    } else {
        response.on_hover_text(hover)
    };
    (rect, response)
}

/// Render the right-aligned gear button + popup for the flame tab's
/// "set once and forget" toggles (aggregation, `kernel+offset labels`,
/// `show module prefix`, `split user-mode bars by pid`). Keeps the
/// main controls row uncluttered for Search.
///
/// Bumps `*needs_rebuild` to true when the user flips a toggle that
/// changes the flame tree's shape (layout, aggregation,
/// `split_user_by_pid`). Label-only toggles (`rel_text`,
/// `show_module`) don't affect tree shape so they don't request a
/// rebuild.
///
/// State is stored in `egui::Memory` keyed by a fixed id so the popup
/// stays open across frames until the user clicks elsewhere. Same
/// pattern as the per-session gear menu in `show_session_view` —
/// both use a custom-drawn glyph (rather than `menu_button`) to
/// center U+2699 inside the button rect regardless of font baseline.
pub fn show_flame_gear_menu(
    ui: &mut egui::Ui,
    flame: &mut FlameViewState,
    have_symbols: bool,
    comparing: bool,
    needs_rebuild: &mut bool,
) {
    let popup_id = flame_gear_popup_id();
    let is_open = ui
        .memory(|m| m.data.get_temp::<bool>(popup_id))
        .unwrap_or(false);

    // Custom-drawn gear button (matches the per-session gear's look
    // exactly so the two read as the same widget across tabs). egui's
    // default text layout baselines U+2699 near the bottom of its
    // line box; painting the glyph ourselves with CENTER_CENTER
    // alignment keeps it visually centred in the button rect.
    let button_h = ui.spacing().interact_size.y;
    let button_size = egui::vec2(button_h, button_h);
    let (rect, response) = ui.allocate_exact_size(button_size, egui::Sense::click());
    let visuals = if is_open {
        ui.visuals().widgets.open
    } else {
        *ui.style().interact(&response)
    };
    // Use the more solid `bg_fill` (button-resting fill) instead of
    // `weak_bg_fill`, which on dark themes is so close to the panel
    // background that the button disappears unless hovered. The
    // user complained twice about chrome icons looking "merged" —
    // a visible button rect fixes that.
    ui.painter().rect(
        rect.expand(visuals.expansion),
        visuals.rounding,
        visuals.bg_fill,
        visuals.bg_stroke,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "\u{2699}",
        egui::FontId::proportional(14.0),
        visuals.text_color(),
    );
    let response = response.on_hover_text("Display options");

    if response.clicked() {
        let next = !is_open;
        ui.memory_mut(|m| m.data.insert_temp(popup_id, next));
        if next {
            flame_chrome_close_others(ui, popup_id);
        }
    }

    if is_open {
        // Pin both min AND max width so the popup can't grow per
        // frame. Without an upper bound, an unbounded `set_min_width`
        // combined with `pivot(RIGHT_TOP)` creates a feedback loop:
        // every frame egui re-measures the content (tooltip widget
        // sizes, disabled-state visuals etc.) at slightly different
        // widths, and the right-anchored area pushes its left edge
        // further left → "popup keeps sliding left when I move the
        // mouse". Forcing exact_width keeps the popup's width
        // perfectly stable across frames.
        const POPUP_W: f32 = 240.0;
        let area = egui::Area::new(popup_id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.right_bottom() + egui::vec2(0.0, 4.0))
            .pivot(egui::Align2::RIGHT_TOP)
            .constrain_to(ui.ctx().screen_rect());
        let mut combo_popup_ids: Vec<egui::Id> = Vec::new();
        let area_resp = area.show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(POPUP_W);
                ui.set_max_width(POPUP_W);
                if comparing {
                    ui.label(
                        RichText::new(
                            "Comparing against a baseline: aggregation is \
                             fixed to by-symbol with user bars merged across \
                             pids (the diff tree is keyed by symbol identity).",
                        )
                        .size(11.0)
                        .weak(),
                    );
                    ui.separator();
                }
                ui.add_enabled_ui(have_symbols && !comparing, |ui| {
                    let mut by_symbol =
                        matches!(flame.aggregation, FlameAggregation::BySymbol { .. });
                    let prev = by_symbol;
                    let button_id = ui.make_persistent_id("flame_agg");
                    combo_popup_ids.push(button_id.with("popup"));
                    let popup_open = ui.memory(|m| m.is_popup_open(button_id.with("popup")));
                    let cb = egui::ComboBox::from_id_source("flame_agg")
                        .width(POPUP_W - 16.0)
                        .selected_text(if by_symbol { "by symbol" } else { "by PC" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut by_symbol, true, "by symbol");
                            ui.selectable_value(&mut by_symbol, false, "by PC");
                        });
                    if !popup_open {
                        cb.response.on_hover_text(
                            "Flame-graph aggregation.\n\n\
                             • By symbol: collapse PCs that resolve to \
                             the same function (kernel or user-space) \
                             into one bar; per-instruction self-time \
                             hotness is preserved as small `+0xINNER` \
                             overlay rectangles above each function bar. \
                             User-mode bars merge across pids by default \
                             — N forks of the same binary fold into one \
                             tower. Flip \"Split user-mode bars by pid\" \
                             below if you'd rather see per-process \
                             attribution.\n\n\
                             • By PC: one bar per unique program counter \
                             (maximum detail; hot functions \"shatter\" \
                             into per-instruction rectangles). User-mode \
                             PCs are always kept pid-scoped in this mode \
                             (raw PCs are ASLR-dependent, so there's no \
                             safe cross-pid key).\n\n\
                             Disabled until some symbol metadata is \
                             available — re-capture with `elog \
                             --embed-symbols -U` (kernel + user) to \
                             enable.",
                        );
                    }
                    if by_symbol != prev {
                        flame.aggregation = if by_symbol {
                            FlameAggregation::BySymbol {
                                with_offset_overlay: true,
                            }
                        } else {
                            FlameAggregation::ByPc
                        };
                        *needs_rebuild = true;
                        flame.back_stack.clear();
                        flame.forward_stack.clear();
                    }
                });
                ui.separator();
                let prev_organize = flame.organize;
                let mut organize = flame.organize;
                let organize_id = ui.make_persistent_id("flame_organize");
                combo_popup_ids.push(organize_id.with("popup"));
                let organize_open = ui.memory(|m| m.is_popup_open(organize_id.with("popup")));
                let organize_cb = egui::ComboBox::from_id_source("flame_organize")
                    .width(POPUP_W - 16.0)
                    .selected_text(organize.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut organize, FlameOrganize::ByProcess, "by process");
                        ui.selectable_value(
                            &mut organize,
                            FlameOrganize::KernelUser,
                            "kernel / user",
                        );
                        ui.selectable_value(
                            &mut organize,
                            FlameOrganize::NoKernelGroup,
                            "no kernel group",
                        );
                    });
                if !organize_open {
                    organize_cb.response.on_hover_text(
                        "How the bottom of the flame is organized.\n\n\
                         • by process: `[all]` at the bottom, then each \
                         process and one `kernel` bar. Thread names \
                         (`idle`, `intr`, `clock`, …) sit on `kernel`. \
                         This is the default.\n\n\
                         • kernel / user: `kernel` and `user` are the \
                         bottom row. Processes sit on `user`. Those \
                         thread names sit on `kernel`.\n\n\
                         • no kernel group: `[all]` at the bottom. User \
                         processes keep a process bar. Kernel stacks \
                         sit directly on `[all]`.",
                    );
                }
                if organize != prev_organize {
                    flame.organize = organize;
                    *needs_rebuild = true;
                    flame.back_stack.clear();
                    flame.forward_stack.clear();
                    flame.zoom_path.clear();
                    flame.search_match_idx = None;
                }
                if ui
                    .checkbox(
                        &mut flame.blue_user_palette,
                        RichText::new("Blue user-space frames").size(12.0),
                    )
                    .on_hover_text(
                        "Render user-space frames in blue while kernel frames retain the \
                         classic red/orange/yellow palette. This matches the CPU graph \
                         above and makes the user-to-kernel transition visible at a glance. \
                         Search matches remain green. This choice is saved between sessions.",
                    )
                    .changed()
                {
                    save_blue_user_flame(flame.blue_user_palette);
                }
                ui.checkbox(
                    &mut flame.rel_text,
                    RichText::new("Kernel+offset labels").size(12.0),
                )
                .on_hover_text(
                    "When no SYMBOL_RANGE matches a frame, fall back to \
                     `kernel+0xOFFSET` (using KERNEL_INFO.text_base) instead of a \
                     raw hex address. Symbolised frames always win.",
                );
                ui.add_enabled_ui(have_symbols, |ui| {
                    ui.checkbox(
                        &mut flame.show_module,
                        RichText::new("Show module prefix").size(12.0),
                    )
                    .on_hover_text(
                        "Prefix each symbolised frame with its owning linker file \
                         (kernel: `kernel:tcp_input+0x40`, `if_em.ko:em_isr+0x18`) \
                         or DSO basename (user: `libc.so.7:recvfrom+0x12`, \
                         `a.out:main+0x40`). Useful when symbols collide across \
                         modules / DSOs; off by default to keep bar labels short.",
                    );
                    // Only meaningful in BySymbol aggregation — ByPc
                    // keeps user PCs pid-scoped unconditionally (raw
                    // PCs are ASLR-dependent, so there's no safe
                    // cross-pid key). Grey-out when ByPc is active
                    // so the toggle visibly does nothing instead of
                    // silently doing nothing.
                    let by_symbol_active =
                        matches!(flame.aggregation, FlameAggregation::BySymbol { .. });
                    ui.add_enabled_ui(by_symbol_active && !comparing, |ui| {
                        if ui
                            .checkbox(
                                &mut flame.split_user_by_pid,
                                RichText::new("Split user-mode bars by pid").size(12.0),
                            )
                            .on_hover_text(
                                "User-mode symbolised bars: one node per pid \
                                 (legacy) vs. collapsed across pids that share \
                                 the same code (default).\n\n\
                                 OFF (default): N forks of the same binary \
                                 (e.g. nginx workers) fold into one \
                                 `_start;main;…` tower. The kept pid for a \
                                 canonical node is the first one we saw for \
                                 that symbol, so tooltip labels still resolve \
                                 correctly.\n\n\
                                 ON: keep user-mode bars scoped to their \
                                 sampling pid — useful when you explicitly \
                                 want to see per-process cost attribution \
                                 (e.g. two daemons with similar call stacks \
                                 whose CPU budgets you want separate).\n\n\
                                 Unresolved user frames and kernel frames \
                                 are unaffected.",
                            )
                            .changed()
                        {
                            *needs_rebuild = true;
                        }
                    });
                });
                ui.separator();
                ui.label(RichText::new("Min bar width").size(12.0));
                let slider_resp = ui.add(
                    egui::Slider::new(&mut flame.min_bar_width_pct, 0.0..=1.0)
                        .text("% of canvas")
                        .step_by(0.05)
                        .fixed_decimals(2),
                );
                slider_resp.on_hover_text(
                    "Hide rectangles narrower than this percent of the canvas \
                     width. Useful when the visible tree is dominated by towers \
                     of one-pixel hex-address bars (e.g. when `elog -U` couldn't \
                     symbolise the user-mode portion). The bars aren't deleted, \
                     just hidden — clicking the parent zooms in and re-grants \
                     them width. The remaining (visible) tree is then re-fitted \
                     to fill the full canvas height, so filtered branches don't \
                     leave a gap above the surviving stacks.\n\n\
                     0% (default) = render down to ~1 px (historical behaviour).\n\
                     0.5% = drop bars covering less than 0.5% of the canvas.\n\
                     1% = aggressive overview mode; only the largest \
                     branches survive at the current zoom.",
                );
            });
        });

        // Close on click-outside-the-popup (and not the gear). The
        // session-view gear uses the same idiom; matching it keeps
        // the close behaviour consistent across tabs. Also closes
        // on Escape — common for popup menus.
        let popup_rect = area_resp.response.rect;
        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.memory_mut(|m| m.data.insert_temp(popup_id, false));
        }
        if ui.ctx().input(|i| i.pointer.any_click()) {
            if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
                let combo_open = combo_popup_ids
                    .iter()
                    .any(|id| ui.memory(|m| m.is_popup_open(*id)));
                if !rect.contains(pos) && !popup_rect.contains(pos) && !combo_open {
                    ui.memory_mut(|m| m.data.insert_temp(popup_id, false));
                }
            }
        }
    }
}

/// Catalog-lookup counts for the flame info popup. Zeroed on wasm,
/// where packs are not fetched.
#[derive(Clone, Copy, Default)]
pub struct FlamePackStatus {
    pub unique: usize,
    pub cached: usize,
    pub downloaded: usize,
    pub missing: usize,
    pub failed: usize,
    pub done: usize,
    pub fetching: bool,
}

fn flame_info_row(ui: &mut egui::Ui, k: &str, v: String) {
    ui.label(RichText::new(k).weak().size(11.0));
    ui.label(RichText::new(v).size(11.0));
    ui.end_row();
}

fn flame_info_packs(ui: &mut egui::Ui, cpu_profile: &CpuProfile, packs: FlamePackStatus) {
    let refs = &cpu_profile.pack_refs;
    if refs.is_empty() {
        return;
    }
    let mut kernel_refs = 0usize;
    let mut user_refs = 0usize;
    let mut inline = 0usize;
    for r in refs {
        let has_inline = if r.kind == elog_parser::SymbolPackRef::KIND_LINKER {
            kernel_refs += 1;
            cpu_profile
                .symbols
                .get(&r.file_id)
                .is_some_and(|v| !v.is_empty())
        } else {
            user_refs += 1;
            cpu_profile
                .user_symbols
                .get(&r.file_id)
                .is_some_and(|v| !v.is_empty())
        };
        if has_inline {
            inline += 1;
        }
    }
    let mut head = vec![format!("{} refs", fmt_count(refs.len() as u64))];
    if kernel_refs > 0 && user_refs > 0 {
        head.push(format!("{} kernel", fmt_count(kernel_refs as u64)));
        head.push(format!("{} user", fmt_count(user_refs as u64)));
    }
    if inline > 0 {
        head.push(format!("{} inline", fmt_count(inline as u64)));
    }
    flame_info_row(ui, "Symbol packs", head.join(" · "));

    let mut detail = Vec::new();
    if packs.cached > 0 {
        detail.push(format!("{} cached", fmt_count(packs.cached as u64)));
    }
    if packs.downloaded > 0 {
        detail.push(format!("{} downloaded", fmt_count(packs.downloaded as u64)));
    }
    if packs.missing > 0 {
        detail.push(format!("{} missing", fmt_count(packs.missing as u64)));
    }
    if packs.failed > 0 {
        detail.push(format!("{} failed", fmt_count(packs.failed as u64)));
    }
    if packs.fetching {
        detail.push(format!(
            "fetching {}/{}",
            fmt_count(packs.done as u64),
            fmt_count(packs.unique as u64),
        ));
    }
    if detail.is_empty() {
        return;
    }
    ui.label(RichText::new("").size(11.0));
    ui.label(RichText::new(detail.join(" · ")).size(11.0))
        .on_hover_text(
            "Cached packs were already on disk. Downloaded packs came from the \
         catalog this session. Missing is a catalog 404. Failed is a lookup \
         or parse error.",
        );
    ui.end_row();
}

fn flame_info_unresolved(ui: &mut egui::Ui, cpu_profile: &CpuProfile) {
    let mut bare = Vec::new();
    for lf in cpu_profile.linker_files.values() {
        let inline = cpu_profile
            .symbols
            .get(&lf.file_id)
            .is_some_and(|v| !v.is_empty());
        if !inline && !cpu_profile.pack_symbols.contains(lf.file_id) {
            bare.push(lf.filename.clone());
        }
    }
    for uf in cpu_profile.user_files.values() {
        let inline = cpu_profile
            .user_symbols
            .get(&uf.user_file_id)
            .is_some_and(|v| !v.is_empty());
        if !inline && !cpu_profile.pack_user_symbols.contains(uf.user_file_id) {
            bare.push(uf.short_name().to_string());
        }
    }
    if bare.is_empty() {
        return;
    }
    bare.sort();
    const TIP_MAX: usize = 16;
    let tip = if bare.len() <= TIP_MAX {
        bare.join("\n")
    } else {
        format!(
            "{}\n+{} more",
            bare[..TIP_MAX].join("\n"),
            bare.len() - TIP_MAX
        )
    };
    ui.label(RichText::new("Unresolved").weak().size(11.0));
    ui.label(RichText::new(format!("{} files", fmt_count(bare.len() as u64))).size(11.0))
        .on_hover_text(tip);
    ui.end_row();
}

/// Sample rate, symbol coverage, and symbol-pack results for this
/// capture. Host, file, and duration stay on the global info button.
pub fn show_flame_info_button(ui: &mut egui::Ui, cpu_profile: &CpuProfile, packs: FlamePackStatus) {
    let info_id = flame_info_popup_id();
    let info_open = ui
        .memory(|m| m.data.get_temp::<bool>(info_id))
        .unwrap_or(false);

    // Custom-drawn "i" glyph — matches the per-session and aggregate
    // info buttons. Painted with CENTER_CENTER alignment to avoid
    // baseline drift across egui font fallbacks.
    let (info_rect, info_response) =
        flame_chrome_glyph_button(ui, "i", 13.0, info_open, "Trace info");
    if info_response.clicked() {
        let next = !info_open;
        ui.memory_mut(|m| m.data.insert_temp(info_id, next));
        if next {
            flame_chrome_close_others(ui, info_id);
        }
    }

    if !info_open {
        return;
    }

    // Pin both min and max width — same anti-drift trick as the
    // gear popup. Without an upper bound, the right-anchored Area
    // would shift left every frame as egui re-measured the inner
    // grid widths (hover state changes etc.).
    const POPUP_MIN_W: f32 = 260.0;
    const POPUP_MAX_W: f32 = 480.0;

    let total_samples = cpu_profile.samples.len();
    // Same rollup as the Processes tab: one row per sampled pid,
    // grouped by command the way that tab's header counts processes.
    let process_rows = analysis::compute_process_summaries(cpu_profile, &[]);
    let process_groups = analysis::group_process_summaries_by_flame(&process_rows, cpu_profile);
    let sampled_threads: usize = process_rows.iter().map(|r| r.thread_count).sum();
    let total_lfs = cpu_profile.linker_files.len();
    let total_syms = cpu_profile.symbol_count();
    let total_ufs = cpu_profile.user_files.len();
    let total_usyms = cpu_profile.user_symbol_count();
    let total_pmaps = cpu_profile.proc_map_count();
    let pids_with_maps = cpu_profile.proc_maps.len();

    let area = egui::Area::new(info_id.with("area"))
        .order(egui::Order::Foreground)
        .fixed_pos(info_rect.right_bottom() + egui::vec2(0.0, 4.0))
        .pivot(egui::Align2::RIGHT_TOP)
        .constrain_to(ui.ctx().screen_rect());
    let area_resp = area.show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(POPUP_MIN_W);
            ui.set_max_width(POPUP_MAX_W);

            egui::Grid::new("flame_info")
                .num_columns(2)
                .spacing([12.0, 1.0])
                .show(ui, |ui| {
                    let mut sample_bits = vec![fmt_count(total_samples as u64)];
                    if !process_groups.is_empty() {
                        sample_bits.push(format!(
                            "{} processes",
                            fmt_count(process_groups.len() as u64)
                        ));
                    }
                    if !process_rows.is_empty() {
                        sample_bits.push(format!("{} pids", fmt_count(process_rows.len() as u64)));
                    }
                    if sampled_threads > 0 {
                        sample_bits.push(format!("{} threads", fmt_count(sampled_threads as u64)));
                    }
                    flame_info_row(ui, "Samples", sample_bits.join(" · "));

                    if let Some(k) = &cpu_profile.kernel {
                        if k.sample_hz() > 0 {
                            let mut rate = format!("{} Hz", fmt_count(k.sample_hz() as u64));
                            if k.pmchz > 0 && k.pmchz != k.hz {
                                rate.push_str(&format!(" · tick {} Hz", fmt_count(k.hz as u64)));
                            }
                            flame_info_row(ui, "Rate", rate);
                            if let Some(mhz) = k.rated_freq_mhz() {
                                let rated = if mhz >= 1000 {
                                    format!("{:.1} GHz", mhz as f64 / 1000.0)
                                } else {
                                    format!("{mhz} MHz")
                                };
                                flame_info_row(ui, "Rated max", rated);
                            }
                        }
                        if k.text_size > 0 {
                            ui.label(RichText::new("Kernel text").weak().size(11.0));
                            ui.label(
                                RichText::new(format!(
                                    "{:#x} · {}",
                                    k.text_base,
                                    format_bytes(k.text_size),
                                ))
                                .size(11.0),
                            )
                            .on_hover_text(format!(
                                "{:#x}..{:#x}",
                                k.text_base,
                                k.text_base.saturating_add(k.text_size),
                            ));
                            ui.end_row();
                        }
                        if !k.build_id.is_empty() {
                            flame_info_row(ui, "Kernel build", k.build_id.clone());
                        }
                    }

                    if total_lfs > 0 || total_syms > 0 {
                        flame_info_row(
                            ui,
                            "Kernel symbols",
                            format!(
                                "{} symbols · {} files",
                                fmt_count(total_syms as u64),
                                fmt_count(total_lfs as u64),
                            ),
                        );
                    }
                    if total_ufs > 0 || total_usyms > 0 || total_pmaps > 0 {
                        let mut bits = Vec::new();
                        bits.push(format!("{} symbols", fmt_count(total_usyms as u64)));
                        if total_ufs > 0 {
                            bits.push(format!("{} DSOs", fmt_count(total_ufs as u64)));
                        }
                        if total_pmaps > 0 {
                            bits.push(format!("{} maps", fmt_count(total_pmaps as u64)));
                        }
                        if pids_with_maps > 0 {
                            bits.push(format!("{} PIDs", fmt_count(pids_with_maps as u64)));
                        }
                        flame_info_row(ui, "User symbols", bits.join(" · "));
                    }
                    if !cpu_profile.source_files.is_empty() {
                        flame_info_row(
                            ui,
                            "Source",
                            format!(
                                "{} files · {} lines",
                                fmt_count(cpu_profile.source_files.len() as u64),
                                fmt_count(cpu_profile.source_line_count() as u64),
                            ),
                        );
                    }
                    flame_info_packs(ui, cpu_profile, packs);
                    if !packs.fetching {
                        flame_info_unresolved(ui, cpu_profile);
                    }
                });
        });
    });

    // Click-outside-the-popup (and not the (i) button) closes the
    // popup. Escape too. Same idiom as the gear menu and the
    // session-view info button — keeps the close behaviour
    // consistent across tabs.
    let popup_rect = area_resp.response.rect;
    if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.memory_mut(|m| m.data.insert_temp(info_id, false));
    }
    if ui.ctx().input(|i| i.pointer.any_click()) {
        if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
            if !info_rect.contains(pos) && !popup_rect.contains(pos) {
                ui.memory_mut(|m| m.data.insert_temp(info_id, false));
            }
        }
    }
}

/// Mouse and key cheat-sheet for the flame canvas. Same chrome
/// button as the (i) info glyph; sits to its left.
pub fn show_flame_help_button(ui: &mut egui::Ui) {
    let help_id = flame_help_popup_id();
    let help_open = ui
        .memory(|m| m.data.get_temp::<bool>(help_id))
        .unwrap_or(false);

    let (help_rect, help_response) =
        flame_chrome_glyph_button(ui, "?", 13.0, help_open, "Mouse and keyboard");
    if help_response.clicked() {
        let next = !help_open;
        ui.memory_mut(|m| m.data.insert_temp(help_id, next));
        if next {
            flame_chrome_close_others(ui, help_id);
        }
    }

    if !help_open {
        return;
    }

    const POPUP_MIN_W: f32 = 340.0;
    const POPUP_MAX_W: f32 = 420.0;

    let area = egui::Area::new(help_id.with("area"))
        .order(egui::Order::Foreground)
        .fixed_pos(help_rect.right_bottom() + egui::vec2(0.0, 4.0))
        .pivot(egui::Align2::RIGHT_TOP)
        .constrain_to(ui.ctx().screen_rect());
    let area_resp = area.show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(POPUP_MIN_W);
            ui.set_max_width(POPUP_MAX_W);

            let row = |ui: &mut egui::Ui, k: &str, v: &str| {
                ui.label(RichText::new(k).weak().size(11.0));
                ui.label(RichText::new(v).size(11.0));
                ui.end_row();
            };

            ui.label(RichText::new("Mouse").strong().size(12.0));
            ui.separator();
            egui::Grid::new("flame_help_mouse")
                .num_columns(2)
                .spacing([16.0, 2.0])
                .show(ui, |ui| {
                    row(ui, "Hover", "Label, samples, and percent");
                    row(ui, "Click", "Zoom in; stack below folds into one bar");
                    row(ui, "Click folded bar", "Step down one level");
                    row(ui, "Right-click function", "Open annotated source");
                    row(ui, "Right-click background / Back", "Previous zoom");
                    row(ui, "Forward", "Redo zoom");
                    row(ui, "Middle-click", "Clear zoom, history, and stack filters");
                    row(
                        ui,
                        if ui.ctx().os() == egui::os::OperatingSystem::Mac {
                            "Cmd+click"
                        } else {
                            "Ctrl+click"
                        },
                        "Keep stacks through this bar (applies when the modifier is released)",
                    );
                    row(ui, "Alt+click", "Hide stacks through this bar");
                    row(ui, "Drag on CPU Usage", "Keep samples in that time window");
                    row(
                        ui,
                        "Shift+drag CPU Usage",
                        "Slide that window without resizing",
                    );
                    row(
                        ui,
                        "Alt+drag CPU Usage",
                        "Set the orange region (A) and compare it to the blue time window (B)",
                    );
                    row(ui, "Right-click CPU Usage", "Clear the time window");
                });

            ui.add_space(6.0);
            ui.label(RichText::new("Keys").strong().size(12.0));
            ui.separator();
            egui::Grid::new("flame_help_keys")
                .num_columns(2)
                .spacing([16.0, 2.0])
                .show(ui, |ui| {
                    row(ui, "Escape", "Clear zoom, history, and stack filters");
                });
            ui.add_space(2.0);
            ui.label(
                RichText::new(
                    "Escape first closes this popup, the info popup, \
                     or the gear menu, and surrenders the search box \
                     when it has focus.",
                )
                .size(10.5)
                .weak(),
            );
        });
    });

    let popup_rect = area_resp.response.rect;
    if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.memory_mut(|m| m.data.insert_temp(help_id, false));
    }
    if ui.ctx().input(|i| i.pointer.any_click()) {
        if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
            if !help_rect.contains(pos) && !popup_rect.contains(pos) {
                ui.memory_mut(|m| m.data.insert_temp(help_id, false));
            }
        }
    }
}

/// Resolve a frame address to a display label. Cascade is:
///   1. embedded SYMBOL_RANGE -> `name+0xINNER` (or `file:name+0xINNER`
///      when `with_file` is true).
///   2. user-space USER_SYMBOL_RANGE (when pid != 0 and the PC falls
///      in some PROC_MAP for that pid AND a symbol covers it) ->
///      same shape.
///   3. user-space module-only fallback (PC is in a known DSO
///      mapping but no symbol matched) -> `libfoo.so+0xRUNTIME_OFFSET`.
///      Strictly more useful than `0xADDR` because the user can
///      see *which library* is hot even when symbol resolution
///      failed for it.
///   4. otherwise, when `rel_text` is on and `KernelInfo` puts the PC
///      in the kernel text segment -> `kernel+0xOFFSET`.
///   5. otherwise -> raw `0xADDR`.
///
/// Used both from the renderer (rect labels + tooltip) and from
/// search-string matching, so what the user reads is what their
/// search query matches. The synthetic root (`addr == 0`) is reported
/// as `"<root>"` to avoid the meaningless `"0x0"`.
pub fn flame_label(
    addr: u64,
    pid: u32,
    profile: &CpuProfile,
    rel_text: bool,
    with_file: bool,
) -> String {
    if addr == 0 {
        return "<root>".to_string();
    }
    let pid_opt = if pid == 0 { None } else { Some(pid) };
    if let Some(hit) = profile.symbolize(addr, pid_opt) {
        return if with_file {
            hit.label_with_file()
        } else {
            hit.label()
        };
    }
    if rel_text {
        if let Some(k) = &profile.kernel {
            if addr >= k.text_base && addr < k.text_base.saturating_add(k.text_size) {
                return format!("kernel+{:#x}", addr - k.text_base);
            }
        }
    }
    format!("{addr:#x}")
}

/// Label for a rendered flame node: process / kernel grouping bars
/// use their stored `group_label`; overlay children show `+0xINNER`;
/// everything else goes through [`flame_label`].
pub fn flame_node_label(
    node: &FlameNode,
    profile: &CpuProfile,
    rel_text: bool,
    with_file: bool,
) -> String {
    if let Some(label) = &node.group_label {
        return label.clone();
    }
    if node.is_self_pc {
        return flame_offset_label(node.addr, node.pid, profile);
    }
    flame_label(node.addr, node.pid, profile, rel_text, with_file)
}

/// Root-to-parent labels of the frames under the zoomed frame, joined
/// with ` > `. A label is returned when at least one real frame sits
/// under the zoomed frame.
fn ancestor_trail_label(
    tree: &FlameNode,
    zoom_path: &[(u64, u32)],
    profile: &CpuProfile,
    rel_text: bool,
    with_file: bool,
) -> Option<String> {
    if zoom_path.len() < 2 {
        return None;
    }
    let mut node = tree;
    let mut labels = Vec::with_capacity(zoom_path.len() - 1);
    for &(addr, pid) in &zoom_path[..zoom_path.len() - 1] {
        let child = node
            .children
            .iter()
            .find(|c| c.addr == addr && c.pid == pid)?;
        labels.push(flame_node_label(child, profile, rel_text, with_file));
        node = child;
    }
    Some(labels.join(" > "))
}

/// Colour key for the differential legend. A filled rect, because the
/// built-in font has no glyph for a square and draws a hollow box.
fn flame_legend_swatch(ui: &mut egui::Ui, color: Color32, label: &str) {
    let row_h = ui.text_style_height(&egui::TextStyle::Body).max(12.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(row_h, row_h), egui::Sense::hover());
    let swatch = egui::Rect::from_center_size(rect.center(), egui::vec2(9.0, 9.0));
    ui.painter().rect_filled(swatch, 1.0, color);
    ui.label(RichText::new(label).size(12.0).color(color));
}

/// Monospace font that fits in a flame row, plus one glyph's advance.
/// The painter draws only whole glyphs that fit the bar — a clip
/// rect slicing a glyph renders as a hollow box.
pub(crate) fn flame_bar_font(ui: &egui::Ui, row_h: f32) -> (egui::FontId, f32) {
    let avail = (row_h - 2.0).max(1.0);
    let mut size = 11.0_f32;
    let font = loop {
        let id = egui::FontId::monospace(size);
        let h = ui.fonts(|f| f.row_height(&id));
        if h <= avail || size <= 8.0 {
            break id;
        }
        size -= 1.0;
    };
    let advance = ui.fonts(|f| f.glyph_width(&font, '0')).max(1.0);
    (font, advance)
}

fn flame_label_prefix(label: &str, max_chars: usize) -> &str {
    match label.char_indices().nth(max_chars) {
        Some((i, _)) => &label[..i],
        None => label,
    }
}

/// Whole-glyph prefix whose laid-out width fits `max_w`. A clip rect
/// (or the next bar's fill) cutting through a glyph paints as a box.
pub(crate) fn flame_fitted_galley(
    ui: &egui::Ui,
    label: &str,
    font: egui::FontId,
    color: Color32,
    max_w: f32,
    advance: f32,
) -> Option<std::sync::Arc<egui::Galley>> {
    if max_w < advance || label.is_empty() {
        return None;
    }
    let mut n = (max_w / advance).floor() as usize;
    loop {
        let s = flame_label_prefix(label, n);
        if s.is_empty() {
            return None;
        }
        let galley = ui.fonts(|f| f.layout_no_wrap(s.to_owned(), font.clone(), color));
        if galley.rect.width() <= max_w {
            return Some(galley);
        }
        n = n.saturating_sub(1);
        if n == 0 {
            return None;
        }
    }
}

/// Stack-filter selector for a clicked bar: group bars select by
/// label, everything else by its `(addr, pid)` node key.
fn selector_for_node(node: &FlameNode) -> StackSelector {
    match &node.group_label {
        Some(label) => StackSelector::Group(label.clone()),
        None => StackSelector::Frame(node.addr, node.pid),
    }
}

/// Display label for a stack-filter selector chip.
fn selector_label(
    sel: &StackSelector,
    profile: &CpuProfile,
    rel_text: bool,
    show_module: bool,
) -> String {
    match sel {
        StackSelector::Frame(addr, pid) => flame_label(*addr, *pid, profile, rel_text, show_module),
        StackSelector::Group(label) | StackSelector::Name(label) => label.clone(),
    }
}

/// Colour for a process / kernel grouping bar. Matches the CPU-usage
/// plot's per-pid hash so `ella_server` is the same colour on both
/// widgets; the synthetic `kernel` bar uses the kernel/user strip's
/// kernel colour.
fn flame_group_color(node: &FlameNode, highlight: bool) -> Color32 {
    if highlight {
        return flame_search_color(node.pid as u64);
    }
    if node.group_label.as_deref() == Some(analysis::KERNEL_GROUP_LABEL) {
        per_pid_color(analysis::KERNEL_PSEUDO_PID)
    } else {
        per_pid_color(node.pid)
    }
}

/// Empty canvas. Light theme uses the panel fill; dark theme stays gray.
pub(crate) fn flame_canvas_bg(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_gray(28)
    } else {
        visuals.panel_fill
    }
}

/// Label for a synthetic per-PC self-offset overlay child injected in
/// `BySymbol { with_offset_overlay: true }` mode. The function name
/// is already on the parent bar so we elide it and show only the
/// offset (`+0xINNER`). Falls back to a raw hex address when the PC
/// surprisingly fails to symbolise (shouldn't happen since the
/// builder only injects overlay children for resolved PCs, but kept
/// defensive in case the symbol table is filtered between build and
/// render).
pub fn flame_offset_label(addr: u64, pid: u32, profile: &CpuProfile) -> String {
    let pid_opt = if pid == 0 { None } else { Some(pid) };
    if let Some(hit) = profile.symbolize(addr, pid_opt) {
        format!("+{:#x}", hit.inner_offset())
    } else {
        format!("{addr:#x}")
    }
}

/// Maximum depth (in rows) of the rendered flame-graph subtree
/// rooted at `root`, taking the renderer's width-filter into
/// account. The root itself counts as depth 1, a leaf returns 1,
/// a root with one level of children returns 2, etc.
///
/// Used by `show_cpu_flame_view` to pick a row height that makes
/// the tree fill the canvas vertically. Iterative DFS rather than
/// recursive because real captures occasionally produce 60+ frame
/// stacks and a recursive walk would risk a stack overflow on
/// debug builds.
///
/// The traversal mirrors the renderer's exact layout pass: each
/// child gets a slice of its parent's bar proportional to
/// `total_count / parent_total_count`, and the recursion only
/// continues into children whose pixel width is at or above
/// `min_bar_w_px`. This keeps `row_h` honest when the user raises
/// the "Min bar width" slider — the canvas re-fits its remaining
/// rows instead of leaving a tall blank band above the last
/// surviving stack.
///
/// `canvas_w_px` is the current canvas width in pixels (the same
/// `rect.width()` the renderer uses). When the slider is at 0 %
/// the threshold collapses to ~0.6 px (the renderer's one-pixel
/// floor) and this returns the full unfiltered depth — historical
/// behaviour preserved.
pub fn flame_subtree_depth_filtered(
    root: &FlameNode,
    canvas_w_px: f32,
    min_bar_w_px: f32,
) -> usize {
    if canvas_w_px <= 0.0 {
        return 1;
    }
    let mut max_depth = 1usize;
    let mut stack: Vec<(&FlameNode, f32, usize)> = vec![(root, canvas_w_px, 1)];
    while let Some((node, bar_w, depth)) = stack.pop() {
        if depth > max_depth {
            max_depth = depth;
        }
        if node.children.is_empty() {
            continue;
        }
        let parent_total = node.total_count.max(1) as f32;
        for child in &node.children {
            let cw = bar_w * (child.total_count as f32) / parent_total;
            if cw >= min_bar_w_px {
                stack.push((child, cw, depth + 1));
            }
        }
    }
    max_depth
}

/// Depth of `root`'s children, treating each child as a depth-1 bar.
/// Used when the synthetic root is not drawn.
fn flame_children_depth(root: &FlameNode, canvas_w_px: f32, min_bar_w_px: f32) -> usize {
    let mut max_depth = 1usize;
    let parent_total = root.total_count.max(1) as f32;
    for child in &root.children {
        let cw = canvas_w_px * (child.total_count as f32) / parent_total;
        if cw >= min_bar_w_px {
            max_depth = max_depth.max(flame_subtree_depth_filtered(child, cw, min_bar_w_px));
        }
    }
    max_depth
}

/// Color for a per-PC overlay child (BySymbol-with-overlay mode).
/// Maps the same address-hash hue mapping as [`flame_color`] into a
/// muted green (90–130°) so offset rectangles read as intra-function
/// samples rather than another callee. Search-matched overlays
/// short-circuit to [`flame_search_color`] (brighter green).
pub fn flame_self_pc_color(addr: u64, highlight: bool) -> Color32 {
    if highlight {
        return flame_search_color(addr);
    }
    let h = addr.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let hue_byte = ((h >> 32) ^ h) as u32 & 0xFF;
    // Green: hue ∈ [90°, 130°].
    let hue_deg = 90.0 + (hue_byte as f32) * (40.0 / 255.0);
    let (r, g, b) = hsv_to_rgb(hue_deg, 0.50, 0.78);
    Color32::from_rgb(r, g, b)
}

/// Light purple for a user-space offset overlay when the blue palette is on.
/// Same address hash as [`flame_self_pc_color`]; kernel offsets stay green.
fn flame_user_offset_color(addr: u64, highlight: bool) -> Color32 {
    if highlight {
        return flame_search_color(addr);
    }
    let h = addr.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let hue_byte = ((h >> 32) ^ h) as u32 & 0xFF;
    // Light purple: hue ∈ [265°, 290°].
    let hue_deg = 265.0 + (hue_byte as f32) * (25.0 / 255.0);
    let (r, g, b) = hsv_to_rgb(hue_deg, 0.38, 0.93);
    Color32::from_rgb(r, g, b)
}

/// Stable HSV-derived color for a frame. Hash the address and pick a
/// hue in the warm half of the wheel to mirror Brendan Gregg's
/// classic flame-graph look. Search-match frames short-circuit to
/// [`flame_search_color`] (bright green) — the entire flame canvas
/// is otherwise warm-hued (red→yellow), so a green band reads
/// instantly as "this matched my filter" without any close
/// inspection.
pub fn flame_color(addr: u64, highlight: bool) -> Color32 {
    if highlight {
        return flame_search_color(addr);
    }
    // Mix the address bits — addresses cluster in narrow ranges and
    // identity hashing yields adjacent colors for adjacent functions,
    // which makes neighbouring rectangles visually merge. A small
    // multiplicative mix gives much better separation.
    let h = addr.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let hue_byte = ((h >> 32) ^ h) as u32 & 0xFF;
    // Map 0..255 -> warm range (red→yellow): hue ∈ [0°, 50°].
    let hue_deg = (hue_byte as f32) * (50.0 / 255.0);
    let (r, g, b) = hsv_to_rgb(hue_deg, 0.65, 0.86);
    Color32::from_rgb(r, g, b)
}

/// Light blue for user-space frames when the split palette is enabled.
/// Same address hash as [`flame_color`]. Hue runs cyan → blue (the same
/// 50° span as the warm kernel palette) and saturation jitters so
/// neighbouring bars separate, while value stays high enough to match
/// the CPU-usage user band.
pub fn flame_user_color(addr: u64, highlight: bool) -> Color32 {
    if highlight {
        return flame_search_color(addr);
    }
    let h = addr.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mixed = (h >> 32) ^ h;
    let hue_byte = (mixed as u32) & 0xFF;
    let sat_byte = ((mixed >> 8) as u32) & 0xFF;
    // Cyan through blue: hue ∈ [185°, 235°], sat ∈ [0.40, 0.62].
    let hue_deg = 185.0 + (hue_byte as f32) * (50.0 / 255.0);
    let sat = 0.40 + (sat_byte as f32) * (0.22 / 255.0);
    let (r, g, b) = hsv_to_rgb(hue_deg, sat, 0.93);
    Color32::from_rgb(r, g, b)
}

/// Bright green used for any flame rectangle that matches the
/// active search query. Keeps a small amount of per-address hue
/// jitter (within the green band, 95°–135°) so neighbouring
/// matches stay individually distinguishable instead of merging
/// into one solid green slab — but the band is narrow enough that
/// the eye still reads "everything green is a match" at a glance
/// against the warm-hued (red→yellow) flame canvas.
pub fn flame_search_color(addr: u64) -> Color32 {
    let h = addr.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let hue_byte = ((h >> 32) ^ h) as u32 & 0xFF;
    // Narrow green band: hue ∈ [95°, 135°].
    let hue_deg = 95.0 + (hue_byte as f32) * (40.0 / 255.0);
    let (r, g, b) = hsv_to_rgb(hue_deg, 0.85, 0.95);
    Color32::from_rgb(r, g, b)
}

/// Differential-flame bar colour (compare mode). `base` / `target` are
/// the frame's raw inclusive counts in the baseline / current capture;
/// each is normalized by its own capture's total first so a busier
/// capture doesn't read as "everything grew". The delta
/// `(t̂ − b̂) / max(t̂, b̂)` ∈ [-1, 1] drives a red ramp (grew in
/// current) or blue ramp (shrank), with saturation proportional to the
/// magnitude; below ~2 % the bar reads as neutral warm gray. Frames
/// new to the current capture (`base == 0`) pin at full red; vanished
/// frames pin at full blue.
pub fn flame_diff_color(base: u64, target: u64, base_total: u64, target_total: u64) -> Color32 {
    let b = base as f64 / base_total.max(1) as f64;
    let t = target as f64 / target_total.max(1) as f64;
    let delta = if b == 0.0 && t == 0.0 {
        0.0
    } else {
        (t - b) / t.max(b)
    };
    let mag = delta.abs();
    if mag < 0.02 {
        return Color32::from_rgb(0x96, 0x8a, 0x7c);
    }
    let sat = (0.35 + 0.55 * mag) as f32;
    let hue = if delta > 0.0 { 8.0 } else { 210.0 };
    let (r, g, b) = hsv_to_rgb(hue, sat, 0.92);
    Color32::from_rgb(r, g, b)
}

/// Lowercased `|`-separated search terms. Empty pieces (`a||b`,
/// leading/trailing `|`, whitespace-only) are dropped.
fn flame_search_terms(search: &str) -> Vec<String> {
    search
        .split('|')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

fn flame_search_hit(label: &str, addr: u64, terms: &[String]) -> bool {
    if terms.is_empty() {
        return false;
    }
    let label_lc = label.to_lowercase();
    let hex = format!("{addr:#x}");
    terms
        .iter()
        .any(|t| label_lc.contains(t) || hex.contains(t))
}

/// Child draw / walk order: largest `total_count` first (leftmost
/// on the canvas). Addr/pid break ties so the order is stable.
pub(crate) fn flame_child_order(node: &FlameNode) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..node.children.len()).collect();
    indices.sort_by(|&a, &b| {
        node.children[b]
            .total_count
            .cmp(&node.children[a].total_count)
            .then(node.children[a].addr.cmp(&node.children[b].addr))
            .then(node.children[a].pid.cmp(&node.children[b].pid))
    });
    indices
}

/// Canvas-order search hits plus the union of their sample cost.
struct SearchMatches {
    paths: Vec<Vec<(u64, u32)>>,
    /// Inclusive samples under matching frames, each sample counted
    /// once (a match nested under another match is skipped).
    samples: u64,
}

/// Matching frames in canvas order: one row at a time, left to
/// right, starting at the bottom (the synthetic root is skipped).
fn collect_search_matches(
    root: &FlameNode,
    profile: &CpuProfile,
    rel_text: bool,
    show_module: bool,
    terms: &[String],
) -> SearchMatches {
    if terms.is_empty() {
        return SearchMatches {
            paths: Vec::new(),
            samples: 0,
        };
    }
    let mut matches = Vec::new();
    let mut samples = 0;
    type Walk<'a> = (&'a FlameNode, Vec<(u64, u32)>, bool);
    let mut layer: Vec<Walk<'_>> = flame_child_order(root)
        .into_iter()
        .map(|i| {
            let c = &root.children[i];
            (c, vec![(c.addr, c.pid)], false)
        })
        .collect();
    while !layer.is_empty() {
        let mut next = Vec::new();
        for (node, path, ancestor_hit) in layer {
            let label = flame_node_label(node, profile, rel_text, show_module);
            let hit = flame_search_hit(&label, node.addr, terms);
            if hit {
                matches.push(path.clone());
                if !ancestor_hit {
                    samples += node.total_count;
                }
            }
            for i in flame_child_order(node) {
                let c = &node.children[i];
                let mut child_path = path.clone();
                child_path.push((c.addr, c.pid));
                next.push((c, child_path, ancestor_hit || hit));
            }
        }
        layer = next;
    }
    SearchMatches {
        paths: matches,
        samples,
    }
}

pub fn hsv_to_rgb(h_deg: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let h_prime = (h_deg.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match h_prime as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (
        ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

// ---------------------------------------------------------------------------
// Flame-graph view (impl on EventExplorerApp; lives in its own block to
// keep the diff against the previously single-tab app readable)
// ---------------------------------------------------------------------------

impl EventExplorerApp {
    /// Sample count above which the O(samples×depth) flame-tree build
    /// is pushed onto a background thread (native only) so it can't
    /// freeze the UI. Captures below this rebuild synchronously — the
    /// build is sub-100 ms there, and doing it inline avoids a
    /// one-frame spinner flash. Tuned so multi-million-sample OCA
    /// profiles (which take ~15 s) always go async while everyday
    /// captures stay snappy.
    #[cfg(not(target_arch = "wasm32"))]
    const FLAME_ASYNC_THRESHOLD: usize = 400_000;
    #[cfg(target_arch = "wasm32")]
    const FLAME_ASYNC_THRESHOLD: usize = 10_000;

    /// Start (or synchronously perform) a flame-tree rebuild for
    /// `flame`. Captures above [`Self::FLAME_ASYNC_THRESHOLD`]
    /// samples clone the `Arc<CpuProfile>` and filter params onto a
    /// background task, storing the result channel in `flame_build`;
    /// [`Self::poll_flame_build`] installs the tree when it lands.
    /// Everything else rebuilds inline.
    ///
    /// Written as an associated fn taking the disjoint fields
    /// explicitly (rather than `&mut self`) so callers can invoke it
    /// while still holding a `&mut` borrow of `self.flame`.
    pub(crate) fn start_flame_rebuild(
        flame: &mut FlameViewState,
        profile: &Arc<CpuProfile>,
        flame_build: &mut Option<mpsc::Receiver<FlameBuild>>,
    ) {
        let samples = profile.samples.len()
            + flame
                .compare
                .as_ref()
                .map_or(0, |c| c.baseline.samples.len());
        if samples < Self::FLAME_ASYNC_THRESHOLD {
            *flame_build = None;
            flame.rebuild(profile);
            return;
        }
        let profile = Arc::clone(profile);
        let baseline = flame.compare.as_ref().map(|c| Arc::clone(&c.baseline));
        let base_window = flame
            .compare
            .as_ref()
            .map_or((0, u64::MAX), |c| (c.base_ts_start_us, c.base_ts_end_us));
        let filter = flame.filter.clone();
        let target_window = (filter.ts_start_us, filter.ts_end_us);
        let aggregation = flame.aggregation;
        let split = flame.split_user_by_pid;
        let (group_by_process, kernel_user_base, kernel_group) = flame.organize.flags();
        let (tx, rx) = mpsc::channel();
        let compute = move || match baseline {
            Some(base) => FlameBuild::Diff(analysis::build_flame_diff(
                &base,
                &profile,
                &filter,
                base_window,
                target_window,
                group_by_process,
                kernel_user_base,
                kernel_group,
            )),
            None => FlameBuild::Single(build_flame_tree_with_base(
                &profile,
                &filter,
                aggregation,
                split,
                group_by_process,
                kernel_user_base,
                kernel_group,
            )),
        };
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(move || {
            let _ = tx.send(compute());
        });
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            crate::app::recent::yield_to_browser().await;
            let _ = tx.send(compute());
        });
        *flame_build = Some(rx);
    }

    /// Poll the in-flight background flame build, if any. Returns
    /// `true` while a build is still pending (the caller keeps the
    /// canvas in its "Building…" state and requests another repaint).
    /// Installs the finished tree into `self.flame` and clears
    /// `flame_build` when the worker completes.
    fn poll_flame_build(&mut self) -> bool {
        let Some(rx) = self.flame_build.as_ref() else {
            return false;
        };
        match rx.try_recv() {
            Ok(build) => {
                if let Some(flame) = self.flame.as_mut() {
                    match build {
                        FlameBuild::Single(tree) => flame.install_tree(tree),
                        FlameBuild::Diff(diff) => flame.install_diff(diff),
                    }
                }
                self.flame_build = None;
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            // Worker panicked / hung up without sending — stop waiting
            // so the tab doesn't spin forever.
            Err(mpsc::TryRecvError::Disconnected) => {
                self.flame_build = None;
                false
            }
        }
    }

    pub(crate) fn show_cpu_flame_view(&mut self, ui: &mut egui::Ui) {
        // Snapshot before chrome popups consume Escape so a dismiss
        // doesn't also reset zoom and stack filters this frame.
        let chrome_popup_was_open = flame_chrome_popup_open(ui);
        let share_open = self.share_popup_open;
        let stack_modifier = if ui.ctx().os() == egui::os::OperatingSystem::Mac {
            "Cmd"
        } else {
            "CTRL"
        };
        // Lazy build: first time we render this tab after a load we
        // construct the tree from the current filter. The build cost
        // is ~O(samples × depth) which is fine even for 300k samples
        // (sub-100 ms on a typical workstation).
        // Time-window sync from System can stash a filter before the
        // first Flame-tab visit; share `t=` uses the same slot.
        let pending_filter = self.pending_flame_filter.take().unwrap_or_default();
        if self.flame.is_none() {
            self.flame = Some(FlameViewState::new_empty());
            self.flame.as_mut().unwrap().filter = pending_filter;
            // Debug seed: pre-open a same-file region compare with
            // region A from the environment.
            if let Some((lo_s, hi_s)) = std::env::var("EVENT_EXPLORER_FLAME_REGION_A")
                .ok()
                .as_deref()
                .and_then(parse_region_a_env)
            {
                let capture_start = self
                    .file_info
                    .as_ref()
                    .map(|fi| fi.capture_start)
                    .unwrap_or(0);
                let state = self.flame.as_mut().unwrap();
                state.compare = Some(FlameCompareState {
                    baseline: Arc::clone(&self.cpu_profile),
                    baseline_name: String::new(),
                    base_ts_start_us: capture_start.saturating_add((lo_s * 1e6).round() as u64),
                    base_ts_end_us: capture_start.saturating_add((hi_s * 1e6).round() as u64),
                    cpu_show_baseline: false,
                    cpu_panel: None,
                    swapped: false,
                    same_file: true,
                    diff: analysis::FlameDiff::empty(),
                });
            }
            Self::start_flame_rebuild(
                self.flame.as_mut().unwrap(),
                &self.cpu_profile,
                &mut self.flame_build,
            );
        }

        // Poll any in-flight background build before we borrow `flame`
        // for rendering. While a build is pending the canvas shows a
        // "Building…" placeholder instead of the (stale/empty) tree.
        let flame_building = self.poll_flame_build();
        if !flame_building {
            self.apply_pending_flame_zoom();
        }

        // Header chrome consolidated into a single controls row +
        // an info button. The verbose kernel ident / sample counts /
        // symbol metadata that used to take two header rows now lives
        // in the info popup (the (i) button on the right). Saves
        // ~36 px of vertical chrome which the flame canvas reclaims.
        let mut needs_rebuild = false;
        let mut reset_zoom = false;
        let mut go_back = false;
        let mut go_forward = false;
        let mut clear_compare = false;

        let total_syms = self.cpu_profile.symbol_count();
        let total_usyms = self.cpu_profile.user_symbol_count();

        // Reborrow the shared bits we want inside the right-aligned
        // info button BEFORE taking `&mut self.flame` — keeps the
        // closure capturing only the disjoint fields it needs (no
        // double-mutable-borrow on `self`).
        let utc_time_labels = self.utc_time_labels;
        let show_legends = self.show_legends;
        let mut scale_cpu_by_freq = self.scale_cpu_by_freq;
        let mut cpu_zoom = self.cpu_zoom_flame;
        let cpu_zoom_before = cpu_zoom;
        let capture_start_us = self
            .file_info
            .as_ref()
            .map(|fi| fi.capture_start)
            .unwrap_or(0);
        let agg_time_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);

        // Poll a baseline-capture parse kicked off by Compare…. On
        // success, install the compare state and kick a diff rebuild;
        // while in flight, keep repainting so the chip-row progress
        // advances.
        let mut baseline_done: Option<Result<BaselineCapture, String>> = None;
        if let Some(load) = &self.baseline_load {
            match load.receiver.try_recv() {
                Ok(result) => baseline_done = Some(result),
                Err(mpsc::TryRecvError::Empty) => {
                    ui.ctx().request_repaint();
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    baseline_done = Some(Err("baseline loader exited".to_string()));
                }
            }
        }
        if let Some(result) = baseline_done {
            self.baseline_load = None;
            match result {
                Ok(cap) => {
                    let flame = self.flame.as_mut().unwrap();
                    flame.compare = Some(FlameCompareState {
                        baseline: cap.cpu_profile,
                        baseline_name: cap.name,
                        base_ts_start_us: 0,
                        base_ts_end_us: u64::MAX,
                        cpu_show_baseline: false,
                        cpu_panel: cap.cpu_panel,
                        swapped: false,
                        same_file: false,
                        diff: analysis::FlameDiff::empty(),
                    });
                    // The merged tree has a different key space
                    // (label hashes, not PCs): zoom/history don't
                    // carry over, and address-space-scoped Frame
                    // selectors can't match the baseline — keep
                    // only the capture-independent Group ones.
                    flame.zoom_path.clear();
                    flame.back_stack.clear();
                    flame.forward_stack.clear();
                    flame.ctrl_pending = None;
                    flame.hovered = None;
                    flame.search_match_idx = None;
                    flame
                        .filter
                        .include_stacks
                        .retain(|s| matches!(s, StackSelector::Group(_) | StackSelector::Name(_)));
                    flame
                        .filter
                        .exclude_stacks
                        .retain(|s| matches!(s, StackSelector::Group(_) | StackSelector::Name(_)));
                    needs_rebuild = true;
                }
                Err(msg) => {
                    self.baseline_error = Some(msg);
                }
            }
        }

        let plot_bins = self.plot_bins;
        let pack_status = self.flame_pack_status();
        let have_symbols = total_syms > 0 || total_usyms > 0;
        let show_flame_cpu = self.flame_layout.is_visible(PlotId::CpuUsage);
        {
            let flame = self.flame.as_mut().unwrap();
            let comparing = flame.compare.is_some();
            let same_file_comp = flame.compare.as_ref().is_some_and(|c| c.same_file);

            // Command-held stack multi-select (Ctrl on Linux/FreeBSD, Cmd on
            // macOS): seed the pending set when pressed; apply it (and
            // rebuild) when released. State-based rather than
            // edge-based so a missed frame can't strand a pending set.
            let ctrl_down = ui.input(|i| i.modifiers.command)
                && !ui.ctx().wants_keyboard_input()
                && !share_open;
            // Compare-mode clicks store the bar's display name
            // (`StackSelector::Name`), which both captures can match.
            // Address-scoped `Frame` selectors still can't.
            if ctrl_down {
                if flame.ctrl_pending.is_none() && !flame.ctrl_cancelled {
                    flame.ctrl_pending = Some(flame.filter.include_stacks.clone());
                }
            } else if !ctrl_down {
                flame.ctrl_cancelled = false;
                if let Some(pending) = flame.ctrl_pending.take() {
                    if pending != flame.filter.include_stacks {
                        // The two lists stay disjoint: a stack the include
                        // set now keeps can't remain excluded.
                        flame.filter.exclude_stacks.retain(|s| !pending.contains(s));
                        flame.filter.include_stacks = pending;
                        needs_rebuild = true;
                    }
                }
            }
            // The System tab's `CpuUsagePanel` (mode / scale-by-frequency).
            // Plot widget ids are Flame-specific so PlotMemory x-bounds stay
            // independent of System and session tabs.
            let mut panel = self.aggregate.as_mut().and_then(|a| a.cpu_panel.as_mut());
            // "Have symbols" gates the show-module checkbox and the
            // by-symbol Aggregate dropdown. Either kernel or user
            // symbols qualify — `BySymbol` mode collapses both kinds of
            // resolved hits into per-symbol bars and falls through to
            // raw PCs for unresolved ones, so it's useful as long as
            // *something* will collapse.
            if show_flame_cpu {
                // CPU-usage plot first — identical widget/header/gear to the
                // System tab (see `panel` above), just reused here so the
                // flame graph below can be filtered by dragging a range.
                // Drag a range to focus the flame on that interval;
                // right-click clears it. While comparing, the file names next
                // to the title switch which capture's timeline is shown; each
                // keeps its own selection.
                let primary_cpu_name = self
                    .loaded_file_name
                    .clone()
                    .unwrap_or_else(|| "this capture".to_string());
                let show_baseline = flame
                    .compare
                    .as_ref()
                    .is_some_and(|c| c.cpu_show_baseline && c.cpu_panel.is_some());
                ui.horizontal(|ui| {
                    ui.strong(RichText::new("CPU Usage").size(14.0));
                    ui.add_space(6.0);
                    if comparing && !same_file_comp {
                        let baseline_name = flame
                            .compare
                            .as_ref()
                            .map(|c| c.baseline_name.clone())
                            .unwrap_or_default();
                        if ui
                            .selectable_label(!show_baseline, &primary_cpu_name)
                            .clicked()
                        {
                            if let Some(c) = flame.compare.as_mut() {
                                c.cpu_show_baseline = false;
                            }
                        }
                        if ui.selectable_label(show_baseline, &baseline_name).clicked() {
                            if let Some(c) = flame.compare.as_mut() {
                                c.cpu_show_baseline = true;
                            }
                        }
                        ui.add_space(6.0);
                    }
                    if show_baseline {
                        if let Some(bp) = flame.compare.as_mut().and_then(|c| c.cpu_panel.as_mut())
                        {
                            bp.show_gear(
                                ui,
                                "flame_baseline_cpu",
                                &mut scale_cpu_by_freq,
                                &mut cpu_zoom,
                            );
                        }
                    } else if let Some(p) = panel.as_mut() {
                        p.show_gear(
                            ui,
                            "aggregate_cpu_panel",
                            &mut scale_cpu_by_freq,
                            &mut cpu_zoom,
                        );
                    }
                });
                if cpu_zoom && !cpu_zoom_before {
                    let (lo, hi) = (flame.filter.ts_start_us, flame.filter.ts_end_us);
                    self.agg_time_axis
                        .set_view_x(TimeAxisController::view_x_for_window(
                            capture_start_us,
                            lo,
                            hi,
                        ));
                }
                if show_baseline {
                    if let Some(compare) = flame.compare.as_mut() {
                        let current_sel =
                            if compare.base_ts_start_us > 0 || compare.base_ts_end_us != u64::MAX {
                                Some((compare.base_ts_start_us, compare.base_ts_end_us))
                            } else {
                                None
                            };
                        let change = if let Some(bp) = compare.cpu_panel.as_mut() {
                            bp.scale_by_freq = scale_cpu_by_freq;
                            bp.show_legends = show_legends;
                            let host_t0_us = bp.t0_us();
                            bp.set_plot_ids("cpu_usage_strip_flame_base");
                            bp.sync_bins(plot_bins, &flame.filter.cpus);
                            let (selection, view_us) = if cpu_zoom {
                                (None, current_sel)
                            } else {
                                (current_sel, None)
                            };
                            let change = bp.draw(ui, host_t0_us, selection, view_us, None, true);
                            if let Some((rect, tf)) = bp.last_rect_transform() {
                                let mut cpu_cursor = SysCursor::new(ui, None, plot_bins);
                                if let Some(data_x) = cpu_cursor.panel(rect, &tf, false) {
                                    cpu_cursor.add_cpu_series(
                                        &tf,
                                        data_x,
                                        &bp.cursor_series(data_x),
                                    );
                                }
                                cpu_cursor.draw(ui);
                            }
                            change
                        } else {
                            None
                        };
                        match change {
                            Some(SelectionChange::Set(lo, hi)) => {
                                compare.base_ts_start_us = lo;
                                compare.base_ts_end_us = hi;
                                needs_rebuild = true;
                            }
                            Some(SelectionChange::Clear) => {
                                compare.base_ts_start_us = 0;
                                compare.base_ts_end_us = u64::MAX;
                                needs_rebuild = true;
                            }
                            None => {}
                        }
                    }
                } else if let Some(p) = panel.as_mut() {
                    p.scale_by_freq = scale_cpu_by_freq;
                    p.show_legends = show_legends;
                    let host_t0_us = p.t0_us();
                    let current_sel =
                        if flame.filter.ts_start_us > 0 || flame.filter.ts_end_us != u64::MAX {
                            Some((flame.filter.ts_start_us, flame.filter.ts_end_us))
                        } else {
                            None
                        };
                    p.set_plot_ids("cpu_usage_strip_flame");
                    p.sync_bins(plot_bins, &flame.filter.cpus);
                    let (draw_t0, selection, view_us, origin, own_gestures) = if cpu_zoom {
                        let view_us =
                            self.agg_time_axis.window_us(capture_start_us).or_else(|| {
                                let hi_us = capture_start_us
                                    .saturating_add((agg_time_max * 1_000_000.0).round() as u64)
                                    .max(capture_start_us.saturating_add(1));
                                Some((capture_start_us, hi_us))
                            });
                        (
                            capture_start_us,
                            None,
                            view_us,
                            utc_axis_origin(
                                utc_time_labels,
                                self.file_info.as_ref(),
                                capture_start_us,
                            ),
                            false,
                        )
                    } else {
                        (
                            host_t0_us,
                            current_sel,
                            None,
                            utc_axis_origin(utc_time_labels, self.file_info.as_ref(), host_t0_us),
                            true,
                        )
                    };
                    // Region A (same-file compare) rides along as the alt band.
                    let alt_sel = flame
                        .compare
                        .as_ref()
                        .filter(|c| c.same_file)
                        .map(|c| (c.base_ts_start_us, c.base_ts_end_us));
                    let (change, alt_change) = p.draw_with_alt(
                        ui,
                        draw_t0,
                        StripSelections {
                            primary: selection,
                            alt: alt_sel,
                        },
                        view_us,
                        origin,
                        own_gestures,
                    );
                    if cpu_zoom {
                        if let Some((rect, tf)) = p.last_rect_transform() {
                            let outcome =
                                self.agg_time_axis
                                    .handle(ui, &[(rect, tf)], 0.0, agg_time_max);
                            let pointer_down = ui.input(|i| i.pointer.any_down());
                            if !pointer_down || matches!(outcome, TimeAxisOutcome::Reset { .. }) {
                                let (lo, hi) = self
                                    .agg_time_axis
                                    .window_us(capture_start_us)
                                    .unwrap_or((0, u64::MAX));
                                if flame.filter.ts_start_us != lo || flame.filter.ts_end_us != hi {
                                    flame.filter.ts_start_us = lo;
                                    flame.filter.ts_end_us = hi;
                                    needs_rebuild = true;
                                }
                            }
                            let mut cpu_cursor = SysCursor::new(ui, origin, plot_bins);
                            if let Some(data_x) = cpu_cursor.panel(rect, &tf, false) {
                                cpu_cursor.add_cpu_series(&tf, data_x, &p.cursor_series(data_x));
                            }
                            cpu_cursor.draw(ui);
                        }
                    } else {
                        match change {
                            Some(SelectionChange::Set(lo, hi)) => {
                                flame.filter.ts_start_us = lo;
                                flame.filter.ts_end_us = hi;
                                needs_rebuild = true;
                            }
                            Some(SelectionChange::Clear) => {
                                flame.filter.ts_start_us = 0;
                                flame.filter.ts_end_us = u64::MAX;
                                needs_rebuild = true;
                            }
                            None => {}
                        }
                        // Alt+drag sets region A and opens the same-file
                        // region compare; Alt+right-click on the band leaves it.
                        match alt_change {
                            Some(SelectionChange::Set(lo, hi)) => {
                                if flame.compare.as_ref().is_none_or(|c| !c.same_file) {
                                    flame.compare = Some(FlameCompareState {
                                        baseline: Arc::clone(&self.cpu_profile),
                                        baseline_name: String::new(),
                                        base_ts_start_us: lo,
                                        base_ts_end_us: hi,
                                        cpu_show_baseline: false,
                                        cpu_panel: None,
                                        swapped: false,
                                        same_file: true,
                                        diff: analysis::FlameDiff::empty(),
                                    });
                                    // Label-keyed diff tree: per-capture zoom
                                    // keys and address-scoped selectors don't
                                    // carry over, same as a file baseline.
                                    flame.zoom_path.clear();
                                    flame.back_stack.clear();
                                    flame.forward_stack.clear();
                                    flame.ctrl_pending = None;
                                    flame.hovered = None;
                                    flame.search_match_idx = None;
                                    flame.filter.include_stacks.retain(|s| {
                                        matches!(
                                            s,
                                            StackSelector::Group(_) | StackSelector::Name(_)
                                        )
                                    });
                                    flame.filter.exclude_stacks.retain(|s| {
                                        matches!(
                                            s,
                                            StackSelector::Group(_) | StackSelector::Name(_)
                                        )
                                    });
                                    self.baseline_load = None;
                                } else if let Some(compare) = flame.compare.as_mut() {
                                    compare.base_ts_start_us = lo;
                                    compare.base_ts_end_us = hi;
                                }
                                needs_rebuild = true;
                            }
                            Some(SelectionChange::Clear) if same_file_comp => {
                                clear_compare = true;
                            }
                            _ => {}
                        }
                        if let Some((rect, tf)) = p.last_rect_transform() {
                            let mut cpu_cursor = SysCursor::new(ui, origin, plot_bins);
                            if let Some(data_x) = cpu_cursor.panel(rect, &tf, false) {
                                cpu_cursor.add_cpu_series(&tf, data_x, &p.cursor_series(data_x));
                            }
                            cpu_cursor.draw(ui);
                        }
                    }
                }
                self.scale_cpu_by_freq = scale_cpu_by_freq;
                self.cpu_zoom_flame = cpu_zoom;
            }
        }
        {
            let show_tx = self.flame_layout.is_visible(PlotId::NetworkTx);
            let show_rx = self.flame_layout.is_visible(PlotId::NetworkRx);
            let tx_h = self.flame_layout.plot_height(PlotId::NetworkTx);
            let rx_h = self.flame_layout.plot_height(PlotId::NetworkRx);
            self.show_nic_plots(ui, show_tx, show_rx, tx_h, rx_h, "flame_");
        }
        let info_cpu_profile = &self.cpu_profile;
        let flame = self.flame.as_mut().unwrap();
        let comparing = flame.compare.is_some();

        // Non-wrapped horizontal: aggregation lives in the gear
        // popup, so the visible controls (Search + match nav, gear)
        // fit one line even on narrow windows. Wrapping here
        // interacts badly with the right-aligned gear layout (each
        // frame the wrapped layout re-measures available width, the
        // gear's `rect` shifts left a bit, the popup follows it →
        // "popup keeps sliding left").
        //
        //   Flame Graph  Sample CPUs [All CPUs]  [search] [n/N] [<] [>] [cost]   [?] [(i)] [⚙]
        ui.add_space(8.0);
        let mut current_match_path: Option<Vec<(u64, u32)>> = None;
        let mut search_focused = false;
        let mut cpus_changed = false;
        let cpu_filter_ui = &mut self.cpu_filter_ui;
        let baseline_loading = self.baseline_load.is_some();
        let primary_name = self
            .loaded_file_name
            .clone()
            .unwrap_or_else(|| "this capture".to_string());
        let mut open_compare = false;
        #[cfg(not(target_arch = "wasm32"))]
        let mut source_toggle = false;
        ui.horizontal(|ui| {
            // Two compounding tweaks for visible breathing room:
            //   1. item_spacing.x = 8 (vs egui's ~4 default) is the
            //      automatic gap between consecutive widgets on this
            //      row. 8 is enough that adjacent widgets don't
            //      visually merge.
            //   2. Explicit `ui.separator()` between functional
            //      groups paints a visible vertical line at higher
            //      contrast than the default — much more "this is a
            //      new section" than spacing alone.
            // The earlier 12 px-spacing-only attempt looked too
            // tight to the user three iterations in a row.
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.strong(RichText::new("Flame Graph").size(14.0));
            #[cfg(not(target_arch = "wasm32"))]
            {
                if ui
                    .selectable_label(flame.show_callstack, "Callstack")
                    .on_hover_text("Callers and callees of the current frame")
                    .clicked()
                {
                    flame.show_callstack = !flame.show_callstack;
                }
            }
            ui.add_space(4.0);
            ui.label("Sample CPUs");
            cpus_changed = cpu_filter_ui.show(ui, info_cpu_profile, &mut flame.filter.cpus);
            ui.add_space(4.0);

            // ── Search ─────────────────────────────────────────────
            // Substring / 0xADDR colour highlight + match walker.
            // `|` is OR. Next / Prev zoom to each hit, left-to-right
            // then bottom-to-top (same visual order as the canvas).
            let search_terms = flame_search_terms(&flame.search);
            let search_tree_total;
            let search_matches = if search_terms.is_empty() {
                search_tree_total = 0;
                SearchMatches {
                    paths: Vec::new(),
                    samples: 0,
                }
            } else {
                let search_root = match &flame.compare {
                    Some(c) => &c.diff.tree,
                    None => &flame.tree,
                };
                search_tree_total = search_root.total_count.max(1);
                collect_search_matches(
                    search_root,
                    info_cpu_profile,
                    flame.rel_text,
                    flame.show_module,
                    &search_terms,
                )
            };
            if let Some(i) = flame.search_match_idx {
                if i >= search_matches.paths.len() {
                    flame.search_match_idx = None;
                }
            }
            let resp = ui
                .add(
                    egui::TextEdit::singleline(&mut flame.search)
                        .desired_width(180.0)
                        .hint_text("substring, 0xADDR, a|b"),
                )
                .on_hover_text(
                    "Highlight matching frames in a brighter, more \
                     saturated colour. Substring matches against the \
                     visible label (`recvfrom`, `kernel:tcp_input`, \
                     `libc.so.7:`, `ella_server`); a `0x...` prefix \
                     matches the raw address. Separate alternatives \
                     with `|` (`ella|mrelay`). Next / Prev zoom to \
                     each match, left-to-right then bottom-to-top.",
                );
            if resp.changed() {
                flame.search_match_idx = None;
            }
            search_focused = resp.has_focus();
            if !search_terms.is_empty() {
                let n = search_matches.paths.len();
                let count_text = match flame.search_match_idx {
                    Some(i) if n > 0 => format!("{}/{}", i + 1, n),
                    _ => format!("{n}"),
                };
                ui.label(
                    RichText::new(count_text)
                        .size(12.0)
                        .color(Color32::from_gray(180)),
                )
                .on_hover_text("Matching frames (left-to-right, bottom-to-top).");
                ui.add_enabled_ui(n > 0, |ui| {
                    if ui
                        .small_button("<")
                        .on_hover_text("Previous match")
                        .clicked()
                    {
                        flame.step_search_match(&search_matches.paths, -1);
                    }
                    if ui.small_button(">").on_hover_text("Next match").clicked() {
                        flame.step_search_match(&search_matches.paths, 1);
                    }
                });
                let tree_total = search_tree_total as f64;
                let pct = search_matches.samples as f64 * 100.0 / tree_total;
                ui.label(
                    RichText::new(format!(
                        "{} ({:.2}%)",
                        fmt_count(search_matches.samples),
                        pct
                    ))
                    .size(12.0)
                    .color(Color32::from_gray(180)),
                )
                .on_hover_text(
                    "Samples in matching frames, as a share of the \
                     full tree. Nested matches are counted once.",
                );
            }
            current_match_path = flame
                .search_match_idx
                .and_then(|i| search_matches.paths.get(i).cloned());

            // ── Right-aligned cluster: [?] [(i)] [⚙] ───────────────
            // `right_to_left` lays out right-first so gear is rightmost,
            // then (i), then (?). The CPU-usage plot's own mode
            // dropdown + frequency-scaling toggle live in that plot's
            // own gear menu, on the "CPU Usage" header line above.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                show_flame_gear_menu(ui, flame, have_symbols, comparing, &mut needs_rebuild);
                show_flame_info_button(ui, info_cpu_profile, pack_status);
                show_flame_help_button(ui);
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let source_button = ui
                        .selectable_label(flame.show_source, "Source")
                        .on_hover_text(if flame.show_source {
                            "Hide the source window"
                        } else {
                            "Show source for the current frame"
                        });
                    if source_button.clicked() {
                        source_toggle = true;
                    }
                }
                // Compare… sits leftmost of the icon cluster.
                if ui
                    .add_enabled(!baseline_loading, egui::Button::new("Compare…"))
                    .on_hover_text(
                        "Compare against another capture: pick a baseline \
                         .elog and the flame merges both captures into one \
                         tree, colouring each frame by how its share of \
                         samples changed (red = grew in the current \
                         capture, blue = shrank). Bar widths cover both \
                         captures, normalized so each contributes equally.",
                    )
                    .clicked()
                {
                    open_compare = true;
                }
            });
        });
        if cpus_changed {
            needs_rebuild = true;
            self.process_functions_build = None;
        }

        if needs_rebuild {
            Self::start_flame_rebuild(flame, &self.cpu_profile, &mut self.flame_build);
            // Clear so the late apply at the bottom (canvas/chip
            // filter changes) doesn't start a second, identical build.
            needs_rebuild = false;
        }
        // `reset_zoom` is *not* applied here — the canvas below can
        // also set it (middle-click). All zoom-state mutations
        // (reset, go-back, clicked-path) are deferred to a single
        // apply block at the end of the function so they don't fight
        // the immutable borrow of `flame.tree` we take when resolving
        // `visible_root`.

        if open_compare {
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.native_file_dialog = Some(NativeFileDialog::compare_capture());
            }
            #[cfg(target_arch = "wasm32")]
            {
                let (tx, rx) = mpsc::channel();
                self.pending_baseline_pick = Some(rx);
                wasm_bindgen_futures::spawn_local(async move {
                    let handle = rfd::AsyncFileDialog::new()
                        .add_filter("EventLog", &["elog", "gz"])
                        .pick_file()
                        .await;
                    let result = match handle {
                        Some(h) => Some((h.file_name(), h.read().await)),
                        None => None,
                    };
                    let _ = tx.send(result);
                });
            }
        }

        // Compare chip row: baseline load progress, the active
        // comparison, or the last load error.
        let mut swap_compare = false;
        let mut clear_baseline_error = false;
        if let Some(load) = &self.baseline_load {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add(egui::Spinner::new().size(13.0));
                let pct = load.progress.permille() / 10;
                let status = load.progress.status();
                let text = if status.is_empty() {
                    format!("Loading baseline {} … {pct}%", load.name)
                } else {
                    format!("Loading baseline {} — {status} {pct}%", load.name)
                };
                ui.label(
                    RichText::new(text)
                        .size(12.0)
                        .color(Color32::from_gray(160)),
                );
            });
        } else if let Some(err) = &self.baseline_error {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new(format!("Baseline load failed: {err}"))
                        .size(12.0)
                        .color(Color32::from_rgb(220, 100, 90)),
                );
                if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                    clear_baseline_error = true;
                }
            });
        }
        if let Some(compare) = &flame.compare {
            // Left of the arrow is the reference side (blue anchor),
            // right is the comparison side (red anchor); Swap A/B
            // exchanges which capture fills each role.
            let (ref_total, cur_total) = compare.totals();
            let (left_total, right_total) = (ref_total, cur_total);
            let left_label;
            let right_label;
            if compare.same_file {
                let a = fmt_region_label(
                    "A",
                    compare.base_ts_start_us,
                    compare.base_ts_end_us,
                    capture_start_us,
                );
                let b = fmt_region_label(
                    "B",
                    flame.filter.ts_start_us,
                    flame.filter.ts_end_us,
                    capture_start_us,
                );
                if compare.swapped {
                    left_label = b;
                    right_label = a;
                } else {
                    left_label = a;
                    right_label = b;
                }
            } else {
                let (l, r) = compare.side_names(&primary_name);
                left_label = l.to_string();
                right_label = r.to_string();
            }
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new("Compare:")
                        .size(12.0)
                        .color(Color32::from_gray(160)),
                );
                ui.label(
                    RichText::new(format!(
                        "{} ({}) -> {} ({})",
                        left_label,
                        fmt_count(left_total),
                        right_label,
                        fmt_count(right_total),
                    ))
                    .size(12.0),
                )
                .on_hover_text(if compare.same_file {
                    "Differential flame graph: each frame's colour shows how \
                     its share of samples changed from region A to region B. \
                     Bar widths cover both regions, normalized so each \
                     contributes equally."
                } else {
                    "Differential flame graph: each frame's colour shows how \
                     its share of samples changed from the left capture to \
                     the right one. Bar widths cover both captures, \
                     normalized so each contributes equally."
                });
                let (r, g, b) = hsv_to_rgb(210.0, 0.85, 0.92);
                flame_legend_swatch(ui, Color32::from_rgb(r, g, b), "shrank");
                let (r, g, b) = hsv_to_rgb(8.0, 0.85, 0.92);
                flame_legend_swatch(ui, Color32::from_rgb(r, g, b), "grew");
                ui.label(
                    RichText::new("(left -> right)")
                        .size(12.0)
                        .color(Color32::from_gray(160)),
                );
                if ui
                    .small_button("Swap A/B")
                    .on_hover_text(
                        "Invert the comparison direction: treat the right \
                         capture as the reference and vice versa. Colours \
                         and deltas invert; bar widths are unchanged.",
                    )
                    .clicked()
                {
                    swap_compare = true;
                }
                if ui
                    .small_button("×")
                    .on_hover_text("Clear the baseline and return to the normal flame graph")
                    .clicked()
                {
                    clear_compare = true;
                }
            });
        }
        if clear_baseline_error {
            self.baseline_error = None;
        }
        if swap_compare {
            if let Some(compare) = flame.compare.as_mut() {
                compare.swapped = !compare.swapped;
            }
            flame.hovered = None;
        }
        if clear_compare {
            flame.compare = None;
            // Back to the per-capture tree: the diff zoom keys (label
            // hashes) are meaningless there.
            flame.zoom_path.clear();
            flame.back_stack.clear();
            flame.forward_stack.clear();
            flame.ctrl_pending = None;
            flame.hovered = None;
            flame.search_match_idx = None;
            needs_rebuild = true;
        }

        // Stack-filter chips (CTRL/ALT+click on the canvas) plus the
        // pending CTRL selection while it's being built.
        let has_stack_filters =
            !flame.filter.include_stacks.is_empty() || !flame.filter.exclude_stacks.is_empty();
        if has_stack_filters || flame.ctrl_pending.is_some() {
            ui.add_space(2.0);
            let mut remove_include: Option<usize> = None;
            let mut remove_exclude: Option<usize> = None;
            let mut clear_filters = false;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new("Filter:")
                        .size(12.0)
                        .color(Color32::from_gray(160)),
                );
                if !flame.filter.include_stacks.is_empty() {
                    ui.label(
                        RichText::new("only")
                            .size(12.0)
                            .color(Color32::from_gray(160)),
                    );
                    for (i, sel) in flame.filter.include_stacks.iter().enumerate() {
                        let label = selector_label(
                            sel,
                            &self.cpu_profile,
                            flame.rel_text,
                            flame.show_module,
                        );
                        if ui
                            .small_button(format!("{label} ×"))
                            .on_hover_text("Drop this include filter")
                            .clicked()
                        {
                            remove_include = Some(i);
                        }
                    }
                }
                if !flame.filter.exclude_stacks.is_empty() {
                    ui.label(
                        RichText::new("hiding")
                            .size(12.0)
                            .color(Color32::from_gray(160)),
                    );
                    for (i, sel) in flame.filter.exclude_stacks.iter().enumerate() {
                        let label = selector_label(
                            sel,
                            &self.cpu_profile,
                            flame.rel_text,
                            flame.show_module,
                        );
                        if ui
                            .small_button(format!("{label} ×"))
                            .on_hover_text("Drop this exclude filter")
                            .clicked()
                        {
                            remove_exclude = Some(i);
                        }
                    }
                }
                if let Some(pending) = &flame.ctrl_pending {
                    let cyan = Color32::from_rgb(80, 220, 250);
                    ui.label(RichText::new("selecting:").size(12.0).color(cyan));
                    for sel in pending.iter() {
                        let label = selector_label(
                            sel,
                            &self.cpu_profile,
                            flame.rel_text,
                            flame.show_module,
                        );
                        ui.label(RichText::new(label).size(12.0).color(cyan));
                    }
                    ui.label(
                        RichText::new(format!("(release {stack_modifier} to apply)"))
                            .size(12.0)
                            .color(cyan),
                    );
                }
                if has_stack_filters && ui.small_button("clear").clicked() {
                    clear_filters = true;
                }
            });
            if clear_filters {
                flame.filter.include_stacks.clear();
                flame.filter.exclude_stacks.clear();
                needs_rebuild = true;
            }
            if let Some(i) = remove_include {
                flame.filter.include_stacks.remove(i);
                needs_rebuild = true;
            }
            if let Some(i) = remove_exclude {
                flame.filter.exclude_stacks.remove(i);
                needs_rebuild = true;
            }
        }

        ui.separator();
        // The flame-graph canvas fills the rest of the central panel,
        // minus the docked source panel when one is open.
        let avail = ui.available_size();
        #[cfg(not(target_arch = "wasm32"))]
        let mut has_source = flame.show_source;
        #[cfg(target_arch = "wasm32")]
        let has_source = false;
        #[cfg(not(target_arch = "wasm32"))]
        let show_callstack = flame.show_callstack;
        #[cfg(target_arch = "wasm32")]
        let show_callstack = false;
        #[cfg(not(target_arch = "wasm32"))]
        let source_w = if has_source {
            let glyph = ui.fonts(|fonts| fonts.glyph_width(&egui::FontId::monospace(11.0), 'm'));
            let auto_w = glyph * 80.0 + 80.0 + 42.0 + 16.0;
            let w = if flame.source_width_px > 0.0 {
                flame.source_width_px
            } else {
                auto_w
            };
            w.clamp(280.0, (avail.x - 240.0).max(280.0))
        } else {
            0.0
        };
        #[cfg(target_arch = "wasm32")]
        let source_w = 0.0;
        let flame_w = avail.x - source_w;
        // Reserve a thin status strip at the bottom for the hovered
        // frame. The renderer uses the rest.
        let status_h: f32 = 22.0;
        let canvas_h = (avail.y - status_h).max(64.0);
        // One horizontal row: flame canvas, then the drag handle and
        // source panel when a source is docked. A vertical allocation
        // would drop the panel below the canvas (off screen) and leave
        // the unused width of the flame row empty.
        let (_, row) = ui.allocate_space(egui::vec2(avail.x, canvas_h));
        let flame_rect = egui::Rect::from_min_size(row.min, egui::vec2(flame_w, canvas_h));
        let canvas_resp = ui.interact(
            flame_rect,
            ui.id().with("cpu_flame_canvas"),
            if show_callstack {
                egui::Sense::hover()
            } else {
                egui::Sense::click_and_drag()
            },
        );
        let painter = ui.painter_at(flame_rect);
        let rect = flame_rect;
        #[cfg(not(target_arch = "wasm32"))]
        let sizer_w = if has_source { 8.0 } else { 0.0 };
        #[cfg(not(target_arch = "wasm32"))]
        let sizer_rect =
            egui::Rect::from_min_size(flame_rect.right_top(), egui::vec2(sizer_w, canvas_h));
        #[cfg(not(target_arch = "wasm32"))]
        let source_rect = egui::Rect::from_min_max(sizer_rect.right_top(), row.right_bottom());

        // Escape clears zoom, history, and stack filters — same as
        // middle-click. A pending CTRL selection is cancelled too.
        // Suppressed while the search box has focus (Escape there
        // surrenders focus) or a flame chrome popup is open (Escape
        // closes the popup).
        if ui.input(|i| i.key_pressed(egui::Key::Escape))
            && !search_focused
            && !chrome_popup_was_open
        {
            reset_zoom = true;
        }

        // While a background worker is aggregating the tree (large
        // native captures) show a spinner over the canvas instead of
        // the empty/stale tree, and skip all canvas interaction. We
        // still honour a `reset_zoom`/`go_back` gesture so it isn't
        // dropped mid-build. `flame_building` is always false on wasm
        // and for sub-threshold captures (those rebuild inline).
        if flame_building {
            painter.rect_filled(rect, 0.0, flame_canvas_bg(ui.visuals()));
            let c = rect.center();
            ui.put(
                egui::Rect::from_center_size(c, egui::vec2(28.0, 28.0)),
                egui::Spinner::new(),
            );
            painter.text(
                egui::pos2(c.x, c.y + 28.0),
                egui::Align2::CENTER_CENTER,
                "Building flame graph…",
                egui::FontId::proportional(15.0),
                Color32::from_gray(200),
            );
            ui.ctx().request_repaint();
            if reset_zoom {
                if flame.reset_all() {
                    Self::start_flame_rebuild(flame, &self.cpu_profile, &mut self.flame_build);
                }
            } else if go_back {
                flame.go_back();
            } else if go_forward {
                flame.go_forward();
            }
            if has_source {
                ui.ctx().request_repaint();
            }
            return;
        }

        // Mouse-history gestures, browser-style:
        //   - right-click a resolved function          -> annotated source
        //   - right-click the canvas background        -> back one snapshot
        //   - mouse Back side-button (Extra1) hovered  -> back one snapshot
        //   - mouse Fwd side-button (Extra2) hovered   -> redo one snapshot
        //   - middle-click anywhere on the canvas     -> Reset (clear path,
        //                                                history, and stack
        //                                                filters)
        // None of these require aiming at a specific bar; making the
        // gesture work over the whole canvas matches what browsers,
        // file managers, and other flame-graph tools do. We check
        // egui's input layer directly for Extra1 because Response
        // only surfaces clicked_by() for the three "main" buttons.
        let secondary_clicked = canvas_resp.secondary_clicked();
        if canvas_resp.hovered()
            && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Extra1))
        {
            go_back = true;
        }
        if canvas_resp.hovered()
            && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Extra2))
        {
            go_forward = true;
        }
        if canvas_resp.middle_clicked() {
            reset_zoom = true;
        }

        // Resolve the visible root for rendering by walking the zoom
        // path. Match on (addr, pid) so user-mode frames stay
        // pid-scoped on the way down. If the path entry is missing
        // (e.g. due to a filter change racing the click), bail out
        // at that level. In compare mode the merged diff tree is the
        // render tree (its keys are label hashes, pid always 0).
        let tree_root: &FlameNode = match &flame.compare {
            Some(c) => &c.diff.tree,
            None => &flame.tree,
        };
        // While the source panel is shown, draw from the tree root so
        // the frames under the current one stay on screen. Zoom itself
        // is unchanged, and hiding the panel returns to it.
        #[cfg(not(target_arch = "wasm32"))]
        let focus_path: Vec<(u64, u32)> = if flame.show_source && !flame.source_focus_suspended {
            flame
                .source_view
                .as_ref()
                .map(|view| view.request.stack_path.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        #[cfg(target_arch = "wasm32")]
        let focus_path: Vec<(u64, u32)> = Vec::new();
        let mut visible_root: &FlameNode = tree_root;
        if focus_path.is_empty() {
            for &(addr, pid) in &flame.zoom_path {
                match visible_root
                    .children
                    .iter()
                    .find(|c| c.addr == addr && c.pid == pid)
                {
                    Some(child) => visible_root = child,
                    None => break,
                }
            }
        }

        let visible_total = visible_root.total_count.max(1);
        // Pick a row height that lets the rendered tree fill the
        // canvas vertically: split the canvas evenly across the
        // visible subtree's actual depth so resizing the window (or
        // zooming into a shallower subtree) grows the bars instead of
        // leaving dead space above the topmost frame. Clamped to a
        // readability floor — when the canvas is too short to fit
        // every row at `min_row_h`, the deepest frames clip from the
        // top (preserves the existing "tall stack overflows the
        // viewport" behaviour rather than silently squishing labels
        // into invisibility).
        //
        // Width-aware filtering: the "min bar width" slider in the
        // gear menu can hide bars narrower than a percent of the
        // canvas. Compute the visible depth using the SAME width
        // threshold the renderer will apply below, so the canvas
        // re-fits to the surviving rows instead of leaving a tall
        // blank band above the last visible stack. (When the slider
        // is at 0 the threshold is the renderer's one-pixel floor,
        // and the filtered depth is identical to the unfiltered
        // depth — historical behaviour preserved.)
        let min_bar_w_px = (rect.width() * flame.min_bar_width_pct / 100.0).max(0.6);
        let focus_node = if focus_path.is_empty() {
            None
        } else {
            let mut node = tree_root;
            let mut found = true;
            for &(addr, pid) in &focus_path {
                match node
                    .children
                    .iter()
                    .find(|child| child.addr == addr && child.pid == pid)
                {
                    Some(child) => node = child,
                    None => {
                        found = false;
                        break;
                    }
                }
            }
            found.then_some(node)
        };
        let ancestor_trail = if focus_path.is_empty() {
            ancestor_trail_label(
                tree_root,
                &flame.zoom_path,
                &self.cpu_profile,
                flame.rel_text,
                flame.show_module,
            )
        } else {
            None
        };
        // Kernel / user mode draws those two bars as the bottom row
        // and leaves the synthetic `[all]` root off the canvas.
        let hide_root = flame.organize == FlameOrganize::KernelUser
            && visible_root.addr == 0
            && visible_root.group_label.is_none()
            && !visible_root.children.is_empty();
        let tree_depth = if let Some(node) = focus_node {
            let ancestors = if hide_root {
                focus_path.len().saturating_sub(1)
            } else {
                focus_path.len()
            };
            (ancestors + flame_subtree_depth_filtered(node, rect.width(), min_bar_w_px)).max(1)
        } else if hide_root {
            flame_children_depth(visible_root, rect.width(), min_bar_w_px)
        } else {
            flame_subtree_depth_filtered(visible_root, rect.width(), min_bar_w_px).max(1)
        };
        let min_row_h: f32 = 12.0;
        // The collapsed bar sits under the zoomed frame, so it only
        // takes a row when the canvas can hold both.
        let show_trail = ancestor_trail.is_some() && rect.height() >= min_row_h * 2.0;
        let visible_depth = (tree_depth + usize::from(show_trail)) as f32;
        let row_h: f32 = (rect.height() / visible_depth).max(min_row_h);
        let (bar_font, bar_advance) = flame_bar_font(ui, row_h);
        let max_depth = ((rect.height() / row_h).floor() as usize).max(1);
        let row_base = usize::from(show_trail);

        // Draw a faint backdrop so empty regions are visible.
        painter.rect_filled(rect, 0.0, flame_canvas_bg(ui.visuals()));

        // Iterative draw using an explicit work stack. This avoids
        // unbounded recursion for very deep stacks (we've seen 64
        // frames in the wild) and keeps the egui paint allocation
        // pattern flat.
        //
        // `path_from_root` records the (addr, pid) chain from
        // `visible_root.children[?]` down to this node (inclusive of
        // the node's own key). It is empty for the visible root bar
        // itself (canvas depth 0). On click we extend the app-level
        // `zoom_path` with this whole chain so the next frame's
        // `visible_root` resolution lands exactly on the clicked
        // node, regardless of canvas-relative depth. The pid half
        // of each entry keeps user-mode bars from being confused
        // with kernel bars that happen to share an address.
        struct DrawTask<'a> {
            node: &'a FlameNode,
            x0: f32,
            x1: f32,
            depth: usize,
            path_from_root: Vec<(u64, u32)>,
            /// Full root-to-node path hash, matching the keys
            /// `build_flame_diff` inserted into `FlameDiff::counts`.
            /// Only consulted in compare mode.
            path_hash: u64,
        }

        let mut hovered: Option<HoveredFrame> = None;
        let mut clicked_path: Option<Vec<(u64, u32)>> = None;
        let mut zoom_out_one = false;
        #[cfg(not(target_arch = "wasm32"))]
        let mut source_pc: Option<super::source::SourceJump> = None;
        #[cfg(target_arch = "wasm32")]
        let source_pc: Option<super::source::SourceJump> = None;
        #[cfg(not(target_arch = "wasm32"))]
        let mut remembered_frame: Option<super::source::SourceJump> = None;
        #[cfg(target_arch = "wasm32")]
        let remembered_frame: Option<super::source::SourceJump> = None;
        #[cfg(not(target_arch = "wasm32"))]
        let mut clear_loaded_source = false;
        #[cfg(target_arch = "wasm32")]
        let clear_loaded_source = false;
        // Modifier-click stack filtering: ALT+click excludes a stack
        // immediately; CTRL+click toggles it in the pending include
        // set (applied when CTRL is released).
        let mut exclude_sel: Option<StackSelector> = None;
        let mut pending_toggle: Option<StackSelector> = None;
        let click_mods = ui.input(|i| i.modifiers);
        let pending_sels: &[StackSelector] = flame.ctrl_pending.as_deref().unwrap_or(&[]);

        let profile_ref = &self.cpu_profile;
        let rel_text = flame.rel_text;
        let show_module = flame.show_module;
        let blue_user_palette = flame.blue_user_palette;
        // `hover_pos` is None when the pointer hasn't moved over the
        // canvas this frame — but a click still carries a position via
        // `interact_pos`, so fall back to it. Without this, a click on
        // a window that doesn't hold keyboard focus (hover events get
        // swallowed) can't hit-test any bar even though egui reports
        // the click itself.
        let hover_pos = canvas_resp
            .hover_pos()
            .or_else(|| canvas_resp.interact_pointer_pos());
        let search_terms = flame_search_terms(&flame.search);
        let search_active = !search_terms.is_empty();

        // `min_bar_w_px` was computed above (next to `visible_depth`)
        // so the depth-fitting and the renderer agree on which bars
        // count as "visible". The slider value is a percent of
        // canvas width; 0 reproduces the historical ~1 px floor,
        // anything higher hides skinny bars so the visible tree
        // reads as a tidy overview. Children dropped here cause
        // their parent to appear "shorter" — clicking the parent
        // re-grants the filtered children real width via the zoom
        // path.

        // The visible root's path hash folds the whole zoom path (its
        // own identity included), matching how the diff merge keyed it.
        let mut root_path_hash = analysis::FLAME_DIFF_PATH_SEED;
        if focus_path.is_empty() {
            for &(addr, pid) in &flame.zoom_path {
                root_path_hash = analysis::flame_diff_path_hash(root_path_hash, addr, pid);
            }
        }
        let mut tasks: Vec<DrawTask> = Vec::new();
        if hide_root {
            if focus_path.is_empty() {
                let parent_total = visible_root.total_count.max(1);
                let mut x = rect.left();
                let span = rect.right() - rect.left();
                for idx in flame_child_order(visible_root) {
                    let child = &visible_root.children[idx];
                    let cw = span * (child.total_count as f32) / parent_total as f32;
                    let cx0 = x;
                    let cx1 = (x + cw).min(rect.right());
                    x = cx1;
                    if cx1 - cx0 >= min_bar_w_px {
                        tasks.push(DrawTask {
                            node: child,
                            x0: cx0,
                            x1: cx1,
                            depth: 0,
                            path_from_root: vec![(child.addr, child.pid)],
                            path_hash: analysis::flame_diff_path_hash(
                                root_path_hash,
                                child.addr,
                                child.pid,
                            ),
                        });
                    }
                }
            } else if let Some(&(addr, pid)) = focus_path.first() {
                if let Some(child) = visible_root
                    .children
                    .iter()
                    .find(|c| c.addr == addr && c.pid == pid)
                {
                    tasks.push(DrawTask {
                        node: child,
                        x0: rect.left(),
                        x1: rect.right(),
                        depth: 0,
                        path_from_root: vec![(addr, pid)],
                        path_hash: analysis::flame_diff_path_hash(root_path_hash, addr, pid),
                    });
                }
            }
        } else {
            tasks.push(DrawTask {
                node: visible_root,
                x0: rect.left(),
                x1: rect.right(),
                depth: 0,
                path_from_root: Vec::new(),
                path_hash: root_path_hash,
            });
        }

        if !show_callstack {
            if let Some(label) = ancestor_trail.as_ref().filter(|_| show_trail) {
                let y_bot = rect.bottom();
                let y_top = rect.bottom() - row_h;
                let bar = egui::Rect::from_min_max(
                    egui::pos2(rect.left(), y_top),
                    egui::pos2(rect.right(), y_bot),
                );
                let color = Color32::from_gray(20);
                painter.rect(
                    bar,
                    1.0,
                    Color32::from_gray(140),
                    Stroke::new(0.5_f32, Color32::from_gray(20)),
                );
                if let Some(galley) = flame_fitted_galley(
                    ui,
                    label,
                    bar_font.clone(),
                    color,
                    (rect.width() - 8.0).max(0.0),
                    bar_advance,
                ) {
                    let y = (y_top + y_bot) * 0.5 - galley.rect.height() * 0.5;
                    painter.galley(egui::pos2(rect.left() + 4.0, y), galley, color);
                }
                if let Some(pos) = hover_pos {
                    if bar.contains(pos) {
                        hovered = Some(HoveredFrame {
                            label: label.clone(),
                            addr: 0,
                            pid: 0,
                            self_count: 0,
                            total_count: visible_root.total_count,
                            pct_of_visible: 100.0,
                            pct_of_total: visible_root.total_count as f64 * 100.0
                                / tree_root.total_count.max(1) as f64,
                            diff: None,
                            collapsed_below: true,
                        });
                        // The folded bar is the stack under the zoomed
                        // frame. A plain click reveals the parent.
                        if canvas_resp.clicked() && !click_mods.alt && !click_mods.command {
                            zoom_out_one = true;
                        }
                    }
                }
            }
            while let Some(task) = tasks.pop() {
                let DrawTask {
                    node,
                    x0,
                    x1,
                    depth,
                    path_from_root,
                    path_hash,
                } = task;
                if depth + row_base >= max_depth {
                    continue;
                }
                let bar_w = x1 - x0;
                // Skip rectangles narrower than the user's minimum-width
                // filter. `min_bar_w_px` defaults to ~0.6 (one screen
                // pixel) but the gear-menu slider can raise it to e.g.
                // 1% of canvas width to drop one-pixel hex-address
                // towers from the visible tree.
                if bar_w < min_bar_w_px {
                    continue;
                }
                // Bottom-up flame layout. Depth 0 is the zoomed frame;
                // `row_base` lifts it above the collapsed stack bar.
                let y_top = rect.bottom() - (depth + row_base + 1) as f32 * row_h;
                let y_bot = rect.bottom() - (depth + row_base) as f32 * row_h;
                let bar = egui::Rect::from_min_max(egui::pos2(x0, y_top), egui::pos2(x1, y_bot));

                let label = if depth == 0 && node.addr == 0 {
                    // The synthetic root only ever appears as the bottom
                    // bar when nothing is zoomed. Show the count rather
                    // than "0x0" which is meaningless. In compare mode the
                    // tree total is a normalized union, so show the raw
                    // per-capture totals instead.
                    match &flame.compare {
                        Some(compare) => {
                            let (ref_total, cur_total) = compare.totals();
                            format!(
                                "[{} -> {} samples]",
                                fmt_count(ref_total),
                                fmt_count(cur_total)
                            )
                        }
                        None => format!("[all {} samples]", fmt_count(visible_root.total_count)),
                    }
                } else {
                    flame_node_label(node, profile_ref, rel_text, show_module)
                };

                let highlight = search_active && flame_search_hit(&label, node.addr, &search_terms);
                let is_current_match = current_match_path.as_ref().is_some_and(|target| {
                    flame.zoom_path.len() + path_from_root.len() == target.len()
                        && flame
                            .zoom_path
                            .iter()
                            .chain(path_from_root.iter())
                            .eq(target.iter())
                });
                // Pending CTRL-selection bars get a cyan outline so the
                // accumulated set is visible before release applies it.
                let is_pending = pending_sels.iter().any(|sel| match sel {
                    StackSelector::Frame(a, p) => node.addr == *a && node.pid == *p,
                    StackSelector::Group(l) | StackSelector::Name(l) => {
                        node.group_label.as_deref() == Some(l.as_str())
                    }
                });

                let is_kernel_frame = node.group_label.is_none()
                    && node.addr != 0
                    && analysis::flame_pc_is_kernel(node.addr);

                let bg = if !focus_path.is_empty()
                    && path_from_root == focus_path
                    && ui.visuals().dark_mode
                {
                    Color32::WHITE
                } else if depth == 0 && node.addr == 0 {
                    // Lighter than the dark canvas so the "[all N samples]" label
                    // (dark text) stays readable, and still distinct from frames.
                    Color32::from_gray(140)
                } else if let Some(compare) = &flame.compare {
                    // Compare mode: every bar is delta-coloured (the merged
                    // tree's group_label / addr fields are label identity,
                    // not palette inputs).
                    if highlight {
                        flame_search_color(node.addr)
                    } else {
                        let counts = compare
                            .diff
                            .counts
                            .get(&path_hash)
                            .copied()
                            .unwrap_or_default();
                        let (b, t) = compare.ref_cur(counts);
                        let (bt, tt) = compare.totals();
                        flame_diff_color(b, t, bt, tt)
                    }
                } else if node.group_label.is_some() {
                    flame_group_color(node, highlight)
                } else if node.is_self_pc {
                    if blue_user_palette && !is_kernel_frame {
                        flame_user_offset_color(node.addr, highlight)
                    } else {
                        flame_self_pc_color(node.addr, highlight)
                    }
                } else if blue_user_palette && !is_kernel_frame {
                    flame_user_color(node.addr, highlight)
                } else {
                    flame_color(node.addr, highlight)
                };
                // Slightly heavier outline on overlay children to give
                // them a "stitched onto the parent bar" feel rather than
                // reading as a sibling callee.
                let stroke = if is_pending {
                    Stroke::new(1.5_f32, Color32::from_rgb(80, 220, 250))
                } else if is_current_match {
                    Stroke::new(1.5_f32, Color32::from_rgb(250, 250, 250))
                } else if node.is_self_pc {
                    Stroke::new(0.75_f32, Color32::from_gray(40))
                } else {
                    Stroke::new(0.5_f32, Color32::from_gray(20))
                };
                painter.rect(bar, 1.0, bg, stroke);

                // Label only fits if the bar is wide enough for ~5
                // characters. egui's default Body font is ~7 px/char at
                // size 11, so threshold is ~35 px; use 30 to err on the
                // side of "show something".
                if bar_w >= 30.0 {
                    let color = Color32::from_gray(20);
                    if let Some(galley) = flame_fitted_galley(
                        ui,
                        &label,
                        bar_font.clone(),
                        color,
                        (bar_w - 8.0).max(0.0),
                        bar_advance,
                    ) {
                        let y = (y_top + y_bot) * 0.5 - galley.rect.height() * 0.5;
                        painter.galley(egui::pos2(x0 + 4.0, y), galley, color);
                    }
                }

                // Hover hit detection. The bottom bar of the visible root
                // is also clickable but clicking it does nothing useful
                // (we're already there); we just emit hover info.
                if let Some(pos) = hover_pos {
                    if bar.contains(pos) {
                        // For overlay children the on-bar label is just
                        // "+0xINNER"; the tooltip / status strip should
                        // include the function name too so the user
                        // doesn't have to look at the bar below for
                        // context.
                        let hover_label = if node.is_self_pc {
                            flame_label(node.addr, node.pid, profile_ref, rel_text, show_module)
                        } else {
                            label.clone()
                        };
                        hovered = Some(HoveredFrame {
                            label: hover_label,
                            addr: node.addr,
                            pid: node.pid,
                            self_count: node.self_count,
                            total_count: node.total_count,
                            pct_of_visible: node.total_count as f64 * 100.0 / visible_total as f64,
                            pct_of_total: node.total_count as f64 * 100.0
                                / tree_root.total_count.max(1) as f64,
                            diff: flame.compare.as_ref().map(|compare| {
                                compare
                                    .diff
                                    .counts
                                    .get(&path_hash)
                                    .copied()
                                    .unwrap_or_default()
                            }),
                            collapsed_below: false,
                        });
                        if canvas_resp.clicked() && (depth > 0 || hide_root) {
                            // Compare bars are keyed by display name, so
                            // the filter has to be a name both captures
                            // can match. Single-capture bars stay
                            // address- or group-scoped. The `user` bar
                            // is a mode bucket, matched as a group.
                            let sel = if comparing {
                                match node.group_label.as_deref() {
                                    Some(analysis::USER_GROUP_LABEL) => {
                                        StackSelector::Group(analysis::USER_GROUP_LABEL.to_string())
                                    }
                                    _ => StackSelector::Name(
                                        node.group_label.clone().unwrap_or_else(|| label.clone()),
                                    ),
                                }
                            } else {
                                selector_for_node(node)
                            };
                            if click_mods.alt {
                                exclude_sel = Some(sel);
                            } else if click_mods.command {
                                pending_toggle = Some(sel);
                            } else {
                                clicked_path = Some(path_from_root.clone());
                                #[cfg(not(target_arch = "wasm32"))]
                                if node.group_label.is_none()
                                    && analysis::resolve_profile_function(
                                        profile_ref,
                                        node.addr,
                                        node.pid,
                                    )
                                    .is_some()
                                {
                                    let jump = super::source::SourceJump {
                                        pc: node.addr,
                                        pid: node.pid,
                                        stack_path: source_stack_path(
                                            &flame.zoom_path,
                                            &path_from_root,
                                            !focus_path.is_empty(),
                                            node.is_self_pc,
                                        ),
                                    };
                                    if flame.show_source {
                                        source_pc = Some(jump);
                                    } else {
                                        remembered_frame = Some(jump);
                                    }
                                } else if flame.show_source {
                                    clear_loaded_source = true;
                                }
                            }
                        }
                        #[cfg(not(target_arch = "wasm32"))]
                        if secondary_clicked
                            && depth > 0
                            && node.group_label.is_none()
                            && analysis::resolve_profile_function(profile_ref, node.addr, node.pid)
                                .is_some()
                        {
                            source_pc = Some(super::source::SourceJump {
                                pc: node.addr,
                                pid: node.pid,
                                stack_path: source_stack_path(
                                    &flame.zoom_path,
                                    &path_from_root,
                                    !focus_path.is_empty(),
                                    node.is_self_pc,
                                ),
                            });
                        }
                    }
                }

                // Lay out children. Each child gets a slice of the parent
                // bar proportional to its `total_count`. We sort children
                // by total_count descending so the visually-dominant
                // branches always end up on the left, which reads as the
                // "primary" path. Sort the indices, not the children
                // themselves, to keep the borrow checker happy.
                if !node.children.is_empty() && depth + row_base + 1 < max_depth {
                    let on_focus_chain = path_from_root.len() < focus_path.len()
                        && focus_path.starts_with(&path_from_root);
                    if on_focus_chain {
                        let (addr, pid) = focus_path[path_from_root.len()];
                        if let Some(child) = node
                            .children
                            .iter()
                            .find(|child| child.addr == addr && child.pid == pid)
                        {
                            let mut child_path = path_from_root.clone();
                            child_path.push((child.addr, child.pid));
                            tasks.push(DrawTask {
                                node: child,
                                x0,
                                x1,
                                depth: depth + 1,
                                path_from_root: child_path,
                                path_hash: analysis::flame_diff_path_hash(
                                    path_hash, child.addr, child.pid,
                                ),
                            });
                        }
                    } else {
                        let parent_total = node.total_count.max(1);
                        let mut x = x0;
                        for idx in flame_child_order(node) {
                            let child = &node.children[idx];
                            let frac = child.total_count as f32 / parent_total as f32;
                            let cw = bar_w * frac;
                            let cx0 = x;
                            let cx1 = (x + cw).min(x1);
                            x = cx1;
                            if cx1 - cx0 >= min_bar_w_px {
                                let mut child_path = path_from_root.clone();
                                child_path.push((child.addr, child.pid));
                                tasks.push(DrawTask {
                                    node: child,
                                    x0: cx0,
                                    x1: cx1,
                                    depth: depth + 1,
                                    path_from_root: child_path,
                                    path_hash: analysis::flame_diff_path_hash(
                                        path_hash, child.addr, child.pid,
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }

        flame.hovered = hovered;
        if secondary_clicked && source_pc.is_none() {
            go_back = true;
        }

        // Click application is deferred to the bottom of the
        // function — same reason as `reset_zoom` and `go_back`: we
        // hold an immutable borrow of `flame.tree` via
        // `visible_root` through the status strip, so any
        // `&mut flame` call has to come after that borrow ends.

        // Also surface a regular egui tooltip (multi-line). The bottom
        // status strip still shows a one-line summary even when the
        // user isn't hovering.
        if hover_pos.is_some() {
            if let Some(hov) = &flame.hovered {
                let tooltip = if hov.collapsed_below {
                    format!(
                        "{}\n\n{} samples ({:.2}% of visible, {:.2}% of total)",
                        hov.label,
                        fmt_count(hov.total_count),
                        hov.pct_of_visible,
                        hov.pct_of_total,
                    )
                } else {
                    match (&flame.compare, hov.diff) {
                        (Some(compare), Some(d)) => {
                            let line = diff_hover_line(compare, &d);
                            let (ref_self, cur_self) = if compare.swapped {
                                (d.target_self, d.base_self)
                            } else {
                                (d.base_self, d.target_self)
                            };
                            format!(
                                "{}\n\n{}\nself {} -> {}",
                                hov.label,
                                line,
                                fmt_count(ref_self),
                                fmt_count(cur_self),
                            )
                        }
                        _ => format!(
                            "{}\n\n{} samples ({:.2}% of visible, {:.2}% of total)\n{} self ({} children)",
                            hov.label,
                            fmt_count(hov.total_count),
                            hov.pct_of_visible,
                            hov.pct_of_total,
                            fmt_count(hov.self_count),
                            fmt_count(hov.total_count - hov.self_count),
                        ),
                    }
                };
                // Manual tooltip placement: pick a corner anchor based
                // on which quadrant of the canvas the cursor is in,
                // so the tooltip flows AWAY from the bar under the
                // pointer instead of obscuring it (egui's built-in
                // `show_tooltip_at_pointer` always drops the tooltip
                // down-and-right of the pointer, which covers the
                // hovered rectangle whenever the cursor is near the
                // right or bottom edge of the canvas).
                if let Some(pos) = hover_pos {
                    let center = rect.center();
                    let on_right = pos.x > center.x;
                    let on_bottom = pos.y > center.y;
                    let pivot = match (on_right, on_bottom) {
                        (false, false) => egui::Align2::LEFT_TOP,
                        (true, false) => egui::Align2::RIGHT_TOP,
                        (false, true) => egui::Align2::LEFT_BOTTOM,
                        (true, true) => egui::Align2::RIGHT_BOTTOM,
                    };
                    // Small offset so the tooltip never sits directly
                    // under the cursor (which would block hover and
                    // also feel clipped against the bar edge).
                    let nudge = 14.0;
                    let anchor = egui::pos2(
                        if on_right {
                            pos.x - nudge
                        } else {
                            pos.x + nudge
                        },
                        if on_bottom {
                            pos.y - nudge
                        } else {
                            pos.y + nudge
                        },
                    );
                    egui::Area::new(egui::Id::new("flame_tooltip"))
                        .order(egui::Order::Tooltip)
                        .fixed_pos(anchor)
                        .pivot(pivot)
                        .interactable(false)
                        .show(ui.ctx(), |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.label(RichText::new(tooltip).monospace().size(12.0));
                            });
                        });
                }
            }
        }

        // Status strip at the bottom.
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if let Some(h) = &flame.hovered {
                let text = match (&flame.compare, h.diff) {
                    (Some(compare), Some(d)) => {
                        format!("{} | {}", h.label, diff_hover_line(compare, &d))
                    }
                    _ => format!(
                        "{} | {} samples ({:.2}% visible, {:.2}% total)",
                        h.label,
                        fmt_count(h.total_count),
                        h.pct_of_visible,
                        h.pct_of_total,
                    ),
                };
                ui.label(RichText::new(text).size(12.0).monospace());
            } else if let Some(compare) = &flame.compare {
                let (ref_total, cur_total) = compare.totals();
                ui.label(
                    RichText::new(format!(
                        "Compare mode: bar widths are normalized -- {} reference + \
                         {} current samples. Hover a frame for per-capture \
                         counts; click zooms.",
                        fmt_count(ref_total),
                        fmt_count(cur_total),
                    ))
                    .size(12.0)
                    .color(Color32::from_gray(160)),
                );
            } else {
                ui.label(
                    RichText::new(format!(
                        "Hover a frame for details. Click zooms; \
                         {stack_modifier}-click keeps stacks (release applies); \
                         ALT-click hides a stack. \
                         Showing {} of {} samples in {} frames at root.",
                        fmt_count(visible_root.total_count),
                        fmt_count(tree_root.total_count),
                        visible_root.children.len(),
                    ))
                    .size(12.0)
                    .color(Color32::from_gray(160)),
                );
            }
        });

        // Final apply of all state mutations gathered above:
        //   - exclude_sel: ALT+click filtered a stack out (rebuild).
        //   - pending_toggle: CTRL+click toggled a selector in the
        //     pending include set (applied on CTRL release, at the
        //     top of this function).
        //   - reset_zoom: canvas middle-click or Escape. Wins over
        //     the navigation actions and also drops stack filters.
        //   - zoom_out_one: primary-click on the folded stack bar.
        //   - clicked_path: primary-click on a frame.
        //   - go_back: canvas right-click or mouse Back side-button.
        //   - go_forward: mouse Forward side-button.
        //
        // Applied here (after every read of `flame.tree` /
        // `visible_root` has dropped) because the renderer holds an
        // immutable borrow of `flame` via `visible_root` for the
        // entire draw + status-strip pass. The next frame re-resolves
        // `visible_root` from the new `zoom_path`.
        if let Some(sel) = exclude_sel {
            flame.filter.include_stacks.retain(|s| s != &sel);
            if !flame.filter.exclude_stacks.contains(&sel) {
                flame.filter.exclude_stacks.push(sel);
            }
            needs_rebuild = true;
        }
        if let Some(sel) = pending_toggle {
            let pending = flame
                .ctrl_pending
                .get_or_insert_with(|| flame.filter.include_stacks.clone());
            if let Some(i) = pending.iter().position(|s| s == &sel) {
                pending.remove(i);
            } else {
                pending.push(sel);
            }
        }
        if reset_zoom {
            if flame.reset_all() {
                needs_rebuild = true;
            }
        } else if zoom_out_one {
            flame.navigate(|zp| {
                zp.pop();
            });
        } else if let Some(path) = clicked_path {
            flame.navigate(|zp| zp.extend(path));
        } else if go_back {
            flame.go_back();
        } else if go_forward {
            flame.go_forward();
        }
        // Late rebuild apply for the filter changes gathered during
        // the draw (the early apply above only saw the gear menu's).
        // A rebuilt tree can drop the zoom path the docked source is
        // anchored to, so the panel closes along with the rebuild.
        #[cfg(not(target_arch = "wasm32"))]
        if needs_rebuild {
            flame.source_view = None;
            flame.show_source = false;
            flame.show_callstack = false;
        }
        if needs_rebuild {
            Self::start_flame_rebuild(flame, &self.cpu_profile, &mut self.flame_build);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if source_pc.is_some() {
            flame.source_view = None;
            has_source = false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if clear_loaded_source {
            flame.source_view = None;
        }
        #[cfg(target_arch = "wasm32")]
        let _ = clear_loaded_source;
        #[cfg(not(target_arch = "wasm32"))]
        let mut callstack_zoom: Option<Vec<(u64, u32)>> = None;
        #[cfg(not(target_arch = "wasm32"))]
        let mut callstack_jump_apply = None;
        // The function callstack follows the retained source, so hiding
        // the panel does not switch it to the layer list.
        #[cfg(not(target_arch = "wasm32"))]
        if show_callstack {
            if let Some(mut source_state) = flame.source_view.take() {
                source_state.poll();
                let mut callstack_jump = None;
                let mut callstack_ui =
                    ui.child_ui(flame_rect, egui::Layout::top_down(egui::Align::LEFT));
                super::source::show_callstack_panel(
                    &mut callstack_ui,
                    &mut source_state,
                    &mut callstack_jump,
                    &mut callstack_zoom,
                );
                if callstack_zoom.is_none() {
                    flame.source_view = Some(source_state);
                    callstack_jump_apply = callstack_jump;
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if has_source {
            let sizer_resp = ui
                .interact(
                    sizer_rect,
                    ui.id().with("cpu_flame_source_sizer"),
                    egui::Sense::click_and_drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if sizer_resp.dragged() {
                let new_w = (source_w - sizer_resp.drag_delta().x)
                    .clamp(280.0, (avail.x - 240.0).max(280.0));
                flame.source_width_px = new_w;
            }
            ui.painter().rect_filled(
                sizer_rect.shrink2(egui::vec2(3.0, 0.0)),
                1.0,
                Color32::from_gray(60),
            );
            // Take the panel out of the flame so the renderer can borrow
            // `self` for rewrites / preferences; put it back unless the
            // panel closed or navigated to another function.
            if let Some(mut source_state) = flame.source_view.take() {
                source_state.poll();
                let mut panel_ui =
                    ui.child_ui(source_rect, egui::Layout::top_down(egui::Align::LEFT));
                let action = self.show_flame_source_panel(&mut panel_ui, &mut source_state);
                match action {
                    SourcePanelAction::Open(jump) => self.open_flame_source(jump),
                    SourcePanelAction::FocusLine | SourcePanelAction::None => {
                        self.flame.as_mut().unwrap().source_view = Some(source_state);
                    }
                }
            } else {
                let mut panel_ui =
                    ui.child_ui(source_rect, egui::Layout::top_down(egui::Align::LEFT));
                panel_ui.add_space(12.0);
                panel_ui.label(
                    RichText::new("Click a frame to see its source.")
                        .color(Color32::from_gray(170)),
                );
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let mut layer_click = None;
        #[cfg(not(target_arch = "wasm32"))]
        if show_callstack
            && callstack_zoom.is_none()
            && self
                .flame
                .as_ref()
                .is_some_and(|flame| flame.source_view.is_none())
        {
            ui.painter()
                .rect_filled(flame_rect, 0.0, flame_canvas_bg(ui.visuals()));
            let mut callstack_ui =
                ui.child_ui(flame_rect, egui::Layout::top_down(egui::Align::LEFT));
            let flame = self.flame.as_ref().unwrap();
            let tree = match &flame.compare {
                Some(compare) => &compare.diff.tree,
                None => &flame.tree,
            };
            layer_click = show_tree_callstack(
                &mut callstack_ui,
                tree,
                &flame.zoom_path,
                &self.cpu_profile,
                flame.organize == FlameOrganize::KernelUser,
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = callstack_zoom {
            if let Some(flame) = self.flame.as_mut() {
                flame.navigate(|zoom| *zoom = path);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(click) = layer_click {
            let open_source = self
                .flame
                .as_ref()
                .is_some_and(|flame| flame.show_source && click.jump.is_some());
            if let Some(flame) = self.flame.as_mut() {
                flame.navigate(|zoom| *zoom = click.path);
                if flame.show_source && click.jump.is_none() {
                    flame.source_view = None;
                }
            }
            if open_source {
                if let Some(jump) = click.jump {
                    self.open_flame_source(jump);
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(jump) = remembered_frame {
            if let Some(flame) = self.flame.as_mut() {
                flame.source_frame = Some(jump);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = remembered_frame;
        #[cfg(not(target_arch = "wasm32"))]
        if source_toggle {
            let opening = self.flame.as_ref().is_some_and(|flame| !flame.show_source);
            let retained = self
                .flame
                .as_ref()
                .is_some_and(|flame| flame.source_view.is_some());
            if let Some(flame) = self.flame.as_mut() {
                flame.show_source = !flame.show_source;
            }
            // A later show puts the same view back. Load only the first
            // time, when nothing is retained.
            if opening && !retained {
                let jump = self
                    .flame
                    .as_ref()
                    .and_then(|flame| source_jump_at_zoom(flame, &self.cpu_profile));
                if let Some(jump) = jump {
                    self.open_flame_source(jump);
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let opened_from_click = source_pc.is_some();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(jump) = source_pc {
            self.open_flame_source(jump);
        }
        // Search, back, and forward move the zoom without a source click.
        // Follow that frame. A middle-click reset does not set the flag,
        // so the panel stays on the function it already had.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let follow = self
                .flame
                .as_ref()
                .is_some_and(|flame| flame.source_follow_zoom);
            if let Some(flame) = self.flame.as_mut() {
                flame.source_follow_zoom = false;
            }
            if follow && !opened_from_click {
                let show = self.flame.as_ref().is_some_and(|flame| flame.show_source);
                if show {
                    let jump = self
                        .flame
                        .as_ref()
                        .and_then(|flame| source_jump_at_zoom(flame, &self.cpu_profile));
                    if let Some(jump) = jump {
                        self.open_flame_source(jump);
                    } else if let Some(flame) = self.flame.as_mut() {
                        flame.source_view = None;
                        flame.source_focus_suspended = false;
                    }
                }
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(jump) = callstack_jump_apply {
            self.open_flame_source(jump);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct LayerClick {
    path: Vec<(u64, u32)>,
    jump: Option<super::source::SourceJump>,
}

#[cfg(not(target_arch = "wasm32"))]
fn source_jump_for_node(
    node: &FlameNode,
    mut path: Vec<(u64, u32)>,
    profile: &CpuProfile,
) -> Option<super::source::SourceJump> {
    if node.addr == 0 || node.group_label.is_some() {
        return None;
    }
    analysis::resolve_profile_function(profile, node.addr, node.pid)?;
    if node.is_self_pc {
        path.pop();
    }
    Some(super::source::SourceJump {
        pc: node.addr,
        pid: node.pid,
        stack_path: path,
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn source_jump_at_zoom(
    flame: &FlameViewState,
    profile: &CpuProfile,
) -> Option<super::source::SourceJump> {
    let tree = match &flame.compare {
        Some(compare) => &compare.diff.tree,
        None => &flame.tree,
    };
    let mut node = tree;
    let mut path = Vec::new();
    for &(addr, pid) in &flame.zoom_path {
        node = node
            .children
            .iter()
            .find(|child| child.addr == addr && child.pid == pid)?;
        path.push((addr, pid));
    }
    source_jump_for_node(node, path, profile)
}

#[cfg(not(target_arch = "wasm32"))]
fn tree_frame_label(node: &FlameNode, profile: &CpuProfile) -> String {
    if node.addr == 0 && node.group_label.is_none() {
        return "all".to_string();
    }
    flame_node_label(node, profile, true, false)
}

#[cfg(not(target_arch = "wasm32"))]
fn callstack_pct(count: u64, total: u64) -> String {
    let pct = count as f64 * 100.0 / total.max(1) as f64;
    if count > 0 && pct < 0.1 {
        "<0.1%".to_string()
    } else {
        format!("{pct:.1}%")
    }
}

/// Unzoomed kernel/user callstack: both base bars, then the row above each.
#[cfg(not(target_arch = "wasm32"))]
fn show_kernel_user_callstack(
    ui: &mut egui::Ui,
    tree: &FlameNode,
    profile: &CpuProfile,
) -> Option<LayerClick> {
    let mut clicked: Option<LayerClick> = None;
    let total = tree.total_count.max(1);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_source("tree_callstack")
        .show(ui, |ui| {
            for idx in flame_child_order(tree) {
                let base = &tree.children[idx];
                if base.total_count == 0 {
                    continue;
                }
                let base_path = vec![(base.addr, base.pid)];
                ui.horizontal(|ui| {
                    ui.monospace(format!("{:>6}", callstack_pct(base.total_count, total)));
                    let resp = ui.add(
                        egui::Label::new(
                            RichText::new(tree_frame_label(base, profile))
                                .monospace()
                                .size(11.0)
                                .strong(),
                        )
                        .selectable(false)
                        .sense(egui::Sense::click()),
                    );
                    resp.clone().on_hover_text("Show this frame");
                    if resp.clicked() {
                        clicked = Some(LayerClick {
                            jump: source_jump_for_node(base, base_path.clone(), profile),
                            path: base_path.clone(),
                        });
                    }
                });
                let child_total = base.total_count.max(1);
                for child_index in flame_child_order(base) {
                    let child = &base.children[child_index];
                    if child.total_count == 0 {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.monospace(format!(
                            "{:>6}",
                            callstack_pct(child.total_count, child_total)
                        ));
                        let resp = ui.add(
                            egui::Label::new(
                                RichText::new(tree_frame_label(child, profile))
                                    .monospace()
                                    .size(11.0),
                            )
                            .selectable(false)
                            .sense(egui::Sense::click()),
                        );
                        resp.clone().on_hover_text("Show this frame");
                        if resp.clicked() {
                            let mut path = base_path.clone();
                            path.push((child.addr, child.pid));
                            clicked = Some(LayerClick {
                                jump: source_jump_for_node(child, path.clone(), profile),
                                path,
                            });
                        }
                    });
                }
                if base.self_count > 0 {
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.monospace(format!(
                            "{:>5.1}%",
                            base.self_count as f64 * 100.0 / child_total as f64
                        ));
                        ui.label(
                            RichText::new("(self)")
                                .monospace()
                                .size(11.0)
                                .color(Color32::from_gray(150)),
                        );
                    });
                }
            }
        });
    clicked
}

#[cfg(not(target_arch = "wasm32"))]
fn show_tree_callstack(
    ui: &mut egui::Ui,
    tree: &FlameNode,
    zoom_path: &[(u64, u32)],
    profile: &CpuProfile,
    kernel_user_base: bool,
) -> Option<LayerClick> {
    let skip_root =
        kernel_user_base && tree.addr == 0 && tree.group_label.is_none() && !zoom_path.is_empty();
    if kernel_user_base && tree.addr == 0 && tree.group_label.is_none() && zoom_path.is_empty() {
        return show_kernel_user_callstack(ui, tree, profile);
    }
    struct Step<'a> {
        node: &'a FlameNode,
        path: Vec<(u64, u32)>,
    }
    let mut chain = Vec::new();
    if !skip_root {
        chain.push(Step {
            node: tree,
            path: Vec::new(),
        });
    }
    let mut parent = tree;
    for &(addr, pid) in zoom_path {
        let Some(child) = parent
            .children
            .iter()
            .find(|child| child.addr == addr && child.pid == pid)
        else {
            break;
        };
        let mut path = chain
            .last()
            .map(|step| step.path.clone())
            .unwrap_or_default();
        path.push((addr, pid));
        chain.push(Step { node: child, path });
        parent = child;
    }
    if chain.is_empty() {
        return None;
    }
    let current = chain.len() - 1;
    let mut clicked: Option<LayerClick> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_source("tree_callstack")
        .show(ui, |ui| {
            let num_width = if chain.len() >= 10 { 2 } else { 1 };
            for (index, step) in chain.iter().enumerate() {
                let is_current = index == current;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("#{index:>width$}", width = num_width))
                            .monospace()
                            .size(11.0)
                            .color(if is_current {
                                Color32::from_gray(210)
                            } else {
                                Color32::from_gray(130)
                            }),
                    );
                    let text = RichText::new(tree_frame_label(step.node, profile))
                        .monospace()
                        .size(11.0);
                    let text = if is_current { text.strong() } else { text };
                    let resp = ui.add(
                        egui::Label::new(text)
                            .selectable(false)
                            .sense(egui::Sense::click()),
                    );
                    if !is_current {
                        resp.clone().on_hover_text("Show this frame");
                        if resp.clicked() {
                            clicked = Some(LayerClick {
                                jump: source_jump_for_node(step.node, step.path.clone(), profile),
                                path: step.path.clone(),
                            });
                        }
                    }
                });
                if !is_current {
                    continue;
                }
                let total = step.node.total_count.max(1);
                let mut order: Vec<usize> = (0..step.node.children.len()).collect();
                order.sort_by(|&left, &right| {
                    step.node.children[right]
                        .total_count
                        .cmp(&step.node.children[left].total_count)
                        .then(left.cmp(&right))
                });
                for child_index in order {
                    let child = &step.node.children[child_index];
                    if child.total_count == 0 {
                        continue;
                    }
                    let pct = child.total_count as f64 * 100.0 / total as f64;
                    let pct_label = if pct < 0.1 {
                        "<0.1%".to_string()
                    } else {
                        format!("{pct:.1}%")
                    };
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.monospace(format!("{pct_label:>6}"));
                        let resp = ui.add(
                            egui::Label::new(
                                RichText::new(tree_frame_label(child, profile))
                                    .monospace()
                                    .size(11.0),
                            )
                            .selectable(false)
                            .sense(egui::Sense::click()),
                        );
                        resp.clone().on_hover_text("Show this frame");
                        if resp.clicked() {
                            let mut path = step.path.clone();
                            path.push((child.addr, child.pid));
                            clicked = Some(LayerClick {
                                jump: source_jump_for_node(child, path.clone(), profile),
                                path,
                            });
                        }
                    });
                }
                if step.node.self_count > 0 {
                    let pct = step.node.self_count as f64 * 100.0 / total as f64;
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.monospace(format!("{pct:>5.1}%"));
                        ui.label(
                            RichText::new("(self)")
                                .monospace()
                                .size(11.0)
                                .color(Color32::from_gray(150)),
                        );
                    });
                }
            }
        });
    clicked
}

#[cfg(not(target_arch = "wasm32"))]
fn source_stack_path(
    zoom_path: &[(u64, u32)],
    path_from_root: &[(u64, u32)],
    focus_active: bool,
    is_self_pc: bool,
) -> Vec<(u64, u32)> {
    let mut path = if focus_active {
        path_from_root.to_vec()
    } else {
        let mut abs = zoom_path.to_vec();
        abs.extend(path_from_root.iter().copied());
        abs
    };
    if is_self_pc {
        path.pop();
    }
    path
}

#[cfg(test)]
mod tests {
    use super::egui::Color32;
    use super::{
        ancestor_trail_label, collect_search_matches, flame_canvas_bg, flame_color,
        flame_search_color, flame_search_hit, flame_search_terms, flame_self_pc_color,
        flame_user_color, flame_user_offset_color, zoom_path_for_process, zoom_path_for_target,
        CpuProfile, FlameAggregation, FlameFilter, FlameNode, FlameOrganize, FlameViewState,
        FlameZoomTarget, StackSelector,
    };
    use crate::analysis;
    use crate::elog_parser;

    fn nav_state() -> FlameViewState {
        FlameViewState {
            tree: FlameNode {
                addr: 0,
                pid: 0,
                total_count: 0,
                self_count: 0,
                children: Vec::new(),
                is_self_pc: false,
                group_label: None,
            },
            zoom_path: Vec::new(),
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            ctrl_pending: None,
            ctrl_cancelled: false,
            search: String::new(),
            search_match_idx: None,
            filter: FlameFilter::new(),
            rel_text: true,
            show_module: false,
            aggregation: FlameAggregation::ByPc,
            split_user_by_pid: false,
            organize: FlameOrganize::ByProcess,
            blue_user_palette: false,
            hovered: None,
            min_bar_width_pct: 0.0,
            compare: None,
            #[cfg(not(target_arch = "wasm32"))]
            source_view: None,
            #[cfg(not(target_arch = "wasm32"))]
            source_width_px: 0.0,
            #[cfg(not(target_arch = "wasm32"))]
            show_callstack: false,
            #[cfg(not(target_arch = "wasm32"))]
            show_source: false,
            #[cfg(not(target_arch = "wasm32"))]
            source_focus_suspended: false,
            #[cfg(not(target_arch = "wasm32"))]
            source_follow_zoom: false,
            #[cfg(not(target_arch = "wasm32"))]
            source_frame: None,
        }
    }

    #[test]
    fn split_palette_keeps_kernel_warm_and_user_blue() {
        let user = flame_color(0x401000, false);
        assert!(user.r() > user.b());
        let mut min_g = 255u8;
        let mut max_g = 0u8;
        for addr in [
            0x401000u64,
            0x401080,
            0x402000,
            0x410000,
            0x500000,
            0x7fff_0000,
        ] {
            let blue_user = flame_user_color(addr, false);
            assert!(blue_user.b() > blue_user.r(), "{addr:#x} {blue_user:?}");
            assert!(blue_user.b() >= 190, "{addr:#x} {blue_user:?}");
            min_g = min_g.min(blue_user.g());
            max_g = max_g.max(blue_user.g());
        }
        assert!(max_g - min_g > 25, "green channel {min_g}..{max_g}");

        let overlay = flame_self_pc_color(0x401010, false);
        assert!(overlay.g() > overlay.r() && overlay.g() > overlay.b());
        let user_offset = flame_user_offset_color(0x401010, false);
        assert!(user_offset.r() > user_offset.g() && user_offset.b() > user_offset.g());
        assert_eq!(
            flame_user_offset_color(0x401010, true),
            flame_search_color(0x401010)
        );

        let addr = 0x401020;
        assert_eq!(flame_user_color(addr, true), flame_search_color(addr));
    }

    #[test]
    fn light_canvas_matches_panel_fill() {
        let light = super::egui::Visuals::light();
        assert_eq!(flame_canvas_bg(&light), light.panel_fill);
        assert_eq!(
            flame_canvas_bg(&super::egui::Visuals::dark()),
            Color32::from_gray(28)
        );
    }

    #[test]
    fn forward_stack_redoes_undone_zooms() {
        let mut f = nav_state();
        f.navigate(|zp| zp.push((0x10, 0)));
        f.navigate(|zp| zp.push((0x20, 0)));
        assert_eq!(f.zoom_path, vec![(0x10, 0), (0x20, 0)]);
        assert!(f.go_back());
        assert_eq!(f.zoom_path, vec![(0x10, 0)]);
        assert!(f.go_forward());
        assert_eq!(f.zoom_path, vec![(0x10, 0), (0x20, 0)]);
        // The redo itself is undoable.
        assert!(f.go_back());
        assert_eq!(f.zoom_path, vec![(0x10, 0)]);
    }

    #[test]
    fn fresh_navigation_clears_forward_stack() {
        let mut f = nav_state();
        f.navigate(|zp| zp.push((0x10, 0)));
        assert!(f.go_back());
        assert_eq!(f.forward_stack.len(), 1);
        f.navigate(|zp| zp.push((0x30, 0)));
        assert!(f.forward_stack.is_empty());
        assert!(!f.go_forward());
    }

    #[test]
    fn reset_all_clears_zoom_and_history() {
        let mut f = nav_state();
        f.navigate(|zp| zp.push((0x10, 0)));
        assert!(f.go_back());
        assert!(!f.reset_all());
        assert!(f.zoom_path.is_empty());
        assert!(f.back_stack.is_empty());
        assert!(f.forward_stack.is_empty());
    }

    #[test]
    fn reset_all_clears_stack_filters() {
        let mut f = nav_state();
        f.filter.include_stacks = vec![StackSelector::Frame(0x10, 0)];
        f.filter.exclude_stacks = vec![StackSelector::Group("idle".into())];
        f.ctrl_pending = Some(vec![StackSelector::Frame(0x20, 0)]);
        assert!(f.reset_all());
        assert!(f.filter.include_stacks.is_empty());
        assert!(f.filter.exclude_stacks.is_empty());
        assert!(f.ctrl_pending.is_none());
    }

    #[test]
    fn empty_search_is_inactive() {
        assert!(flame_search_terms("").is_empty());
        assert!(flame_search_terms("   ").is_empty());
        assert!(flame_search_terms("|").is_empty());
        assert!(flame_search_terms(" | | ").is_empty());
        assert!(!flame_search_hit("ella_server", 0x1000, &[]));
    }

    #[test]
    fn single_term_matches_label_or_hex() {
        let terms = flame_search_terms("ella");
        assert!(flame_search_hit("ella_server", 0xabc, &terms));
        assert!(!flame_search_hit("mrelay", 0xabc, &terms));
        let hex_terms = flame_search_terms("0xABC");
        assert!(flame_search_hit("mrelay", 0xabc, &hex_terms));
    }

    #[test]
    fn pipe_is_or() {
        let terms = flame_search_terms("ella|mrelay");
        assert!(flame_search_hit("ella_server", 0x1, &terms));
        assert!(flame_search_hit("mrelay", 0x1, &terms));
        assert!(!flame_search_hit("nginx", 0x1, &terms));
        let spaced = flame_search_terms("ella | mrelay | kernel");
        assert!(flame_search_hit("kernel", 0x1, &spaced));
        assert_eq!(spaced, ["ella", "mrelay", "kernel"]);
    }

    #[test]
    fn empty_alternatives_are_dropped() {
        let terms = flame_search_terms("|ella||mrelay|");
        assert_eq!(terms, ["ella", "mrelay"]);
        assert!(flame_search_hit("ella_server", 0x1, &terms));
    }

    fn labeled(
        addr: u64,
        pid: u32,
        count: u64,
        label: &str,
        children: Vec<FlameNode>,
    ) -> FlameNode {
        FlameNode {
            addr,
            pid,
            total_count: count,
            self_count: 0,
            children,
            is_self_pc: false,
            group_label: Some(label.to_string()),
        }
    }

    #[test]
    fn ancestor_trail_joins_frames_under_the_zoom() {
        let tree = FlameNode {
            addr: 0,
            pid: 0,
            total_count: 4,
            self_count: 0,
            children: vec![labeled(
                1,
                7,
                4,
                "nginx",
                vec![labeled(
                    2,
                    7,
                    4,
                    "_start",
                    vec![labeled(3, 7, 4, "main", vec![])],
                )],
            )],
            is_self_pc: false,
            group_label: None,
        };
        let profile = CpuProfile::default();
        assert_eq!(
            ancestor_trail_label(&tree, &[(1, 7)], &profile, true, false),
            None
        );
        assert_eq!(
            ancestor_trail_label(&tree, &[(1, 7), (2, 7), (3, 7)], &profile, true, false),
            Some("nginx > _start".to_string())
        );
        assert_eq!(
            ancestor_trail_label(&tree, &[], &profile, true, false),
            None
        );
    }

    #[test]
    fn search_matches_are_left_to_right_bottom_to_top() {
        // Visual order: kernel(10) | ella(4) | mrelay(2) on the
        // bottom row; tcp_input(6) then idle(4) above kernel.
        let tree = FlameNode {
            addr: 0,
            pid: 0,
            total_count: 16,
            self_count: 0,
            children: vec![
                labeled(
                    1,
                    0,
                    10,
                    "kernel",
                    vec![
                        labeled(10, 0, 6, "tcp_input", vec![]),
                        labeled(11, 0, 4, "idle", vec![]),
                    ],
                ),
                labeled(
                    2,
                    1,
                    4,
                    "ella_server",
                    vec![labeled(20, 1, 4, "worker", vec![])],
                ),
                labeled(3, 2, 2, "mrelay", vec![]),
            ],
            is_self_pc: false,
            group_label: None,
        };
        let profile = CpuProfile::default();
        let terms = flame_search_terms("ella|mrelay|idle|tcp");
        let found = collect_search_matches(&tree, &profile, true, false, &terms);
        let addrs: Vec<u64> = found
            .paths
            .iter()
            .filter_map(|p| p.last().map(|k| k.0))
            .collect();
        assert_eq!(addrs, vec![2, 3, 10, 11]);
        // Disjoint matches: ella(4) + mrelay(2) + tcp_input(6) + idle(4).
        assert_eq!(found.samples, 16);
    }

    #[test]
    fn search_cost_counts_nested_matches_once() {
        let tree = FlameNode {
            addr: 0,
            pid: 0,
            total_count: 16,
            self_count: 0,
            children: vec![
                labeled(
                    1,
                    0,
                    10,
                    "kernel",
                    vec![
                        labeled(10, 0, 6, "tcp_input", vec![]),
                        labeled(11, 0, 4, "idle", vec![]),
                    ],
                ),
                labeled(
                    2,
                    1,
                    4,
                    "ella_server",
                    vec![labeled(20, 1, 4, "worker", vec![])],
                ),
                labeled(3, 2, 2, "mrelay", vec![]),
            ],
            is_self_pc: false,
            group_label: None,
        };
        let profile = CpuProfile::default();
        let kernel =
            collect_search_matches(&tree, &profile, true, false, &flame_search_terms("kernel"));
        assert_eq!(kernel.paths.len(), 1);
        assert_eq!(kernel.samples, 10);

        let nested = collect_search_matches(
            &tree,
            &profile,
            true,
            false,
            &flame_search_terms("kernel|tcp"),
        );
        assert_eq!(nested.paths.len(), 2);
        assert_eq!(nested.samples, 10);
    }

    #[test]
    fn zoom_path_for_process_finds_group_bar() {
        let tree = FlameNode {
            addr: 0,
            pid: 0,
            total_count: 6,
            self_count: 0,
            children: vec![
                labeled(analysis::FLAME_GROUP_ADDR, 0, 2, "kernel", vec![]),
                labeled(analysis::FLAME_GROUP_ADDR, 9104, 4, "ella_server", vec![]),
            ],
            is_self_pc: false,
            group_label: None,
        };
        let profile = CpuProfile {
            samples: vec![
                elog_parser::CpuStackSample {
                    ts: 0,
                    cpu: 0,
                    tid: 1,
                    pid: 9104,
                    sample_type: 0,
                    flags: elog_parser::CPU_SAMPLE_FLAG_USER_MODE,
                    freq_mhz: None,
                    frames: vec![0x4000_0000],
                },
                elog_parser::CpuStackSample {
                    ts: 0,
                    cpu: 0,
                    tid: 2,
                    pid: 42,
                    sample_type: 0,
                    flags: elog_parser::CPU_SAMPLE_FLAG_USER_MODE,
                    freq_mhz: None,
                    frames: vec![0x4000_0000],
                },
            ],
            threads: [
                (
                    1,
                    elog_parser::ThreadInfo {
                        tid: 1,
                        pid: 9104,
                        comm: "ella_server".to_string(),
                        tdname: String::new(),
                    },
                ),
                (
                    2,
                    elog_parser::ThreadInfo {
                        tid: 2,
                        pid: 42,
                        comm: "ghost".to_string(),
                        tdname: String::new(),
                    },
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        assert_eq!(
            zoom_path_for_process(&tree, &profile, 9104),
            Some(vec![(analysis::FLAME_GROUP_ADDR, 9104)])
        );
        assert_eq!(zoom_path_for_process(&tree, &profile, 42), None);
    }

    #[test]
    fn zoom_path_for_thread_nests_under_kernel() {
        let idle_pid = analysis::flame_thread_group_pid("idle");
        let tree = FlameNode {
            addr: 0,
            pid: 0,
            total_count: 6,
            self_count: 0,
            children: vec![
                labeled(
                    analysis::FLAME_GROUP_ADDR,
                    0,
                    2,
                    "kernel",
                    vec![labeled(
                        analysis::FLAME_GROUP_ADDR,
                        idle_pid,
                        2,
                        "idle",
                        vec![],
                    )],
                ),
                labeled(analysis::FLAME_GROUP_ADDR, 9104, 4, "ella_server", vec![]),
            ],
            is_self_pc: false,
            group_label: None,
        };
        assert_eq!(
            zoom_path_for_target(&tree, &FlameZoomTarget::Process("kernel".into())),
            Some(vec![(analysis::FLAME_GROUP_ADDR, 0)])
        );
        assert_eq!(
            zoom_path_for_target(&tree, &FlameZoomTarget::Thread("idle".into())),
            Some(vec![
                (analysis::FLAME_GROUP_ADDR, 0),
                (analysis::FLAME_GROUP_ADDR, idle_pid),
            ])
        );
        assert_eq!(
            zoom_path_for_target(&tree, &FlameZoomTarget::Thread("ella_server".into())),
            Some(vec![(analysis::FLAME_GROUP_ADDR, 9104)])
        );
        let merged = FlameNode {
            children: vec![labeled(analysis::FLAME_GROUP_ADDR, 0, 2, "kernel", vec![])],
            ..tree
        };
        assert_eq!(
            zoom_path_for_target(&merged, &FlameZoomTarget::Thread("swi1: hpts".into())),
            Some(vec![(analysis::FLAME_GROUP_ADDR, 0)])
        );
    }

    #[test]
    fn zoom_path_for_process_nests_under_user() {
        let tree = FlameNode {
            addr: 0,
            pid: 0,
            total_count: 4,
            self_count: 0,
            children: vec![
                labeled(analysis::FLAME_GROUP_ADDR, 0, 1, "kernel", vec![]),
                labeled(
                    analysis::FLAME_GROUP_ADDR,
                    analysis::USER_PSEUDO_PID,
                    3,
                    "user",
                    vec![labeled(
                        analysis::FLAME_GROUP_ADDR,
                        9104,
                        3,
                        "nginx",
                        vec![],
                    )],
                ),
            ],
            is_self_pc: false,
            group_label: None,
        };
        assert_eq!(
            zoom_path_for_target(&tree, &FlameZoomTarget::Process("nginx".into())),
            Some(vec![
                (analysis::FLAME_GROUP_ADDR, analysis::USER_PSEUDO_PID),
                (analysis::FLAME_GROUP_ADDR, 9104),
            ])
        );
        assert_eq!(
            zoom_path_for_target(&tree, &FlameZoomTarget::Process("user".into())),
            Some(vec![(
                analysis::FLAME_GROUP_ADDR,
                analysis::USER_PSEUDO_PID
            )])
        );
    }
}
