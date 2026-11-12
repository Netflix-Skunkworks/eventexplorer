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
// Recent files persistence
// ---------------------------------------------------------------------------
//
// Filesystem-backed on desktop (`~/.config/event_explorer/recent_files.txt`).
// On wasm there is no real filesystem, so the stubs below make the
// recent-files list ephemeral (it lives in memory for the page lifetime
// and resets on reload). A future improvement would be to persist via
// eframe's `Storage` (localStorage on web).

pub const MAX_RECENT_FILES: usize = 10;

#[cfg(not(target_arch = "wasm32"))]
pub fn recent_files_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("event_explorer").join("recent_files.txt"))
}

/// Separator between the files of one recent entry. A rolled set is
/// reopened as the set it was loaded as, so an entry is a list of paths;
/// one path per line stays a single-file entry, keeping older
/// `recent_files.txt` contents readable.
#[cfg(not(target_arch = "wasm32"))]
const GROUP_SEPARATOR: char = '\t';

#[cfg(not(target_arch = "wasm32"))]
pub fn load_recent_files() -> Vec<Vec<PathBuf>> {
    let Some(path) = recent_files_path() else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.split(GROUP_SEPARATOR)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .filter(|group| {
            // Keep entries that exist locally OR look like remote SCP paths
            // (we can't probe remote existence cheaply here).
            !group.is_empty()
                && group.iter().all(|p| {
                    p.exists() || elog_parser::is_remote_path(&p.to_string_lossy()).is_some()
                })
        })
        .take(MAX_RECENT_FILES)
        .collect()
}

#[cfg(target_arch = "wasm32")]
pub fn load_recent_files() -> Vec<Vec<PathBuf>> {
    Vec::new()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save_recent_files(files: &[Vec<PathBuf>]) {
    let Some(path) = recent_files_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content: String = files
        .iter()
        .take(MAX_RECENT_FILES)
        .map(|group| {
            group
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(&GROUP_SEPARATOR.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(&path, content);
}

#[cfg(target_arch = "wasm32")]
pub fn save_recent_files(_files: &[Vec<PathBuf>]) {}

#[cfg(target_arch = "wasm32")]
const DARK_MODE_STORAGE_KEY: &str = "event_explorer_dark_mode";
#[cfg(target_arch = "wasm32")]
const BLUE_USER_FLAME_STORAGE_KEY: &str = "event_explorer_blue_user_flame";
#[cfg(target_arch = "wasm32")]
const PLOT_BINS_STORAGE_KEY: &str = "event_explorer_plot_bins";

/// Time buckets count. Out of range or unreadable text falls back to
/// [`STACK_BINS`].
pub(crate) fn parse_plot_bins(text: &str) -> usize {
    text.trim()
        .parse::<usize>()
        .ok()
        .filter(|n| (PLOT_BINS_MIN..=PLOT_BINS_MAX).contains(n))
        .unwrap_or(STACK_BINS)
}

/// `true` when unset.
pub fn load_dark_mode() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return true;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            return true;
        };
        return match storage.get_item(DARK_MODE_STORAGE_KEY) {
            Ok(Some(s)) => s != "0",
            _ => true,
        };
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = dirs::config_dir().map(|d| d.join("event_explorer").join("dark_mode"))
        else {
            return true;
        };
        match std::fs::read_to_string(path) {
            Ok(s) => s.trim() != "0",
            Err(_) => true,
        }
    }
}

pub fn save_dark_mode(dark: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            return;
        };
        let _ = storage.set_item(DARK_MODE_STORAGE_KEY, if dark { "1" } else { "0" });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = dirs::config_dir().map(|d| d.join("event_explorer").join("dark_mode"))
        else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, if dark { "1" } else { "0" });
    }
}

/// Time buckets count from the tab-bar gear. [`STACK_BINS`] when unset.
pub fn load_plot_bins() -> usize {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return STACK_BINS;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            return STACK_BINS;
        };
        return match storage.get_item(PLOT_BINS_STORAGE_KEY) {
            Ok(Some(s)) => parse_plot_bins(&s),
            _ => STACK_BINS,
        };
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = dirs::config_dir().map(|d| d.join("event_explorer").join("plot_bins"))
        else {
            return STACK_BINS;
        };
        match std::fs::read_to_string(path) {
            Ok(s) => parse_plot_bins(&s),
            Err(_) => STACK_BINS,
        }
    }
}

