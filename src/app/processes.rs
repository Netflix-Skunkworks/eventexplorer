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
// Top-level app
// ---------------------------------------------------------------------------

/// Sortable column on the Processes tab's process table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSortColumn {
    Pid,
    Comm,
    Samples,
    PctCpu,
    Threads,
    Modules,
}

/// Selected Processes-tab row. `Pid` when split-by-pid is on;
/// `Comm` when rows are grouped (empty string is the unknown-comm
/// group; `"kernel"` is the flame-style kernel bucket).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProcessSelection {
    Pid(u32),
    Comm(String),
}

pub(crate) const PROCESS_SPLITTER_SIZE: f32 = 8.0;

fn draw_process_vertical_splitter(ui: &mut egui::Ui, height: f32) -> (f32, bool) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(PROCESS_SPLITTER_SIZE, height),
        egui::Sense::drag(),
    );
    let active = response.hovered() || response.dragged();
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(
            if active { 2.0_f32 } else { 1.0_f32 },
            Color32::from_gray(if active { 130 } else { 65 }),
        ),
    );
    let response = response
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
        .on_hover_text("Drag to resize the process list and detail panes.");
    let delta = if response.dragged() {
        ui.input(|input| input.pointer.delta().x)
    } else {
        0.0
    };
    (delta, response.drag_stopped())
}

pub(crate) fn draw_process_horizontal_splitter(ui: &mut egui::Ui) -> (f32, bool) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), PROCESS_SPLITTER_SIZE),
        egui::Sense::drag(),
    );
    let active = response.hovered() || response.dragged();
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(
            if active { 2.0_f32 } else { 1.0_f32 },
            Color32::from_gray(if active { 130 } else { 65 }),
        ),
    );
    let response = response
        .on_hover_cursor(egui::CursorIcon::ResizeVertical)
        .on_hover_text("Drag to resize Top functions and the stack list.");
    let delta = if response.dragged() {
        ui.input(|input| input.pointer.delta().y)
    } else {
        0.0
    };
    (delta, response.drag_stopped())
}

/// View state for `Tab::Processes`. Built lazily on first switch
/// to the tab. A file load drops it. The CPU usage strip is the
/// Flame one: one time window shared with Flame and System.
pub struct ProcessesViewState {
    pub(crate) rows: Vec<analysis::ProcessSummary>,
    /// Flame-style groups: user `comm` + one `kernel` row.
    pub(crate) grouped_by_process: Vec<analysis::ProcessGroupSummary>,
    /// One row per thread `comm`, including kernel threads.
    pub(crate) grouped_by_thread: Vec<analysis::ProcessGroupSummary>,
    /// One row per pid. Off (default) uses a grouped table.
    pub(crate) split_by_pid: bool,
    /// When grouped, split kernel threads into idle/intr/clock/…
    /// rows instead of one `kernel` row. Ignored when `split_by_pid`.
    pub(crate) split_by_thread_name: bool,
    pub(crate) sort_column: ProcessSortColumn,
    /// `true` when the sort is ascending. Default ordering on
    /// each column descends *for the magnitude columns* (samples,
    /// % CPU, threads, modules) and ascends for identifier
    /// columns (pid, comm).
    pub(crate) sort_ascending: bool,
    /// Selected row. Always the top visible row until the user
    /// clicks another. `None` only when the table is empty.
    pub(crate) selected: Option<ProcessSelection>,
    /// Time window the current rows were built for. Kept equal to the
    /// Flame/System filter. `ts_end_us == u64::MAX` is the rest of
    /// the capture.
    pub(crate) ts_start_us: u64,
    pub(crate) ts_end_us: u64,
}

/// Background rollup for a Processes time window.
pub(crate) struct ProcessRollup {
    ts_start_us: u64,
    ts_end_us: u64,
    rows: Vec<analysis::ProcessSummary>,
    grouped_by_process: Vec<analysis::ProcessGroupSummary>,
    grouped_by_thread: Vec<analysis::ProcessGroupSummary>,
}

fn build_process_rollup(
    profile: &elog_parser::CpuProfile,
    summaries: &[SessionSummary],
    ts_start_us: u64,
    ts_end_us: u64,
) -> ProcessRollup {
    let rows =
        analysis::compute_process_summaries_between(profile, summaries, ts_start_us, ts_end_us);
    let grouped_by_thread = analysis::group_process_summaries_by_comm(&rows, profile);
    let grouped_by_process = analysis::group_process_summaries_by_flame(&rows, profile);
    ProcessRollup {
        ts_start_us,
        ts_end_us,
        rows,
        grouped_by_process,
        grouped_by_thread,
    }
}

impl ProcessesViewState {
    pub(crate) fn from_profile(
        profile: &elog_parser::CpuProfile,
        summaries: &[SessionSummary],
    ) -> Self {
        let rows = analysis::compute_process_summaries(profile, summaries);
        let grouped_by_thread = analysis::group_process_summaries_by_comm(&rows, profile);
        let grouped_by_process = analysis::group_process_summaries_by_flame(&rows, profile);
        Self {
            rows,
            grouped_by_process,
            grouped_by_thread,
            split_by_pid: false,
            split_by_thread_name: false,
            sort_column: ProcessSortColumn::Samples,
            sort_ascending: false,
            selected: None,
            ts_start_us: 0,
            ts_end_us: u64::MAX,
        }
    }

