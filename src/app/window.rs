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

#[allow(unused_imports)]
use super::*;

// ---------------------------------------------------------------------------
// Window state persistence
// ---------------------------------------------------------------------------

pub const DEFAULT_WINDOW_WIDTH: f32 = 1400.0;
pub const DEFAULT_WINDOW_HEIGHT: f32 = 900.0;

pub struct WindowState {
    pub pos_x: f32,
    pub pos_y: f32,
    pub has_position: bool,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
    pub monitor_width: f32,
    pub monitor_height: f32,
}

#[cfg(not(target_arch = "wasm32"))]
pub fn window_state_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("event_explorer").join("window_state.txt"))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_window_state() -> Option<WindowState> {
    let path = window_state_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let mut pos_x = None;
    let mut pos_y = None;
    let mut has_position = false;
    let mut width = None;
    let mut height = None;
    let mut maximized = false;
    let mut monitor_width = None;
    let mut monitor_height = None;

    for line in content.lines() {
        if let Some((key, val)) = line.split_once('=') {
            match key.trim() {
                "pos_x" => pos_x = val.trim().parse().ok(),
                "pos_y" => pos_y = val.trim().parse().ok(),
                "has_position" => has_position = val.trim() == "true",
                "width" => width = val.trim().parse().ok(),
                "height" => height = val.trim().parse().ok(),
                "maximized" => maximized = val.trim() == "true",
                "monitor_width" => monitor_width = val.trim().parse().ok(),
                "monitor_height" => monitor_height = val.trim().parse().ok(),
                _ => {}
            }
        }
    }

    Some(WindowState {
        pos_x: pos_x.unwrap_or(0.0),
        pos_y: pos_y.unwrap_or(0.0),
        has_position,
        width: width?,
        height: height?,
        maximized,
        monitor_width: monitor_width?,
        monitor_height: monitor_height?,
    })
}

/// Query the primary monitor's physical resolution via xrandr.
/// Returns None if xrandr is unavailable or the output can't be parsed.
#[cfg(all(not(target_arch = "wasm32"), not(target_os = "macos")))]
pub fn query_primary_monitor() -> Option<(f32, f32)> {
    let output = std::process::Command::new("xrandr")
        .arg("--current")
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut in_primary = false;
    for line in stdout.lines() {
        if line.contains(" connected") {
            in_primary = line.contains("primary");
        }
        if in_primary && line.contains('*') {
            let mode = line.split_whitespace().next()?;
            let mut parts = mode.split('x');
            let w: f32 = parts.next()?.parse().ok()?;
            let h: f32 = parts.next()?.parse().ok()?;
            return Some((w, h));
        }
    }
    None
}

/// Query the native display directly: macOS does not need XQuartz or xrandr.
#[cfg(target_os = "macos")]
pub fn query_primary_monitor() -> Option<(f32, f32)> {
    let display = core_graphics::display::CGDisplay::main();
    let (width, height) = (display.pixels_wide(), display.pixels_high());
    (width > 0 && height > 0).then_some((width as f32, height as f32))
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "macos")))]
pub fn query_x11_window_position() -> Option<(f32, f32)> {
    let output = std::process::Command::new("xwininfo")
        .args(["-name", "EventExplorer"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut x = None;
    let mut y = None;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix("Absolute upper-left X:") {
            x = val.trim().parse().ok();
        } else if let Some(val) = trimmed.strip_prefix("Absolute upper-left Y:") {
            y = val.trim().parse().ok();
        }
    }
    Some((x?, y?))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save_window_state(state: &WindowState) {
    let Some(path) = window_state_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = format!(
        "pos_x={}\npos_y={}\nhas_position={}\nwidth={}\nheight={}\nmaximized={}\nmonitor_width={}\nmonitor_height={}",
        state.pos_x, state.pos_y, state.has_position,
        state.width, state.height,
        state.maximized, state.monitor_width, state.monitor_height,
    );
    let _ = std::fs::write(&path, content);
}
