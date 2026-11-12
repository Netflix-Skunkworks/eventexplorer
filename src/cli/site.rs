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

//! `site` subcommand: install and inspect the site config (`docs/site-config.md`).

use crate::site::{self, Key};
use std::process;

pub fn cmd_site(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("show") => site_show(),
        Some("install") if args.len() == 2 => site_install(&args[1]),
        _ => {
            eprintln!("Usage: elog_cli site show");
            eprintln!("       elog_cli site install <file|url>");
            process::exit(1);
        }
    }
}

fn site_show() {
    println!("Site files (first one with a key wins; $ELOG_<KEY> beats them all):");
    for path in site::site_files() {
        let state = if path.is_file() { "" } else { "  (missing)" };
        println!("  {}{state}", path.display());
    }
    println!();
    for key in Key::ALL {
        match site::value_with_origin(key) {
            Some((v, origin)) => println!("{:<20} = {v}\n{:<20}   from {origin}", key.name(), ""),
            None => println!("{:<20}   (unset)", key.name()),
        }
    }
}

fn site_install(source: &str) {
    let text = if source.starts_with("https://") || source.starts_with("http://") {
        match process::Command::new("curl")
            .args(["-fsSL", source])
            .output()
        {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
            Ok(out) => {
                eprintln!(
                    "Error: could not fetch {source}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                process::exit(1);
            }
            Err(e) => {
                eprintln!("Error: could not run curl: {e}");
                process::exit(1);
            }
        }
    } else {
        match std::fs::read_to_string(source) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Error: cannot read {source}: {e}");
                process::exit(1);
            }
        }
    };
    let pairs = match site::parse_site_file(&text) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {source} is not a site file: {e}");
            process::exit(1);
        }
    };
    if let Some((k, _)) = pairs.iter().find(|(k, _)| Key::from_name(k).is_none()) {
        eprintln!("Error: {source} has unknown key `{k}`");
        process::exit(1);
    }
    let Some(dest) = site::user_site_file() else {
        eprintln!("Error: no user config directory on this system");
        process::exit(1);
    };
    if let Some(dir) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("Error: cannot create {}: {e}", dir.display());
            process::exit(1);
        }
    }
    if let Err(e) = std::fs::write(&dest, text) {
        eprintln!("Error: cannot write {}: {e}", dest.display());
        process::exit(1);
    }
    println!("Installed {} key(s) to {}", pairs.len(), dest.display());
}
