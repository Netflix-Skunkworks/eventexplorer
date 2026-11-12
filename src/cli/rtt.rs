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

pub fn cmd_rtt(sessions: &Sessions, filter: Option<u64>) {
    let mut session_list: Vec<_> = sessions.iter().collect();
    session_list.sort_by_key(|(sid, _)| *sid);

    for (&sid, events) in &session_list {
        if let Some(f) = filter {
            if sid != f {
                continue;
            }
        }

        let rtt_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::Rtt { rtt, .. } if rtt > 0))
            .collect();

        if rtt_events.is_empty() {
            continue;
        }

        let t0 = events.first().map(|e| e.ts).unwrap_or(0);

        println!(
            "=== RTT events for session {sid:#x} ({} RTT events, {} total) ===",
            rtt_events.len(),
            events.len()
        );
        println!(
            "{:>12}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}",
            "time_s", "rtt_us", "srtt_us", "path_us", "lowest_us", "gpmin_us", "gpsrtt_us"
        );
        println!("{}", "-".repeat(88));

        let mut min_rtt = u32::MAX;
        let mut max_rtt = 0u32;
        let mut sum_rtt = 0u64;
        let mut count = 0u64;

        for e in &rtt_events {
            if let EventPayload::Rtt {
                rtt,
                srtt,
                path_rtt,
                lowest_rtt,
                gp_filter_min,
                gp_srtt,
            } = &e.payload
            {
                let t = (e.ts - t0) as f64 / 1_000_000.0;
                println!(
                    "{:>10.3}s  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}",
                    t, rtt, srtt, path_rtt, lowest_rtt, gp_filter_min, gp_srtt
                );
                min_rtt = min_rtt.min(*rtt);
                max_rtt = max_rtt.max(*rtt);
                sum_rtt += *rtt as u64;
                count += 1;
            }
        }
        println!("{}", "-".repeat(88));
        let avg = sum_rtt as f64 / count as f64;
        println!("  raw (us):  min={min_rtt}  avg={avg:.0}  max={max_rtt}");
        println!(
            "  as ms:     min={:.1}  avg={:.1}  max={:.1}",
            min_rtt as f64 / 1000.0,
            avg / 1000.0,
            max_rtt as f64 / 1000.0,
        );
        println!();
    }
}

// ---------------------------------------------------------------------------
// RTT metric analysis: srtt vs gp_srtt vs path_rtt
// ---------------------------------------------------------------------------

pub struct RttTriple {
    srtt: f64,
    gp_srtt: f64,
    path_rtt: f64,
}

pub struct DescriptiveStats {
    #[allow(dead_code)]
    count: usize,
    min: f64,
    #[allow(dead_code)]
    p5: f64,
    p25: f64,
    p50: f64,
    p75: f64,
    p95: f64,
    p99: f64,
    max: f64,
    mean: f64,
    stddev: f64,
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = p / 100.0 * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let frac = rank - lo as f64;
    if lo == hi || hi >= sorted.len() {
        sorted[lo.min(sorted.len() - 1)]
    } else {
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }
}

pub fn compute_stats(values: &mut [f64]) -> DescriptiveStats {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = values.len();
    let mean = values.iter().sum::<f64>() / n as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
    DescriptiveStats {
        count: n,
        min: values[0],
        p5: percentile(values, 5.0),
        p25: percentile(values, 25.0),
        p50: percentile(values, 50.0),
        p75: percentile(values, 75.0),
        p95: percentile(values, 95.0),
        p99: percentile(values, 99.0),
        max: values[n - 1],
        mean,
        stddev: variance.sqrt(),
    }
}

pub fn pearson_r(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len() as f64;
    let sum_x: f64 = xs.iter().sum();
    let sum_y: f64 = ys.iter().sum();
    let sum_xy: f64 = xs.iter().zip(ys).map(|(x, y)| x * y).sum();
    let sum_x2: f64 = xs.iter().map(|x| x * x).sum();
    let sum_y2: f64 = ys.iter().map(|y| y * y).sum();
    let num = n * sum_xy - sum_x * sum_y;
    let den = ((n * sum_x2 - sum_x * sum_x) * (n * sum_y2 - sum_y * sum_y)).sqrt();
    if den.abs() < 1e-15 {
        1.0
    } else {
        num / den
    }
}

pub struct PairwiseStats {
    label: &'static str,
    mean_abs_us: f64,
    median_abs_us: f64,
    p95_abs_us: f64,
    max_abs_us: f64,
    mean_pct: f64,
    median_pct: f64,
    p95_pct: f64,
    pearson: f64,
}

