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

//! Symbol packs: content-addressed `.elog` files holding one binary's
//! function ranges, fetched by GNU build-id.
//!
//! A reference-mode capture keeps `SYMBOL_PACK_REF` rows instead of
//! ranges. For each ref whose file has no in-file ranges, fetch the
//! named pack from the symbol catalog and install its ranges under the
//! referencing file id. In-file ranges always win; a fetch failure
//! leaves the file's frames at the raw `kernel+0xOFF` fallback.
//!
//! Hits are stored under the user cache directory, keyed by pack id
//! (`~/.cache/event_explorer/symbol-packs/<id>`). A confirmed 404 is
//! remembered for six hours so reopening a capture does not
//! repeat those lookups. `ELOG_SYMBOLS_CACHE=off` skips the cache;
//! `ELOG_SYMBOLS_CACHE_DIR` overrides the directory.
//!
//! Packs are gzip-compressed at rest; fetches inflate on the way in,
//! so the cache and parser only see raw `.elog` bytes. The catalog
//! holds only compressed objects; anything else is a fetch error.

#[allow(unused_imports)]
use super::*;

/// Default symbol-catalog base URL (elog-access on the tseriesui app).
/// `<base>/<build_id>` 307-redirects to a one-hour presigned S3 GET.
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_SYMBOLS_BASE_URL: &str =
    "https://tseriesui.cluster.us-east-1.prod.cloud.netflix.net:7004/elog-access/api/symbols";

/// Browser default: the same-origin route ocdata fronts elog-access
/// under. SSO cookies ride along, the 307 to S3 passes CORS (the
/// bucket allows the ocdata origin), and a non-ocdata host just 404s.
#[cfg(target_arch = "wasm32")]
pub const DEFAULT_SYMBOLS_BASE_URL: &str = "/elog-access/api/symbols";

/// Metatron application the catalog endpoint lives behind.
const METATRON_APP: &str = "tseriesui";

/// Environment variable overriding [`DEFAULT_SYMBOLS_BASE_URL`].
pub const SYMBOLS_URL_ENV: &str = "ELOG_SYMBOLS_URL";

/// Base URL for pack fetches: `$ELOG_SYMBOLS_URL` or the default.
#[cfg(not(target_arch = "wasm32"))]
pub fn symbols_base_url() -> String {
    std::env::var(SYMBOLS_URL_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_SYMBOLS_BASE_URL.to_string())
}

/// Base URL for pack fetches on web: the same-origin default.
#[cfg(target_arch = "wasm32")]
pub fn symbols_base_url() -> String {
    DEFAULT_SYMBOLS_BASE_URL.to_string()
}

/// Concurrent catalog GETs. Each pack is one small object, so the
/// wait is round-trips, not bytes.
#[cfg(not(target_arch = "wasm32"))]
const PACK_FETCH_THREADS: usize = 64;

/// Where one pack's bytes came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackFetchKind {
    /// Read from the on-disk cache.
    Cached,
    /// Catalog GET that returned a pack.
    Downloaded,
    /// Catalog 404, or a cached 404 that is still fresh.
    Missing,
    /// Transport error.
    Failed,
}

/// One catalog lookup. `bytes` is set for [`PackFetchKind::Cached`]
/// and [`PackFetchKind::Downloaded`].
#[derive(Debug)]
pub struct FetchedPack {
    pub bytes: Option<Vec<u8>>,
    pub kind: PackFetchKind,
}

/// Result of one [`install_packs`] pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct PackInstallStats {
    /// `SYMBOL_PACK_REF` rows considered.
    pub refs: usize,
    /// Files that already carried in-file ranges (in-file wins).
    pub already_inline: usize,
    /// Files whose pack ranges were installed.
    pub installed: usize,
    /// Refs whose pack could not be fetched or parsed.
    pub failed: usize,
}

/// One fetched pack, parsed as the tiny v3 `.elog` the publisher wrote
/// (range rows for one binary, `file_id` fixed at 1).
struct ParsedPack {
    symbols: FxHashMap<u32, Vec<SymbolRange>>,
    user_symbols: FxHashMap<u32, Vec<UserSymbolRange>>,
}

impl ParsedPack {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let capture = parse_elog_core(bytes, None).ok()?;
        let profile = capture.3;
        Some(Self {
            symbols: profile.symbols,
            user_symbols: profile.user_symbols,
        })
    }
}