    fn for_window(ts_start_us: u64, ts_end_us: u64) -> Self {
        Self {
            rows: Vec::new(),
            grouped_by_process: Vec::new(),
            grouped_by_thread: Vec::new(),
            split_by_pid: false,
            split_by_thread_name: false,
            sort_column: ProcessSortColumn::Samples,
            sort_ascending: false,
            selected: None,
            ts_start_us,
            ts_end_us,
        }
    }

    fn replace_rows(&mut self, rollup: ProcessRollup) {
        self.rows = rollup.rows;
        self.grouped_by_process = rollup.grouped_by_process;
        self.grouped_by_thread = rollup.grouped_by_thread;
        self.sort_rows();
    }

    pub(crate) fn groups(&self) -> &[analysis::ProcessGroupSummary] {
        if self.split_by_thread_name {
            &self.grouped_by_thread
        } else {
            &self.grouped_by_process
        }
    }

    fn groups_mut(&mut self) -> &mut Vec<analysis::ProcessGroupSummary> {
        if self.split_by_thread_name {
            &mut self.grouped_by_thread
        } else {
            &mut self.grouped_by_process
        }
    }

    pub(crate) fn set_split_by_pid(&mut self, split: bool) {
        if self.split_by_pid == split {
            return;
        }
        self.split_by_pid = split;
        self.selected = match (split, self.selected.take()) {
            (true, Some(ProcessSelection::Comm(c))) => self
                .groups()
                .iter()
                .find(|g| g.comm == c)
                .and_then(|g| g.pids.first().copied())
                .map(ProcessSelection::Pid),
            (false, Some(ProcessSelection::Pid(pid))) => {
                if self.split_by_thread_name {
                    self.rows
                        .iter()
                        .find(|r| r.pid == pid)
                        .map(|r| ProcessSelection::Comm(r.comm.clone()))
                } else {
                    self.grouped_by_process
                        .iter()
                        .find(|g| g.pids.contains(&pid))
                        .map(|g| ProcessSelection::Comm(g.comm.clone()))
                }
            }
            (_, other) => other,
        };
        if !split && self.sort_column == ProcessSortColumn::Pid {
            self.sort_column = ProcessSortColumn::Comm;
            self.sort_ascending = true;
        }
        self.sort_rows();
    }

    pub(crate) fn set_split_by_thread_name(&mut self, split: bool) {
        if self.split_by_thread_name == split {
            return;
        }
        self.split_by_thread_name = split;
        self.selected = match (split, self.selected.take()) {
            (false, Some(ProcessSelection::Comm(c))) => {
                if self.grouped_by_process.iter().any(|g| g.comm == c) {
                    Some(ProcessSelection::Comm(c))
                } else {
                    Some(ProcessSelection::Comm(
                        analysis::KERNEL_GROUP_LABEL.to_string(),
                    ))
                }
            }
            (_, other) => other,
        };
        self.sort_rows();
    }

    pub(crate) fn sort_rows(&mut self) {
        let asc = self.sort_ascending;
        let col = self.sort_column;
        let ord_pct = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
        if self.split_by_pid {
            self.rows.sort_by(|a, b| {
                use ProcessSortColumn::*;
                let ord = match col {
                    Pid => a.pid.cmp(&b.pid),
                    Comm => a.comm.cmp(&b.comm),
                    Samples => a.busy_sample_count.cmp(&b.busy_sample_count),
                    PctCpu => ord_pct(a.pct_cpu, b.pct_cpu),
                    Threads => a.thread_count.cmp(&b.thread_count),
                    Modules => a.module_count.cmp(&b.module_count),
                };
                if asc {
                    ord
                } else {
                    ord.reverse()
                }
            });
        } else {
            self.groups_mut().sort_by(|a, b| {
                use ProcessSortColumn::*;
                let ord = match col {
                    Pid => a.pids.first().cmp(&b.pids.first()),
                    Comm => a.comm.cmp(&b.comm),
                    Samples => a.busy_sample_count.cmp(&b.busy_sample_count),
                    PctCpu => ord_pct(a.pct_cpu, b.pct_cpu),
                    Threads => a.thread_count.cmp(&b.thread_count),
                    Modules => a.module_count.cmp(&b.module_count),
                };
                if asc {
                    ord
                } else {
                    ord.reverse()
                }
            });
        }
        self.ensure_selection();
    }

    fn selection_visible(&self) -> bool {
        match &self.selected {
            Some(ProcessSelection::Pid(pid)) if self.split_by_pid => {
                self.rows.iter().any(|r| r.pid == *pid)
            }
            Some(ProcessSelection::Comm(comm)) if !self.split_by_pid => {
                self.groups().iter().any(|g| g.comm == *comm)
            }
            _ => false,
        }
    }

    /// Keep a row selected. A missing or stale selection becomes the
    /// first row in the current sort.
    fn ensure_selection(&mut self) {
        if self.selection_visible() {
            return;
        }
        self.selected = if self.split_by_pid {
            self.rows.first().map(|r| ProcessSelection::Pid(r.pid))
        } else {
            self.groups()
                .first()
                .map(|g| ProcessSelection::Comm(g.comm.clone()))
        };
    }

    /// Clicking a column header for the *first* time picks the
    /// column's natural direction (descending for magnitudes,
    /// ascending for identifiers). Clicking the same column again
    /// flips direction.
    pub(crate) fn on_sort_click(&mut self, col: ProcessSortColumn) {
        if self.sort_column == col {
            self.sort_ascending = !self.sort_ascending;
        } else {
            self.sort_column = col;
            self.sort_ascending = matches!(col, ProcessSortColumn::Pid | ProcessSortColumn::Comm);
        }
        self.sort_rows();
    }
}

