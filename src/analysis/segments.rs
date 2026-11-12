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

//! Origin segment metadata and ground-truth video bitrate series.
//!
//! Parses CSV files containing origin video segment publish timestamps and byte
//! sizes (e.g. dual-encoder live streams from `us-east-1`), aligns them against
//! an `.elog` capture's UTC anchor (`start_utc_us`), and produces rate-over-time
//! series for the overlapping window.

/// One raw row parsed from an origin segments CSV file.
#[derive(Clone, Debug, Default)]
pub struct OriginSegmentRow {
    pub filename: String,
    pub date: String,
    pub time_str_enc1: Option<String>,
    pub time_str_enc2: Option<String>,
    pub size_enc1: Option<u64>,
    pub size_enc2: Option<u64>,
    pub utc_us_enc1: Option<u64>,
    pub utc_us_enc2: Option<u64>,
}

/// Aligned origin segment rate and size profile for an `.elog` capture.
#[derive(Clone, Debug)]
pub struct OriginSegmentsProfile {
    pub file_path: String,
    pub file_name: String,
    pub region: String,
    pub total_segments: usize,
    pub overlapping_segments: usize,
    pub nominal_duration_s: f64,
    /// Bitrate series `[t_s, rate_mbps]` relative to capture start.
    pub enc1_rate_mbps: Vec<[f64; 2]>,
    pub enc2_rate_mbps: Vec<[f64; 2]>,
    /// Segment size series `[t_s, size_mb]` relative to capture start.
    pub enc1_size_mb: Vec<[f64; 2]>,
    pub enc2_size_mb: Vec<[f64; 2]>,
    pub enc1_avg_mbps: f64,
    pub enc2_avg_mbps: f64,
    pub enc1_min_mbps: f64,
    pub enc1_max_mbps: f64,
    pub enc2_min_mbps: f64,
    pub enc2_max_mbps: f64,
    /// Capture window the series was aligned to, UTC microseconds.
    pub capture_start_utc_us: u64,
    pub capture_end_utc_us: u64,
    /// First and last segment timestamp in the loaded CSVs, UTC microseconds.
    pub segments_first_utc_us: u64,
    pub segments_last_utc_us: u64,
    /// One entry per loaded CSV, including files with no rows in the capture.
    pub files: Vec<OriginSegmentFileNote>,
    pub raw_rows: Vec<OriginSegmentRow>,
}

/// Overlap of one loaded CSV against the capture window.
#[derive(Clone, Debug)]
pub struct OriginSegmentFileNote {
    pub file_name: String,
    pub total_segments: usize,
    pub overlapping_segments: usize,
    pub first_utc_us: u64,
    pub last_utc_us: u64,
}

impl OriginSegmentsProfile {
    pub fn is_empty(&self) -> bool {
        self.enc1_rate_mbps.is_empty() && self.enc2_rate_mbps.is_empty()
    }
}

/// Convert (year, month, day) to days since Unix epoch (1970-01-01).
/// Howard Hinnant's algorithm (inverse of `civil_from_days`).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era as i64 * 146_097 + doe as i64 - 719_468
}

/// Returns true if US Daylight Saving Time (EDT) was in effect at the given local time.
/// In the US (since 2007), DST begins the second Sunday of March at 2:00 AM local,
/// and ends the first Sunday of November at 2:00 AM local.
pub fn is_us_dst(year: i32, month: u32, day: u32, hour: u32) -> bool {
    if !(3..=11).contains(&month) {
        return false;
    }
    if (4..=10).contains(&month) {
        return true;
    }
    if month == 3 {
        // March: 2nd Sunday at 2:00 AM
        let first_day_epoch_days = days_from_civil(year, 3, 1);
        let first_weekday = ((first_day_epoch_days + 4).rem_euclid(7)) as u32; // 0 = Sunday
        let days_to_first_sun = (7 - first_weekday) % 7;
        let second_sunday = 1 + days_to_first_sun + 7;
        if day < second_sunday {
            false
        } else if day > second_sunday {
            true
        } else {
            hour >= 2
        }
    } else {
        // November: 1st Sunday at 2:00 AM
        let first_day_epoch_days = days_from_civil(year, 11, 1);
        let first_weekday = ((first_day_epoch_days + 4).rem_euclid(7)) as u32; // 0 = Sunday
        let days_to_first_sun = (7 - first_weekday) % 7;
        let first_sunday = 1 + days_to_first_sun;
        if day < first_sunday {
            true
        } else if day > first_sunday {
            false
        } else {
            hour < 2
        }
    }
}