pub fn save_plot_bins(n: usize) {
    let text = parse_plot_bins(&n.to_string()).to_string();
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            return;
        };
        let _ = storage.set_item(PLOT_BINS_STORAGE_KEY, &text);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = dirs::config_dir().map(|d| d.join("event_explorer").join("plot_bins"))
        else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, text);
    }
}

/// Whether user-space flame frames use the blue palette. `true` when unset.
pub fn load_blue_user_flame() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return true;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            return true;
        };
        return match storage.get_item(BLUE_USER_FLAME_STORAGE_KEY) {
            Ok(Some(s)) => s != "0",
            _ => true,
        };
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) =
            dirs::config_dir().map(|d| d.join("event_explorer").join("blue_user_flame"))
        else {
            return true;
        };
        match std::fs::read_to_string(path) {
            Ok(s) => s.trim() != "0",
            Err(_) => true,
        }
    }
}

pub fn save_blue_user_flame(enabled: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            return;
        };
        let _ = storage.set_item(BLUE_USER_FLAME_STORAGE_KEY, if enabled { "1" } else { "0" });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) =
            dirs::config_dir().map(|d| d.join("event_explorer").join("blue_user_flame"))
        else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, if enabled { "1" } else { "0" });
    }
}

pub(crate) const DEFAULT_STACK_FONT_SIZE: f32 = 14.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ProcessesPreferences {
    pub(crate) table_width: Option<f32>,
    pub(crate) function_list_height: Option<f32>,
    pub(crate) merge_modules: bool,
    pub(crate) show_stack_modules: bool,
    pub(crate) wrap_stacks: bool,
    pub(crate) stack_font_size: f32,
}

impl Default for ProcessesPreferences {
    fn default() -> Self {
        Self {
            table_width: None,
            function_list_height: None,
            merge_modules: false,
            show_stack_modules: true,
            wrap_stacks: false,
            stack_font_size: DEFAULT_STACK_FONT_SIZE,
        }
    }
}

fn parse_processes_preferences(content: &str) -> ProcessesPreferences {
    let mut preferences = ProcessesPreferences::default();
    for line in content.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "table_width" => {
                preferences.table_width = value
                    .parse::<f32>()
                    .ok()
                    .filter(|value| value.is_finite() && *value >= 80.0 && *value <= 10_000.0);
            }
            "function_list_height" => {
                preferences.function_list_height = value
                    .parse::<f32>()
                    .ok()
                    .filter(|value| value.is_finite() && *value >= 40.0 && *value <= 10_000.0);
            }
            "merge_modules" => {
                preferences.merge_modules = value == "true" || value == "1";
            }
            "show_stack_modules" => {
                preferences.show_stack_modules = value == "true" || value == "1";
            }
            "wrap_stacks" => {
                preferences.wrap_stacks = value == "true" || value == "1";
            }
            "stack_font_size" => {
                if let Ok(size) = value.parse::<f32>() {
                    if size.is_finite() {
                        preferences.stack_font_size = size.clamp(6.0, DEFAULT_STACK_FONT_SIZE);
                    }
                }
            }
            _ => {}
        }
    }
    preferences
}

fn format_processes_preferences(preferences: &ProcessesPreferences) -> String {
    format!(
        "table_width={}\nfunction_list_height={}\nmerge_modules={}\nshow_stack_modules={}\nwrap_stacks={}\nstack_font_size={}",
        preferences
            .table_width
            .map(|value| value.to_string())
            .unwrap_or_default(),
        preferences
            .function_list_height
            .map(|value| value.to_string())
            .unwrap_or_default(),
        preferences.merge_modules,
        preferences.show_stack_modules,
        preferences.wrap_stacks,
        preferences.stack_font_size,
    )
}

