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

//! `event_explorer` — shared library backing both the desktop binaries
//! (`event_explorer` GUI, `elog_cli`) and the wasm/browser build.
//!
//! The two desktop bins (`src/main.rs`, `src/cli.rs`) consume the modules
//! here directly. The wasm build exposes `WebHandle` (below) via
//! `wasm-bindgen` so a thin `index.html` can boot the same `app::EventExplorerApp`
//! inside a browser canvas.

pub mod analysis;
pub mod app;
pub mod elog_parser;
pub mod site;

// CLI command logic lives in `src/cli/`; the `elog_cli` binary (`src/cli.rs`)
// is a thin shim over `cli::run`. Native-only (uses std::process, filesystem
// and SSH loaders), so it is gated out of the wasm/web build.
#[cfg(feature = "native-bins")]
#[path = "cli/mod.rs"]
pub mod cli;

// ---------------------------------------------------------------------------
// Web entry point
// ---------------------------------------------------------------------------
//
// Compiled only for `wasm32-unknown-unknown`. Mirrors the pattern from
// the eframe 0.27 docs: a `WebHandle` that wraps a `WebRunner`, with an
// async `start(canvas_id)` method that JS calls once on page load.
#[cfg(target_arch = "wasm32")]
mod web {
    use wasm_bindgen::prelude::*;

    /// Read the page's full query string (the part after `?`) so it
    /// can be fed to `ShareParams::from_query_string` and applied as
    /// soon as the wasm app boots. Drives both `?file=…` auto-load
    /// (existing) and the broader Share URL state — `session=`,
    /// `t=`, per-session toggles — that the in-app Share button
    /// emits.
    fn query_string_from_location() -> String {
        let Some(window) = web_sys::window() else {
            return String::new();
        };
        window.location().search().unwrap_or_default()
    }

    #[derive(Clone)]
    #[wasm_bindgen]
    pub struct WebHandle {
        runner: eframe::WebRunner,
    }

    #[wasm_bindgen]
    impl WebHandle {
        #[allow(clippy::new_without_default)]
        #[wasm_bindgen(constructor)]
        pub fn new() -> Self {
            // Surface Rust panics in the browser console with a real backtrace.
            console_error_panic_hook::set_once();
            eframe::WebLogger::init(log::LevelFilter::Debug).ok();
            Self {
                runner: eframe::WebRunner::new(),
            }
        }

        #[wasm_bindgen]
        pub async fn start(&self, canvas_id: &str) -> Result<(), JsValue> {
            let search = query_string_from_location();
            let params = crate::app::ShareParams::from_query_string(&search);
            let dark = crate::app::load_dark_mode();
            let web_options = eframe::WebOptions {
                follow_system_theme: false,
                default_theme: if dark {
                    eframe::Theme::Dark
                } else {
                    eframe::Theme::Light
                },
                ..Default::default()
            };
            self.runner
                .start(
                    canvas_id,
                    web_options,
                    Box::new(move |_cc| {
                        Box::new(crate::app::EventExplorerApp::new_empty_web(params))
                    }),
                )
                .await
        }

        #[wasm_bindgen]
        pub fn destroy(&self) {
            self.runner.destroy();
        }

        #[wasm_bindgen]
        pub fn has_panicked(&self) -> bool {
            self.runner.has_panicked()
        }

        #[wasm_bindgen]
        pub fn panic_message(&self) -> Option<String> {
            self.runner.panic_summary().map(|s| s.message())
        }

        #[wasm_bindgen]
        pub fn panic_callstack(&self) -> Option<String> {
            self.runner.panic_summary().map(|s| s.callstack())
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::WebHandle;