fn parse_month(s: &str) -> Option<u32> {
    match s.to_ascii_lowercase().as_str() {
        "january" | "jan" => Some(1),
        "february" | "feb" => Some(2),
        "march" | "mar" => Some(3),
        "april" | "apr" => Some(4),
        "may" => Some(5),
        "june" | "jun" => Some(6),
        "july" | "jul" => Some(7),
        "august" | "aug" => Some(8),
        "september" | "sep" | "sept" => Some(9),
        "october" | "oct" => Some(10),
        "november" | "nov" => Some(11),
        "december" | "dec" => Some(12),
        _ => None,
    }
}

/// Parse date string in forms like "October 6th 2026", "Oct 6 2026", "2026-10-06".
pub fn parse_date(s: &str) -> Option<(i32, u32, u32)> {
    let s = s.trim();
    if s.contains('-') {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() == 3 {
            let y: i32 = parts[0].parse().ok()?;
            let m: u32 = parts[1].parse().ok()?;
            let d: u32 = parts[2].parse().ok()?;
            return Some((y, m, d));
        }
    }
    let words: Vec<&str> = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .collect();
    if words.len() >= 3 {
        if let Some(m) = parse_month(words[0]) {
            let day_clean: String = words[1].chars().filter(|c| c.is_ascii_digit()).collect();
            let d: u32 = day_clean.parse().ok()?;
            let y: i32 = words[2].parse().ok()?;
            return Some((y, m, d));
        }
        if let Some(m) = parse_month(words[1]) {
            let day_clean: String = words[0].chars().filter(|c| c.is_ascii_digit()).collect();
            let d: u32 = day_clean.parse().ok()?;
            let y: i32 = words[2].parse().ok()?;
            return Some((y, m, d));
        }
    }
    None
}

/// Parse time string like "12:01:24.735 PM" or "14:01:24.735".
pub fn parse_time_of_day(s: &str) -> Option<(u32, u32, u32, u32)> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("null") || s.eq_ignore_ascii_case("undefined") {
        return None;
    }
    let is_pm = s.to_ascii_uppercase().ends_with("PM");
    let is_am = s.to_ascii_uppercase().ends_with("AM");
    let has_meridiem = is_pm || is_am;
    let time_part = s.split_whitespace().next()?;
    let hms: Vec<&str> = time_part.split(':').collect();
    if hms.len() < 3 {
        return None;
    }
    let mut hour: u32 = hms[0].parse().ok()?;
    let minute: u32 = hms[1].parse().ok()?;
    let sec_parts: Vec<&str> = hms[2].split('.').collect();
    let second: u32 = sec_parts[0].parse().ok()?;
    let micros: u32 = if sec_parts.len() > 1 {
        let f = sec_parts[1];
        if f.len() >= 6 {
            f[..6].parse().ok()?
        } else {
            let mut buf = f.to_string();
            while buf.len() < 6 {
                buf.push('0');
            }
            buf.parse().ok()?
        }
    } else {
        0
    };

    if has_meridiem {
        if is_pm && hour < 12 {
            hour += 12;
        } else if is_am && hour == 12 {
            hour = 0;
        }
    }
    Some((hour, minute, second, micros))
}

/// Parse a date and time string into UTC microseconds since Unix epoch.
pub fn timestamp_to_utc_us(date_str: &str, time_str: &str, region: &str) -> Option<u64> {
    let (y, m, d) = parse_date(date_str)?;
    let (hour, min, sec, micros) = parse_time_of_day(time_str)?;
    let days = days_from_civil(y, m, d);
    let local_secs = days * 86_400 + hour as i64 * 3_600 + min as i64 * 60 + sec as i64;

    let offset_secs = if region.eq_ignore_ascii_case("us-east-1")
        || region.eq_ignore_ascii_case("eastern")
    {
        if is_us_dst(y, m, d, hour) {
            -4 * 3_600
        } else {
            -5 * 3_600
        }
    } else if region.eq_ignore_ascii_case("us-west-2") || region.eq_ignore_ascii_case("pacific") {
        if is_us_dst(y, m, d, hour) {
            -7 * 3_600
        } else {
            -8 * 3_600
        }
    } else {
        0
    };

    let utc_secs = local_secs - offset_secs;
    if utc_secs < 0 {
        return None;
    }
    Some(utc_secs as u64 * 1_000_000 + micros as u64)
}

