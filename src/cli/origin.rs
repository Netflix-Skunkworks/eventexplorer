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

use crate::analysis;
use crate::elog_parser::FileInfo;

pub fn cmd_origin(file_info: &FileInfo, args: &[String]) {
    let raw = args.iter().any(|a| a == "--raw");
    let csv_paths: Vec<&String> = args.iter().filter(|a| a.as_str() != "--raw").collect();
    if csv_paths.is_empty() {
        eprintln!(
            "Usage: elog_cli <file.elog[.gz]> origin <origin-segments.csv> [more.csv ...] [--raw]"
        );
        std::process::exit(1);
    }

    let mut loaded = Vec::new();
    for csv_path in &csv_paths {
        let content = match std::fs::read_to_string(csv_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to read {csv_path}: {e}");
                std::process::exit(1);
            }
        };
        let (region, rows) = analysis::parse_origin_segments_csv(&content);
        loaded.push(((*csv_path).clone(), region, rows));
    }
    let files: Vec<(&str, &str, &[analysis::OriginSegmentRow])> = loaded
        .iter()
        .map(|(path, region, rows)| (path.as_str(), region.as_str(), rows.as_slice()))
        .collect();
    let dur_s = file_info
        .capture_end
        .saturating_sub(file_info.capture_start) as f64
        / 1_000_000.0;
    let prof = analysis::align_origin_segment_files(&files, file_info.start_utc_us, dur_s);

    println!("=== Origin Segments Alignment ===");
    println!("  CSV file:         {}", prof.file_path);
    println!("  Detected region:  {}", prof.region);
    println!("  Total segments:   {}", prof.total_segments);
    println!("  Capture start:    {} us", file_info.start_utc_us);
    println!("  Capture duration: {:.3} s", dur_s);
    println!("  Overlap segments: {}", prof.overlapping_segments);
    for file in &prof.files {
        println!(
            "  {:<18} {}/{} in window",
            file.file_name, file.overlapping_segments, file.total_segments
        );
        if file.overlapping_segments == 0 && file.first_utc_us != 0 {
            println!(
                "                     segments {} – {} us",
                file.first_utc_us, file.last_utc_us
            );
        }
    }
    if prof.overlapping_segments == 0 && prof.segments_first_utc_us != 0 {
        println!(
            "  Segment span:     {} – {} us",
            prof.segments_first_utc_us, prof.segments_last_utc_us
        );
        println!(
            "  Capture span:     {} – {} us",
            prof.capture_start_utc_us, prof.capture_end_utc_us
        );
    }
    println!("  Nominal duration: {:.3} s/seg", prof.nominal_duration_s);
    println!(
        "  Enc1 bitrate:     avg {:.2} Mbps (min {:.2} Mbps, max {:.2} Mbps)",
        prof.enc1_avg_mbps, prof.enc1_min_mbps, prof.enc1_max_mbps
    );
    println!(
        "  Enc2 bitrate:     avg {:.2} Mbps (min {:.2} Mbps, max {:.2} Mbps)",
        prof.enc2_avg_mbps, prof.enc2_min_mbps, prof.enc2_max_mbps
    );

    if raw {
        println!();
        println!(
            "{:>10} {:>10} {:>10} {:>12}",
            "t_s", "enc1_mbps", "enc2_mbps", "size1_bytes"
        );
        for (i, p) in prof.enc1_rate_mbps.iter().enumerate() {
            let enc2_m = prof.enc2_rate_mbps.get(i).map(|p2| p2[1]).unwrap_or(0.0);
            let sz1 = prof
                .enc1_size_mb
                .get(i)
                .map(|s| (s[1] * 1024.0 * 1024.0).round() as u64)
                .unwrap_or(0);
            println!("{:10.3} {:10.2} {:10.2} {:12}", p[0], p[1], enc2_m, sz1);
        }
    }
}