#[cfg(target_arch = "wasm32")]
const PROCESSES_PREFERENCES_STORAGE_KEY: &str = "event_explorer_processes_preferences";

pub(crate) fn load_processes_preferences() -> ProcessesPreferences {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return ProcessesPreferences::default();
        };
        let Ok(Some(storage)) = window.local_storage() else {
            return ProcessesPreferences::default();
        };
        return storage
            .get_item(PROCESSES_PREFERENCES_STORAGE_KEY)
            .ok()
            .flatten()
            .map(|content| parse_processes_preferences(&content))
            .unwrap_or_default();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = dirs::config_dir()
            .map(|directory| directory.join("event_explorer").join("processes_view.txt"))
        else {
            return ProcessesPreferences::default();
        };
        std::fs::read_to_string(path)
            .ok()
            .map(|content| parse_processes_preferences(&content))
            .unwrap_or_default()
    }
}

pub(crate) fn save_processes_preferences(preferences: &ProcessesPreferences) {
    let content = format_processes_preferences(preferences);
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            return;
        };
        let _ = storage.set_item(PROCESSES_PREFERENCES_STORAGE_KEY, &content);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = dirs::config_dir()
            .map(|directory| directory.join("event_explorer").join("processes_view.txt"))
        else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, content);
    }
}

#[cfg(target_arch = "wasm32")]
pub fn apply_web_color_scheme(dark: bool) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(doc) = window.document() else {
        return;
    };
    let Some(el) = doc.document_element() else {
        return;
    };
    let _ = el.set_attribute("data-theme", if dark { "dark" } else { "light" });
}

pub fn apply_visuals(ctx: &egui::Context, dark: bool) {
    if ctx.style().visuals.dark_mode != dark {
        ctx.set_visuals(if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        #[cfg(target_arch = "wasm32")]
        apply_web_color_scheme(dark);
    }
}

/// Build `pathname?file=…&file=…` for the current page.
#[cfg(target_arch = "wasm32")]
fn file_query_url(file_urls: &[String]) -> Option<String> {
    let window = web_sys::window()?;
    let path = window.location().pathname().unwrap_or_else(|_| "/".into());
    let query = ShareParams {
        file_urls: file_urls.to_vec(),
        ..ShareParams::default()
    }
    .to_query_string();
    Some(format!("{path}?{query}"))
}

/// Push `?file=` so the browser Back button returns to the splash.
#[cfg(target_arch = "wasm32")]
pub fn push_file_query(file_urls: &[String]) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(history) = window.history() else {
        return;
    };
    let Some(next) = file_query_url(file_urls) else {
        return;
    };
    let _ = history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&next));
}

/// Replace the current entry's `?file=` (no new history entry): a
/// finished share-upload swaps the picker load for its canonical
/// URLs in place, so a refresh reloads the uploaded capture.
#[cfg(target_arch = "wasm32")]
pub fn replace_file_query(file_urls: &[String]) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(history) = window.history() else {
        return;
    };
    let Some(next) = file_query_url(file_urls) else {
        return;
    };
    let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&next));
}

/// Fire `tx` with `location.search` whenever the user hits Back/Forward.
#[cfg(target_arch = "wasm32")]
pub fn install_popstate_listener(tx: mpsc::Sender<String>) {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;

    let Some(window) = web_sys::window() else {
        return;
    };
    let cb = Closure::<dyn FnMut()>::new(move || {
        let search = web_sys::window()
            .and_then(|w| w.location().search().ok())
            .unwrap_or_default();
        let _ = tx.send(search);
    });
    let _ = window.add_event_listener_with_callback("popstate", cb.as_ref().unchecked_ref());
    cb.forget();
}

// ---------------------------------------------------------------------------
// Web fetch helper (used by `?file=…` auto-load on startup)
// ---------------------------------------------------------------------------