/// Split a CSV line respecting double quotes.
pub fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    for c in line.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if c == ',' && !in_quotes {
            fields.push(field.trim().to_string());
            field.clear();
        } else {
            field.push(c);
        }
    }
    fields.push(field.trim().to_string());
    fields
}

/// Parse origin segments CSV content.
pub fn parse_origin_segments_csv(content: &str) -> (String, Vec<OriginSegmentRow>) {
    let mut lines = content.lines();
    let Some(header_line) = lines.next() else {
        return ("us-east-1".to_string(), Vec::new());
    };

    let headers = split_csv_line(header_line);
    let mut file_idx = None;
    let mut size_enc1_idx = None;
    let mut size_enc2_idx = None;
    let mut date_idx = None;
    let mut time_enc1_idx = None;
    let mut time_enc2_idx = None;

    let mut detected_region = "us-east-1".to_string();

    for (i, h) in headers.iter().enumerate() {
        let lower = h.to_ascii_lowercase();
        if lower.contains("diff") {
            continue;
        }

        if lower.contains("us-east-1") {
            detected_region = "us-east-1".to_string();
        } else if lower.contains("us-west-2") {
            detected_region = "us-west-2".to_string();
        } else if lower.contains("utc") {
            detected_region = "utc".to_string();
        }

        if lower.contains("file") || lower.contains("segment") || lower.contains("name") {
            file_idx = Some(i);
        } else if lower.contains("date") {
            date_idx = Some(i);
        } else if lower.contains("size") || lower.contains("bytes") {
            if lower.contains("enc1") || lower.contains("enc 1") || lower.contains("encoder1") {
                size_enc1_idx = Some(i);
            } else if lower.contains("enc2")
                || lower.contains("enc 2")
                || lower.contains("encoder2")
            {
                size_enc2_idx = Some(i);
            } else if size_enc1_idx.is_none() {
                size_enc1_idx = Some(i);
            }
        } else if lower.contains("time") {
            if lower.contains("enc1") || lower.contains("enc 1") || lower.contains("encoder1") {
                time_enc1_idx = Some(i);
            } else if lower.contains("enc2")
                || lower.contains("enc 2")
                || lower.contains("encoder2")
            {
                time_enc2_idx = Some(i);
            } else if time_enc1_idx.is_none() {
                time_enc1_idx = Some(i);
            }
        }
    }

    let mut rows = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let cols = split_csv_line(line);
        let get_col = |idx: Option<usize>| -> Option<String> {
            let i = idx?;
            let val = cols.get(i)?;
            if val.is_empty()
                || val.eq_ignore_ascii_case("null")
                || val.eq_ignore_ascii_case("undefined")
            {
                None
            } else {
                Some(val.clone())
            }
        };

        let filename = get_col(file_idx).unwrap_or_default();
        let date = get_col(date_idx).unwrap_or_default();
        let time_str_enc1 = get_col(time_enc1_idx);
        let time_str_enc2 = get_col(time_enc2_idx);

        let parse_size = |idx: Option<usize>| -> Option<u64> {
            let s = get_col(idx)?;
            let clean: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
            clean.parse().ok()
        };

        let size_enc1 = parse_size(size_enc1_idx);
        let size_enc2 = parse_size(size_enc2_idx);

        let utc_us_enc1 = time_str_enc1
            .as_deref()
            .and_then(|t| timestamp_to_utc_us(&date, t, &detected_region));
        let utc_us_enc2 = time_str_enc2
            .as_deref()
            .and_then(|t| timestamp_to_utc_us(&date, t, &detected_region));

        rows.push(OriginSegmentRow {
            filename,
            date,
            time_str_enc1,
            time_str_enc2,
            size_enc1,
            size_enc2,
            utc_us_enc1,
            utc_us_enc2,
        });
    }

    (detected_region, rows)
}