pub fn compute_pairwise(label: &'static str, a: &[f64], b: &[f64]) -> PairwiseStats {
    let mut abs_diffs: Vec<f64> = a.iter().zip(b).map(|(x, y)| (x - y).abs()).collect();
    let mut pct_diffs: Vec<f64> = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            let base = (x + y) / 2.0;
            if base > 0.0 {
                (x - y).abs() / base * 100.0
            } else {
                0.0
            }
        })
        .collect();
    abs_diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    pct_diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = abs_diffs.len() as f64;
    PairwiseStats {
        label,
        mean_abs_us: abs_diffs.iter().sum::<f64>() / n,
        median_abs_us: percentile(&abs_diffs, 50.0),
        p95_abs_us: percentile(&abs_diffs, 95.0),
        max_abs_us: *abs_diffs.last().unwrap_or(&0.0),
        mean_pct: pct_diffs.iter().sum::<f64>() / n,
        median_pct: percentile(&pct_diffs, 50.0),
        p95_pct: percentile(&pct_diffs, 95.0),
        pearson: pearson_r(a, b),
    }
}

pub struct SessionRttAnalysis {
    sid: u64,
    remote: String,
    samples: usize,
    duration_sec: f64,
    srtt_stats: DescriptiveStats,
    gp_srtt_stats: DescriptiveStats,
    path_rtt_stats: DescriptiveStats,
    pair_srtt_gp: PairwiseStats,
    pair_srtt_path: PairwiseStats,
    pair_gp_path: PairwiseStats,
}

pub fn collect_rtt_triples(events: &[Event]) -> (Vec<RttTriple>, String) {
    let mut triples = Vec::new();
    let mut remote = String::new();
    for e in events {
        match &e.payload {
            EventPayload::Rtt {
                srtt,
                gp_srtt,
                path_rtt,
                ..
            } => {
                if *srtt > 0
                    && *srtt < RTT_UNDERFLOW_THRESHOLD
                    && *gp_srtt > 0
                    && *gp_srtt < RTT_UNDERFLOW_THRESHOLD
                    && *path_rtt > 0
                    && *path_rtt < RTT_UNDERFLOW_THRESHOLD
                {
                    triples.push(RttTriple {
                        srtt: *srtt as f64,
                        gp_srtt: *gp_srtt as f64,
                        path_rtt: *path_rtt as f64,
                    });
                }
            }
            EventPayload::ConnSetIpV4 {
                dst_addr, dst_port, ..
            } => {
                remote = format!("{dst_addr}:{dst_port}");
            }
            EventPayload::ConnSetIpV6 {
                dst_addr, dst_port, ..
            } => {
                remote = format!("[{dst_addr}]:{dst_port}");
            }
            _ => {}
        }
    }
    (triples, remote)
}

pub fn analyze_session(sid: u64, events: &[Event]) -> Option<SessionRttAnalysis> {
    let (triples, remote) = collect_rtt_triples(events);
    if triples.len() < 2 {
        return None;
    }

    let t0 = events.first().map(|e| e.ts).unwrap_or(0);
    let t_last = events.last().map(|e| e.ts).unwrap_or(0);
    let duration_sec = (t_last - t0) as f64 / 1_000_000.0;

    let srtts: Vec<f64> = triples.iter().map(|t| t.srtt).collect();
    let gp_srtts: Vec<f64> = triples.iter().map(|t| t.gp_srtt).collect();
    let path_rtts: Vec<f64> = triples.iter().map(|t| t.path_rtt).collect();

    let srtt_stats = compute_stats(&mut srtts.clone());
    let gp_srtt_stats = compute_stats(&mut gp_srtts.clone());
    let path_rtt_stats = compute_stats(&mut path_rtts.clone());

    let pair_srtt_gp = compute_pairwise("srtt vs gp_srtt", &srtts, &gp_srtts);
    let pair_srtt_path = compute_pairwise("srtt vs path_rtt", &srtts, &path_rtts);
    let pair_gp_path = compute_pairwise("gp_srtt vs path_rtt", &gp_srtts, &path_rtts);

    Some(SessionRttAnalysis {
        sid,
        remote,
        samples: triples.len(),
        duration_sec,
        srtt_stats,
        gp_srtt_stats,
        path_rtt_stats,
        pair_srtt_gp,
        pair_srtt_path,
        pair_gp_path,
    })
}