/// Yield to the browser so egui can paint the loading screen.
///
/// `spawn_local` tasks share the tab's JS thread with rendering. A
/// `setTimeout(0)` turn is enough for `request_repaint_after` to
/// run and show the latest `LoadProgress` text/bar.
#[cfg(target_arch = "wasm32")]
pub async fn yield_to_browser() {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;

    let Some(window) = web_sys::window() else {
        return;
    };
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let resolve_if_timeout_fails = resolve.clone();
        let cb = Closure::once(move || {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        });
        let func: &js_sys::Function = cb.as_ref().unchecked_ref();
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_0(func, 0)
            .is_err()
        {
            let _ = resolve_if_timeout_fails.call0(&JsValue::UNDEFINED);
        }
        cb.forget();
    });
    let _ = JsFuture::from(promise).await;
}

/// Display label for a URL: the `#fragment` when one is present
/// (share links carry the original file name there — it is never
/// sent to the server), else the last non-empty path segment.
#[cfg(target_arch = "wasm32")]
pub fn url_file_label(url: &str) -> &str {
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

/// Fetch the bytes of `url` via `XMLHttpRequest`.
///
/// The URL is resolved against the page origin, so relative paths
/// work for files served alongside the wasm bundle. Errors are
/// stringified for the loading-screen "Failed to parse" branch.
///
/// `file://` URLs are rejected up-front with a self-explanatory
/// error: every browser refuses to fetch `file://` from a page
/// served over `http(s)://` ("Not allowed to load local resource"
/// in Chromium), and the raw fetch rejection isn't actionable.
/// Callers who want to load a local capture must either serve it
/// over HTTP or use the file picker.
///
/// Progress comes from XHR `onprogress` while the browser fills an
/// `ArrayBuffer` at native download speed.
#[cfg(target_arch = "wasm32")]
pub async fn fetch_bytes(url: &str, progress: Arc<LoadProgress>) -> Result<Vec<u8>, String> {
    use std::cell::Cell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{ProgressEvent, XmlHttpRequest, XmlHttpRequestResponseType};

    if url.starts_with("file://") {
        return Err(format!(
            "browsers refuse to fetch local files from a web page \
             ({url}). Either serve the file over HTTP \
             (e.g. `python3 -m http.server` in its directory, then \
             ?file=http://localhost:8000/...) or use the file picker."
        ));
    }

    let label = url_file_label(url).to_string();
    progress.set_status(format!("Fetching {label}…"));

    let xhr = XmlHttpRequest::new().map_err(|e| format!("XMLHttpRequest: {e:?}"))?;
    xhr.open_with_async("GET", url, true)
        .map_err(|e| format!("XHR open failed: {e:?}"))?;
    xhr.set_response_type(XmlHttpRequestResponseType::Arraybuffer);

    let last_status_at = Rc::new(Cell::new(0u64));
    let progress_cb = progress.clone();
    let label_cb = label.clone();
    let last_cb = last_status_at.clone();
    let onprogress = Closure::<dyn FnMut(ProgressEvent)>::new(move |e: ProgressEvent| {
        let loaded = e.loaded() as u64;
        const STATUS_INTERVAL: u64 = 256 * 1024;
        let total = if e.length_computable() && e.total() > 0.0 {
            Some(e.total() as u64)
        } else {
            None
        };
        let finished = total.is_some_and(|t| loaded >= t);
        if loaded.saturating_sub(last_cb.get()) < STATUS_INTERVAL && !finished {
            return;
        }
        last_cb.set(loaded);
        report_fetch_progress(&label_cb, loaded, total, &progress_cb);
    });
    xhr.set_onprogress(Some(onprogress.as_ref().unchecked_ref()));

    let xhr_for_handlers = xhr.clone();
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let xhr_load = xhr_for_handlers.clone();
        let resolve_ok = resolve.clone();
        let reject_http = reject.clone();
        let onload = Closure::once(move || match xhr_load.status() {
            Ok(code) if (200..300).contains(&code) => match xhr_load.response() {
                Ok(buf) => {
                    let _ = resolve_ok.call1(&JsValue::UNDEFINED, &buf);
                }
                Err(e) => {
                    let _ = reject_http.call1(&JsValue::UNDEFINED, &e);
                }
            },
            Ok(code) => {
                let text = xhr_load.status_text().unwrap_or_default();
                let msg = format!("HTTP {code} {text}");
                let _ = reject_http.call1(&JsValue::UNDEFINED, &JsValue::from_str(&msg));
            }
            Err(e) => {
                let _ = reject_http.call1(&JsValue::UNDEFINED, &e);
            }
        });
        xhr_for_handlers.set_onload(Some(onload.as_ref().unchecked_ref()));
        onload.forget();

        let onerror = Closure::once(move || {
            let _ = reject.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str("network error fetching capture"),
            );
        });
        xhr_for_handlers.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();
    });

    xhr.send().map_err(|e| format!("XHR send failed: {e:?}"))?;

    let buf_value = JsFuture::from(promise)
        .await
        .map_err(|e| format!("fetch rejected: {:?}", e))?;
    drop(onprogress);

    let array = js_sys::Uint8Array::new(&buf_value);
    let mut bytes = vec![0u8; array.length() as usize];
    array.copy_to(&mut bytes);
    report_fetch_progress(
        &label,
        bytes.len() as u64,
        Some(bytes.len() as u64),
        &progress,
    );
    progress.store_permille(1000);
    Ok(bytes)
}

