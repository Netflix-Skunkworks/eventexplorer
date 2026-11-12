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

/// One catalog response, or the end of the batch.
enum PackFetchEvent {
    Item {
        gen: u64,
        id: String,
        fetched: elog_parser::FetchedPack,
    },
    Done {
        gen: u64,
    },
}

/// Pack-fetch tallies that outlive the in-flight job, so the flame
/// info popup can still show them after the catalog lookup finishes.
#[derive(Default, Clone, Copy)]
pub(crate) struct SymbolPackProgress {
    pub unique: usize,
    pub cached: usize,
    pub downloaded: usize,
    pub missing: usize,
    pub failed: usize,
}

/// In-flight parallel catalog fetch. Ranges are installed into the
/// profile's shared tables as each response lands.
pub(crate) struct PackFetchJob {
    rx: mpsc::Receiver<PackFetchEvent>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub total: usize,
    pub done: usize,
    finished: bool,
}

fn paint_splash_mark(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let p = ui.painter();
    let navy = Color32::from_rgb(0x19, 0x2A, 0x56);
    let cyan = Color32::from_rgb(0x00, 0xB0, 0xD8);
    let gold = Color32::from_rgb(0xF8, 0xC8, 0x30);
    p.rect_filled(rect, size * 0.18, navy);
    let x = |v: f32| rect.left() + size * v / 16.0;
    let y = |v: f32| rect.top() + size * v / 16.0;
    let cell = |x0: f32, y0: f32, x1: f32, y1: f32, c: Color32| {
        p.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x(x0), y(y0)), egui::pos2(x(x1), y(y1))),
            0.0,
            c,
        );
    };
    cell(1.0, 3.0, 3.0, 13.0, cyan);
    cell(13.0, 3.0, 15.0, 13.0, gold);
    cell(4.0, 3.0, 6.0, 13.0, Color32::WHITE);
    cell(4.0, 3.0, 12.0, 5.0, Color32::WHITE);
    cell(4.0, 7.0, 11.0, 9.0, Color32::WHITE);
    cell(4.0, 11.0, 12.0, 13.0, Color32::WHITE);
}

fn tab_gear_popup_id() -> egui::Id {
    egui::Id::new("global_gear_menu")
}

fn tab_info_popup_id() -> egui::Id {
    egui::Id::new("aggregate_info_popup")
}

fn tab_help_popup_id() -> egui::Id {
    egui::Id::new("plot_help_popup")
}

fn tab_chrome_popup_open(ctx: &egui::Context) -> bool {
    ctx.memory(|m| {
        m.data
            .get_temp::<bool>(tab_gear_popup_id())
            .unwrap_or(false)
            || m.data
                .get_temp::<bool>(tab_info_popup_id())
                .unwrap_or(false)
            || m.data
                .get_temp::<bool>(tab_help_popup_id())
                .unwrap_or(false)
            || m.data
                .get_temp::<bool>(super::memory::memory_help_popup_id())
                .unwrap_or(false)
    })
}

pub(super) fn tab_chrome_close_others(ui: &mut egui::Ui, keep: egui::Id) {
    for id in [
        tab_gear_popup_id(),
        tab_info_popup_id(),
        tab_help_popup_id(),
        super::memory::memory_help_popup_id(),
    ] {
        if id != keep {
            ui.memory_mut(|m| m.data.insert_temp(id, false));
        }
    }
}

fn snapshot_tab_chrome_escape(ctx: &egui::Context) {
    let open = tab_chrome_popup_open(ctx);
    ctx.memory_mut(|m| {
        m.data
            .insert_temp(egui::Id::new("tab_chrome_was_open"), open)
    });
}

fn tab_chrome_was_open(ctx: &egui::Context) -> bool {
    ctx.memory(|m| {
        m.data
            .get_temp::<bool>(egui::Id::new("tab_chrome_was_open"))
            .unwrap_or(false)
    })
}

fn escape_restores_plot_maximize(ui: &egui::Ui) -> bool {
    ui.input(|i| i.key_pressed(egui::Key::Escape)) && !tab_chrome_was_open(ui.ctx())
}

pub(super) fn tab_chrome_glyph_button(
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
        visuals.weak_bg_fill,
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

/// One capture file to load on web: fetched from a URL, or already in
/// memory from the file picker.
#[cfg(target_arch = "wasm32")]
pub(crate) enum WebSource {
    Url(String),
    Bytes(String, Vec<u8>),
}

#[cfg(target_arch = "wasm32")]
impl WebSource {
    fn label(&self) -> String {
        match self {
            WebSource::Url(url) => url_file_name(url).to_string(),
            WebSource::Bytes(name, _) => name.clone(),
        }
    }
}

/// Display name for a URL-loaded capture: the `#fragment` when one
/// is present (share links carry the original file name there — it
/// is never sent to the server), else the last path segment.
#[cfg(target_arch = "wasm32")]
fn url_file_name(url: &str) -> &str {
    if let Some((_, frag)) = url.rsplit_once('#') {
        if !frag.is_empty() {
            return frag;
        }
    }
    url.rsplit_once('/')
        .map(|(_, name)| name)
        .filter(|n| !n.is_empty())
        .unwrap_or(url)
}

/// `name`, or `name +N more` for a multi-file load.
#[cfg(target_arch = "wasm32")]
fn web_capture_label(sources: &[WebSource]) -> String {
    let first = sources.first().map(WebSource::label).unwrap_or_default();
    if sources.len() > 1 {
        format!("{first} +{} more", sources.len() - 1)
    } else {
        first
    }
}

/// Splash label for a failed open. A composite-download URL is the
/// cache and the capture stem; anything else is the name as given.
fn brief_open_name(name: &str) -> String {
    match (query_value(name, "cache_name"), query_value(name, "stem")) {
        (Some(cache), Some(stem)) => format!("{cache} / {stem}"),
        _ => name.to_string(),
    }
}

fn query_value<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let q = s.split_once('?').map(|(_, q)| q).unwrap_or(s);
    q.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key && !v.is_empty()).then_some(v)
    })
}

fn show_open_error(ui: &mut egui::Ui, err: &str, weak: egui::Color32) {
    let err_color = egui::Color32::from_rgb(0xE0, 0x70, 0x70);
    let Some(rest) = err.strip_prefix("Failed to open\n") else {
        ui.add(egui::Label::new(egui::RichText::new(err).size(13.0).color(err_color)).wrap(true));
        return;
    };
    let (name, detail) = rest.split_once('\n').unwrap_or((rest, ""));
    ui.label(
        egui::RichText::new("Failed to open")
            .size(14.0)
            .strong()
            .color(err_color),
    );
    if !name.is_empty() {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(brief_open_name(name))
                .size(12.0)
                .color(weak),
        );
    }
    if !detail.is_empty() {
        ui.add_space(8.0);
        // The splash column centers its children. A galley the full
        // width of the card, painted at the left edge, wraps on the
        // card and stays left-aligned.
        let width = ui.available_width();
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = width;
        job.append(
            detail,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(13.0),
                color: err_color,
                ..Default::default()
            },
        );
        let galley = ui.fonts(|f| f.layout_job(job));
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(width, galley.size().y), egui::Sense::hover());
        ui.painter().galley(rect.left_top(), galley, err_color);
    }
}

/// Fetch (if needed), inflate, and parse one file into `[lo, hi)` of the bar.
#[cfg(target_arch = "wasm32")]
async fn web_parse_one(
    source: WebSource,
    prog: &Arc<LoadProgress>,
    (lo, hi): (u32, u32),
) -> Result<elog_parser::ParsedCapture, String> {
    let label = source.label();
    let span = hi - lo;
    let at = |f: u32| lo + span * f / 10;
    let (bytes, decomp, parse) = match source {
        WebSource::Url(url) => {
            prog.set_window(lo, at(4));
            prog.set_status("Fetching…");
            let bytes = fetch_bytes(&url, prog.clone())
                .await
                .map_err(|e| format!("Fetch failed ({url}): {e}"))?;
            yield_to_browser().await;
            (bytes, (at(4), at(5)), (at(5), hi))
        }
        WebSource::Bytes(_, bytes) => {
            yield_to_browser().await;
            (bytes, (lo, at(1)), (at(1), hi))
        }
    };
    let gzip = bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b;
    let mut capture = if gzip {
        prog.set_window(decomp.0, decomp.1);
        let parts = decompress_gzip_yielding(&bytes, &label, prog).await?;
        drop(bytes);
        prog.set_window(parse.0, parse.1);
        if parts.len() == 1 {
            let (_, mut inflated) = parts.into_iter().next().unwrap();
            prog.store_permille(0);
            prog.set_status("Parsing events…");
            yield_to_browser().await;
            elog_parser::parse_elog_chunked(&mut inflated, Some(prog))
                .map_err(|e| format!("{label}: {e}"))?
        } else {
            let (plo, phi) = prog.window();
            let n = parts.len() as u32;
            let merge_lo = plo + (phi - plo) * 90 / 100;
            let mut captures = Vec::with_capacity(parts.len());
            for (i, (name, mut inflated)) in parts.into_iter().enumerate() {
                let i = i as u32;
                let lo = plo + (merge_lo - plo) * i / n;
                let hi = plo + (merge_lo - plo) * (i + 1) / n;
                prog.set_window(lo, hi);
                prog.store_permille(0);
                prog.set_status(format!("Parsing {name}…"));
                yield_to_browser().await;
                let mut one = elog_parser::parse_elog_chunked(&mut inflated, Some(prog))
                    .map_err(|e| format!("{label}: {e}"))?;
                elog_parser::attach_source_named(&mut one.8, &one.9, name);
                captures.push(one);
                yield_to_browser().await;
            }
            prog.set_window(merge_lo, phi);
            prog.store_permille(0);
            prog.set_status(format!("Merging {n} captures…"));
            yield_to_browser().await;
            let merged = elog_parser::merge_captures(captures).map_err(|e| e.to_string())?;
            prog.set_window(plo, phi);
            merged
        }
    } else {
        prog.set_window(parse.0, parse.1);
        prog.store_permille(0);
        prog.set_status("Parsing events…");
        yield_to_browser().await;
        elog_parser::parse_elog_core(&bytes, Some(prog)).map_err(|e| format!("{label}: {e}"))?
    };
    if capture.8.sources.is_empty() {
        elog_parser::attach_source_named(&mut capture.8, &capture.9, label);
    }
    yield_to_browser().await;
    Ok(capture)
}

/// Web counterpart to `parse_elogs_with_progress` + the summary build:
/// load every source, merge a rolled set into one capture, and produce
/// the plot-ready `LoadResult`. Yields between stages so the loading
/// screen can paint.
#[cfg(target_arch = "wasm32")]
async fn web_ingest(
    sources: Vec<WebSource>,
    prog: &Arc<LoadProgress>,
    filter_low_events: bool,
) -> LoadResult {
    let n = sources.len() as u32;
    if n == 0 {
        return Err("no capture files given".to_string());
    }
    let mut captures = Vec::with_capacity(sources.len());
    for (i, source) in sources.into_iter().enumerate() {
        let i = i as u32;
        if n > 1 {
            prog.set_prefix(format!("[{}/{n}] ", i + 1));
        }
        captures.push(web_parse_one(source, prog, (i * 900 / n, (i + 1) * 900 / n)).await?);
    }
    prog.set_prefix("");
    let capture = if captures.len() > 1 {
        prog.set_window(900, 1000);
        prog.store_permille(0);
        prog.set_status(format!("Merging {n} captures…"));
        yield_to_browser().await;
        elog_parser::merge_captures(captures).map_err(|e| e.to_string())?
    } else {
        captures.remove(0)
    };
    let (
        mut sessions,
        stars_agg_samples,
        _stars_pace_samples,
        cpu_profile,
        if_profile,
        tcp_sample_profile,
        ella_profile,
        nginx_profile,
        file_info,
        providers,
        schema,
    ) = capture;
    prog.set_window(900, 1000);
    prog.set_status("Computing session summaries…");
    yield_to_browser().await;
    let mut summaries = analysis::session_summaries(&sessions);
    analysis::merge_nginx_session_summaries(&mut summaries, &sessions, &nginx_profile);
    prog.set_status("Building plot data…");
    yield_to_browser().await;
    let aux = analysis::retain_plot_events(&mut sessions);
    let cpu_profile = Arc::new(cpu_profile);
    let mem_series = analysis::build_mem_series(&schema.mem, file_info.capture_start);
    prog.set_status("Building aggregate view…");
    yield_to_browser().await;
    let aggregate = compute_aggregate(
        &summaries,
        &stars_agg_samples,
        filter_low_events,
        file_info.capture_start,
    )
    .with_cpu_strips(&cpu_profile, file_info.capture_start)
    .with_tcp_state(&tcp_sample_profile, file_info.capture_start)
    .with_if_profile(&if_profile, file_info.capture_start)
    .with_ella_profile(&ella_profile, file_info.capture_start)
    .with_nginx_profile(&nginx_profile, file_info.capture_start)
    .with_mem_series(&mem_series);
    prog.store_permille(1000);
    prog.set_status("Done");
    Ok((
        sessions,
        summaries,
        stars_agg_samples,
        cpu_profile,
        if_profile,
        tcp_sample_profile,
        ella_profile,
        nginx_profile,
        aux,
        file_info,
        providers,
        aggregate,
        schema.hpts_wheel_events,
        mem_series,
    ))
}

