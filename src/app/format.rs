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

pub fn format_pct(pct: f64) -> String {
    if pct >= 98.5 {
        format!("P{:.1}", pct)
    } else if pct >= 10.0 {
        format!("P{:.0}", pct)
    } else {
        format!("P{:.1}", pct)
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

pub fn format_time_span(seconds: f64) -> String {
    if seconds < 0.001 {
        format!("{:.1} µs", seconds * 1_000_000.0)
    } else if seconds < 1.0 {
        format!("{:.3} ms", seconds * 1_000.0)
    } else if seconds < 60.0 {
        format!("{:.3} s", seconds)
    } else {
        // Round to tenths before splitting out the minutes, so a span a
        // hair under the minute (a rolled capture's boundary sits at an
        // exact minute, one sample short) reads as "180m 0.0s" rather
        // than "179m 60.0s".
        let tenths = (seconds * 10.0).round() as u64;
        format!("{}m {:.1}s", tenths / 600, (tenths % 600) as f64 / 10.0)
    }
}

/// Convert "days since the Unix epoch" to a (year, month, day) triple
/// in the proleptic Gregorian calendar. Howard Hinnant's
/// `civil_from_days` algorithm — reproduced in pure stdlib so we don't
/// have to pull in `chrono` just for the aggregate info popup.
/// Valid for any 32-bit year. Months and days are 1-based.
pub fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    (y as i32, m as u32, d as u32)
}

/// Group a non-negative integer with comma thousand-separators
/// (e.g. `1234567` → `"1,234,567"`). Used in the aggregate info popup
/// where six- and seven-digit event counts are common and a raw run
/// of digits is hard to scan at a glance.
pub fn format_count(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

pub fn format_utc_us(utc_us: u64) -> String {
    if utc_us == 0 {
        return "(unknown)".to_string();
    }
    let secs = (utc_us / 1_000_000) as i64;
    let micros = (utc_us % 1_000_000) as u32;
    let days = secs.div_euclid(86_400);
    let secs_in_day = secs.rem_euclid(86_400);
    let hour = secs_in_day / 3_600;
    let minute = (secs_in_day / 60) % 60;
    let second = secs_in_day % 60;
    let (y, mo, d) = civil_from_days(days);
    format!(
        "{y:04}-{mo:02}-{d:02} {hour:02}:{minute:02}:{second:02}.{micros:03} UTC",
        micros = micros / 1_000,
    )
}

/// Wall-clock time-of-day for a UTC microsecond timestamp, no date, no
/// `UTC+` prefix: `HH:MM:SS`, or `HH:MM:SS.mmm` when `show_millis` is
/// set. The shared clock format behind [`format_axis_utc`] and
/// [`format_tooltip_utc`].
pub fn format_utc_clock_us(utc_us: u64, show_millis: bool) -> String {
    let secs_in_day = ((utc_us / 1_000_000) as i64).rem_euclid(86_400);
    let hour = secs_in_day / 3_600;
    let minute = (secs_in_day / 60) % 60;
    let second = secs_in_day % 60;
    if show_millis {
        let millis = ((utc_us % 1_000_000) / 1_000) as u32;
        format!("{hour:02}:{minute:02}:{second:02}.{millis:03}")
    } else {
        format!("{hour:02}:{minute:02}:{second:02}")
    }
}

fn utc_us_at(seconds: f64, origin_utc_us: u64) -> u64 {
    (origin_utc_us as f64 + seconds * 1_000_000.0)
        .max(0.0)
        .round() as u64
}

/// Axis-tick UTC label, given `origin_utc_us` — the wall-clock timestamp
/// at the trace-relative origin — and `precision_secs`, the current
/// tick's step size in seconds. Always bare (`HH:MM:SS[.mmm]`, no
/// `UTC+` prefix): repeating "UTC" on every tick is redundant and, at
/// tight tick spacing, wide enough that egui_plot's per-tick width
/// check (it silently skips any label wider than the gap to its
/// neighbor) can cull most of the row. Below one second of spacing the
/// label carries milliseconds; at coarser spacing it doesn't, so ticks
/// don't all show the same uninformative trailing `.000`. Callers show
/// the `UTC+` prefix once, on the `t=0` tick, via [`format_tooltip_utc`].
pub fn format_axis_utc(seconds: f64, origin_utc_us: u64, precision_secs: f64) -> String {
    let utc_us = utc_us_at(seconds, origin_utc_us);
    format_utc_clock_us(utc_us, precision_secs < 1.0)
}

/// Always-prefixed UTC label: `UTC+HH:MM:SS`, or `UTC+HH:MM:SS.mmm` once
/// `precision_secs` drops below one second. Used for the cursor tooltip
/// (a floating box, not width-constrained by neighboring ticks) and for
/// the axis's single left-edge anchor tick, which carries the "UTC+"
/// prefix so it doesn't have to repeat on every other [`format_axis_utc`]
/// tick.
pub fn format_tooltip_utc(seconds: f64, origin_utc_us: u64, precision_secs: f64) -> String {
    let utc_us = utc_us_at(seconds, origin_utc_us);
    format!("UTC+{}", format_utc_clock_us(utc_us, precision_secs < 1.0))
}

/// Format one time-axis tick, given `range` — the axis's current visible
/// bounds, exactly what egui_plot passes as an [`AxisHints`] formatter's
/// third argument. The mark nearest the left edge of `range` renders as
/// the row's one `UTC+`-prefixed anchor; every other mark is bare. Unlike
/// checking `mark.value.abs() < 1e-9`, this anchors on wherever the view
/// actually starts — the un-zoomed capture start, a zoomed-in window, or
/// a strip whose data doesn't begin exactly at trace `t=0` — so the
/// anchor is never lost to a coincidence of the grid spacer's "nice"
/// numbers. [`utc_x_grid_spacer`] guarantees a mark actually exists
/// there to format. Marks closer than ~1.35 grid steps to that edge
/// are unlabeled: egui_plot centers each galley on its tick, and the
/// extra `UTC+` width would otherwise paint on top of the next clock.
pub fn format_time_axis_tick(
    mark_value: f64,
    mark_step_size: f64,
    origin_utc_us: u64,
    range: &std::ops::RangeInclusive<f64>,
) -> String {
    let view_secs = (range.end() - range.start()).abs();
    let eps = view_secs.max(1.0) * 1e-6;
    let start = *range.start();
    if (mark_value - start).abs() < eps {
        format_tooltip_utc(mark_value, origin_utc_us, view_secs / 6.0)
    } else if mark_value > start && (mark_value - start) < mark_step_size * 1.35 {
        String::new()
    } else {
        format_axis_utc(mark_value, origin_utc_us, mark_step_size)
    }
}

/// UTC timestamp (microseconds) corresponding to `host_t0_us`, a
/// kernel-monotonic origin some time panel's x-axis is relative to.
/// `None` unless `utc_time_labels` (the toggle) is on and `file_info`
/// carried a usable `start_utc_us`. Every time panel's x=0 means a
/// different monotonic instant (the per-session / System panels use
/// `capture_start`, the Flame tab's CPU strip uses its first bin), so
/// callers pass their own `host_t0_us` and this does the monotonic →
/// wall-clock shift relative to the file header's `(capture_start,
/// start_utc_us)` anchor pair.
///
/// A plain free function (rather than an `EventExplorerApp` method) so
/// callers can invoke it with a disjoint-field snapshot — e.g.
/// `utc_axis_origin(self.utc_time_labels, self.file_info.as_ref(), t0)`
/// — while some *other* field of `self` (like `self.flame`) is already
/// mutably borrowed.
pub fn utc_axis_origin(
    utc_time_labels: bool,
    file_info: Option<&FileInfo>,
    host_t0_us: u64,
) -> Option<u64> {
    if !utc_time_labels {
        return None;
    }
    let fi = file_info?;
    if fi.start_utc_us == 0 {
        return None;
    }
    let delta_us = host_t0_us as i64 - fi.capture_start as i64;
    Some((fi.start_utc_us as i64 + delta_us).max(0) as u64)
}

/// Wall-clock UTC microseconds for `fi.capture_end` (the last event's
/// raw monotonic timestamp), anchored the same way `capture_start` maps
/// to `start_utc_us`. `0` when the file carried no usable UTC anchor.
pub fn capture_end_utc_us(fi: &FileInfo) -> u64 {
    if fi.start_utc_us == 0 {
        return 0;
    }
    let delta_us = fi.capture_end as i64 - fi.capture_start as i64;
    (fi.start_utc_us as i64 + delta_us).max(0) as u64
}

pub fn format_goodput(mbps: f64) -> String {
    if mbps >= 1000.0 {
        format!("{:.2} Gbps", mbps / 1000.0)
    } else if mbps >= 1.0 {
        format!("{:.1} Mbps", mbps)
    } else if mbps >= 0.001 {
        format!("{:.1} Kbps", mbps * 1000.0)
    } else {
        format!("{:.1} Mbps", mbps)
    }
}

/// Format a duration in seconds with sensible units (s / ms / us)
/// for the time-series chrome. Used by the bin width / selection
/// chips.
pub fn fmt_seconds(s: f64) -> String {
    if s.abs() >= 1.0 {
        format!("{s:.2} s")
    } else if s.abs() >= 1e-3 {
        format!("{:.0} ms", s * 1e3)
    } else if s.abs() >= 1e-6 {
        format!("{:.0} us", s * 1e6)
    } else {
        format!("{s:.3e} s")
    }
}

/// Compact number formatter for the flame-graph header / status line.
/// Rounds to one decimal of K / M; falls through to a comma-separated
/// integer for small values.
pub fn fmt_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        // 4 -> "4"; 1234 -> "1,234"
        let s = n.to_string();
        let mut out = String::with_capacity(s.len() + s.len() / 3);
        for (i, c) in s.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        out.chars().rev().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_left_edge_tick_keeps_prefix_and_skips_the_next_step() {
        let origin = 0_u64;
        let range = 0.0..=600.0;
        let step = 100.0;
        let edge = format_time_axis_tick(0.0, f64::INFINITY, origin, &range);
        assert!(edge.starts_with("UTC+"), "{edge}");
        assert!(format_time_axis_tick(100.0, step, origin, &range).is_empty());
        let later = format_time_axis_tick(200.0, step, origin, &range);
        assert!(!later.is_empty(), "{later}");
        assert!(!later.starts_with("UTC+"), "{later}");
    }
}
