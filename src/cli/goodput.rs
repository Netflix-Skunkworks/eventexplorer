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

pub const GP_IDLE_THRESH_US: u64 = 100_000;
pub const GP_SRTT_MULT: u64 = 4;
pub const GP_BDP_MULT: u64 = 4;
pub const GP_MIN_BYTES: u32 = 16384;
pub const GP_MAX_UTIM: u64 = 10_000_000;
pub const USEC_IN_SEC_U64: u64 = 1_000_000;
pub const GP_FAST_THRESH: u8 = 4;

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) as f64 * p / 100.0).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Cross-tabulate, per ACK, the wall-clock gap since the previous ACK
/// (what trips the 100ms idle rule) against the NIC receive-timestamp
/// gap (what says the connection was actually quiet), bucketed by SRTT.
/// Answers: how often does the wall cutoff fire while the NIC saw the
/// ACKs arrive close together, and does that grow with RTT.
pub fn cmd_ackgaps(sessions: &Sessions) {
    const WALL_BUCKETS: [u64; 6] = [5_000, 20_000, 50_000, 100_000, 250_000, 1_000_000];
    let wall_labels = [
        "<5ms",
        "5-20ms",
        "20-50ms",
        "50-100ms",
        "100-250ms",
        "250ms-1s",
        ">1s",
    ];
    // SRTT buckets in us: <10ms, 10-50, 50-100, 100-200, >200
    let srtt_edges: [u64; 4] = [10_000, 50_000, 100_000, 200_000];
    let srtt_labels = ["<10ms", "10-50ms", "50-100ms", "100-200ms", ">200ms"];

    // counts[srtt_bucket][wall_bucket]
    let mut counts = vec![vec![0u64; wall_labels.len()]; srtt_labels.len()];
    // Of the wall>100ms ACKs, how many had a NIC gap <= 100ms (still active).
    let mut wall_over_nic_active = vec![0u64; srtt_labels.len()];
    let mut wall_over_total = vec![0u64; srtt_labels.len()];
    let mut all_wall_gaps: Vec<u64> = Vec::new();
    let mut nic_gaps_when_wall_over: Vec<u64> = Vec::new();
    let mut srtt_samples: Vec<u64> = Vec::new();
    let mut acks_total = 0u64;
    let mut acks_no_nic_ts = 0u64;

    for events in sessions.values() {
        let mut last_wall: Option<u64> = None;
        let mut last_nic: Option<u64> = None;
        let mut srtt: u64 = 0;
        for e in events.iter() {
            match &e.payload {
                EventPayload::Rtt { srtt: s, .. } => {
                    if *s > 0 && *s < 0x7FFF_FFFF {
                        srtt = *s as u64;
                        srtt_samples.push(srtt);
                    }
                }
                EventPayload::Ack {
                    acked, rcv_time_us, ..
                } => {
                    if *acked == 0 {
                        continue;
                    }
                    acks_total += 1;
                    let wall = e.ts;
                    let nic = *rcv_time_us;
                    if let (Some(lw), Some(ln)) = (last_wall, last_nic) {
                        let wall_gap = wall.saturating_sub(lw);
                        let sb = srtt_edges
                            .iter()
                            .position(|&e| srtt < e)
                            .unwrap_or(srtt_labels.len() - 1);
                        let wb = WALL_BUCKETS
                            .iter()
                            .position(|&b| wall_gap < b)
                            .unwrap_or(wall_labels.len() - 1);
                        counts[sb][wb] += 1;
                        all_wall_gaps.push(wall_gap);
                        if wall_gap > GP_IDLE_THRESH_US {
                            wall_over_total[sb] += 1;
                            if nic > 0 && ln > 0 && nic >= ln {
                                let nic_gap = nic - ln;
                                nic_gaps_when_wall_over.push(nic_gap);
                                if nic_gap <= GP_IDLE_THRESH_US {
                                    wall_over_nic_active[sb] += 1;
                                }
                            } else {
                                acks_no_nic_ts += 1;
                            }
                        }
                    }
                    last_wall = Some(wall);
                    if nic > 0 {
                        last_nic = Some(nic);
                    }
                }
                _ => {}
            }
        }
    }

    println!("=== ACK gap analysis (wall-clock vs NIC rcv_time) ===");
    println!("ACKs with progress: {acks_total}");
    println!();
    println!(
        "SRTT distribution (us): p50={} p90={} p99={}",
        percentile(
            &{
                let mut v = srtt_samples.clone();
                v.sort_unstable();
                v
            },
            50.0
        ),
        percentile(
            &{
                let mut v = srtt_samples.clone();
                v.sort_unstable();
                v
            },
            90.0
        ),
        percentile(
            &{
                let mut v = srtt_samples;
                v.sort_unstable();
                v
            },
            99.0
        )
    );
    println!();
    println!(
        "Wall-clock inter-ACK gap distribution (us): p50={} p90={} p99={} max={}",
        percentile(
            &{
                let mut v = all_wall_gaps.clone();
                v.sort_unstable();
                v
            },
            50.0
        ),
        percentile(
            &{
                let mut v = all_wall_gaps.clone();
                v.sort_unstable();
                v
            },
            90.0
        ),
        percentile(
            &{
                let mut v = all_wall_gaps.clone();
                v.sort_unstable();
                v
            },
            99.0
        ),
        all_wall_gaps.iter().max().copied().unwrap_or(0)
    );
    println!();

    // Header
    print!("{:>10}", "SRTT\\wall");
    for l in &wall_labels {
        print!("{:>10}", l);
    }
    println!("{:>12}{:>12}", ">100ms", "of which");
    print!("{:>10}", "");
    for _ in &wall_labels {
        print!("{:>10}", "");
    }
    println!("{:>12}{:>12}", "total", "NIC-active");

    for (sb, sl) in srtt_labels.iter().enumerate() {
        print!("{:>10}", sl);
        let row_total: u64 = counts[sb].iter().sum();
        for &c in &counts[sb] {
            if row_total > 0 {
                print!(
                    "{:>10}",
                    format!("{:.1}%", c as f64 * 100.0 / row_total as f64)
                );
            } else {
                print!("{:>10}", "-");
            }
        }
        let over = wall_over_total[sb];
        let active = wall_over_nic_active[sb];
        let pct = if over > 0 {
            active as f64 * 100.0 / over as f64
        } else {
            0.0
        };
        println!("{:>12}{:>12}", over, format!("{active} ({pct:.0}%)"));
    }

    println!();
    let total_over: u64 = wall_over_total.iter().sum();
    let total_active: u64 = wall_over_nic_active.iter().sum();
    println!("Wall gap >100ms (idle rule fires): {total_over} ACKs");
    println!(
        "  of those, NIC gap <=100ms (connection still active): {total_active} ({:.1}%)",
        if total_over > 0 {
            total_active as f64 * 100.0 / total_over as f64
        } else {
            0.0
        }
    );
    println!("  ACKs past 100ms wall with unusable NIC ts: {acks_no_nic_ts}");
    if !nic_gaps_when_wall_over.is_empty() {
        let mut v = nic_gaps_when_wall_over.clone();
        v.sort_unstable();
        println!(
            "  NIC gap when wall>100ms (us): p50={} p90={} p99={}",
            percentile(&v, 50.0),
            percentile(&v, 90.0),
            percentile(&v, 99.0)
        );
    }
}

