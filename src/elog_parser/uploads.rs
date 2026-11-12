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

//! One-off capture uploads to elog-access's Uploads sharing service
//! (sde-oc-batch `sde_elog_access.uploads`, elog-access ≥ 0.23.0).
//!
//! The service hands out a presigned S3 PUT, so capture bytes never
//! pass through the app server: `POST /api/uploads` signs, the client
//! PUTs straight to S3, and `POST /api/uploads/<id>/complete` checks
//! the header and indexes the upload. A finished upload is reachable
//! at `/api/uploads/<id>/download`, a 307 to a freshly-signed S3 GET,
//! which is what EventExplorer share links embed as `?file=`.
//!
//! Native reaches the API with `metatron curl -provideE2eToken` (the
//! server names the uploader from the initial E2E identity); the
//! browser build calls the same-origin route with SSO cookies. The
//! S3 PUT itself is unsigned-credentialed in both cases (the URL
//! carries its own signature) and needs full-tunnel VPN off-cloud.

#[allow(unused_imports)]
use super::*;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

/// Every fallible upload call fails with a human-readable message.
type UploadResult<T> = std::result::Result<T, String>;

/// Production uploads API on the tseriesui mesh endpoint (native).
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_UPLOADS_BASE_URL: &str =
    "https://tseriesui.cluster.us-east-1.prod.cloud.netflix.net:7004/elog-access/api/uploads";

/// Browser default: the same-origin route the hosting page fronts
/// elog-access under (ocdata, or a tseriesui preview). SSO cookies
/// ride along, as with symbol packs.
#[cfg(target_arch = "wasm32")]
pub const DEFAULT_UPLOADS_BASE_URL: &str = "/elog-access/api/uploads";

/// Canonical host share links point at: the SSO'd browser front door.
/// Each fetch of a `/download` URL re-signs, so links live as long as
/// the upload itself (the bucket's 365-day expiry).
pub const DEFAULT_CANONICAL_BASE_URL: &str =
    "https://ocdata.prod.netflix.net/elog-access/api/uploads";
pub const DEFAULT_EVENTEXPLORER_PAGE: &str = "https://ocdata.prod.netflix.net/eventexplorer/";

/// Environment variables overriding the prod endpoints (testing
/// against previews / local servers). Native only.
pub const UPLOADS_URL_ENV: &str = "ELOG_UPLOADS_URL";
pub const EVENTEXPLORER_PAGE_ENV: &str = "ELOG_EVENTEXPLORER_PAGE";

/// Matches the server's `MAX_UPLOAD_BYTES` (one presigned PUT, no
/// multipart). A server-side change wins: the start call 400s.
pub const MAX_UPLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[cfg(not(target_arch = "wasm32"))]
const METATRON_APP: &str = "tseriesui";

/// Uploads API base: `$ELOG_UPLOADS_URL` or the per-target default.
#[cfg(not(target_arch = "wasm32"))]
pub fn uploads_base_url() -> String {
    std::env::var(UPLOADS_URL_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_UPLOADS_BASE_URL.to_string())
}

/// Uploads API base on web: the same-origin default.
#[cfg(target_arch = "wasm32")]
pub fn uploads_base_url() -> String {
    DEFAULT_UPLOADS_BASE_URL.to_string()
}

/// Base share links embed in `?file=`: the ocdata front door, or the
/// overridden API base when testing.
#[cfg(not(target_arch = "wasm32"))]
pub fn canonical_base_url() -> String {
    std::env::var(UPLOADS_URL_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CANONICAL_BASE_URL.to_string())
}

/// Canonical base on web: ocdata (absolute, so the link works from
/// anywhere the recipient can reach SSO).
#[cfg(target_arch = "wasm32")]
pub fn canonical_base_url() -> String {
    DEFAULT_CANONICAL_BASE_URL.to_string()
}

