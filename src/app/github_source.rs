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

//! Map a captured DWARF pathname onto a GitHub blob and fetch its text.
//!
//! `elog -L` records the path the compiler embedded and, when it knows
//! it, a commit. Captures that leave `commit_id` empty still name a
//! repo through the path: the firmware tree, a Go GOROOT, or a ports
//! work directory whose version is in the extracted tarball name.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GithubBlob {
    pub host: &'static str,
    pub owner: &'static str,
    pub repo: &'static str,
    pub git_ref: String,
    pub path: String,
}

impl GithubBlob {
    pub(crate) fn url(&self) -> String {
        format!(
            "https://{}/{}/{}/blob/{}/{}",
            self.host, self.owner, self.repo, self.git_ref, self.path
        )
    }

    fn api_contents(&self) -> String {
        format!(
            "/repos/{}/{}/contents/{}?ref={}",
            self.owner,
            self.repo,
            encode_path(&self.path),
            encode_component(&self.git_ref)
        )
    }
}

struct PortRepo {
    category: &'static str,
    port: &'static str,
    host: &'static str,
    owner: &'static str,
    repo: &'static str,
    tag: fn(&str) -> Option<String>,
}

/// The site's firmware repo ([`crate::site::Key::FirmwareRepo`]).
/// Unset leaves firmware paths unmapped.
fn firmware_repo() -> Option<&'static crate::site::Repo> {
    crate::site::firmware_repo()
}

/// Whether `blob` points into the firmware repo.
pub(crate) fn is_firmware_blob(blob: &GithubBlob) -> bool {
    firmware_repo().is_some_and(|r| (r.host, r.owner, r.repo) == (blob.host, blob.owner, blob.repo))
}

const PORTS: &[PortRepo] = &[
    PortRepo {
        category: "ftp",
        port: "curl",
        host: "github.com",
        owner: "curl",
        repo: "curl",
        tag: curl_tag,
    },
    PortRepo {
        category: "lang",
        port: "perl5",
        host: "github.com",
        owner: "Perl",
        repo: "perl5",
        tag: perl_tag,
    },
    PortRepo {
        category: "sysutils",
        port: "syslog-ng",
        host: "github.com",
        owner: "syslog-ng",
        repo: "syslog-ng",
        tag: identity_tag,
    },
    PortRepo {
        category: "www",
        port: "nginx",
        host: "github.com",
        owner: "nginx",
        repo: "nginx",
        tag: nginx_tag,
    },
    PortRepo {
        category: "devel",
        port: "glib20",
        host: "github.com",
        owner: "GNOME",
        repo: "glib",
        tag: glib_tag,
    },
    PortRepo {
        category: "devel",
        port: "pcre2",
        host: "github.com",
        owner: "PCRE2Project",
        repo: "pcre2",
        tag: identity_tag,
    },
    PortRepo {
        category: "www",
        port: "libnghttp2",
        host: "github.com",
        owner: "nghttp2",
        repo: "nghttp2",
        tag: nghttp2_tag,
    },
    PortRepo {
        category: "net",
        port: "bird2",
        host: "github.com",
        owner: "CZ-NIC",
        repo: "bird",
        tag: bird_tag,
    },
    PortRepo {
        category: "net",
        port: "chrony",
        host: "github.com",
        owner: "mlichvar",
        repo: "chrony",
        tag: chrony_tag,
    },
];

fn identity_tag(dist: &str) -> Option<String> {
    (!dist.is_empty()).then(|| dist.to_string())
}

fn curl_tag(dist: &str) -> Option<String> {
    let version = dist.strip_prefix("curl-")?;
    (!version.is_empty()).then(|| format!("curl-{}", version.replace('.', "_")))
}

fn perl_tag(dist: &str) -> Option<String> {
    let version = dist.strip_prefix("perl-")?;
    (!version.is_empty()).then(|| format!("v{version}"))
}

fn nginx_tag(dist: &str) -> Option<String> {
    let version = dist.strip_prefix("nginx-")?;
    (!version.is_empty()).then(|| format!("release-{version}"))
}

fn glib_tag(dist: &str) -> Option<String> {
    let version = dist.strip_prefix("glib-")?;
    (!version.is_empty()).then(|| version.to_string())
}

fn nghttp2_tag(dist: &str) -> Option<String> {
    let version = dist.strip_prefix("nghttp2-")?;
    (!version.is_empty()).then(|| format!("v{version}"))
}