pub fn cmd_gpsim(sessions: &Sessions, sid: u64, start_s: f64, end_s: f64) {
    let events = match sessions.get(&sid) {
        Some(e) => e,
        None => {
            eprintln!("Session {sid:#x} not found");
            return;
        }
    };
    let t0 = events.first().map(|e| e.ts).unwrap_or(0);

    // Window state
    let mut on: bool = false;
    let mut start_ts: u64 = 0;
    let mut start_seq: u32 = 0;
    let mut last_ack_us: u64 = 0;
    let mut last_ack_seq: u32 = 0;
    let mut active_us: u64 = 0;
    let mut gp_bw: u64 = 0;
    let mut gp_filled: bool = false;
    let mut num_measurements: u8 = 0;
    let mut srtt: u64 = 0;
    let mut sample_count: u64 = 0;
    let mut snd_max: u32 = 0;

    let in_range = |ts: u64| -> bool {
        let t = (ts - t0) as f64 / 1_000_000.0;
        t >= start_s && t <= end_s
    };

    let show = start_s > 0.0 || end_s < f64::MAX;

    println!("=== Goodput Window Simulation for session {sid:#x} ===");
    if show {
        println!("  Showing events in t=[{start_s:.3}..{end_s:.3}]s");
    }
    println!();

    for e in events.iter() {
        let us_cts = e.ts;
        let t = (us_cts - t0) as f64 / 1_000_000.0;

        match &e.payload {
            EventPayload::Rtt { srtt: s, .. } => {
                if *s > 0 && *s < 0x7FFF_FFFF {
                    srtt = *s as u64;
                }
            }
            EventPayload::Out { seq, len, .. } => {
                let end_seq = seq.wrapping_add(*len);
                if end_seq.wrapping_sub(snd_max) < 0x8000_0000 {
                    snd_max = end_seq;
                }
                if *len > 0 && !on {
                    // Output path: start goodput window
                    start_ts = us_cts;
                    start_seq = snd_max;
                    last_ack_us = 0;
                    last_ack_seq = snd_max;
                    active_us = 0;
                    on = true;
                    if show && in_range(us_cts) {
                        println!(
                            "{:>10.4}s  WINDOW-OPEN  start_seq={} (from output path)",
                            t, snd_max
                        );
                    }
                }
            }
            EventPayload::Ack {
                snd_una: th_ack, ..
            } => {
                if !on {
                    continue;
                }

                // --- accumulate_ack ---
                if last_ack_us == 0 {
                    start_ts = us_cts;
                    start_seq = *th_ack;
                    if show && in_range(us_cts) {
                        println!(
                            "{:>10.4}s  RE-ANCHOR    start_seq={} start_ts={:.4}s",
                            t,
                            *th_ack,
                            (us_cts - t0) as f64 / 1e6
                        );
                    }
                } else if us_cts > last_ack_us {
                    let gap = us_cts - last_ack_us;
                    if gap <= GP_IDLE_THRESH_US {
                        active_us += gap;
                    } else {
                        // Idle gap — try flush
                        let flush_bytes = last_ack_seq.wrapping_sub(start_seq);
                        let flush_ok = flush_bytes > 0
                            && srtt > 0
                            && active_us >= srtt
                            && active_us <= GP_MAX_UTIM;
                        if show && in_range(us_cts) {
                            println!("{:>10.4}s  IDLE-GAP     gap={:.1}ms flush_bytes={} active={:.2}ms flush={}",
                                t, gap as f64 / 1000.0, flush_bytes, active_us as f64 / 1000.0,
                                if flush_ok { "SAMPLE" } else { "rejected" });
                        }
                        if flush_ok {
                            let bytes_ps = flush_bytes as u64 * USEC_IN_SEC_U64 / active_us;
                            if !gp_filled {
                                gp_bw = bytes_ps;
                                gp_filled = true;
                                num_measurements = 1;
                            } else if num_measurements < GP_FAST_THRESH {
                                gp_bw = (gp_bw + bytes_ps) / 2;
                                num_measurements = num_measurements.saturating_add(1);
                            } else {
                                gp_bw = (7 * gp_bw + bytes_ps) / 8;
                                num_measurements = num_measurements.saturating_add(1);
                            }
                            sample_count += 1;
                            if show && in_range(us_cts) {
                                println!("            -> FLUSH-SAMPLE #{} bytes={} utim={:.2}ms raw={:.1}Mbps gp_bw={:.1}Mbps",
                                    sample_count, flush_bytes, active_us as f64 / 1000.0,
                                    bytes_ps as f64 * 8.0 / 1e6, gp_bw as f64 * 8.0 / 1e6);
                            }
                        }
                        // Restart window
                        start_seq = *th_ack;
                        start_ts = us_cts;
                        active_us = 0;
                    }
                }
                last_ack_us = us_cts;
                last_ack_seq = *th_ack;

                // --- check_goodput_sample ---
                if !on || srtt == 0 {
                    continue;
                }

                if us_cts <= start_ts {
                    continue;
                }
                let elapsed = us_cts - start_ts;
                if elapsed < GP_SRTT_MULT * srtt {
                    continue;
                }

                let bytes = th_ack.wrapping_sub(start_seq);
                let min_bytes_val = ((gp_bw as u128 * srtt as u128 * GP_BDP_MULT as u128)
                    / USEC_IN_SEC_U64 as u128) as u32;
                let min_bytes_val = min_bytes_val.max(GP_MIN_BYTES);
                if bytes < min_bytes_val {
                    continue;
                }

                // --- do_goodput_sample ---
                if bytes == 0 {
                    continue;
                }
                let utim = active_us;
                if utim < srtt || utim > GP_MAX_UTIM {
                    if show && in_range(us_cts) {
                        println!(
                            "{:>10.4}s  SAMPLE-REJECT utim={:.2}ms srtt={:.2}ms",
                            t,
                            utim as f64 / 1000.0,
                            srtt as f64 / 1000.0
                        );
                    }
                    continue;
                }

                let bytes_ps = bytes as u64 * USEC_IN_SEC_U64 / utim;
                if !gp_filled {
                    gp_bw = bytes_ps;
                    gp_filled = true;
                    num_measurements = 1;
                } else if num_measurements < GP_FAST_THRESH {
                    gp_bw = (gp_bw + bytes_ps) / 2;
                    num_measurements = num_measurements.saturating_add(1);
                } else {
                    gp_bw = (7 * gp_bw + bytes_ps) / 8;
                    num_measurements = num_measurements.saturating_add(1);
                }
                sample_count += 1;
                println!("{:>10.4}s  SAMPLE #{:<4} bytes={} utim={:.2}ms elapsed={:.2}ms raw={:.1}Mbps gp_bw={:.1}Mbps min_bytes={}",
                    t, sample_count, bytes, utim as f64 / 1000.0, elapsed as f64 / 1000.0,
                    bytes_ps as f64 * 8.0 / 1e6, gp_bw as f64 * 8.0 / 1e6, min_bytes_val);

                // Restart window
                start_ts = us_cts;
                start_seq = *th_ack;
                last_ack_us = us_cts;
                last_ack_seq = *th_ack;
                active_us = 0;
                // on stays true
            }
            _ => {}
        }
    }
    println!();
    println!("  Total samples: {sample_count}");
}

