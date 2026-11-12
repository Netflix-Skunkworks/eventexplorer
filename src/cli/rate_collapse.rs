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
use std::collections::HashMap;

const SYNTHETIC_NORATE_BPS: u64 = 1 << 24;

struct Episode {
    session_id: u64,
    start_ts: u64,
    end_ts: u64,
    n_samples: u32,
    min_bps: u64,
    recovery_bps: Option<u64>,
}

/// Finds runs of consecutive STARS_PACE samples, per session, where
/// `natural_bps` (the PRE-squeeze rate -- what `rack_get_pacing_delay()`
/// computed before STARS's own pressure scaling ever touches it) sits below
/// a threshold for a sustained stretch of wall-clock time. This is the
/// signature of the "natural_bps collapses to a corrupted near-zero value
/// after an RTO, and stays stuck until a fresh rate measurement completes"
/// mechanism -- distinct from a real bandwidth/CPU limit, and distinct from
/// the STARS squeeze itself (which never sees a rate this low to squeeze).
pub fn cmd_rate_collapse(samples: &[StarsPaceSample], capture_start: u64, args: &[String]) {
    let threshold_bps: u64 = args
        .iter()
        .position(|a| a == "--threshold-bps")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(500_000);
    let min_duration_s: f64 = args
        .iter()
        .position(|a| a == "--min-duration-s")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);
    let since: Option<f64> = args
        .iter()
        .position(|a| a == "--since")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok());
    let until: Option<f64> = args
        .iter()
        .position(|a| a == "--until")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok());
    let raw = args.iter().any(|a| a == "--raw");

    let in_window = |ts: u64| -> bool {
        let t = ts.saturating_sub(capture_start) as f64 / 1e6;
        since.is_none_or(|lo| t >= lo) && until.is_none_or(|hi| t <= hi)
    };

    let mut by_session: HashMap<u64, Vec<&StarsPaceSample>> = HashMap::new();
    for s in samples {
        if in_window(s.ts) {
            by_session.entry(s.session_id).or_default().push(s);
        }
    }

    let mut episodes: Vec<Episode> = Vec::new();
    let mut session_ids: Vec<u64> = by_session.keys().copied().collect();
    session_ids.sort_unstable();

    for sid in session_ids {
        let seq = &by_session[&sid];
        let mut i = 0usize;
        while i < seq.len() {
            if seq[i].natural_bps < threshold_bps && seq[i].natural_bps != SYNTHETIC_NORATE_BPS {
                let start = i;
                let mut j = i;
                let mut min_bps = seq[i].natural_bps;
                while j + 1 < seq.len()
                    && seq[j + 1].natural_bps < threshold_bps
                    && seq[j + 1].natural_bps != SYNTHETIC_NORATE_BPS
                {
                    j += 1;
                    min_bps = min_bps.min(seq[j].natural_bps);
                }
                let start_ts = seq[start].ts;
                let end_ts = seq[j].ts;
                let duration_s = end_ts.saturating_sub(start_ts) as f64 / 1e6;
                if duration_s >= min_duration_s {
                    episodes.push(Episode {
                        session_id: sid,
                        start_ts,
                        end_ts,
                        n_samples: (j - start + 1) as u32,
                        min_bps,
                        recovery_bps: seq.get(j + 1).map(|s| s.natural_bps),
                    });
                }
                i = j + 1;
            } else {
                i += 1;
            }
        }
    }

    if raw {
        println!("session_id\tstart_s\tend_s\tduration_s\tn_samples\tmin_bps\trecovery_bps");
        for e in &episodes {
            let start_s = e.start_ts.saturating_sub(capture_start) as f64 / 1e6;
            let end_s = e.end_ts.saturating_sub(capture_start) as f64 / 1e6;
            println!(
                "{:#x}\t{:.6}\t{:.6}\t{:.3}\t{}\t{}\t{}",
                e.session_id,
                start_s,
                end_s,
                end_s - start_s,
                e.n_samples,
                e.min_bps,
                e.recovery_bps
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "n/a (session ends mid-episode)".to_string()),
            );
        }
        return;
    }

    let total_sessions: std::collections::HashSet<u64> =
        samples.iter().map(|s| s.session_id).collect();
    let touched_sessions: std::collections::HashSet<u64> =
        episodes.iter().map(|e| e.session_id).collect();
    let total_episode_s: f64 = episodes
        .iter()
        .map(|e| e.end_ts.saturating_sub(e.start_ts) as f64 / 1e6)
        .sum();

    println!(
        "=== Rate-collapse episodes: natural_bps < {threshold_bps} B/s sustained >= {min_duration_s:.1}s ==="
    );
    println!(
        "{} episodes, touching {} of {} sessions ({:.1}%), {:.1}s total episode time",
        episodes.len(),
        touched_sessions.len(),
        total_sessions.len(),
        100.0 * touched_sessions.len() as f64 / total_sessions.len().max(1) as f64,
        total_episode_s,
    );
    if episodes.is_empty() {
        return;
    }
    let mut durations: Vec<f64> = episodes
        .iter()
        .map(|e| e.end_ts.saturating_sub(e.start_ts) as f64 / 1e6)
        .collect();
    durations.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| -> f64 {
        let idx = ((durations.len() - 1) as f64 * p).round() as usize;
        durations[idx]
    };
    println!(
        "episode duration: mean={:.2}s p50={:.2}s p90={:.2}s p99={:.2}s max={:.2}s",
        durations.iter().sum::<f64>() / durations.len() as f64,
        pct(0.50),
        pct(0.90),
        pct(0.99),
        durations.last().unwrap(),
    );

    println!("\ntop 15 longest episodes:");
    let mut by_dur: Vec<&Episode> = episodes.iter().collect();
    by_dur.sort_by(|a, b| {
        let da = a.end_ts.saturating_sub(a.start_ts);
        let db = b.end_ts.saturating_sub(b.start_ts);
        db.cmp(&da)
    });
    for e in by_dur.iter().take(15) {
        let start_s = e.start_ts.saturating_sub(capture_start) as f64 / 1e6;
        let dur = e.end_ts.saturating_sub(e.start_ts) as f64 / 1e6;
        println!(
            "  session={:#x}  start={:.3}s  dur={:.2}s  n={}  min={}B/s  recovery={}",
            e.session_id,
            start_s,
            dur,
            e.n_samples,
            e.min_bps,
            e.recovery_bps
                .map(|b| format!("{b}B/s"))
                .unwrap_or_else(|| "n/a".to_string()),
        );
    }
}