fn bird_tag(dist: &str) -> Option<String> {
    let version = dist.strip_prefix("bird-")?;
    (!version.is_empty()).then(|| format!("v{version}"))
}

fn chrony_tag(dist: &str) -> Option<String> {
    dist.strip_prefix("chrony-")
        .filter(|version| !version.is_empty())
        .map(str::to_string)
}

fn path_components(pathname: &str) -> Vec<&str> {
    pathname
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

fn normalize_relative(parts: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for part in parts {
        if *part == ".." {
            if out.last().is_some() {
                out.pop();
            }
        } else {
            out.push((*part).to_string());
        }
    }
    out
}

fn with_commit(git_ref: String, commit_id: &str) -> String {
    let commit = commit_id.trim();
    if commit.is_empty() {
        git_ref
    } else {
        commit.to_string()
    }
}

fn firmware_blob(path: String, commit_id: &str, firmware_commit: &str) -> Option<GithubBlob> {
    let repo = firmware_repo()?;
    let fallback = firmware_commit.trim();
    let fallback = if fallback.is_empty() {
        repo.git_ref.to_string()
    } else {
        fallback.to_string()
    };
    Some(GithubBlob {
        host: repo.host,
        owner: repo.owner,
        repo: repo.repo,
        git_ref: with_commit(fallback, commit_id),
        path,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FirmwareIdent {
    pub commit: String,
    pub dirty: bool,
}

/// `newvers.sh` ends the ident in `n<count>-<hash>`, optionally `-dirty`.
/// PR images look like `pull/123-n8635-a36b8d97a3a7`; a local build is
/// `n8622-d9885eb13475` or `n8622-d9885eb13475-dirty`.
pub(crate) fn firmware_ident_from_kernel_version(version: &str) -> Option<FirmwareIdent> {
    version
        .split_whitespace()
        .find_map(firmware_ident_from_token)
}

pub(crate) fn firmware_commit_from_kernel_version(version: &str) -> Option<String> {
    firmware_ident_from_kernel_version(version).map(|ident| ident.commit)
}

/// Shown when the blob is a firmware file and the ident is dirty or has
/// no commit. A clean ident needs no note; the URL carries the hash.
pub(crate) fn firmware_source_note(version: &str) -> Option<String> {
    match firmware_ident_from_kernel_version(version) {
        Some(ident) if ident.dirty => Some(format!(
            "Kernel built with local changes on {}. Showing that commit; the local changes are not in it.",
            ident.commit
        )),
        Some(_) => None,
        None => {
            let git_ref = firmware_repo().map_or("HEAD", |r| r.git_ref);
            Some(if version.trim().is_empty() {
                format!("No kernel ident in this capture. Showing {git_ref}.")
            } else {
                format!("No firmware commit in this kernel ident. Showing {git_ref}.")
            })
        }
    }
}

fn firmware_ident_from_token(token: &str) -> Option<FirmwareIdent> {
    let token = token.trim_end_matches(':');
    let (token, dirty) = token
        .strip_suffix("-dirty")
        .map(|rest| (rest, true))
        .unwrap_or((token, false));
    let mut parts = token.rsplitn(3, '-');
    let hash = parts.next()?;
    let count = parts.next()?;
    if hash.len() < 7
        || hash.len() > 40
        || !hash.chars().all(|c| c.is_ascii_hexdigit())
        || count.len() < 2
        || !count.starts_with('n')
        || !count[1..].chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(FirmwareIdent {
        commit: hash.to_string(),
        dirty,
    })
}

fn ports_blob(parts: &[&str], commit_id: &str) -> Option<GithubBlob> {
    let ports_at = parts.iter().position(|part| *part == "ports")?;
    let category = parts.get(ports_at + 1)?;
    let port = parts.get(ports_at + 2)?;
    let work = parts.get(ports_at + 3)?;
    if !work.starts_with("work") {
        return None;
    }
    let dist = parts.get(ports_at + 4)?;
    let relative = normalize_relative(&parts[ports_at + 5..]);
    if relative.is_empty() {
        return None;
    }
    let spec = PORTS.iter().find(|spec| {
        spec.category == *category && (*port == spec.port || port.starts_with(spec.port))
    })?;
    Some(GithubBlob {
        host: spec.host,
        owner: spec.owner,
        repo: spec.repo,
        git_ref: with_commit((spec.tag)(dist)?, commit_id),
        path: relative.join("/"),
    })
}

struct PortWork<'a> {
    category: &'a str,
    port: &'a str,
    dist: &'a str,
    relative: Vec<String>,
}

fn port_work<'a>(parts: &'a [&'a str]) -> Option<PortWork<'a>> {
    let ports_at = parts.iter().position(|part| *part == "ports")?;
    let category = *parts.get(ports_at + 1)?;
    let port = *parts.get(ports_at + 2)?;
    let next = parts.get(ports_at + 3).copied()?;
    if !next.starts_with("work") {
        return None;
    }
    let dist = *parts.get(ports_at + 4)?;
    let relative = normalize_relative(&parts[ports_at + 5..]);
    if relative.is_empty() {
        return None;
    }
    Some(PortWork {
        category,
        port,
        dist,
        relative,
    })
}

fn strip_dist_version(dist: &str) -> &str {
    match dist.rfind('-') {
        Some(dash)
            if dist[dash + 1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit()) =>
        {
            &dist[..dash]
        }
        _ => dist,
    }
}

/// `entries` are the names in `ports/<category>/<port>/files`. Returns the
/// path under that port when the compiled file is one of those sources.
fn vendored_files_suffix(
    port: &str,
    dist: &str,
    relative: &[String],
    entries: &[(&str, bool)],
) -> Option<String> {
    let first = relative.first()?;
    let rel = relative.join("/");
    let port_lower = port.to_ascii_lowercase();
    let stem = strip_dist_version(dist);
    for wrapper in [stem, dist, port, port_lower.as_str()] {
        if entries
            .iter()
            .any(|(name, is_dir)| *is_dir && *name == wrapper)
        {
            return Some(format!("files/{wrapper}/{rel}"));
        }
    }
    entries
        .iter()
        .any(|(name, _)| *name == first)
        .then(|| format!("files/{rel}"))
}

fn netflix_ports_blob(
    parts: &[&str],
    commit_id: &str,
    firmware_commit: &str,
) -> Option<GithubBlob> {
    let ports_at = parts.iter().position(|part| *part == "ports")?;
    let category = parts.get(ports_at + 1)?;
    if *category != "netflix" {
        return None;
    }
    let port = parts.get(ports_at + 2)?;
    let next = parts.get(ports_at + 3).copied();
    let path = if next.is_some_and(|w| w.starts_with("work")) {
        let relative = normalize_relative(&parts[ports_at + 5..]);
        if relative.is_empty() {
            return None;
        }
        if *port == "nginx" {
            format!("ports/netflix/nginx/files/nginx/{}", relative.join("/"))
        } else {
            format!("ports/netflix/{port}/files/{}", relative.join("/"))
        }
    } else if parts[ports_at..].contains(&"files") {
        normalize_relative(&parts[ports_at..]).join("/")
    } else {
        let relative = normalize_relative(&parts[ports_at + 3..]);
        if relative.is_empty() {
            return None;
        }
        if *port == "nginx" {
            format!("ports/netflix/nginx/files/nginx/{}", relative.join("/"))
        } else {
            format!("ports/netflix/{port}/files/{}", relative.join("/"))
        }
    };
    if path.is_empty() {
        return None;
    }
    firmware_blob(path, commit_id, firmware_commit)
}

fn committed_port_tree(
    parts: &[&str],
    commit_id: &str,
    firmware_commit: &str,
) -> Option<GithubBlob> {
    let ports_at = parts.iter().position(|part| *part == "ports")?;
    if parts
        .get(ports_at + 3)
        .is_some_and(|next| next.starts_with("work"))
    {
        return None;
    }
    if !parts[ports_at..].contains(&"files") {
        return None;
    }
    let path = normalize_relative(&parts[ports_at..]).join("/");
    if path.is_empty() {
        return None;
    }
    firmware_blob(path, commit_id, firmware_commit)
}

fn goroot_blob(parts: &[&str], commit_id: &str) -> Option<GithubBlob> {
    let goroot = parts.iter().position(|part| *part == ".goroot")?;
    let version = parts.get(goroot + 1)?;
    if !version.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let src_at = parts
        .iter()
        .skip(goroot + 2)
        .position(|part| *part == "src")?
        + goroot
        + 2;
    let relative = normalize_relative(&parts[src_at..]);
    if relative.is_empty() {
        return None;
    }
    Some(GithubBlob {
        host: "github.com",
        owner: "golang",
        repo: "go",
        git_ref: with_commit(format!("go{version}"), commit_id),
        path: relative.join("/"),
    })
}

pub(crate) fn resolve_github_blob(
    pathname: &str,
    commit_id: &str,
    firmware_commit: &str,
) -> Option<GithubBlob> {
    let parts = path_components(pathname);
    if let Some(freebsd) = parts.iter().position(|part| *part == "FreeBSD") {
        // The kernel objdir is itself under FreeBSD/, and .debug_line
        // walks back out of it with "..". Collapse before the blob path.
        let path = normalize_relative(&parts[freebsd..]).join("/");
        if !path.is_empty() {
            return firmware_blob(path, commit_id, firmware_commit);
        }
    }
    if let Some(blob) = committed_port_tree(&parts, commit_id, firmware_commit) {
        return Some(blob);
    }
    if let Some(blob) = netflix_ports_blob(&parts, commit_id, firmware_commit) {
        return Some(blob);
    }
    if let Some(blob) = goroot_blob(&parts, commit_id) {
        return Some(blob);
    }
    if let Some(blob) = ports_blob(&parts, commit_id) {
        return Some(blob);
    }
    if let Some(include_at) = parts.windows(2).position(|pair| pair == ["usr", "include"]) {
        let relative = normalize_relative(&parts[include_at + 2..]);
        if !relative.is_empty() {
            let mut path = vec!["FreeBSD".to_string(), "include".to_string()];
            path.extend(relative);
            return firmware_blob(path.join("/"), commit_id, firmware_commit);
        }
    }
    None
}

fn encode_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn json_string_field(body: &str, field: &str) -> Option<String> {
    let patterns = [format!("\"{field}\":\""), format!("\"{field}\": \"")];
    let start = patterns.iter().find_map(|pattern| {
        body.find(pattern.as_str())
            .map(|index| index + pattern.len())
    })?;
    let mut out = String::new();
    let mut chars = body[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '"' => break,
            _ => out.push(c),
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Look up `filename` in the repo `hint` already resolved, and return a
/// blob at the same git ref. Used when DWARF recorded only a basename.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn search_repo_filename(hint: &GithubBlob, filename: &str) -> Option<GithubBlob> {
    if filename.is_empty()
        || filename.contains('/')
        || !filename
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
    {
        return None;
    }
    let query = format!("filename:{filename} repo:{}/{}", hint.owner, hint.repo);
    let api = format!("/search/code?q={}", encode_component(&query));
    let output = gh_api(
        hint.host,
        &[&api, "-H", "Accept: application/vnd.github+json"],
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let path = json_string_field(&body, "path")?;
    Some(GithubBlob {
        host: hint.host,
        owner: hint.owner,
        repo: hint.repo,
        git_ref: hint.git_ref.clone(),
        path,
    })
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_component)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(not(target_arch = "wasm32"))]
fn gh_api(host: &str, extra: &[&str]) -> Result<std::process::Output, String> {
    let mut command = std::process::Command::new("gh");
    command
        .arg("api")
        .arg("--hostname")
        .arg(host)
        .args(extra)
        .stdin(std::process::Stdio::null())
        .env("GH_PROMPT_DISABLED", "1");
    command
        .output()
        .map_err(|error| format!("Could not run gh: {error}"))
}

#[cfg(not(target_arch = "wasm32"))]
fn command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut text = format!("{stderr}{stdout}");
    if text.len() > 500 {
        text.truncate(500);
    }
    text
}

fn json_objects(body: &str) -> Vec<&str> {
    let bytes = body.as_bytes();
    let mut objects = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let start = i;
        let mut depth = 0;
        let mut in_string = false;
        let mut escape = false;
        while i < bytes.len() {
            let c = bytes[i];
            if in_string {
                if escape {
                    escape = false;
                } else if c == b'\\' {
                    escape = true;
                } else if c == b'"' {
                    in_string = false;
                }
            } else if c == b'"' {
                in_string = true;
            } else if c == b'{' {
                depth += 1;
            } else if c == b'}' {
                depth -= 1;
                if depth == 0 {
                    i += 1;
                    objects.push(&body[start..i]);
                    break;
                }
            }
            i += 1;
        }
        if i < bytes.len() && depth != 0 {
            break;
        }
    }
    objects
}

#[cfg(not(target_arch = "wasm32"))]
fn port_files_entries(category: &str, port: &str, git_ref: &str) -> Option<Vec<(String, bool)>> {
    use std::sync::{Mutex, OnceLock};

    #[derive(Clone)]
    enum Listing {
        Entries(Vec<(String, bool)>),
        Missing,
    }

    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, Listing>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let key = format!("{category}\0{port}\0{git_ref}");
    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.get(&key) {
            return match cached {
                Listing::Entries(entries) => Some(entries.clone()),
                Listing::Missing => None,
            };
        }
    }
    let repo = firmware_repo()?;
    let api = format!(
        "/repos/{}/{}/contents/{}",
        repo.owner,
        repo.repo,
        encode_path(&format!("ports/{category}/{port}/files"))
    );
    let query = format!("{api}?ref={}", encode_component(git_ref));
    let output = gh_api(
        repo.host,
        &[&query, "-H", "Accept: application/vnd.github+json"],
    )
    .ok()?;
    let body = String::from_utf8_lossy(&output.stdout);
    let listing = if output.status.success() {
        let entries = json_objects(&body)
            .into_iter()
            .filter_map(|object| {
                let name = json_string_field(object, "name")?;
                let is_dir = json_string_field(object, "type").as_deref() == Some("dir");
                Some((name, is_dir))
            })
            .collect();
        Listing::Entries(entries)
    } else if body.contains("Not Found") {
        Listing::Missing
    } else {
        return None;
    };
    let result = match &listing {
        Listing::Entries(entries) => Some(entries.clone()),
        Listing::Missing => None,
    };
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, listing);
    }
    result
}