pub fn print_stats_row(label: &str, s: &DescriptiveStats) {
    println!(
        "    {:<20} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>8.0} {:>8.1}",
        label, s.min, s.p25, s.p50, s.p75, s.p95, s.p99, s.max, s.mean, s.stddev,
    );
}

pub fn print_pairwise_row(p: &PairwiseStats) {
    println!(
        "    {:<24} {:>8.0} {:>8.0} {:>8.0} {:>8.0}    {:>6.2}% {:>6.2}% {:>6.2}%   {:>7.5}",
        p.label,
        p.mean_abs_us,
        p.median_abs_us,
        p.p95_abs_us,
        p.max_abs_us,
        p.mean_pct,
        p.median_pct,
        p.p95_pct,
        p.pearson,
    );
}

pub fn cmd_rtt_analysis(sessions: &Sessions) {
    println!("=== RTT Metric Analysis: srtt vs gp_srtt vs path_rtt ===");
    println!("  Goal: determine if these 3 smoothed RTT metrics can be collapsed.");
    println!("  Only samples where all 3 are valid (>0, not underflow) are included.");
    println!();

    let mut analyses: Vec<SessionRttAnalysis> = Vec::new();
    let mut all_srtts: Vec<f64> = Vec::new();
    let mut all_gp_srtts: Vec<f64> = Vec::new();
    let mut all_path_rtts: Vec<f64> = Vec::new();

    let mut session_list: Vec<_> = sessions.iter().collect();
    session_list.sort_by(|a, b| {
        let ac: usize =
            a.1.iter()
                .filter(|e| matches!(e.payload, EventPayload::Rtt { .. }))
                .count();
        let bc: usize =
            b.1.iter()
                .filter(|e| matches!(e.payload, EventPayload::Rtt { .. }))
                .count();
        bc.cmp(&ac)
    });

    for (&sid, events) in &session_list {
        if let Some(analysis) = analyze_session(sid, events) {
            let (triples, _) = collect_rtt_triples(events);
            all_srtts.extend(triples.iter().map(|t| t.srtt));
            all_gp_srtts.extend(triples.iter().map(|t| t.gp_srtt));
            all_path_rtts.extend(triples.iter().map(|t| t.path_rtt));
            analyses.push(analysis);
        }
    }

    if analyses.is_empty() {
        println!("  No sessions with valid RTT triples found.");
        return;
    }

    // -----------------------------------------------------------------------
    // Per-session results (top 20 by sample count)
    // -----------------------------------------------------------------------
    analyses.sort_by_key(|b| std::cmp::Reverse(b.samples));

    let show_count = analyses.len().min(20);
    println!(
        "--- Per-Session Detail (top {show_count} of {} sessions by sample count) ---",
        analyses.len()
    );

    for a in analyses.iter().take(show_count) {
        println!();
        println!(
            "  Session {:#x}  remote={:<22}  samples={}  duration={:.1}s",
            a.sid,
            if a.remote.is_empty() {
                "(unknown)"
            } else {
                &a.remote
            },
            a.samples,
            a.duration_sec,
        );
        println!(
            "    {:<20} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
            "metric (us)", "min", "p25", "p50", "p75", "p95", "p99", "max", "mean", "stddev",
        );
        println!("    {}", "-".repeat(104));
        print_stats_row("Smoothed RTT", &a.srtt_stats);
        print_stats_row("GP Smoothed RTT", &a.gp_srtt_stats);
        print_stats_row("Path RTT", &a.path_rtt_stats);

        println!();
        println!(
            "    {:<24} {:>8} {:>8} {:>8} {:>8}    {:>7} {:>7} {:>7}   {:>7}",
            "pair (abs diff us)", "mean", "median", "p95", "max", "mean%", "med%", "p95%", "corr",
        );
        println!("    {}", "-".repeat(104));
        print_pairwise_row(&a.pair_srtt_gp);
        print_pairwise_row(&a.pair_srtt_path);
        print_pairwise_row(&a.pair_gp_path);
    }

    // -----------------------------------------------------------------------
    // Aggregate analysis
    // -----------------------------------------------------------------------
    println!();
    println!();
    println!(
        "--- Aggregate Analysis ({} sessions, {} total samples) ---",
        analyses.len(),
        all_srtts.len(),
    );

    println!();
    println!(
        "    {:<20} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "metric (us)", "min", "p25", "p50", "p75", "p95", "p99", "max", "mean", "stddev",
    );
    println!("    {}", "-".repeat(104));
    print_stats_row("Smoothed RTT", &compute_stats(&mut all_srtts.clone()));
    print_stats_row("GP Smoothed RTT", &compute_stats(&mut all_gp_srtts.clone()));
    print_stats_row("Path RTT", &compute_stats(&mut all_path_rtts.clone()));

    println!();
    println!(
        "    {:<24} {:>8} {:>8} {:>8} {:>8}    {:>7} {:>7} {:>7}   {:>7}",
        "pair (abs diff us)", "mean", "median", "p95", "max", "mean%", "med%", "p95%", "corr",
    );
    println!("    {}", "-".repeat(104));
    let agg_srtt_gp = compute_pairwise("srtt vs gp_srtt", &all_srtts, &all_gp_srtts);
    let agg_srtt_path = compute_pairwise("srtt vs path_rtt", &all_srtts, &all_path_rtts);
    let agg_gp_path = compute_pairwise("gp_srtt vs path_rtt", &all_gp_srtts, &all_path_rtts);
    print_pairwise_row(&agg_srtt_gp);
    print_pairwise_row(&agg_srtt_path);
    print_pairwise_row(&agg_gp_path);

    // -----------------------------------------------------------------------
    // Distribution of per-session mean % divergence
    // -----------------------------------------------------------------------
    println!();
    println!("--- Per-Session Divergence Distribution (mean % diff per session) ---");
    println!();

    let mut sess_pct_srtt_gp: Vec<f64> = analyses.iter().map(|a| a.pair_srtt_gp.mean_pct).collect();
    let mut sess_pct_srtt_path: Vec<f64> =
        analyses.iter().map(|a| a.pair_srtt_path.mean_pct).collect();
    let mut sess_pct_gp_path: Vec<f64> = analyses.iter().map(|a| a.pair_gp_path.mean_pct).collect();
    sess_pct_srtt_gp.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sess_pct_srtt_path.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sess_pct_gp_path.sort_by(|a, b| a.partial_cmp(b).unwrap());

    println!(
        "    {:<24} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "pair", "p25", "p50", "p75", "p95", "max",
    );
    println!("    {}", "-".repeat(68));
    for (label, v) in [
        ("srtt vs gp_srtt", &sess_pct_srtt_gp),
        ("srtt vs path_rtt", &sess_pct_srtt_path),
        ("gp_srtt vs path_rtt", &sess_pct_gp_path),
    ] {
        println!(
            "    {:<24} {:>7.2}% {:>7.2}% {:>7.2}% {:>7.2}% {:>7.2}%",
            label,
            percentile(v, 25.0),
            percentile(v, 50.0),
            percentile(v, 75.0),
            percentile(v, 95.0),
            v.last().unwrap_or(&0.0),
        );
    }

    // -----------------------------------------------------------------------
    // Distribution of per-session Pearson correlation
    // -----------------------------------------------------------------------
    println!();
    println!("--- Per-Session Correlation Distribution (Pearson r) ---");
    println!();

    let mut sess_r_srtt_gp: Vec<f64> = analyses.iter().map(|a| a.pair_srtt_gp.pearson).collect();
    let mut sess_r_srtt_path: Vec<f64> =
        analyses.iter().map(|a| a.pair_srtt_path.pearson).collect();
    let mut sess_r_gp_path: Vec<f64> = analyses.iter().map(|a| a.pair_gp_path.pearson).collect();
    sess_r_srtt_gp.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sess_r_srtt_path.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sess_r_gp_path.sort_by(|a, b| a.partial_cmp(b).unwrap());

    println!(
        "    {:<24} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "pair", "min", "p5", "p25", "p50", "p75",
    );
    println!("    {}", "-".repeat(68));
    for (label, v) in [
        ("srtt vs gp_srtt", &sess_r_srtt_gp),
        ("srtt vs path_rtt", &sess_r_srtt_path),
        ("gp_srtt vs path_rtt", &sess_r_gp_path),
    ] {
        println!(
            "    {:<24} {:>8.5} {:>8.5} {:>8.5} {:>8.5} {:>8.5}",
            label,
            v.first().unwrap_or(&0.0),
            percentile(v, 5.0),
            percentile(v, 25.0),
            percentile(v, 50.0),
            percentile(v, 75.0),
        );
    }

    // -----------------------------------------------------------------------
    // Verdict
    // -----------------------------------------------------------------------
    println!();
    println!("--- Collapsibility Verdict ---");
    println!();
    let pairs = [
        (
            "srtt vs gp_srtt",
            &agg_srtt_gp,
            &sess_pct_srtt_gp,
            &sess_r_srtt_gp,
        ),
        (
            "srtt vs path_rtt",
            &agg_srtt_path,
            &sess_pct_srtt_path,
            &sess_r_srtt_path,
        ),
        (
            "gp_srtt vs path_rtt",
            &agg_gp_path,
            &sess_pct_gp_path,
            &sess_r_gp_path,
        ),
    ];
    for (label, agg, sess_pcts, sess_rs) in &pairs {
        let p95_pct = percentile(sess_pcts, 95.0);
        let p5_r = percentile(sess_rs, 5.0);
        let verdict = if agg.median_pct < 1.0 && p95_pct < 5.0 && p5_r > 0.99 {
            "COLLAPSIBLE - nearly identical"
        } else if agg.median_pct < 5.0 && p95_pct < 15.0 && p5_r > 0.95 {
            "SIMILAR - minor divergence, collapsible with small accuracy loss"
        } else if agg.median_pct < 15.0 && p5_r > 0.80 {
            "MODERATE - noticeable divergence in some sessions"
        } else {
            "DIVERGENT - these metrics capture different information"
        };
        println!("  {label}:");
        println!(
            "    aggregate: median_diff={:.2}%, p95_diff={:.2}%, corr={:.5}",
            agg.median_pct, agg.p95_pct, agg.pearson,
        );
        println!(
            "    per-session p95 mean_diff={:.2}%, per-session p5 corr={:.5}",
            p95_pct, p5_r,
        );
        println!("    => {verdict}");
        println!();
    }
}