pub fn cmd_ackdump(sessions: &Sessions, sid: u64, start_s: f64, end_s: f64) {
    let events = match sessions.get(&sid) {
        Some(e) => e,
        None => {
            eprintln!("Session {sid:#x} not found");
            return;
        }
    };
    let t0 = events.first().map(|e| e.ts).unwrap_or(0);

    println!("=== ACK/Goodput dump for session {sid:#x}, t=[{start_s:.3}..{end_s:.3}]s ===");
    println!(
        "{:>12}  {:>6}  {:>14}  {:>12}  {:>10}  {:>10}  {:>10}",
        "time_s", "type", "detail", "bytes_ack", "delta_ms", "srtt_us", "cwnd"
    );
    println!("{}", "-".repeat(90));

    let mut last_ts: Option<u64> = None;
    let mut last_srtt: u32 = 0;
    let mut last_cwnd: u32 = 0;
    let mut ack_count = 0u64;
    let mut gap_gt_50ms = 0u64;

    for e in events.iter() {
        let t = (e.ts - t0) as f64 / 1_000_000.0;

        match &e.payload {
            EventPayload::Rtt { srtt, .. } => {
                if *srtt > 0 && *srtt < 0x7FFF_FFFF {
                    last_srtt = *srtt;
                }
                continue;
            }
            EventPayload::Cwnd { cwnd, .. } => {
                last_cwnd = *cwnd;
                continue;
            }
            _ => {}
        }

        if t < start_s || t > end_s {
            {}
            if last_ts.is_some() {
            } else {
                last_ts = Some(e.ts);
            }
            continue;
        }

        let delta_ms = last_ts.map(|lt| (e.ts - lt) as f64 / 1000.0).unwrap_or(0.0);

        match &e.payload {
            EventPayload::Ack {
                acked,
                snd_una,
                inflight,
                ..
            } => {
                let marker = if delta_ms > 50.0 {
                    " ***"
                } else if delta_ms > 20.0 {
                    " *"
                } else {
                    ""
                };
                if delta_ms > 50.0 {
                    gap_gt_50ms += 1;
                }
                println!(
                    "{:>10.4}s  {:>6}  acked={:<8} una={:<12} inflt={:<8}  {:>8.2}ms  {:>10}  {:>10}{}",
                    t, "ACK", acked, snd_una, inflight,
                    delta_ms, last_srtt, last_cwnd, marker
                );
                ack_count += 1;
                last_ts = Some(e.ts);
            }
            EventPayload::Goodput {
                goodput,
                raw_bw,
                utim,
                ..
            } => {
                println!(
                    "{:>10.4}s  {:>6}  gp={:.1}Mb raw={:.1}Mb utim={:.1}ms",
                    t,
                    "GP",
                    *goodput as f64 * 8.0 / 1e6,
                    *raw_bw as f64 * 8.0 / 1e6,
                    *utim as f64 / 1000.0,
                );
                last_ts = Some(e.ts);
            }
            EventPayload::GpCheck {
                reason,
                elapsed_us,
                utim_us,
                now_us,
                start_ts_us,
                last_ack_us,
                srtt_us,
                th_ack: _,
                bytes,
                min_bytes,
            } => {
                let reason_str = match reason {
                    0 => "OFF",
                    1 => "OFF2",
                    2 => "RECOV",
                    3 => "TIME",
                    4 => "BYTES",
                    5 => "SAMPLE",
                    _ => "?",
                };
                println!(
                    "{:>10.4}s  {:>6}  reason={:<6} elapsed={:.2}ms utim={:.2}ms now={} start={} last_ack={} srtt={}us bytes={} min={}",
                    t, "GPCHK",
                    reason_str,
                    *elapsed_us as f64 / 1000.0,
                    *utim_us as f64 / 1000.0,
                    now_us, start_ts_us, last_ack_us,
                    srtt_us, bytes, min_bytes,
                );
                last_ts = Some(e.ts);
            }
            EventPayload::Out { seq, len, .. } => {
                println!("{:>10.4}s  {:>6}  seq={} len={}", t, "OUT", seq, len,);
                last_ts = Some(e.ts);
            }
            _ => {
                last_ts = Some(e.ts);
            }
        }
    }
    println!();
    println!("  ACKs in range: {ack_count}");
    println!("  ACK gaps > 50ms: {gap_gt_50ms}");
}