#[cfg(target_arch = "wasm32")]
fn report_fetch_progress(label: &str, so_far: u64, total: Option<u64>, progress: &LoadProgress) {
    if let Some(total) = total {
        let done = so_far.min(total);
        progress.set_status(format!(
            "Fetching {label}: {} / {}",
            elog_parser::fmt_bytes(done),
            elog_parser::fmt_bytes(total),
        ));
        let permille = (done as f64 / total.max(1) as f64 * 1000.0) as u32;
        progress.store_permille(permille.min(1000));
    } else {
        progress.set_status(format!(
            "Fetching {label}: {}",
            elog_parser::fmt_bytes(so_far)
        ));
        let scale = 1000.0_f64;
        let inched = (scale - scale * (-(so_far as f64) / (8.0 * 1024.0 * 1024.0)).exp()) as u32;
        progress.store_permille(inched.min(1000));
    }
}

/// Inflate a gzip payload in chunks, updating `progress` and yielding
/// so the loading screen can paint. One entry per compressed capture
/// inside the gzip. Used by the wasm load path.
#[cfg(target_arch = "wasm32")]
pub async fn decompress_gzip_yielding(
    data: &[u8],
    file_label: &str,
    progress: &LoadProgress,
) -> Result<Vec<(String, elog_parser::ChunkedSrc)>, String> {
    use std::io::Read;

    progress.set_status(format!("Decompressing {file_label}…"));
    yield_to_browser().await;

    let compressed_total = data.len() as u64;
    let mut split = elog_parser::GzipSplit::default();
    let mut rest = data;
    let mut used = std::collections::HashSet::new();
    let mut parts: Vec<(String, elog_parser::ChunkedSrc)> = Vec::new();
    let mut current: Option<(String, elog_parser::ChunkedSrc, Vec<u8>)> = None;
    let mut produced = 0u64;
    let mut tmp = vec![0u8; 256 * 1024];
    let mut last_yield_at = 0u64;
    const YIELD_EVERY: u64 = 4 * 1024 * 1024;

    loop {
        let next: Option<elog_parser::GzipMember<&[u8]>> =
            elog_parser::open_gzip_member(rest, &mut split)
                .map_err(|e| format!("gzip decompress failed: {e}"))?;
        let Some(mut member) = next else {
            break;
        };
        if member.new_capture {
            if let Some((name, mut out, cur)) = current.take() {
                if !cur.is_empty() {
                    out.push(cur);
                }
                parts.push((name, out));
            }
            let name = elog_parser::unique_part_name(
                &mut used,
                elog_parser::member_display_name(
                    member.name.as_deref(),
                    file_label,
                    member.capture_index,
                ),
            );
            current = Some((
                name,
                elog_parser::ChunkedSrc::new(),
                Vec::with_capacity(elog_parser::CHUNK_BYTES),
            ));
        }
        let (_, out, cur) = current
            .as_mut()
            .expect("a continuing gzip member follows a capture");
        loop {
            let n = member
                .read(&mut tmp)
                .map_err(|e| format!("gzip decompress failed: {e}"))?;
            if n == 0 {
                break;
            }
            let mut read = &tmp[..n];
            while !read.is_empty() {
                let room = elog_parser::CHUNK_BYTES - cur.len();
                let take = room.min(read.len());
                cur.extend_from_slice(&read[..take]);
                read = &read[take..];
                if cur.len() == elog_parser::CHUNK_BYTES {
                    produced += cur.len() as u64;
                    out.push(std::mem::take(cur));
                    cur.reserve(elog_parser::CHUNK_BYTES);
                }
            }
            let inn = compressed_total.saturating_sub(member.unread_compressed() as u64);
            progress.set_status(format!(
                "Decompressing {file_label}: {} → {}",
                elog_parser::fmt_bytes(inn.min(compressed_total)),
                elog_parser::fmt_bytes(produced + cur.len() as u64),
            ));
            let permille =
                (inn.min(compressed_total) as f64 / compressed_total.max(1) as f64 * 1000.0) as u32;
            progress.store_permille(permille.min(1000));
            if produced + cur.len() as u64 - last_yield_at >= YIELD_EVERY {
                last_yield_at = produced + cur.len() as u64;
                yield_to_browser().await;
            }
        }
        rest = member.into_inner();
    }
    if let Some((name, mut out, cur)) = current.take() {
        if !cur.is_empty() {
            out.push(cur);
        }
        parts.push((name, out));
    }
    if parts.is_empty() {
        return Err("gzip decompress failed: empty gzip stream".to_string());
    }

    progress.store_permille(1000);
    Ok(parts)
}

