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

//! `upload` subcommand: push local captures to elog-access's Uploads
//! sharing service (presigned S3 PUT, 365-day retention) and print a
//! shareable EventExplorer link. Skips the full parse — the bytes go
//! to S3 unopened, so only the magic is checked.

pub(crate) use crate::cli::*;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

struct UploadJob<'a> {
    path: &'a str,
    cache_name: String,
    size: u64,
}

fn looks_like_capture(path: &str) -> std::io::Result<bool> {
    let mut head = [0u8; 4];
    let mut f = File::open(path)?;
    let n = f.read(&mut head)?;
    let elog = n == 4 && &head == b"ELOG";
    let gzip = n >= 2 && head[0] == 0x1f && head[1] == 0x8b;
    Ok(elog || gzip)
}

#[cfg(target_os = "macos")]
fn open_in_browser(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

#[cfg(not(target_os = "macos"))]
fn open_in_browser(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

pub fn cmd_upload(files: &[String], args: &[String]) {
    let mut cache: Option<String> = None;
    let mut note = String::new();
    let mut open = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--cache" => {
                cache = args.get(i + 1).cloned();
                if cache.is_none() {
                    eprintln!("upload: --cache needs a value");
                    process::exit(1);
                }
                i += 2;
            }
            "--note" => {
                note = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--open" => {
                open = true;
                i += 1;
            }
            other => {
                eprintln!("upload: unknown argument {other}");
                process::exit(1);
            }
        }
    }

    let mut jobs: Vec<UploadJob> = Vec::with_capacity(files.len());
    for f in files {
        if is_remote_path(f).is_some() {
            eprintln!("Error: {f} is remote; run `elog_cli upload` on that host instead");
            process::exit(1);
        }
        match looks_like_capture(f) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("Error: {f} is not an elog capture (.elog or .elog.gz)");
                process::exit(1);
            }
            Err(e) => {
                eprintln!("Error: cannot read {f}: {e}");
                process::exit(1);
            }
        }
        let size = match std::fs::metadata(f) {
            Ok(m) => m.len(),
            Err(e) => {
                eprintln!("Error: cannot stat {f}: {e}");
                process::exit(1);
            }
        };
        let cache_name = match cache.clone().or_else(|| {
            let inferred = infer_cache_name(f);
            (!inferred.is_empty()).then_some(inferred)
        }) {
            Some(name) => name,
            None => {
                eprintln!("Error: cannot infer a cache name from {f}; pass --cache NAME");
                process::exit(1);
            }
        };
        jobs.push(UploadJob {
            path: f,
            cache_name,
            size,
        });
    }

    // Sign-in probe: fails fast with the actionable error (no server
    // configured, HTTP command missing, not signed in) before any bytes move.
    let (user, max_bytes) = match uploads_whoami() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    };
    for job in &jobs {
        if job.size == 0 || job.size > max_bytes {
            eprintln!(
                "Error: {} is {size} bytes; uploads are 1 byte..{} GiB",
                job.path,
                max_bytes / (1024 * 1024 * 1024),
                size = job.size
            );
            process::exit(1);
        }
    }

    // The progress Arc updates from curl's stderr on the upload
    // thread (upload_file is blocking); this thread renders it.
    let progress = Arc::new(AtomicU32::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let bar = {
        let progress = progress.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                eprint!("\r  [{:>3}%]", progress.load(Ordering::Relaxed) / 10);
                let _ = std::io::Write::flush(&mut std::io::stderr());
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            eprint!("\r            \r");
        })
    };

    let mut urls: Vec<String> = Vec::with_capacity(jobs.len());
    let n = jobs.len().max(1) as u32;
    for (i, job) in jobs.iter().enumerate() {
        let i = i as u32;
        eprintln!(
            "Uploading {} ({}) for {}…",
            job.path,
            fmt_bytes(job.size),
            job.cache_name
        );
        let result = upload_file(
            std::path::Path::new(job.path),
            &job.cache_name,
            &note,
            &progress,
            (i * 1000 / n, (i + 1) * 1000 / n),
            &|_s| {},
        );
        match result {
            Ok(rec) => urls.push(rec.canonical_url()),
            Err(e) => {
                done.store(true, Ordering::Relaxed);
                let _ = bar.join();
                eprintln!("Error: upload of {} failed: {e}", job.path);
                process::exit(1);
            }
        }
    }
    done.store(true, Ordering::Relaxed);
    let _ = bar.join();

    println!("Uploaded as {user}:");
    for url in &urls {
        println!("  {url}");
    }
    let link = eventexplorer_link(&urls, "");
    println!();
    println!("Open in EventExplorer:");
    println!("  {link}");
    if open {
        open_in_browser(&link);
    }
}