pub fn cmd_gpcheck(sessions: &Sessions, sid: u64, start_s: f64, end_s: f64) {
    let events = match sessions.get(&sid) {
        Some(e) => e,
        None => {
            eprintln!("Session {sid:#x} not found");
            return;
        }
    };
    let t0 = events.first().map(|e| e.ts).unwrap_or(0);
    println!("=== GP_CHECK events for session {sid:#x}, t=[{start_s:.3}..{end_s:.3}]s ===");
    println!(
        "{:>12}  {:>6}  {:>10}  {:>10}  {:>14}  {:>14}  {:>14}  {:>10}  {:>10}  {:>10}",
        "time_s",
        "reason",
        "elapsed_ms",
        "utim_ms",
        "now_us",
        "start_us",
        "last_ack_us",
        "srtt_us",
        "bytes",
        "min_bytes"
    );
    println!("{}", "-".repeat(140));

    let mut count = 0u32;
    let mut by_reason = [0u32; 6];

    for e in events.iter() {
        let t = (e.ts - t0) as f64 / 1_000_000.0;
        if t < start_s || t > end_s {
            continue;
        }

        if let EventPayload::GpCheck {
            reason,
            elapsed_us,
            utim_us,
            now_us,
            start_ts_us,
            last_ack_us,
            srtt_us,
            th_ack: _,
            bytes,
            min_bytes,
        } = &e.payload
        {
            let reason_str = match reason {
                0 => "OFF",
                1 => "OFF2",
                2 => "RECOV",
                3 => "TIME",
                4 => "BYTES",
                5 => "SAMPLE",
                _ => "?",
            };
            println!(
                "{:>10.4}s  {:>6}  {:>8.2}ms  {:>8.2}ms  {:>14}  {:>14}  {:>14}  {:>10}  {:>10}  {:>10}",
                t, reason_str,
                *elapsed_us as f64 / 1000.0,
                *utim_us as f64 / 1000.0,
                now_us, start_ts_us, last_ack_us,
                srtt_us, bytes, min_bytes,
            );
            count += 1;
            let r = *reason as usize;
            if r < 6 {
                by_reason[r] += 1;
            }
        }
    }
    println!();
    println!("  Total GP_CHECK events: {count}");
    println!(
        "  OFF={} OFF2={} RECOV={} TIME={} BYTES={} SAMPLE={}",
        by_reason[0], by_reason[1], by_reason[2], by_reason[3], by_reason[4], by_reason[5]
    );
}