fn segment_utc_span(rows: &[OriginSegmentRow]) -> (u64, u64) {
    let mut lo = u64::MAX;
    let mut hi = 0u64;
    for row in rows {
        for ts in [row.utc_us_enc1, row.utc_us_enc2].into_iter().flatten() {
            lo = lo.min(ts);
            hi = hi.max(ts);
        }
    }
    if lo == u64::MAX {
        (0, 0)
    } else {
        (lo, hi)
    }
}

fn capture_end_utc(start_utc_us: u64, capture_duration_s: f64) -> u64 {
    if start_utc_us == 0 {
        return 0;
    }
    start_utc_us.saturating_add((capture_duration_s.max(0.0) * 1_000_000.0).round() as u64)
}

/// Align origin segments to an `.elog` capture's UTC anchor and build rate series.
pub fn align_origin_segments(
    rows: &[OriginSegmentRow],
    file_path: &str,
    region: &str,
    start_utc_us: u64,
    capture_duration_s: f64,
) -> OriginSegmentsProfile {
    let file_name = std::path::Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| file_path.to_string());
    let (segments_first_utc_us, segments_last_utc_us) = segment_utc_span(rows);
    let capture_end_utc_us = capture_end_utc(start_utc_us, capture_duration_s);

    if rows.is_empty() || start_utc_us == 0 {
        return OriginSegmentsProfile {
            file_path: file_path.to_string(),
            file_name,
            region: region.to_string(),
            total_segments: rows.len(),
            overlapping_segments: 0,
            nominal_duration_s: 2.0,
            enc1_rate_mbps: Vec::new(),
            enc2_rate_mbps: Vec::new(),
            enc1_size_mb: Vec::new(),
            enc2_size_mb: Vec::new(),
            enc1_avg_mbps: 0.0,
            enc2_avg_mbps: 0.0,
            enc1_min_mbps: 0.0,
            enc1_max_mbps: 0.0,
            enc2_min_mbps: 0.0,
            enc2_max_mbps: 0.0,
            capture_start_utc_us: start_utc_us,
            capture_end_utc_us,
            segments_first_utc_us,
            segments_last_utc_us,
            files: vec![file_note(
                file_path,
                rows,
                0,
                segments_first_utc_us,
                segments_last_utc_us,
            )],
            raw_rows: rows.to_vec(),
        };
    }

    let mut ordered: Vec<&OriginSegmentRow> = rows.iter().collect();
    ordered.sort_by_key(|r| r.utc_us_enc1.or(r.utc_us_enc2).unwrap_or(u64::MAX));

    // Determine nominal segment duration from median interval between successive segments.
    let mut intervals = Vec::new();
    let mut prev_utc: Option<u64> = None;
    for r in &ordered {
        let ts = r.utc_us_enc1.or(r.utc_us_enc2);
        if let Some(t) = ts {
            if let Some(p) = prev_utc {
                if t > p {
                    let dt = (t - p) as f64 / 1_000_000.0;
                    if dt > 0.1 && dt < 30.0 {
                        intervals.push(dt);
                    }
                }
            }
            prev_utc = Some(t);
        }
    }
    let nominal_duration_s = if intervals.len() >= 5 {
        intervals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = intervals[intervals.len() / 2];
        // Round to nearest tenth of a second (e.g. 2.0s)
        (med * 10.0).round() / 10.0
    } else {
        2.0
    }
    .max(0.1);

    let mut enc1_rate_mbps = Vec::new();
    let mut enc2_rate_mbps = Vec::new();
    let mut enc1_size_mb = Vec::new();
    let mut enc2_size_mb = Vec::new();

    let mut overlapping_segments = 0;
    let mut enc1_rates = Vec::new();
    let mut enc2_rates = Vec::new();

    // Include points within [-nominal_duration_s, capture_duration_s + nominal_duration_s]
    // so lines reach the capture boundaries cleanly.
    let lo_limit = -nominal_duration_s;
    let hi_limit = capture_duration_s + nominal_duration_s;

    for r in &ordered {
        let mut row_in_window = false;

        if let (Some(utc_1), Some(sz_1)) = (r.utc_us_enc1, r.size_enc1) {
            let t_s = (utc_1 as i64 - start_utc_us as i64) as f64 / 1_000_000.0;
            if t_s >= lo_limit && t_s <= hi_limit {
                let mbps = (sz_1 as f64 * 8.0) / (nominal_duration_s * 1_000_000.0);
                let size_mb = sz_1 as f64 / (1024.0 * 1024.0);
                enc1_rate_mbps.push([t_s, mbps]);
                enc1_size_mb.push([t_s, size_mb]);
                if t_s >= 0.0 && t_s <= capture_duration_s {
                    row_in_window = true;
                    enc1_rates.push(mbps);
                }
            }
        }

        if let (Some(utc_2), Some(sz_2)) = (r.utc_us_enc2, r.size_enc2) {
            let t_s = (utc_2 as i64 - start_utc_us as i64) as f64 / 1_000_000.0;
            if t_s >= lo_limit && t_s <= hi_limit {
                let mbps = (sz_2 as f64 * 8.0) / (nominal_duration_s * 1_000_000.0);
                let size_mb = sz_2 as f64 / (1024.0 * 1024.0);
                enc2_rate_mbps.push([t_s, mbps]);
                enc2_size_mb.push([t_s, size_mb]);
                if t_s >= 0.0 && t_s <= capture_duration_s {
                    row_in_window = true;
                    enc2_rates.push(mbps);
                }
            }
        }

        if row_in_window {
            overlapping_segments += 1;
        }
    }

    let stats = |vals: &[f64]| -> (f64, f64, f64) {
        if vals.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        let sum: f64 = vals.iter().sum();
        let avg = sum / vals.len() as f64;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for &v in vals {
            if v < min {
                min = v;
            }
            if v > max {
                max = v;
            }
        }
        (avg, min, max)
    };

    let (enc1_avg, enc1_min, enc1_max) = stats(&enc1_rates);
    let (enc2_avg, enc2_min, enc2_max) = stats(&enc2_rates);

    OriginSegmentsProfile {
        file_path: file_path.to_string(),
        file_name,
        region: region.to_string(),
        total_segments: rows.len(),
        overlapping_segments,
        nominal_duration_s,
        enc1_rate_mbps,
        enc2_rate_mbps,
        enc1_size_mb,
        enc2_size_mb,
        enc1_avg_mbps: enc1_avg,
        enc2_avg_mbps: enc2_avg,
        enc1_min_mbps: if enc1_min.is_finite() { enc1_min } else { 0.0 },
        enc1_max_mbps: if enc1_max.is_finite() { enc1_max } else { 0.0 },
        enc2_min_mbps: if enc2_min.is_finite() { enc2_min } else { 0.0 },
        enc2_max_mbps: if enc2_max.is_finite() { enc2_max } else { 0.0 },
        capture_start_utc_us: start_utc_us,
        capture_end_utc_us,
        segments_first_utc_us,
        segments_last_utc_us,
        files: vec![file_note(
            file_path,
            rows,
            overlapping_segments,
            segments_first_utc_us,
            segments_last_utc_us,
        )],
        raw_rows: rows.to_vec(),
    }
}

