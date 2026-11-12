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
// Shareable view state (URL query parameters)
// ---------------------------------------------------------------------------
//
// On web the page URL doubles as a permalink to a particular *view* of a
// capture: the file URL, which session is open, the visible time window,
// and any non-default toggles. The Share button in the tab bar copies a
// URL with the relevant parameters; on startup, `WebHandle::start` parses
// the same parameters and queues them up to be applied as soon as the
// file finishes loading.
//
// The struct is cross-target so the encode/decode logic stays in one
// place — only the actual `window.location` reads and clipboard writes
// are wasm-gated. Every parameter is `Option`-shaped: `None` means
// "leave at default / current state", which lets a share URL specify
// only the bits the sharer cares about and lets the encoder keep URLs
// short by omitting fields that match the app's defaults.

#[derive(Clone, Default, Debug)]
pub struct ShareParams {
    /// Capture URLs, one `file=` each; several load as one merged rolled set.
    pub file_urls: Vec<String>,
    pub session: Option<u64>,
    /// Fanout channel tab to open: `(provider_id, session_id)`.
    pub channel: Option<(u16, u64)>,
    /// Static tab to focus (`system` / `sessions` / `flame` / `processes`).
    pub tab: Option<String>,
    /// Catalog row to scroll into view (a `PlotId::chrome_slot`).
    pub plot: Option<String>,
    /// Visible time range, seconds since session start.
    pub t_range: Option<[f64; 2]>,
    pub agg_filter_low_events: Option<bool>,
    pub utc_time_labels: Option<bool>,
    pub show_recovery_spans: Option<bool>,
    pub show_goodput_spans: Option<bool>,
    pub show_pacing_cap: Option<bool>,
    pub show_limiter: Option<bool>,
    pub show_rto: Option<bool>,
    pub show_window_collapse: Option<bool>,
    pub show_dsack: Option<bool>,
    pub show_http_req: Option<bool>,
    pub show_legends: Option<bool>,
    pub maximized_plot: Option<usize>,
    pub y_max_override: [Option<f64>; 5],
}

impl ShareParams {
    /// Returns true when the params carry any session/view/options info
    /// worth applying — i.e. anything beyond just `file_urls`. (`file_urls`
    /// alone is already handled by the existing auto-load path before
    /// we even reach the `View::Loaded` state.)
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn has_post_load_state(&self) -> bool {
        self.session.is_some()
            || self.channel.is_some()
            || self.tab.is_some()
            || self.plot.is_some()
            || self.t_range.is_some()
            || self.agg_filter_low_events.is_some()
            || self.utc_time_labels.is_some()
            || self.show_recovery_spans.is_some()
            || self.show_goodput_spans.is_some()
            || self.show_pacing_cap.is_some()
            || self.show_limiter.is_some()
            || self.show_rto.is_some()
            || self.show_window_collapse.is_some()
            || self.show_dsack.is_some()
            || self.show_http_req.is_some()
            || self.show_legends.is_some()
            || self.maximized_plot.is_some()
            || self.y_max_override.iter().any(|y| y.is_some())
    }

