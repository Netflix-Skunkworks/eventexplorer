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
use std::collections::BTreeMap;

/// `natural_bps` exactly equal to this marks the synthetic no-rate-estimate
/// placeholder (`1 << 24` = 134.217728 Mbps) rather than a real goodput-based
/// estimate -- see `synthetic_norate_hits` on `StarsAggSample` and the
/// `hpts_stars_pace_rate()` call sites that fall back to it when no RTT/
/// goodput estimate is available yet for the connection.
const SYNTHETIC_NORATE_BPS: u64 = 1 << 24;

const NBUCKETS: usize = 20;

/// `natural_bps`/`scaled_bps` are BYTES per second (see the doc comment
/// above `hpts_stars_pace_rate()` in tcp_hpts.c); goodput tiers elsewhere in
/// this codebase (`goodput_bucket_label`, `gput-tier-mix`, etc.) are
/// conventionally bits-per-second Mbps, so bucketing multiplies by 8 first
/// -- matching elog.py's own `1<<24 B/s == 134.217728 Mbps` convention.
fn bytes_per_sec_to_mbps(bps: u64) -> f64 {
    bps as f64 * 8.0 / 1e6
}

fn bucket_for_bps(bps: u64) -> usize {
    let mbps = bytes_per_sec_to_mbps(bps);
    if mbps < 1.0 {
        return 0;
    }
    let b = mbps.log2().floor() as i64 + 1;
    b.clamp(0, NBUCKETS as i64 - 1) as usize
}

struct TierStats {
    count: u64,
    sessions: std::collections::HashSet<u64>,
    pressure_sum: u64,
    natural_bps_sum: u64,
    scaled_tier_counts: [u64; NBUCKETS],
}

impl TierStats {
    fn new() -> Self {
        TierStats {
            count: 0,
            sessions: std::collections::HashSet::new(),
            pressure_sum: 0,
            natural_bps_sum: 0,
            scaled_tier_counts: [0; NBUCKETS],
        }
    }

    fn add(&mut self, s: &StarsPaceSample) {
        self.count += 1;
        self.sessions.insert(s.session_id);
        self.pressure_sum += s.pressure as u64;
        self.natural_bps_sum += s.natural_bps;
        self.scaled_tier_counts[bucket_for_bps(s.scaled_bps)] += 1;
    }

    fn avg_pressure(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.pressure_sum as f64 / self.count as f64
        }
    }

    fn avg_natural_mbps(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            bytes_per_sec_to_mbps(self.natural_bps_sum / self.count.max(1))
        }
    }

    fn print_scaled_tier_mix(&self, indent: &str) {
        let mut tiers: Vec<(usize, u64)> = self
            .scaled_tier_counts
            .iter()
            .enumerate()
            .filter(|(_, &c)| c > 0)
            .map(|(b, &c)| (b, c))
            .collect();
        tiers.sort_unstable_by_key(|&(_, c)| std::cmp::Reverse(c));
        for (b, c) in tiers {
            let share = 100.0 * c as f64 / self.count as f64;
            if share >= 0.5 {
                println!(
                    "{indent}scaled/actual rate lands in {:>8}: {share:5.1}% of samples",
                    goodput_bucket_label(b, NBUCKETS),
                );
            }
        }
    }
}

