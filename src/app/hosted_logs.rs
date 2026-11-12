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

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

#[allow(unused_imports)]
use super::*;

/// Sibling directory served next to a typical dev-host wasm drop
/// (`…/eventexplorer/` → `…/logs/`).
#[cfg(any(test, target_arch = "wasm32"))]
pub const HOSTED_LOGS_URL: &str = "../logs/";

/// How many subdirectory levels to flatten into the splash list.
#[cfg(target_arch = "wasm32")]
const HOSTED_LOGS_EXTRA_DEPTH: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedLogFile {
    pub name: String,
    pub url: String,
    pub size: Option<u64>,
    sort_ts: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub enum HostedLogs {
    #[default]
    Idle,
    Loading,
    Ready(Vec<HostedLogFile>),
    Hidden,
}

#[derive(Debug, PartialEq, Eq)]
struct AutoIndex {
    files: Vec<IndexFile>,
    dirs: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct IndexFile {
    href: String,
    size: Option<u64>,
    sort_ts: Option<u64>,
}

fn is_elog_href(href: &str) -> bool {
    let name = href.rsplit('/').next().unwrap_or(href);
    let n = name.to_ascii_lowercase();
    n.ends_with(".elog") || n.ends_with(".elog.gz")
}

fn is_dir_href(href: &str) -> bool {
    href.ends_with('/') && href != "../" && href != "./" && href != "/"
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

pub fn join_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") || href.starts_with('/') {
        return href.to_string();
    }
    let href = href.strip_prefix("./").unwrap_or(href);
    if base.ends_with('/') {
        format!("{base}{href}")
    } else {
        format!("{base}/{href}")
    }
}

fn relative_name(root: &str, url: &str) -> String {
    let stripped = url.strip_prefix(root).unwrap_or(url);
    let stripped = stripped.strip_prefix('/').unwrap_or(stripped);
    if stripped.is_empty() {
        url.rsplit('/').next().unwrap_or(url).to_string()
    } else {
        stripped.to_string()
    }
}

fn parse_listing_meta(after_anchor: &str) -> (Option<u64>, Option<u64>) {
    let rest = after_anchor
        .find("</a>")
        .map(|i| after_anchor[i + 4..].trim())
        .unwrap_or("")
        .replace("&nbsp;", " ");
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let size = tokens.last().and_then(|t| t.parse::<u64>().ok());
    let sort_ts = match tokens.as_slice() {
        [date, time, ..] => parse_nginx_date(date, time),
        _ => None,
    };
    (size, sort_ts)
}

fn pack_ts(year: u64, month: u64, day: u64, hour: u64, minute: u64) -> u64 {
    year * 100_000_000 + month * 1_000_000 + day * 10_000 + hour * 100 + minute
}

fn parse_nginx_date(date: &str, time: &str) -> Option<u64> {
    let mut dparts = date.split('-');
    let day: u64 = dparts.next()?.parse().ok()?;
    let month = month_num(dparts.next()?)?;
    let year: u64 = dparts.next()?.parse().ok()?;
    let mut tparts = time.split(':');
    let hour: u64 = tparts.next()?.parse().ok()?;
    let minute: u64 = tparts.next()?.parse().ok()?;
    Some(pack_ts(year, month, day, hour, minute))
}

fn parse_name_ts(name: &str) -> Option<u64> {
    let t = name.find('T')?;
    if t < 10 {
        return None;
    }
    let date = name.get(t - 10..t)?;
    let time = name.get(t + 1..t + 7)?;
    let mut dp = date.split('-');
    let year: u64 = dp.next()?.parse().ok()?;
    let month: u64 = dp.next()?.parse().ok()?;
    let day: u64 = dp.next()?.parse().ok()?;
    let hour: u64 = time.get(0..2)?.parse().ok()?;
    let minute: u64 = time.get(2..4)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(pack_ts(year, month, day, hour, minute))
}

fn month_num(m: &str) -> Option<u64> {
    Some(match m {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    })
}

fn parse_autoindex(html: &str) -> AutoIndex {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("href=\"") {
        rest = &rest[i + 6..];
        let Some(end) = rest.find('"') else { break };
        let href = html_unescape(&rest[..end]);
        let after = rest;
        rest = &rest[end + 1..];
        if href.starts_with('?') || href == "../" || href == "./" || href.is_empty() {
            continue;
        }
        if is_dir_href(&href) {
            dirs.push(href);
            continue;
        }
        if !is_elog_href(&href) {
            continue;
        }
        let line_end = after.find('\n').unwrap_or(after.len());
        let (size, sort_ts) = parse_listing_meta(&after[..line_end]);
        files.push(IndexFile {
            href,
            size,
            sort_ts,
        });
    }
    AutoIndex { files, dirs }
}

fn sort_hosted_logs(files: &mut [HostedLogFile]) {
    files.sort_by(|a, b| match (a.sort_ts, b.sort_ts) {
        (Some(x), Some(y)) => y.cmp(&x).then_with(|| a.name.cmp(&b.name)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.name.cmp(&b.name),
    });
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn fetch_text(url: &str) -> Result<String, String> {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{XmlHttpRequest, XmlHttpRequestResponseType};

    let xhr = XmlHttpRequest::new().map_err(|e| format!("XMLHttpRequest: {e:?}"))?;
    xhr.open_with_async("GET", url, true)
        .map_err(|e| format!("XHR open failed: {e:?}"))?;
    xhr.set_response_type(XmlHttpRequestResponseType::Text);

    let xhr_for_handlers = xhr.clone();
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let xhr_load = xhr_for_handlers.clone();
        let resolve_ok = resolve.clone();
        let reject_http = reject.clone();
        let onload = Closure::once(move || match xhr_load.status() {
            Ok(code) if (200..300).contains(&code) => match xhr_load.response_text() {
                Ok(Some(text)) => {
                    let _ = resolve_ok.call1(&JsValue::UNDEFINED, &JsValue::from_str(&text));
                }
                Ok(None) => {
                    let _ = resolve_ok.call1(&JsValue::UNDEFINED, &JsValue::from_str(""));
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
                &JsValue::from_str("network error fetching listing"),
            );
        });
        xhr_for_handlers.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();
    });

    xhr.send().map_err(|e| format!("XHR send failed: {e:?}"))?;
    let value = JsFuture::from(promise)
        .await
        .map_err(|e| format!("listing fetch rejected: {:?}", e))?;
    value
        .as_string()
        .ok_or_else(|| "listing was not text".into())
}

#[cfg(target_arch = "wasm32")]
async fn load_hosted_logs_at(
    root: &str,
    start_url: &str,
    extra_depth: u32,
) -> Result<Vec<HostedLogFile>, String> {
    let mut pending = vec![(start_url.to_string(), extra_depth)];
    let mut out = Vec::new();
    let mut saw_listing = false;
    while let Some((url, depth)) = pending.pop() {
        let html = fetch_text(&url).await?;
        let parsed = parse_autoindex(&html);
        if parsed.files.is_empty() && parsed.dirs.is_empty() {
            continue;
        }
        saw_listing = true;
        for f in parsed.files {
            let file_url = join_url(&url, &f.href);
            let name = relative_name(root, &file_url);
            let sort_ts = f.sort_ts.or_else(|| parse_name_ts(&name));
            out.push(HostedLogFile {
                name,
                url: file_url,
                size: f.size,
                sort_ts,
            });
        }
        if depth > 0 {
            for dir in parsed.dirs {
                pending.push((join_url(&url, &dir), depth - 1));
            }
        }
    }
    if !saw_listing {
        return Err("not a directory listing".into());
    }
    Ok(out)
}

#[cfg(target_arch = "wasm32")]
impl EventExplorerApp {
    pub(crate) fn start_hosted_logs_fetch(&mut self) {
        if !matches!(self.hosted_logs, HostedLogs::Idle) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.hosted_logs = HostedLogs::Loading;
        self.pending_hosted_logs = Some(rx);
        wasm_bindgen_futures::spawn_local(async move {
            let result = match load_hosted_logs_at(
                HOSTED_LOGS_URL,
                HOSTED_LOGS_URL,
                HOSTED_LOGS_EXTRA_DEPTH,
            )
            .await
            {
                Ok(mut files) if !files.is_empty() => {
                    sort_hosted_logs(&mut files);
                    HostedLogs::Ready(files)
                }
                _ => HostedLogs::Hidden,
            };
            let _ = tx.send(result);
        });
    }

    pub(crate) fn drain_hosted_logs(&mut self) {
        let Some(rx) = &self.pending_hosted_logs else {
            return;
        };
        match rx.try_recv() {
            Ok(state) => {
                self.hosted_logs = state;
                self.pending_hosted_logs = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.hosted_logs = HostedLogs::Hidden;
                self.pending_hosted_logs = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    pub(crate) fn show_hosted_logs(&mut self, ui: &mut egui::Ui) {
        match &self.hosted_logs {
            HostedLogs::Idle | HostedLogs::Hidden => return,
            HostedLogs::Loading => {
                ui.add_space(20.0);
                ui.label(
                    RichText::new("Looking for captures in ../logs/…")
                        .size(12.0)
                        .color(ui.visuals().weak_text_color()),
                );
                return;
            }
            HostedLogs::Ready(_) => {}
        }

        ui.add_space(22.0);
        let list_w = ui.available_width().min(560.0);
        let mut selected: Option<String> = None;
        // Same trick as the splash card: a fixed-width child so the
        // parent `vertical_centered` actually centers it. ScrollArea
        // otherwise expands to the window and the rows hug the left edge.
        ui.allocate_ui(egui::vec2(list_w, 320.0), |ui| {
            ui.set_width(list_w);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Logs").strong().size(15.0));
                ui.label(
                    RichText::new("../logs/")
                        .size(11.0)
                        .color(ui.visuals().weak_text_color()),
                );
            });
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(280.0)
                .id_source("hosted_logs_list")
                .show(ui, |ui| {
                    ui.set_width(list_w);
                    let HostedLogs::Ready(files) = &self.hosted_logs else {
                        return;
                    };
                    for f in files {
                        ui.horizontal(|ui| {
                            let resp = ui.add(
                                egui::Button::new(RichText::new(&f.name).size(13.0)).frame(false),
                            );
                            if resp.clicked() {
                                selected = Some(f.url.clone());
                            }
                            if let Some(size) = f.size {
                                ui.label(
                                    RichText::new(elog_parser::fmt_bytes(size))
                                        .size(11.0)
                                        .color(Color32::from_gray(120)),
                                );
                            }
                        });
                    }
                });
        });

        if let Some(url) = selected {
            push_file_query(std::slice::from_ref(&url));
            self.splash_url = url.clone();
            self.start_loading_urls(vec![url]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NGINX_LISTING: &str = r#"<html>
<head><title>Index of /~user/logs/</title></head>
<body>
<h1>Index of /~user/logs/</h1><hr><pre><a href="../">../</a>
<a href="ella-scale/">ella-scale/</a>                                        23-Sep-2026 14:11                   -
<a href="ella-c102.lab002.ix.elog.gz">ella-c102.lab002.ix.elog.gz</a>                        16-Sep-2026 13:00            95045753
<a href="spreadout-to-cores-96-2026-09-23T155934Z.elog.gz">spreadout-to-cores-96-2026-09-23T155934Z.elog.gz</a>   23-Sep-2026 17:37           518623067
<a href="notes.txt">notes.txt</a>                                          01-Jan-2026 00:00                 12
</pre><hr></body>
</html>
"#;

    #[test]
    fn parse_nginx_autoindex_keeps_elog_files_and_dirs() {
        let parsed = parse_autoindex(NGINX_LISTING);
        assert_eq!(parsed.dirs, vec!["ella-scale/".to_string()]);
        assert_eq!(parsed.files.len(), 2);
        assert_eq!(parsed.files[0].href, "ella-c102.lab002.ix.elog.gz");
        assert_eq!(parsed.files[0].size, Some(95_045_753));
        assert_eq!(
            parsed.files[1].href,
            "spreadout-to-cores-96-2026-09-23T155934Z.elog.gz"
        );
        assert_eq!(parsed.files[1].size, Some(518_623_067));
        assert!(parsed.files[1].sort_ts.unwrap() > parsed.files[0].sort_ts.unwrap());
    }

    #[test]
    fn join_and_relative_name_flatten_subdir_paths() {
        let url = join_url(
            &join_url(HOSTED_LOGS_URL, "ella-scale/"),
            "ella-fixed-12k.elog.gz",
        );
        assert_eq!(url, "../logs/ella-scale/ella-fixed-12k.elog.gz");
        assert_eq!(
            relative_name(HOSTED_LOGS_URL, &url),
            "ella-scale/ella-fixed-12k.elog.gz"
        );
    }

    #[test]
    fn sort_hosted_logs_newest_first() {
        let mut files = vec![
            HostedLogFile {
                name: "old.elog".into(),
                url: "../logs/old.elog".into(),
                size: None,
                sort_ts: Some(pack_ts(2026, 9, 16, 13, 0)),
            },
            HostedLogFile {
                name: "new.elog".into(),
                url: "../logs/new.elog".into(),
                size: None,
                sort_ts: Some(pack_ts(2026, 9, 23, 17, 37)),
            },
            HostedLogFile {
                name: "late-evening.elog".into(),
                url: "../logs/late-evening.elog".into(),
                size: None,
                sort_ts: Some(pack_ts(2026, 9, 22, 20, 1)),
            },
        ];
        sort_hosted_logs(&mut files);
        assert_eq!(files[0].name, "new.elog");
        assert_eq!(files[1].name, "late-evening.elog");
        assert_eq!(files[2].name, "old.elog");
    }

    #[test]
    fn name_timestamp_is_a_fallback_sort_key() {
        assert_eq!(
            parse_name_ts("spreadout-to-cores-96-2026-09-23T155934Z.elog.gz"),
            Some(pack_ts(2026, 9, 23, 15, 59))
        );
    }
}