/// EventExplorer page share links open.
#[cfg(not(target_arch = "wasm32"))]
pub fn eventexplorer_page_url() -> String {
    std::env::var(EVENTEXPLORER_PAGE_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_EVENTEXPLORER_PAGE.to_string())
}

/// EventExplorer page share links open (web).
#[cfg(target_arch = "wasm32")]
pub fn eventexplorer_page_url() -> String {
    DEFAULT_EVENTEXPLORER_PAGE.to_string()
}

/// The long-lived GET link for one upload: a 307 to a presigned S3
/// GET, re-signed per fetch. `#name` is a display hint only — never
/// sent to the server; `url_file_name` prefers it over the path tail.
pub fn canonical_download_url(upload_id: &str, file_name: &str) -> String {
    let mut url = format!(
        "{}/{}/download",
        canonical_base_url().trim_end_matches('/'),
        upload_id
    );
    if !file_name.is_empty() {
        url.push('#');
        url.push_str(file_name);
    }
    url
}

/// Build an EventExplorer share link for uploaded captures.
/// `extra_query` is an already-encoded query tail (view state) and
/// may be empty.
pub fn eventexplorer_link(file_urls: &[String], extra_query: &str) -> String {
    let mut q = String::new();
    for u in file_urls {
        if !q.is_empty() {
            q.push('&');
        }
        q.push_str("file=");
        q.push_str(&query_escape(u));
    }
    if !extra_query.is_empty() {
        if !q.is_empty() {
            q.push('&');
        }
        q.push_str(extra_query);
    }
    let page = eventexplorer_page_url();
    if q.is_empty() {
        page
    } else {
        format!("{page}?{q}")
    }
}