/// Aggregate `STARS_PACE` events (natural/pre-squeeze rate, scaled/post-
/// squeeze rate, and the pressure in effect for that send) file-wide into a
/// natural-rate-tier -> resulting-scaled-rate-tier breakdown. Unlike
/// elog.py's `pressure-squeeze-predict` (which replays the kernel formula
/// against an *assumed* natural_bps against `starsagg`'s fleet-wide
/// pressure), this uses the real per-connection natural_bps and scaled_bps
/// the kernel actually computed -- ground truth, not prediction. Splits out
/// the synthetic no-rate-estimate placeholder (`natural_bps == 1<<24`) from
/// real goodput-based estimates since they answer different questions: the
/// placeholder population is "what happens before we have any estimate",
/// the real-estimate population is "does pressure actually crush an
/// already-known-fast flow toward the fair-share tier".
pub fn cmd_stars_pace(samples: &[StarsPaceSample], capture_start: u64, args: &[String]) {
    let min_samples: u64 = args
        .iter()
        .position(|a| a == "--min-samples")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    // Seconds relative to capture_start, same convention as starsagg's t(s)
    // column -- lets a caller isolate e.g. just the "hold" plateau of one
    // overload episode instead of averaging it away against the rest of
    // the file.
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

    if samples.is_empty() {
        println!(
            "No STARS_PACE events in this capture (capture predates the STARS_PACE event, or no \
             session had its eventlog enabled -- e.g. via auto_ratio -- during the run)."
        );
        return;
    }

    let in_window = |s: &StarsPaceSample| -> bool {
        let t = s.ts.saturating_sub(capture_start) as f64 / 1e6;
        since.is_none_or(|lo| t >= lo) && until.is_none_or(|hi| t <= hi)
    };
    let samples: Vec<&StarsPaceSample> = samples.iter().filter(|s| in_window(s)).collect();
    if since.is_some() || until.is_some() {
        println!(
            "(time window applied: {:.1}s..{:.1}s -- {} of the capture's STARS_PACE events fall \
             inside it)",
            since.unwrap_or(0.0),
            until.unwrap_or(f64::INFINITY),
            samples.len(),
        );
    }
    if samples.is_empty() {
        println!("No STARS_PACE events fall inside the requested --since/--until window.");
        return;
    }

    if args.iter().any(|a| a == "--raw") {
        // One line per sample: ts_s, natural_bps, scaled_bps, pressure,
        // session_id, len -- for offline what-if analysis (e.g. replaying
        // alternate pressure->rate curve shapes against real captured
        // (natural_bps, pressure) pairs) that doesn't warrant a bespoke
        // CLI flag per curve tried.
        println!("ts_s\tnatural_bps\tscaled_bps\tpressure\tsession_id\tlen");
        for s in samples {
            let t = s.ts.saturating_sub(capture_start) as f64 / 1e6;
            println!(
                "{t:.6}\t{}\t{}\t{}\t{}\t{}",
                s.natural_bps, s.scaled_bps, s.pressure, s.session_id, s.len
            );
        }
        return;
    }

    let total_samples = samples.len();
    let mut placeholder = TierStats::new();
    let mut real_by_tier: BTreeMap<usize, TierStats> = BTreeMap::new();
    let mut all_sessions = std::collections::HashSet::new();

    for s in samples {
        all_sessions.insert(s.session_id);
        if s.natural_bps == SYNTHETIC_NORATE_BPS {
            placeholder.add(s);
        } else {
            real_by_tier
                .entry(bucket_for_bps(s.natural_bps))
                .or_insert_with(TierStats::new)
                .add(s);
        }
    }
    let real_count: u64 = real_by_tier.values().map(|t| t.count).sum();

    println!(
        "=== STARS_PACE ({total_samples} events, {} sessions) ===",
        all_sessions.len()
    );
    println!(
        "{} synthetic no-rate-estimate placeholder (natural_bps == {} == {:.1} Mbps), {} real \
         goodput-based estimates ({:.1}% / {:.1}%)\n",
        placeholder.count,
        SYNTHETIC_NORATE_BPS,
        bytes_per_sec_to_mbps(SYNTHETIC_NORATE_BPS),
        real_count,
        100.0 * placeholder.count as f64 / total_samples as f64,
        100.0 * real_count as f64 / total_samples as f64,
    );

    if placeholder.count > 0 {
        println!(
            "--- synthetic placeholder: {} samples, {} sessions, avg pressure {:.0}/1024 ({:.1}%) \
             ---",
            placeholder.count,
            placeholder.sessions.len(),
            placeholder.avg_pressure(),
            100.0 * placeholder.avg_pressure() / 1024.0,
        );
        placeholder.print_scaled_tier_mix("  ");
        println!();
    }

    if real_by_tier.is_empty() {
        println!("No real (non-placeholder) goodput-based natural_bps estimates in this capture.");
        return;
    }

    println!("--- real goodput-based estimates, by natural-rate tier ---");
    println!(
        "(tiers with fewer than {min_samples} samples are hidden; pass --min-samples to change)\n"
    );
    for (&b, t) in real_by_tier.iter().rev() {
        if t.count < min_samples {
            continue;
        }
        println!(
            "natural rate {:>8} ({} samples, {} sessions, avg {:.1} Mbps, avg pressure {:.0}/1024 \
             [{:.1}%]):",
            goodput_bucket_label(b, NBUCKETS),
            t.count,
            t.sessions.len(),
            t.avg_natural_mbps(),
            t.avg_pressure(),
            100.0 * t.avg_pressure() / 1024.0,
        );
        t.print_scaled_tier_mix("  ");
    }

    // Headline: for the highest-natural-rate tier with enough samples, how
    // concentrated did the resulting scaled rate end up -- the direct
    // bitrate-concentration signal.
    if let Some((&top_b, top_t)) = real_by_tier
        .iter()
        .rev()
        .find(|(_, t)| t.count >= min_samples)
    {
        let top_scaled_tier = top_t
            .scaled_tier_counts
            .iter()
            .enumerate()
            .max_by_key(|(_, &c)| c)
            .map(|(b, _)| b)
            .unwrap_or(0);
        let top_scaled_share =
            100.0 * top_t.scaled_tier_counts[top_scaled_tier] as f64 / top_t.count as f64;
        println!(
            "\nHighest-volume real natural-rate tier ({}) lands {:.1}% of its scaled/actual rate \
             in {} -- {}.",
            goodput_bucket_label(top_b, NBUCKETS),
            top_scaled_share,
            goodput_bucket_label(top_scaled_tier, NBUCKETS),
            if top_scaled_tier < top_b {
                "pressure IS squeezing this tier down"
            } else {
                "pressure is NOT meaningfully squeezing this tier"
            },
        );
    }
}
