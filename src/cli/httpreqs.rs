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

/// One completed `HTTP_REQ_FREED` slot lifetime, reduced to the fields
/// needed to answer "how fast did this response actually get delivered":
/// bytes served (`end_seq - start_seq`) over the time from the request
/// being received to the last byte being acked. Prefers `first_send` as
/// the interval start (excludes time queued before the response started
/// sending) when the running kernel populates it; falls back to
/// `first_rcv_us` (whole-response latency, including any queueing) when it
/// doesn't -- some builds leave `first_send`/`sent_at_fs`/`sent_at_ls` zeroed.
struct ReqRecord {
    ts_s: f64,
    session_id: u64,
    bytes: u64,
    duration_us: u64,
}

fn mbps(bytes: u64, duration_us: u64) -> f64 {
    if duration_us == 0 {
        return 0.0;
    }
    (bytes as f64 * 8.0) / (duration_us as f64 / 1e6) / 1e6
}

/// Answers "are severe single-request stalls (>= `stall_us`) spread thinly
/// across many sessions (a systemic occasional hiccup), or concentrated
/// onto a small number of repeatedly-hit sessions (a few pathologically
/// broken connections)" -- the two look identical in an aggregate
/// percentile breakdown but imply very different fixes.
fn print_stall_report(records: &[ReqRecord], stall_us: u64) {
    use std::collections::HashMap;
    let mut per_session: HashMap<u64, (u32, f64)> = HashMap::new();
    let mut n_stalled = 0u64;
    for r in records {
        if r.duration_us >= stall_us {
            n_stalled += 1;
            let e = per_session.entry(r.session_id).or_insert((0, 0.0));
            e.0 += 1;
            e.1 = e.1.max(r.duration_us as f64 / 1e6);
        }
    }
    println!(
        "=== Stall report (requests >= {:.1}s to deliver) ===",
        stall_us as f64 / 1e6
    );
    println!(
        "{} of {} requests stalled ({:.3}%), spread across {} distinct sessions ({} total \
         sessions with at least one request in this capture)",
        n_stalled,
        records.len(),
        100.0 * n_stalled as f64 / records.len() as f64,
        per_session.len(),
        records
            .iter()
            .map(|r| r.session_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
    );
    if per_session.is_empty() {
        return;
    }
    let mut by_hits: Vec<(u64, u32, f64)> = per_session
        .into_iter()
        .map(|(sid, (hits, worst))| (sid, hits, worst))
        .collect();
    by_hits.sort_by_key(|&(_, hits, _)| std::cmp::Reverse(hits));
    let repeat_offenders = by_hits.iter().filter(|&&(_, hits, _)| hits > 1).count();
    let stalls_from_repeaters: u32 = by_hits
        .iter()
        .filter(|&&(_, hits, _)| hits > 1)
        .map(|&(_, hits, _)| hits)
        .sum();
    println!(
        "{repeat_offenders} sessions were stalled more than once (accounting for {stalls_from_repeaters} \
         of the {n_stalled} stalls) -- {}",
        if repeat_offenders as f64 / by_hits.len().max(1) as f64 > 0.3 {
            "stalls concentrate onto a repeat-offender subset, not spread evenly"
        } else {
            "stalls are mostly one-off, spread across distinct sessions"
        }
    );
    println!("\ntop 15 most-stalled sessions (session_id, hits, worst stall):");
    for &(sid, hits, worst) in by_hits.iter().take(15) {
        println!("  {sid:#018x}  hits={hits:<4}  worst={worst:.1}s");
    }
}

/// Per-connection HTTP response delivery rate, computed straight from
/// `HTTP_REQ_FREED`'s own `start_seq`/`end_seq`/`first_send`/`last_ack_us`
/// fields -- the direct, ground-truth answer to "did responses actually
/// get delivered slower", as opposed to inferring it from aggregate kernel
/// CPU/HPTS-volume proxies. `--bin-s` (default 60) controls the time-binned
/// breakdown's bucket width; `--raw` dumps one TSV line per completed
/// request (ts_s/session_id/bytes/duration_us/mbps) for offline analysis.
/// `--stall-report [secs]` (default 5s) instead reports whether requests
/// slower than that threshold are spread thinly across many sessions or
/// concentrated onto a repeat-offender few.
pub fn cmd_httpreqs(sessions: &Sessions, capture_start: u64, args: &[String]) {
    let bin_s: f64 = args
        .iter()
        .position(|a| a == "--bin-s")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(60.0);
    let raw = args.iter().any(|a| a == "--raw");
    let min_bytes: u64 = args
        .iter()
        .position(|a| a == "--min-bytes")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut records: Vec<ReqRecord> = Vec::new();
    let mut n_stale: u64 = 0;
    let mut n_forcefree: u64 = 0;
    let mut n_allocfail: u64 = 0;

    for (&session_id, events) in sessions {
        for ev in events {
            match &ev.payload {
                EventPayload::HttpReqFreed {
                    start_seq,
                    end_seq,
                    first_send,
                    last_ack_us,
                    first_rcv_us,
                    ..
                } => {
                    let bytes = end_seq.wrapping_sub(*start_seq) as u64;
                    let start_ts = if *first_send != 0 {
                        *first_send
                    } else {
                        *first_rcv_us
                    };
                    // > 100MB is never a real HTTP response in this workload -- a stale/re-used
                    // slot whose start_seq/end_seq pairing wrapped around, not a real sample.
                    if bytes < min_bytes.max(1)
                        || bytes > 100_000_000
                        || start_ts == 0
                        || *last_ack_us <= start_ts
                    {
                        continue;
                    }
                    records.push(ReqRecord {
                        ts_s: ev.ts.saturating_sub(capture_start) as f64 / 1e6,
                        session_id,
                        bytes,
                        duration_us: last_ack_us - start_ts,
                    });
                }
                EventPayload::HttpReqStale { .. } => n_stale += 1,
                EventPayload::HttpReqForcefree { .. } => n_forcefree += 1,
                EventPayload::HttpReqAllocfail { .. } => n_allocfail += 1,
                _ => {}
            }
        }
    }

    if records.is_empty() {
        println!(
            "No usable HTTP_REQ_FREED completions in this capture (either no HTTP traffic, or \
             every completion is missing first_send/last_ack_us)."
        );
        return;
    }
    records.sort_by(|a, b| a.ts_s.partial_cmp(&b.ts_s).unwrap());

    if raw {
        println!("ts_s\tsession_id\tbytes\tduration_us\tmbps");
        for r in &records {
            println!(
                "{:.6}\t{:#x}\t{}\t{}\t{:.3}",
                r.ts_s,
                r.session_id,
                r.bytes,
                r.duration_us,
                mbps(r.bytes, r.duration_us)
            );
        }
        return;
    }

    if let Some(i) = args.iter().position(|a| a == "--stall-report") {
        let stall_us: u64 = args
            .get(i + 1)
            .and_then(|s| s.parse::<f64>().ok())
            .map(|secs| (secs * 1e6) as u64)
            .unwrap_or(5_000_000);
        print_stall_report(&records, stall_us);
        return;
    }

    let n = records.len();
    let size_note = if min_bytes > 0 {
        format!(" [--min-bytes {min_bytes}: excludes small manifest/heartbeat-sized requests]")
    } else {
        String::new()
    };
    println!(
        "=== HTTP request delivery rate ({n} completed, {n_stale} stale, {n_forcefree} \
         forcefree, {n_allocfail} allocfail){size_note} ==="
    );
    println!(
        "(delivery rate = bytes served / (last_ack_us - first_send, or - first_rcv_us if this \
         build didn't populate first_send) -- stale/forcefree/allocfail are requests that never \
         cleanly completed)\n"
    );

    let mut rates: Vec<f64> = records
        .iter()
        .map(|r| mbps(r.bytes, r.duration_us))
        .collect();
    rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean: f64 = rates.iter().sum::<f64>() / rates.len() as f64;
    println!(
        "overall: mean={:.1} Mbps  p10={:.1}  p50={:.1}  p90={:.1}  p99={:.1}  (n={n})",
        mean,
        percentile(&rates, 10.0),
        percentile(&rates, 50.0),
        percentile(&rates, 90.0),
        percentile(&rates, 99.0),
    );

    let t_min = records.first().map(|r| r.ts_s).unwrap_or(0.0);
    let t_max = records.last().map(|r| r.ts_s).unwrap_or(0.0);
    let n_bins = ((t_max - t_min) / bin_s).ceil() as usize + 1;

    println!(
        "\n{:>8}  {:>6}  {:>8}  {:>8}  {:>8}  {:>8}",
        "t(s)", "n", "p10", "p50", "p90", "p99"
    );
    for b in 0..n_bins {
        let lo = t_min + b as f64 * bin_s;
        let hi = lo + bin_s;
        let mut bin_rates: Vec<f64> = records
            .iter()
            .filter(|r| r.ts_s >= lo && r.ts_s < hi)
            .map(|r| mbps(r.bytes, r.duration_us))
            .collect();
        if bin_rates.is_empty() {
            continue;
        }
        bin_rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "{:>8.0}  {:>6}  {:>8.1}  {:>8.1}  {:>8.1}  {:>8.1}",
            lo,
            bin_rates.len(),
            percentile(&bin_rates, 10.0),
            percentile(&bin_rates, 50.0),
            percentile(&bin_rates, 90.0),
            percentile(&bin_rates, 99.0),
        );
    }
}
