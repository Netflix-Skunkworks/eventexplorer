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
// Load progress (permille + status text)
// ---------------------------------------------------------------------------

/// Shared state for reporting load progress to the UI.
///
/// Bundles two pieces of feedback:
/// - a permille (0–1000) progress fraction for a determinate bar
/// - a human-readable status string ("Connecting to devhost…",
///   "Downloaded 4.2 MiB", "Parsing events…", "Sorting sessions…")
///
/// Both halves are independently updated by the parser thread and read by
/// the loading screen on every frame.
/// A multi-file load hands each file a slice of the bar and a status
/// prefix naming it, so the reporting stages stay unaware of how many
/// files there are.
pub struct LoadProgress {
    permille: AtomicU32,
    status: Mutex<String>,
    window_lo: AtomicU32,
    window_span: AtomicU32,
    prefix: Mutex<String>,
}

impl LoadProgress {
    pub fn new() -> Self {
        Self {
            permille: AtomicU32::new(0),
            status: Mutex::new(String::new()),
            window_lo: AtomicU32::new(0),
            window_span: AtomicU32::new(1000),
            prefix: Mutex::new(String::new()),
        }
    }

    pub fn permille(&self) -> u32 {
        self.permille.load(Ordering::Relaxed)
    }

    /// Store a 0–1000 fraction of the current window, mapped onto the
    /// full bar.
    pub fn store_permille(&self, p: u32) {
        let lo = self.window_lo.load(Ordering::Relaxed);
        let span = self.window_span.load(Ordering::Relaxed);
        let scaled = lo + (p.min(1000) as u64 * span as u64 / 1000) as u32;
        self.permille.store(scaled.min(1000), Ordering::Relaxed);
    }

    /// The `[lo, hi)` range [`Self::set_window`] last installed.
    pub fn window(&self) -> (u32, u32) {
        let lo = self.window_lo.load(Ordering::Relaxed);
        let span = self.window_span.load(Ordering::Relaxed);
        (lo, lo.saturating_add(span).min(1000))
    }

    /// Map subsequent [`Self::store_permille`] calls onto `[lo, hi)` of
    /// the bar. `set_window(0, 1000)` is the identity.
    pub fn set_window(&self, lo: u32, hi: u32) {
        self.window_lo.store(lo.min(1000), Ordering::Relaxed);
        self.window_span
            .store(hi.saturating_sub(lo).min(1000), Ordering::Relaxed);
    }

    pub fn status(&self) -> String {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn set_status(&self, s: impl Into<String>) {
        let prefix = self.prefix.lock().map(|p| p.clone()).unwrap_or_default();
        if let Ok(mut g) = self.status.lock() {
            *g = format!("{prefix}{}", s.into());
        }
    }

    /// Prepend `prefix` to every subsequent status line, e.g. `"[2/3] "`.
    pub fn set_prefix(&self, prefix: impl Into<String>) {
        if let Ok(mut g) = self.prefix.lock() {
            *g = prefix.into();
        }
    }
}

impl Default for LoadProgress {
    fn default() -> Self {
        Self::new()
    }
}

/// Format a byte count for human display ("4.2 MiB", "812 KiB", …).
pub fn fmt_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if n < KIB {
        format!("{n} B")
    } else if n < MIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else if n < GIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else {
        format!("{:.2} GiB", n as f64 / GIB as f64)
    }
}