// ---------------------------------------------------------------------------
// Processes tab (impl on EventExplorerApp; lives in its own block to
// keep the diff against the previously single-tab app readable)
// ---------------------------------------------------------------------------

fn process_comm_label(comm: &str) -> String {
    if comm.is_empty() {
        "(unknown)".to_string()
    } else {
        comm.to_string()
    }
}

/// Gear + popup for the Processes tab. Same painted-glyph /
/// Area/Frame::popup pattern as the flame and CPU-usage gears.
fn show_processes_gear_menu(
    ui: &mut egui::Ui,
    split_by_pid: &mut bool,
    split_by_thread_name: &mut bool,
) {
    let popup_id = ui.make_persistent_id("processes_gear_menu");
    let is_open = ui
        .memory(|m| m.data.get_temp::<bool>(popup_id))
        .unwrap_or(false);

    let button_h = ui.spacing().interact_size.y;
    let button_size = egui::vec2(button_h, button_h);
    let (rect, response) = ui.allocate_exact_size(button_size, egui::Sense::click());
    let visuals = if is_open {
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
        "\u{2699}",
        egui::FontId::proportional(14.0),
        visuals.text_color(),
    );
    let response = if is_open {
        response
    } else {
        response.on_hover_text("Display options")
    };
    if response.clicked() {
        ui.memory_mut(|m| m.data.insert_temp(popup_id, !is_open));
    }
    if !is_open {
        return;
    }

    const POPUP_W: f32 = 220.0;
    let area = egui::Area::new(popup_id.with("area"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.left_bottom() + egui::vec2(0.0, 4.0))
        .pivot(egui::Align2::LEFT_TOP)
        .constrain_to(ui.ctx().screen_rect());
    let area_resp = area.show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(POPUP_W);
            ui.set_max_width(POPUP_W);
            ui.add_enabled_ui(!*split_by_pid, |ui| {
                ui.checkbox(
                    split_by_thread_name,
                    RichText::new("Split by thread name").size(12.0),
                )
                .on_hover_text(
                    "Show idle, ithreads, clock, … as their own rows. \
                 Off (default) folds kernel threads into one kernel \
                 row, matching the flame's Group-by-process bars.",
                );
            });
            ui.checkbox(split_by_pid, RichText::new("Split by pid").size(12.0))
                .on_hover_text(
                    "One table row per pid. Off (default) groups \
                 same-name processes into one row and hides the \
                 pid column; the detail panel expands the member \
                 pids.",
                );
        });
    });
    let popup_rect = area_resp.response.rect;
    if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.memory_mut(|m| m.data.insert_temp(popup_id, false));
    }
    if ui.ctx().input(|i| i.pointer.any_click()) {
        if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
            if !rect.contains(pos) && !popup_rect.contains(pos) {
                ui.memory_mut(|m| m.data.insert_temp(popup_id, false));
            }
        }
    }
}

const OPEN_FLAME_HOVER: &str = "Zoom the Flame tab to this group. \
     Zoom: all, middle-click, or mouse-back restores the full graph.";

struct ProcessRowCells<'a> {
    show_pid: bool,
    pid: Option<u32>,
    comm: &'a str,
    busy: u64,
    pct_cpu: f64,
    threads: usize,
    modules: usize,
    is_sel: bool,
}

fn fill_process_row(row: &mut egui_extras::TableRow<'_, '_>, c: ProcessRowCells<'_>) -> bool {
    let mut clicked = false;
    let mut mk_label = |ui: &mut egui::Ui, s: String| {
        let mut text = RichText::new(s).monospace().size(12.0);
        if c.is_sel {
            text = text.color(Color32::from_rgb(255, 220, 120));
        }
        let hit = ui.interact(ui.max_rect(), ui.id().with("row_hit"), egui::Sense::click());
        ui.add(egui::Label::new(text));
        if hit.clicked() {
            clicked = true;
        }
    };
    if c.show_pid {
        row.col(|ui| mk_label(ui, format!("{}", c.pid.unwrap_or(0))));
    }
    row.col(|ui| mk_label(ui, process_comm_label(c.comm)));
    row.col(|ui| mk_label(ui, fmt_count(c.busy)));
    row.col(|ui| mk_label(ui, format!("{:.2}%", c.pct_cpu)));
    row.col(|ui| mk_label(ui, format!("{}", c.threads)));
    row.col(|ui| mk_label(ui, format!("{}", c.modules)));
    clicked
}

struct ProcessStats {
    total: u64,
    first_ts_us: u64,
    last_ts_us: u64,
    t0_us: u64,
}

fn show_process_stats_grid(ui: &mut egui::Ui, id: &str, s: ProcessStats) {
    let first_s = s.first_ts_us.saturating_sub(s.t0_us) as f64 / 1_000_000.0;
    let last_s = s.last_ts_us.saturating_sub(s.t0_us) as f64 / 1_000_000.0;
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([24.0, 4.0])
        .show(ui, |ui| {
            let row = |ui: &mut egui::Ui, k: &str, v: String| {
                ui.label(RichText::new(k).color(Color32::from_gray(160)));
                ui.label(RichText::new(v).monospace());
                ui.end_row();
            };
            row(ui, "Total samples", fmt_count(s.total));
            row(
                ui,
                "Lifetime in capture",
                fmt_seconds((last_s - first_s).max(0.0)),
            );
        });
}