/// Minimal RFC 3986 percent-encoder for query values (same alphabet
/// as `app::share`'s encoder).
pub fn query_escape(s: &str) -> String {
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

/// Cache-name inference from a capture file name, ported from
/// elog-access's `uploads.js`: `c174.ord001.dev-eventlog-…` yields
/// `c174.ord001.dev`. Empty when the name doesn't lead with an
/// appliance-style prefix.
pub fn infer_cache_name(file_name: &str) -> String {
    let name = file_name.rsplit('/').next().unwrap_or(file_name);
    let lower = name.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut i = 0;
    if b.is_empty() || b[0] != b'c' {
        return String::new();
    }
    i += 1;
    let digits = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits || i >= b.len() || b[i] != b'.' {
        return String::new();
    }
    i += 1;
    if i + 3 > b.len() || !b[i..i + 3].iter().all(|c| c.is_ascii_lowercase()) {
        return String::new();
    }
    i += 3;
    if i + 3 > b.len() || !b[i..i + 3].iter().all(|c| c.is_ascii_digit()) {
        return String::new();
    }
    i += 3;
    if i >= b.len() || b[i] != b'.' {
        return String::new();
    }
    i += 1;
    let site = i;
    while i < b.len() && (b[i].is_ascii_lowercase() || b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i == site || i >= b.len() || b[i] != b'-' {
        return String::new();
    }
    name[..i].to_string()
}

/// What the signing call returns: the presigned PUT target.
#[derive(Debug, Clone, Default)]
pub struct UploadStart {
    pub upload_id: String,
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub expires_in: u64,
}

/// One finished upload, as its index record reads.
#[derive(Debug, Clone, Default)]
pub struct UploadRecord {
    pub upload_id: String,
    pub user: String,
    pub cache_name: String,
    pub file_name: String,
    pub size: u64,
    pub note: String,
    pub etag: String,
}

impl UploadRecord {
    /// The `?file=` URL a share link embeds for this upload.
    pub fn canonical_url(&self) -> String {
        canonical_download_url(&self.upload_id, &self.file_name)
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn start_body(cache_name: &str, file_name: &str, size: u64, note: &str) -> String {
    format!(
        "{{\"cache_name\":\"{}\",\"file_name\":\"{}\",\"content_length\":{},\"note\":\"{}\"}}",
        json_escape(cache_name),
        json_escape(file_name),
        size,
        json_escape(note)
    )
}

/// Surface FastAPI's `detail` (a string, or the validation-error
/// array) as the error text; fall back to a trimmed body snippet.
fn api_error(text: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(d) = v.get("detail") {
            if let Some(s) = d.as_str() {
                return s.to_string();
            }
            if let Some(arr) = d.as_array() {
                let msgs: Vec<String> = arr
                    .iter()
                    .filter_map(|e| e.get("msg").and_then(|m| m.as_str()).map(str::to_string))
                    .collect();
                if !msgs.is_empty() {
                    return msgs.join("; ");
                }
            }
        }
    }
    let trimmed = text.trim();
    if trimmed.chars().count() > 300 {
        format!("{}…", trimmed.chars().take(300).collect::<String>())
    } else {
        trimmed.to_string()
    }
}

fn parse_start_response(text: &str) -> UploadResult<UploadStart> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| api_error(text))?;
    let upload_id = v
        .get("upload_id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let url = v
        .get("url")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if upload_id.is_empty() || url.is_empty() {
        return Err(api_error(text));
    }
    let method = v
        .get("method")
        .and_then(|x| x.as_str())
        .unwrap_or("PUT")
        .to_string();
    let headers = v
        .get("headers")
        .and_then(|h| h.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(0);
    Ok(UploadStart {
        upload_id,
        url,
        method,
        headers,
        expires_in,
    })
}

fn parse_record(v: &serde_json::Value) -> UploadRecord {
    let s = |key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    UploadRecord {
        upload_id: s("upload_id"),
        user: s("user"),
        cache_name: s("cache_name"),
        file_name: s("file_name"),
        size: v.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
        note: s("note"),
        etag: s("etag"),
    }
}

fn parse_complete_response(text: &str) -> UploadResult<UploadRecord> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| api_error(text))?;
    let Some(u) = v.get("upload") else {
        return Err(api_error(text));
    };
    Ok(parse_record(u))
}

// ---------------------------------------------------------------------------
// Native transport: metatron for the API, plain curl for the S3 PUT.
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
fn metatron_json(method: &str, path: &str, body: Option<&str>) -> UploadResult<String> {
    let url = format!("{}{}", uploads_base_url().trim_end_matches('/'), path);
    let mut cmd = std::process::Command::new("metatron");
    cmd.arg("curl")
        .arg("-a")
        .arg(METATRON_APP)
        .arg("-provideE2eToken");
    if method != "GET" {
        cmd.arg("-X").arg(method);
    }
    if let Some(b) = body {
        cmd.arg("-H")
            .arg("Content-Type: application/json")
            .arg("-d")
            .arg(b);
    }
    cmd.arg(&url);
    let out = cmd
        .output()
        .map_err(|e| format!("could not run `metatron`: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("request to {url} failed")
        } else {
            err
        });
    }
    Ok(text)
}