fn file_note(
    file_path: &str,
    rows: &[OriginSegmentRow],
    overlapping_segments: usize,
    first_utc_us: u64,
    last_utc_us: u64,
) -> OriginSegmentFileNote {
    let file_name = std::path::Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| file_path.to_string());
    OriginSegmentFileNote {
        file_name,
        total_segments: rows.len(),
        overlapping_segments,
        first_utc_us,
        last_utc_us,
    }
}

fn row_in_capture(row: &OriginSegmentRow, start_utc_us: u64, capture_duration_s: f64) -> bool {
    if start_utc_us == 0 {
        return false;
    }
    let hit = |utc: Option<u64>, size: Option<u64>| {
        let Some(utc) = utc else {
            return false;
        };
        if size.is_none() {
            return false;
        }
        let t_s = (utc as i64 - start_utc_us as i64) as f64 / 1_000_000.0;
        t_s >= 0.0 && t_s <= capture_duration_s
    };
    hit(row.utc_us_enc1, row.size_enc1) || hit(row.utc_us_enc2, row.size_enc2)
}

/// Align several CSVs as one series. A file with no rows in the capture
/// stays in [`OriginSegmentsProfile::files`] and does not drop the others.
pub fn align_origin_segment_files(
    files: &[(&str, &str, &[OriginSegmentRow])],
    start_utc_us: u64,
    capture_duration_s: f64,
) -> OriginSegmentsProfile {
    let region = files.first().map(|(_, region, _)| *region).unwrap_or("");
    let label = if files.len() == 1 {
        files[0].0.to_string()
    } else {
        files
            .iter()
            .map(|(path, _, _)| *path)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut rows = Vec::new();
    for (_, _, file_rows) in files {
        rows.extend(file_rows.iter().cloned());
    }
    let mut prof = align_origin_segments(&rows, &label, region, start_utc_us, capture_duration_s);
    prof.files = files
        .iter()
        .map(|(path, _, file_rows)| {
            let (first, last) = segment_utc_span(file_rows);
            let overlapping = file_rows
                .iter()
                .filter(|row| row_in_capture(row, start_utc_us, capture_duration_s))
                .count();
            file_note(path, file_rows, overlapping, first, last)
        })
        .collect();
    prof
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_date_parsing() {
        assert_eq!(parse_date("October 6th 2026"), Some((2026, 10, 6)));
        assert_eq!(parse_date("Oct 6, 2026"), Some((2026, 10, 6)));
        assert_eq!(parse_date("2026-10-06"), Some((2026, 10, 6)));
        assert_eq!(parse_date("6th October 2026"), Some((2026, 10, 6)));
    }

    #[test]
    fn test_time_parsing() {
        assert_eq!(
            parse_time_of_day("12:01:24.735 PM"),
            Some((12, 1, 24, 735_000))
        );
        assert_eq!(
            parse_time_of_day("1:05:00.123 PM"),
            Some((13, 5, 0, 123_000))
        );
        assert_eq!(parse_time_of_day("12:01:24 AM"), Some((0, 1, 24, 0)));
        assert_eq!(parse_time_of_day("null"), None);
    }

    #[test]
    fn test_us_dst_and_timestamp_utc() {
        assert!(is_us_dst(2026, 10, 6, 12));
        assert!(!is_us_dst(2026, 1, 6, 12));
        let ts = timestamp_to_utc_us("October 6th 2026", "12:52:49.822 PM", "us-east-1");
        assert_eq!(ts, Some(1791305569822000));
    }

    #[test]
    fn test_csv_parse_and_alignment() {
        let csv_data = "Filename,Size (enc1) in Bytes,Size (enc2) in Bytes,Date,Origin Time (us-east-1 | enc1),Origin Time (us-east-1 | enc2),Time Difference (us-east-1)
s_1581.cmfv,307013,310000,October 6th 2026,12:52:49.822 PM,12:52:49.800 PM,22 ms
s_1582.cmfv,274680,275000,October 6th 2026,12:52:51.822 PM,12:52:51.800 PM,22 ms";

        let (region, rows) = parse_origin_segments_csv(csv_data);
        assert_eq!(region, "us-east-1");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].filename, "s_1581.cmfv");
        assert_eq!(rows[0].size_enc1, Some(307013));

        let start_utc = 1791305569012688;
        let prof = align_origin_segments(&rows, "test.csv", &region, start_utc, 10.0);
        assert_eq!(prof.overlapping_segments, 2);
        assert_eq!(prof.enc1_rate_mbps.len(), 2);
        assert!((prof.enc1_rate_mbps[0][0] - 0.809312).abs() < 0.001);
        let expected_mbps = (307013.0 * 8.0) / (2.0 * 1_000_000.0);
        assert!((prof.enc1_rate_mbps[0][1] - expected_mbps).abs() < 0.01);

        // baseline-20k starts at 09:13 EDT, before this afternoon segment file.
        let morning = align_origin_segments(&rows, "test.csv", &region, 1791292436118981, 2400.0);
        assert!(morning.is_empty());
        assert_eq!(morning.overlapping_segments, 0);
        assert_eq!(morning.capture_start_utc_us, 1791292436118981);
        assert!(morning.segments_first_utc_us > morning.capture_end_utc_us);
    }

    #[test]
    fn test_real_origin_segments_file() {
        let path = "/home/user/Downloads/origin-segments-2557826-2087026450.csv";
        if let Ok(content) = std::fs::read_to_string(path) {
            let (region, rows) = parse_origin_segments_csv(&content);
            assert_eq!(region, "us-east-1");
            assert_eq!(rows.len(), 11036);
            let start_utc_us = 1791305569012688;
            let duration_s = 2459.503;
            let prof = align_origin_segments(&rows, path, &region, start_utc_us, duration_s);
            assert_eq!(prof.overlapping_segments, 1229);
            assert_eq!(prof.nominal_duration_s, 2.0);
            assert!((prof.enc1_avg_mbps - 5.20).abs() < 0.05);
            assert!((prof.enc2_avg_mbps - 5.17).abs() < 0.05);
        }
    }

    #[test]
    fn partial_overlap_across_utc_midnight_keeps_rows_inside_the_capture() {
        let during = "Filename,Size (enc1) in Bytes,Size (enc2) in Bytes,Date,Origin Time (us-east-1 | enc1),Origin Time (us-east-1 | enc2),Time Difference
s_before.cmfv,1000000,1000000,October 6th 2026,7:00:00.000 PM,7:00:00.000 PM,0 ms
s_evening.cmfv,1000000,1000000,October 6th 2026,7:45:00.000 PM,7:45:00.000 PM,0 ms
s_after_midnight.cmfv,1000000,1000000,October 6th 2026,8:30:00.000 PM,8:30:00.000 PM,0 ms
s_later.cmfv,1000000,1000000,October 6th 2026,10:00:00.000 PM,10:00:00.000 PM,0 ms";
        let next_day = "Filename,Size (enc1) in Bytes,Size (enc2) in Bytes,Date,Origin Time (us-east-1 | enc1),Origin Time (us-east-1 | enc2),Time Difference
s_morning.cmfv,1000000,1000000,October 7th 2026,1:26:00.000 AM,1:26:00.000 AM,0 ms";
        let (region, during_rows) = parse_origin_segments_csv(during);
        let (_, next_rows) = parse_origin_segments_csv(next_day);
        let start = timestamp_to_utc_us("October 6th 2026", "7:30:00.000 PM", "us-east-1").unwrap();
        let after_midnight =
            timestamp_to_utc_us("October 6th 2026", "8:30:00.000 PM", "us-east-1").unwrap();
        assert_eq!((after_midnight / 1_000_000) % 86_400, 30 * 60);

        let prof = align_origin_segment_files(
            &[
                ("oct6.csv", region.as_str(), during_rows.as_slice()),
                ("oct7.csv", region.as_str(), next_rows.as_slice()),
            ],
            start,
            2.0 * 3600.0,
        );
        assert!(!prof.is_empty());
        assert_eq!(prof.overlapping_segments, 2);
        assert_eq!(prof.files.len(), 2);
        assert_eq!(prof.files[0].file_name, "oct6.csv");
        assert_eq!(prof.files[0].overlapping_segments, 2);
        assert_eq!(prof.files[1].overlapping_segments, 0);
        assert_eq!(prof.enc1_rate_mbps.len(), 2);
    }

    #[test]
    fn confetti_segment_files_and_the_35k_window() {
        let dir = "/home/user/logs/stars-confetti-2";
        let oct6 = format!("{dir}/origin-segments-2555267-397374817.csv");
        let oct7 = format!("{dir}/origin-segments-2555267-397374817-2.csv");
        let (Ok(oct6_csv), Ok(oct7_csv)) = (
            std::fs::read_to_string(&oct6),
            std::fs::read_to_string(&oct7),
        ) else {
            return;
        };
        let (region, oct6_rows) = parse_origin_segments_csv(&oct6_csv);
        let (_, oct7_rows) = parse_origin_segments_csv(&oct7_csv);
        let files = [
            (oct6.as_str(), region.as_str(), oct6_rows.as_slice()),
            (oct7.as_str(), region.as_str(), oct7_rows.as_slice()),
        ];
        // baseline-confetti-35k starts 2026-10-07 00:37:50 UTC, about 31 minutes.
        let during_35k = align_origin_segment_files(&files, 1791333470084955, 31.0 * 60.0);
        assert!(during_35k.is_empty());
        assert_eq!(during_35k.files[0].overlapping_segments, 0);
        assert_eq!(during_35k.files[1].overlapping_segments, 0);

        // 25k baseline starts 22:01:55 UTC Oct 6, inside the Oct 6 CSV and
        // before the Oct 7 CSV. The Oct 6 file also starts before the capture.
        let during_25k = align_origin_segment_files(&files, 1791324115470334, 3.0 * 3600.0);
        assert!(!during_25k.is_empty());
        assert!(during_25k.files[0].overlapping_segments > 0);
        assert_eq!(during_25k.files[1].overlapping_segments, 0);
    }
}