pub fn add_recent_capture(recent_files: &mut Vec<Vec<PathBuf>>, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    recent_files.retain(|group| group != &paths);
    recent_files.insert(0, paths);
    recent_files.truncate(MAX_RECENT_FILES);
    save_recent_files(recent_files);
}

/// Display name for a capture: the file's basename, or the first file's
/// basename plus a count of the rest for a merged set.
pub fn capture_label(paths: &[PathBuf]) -> String {
    let Some(first) = paths.first() else {
        return String::new();
    };
    let name = first
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| first.to_string_lossy().into_owned());
    if paths.len() > 1 {
        format!("{name} +{} more", paths.len() - 1)
    } else {
        name
    }
}

#[cfg(test)]
mod processes_preferences_tests {
    use super::*;

    #[test]
    fn plot_bins_parse_clamps_to_the_slider_range() {
        assert_eq!(parse_plot_bins("250"), 250);
        assert_eq!(parse_plot_bins("  40 \n"), 40);
        assert_eq!(parse_plot_bins("1"), STACK_BINS);
        assert_eq!(parse_plot_bins("9000"), STACK_BINS);
        assert_eq!(parse_plot_bins("nope"), STACK_BINS);
        assert_eq!(parse_plot_bins(""), STACK_BINS);
    }

    #[test]
    fn processes_preferences_round_trip() {
        let expected = ProcessesPreferences {
            table_width: Some(172.5),
            function_list_height: Some(384.0),
            merge_modules: true,
            show_stack_modules: false,
            wrap_stacks: true,
            stack_font_size: 10.0,
        };
        let parsed = parse_processes_preferences(&format_processes_preferences(&expected));
        assert_eq!(parsed, expected);
    }

    #[test]
    fn processes_preferences_keep_defaults_for_missing_or_invalid_values() {
        let parsed = parse_processes_preferences(
            "table_width=nope\nfunction_list_height=\nstack_font_size=99",
        );
        assert_eq!(parsed.table_width, None);
        assert_eq!(parsed.function_list_height, None);
        assert!(!parsed.merge_modules);
        assert!(parsed.show_stack_modules);
        assert!(!parsed.wrap_stacks);
        assert_eq!(parsed.stack_font_size, DEFAULT_STACK_FONT_SIZE);

        let smaller = parse_processes_preferences("stack_font_size=6");
        assert_eq!(smaller.stack_font_size, 6.0);
    }
}
