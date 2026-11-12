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

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use event_explorer::app;

fn load_icon() -> eframe::egui::IconData {
    let png_bytes = include_bytes!("../assets/icon_256.png");
    let img = image::load_from_memory(png_bytes)
        .expect("Failed to decode embedded icon")
        .into_rgba8();
    let (width, height) = img.dimensions();
    eframe::egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }
}

fn main() -> eframe::Result<()> {
    #[cfg(target_os = "linux")]
    if std::env::var("DISPLAY").is_ok() {
        std::env::remove_var("WAYLAND_DISPLAY");
    }

    let current_monitor = app::query_primary_monitor();
    let saved_state = app::load_window_state();

    let screen_changed = match (&saved_state, current_monitor) {
        (Some(saved), Some((cur_w, cur_h))) => {
            (saved.monitor_width - cur_w).abs() > 1.0 || (saved.monitor_height - cur_h).abs() > 1.0
        }
        _ => false,
    };

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([app::DEFAULT_WINDOW_WIDTH, app::DEFAULT_WINDOW_HEIGHT])
        .with_icon(std::sync::Arc::new(load_icon()))
        .with_app_id("event-explorer");

    if let Some(ref state) = saved_state {
        if !screen_changed {
            viewport = viewport.with_inner_size([state.width, state.height]);
            if state.has_position {
                viewport = viewport.with_position([state.pos_x, state.pos_y]);
            }
            if state.maximized {
                viewport = viewport.with_maximized(true);
            }
        }
    }

    let dark = app::load_dark_mode();
    let native_options = eframe::NativeOptions {
        viewport,
        follow_system_theme: false,
        default_theme: if dark {
            eframe::Theme::Dark
        } else {
            eframe::Theme::Light
        },
        ..Default::default()
    };

    // Every path on the command line, so a shell glob over a rolled set
    // (`event_explorer capture-*.elog`) opens as one continuous capture.
    let cli_paths: Vec<std::path::PathBuf> = std::env::args()
        .skip(1)
        .map(std::path::PathBuf::from)
        .collect();

    eframe::run_native(
        "EventExplorer",
        native_options,
        Box::new(move |_cc| {
            let mut app = app::EventExplorerApp::new_empty(current_monitor);
            if !cli_paths.is_empty() {
                app.load_from_cli(cli_paths);
            }
            Box::new(app)
        }),
    )
}