/// Install pack ranges for every ref whose file lacks in-file ranges,
/// fetching packs from `base_url`. Repeated refs naming one pack fetch
/// it once.
pub fn install_packs(profile: &mut CpuProfile, base_url: &str) -> PackInstallStats {
    let ids = pack_ids_missing_ranges(profile);
    let bytes = fetch_pack_bytes(&ids, base_url);
    install_fetched(profile, &bytes)
}

/// [`install_packs`] that calls `on_done(finished, total)` after each
/// pack id returns. `total` counts distinct ids that still need a pack.
#[cfg(not(target_arch = "wasm32"))]
pub fn install_packs_reporting(
    profile: &mut CpuProfile,
    base_url: &str,
    on_done: &(dyn Fn(u32, u32) + Sync),
) -> PackInstallStats {
    let ids = pack_ids_missing_ranges(profile);
    let total = ids.len() as u32;
    if total == 0 {
        return PackInstallStats::default();
    }
    let done = AtomicU32::new(0);
    let bytes = Mutex::new(FxHashMap::default());
    let cancel = std::sync::atomic::AtomicBool::new(false);
    fetch_packs_each(ids, base_url, &cancel, &|id, fetched| {
        if let Some(b) = fetched.bytes {
            bytes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_string(), b);
        }
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        on_done(n, total);
    });
    let bytes = bytes.into_inner().unwrap_or_else(|e| e.into_inner());
    install_fetched(profile, &bytes)
}

/// Pack ids whose file has no in-file ranges yet. One entry per id.
pub fn pack_ids_missing_ranges(profile: &CpuProfile) -> Vec<String> {
    let mut seen = FxHashSet::default();
    let mut ids = Vec::new();
    for r in &profile.pack_refs {
        let has_ranges = if r.kind == SymbolPackRef::KIND_LINKER {
            profile
                .symbols
                .get(&r.file_id)
                .is_some_and(|v| !v.is_empty())
        } else {
            profile
                .user_symbols
                .get(&r.file_id)
                .is_some_and(|v| !v.is_empty())
        };
        if has_ranges {
            continue;
        }
        if seen.insert(r.pack_id.clone()) {
            ids.push(r.pack_id.clone());
        }
    }
    ids
}

/// Fetch `ids` from the catalog. Native builds issue up to
/// [`PACK_FETCH_THREADS`] requests at once. The wasm build has no
/// catalog client, so every id misses.
pub fn fetch_pack_bytes(ids: &[String], base_url: &str) -> FxHashMap<String, Vec<u8>> {
    if ids.is_empty() {
        return FxHashMap::default();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let threads = PACK_FETCH_THREADS.min(ids.len()).max(1);
        let fetched = |ids: &[String]| -> FxHashMap<String, Vec<u8>> {
            use rayon::prelude::*;
            ids.par_iter()
                .filter_map(|id| {
                    fetch_pack(id, base_url)
                        .bytes
                        .map(|bytes| (id.clone(), bytes))
                })
                .collect()
        };
        match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
            Ok(pool) => pool.install(|| fetched(ids)),
            Err(_) => ids
                .iter()
                .filter_map(|id| {
                    fetch_pack(id, base_url)
                        .bytes
                        .map(|bytes| (id.clone(), bytes))
                })
                .collect(),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (ids, base_url);
        FxHashMap::default()
    }
}

