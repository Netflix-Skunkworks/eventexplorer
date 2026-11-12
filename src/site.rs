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

//! Site configuration: the servers and repos a deployment points at.
//!
//! Every key is optional, and a feature whose key is unset stays off
//! (capture sharing, symbol packs, firmware source links). A native
//! build looks each key up in order:
//!
//! 1. the `ELOG_<KEY>` environment variable,
//! 2. the site file: `$ELOG_SITE_CONFIG`, else
//!    `<config dir>/event_explorer/site.toml`,
//! 3. the value baked in at build time from `EE_SITE_CONFIG` (`build.rs`).
//!
//! The browser build has only the baked-in values. See
//! [`docs/site-config.md`](../docs/site-config.md) for the keys.

include!("site_file.rs");

/// One site setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Uploads API base (`POST <base>`, `<base>/<id>/complete`, `<base>/whoami`).
    UploadsUrl,
    /// Base share links embed as `?file=<base>/<id>/download`. Defaults to `UploadsUrl`.
    ShareUrl,
    /// Hosted EventExplorer page share links open.
    EventExplorerPage,
    /// Symbol catalog base; `<base>/<build_id>` returns a gzip'd pack.
    SymbolsUrl,
    /// Command that performs site API requests in place of `curl -sS`,
    /// given curl-style `-X`/`-H`/`-d`/`-v` arguments and the URL.
    HttpCommand,
    /// Firmware source repo, `host/owner/repo[@ref]`.
    FirmwareRepo,
    /// Extra text for the share dialog (retention, network needs).
    UploadNote,
}

impl Key {
    pub const ALL: [Key; 7] = [
        Key::UploadsUrl,
        Key::ShareUrl,
        Key::EventExplorerPage,
        Key::SymbolsUrl,
        Key::HttpCommand,
        Key::FirmwareRepo,
        Key::UploadNote,
    ];

    /// Name in the site file.
    pub fn name(self) -> &'static str {
        match self {
            Key::UploadsUrl => "uploads_url",
            Key::ShareUrl => "share_url",
            Key::EventExplorerPage => "eventexplorer_page",
            Key::SymbolsUrl => "symbols_url",
            Key::HttpCommand => "http_command",
            Key::FirmwareRepo => "firmware_repo",
            Key::UploadNote => "upload_note",
        }
    }

    /// Environment override.
    pub fn env(self) -> &'static str {
        match self {
            Key::UploadsUrl => "ELOG_UPLOADS_URL",
            Key::ShareUrl => "ELOG_SHARE_URL",
            Key::EventExplorerPage => "ELOG_EVENTEXPLORER_PAGE",
            Key::SymbolsUrl => "ELOG_SYMBOLS_URL",
            Key::HttpCommand => "ELOG_HTTP_COMMAND",
            Key::FirmwareRepo => "ELOG_FIRMWARE_REPO",
            Key::UploadNote => "ELOG_UPLOAD_NOTE",
        }
    }

    #[cfg_attr(test, allow(dead_code))]
    fn baked(self) -> Option<&'static str> {
        match self {
            Key::UploadsUrl => option_env!("EE_SITE_UPLOADS_URL"),
            Key::ShareUrl => option_env!("EE_SITE_SHARE_URL"),
            Key::EventExplorerPage => option_env!("EE_SITE_EVENTEXPLORER_PAGE"),
            Key::SymbolsUrl => option_env!("EE_SITE_SYMBOLS_URL"),
            Key::HttpCommand => option_env!("EE_SITE_HTTP_COMMAND"),
            Key::FirmwareRepo => option_env!("EE_SITE_FIRMWARE_REPO"),
            Key::UploadNote => option_env!("EE_SITE_UPLOAD_NOTE"),
        }
    }

    #[cfg_attr(any(test, target_arch = "wasm32"), allow(dead_code))]
    fn from_name(name: &str) -> Option<Key> {
        Key::ALL.into_iter().find(|k| k.name() == name)
    }
}

/// Unit tests see one fixed example site, whatever the host has set.
#[cfg(test)]
fn lookup(key: Key) -> Option<String> {
    let v = match key {
        Key::UploadsUrl => "https://share.example.com/api/uploads",
        Key::EventExplorerPage => "https://share.example.com/eventexplorer/",
        Key::SymbolsUrl => "https://share.example.com/api/symbols",
        Key::FirmwareRepo => "git.example.com/acme/firmware@main",
        _ => return None,
    };
    Some(v.to_string())
}

#[cfg(all(not(test), not(target_arch = "wasm32")))]
fn lookup(key: Key) -> Option<String> {
    if let Ok(v) = std::env::var(key.env()) {
        return Some(v);
    }
    if let Some(v) = site_file_values()
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.clone())
    {
        return Some(v);
    }
    key.baked().map(str::to_string)
}

#[cfg(all(not(test), target_arch = "wasm32"))]
fn lookup(key: Key) -> Option<String> {
    key.baked().map(str::to_string)
}