/// Port sources committed under `ports/<category>/<port>/files` open in the
/// firmware repo. A port that only carries patches keeps its upstream blob.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn resolve_vendored_port(
    pathname: &str,
    commit_id: &str,
    firmware_commit: &str,
) -> Option<GithubBlob> {
    let parts = path_components(pathname);
    let work = port_work(&parts)?;
    let git_ref = firmware_blob(String::new(), commit_id, firmware_commit)?.git_ref;
    let entries = port_files_entries(work.category, work.port, &git_ref)?;
    let borrowed: Vec<(&str, bool)> = entries
        .iter()
        .map(|(name, is_dir)| (name.as_str(), *is_dir))
        .collect();
    let suffix = vendored_files_suffix(work.port, work.dist, &work.relative, &borrowed)?;
    firmware_blob(
        format!("ports/{}/{}/{}", work.category, work.port, suffix),
        commit_id,
        firmware_commit,
    )
}

/// Downloaded blob text kept under the user cache dir. Newest 50 by last use.
#[cfg(not(target_arch = "wasm32"))]
const SOURCE_CACHE_LIMIT: usize = 50;

#[cfg(not(target_arch = "wasm32"))]
fn source_cache_root() -> Option<std::path::PathBuf> {
    dirs::cache_dir().map(|dir| dir.join("event_explorer").join("source"))
}