/// The signed-in user and the server's size cap, or an error naming
/// why uploads won't work (metatron missing, not signed in).
#[cfg(not(target_arch = "wasm32"))]
pub fn uploads_whoami() -> UploadResult<(String, u64)> {
    let text = metatron_json("GET", "/whoami", None)?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| api_error(&text))?;
    let user = v
        .get("user")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    if user.is_empty() {
        return Err("elog-access says you are not signed in".to_string());
    }
    let max = v
        .get("max_bytes")
        .and_then(|b| b.as_u64())
        .unwrap_or(MAX_UPLOAD_BYTES);
    Ok((user, max))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn start_upload(
    cache_name: &str,
    file_name: &str,
    size: u64,
    note: &str,
) -> UploadResult<UploadStart> {
    let body = start_body(cache_name, file_name, size, note);
    parse_start_response(&metatron_json("POST", "", Some(&body))?)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn complete_upload(upload_id: &str) -> UploadResult<UploadRecord> {
    parse_complete_response(&metatron_json(
        "POST",
        &format!("/{upload_id}/complete"),
        Some("{}"),
    )?)
}

/// One `--progress-bar` fragment: `#####…  42.3%` → 423 permille.
#[cfg(not(target_arch = "wasm32"))]
fn parse_curl_progress(fragment: &str) -> Option<u32> {
    let pct = fragment.trim().strip_suffix('%')?;
    let num = pct
        .trim_start_matches(|c: char| c == '#' || c.is_whitespace())
        .trim();
    let v: f32 = num.parse().ok()?;
    Some((v.clamp(0.0, 100.0) * 10.0) as u32)
}

#[cfg(not(target_arch = "wasm32"))]
fn s3_error_message(body: &[u8], code: u16) -> String {
    let text = String::from_utf8_lossy(body);
    let msg = text
        .split("<Message>")
        .nth(1)
        .and_then(|r| r.split("</Message>").next());
    match msg {
        Some(m) => format!("S3 refused the upload (HTTP {code}): {m}"),
        None => format!("S3 refused the upload (HTTP {code})"),
    }
}

/// PUT `path` to the presigned URL, reporting permille progress.
/// `curl -T` sets Content-Length from the file, matching the length
/// the signature covers; the presigned headers are replayed verbatim.
#[cfg(not(target_arch = "wasm32"))]
fn s3_put_file(
    path: &std::path::Path,
    start: &UploadStart,
    on_progress: &dyn Fn(u32),
) -> UploadResult<()> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new("curl");
    cmd.arg("-X")
        .arg("PUT")
        .arg("--progress-bar")
        .arg("-w")
        .arg("\n%{http_code}")
        .arg("-T")
        .arg(path);
    for (name, value) in &start.headers {
        cmd.arg("-H").arg(format!("{name}: {value}"));
    }
    cmd.arg(&start.url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not run curl: {e}"))?;
    let mut stderr = child.stderr.take().expect("stderr piped");
    let mut frag = String::new();
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                for &b in &buf[..n] {
                    if b == b'\r' || b == b'\n' {
                        if let Some(p) = parse_curl_progress(&frag) {
                            on_progress(p);
                        }
                        frag.clear();
                    } else {
                        frag.push(b as char);
                    }
                }
            }
            Err(_) => break,
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl wait failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "the upload did not reach S3 (curl {}); S3 PUTs need full-tunnel VPN",
            out.status
        ));
    }
    let Some((body, code)) = split_curl_body(&out.stdout) else {
        return Err("could not read curl's HTTP status".to_string());
    };
    if (200..300).contains(&code) {
        return Ok(());
    }
    Err(s3_error_message(&body, code))
}

/// Upload one capture file end to end: sign, PUT to S3, finish.
/// `progress` receives permille within `window` (so a multi-file
/// caller can give each file a slice of one bar); `status` gets a
/// short stage label.
#[cfg(not(target_arch = "wasm32"))]
pub fn upload_file(
    path: &std::path::Path,
    cache_name: &str,
    note: &str,
    progress: &Arc<AtomicU32>,
    window: (u32, u32),
    status: &(dyn Fn(&str) + Sync),
) -> UploadResult<UploadRecord> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("cannot stat {}: {e}", path.display()))?
        .len();
    if size == 0 || size > MAX_UPLOAD_BYTES {
        return Err(format!(
            "{} is {} bytes; uploads are 1 byte..{} GiB",
            path.display(),
            size,
            MAX_UPLOAD_BYTES / (1024 * 1024 * 1024)
        ));
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "capture.elog".to_string());
    status("Signing upload…");
    let start = start_upload(cache_name, &file_name, size, note)?;
    let (lo, hi) = window;
    let put_hi = lo + (hi - lo) * 9 / 10;
    status("Uploading to S3…");
    s3_put_file(path, &start, &|p| {
        progress.store(lo + (put_hi - lo) * p / 1000, Ordering::Relaxed);
    })?;
    status("Finishing…");
    let record = complete_upload(&start.upload_id)?;
    progress.store(hi, Ordering::Relaxed);
    status("Uploaded");
    Ok(record)
}