impl EventExplorerApp {
    pub fn new_empty(startup_monitor: Option<(f32, f32)>) -> Self {
        Self {
            sessions: Sessions::default(),
            session_aux: SessionAuxMap::default(),
            summaries: Vec::new(),
            stars_agg_samples: Vec::new(),
            stars_agg_view: None,
            hpts_wheel_events: Vec::new(),
            hpts_hist_view: None,
            hpts_has_slot_gate: false,
            hpts_slot_gate_view: None,
            cpu_profile: Arc::new(CpuProfile::default()),
            if_profile: IfProfile::default(),
            tcp_samples: TcpSampleProfile::default(),
            interfaces_view: None,
            tcp_samples_view: None,
            ella_profile: Arc::new(EllaProfile::default()),
            ella_has_frame_timing: false,
            ella_fanout_ids: Vec::new(),
            nginx_profile: NginxProfile::default(),
            nginx_has_diag: false,
            mem_series: analysis::MemSeries::default(),
            ella_channel_list: Vec::new(),
            info_if_bytes: Vec::new(),
            ella_info: analysis::EllaRelaySummary::default(),
            ella_sort_column: EllaSortColumn::Clients,
            ella_sort_ascending: false,
            ella_samples_view: None,
            open_ella_channels: Vec::new(),
            nginx_samples_view: None,
            origin_segments: None,
            raw_origin_segments: Vec::new(),
            origin_segments_view: OriginSegmentViewMode::RateMbps,
            #[cfg(not(target_arch = "wasm32"))]
            pending_origin_csv: Vec::new(),
            flame_build: None,
            pack_fetch_gen: 0,
            pack_fetch: None,
            pack_progress: SymbolPackProgress::default(),
            flame: None,
            cpu_filter_ui: CpuFilterUi::default(),
            baseline_load: None,
            baseline_error: None,
            processes_view: None,
            processes_preferences: load_processes_preferences(),
            process_functions_view: None,
            process_functions_owner: None,
            process_functions_build: None,
            processes_rollup_build: None,
            #[cfg(not(target_arch = "wasm32"))]
            source_path_rewrites: rustc_hash::FxHashMap::default(),
            #[cfg(not(target_arch = "wasm32"))]
            source_binary_paths: rustc_hash::FxHashMap::default(),
            #[cfg(not(target_arch = "wasm32"))]
            source_lookup_preferences: load_source_lookup_preferences(),
            #[cfg(not(target_arch = "wasm32"))]
            native_file_dialog: None,
            #[cfg(all(
                unix,
                not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
            ))]
            unix_text_selection: UnixTextSelectionState::default(),
            #[cfg(target_os = "macos")]
            macos_control_click: false,
            next_source_id: 1,
            pending_flame_filter: None,
            pending_flame_zoom: None,
            file_info: None,
            providers: Vec::new(),
            events_tab: EventsTabState::default(),
            sort_column: SortColumn::BytesSent,
            sort_ascending: false,
            filter_low_events: true,
            session_time_filter: None,
            session_filter_drag: None,
            srtt_avg_view: AvgView::OverTime,
            goodput_avg_view: AvgView::OverTime,
            utc_time_labels: false,
            show_legends: true,
            plot_bins: load_plot_bins(),
            dark_mode: load_dark_mode(),
            #[cfg(target_arch = "wasm32")]
            splash_url: String::new(),
            #[cfg(target_arch = "wasm32")]
            hosted_logs: HostedLogs::Idle,
            #[cfg(target_arch = "wasm32")]
            pending_hosted_logs: None,
            #[cfg(target_arch = "wasm32")]
            pending_history_nav: {
                let (tx, rx) = mpsc::channel();
                install_popstate_listener(tx);
                rx
            },
            scale_cpu_by_freq: true,
            cpu_zoom_system: true,
            cpu_zoom_flame: false,
            cpu_zoom_processes: false,
            cpu_zoom_session: true,
            view: View::NoFile,
            recent_files: load_recent_files(),
            loading_paths: Vec::new(),
            open_error: None,
            active_tab: Tab::Aggregate,
            system_layout: default_layout(PlotCatalogKind::System),
            system_draft: default_layout(PlotCatalogKind::System),
            flame_layout: default_layout(PlotCatalogKind::Flame),
            flame_draft: default_layout(PlotCatalogKind::Flame),
            processes_layout: default_layout(PlotCatalogKind::Processes),
            processes_draft: default_layout(PlotCatalogKind::Processes),
            plot_sidebar_open: false,
            plot_catalog_show_empty: false,
            plot_sidebar_drag: None,
            aggregate: None,
            agg_time_axis: TimeAxisController::default(),
            open_sessions: Vec::new(),
            renaming_tab: None,
            startup_monitor,
            current_window_state: None,
            last_window_save: None,
            loaded_file_name: None,
            loaded_file_urls: Vec::new(),
            current_title: None,
            remote_input: None,
            pending_share_params: None,
            share_popup_open: false,
            share_upload: None,
            #[cfg(not(target_arch = "wasm32"))]
            loaded_paths: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            share_form: None,
            #[cfg(not(target_arch = "wasm32"))]
            share_clipboard: None,
            #[cfg(target_arch = "wasm32")]
            loaded_local_files: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            share_upload_error: None,
            pending_scroll_plot: None,
            #[cfg(target_arch = "wasm32")]
            pending_file_pick: None,
            #[cfg(target_arch = "wasm32")]
            pending_baseline_pick: None,
        }
    }

    /// Web-only constructor used by `WebHandle::start`. Boots the same
    /// `EventExplorerApp` as `new_empty`, then applies any URL query
    /// parameters: a `file=…` URL kicks off an auto-load on the first
    /// frame, and the remaining fields (`session`, `t`, toggles) are
    /// stashed in `pending_share_params` for `apply_pending_share_params`
    /// to apply once the capture finishes loading.
    #[cfg(target_arch = "wasm32")]
    pub fn new_empty_web(params: ShareParams) -> Self {
        let mut app = Self::new_empty(None);
        app.start_hosted_logs_fetch();
        if !params.file_urls.is_empty() {
            app.start_loading_urls(params.file_urls.clone());
        }
        if params.has_post_load_state() {
            app.pending_share_params = Some(params);
        }
        apply_web_color_scheme(app.dark_mode);
        app
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_from_cli(&mut self, paths: Vec<PathBuf>) {
        let (csv_paths, capture_paths): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| {
            p.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
        });
        self.pending_origin_csv.extend(csv_paths);
        self.start_loading_paths(capture_paths);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_origin_segments_dialog(&mut self) {
        self.native_file_dialog = Some(NativeFileDialog::segments_csv());
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn load_origin_segments_path(&mut self, path: PathBuf) {
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let path_str = path.to_string_lossy().to_string();
                self.load_origin_segments_csv_str(&content, &path_str);
            }
            Err(e) => {
                eprintln!(
                    "Failed to read origin segments file {}: {e}",
                    path.display()
                );
            }
        }
    }

    pub(crate) fn load_origin_segments_csv_str(&mut self, content: &str, path_str: &str) {
        let (region, rows) = analysis::parse_origin_segments_csv(content);
        if let Some(existing) = self
            .raw_origin_segments
            .iter_mut()
            .find(|(path, _, _)| path == path_str)
        {
            *existing = (path_str.to_string(), region, rows);
        } else {
            self.raw_origin_segments
                .push((path_str.to_string(), region, rows));
        }
        self.realign_origin_segments();
    }

    pub(crate) fn realign_origin_segments(&mut self) {
        if self.raw_origin_segments.is_empty() {
            self.origin_segments = None;
            return;
        }
        let (start_utc_us, duration_s) = if let Some(fi) = self.file_info.as_ref() {
            let dur = (fi.capture_end.saturating_sub(fi.capture_start)) as f64 / 1_000_000.0;
            (fi.start_utc_us, dur)
        } else {
            (0, 0.0)
        };
        let files: Vec<(&str, &str, &[analysis::OriginSegmentRow])> = self
            .raw_origin_segments
            .iter()
            .map(|(path, region, rows)| (path.as_str(), region.as_str(), rows.as_slice()))
            .collect();
        let prof = analysis::align_origin_segment_files(&files, start_utc_us, duration_s);
        self.origin_segments = Some(prof);
        self.system_layout.hidden.remove(&PlotId::OriginSegments);
        self.system_draft.hidden.remove(&PlotId::OriginSegments);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn start_loading(&mut self, path: PathBuf) {
        self.start_loading_paths(vec![path]);
    }

    /// Load one capture, or a rolled set merged into one continuous
    /// capture (see `elog_parser::merge_captures`).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn start_loading_paths(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.open_error = None;
        let progress = Arc::new(LoadProgress::new());
        let (tx, rx) = mpsc::channel();
        let prog = progress.clone();
        let path_strs: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let file_name = capture_label(&paths);

        self.loading_paths = paths;
        self.loaded_file_name = None;
        self.loaded_file_urls.clear();
        self.loaded_paths.clear();
        self.share_popup_open = false;
        self.share_upload = None;
        self.share_form = None;
        self.open_sessions.clear();
        self.session_time_filter = None;
        self.session_filter_drag = None;
        self.open_ella_channels.clear();
        #[cfg(not(target_arch = "wasm32"))]
        self.source_path_rewrites.clear();
        #[cfg(not(target_arch = "wasm32"))]
        self.source_binary_paths.clear();
        self.ella_channel_list.clear();
        self.info_if_bytes.clear();
        self.ella_info = analysis::EllaRelaySummary::default();
        // A new primary capture invalidates any flame-compare baseline
        // (in-flight or installed). The compare state itself dies with
        // `self.flame = None` when the load lands.
        self.baseline_load = None;
        self.baseline_error = None;
        self.pack_fetch_gen = self.pack_fetch_gen.wrapping_add(1);
        if let Some(job) = self.pack_fetch.take() {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.pack_progress = SymbolPackProgress::default();
        let filter_low_events = self.filter_low_events;

        std::thread::spawn(move || {
            let result = elog_parser::parse_elogs_with_progress(&path_strs, &prog)
                .map(
                    |(
                        mut sessions,
                        stars_agg_samples,
                        _stars_pace_samples,
                        cpu_profile,
                        if_profile,
                        tcp_sample_profile,
                        ella_profile,
                        nginx_profile,
                        file_info,
                        providers,
                        schema,
                    )| {
                        prog.set_status("Computing session summaries…");
                        let mut summaries = analysis::session_summaries(&sessions);
                        analysis::merge_nginx_session_summaries(
                            &mut summaries,
                            &sessions,
                            &nginx_profile,
                        );
                        prog.set_status("Building plot data…");
                        let aux = analysis::retain_plot_events(&mut sessions);
                        // Arc'd here (not on the UI thread) so the CPU
                        // strip's zoom-rebin can share it without cloning
                        // the sample vector.
                        let cpu_profile = Arc::new(cpu_profile);
                        let mem_series =
                            analysis::build_mem_series(&schema.mem, file_info.capture_start);
                        prog.set_status("Building aggregate view…");
                        let aggregate = compute_aggregate(
                            &summaries,
                            &stars_agg_samples,
                            filter_low_events,
                            file_info.capture_start,
                        )
                        .with_cpu_strips(&cpu_profile, file_info.capture_start)
                        .with_tcp_state(&tcp_sample_profile, file_info.capture_start)
                        .with_if_profile(&if_profile, file_info.capture_start)
                        .with_ella_profile(&ella_profile, file_info.capture_start)
                        .with_nginx_profile(&nginx_profile, file_info.capture_start)
                        .with_mem_series(&mem_series);
                        prog.store_permille(1000);
                        prog.set_status("Done");
                        (
                            sessions,
                            summaries,
                            stars_agg_samples,
                            cpu_profile,
                            if_profile,
                            tcp_sample_profile,
                            ella_profile,
                            nginx_profile,
                            aux,
                            file_info,
                            providers,
                            aggregate,
                            schema.hpts_wheel_events,
                            mem_series,
                        )
                    },
                )
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });

        self.view = View::Loading {
            file_name,
            progress,
            receiver: rx,
        };
    }

    /// Fetch symbol packs off the UI thread. The capture is already
    /// on screen; [`Self::poll_pack_fetch`] installs ranges when the
    /// catalog responses land.
    #[cfg(not(target_arch = "wasm32"))]
    fn start_pack_fetch(&mut self) {
        let ids = elog_parser::pack_ids_missing_ranges(&self.cpu_profile);
        let Some(base) = elog_parser::symbols_base_url() else {
            return;
        };
        if ids.is_empty() {
            return;
        }
        let total = ids.len();
        let gen = self.pack_fetch_gen;
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_worker = std::sync::Arc::clone(&cancel);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            elog_parser::fetch_packs_each(ids, &base, &cancel_worker, &|id, fetched| {
                let _ = tx.send(PackFetchEvent::Item {
                    gen,
                    id: id.to_string(),
                    fetched,
                });
            });
            let _ = tx.send(PackFetchEvent::Done { gen });
        });
        self.pack_progress = SymbolPackProgress {
            unique: total,
            ..SymbolPackProgress::default()
        };
        self.pack_fetch = Some(PackFetchJob {
            rx,
            cancel,
            total,
            done: 0,
            finished: false,
        });
    }

    /// Browser variant: one XHR per id (the catalog and its redirect
    /// target carry CORS for the hosting origin). No disk cache on web.
    #[cfg(target_arch = "wasm32")]
    fn start_pack_fetch(&mut self) {
        let ids = elog_parser::pack_ids_missing_ranges(&self.cpu_profile);
        let Some(base) = elog_parser::symbols_base_url() else {
            return;
        };
        if ids.is_empty() {
            return;
        }
        let total = ids.len();
        let gen = self.pack_fetch_gen;
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let remaining = std::rc::Rc::new(std::cell::Cell::new(total));
        for id in ids {
            let base = base.clone();
            let tx = tx.clone();
            let remaining = std::rc::Rc::clone(&remaining);
            let cancel = std::sync::Arc::clone(&cancel);
            wasm_bindgen_futures::spawn_local(async move {
                let fetched = elog_parser::fetch_pack_wasm(&id, &base).await;
                if !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = tx.send(PackFetchEvent::Item { gen, id, fetched });
                }
                remaining.set(remaining.get() - 1);
                if remaining.get() == 0 {
                    let _ = tx.send(PackFetchEvent::Done { gen });
                }
            });
        }
        self.pack_progress = SymbolPackProgress {
            unique: total,
            ..SymbolPackProgress::default()
        };
        self.pack_fetch = Some(PackFetchJob {
            rx,
            cancel,
            total,
            done: 0,
            finished: false,
        });
    }

    /// Install packs that have arrived and rebuild views that baked
    /// symbol names. Ranges go into the profile's shared tables, so
    /// the CPU strips' `Arc` clones see them on the next paint.
    pub(crate) fn poll_pack_fetch(&mut self, ctx: &egui::Context) {
        if self.pack_fetch.is_none() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        let gen = self.pack_fetch_gen;
        let mut installed = 0;
        let mut pending: Vec<(String, Vec<u8>, elog_parser::PackFetchKind)> = Vec::new();
        let mut misses: Vec<elog_parser::PackFetchKind> = Vec::new();
        loop {
            let step = match self.pack_fetch.as_ref().map(|job| job.rx.try_recv()) {
                Some(Ok(event)) => Some(event),
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    if let Some(job) = self.pack_fetch.as_mut() {
                        job.finished = true;
                    }
                    break;
                }
                _ => break,
            };
            let Some(event) = step else {
                break;
            };
            let Some(job) = self.pack_fetch.as_mut() else {
                break;
            };
            match event {
                PackFetchEvent::Item {
                    gen: g,
                    id,
                    fetched,
                } if g == gen => {
                    job.done += 1;
                    match fetched.kind {
                        elog_parser::PackFetchKind::Cached
                        | elog_parser::PackFetchKind::Downloaded => {
                            if let Some(bytes) = fetched.bytes {
                                pending.push((id, bytes, fetched.kind));
                            } else {
                                misses.push(elog_parser::PackFetchKind::Failed);
                            }
                        }
                        kind => misses.push(kind),
                    }
                }
                PackFetchEvent::Done { gen: g } if g == gen => job.finished = true,
                _ => {}
            }
        }
        for kind in misses {
            match kind {
                elog_parser::PackFetchKind::Missing => self.pack_progress.missing += 1,
                _ => self.pack_progress.failed += 1,
            }
        }
        for (id, bytes, kind) in &pending {
            let n = elog_parser::install_one_pack(&self.cpu_profile, id, bytes);
            installed += n;
            if n > 0 {
                match kind {
                    elog_parser::PackFetchKind::Cached => self.pack_progress.cached += 1,
                    _ => self.pack_progress.downloaded += 1,
                }
            } else {
                self.pack_progress.failed += 1;
            }
        }
        if installed > 0 {
            self.process_functions_view = None;
            self.process_functions_build = None;
            if let Some(flame) = self.flame.as_mut() {
                Self::start_flame_rebuild(flame, &self.cpu_profile, &mut self.flame_build);
            }
        }
        if self.pack_fetch.as_ref().is_some_and(|job| job.finished) {
            self.pack_fetch = None;
        }
    }

    pub(crate) fn flame_pack_status(&self) -> FlamePackStatus {
        let fetching = self.pack_fetch.is_some();
        let done = self.pack_fetch.as_ref().map(|job| job.done).unwrap_or(
            self.pack_progress.cached
                + self.pack_progress.downloaded
                + self.pack_progress.missing
                + self.pack_progress.failed,
        );
        let unique = self
            .pack_fetch
            .as_ref()
            .map(|job| job.total)
            .unwrap_or(self.pack_progress.unique);
        FlamePackStatus {
            unique,
            cached: self.pack_progress.cached,
            downloaded: self.pack_progress.downloaded,
            missing: self.pack_progress.missing,
            failed: self.pack_progress.failed,
            done,
            fetching,
        }
    }

    /// CPU Usage strip for a baseline capture, matching the Flame tab's
    /// reuse of the Aggregate panel (height, no x-axis caption).
    fn baseline_cpu_panel(profile: &Arc<CpuProfile>) -> Option<CpuUsagePanel> {
        CpuUsagePanel::build(
            profile,
            CpuUsagePanelStyle {
                plot_height: 150.0,
                x_axis_title: None,
                y_axis_label_size: 12.0,
            },
        )
    }

    /// Fetch symbol packs for a compare capture and install them into
    /// `profile` before the diff is built. The last tenth of the load
    /// bar tracks how many packs have returned.
    #[cfg(not(target_arch = "wasm32"))]
    fn install_baseline_symbol_packs(profile: &mut CpuProfile, progress: &LoadProgress) {
        let missing = elog_parser::pack_ids_missing_ranges(profile);
        let base = elog_parser::symbols_base_url();
        let Some(base) = base.filter(|_| !missing.is_empty()) else {
            progress.set_window(0, 1000);
            progress.store_permille(1000);
            return;
        };
        let total = missing.len() as u32;
        progress.set_status(format!("Fetching {total} symbol packs…"));
        progress.set_window(900, 1000);
        progress.store_permille(0);
        elog_parser::install_packs_reporting(profile, &base, &|done, total| {
            progress.store_permille(done.saturating_mul(1000) / total.max(1));
        });
        progress.set_window(0, 1000);
        progress.store_permille(1000);
    }

    /// Fetch symbol packs for a compare capture in the browser and
    /// install them into `profile` before the diff is built.
    #[cfg(target_arch = "wasm32")]
    async fn install_baseline_symbol_packs(profile: &mut CpuProfile, progress: &LoadProgress) {
        let ids = elog_parser::pack_ids_missing_ranges(profile);
        let base = elog_parser::symbols_base_url();
        let Some(base) = base.filter(|_| !ids.is_empty()) else {
            progress.set_window(0, 1000);
            progress.store_permille(1000);
            return;
        };
        let total = ids.len() as u32;
        progress.set_status(format!("Fetching {total} symbol packs…"));
        progress.set_window(900, 1000);
        progress.store_permille(0);
        let mut bytes = rustc_hash::FxHashMap::default();
        for (i, id) in ids.into_iter().enumerate() {
            let fetched = elog_parser::fetch_pack_wasm(&id, &base).await;
            if let Some(b) = fetched.bytes {
                bytes.insert(id, b);
            }
            progress.store_permille((i as u32 + 1).saturating_mul(1000) / total.max(1));
        }
        elog_parser::install_fetched(profile, &bytes);
        progress.set_window(0, 1000);
        progress.store_permille(1000);
    }

    /// Parse a second capture in the background for flame-graph
    /// comparison, keeping only its CPU profile. Symbol packs are
    /// installed before the profile is published, so the diff matches
    /// frames by function the way a standalone open does.
    /// `show_cpu_flame_view` polls `baseline_load` and installs the
    /// result into `FlameViewState.compare`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn start_baseline_load(&mut self, path: PathBuf) {
        let progress = Arc::new(LoadProgress::new());
        let (tx, rx) = mpsc::channel();
        let prog = progress.clone();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let path_str = path.to_string_lossy().to_string();
        let thread_name = name.clone();
        std::thread::spawn(move || {
            let result = elog_parser::parse_elog_with_progress(&path_str, &prog)
                .map_err(|e| e.to_string())
                .and_then(|capture| {
                    if capture.3.samples.is_empty() {
                        return Err("no cpu-provider stack samples".to_string());
                    }
                    let mut cpu = capture.3;
                    Self::install_baseline_symbol_packs(&mut cpu, &prog);
                    let cpu_profile = Arc::new(cpu);
                    let cpu_panel = Self::baseline_cpu_panel(&cpu_profile);
                    Ok(BaselineCapture {
                        cpu_profile,
                        name: thread_name,
                        cpu_panel,
                    })
                });
            let _ = tx.send(result);
        });
        self.baseline_error = None;
        self.baseline_load = Some(BaselineLoad {
            name,
            progress,
            receiver: rx,
        });
    }

    /// Web-side counterpart: parse picked baseline bytes, keeping only
    /// the CPU profile + capture origin. Symbol packs are installed
    /// before the profile is published.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn start_baseline_load_web(&mut self, name: String, bytes: Vec<u8>) {
        let progress = Arc::new(LoadProgress::new());
        let (tx, rx) = mpsc::channel();
        let prog = progress.clone();
        let task_name = name.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = async {
                let capture =
                    web_parse_one(WebSource::Bytes(task_name.clone(), bytes), &prog, (0, 1000))
                        .await?;
                if capture.3.samples.is_empty() {
                    return Err("no cpu-provider stack samples".to_string());
                }
                let mut cpu = capture.3;
                Self::install_baseline_symbol_packs(&mut cpu, &prog).await;
                let cpu_profile = Arc::new(cpu);
                let cpu_panel = Self::baseline_cpu_panel(&cpu_profile);
                Ok(BaselineCapture {
                    cpu_profile,
                    name: task_name,
                    cpu_panel,
                })
            }
            .await;
            let _ = tx.send(result);
        });
        self.baseline_error = None;
        self.baseline_load = Some(BaselineLoad {
            name,
            progress,
            receiver: rx,
        });
    }

    /// Web-side counterpart to `start_loading_paths`: load picked files
    /// and/or URLs, merging a rolled set into one capture. Gzip inflate
    /// yields so the loading bar can move; each event scan still runs
    /// on one JS turn after the status switches to "Parsing events…".
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn start_loading_web(&mut self, mut sources: Vec<WebSource>) {
        let mut seen: Vec<String> = Vec::new();
        sources.retain(|s| match s {
            WebSource::Url(url) if seen.contains(url) => false,
            WebSource::Url(url) => {
                seen.push(url.clone());
                true
            }
            WebSource::Bytes(..) => true,
        });
        if sources.is_empty() {
            return;
        }
        let progress = Arc::new(LoadProgress::new());
        let (tx, rx) = mpsc::channel();
        let prog = progress.clone();
        let file_name = web_capture_label(&sources);

        self.open_error = None;
        self.loading_paths.clear();
        self.loaded_file_name = None;
        self.loaded_local_files.clear();
        self.share_popup_open = false;
        self.share_upload = None;
        self.share_upload_error = None;
        // Only an all-URL load can be re-created from a share link.
        self.loaded_file_urls = sources
            .iter()
            .map(|s| match s {
                WebSource::Url(url) => Some(url.clone()),
                WebSource::Bytes(..) => None,
            })
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
        self.open_sessions.clear();
        self.session_time_filter = None;
        self.session_filter_drag = None;
        self.open_ella_channels.clear();
        #[cfg(not(target_arch = "wasm32"))]
        self.source_path_rewrites.clear();
        #[cfg(not(target_arch = "wasm32"))]
        self.source_binary_paths.clear();
        self.ella_channel_list.clear();
        self.info_if_bytes.clear();
        self.ella_info = analysis::EllaRelaySummary::default();
        // A new primary capture invalidates any flame-compare baseline
        // (in-flight or installed). The compare state itself dies with
        // `self.flame = None` when the load lands.
        self.baseline_load = None;
        self.baseline_error = None;
        let filter_low_events = self.filter_low_events;

        wasm_bindgen_futures::spawn_local(async move {
            let result = web_ingest(sources, &prog, filter_low_events).await;
            if let Err(ref msg) = result {
                web_sys::console::error_1(&format!("EventExplorer: load failed: {msg}").into());
            }
            let _ = tx.send(result);
        });

        self.view = View::Loading {
            file_name,
            progress,
            receiver: rx,
        };
    }

    /// Fetch one or more `.elog`/`.elog.gz` URLs (the `?file=` query
    /// parameter, repeatable) and load them as one capture. URLs resolve
    /// against the page origin, so relative paths work for files served
    /// alongside the wasm bundle.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn start_loading_urls(&mut self, urls: Vec<String>) {
        self.start_loading_web(urls.into_iter().map(WebSource::Url).collect());
    }

    pub(crate) fn open_file_dialog(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.native_file_dialog = Some(NativeFileDialog::captures());
        }
        #[cfg(target_arch = "wasm32")]
        {
            // rfd's `AsyncFileDialog` wraps `<input type="file">` on
            // web. Spawn an async task to await the user's selection,
            // read the bytes, and report back via an mpsc channel that
            // the next `update` frame drains. The browser File handle
            // rides along so a Share-overlay upload can re-read it.
            let (tx, rx) = mpsc::channel();
            self.pending_file_pick = Some(rx);
            wasm_bindgen_futures::spawn_local(async move {
                let handles = rfd::AsyncFileDialog::new()
                    .add_filter("EventLog", &["elog", "gz"])
                    .pick_files()
                    .await;
                let result = match handles {
                    Some(hs) => {
                        let mut files = Vec::with_capacity(hs.len());
                        for h in hs {
                            let bytes = h.read().await;
                            files.push((h.file_name(), bytes, h.inner().clone()));
                        }
                        Some(files)
                    }
                    None => None,
                };
                let _ = tx.send(result);
            });
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn show_native_file_dialog(&mut self, ctx: &egui::Context) {
        let outcome = self
            .native_file_dialog
            .as_mut()
            .map(|dialog| dialog.show(ctx));
        match outcome {
            None | Some(NativeFileDialogOutcome::Pending) => {}
            Some(NativeFileDialogOutcome::Cancelled) => {
                self.native_file_dialog = None;
            }
            Some(NativeFileDialogOutcome::Selected { purpose, paths }) => {
                self.native_file_dialog = None;
                match purpose {
                    NativeFileDialogPurpose::OpenCapture => {
                        let (csvs, elogs): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| {
                            p.extension()
                                .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
                        });
                        if !csvs.is_empty() {
                            if !elogs.is_empty() {
                                self.pending_origin_csv.extend(csvs);
                            } else {
                                for csv_path in csvs {
                                    self.load_origin_segments_path(csv_path);
                                }
                            }
                        }
                        if !elogs.is_empty() {
                            self.start_loading_paths(elogs);
                        }
                    }
                    NativeFileDialogPurpose::CompareCapture => {
                        if let Some(path) = paths.into_iter().next() {
                            self.start_baseline_load(path);
                        }
                    }
                    NativeFileDialogPurpose::LoadSegmentsCsv => {
                        for path in paths {
                            self.load_origin_segments_path(path);
                        }
                    }
                    NativeFileDialogPurpose::SourceBinary(id) => {
                        let Some(path) = paths.into_iter().next() else {
                            return;
                        };
                        let Some(view) = self
                            .flame
                            .as_mut()
                            .and_then(|flame| flame.source_view.as_deref_mut())
                            .filter(|view| view.id == id)
                        else {
                            return;
                        };
                        let binary_id = view.request.function.binary;
                        let source_lookup_request = view.request.clone();
                        let rewrites = self
                            .source_path_rewrites
                            .get(&binary_id)
                            .cloned()
                            .unwrap_or_default();
                        self.source_binary_paths.insert(binary_id, path.clone());
                        view.binary_path_input = path.display().to_string();
                        view.restart(path.clone(), rewrites);
                        self.remember_source_binary(&source_lookup_request, path);
                    }
                    NativeFileDialogPurpose::SourceFile(id) => {
                        let Some(path) = paths.into_iter().next() else {
                            return;
                        };
                        let Some(view) = self
                            .flame
                            .as_mut()
                            .and_then(|flame| flame.source_view.as_deref_mut())
                            .filter(|view| view.id == id)
                        else {
                            return;
                        };
                        let binary_id = view.request.function.binary;
                        let source_lookup_request = view.request.clone();
                        let inferred = view.expected_source_path().and_then(|expected_path| {
                            infer_source_rewrite_from_files(expected_path, &path)
                        });
                        view.source_path_input = path.display().to_string();
                        if view.load_source_file(path) {
                            if let Some(rewrite) = inferred {
                                view.set_source_rewrite_inputs(&rewrite);
                                let rewrites =
                                    self.source_path_rewrites.entry(binary_id).or_default();
                                rewrites.retain(|existing| {
                                    existing.dwarf_prefix != rewrite.dwarf_prefix
                                });
                                rewrites.push(rewrite.clone());
                                self.remember_source_rewrite(&source_lookup_request, rewrite);
                            }
                        }
                    }
                    NativeFileDialogPurpose::SourcePrefix(id) => {
                        let Some(path) = paths.into_iter().next() else {
                            return;
                        };
                        let Some(view) = self
                            .flame
                            .as_mut()
                            .and_then(|flame| flame.source_view.as_deref_mut())
                            .filter(|view| view.id == id)
                        else {
                            return;
                        };
                        view.set_local_source_prefix(&path);
                    }
                }
            }
        }
    }

    /// Open the SSH-remote-path dialog. Desktop only — the web build
    /// has no way to spawn `ssh`, so the entry point and dialog are
    /// gated out and the toolbar button hidden.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_remote_dialog(&mut self) {
        if self.remote_input.is_none() {
            self.remote_input = Some(String::new());
        }
    }

    /// Render the modal text-input dialog used for entering an
    /// `[user@]host:path` remote URL. Returns early if no input is active.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn show_remote_input(&mut self, ctx: &egui::Context) {
        if self.remote_input.is_none() {
            return;
        }

        let mut open = true;
        let mut submit = false;
        let mut cancel = false;

        egui::Window::new("Open Remote File")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(520.0);
                ui.label("Enter an SSH path in the form [user@]host:/path/to/file.elog[.gz]");
                ui.add_space(6.0);
                let buf = self.remote_input.as_mut().expect("remote_input set");
                let resp = ui.add(
                    egui::TextEdit::singleline(buf)
                        .desired_width(f32::INFINITY)
                        .hint_text("ocadev:/octal/elog/foo.elog.gz"),
                );
                resp.request_focus();

                let url_ok = elog_parser::is_remote_path(buf.trim()).is_some();

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let open_btn =
                        ui.add_enabled(url_ok, egui::Button::new(RichText::new("Open").size(14.0)));
                    if open_btn.clicked() {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });

                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && url_ok {
                    submit = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
            });

        if !open || cancel {
            self.remote_input = None;
            return;
        }
        if submit {
            if let Some(buf) = self.remote_input.take() {
                let url = buf.trim().to_string();
                if !url.is_empty() {
                    self.start_loading(PathBuf::from(url));
                }
            }
        }
    }

    pub(crate) fn select_session(&mut self, session_id: u64) {
        if self
            .open_sessions
            .iter()
            .any(|pv| pv.summary.session_id == session_id)
        {
            self.active_tab = Tab::Session(session_id);
            return;
        }
        // Sessions seen only in the nginx provider have no TCP events;
        // open them anyway so their request lanes render.
        let Some(summary) = self
            .summaries
            .iter()
            .find(|s| s.session_id == session_id)
            .cloned()
        else {
            return;
        };
        let events = self
            .sessions
            .get(&session_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let aux = self.session_aux.get(&session_id);
        let trace_t0_us = self
            .file_info
            .as_ref()
            .map(|fi| fi.capture_start)
            .unwrap_or(0);
        self.open_sessions.push(
            PlotViewState::from_events(events, summary, aux, trace_t0_us)
                .with_cpu_strips(&self.cpu_profile)
                .with_nginx_spans(&self.nginx_profile, session_id, trace_t0_us),
        );
        self.active_tab = Tab::Session(session_id);
    }

    /// Apply any URL-supplied share parameters once, immediately after
    /// the first capture finishes loading. Drains
    /// `pending_share_params` so reopening a different file later
    /// (via the file picker) doesn't clobber it with the original
    /// link's session/time/options.
    pub(crate) fn apply_pending_share_params(&mut self) {
        let Some(params) = self.pending_share_params.take() else {
            return;
        };
        if let Some(v) = params.agg_filter_low_events {
            if self.filter_low_events != v {
                self.filter_low_events = v;
                let trace_t0_us = self
                    .file_info
                    .as_ref()
                    .map(|fi| fi.capture_start)
                    .unwrap_or(0);
                self.aggregate = Some(
                    compute_aggregate(
                        &self.summaries,
                        &self.stars_agg_samples,
                        self.filter_low_events,
                        trace_t0_us,
                    )
                    .with_cpu_strips(&self.cpu_profile, trace_t0_us)
                    .with_tcp_state(&self.tcp_samples, trace_t0_us)
                    .with_if_profile(&self.if_profile, trace_t0_us)
                    .with_ella_profile(&self.ella_profile, trace_t0_us)
                    .with_mem_series(&self.mem_series),
                );
            }
        }
        if let Some(v) = params.utc_time_labels {
            self.utc_time_labels = v;
        }
        if let Some(v) = params.show_legends {
            self.show_legends = v;
        }
        if let Some(tab) = params.tab.as_deref() {
            match tab.to_ascii_lowercase().as_str() {
                "system" | "aggregate" => self.active_tab = Tab::Aggregate,
                "sessions" | "session-list" | "ella" | "fanout" => {
                    self.active_tab = Tab::SessionList
                }
                "flame" => self.active_tab = Tab::CpuFlame,
                "processes" => self.active_tab = Tab::Processes,
                "events" => {
                    self.events_tab.open = true;
                    self.active_tab = Tab::Events;
                }
                _ => {}
            }
        }
        if let Some((pid, sid)) = params.channel {
            if self.ella_profile.channels.contains_key(&(pid, sid)) {
                self.select_ella_channel(pid, sid);
            }
        }
        if let Some(sid) = params.session {
            if self.sessions.contains_key(&sid) {
                self.select_session(sid);
            }
        }
        if let Some(plot) = params.plot.as_deref() {
            if let Some(id) = PlotId::from_chrome_slot(plot) {
                self.pending_scroll_plot = Some(id);
            }
        }
        match self.active_tab {
            Tab::Aggregate | Tab::CpuFlame => {
                if let Some([a, b]) = params.t_range {
                    self.apply_share_time_window(a, b);
                }
            }
            Tab::EllaChannel {
                provider_id,
                session_id,
            } => {
                if let Some([a, b]) = params.t_range {
                    if let Some(ch) = self
                        .open_ella_channels
                        .iter_mut()
                        .find(|c| c.provider_id == provider_id && c.session_id == session_id)
                    {
                        let lo = a.max(ch.t_min).max(0.0);
                        let hi = b.min(ch.t_max);
                        if hi > lo {
                            ch.time_axis.view_x = Some([lo, hi]);
                        }
                    }
                }
            }
            _ => {}
        }
        let Tab::Session(sid) = self.active_tab else {
            return;
        };
        let Some(idx) = self
            .open_sessions
            .iter()
            .position(|pv| pv.summary.session_id == sid)
        else {
            return;
        };
        let pv = &mut self.open_sessions[idx];
        if let Some([a, b]) = params.t_range {
            // Clamp the requested range to what the session actually
            // covers so a stale share link from before extra retention
            // doesn't push the plot off-screen.
            let lo = a.max(pv.x_min).max(0.0);
            let hi = b.min(pv.x_max);
            if hi > lo {
                pv.time_axis.view_x = Some([lo, hi]);
                pv.needs_reset = false;
            }
        }
        if let Some(v) = params.show_recovery_spans {
            pv.show_recovery_spans = v;
        }
        if let Some(v) = params.show_goodput_spans {
            pv.show_goodput_spans = v;
        }
        if let Some(v) = params.show_pacing_cap {
            pv.show_pacing_cap = v;
        }
        if let Some(v) = params.show_limiter {
            pv.show_limiter = v;
        }
        if let Some(v) = params.show_rto {
            pv.show_rto = v;
        }
        if let Some(v) = params.show_window_collapse {
            pv.show_window_collapse = v;
        }
        if let Some(v) = params.show_dsack {
            pv.show_dsack = v;
        }
        if let Some(v) = params.show_http_req {
            if v {
                pv.plot_layout.hidden.remove(&PlotId::HttpReqLanes);
                pv.plot_draft.hidden.remove(&PlotId::HttpReqLanes);
            } else {
                pv.plot_layout.hidden.insert(PlotId::HttpReqLanes);
                pv.plot_draft.hidden.insert(PlotId::HttpReqLanes);
            }
            pv.show_http_req = v;
        }
        if let Some(i) = params.maximized_plot {
            if let Some(id) = PlotId::from_session_index(i) {
                pv.plot_layout.maximized = Some(id);
                pv.plot_draft.maximized = Some(id);
            }
        }
        for (i, v) in params.y_max_override.iter().enumerate() {
            if let Some(v) = v {
                pv.y_max_override[i] = Some(*v);
            }
        }
    }

    /// System + Flame time window from share `t=` (seconds since capture
    /// start). Seeds `pending_flame_filter` when the Flame tab has not
    /// been opened yet.
    fn apply_share_time_window(&mut self, lo_s: f64, hi_s: f64) {
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let lo = lo_s.max(0.0);
        let hi = hi_s.min(x_max);
        if hi <= lo {
            return;
        }
        self.agg_time_axis.view_x = Some([lo, hi]);
        let Some(capture_start) = self.file_info.as_ref().map(|fi| fi.capture_start) else {
            return;
        };
        let lo_us = capture_start.saturating_add((lo * 1_000_000.0).round() as u64);
        let hi_us = capture_start.saturating_add((hi * 1_000_000.0).round() as u64);
        if let Some(flame) = self.flame.as_mut() {
            flame.filter.ts_start_us = lo_us;
            flame.filter.ts_end_us = hi_us;
            flame.zoom_path.clear();
            flame.back_stack.clear();
            Self::start_flame_rebuild(flame, &self.cpu_profile, &mut self.flame_build);
        } else {
            let mut filter = self.pending_flame_filter.take().unwrap_or_default();
            filter.ts_start_us = lo_us;
            filter.ts_end_us = hi_us;
            self.pending_flame_filter = Some(filter);
        }
    }

    /// Flame highlight (or System zoom if the filter is the full capture)
    /// as share `t=` seconds.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fn flame_share_t_range(&self) -> Option<[f64; 2]> {
        let capture_start = self.file_info.as_ref()?.capture_start;
        let (lo_us, hi_us) = if let Some(flame) = self.flame.as_ref() {
            if flame.filter.ts_start_us > 0 || flame.filter.ts_end_us != u64::MAX {
                (flame.filter.ts_start_us, flame.filter.ts_end_us)
            } else {
                return self.agg_time_axis.view_x;
            }
        } else if let Some(filter) = self.pending_flame_filter.as_ref() {
            if filter.ts_start_us > 0 || filter.ts_end_us != u64::MAX {
                (filter.ts_start_us, filter.ts_end_us)
            } else {
                return self.agg_time_axis.view_x;
            }
        } else {
            return self.agg_time_axis.view_x;
        };
        let lo = (lo_us as i64 - capture_start as i64) as f64 / 1_000_000.0;
        let hi = if hi_us == u64::MAX {
            self.aggregate.as_ref().map(|a| a.time_max).unwrap_or(lo)
        } else {
            (hi_us as i64 - capture_start as i64) as f64 / 1_000_000.0
        };
        if hi > lo {
            Some([lo.max(0.0), hi])
        } else {
            None
        }
    }

    /// Snapshot the current view state as a `ShareParams`, suitable
    /// for either rendering as a query string (Share popup) or for
    /// comparing against defaults. Only includes the active
    /// session/time/options the sharer is actually looking at; tabs
    /// that aren't currently focused are intentionally omitted to keep
    /// share URLs short and predictable.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn current_share_params(&self) -> ShareParams {
        let mut p = ShareParams {
            file_urls: self.loaded_file_urls.clone(),
            ..ShareParams::default()
        };
        if !self.filter_low_events {
            p.agg_filter_low_events = Some(false);
        }
        if self.utc_time_labels {
            p.utc_time_labels = Some(true);
        }
        match self.active_tab {
            Tab::Aggregate => {
                p.tab = Some("system".to_string());
                if let Some(range) = self.agg_time_axis.view_x {
                    p.t_range = Some(range);
                }
            }
            Tab::SessionList => p.tab = Some("sessions".to_string()),
            Tab::CpuFlame => {
                p.tab = Some("flame".to_string());
                if let Some(range) = self.flame_share_t_range() {
                    p.t_range = Some(range);
                }
            }
            Tab::Processes => {
                p.tab = Some("processes".to_string());
                if let Some(range) = self.flame_share_t_range() {
                    p.t_range = Some(range);
                }
            }
            Tab::EllaChannel {
                provider_id,
                session_id,
            } => {
                p.channel = Some((provider_id, session_id));
                if let Some(ch) = self
                    .open_ella_channels
                    .iter()
                    .find(|c| c.provider_id == provider_id && c.session_id == session_id)
                {
                    if let Some(range) = ch.time_axis.view_x {
                        p.t_range = Some(range);
                    }
                }
            }
            Tab::Session(_) => {}
            Tab::Events => p.tab = Some("events".to_string()),
        }
        if let Tab::Session(sid) = self.active_tab {
            if let Some(pv) = self
                .open_sessions
                .iter()
                .find(|pv| pv.summary.session_id == sid)
            {
                p.session = Some(sid);
                if let Some(range) = pv.time_axis.view_x {
                    p.t_range = Some(range);
                }
                if !pv.show_recovery_spans {
                    p.show_recovery_spans = Some(false);
                }
                if pv.show_goodput_spans {
                    p.show_goodput_spans = Some(true);
                }
                if pv.show_pacing_cap {
                    p.show_pacing_cap = Some(true);
                }
                if pv.show_limiter {
                    p.show_limiter = Some(true);
                }
                if pv.show_rto {
                    p.show_rto = Some(true);
                }
                if pv.show_window_collapse {
                    p.show_window_collapse = Some(true);
                }
                if pv.show_dsack {
                    p.show_dsack = Some(true);
                }
                if !pv.plot_layout.is_visible(PlotId::HttpReqLanes) {
                    p.show_http_req = Some(false);
                }
                if !self.show_legends {
                    p.show_legends = Some(false);
                }
                if let Some(id) = pv.plot_layout.maximized {
                    if let Some(i) = id.session_index() {
                        p.maximized_plot = Some(i);
                    }
                }
                p.y_max_override = pv.y_max_override;
            }
        }
        p
    }

    /// Build the full shareable URL for the current view (web only).
    /// Returns the page's origin + path with the encoded query string
    /// appended. Returns `None` if no `window`/`location` is available
    /// (extremely unlikely; the wasm runtime always has one).
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn current_share_url(&self) -> Option<String> {
        let window = web_sys::window()?;
        let location = window.location();
        let origin = location.origin().ok()?;
        let pathname = location.pathname().ok().unwrap_or_else(|| "/".into());
        let query = self.current_share_params().to_query_string();
        if query.is_empty() {
            Some(format!("{origin}{pathname}"))
        } else {
            Some(format!("{origin}{pathname}?{query}"))
        }
    }

    /// The cache name offered as the upload default: the capture's
    /// own `MACHINE_INFO` hostname when it has one, else the
    /// share server's filename rule.
    pub(crate) fn default_cache_name(&self) -> String {
        if let Some(host) = self
            .file_info
            .as_ref()
            .and_then(|fi| fi.machine.as_ref())
            .map(|mi| mi.hostname.trim())
            .filter(|h| !h.is_empty())
        {
            return host.to_string();
        }
        #[cfg(target_arch = "wasm32")]
        if let Some((name, _)) = self.loaded_local_files.first() {
            let inferred = elog_parser::infer_cache_name(name);
            if !inferred.is_empty() {
                return inferred;
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(p) = self.loaded_paths.first() {
            let inferred = elog_parser::infer_cache_name(&p.to_string_lossy());
            if !inferred.is_empty() {
                return inferred;
            }
        }
        String::new()
    }

    /// Render the Share overlay (web). Toggled by the 🔗 button in
    /// the tab bar. The URL lives in a real HTML `<textarea>`
    /// (`#ee-share-overlay` in `index.html`) so the browser owns Copy,
    /// Ctrl+C, and the context menu. When the capture came from the
    /// file picker there is no link to show; the overlay instead
    /// offers an upload to the share server, and a finished upload swaps
    /// in the canonical link.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn show_share_popup(&mut self, ctx: &egui::Context) {
        if !self.share_popup_open {
            hide_share_overlay();
            return;
        }
        if let Some((cache, note)) = share_overlay_upload_request() {
            if cache.is_empty() {
                self.share_upload_error = Some("Cache name is required".to_string());
            } else if self.share_upload.is_none() {
                self.share_upload_error = None;
                self.start_share_upload_web(cache, note);
            }
        }
        // Drain a finished upload: the link section replaces the card.
        let finished = if let Some(job) = &mut self.share_upload {
            match &job.receiver {
                Some(rx) => match rx.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err("upload worker dropped".to_string()))
                    }
                },
                None => None,
            }
        } else {
            None
        };
        if let Some(result) = finished {
            match result {
                Ok(records) => {
                    let urls: Vec<String> = records.iter().map(|r| r.canonical_url()).collect();
                    replace_file_query(&urls);
                    self.loaded_file_urls = urls;
                    self.share_upload = None;
                    self.share_upload_error = None;
                }
                Err(e) => {
                    if let Some(job) = &mut self.share_upload {
                        job.receiver = None;
                        job.outcome = Some(Err(e));
                    }
                }
            }
        }
        let url = self.current_share_url().unwrap_or_default();
        let has_file_url = !self.loaded_file_urls.is_empty() || !elog_parser::uploads_configured();
        let view = if has_file_url {
            None
        } else {
            Some(self.share_upload_view())
        };
        sync_share_overlay(&url, has_file_url, view.as_ref());
        ctx.request_repaint();
        if share_overlay_wants_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.share_popup_open = false;
            hide_share_overlay();
        }
    }

    /// Snapshot of the upload card for `sync_share_overlay`: prefill
    /// on first open, then the in-flight (or failed) job's status.
    #[cfg(target_arch = "wasm32")]
    fn share_upload_view(&self) -> ShareUploadView {
        let prefill_cache = self.default_cache_name();
        match &self.share_upload {
            Some(job) => {
                let (status, is_error) = match &job.outcome {
                    Some(Err(e)) => (e.clone(), true),
                    _ => (job.status.lock().unwrap().clone(), false),
                };
                ShareUploadView {
                    prefill_cache,
                    busy: job.receiver.is_some(),
                    permille: job.progress.load(Ordering::Relaxed),
                    status,
                    is_error,
                }
            }
            None => ShareUploadView {
                prefill_cache,
                busy: false,
                permille: 0,
                status: self.share_upload_error.clone().unwrap_or_default(),
                is_error: self.share_upload_error.is_some(),
            },
        }
    }

    /// Kick off the browser upload of every picked file (usually one;
    /// a rolled set uploads each in turn and links the whole set).
    #[cfg(target_arch = "wasm32")]
    fn start_share_upload_web(&mut self, cache_name: String, note: String) {
        let files = self.loaded_local_files.clone();
        if files.is_empty() {
            self.share_upload_error =
                Some("no local file bytes are available to upload".to_string());
            return;
        }
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(AtomicU32::new(0));
        let status = Arc::new(std::sync::Mutex::new(String::new()));
        {
            let progress = progress.clone();
            let status = status.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let n = files.len().max(1) as u32;
                let mut records = Vec::with_capacity(files.len());
                for (i, (_name, file)) in files.iter().enumerate() {
                    let i = i as u32;
                    let on_progress: std::rc::Rc<dyn Fn(u32, &str)> = std::rc::Rc::new({
                        let progress = progress.clone();
                        let status = status.clone();
                        move |p: u32, stage: &str| {
                            progress.store(i * 1000 / n + p / n, Ordering::Relaxed);
                            *status.lock().unwrap() = if n > 1 {
                                format!("[{}/{n}] {stage}", i + 1)
                            } else {
                                stage.to_string()
                            };
                        }
                    });
                    match elog_parser::upload_file_web(file, &cache_name, &note, on_progress).await
                    {
                        Ok(rec) => records.push(rec),
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            return;
                        }
                    }
                }
                let _ = tx.send(Ok(records));
            });
        }
        self.share_upload = Some(ShareUpload {
            progress,
            status,
            receiver: Some(rx),
            outcome: None,
        });
    }

    /// Render the Share dialog (desktop). The 🔗 button opens it;
    /// "Upload & Copy Link" pushes the capture to the share server on a
    /// background thread and lands the permalink on the clipboard.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn show_share_dialog(&mut self, ctx: &egui::Context) {
        if !self.share_popup_open {
            return;
        }
        self.poll_share_upload_native(ctx);
        if self.share_form.is_none() {
            self.share_form = Some(ShareForm {
                cache_name: self.default_cache_name(),
                note: String::new(),
            });
        }
        let mut open = true;
        // Stay inside the app window. Width-filling fields lock egui's
        // remembered size, so set the width explicitly every frame.
        let screen = ctx.screen_rect();
        let max_w = (screen.width() - 64.0).clamp(200.0, 560.0);
        let max_h = (screen.height() - 80.0).max(160.0);
        egui::Window::new("Share Capture")
            .collapsible(false)
            .resizable(false)
            .max_width(max_w)
            .max_height(max_h)
            .vscroll(true)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(max_w);
                ui.label("Upload to the share server to get a link anyone with access can open.");
                if let Some(note) = elog_parser::upload_note() {
                    ui.label(note);
                }
                ui.add_space(6.0);

                // What will be uploaded.
                let mut total = 0u64;
                let mut names = Vec::new();
                for p in &self.loaded_paths {
                    let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                    total += size;
                    names.push(format!(
                        "{} ({})",
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| p.to_string_lossy().into_owned()),
                        format_bytes(size)
                    ));
                }
                let summary = if names.len() > 1 {
                    format!("{} files, {} total", names.len(), format_bytes(total))
                } else {
                    names.into_iter().next().unwrap_or_default()
                };
                ui.label(RichText::new(summary).strong());
                ui.add_space(6.0);

                let form = self.share_form.as_mut().expect("share_form set");
                ui.horizontal(|ui| {
                    ui.label("Cache name");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.cache_name)
                            .desired_width(f32::INFINITY)
                            .hint_text("host name"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Note");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.note)
                            .desired_width(f32::INFINITY)
                            .hint_text("repro for ticket X (optional)"),
                    );
                });
                ui.add_space(8.0);

                match &self.share_upload {
                    None => {
                        let remote_only = !self.loaded_paths.is_empty()
                            && self.loaded_paths.iter().all(|p| {
                                elog_parser::is_remote_path(&p.to_string_lossy()).is_some()
                            });
                        let ready = !form.cache_name.trim().is_empty()
                            && !self.loaded_paths.is_empty()
                            && !remote_only;
                        if remote_only {
                            ui.label(
                                RichText::new(
                                    "Captures loaded over SSH can't be uploaded from here; \
                                     upload them with `elog_cli upload` on that host instead.",
                                )
                                .size(12.0),
                            );
                        }
                        let btn = ui.add_enabled(ready, egui::Button::new("Upload & Copy Link"));
                        if btn.clicked() {
                            self.start_share_upload_native();
                        }
                    }
                    Some(job) => {
                        if job.receiver.is_some() {
                            let permille = job.progress.load(Ordering::Relaxed);
                            ui.add(
                                egui::ProgressBar::new(permille as f32 / 1000.0).show_percentage(),
                            );
                            ui.label(job.status.lock().unwrap().clone());
                            ctx.request_repaint_after(std::time::Duration::from_millis(100));
                        }
                        match &job.outcome {
                            Some(Ok(outcome)) => {
                                let rec = &outcome.records[0];
                                ui.label(
                                    RichText::new(format!(
                                        "Uploaded as {} for {}. Link copied to clipboard.",
                                        rec.user, rec.cache_name
                                    ))
                                    .color(Color32::from_rgb(0x6c, 0xc6, 0x44)),
                                );
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&outcome.link).monospace().size(11.0),
                                    )
                                    .selectable(true)
                                    .wrap(true),
                                );
                                let mut copy_again = false;
                                ui.horizontal(|ui| {
                                    if ui.button("Copy again").clicked() {
                                        copy_again = true;
                                    }
                                    if ui.button("Close").clicked() {
                                        self.share_popup_open = false;
                                    }
                                });
                                if copy_again {
                                    let link = outcome.link.clone();
                                    self.share_clipboard_copy(ctx, link);
                                }
                            }
                            Some(Err(e)) => {
                                ui.label(
                                    RichText::new(format!("Upload failed: {e}"))
                                        .color(Color32::from_rgb(0xe0, 0x50, 0x50)),
                                );
                                ui.horizontal(|ui| {
                                    if ui.button("Retry").clicked() {
                                        self.share_upload = None;
                                    }
                                    if ui.button("Close").clicked() {
                                        self.share_popup_open = false;
                                    }
                                });
                            }
                            None => {}
                        }
                    }
                }
            });
        if !open {
            self.share_popup_open = false;
        }
    }

    /// Put text on the desktop clipboard: arboard directly (egui's
    /// `copied_text` path has proven unreliable on X11), with
    /// `copied_text` as the fallback if arboard can't init.
    #[cfg(not(target_arch = "wasm32"))]
    fn share_clipboard_copy(&mut self, ctx: &egui::Context, text: String) {
        if self.share_clipboard.is_none() {
            self.share_clipboard = arboard::Clipboard::new().ok();
        }
        let copied = match self.share_clipboard.as_mut() {
            Some(cb) => cb.set_text(text.clone()).is_ok(),
            None => false,
        };
        if !copied {
            ctx.output_mut(|o| o.copied_text = text);
        }
    }

    /// Drain a finished desktop upload: build the permalink from the
    /// current view state and put it on the clipboard.
    #[cfg(not(target_arch = "wasm32"))]
    fn poll_share_upload_native(&mut self, ctx: &egui::Context) {
        let result = {
            let Some(job) = &mut self.share_upload else {
                return;
            };
            let Some(rx) = &job.receiver else {
                return;
            };
            match rx.try_recv() {
                Ok(r) => r,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => Err("upload worker dropped".to_string()),
            }
        };
        let outcome = match result {
            Ok(records) => {
                let mut p = self.current_share_params();
                p.file_urls = records.iter().map(|r| r.canonical_url()).collect();
                let link = format!(
                    "{}?{}",
                    elog_parser::eventexplorer_page_url(),
                    p.to_query_string()
                );
                self.share_clipboard_copy(ctx, link.clone());
                Ok(ShareUploadOutcome { records, link })
            }
            Err(e) => Err(e),
        };
        if let Some(job) = &mut self.share_upload {
            job.receiver = None;
            job.outcome = Some(outcome);
        }
    }

    /// Spawn the desktop upload worker over `loaded_paths`.
    #[cfg(not(target_arch = "wasm32"))]
    fn start_share_upload_native(&mut self) {
        let Some(form) = &self.share_form else {
            return;
        };
        let cache_name = form.cache_name.trim().to_string();
        let note = form.note.trim().to_string();
        let paths = self.loaded_paths.clone();
        if paths.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(AtomicU32::new(0));
        let status = Arc::new(std::sync::Mutex::new(String::new()));
        {
            let progress = progress.clone();
            let status = status.clone();
            std::thread::spawn(move || {
                let n = paths.len().max(1) as u32;
                let status_cb = |s: &str| *status.lock().unwrap() = s.to_string();
                let mut records = Vec::with_capacity(paths.len());
                for (i, p) in paths.iter().enumerate() {
                    let i = i as u32;
                    match elog_parser::upload_file(
                        p,
                        &cache_name,
                        &note,
                        &progress,
                        (i * 1000 / n, (i + 1) * 1000 / n),
                        &status_cb,
                    ) {
                        Ok(rec) => records.push(rec),
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            return;
                        }
                    }
                }
                let _ = tx.send(Ok(records));
            });
        }
        self.share_upload = Some(ShareUpload {
            progress,
            status,
            receiver: Some(rx),
            outcome: None,
        });
    }

    pub(crate) fn snapshot_tab_chrome_escape(ctx: &egui::Context) {
        snapshot_tab_chrome_escape(ctx);
    }

    pub(crate) fn escape_restores_plot_maximize(ui: &egui::Ui) -> bool {
        escape_restores_plot_maximize(ui)
    }

    pub(crate) fn show_dark_checkbox(&mut self, ui: &mut egui::Ui) {
        if ui
            .checkbox(&mut self.dark_mode, RichText::new("Dark").size(13.0))
            .on_hover_text("egui dark palette. Uncheck for a light theme.")
            .changed()
        {
            save_dark_mode(self.dark_mode);
            apply_visuals(ui.ctx(), self.dark_mode);
        }
    }

    pub(crate) fn show_global_gear(&mut self, ui: &mut egui::Ui) {
        let popup_id = tab_gear_popup_id();
        let is_open = ui
            .memory(|m| m.data.get_temp::<bool>(popup_id))
            .unwrap_or(false);
        let (rect, response) =
            tab_chrome_glyph_button(ui, "\u{2699}", 14.0, is_open, "Display options");
        if response.clicked() {
            let next = !is_open;
            ui.memory_mut(|m| m.data.insert_temp(popup_id, next));
            if next {
                tab_chrome_close_others(ui, popup_id);
            }
        }
        if !is_open {
            return;
        }
        let area = egui::Area::new(popup_id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.right_bottom() + egui::vec2(0.0, 4.0))
            .pivot(egui::Align2::RIGHT_TOP)
            .constrain_to(ui.ctx().screen_rect())
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(220.0);
                    self.show_dark_checkbox(ui);
                    ui.checkbox(&mut self.utc_time_labels, RichText::new("UTC").size(13.0))
                        .on_hover_text(
                            "Show absolute UTC clock time on time axes and cursor \
                             tooltips instead of seconds since capture start.",
                        );
                    ui.checkbox(&mut self.show_legends, RichText::new("Legends").size(13.0))
                        .on_hover_text(
                            "Show the per-plot legend box. Turn off to reclaim \
                         screen space or to stop the legend from obscuring data.",
                        );
                    ui.add_space(6.0);
                    ui.label(RichText::new("Time buckets").size(13.0));
                    let bins_slider = ui.add(
                        egui::Slider::new(&mut self.plot_bins, PLOT_BINS_MIN..=PLOT_BINS_MAX)
                            .logarithmic(true)
                            .text("buckets"),
                    );
                    if bins_slider.changed() {
                        save_plot_bins(self.plot_bins);
                    }
                    bins_slider.on_hover_text(
                        "Equal-time buckets each time-series plot uses across \
                         the visible window. Zooming re-bins at this count. \
                         Remembered across launches. Default is 250.",
                    );
                });
            });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.memory_mut(|m| m.data.insert_temp(popup_id, false));
        } else if ui.input(|i| i.pointer.any_click()) {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if !rect.contains(pos) && !area.response.rect.contains(pos) {
                    ui.memory_mut(|m| m.data.insert_temp(popup_id, false));
                }
            }
        }
    }

    pub(crate) fn show_tab_info_button(&mut self, ui: &mut egui::Ui) {
        let info_id = tab_info_popup_id();
        let info_open = ui
            .memory(|m| m.data.get_temp::<bool>(info_id))
            .unwrap_or(false);
        let (info_rect, info_response) =
            tab_chrome_glyph_button(ui, "i", 13.0, info_open, "Trace info");
        if info_response.clicked() {
            let next = !info_open;
            ui.memory_mut(|m| m.data.insert_temp(info_id, next));
            if next {
                tab_chrome_close_others(ui, info_id);
            }
        }
        if info_open && self.show_aggregate_info_popup(ui, info_id, info_rect) {
            self.events_tab.open = true;
            self.active_tab = Tab::Events;
            ui.memory_mut(|m| m.data.insert_temp(info_id, false));
        }
    }

    pub(crate) fn show_plot_help_button(&mut self, ui: &mut egui::Ui) {
        let help_id = tab_help_popup_id();
        let help_open = ui
            .memory(|m| m.data.get_temp::<bool>(help_id))
            .unwrap_or(false);
        let (help_rect, help_response) =
            tab_chrome_glyph_button(ui, "?", 13.0, help_open, "Plot mouse and keyboard");
        if help_response.clicked() {
            let next = !help_open;
            ui.memory_mut(|m| m.data.insert_temp(help_id, next));
            if next {
                tab_chrome_close_others(ui, help_id);
            }
        }
        if !help_open {
            return;
        }

        const POPUP_MIN_W: f32 = 360.0;
        const POPUP_MAX_W: f32 = 440.0;
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

                ui.label(RichText::new("Time plots").strong().size(12.0));
                ui.separator();
                egui::Grid::new("plot_help_mouse")
                    .num_columns(2)
                    .spacing([16.0, 2.0])
                    .show(ui, |ui| {
                        row(ui, "Hover", "Shared cursor line and series tooltip");
                        row(ui, "Left-drag", "Box-zoom the shared time window");
                        row(
                            ui,
                            "Shift+left-drag",
                            "Slide the zoom window without resizing",
                        );
                        row(ui, "Double-click", "Undo one zoom; again to reset");
                        row(ui, "Middle-click", "Reset zoom to the full capture");
                        row(ui, "Right-drag", "Measure Δt across the plots");
                        row(ui, "Shift+right-drag", "Slide the Δt band without resizing");
                        row(ui, "Right-click Y-axis", "Set that plot's Y maximum");
                        row(ui, "Legend", "Click a name to hide or show a series");
                        row(ui, "Sizer under plot", "Drag to change that plot's height");
                        row(ui, "Maximize", "Upper-right expand; Escape restores");
                    });

                ui.add_space(6.0);
                ui.label(RichText::new("Keys").strong().size(12.0));
                ui.separator();
                egui::Grid::new("plot_help_keys")
                    .num_columns(2)
                    .spacing([16.0, 2.0])
                    .show(ui, |ui| {
                        row(ui, "Escape", "Restore a maximized plot");
                    });
                ui.add_space(2.0);
                ui.label(
                    RichText::new(
                        "The mouse wheel scrolls the tab; plots do not \
                         wheel-zoom. The Flame canvas has its own ?.",
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

    pub(crate) fn show_no_file(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            self.show_dark_checkbox(ui);
        });

        let has_recent = !self.recent_files.is_empty();
        let top_space = if has_recent { 0.08 } else { 0.14 };
        let weak = ui.visuals().weak_text_color();
        let stroke = Color32::from_rgba_unmultiplied(0x00, 0xB0, 0xD8, 70);

        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * top_space);
            #[cfg(target_arch = "wasm32")]
            let card_w = ui.available_width().min(560.0);
            #[cfg(not(target_arch = "wasm32"))]
            let card_w = ui.available_width().min(480.0);
            egui::Frame::none()
                .fill(ui.visuals().faint_bg_color)
                .rounding(10.0)
                .stroke(Stroke::new(1.0_f32, stroke))
                .inner_margin(egui::Margin::same(28.0))
                .show(ui, |ui| {
                    ui.set_width(card_w - 56.0);
                    ui.vertical_centered(|ui| {
                        paint_splash_mark(ui, 56.0);
                        ui.add_space(14.0);
                        ui.label(RichText::new("EventExplorer").size(26.0).strong());
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("FreeBSD EventLog analyzer")
                                .size(14.0)
                                .color(weak),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                .size(12.0)
                                .color(weak),
                        );
                        if let Some(err) = &self.open_error {
                            ui.add_space(14.0);
                            show_open_error(ui, err, weak);
                        }
                        ui.add_space(20.0);

                        let style = ui.style();
                        let font = egui::FontId::proportional(16.0);
                        let pad = style.spacing.button_padding;
                        let item_spacing = 8.0_f32;
                        let measure = |s: &str| {
                            ui.fonts(|f| {
                                f.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE)
                            })
                            .size()
                            .x
                        };
                        #[cfg(not(target_arch = "wasm32"))]
                        let row_width = measure("Open File...")
                            + measure("Open Remote...")
                            + 4.0 * pad.x
                            + item_spacing;
                        #[cfg(target_arch = "wasm32")]
                        let row_width = measure("Open File...") + 2.0 * pad.x;
                        ui.allocate_ui_with_layout(
                            egui::vec2(row_width, 32.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.spacing_mut().item_spacing.x = item_spacing;
                                if ui
                                    .button(RichText::new("Open File...").size(16.0))
                                    .clicked()
                                {
                                    self.open_file_dialog();
                                }
                                #[cfg(not(target_arch = "wasm32"))]
                                if ui
                                    .button(RichText::new("Open Remote...").size(16.0))
                                    .clicked()
                                {
                                    self.open_remote_dialog();
                                }
                            },
                        );
                    });

                    #[cfg(target_arch = "wasm32")]
                    {
                        ui.add_space(22.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new("Or open from a URL").size(13.0).color(weak));
                        });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            let edit = egui::TextEdit::singleline(&mut self.splash_url)
                                .hint_text(
                                    "https://host/capture.elog.gz (space-separate a rolled set)",
                                )
                                .desired_width((ui.available_width() - 64.0).max(160.0));
                            let resp = ui.add(edit);
                            let go = ui.button("Open").clicked()
                                || (resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                            if go {
                                self.open_splash_url();
                            }
                        });
                        ui.add_space(6.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new(
                                    "http(s) only, and the host must allow this origin (CORS).",
                                )
                                .size(11.0)
                                .color(weak),
                            );
                        });
                    }
                });

            #[cfg(target_arch = "wasm32")]
            self.show_hosted_logs(ui);

            #[cfg(not(target_arch = "wasm32"))]
            if has_recent {
                ui.add_space(28.0);
                ui.label(RichText::new("Recent Files").strong().size(15.0));
                ui.add_space(8.0);

                let mut selected: Option<Vec<PathBuf>> = None;
                for group in &self.recent_files {
                    let file_name = capture_label(group);

                    let parent = group
                        .first()
                        .and_then(|p| p.parent())
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();

                    let resp = ui
                        .add(egui::Button::new(RichText::new(&file_name).size(14.0)).frame(false));
                    if !parent.is_empty() {
                        ui.label(
                            RichText::new(&parent)
                                .size(11.0)
                                .color(Color32::from_gray(120)),
                        );
                    }
                    ui.add_space(4.0);

                    if resp.clicked() {
                        selected = Some(group.clone());
                    }
                }

                if let Some(paths) = selected {
                    self.start_loading_paths(paths);
                }
            }
        });
    }

    fn fail_open(&mut self, msg: String) {
        eprintln!("{msg}");
        self.loading_paths.clear();
        #[cfg(not(target_arch = "wasm32"))]
        self.loaded_paths.clear();
        self.loaded_file_name = None;
        self.open_error = Some(msg);
        self.view = View::NoFile;
    }

    #[cfg(target_arch = "wasm32")]
    fn reset_to_splash(&mut self) {
        self.loading_paths.clear();
        self.loaded_file_name = None;
        self.loaded_file_urls.clear();
        self.loaded_local_files.clear();
        self.open_error = None;
        self.open_sessions.clear();
        self.open_ella_channels.clear();
        self.share_popup_open = false;
        self.share_upload = None;
        self.share_upload_error = None;
        self.current_title = None;
        self.view = View::NoFile;
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn drain_history_nav(&mut self) {
        while let Ok(search) = self.pending_history_nav.try_recv() {
            self.apply_history_search(&search);
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn apply_history_search(&mut self, search: &str) {
        let params = ShareParams::from_query_string(search);
        if params.file_urls.is_empty() {
            if !matches!(self.view, View::NoFile) {
                self.reset_to_splash();
            }
            return;
        }
        let already = self.loaded_file_urls == params.file_urls
            && matches!(self.view, View::Loaded | View::Loading { .. });
        if already {
            return;
        }
        let urls = params.file_urls.clone();
        if params.has_post_load_state() {
            self.pending_share_params = Some(params);
        }
        self.start_loading_urls(urls);
    }

    #[cfg(target_arch = "wasm32")]
    fn open_splash_url(&mut self) {
        let urls = split_splash_urls(&self.splash_url);
        if urls.is_empty() {
            return;
        }
        push_file_query(&urls);
        self.start_loading_urls(urls);
    }

    pub(crate) fn show_loading(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let (file_name, permille, status, done) = match &self.view {
            View::Loading {
                file_name,
                progress,
                receiver,
            } => {
                let p = progress.permille();
                let s = progress.status();
                let result = receiver.try_recv().ok();
                (file_name.clone(), p, s, result)
            }
            _ => return,
        };

        if let Some(result) = done {
            match result {
                Ok((
                    sessions,
                    summaries,
                    stars_agg_samples,
                    cpu_profile,
                    if_profile,
                    tcp_sample_profile,
                    ella_profile,
                    nginx_profile,
                    aux,
                    file_info,
                    providers,
                    aggregate,
                    hpts_wheel_events,
                    mem_series,
                )) => {
                    self.events_tab = EventsTabState::default();
                    // Aggregate-only content (no per-connection sessions):
                    // interface, TCP, ELLA, or NIC-rate samples still make
                    // the Aggregate tab (which now hosts them all) worth
                    // landing on.
                    let agg_only = !if_profile.is_empty()
                        || !tcp_sample_profile.is_empty()
                        || !stars_agg_samples.is_empty()
                        || !hpts_wheel_events.is_empty()
                        || !ella_profile.is_empty()
                        || !nginx_profile.is_empty()
                        || !mem_series.is_empty();
                    // A capture is "useful" if it has at least one TCP
                    // session, cpu-provider data, or aggregate-only content
                    // (e.g. `if`-provider interface samples, which is what
                    // an `if`-only capture -- no tcp/cpu providers enabled
                    // at all -- produces).
                    if summaries.is_empty() && cpu_profile.is_empty() && !agg_only {
                        self.fail_open(format!(
                            "No TCP sessions, cpu samples, or aggregate data found in {file_name}"
                        ));
                        return;
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        self.loaded_paths = std::mem::take(&mut self.loading_paths);
                        add_recent_capture(&mut self.recent_files, self.loaded_paths.clone());
                    }
                    #[cfg(target_arch = "wasm32")]
                    add_recent_capture(
                        &mut self.recent_files,
                        std::mem::take(&mut self.loading_paths),
                    );
                    self.loaded_file_name = Some(file_name.clone());
                    let multi = summaries.len() > 1;
                    let has_cpu = !cpu_profile.is_empty();
                    let system_available = multi
                        || !if_profile.is_empty()
                        || !tcp_sample_profile.state_samples.is_empty()
                        || !tcp_sample_profile.rps_samples.is_empty()
                        || !tcp_sample_profile.gp_samples.is_empty()
                        || !tcp_sample_profile.send_samples.is_empty()
                        || !tcp_sample_profile.lro_flush_samples.is_empty()
                        || !tcp_sample_profile.srtt_samples.is_empty()
                        || !nginx_profile.requests.is_empty()
                        || !nginx_profile.is_empty()
                        || !stars_agg_samples.is_empty()
                        || !hpts_wheel_events.is_empty()
                        || !mem_series.is_empty()
                        || ella_profile.has_channel_counts()
                        || ella_profile.has_frame_timing()
                        || (has_cpu && !summaries.is_empty());
                    // Tab focus: System whenever it has any catalog
                    // content; cpu-only -> Flame; else the lone session.
                    if summaries.is_empty() && !system_available && has_cpu {
                        self.active_tab = Tab::CpuFlame;
                    } else if system_available {
                        self.active_tab = Tab::Aggregate;
                    } else {
                        let top = &summaries[0];
                        let events = &sessions[&top.session_id];
                        let plot_state = PlotViewState::from_events(
                            events,
                            top.clone(),
                            aux.get(&top.session_id),
                            file_info.capture_start,
                        )
                        .with_cpu_strips(&cpu_profile);
                        self.active_tab = Tab::Session(top.session_id);
                        self.open_sessions.push(plot_state);
                    }
                    // Optional override: `EVENT_EXPLORER_TAB=processes`
                    // (or aggregate / sessions / flame / ella) overrides the
                    // auto-focus rule above. `EVENT_EXPLORER_FLAME_SEARCH`
                    // seeds the flame-tab search box. Useful for headless
                    // screenshot validation and for users who prefer
                    // a non-default landing tab. Unknown values are
                    // ignored. Only honoured when the requested tab
                    // is actually available for this capture (e.g.
                    // `processes` on a tcp-only file silently falls
                    // back to the auto-focused tab).
                    if let Ok(v) = std::env::var("EVENT_EXPLORER_TAB") {
                        match v.to_ascii_lowercase().as_str() {
                            "sessions" | "session-list" | "ella" | "fanout"
                                if !summaries.is_empty() || !ella_profile.channels.is_empty() =>
                            {
                                self.active_tab = Tab::SessionList
                            }
                            "flame" if has_cpu => self.active_tab = Tab::CpuFlame,
                            "processes" if has_cpu => self.active_tab = Tab::Processes,
                            "events" => {
                                self.events_tab.open = true;
                                self.active_tab = Tab::Events;
                            }
                            "aggregate" | "interfaces" | "network" | "tcp-stack" | "tcpstack"
                                if system_available || multi =>
                            {
                                self.active_tab = Tab::Aggregate
                            }
                            "session-top" | "session-hero" if !summaries.is_empty() => {
                                let top = &summaries[0];
                                if !self
                                    .open_sessions
                                    .iter()
                                    .any(|pv| pv.summary.session_id == top.session_id)
                                {
                                    let events = sessions
                                        .get(&top.session_id)
                                        .map(Vec::as_slice)
                                        .unwrap_or(&[]);
                                    let mut pv = PlotViewState::from_events(
                                        events,
                                        top.clone(),
                                        aux.get(&top.session_id),
                                        file_info.capture_start,
                                    )
                                    .with_cpu_strips(&cpu_profile)
                                    .with_nginx_spans(
                                        &nginx_profile,
                                        top.session_id,
                                        file_info.capture_start,
                                    );
                                    if v.to_ascii_lowercase().as_str() == "session-hero" {
                                        pv.show_limiter = true;
                                        pv.show_dsack = true;
                                        pv.show_recovery_spans = true;
                                        pv.plot_layout.hidden.insert(PlotId::SizeCdf);
                                        pv.plot_draft.hidden.insert(PlotId::SizeCdf);
                                        pv.open_gear_menu = true;
                                    }
                                    self.open_sessions.push(pv);
                                }
                                self.active_tab = Tab::Session(top.session_id);
                            }
                            _ => {}
                        }
                    }
                    self.sessions = sessions;
                    self.session_aux = aux;
                    self.summaries = summaries;
                    self.stars_agg_samples = stars_agg_samples;
                    self.cpu_profile = cpu_profile;
                    self.if_profile = if_profile;
                    self.tcp_samples = tcp_sample_profile;
                    self.ella_has_frame_timing = ella_profile.has_frame_timing();
                    self.ella_fanout_ids = ella_profile.fanout_provider_ids();
                    self.ella_profile = Arc::new(ella_profile);
                    self.nginx_has_diag = nginx_profile
                        .events
                        .iter()
                        .any(|e| analysis::NginxDiagKind::of(&e.payload).is_some());
                    self.nginx_profile = nginx_profile;
                    self.ella_channel_list = analysis::build_ella_channel_list(&self.ella_profile);
                    self.info_if_bytes = analysis::if_byte_totals(&self.if_profile);
                    self.ella_info = analysis::ella_relay_summary(&self.ella_profile);
                    self.hpts_has_slot_gate = hpts_wheel_events
                        .iter()
                        .any(|e| matches!(e.payload, HptsWheelPayload::SlotGateSample { .. }));
                    self.hpts_wheel_events = hpts_wheel_events;
                    self.mem_series = mem_series;
                    self.interfaces_view = None;
                    self.tcp_samples_view = None;
                    self.ella_samples_view = None;
                    self.nginx_samples_view = None;
                    self.open_ella_channels.clear();
                    #[cfg(not(target_arch = "wasm32"))]
                    self.source_path_rewrites.clear();
                    #[cfg(not(target_arch = "wasm32"))]
                    self.source_binary_paths.clear();
                    self.stars_agg_view = None;
                    self.hpts_hist_view = None;
                    self.hpts_slot_gate_view = None;
                    // Only a new-file load drops the shared System-tab zoom;
                    // the aggregate-filter rebuilds below (and "Hide short
                    // sessions") deliberately leave it intact.
                    self.agg_time_axis.reset();
                    self.system_layout = default_layout(PlotCatalogKind::System);
                    self.system_draft = default_layout(PlotCatalogKind::System);
                    // HTTP responses / duration / completion bytes start
                    // catalog-hidden. nginx data un-hides responses and
                    // duration; a TCP_RPS_SAMPLE rate series un-hides
                    // responses.
                    if !self.nginx_profile.is_empty() {
                        self.system_layout.hidden.remove(&PlotId::HttpRps);
                        self.system_draft.hidden.remove(&PlotId::HttpRps);
                        self.system_layout.hidden.remove(&PlotId::HttpLatency);
                        self.system_draft.hidden.remove(&PlotId::HttpLatency);
                    }
                    if !analysis::build_tcp_rps_series(&self.tcp_samples, file_info.capture_start)
                        .is_empty()
                    {
                        self.system_layout.hidden.remove(&PlotId::HttpRps);
                        self.system_draft.hidden.remove(&PlotId::HttpRps);
                    }
                    self.flame_build = None;
                    self.flame = None; // rebuilt lazily on tab switch
                    self.cpu_filter_ui = CpuFilterUi::default();
                    self.processes_view = None;
                    self.process_functions_view = None;
                    self.process_functions_owner = None;
                    self.process_functions_build = None;
                    self.processes_rollup_build = None;
                    self.pending_flame_filter = None;
                    self.pending_flame_zoom = None;
                    self.file_info = Some(file_info);
                    self.providers = providers;
                    // Computed on the background load thread (alongside
                    // the parse) so this synchronous UI-thread handoff
                    // doesn't have to re-scan the full cpu-sample vector
                    // (a multi-second stall on cpu-heavy captures).
                    self.aggregate = Some(aggregate);
                    self.view = View::Loaded;
                    self.start_pack_fetch();
                    // ELLA-channel-only captures have no System-tab series
                    // (CHANNEL_COUNT and detailed frame timing are the
                    // system-wide plots). Land on the channel list, or the
                    // single channel if there's one.
                    // nginx-only captures land on the request list for the
                    // same reason: the useful view is `+`, not System.
                    if self.summaries.is_empty()
                        && !self.ella_profile.channels.is_empty()
                        && !self.ella_profile.has_channel_counts()
                        && !self.ella_has_frame_timing
                        && self.if_profile.is_empty()
                        && self.tcp_samples.state_samples.is_empty()
                        && !self.tcp_samples.has_stack_series()
                        && self.stars_agg_samples.is_empty()
                        && self.hpts_wheel_events.is_empty()
                    {
                        if self.ella_profile.channels.len() == 1 {
                            let (pid, sid) = *self.ella_profile.channels.keys().next().unwrap();
                            self.select_ella_channel(pid, sid);
                        } else {
                            self.active_tab = Tab::SessionList;
                        }
                    } else if self.sessions.is_empty()
                        && !self.nginx_profile.requests.is_empty()
                        && self.ella_profile.channels.is_empty()
                        && self.if_profile.is_empty()
                        && self.tcp_samples.state_samples.is_empty()
                        && !self.tcp_samples.has_stack_series()
                        && self.stars_agg_samples.is_empty()
                        && self.hpts_wheel_events.is_empty()
                        && self.cpu_profile.is_empty()
                    {
                        self.active_tab = Tab::SessionList;
                    }
                    self.apply_pending_share_params();
                    if let Ok(v) = std::env::var("EVENT_EXPLORER_TAB") {
                        if matches!(v.to_ascii_lowercase().as_str(), "ella" | "fanout")
                            && !self.ella_profile.channels.is_empty()
                        {
                            let rows = &self.ella_channel_list;
                            if let Some(r) = rows.first() {
                                self.select_ella_channel(r.provider_id, r.session_id);
                            }
                        } else if v.to_ascii_lowercase().as_str() == "session-hero" {
                            if let Some(pv) = self.open_sessions.first_mut() {
                                pv.show_limiter = true;
                                pv.show_dsack = true;
                                pv.show_recovery_spans = true;
                                pv.plot_layout.hidden.insert(PlotId::SizeCdf);
                                pv.plot_draft.hidden.insert(PlotId::SizeCdf);
                                pv.open_gear_menu = true;
                            }
                        }
                    }
                    // Queued CSV paths are a desktop file-dialog concern.
                    // A capture that already holds segment rows (either
                    // target) still re-aligns them onto the new window.
                    #[cfg(not(target_arch = "wasm32"))]
                    let loaded_pending = {
                        let pending_csvs = std::mem::take(&mut self.pending_origin_csv);
                        let any = !pending_csvs.is_empty();
                        for csv_path in pending_csvs {
                            self.load_origin_segments_path(csv_path);
                        }
                        any
                    };
                    #[cfg(target_arch = "wasm32")]
                    let loaded_pending = false;
                    if !loaded_pending && !self.raw_origin_segments.is_empty() {
                        self.realign_origin_segments();
                    }

                    // We flipped to `Loaded` mid-frame; without an
                    // explicit repaint egui (being event-driven) won't
                    // render the newly-loaded view until the next input
                    // event, so tabs would sit blank until the user
                    // moves the mouse.
                    ctx.request_repaint();
                }
                Err(e) => {
                    let prefixed = format!("{file_name}: ");
                    let detail = e.strip_prefix(&prefixed).unwrap_or(&e);
                    self.fail_open(format!("Failed to open\n{file_name}\n{detail}"));
                }
            }
            return;
        }

        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.30);
            ui.heading(format!("Loading {file_name}..."));
            ui.add_space(8.0);
            if !status.is_empty() {
                ui.label(
                    RichText::new(&status)
                        .size(14.0)
                        .color(Color32::from_gray(180)),
                );
            } else {
                ui.add(egui::Spinner::new());
            }
            ui.add_space(16.0);
            ui.scope(|ui| {
                ui.set_max_width(420.0);
                let fraction = permille as f32 / 1000.0;
                let bar =
                    egui::ProgressBar::new(fraction).text(format!("{:.0}%", fraction * 100.0));
                ui.add(bar);
            });
        });

        ctx.request_repaint();
    }

    /// Map `[start_us, end_us]` onto 0..=1 fractions of the trace span,
    /// clamped to the track. A zero-width trace span maps to (0, 0).
    fn span_fracs(start_us: u64, end_us: u64, trace_span: (u64, u64)) -> (f32, f32) {
        let (t0, t1) = trace_span;
        let span = t1.saturating_sub(t0);
        if span == 0 {
            return (0.0, 0.0);
        }
        let f = |ts: u64| (ts.saturating_sub(t0) as f64 / span as f64).clamp(0.0, 1.0) as f32;
        (f(start_us), f(end_us))
    }

    /// True when the session's `[start, end]` overlaps the filter window.
    fn overlaps_window(s: &SessionSummary, window: (u64, u64)) -> bool {
        s.start_ts_us <= window.1 && s.end_ts_us >= window.0
    }

    /// Trace-wide overview strip above the session table: every
    /// checkbox-filtered session's lifetime on one shared lane, so
    /// overlaps darken into a concurrency read. Drag selects a
    /// time-window filter for the table; click clears it.
    fn show_session_minimap(
        &mut self,
        ui: &mut egui::Ui,
        trace_span: (u64, u64),
        spans: &[(u64, u64)],
        utc_origin_us: Option<u64>,
    ) {
        let (t0, t1) = trace_span;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 44.0),
            egui::Sense::click_and_drag(),
        );
        if !ui.is_rect_visible(rect) {
            return;
        }
        let x_of = |ts: u64| -> f32 {
            rect.left() + Self::span_fracs(ts, ts, trace_span).0 * rect.width()
        };
        let ts_of = |x: f32| -> u64 {
            let frac = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
            t0.saturating_add((frac as f64 * t1.saturating_sub(t0) as f64) as u64)
        };

        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, COLOR_SESSION_TRACK);
        let lane = rect.shrink2(egui::vec2(0.0, 4.0));
        for &(s, e) in spans {
            let x0 = x_of(s);
            let x1 = x_of(e).max(x0 + 1.0);
            painter.rect_filled(
                egui::Rect::from_x_y_ranges(x0..=x1, lane.y_range()),
                0.0,
                COLOR_MINIMAP_SESSION,
            );
        }

        // Dim everything outside the active (or in-progress) window.
        let pointer_ts = || {
            ui.ctx()
                .input(|i| i.pointer.latest_pos())
                .map(|p| ts_of(p.x))
        };
        let window = self.session_time_filter.or_else(|| {
            let anchor = self.session_filter_drag?;
            let cur = pointer_ts()?;
            Some((anchor.min(cur), anchor.max(cur)))
        });
        if let Some((w0, w1)) = window {
            let wx0 = x_of(w0);
            let wx1 = x_of(w1);
            painter.rect_filled(
                egui::Rect::from_x_y_ranges(rect.left()..=wx0, rect.y_range()),
                0.0,
                COLOR_MINIMAP_DIM,
            );
            painter.rect_filled(
                egui::Rect::from_x_y_ranges(wx1..=rect.right(), rect.y_range()),
                0.0,
                COLOR_MINIMAP_DIM,
            );
            let stroke = egui::Stroke::new(1.0_f32, COLOR_SESSION_ACTIVE);
            painter.vline(wx0, rect.y_range(), stroke);
            painter.vline(wx1, rect.y_range(), stroke);
        }

        // Secondary-button drags are accepted alongside primary so the
        // path is exercisable by XTest synthetic input (which lands as
        // secondary on this machine's egui/winit).
        let drag_started = response.drag_started_by(egui::PointerButton::Primary)
            || response.drag_started_by(egui::PointerButton::Secondary);
        let drag_stopped = response.drag_stopped_by(egui::PointerButton::Primary)
            || response.drag_stopped_by(egui::PointerButton::Secondary);
        if drag_started {
            self.session_filter_drag = pointer_ts();
        }
        if drag_stopped {
            if let (Some(anchor), Some(cur)) = (self.session_filter_drag.take(), pointer_ts()) {
                let (lo, hi) = (anchor.min(cur), anchor.max(cur));
                // A drag covering <0.5% of the trace reads as a click.
                let min_span = (t1.saturating_sub(t0) / 200).max(1);
                self.session_time_filter = (hi - lo >= min_span).then_some((lo, hi));
            }
        }
        if response.clicked() || response.secondary_clicked() {
            self.session_time_filter = None;
        }

        response.on_hover_ui(|ui| {
            if let Some(ts) = pointer_ts() {
                let rel_s = ts.saturating_sub(t0) as f64 / 1e6;
                let mut text = format!("t+{}", format_time_span(rel_s));
                if let Some(origin) = utc_origin_us {
                    text.push_str(&format!(
                        "  ({})",
                        format_utc_clock_us(origin + ts.saturating_sub(t0), true)
                    ));
                }
                ui.label(text);
            }
        });

        if let Some((w0, w1)) = self.session_time_filter {
            ui.horizontal(|ui| {
                let lo_s = w0.saturating_sub(t0) as f64 / 1e6;
                let hi_s = w1.saturating_sub(t0) as f64 / 1e6;
                ui.label(
                    RichText::new(format!(
                        "Window: t+{} – t+{}",
                        format_time_span(lo_s),
                        format_time_span(hi_s)
                    ))
                    .size(12.0),
                );
                if ui
                    .small_button("✕")
                    .on_hover_text("Clear time window")
                    .clicked()
                {
                    self.session_time_filter = None;
                }
            });
        }
    }

    /// Paint one Timeline cell: a track for the full trace span, the
    /// session's lifetime bar, and a brighter segment for the
    /// byte-weighted active window.
    fn paint_timeline_cell(
        ui: &mut egui::Ui,
        s: &SessionSummary,
        trace_span: (u64, u64),
        utc_origin_us: Option<u64>,
    ) {
        let width = ui.available_width().max(60.0);
        let height = ui.text_style_height(&egui::TextStyle::Body) * 1.4;
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let painter = ui.painter();
        let bar_h = (height * 0.45).min(10.0);
        let cy = rect.center().y;
        painter.rect_filled(
            egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), bar_h)),
            bar_h / 2.0,
            COLOR_SESSION_TRACK,
        );

        let (f0, f1) = Self::span_fracs(s.start_ts_us, s.end_ts_us, trace_span);
        let x0 = rect.left() + f0 * rect.width();
        let x1 = (rect.left() + f1 * rect.width()).max(x0 + 1.5);
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x0, cy - bar_h / 2.0),
                egui::pos2(x1, cy + bar_h / 2.0),
            ),
            bar_h / 2.0,
            COLOR_SESSION_LIFETIME,
        );

        let active_end = s
            .start_ts_us
            .saturating_add((s.duration_sec * 1e6) as u64)
            .min(s.end_ts_us);
        let ax1 = (rect.left()
            + Self::span_fracs(s.start_ts_us, active_end, trace_span).1 * rect.width())
        .clamp(x0, x1);
        if ax1 > x0 + 0.5 {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x0, cy - bar_h / 2.0),
                    egui::pos2(ax1.max(x0 + 1.0), cy + bar_h / 2.0),
                ),
                bar_h / 2.0,
                COLOR_SESSION_ACTIVE,
            );
        }

        response.on_hover_ui(|ui| {
            let t0 = trace_span.0;
            let start_s = s.start_ts_us.saturating_sub(t0) as f64 / 1e6;
            let end_s = s.end_ts_us.saturating_sub(t0) as f64 / 1e6;
            let life_s = s.end_ts_us.saturating_sub(s.start_ts_us) as f64 / 1e6;
            ui.label(format!(
                "t+{} – t+{}",
                format_time_span(start_s),
                format_time_span(end_s)
            ));
            if let Some(origin) = utc_origin_us {
                ui.label(format!(
                    "{} – {} UTC",
                    format_utc_clock_us(origin + s.start_ts_us.saturating_sub(t0), true),
                    format_utc_clock_us(origin + s.end_ts_us.saturating_sub(t0), true)
                ));
            }
            let pct = if life_s > 0.0 {
                s.duration_sec / life_s * 100.0
            } else {
                100.0
            };
            ui.label(format!(
                "Lifetime {}, active {} ({pct:.0}%)",
                format_time_span(life_s),
                format_time_span(s.duration_sec)
            ));
        });
    }

    pub(crate) fn show_session_list(&mut self, ui: &mut egui::Ui) {
        sort_summaries(&mut self.summaries, self.sort_column, self.sort_ascending);

        if self.summaries.is_empty() {
            self.show_ella_channel_list(ui);
            return;
        }

        // Trace span the Timeline column and minimap map onto: the
        // capture header's first/last event, falling back to the
        // session summaries when the header is missing.
        let trace_span = self
            .file_info
            .as_ref()
            .map(|fi| (fi.capture_start, fi.capture_end.max(fi.capture_start + 1)))
            .unwrap_or_else(|| {
                let t0 = self
                    .summaries
                    .iter()
                    .map(|s| s.start_ts_us)
                    .min()
                    .unwrap_or(0);
                let t1 = self
                    .summaries
                    .iter()
                    .map(|s| s.end_ts_us)
                    .max()
                    .unwrap_or(t0 + 1);
                (t0, t1.max(t0 + 1))
            });
        let utc_origin_us =
            utc_axis_origin(self.utc_time_labels, self.file_info.as_ref(), trace_span.0);

        let checkbox_filtered: Vec<usize> = self
            .summaries
            .iter()
            .enumerate()
            .filter(|(_, s)| !self.filter_low_events || is_significant_session(s))
            .map(|(i, _)| i)
            .collect();
        let minimap_spans: Vec<(u64, u64)> = checkbox_filtered
            .iter()
            .map(|&i| {
                let s = &self.summaries[i];
                (s.start_ts_us, s.end_ts_us)
            })
            .collect();
        let time_filter = self.session_time_filter;
        let filtered: Vec<usize> = checkbox_filtered
            .into_iter()
            .filter(|&i| time_filter.is_none_or(|w| Self::overlaps_window(&self.summaries[i], w)))
            .collect();
        let shown = filtered.len();

        ui.horizontal(|ui| {
            ui.heading(format!("Sessions ({shown})"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(
                    &mut self.filter_low_events,
                    RichText::new("Hide short sessions (<250 events or <1s)").size(12.0),
                );
            });
        });
        ui.separator();

        self.show_session_minimap(ui, trace_span, &minimap_spans, utc_origin_us);

        let mut selected_session: Option<u64> = None;

        let header_label =
            |col: SortColumn, text: &str, current: SortColumn, asc: bool| -> String {
                if col == current {
                    let arrow = if asc { " ^" } else { " v" };
                    format!("{text}{arrow}")
                } else {
                    text.to_string()
                }
            };

        let has_ella = !self.ella_profile.channels.is_empty();
        let text_height = ui.text_style_height(&egui::TextStyle::Body);
        let available_height = ui.available_height();
        // Cap the TCP table so the Fanout list below keeps a usable pane.
        let tcp_max_h = if has_ella {
            if shown == 0 {
                text_height * 2.5
            } else {
                let floor = (available_height * 0.45).clamp(200.0, available_height * 0.7);
                (available_height - floor).max(text_height * 5.0)
            }
        } else {
            available_height
        };
        let open_ids: Vec<u64> = self
            .open_sessions
            .iter()
            .map(|pv| pv.summary.session_id)
            .collect();

        // Minimum total width we want to guarantee for the table so all
        // 13 columns stay readable even on narrow windows. If the
        // available width is less than this, the outer horizontal
        // ScrollArea kicks in. Sum of per-column `at_least` values
        // (120+60+140+180+80+90+100+110+80+80+70+70+70 = 1250) plus a
        // small per-cell padding budget so columns don't render
        // visually cramped right up against their minimums.
        const MIN_TABLE_WIDTH: f32 = 1350.0;

        egui::ScrollArea::horizontal()
            .id_source("session_list_hscroll")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                // Force the inner region to at least MIN_TABLE_WIDTH so
                // the TableBuilder's column minimums are honored
                // regardless of the outer window width — otherwise
                // egui_extras squeezes all `auto()` columns to fit and
                // the scroll bar never appears.
                ui.set_min_width(MIN_TABLE_WIDTH);

                TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::auto().at_least(120.0))
                    .column(Column::auto().at_least(60.0))
                    .column(Column::auto().at_least(140.0))
                    .column(Column::remainder().at_least(180.0))
                    .column(Column::auto().at_least(80.0))
                    .column(Column::auto().at_least(90.0))
                    .column(Column::auto().at_least(100.0))
                    .column(Column::auto().at_least(110.0))
                    .column(Column::auto().at_least(80.0))
                    .column(Column::auto().at_least(80.0))
                    .column(Column::auto().at_least(70.0))
                    .column(Column::auto().at_least(70.0))
                    .column(Column::auto().at_least(70.0))
                    .min_scrolled_height(0.0)
                    .max_scroll_height(tcp_max_h)
                    .sense(egui::Sense::click())
                    .header(text_height * 1.5, |mut header| {
                        let cols = [
                            (SortColumn::StartTime, "Timeline"),
                            (SortColumn::SessionId, "Session ID"),
                            (SortColumn::Remote, "Remote"),
                            (SortColumn::LogId, "Log ID"),
                            (SortColumn::Duration, "Duration"),
                            (SortColumn::BytesSent, "Bytes Sent"),
                            (SortColumn::Goodput, "Goodput"),
                            (SortColumn::PacingRate, "Pacing Rate"),
                            (SortColumn::Rtt, "RTT"),
                            (SortColumn::MinRtt, "Min RTT"),
                            (SortColumn::EventCount, "Events"),
                            (SortColumn::RecoveryCount, "Recovery"),
                            (SortColumn::NginxRequests, "HTTP Reqs"),
                        ];
                        for (col, text) in cols {
                            header.col(|ui| {
                                let label =
                                    header_label(col, text, self.sort_column, self.sort_ascending);
                                let mut resp = ui.button(label);
                                if col == SortColumn::NginxRequests {
                                    resp = resp.on_hover_text(
                                        "HTTP requests nginx logged on this connection.",
                                    );
                                }
                                if resp.clicked() {
                                    if self.sort_column == col {
                                        self.sort_ascending = !self.sort_ascending;
                                    } else {
                                        self.sort_column = col;
                                        // Chronological reads top-down;
                                        // every other column sorts
                                        // largest-first on first click.
                                        self.sort_ascending = col == SortColumn::StartTime;
                                    }
                                }
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(text_height * 1.4, shown, |mut row| {
                            let idx = filtered[row.index()];
                            let s = &self.summaries[idx];
                            let sid = s.session_id;

                            row.set_selected(open_ids.contains(&sid));

                            row.col(|ui| {
                                Self::paint_timeline_cell(ui, s, trace_span, utc_origin_us);
                            });
                            row.col(|ui| {
                                ui.label(format!("{}", s.session_id));
                            });
                            row.col(|ui| {
                                ui.label(if s.remote.is_empty() {
                                    "(unknown)"
                                } else {
                                    &s.remote
                                });
                            });
                            row.col(|ui| {
                                ui.label(&s.log_id);
                            });
                            row.col(|ui| {
                                ui.label(format!("{:.1}s", s.duration_sec));
                            });
                            row.col(|ui| {
                                ui.label(format_bytes(s.total_bytes_sent));
                            });
                            row.col(|ui| {
                                ui.label(format_goodput(s.avg_kernel_goodput_mbps));
                            });
                            row.col(|ui| {
                                ui.label(format_goodput(s.avg_pacing_rate_mbps));
                            });
                            row.col(|ui| {
                                ui.label(format!("{:.1} ms", s.avg_srtt_ms));
                            });
                            row.col(|ui| {
                                ui.label(format!("{:.1} ms", s.min_rtt_ms));
                            });
                            row.col(|ui| {
                                ui.label(format!("{}", s.event_count));
                            });
                            row.col(|ui| {
                                ui.label(format!("{}", s.recovery_count));
                            });
                            row.col(|ui| {
                                ui.label(if s.nginx_requests > 0 {
                                    format!("{}", s.nginx_requests)
                                } else {
                                    String::new()
                                });
                            });

                            if row.response().clicked() {
                                selected_session = Some(sid);
                            }
                        });
                    });
            });

        if let Some(sid) = selected_session {
            self.select_session(sid);
        }

        self.show_ella_channel_list(ui);
    }

    /// Whether the tab bar should show `+` (the session / Fanout /
    /// nginx picker).
    ///
    /// Presence-based: any TCP session, Fanout channel, or nginx
    /// request list puts `+` on the bar.
    pub(crate) fn show_session_list_tab(&self) -> bool {
        !self.summaries.is_empty()
            || !self.ella_profile.channels.is_empty()
            || !self.nginx_profile.requests.is_empty()
    }

    /// High-level System tab: default-visible plots plus any catalog
    /// entry that has data (hidden plots still make the tab worth showing).
    pub(crate) fn system_tab_available(&self) -> bool {
        let high_level = self.summaries.len() > 1
            || !self.if_profile.is_empty()
            || !self.tcp_samples.state_samples.is_empty()
            || !self.tcp_samples.rps_samples.is_empty()
            || !self.nginx_profile.requests.is_empty()
            || !self.tcp_samples.gp_samples.is_empty()
            || !self.tcp_samples.send_samples.is_empty()
            || !self.tcp_samples.lro_flush_samples.is_empty()
            || !self.tcp_samples.srtt_samples.is_empty()
            || !self.stars_agg_samples.is_empty()
            || !self.hpts_wheel_events.is_empty()
            || self.ella_profile.has_channel_counts()
            || self.ella_has_frame_timing
            || !self.nginx_profile.is_empty();
        let has_cpu = !self.cpu_profile.is_empty();
        high_level || (has_cpu && !self.summaries.is_empty())
    }

    fn finish_shared_time_axis(
        &mut self,
        ui: &mut egui::Ui,
        cursor: &mut SysCursor,
        plot_ids: &[&str],
    ) {
        let agg_time_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        if let TimeAxisOutcome::Reset { .. } =
            self.agg_time_axis
                .handle(ui, cursor.geoms(), 0.0, agg_time_max)
        {
            let mut ids: Vec<&str> = plot_ids.to_vec();
            let extra: Vec<String> = self
                .ella_samples_view
                .as_ref()
                .filter(|s| s.multi)
                .map(|s| {
                    s.nodes
                        .iter()
                        .flat_map(|n| {
                            ELLA_NODE_PLOT_IDS
                                .iter()
                                .map(|id| format!("{id}_{}", n.provider_id))
                                .collect::<Vec<_>>()
                        })
                        .collect()
                })
                .unwrap_or_default();
            ids.extend(extra.iter().map(String::as_str));
            self.agg_time_axis.reset_plots(ui.ctx(), &ids);
        }
        cursor.draw(ui);
    }

    pub(crate) fn show_aggregate_view(&mut self, _ctx: &egui::Context, ui: &mut egui::Ui) {
        let (has_states, has_count, has_srtt, has_gp) = {
            let Some(agg) = self.aggregate.as_ref() else {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.label("No system data available.");
                });
                return;
            };
            (
                !agg.tcp_states.estab.is_empty(),
                !agg.session_count_paced.is_empty() || !agg.session_count_unpaced.is_empty(),
                !agg.srtt_paced_cdf.is_empty() || !agg.srtt_unpaced_cdf.is_empty(),
                !agg.goodput_paced_cdf.is_empty() || !agg.goodput_unpaced_cdf.is_empty(),
            )
        };
        // The consolidated tab stacks a data-dependent number of panels
        // (aggregate + interfaces + per-stack TCP), so every panel gets a
        // uniform fixed height and the outer scroll area handles overflow.
        let spacing = 6.0;

        // Set inside the scroll closure below (right above the plots it
        // actually filters); applied after the closure returns so the
        // rebuild-on-change doesn't fight the closure's borrow of `agg`.
        let mut filter_low_events_changed = false;

        let agg_trace_t0_us = self
            .file_info
            .as_ref()
            .map(|fi| fi.capture_start)
            .unwrap_or(0);
        let agg_utc_origin = utc_axis_origin(
            self.utc_time_labels,
            self.file_info.as_ref(),
            agg_trace_t0_us,
        );

        let mut cursor = SysCursor::new(ui, agg_utc_origin, self.plot_bins);
        cursor.show_legends = self.show_legends;

        let nines_x_formatter =
            |mark: GridMark, _digits: usize, _range: &std::ops::RangeInclusive<f64>| {
                let pct = nines_to_pct(mark.value);
                for &(tick_pct, label) in NINES_TICKS {
                    if (pct - tick_pct).abs() < 0.01 {
                        return label.to_string();
                    }
                }
                String::new()
            };

        let sys_order = self.system_layout.order.clone();
        let sys_hidden = self.system_layout.hidden.clone();
        let session_derived_visible = [PlotId::SessionCount, PlotId::SrttAvg, PlotId::GoodputAvg]
            .iter()
            .any(|id| !sys_hidden.contains(id));

        #[allow(unused_assignments)]
        let mut count_resp = None;
        let mut srtt_resp = None;
        let mut gp_resp = None;

        let agg_seed = self.agg_time_axis.seed(
            0.0,
            self.aggregate
                .as_ref()
                .map(|a| a.time_max.max(1.0))
                .unwrap_or(1.0),
        );

        if session_derived_visible && (has_count || has_srtt || has_gp) {
            let prev_filter = self.filter_low_events;
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.checkbox(
                        &mut self.filter_low_events,
                        RichText::new("Hide short sessions (<250 events or <1s)").size(12.0),
                    );
                });
            });
            filter_low_events_changed = self.filter_low_events != prev_filter;
            ui.add_space(spacing);
        }

        let shown: Vec<PlotId> = sys_order
            .iter()
            .copied()
            .filter(|id| !sys_hidden.contains(id))
            .filter(|id| self.system_layout.maximized.is_none_or(|m| m == *id))
            .filter(|&id| self.plot_has_capture_data(id))
            .collect();
        let (pinned, rest) = self.system_layout.split_pinned(&shown);

        macro_rules! draw_system_ids {
            ($plot_ui:ident, $ids:expr) => {{
                let ui = &mut *$plot_ui;
                for &plot_id in $ids {
                    cursor.last_plot_rect = None;
                    cursor.fill_remaining = self.system_layout.fills_catalog_row(plot_id);
                    cursor.panel_h = self.system_layout.draw_height(plot_id, ui.available_height());
                    if self.pending_scroll_plot == Some(plot_id) {
                        ui.scroll_to_cursor(Some(egui::Align::Center));
                        self.pending_scroll_plot = None;
                    }
                    match plot_id {
                PlotId::CpuUsage => {
        let strip_view_us = self.agg_time_axis.window_us(agg_trace_t0_us).or_else(|| {
            let t_max = self
                .aggregate
                .as_ref()
                .map(|a| a.time_max.max(1.0))
                .unwrap_or(1.0);
            let hi_us = agg_trace_t0_us
                .saturating_add((t_max * 1_000_000.0).round() as u64)
                .max(agg_trace_t0_us.saturating_add(1));
            Some((agg_trace_t0_us, hi_us))
        });
        let has_cpu_panel = self
            .aggregate
            .as_ref()
            .map(|a| a.cpu_panel.is_some())
            .unwrap_or(false);
        let mut cpu_zoom_system = self.cpu_zoom_system;
        if has_cpu_panel {
            if !cursor.fill_remaining {
                ui.add_space(2.0);
            }
            let mut scale_cpu_by_freq = self.scale_cpu_by_freq;
            if let Some(panel) = self.aggregate.as_mut().and_then(|a| a.cpu_panel.as_mut()) {
                panel.show_header(
                    ui,
                    "aggregate_cpu_panel",
                    &mut scale_cpu_by_freq,
                    &mut cpu_zoom_system,
                );
            }
            self.scale_cpu_by_freq = scale_cpu_by_freq;
        }
        self.cpu_zoom_system = cpu_zoom_system;
        let cpu_selection = if cpu_zoom_system {
            None
        } else {
            self.agg_time_axis.window_us(agg_trace_t0_us)
        };
        let cpus = self.current_function_filter().cpus;
        if let Some(panel) = self.aggregate.as_mut().and_then(|a| a.cpu_panel.as_mut()) {
            panel.scale_by_freq = self.scale_cpu_by_freq;
            panel.fill_remaining = cursor.fill_remaining;
            panel.set_plot_height(cursor.plot_h(ui));
            panel.show_legends = self.show_legends;
            panel.set_plot_ids("cpu_usage_strip_system");
            panel.sync_bins(self.plot_bins, &cpus);
            let change = panel.draw(
                ui,
                agg_trace_t0_us,
                cpu_selection,
                if cpu_zoom_system {
                    strip_view_us
                } else {
                    None
                },
                agg_utc_origin,
                !cpu_zoom_system,
            );
            if !cpu_zoom_system {
                match change {
                    Some(SelectionChange::Set(lo, hi)) => {
                        self.agg_time_axis
                            .set_window_us(agg_trace_t0_us, lo, hi);
                    }
                    Some(SelectionChange::Clear) => self.agg_time_axis.clear_zoom(),
                    None => {}
                }
            }
        }

        // Shared-cursor contribution for the CPU usage panel.
        let strip_rt = self
            .aggregate
            .as_ref()
            .and_then(|a| a.cpu_panel.as_ref())
            .and_then(|p| p.last_rect_transform());
        if let Some((rect, transform)) = strip_rt {
            if let Some(data_x) = cursor.panel(rect, &transform, self.cpu_zoom_system) {
                // One tooltip row per visible layer (Aggregate/Per-CPU:
                // just the one aggregate value; Per-Process/Kernel vs
                // User: one row per stacked layer), dots on each layer.
                if let Some(p) = self.aggregate.as_ref().and_then(|a| a.cpu_panel.as_ref()) {
                    cursor.add_cpu_series(&transform, data_x, &p.cursor_series(data_x));
                }
            }
            cursor.last_plot_rect = Some(rect);
        }
                }
                PlotId::SessionCount => {
        let Some(agg) = self.aggregate.as_ref() else { continue };
        // --- TCP Session Count ---
        count_resp = if has_count {
            ui.horizontal(|ui| {
                ui.strong(RichText::new("TCP Session Count").size(14.0));
                ui.add_space(16.0);
                ui.label(
                    RichText::new(format!(
                        "Paced Peak: {:.0}  |  Not Paced Peak: {:.0}  |  Duration: {:.1}s  |  {} total ({} paced, {} not paced)",
                        agg.peak_paced, agg.peak_unpaced, agg.time_max,
                        agg.total_sessions, agg.paced_sessions, agg.unpaced_sessions,
                    ))
                    .size(12.0)
                    .color(Color32::from_gray(180)),
                );
            });

            let y_axis_count = agg_y_axis("Sessions");
            let count_top = agg.peak_paced.max(agg.peak_unpaced);
            self.agg_time_axis.apply(ui.ctx(), "agg_session_count_plot");
            let mut count_plot = Plot::new("agg_session_count_plot")
                .height(cursor.plot_h(ui))
                .set_margin_fraction(egui::vec2(0.0, 0.0))
                .include_x(agg_seed.0)
                .include_x(agg_seed.1)
                .auto_bounds(Vec2b::new(agg_seed.2, true))
                .include_y(0.0)
                .include_y(if count_top > 0.0 { count_top * 1.08 } else { 1.0 })
                .show_x(false)
                .show_y(false)
                .allow_scroll(false)
                .allow_drag(false)
                .allow_zoom(false)
                .allow_boxed_zoom(false)
                .allow_double_click_reset(false)
                .custom_x_axes(vec![agg_x_time_axis(agg_utc_origin)])
                .custom_y_axes(vec![y_axis_count])
                .y_grid_spacer(nice_y_grid_spacer);
            count_plot = maybe_legend(count_plot, self.show_legends);
            if agg_utc_origin.is_some() {
                count_plot = count_plot.x_grid_spacer(utc_x_grid_spacer);
            }
            let resp = count_plot.show(ui, |plot_ui| {
                    if !agg.session_count_paced.is_empty() {
                        let stepped =
                            analysis::to_step_post(&agg.session_count_paced);
                        plot_ui.line(
                            Line::new(PlotPoints::new(stepped))
                                .color(COLOR_CWND)
                                .width(1.5_f32)
                                .name("Paced"),
                        );
                    }
                    if !agg.session_count_unpaced.is_empty() {
                        let stepped =
                            analysis::to_step_post(&agg.session_count_unpaced);
                        plot_ui.line(
                            Line::new(PlotPoints::new(stepped))
                                .color(COLOR_COUNT_UNPACED)
                                .width(1.5_f32)
                                .name("Not Paced"),
                        );
                    }
                });

            ui.add_space(spacing);
            Some(resp)
        } else {
            None
        };

        // Shared-cursor contribution for the session-count panel.
        if let Some(resp) = count_resp.as_ref() {
            if let Some(data_x) = cursor.panel(resp.response.rect, &resp.transform, true) {
                for (series, name, color) in [
                    (&agg.session_count_paced, "Paced", COLOR_CWND),
                    (&agg.session_count_unpaced, "Not Paced", COLOR_COUNT_UNPACED),
                ] {
                    if let Some(y) = lookup_step_value(series, data_x) {
                        cursor.add(
                            &resp.transform,
                            data_x,
                            y,
                            format!("{name}: {y:.0}"),
                            color,
                        );
                    }
                }
            }
        }
                }
                PlotId::SrttAvg => {
        let Some(agg) = self.aggregate.as_ref() else { continue };
        srtt_resp = if has_srtt {
            ui.horizontal(|ui| {
                ui.strong(RichText::new("TCP Average RTT").size(14.0));
                ui.add_space(16.0);
                match self.srtt_avg_view {
                    AvgView::Distribution => {
                        ui.label(
                            RichText::new(format!(
                                "Paced P50: {:.1}  P95: {:.1}  P99: {:.1} ms  |  Not Paced P50: {:.1}  P95: {:.1}  P99: {:.1} ms",
                                agg.srtt_paced_stats.p50, agg.srtt_paced_stats.p95, agg.srtt_paced_stats.p99,
                                agg.srtt_unpaced_stats.p50, agg.srtt_unpaced_stats.p95, agg.srtt_unpaced_stats.p99,
                            ))
                            .size(12.0)
                            .color(Color32::from_gray(180)),
                        );
                    }
                    AvgView::OverTime => {
                        ui.label(
                            RichText::new(format!(
                                "Paced mean: {:.1} ms  |  Not Paced mean: {:.1} ms",
                                agg.srtt_paced_stats.mean, agg.srtt_unpaced_stats.mean,
                            ))
                            .size(12.0)
                            .color(Color32::from_gray(180)),
                        );
                    }
                }
                ui.separator();
                ui.selectable_value(
                    &mut self.srtt_avg_view,
                    AvgView::Distribution,
                    RichText::new("Distribution").size(14.0),
                );
                ui.selectable_value(
                    &mut self.srtt_avg_view,
                    AvgView::OverTime,
                    RichText::new("Over Time").size(14.0),
                );
            });

            match self.srtt_avg_view {
                AvgView::OverTime => {
                    Self::avg_over_time_panel(
                        &self.agg_time_axis,
                        ui,
                        &mut cursor,
                        "agg_srtt_avg_plot",
                        "RTT (ms)",
                        |v| format!("{v:.2} ms"),
                        [
                            (&agg.srtt_paced_avg, "Paced", COLOR_SRTT),
                            (&agg.srtt_unpaced_avg, "Not Paced", COLOR_SRTT_UNPACED),
                        ],
                        agg_seed,
                        agg.time_max,
                    );
                    ui.add_space(spacing);
                    None
                }
                AvgView::Distribution => {
            let y_axis_rtt = agg_y_axis("RTT (ms)");
            let x_axis_rtt = AxisHints::new_x()
                .label(RichText::new("Percentile").size(14.0))
                .formatter(nines_x_formatter);
            let srtt_top = agg
                .srtt_paced_cdf
                .iter()
                .chain(agg.srtt_unpaced_cdf.iter())
                .fold(0.0_f64, |m, p| m.max(p[1]));
            let resp = Plot::new("agg_srtt_plot")
                .height(cursor.plot_h(ui))
                .set_margin_fraction(egui::vec2(0.0, 0.0))
                .include_x(0.0)
                .include_x(pct_to_nines(99.0))
                .include_y(0.0)
                .include_y(if srtt_top > 0.0 { srtt_top * 1.08 } else { 1.0 })
                .show_x(false)
                .show_y(false)
                .allow_scroll(false)
                .allow_drag(false)
                .allow_zoom(false)
                .allow_boxed_zoom(false)
                .allow_double_click_reset(false)
                .custom_x_axes(vec![x_axis_rtt])
                .x_grid_spacer(nines_grid_spacer)
                .custom_y_axes(vec![y_axis_rtt])
                .y_grid_spacer(nice_y_grid_spacer)
                .legend(Legend::default().position(Corner::RightBottom))
                .show(ui, |plot_ui| {
                    if !agg.srtt_paced_cdf.is_empty() {
                        plot_ui.line(
                            Line::new(PlotPoints::new(agg.srtt_paced_cdf.clone()))
                                .color(COLOR_SRTT)
                                .width(2.0_f32)
                                .name("Paced"),
                        );
                    }
                    if !agg.srtt_unpaced_cdf.is_empty() {
                        plot_ui.line(
                            Line::new(PlotPoints::new(agg.srtt_unpaced_cdf.clone()))
                                .color(COLOR_SRTT_UNPACED)
                                .width(2.0_f32)
                                .name("Not Paced"),
                        );
                    }
                });

            ui.add_space(spacing);
            Some(resp)
                }
            }
        } else {
            None
        };
                }
                PlotId::GoodputAvg => {
        let Some(agg) = self.aggregate.as_ref() else { continue };
        gp_resp = if has_gp {
            ui.horizontal(|ui| {
                ui.strong(RichText::new("TCP Average Goodput").size(14.0));
                ui.add_space(16.0);
                match self.goodput_avg_view {
                    AvgView::Distribution => {
                        ui.label(
                            RichText::new(format!(
                                "Paced P50: {}  P95: {}  P99: {}  |  Not Paced P50: {}  P95: {}  P99: {}",
                                format_goodput(agg.goodput_paced_stats.p50),
                                format_goodput(agg.goodput_paced_stats.p95),
                                format_goodput(agg.goodput_paced_stats.p99),
                                format_goodput(agg.goodput_unpaced_stats.p50),
                                format_goodput(agg.goodput_unpaced_stats.p95),
                                format_goodput(agg.goodput_unpaced_stats.p99),
                            ))
                            .size(12.0)
                            .color(Color32::from_gray(180)),
                        );
                    }
                    AvgView::OverTime => {
                        ui.label(
                            RichText::new(format!(
                                "Paced mean: {}  |  Not Paced mean: {}",
                                format_goodput(agg.goodput_paced_stats.mean),
                                format_goodput(agg.goodput_unpaced_stats.mean),
                            ))
                            .size(12.0)
                            .color(Color32::from_gray(180)),
                        );
                    }
                }
                ui.separator();
                ui.selectable_value(
                    &mut self.goodput_avg_view,
                    AvgView::Distribution,
                    RichText::new("Distribution").size(14.0),
                );
                ui.selectable_value(
                    &mut self.goodput_avg_view,
                    AvgView::OverTime,
                    RichText::new("Over Time").size(14.0),
                );
            });

            match self.goodput_avg_view {
                AvgView::OverTime => {
                    Self::avg_over_time_panel(
                        &self.agg_time_axis,
                        ui,
                        &mut cursor,
                        "agg_goodput_avg_plot",
                        "Goodput (Mbps)",
                        format_goodput,
                        [
                            (&agg.goodput_paced_avg, "Paced", COLOR_KERNEL_GOODPUT),
                            (&agg.goodput_unpaced_avg, "Not Paced", COLOR_GOODPUT_UNPACED),
                        ],
                        agg_seed,
                        agg.time_max,
                    );
                    ui.add_space(spacing);
                    None
                }
                AvgView::Distribution => {
            let y_axis_gp = agg_y_axis("Goodput (Mbps)");
            let x_axis_gp = AxisHints::new_x()
                .label(RichText::new("Percentile").size(14.0))
                .formatter(nines_x_formatter);
            let gp_top = agg
                .goodput_paced_cdf
                .iter()
                .chain(agg.goodput_unpaced_cdf.iter())
                .fold(0.0_f64, |m, p| m.max(p[1]));
            let resp = Plot::new("agg_goodput_plot")
                .height(cursor.plot_h(ui))
                .set_margin_fraction(egui::vec2(0.0, 0.0))
                .include_x(0.0)
                .include_x(pct_to_nines(99.0))
                .include_y(0.0)
                .include_y(if gp_top > 0.0 { gp_top * 1.08 } else { 1.0 })
                .show_x(false)
                .show_y(false)
                .allow_scroll(false)
                .allow_drag(false)
                .allow_zoom(false)
                .allow_boxed_zoom(false)
                .allow_double_click_reset(false)
                .custom_x_axes(vec![x_axis_gp])
                .x_grid_spacer(nines_grid_spacer)
                .custom_y_axes(vec![y_axis_gp])
                .y_grid_spacer(nice_y_grid_spacer)
                .legend(Legend::default().position(Corner::RightBottom))
                .show(ui, |plot_ui| {
                    if !agg.goodput_paced_cdf.is_empty() {
                        plot_ui.line(
                            Line::new(PlotPoints::new(agg.goodput_paced_cdf.clone()))
                                .color(COLOR_KERNEL_GOODPUT)
                                .width(2.0_f32)
                                .name("Paced"),
                        );
                    }
                    if !agg.goodput_unpaced_cdf.is_empty() {
                        plot_ui.line(
                            Line::new(PlotPoints::new(agg.goodput_unpaced_cdf.clone()))
                                .color(COLOR_GOODPUT_UNPACED)
                                .width(2.0_f32)
                                .name("Not Paced"),
                        );
                    }
                });
            Some(resp)
                }
            }
        } else {
            None
        };
                }
                PlotId::NetworkTx => {
                    self.interfaces_section(ui, &mut cursor, IfCatalogPanel::Tx)
                }
                PlotId::Memory => self.memory_section(ui, &mut cursor),
                PlotId::MemoryPressure => self.memory_pressure_section(ui, &mut cursor),
                PlotId::Swap => self.swap_section(ui, &mut cursor),
                PlotId::NetworkRx => {
                    self.interfaces_section(ui, &mut cursor, IfCatalogPanel::Rx)
                }
                PlotId::HttpRps => self.http_rps_section(ui, &mut cursor),
                PlotId::HttpLatency => self.http_latency_section(ui, &mut cursor),
                PlotId::TcpStates => self.draw_tcp_states_section(ui, &mut cursor),
                PlotId::ConnRate => self.conn_rate_section(ui, &mut cursor),
                PlotId::TcpGoodput => self.tcp_goodput_section(ui, &mut cursor),
                PlotId::TcpSrtt => self.tcp_srtt_section(ui, &mut cursor),
                PlotId::TcpSend => self.tcp_send_section(ui, &mut cursor),
                PlotId::TcpLro => self.tcp_lro_flush_section(ui, &mut cursor),
                id if id.fanout_split().is_some() => {
                    self.ella_section(ui, &mut cursor, id)
                }
                PlotId::NginxBytes => self.nginx_section(ui, &mut cursor),
                PlotId::NginxDiag => self.nginx_diag_section(ui, &mut cursor),
                PlotId::OriginSegments => self.origin_segments_section(ui, &mut cursor),
                PlotId::Stars => self.stars_agg_section(ui, &mut cursor),
                PlotId::HptsLateness | PlotId::HptsBatch => {
                    self.hpts_hist_section(ui, &mut cursor, plot_id)
                }
                PlotId::HptsSlotGateOutput
                | PlotId::HptsSlotGateFires
                | PlotId::HptsSlotGateChecks
                | PlotId::HptsSlotGateRollover => {
                    self.hpts_slot_gate_section(ui, &mut cursor, plot_id)
                }
                _ => {}
                    }
                    if let Some(r) = count_resp.as_ref().map(|r| r.response.rect) {
                        if plot_id == PlotId::SessionCount {
                            cursor.last_plot_rect = Some(r);
                        }
                    }
                    if !plot_id.self_chromes_plots() {
                        finish_catalog_plot(
                            ui,
                            &mut self.system_layout,
                            &mut self.system_draft,
                            plot_id,
                            cursor.last_plot_rect,
                        );
                    }
                }
            }};
        }

        draw_system_ids!(ui, &pinned);
        egui::ScrollArea::vertical()
            .id_source("aggregate_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                draw_system_ids!(ui, &rest);

                if let Some(agg) = self.aggregate.as_ref() {
                    // --- Cursor tracking with multi-line tooltip ---
                    let pointer_pos = ui.input(|i| i.pointer.latest_pos());
                    if let Some(pos) = pointer_pos {
                        struct TooltipState {
                            rect: egui::Rect,
                            dots: Vec<(egui::Pos2, Color32)>,
                            entries: Vec<(String, Color32)>,
                        }

                        // NIC-rate and session-count panels (both x = time) feed the
                        // shared System-tab cursor built below; only the percentile-x
                        // distribution panels (SRTT / goodput CDFs) use this local
                        // per-plot tooltip, since their x-axis isn't the capture clock.
                        let mut tip: Option<TooltipState> = None;

                        if let Some(srtt_resp) = srtt_resp.as_ref() {
                            if tip.is_none() && srtt_resp.response.rect.contains(pos) {
                                let data_x = srtt_resp.transform.value_from_position(pos).x;
                                let pct = nines_to_pct(data_x);
                                let mut dots = Vec::new();
                                let mut entries = Vec::new();
                                for (cdf, name, color) in [
                                    (&agg.srtt_paced_cdf, "Paced", COLOR_SRTT),
                                    (&agg.srtt_unpaced_cdf, "Not Paced", COLOR_SRTT_UNPACED),
                                ] {
                                    if let Some(y) = interpolate_line(cdf, data_x) {
                                        let dot = srtt_resp
                                            .transform
                                            .position_from_point(&PlotPoint { x: data_x, y });
                                        dots.push((dot, color));
                                        entries.push((
                                            format!("{} {}: {:.2} ms", format_pct(pct), name, y),
                                            color,
                                        ));
                                    }
                                }
                                if !entries.is_empty() {
                                    tip = Some(TooltipState {
                                        rect: srtt_resp.response.rect,
                                        dots,
                                        entries,
                                    });
                                }
                            }
                        }

                        if let Some(gp_resp) = gp_resp.as_ref() {
                            if tip.is_none() && gp_resp.response.rect.contains(pos) {
                                let data_x = gp_resp.transform.value_from_position(pos).x;
                                let pct = nines_to_pct(data_x);
                                let mut dots = Vec::new();
                                let mut entries = Vec::new();
                                for (cdf, name, color) in [
                                    (&agg.goodput_paced_cdf, "Paced", COLOR_KERNEL_GOODPUT),
                                    (&agg.goodput_unpaced_cdf, "Not Paced", COLOR_GOODPUT_UNPACED),
                                ] {
                                    if let Some(y) = interpolate_line(cdf, data_x) {
                                        let dot = gp_resp
                                            .transform
                                            .position_from_point(&PlotPoint { x: data_x, y });
                                        dots.push((dot, color));
                                        entries.push((
                                            format!(
                                                "{} {}: {}",
                                                format_pct(pct),
                                                name,
                                                format_goodput(y)
                                            ),
                                            color,
                                        ));
                                    }
                                }
                                if !entries.is_empty() {
                                    tip = Some(TooltipState {
                                        rect: gp_resp.response.rect,
                                        dots,
                                        entries,
                                    });
                                }
                            }
                        }

                        if let Some(state) = tip {
                            ui.painter().vline(
                                pos.x,
                                state.rect.top()..=state.rect.bottom(),
                                Stroke::new(1.0_f32, Color32::from_gray(180)),
                            );

                            for (dot, color) in &state.dots {
                                if state.rect.contains(*dot) {
                                    ui.painter()
                                        .circle_filled(*dot, 4.5, Color32::from_gray(20));
                                    ui.painter().circle_filled(*dot, 3.0, *color);
                                }
                            }

                            let font = egui::FontId::proportional(12.0);
                            let padding = 6.0_f32;
                            let line_spacing = 2.0_f32;
                            let galleys: Vec<_> = state
                                .entries
                                .iter()
                                .map(|(text, color)| {
                                    ui.painter()
                                        .layout_no_wrap(text.clone(), font.clone(), *color)
                                })
                                .collect();

                            let box_w = galleys.iter().map(|g| g.size().x).fold(0.0_f32, f32::max)
                                + padding * 2.0;
                            let box_h = galleys.iter().map(|g| g.size().y).sum::<f32>()
                                + (galleys.len().saturating_sub(1) as f32) * line_spacing
                                + padding * 2.0;
                            let box_x = if pos.x + 14.0 + box_w < state.rect.right() {
                                pos.x + 14.0
                            } else {
                                pos.x - 14.0 - box_w
                            };
                            let anchor_y = state.dots.first().map(|(d, _)| d.y).unwrap_or(pos.y);
                            let box_y = (anchor_y - box_h * 0.5).clamp(
                                state.rect.top(),
                                (state.rect.bottom() - box_h).max(state.rect.top()),
                            );
                            let box_rect = egui::Rect::from_min_size(
                                egui::pos2(box_x, box_y),
                                egui::vec2(box_w, box_h),
                            );
                            ui.painter().rect_filled(
                                box_rect,
                                4.0,
                                Color32::from_rgba_premultiplied(25, 25, 30, 225),
                            );
                            ui.painter().rect_stroke(
                                box_rect,
                                4.0,
                                Stroke::new(1.0_f32, Color32::from_gray(70)),
                            );
                            let mut text_y = box_y + padding;
                            for galley in galleys {
                                ui.painter().galley(
                                    egui::pos2(box_x + padding, text_y),
                                    galley.clone(),
                                    Color32::TRANSPARENT,
                                );
                                text_y += galley.size().y + line_spacing;
                            }
                        }
                    }
                }

                // Nothing at all to show — say so plainly rather than leaving a
                // blank page. (The context strips above still render when cpu data
                // is present.)
                if !has_states
                    && !has_count
                    && !has_srtt
                    && !has_gp
                    && self.if_profile.is_empty()
                    && self.tcp_samples.rps_samples.is_empty()
                    && self.nginx_profile.requests.is_empty()
                {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.label(
                            RichText::new("No system data in this capture.")
                                .size(14.0)
                                .color(Color32::from_gray(160)),
                        );
                    });
                }
            }); // end ScrollArea

        if self.system_layout.maximized.is_some() && Self::escape_restores_plot_maximize(ui) {
            self.system_layout.clear_maximize();
            self.system_draft.clear_maximize();
        }

        self.finish_shared_time_axis(ui, &mut cursor, SYSTEM_TIME_PLOT_IDS);

        // Deferred from the checkbox above: applied here, after the
        // scroll closure's borrow of `agg` (and everything else it
        // read from `self.aggregate`) has ended.
        if filter_low_events_changed {
            let trace_t0_us = self
                .file_info
                .as_ref()
                .map(|fi| fi.capture_start)
                .unwrap_or(0);
            self.aggregate = Some(
                compute_aggregate(
                    &self.summaries,
                    &self.stars_agg_samples,
                    self.filter_low_events,
                    trace_t0_us,
                )
                .with_cpu_strips(&self.cpu_profile, trace_t0_us)
                .with_tcp_state(&self.tcp_samples, trace_t0_us)
                .with_if_profile(&self.if_profile, trace_t0_us)
                .with_ella_profile(&self.ella_profile, trace_t0_us)
                .with_mem_series(&self.mem_series),
            );
        }
    }

    /// Over-time mode of the TCP Average RTT / Goodput panels: 250-bin
    /// mean of the per-session average over the active sessions, on the
    /// shared System time axis. Idle bins are gaps.
    /// Associated fn (not `&mut self`) so it can run while `aggregate`
    /// is borrowed.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn avg_over_time_panel(
        time_axis: &TimeAxisController,
        ui: &mut egui::Ui,
        cursor: &mut SysCursor,
        plot_id: &str,
        y_label: &str,
        fmt: fn(f64) -> String,
        series: [(&AvgSegments, &str, Color32); 2],
        seed: (f64, f64, bool),
        time_max: f64,
    ) {
        let height = cursor.plot_h(ui);
        time_axis.apply(ui.ctx(), plot_id);
        let view = seed_view_x(seed.0, seed.1, 0.0, time_max.max(1.0));
        let binned: [Vec<[f64; 2]>; 2] = [
            avg_timeline_bins(series[0].0, view[0], view[1], cursor.plot_bins),
            avg_timeline_bins(series[1].0, view[0], view[1], cursor.plot_bins),
        ];
        let hidden = PlotMemory::load(ui.ctx(), egui::Id::new(plot_id))
            .map(|m| m.hidden_items)
            .unwrap_or_default();
        let bin_w = (view[1] - view[0]) / cursor.plot_bins as f64;
        let lines: [Vec<Vec<[f64; 2]>>; 2] = [
            avg_timeline_polylines(&binned[0], bin_w),
            avg_timeline_polylines(&binned[1], bin_w),
        ];
        let mut y_peak = 0.0_f64;
        for (i, (_, name, _)) in series.iter().enumerate() {
            if hidden.contains(*name) {
                continue;
            }
            for seg in &lines[i] {
                for p in seg {
                    y_peak = y_peak.max(p[1]);
                }
            }
        }
        let y_top = y_axis_top(y_peak);
        let mut plot = Plot::new(plot_id)
            .id(egui::Id::new(plot_id))
            .height(height)
            .set_margin_fraction(egui::vec2(0.0, 0.0))
            .include_x(seed.0)
            .include_x(seed.1)
            .auto_bounds(Vec2b::new(false, false))
            .include_y(0.0)
            .include_y(y_top)
            .show_x(false)
            .show_y(false)
            .allow_scroll(false)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .custom_x_axes(vec![agg_x_time_axis(cursor.utc_origin)])
            .custom_y_axes(vec![agg_y_axis(y_label)])
            .y_grid_spacer(nice_y_grid_spacer);
        plot = maybe_legend(plot, cursor.show_legends);
        if cursor.utc_origin.is_some() {
            plot = plot.x_grid_spacer(utc_x_grid_spacer);
        }
        let resp = plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(PlotBounds::from_min_max([seed.0, 0.0], [seed.1, y_top]));
            plot_ui.set_auto_bounds(Vec2b::new(false, false));
            for (i, (_, name, color)) in series.iter().enumerate() {
                for seg in &lines[i] {
                    if seg.len() < 2 {
                        continue;
                    }
                    plot_ui.line(
                        Line::new(PlotPoints::new(seg.clone()))
                            .color(*color)
                            .width(1.5_f32)
                            .name(*name),
                    );
                }
            }
        });

        if let Some(data_x) = cursor.panel(resp.response.rect, &resp.transform, true) {
            let hidden = PlotMemory::load(ui.ctx(), egui::Id::new(plot_id))
                .map(|m| m.hidden_items)
                .unwrap_or_default();
            for (i, (_, name, color)) in series.iter().enumerate() {
                if hidden.contains(*name) || binned[i].is_empty() {
                    continue;
                }
                // Pick the bin under the cursor; idle bins have no mean.
                let y = if bin_w > 0.0 && data_x >= view[0] && data_x <= view[1] {
                    let idx = ((data_x - view[0]) / bin_w).floor() as usize;
                    binned[i]
                        .get(idx.min(binned[i].len().saturating_sub(1)))
                        .and_then(|p| (p[1] > 0.0).then_some(p[1]))
                } else {
                    None
                };
                if let Some(y) = y {
                    cursor.add(
                        &resp.transform,
                        data_x,
                        y,
                        format!("{name}: {}", fmt(y)),
                        *color,
                    );
                }
            }
        }
        cursor.last_plot_rect = Some(resp.response.rect);
    }

    /// Establish / teardown conn/s from the same `TCP_SAMPLE` ticks as
    /// TCP Connections by State, equal-time buckets over the visible window.
    pub(crate) fn conn_rate_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        let Some(agg) = self.aggregate.as_ref() else {
            return;
        };
        let rate = &agg.tcp_conn_rate;
        if rate.estab.is_empty() && rate.teardown.is_empty() {
            return;
        }
        let seed = self.agg_time_axis.seed(0.0, agg.time_max.max(1.0));
        self.agg_time_axis.apply(ui.ctx(), "agg_conn_rate_plot");

        let mut data_lo = f64::INFINITY;
        let mut data_hi = f64::NEG_INFINITY;
        for &(t, _) in rate.estab.iter().chain(&rate.teardown) {
            data_lo = data_lo.min(t);
            data_hi = data_hi.max(t);
        }
        if !data_lo.is_finite() {
            data_lo = 0.0;
        }
        if !data_hi.is_finite() || data_hi <= data_lo {
            data_hi = data_lo + 1.0;
        }
        let [vx0, vx1] = seed_view_x(seed.0, seed.1, data_lo, data_hi);
        let estab = analysis::tcp_conn_rate_bins(&rate.estab, vx0, vx1, cursor.plot_bins);
        let down = analysis::tcp_conn_rate_bins(&rate.teardown, vx0, vx1, cursor.plot_bins);
        let peak_e = estab.iter().map(|p| p[1]).fold(0.0_f64, f64::max);
        let peak_d = down.iter().map(|p| p[1]).fold(0.0_f64, f64::max);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.strong(RichText::new("TCP Connection Rate").size(14.0));
            ui.add_space(16.0);
            ui.label(
                RichText::new(format!(
                    "Established peak: {:.0}/s  |  Torn down peak: {:.0}/s",
                    peak_e, peak_d
                ))
                .size(12.0)
                .color(Color32::from_gray(180)),
            );
        });

        let mut lines: Vec<PlotLine<'_>> = Vec::new();
        if estab.iter().any(|p| p[1] > 0.0) {
            lines.push(PlotLine {
                name: "Established".to_string(),
                color: COLOR_TCP_ESTAB,
                data: &estab,
            });
        }
        if down.iter().any(|p| p[1] > 0.0) {
            lines.push(PlotLine {
                name: "Torn Down".to_string(),
                color: COLOR_TCP_RPS,
                data: &down,
            });
        }
        draw_series_plot(
            ui,
            "agg_conn_rate_plot",
            "conn/s",
            cursor.panel_h,
            &lines,
            cursor,
            seed,
        );
    }

    /// Render the subsystem-wide "TCP Connections by State" panel. Placed
    /// after the HTTP Requests per Second panel on the Aggregate tab. `TCP_SAMPLE`
    /// counts every PCB in the subsystem (across all stacks / VNETs), so
    /// this is a cross-stack gauge, not any single stack's. `established`
    /// dwarfs the lifecycle states by 1–3 orders of magnitude — toggle it
    /// off in the legend to rescale. No-op when the capture has no
    /// `TCP_SAMPLE` data.
    pub(crate) fn draw_tcp_states_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        let agg = match &self.aggregate {
            Some(a) => a,
            None => return,
        };
        if agg.tcp_states.estab.is_empty() {
            return;
        }
        let st = &agg.tcp_states;
        let seed = self.agg_time_axis.seed(0.0, agg.time_max.max(1.0));
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.strong(RichText::new("TCP Connections by State").size(14.0));
            ui.add_space(16.0);
            ui.label(
                RichText::new(format!("Established peak: {:.0}", agg.tcp_states_peak))
                    .size(12.0)
                    .color(Color32::from_gray(180)),
            );
        });
        self.agg_time_axis.apply(ui.ctx(), "agg_tcp_states_plot");
        let view = seed_view_x(seed.0, seed.1, 0.0, agg.time_max.max(1.0));
        let hidden = PlotMemory::load(ui.ctx(), egui::Id::new("agg_tcp_states_plot"))
            .map(|m| m.hidden_items)
            .unwrap_or_default();
        let mut y_peak = 0.0_f64;
        if !hidden.contains("established") {
            y_peak = y_peak.max(step_peak_in_view(&st.estab, view));
        }
        for line in &st.others {
            if hidden.contains(line.name) {
                continue;
            }
            y_peak = y_peak.max(step_peak_in_view(&line.data, view));
        }
        let y_top = y_axis_top(y_peak);
        let mut tcp_states_plot = Plot::new("agg_tcp_states_plot")
            .id(egui::Id::new("agg_tcp_states_plot"))
            .height(cursor.plot_h(ui))
            // Edge-to-edge like the CPU strip: no interior padding, 0 pinned
            // to the bottom frame. Top is the visible-series peak plus 8% so
            // a line isn't clipped; hiding `established` still rescales.
            .set_margin_fraction(egui::vec2(0.0, 0.0))
            .include_x(seed.0)
            .include_x(seed.1)
            .auto_bounds(Vec2b::new(false, false))
            .include_y(0.0)
            .include_y(y_top)
            .show_x(false)
            .show_y(false)
            .allow_scroll(false)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .custom_x_axes(vec![agg_x_time_axis(cursor.utc_origin)])
            .custom_y_axes(vec![agg_y_axis("Connections")])
            .y_grid_spacer(nice_y_grid_spacer);
        tcp_states_plot = maybe_legend(tcp_states_plot, cursor.show_legends);
        if cursor.utc_origin.is_some() {
            tcp_states_plot = tcp_states_plot.x_grid_spacer(utc_x_grid_spacer);
        }
        let resp = tcp_states_plot.show(ui, |plot_ui| {
            plot_ui.set_plot_bounds(PlotBounds::from_min_max([seed.0, 0.0], [seed.1, y_top]));
            plot_ui.set_auto_bounds(Vec2b::new(false, false));
            // Downsample each state series to the visible window every
            // frame (like the interface/RPS panels) instead of cloning
            // and plotting every raw sample. On large captures the raw
            // path is O(all samples) per frame and was the dominant
            // System-tab render cost. `downsample_mean` returns the exact
            // visible slice once it fits under MAX_DISPLAY_POINTS, so
            // zoomed-in detail is preserved.
            plot_ui.line(
                Line::new(PlotPoints::from(downsample_mean(
                    &st.estab,
                    view,
                    MAX_DISPLAY_POINTS,
                )))
                .color(COLOR_TCP_ESTAB)
                .width(1.5_f32)
                .name("established"),
            );
            for (i, line) in st.others.iter().enumerate() {
                plot_ui.line(
                    Line::new(PlotPoints::from(downsample_mean(
                        &line.data,
                        view,
                        MAX_DISPLAY_POINTS,
                    )))
                    .color(series_color(i))
                    .width(1.2_f32)
                    .name(line.name),
                );
            }
        });

        // Shared-cursor contribution: one tooltip row per visible state.
        if let Some(data_x) = cursor.panel(resp.response.rect, &resp.transform, true) {
            let hidden = PlotMemory::load(ui.ctx(), egui::Id::new("agg_tcp_states_plot"))
                .map(|m| m.hidden_items)
                .unwrap_or_default();
            if !hidden.contains("established") {
                if let Some(y) = interpolate_line(&st.estab, data_x) {
                    cursor.add(
                        &resp.transform,
                        data_x,
                        y,
                        format!("established: {y:.0}"),
                        COLOR_TCP_ESTAB,
                    );
                }
            }
            for (i, line) in st.others.iter().enumerate() {
                if hidden.contains(line.name) {
                    continue;
                }
                if let Some(y) = interpolate_line(&line.data, data_x) {
                    cursor.add(
                        &resp.transform,
                        data_x,
                        y,
                        format!("{}: {y:.0}", line.name),
                        series_color(i),
                    );
                }
            }
        }
        cursor.last_plot_rect = Some(resp.response.rect);
    }

    /// Trace-wide info popup, shown from the "i" button in the tab bar
    /// (next to the gear) so it's reachable from any tab, not
    /// just System. Anchored just below the button (passed in as
    /// `info_rect`) and dismissed on Escape or any click that lands
    /// outside both the button and the popup body. Mirrors the
    /// click-outside / Escape handling in the per-session info button.
    pub(crate) fn show_aggregate_info_popup(
        &self,
        ui: &mut egui::Ui,
        info_id: egui::Id,
        info_rect: egui::Rect,
    ) -> bool {
        let screen = ui.ctx().screen_rect();
        let popup_right = screen.right() - 8.0;
        let popup_top = info_rect.bottom() + 4.0;

        let total_sessions = self.summaries.len();
        let paced_sessions = self.summaries.iter().filter(|s| is_paced(s)).count();
        let unpaced_sessions = total_sessions.saturating_sub(paced_sessions);

        // Provider-aware section toggle: only show TCP-specific stats
        // when the `tcp` provider was actually present in the file.
        let has_tcp = self.providers.iter().any(|p| p.name == "tcp");
        let has_hpts = self.providers.iter().any(|p| p.name == "hpts");

        // Ascending by id, not file registration order, so the table
        // reads as a stable, predictable list.
        let mut providers_by_id: Vec<&Provider> = self.providers.iter().collect();
        providers_by_id.sort_by_key(|p| p.id);
        let tcp_stacks: Vec<&Provider> = providers_by_id
            .iter()
            .copied()
            .filter(|p| p.name == "tcp")
            .collect();

        let info_area = egui::Area::new(info_id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(popup_right, popup_top))
            .pivot(egui::Align2::RIGHT_TOP)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .show(ui, |ui| {
                        ui.set_min_width(280.0);
                        ui.set_max_width(440.0);

                        let row = |ui: &mut egui::Ui, k: &str, v: String| {
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                                ui.label(RichText::new(k).weak().size(11.0));
                            });
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                                ui.add(egui::Label::new(RichText::new(v).size(11.0)).wrap(true));
                            });
                            ui.end_row();
                        };

                        let open_events = egui::ScrollArea::vertical()
                            .max_height(432.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                let mut open_events = false;
                                // ── Host ────────────────────────────────────────────
                                if let Some(fi) = &self.file_info {
                                    if let Some(mi) = &fi.machine {
                                        ui.label(RichText::new("Host").strong().size(12.0));
                                        ui.separator();
                                        egui::Grid::new("agg_info_host")
                                            .num_columns(2)
                                            .spacing([16.0, 2.0])
                                            .show(ui, |ui| {
                                                if !mi.hostname.is_empty() {
                                                    row(ui, "Hostname", mi.hostname.clone());
                                                }
                                                if !mi.hostuuid.is_empty() {
                                                    row(ui, "Host UUID", mi.hostuuid.clone());
                                                }
                                                if !mi.firmware_version.is_empty() {
                                                    row(
                                                        ui,
                                                        "Firmware",
                                                        mi.firmware_version.clone(),
                                                    );
                                                }
                                                if !mi.osrelease.is_empty() {
                                                    row(ui, "OS release", mi.osrelease.clone());
                                                }
                                                if !mi.kernel_version.is_empty() {
                                                    row(ui, "Kernel", mi.kernel_version.clone());
                                                }
                                                if !mi.platform.is_empty() {
                                                    row(ui, "Platform", mi.platform.clone());
                                                }
                                                if !mi.cpu_model.is_empty() {
                                                    row(ui, "CPU", mi.cpu_model.clone());
                                                }
                                                if mi.ncpu > 0 {
                                                    row(ui, "CPUs", mi.ncpu.to_string());
                                                }
                                                if mi.physmem > 0 {
                                                    row(ui, "Memory", format_bytes(mi.physmem));
                                                }
                                                for (k, v) in &mi.metadata_kv {
                                                    row(ui, k.as_str(), v.clone());
                                                }
                                            });
                                        ui.add_space(6.0);
                                    }
                                }

                                // ── File ────────────────────────────────────────────
                                ui.label(RichText::new("File").strong().size(12.0));
                                ui.separator();
                                egui::Grid::new("agg_info_file")
                                    .num_columns(2)
                                    .spacing([16.0, 2.0])
                                    .show(ui, |ui| {
                                        if let Some(name) = &self.loaded_file_name {
                                            row(ui, "Name", name.clone());
                                        }
                                        if let Some(fi) = &self.file_info {
                                            row(ui, "Format version", fi.version.to_string());
                                            row(
                                                ui,
                                                "Capture start",
                                                format_utc_us(fi.start_utc_us),
                                            );
                                            row(
                                                ui,
                                                "Capture end",
                                                format_utc_us(capture_end_utc_us(fi)),
                                            );
                                            row(
                                                ui,
                                                "Duration",
                                                format_time_span(
                                                    fi.capture_end.saturating_sub(fi.capture_start)
                                                        as f64
                                                        / 1_000_000.0,
                                                ),
                                            );
                                            row(
                                                ui,
                                                "Events",
                                                format!(
                                                    "{} ({} parsed)",
                                                    format_count(fi.event_count),
                                                    format_count(fi.parsed as u64)
                                                ),
                                            );
                                            if fi.sources.len() > 1 {
                                                row(
                                                    ui,
                                                    "Merged files",
                                                    fi.sources.len().to_string(),
                                                );
                                                for src in &fi.sources {
                                                    row(
                                                        ui,
                                                        "",
                                                        format!(
                                                            "{} @ {}",
                                                            src.name,
                                                            format_time_span(
                                                                src.capture_start.saturating_sub(
                                                                    fi.capture_start
                                                                )
                                                                    as f64
                                                                    / 1_000_000.0,
                                                            ),
                                                        ),
                                                    );
                                                }
                                            }
                                            if fi.backlog_events > 0 {
                                                row(
                                                    ui,
                                                    "Backlog skipped",
                                                    format!(
                                                        "{} ({} event{})",
                                                        format_time_span(
                                                            fi.capture_start.saturating_sub(
                                                                fi.header_capture_start
                                                            )
                                                                as f64
                                                                / 1_000_000.0,
                                                        ),
                                                        fi.backlog_events,
                                                        if fi.backlog_events == 1 {
                                                            ""
                                                        } else {
                                                            "s"
                                                        },
                                                    ),
                                                );
                                            }
                                            if fi.dropped > 0 {
                                                row(
                                                    ui,
                                                    "Dropped (kernel)",
                                                    format_count(fi.dropped),
                                                );
                                            }
                                            if fi.user_dropped_full > 0 {
                                                row(
                                                    ui,
                                                    "Dropped (user, ring full)",
                                                    format_count(fi.user_dropped_full),
                                                );
                                            }
                                            if fi.user_dropped_prejoin > 0 {
                                                row(
                                                    ui,
                                                    "Dropped (user, prejoin)",
                                                    format_count(fi.user_dropped_prejoin),
                                                );
                                            }
                                            if fi.user_dropped_late > 0 {
                                                row(
                                                    ui,
                                                    "Dropped (user, late)",
                                                    format_count(fi.user_dropped_late),
                                                );
                                            }
                                            if self.cpu_profile.overflow_dropped > 0 {
                                                row(
                                                    ui,
                                                    "Dropped (CPU sampler overflow)",
                                                    format_count(self.cpu_profile.overflow_dropped),
                                                );
                                            }
                                        } else {
                                            ui.label(
                                                RichText::new("(file info unavailable)")
                                                    .weak()
                                                    .size(11.0),
                                            );
                                            ui.end_row();
                                        }
                                    });

                                // ── Providers ───────────────────────────────────────
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.label(RichText::new("Providers").strong().size(12.0));
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let resp = ui
                                                .link(RichText::new("More details...").size(11.0))
                                                .on_hover_text(
                                                    "Event ids, record sizes, and schema fields \
                                                 for this capture",
                                                );
                                            if resp.clicked() {
                                                open_events = true;
                                            }
                                        },
                                    );
                                });
                                ui.separator();
                                if providers_by_id.is_empty() {
                                    ui.label(
                                        RichText::new("(no provider table)").weak().size(11.0),
                                    );
                                } else {
                                    let mut groups: std::collections::BTreeMap<
                                        String,
                                        (usize, u64, u64, u64),
                                    > = std::collections::BTreeMap::new();
                                    for p in &providers_by_id {
                                        let name = if p.name.is_empty() {
                                            "(unnamed)".to_string()
                                        } else {
                                            p.name.clone()
                                        };
                                        let e = groups.entry(name).or_insert((0, 0, 0, 0));
                                        e.0 += 1;
                                        e.1 += p.session_count as u64;
                                        e.2 += p.event_count;
                                        e.3 += p.byte_size;
                                    }
                                    egui::Grid::new("agg_info_providers")
                                        .num_columns(5)
                                        .spacing([16.0, 2.0])
                                        .show(ui, |ui| {
                                            ui.label(RichText::new("Name").weak().size(11.0));
                                            ui.label(RichText::new("Providers").weak().size(11.0));
                                            ui.label(RichText::new("Sessions").weak().size(11.0));
                                            ui.label(RichText::new("Events").weak().size(11.0));
                                            ui.label(RichText::new("Size").weak().size(11.0));
                                            ui.end_row();
                                            for (name, (n, sessions, events, bytes)) in &groups {
                                                ui.label(RichText::new(name.clone()).size(11.0));
                                                ui.label(
                                                    RichText::new(format_count(*n as u64))
                                                        .size(11.0),
                                                );
                                                ui.label(
                                                    RichText::new(format_count(*sessions))
                                                        .size(11.0),
                                                );
                                                ui.label(
                                                    RichText::new(format_count(*events)).size(11.0),
                                                );
                                                ui.label(
                                                    RichText::new(format_bytes(*bytes)).size(11.0),
                                                );
                                                ui.end_row();
                                            }
                                        });
                                }

                                // ── Network Interfaces (provider-specific) ─────────
                                if !self.info_if_bytes.is_empty() {
                                    ui.add_space(6.0);
                                    ui.label(
                                        RichText::new("Network Interfaces").strong().size(12.0),
                                    );
                                    ui.separator();
                                    egui::Grid::new("agg_info_interfaces")
                                        .num_columns(3)
                                        .spacing([16.0, 2.0])
                                        .show(ui, |ui| {
                                            ui.label(RichText::new("Name").weak().size(11.0));
                                            ui.label(RichText::new("TX Bytes").weak().size(11.0));
                                            ui.label(RichText::new("RX Bytes").weak().size(11.0));
                                            ui.end_row();
                                            for s in &self.info_if_bytes {
                                                ui.label(RichText::new(s.name.clone()).size(11.0));
                                                ui.label(
                                                    RichText::new(format_bytes(s.total_tx_bytes))
                                                        .size(11.0),
                                                );
                                                ui.label(
                                                    RichText::new(format_bytes(s.total_rx_bytes))
                                                        .size(11.0),
                                                );
                                                ui.end_row();
                                            }
                                        });
                                }

                                // ── TCP (provider-specific) ────────────────────────
                                if has_tcp {
                                    ui.add_space(6.0);
                                    ui.label(RichText::new("TCP").strong().size(12.0));
                                    ui.separator();
                                    if total_sessions > 0 {
                                        egui::Grid::new("agg_info_tcp_totals")
                                            .num_columns(2)
                                            .spacing([16.0, 2.0])
                                            .show(ui, |ui| {
                                                row(
                                                    ui,
                                                    "Paced sessions",
                                                    paced_sessions.to_string(),
                                                );
                                                row(
                                                    ui,
                                                    "Not paced sessions",
                                                    unpaced_sessions.to_string(),
                                                );
                                            });
                                        ui.add_space(4.0);
                                    }
                                    if !tcp_stacks.is_empty() {
                                        egui::Grid::new("agg_info_tcp_stacks")
                                            .num_columns(2)
                                            .spacing([16.0, 2.0])
                                            .show(ui, |ui| {
                                                ui.label(RichText::new("Name").weak().size(11.0));
                                                ui.label(
                                                    RichText::new("Sessions").weak().size(11.0),
                                                );
                                                ui.end_row();
                                                for p in &tcp_stacks {
                                                    let name = self.tcp_samples.stack_label(
                                                        p.id,
                                                        TCP_SESSION_ID_STACK_GLOBAL,
                                                    );
                                                    ui.label(RichText::new(name).size(11.0));
                                                    ui.label(
                                                        RichText::new(format_count(
                                                            p.session_count as u64,
                                                        ))
                                                        .size(11.0),
                                                    );
                                                    ui.end_row();
                                                }
                                            });
                                    }
                                }

                                // ── Fanout (provider-specific) ─────────────────────
                                if !self.ella_profile.is_empty() {
                                    let ella = &self.ella_info;
                                    ui.add_space(6.0);
                                    ui.label(RichText::new("Fanout").strong().size(12.0));
                                    ui.separator();
                                    egui::Grid::new("agg_info_ella")
                                        .num_columns(2)
                                        .spacing([16.0, 2.0])
                                        .show(ui, |ui| {
                                            let instances = match (
                                                ella.upstream_instances,
                                                ella.downstream_instances,
                                            ) {
                                                (0, d) => format!("{d} downstream"),
                                                (u, 0) => format!("{u} upstream"),
                                                (u, d) => format!("{u} upstream, {d} downstream"),
                                            };
                                            row(ui, "Fanout instances", instances);
                                            row(ui, "Titles", format_count(ella.titles as u64));
                                            row(
                                                ui,
                                                "Channel sessions",
                                                format!(
                                                    "{} opened, peak live {}",
                                                    format_count(ella.channel_sessions as u64),
                                                    format_count(ella.peak_live_sessions as u64)
                                                ),
                                            );
                                            row(
                                                ui,
                                                "Peak subscribers",
                                                format_count(ella.peak_subscribers as u64),
                                            );
                                            if ella.relay_errors > 0 {
                                                row(
                                                    ui,
                                                    "Node errors",
                                                    format_count(ella.relay_errors as u64),
                                                );
                                            }
                                        });
                                }

                                // ── HPTS (provider-specific) ───────────────────────
                                if has_hpts {
                                    ui.add_space(6.0);
                                    ui.label(RichText::new("HPTS").strong().size(12.0));
                                    ui.separator();
                                    egui::Grid::new("agg_info_hpts")
                                        .num_columns(2)
                                        .spacing([16.0, 2.0])
                                        .show(ui, |ui| {
                                            row(
                                                ui,
                                                "STARS aggregate samples",
                                                format_count(self.stars_agg_samples.len() as u64),
                                            );
                                            row(
                                                ui,
                                                "Wheel events",
                                                format_count(self.hpts_wheel_events.len() as u64),
                                            );
                                        });
                                }
                                open_events
                            })
                            .inner;
                        open_events
                    })
                    .inner
            });
        let open_events = info_area.inner;
        let info_area_rect = info_area.response.rect;

        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.memory_mut(|m| m.data.insert_temp(info_id, false));
        }
        if ui.ctx().input(|i| i.pointer.any_click()) {
            if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
                if !info_rect.contains(pos) && !info_area_rect.contains(pos) {
                    ui.memory_mut(|m| m.data.insert_temp(info_id, false));
                }
            }
        }
        open_events
    }

    pub(crate) fn show_plot_view(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        session_idx: usize,
    ) {
        let ids = plot_ids(self.open_sessions[session_idx].summary.session_id);

        {
            let pv = &mut self.open_sessions[session_idx];
            if pv.needs_reset {
                pv.needs_reset = false;
                pv.y_max_override = [None; 5];
                pv.y_max_editor = None;
                for id_str in &ids {
                    let plot_id = egui::Id::new(id_str.as_str());
                    ctx.data_mut(|d| d.remove::<PlotMemory>(plot_id));
                }
            }
        }

        let session_id = self.open_sessions[session_idx].summary.session_id;
        let limiter_compact = self.open_sessions[session_idx].limiter_summary.compact();

        ui.horizontal(|ui| {
            if !limiter_compact.is_empty() {
                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("Limiter: {limiter_compact}"))
                        .size(12.0)
                        .color(Color32::from_gray(180)),
                )
                .on_hover_text(
                    "Time-weighted breakdown of what was limiting the sender during \
                     this session. Toggle the overlay in the gear menu to see it on \
                     the timeline.",
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Custom-drawn gear button: egui's default text layout baselines the
                // U+2699 glyph near the bottom of its line box, which makes it look
                // visibly off-center inside a standard `menu_button`. Painting the
                // glyph ourselves with `Align2::CENTER_CENTER` centers it in the
                // button rect regardless of the font's baseline.
                let popup_id = ui.make_persistent_id(("gear_menu", session_id));
                let mut is_open = ui
                    .memory(|m| m.data.get_temp::<bool>(popup_id))
                    .unwrap_or(false);
                if self.open_sessions[session_idx].open_gear_menu {
                    is_open = true;
                    self.open_sessions[session_idx].open_gear_menu = false;
                    ui.memory_mut(|m| m.data.insert_temp(popup_id, true));
                }

                let button_h = ui.spacing().interact_size.y;
                let button_size = egui::vec2(button_h, button_h);
                let (info_rect, info_response) =
                    ui.allocate_exact_size(button_size, egui::Sense::click());
                let (rect, response) = ui.allocate_exact_size(button_size, egui::Sense::click());
                let visuals = if is_open {
                    ui.visuals().widgets.open
                } else {
                    *ui.style().interact(&response)
                };
                ui.painter().rect(
                    rect.expand(visuals.expansion),
                    visuals.rounding,
                    visuals.weak_bg_fill,
                    visuals.bg_stroke,
                );
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "\u{2699}",
                    egui::FontId::proportional(14.0),
                    visuals.text_color(),
                );
                let response = response.on_hover_text("Plot overlay options");

                if response.clicked() {
                    self.open_sessions[session_idx].open_gear_menu = false;
                    ui.memory_mut(|m| m.data.insert_temp(popup_id, !is_open));
                }

                if is_open {
                    let area = egui::Area::new(popup_id.with("area"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(rect.right_bottom() + egui::vec2(0.0, 4.0))
                        .pivot(egui::Align2::RIGHT_TOP)
                        .constrain_to(ui.ctx().screen_rect())
                        .show(ui.ctx(), |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.set_min_width(200.0);
                                let has_dsack =
                                    !self.open_sessions[session_idx].dsack_markers.is_empty();
                                ui.add_enabled_ui(has_dsack, |ui| {
                                    let resp = ui.checkbox(
                                        &mut self.open_sessions[session_idx].show_dsack,
                                        RichText::new("DSACK markers").size(12.0),
                                    );
                                    if has_dsack {
                                        resp.on_hover_text(
                                            "Thin vertical lines marking duplicate-SACK \
                                             ranges reported by the receiver. Typically \
                                             the noisiest of the three marker streams.",
                                        );
                                    } else {
                                        resp.on_hover_text("No DSACK events in this session");
                                    }
                                });
                                ui.checkbox(
                                    &mut self.open_sessions[session_idx].show_goodput_spans,
                                    RichText::new("Goodput spans").size(12.0),
                                )
                                .on_hover_text("Shade each kernel-goodput measurement window");
                                let has_limiter =
                                    !self.open_sessions[session_idx].limiter_spans.is_empty();
                                ui.add_enabled_ui(has_limiter, |ui| {
                                    let resp = ui.checkbox(
                                        &mut self.open_sessions[session_idx].show_limiter,
                                        RichText::new("Limiter overlay").size(12.0),
                                    );
                                    if has_limiter {
                                        resp.on_hover_text(
                                            "Shade background by what was limiting the sender \
                                             in each 50 ms bucket (cwnd / rwnd / pacing / app). \
                                             Unclassifiable buckets are left un-shaded.",
                                        );
                                    } else {
                                        resp.on_hover_text(
                                            "Not enough data-plane activity to classify a limiter",
                                        );
                                    }
                                });
                                let has_caps =
                                    !self.open_sessions[session_idx].pacing_cap_spans.is_empty();
                                ui.add_enabled_ui(has_caps, |ui| {
                                    let resp = ui.checkbox(
                                        &mut self.open_sessions[session_idx].show_pacing_cap,
                                        RichText::new("Pacing cap").size(12.0),
                                    );
                                    if has_caps {
                                        resp.on_hover_text(
                                            "Shade regions where PACING_CALC_V2 reports a \
                                             software cap (STARS / bw_rate_cap / fixed) on \
                                             the pacing rate",
                                        );
                                    } else {
                                        resp.on_hover_text(
                                            "No PACING_CALC_V2 events in this session (or \
                                             none were clamped)",
                                        );
                                    }
                                });
                                let has_recovery =
                                    !self.open_sessions[session_idx].recovery_spans.is_empty();
                                ui.add_enabled_ui(has_recovery, |ui| {
                                    let resp = ui.checkbox(
                                        &mut self.open_sessions[session_idx].show_recovery_spans,
                                        RichText::new("Recovery spans").size(12.0),
                                    );
                                    if has_recovery {
                                        resp.on_hover_text(
                                            "Pink vertical bands marking each \
                                             ENTER_RECOVERY → EXIT_RECOVERY pair. \
                                             Hover one to see how long it lasted.",
                                        );
                                    } else {
                                        resp.on_hover_text("No recovery events in this session");
                                    }
                                });
                                let has_rto =
                                    !self.open_sessions[session_idx].rto_markers.is_empty();
                                ui.add_enabled_ui(has_rto, |ui| {
                                    let resp = ui.checkbox(
                                        &mut self.open_sessions[session_idx].show_rto,
                                        RichText::new("RTO markers").size(12.0),
                                    );
                                    if has_rto {
                                        resp.on_hover_text(
                                            "Thin vertical lines marking retransmission \
                                             timeout events. The reason text appears in \
                                             the shared cursor tooltip.",
                                        );
                                    } else {
                                        resp.on_hover_text("No RTO events in this session");
                                    }
                                });
                                let has_wc = !self.open_sessions[session_idx]
                                    .window_collapse_markers
                                    .is_empty();
                                ui.add_enabled_ui(has_wc, |ui| {
                                    let resp = ui.checkbox(
                                        &mut self.open_sessions[session_idx].show_window_collapse,
                                        RichText::new("Window Collapse markers").size(12.0),
                                    );
                                    if has_wc {
                                        resp.on_hover_text(
                                            "Thin vertical lines marking receive-window \
                                             collapse events (the peer shrank the window \
                                             below what we had in flight).",
                                        );
                                    } else {
                                        resp.on_hover_text(
                                            "No Window Collapse events in this session",
                                        );
                                    }
                                });
                            });
                        });
                    let area_rect = area.response.rect;

                    if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.open_sessions[session_idx].open_gear_menu = false;
                        ui.memory_mut(|m| m.data.insert_temp(popup_id, false));
                    }
                    if ui.ctx().input(|i| i.pointer.any_click()) {
                        if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
                            if !rect.contains(pos) && !area_rect.contains(pos) {
                                self.open_sessions[session_idx].open_gear_menu = false;
                                ui.memory_mut(|m| m.data.insert_temp(popup_id, false));
                            }
                        }
                    }
                }

                // ─── Session info button ────────────────────────────────
                // Uses "i" (lowercase) painted with the same centered-glyph
                // technique as the gear — avoids the box/baseline issues that
                // plague unicode circled characters in egui's default font.
                let info_id = ui.make_persistent_id(("info_popup", session_id));
                let info_open = ui
                    .memory(|m| m.data.get_temp::<bool>(info_id))
                    .unwrap_or(false);

                let info_visuals = if info_open {
                    ui.visuals().widgets.open
                } else {
                    *ui.style().interact(&info_response)
                };
                ui.painter().rect(
                    info_rect.expand(info_visuals.expansion),
                    info_visuals.rounding,
                    info_visuals.weak_bg_fill,
                    info_visuals.bg_stroke,
                );
                ui.painter().text(
                    info_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "i",
                    egui::FontId::proportional(13.0),
                    info_visuals.text_color(),
                );
                let info_response = info_response.on_hover_text("Session info");
                if info_response.clicked() {
                    ui.memory_mut(|m| m.data.insert_temp(info_id, !info_open));
                }

                if info_open {
                    let conn_params = self.open_sessions[session_idx].conn_params.clone();
                    let summary = self.open_sessions[session_idx].summary.clone();
                    let limiter = self.open_sessions[session_idx].limiter_summary;
                    let nginx_conn = self
                        .nginx_profile
                        .conns
                        .values()
                        .find(|c| c.session_id == summary.session_id)
                        .cloned();

                    // Position the popup just below the button, inset 8 px
                    // from the right edge of the window so it doesn't butt
                    // up against the frame border.
                    let screen = ui.ctx().screen_rect();
                    let popup_right = screen.right() - 8.0;
                    let popup_top = info_rect.bottom() + 4.0;

                    let info_area = egui::Area::new(info_id.with("area"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(egui::pos2(popup_right, popup_top))
                        .pivot(egui::Align2::RIGHT_TOP)
                        .show(ui.ctx(), |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.set_min_width(240.0);
                                ui.set_max_width(320.0);

                                ui.label(RichText::new("Session").strong().size(12.0));
                                ui.separator();
                                egui::Grid::new("info_grid_session")
                                    .num_columns(2)
                                    .spacing([16.0, 2.0])
                                    .show(ui, |ui| {
                                        let row = |ui: &mut egui::Ui, k: &str, v: String| {
                                            ui.label(RichText::new(k).weak().size(11.0));
                                            ui.label(RichText::new(v).size(11.0));
                                            ui.end_row();
                                        };
                                        row(ui, "Session ID", summary.session_id.to_string());
                                        row(
                                            ui,
                                            "Remote",
                                            if summary.remote.is_empty() {
                                                "(unknown)".into()
                                            } else {
                                                summary.remote.clone()
                                            },
                                        );
                                        row(
                                            ui,
                                            "Log ID",
                                            if summary.log_id.is_empty() {
                                                "(none)".into()
                                            } else {
                                                summary.log_id.clone()
                                            },
                                        );
                                        row(
                                            ui,
                                            "Duration",
                                            format!("{:.3} s", summary.duration_sec),
                                        );
                                        row(
                                            ui,
                                            "Bytes ACKed",
                                            format!(
                                                "{:.1} MB",
                                                summary.total_bytes_acked as f64 / 1e6
                                            ),
                                        );
                                        row(
                                            ui,
                                            "Mean goodput",
                                            format!("{:.1} Mb/s", summary.avg_goodput_mbps),
                                        );
                                        row(ui, "Recoveries", summary.recovery_count.to_string());
                                        // Owner-pid row lights up only when the
                                        // future EVT_CONN_OWNER_PID event has
                                        // populated SessionSummary::owner_pid.
                                        // We look up the comm via the cpu provider's
                                        // THREAD_CREATE map for a `comm (pid N)`
                                        // display, falling back to a bare pid when
                                        // the comm isn't available (cpu-only or
                                        // never-sampled pid).
                                        if let Some(pid) = summary.owner_pid {
                                            let comm = self
                                                .cpu_profile
                                                .threads
                                                .values()
                                                .find(|t| t.pid == pid && !t.comm.is_empty())
                                                .map(|t| t.comm.clone())
                                                .unwrap_or_default();
                                            let v = if comm.is_empty() {
                                                format!("pid {pid}")
                                            } else {
                                                format!("{comm} (pid {pid})")
                                            };
                                            row(ui, "Owner", v);
                                        }
                                    });

                                if let Some(ref p) = conn_params {
                                    ui.add_space(6.0);
                                    ui.label(
                                        RichText::new("Connection params").strong().size(12.0),
                                    );
                                    ui.separator();
                                    egui::Grid::new("info_grid_conn")
                                        .num_columns(2)
                                        .spacing([16.0, 2.0])
                                        .show(ui, |ui| {
                                            let row = |ui: &mut egui::Ui, k: &str, v: String| {
                                                ui.label(RichText::new(k).weak().size(11.0));
                                                ui.label(RichText::new(v).size(11.0));
                                                ui.end_row();
                                            };
                                            row(
                                                ui,
                                                "CC algo",
                                                if p.cc_algo.is_empty() {
                                                    "?".into()
                                                } else {
                                                    p.cc_algo.clone()
                                                },
                                            );
                                            if !p.tcp_stack.is_empty() {
                                                row(ui, "TCP stack", p.tcp_stack.clone());
                                            }
                                            row(ui, "snd_wscale", p.snd_wscale.to_string());
                                            row(ui, "rcv_wscale", p.rcv_wscale.to_string());
                                            row(
                                                ui,
                                                "Options",
                                                elog_parser::format_conn_params_flags(p.flags)
                                                    .to_string(),
                                            );
                                        });
                                }

                                if let Some(ref c) = nginx_conn {
                                    ui.add_space(6.0);
                                    ui.label(RichText::new("nginx connection").strong().size(12.0));
                                    ui.separator();
                                    egui::Grid::new("info_grid_nginx_conn")
                                        .num_columns(2)
                                        .spacing([16.0, 2.0])
                                        .show(ui, |ui| {
                                            let row = |ui: &mut egui::Ui, k: &str, v: String| {
                                                ui.label(RichText::new(k).weak().size(11.0));
                                                ui.label(RichText::new(v).size(11.0));
                                                ui.end_row();
                                            };
                                            if c.server_port != 0 {
                                                row(ui, "Port", c.server_port.to_string());
                                            }
                                            if let Some(t) = &c.tls {
                                                row(
                                                    ui,
                                                    "TLS",
                                                    format!(
                                                        "{} {}",
                                                        elog_parser::nginx_tls_version_name(
                                                            t.version
                                                        ),
                                                        t.cipher
                                                    ),
                                                );
                                                if !t.alpn.is_empty() {
                                                    row(ui, "ALPN", t.alpn.clone());
                                                }
                                                let flags =
                                                    elog_parser::nginx_tls_flag_names(t.flags);
                                                row(
                                                    ui,
                                                    "Handshake",
                                                    if flags.is_empty() {
                                                        format!("{} ms", t.handshake_ms)
                                                    } else {
                                                        format!("{} ms, {flags}", t.handshake_ms)
                                                    },
                                                );
                                            }
                                            if let Some(f) = &c.tls_fail {
                                                row(
                                                    ui,
                                                    "TLS failed",
                                                    format!(
                                                        "{} (ssl_error {}, errno {}) after {} ms",
                                                        f.kind,
                                                        f.ssl_error,
                                                        f.sys_errno,
                                                        f.handshake_ms
                                                    ),
                                                );
                                            }
                                            if let Some(r) = &c.close_reason {
                                                row(
                                                    ui,
                                                    "Closed",
                                                    format!(
                                                        "{r} after {} requests, {} ms",
                                                        c.requests, c.duration_ms
                                                    ),
                                                );
                                            }
                                            if c.busy_defers > 0 {
                                                row(
                                                    ui,
                                                    "Disk EBUSY",
                                                    format!(
                                                        "{} deferrals, {} ms",
                                                        c.busy_defers, c.busy_defer_ms
                                                    ),
                                                );
                                            }
                                            if c.sendfile_errors > 0 {
                                                row(
                                                    ui,
                                                    "Sendfile errors",
                                                    c.sendfile_errors.to_string(),
                                                );
                                            }
                                        });
                                }

                                ui.add_space(6.0);
                                ui.label(RichText::new("Limiter").strong().size(12.0));
                                ui.separator();
                                egui::Grid::new("info_grid_limiter")
                                    .num_columns(2)
                                    .spacing([16.0, 2.0])
                                    .show(ui, |ui| {
                                        let row = |ui: &mut egui::Ui, k: &str, v: f32| {
                                            ui.label(RichText::new(k).weak().size(11.0));
                                            ui.label(RichText::new(format!("{v:.1}%")).size(11.0));
                                            ui.end_row();
                                        };
                                        row(ui, "Pacing", limiter.pacing_pct as f32);
                                        row(ui, "CWND", limiter.cwnd_pct as f32);
                                        row(ui, "RWND", limiter.rwnd_pct as f32);
                                        row(ui, "App", limiter.app_pct as f32);
                                        row(ui, "Idle", limiter.unknown_pct as f32);
                                    });
                            });
                        });
                    let info_area_rect = info_area.response.rect;

                    if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                        ui.memory_mut(|m| m.data.insert_temp(info_id, false));
                    }
                    if ui.ctx().input(|i| i.pointer.any_click()) {
                        if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
                            if !info_rect.contains(pos) && !info_area_rect.contains(pos) {
                                ui.memory_mut(|m| m.data.insert_temp(info_id, false));
                            }
                        }
                    }
                }
            });
        });

        ui.separator();

        let has_if = !self.if_profile.is_empty();
        let pv = &mut self.open_sessions[session_idx];
        pv.show_legends = self.show_legends;

        let is_maximized = pv.plot_layout.maximized.is_some();
        // Panels with no underlying events (e.g. a STARS-pace-only
        // session has no IN/OUT/ACK, so RTT/Send State/Sizes would just
        // be blank axes) are hidden rather than rendered empty.
        let has_data = pv.plot_has_data();
        let vis_order = session_visible_order(&pv.plot_layout, has_data);
        let mut show_plot = [false; 5];
        for &i in &vis_order {
            show_plot[i] = true;
        }
        let catalog_shown = session_catalog_shown(
            &pv.plot_layout,
            has_data,
            SessionCatalogData {
                has_http: pv.has_http_req_data(),
                has_http_size: pv.has_http_resp_size_data(),
                has_http_latency: pv.has_http_latency_data(),
                has_cpu: pv.cpu_panel.is_some(),
                has_if,
            },
        );
        let (pinned_ids, rest_ids) = pv.plot_layout.split_pinned(&catalog_shown);
        // Every visible time-series plot shows its own x-axis ticks, so the time
        // scale is always directly below whichever plot you are reading — no need to
        // scroll past the CDF (index 4, which has its own "Percentile" x-axis) to find
        // the time axis. The "Time (seconds)" caption is drawn only on the bottom-most
        // visible time-series plot to avoid visual clutter.
        let session_t0_us = pv.summary.start_ts_us;
        // Trace-relative: pv.time_axis.view_x is in seconds since trace start
        // (the new time-axis convention). To turn it into the
        // absolute capture-us range the strip wants, add
        // capture_start_us — NOT session_t0_us. The previous
        // session-local conversion is what made the strip’s
        // x-axis disagree with the panels’ after a panel zoom.
        let conv_origin_us = self
            .file_info
            .as_ref()
            .map(|fi| fi.capture_start)
            .unwrap_or(session_t0_us);
        let current_view_us = pv.time_axis.view_x.map(|[lo, hi]| {
            let lo_us = conv_origin_us.saturating_add((lo.max(0.0) * 1_000_000.0).round() as u64);
            let hi_us = conv_origin_us.saturating_add((hi.max(0.0) * 1_000_000.0).round() as u64);
            (lo_us, hi_us)
        });
        // Default view (when no zoom is active) covers the whole
        // session, expressed in absolute capture us. Translates to
        // [0, duration] in host-relative seconds inside the strip,
        // matching the panels’ default x-range.
        let session_end_ts_us = pv.summary.end_ts_us;
        let default_view_us = (session_t0_us, session_end_ts_us.max(session_t0_us + 1));
        let session_view_us = current_view_us.unwrap_or(default_view_us);
        // Trace-relative: the strip’s x=0 means trace start (same
        // as the panels’ x=0 now that PlotViewState is
        // trace-relative). Pass trace_t0_us as host_t0_us so the
        // strip renders pixel-aligned with the panels below.
        let trace_t0_us = self
            .file_info
            .as_ref()
            .map(|fi| fi.capture_start)
            .unwrap_or(session_t0_us);
        let utc_origin =
            utc_axis_origin(self.utc_time_labels, self.file_info.as_ref(), trace_t0_us);

        let bottom_ts_plot = catalog_shown
            .iter()
            .copied()
            .rev()
            .find(|id| id.is_session_time_series());

        // Time-series plots (indices 0..4); skip the CDF (index 4).
        for (i, id) in ids[..4].iter().enumerate() {
            if show_plot[i] {
                pv.apply_view_x(ctx, id);
            }
        }
        if catalog_shown.contains(&PlotId::HttpReqLanes) {
            pv.apply_view_x(ctx, &format!("http_req_plot_{session_id}"));
        }
        if catalog_shown.contains(&PlotId::HttpRespSize) {
            pv.apply_view_x(ctx, &format!("http_resp_size_plot_{session_id}"));
        }
        if catalog_shown.contains(&PlotId::HttpRespLatency) {
            pv.apply_view_x(ctx, &format!("http_latency_plot_{session_id}"));
        }
        if catalog_shown
            .iter()
            .any(|id| matches!(id, PlotId::NetworkTx | PlotId::NetworkRx))
        {
            for suffix in [
                "if_tx_plot",
                "if_rx_plot",
                "if_single_tx_plot",
                "if_single_rx_plot",
            ] {
                pv.apply_view_x(ctx, &format!("s{session_id}_{suffix}"));
            }
        }

        // RTT and Rate follow the Time buckets slider. Send State and
        // Sizes stay on the point-count downsample.
        let view_range = pv.time_axis.view_x.unwrap_or([pv.x_min, pv.x_max]);
        let nbins = self.plot_bins;
        let ds_rtt = bucket_series(&pv.raw_rtt, view_range, nbins);
        let ds_srtt = bucket_series(&pv.raw_srtt, view_range, nbins);
        let ds_path_rtt = bucket_series(&pv.raw_path_rtt, view_range, nbins);
        let ds_min_rtt = bucket_series(&pv.raw_min_rtt, view_range, nbins);
        let ds_gp_filter_min = bucket_series(&pv.raw_gp_filter_min, view_range, nbins);
        let ds_gp_srtt = bucket_series(&pv.raw_gp_srtt, view_range, nbins);
        let ds_goodput = bucket_series(&pv.raw_goodput, view_range, nbins);
        let ds_pacing = bucket_series(&pv.raw_pacing, view_range, nbins);
        let ds_ack_rate = bucket_series(&pv.raw_ack_rate, view_range, nbins);
        let ds_cwnd = prepare_series(&pv.raw_cwnd, view_range, MAX_DISPLAY_POINTS);
        let ds_ssthresh = prepare_series(&pv.raw_ssthresh, view_range, MAX_DISPLAY_POINTS);
        let ds_inflight = prepare_series(&pv.raw_inflight, view_range, MAX_DISPLAY_POINTS);
        let ds_peer_rwnd = prepare_series(&pv.raw_peer_rwnd, view_range, MAX_DISPLAY_POINTS);
        let ds_sndbuf = prepare_series(&pv.raw_sndbuf, view_range, MAX_DISPLAY_POINTS);
        let ds_sacked = prepare_series(&pv.raw_sacked, view_range, MAX_DISPLAY_POINTS);
        let ds_send_size = downsample_raw(&pv.raw_send_size, view_range, MAX_DISPLAY_POINTS);
        let ds_rtx_size = downsample_raw(&pv.raw_retx_size, view_range, MAX_DISPLAY_POINTS);
        let ds_ack_size = downsample_raw(&pv.raw_ack_size, view_range, MAX_DISPLAY_POINTS);

        // Build CDFs from data in the current view window
        let cdf_from_range = |raw: &[[f64; 2]], range: [f64; 2]| -> Vec<[f64; 2]> {
            let mut vals: Vec<f64> = raw
                .iter()
                .filter(|p| p[0] >= range[0] && p[0] <= range[1])
                .map(|p| p[1])
                .collect();
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
            make_cdf(&vals)
        };
        // The Send Size CDF describes the size distribution of *every*
        // kernel OUT — a retx is still a send, just labelled red in
        // the time series. Merge both scatter sources so the CDF stays
        // faithful to "distribution of OUT lengths".
        let send_size_cdf = {
            let mut vals: Vec<f64> = pv
                .raw_send_size
                .iter()
                .chain(pv.raw_retx_size.iter())
                .filter(|p| p[0] >= view_range[0] && p[0] <= view_range[1])
                .map(|p| p[1])
                .collect();
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
            make_cdf(&vals)
        };
        let ack_size_cdf = cdf_from_range(&pv.raw_ack_size, view_range);

        // Filter recovery/goodput spans to the visible x range for manual drawing later
        let visible_recovery: Vec<[f64; 2]> = if pv.show_recovery_spans {
            pv.recovery_spans
                .iter()
                .filter(|&&[t_enter, t_exit]| t_exit >= view_range[0] && t_enter <= view_range[1])
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        let visible_goodput: Vec<[f64; 2]> = if pv.show_goodput_spans {
            pv.goodput_spans
                .iter()
                .filter(|&&[t_start, t_end]| t_end >= view_range[0] && t_start <= view_range[1])
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        let visible_pacing_caps: Vec<PacingCapSpan> = if pv.show_pacing_cap {
            pv.pacing_cap_spans
                .iter()
                .filter(|s| s.t_end >= view_range[0] && s.t_start <= view_range[1])
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        let visible_limiter: Vec<LimiterSpan> = if pv.show_limiter {
            pv.limiter_spans
                .iter()
                .filter(|s| s.t_end >= view_range[0] && s.t_start <= view_range[1])
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        let visible_rto: Vec<(f64, String)> = if pv.show_rto {
            pv.rto_markers
                .iter()
                .filter(|(t, _)| *t >= view_range[0] && *t <= view_range[1])
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let visible_window_collapse: Vec<(f64, String)> = if pv.show_window_collapse {
            pv.window_collapse_markers
                .iter()
                .filter(|(t, _)| *t >= view_range[0] && *t <= view_range[1])
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let visible_dsack: Vec<(f64, String)> = if pv.show_dsack {
            pv.dsack_markers
                .iter()
                .filter(|(t, _)| *t >= view_range[0] && *t <= view_range[1])
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        let rtt_y_min_fold = [
            &ds_rtt,
            &ds_srtt,
            &ds_path_rtt,
            &ds_min_rtt,
            &ds_gp_filter_min,
            &ds_gp_srtt,
        ]
        .iter()
        .flat_map(|s| s.iter().map(|p| p[1]))
        .fold(f64::INFINITY, f64::min);
        let rtt_y_min = if rtt_y_min_fold.is_finite() {
            Some((rtt_y_min_fold * 0.95).max(0.0))
        } else {
            None
        };

        for i in 0..5 {
            if !show_plot[i] {
                continue;
            }
            if let Some(ymax) = pv.y_max_override[i] {
                pv.apply_y_max_bounds(ctx, &ids[i], ymax);
            }
        }

        let hovered_gp_span: std::cell::Cell<Option<Option<usize>>> = std::cell::Cell::new(None);
        let paint_gp_spans =
            |resp: &egui_plot::PlotResponse<()>, spans: &[[f64; 2]], ui: &egui::Ui| {
                let plot_rect = resp.response.rect;
                let hovered_span = match hovered_gp_span.get() {
                    Some(v) => v,
                    None => {
                        let hover_pos = ui.ctx().input(|i| i.pointer.hover_pos());
                        let val = hover_pos
                            .filter(|p| p.x >= plot_rect.left() && p.x <= plot_rect.right())
                            .and_then(|pos| {
                                let data_x = resp.transform.value_from_position(pos).x;
                                spans.iter().position(|&[s, e]| data_x >= s && data_x <= e)
                            });
                        hovered_gp_span.set(Some(val));
                        val
                    }
                };
                let painter = ui.painter_at(plot_rect);
                for (i, &[t_start, t_end]) in spans.iter().enumerate() {
                    let x_left = resp.transform.position_from_point_x(t_start);
                    let x_right = resp.transform.position_from_point_x(t_end);
                    let (fill, stroke) = if hovered_span == Some(i) {
                        (COLOR_GP_SPAN_FILL_HOVER, COLOR_GP_SPAN_STROKE_HOVER)
                    } else {
                        (COLOR_GP_SPAN_FILL, COLOR_GP_SPAN_STROKE)
                    };
                    let shading = egui::Rect::from_x_y_ranges(
                        x_left..=x_right,
                        plot_rect.top()..=plot_rect.bottom(),
                    );
                    painter.rect_filled(shading, 0.0, fill);
                    painter.vline(
                        x_left,
                        plot_rect.top()..=plot_rect.bottom(),
                        Stroke::new(0.5_f32, stroke),
                    );
                    painter.vline(
                        x_right,
                        plot_rect.top()..=plot_rect.bottom(),
                        Stroke::new(0.5_f32, stroke),
                    );
                }
            };

        // Paint per-cap translucent spans behind a plot.  Fill is kept
        // extremely faint so the plot content remains fully readable — the
        // shading is meant to be a barely-there tint that tells you "a cap
        // was active here" without obscuring any series.  The edge strokes
        // are likewise very soft; colour varies with cap_source.
        let paint_pacing_caps =
            |resp: &egui_plot::PlotResponse<()>, spans: &[PacingCapSpan], ui: &egui::Ui| {
                let plot_rect = resp.response.rect;
                let painter = ui.painter_at(plot_rect);
                for s in spans {
                    if s.t_end <= s.t_start {
                        continue;
                    }
                    let (fill, stroke, _legend) = pacing_cap_colors(s.cap);
                    let x_left = resp.transform.position_from_point_x(s.t_start);
                    let x_right = resp.transform.position_from_point_x(s.t_end);
                    let shading = egui::Rect::from_x_y_ranges(
                        x_left..=x_right,
                        plot_rect.top()..=plot_rect.bottom(),
                    );
                    painter.rect_filled(shading, 0.0, fill);
                    painter.vline(
                        x_left,
                        plot_rect.top()..=plot_rect.bottom(),
                        Stroke::new(0.5_f32, stroke),
                    );
                    painter.vline(
                        x_right,
                        plot_rect.top()..=plot_rect.bottom(),
                        Stroke::new(0.5_f32, stroke),
                    );
                }
            };

        // Limiter overlay: behind everything else, no strokes — the
        // classification is per-bucket and we don't want to draw 4000
        // hairlines for a thrashing session.
        let paint_limiter =
            |resp: &egui_plot::PlotResponse<()>, spans: &[LimiterSpan], ui: &egui::Ui| {
                let plot_rect = resp.response.rect;
                let painter = ui.painter_at(plot_rect);
                for s in spans {
                    if s.t_end <= s.t_start {
                        continue;
                    }
                    // Truly idle spans are left un-shaded — the
                    // idle_spans overlay handles long idle periods.
                    // Unknown (mystery gap) spans ARE shaded so they're
                    // visible against the dark background.
                    if matches!(s.class, Limiter::Idle) {
                        continue;
                    }
                    let (fill, _stroke, _legend) = limiter_colors(s.class);
                    let x_left = resp.transform.position_from_point_x(s.t_start);
                    let x_right = resp.transform.position_from_point_x(s.t_end);
                    let shading = egui::Rect::from_x_y_ranges(
                        x_left..=x_right,
                        plot_rect.top()..=plot_rect.bottom(),
                    );
                    painter.rect_filled(shading, 0.0, fill);
                }
            };

        // Bottom-most time-linked panel gets the axis caption; every
        // panel (bottom or not) gets UTC tick labels when the
        // toggle is on, since each stacked panel prints its own
        // x ticks (see the "Time (seconds)" comment above).
        let apply_time_axis = |mut plot: Plot, is_bottom: bool| -> Plot {
            let mut axis = agg_x_time_axis(utc_origin);
            if is_bottom {
                axis = axis.label(if utc_origin.is_some() {
                    "Time (UTC)"
                } else {
                    "Time (seconds)"
                });
            }
            plot = plot.custom_x_axes(vec![axis]);
            if utc_origin.is_some() {
                plot = plot.x_grid_spacer(utc_x_grid_spacer);
            }
            plot
        };

        // Session plots follow the applied catalog order.
        let legend_init = self.open_sessions[session_idx].needs_legend_init;
        let mut rtt_resp = None;
        let mut rate_resp = None;
        let mut cwnd_resp = None;
        let mut sizes_resp = None;
        let mut size_cdf_resp = None;
        let mut http_req_resp = None;
        let mut http_resp_size_resp = None;
        let mut http_latency_resp = None;
        let mut if_cursor = SysCursor::new(ui, utc_origin, self.plot_bins);
        if_cursor.show_legends = self.show_legends;
        let if_prefix = format!("s{session_id}_");
        let sess_seed = {
            let pv = &self.open_sessions[session_idx];
            pv.time_axis.seed(pv.x_min, pv.x_max)
        };
        macro_rules! draw_session_ids {
            ($plot_ui:ident, $ids:expr) => {{
                let ui = &mut *$plot_ui;
                for (_k, &plot_id) in $ids.iter().enumerate() {
                    if_cursor.last_plot_rect = None;
                    if_cursor.fill_remaining = self.open_sessions[session_idx]
                        .plot_layout
                        .fills_catalog_row(plot_id);
                    if_cursor.panel_h = self.open_sessions[session_idx]
                        .plot_layout
                        .draw_height(plot_id, ui.available_height());
                    if self.pending_scroll_plot == Some(plot_id) {
                        ui.scroll_to_cursor(Some(egui::Align::Center));
                        self.pending_scroll_plot = None;
                    }
                    if matches!(
                        plot_id,
                        PlotId::Rtt
                            | PlotId::Rate
                            | PlotId::SendState
                            | PlotId::Sizes
                            | PlotId::SizeCdf
                            | PlotId::HttpReqLanes
                            | PlotId::HttpRespSize
                            | PlotId::HttpRespLatency
                    ) {
                        plot_section_title(ui, plot_id.label());
                    }
                    match plot_id {
                        PlotId::Rtt => {
                            let pv = &mut self.open_sessions[session_idx];
                            rtt_resp = {
                                let is_bottom = bottom_ts_plot == Some(PlotId::Rtt);
                                let mut plot = pv.make_plot(
                                    &ids[0],
                                    "RTT (ms)",
                                    if_cursor.plot_h(ui),
                                    true,
                                    rtt_y_min,
                                    pv.y_max_override[0],
                                );
                                plot = apply_time_axis(plot, is_bottom);
                                let r = plot.show(ui, |plot_ui| {
                                    // egui_plot renders legend entries bottom-up, so the
                                    // series are inserted in *reverse* alphabetical order
                                    // (case-insensitive) to produce a top-down alphabetical
                                    // legend. Keep this list sorted Z→A.
                                    for (data, color, width, name, default_on) in [
                                        (
                                            ds_srtt.as_slice(),
                                            COLOR_SRTT,
                                            0.8_f32,
                                            "Smoothed RTT",
                                            true,
                                        ),
                                        (
                                            ds_rtt.as_slice(),
                                            COLOR_RAW_RTT,
                                            0.8_f32,
                                            "Raw RTT",
                                            false,
                                        ),
                                        (
                                            ds_path_rtt.as_slice(),
                                            COLOR_PATH_RTT,
                                            0.8_f32,
                                            "Path RTT",
                                            false,
                                        ),
                                        (
                                            ds_gp_srtt.as_slice(),
                                            COLOR_GP_SRTT,
                                            0.8_f32,
                                            "Goodput Smoothed RTT",
                                            false,
                                        ),
                                        (
                                            ds_gp_filter_min.as_slice(),
                                            COLOR_GP_FILTER_MIN,
                                            0.8_f32,
                                            "Goodput Min RTT",
                                            false,
                                        ),
                                        (
                                            ds_min_rtt.as_slice(),
                                            COLOR_MIN_RTT,
                                            1.0_f32,
                                            "All-Time Min RTT",
                                            true,
                                        ),
                                    ] {
                                        if !data.is_empty() && (default_on || !legend_init) {
                                            plot_ui.line(
                                                Line::new(PlotPoints::new(data.to_vec()))
                                                    .color(color)
                                                    .width(width)
                                                    .name(name),
                                            );
                                        }
                                    }
                                });
                                paint_limiter(&r, &visible_limiter, ui);
                                paint_pacing_caps(&r, &visible_pacing_caps, ui);
                                paint_gp_spans(&r, &visible_goodput, ui);
                                Some(r)
                            };
                        }
                        PlotId::Rate => {
                            let pv = &mut self.open_sessions[session_idx];
                            rate_resp = {
                                let is_bottom = bottom_ts_plot == Some(PlotId::Rate);
                                let mut plot = pv.make_plot(
                                    &ids[1],
                                    "Rate (Mbps)",
                                    if_cursor.plot_h(ui),
                                    true,
                                    None,
                                    pv.y_max_override[1],
                                );
                                plot = apply_time_axis(plot, is_bottom);
                                let r = plot.show(ui, |plot_ui| {
                                    // egui_plot renders legend entries bottom-up; keep
                                    // this list sorted Z→A so the legend reads A→Z.
                                    for (data, color, width, name, default_on) in [
                                        (
                                            ds_pacing.as_slice(),
                                            COLOR_PACING_RATE,
                                            1.2_f32,
                                            "Pacing Rate",
                                            true,
                                        ),
                                        (
                                            ds_goodput.as_slice(),
                                            COLOR_KERNEL_GOODPUT,
                                            2.0_f32,
                                            "Kernel Goodput",
                                            true,
                                        ),
                                        (
                                            ds_ack_rate.as_slice(),
                                            COLOR_ACK_RATE,
                                            1.2_f32,
                                            "ACK Rate",
                                            false,
                                        ),
                                    ] {
                                        if !data.is_empty() && (default_on || !legend_init) {
                                            plot_ui.line(
                                                Line::new(PlotPoints::new(data.to_vec()))
                                                    .color(color)
                                                    .width(width)
                                                    .name(name),
                                            );
                                        }
                                    }
                                });
                                paint_limiter(&r, &visible_limiter, ui);
                                paint_pacing_caps(&r, &visible_pacing_caps, ui);
                                paint_gp_spans(&r, &visible_goodput, ui);
                                Some(r)
                            };
                        }
                        PlotId::SendState => {
                            let pv = &mut self.open_sessions[session_idx];
                            cwnd_resp = {
                                let is_bottom = bottom_ts_plot == Some(PlotId::SendState);
                                let mut plot = pv.make_plot(
                                    &ids[2],
                                    "Send State (KB)",
                                    if_cursor.plot_h(ui),
                                    true,
                                    Some(0.0),
                                    pv.y_max_override[2],
                                );
                                plot = apply_time_axis(plot, is_bottom);
                                let r = plot.show(ui, |plot_ui| {
                                    // egui_plot renders legend entries bottom-up; keep
                                    // this list sorted Z→A (case-insensitive) so the
                                    // legend reads A→Z.
                                    for (data, color, width, name, default_on) in [
                                        (
                                            ds_ssthresh.as_slice(),
                                            COLOR_SSTHRESH,
                                            0.8_f32,
                                            "ssthresh",
                                            false,
                                        ),
                                        (
                                            ds_sndbuf.as_slice(),
                                            COLOR_SNDBUF,
                                            0.8_f32,
                                            "sndbuf",
                                            false,
                                        ),
                                        (
                                            ds_peer_rwnd.as_slice(),
                                            COLOR_PEER_RWND,
                                            0.8_f32,
                                            "rwnd",
                                            false,
                                        ),
                                        (
                                            ds_inflight.as_slice(),
                                            COLOR_INFLIGHT,
                                            0.6_f32,
                                            "inflight",
                                            true,
                                        ),
                                        (ds_cwnd.as_slice(), COLOR_CWND, 1.0_f32, "cwnd", true),
                                    ] {
                                        if !data.is_empty() && (default_on || !legend_init) {
                                            plot_ui.line(
                                                Line::new(PlotPoints::new(data.to_vec()))
                                                    .color(color)
                                                    .width(width)
                                                    .name(name),
                                            );
                                        }
                                    }
                                });
                                paint_limiter(&r, &visible_limiter, ui);
                                paint_pacing_caps(&r, &visible_pacing_caps, ui);
                                paint_gp_spans(&r, &visible_goodput, ui);
                                Some(r)
                            };
                        }
                        PlotId::Sizes => {
                            let pv = &mut self.open_sessions[session_idx];
                            sizes_resp = {
                                let is_bottom = bottom_ts_plot == Some(PlotId::Sizes);
                                let mut plot = pv.make_plot(
                                    &ids[3],
                                    "Size (bytes)",
                                    if_cursor.plot_h(ui),
                                    true,
                                    Some(0.0),
                                    pv.y_max_override[3],
                                );
                                plot = apply_time_axis(plot, is_bottom);
                                let r = plot.show(ui, |plot_ui| {
                                    // egui_plot renders legend entries bottom-up; insert
                                    // Z→A so the legend reads A→Z (ACK Size, RTX Size,
                                    // SACKed, Send Size). RTX points are drawn *after*
                                    // Send so the red marker sits on top if they ever
                                    // overlap — though in practice the two sets are
                                    // disjoint (a given OUT is either fresh or a retx,
                                    // never both), so ordering only matters for z-order
                                    // at pixel-coincident downsampled points. SACKed is
                                    // a step line (continuous state), not a scatter, and
                                    // is hidden by default via PlotMemory.hidden_items.
                                    if !ds_send_size.is_empty() {
                                        plot_ui.points(
                                            Points::new(PlotPoints::new(ds_send_size.clone()))
                                                .color(COLOR_SEND_SIZE)
                                                .radius(1.5_f32)
                                                .name("Send Size"),
                                        );
                                    }
                                    if !ds_sacked.is_empty() {
                                        plot_ui.line(
                                            Line::new(PlotPoints::new(ds_sacked.clone()))
                                                .color(COLOR_SACKED)
                                                .width(0.8_f32)
                                                .name("SACKed"),
                                        );
                                    }
                                    if !ds_rtx_size.is_empty() {
                                        plot_ui.points(
                                            Points::new(PlotPoints::new(ds_rtx_size.clone()))
                                                .color(COLOR_RTX_SIZE)
                                                .radius(2.0_f32)
                                                .name("RTX Size"),
                                        );
                                    }
                                    if !ds_ack_size.is_empty() {
                                        plot_ui.points(
                                            Points::new(PlotPoints::new(ds_ack_size.clone()))
                                                .color(COLOR_ACK_SIZE)
                                                .radius(1.5_f32)
                                                .name("ACK Size"),
                                        );
                                    }
                                });
                                paint_limiter(&r, &visible_limiter, ui);
                                paint_pacing_caps(&r, &visible_pacing_caps, ui);
                                paint_gp_spans(&r, &visible_goodput, ui);
                                Some(r)
                            };
                        }
                        PlotId::SizeCdf => {
                            let pv = &mut self.open_sessions[session_idx];
                            size_cdf_resp = {
                                let y_axis = AxisHints::new_y()
                                    .label(RichText::new("Size (Bytes)").size(14.0))
                                    .max_digits(6);
                                let mut plot = Plot::new(&ids[4])
                                    .id(egui::Id::new(ids[4].as_str()))
                                    .height(if_cursor.plot_h(ui))
                                    .y_axis_width(6)
                                    .set_margin_fraction(egui::vec2(0.01, 0.05))
                                    .include_x(0.0)
                                    .include_x(100.0)
                                    .include_y(0.0);
                                if let Some(y) = pv.y_max_override[4] {
                                    plot = plot.include_y(y);
                                }
                                let plot = plot
                                    .y_grid_spacer(|input: egui_plot::GridInput| {
                                        let (lo, hi) = input.bounds;
                                        let range = (hi - lo).abs().max(1e-9);
                                        let step = nice_step(range);
                                        let start = (lo / step).ceil() as i64;
                                        let end = (hi / step).floor() as i64;
                                        (start..=end)
                                            .map(|i| GridMark {
                                                value: i as f64 * step,
                                                step_size: step,
                                            })
                                            .collect()
                                    })
                                    .custom_y_axes(vec![y_axis])
                                    .x_axis_label("Percentile")
                                    .show_axes([true, true])
                                    .show_x(false)
                                    .show_y(false)
                                    .allow_scroll(false)
                                    .allow_drag(false)
                                    .allow_zoom(false)
                                    .allow_boxed_zoom(false)
                                    .allow_double_click_reset(false)
                                    .sense(egui::Sense::hover());
                                let plot = maybe_legend(plot, pv.show_legends);

                                Some(plot.show(ui, |plot_ui| {
                                    // egui_plot renders legend entries bottom-up; insert
                                    // Z→A so the legend reads A→Z.
                                    if !send_size_cdf.is_empty() {
                                        plot_ui.line(
                                            Line::new(PlotPoints::new(send_size_cdf.clone()))
                                                .color(COLOR_SEND_SIZE)
                                                .width(1.5_f32)
                                                .name("Send Size"),
                                        );
                                    }
                                    if !ack_size_cdf.is_empty() {
                                        plot_ui.line(
                                            Line::new(PlotPoints::new(ack_size_cdf.clone()))
                                                .color(COLOR_ACK_SIZE)
                                                .width(1.5_f32)
                                                .name("ACK Size"),
                                        );
                                    }
                                }))
                            };
                        }
                        PlotId::HttpReqLanes => {
                            let pv = &mut self.open_sessions[session_idx];
                            http_req_resp = {
                                let y_lo = -1.5;
                                let y_hi = pv.http_req_lanes_y_hi();
                                let is_bottom = bottom_ts_plot == Some(PlotId::HttpReqLanes);
                                let mut plot = pv.make_plot(
                                    &format!("http_req_plot_{session_id}"),
                                    "Slot",
                                    if_cursor.plot_h(ui),
                                    true,
                                    Some(y_lo),
                                    Some(y_hi),
                                );
                                plot = apply_time_axis(plot, is_bottom);
                                let (x_lo, x_hi, _) = pv.time_axis.seed(pv.x_min, pv.x_max);
                                let spans = pv.http_req_spans.clone();
                                let allocfails = pv.http_req_allocfails.clone();
                                let nginx_spans = pv.nginx_req_spans.clone();
                                let nginx_base = pv.nginx_req_lane_base();
                                Some(plot.show(ui, |plot_ui| {
                                    plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                                        [x_lo, y_lo],
                                        [x_hi, y_hi],
                                    ));
                                    for (outcome, color, name) in [
                                        (HttpReqOutcome::Freed, COLOR_HTTP_REQ_FREED, "Complete"),
                                        (HttpReqOutcome::Stale, COLOR_HTTP_REQ_STALE, "Stale"),
                                        (
                                            HttpReqOutcome::Forcefree,
                                            COLOR_HTTP_REQ_FORCEFREE,
                                            "Force-freed",
                                        ),
                                    ] {
                                        for s in spans.iter().filter(|s| s.outcome == outcome) {
                                            plot_ui.line(
                                                Line::new(PlotPoints::new(vec![
                                                    [s.t_start, s.slot as f64],
                                                    [s.t_end, s.slot as f64],
                                                ]))
                                                .color(color)
                                                .width(5.0_f32)
                                                .name(name),
                                            );
                                        }
                                    }
                                    for s in &nginx_spans {
                                        let y = nginx_base + s.lane as f64;
                                        let (name, color) = nginx_status_legend(s.status);
                                        plot_ui.line(
                                            Line::new(PlotPoints::new(vec![
                                                [s.t_start, y],
                                                [s.t_end.unwrap_or(x_hi), y],
                                            ]))
                                            .color(color)
                                            .width(5.0_f32)
                                            .name(name),
                                        );
                                    }
                                    if !allocfails.is_empty() {
                                        let pts: Vec<[f64; 2]> =
                                            allocfails.iter().map(|&t| [t, -1.0]).collect();
                                        plot_ui.points(
                                            Points::new(PlotPoints::new(pts))
                                                .shape(MarkerShape::Diamond)
                                                .radius(4.0_f32)
                                                .color(COLOR_HTTP_REQ_ALLOCFAIL)
                                                .name("Alloc fail"),
                                        );
                                    }
                                }))
                            };
                        }
                        PlotId::HttpRespSize => {
                            let pv = &mut self.open_sessions[session_idx];
                            http_resp_size_resp = {
                                let is_bottom = bottom_ts_plot == Some(PlotId::HttpRespSize);
                                let mut plot = pv.make_plot(
                                    &format!("http_resp_size_plot_{session_id}"),
                                    "Size (bytes)",
                                    if_cursor.plot_h(ui),
                                    true,
                                    Some(0.0),
                                    None,
                                );
                                plot = apply_time_axis(plot, is_bottom);
                                let spans = pv.nginx_req_spans.clone();
                                Some(plot.show(ui, |plot_ui| {
                                    let mut by_class: Vec<(&'static str, Color32, Vec<[f64; 2]>)> =
                                        Vec::new();
                                    for s in &spans {
                                        let (Some(t_end), Some(bytes)) = (s.t_end, s.bytes_sent)
                                        else {
                                            continue;
                                        };
                                        let (name, color) = nginx_status_legend(s.status);
                                        let entry =
                                            by_class.iter_mut().find(|(n, _, _)| *n == name);
                                        let pt = [t_end, bytes as f64];
                                        match entry {
                                            Some((_, _, pts)) => pts.push(pt),
                                            None => by_class.push((name, color, vec![pt])),
                                        }
                                    }
                                    for (name, color, pts) in by_class.iter().rev() {
                                        plot_ui.points(
                                            Points::new(PlotPoints::new(pts.clone()))
                                                .color(*color)
                                                .radius(3.0_f32)
                                                .name(*name),
                                        );
                                    }
                                }))
                            };
                        }
                        PlotId::HttpRespLatency => {
                            let pv = &mut self.open_sessions[session_idx];
                            http_latency_resp = {
                                let is_bottom = bottom_ts_plot == Some(PlotId::HttpRespLatency);
                                let mut plot = pv.make_plot(
                                    &format!("http_latency_plot_{session_id}"),
                                    "Duration (ms)",
                                    if_cursor.plot_h(ui),
                                    true,
                                    Some(0.0),
                                    None,
                                );
                                plot = apply_time_axis(plot, is_bottom);
                                let spans = pv.nginx_req_spans.clone();
                                Some(plot.show(ui, |plot_ui| {
                                    let mut by_class: Vec<(&'static str, Color32, Vec<[f64; 2]>)> =
                                        Vec::new();
                                    for s in &spans {
                                        let Some(t_end) = s.t_end else {
                                            continue;
                                        };
                                        let ms = (t_end - s.t_start).max(0.0) * 1000.0;
                                        let (name, color) = nginx_status_legend(s.status);
                                        let entry =
                                            by_class.iter_mut().find(|(n, _, _)| *n == name);
                                        let pt = [t_end, ms];
                                        match entry {
                                            Some((_, _, pts)) => pts.push(pt),
                                            None => by_class.push((name, color, vec![pt])),
                                        }
                                    }
                                    for (name, color, pts) in by_class.iter().rev() {
                                        plot_ui.points(
                                            Points::new(PlotPoints::new(pts.clone()))
                                                .color(*color)
                                                .radius(3.0_f32)
                                                .name(*name),
                                        );
                                    }
                                }))
                            };
                        }
                        PlotId::CpuUsage => {
                            let cpus = self.current_function_filter().cpus;
                            let mut scale_cpu_by_freq = self.scale_cpu_by_freq;
                            let mut cpu_zoom_session = self.cpu_zoom_session;
                            if let Some(panel) = self.open_sessions[session_idx].cpu_panel.as_mut()
                            {
                                panel.show_header(
                                    ui,
                                    session_id,
                                    &mut scale_cpu_by_freq,
                                    &mut cpu_zoom_session,
                                );
                            }
                            self.scale_cpu_by_freq = scale_cpu_by_freq;
                            self.cpu_zoom_session = cpu_zoom_session;
                            let change = if let Some(panel) =
                                self.open_sessions[session_idx].cpu_panel.as_mut()
                            {
                                panel.scale_by_freq = self.scale_cpu_by_freq;
                                panel.fill_remaining = if_cursor.fill_remaining;
                                panel.set_plot_height(if_cursor.plot_h(ui));
                                panel.show_legends = self.show_legends;
                                panel.set_plot_ids(&format!("cpu_usage_strip_{session_id}"));
                                panel.sync_bins(self.plot_bins, &cpus);
                                Some(panel.draw(
                                    ui,
                                    trace_t0_us,
                                    current_view_us,
                                    if cpu_zoom_session {
                                        Some(session_view_us)
                                    } else {
                                        None
                                    },
                                    utc_origin,
                                    !cpu_zoom_session,
                                ))
                            } else {
                                None
                            };
                            if !cpu_zoom_session {
                                if let Some(change) = change.flatten() {
                                    let axis = &mut self.open_sessions[session_idx].time_axis;
                                    match change {
                                        SelectionChange::Set(lo, hi) => {
                                            axis.set_window_us(trace_t0_us, lo, hi);
                                        }
                                        SelectionChange::Clear => axis.clear_zoom(),
                                    }
                                }
                            }
                        }
                        PlotId::NetworkTx => self.interfaces_section_ex(
                            ui,
                            &mut if_cursor,
                            IfCatalogPanel::Tx,
                            Some(sess_seed),
                            &if_prefix,
                        ),
                        PlotId::NetworkRx => self.interfaces_section_ex(
                            ui,
                            &mut if_cursor,
                            IfCatalogPanel::Rx,
                            Some(sess_seed),
                            &if_prefix,
                        ),
                        _ => {}
                    }
                    let rect = match plot_id {
                        PlotId::Rtt => rtt_resp.as_ref().map(|r| r.response.rect),
                        PlotId::Rate => rate_resp.as_ref().map(|r| r.response.rect),
                        PlotId::SendState => cwnd_resp.as_ref().map(|r| r.response.rect),
                        PlotId::Sizes => sizes_resp.as_ref().map(|r| r.response.rect),
                        PlotId::SizeCdf => size_cdf_resp.as_ref().map(|r| r.response.rect),
                        PlotId::HttpReqLanes => http_req_resp.as_ref().map(|r| r.response.rect),
                        PlotId::HttpRespSize => {
                            http_resp_size_resp.as_ref().map(|r| r.response.rect)
                        }
                        PlotId::HttpRespLatency => {
                            http_latency_resp.as_ref().map(|r| r.response.rect)
                        }
                        PlotId::CpuUsage => self.open_sessions[session_idx]
                            .cpu_panel
                            .as_ref()
                            .and_then(|p| p.last_rect_transform())
                            .map(|(r, _)| r),
                        PlotId::NetworkTx | PlotId::NetworkRx => if_cursor.last_plot_rect,
                        _ => None,
                    };
                    let pv = &mut self.open_sessions[session_idx];
                    finish_catalog_plot(ui, &mut pv.plot_layout, &mut pv.plot_draft, plot_id, rect);
                }
            }};
        }

        draw_session_ids!(ui, &pinned_ids);
        egui::ScrollArea::vertical()
            .id_source(session_id)
            .auto_shrink(false)
            .show(ui, |ui| {
                if catalog_shown.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.weak(
                            "No per-packet TCP events (IN/OUT/ACK/RTO) to plot for this session.",
                        );
                    });
                }
                draw_session_ids!(ui, &rest_ids);

                let pv = &mut self.open_sessions[session_idx];
                if pv.needs_legend_init {
                    pv.needs_legend_init = false;
                    let rtt_id = egui::Id::new(ids[0].as_str());
                    if let Some(mut mem) = PlotMemory::load(ctx, rtt_id) {
                        for name in [
                            "Raw RTT",
                            "Path RTT",
                            "Goodput Min RTT",
                            "Goodput Smoothed RTT",
                        ] {
                            mem.hidden_items.insert(name.to_owned());
                        }
                        mem.store(ctx, rtt_id);
                    }
                    let rate_id = egui::Id::new(ids[1].as_str());
                    if let Some(mut mem) = PlotMemory::load(ctx, rate_id) {
                        mem.hidden_items.insert("ACK Rate".to_owned());
                        mem.store(ctx, rate_id);
                    }
                    let cwnd_id = egui::Id::new(ids[2].as_str());
                    if let Some(mut mem) = PlotMemory::load(ctx, cwnd_id) {
                        for name in ["ssthresh", "sndbuf", "rwnd"] {
                            mem.hidden_items.insert(name.to_owned());
                        }
                        // ^^ Harmless without the corresponding series, but kept
                        //    near the render site so re-enabling is a single flip.
                        mem.store(ctx, cwnd_id);
                    }
                    let sizes_id = egui::Id::new(ids[3].as_str());
                    if let Some(mut mem) = PlotMemory::load(ctx, sizes_id) {
                        mem.hidden_items.insert("SACKed".to_owned());
                        mem.store(ctx, sizes_id);
                    }
                    ctx.request_repaint();
                }
            }); // end session ScrollArea

        let session_cpu_zoomed = self.cpu_zoom_session;
        let pv = &mut self.open_sessions[session_idx];
        let pointer_pos = ui.input(|i| i.pointer.latest_pos());

        const Y_AXIS_HIT_W: f32 = 88.0;
        let y_axis_left_strip = |plot_rect: egui::Rect| {
            egui::Rect::from_x_y_ranges(
                (plot_rect.left() - Y_AXIS_HIT_W)..=plot_rect.left(),
                plot_rect.top()..=plot_rect.bottom(),
            )
        };
        let mut y_axis_hit_regions: Vec<(usize, egui::Rect)> = Vec::new();
        if let Some(ref r) = rtt_resp {
            y_axis_hit_regions.push((0, y_axis_left_strip(r.response.rect)));
        }
        if let Some(ref r) = rate_resp {
            y_axis_hit_regions.push((1, y_axis_left_strip(r.response.rect)));
        }
        if let Some(ref r) = cwnd_resp {
            y_axis_hit_regions.push((2, y_axis_left_strip(r.response.rect)));
        }
        if let Some(ref r) = sizes_resp {
            y_axis_hit_regions.push((3, y_axis_left_strip(r.response.rect)));
        }
        if let Some(ref r) = size_cdf_resp {
            y_axis_hit_regions.push((4, y_axis_left_strip(r.response.rect)));
        }

        if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary)) {
            if let Some(pos) = pointer_pos {
                for &(plot_idx, rect) in &y_axis_hit_regions {
                    if rect.contains(pos) {
                        let plot_id = egui::Id::new(ids[plot_idx].as_str());
                        let draft = PlotMemory::load(ctx, plot_id)
                            .filter(|m| m.bounds().max()[1].is_finite())
                            .map(|m| {
                                let v = m.bounds().max()[1];
                                format!("{v}")
                            })
                            .unwrap_or_default();
                        pv.y_max_editor = Some(YAxisMaxEditor {
                            plot_idx,
                            draft,
                            anchor: pos,
                        });
                        break;
                    }
                }
            }
        }

        // --- Collect visible time-series plot info ---
        let ts_transform = rate_resp
            .as_ref()
            .map(|r| r.transform)
            .or_else(|| rtt_resp.as_ref().map(|r| r.transform))
            .or_else(|| cwnd_resp.as_ref().map(|r| r.transform))
            .or_else(|| sizes_resp.as_ref().map(|r| r.transform))
            .or_else(|| http_req_resp.as_ref().map(|r| r.transform))
            .or_else(|| http_resp_size_resp.as_ref().map(|r| r.transform))
            .or_else(|| http_latency_resp.as_ref().map(|r| r.transform))
            .or_else(|| if_cursor.geoms().first().map(|(_, t)| *t));

        let clip = ui.clip_rect();
        let mut all_plots = Vec::new();
        if let Some(ref r) = rtt_resp {
            all_plots.push((r.response.rect, r.transform));
        }
        if let Some(ref r) = rate_resp {
            all_plots.push((r.response.rect, r.transform));
        }
        if let Some(ref r) = cwnd_resp {
            all_plots.push((r.response.rect, r.transform));
        }
        if let Some(ref r) = sizes_resp {
            all_plots.push((r.response.rect, r.transform));
        }
        if let Some(ref r) = http_req_resp {
            all_plots.push((r.response.rect, r.transform));
        }
        if let Some(ref r) = http_resp_size_resp {
            all_plots.push((r.response.rect, r.transform));
        }
        if let Some(ref r) = http_latency_resp {
            all_plots.push((r.response.rect, r.transform));
        }
        for &(r, t) in if_cursor.geoms() {
            all_plots.push((r, t));
        }
        let all_plot_rects: Vec<egui::Rect> = all_plots
            .iter()
            .filter_map(|(r, _)| {
                let clipped = r.intersect(clip);
                if clipped.is_positive() {
                    Some(clipped)
                } else if r.bottom() <= clip.top() + 8.0 {
                    Some(*r)
                } else {
                    None
                }
            })
            .collect();
        let plots_rect = all_plot_rects.iter().copied().reduce(|a, b| a.union(b));

        // Time-series interaction (only when at least one time-series plot is visible)
        if let (Some(transform), Some(plots_rect)) = (ts_transform, plots_rect) {
            // --- Manual recovery shading (drawn outside plot items to avoid affecting bounds) ---
            for &[t_enter, t_exit] in &visible_recovery {
                for &(plot_rect, plot_tf) in &all_plots {
                    let x_left = plot_tf.position_from_point_x(t_enter);
                    let x_right = plot_tf.position_from_point_x(t_exit);
                    let shading = egui::Rect::from_x_y_ranges(
                        x_left..=x_right,
                        plot_rect.top()..=plot_rect.bottom(),
                    );
                    ui.painter().rect_filled(shading, 0.0, COLOR_RECOVERY_FILL);
                    ui.painter().vline(
                        x_left,
                        plot_rect.top()..=plot_rect.bottom(),
                        Stroke::new(0.5_f32, COLOR_RECOVERY_STROKE),
                    );
                    ui.painter().vline(
                        x_right,
                        plot_rect.top()..=plot_rect.bottom(),
                        Stroke::new(0.5_f32, COLOR_RECOVERY_STROKE),
                    );
                }
            }

            // --- Discrete-event markers: thin semi-transparent vertical
            // lines spanning every time-linked plot, so a marker is visible
            // against an RTT spike, a CWND collapse, and a cluster of
            // RTX Size dots at once. Each stream has its own toggle and its own muted
            // color, and is drawn in order of decreasing visual weight
            // (DSACK → Window Collapse → RTO) so the rarer, more serious
            // events end up on top when they coincide.
            for (markers, color, stroke_w) in [
                (&visible_dsack, COLOR_MARKER_DSACK, 0.6_f32),
                (
                    &visible_window_collapse,
                    COLOR_MARKER_WINDOW_COLLAPSE,
                    0.8_f32,
                ),
                (&visible_rto, COLOR_MARKER_RTO, 0.8_f32),
            ] {
                for (t, _) in markers {
                    for &(plot_rect, plot_tf) in &all_plots {
                        let x = plot_tf.position_from_point_x(*t);
                        if x < plot_rect.left() - 1.0 || x > plot_rect.right() + 1.0 {
                            continue;
                        }
                        ui.painter().vline(
                            x,
                            plot_rect.top()..=plot_rect.bottom(),
                            Stroke::new(stroke_w, color),
                        );
                    }
                }
            }

            // --- Custom interaction ---
            let hovering_if = pointer_pos
                .is_some_and(|pos| if_cursor.geoms().iter().any(|(r, _)| r.contains(pos)));
            let pointer_over_plots = pointer_pos.is_some_and(|pos| {
                all_plot_rects.iter().any(|r| r.contains(pos))
                    || pv
                        .cpu_panel
                        .as_ref()
                        .and_then(|p| p.last_rect_transform())
                        .is_some_and(|(r, _)| r.contains(pos))
                    || hovering_if
            });

            // Shared time-axis gestures (box zoom, middle-click reset,
            // undo, Δt measurement) via the reusable controller — the
            // same one the System tab uses. The clipped panel rects
            // paired with the shared time transform are the gesture
            // surface, plus the CPU Usage plot above this clip. On a
            // reset the panels' cached x auto-bounds are restored via
            // `reset_all_plots`; a middle-click (`hard`) also drops
            // the per-plot y-max overrides.
            let mut geoms: Vec<PanelGeom> =
                all_plot_rects.iter().map(|&r| (r, transform)).collect();
            if session_cpu_zoomed {
                if let Some((r, t)) = pv.cpu_panel.as_ref().and_then(|p| p.last_rect_transform()) {
                    geoms.push((r, t));
                }
            }
            let (x_min, x_max) = (pv.x_min, pv.x_max);
            if let TimeAxisOutcome::Reset { hard } = pv.time_axis.handle(ui, &geoms, x_min, x_max) {
                pv.reset_all_plots(ctx, hard);
                if hard {
                    pv.y_max_override = [None; 5];
                    pv.y_max_editor = None;
                }
            }

            // --- Manual cursor (vertical line + tracking dots + value tooltip) ---
            if hovering_if {
                if_cursor.draw(ui);
            } else if pointer_over_plots {
                if let Some(pos) = pointer_pos {
                    let cursor_x = pos.x;
                    let data_x = transform.value_from_position(pos).x;
                    // Approximate the tick spacing egui_plot would
                    // pick for the current view, so the tooltip's
                    // millisecond cutoff (see `format_axis_utc`)
                    // matches what the axis itself is showing.
                    let tooltip_precision_secs = transform.bounds().width() / 6.0;

                    let cursor_stroke = Stroke::new(1.0_f32, Color32::from_gray(180));
                    ui.painter().line_segment(
                        [
                            egui::pos2(cursor_x, plots_rect.top()),
                            egui::pos2(cursor_x, plots_rect.bottom()),
                        ],
                        cursor_stroke,
                    );

                    let font = egui::FontId::proportional(12.0);
                    let mut tooltip_entries: Vec<(String, Color32)> = Vec::new();
                    let time_label = match utc_origin {
                        Some(origin) => format_tooltip_utc(data_x, origin, tooltip_precision_secs),
                        None => format!("t = {:.4}s", data_x),
                    };
                    tooltip_entries.push((time_label, Color32::from_gray(220)));

                    // Extend the shared cursor across the CPU usage
                    // panel above the panels (whichever mode its own
                    // gear menu has selected). Paint a vline over the
                    // panel rect at the same screen-x as the panel
                    // cursor, paint a tracking dot on the panel’s
                    // data line at data_x, and push a tooltip entry.
                    // The panel is already pixel-aligned with the
                    // panels (y_axis_width matches make_plot), so the
                    // same cursor_x is the correct screen-x there too.
                    let strip_rt = pv.cpu_panel.as_ref().and_then(|p| p.last_rect_transform());
                    if let Some((strip_rect, strip_tf)) = strip_rt {
                        let strip_painter = ui.ctx().layer_painter(egui::LayerId::new(
                            egui::Order::Foreground,
                            egui::Id::new(("session_strip_cursor_overlay", session_id)),
                        ));
                        strip_painter.line_segment(
                            [
                                egui::pos2(cursor_x, strip_rect.top()),
                                egui::pos2(cursor_x, strip_rect.bottom()),
                            ],
                            cursor_stroke,
                        );
                        if let Some(p) = pv.cpu_panel.as_ref() {
                            for (label, color, value) in p.cursor_series(data_x) {
                                if !label.starts_with("Total:") {
                                    let dot = strip_tf.position_from_point(
                                        &egui_plot::PlotPoint::new(data_x, value),
                                    );
                                    let dot_pos = egui::pos2(cursor_x, dot.y);
                                    if strip_rect.contains(dot_pos) {
                                        strip_painter.circle_filled(
                                            dot_pos,
                                            4.5,
                                            Color32::from_gray(20),
                                        );
                                        strip_painter.circle_filled(dot_pos, 3.0, color);
                                    }
                                }
                                tooltip_entries.push((label, color));
                            }
                        }
                    }

                    let hidden = |plot_id: &str| {
                        PlotMemory::load(ctx, egui::Id::new(plot_id))
                            .map(|m| m.hidden_items)
                            .unwrap_or_default()
                    };
                    let rtt_hidden = hidden(&ids[0]);
                    let rate_hidden = hidden(&ids[1]);
                    let cwnd_hidden = hidden(&ids[2]);
                    let sizes_hidden = hidden(&ids[3]);

                    // RTT panel series  (tooltip_name, legend_name, color, raw)
                    if let Some(ref rtt_r) = rtt_resp {
                        for (tip, legend, color, raw) in [
                            ("Raw RTT", "Raw RTT", COLOR_RAW_RTT, ds_rtt.as_slice()),
                            (
                                "Smoothed RTT",
                                "Smoothed RTT",
                                COLOR_SRTT,
                                ds_srtt.as_slice(),
                            ),
                            (
                                "Path RTT",
                                "Path RTT",
                                COLOR_PATH_RTT,
                                ds_path_rtt.as_slice(),
                            ),
                            (
                                "All-Time Min RTT",
                                "All-Time Min RTT",
                                COLOR_MIN_RTT,
                                ds_min_rtt.as_slice(),
                            ),
                            (
                                "Goodput Min RTT",
                                "Goodput Min RTT",
                                COLOR_GP_FILTER_MIN,
                                ds_gp_filter_min.as_slice(),
                            ),
                            (
                                "Goodput Smoothed RTT",
                                "Goodput Smoothed RTT",
                                COLOR_GP_SRTT,
                                ds_gp_srtt.as_slice(),
                            ),
                        ] {
                            if rtt_hidden.contains(legend) {
                                continue;
                            }
                            if let Some(y) = lookup_step_value(raw, data_x) {
                                let dot_y = rtt_r
                                    .transform
                                    .position_from_point(&PlotPoint { x: data_x, y })
                                    .y;
                                let dot = egui::pos2(cursor_x, dot_y);
                                if rtt_r.response.rect.contains(dot) {
                                    ui.painter().circle_filled(dot, 4.5, Color32::from_gray(20));
                                    ui.painter().circle_filled(dot, 3.0, color);
                                }
                                tooltip_entries.push((format!("{}: {:.2} ms", tip, y), color));
                            }
                        }
                    }

                    // Rate panel series
                    if let Some(ref rate_r) = rate_resp {
                        for (tip, legend, color, raw) in [
                            (
                                "Goodput",
                                "Kernel Goodput",
                                COLOR_KERNEL_GOODPUT,
                                ds_goodput.as_slice(),
                            ),
                            (
                                "Pacing",
                                "Pacing Rate",
                                COLOR_PACING_RATE,
                                ds_pacing.as_slice(),
                            ),
                            (
                                "ACK Rate",
                                "ACK Rate",
                                COLOR_ACK_RATE,
                                ds_ack_rate.as_slice(),
                            ),
                        ] {
                            if rate_hidden.contains(legend) {
                                continue;
                            }
                            if let Some(y) = lookup_step_value(raw, data_x) {
                                let dot_y = rate_r
                                    .transform
                                    .position_from_point(&PlotPoint { x: data_x, y })
                                    .y;
                                let dot = egui::pos2(cursor_x, dot_y);
                                if rate_r.response.rect.contains(dot) {
                                    ui.painter().circle_filled(dot, 4.5, Color32::from_gray(20));
                                    ui.painter().circle_filled(dot, 3.0, color);
                                }
                                tooltip_entries
                                    .push((format!("{}: {}", tip, format_goodput(y)), color));
                            }
                        }
                    }

                    // CWND panel series. Peer RWND comes from
                    // `EVT_IN.win << snd_wscale`; for captures without a
                    // `CONN_PARAMS_V2` event the parser leaves
                    // `raw_peer_rwnd` empty so this row simply never
                    // appears in the tooltip (matching the suppressed
                    // line on the plot itself).
                    if let Some(ref cwnd_r) = cwnd_resp {
                        for (tip, legend, color, raw, suffix) in [
                            ("cwnd", "cwnd", COLOR_CWND, pv.raw_cwnd.as_slice(), ""),
                            (
                                "ssthresh",
                                "ssthresh",
                                COLOR_SSTHRESH,
                                pv.raw_ssthresh.as_slice(),
                                "",
                            ),
                            (
                                "inflight",
                                "inflight",
                                COLOR_INFLIGHT,
                                pv.raw_inflight.as_slice(),
                                "",
                            ),
                            (
                                "rwnd",
                                "rwnd",
                                COLOR_PEER_RWND,
                                pv.raw_peer_rwnd.as_slice(),
                                "",
                            ),
                            (
                                "sndbuf",
                                "sndbuf",
                                COLOR_SNDBUF,
                                pv.raw_sndbuf.as_slice(),
                                "",
                            ),
                        ] {
                            if cwnd_hidden.contains(legend) {
                                continue;
                            }
                            if let Some(y) = lookup_step_value(raw, data_x) {
                                let dot_y = cwnd_r
                                    .transform
                                    .position_from_point(&PlotPoint { x: data_x, y })
                                    .y;
                                let dot = egui::pos2(cursor_x, dot_y);
                                if cwnd_r.response.rect.contains(dot) {
                                    ui.painter().circle_filled(dot, 4.5, Color32::from_gray(20));
                                    ui.painter().circle_filled(dot, 3.0, color);
                                }
                                tooltip_entries
                                    .push((format!("{}: {:.1} KB{}", tip, y, suffix), color));
                            }
                        }
                    }

                    // Sizes panel series (nearest-point lookup for scatter data)
                    if let Some(ref sizes_r) = sizes_resp {
                        let [vx0, vx1] = pv.time_axis.view_x.unwrap_or([pv.x_min, pv.x_max]);
                        let snap_dist = (vx1 - vx0) * 0.005;
                        for (tip, legend, color, raw) in [
                            (
                                "Send",
                                "Send Size",
                                COLOR_SEND_SIZE,
                                pv.raw_send_size.as_slice(),
                            ),
                            (
                                "RTX",
                                "RTX Size",
                                COLOR_RTX_SIZE,
                                pv.raw_retx_size.as_slice(),
                            ),
                            (
                                "ACK",
                                "ACK Size",
                                COLOR_ACK_SIZE,
                                pv.raw_ack_size.as_slice(),
                            ),
                        ] {
                            if sizes_hidden.contains(legend) {
                                continue;
                            }
                            if let Some(y) = lookup_nearest_value(raw, data_x, snap_dist) {
                                let dot_y = sizes_r
                                    .transform
                                    .position_from_point(&PlotPoint { x: data_x, y })
                                    .y;
                                let dot = egui::pos2(cursor_x, dot_y);
                                if sizes_r.response.rect.contains(dot) {
                                    ui.painter().circle_filled(dot, 4.5, Color32::from_gray(20));
                                    ui.painter().circle_filled(dot, 3.0, color);
                                }
                                tooltip_entries.push((format!("{}: {} B", tip, y as u64), color));
                            }
                        }
                        // SACKed is a continuous step line, so use a step
                        // lookup (not nearest-point) — always show it when
                        // visible, even if the cursor isn't on a discrete
                        // ACK event.
                        if !sizes_hidden.contains("SACKed") {
                            if let Some(y) = lookup_step_value(pv.raw_sacked.as_slice(), data_x) {
                                let dot_y = sizes_r
                                    .transform
                                    .position_from_point(&PlotPoint { x: data_x, y })
                                    .y;
                                let dot = egui::pos2(cursor_x, dot_y);
                                if sizes_r.response.rect.contains(dot) {
                                    ui.painter().circle_filled(dot, 4.5, Color32::from_gray(20));
                                    ui.painter().circle_filled(dot, 3.0, COLOR_SACKED);
                                }
                                tooltip_entries
                                    .push((format!("SACKed: {} B", y as u64), COLOR_SACKED));
                            }
                        }
                    }

                    if pv.show_recovery_spans {
                        if let Some(&[t_enter, t_exit]) = visible_recovery
                            .iter()
                            .find(|&&[s, e]| data_x >= s && data_x <= e)
                        {
                            tooltip_entries.push((
                                format!("Recovery: {}", format_time_span(t_exit - t_enter)),
                                COLOR_RECOVERY_LEGEND,
                            ));
                        }
                    }

                    if pv.show_goodput_spans {
                        if let Some(span) = visible_goodput
                            .iter()
                            .find(|&&[s, e]| data_x >= s && data_x <= e)
                        {
                            let active_ms = (span[1] - span[0]) * 1000.0;
                            tooltip_entries.push((
                                format!("Active-Time: {:.2} ms", active_ms),
                                COLOR_GP_SPAN_FILL_HOVER,
                            ));
                        }
                    }

                    if pv.show_pacing_cap {
                        if let Some(span) = visible_pacing_caps
                            .iter()
                            .find(|s| data_x >= s.t_start && data_x <= s.t_end)
                        {
                            let (_fill, _stroke, legend) = pacing_cap_colors(span.cap);
                            tooltip_entries.push((
                                format!("Pacing cap: {}", elog_parser::pacing_cap_name(span.cap)),
                                legend,
                            ));
                        }
                    }

                    if pv.show_limiter {
                        if let Some(span) = visible_limiter
                            .iter()
                            .find(|s| data_x >= s.t_start && data_x <= s.t_end)
                        {
                            if !matches!(span.class, Limiter::Idle) {
                                let (_fill, _stroke, legend) = limiter_colors(span.class);
                                tooltip_entries
                                    .push((format!("Limiter: {}", span.class.short()), legend));
                            }
                        }
                    }

                    // HTTP request lane strip: report every slot whose
                    // lifetime spans the cursor's x (concurrent slots are
                    // all shown), plus a hover dot at (data_x, slot) on
                    // the strip itself.
                    if let Some(ref http_req_r) = http_req_resp {
                        for s in &pv.http_req_spans {
                            if data_x < s.t_start || data_x > s.t_end {
                                continue;
                            }
                            let (color, name) = match s.outcome {
                                HttpReqOutcome::Freed => (COLOR_HTTP_REQ_FREED, "Complete"),
                                HttpReqOutcome::Stale => (COLOR_HTTP_REQ_STALE, "Stale"),
                                HttpReqOutcome::Forcefree => {
                                    (COLOR_HTTP_REQ_FORCEFREE, "Force-freed")
                                }
                            };
                            let dot_y = http_req_r
                                .transform
                                .position_from_point(&PlotPoint {
                                    x: data_x,
                                    y: s.slot as f64,
                                })
                                .y;
                            let dot = egui::pos2(cursor_x, dot_y);
                            if http_req_r.response.rect.contains(dot) {
                                ui.painter().circle_filled(dot, 4.5, Color32::from_gray(20));
                                ui.painter().circle_filled(dot, 3.0, color);
                            }
                            tooltip_entries.push((
                                format!(
                                    "HTTP req slot {}: {} ({})",
                                    s.slot,
                                    name,
                                    format_time_span(s.t_end - s.t_start)
                                ),
                                color,
                            ));
                        }
                        for s in &pv.nginx_req_spans {
                            if data_x < s.t_start || s.t_end.is_some_and(|e| data_x > e) {
                                continue;
                            }
                            let (class, color) = nginx_status_legend(s.status);
                            let state = match s.status {
                                Some(st) => format!("{st} {class}"),
                                None => class.to_string(),
                            };
                            let y = pv.nginx_req_lane_base() + s.lane as f64;
                            let dot_y = http_req_r
                                .transform
                                .position_from_point(&PlotPoint { x: data_x, y })
                                .y;
                            let dot = egui::pos2(cursor_x, dot_y);
                            if http_req_r.response.rect.contains(dot) {
                                ui.painter().circle_filled(dot, 4.5, Color32::from_gray(20));
                                ui.painter().circle_filled(dot, 3.0, color);
                            }
                            let dur = s
                                .t_end
                                .map(|e| format_time_span(e - s.t_start))
                                .unwrap_or_else(|| "open".to_string());
                            let detail = s.detail();
                            let text = if detail.is_empty() {
                                format!("req {}: {} ({})", s.request, state, dur)
                            } else {
                                format!("req {}: {} ({}) · {}", s.request, state, dur, detail)
                            };
                            tooltip_entries.push((text, color));
                        }
                        let [vx0, vx1] = pv.time_axis.view_x.unwrap_or([pv.x_min, pv.x_max]);
                        let snap = ((vx1 - vx0) * 0.005).max(0.0005);
                        if let Some(&t) = pv
                            .http_req_allocfails
                            .iter()
                            .filter(|&&t| (t - data_x).abs() <= snap)
                            .min_by(|&&a, &&b| {
                                (a - data_x)
                                    .abs()
                                    .partial_cmp(&(b - data_x).abs())
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                        {
                            let dot_y = http_req_r
                                .transform
                                .position_from_point(&PlotPoint { x: t, y: -1.0 })
                                .y;
                            let dot = egui::pos2(cursor_x, dot_y);
                            if http_req_r.response.rect.contains(dot) {
                                ui.painter().circle_filled(dot, 4.5, Color32::from_gray(20));
                                ui.painter()
                                    .circle_filled(dot, 3.0, COLOR_HTTP_REQ_ALLOCFAIL);
                            }
                            tooltip_entries.push((
                                "HTTP req: alloc fail".to_string(),
                                COLOR_HTTP_REQ_ALLOCFAIL,
                            ));
                        }
                    }

                    // HTTP Response Size: snap to the nearest completed
                    // request's (t_end, bytes) point and report it.
                    if let Some(ref resp_size_r) = http_resp_size_resp {
                        let [vx0, vx1] = pv.time_axis.view_x.unwrap_or([pv.x_min, pv.x_max]);
                        let snap = ((vx1 - vx0) * 0.005).max(0.0005);
                        let nearest = pv
                            .nginx_req_spans
                            .iter()
                            .filter_map(|s| Some((s, s.t_end?, s.bytes_sent?)))
                            .filter(|(_, t, _)| (t - data_x).abs() <= snap)
                            .min_by(|(_, a, _), (_, b, _)| {
                                (a - data_x)
                                    .abs()
                                    .partial_cmp(&(b - data_x).abs())
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            });
                        if let Some((s, t, bytes)) = nearest {
                            let (class, color) = nginx_status_legend(s.status);
                            let dot_y = resp_size_r
                                .transform
                                .position_from_point(&PlotPoint {
                                    x: t,
                                    y: bytes as f64,
                                })
                                .y;
                            let dot = egui::pos2(cursor_x, dot_y);
                            if resp_size_r.response.rect.contains(dot) {
                                ui.painter().circle_filled(dot, 4.5, Color32::from_gray(20));
                                ui.painter().circle_filled(dot, 3.0, color);
                            }
                            tooltip_entries.push((
                                format!("req {}: {} {}", s.request, class, format_bytes(bytes)),
                                color,
                            ));
                        }
                    }

                    // HTTP Duration: snap to the nearest completed
                    // request's (t_end, duration) point and report it.
                    if let Some(ref latency_r) = http_latency_resp {
                        let [vx0, vx1] = pv.time_axis.view_x.unwrap_or([pv.x_min, pv.x_max]);
                        let snap = ((vx1 - vx0) * 0.005).max(0.0005);
                        let nearest = pv
                            .nginx_req_spans
                            .iter()
                            .filter_map(|s| s.t_end.map(|t| (s, t)))
                            .filter(|(_, t)| (t - data_x).abs() <= snap)
                            .min_by(|(_, a), (_, b)| {
                                (a - data_x)
                                    .abs()
                                    .partial_cmp(&(b - data_x).abs())
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            });
                        if let Some((s, t)) = nearest {
                            let (class, color) = nginx_status_legend(s.status);
                            let ms = (t - s.t_start).max(0.0) * 1000.0;
                            let dot_y = latency_r
                                .transform
                                .position_from_point(&PlotPoint { x: t, y: ms })
                                .y;
                            let dot = egui::pos2(cursor_x, dot_y);
                            if latency_r.response.rect.contains(dot) {
                                ui.painter().circle_filled(dot, 4.5, Color32::from_gray(20));
                                ui.painter().circle_filled(dot, 3.0, color);
                            }
                            tooltip_entries.push((
                                format!("req {}: {} {:.1} ms", s.request, class, ms),
                                color,
                            ));
                        }
                    }

                    // Snap tooltip to the nearest discrete-event marker
                    // within ~0.5% of the current view width so cursor
                    // placement doesn't have to be pixel-perfect. Each
                    // marker stream is consulted independently so multiple
                    // events that happen to coincide all surface.
                    {
                        let [vx0, vx1] = pv.time_axis.view_x.unwrap_or([pv.x_min, pv.x_max]);
                        let snap = ((vx1 - vx0) * 0.005).max(0.0005);
                        let nearest_reason = |src: &[(f64, String)]| -> Option<String> {
                            src.iter()
                                .filter(|(t, _)| (t - data_x).abs() <= snap)
                                .min_by(|a, b| {
                                    (a.0 - data_x)
                                        .abs()
                                        .partial_cmp(&(b.0 - data_x).abs())
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                })
                                .map(|(_, r)| r.clone())
                        };
                        if pv.show_rto {
                            if let Some(reason) = nearest_reason(&visible_rto) {
                                tooltip_entries.push((format!("RTO: {reason}"), COLOR_MARKER_RTO));
                            }
                        }
                        if pv.show_window_collapse {
                            if let Some(reason) = nearest_reason(&visible_window_collapse) {
                                tooltip_entries.push((
                                    format!("Window Collapse: {reason}"),
                                    COLOR_MARKER_WINDOW_COLLAPSE,
                                ));
                            }
                        }
                        if pv.show_dsack {
                            if let Some(reason) = nearest_reason(&visible_dsack) {
                                tooltip_entries
                                    .push((format!("DSACK: {reason}"), COLOR_MARKER_DSACK));
                            }
                        }
                    }

                    // Draw tooltip box
                    let line_height = 16.0_f32;
                    let padding = 6.0_f32;
                    let indicator_r = 3.0_f32;
                    let indicator_gap = 5.0_f32;
                    let indent = indicator_r * 2.0 + indicator_gap;

                    let galleys: Vec<_> = tooltip_entries
                        .iter()
                        .map(|(text, color)| {
                            ui.painter()
                                .layout_no_wrap(text.clone(), font.clone(), *color)
                        })
                        .collect();

                    let max_text_w = galleys.iter().map(|g| g.size().x).fold(0.0_f32, f32::max);
                    let box_w = max_text_w + indent + padding * 2.0;
                    let box_h = galleys.len() as f32 * line_height + padding * 2.0;

                    let box_x = if cursor_x + 14.0 + box_w < plots_rect.right() {
                        cursor_x + 14.0
                    } else {
                        cursor_x - 14.0 - box_w
                    };
                    let box_y = (pos.y - box_h * 0.5).clamp(
                        plots_rect.top(),
                        (plots_rect.bottom() - box_h).max(plots_rect.top()),
                    );

                    let box_rect = egui::Rect::from_min_size(
                        egui::pos2(box_x, box_y),
                        egui::vec2(box_w, box_h),
                    );
                    ui.painter().rect_filled(
                        box_rect,
                        4.0,
                        Color32::from_rgba_premultiplied(25, 25, 30, 225),
                    );
                    ui.painter().rect_stroke(
                        box_rect,
                        4.0,
                        Stroke::new(1.0_f32, Color32::from_gray(70)),
                    );

                    for (i, ((_, color), galley)) in tooltip_entries.iter().zip(galleys).enumerate()
                    {
                        let y = box_y + padding + i as f32 * line_height;
                        if i == 0 {
                            let text_x = box_x + padding + indent;
                            ui.painter().galley(
                                egui::pos2(text_x, y),
                                galley,
                                Color32::TRANSPARENT,
                            );
                        } else {
                            let cx = box_x + padding + indicator_r;
                            let cy = y + line_height * 0.5;
                            ui.painter()
                                .circle_filled(egui::pos2(cx, cy), indicator_r, *color);
                            let text_x = box_x + padding + indent;
                            ui.painter().galley(
                                egui::pos2(text_x, y),
                                galley,
                                Color32::TRANSPARENT,
                            );
                        }
                    }
                }
            }
        } // end time-series interaction guard

        // --- CDF plot cursor + tooltip (independent from time-series plots) ---
        if let Some(ref cdf_r) = size_cdf_resp {
            let cdf_rect = cdf_r.response.rect;
            let cdf_tf = cdf_r.transform;
            let pointer_in_cdf = pointer_pos.is_some_and(|p| cdf_rect.contains(p));

            if pointer_in_cdf {
                if let Some(pos) = pointer_pos {
                    let cursor_x = pos.x;
                    let data_x = cdf_tf.value_from_position(pos).x;

                    ui.painter().line_segment(
                        [
                            egui::pos2(cursor_x, cdf_rect.top()),
                            egui::pos2(cursor_x, cdf_rect.bottom()),
                        ],
                        Stroke::new(1.0_f32, Color32::from_gray(180)),
                    );

                    let font = egui::FontId::proportional(12.0);
                    let mut entries: Vec<(String, Color32)> = Vec::new();
                    entries.push((
                        format!("P = {:.1}%", data_x.clamp(0.0, 100.0)),
                        Color32::from_gray(220),
                    ));

                    let cdf_hidden = PlotMemory::load(ctx, egui::Id::new(ids[4].as_str()))
                        .map(|m| m.hidden_items)
                        .unwrap_or_default();

                    for (tip, legend, color, cdf_data) in [
                        (
                            "Send",
                            "Send Size",
                            COLOR_SEND_SIZE,
                            send_size_cdf.as_slice(),
                        ),
                        ("ACK", "ACK Size", COLOR_ACK_SIZE, ack_size_cdf.as_slice()),
                    ] {
                        if cdf_hidden.contains(legend) {
                            continue;
                        }
                        if let Some(y) = interpolate_line(cdf_data, data_x) {
                            let dot_y = cdf_tf.position_from_point(&PlotPoint { x: data_x, y }).y;
                            let dot = egui::pos2(cursor_x, dot_y);
                            if cdf_rect.contains(dot) {
                                ui.painter().circle_filled(dot, 4.5, Color32::from_gray(20));
                                ui.painter().circle_filled(dot, 3.0, color);
                            }
                            entries.push((format!("{}: {} B", tip, y as u64), color));
                        }
                    }

                    let line_height = 16.0_f32;
                    let padding = 6.0_f32;
                    let indicator_r = 3.0_f32;
                    let indicator_gap = 5.0_f32;
                    let indent = indicator_r * 2.0 + indicator_gap;

                    let galleys: Vec<_> = entries
                        .iter()
                        .map(|(text, color)| {
                            ui.painter()
                                .layout_no_wrap(text.clone(), font.clone(), *color)
                        })
                        .collect();

                    let max_text_w = galleys.iter().map(|g| g.size().x).fold(0.0_f32, f32::max);
                    let box_w = max_text_w + indent + padding * 2.0;
                    let box_h = galleys.len() as f32 * line_height + padding * 2.0;

                    let box_x = if cursor_x + 14.0 + box_w < cdf_rect.right() {
                        cursor_x + 14.0
                    } else {
                        cursor_x - 14.0 - box_w
                    };
                    let box_y = (pos.y - box_h * 0.5).clamp(
                        cdf_rect.top(),
                        (cdf_rect.bottom() - box_h).max(cdf_rect.top()),
                    );

                    let box_rect = egui::Rect::from_min_size(
                        egui::pos2(box_x, box_y),
                        egui::vec2(box_w, box_h),
                    );
                    ui.painter().rect_filled(
                        box_rect,
                        4.0,
                        Color32::from_rgba_premultiplied(25, 25, 30, 225),
                    );
                    ui.painter().rect_stroke(
                        box_rect,
                        4.0,
                        Stroke::new(1.0_f32, Color32::from_gray(70)),
                    );

                    for (i, ((_, color), galley)) in entries.iter().zip(galleys).enumerate() {
                        let y = box_y + padding + i as f32 * line_height;
                        if i == 0 {
                            ui.painter().galley(
                                egui::pos2(box_x + padding + indent, y),
                                galley,
                                Color32::TRANSPARENT,
                            );
                        } else {
                            ui.painter().circle_filled(
                                egui::pos2(box_x + padding + indicator_r, y + line_height * 0.5),
                                indicator_r,
                                *color,
                            );
                            ui.painter().galley(
                                egui::pos2(box_x + padding + indent, y),
                                galley,
                                Color32::TRANSPARENT,
                            );
                        }
                    }
                }
            }
        }

        if is_maximized && Self::escape_restores_plot_maximize(ui) {
            pv.plot_layout.clear_maximize();
            pv.plot_draft.clear_maximize();
        }

        if let Some(mut editor) = pv.y_max_editor.take() {
            let plot_idx = editor.plot_idx;
            let anchor = editor.anchor;
            let mut submit: Option<Option<f64>> = None;

            egui::Window::new("Y axis maximum")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::LEFT_TOP, egui::vec2(0.0, 0.0))
                .fixed_pos(anchor)
                .default_width(240.0)
                .show(ctx, |ui| {
                    ui.label("Y maximum:");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut editor.draft)
                            .desired_width(200.0)
                            .hint_text("Enter to apply"),
                    );
                    resp.request_focus();

                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        submit = Some(None);
                    }
                    if ui.button("Apply").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        match editor.draft.trim().parse::<f64>() {
                            Ok(v) if v.is_finite() && v > 0.0 => submit = Some(Some(v)),
                            _ => {}
                        }
                    }
                });

            match submit {
                None => {
                    pv.y_max_editor = Some(editor);
                }
                Some(None) => {}
                Some(Some(v)) => {
                    pv.y_max_override[plot_idx] = Some(v);
                    pv.apply_y_max_bounds(ctx, &ids[plot_idx], v);
                    ctx.request_repaint();
                }
            }
        }
    }
}

