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

//! Bakes a site file into the build: `EE_SITE_CONFIG=<path> cargo build`
//! (or `trunk build`) exposes each key as `EE_SITE_<KEY>` to
//! `option_env!` in `src/site.rs`. Without it the build has no site.

include!("src/site_file.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=EE_SITE_CONFIG");
    let Some(path) = std::env::var_os("EE_SITE_CONFIG").filter(|p| !p.is_empty()) else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    println!("cargo:rerun-if-changed={}", path.display());
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("EE_SITE_CONFIG {}: {e}", path.display()));
    let pairs =
        parse_site_file(&text).unwrap_or_else(|e| panic!("EE_SITE_CONFIG {}: {e}", path.display()));
    for (key, value) in pairs {
        if value.contains('\n') {
            panic!("EE_SITE_CONFIG {}: `{key}` spans lines", path.display());
        }
        println!(
            "cargo:rustc-env=EE_SITE_{}={value}",
            key.to_ascii_uppercase()
        );
    }
}