// ---------------------------------------------------------------------------
// Web transport: XHR for both hops (SSO cookies for the API, the
// presigned URL carries its own credentials for S3).
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
async fn xhr_json(method: &str, url: &str, body: Option<&str>) -> UploadResult<String> {
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;
    use web_sys::XmlHttpRequest;

    let xhr = XmlHttpRequest::new().map_err(|e| format!("XMLHttpRequest: {e:?}"))?;
    xhr.open_with_async(method, url, true)
        .map_err(|e| format!("XHR open failed: {e:?}"))?;
    let _ = xhr.set_with_credentials(true);
    if body.is_some() {
        xhr.set_request_header("Content-Type", "application/json")
            .map_err(|e| format!("XHR header failed: {e:?}"))?;
    }
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let xhr_load = xhr.clone();
        let reject_err = reject.clone();
        let onload = wasm_bindgen::closure::Closure::once(move || {
            let code = xhr_load.status().unwrap_or(0);
            let text = xhr_load
                .response_text()
                .unwrap_or_default()
                .unwrap_or_default();
            if (200..300).contains(&code) {
                let _ = resolve.call1(&JsValue::UNDEFINED, &JsValue::from_str(&text));
            } else {
                let _ =
                    reject_err.call1(&JsValue::UNDEFINED, &JsValue::from_str(&api_error(&text)));
            }
        });
        xhr.set_onload(Some(onload.as_ref().unchecked_ref()));
        onload.forget();
        let onerror = wasm_bindgen::closure::Closure::once(move || {
            let _ = reject.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str("network error reaching elog-access"),
            );
        });
        xhr.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();
    });
    xhr.send_with_opt_str(body)
        .map_err(|e| format!("XHR send failed: {e:?}"))?;
    let value = JsFuture::from(promise).await.map_err(|e| {
        e.as_string()
            .unwrap_or_else(|| format!("request rejected: {e:?}"))
    })?;
    Ok(value.as_string().unwrap_or_default())
}

#[cfg(target_arch = "wasm32")]
async fn xhr_put_blob(
    start: &UploadStart,
    blob: &web_sys::Blob,
    on_progress: std::rc::Rc<dyn Fn(u32)>,
) -> UploadResult<()> {
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{ProgressEvent, XmlHttpRequest};

    let xhr = XmlHttpRequest::new().map_err(|e| format!("XMLHttpRequest: {e:?}"))?;
    let method = if start.method.is_empty() {
        "PUT"
    } else {
        start.method.as_str()
    };
    xhr.open_with_async(method, &start.url, true)
        .map_err(|e| format!("XHR open failed: {e:?}"))?;
    for (name, value) in &start.headers {
        xhr.set_request_header(name, value)
            .map_err(|e| format!("XHR header failed: {e:?}"))?;
    }
    if let Ok(upload) = xhr.upload() {
        let cb = wasm_bindgen::closure::Closure::<dyn FnMut(ProgressEvent)>::new(
            move |e: ProgressEvent| {
                if e.length_computable() && e.total() > 0.0 {
                    on_progress(((e.loaded() / e.total()) * 1000.0) as u32);
                }
            },
        );
        upload.set_onprogress(Some(cb.as_ref().unchecked_ref()));
        cb.forget();
    }
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let xhr_load = xhr.clone();
        let reject_err = reject.clone();
        let onload = wasm_bindgen::closure::Closure::once(move || {
            let code = xhr_load.status().unwrap_or(0);
            if (200..300).contains(&code) {
                let _ = resolve.call0(&JsValue::UNDEFINED);
            } else {
                let text = xhr_load
                    .response_text()
                    .unwrap_or_default()
                    .unwrap_or_default();
                let _ = reject_err.call1(
                    &JsValue::UNDEFINED,
                    &JsValue::from_str(&format!(
                        "S3 refused the upload (HTTP {code}); it needs full-tunnel VPN. {text}"
                    )),
                );
            }
        });
        xhr.set_onload(Some(onload.as_ref().unchecked_ref()));
        onload.forget();
        let onerror = wasm_bindgen::closure::Closure::once(move || {
            let _ = reject.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str("the upload did not reach S3; it needs full-tunnel VPN"),
            );
        });
        xhr.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();
    });
    xhr.send_with_opt_blob(Some(blob))
        .map_err(|e| format!("XHR send failed: {e:?}"))?;
    JsFuture::from(promise).await.map_err(|e| {
        e.as_string()
            .unwrap_or_else(|| format!("upload rejected: {e:?}"))
    })?;
    Ok(())
}