/// Path of the site file: `$ELOG_SITE_CONFIG`, else the per-user default.
#[cfg(not(target_arch = "wasm32"))]
pub fn site_file_path() -> Option<std::path::PathBuf> {
    match std::env::var_os("ELOG_SITE_CONFIG") {
        Some(p) if !p.is_empty() => Some(p.into()),
        _ => dirs::config_dir().map(|d| d.join("event_explorer").join("site.toml")),
    }
}

/// The site file's known keys, read once. A missing file is empty; a
/// malformed one is reported on stderr and ignored.
#[cfg(all(not(test), not(target_arch = "wasm32")))]
fn site_file_values() -> &'static [(Key, String)] {
    use std::sync::OnceLock;
    static VALUES: OnceLock<Vec<(Key, String)>> = OnceLock::new();
    VALUES.get_or_init(|| {
        let Some(path) = site_file_path() else {
            return Vec::new();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        match parse_site_file(&text) {
            Ok(pairs) => pairs
                .into_iter()
                .filter_map(|(k, v)| match Key::from_name(&k) {
                    Some(key) => Some((key, v)),
                    None => {
                        eprintln!("{}: unknown site key `{k}`", path.display());
                        None
                    }
                })
                .collect(),
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                Vec::new()
            }
        }
    })
}

/// A key's value; empty counts as unset.
pub fn value(key: Key) -> Option<String> {
    lookup(key).filter(|v| !v.trim().is_empty())
}

/// [`Key::HttpCommand`] as argv: a JSON array, or whitespace-separated
/// words (handy in an env var).
pub fn http_command() -> Option<Vec<String>> {
    let raw = value(Key::HttpCommand)?;
    let argv = parse_command(&raw);
    (!argv.is_empty()).then_some(argv)
}

fn parse_command(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    if raw.starts_with('[') {
        serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
    } else {
        raw.split_whitespace().map(str::to_string).collect()
    }
}

/// A command for one site API request, ready for request arguments and
/// the URL: [`Key::HttpCommand`] when set, else `curl -sS`.
#[cfg(not(target_arch = "wasm32"))]
pub fn api_command() -> std::process::Command {
    let argv = http_command().unwrap_or_else(|| vec!["curl".into(), "-sS".into()]);
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd
}

/// A source repo on a GitHub-API host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub host: &'static str,
    pub owner: &'static str,
    pub repo: &'static str,
    pub git_ref: &'static str,
}

/// `host/owner/repo[@ref]`; the ref defaults to `HEAD`.
pub fn parse_repo(spec: &str) -> Option<(String, String, String, String)> {
    let spec = spec.trim().trim_start_matches("https://");
    let (path, git_ref) = spec.split_once('@').unwrap_or((spec, "HEAD"));
    let mut parts = path.trim_end_matches('/').splitn(3, '/');
    let host = parts.next()?;
    let owner = parts.next()?;
    let repo = parts.next()?.trim_end_matches(".git");
    if [host, owner, repo]
        .iter()
        .any(|p| p.is_empty() || p.contains('/'))
        || git_ref.is_empty()
    {
        return None;
    }
    Some((
        host.to_string(),
        owner.to_string(),
        repo.to_string(),
        git_ref.to_string(),
    ))
}

/// The configured firmware repo, resolved once per process.
pub fn firmware_repo() -> Option<&'static Repo> {
    use std::sync::OnceLock;
    static REPO: OnceLock<Option<Repo>> = OnceLock::new();
    REPO.get_or_init(|| {
        let (host, owner, repo, git_ref) = parse_repo(&value(Key::FirmwareRepo)?)?;
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        Some(Repo {
            host: leak(host),
            owner: leak(owner),
            repo: leak(repo),
            git_ref: leak(git_ref),
        })
    })
    .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_file_parses_strings_arrays_and_comments() {
        let text = r#"
# comment
uploads_url = "https://u.example/api"  # trailing
http_command = ["tool", "curl", "-a", "app"]
"#;
        let pairs = parse_site_file(text).unwrap();
        assert_eq!(
            pairs[0],
            ("uploads_url".into(), "https://u.example/api".into())
        );
        assert_eq!(parse_command(&pairs[1].1), ["tool", "curl", "-a", "app"]);
    }

    #[test]
    fn site_file_rejects_bare_values() {
        assert!(parse_site_file("uploads_url = https://x").is_err());
        assert!(parse_site_file("uploads_url = 3").is_err());
        assert!(parse_site_file("just words").is_err());
    }

    #[test]
    fn command_accepts_words() {
        assert_eq!(
            parse_command("tool curl -a app"),
            ["tool", "curl", "-a", "app"]
        );
    }

    #[test]
    fn repo_spec_parses() {
        assert_eq!(
            parse_repo("git.example.com/acme/firmware@release/1"),
            Some((
                "git.example.com".into(),
                "acme".into(),
                "firmware".into(),
                "release/1".into()
            ))
        );
        assert_eq!(parse_repo("github.com/o/r").unwrap().3, "HEAD");
        assert!(parse_repo("github.com/o").is_none());
    }

    #[test]
    fn unit_tests_see_the_example_site() {
        let repo = firmware_repo().unwrap();
        assert_eq!((repo.host, repo.git_ref), ("git.example.com", "main"));
        assert!(value(Key::HttpCommand).is_none());
    }
}