/// Fetch `ids`, calling `on_each` as each one returns. Shorter ids
/// (GNU build-ids) run before longer content hashes. `cancel` skips
/// ids that have not started.
pub fn fetch_packs_each(
    mut ids: Vec<String>,
    base_url: &str,
    cancel: &std::sync::atomic::AtomicBool,
    on_each: &(dyn Fn(&str, FetchedPack) + Sync),
) {
    if ids.is_empty() {
        return;
    }
    ids.sort_by_key(|id| id.len());
    #[cfg(not(target_arch = "wasm32"))]
    {
        let threads = PACK_FETCH_THREADS.min(ids.len()).max(1);
        let run = || {
            use rayon::prelude::*;
            ids.par_iter().for_each(|id| {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                on_each(id, fetch_pack(id, base_url));
            });
        };
        if let Ok(pool) = rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
            pool.install(run);
        } else {
            for id in &ids {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                on_each(id, fetch_pack(id, base_url));
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (ids, base_url, cancel, on_each);
    }
}

/// Install one pack's ranges under every ref that names `pack_id`.
/// Returns how many files gained ranges.
pub fn install_one_pack(profile: &CpuProfile, pack_id: &str, bytes: &[u8]) -> usize {
    let Some(pack) = ParsedPack::parse(bytes) else {
        return 0;
    };
    let mut installed = 0;
    for r in &profile.pack_refs {
        if r.pack_id != pack_id {
            continue;
        }
        let is_linker = r.kind == SymbolPackRef::KIND_LINKER;
        let has_ranges = if is_linker {
            profile
                .symbols
                .get(&r.file_id)
                .is_some_and(|v| !v.is_empty())
        } else {
            profile
                .user_symbols
                .get(&r.file_id)
                .is_some_and(|v| !v.is_empty())
        };
        if has_ranges {
            continue;
        }
        if is_linker {
            if let Some(ranges) = pack.symbols.get(&1) {
                profile.pack_symbols.insert(r.file_id, ranges.clone());
                installed += 1;
            }
        } else if let Some(ranges) = pack.user_symbols.get(&1) {
            profile.pack_user_symbols.insert(r.file_id, ranges.clone());
            installed += 1;
        }
    }
    installed
}

/// Install ranges from packs already in `bytes`. Missing ids count as
/// fetch failures.
pub fn install_fetched(
    profile: &mut CpuProfile,
    bytes: &FxHashMap<String, Vec<u8>>,
) -> PackInstallStats {
    install_packs_with(profile, &mut |pack_id| bytes.get(pack_id).cloned())
}

/// [`install_packs`] with the fetch step injected (tests serve packs
/// from memory; production uses [`fetch_pack`]).
pub fn install_packs_with(
    profile: &mut CpuProfile,
    fetch: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
) -> PackInstallStats {
    let mut stats = PackInstallStats::default();
    let refs = std::mem::take(&mut profile.pack_refs);
    stats.refs = refs.len();

    let mut cache: FxHashMap<String, Option<ParsedPack>> = FxHashMap::default();
    for r in &refs {
        let is_linker = r.kind == SymbolPackRef::KIND_LINKER;
        let has_ranges = if is_linker {
            profile
                .symbols
                .get(&r.file_id)
                .is_some_and(|v| !v.is_empty())
        } else {
            profile
                .user_symbols
                .get(&r.file_id)
                .is_some_and(|v| !v.is_empty())
        };
        if has_ranges {
            stats.already_inline += 1;
            continue;
        }
        let pack = cache
            .entry(r.pack_id.clone())
            .or_insert_with(|| fetch(&r.pack_id).and_then(|b| ParsedPack::parse(&b)));
        let Some(pack) = pack else {
            stats.failed += 1;
            continue;
        };
        if is_linker {
            if let Some(ranges) = pack.symbols.get(&1) {
                profile.pack_symbols.insert(r.file_id, ranges.clone());
                stats.installed += 1;
            }
        } else if let Some(ranges) = pack.user_symbols.get(&1) {
            profile.pack_user_symbols.insert(r.file_id, ranges.clone());
            stats.installed += 1;
        }
    }

    profile.pack_refs = refs;
    stats
}

/// How long a confirmed catalog 404 is reused before the id is
/// looked up again. Hits are content-addressed and stay until deleted.
#[cfg(not(target_arch = "wasm32"))]
const CACHE_MISS_TTL: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// `off` or `0` disables the on-disk pack cache.
#[cfg(not(target_arch = "wasm32"))]
const CACHE_DISABLE_ENV: &str = "ELOG_SYMBOLS_CACHE";

/// Directory that holds cached packs. Unset uses the user cache dir.
#[cfg(not(target_arch = "wasm32"))]
const CACHE_DIR_ENV: &str = "ELOG_SYMBOLS_CACHE_DIR";

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
enum CachedPack {
    Hit(Vec<u8>),
    Miss,
    Unknown,
}

/// On-disk cache root, or `None` when caching is disabled.
#[cfg(not(target_arch = "wasm32"))]
fn cache_root() -> Option<std::path::PathBuf> {
    if let Ok(v) = std::env::var(CACHE_DISABLE_ENV) {
        if v == "0" || v.eq_ignore_ascii_case("off") {
            return None;
        }
    }
    if let Ok(dir) = std::env::var(CACHE_DIR_ENV) {
        if !dir.is_empty() {
            return Some(std::path::PathBuf::from(dir));
        }
    }
    dirs::cache_dir().map(|d| d.join("event_explorer").join("symbol-packs"))
}

/// Lowercase hex build-id (40) or file sha256 (64). Anything else is
/// refused so a pack id from a capture cannot escape the cache directory.
#[cfg(not(target_arch = "wasm32"))]
fn pack_cache_id(id: &str) -> Option<String> {
    let id = id.to_ascii_lowercase();
    if (id.len() == 40 || id.len() == 64) && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(id)
    } else {
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn cache_lookup(root: &std::path::Path, id: &str) -> CachedPack {
    let Some(id) = pack_cache_id(id) else {
        return CachedPack::Unknown;
    };
    let hit_path = root.join(&id);
    if let Ok(bytes) = std::fs::read(&hit_path) {
        if bytes.starts_with(b"ELOG") {
            return CachedPack::Hit(bytes);
        }
        let _ = std::fs::remove_file(&hit_path);
    }
    let miss_path = root.join(format!("{id}.miss"));
    if let Ok(meta) = std::fs::metadata(&miss_path) {
        let fresh = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < CACHE_MISS_TTL);
        if fresh {
            return CachedPack::Miss;
        }
        let _ = std::fs::remove_file(&miss_path);
    }
    CachedPack::Unknown
}

#[cfg(not(target_arch = "wasm32"))]
fn cache_store_hit(root: &std::path::Path, id: &str, bytes: &[u8]) {
    let Some(id) = pack_cache_id(id) else {
        return;
    };
    if !bytes.starts_with(b"ELOG") {
        return;
    }
    if std::fs::create_dir_all(root).is_err() {
        return;
    }
    let tmp = root.join(format!(".{id}.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, bytes).is_err() {
        return;
    }
    if std::fs::rename(&tmp, root.join(&id)).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    let _ = std::fs::remove_file(root.join(format!("{id}.miss")));
}

#[cfg(not(target_arch = "wasm32"))]
fn cache_store_miss(root: &std::path::Path, id: &str) {
    let Some(id) = pack_cache_id(id) else {
        return;
    };
    if std::fs::create_dir_all(root).is_err() {
        return;
    }
    let _ = std::fs::write(root.join(format!("{id}.miss")), b"");
}

/// Outcome of one catalog GET. [`RemotePack::Error`] is a transport
/// failure and is not cached; a 404 is [`RemotePack::Absent`].
#[cfg(not(target_arch = "wasm32"))]
enum RemotePack {
    Hit(Vec<u8>),
    Absent,
    Error,
}

#[cfg(not(target_arch = "wasm32"))]
fn header_status(text: &str) -> Option<u16> {
    text.lines().find_map(|line| {
        let line = line.trim_start().strip_prefix('<')?.trim_start();
        let rest = line.strip_prefix("HTTP/")?;
        rest.split_whitespace().nth(1)?.parse().ok()
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn redirect_location(text: &str) -> Option<&str> {
    text.lines().find_map(|line| {
        line.trim_start()
            .strip_prefix('<')
            .map(str::trim_start)
            .and_then(|l| l.strip_prefix("Location:"))
            .map(str::trim)
            .filter(|l| l.starts_with("http"))
    })
}

/// `curl -w` appends a newline and the HTTP status after the body.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn split_curl_body(stdout: &[u8]) -> Option<(Vec<u8>, u16)> {
    let nl = stdout.iter().rposition(|&b| b == b'\n')?;
    let code = std::str::from_utf8(&stdout[nl + 1..]).ok()?.parse().ok()?;
    Some((stdout[..nl].to_vec(), code))
}

/// Inflate a gzipped object body. The catalog holds only compressed
/// packs, so anything without the gzip magic is not a pack.
fn inflate_pack(bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.starts_with(b"\x1f\x8b") {
        return None;
    }
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut GzDecoder::new(bytes), &mut out).ok()?;
    Some(out)
}

#[cfg(not(target_arch = "wasm32"))]
fn curl_get(url: &str) -> RemotePack {
    use std::process::Command;

    let Ok(body) = Command::new("curl")
        .args(["-sS", "-L", "-w", "\n%{http_code}", url])
        .output()
    else {
        return RemotePack::Error;
    };
    if !body.status.success() {
        return RemotePack::Error;
    }
    let Some((bytes, code)) = split_curl_body(&body.stdout) else {
        return RemotePack::Error;
    };
    if code == 404 {
        return RemotePack::Absent;
    }
    if code == 200 {
        if let Some(bytes) = inflate_pack(&bytes) {
            if bytes.starts_with(b"ELOG") {
                return RemotePack::Hit(bytes);
            }
        }
    }
    RemotePack::Error
}

/// Fetch one pack by build-id. The default (mesh) endpoint
/// 307-redirects to a presigned S3 URL; `metatron curl` does not
/// follow redirects, so the Location is read from its verbose headers
/// and the presigned GET goes through plain curl (the URL carries its
/// own signature). A non-default base (a local mirror, a test server)
/// is plain HTTP and fetched directly. A cached hit or a fresh 404
/// returns without touching the network.
#[cfg(not(target_arch = "wasm32"))]
fn fetch_pack(pack_id: &str, base_url: &str) -> FetchedPack {
    let root = cache_root();
    if let Some(root) = &root {
        match cache_lookup(root, pack_id) {
            CachedPack::Hit(bytes) => {
                return FetchedPack {
                    bytes: Some(bytes),
                    kind: PackFetchKind::Cached,
                };
            }
            CachedPack::Miss => {
                return FetchedPack {
                    bytes: None,
                    kind: PackFetchKind::Missing,
                };
            }
            CachedPack::Unknown => {}
        }
    }
    let remote = fetch_pack_remote(pack_id, base_url);
    if let Some(root) = &root {
        match &remote {
            RemotePack::Hit(bytes) => cache_store_hit(root, pack_id, bytes),
            RemotePack::Absent => cache_store_miss(root, pack_id),
            RemotePack::Error => {}
        }
    }
    match remote {
        RemotePack::Hit(bytes) => FetchedPack {
            bytes: Some(bytes),
            kind: PackFetchKind::Downloaded,
        },
        RemotePack::Absent => FetchedPack {
            bytes: None,
            kind: PackFetchKind::Missing,
        },
        RemotePack::Error => FetchedPack {
            bytes: None,
            kind: PackFetchKind::Failed,
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_pack_remote(pack_id: &str, base_url: &str) -> RemotePack {
    use std::process::Command;

    let url = format!("{}/{}", base_url.trim_end_matches('/'), pack_id);
    if base_url != DEFAULT_SYMBOLS_BASE_URL {
        return curl_get(&url);
    }
    let Ok(out) = Command::new("metatron")
        .args(["curl", "-a", METATRON_APP, "-v", &url])
        .output()
    else {
        return RemotePack::Error;
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    if header_status(&text) == Some(404) {
        return RemotePack::Absent;
    }
    let Some(location) = redirect_location(&text) else {
        return RemotePack::Error;
    };
    curl_get(location)
}

/// wasm stub: the browser GUI fetches packs via [`fetch_pack_wasm`].
/// This stays only so the CLI-oriented batch helpers keep compiling
/// on wasm, where they are never called.
#[cfg(target_arch = "wasm32")]
fn fetch_pack(_pack_id: &str, _base_url: &str) -> FetchedPack {
    FetchedPack {
        bytes: None,
        kind: PackFetchKind::Failed,
    }
}

/// Browser catalog GET. XHR follows the 307 to the presigned S3 URL
/// on its own; the endpoint and the bucket both carry CORS for the
/// ocdata origin. Credentials ride along so the SSO session on the
/// hosting page authorizes the elog-access hop.
#[cfg(target_arch = "wasm32")]
pub async fn fetch_pack_wasm(pack_id: &str, base_url: &str) -> FetchedPack {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{XmlHttpRequest, XmlHttpRequestResponseType};

    let failed = |bytes: Option<Vec<u8>>| FetchedPack {
        bytes,
        kind: PackFetchKind::Failed,
    };
    let url = format!("{}/{}", base_url.trim_end_matches('/'), pack_id);
    let Ok(xhr) = XmlHttpRequest::new() else {
        return failed(None);
    };
    if xhr.open_with_async("GET", &url, true).is_err() {
        return failed(None);
    }
    xhr.set_response_type(XmlHttpRequestResponseType::Arraybuffer);
    let _ = xhr.set_with_credentials(true);

    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let xhr_load = xhr.clone();
        let onload = wasm_bindgen::closure::Closure::once(move || {
            let code = xhr_load.status().unwrap_or(0);
            let value = match code {
                200..=299 => xhr_load.response().unwrap_or(wasm_bindgen::JsValue::NULL),
                _ => wasm_bindgen::JsValue::from_str(&format!("HTTP {code}")),
            };
            let _ = resolve.call1(&wasm_bindgen::JsValue::UNDEFINED, &value);
            let _ = resolve.call1(&wasm_bindgen::JsValue::UNDEFINED, &value);
        });
        xhr.set_onload(Some(onload.as_ref().unchecked_ref()));
        onload.forget();
        let xhr_err = xhr.clone();
        let onerror = wasm_bindgen::closure::Closure::once(move || {
            let _ = reject.call1(
                &wasm_bindgen::JsValue::UNDEFINED,
                &wasm_bindgen::JsValue::from_str(&format!(
                    "network error (status {:?})",
                    xhr_err.status()
                )),
            );
        });
        xhr.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();
    });
    if xhr.send().is_err() {
        return failed(None);
    }
    let Ok(value) = JsFuture::from(promise).await else {
        return failed(None);
    };
    if value.is_string() {
        let msg = value.as_string().unwrap_or_default();
        return FetchedPack {
            bytes: None,
            kind: if msg == "HTTP 404" {
                PackFetchKind::Missing
            } else {
                PackFetchKind::Failed
            },
        };
    }
    let array = js_sys::Uint8Array::new(&value);
    let mut bytes = vec![0u8; array.length() as usize];
    array.copy_to(&mut bytes);
    match inflate_pack(&bytes) {
        Some(raw) if raw.starts_with(b"ELOG") => FetchedPack {
            bytes: Some(raw),
            kind: PackFetchKind::Downloaded,
        },
        _ => failed(None),
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn scratch() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "elog-pack-cache-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn hit_round_trips_and_rejects_unsafe_ids() {
        let root = scratch();
        let id = "a6a0e20d7313ef50897a0ef2d9a560a0a1b5de87";
        let bytes = b"ELOG\x03\x00pack";
        cache_store_hit(&root, id, bytes);
        match cache_lookup(&root, id) {
            CachedPack::Hit(got) => assert_eq!(got, bytes),
            other => panic!("expected hit, got {other:?}"),
        }
        cache_store_hit(&root, "../etc/passwd", b"ELOG");
        cache_store_hit(&root, "abcd", b"ELOG");
        assert!(root.join(id).is_file());
        assert_eq!(
            std::fs::read_dir(&root)
                .unwrap()
                .filter_map(|e| e.ok())
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fresh_miss_is_reused_until_it_expires() {
        let root = scratch();
        let id = "92ef5e8d60910e5c1cfbc54748f580bc9b7725738e9f63e65c459c90bc9a09c2";
        cache_store_miss(&root, id);
        assert!(matches!(cache_lookup(&root, id), CachedPack::Miss));

        let miss = root.join(format!("{id}.miss"));
        let file = std::fs::File::options().write(true).open(&miss).unwrap();
        file.set_modified(SystemTime::now() - CACHE_MISS_TTL - Duration::from_secs(60))
            .unwrap();
        drop(file);
        assert!(matches!(cache_lookup(&root, id), CachedPack::Unknown));
        assert!(!miss.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_hit_is_dropped() {
        let root = scratch();
        let id = "8d24019e00e8362c5debf018979e44df162359b0";
        std::fs::write(root.join(id), b"not-an-elog").unwrap();
        assert!(matches!(cache_lookup(&root, id), CachedPack::Unknown));
        assert!(!root.join(id).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn split_curl_body_keeps_trailing_bytes() {
        let (body, code) = split_curl_body(b"ELOG\n200").unwrap();
        assert_eq!(body, b"ELOG");
        assert_eq!(code, 200);
        assert!(split_curl_body(b"ELOG").is_none());
    }

    #[test]
    fn inflate_pack_round_trips() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let raw = b"ELOG\x03\x00pack".to_vec();
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(&raw).unwrap();
        assert_eq!(inflate_pack(&enc.finish().unwrap()).unwrap(), raw);
        assert!(inflate_pack(&raw).is_none());
        assert!(inflate_pack(b"\x1f\x8bgarbage").is_none());
    }
}