/// Upload one browser-picked file end to end. `on_progress` receives
/// permille plus a short stage label.
#[cfg(target_arch = "wasm32")]
pub async fn upload_file_web(
    file: &web_sys::File,
    cache_name: &str,
    note: &str,
    on_progress: std::rc::Rc<dyn Fn(u32, &str)>,
) -> UploadResult<UploadRecord> {
    use wasm_bindgen::JsCast;

    let size = file.size() as u64;
    if size == 0 || size > MAX_UPLOAD_BYTES {
        return Err(format!(
            "{} is too large for one upload ({} GiB cap)",
            file.name(),
            MAX_UPLOAD_BYTES / (1024 * 1024 * 1024)
        ));
    }
    let base = uploads_base_url();
    let base = base.trim_end_matches('/');
    on_progress(0, "Signing upload…");
    let start = parse_start_response(
        &xhr_json(
            "POST",
            base,
            Some(&start_body(cache_name, &file.name(), size, note)),
        )
        .await?,
    )?;
    on_progress(0, "Uploading to S3…");
    let blob: &web_sys::Blob = file.unchecked_ref();
    let put_progress: std::rc::Rc<dyn Fn(u32)> = std::rc::Rc::new({
        let on_progress = on_progress.clone();
        move |p| on_progress(p * 9 / 10, "Uploading to S3…")
    });
    xhr_put_blob(&start, blob, put_progress).await?;
    on_progress(950, "Finishing…");
    let record = parse_complete_response(
        &xhr_json(
            "POST",
            &format!("{base}/{}/complete", start.upload_id),
            Some("{}"),
        )
        .await?,
    )?;
    on_progress(1000, "Uploaded");
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escape_covers_specials() {
        assert_eq!(json_escape("a\"b\\c\nd\te"), "a\\\"b\\\\c\\nd\\te");
        assert_eq!(json_escape("plain"), "plain");
        assert_eq!(json_escape("üní"), "üní");
    }

    #[test]
    fn start_body_is_valid_json() {
        let body = start_body("c658.sjc002.dev", "a\"b.elog", 42, "n\n");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["cache_name"], "c658.sjc002.dev");
        assert_eq!(v["file_name"], "a\"b.elog");
        assert_eq!(v["content_length"], 42);
        assert_eq!(v["note"], "n\n");
    }

    #[test]
    fn parse_start_response_reads_put_target() {
        let text = r#"{
            "upload_id": "20261018-171500-deadbeef",
            "url": "https://s3.example/put?X-Amz-Signature=abc",
            "method": "PUT",
            "headers": {"Content-Type": "application/octet-stream", "If-None-Match": "*"},
            "expires_in": 900,
            "upload": {"upload_id": "20261018-171500-deadbeef"}
        }"#;
        let start = parse_start_response(text).unwrap();
        assert_eq!(start.upload_id, "20261018-171500-deadbeef");
        assert_eq!(start.method, "PUT");
        assert_eq!(start.headers.len(), 2);
        assert_eq!(start.expires_in, 900);
    }

    #[test]
    fn parse_complete_response_reads_record() {
        let text = r#"{"upload": {
            "upload_id": "20261018-171500-deadbeef",
            "user": "nickbanks@netflix.com",
            "cache_name": "c658.sjc002.dev.nflxvideo.net",
            "file_name": "run.elog.gz",
            "size": 12345,
            "note": "repro",
            "etag": "abc"
        }}"#;
        let rec = parse_complete_response(text).unwrap();
        assert_eq!(rec.user, "nickbanks@netflix.com");
        assert_eq!(rec.cache_name, "c658.sjc002.dev.nflxvideo.net");
        assert_eq!(rec.size, 12345);
        assert_eq!(rec.etag, "abc");
    }

    #[test]
    fn api_error_extracts_detail_string_and_array() {
        assert_eq!(
            api_error(r#"{"detail":"sign in to upload"}"#),
            "sign in to upload"
        );
        let arr = r#"{"detail":[{"loc":["body","cache_name"],"msg":"field required"}]}"#;
        assert_eq!(api_error(arr), "field required");
        assert!(api_error("not json at all").contains("not json"));
    }

    #[test]
    fn parse_start_response_surfaces_api_errors() {
        let err = parse_start_response(r#"{"detail":"size must be 1..N bytes"}"#).unwrap_err();
        assert_eq!(err, "size must be 1..N bytes");
    }

    #[test]
    fn infer_cache_name_matches_elog_access_rule() {
        assert_eq!(
            infer_cache_name("c174.ord001.dev-eventlog-20261013.elog.gz"),
            "c174.ord001.dev"
        );
        assert_eq!(
            infer_cache_name("c658.sjc002.dev-20260918-085841-nginx.elog"),
            "c658.sjc002.dev"
        );
        assert_eq!(
            infer_cache_name("/tmp/C658.SJC002.dev-x.elog"),
            "C658.SJC002.dev"
        );
        assert_eq!(infer_cache_name("foo.elog"), "");
        assert_eq!(infer_cache_name("c658.elog"), "");
        assert_eq!(infer_cache_name(""), "");
    }

    #[test]
    fn query_escape_encodes_like_share_urls() {
        assert_eq!(query_escape("a/b?c=d e"), "a%2Fb%3Fc%3Dd%20e");
        assert_eq!(query_escape("plain-1.elog_gz~"), "plain-1.elog_gz~");
    }

    #[test]
    fn eventexplorer_link_combines_files_and_tail() {
        let link = eventexplorer_link(
            &[
                "https://ocdata.prod.netflix.net/elog-access/api/uploads/id1/download#a.elog"
                    .into(),
                "https://ocdata.prod.netflix.net/elog-access/api/uploads/id2/download#b.elog"
                    .into(),
            ],
            "session=7&t=1,2",
        );
        assert!(link.starts_with(DEFAULT_EVENTEXPLORER_PAGE));
        assert!(link.contains("file=https%3A%2F%2Focdata.prod.netflix.net%2Felog-access%2Fapi%2Fuploads%2Fid1%2Fdownload%23a.elog"));
        assert!(link.contains("&file="));
        assert!(link.ends_with("&session=7&t=1,2"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn curl_progress_fragments_parse() {
        assert_eq!(
            parse_curl_progress("#####################################  42.3%"),
            Some(423)
        );
        assert_eq!(parse_curl_progress("##  0.0%"), Some(0));
        assert_eq!(
            parse_curl_progress(
                "######################################################################## 100.0%"
            ),
            Some(1000)
        );
        assert_eq!(parse_curl_progress("curl: (7) connection refused"), None);
        assert_eq!(parse_curl_progress(""), None);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn s3_error_message_pulls_xml_message() {
        let body = br#"<?xml version="1.0"?><Error><Code>AccessDenied</Code><Message>denied by rule</Message></Error>"#;
        assert_eq!(
            s3_error_message(body, 403),
            "S3 refused the upload (HTTP 403): denied by rule"
        );
        assert_eq!(
            s3_error_message(b"oops", 500),
            "S3 refused the upload (HTTP 500)"
        );
    }
}