fn show_owned_sessions(ui: &mut egui::Ui, ids: &[u64]) -> Option<u64> {
    if ids.is_empty() {
        return None;
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new(format!("TCP sessions owned ({})", ids.len()))
            .strong()
            .size(13.0),
    );
    ui.add_space(2.0);
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        for sid in ids {
            if ui
                .small_button(
                    RichText::new(format!("session {sid}")).color(Color32::from_rgb(140, 200, 255)),
                )
                .on_hover_text("Open this TCP session in its own per-session tab.")
                .clicked()
            {
                clicked = Some(*sid);
            }
        }
    });
    clicked
}

impl EventExplorerApp {
    fn poll_process_rollup(&mut self) {
        let recv = self.processes_rollup_build.as_ref().map(|rx| rx.try_recv());
        match recv {
            Some(Ok(rollup)) => {
                self.processes_rollup_build = None;
                let matches = self.processes_view.as_ref().is_some_and(|view| {
                    view.ts_start_us == rollup.ts_start_us && view.ts_end_us == rollup.ts_end_us
                });
                if matches {
                    if let Some(view) = self.processes_view.as_mut() {
                        view.replace_rows(rollup);
                    }
                }
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.processes_rollup_build = None;
            }
            _ => {}
        }
    }

    fn invalidate_process_functions(&mut self) {
        self.process_functions_view = None;
        self.process_functions_build = None;
    }

    fn set_process_time_window(&mut self, start: u64, end: u64) {
        let changed = self.processes_view.as_mut().is_some_and(|view| {
            if view.ts_start_us == start && view.ts_end_us == end {
                false
            } else {
                view.ts_start_us = start;
                view.ts_end_us = end;
                true
            }
        });
        if !changed {
            return;
        }
        self.invalidate_process_functions();
        self.start_process_rollup();
    }