#[cfg(test)]
mod session_list_tab_tests {
    use super::*;
    use crate::elog_parser::TcpStateSample;

    fn one_summary(sid: u64) -> SessionSummary {
        analysis::session_summary_for(sid, &[])
    }

    fn state_tick() -> TcpStateSample {
        TcpStateSample {
            ts: 0,
            provider_id: 3,
            session_id: 0,
            closed: 0,
            listen: 0,
            syn_sent: 0,
            syn_rcvd: 0,
            estab: 1,
            close_wait: 0,
            fin_wait_1: 0,
            closing: 0,
            last_ack: 0,
            fin_wait_2: 0,
            time_wait: 0,
        }
    }

    #[test]
    fn empty_capture_hides_plus() {
        let app = EventExplorerApp::new_empty(None);
        assert!(!app.show_session_list_tab());
    }

    #[test]
    fn lone_tcp_session_without_system_shows_plus() {
        let mut app = EventExplorerApp::new_empty(None);
        app.summaries.push(one_summary(0x4ba309));
        assert!(!app.system_tab_available());
        assert!(app.show_session_list_tab());
    }

    #[test]
    fn lone_tcp_session_with_system_shows_plus() {
        let mut app = EventExplorerApp::new_empty(None);
        app.summaries.push(one_summary(0x4ba309));
        app.tcp_samples.state_samples.push(state_tick());
        assert!(app.system_tab_available());
        assert!(app.show_session_list_tab());
    }