#[cfg(not(target_arch = "wasm32"))]
fn source_cache_key(blob: &GithubBlob) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(blob.host);
    hasher.update([0]);
    hasher.update(blob.owner);
    hasher.update([0]);
    hasher.update(blob.repo);
    hasher.update([0]);
    hasher.update(blob.git_ref.as_bytes());
    hasher.update([0]);
    hasher.update(blob.path.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(not(target_arch = "wasm32"))]
fn is_cache_key(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(not(target_arch = "wasm32"))]
fn cache_entries(root: &std::path::Path) -> Vec<(std::time::SystemTime, std::path::PathBuf)> {
    let Ok(dir) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_cache_key(name) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        entries.push((modified, entry.path()));
    }
    entries
}

#[cfg(not(target_arch = "wasm32"))]
fn evict_source_cache(root: &std::path::Path) {
    let mut entries = cache_entries(root);
    if entries.len() <= SOURCE_CACHE_LIMIT {
        return;
    }
    entries.sort_by_key(|(modified, _)| *modified);
    let drop_count = entries.len() - SOURCE_CACHE_LIMIT;
    for (_, path) in entries.into_iter().take(drop_count) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn cache_get(root: &std::path::Path, key: &str) -> Option<String> {
    if !is_cache_key(key) {
        return None;
    }
    let path = root.join(key);
    let text = std::fs::read_to_string(&path).ok()?;
    if let Ok(file) = std::fs::File::options().write(true).open(&path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
    Some(text)
}

#[cfg(not(target_arch = "wasm32"))]
fn cache_put(root: &std::path::Path, key: &str, text: &str) {
    if !is_cache_key(key) {
        return;
    }
    if std::fs::create_dir_all(root).is_err() {
        return;
    }
    let tmp = root.join(format!(".{key}.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, text).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, root.join(key)).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    evict_source_cache(root);
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn fetch_github_blob(blob: &GithubBlob) -> Result<String, String> {
    let key = source_cache_key(blob);
    if let Some(root) = source_cache_root() {
        if let Some(text) = cache_get(&root, &key) {
            return Ok(text);
        }
    }
    let text = download_github_blob(blob)?;
    if let Some(root) = source_cache_root() {
        cache_put(&root, &key, &text);
    }
    Ok(text)
}

#[cfg(not(target_arch = "wasm32"))]
fn download_github_blob(blob: &GithubBlob) -> Result<String, String> {
    let api = blob.api_contents();
    let raw = gh_api(
        blob.host,
        &[&api, "-H", "Accept: application/vnd.github.raw"],
    )?;
    if raw.status.success() {
        return Ok(String::from_utf8_lossy(&raw.stdout).into_owned());
    }
    let failure = command_error(&raw);
    if !failure.contains("too_large") && !failure.contains("too large") {
        return Err(format!("{}\n{failure}", blob.url()));
    }
    let sha_out = gh_api(blob.host, &[&api, "--jq", ".sha"])?;
    if !sha_out.status.success() {
        return Err(format!("{}\n{}", blob.url(), command_error(&sha_out)));
    }
    let sha = String::from_utf8_lossy(&sha_out.stdout).trim().to_string();
    if sha.len() < 7 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "{}\nGitHub contents response had no blob sha",
            blob.url()
        ));
    }
    let blob_api = format!("/repos/{}/{}/git/blobs/{sha}", blob.owner, blob.repo);
    let body = gh_api(
        blob.host,
        &[&blob_api, "-H", "Accept: application/vnd.github.raw"],
    )?;
    if !body.status.success() {
        return Err(format!("{}\n{}", blob.url(), command_error(&body)));
    }
    Ok(String::from_utf8_lossy(&body.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_build_path_maps_onto_the_tree() {
        let blob = resolve_github_blob(
            "/build/workspace/firmware-2/FreeBSD/sys/amd64/include/atomic.h",
            "",
            "",
        )
        .unwrap();
        assert_eq!(blob.host, "git.example.com");
        assert_eq!(blob.owner, "acme");
        assert_eq!(blob.repo, "firmware");
        assert_eq!(blob.git_ref, "main");
        assert_eq!(blob.path, "FreeBSD/sys/amd64/include/atomic.h");
    }

    #[test]
    fn kernel_objdir_path_collapses_to_the_tree() {
        let blob = resolve_github_blob(
            "/home/user/firmware/.obj/nano/home/user/firmware/FreeBSD/amd64.amd64/sys/GENERIC/../../../../../../../../../FreeBSD/sys/netinet/tcp_lro.c",
            "",
            "d9885eb13475",
        )
        .unwrap();
        assert_eq!(blob.git_ref, "d9885eb13475");
        assert_eq!(blob.path, "FreeBSD/sys/netinet/tcp_lro.c");
    }

    #[test]
    fn relative_firmware_path_and_commit_win() {
        let blob = resolve_github_blob(
            "./../../../../FreeBSD/sys/modules/tcp/23q12p14/ngen.c",
            "0123456789abcdef0123456789abcdef01234567",
            "a36b8d97a3a7",
        )
        .unwrap();
        assert_eq!(blob.git_ref, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(blob.path, "FreeBSD/sys/modules/tcp/23q12p14/ngen.c");
    }

    #[test]
    fn netflix_ports_map_into_the_firmware_tree() {
        let nginx = resolve_github_blob(
            "/wrkdirs/usr/ports/netflix/nginx/work-default/nginx-1.31.4/src/http/ngx_http_parse.c",
            "",
            "a36b8d97a3a7",
        )
        .unwrap();
        assert_eq!(nginx.host, "git.example.com");
        assert_eq!(nginx.owner, "acme");
        assert_eq!(nginx.repo, "firmware");
        assert_eq!(nginx.git_ref, "a36b8d97a3a7");
        assert_eq!(
            nginx.path,
            "ports/netflix/nginx/files/nginx/src/http/ngx_http_parse.c"
        );

        let module = resolve_github_blob(
            "/wrkdirs/usr/ports/netflix/nginx/work-default/nginx-1.31.4/health_stats/health_stats_module.c",
            "",
            "",
        )
        .unwrap();
        assert_eq!(module.host, "git.example.com");
        assert_eq!(module.owner, "acme");
        assert_eq!(module.repo, "firmware");
        assert_eq!(module.git_ref, "main");
        assert_eq!(
            module.path,
            "ports/netflix/nginx/files/nginx/health_stats/health_stats_module.c"
        );

        let download = resolve_github_blob(
            "/wrkdirs/usr/ports/netflix/download_helper/work/download_helper-20251029/curl.c",
            "",
            "",
        )
        .unwrap();
        assert_eq!(download.host, "git.example.com");
        assert_eq!(download.owner, "acme");
        assert_eq!(download.repo, "firmware");
        assert_eq!(download.path, "ports/netflix/download_helper/files/curl.c");
    }

    fn rel(path: &str) -> Vec<String> {
        path.split('/').map(str::to_string).collect()
    }

    #[test]
    fn vendored_port_files_follow_the_files_directory() {
        let nginx = [("nginx", true), ("nginx.in", false)];
        assert_eq!(
            vendored_files_suffix(
                "nginx",
                "nginx-1.31.4",
                &rel("src/http/ngx_http_parse.c"),
                &nginx
            )
            .unwrap(),
            "files/nginx/src/http/ngx_http_parse.c"
        );
        let flat = [("curl.c", false), ("Makefile", false)];
        assert_eq!(
            vendored_files_suffix(
                "download_helper",
                "download_helper-20251029",
                &rel("curl.c"),
                &flat
            )
            .unwrap(),
            "files/curl.c"
        );
        let nested = [("djbdns", true)];
        assert_eq!(
            vendored_files_suffix("djbdns", "djbdns-1.05", &rel("dnscache.c"), &nested).unwrap(),
            "files/djbdns/dnscache.c"
        );
        let versioned = [("lsiutil-1.72", true)];
        assert_eq!(
            vendored_files_suffix("lsiutil", "lsiutil-1.72", &rel("lsiutil.c"), &versioned)
                .unwrap(),
            "files/lsiutil-1.72/lsiutil.c"
        );
        let cksum = [("dist", true), ("Makefile", false)];
        assert_eq!(
            vendored_files_suffix("cksum32", "cksum32-20220614", &rel("dist/hw.c"), &cksum)
                .unwrap(),
            "files/dist/hw.c"
        );
        let openssl = [("eideticom-noload-openssl", true)];
        assert_eq!(
            vendored_files_suffix(
                "Eideticom-noload-openssl",
                "noload-openssl-sd-1.5",
                &rel("openssl.c"),
                &openssl
            )
            .unwrap(),
            "files/eideticom-noload-openssl/openssl.c"
        );
        let patches = [("patch-configure", false), ("Makefile", false)];
        assert!(
            vendored_files_suffix("curl", "curl-8.22.0", &rel("lib/multi.c"), &patches).is_none()
        );
    }

    #[test]
    fn checked_out_port_files_map_into_the_firmware_tree() {
        let blob = resolve_github_blob(
            "/home/user/firmware/ports/devel/gdb/files/kgdb/kgdb.c",
            "",
            "a36b8d97a3a7",
        )
        .unwrap();
        assert_eq!(blob.host, "git.example.com");
        assert_eq!(blob.repo, "firmware");
        assert_eq!(blob.git_ref, "a36b8d97a3a7");
        assert_eq!(blob.path, "ports/devel/gdb/files/kgdb/kgdb.c");
    }

    #[test]
    fn ports_and_goroot_paths_keep_the_version_in_the_ref() {
        let curl = resolve_github_blob(
            "/wrkdirs/usr/ports/ftp/curl/work/curl-8.21.0/lib/multi.c",
            "",
            "a36b8d97a3a7",
        )
        .unwrap();
        assert_eq!(curl.repo, "curl");
        assert_eq!(curl.git_ref, "curl-8_21_0");
        assert_eq!(curl.path, "lib/multi.c");

        let perl = resolve_github_blob(
            "/wrkdirs/usr/ports/lang/perl5.42/work/perl-5.42.3/sv.c",
            "",
            "",
        )
        .unwrap();
        assert_eq!(perl.git_ref, "v5.42.3");
        assert_eq!(perl.path, "sv.c");

        let glib = resolve_github_blob(
            "/wrkdirs/usr/ports/devel/glib20/work-default/glib-2.88.3/_build/../glib/gthread-posix.c",
            "",
            "",
        )
        .unwrap();
        assert_eq!(glib.git_ref, "2.88.3");
        assert_eq!(glib.path, "glib/gthread-posix.c");

        let go = resolve_github_blob("./.goroot/1.24.2/src/runtime/traceback.go", "", "").unwrap();
        assert_eq!(go.git_ref, "go1.24.2");
        assert_eq!(go.path, "src/runtime/traceback.go");
    }

    #[test]
    fn system_headers_use_the_firmware_include_tree() {
        let blob = resolve_github_blob("/usr/include/_ctype.h", "", "").unwrap();
        assert_eq!(blob.path, "FreeBSD/include/_ctype.h");
    }

    #[test]
    fn pr_kernel_version_is_the_firmware_commit() {
        let version = "FreeBSD 16.0-CURRENT pull/123-n8635-a36b8d97a3a7 GENERIC";
        let commit = firmware_commit_from_kernel_version(version).unwrap();
        assert_eq!(commit, "a36b8d97a3a7");
        let blob = resolve_github_blob(
            "/build/workspace/firmware-2/FreeBSD/sys/netinet/tcp_stacks/ngen.c",
            "",
            &commit,
        )
        .unwrap();
        assert_eq!(blob.git_ref, "a36b8d97a3a7");
        assert!(firmware_commit_from_kernel_version("FreeBSD 16.0-CURRENT").is_none());
        assert!(firmware_source_note("FreeBSD 16.0-CURRENT")
            .unwrap()
            .contains("main"));
    }

    #[test]
    fn local_dirty_ident_keeps_the_commit_and_says_so() {
        let version = "FreeBSD 16.0-CURRENT #0 n8622-d9885eb13475-dirty: Wed Sep 30";
        let ident = firmware_ident_from_kernel_version(version).unwrap();
        assert_eq!(ident.commit, "d9885eb13475");
        assert!(ident.dirty);
        let note = firmware_source_note(version).unwrap();
        assert!(note.contains("d9885eb13475"));
        assert!(note.contains("local changes"));
        let clean =
            firmware_ident_from_kernel_version("elog-source-lines-n8622-d9885eb13475").unwrap();
        assert_eq!(clean.commit, "d9885eb13475");
        assert!(!clean.dirty);
        assert!(firmware_source_note("elog-source-lines-n8622-d9885eb13475").is_none());
    }

    #[test]
    fn json_string_field_reads_the_first_path() {
        let body = r#"{"items":[{"name":"ngx.c","path":"src/event/ngx_event.c"}]}"#;
        assert_eq!(
            json_string_field(body, "path").as_deref(),
            Some("src/event/ngx_event.c")
        );
        assert_eq!(
            json_string_field(r#"{"path": "FreeBSD/lib/libc/gen/getpid.S"}"#, "path").as_deref(),
            Some("FreeBSD/lib/libc/gen/getpid.S")
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn source_cache_key_changes_with_the_path() {
        let blob = GithubBlob {
            host: "git.example.com",
            owner: "acme",
            repo: "firmware",
            git_ref: "abc".to_string(),
            path: "FreeBSD/sys/netinet/tcp_lro.c".to_string(),
        };
        let key = source_cache_key(&blob);
        let other = GithubBlob {
            path: "FreeBSD/sys/netinet/tcp_lro.h".to_string(),
            ..blob
        };
        assert_ne!(source_cache_key(&other), key);
        assert!(is_cache_key(&key));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn source_cache_drops_the_least_recently_used() {
        let root = std::env::temp_dir().join(format!("ee-source-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let key = |i: u64| format!("{i:064x}");
        for i in 1..=SOURCE_CACHE_LIMIT as u64 {
            let path = root.join(key(i));
            std::fs::write(&path, format!("body {i}")).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(i))
                .unwrap();
        }
        assert_eq!(cache_get(&root, &key(1)).as_deref(), Some("body 1"));
        let newest = (SOURCE_CACHE_LIMIT as u64) + 1;
        cache_put(&root, &key(newest), "body newest");
        assert_eq!(cache_entries(&root).len(), SOURCE_CACHE_LIMIT);
        assert!(root.join(key(1)).is_file());
        assert!(!root.join(key(2)).exists());
        assert_eq!(
            cache_get(&root, &key(newest)).as_deref(),
            Some("body newest")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