pub fn cmd_goodput(sessions: &Sessions, filter: Option<u64>) {
    let mut session_list: Vec<_> = sessions.iter().collect();
    session_list.sort_by_key(|(sid, _)| *sid);

    for (&sid, events) in &session_list {
        if let Some(f) = filter {
            if sid != f {
                continue;
            }
        }
        let t0 = events.first().map(|e| e.ts).unwrap_or(0);

        let gp_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::Goodput { .. }))
            .collect();

        if gp_events.is_empty() {
            continue;
        }

        println!(
            "=== Goodput events for session {sid:#x} ({} samples) ===",
            gp_events.len()
        );
        println!(
            "{:>3}  {:>10}  {:>12}  {:>12}  {:>10}  {:>10}  {:>10}  {:>12}",
            "#",
            "time_s",
            "gp_bw(Mbps)",
            "raw_bw(Mbps)",
            "utim_ms",
            "span_ms",
            "ratio",
            "min_bytes"
        );
        println!("{}", "-".repeat(96));

        let mut prev_gp_bw: Option<u64> = None;

        for (i, e) in gp_events.iter().enumerate() {
            if let EventPayload::Goodput {
                goodput,
                raw_bw,
                utim,
                ..
            } = &e.payload
            {
                let t = (e.ts - t0) as f64 / 1_000_000.0;
                let gp_mbps = *goodput as f64 * 8.0 / 1e6;
                let raw_mbps = *raw_bw as f64 * 8.0 / 1e6;
                let utim_ms = *utim as f64 / 1000.0;

                // Find SRTT at this point from the nearest preceding RTT event
                let srtt_us = events
                    .iter()
                    .filter(|ev| ev.ts <= e.ts)
                    .rev()
                    .find_map(|ev| match &ev.payload {
                        EventPayload::Rtt { srtt, .. } if *srtt > 0 && *srtt < 0x7FFF_FFFF => {
                            Some(*srtt as u64)
                        }
                        _ => None,
                    })
                    .unwrap_or(0);

                // The byte threshold used gp_bw from BEFORE this sample's EWMA update
                let threshold_gp_bw = prev_gp_bw.unwrap_or(0);
                let min_bytes = if threshold_gp_bw > 0 && srtt_us > 0 {
                    let v = threshold_gp_bw as u128 * srtt_us as u128 * 4 / 1_000_000;
                    (v as u64).max(16384)
                } else {
                    16384
                };

                // Compute expected span duration at actual delivery rate
                let expected_ms = if *raw_bw > 0 {
                    min_bytes as f64 / *raw_bw as f64 * 1000.0
                } else {
                    0.0
                };

                let ratio = if expected_ms > 0.0 {
                    utim_ms / expected_ms
                } else {
                    0.0
                };

                let marker = if ratio > 2.0 { " <--" } else { "" };
                println!(
                    "{:>3}  {:>8.3}s  {:>12.1}  {:>12.1}  {:>8.2}ms  {:>8.2}ms  {:>8.1}x  {:>12}{}",
                    i, t, gp_mbps, raw_mbps, utim_ms, expected_ms, ratio, min_bytes, marker
                );

                prev_gp_bw = Some(*goodput);
            }
        }
        println!();
    }
}

/// Helper: parse a comma-separated list of unsigned integers (decimal
/// or `0x`-prefixed hex). Bad tokens are silently skipped so users can
/// paste output from `vmstat -P` without manual cleanup.
pub fn parse_csv_u<T>(s: &str) -> Vec<T>
where
    T: std::str::FromStr + std::convert::TryFrom<u64>,
{
    s.split(',')
        .filter_map(|tok| {
            let tok = tok.trim();
            if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
                u64::from_str_radix(hex, 16)
                    .ok()
                    .and_then(|v| T::try_from(v).ok())
            } else {
                tok.parse::<T>().ok()
            }
        })
        .collect()
}