    #[test]
    fn two_tcp_sessions_show_plus_without_system_samples() {
        let mut app = EventExplorerApp::new_empty(None);
        app.summaries.push(one_summary(0x4ba309));
        app.summaries.push(one_summary(0x4ba30a));
        assert!(app.show_session_list_tab());
    }

    #[test]
    fn span_fracs_maps_onto_trace() {
        let (f0, f1) = EventExplorerApp::span_fracs(1_500_000, 2_000_000, (1_000_000, 3_000_000));
        assert!((f0 - 0.25).abs() < 1e-6);
        assert!((f1 - 0.5).abs() < 1e-6);
    }

    #[test]
    fn span_fracs_clamps_out_of_range() {
        let (f0, f1) = EventExplorerApp::span_fracs(0, 9_000_000, (1_000_000, 3_000_000));
        assert_eq!((f0, f1), (0.0, 1.0));
    }

    #[test]
    fn span_fracs_zero_width_trace() {
        assert_eq!(EventExplorerApp::span_fracs(5, 10, (7, 7)), (0.0, 0.0));
    }

    #[test]
    fn overlaps_window_edge_cases() {
        let mut s = one_summary(1);
        s.start_ts_us = 100;
        s.end_ts_us = 200;
        assert!(EventExplorerApp::overlaps_window(&s, (150, 250)));
        assert!(EventExplorerApp::overlaps_window(&s, (50, 150)));
        assert!(EventExplorerApp::overlaps_window(&s, (200, 300)));
        assert!(EventExplorerApp::overlaps_window(&s, (0, 100)));
        assert!(!EventExplorerApp::overlaps_window(&s, (201, 300)));
        assert!(!EventExplorerApp::overlaps_window(&s, (0, 99)));
    }

    #[test]
    fn start_time_sort_is_chronological() {
        let mut a = one_summary(1);
        a.start_ts_us = 300;
        let mut b = one_summary(2);
        b.start_ts_us = 100;
        let mut c = one_summary(3);
        c.start_ts_us = 200;
        let mut v = vec![a, b, c];
        sort_summaries(&mut v, SortColumn::StartTime, true);
        let ids: Vec<u64> = v.iter().map(|s| s.session_id).collect();
        assert_eq!(ids, [2, 3, 1]);
    }
}