    /// Parse a query string of the form `key=val&key=val&…` (with or
    /// without a leading `?`) into a `ShareParams`. Unknown keys are
    /// silently ignored so old browsers / bookmarks bearing future
    /// parameters still load.
    pub fn from_query_string(search: &str) -> Self {
        let mut p = ShareParams::default();
        let s = search.strip_prefix('?').unwrap_or(search);
        if s.is_empty() {
            return p;
        }
        for pair in s.split('&') {
            let (key, val) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            let val = url_decode(val);
            match key {
                "file" => {
                    if !val.is_empty() && !p.file_urls.contains(&val) {
                        p.file_urls.push(val);
                    }
                }
                "session" => {
                    if let Ok(sid) = val.parse::<u64>() {
                        p.session = Some(sid);
                    }
                }
                "channel" => {
                    if let Some((pid, sid)) = val.split_once(',') {
                        if let (Ok(pid), Ok(sid)) = (pid.parse::<u16>(), sid.parse::<u64>()) {
                            p.channel = Some((pid, sid));
                        }
                    }
                }
                "tab" => {
                    if !val.is_empty() {
                        p.tab = Some(val);
                    }
                }
                "plot" => {
                    if !val.is_empty() {
                        p.plot = Some(val);
                    }
                }
                "t" => {
                    if let Some((a, b)) = val.split_once(',') {
                        if let (Ok(a), Ok(b)) = (a.parse::<f64>(), b.parse::<f64>()) {
                            if a.is_finite() && b.is_finite() && b > a {
                                p.t_range = Some([a, b]);
                            }
                        }
                    }
                }
                "agg_filter" => p.agg_filter_low_events = parse_bool(&val),
                "utc" => p.utc_time_labels = parse_bool(&val),
                "recovery" => p.show_recovery_spans = parse_bool(&val),
                "goodput_spans" => p.show_goodput_spans = parse_bool(&val),
                "pacing_cap" => p.show_pacing_cap = parse_bool(&val),
                "limiter" => p.show_limiter = parse_bool(&val),
                "rto" => p.show_rto = parse_bool(&val),
                "wcoll" => p.show_window_collapse = parse_bool(&val),
                "dsack" => p.show_dsack = parse_bool(&val),
                "http_req" => p.show_http_req = parse_bool(&val),
                "legends" => p.show_legends = parse_bool(&val),
                "max" => {
                    if let Ok(i) = val.parse::<usize>() {
                        if i < 5 {
                            p.maximized_plot = Some(i);
                        }
                    }
                }
                "ymax" => {
                    // `ymax=0:120,2:200` — comma-separated `idx:val` pairs
                    // assigning a Y-axis upper bound to one or more plot
                    // panels (0=RTT, 1=Rate, 2=CWND, 3=Sizes, 4=SizeCDF).
                    for entry in val.split(',') {
                        if let Some((i, v)) = entry.split_once(':') {
                            if let (Ok(i), Ok(v)) = (i.parse::<usize>(), v.parse::<f64>()) {
                                if i < 5 && v.is_finite() && v > 0.0 {
                                    p.y_max_override[i] = Some(v);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        p
    }

    /// Encode `self` as a `key=val&key=val&…` query string (no leading
    /// `?`). Fields whose value matches the app's default are omitted,
    /// so the URLs read cleanly: a sharer with everything at defaults
    /// gets just `?file=…&session=…&t=…`.
    pub fn to_query_string(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for url in &self.file_urls {
            parts.push(format!("file={}", url_encode(url)));
        }
        if let Some(sid) = self.session {
            parts.push(format!("session={sid}"));
        }
        if let Some((pid, sid)) = self.channel {
            parts.push(format!("channel={pid},{sid}"));
        }
        if let Some(tab) = &self.tab {
            parts.push(format!("tab={}", url_encode(tab)));
        }
        if let Some(plot) = &self.plot {
            parts.push(format!("plot={}", url_encode(plot)));
        }
        if let Some([a, b]) = self.t_range {
            parts.push(format!("t={},{}", fmt_t(a), fmt_t(b)));
        }
        // Per-session / per-aggregate toggles. Each line emits the
        // param only when the value differs from the app's default.
        if let Some(v) = self.agg_filter_low_events {
            if !v {
                parts.push("agg_filter=0".into());
            }
        }
        if let Some(v) = self.utc_time_labels {
            if v {
                parts.push("utc=1".into());
            }
        }
        if let Some(v) = self.show_recovery_spans {
            if !v {
                parts.push("recovery=0".into());
            }
        }
        if let Some(v) = self.show_goodput_spans {
            if v {
                parts.push("goodput_spans=1".into());
            }
        }
        if let Some(v) = self.show_pacing_cap {
            if v {
                parts.push("pacing_cap=1".into());
            }
        }
        if let Some(v) = self.show_limiter {
            if v {
                parts.push("limiter=1".into());
            }
        }
        if let Some(v) = self.show_rto {
            if v {
                parts.push("rto=1".into());
            }
        }
        if let Some(v) = self.show_window_collapse {
            if v {
                parts.push("wcoll=1".into());
            }
        }
        if let Some(v) = self.show_dsack {
            if v {
                parts.push("dsack=1".into());
            }
        }
        if let Some(v) = self.show_http_req {
            if !v {
                parts.push("http_req=0".into());
            }
        }
        if let Some(v) = self.show_legends {
            if !v {
                parts.push("legends=0".into());
            }
        }
        if let Some(i) = self.maximized_plot {
            parts.push(format!("max={i}"));
        }
        let ymax_pairs: Vec<String> = self
            .y_max_override
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.map(|v| format!("{i}:{}", fmt_t(v))))
            .collect();
        if !ymax_pairs.is_empty() {
            parts.push(format!("ymax={}", ymax_pairs.join(",")));
        }
        parts.join("&")
    }
}

/// Split the splash URL field into one URL per whitespace-separated token.
pub fn split_splash_urls(s: &str) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    for u in s.split_whitespace() {
        if !urls.iter().any(|x| x == u) {
            urls.push(u.to_string());
        }
    }
    urls
}

pub fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Format a float for a URL parameter — strip a trailing `.0` and cap
/// at 6 decimals so the URLs read cleanly (`t=10,20.5` rather than
/// `t=10.000000,20.500000`).
pub fn fmt_t(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        // 6 decimals is enough to address any pixel of a multi-day
        // capture; trim trailing zeros for readability.
        let s = format!("{v:.6}");
        let trimmed = s.trim_end_matches('0').trim_end_matches('.').to_string();
        if trimmed.is_empty() {
            "0".to_string()
        } else {
            trimmed
        }
    }
}

/// Minimal RFC 3986 percent-encoder for URL query values. Encodes
/// every byte that isn't `unreserved` (A-Z a-z 0-9 - _ . ~). Good
/// enough for embedding our short ASCII values *and* the user-supplied
/// `file=` URL; the latter will already include scheme/host/path
/// characters that need escaping (`:` `/` `?` etc.).
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if unreserved {
            out.push(b as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Decoder for `%XX`-style percent-encoding plus the legacy `+` →
/// space convention for query strings. Mirrors `URLSearchParams.get`
/// behavior closely enough for our purposes (parsing our own URLs).
pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `#ee-share-overlay` in `index.html`. A `<textarea>` so Copy / Ctrl+C /
/// the context menu go through the browser.
#[cfg(target_arch = "wasm32")]
const SHARE_OVERLAY_ID: &str = "ee-share-overlay";
#[cfg(target_arch = "wasm32")]
const SHARE_URL_ID: &str = "ee-share-url";
#[cfg(target_arch = "wasm32")]
const SHARE_LINK_ID: &str = "ee-share-link";
#[cfg(target_arch = "wasm32")]
const SHARE_UPLOAD_ID: &str = "ee-share-upload";
#[cfg(target_arch = "wasm32")]
const SHARE_UPLOAD_CACHE_ID: &str = "ee-upload-cache";
#[cfg(target_arch = "wasm32")]
const SHARE_UPLOAD_NOTE_ID: &str = "ee-upload-note";
#[cfg(target_arch = "wasm32")]
const SHARE_UPLOAD_START_ID: &str = "ee-upload-start";
#[cfg(target_arch = "wasm32")]
const SHARE_UPLOAD_PROGRESS_ID: &str = "ee-upload-progress";
#[cfg(target_arch = "wasm32")]
const SHARE_UPLOAD_STATUS_ID: &str = "ee-upload-status";

#[cfg(target_arch = "wasm32")]
fn share_el(id: &str) -> Option<web_sys::Element> {
    web_sys::window()?.document()?.get_element_by_id(id)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn hide_share_overlay() {
    if let Some(overlay) = share_el(SHARE_OVERLAY_ID) {
        let _ = overlay.set_attribute("hidden", "");
        let _ = overlay.remove_attribute("data-close");
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn share_overlay_wants_close() -> bool {
    share_el(SHARE_OVERLAY_ID)
        .and_then(|e| e.get_attribute("data-close"))
        .is_some_and(|v| v == "1")
}

/// What the overlay's upload card should paint, pulled together by
/// `EventExplorerApp::share_upload_view` each frame the popup is open.
#[cfg(target_arch = "wasm32")]
pub(crate) struct ShareUploadView {
    /// Offered cache name, written into the input only on open.
    pub(crate) prefill_cache: String,
    /// A job is in flight: button disabled, progress bar shown.
    pub(crate) busy: bool,
    pub(crate) permille: u32,
    /// Status line (stage, or the failure); `is_error` paints it red.
    pub(crate) status: String,
    pub(crate) is_error: bool,
}

#[cfg(target_arch = "wasm32")]
fn set_hidden(el: Option<web_sys::Element>, hidden: bool) {
    if let Some(el) = el {
        if hidden {
            let _ = el.set_attribute("hidden", "");
        } else {
            let _ = el.remove_attribute("hidden");
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn set_text(el: Option<web_sys::Element>, text: &str) {
    use wasm_bindgen::JsValue;
    if let Some(el) = el {
        let _ = js_sys::Reflect::set(
            &el,
            &JsValue::from_str("textContent"),
            &JsValue::from_str(text),
        );
    }
}

#[cfg(target_arch = "wasm32")]
fn set_value(el: Option<web_sys::Element>, value: &str) {
    use wasm_bindgen::JsValue;
    if let Some(el) = el {
        let _ = js_sys::Reflect::set(&el, &JsValue::from_str("value"), &JsValue::from_str(value));
    }
}

#[cfg(target_arch = "wasm32")]
fn input_value(id: &str) -> String {
    use wasm_bindgen::JsValue;
    share_el(id)
        .and_then(|el| js_sys::Reflect::get(&el, &JsValue::from_str("value")).ok())
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

/// A click on the card's Upload button (signalled via `data-upload`
/// from `index.html`): the cache name and note as typed. Consumed.
#[cfg(target_arch = "wasm32")]
pub(crate) fn share_overlay_upload_request() -> Option<(String, String)> {
    let overlay = share_el(SHARE_OVERLAY_ID)?;
    let req = overlay.get_attribute("data-upload")?;
    let _ = overlay.remove_attribute("data-upload");
    if req != "1" {
        return None;
    }
    Some((
        input_value(SHARE_UPLOAD_CACHE_ID).trim().to_string(),
        input_value(SHARE_UPLOAD_NOTE_ID).trim().to_string(),
    ))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn sync_share_overlay(url: &str, has_file_url: bool, upload: Option<&ShareUploadView>) {
    use wasm_bindgen::{JsCast, JsValue};

    let Some(overlay) = share_el(SHARE_OVERLAY_ID) else {
        return;
    };
    let opening = overlay.has_attribute("hidden");
    let _ = overlay.remove_attribute("hidden");
    if let Some(ta) = share_el(SHARE_URL_ID) {
        let current = js_sys::Reflect::get(&ta, &JsValue::from_str("value"))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        if current != url {
            let _ = js_sys::Reflect::set(&ta, &JsValue::from_str("value"), &JsValue::from_str(url));
        }
        if opening {
            if let Ok(focus) = js_sys::Reflect::get(&ta, &JsValue::from_str("focus")) {
                if let Ok(func) = focus.dyn_into::<js_sys::Function>() {
                    let _ = func.call0(&ta);
                }
            }
            if let Ok(select) = js_sys::Reflect::get(&ta, &JsValue::from_str("select")) {
                if let Ok(func) = select.dyn_into::<js_sys::Function>() {
                    let _ = func.call0(&ta);
                }
            }
        }
    }
    // Link section vs upload card: exactly one is visible.
    set_hidden(share_el(SHARE_LINK_ID), !has_file_url);
    set_hidden(share_el(SHARE_UPLOAD_ID), has_file_url);
    if let Some(u) = upload {
        if opening && !u.prefill_cache.is_empty() {
            set_value(share_el(SHARE_UPLOAD_CACHE_ID), &u.prefill_cache);
        }
        if let Some(btn) = share_el(SHARE_UPLOAD_START_ID) {
            if u.busy {
                let _ = btn.set_attribute("disabled", "");
            } else {
                let _ = btn.remove_attribute("disabled");
            }
        }
        set_hidden(share_el(SHARE_UPLOAD_PROGRESS_ID), !u.busy);
        if let Some(bar) = share_el(SHARE_UPLOAD_PROGRESS_ID) {
            let _ = js_sys::Reflect::set(
                &bar,
                &JsValue::from_str("value"),
                &JsValue::from_f64(u.permille as f64),
            );
        }
        set_hidden(share_el(SHARE_UPLOAD_STATUS_ID), u.status.is_empty());
        set_text(share_el(SHARE_UPLOAD_STATUS_ID), &u.status);
        if let Some(status) = share_el(SHARE_UPLOAD_STATUS_ID) {
            if u.is_error {
                let _ = status.set_attribute("class", "ee-upload-error");
            } else {
                let _ = status.remove_attribute("class");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_file_params_round_trip_in_order() {
        let p = ShareParams::from_query_string(
            "?file=a%2F1.elog&session=7&file=b.elog&file=a%2F1.elog",
        );
        assert_eq!(
            p.file_urls,
            vec!["a/1.elog".to_string(), "b.elog".to_string()]
        );
        let q = p.to_query_string();
        assert_eq!(q, "file=a%2F1.elog&file=b.elog&session=7");
        assert_eq!(ShareParams::from_query_string(&q).file_urls, p.file_urls);
    }
}