    fn start_process_rollup(&mut self) {
        let Some(view) = self.processes_view.as_ref() else {
            return;
        };
        let ts_start_us = view.ts_start_us;
        let ts_end_us = view.ts_end_us;
        let profile = Arc::clone(&self.cpu_profile);
        let summaries = self.summaries.clone();
        let (tx, rx) = mpsc::channel();
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(move || {
            let _ = tx.send(build_process_rollup(
                &profile,
                &summaries,
                ts_start_us,
                ts_end_us,
            ));
        });
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            crate::app::recent::yield_to_browser().await;
            let _ = tx.send(build_process_rollup(
                &profile,
                &summaries,
                ts_start_us,
                ts_end_us,
            ));
        });
        self.processes_rollup_build = Some(rx);
    }

    /// Flame filter when that tab has been built, otherwise the
    /// window stashed for the first Flame visit. `(0, u64::MAX)` is
    /// the full capture.
    fn shared_flame_time_window(&self) -> (u64, u64) {
        if let Some(flame) = self.flame.as_ref() {
            (flame.filter.ts_start_us, flame.filter.ts_end_us)
        } else if let Some(filter) = self.pending_flame_filter.as_ref() {
            (filter.ts_start_us, filter.ts_end_us)
        } else {
            (0, u64::MAX)
        }
    }

    /// Write the Flame/System time window and rebuild the flame when
    /// it changes. Process rows follow in [`Self::set_process_time_window`].
    fn set_shared_flame_time_window(&mut self, start: u64, end: u64) {
        let changed = if let Some(flame) = self.flame.as_mut() {
            if flame.filter.ts_start_us == start && flame.filter.ts_end_us == end {
                false
            } else {
                flame.filter.ts_start_us = start;
                flame.filter.ts_end_us = end;
                Self::start_flame_rebuild(flame, &self.cpu_profile, &mut self.flame_build);
                true
            }
        } else {
            let current = self
                .pending_flame_filter
                .as_ref()
                .map(|filter| (filter.ts_start_us, filter.ts_end_us))
                .unwrap_or((0, u64::MAX));
            if current == (start, end) {
                false
            } else {
                let mut filter = self.pending_flame_filter.take().unwrap_or_default();
                filter.ts_start_us = start;
                filter.ts_end_us = end;
                self.pending_flame_filter = Some(filter);
                true
            }
        };
        if changed {
            self.set_process_time_window(start, end);
        }
    }

    fn show_processes_cpu_strip(&mut self, ui: &mut egui::Ui) {
        let (ts_start, ts_end) = self.shared_flame_time_window();
        let cpus = self.current_function_filter().cpus;
        let capture_start = self
            .file_info
            .as_ref()
            .map(|fi| fi.capture_start)
            .unwrap_or(0);
        let agg_time_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let mut cpu_zoom = self.cpu_zoom_processes;
        let zoom_before = cpu_zoom;
        let (change, scale_cpu_by_freq, zoomed_window) = {
            let info_file = self.file_info.as_ref();
            let utc_time_labels = self.utc_time_labels;
            let show_legends = self.show_legends;
            let plot_bins = self.plot_bins;
            let mut scale_cpu_by_freq = self.scale_cpu_by_freq;
            let mut panel = self.aggregate.as_mut().and_then(|a| a.cpu_panel.as_mut());
            let result = if let Some(p) = panel.as_mut() {
                p.show_header(
                    ui,
                    "aggregate_cpu_panel",
                    &mut scale_cpu_by_freq,
                    &mut cpu_zoom,
                );
                if cpu_zoom && !zoom_before {
                    self.agg_time_axis
                        .set_view_x(TimeAxisController::view_x_for_window(
                            capture_start,
                            ts_start,
                            ts_end,
                        ));
                }
                p.scale_by_freq = scale_cpu_by_freq;
                p.show_legends = show_legends;
                let host_t0_us = p.t0_us();
                let current_sel = if ts_start > 0 || ts_end != u64::MAX {
                    Some((ts_start, ts_end))
                } else {
                    None
                };
                p.set_plot_ids("cpu_usage_strip_flame");
                p.sync_bins(plot_bins, &cpus);
                if cpu_zoom {
                    let view_us = self.agg_time_axis.window_us(capture_start).or_else(|| {
                        let hi_us = capture_start
                            .saturating_add((agg_time_max * 1_000_000.0).round() as u64)
                            .max(capture_start.saturating_add(1));
                        Some((capture_start, hi_us))
                    });
                    let origin = utc_axis_origin(utc_time_labels, info_file, capture_start);
                    p.draw(ui, capture_start, None, view_us, origin, false);
                    let zoomed_window = if let Some((rect, tf)) = p.last_rect_transform() {
                        let outcome =
                            self.agg_time_axis
                                .handle(ui, &[(rect, tf)], 0.0, agg_time_max);
                        let mut cpu_cursor = SysCursor::new(ui, origin, plot_bins);
                        if let Some(data_x) = cpu_cursor.panel(rect, &tf, false) {
                            cpu_cursor.add_cpu_series(&tf, data_x, &p.cursor_series(data_x));
                        }
                        cpu_cursor.draw(ui);
                        let pointer_down = ui.input(|i| i.pointer.any_down());
                        if !pointer_down || matches!(outcome, TimeAxisOutcome::Reset { .. }) {
                            Some(
                                self.agg_time_axis
                                    .window_us(capture_start)
                                    .unwrap_or((0, u64::MAX)),
                            )
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    (None, zoomed_window)
                } else {
                    let origin = utc_axis_origin(utc_time_labels, info_file, host_t0_us);
                    let change = p.draw(ui, host_t0_us, current_sel, None, origin, true);
                    if let Some((rect, tf)) = p.last_rect_transform() {
                        let mut cpu_cursor = SysCursor::new(ui, origin, plot_bins);
                        if let Some(data_x) = cpu_cursor.panel(rect, &tf, false) {
                            cpu_cursor.add_cpu_series(&tf, data_x, &p.cursor_series(data_x));
                        }
                        cpu_cursor.draw(ui);
                    }
                    (change, None)
                }
            } else {
                (None, None)
            };
            (result.0, scale_cpu_by_freq, result.1)
        };
        self.scale_cpu_by_freq = scale_cpu_by_freq;
        self.cpu_zoom_processes = cpu_zoom;
        if let Some((lo, hi)) = zoomed_window {
            self.set_shared_flame_time_window(lo, hi);
        }
        match change {
            Some(SelectionChange::Set(lo, hi)) => self.set_shared_flame_time_window(lo, hi),
            Some(SelectionChange::Clear) => self.set_shared_flame_time_window(0, u64::MAX),
            None => {}
        }
    }

    pub(crate) fn show_processes_view(&mut self, ui: &mut egui::Ui) {
        // Lazy build: the rollup is a single-pass function of the
        // current CpuProfile, so we cache it on tab entry and reuse
        // it across re-renders. A new file load clears the slot
        // (see start_loading). The time window is the Flame/System
        // filter; a drag on either strip recomputes these rows.
        let (ts_start, ts_end) = self.shared_flame_time_window();
        if self.processes_view.is_none() {
            let threshold = if cfg!(target_arch = "wasm32") {
                10_000
            } else {
                50_000
            };
            if ts_start == 0 && ts_end == u64::MAX && self.cpu_profile.samples.len() < threshold {
                let mut state =
                    ProcessesViewState::from_profile(&self.cpu_profile, &self.summaries);
                state.sort_rows();
                self.processes_view = Some(state);
            } else {
                self.processes_view = Some(ProcessesViewState::for_window(ts_start, ts_end));
                self.start_process_rollup();
            }
        } else {
            self.set_process_time_window(ts_start, ts_end);
        }
        self.poll_process_rollup();
        if self.processes_layout.is_visible(PlotId::CpuUsage) {
            self.show_processes_cpu_strip(ui);
        }
        {
            let show_tx = self.processes_layout.is_visible(PlotId::NetworkTx);
            let show_rx = self.processes_layout.is_visible(PlotId::NetworkRx);
            let tx_h = self.processes_layout.plot_height(PlotId::NetworkTx);
            let rx_h = self.processes_layout.plot_height(PlotId::NetworkRx);
            self.show_nic_plots(ui, show_tx, show_rx, tx_h, rx_h, "processes_");
        }
        ui.add_space(6.0);

        let mut split_by_pid = self
            .processes_view
            .as_ref()
            .map(|v| v.split_by_pid)
            .unwrap_or(false);
        let mut split_by_thread_name = self
            .processes_view
            .as_ref()
            .map(|v| v.split_by_thread_name)
            .unwrap_or(false);
        let total_busy: u64 = self
            .processes_view
            .as_ref()
            .map(|v| v.rows.iter().map(|r| r.busy_sample_count).sum())
            .unwrap_or(0);
        let total_pids: usize = self
            .processes_view
            .as_ref()
            .map(|v| v.rows.len())
            .unwrap_or(0);
        let total_groups: usize = self
            .processes_view
            .as_ref()
            .map(|v| v.groups().len())
            .unwrap_or(0);
        let (ts_start, ts_end) = self
            .processes_view
            .as_ref()
            .map(|v| (v.ts_start_us, v.ts_end_us))
            .unwrap_or((0, u64::MAX));
        let summary = if split_by_pid {
            format!(
                "{} processes, {} busy samples",
                total_pids,
                fmt_count(total_busy),
            )
        } else {
            format!(
                "{} processes, {} pids, {} busy samples",
                total_groups,
                total_pids,
                fmt_count(total_busy),
            )
        };
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(summary)
                    .size(13.0)
                    .color(Color32::from_gray(180)),
            );
            if ts_start > 0 || ts_end != u64::MAX {
                let origin = self
                    .file_info
                    .as_ref()
                    .map(|f| f.capture_start)
                    .unwrap_or(0);
                let lo = ts_start.saturating_sub(origin) as f64 / 1_000_000.0;
                let hi = ts_end.saturating_sub(origin) as f64 / 1_000_000.0;
                ui.label(
                    RichText::new(format!("filtered {lo:.2}-{hi:.2} s"))
                        .size(13.0)
                        .color(Color32::from_rgb(255, 190, 80)),
                );
            }
            if self.processes_rollup_build.is_some() {
                ui.spinner();
                ui.ctx().request_repaint();
            }
            ui.add_space(6.0);
            show_processes_gear_menu(ui, &mut split_by_pid, &mut split_by_thread_name);
        });
        if let Some(view) = self.processes_view.as_mut() {
            view.set_split_by_pid(split_by_pid);
            view.set_split_by_thread_name(split_by_thread_name);
        }
        ui.separator();

        // List on the left, selected process on the right. The table
        // always has a selection (the top row until the user clicks).
        let avail = ui.available_size();
        let building = self.processes_rollup_build.is_some();
        let empty = self
            .processes_view
            .as_ref()
            .is_some_and(|v| v.rows.is_empty());
        if building && empty {
            ui.vertical_centered(|ui| {
                ui.add_space(avail.y * 0.35);
                ui.spinner();
                ui.label(
                    RichText::new("Building process list…")
                        .size(15.0)
                        .color(Color32::from_gray(200)),
                );
            });
            return;
        }
        // Column initials (pid is optional) plus the default 8px gap,
        // with modules held to a fixed skinny width so the detail pane
        // takes the rest of the tab.
        let mut default_table_w: f32 = 140.0 + 90.0 + 70.0 + 70.0 + 56.0 + 4.0 * 8.0 + 18.0;
        if split_by_pid {
            default_table_w += 70.0 + 8.0;
        }
        // The table may be narrowed to a compact strip; its columns keep
        // their useful widths inside a two-axis scroll area.
        let min_table_w = 120.0;
        let max_table_w = (avail.x - PROCESS_SPLITTER_SIZE - 320.0).max(min_table_w);
        let table_w = self
            .processes_preferences
            .table_width
            .unwrap_or(default_table_w)
            .clamp(min_table_w, max_table_w);
        let mut clicked: Option<ProcessSelection> = None;
        let mut clicked_col: Option<ProcessSortColumn> = None;
        let split_by_pid = self.processes_view.as_ref().is_some_and(|v| v.split_by_pid);

        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(table_w);
                egui::ScrollArea::both()
                    .id_source("processes_table_scroll")
                    .max_height(avail.y)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(default_table_w.max(table_w));
                        self.show_process_table(ui, split_by_pid, &mut clicked, &mut clicked_col);
                    });
            });
            let (table_delta, table_drag_stopped) = draw_process_vertical_splitter(ui, avail.y);
            if table_delta != 0.0 {
                self.processes_preferences.table_width =
                    Some((table_w + table_delta).clamp(min_table_w, max_table_w));
            }
            if table_drag_stopped {
                save_processes_preferences(&self.processes_preferences);
            }
            ui.vertical(|ui| {
                ui.set_min_width((avail.x - table_w - PROCESS_SPLITTER_SIZE).max(200.0));
                if let Some(col) = clicked_col.take() {
                    if let Some(view) = self.processes_view.as_mut() {
                        view.on_sort_click(col);
                    }
                }
                if let Some(sel) = clicked.take() {
                    if let Some(view) = self.processes_view.as_mut() {
                        if view.selected.as_ref() != Some(&sel) {
                            view.selected = Some(sel);
                        }
                    }
                }
                if let Some(view) = self.processes_view.as_mut() {
                    view.ensure_selection();
                }
                let selected = self
                    .processes_view
                    .as_ref()
                    .and_then(|view| view.selected.clone());
                if let Some(sel) = selected {
                    self.show_process_detail(ui, &sel);
                }
            });
        });
    }

    fn show_process_table(
        &self,
        ui: &mut egui::Ui,
        split_by_pid: bool,
        clicked: &mut Option<ProcessSelection>,
        clicked_col: &mut Option<ProcessSortColumn>,
    ) {
        use egui_extras::{Column, TableBuilder};
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        if split_by_pid {
            table = table.column(Column::initial(70.0).at_least(50.0));
        }
        let table = table
            .column(Column::initial(140.0).at_least(80.0))
            .column(Column::initial(90.0).at_least(70.0))
            .column(Column::initial(70.0).at_least(56.0))
            .column(Column::initial(70.0).at_least(56.0))
            .column(
                Column::initial(56.0)
                    .at_least(48.0)
                    .at_most(72.0)
                    .clip(true),
            );
        let view = self.processes_view.as_ref().unwrap();
        let sort_col = view.sort_column;
        let sort_asc = view.sort_ascending;
        let header = |ui: &mut egui::Ui, label: &str, col: ProcessSortColumn| {
            let arrow = if sort_col == col {
                if sort_asc {
                    " ^"
                } else {
                    " v"
                }
            } else {
                ""
            };
            let resp = ui.add(
                egui::Label::new(RichText::new(format!("{label}{arrow}")).strong().size(13.0))
                    .sense(egui::Sense::click()),
            );
            if resp.clicked() {
                return Some(col);
            }
            None
        };
        table
            .header(22.0, |mut row| {
                if split_by_pid {
                    row.col(|ui| {
                        if let Some(c) = header(ui, "pid", ProcessSortColumn::Pid) {
                            *clicked_col = Some(c);
                        }
                    });
                }
                row.col(|ui| {
                    if let Some(c) = header(ui, "comm", ProcessSortColumn::Comm) {
                        *clicked_col = Some(c);
                    }
                });
                row.col(|ui| {
                    if let Some(c) = header(ui, "samples", ProcessSortColumn::Samples) {
                        *clicked_col = Some(c);
                    }
                });
                row.col(|ui| {
                    if let Some(c) = header(ui, "% CPU", ProcessSortColumn::PctCpu) {
                        *clicked_col = Some(c);
                    }
                });
                row.col(|ui| {
                    if let Some(c) = header(ui, "threads", ProcessSortColumn::Threads) {
                        *clicked_col = Some(c);
                    }
                });
                row.col(|ui| {
                    if let Some(c) = header(ui, "modules", ProcessSortColumn::Modules) {
                        *clicked_col = Some(c);
                    }
                });
            })
            .body(|body| {
                let row_h = 20.0;
                let selected = view.selected.as_ref();
                if split_by_pid {
                    let n = view.rows.len();
                    body.rows(row_h, n, |mut row| {
                        let r = &view.rows[row.index()];
                        let is_sel = selected == Some(&ProcessSelection::Pid(r.pid));
                        if fill_process_row(
                            &mut row,
                            ProcessRowCells {
                                show_pid: true,
                                pid: Some(r.pid),
                                comm: &r.comm,
                                busy: r.busy_sample_count,
                                pct_cpu: r.pct_cpu,
                                threads: r.thread_count,
                                modules: r.module_count,
                                is_sel,
                            },
                        ) {
                            *clicked = Some(ProcessSelection::Pid(r.pid));
                        }
                    });
                } else {
                    let groups = view.groups();
                    let n = groups.len();
                    body.rows(row_h, n, |mut row| {
                        let g = &groups[row.index()];
                        let is_sel = matches!(
                            selected,
                            Some(ProcessSelection::Comm(c)) if c == &g.comm
                        );
                        if fill_process_row(
                            &mut row,
                            ProcessRowCells {
                                show_pid: false,
                                pid: None,
                                comm: &g.comm,
                                busy: g.busy_sample_count,
                                pct_cpu: g.pct_cpu,
                                threads: g.thread_count,
                                modules: g.module_count,
                                is_sel,
                            },
                        ) {
                            *clicked = Some(ProcessSelection::Comm(g.comm.clone()));
                        }
                    });
                }
            });
    }

    fn show_process_detail(&mut self, ui: &mut egui::Ui, sel: &ProcessSelection) {
        let t0_us = self
            .processes_view
            .as_ref()
            .map(|v| v.rows.iter().map(|x| x.first_ts_us).min().unwrap_or(0))
            .unwrap_or(0);
        match sel {
            ProcessSelection::Pid(pid) => {
                let row = self
                    .processes_view
                    .as_ref()
                    .and_then(|v| v.rows.iter().find(|r| r.pid == *pid).cloned());
                let Some(r) = row else {
                    ui.label(
                        RichText::new("(selected pid not in current view)")
                            .color(Color32::from_gray(160)),
                    );
                    return;
                };
                ui.horizontal(|ui| {
                    let title = if r.comm.is_empty() {
                        format!("pid {}", r.pid)
                    } else {
                        format!("{} (pid {})", r.comm, r.pid)
                    };
                    ui.heading(title);
                    ui.add_space(16.0);
                    if ui
                        .button("Open flame for this pid")
                        .on_hover_text(OPEN_FLAME_HOVER)
                        .clicked()
                    {
                        self.open_flame_for_pid(r.pid);
                    }
                });
                ui.add_space(4.0);
                show_process_stats_grid(
                    ui,
                    &format!("process_detail_{}", r.pid),
                    ProcessStats {
                        total: r.sample_count,
                        first_ts_us: r.first_ts_us,
                        last_ts_us: r.last_ts_us,
                        t0_us,
                    },
                );
                if let Some(sid) = show_owned_sessions(ui, &r.owned_session_ids) {
                    self.select_session(sid);
                }
            }
            ProcessSelection::Comm(comm) => {
                let split_by_thread = self
                    .processes_view
                    .as_ref()
                    .is_some_and(|v| v.split_by_thread_name);
                let group = self
                    .processes_view
                    .as_ref()
                    .and_then(|v| v.groups().iter().find(|g| g.comm == *comm).cloned());
                let Some(g) = group else {
                    ui.label(
                        RichText::new("(selected process not in current view)")
                            .color(Color32::from_gray(160)),
                    );
                    return;
                };
                ui.horizontal(|ui| {
                    let title = format!(
                        "{} ({} {})",
                        process_comm_label(&g.comm),
                        g.pids.len(),
                        if g.pids.len() == 1 { "pid" } else { "pids" },
                    );
                    ui.heading(title);
                    ui.add_space(16.0);
                    let btn = if split_by_thread {
                        "Open flame for this thread"
                    } else {
                        "Open flame for this process"
                    };
                    if ui.button(btn).on_hover_text(OPEN_FLAME_HOVER).clicked() {
                        let target = if split_by_thread {
                            FlameZoomTarget::Thread(g.comm.clone())
                        } else {
                            FlameZoomTarget::Process(g.comm.clone())
                        };
                        self.open_flame_for_target(target);
                    }
                });
                ui.add_space(4.0);
                egui::CollapsingHeader::new(format!("PIDs ({})", g.pids.len()))
                    .id_source(("process_pids", g.comm.clone()))
                    .default_open(false)
                    .show(ui, |ui| {
                        let width = g
                            .pids
                            .iter()
                            .map(|pid| pid.to_string().len())
                            .max()
                            .unwrap_or(1);
                        // The wrapped layout's first row starts at interact_size.
                        // Pin every row to the font so the line gap stays even.
                        let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
                        ui.scope(|ui| {
                            ui.spacing_mut().interact_size.y = row_h;
                            ui.horizontal_wrapped(|ui| {
                                ui.set_row_height(row_h);
                                ui.spacing_mut().item_spacing.x = 16.0;
                                for pid in &g.pids {
                                    ui.label(RichText::new(format!("{pid:>width$}")).monospace());
                                }
                            });
                        });
                    });
                ui.add_space(4.0);
                show_process_stats_grid(
                    ui,
                    &format!("process_group_detail_{}", g.comm),
                    ProcessStats {
                        total: g.sample_count,
                        first_ts_us: g.first_ts_us,
                        last_ts_us: g.last_ts_us,
                        t0_us,
                    },
                );
                if let Some(sid) = show_owned_sessions(ui, &g.owned_session_ids) {
                    self.select_session(sid);
                }
            }
        }
        ui.separator();
        self.show_process_functions(ui, sel);
    }

    /// Switch to the Flame tab and zoom to `pid`'s group bar
    /// (process `comm`, or the merged `kernel` bar).
    pub(crate) fn open_flame_for_pid(&mut self, pid: u32) {
        let target = flame_zoom_target_for_pid(&self.cpu_profile, pid);
        self.open_flame_for_target(target);
    }

    /// Switch to the Flame tab and zoom to `target`. Switches
    /// **no kernel group** back to **by process** so the bar exists. The full
    /// tree stays in place so Reset / the Zoom breadcrumb /
    /// mouse-back restore the unzoomed view.
    pub(crate) fn open_flame_for_target(&mut self, target: FlameZoomTarget) {
        let had_pid_filter = self
            .flame
            .as_ref()
            .is_some_and(|f| !f.filter.pids.is_empty());
        let mut need_rebuild = had_pid_filter;
        if let Some(flame) = self.flame.as_mut() {
            flame.filter.pids.clear();
            if flame.organize == FlameOrganize::NoKernelGroup {
                flame.organize = FlameOrganize::ByProcess;
                need_rebuild = true;
            }
        }
        let tree_ready = self.flame.as_ref().is_some_and(|f| f.tree.total_count > 0);
        if need_rebuild {
            self.pending_flame_zoom = Some(target);
            if let Some(flame) = self.flame.as_mut() {
                Self::start_flame_rebuild(flame, &self.cpu_profile, &mut self.flame_build);
            }
        } else if tree_ready {
            if let Some(flame) = self.flame.as_mut() {
                flame.zoom_to_target(&target);
            }
        } else {
            self.pending_flame_zoom = Some(target);
        }
        self.active_tab = Tab::CpuFlame;
    }

    /// Consume [`Self::pending_flame_zoom`] once the flame tree
    /// is ready. Called from the Flame-tab paint path after a
    /// background rebuild finishes (or on the first visit).
    pub(crate) fn apply_pending_flame_zoom(&mut self) {
        let Some(target) = self.pending_flame_zoom.take() else {
            return;
        };
        if let Some(flame) = self.flame.as_mut() {
            flame.zoom_to_target(&target);
        }
    }

    /// System tab zoom as absolute capture µs, or `None` when un-zoomed.
    fn system_time_window_us(&self) -> Option<(u64, u64)> {
        let capture_start_us = self.file_info.as_ref().map(|fi| fi.capture_start)?;
        self.agg_time_axis.window_us(capture_start_us)
    }

    /// Entering Flame or Processes: impose the System tab's time window
    /// as the shared filter.
    pub(crate) fn sync_flame_time_from_system(&mut self) {
        let (start_us, end_us) = self.system_time_window_us().unwrap_or((0, u64::MAX));
        if let Some(flame) = self.flame.as_mut() {
            flame.filter.ts_start_us = start_us;
            flame.filter.ts_end_us = end_us;
            flame.zoom_path.clear();
            flame.back_stack.clear();
            Self::start_flame_rebuild(flame, &self.cpu_profile, &mut self.flame_build);
        } else {
            let mut filter = self.pending_flame_filter.take().unwrap_or_default();
            filter.ts_start_us = start_us;
            filter.ts_end_us = end_us;
            self.pending_flame_filter = Some(filter);
        }
    }

    /// Leaving Flame or Processes: write the shared time filter back
    /// onto System only.
    pub(crate) fn sync_system_time_from_flame(&mut self) {
        let Some(capture_start_us) = self.file_info.as_ref().map(|fi| fi.capture_start) else {
            return;
        };
        let window = self.flame.as_ref().and_then(|flame| {
            if flame.filter.ts_start_us > 0 || flame.filter.ts_end_us != u64::MAX {
                Some((flame.filter.ts_start_us, flame.filter.ts_end_us))
            } else {
                None
            }
        });
        match window {
            Some((lo_us, hi_us)) => {
                self.agg_time_axis
                    .set_window_us(capture_start_us, lo_us, hi_us)
            }
            None => self.agg_time_axis.clear_zoom(),
        }
    }
}