pub fn cmd_underflow(sessions: &Sessions, filter: Option<u64>) {
    use std::collections::HashMap;

    struct UnderflowRecord {
        sid: u64,
        hdr_ts: u64,
        rel_time: f64,
        cpu: u16,
        us_rtt: u32,
        minuend: u32,
        subtrahend: u32,
        location: u8,
    }

    let mut records: Vec<UnderflowRecord> = Vec::new();
    let mut session_t0: HashMap<u64, u64> = HashMap::new();

    for (&sid, events) in sessions.iter() {
        if let Some(f) = filter {
            if sid != f {
                continue;
            }
        }
        let t0 = events.first().map(|e| e.ts).unwrap_or(0);
        session_t0.insert(sid, t0);
        for e in events {
            if let EventPayload::RttUnderflow {
                us_rtt,
                minuend,
                subtrahend,
                location,
            } = &e.payload
            {
                records.push(UnderflowRecord {
                    sid,
                    hdr_ts: e.ts,
                    rel_time: (e.ts - t0) as f64 / 1_000_000.0,
                    cpu: e.cpu,
                    us_rtt: *us_rtt,
                    minuend: *minuend,
                    subtrahend: *subtrahend,
                    location: *location,
                });
            }
        }
    }

    records.sort_by_key(|r| r.hdr_ts);

    println!("=== RTT_UNDERFLOW events ({} total) ===", records.len());
    if records.is_empty() {
        println!("  No RTT_UNDERFLOW events found.");
        return;
    }

    let has_operands = records.iter().any(|r| r.minuend != 0 || r.subtrahend != 0);

    println!(
        "{:>12}  {:>18}  {:>10}  {:>4}  {:>12}  {:>12}  {:>12}  {:>5}  {:>12}",
        "session",
        "hdr_ts_us",
        "time_s",
        "cpu",
        "us_rtt",
        "minuend(a)",
        "subtra(b)",
        "where",
        "overshoot"
    );
    println!("{}", "-".repeat(125));

    let mut by_location: HashMap<u8, Vec<usize>> = HashMap::new();
    let mut by_session: HashMap<u64, usize> = HashMap::new();

    for (i, r) in records.iter().enumerate() {
        let overshoot = 4294967296u64.wrapping_sub(r.us_rtt as u64) as u32;
        println!(
            "{:#12x}  {:>18}  {:>8.3}s  {:>4}  {:>12}  {:>12}  {:>12}  {:>5}  {:>12}",
            r.sid,
            r.hdr_ts,
            r.rel_time,
            r.cpu,
            r.us_rtt,
            r.minuend,
            r.subtrahend,
            r.location,
            overshoot
        );
        by_location.entry(r.location).or_default().push(i);
        *by_session.entry(r.sid).or_insert(0) += 1;
    }

    println!();
    println!("=== Summary by 'where' location ===");
    let mut locs: Vec<_> = by_location.iter().collect();
    locs.sort_by_key(|(loc, _)| *loc);
    for (loc, indices) in &locs {
        let mut min_overshoot = u32::MAX;
        let mut max_overshoot = 0u32;
        let mut sum_overshoot = 0u64;
        for &i in indices.iter() {
            let o = 4294967296u64.wrapping_sub(records[i].us_rtt as u64) as u32;
            min_overshoot = min_overshoot.min(o);
            max_overshoot = max_overshoot.max(o);
            sum_overshoot += o as u64;
        }
        let avg_overshoot = sum_overshoot as f64 / indices.len() as f64;
        println!("  where={}: {} events", loc, indices.len(),);
        println!(
            "    overshoot (b-a): min={} us ({:.1} ms), avg={:.0} us ({:.1} ms), max={} us ({:.1} ms)",
            min_overshoot, min_overshoot as f64 / 1000.0,
            avg_overshoot, avg_overshoot / 1000.0,
            max_overshoot, max_overshoot as f64 / 1000.0,
        );
    }

    if has_operands {
        println!();
        println!("=== Operand Analysis (minuend=a, subtrahend=b, us_rtt=a-b underflowed) ===");
        for (loc, indices) in &locs {
            println!("  where={}:", loc);
            let mut min_a = u32::MAX;
            let mut max_a = 0u32;
            let mut min_b = u32::MAX;
            let mut max_b = 0u32;
            let mut diffs: Vec<i64> = Vec::new();
            for &i in indices.iter() {
                let r = &records[i];
                min_a = min_a.min(r.minuend);
                max_a = max_a.max(r.minuend);
                min_b = min_b.min(r.subtrahend);
                max_b = max_b.max(r.subtrahend);
                diffs.push(r.subtrahend as i64 - r.minuend as i64);
            }
            diffs.sort();
            let p50 = diffs[diffs.len() / 2];
            println!("    minuend  (a) range: [{min_a} .. {max_a}]");
            println!("    subtrahend(b) range: [{min_b} .. {max_b}]");
            println!(
                "    b - a (how far b is ahead): min={}, median={}, max={}",
                diffs[0],
                p50,
                diffs[diffs.len() - 1]
            );
            println!(
                "    b - a as ms: min={:.3}, median={:.3}, max={:.3}",
                diffs[0] as f64 / 1000.0,
                p50 as f64 / 1000.0,
                diffs[diffs.len() - 1] as f64 / 1000.0
            );
        }
    }

    println!();
    println!("=== Summary by session (top 20) ===");
    let mut sess_list: Vec<_> = by_session.iter().collect();
    // Count desc, then session id for a stable order on ties.
    sess_list.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (i, (&sid, &count)) in sess_list.iter().enumerate() {
        if i >= 20 {
            break;
        }
        println!("  session {sid:#x}: {count} underflow events");
    }

    println!();
    println!("=== Totals ===");
    println!("  Total underflow events: {}", records.len());
    println!("  Affected sessions:      {}", by_session.len());
    println!(
        "  Location codes:         {:?}",
        locs.iter().map(|(l, _)| **l).collect::<Vec<_>>()
    );

    if has_operands {
        println!();
        println!("=== Per-session detail (first 5 events each, top 10 sessions) ===");
        for (i, (&sid, _)) in sess_list.iter().enumerate() {
            if i >= 10 {
                break;
            }
            let sess_records: Vec<_> = records.iter().filter(|r| r.sid == sid).collect();
            let t0 = session_t0.get(&sid).copied().unwrap_or(0);
            println!(
                "  --- session {sid:#x} ({} underflows, t0={t0}) ---",
                sess_records.len()
            );
            println!(
                "    {:>18}  {:>10}  {:>4}  {:>12}  {:>12}  {:>12}  {:>5}  {:>12}",
                "hdr_ts_us",
                "time_s",
                "cpu",
                "us_rtt",
                "minuend(a)",
                "subtra(b)",
                "where",
                "b-a (us)"
            );
            for (j, r) in sess_records.iter().enumerate() {
                if j >= 5 {
                    println!("    ... ({} more)", sess_records.len() - 5);
                    break;
                }
                let diff = r.subtrahend as i64 - r.minuend as i64;
                println!(
                    "    {:>18}  {:>8.3}s  {:>4}  {:>12}  {:>12}  {:>12}  {:>5}  {:>12}",
                    r.hdr_ts,
                    r.rel_time,
                    r.cpu,
                    r.us_rtt,
                    r.minuend,
                    r.subtrahend,
                    r.location,
                    diff
                );
            }
        }
    }
}
