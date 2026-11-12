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

/// Per-CPU overall busy fraction (whole-capture single bin) plus the
/// top thread(s) by sample count on each CPU. Answers "is any single
/// core saturated even though the system-wide average looks fine" --
/// an aggregate CPU% can hide one pinned/bound thread (e.g. an HPTS
/// instance bound to a specific core) running at or near 100% while
/// other cores have slack, since averaging across cores dilutes it
/// away.
///
/// `--filter <substring>` restricts the top-threads listing to
/// threads whose `comm` or `tdname` contains the substring
/// (case-insensitive) and additionally prints, for each matching
/// thread, exactly which CPU it ran on and what fraction of that
/// CPU's busy samples it accounted for.
///
/// `--timeline [N]` additionally prints an N-bin (default 60)
/// aggregate "CPUs busy" time series (sum across all cores, in units
/// of whole CPUs) so a whole-run average can be checked against
/// whether saturation was sustained or bursty, and lined up against
/// other timelines (e.g. `starsagg`'s deny-rate) by wall-clock offset.
///
/// `--start-s`/`--end-s` (capture-relative seconds, same convention as
/// `flame`) restrict every stat below to that window -- an aggregate
/// whole-capture busy% can hide a saturation episode that's real but
/// short-lived, the same way it can hide a single pinned-core problem.
pub fn cmd_percore(profile: &CpuProfile, capture_start_us: u64, args: &[String]) {
    if profile.is_empty() {
        eprintln!("This capture has no cpu-provider events.");
        return;
    }

    let mut filter: Option<String> = None;
    let mut timeline_bins: Option<usize> = None;
    let mut ts_start_us: u64 = 0;
    let mut ts_end_us: u64 = u64::MAX;
    let mut freq_scale = false;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--filter" {
            filter = args.get(i + 1).cloned();
            i += 2;
        } else if args[i] == "--freq-scale" {
            freq_scale = true;
            i += 1;
        } else if args[i] == "--timeline" {
            let n = args
                .get(i + 1)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(60);
            timeline_bins = Some(n);
            i += if args
                .get(i + 1)
                .and_then(|s| s.parse::<usize>().ok())
                .is_some()
            {
                2
            } else {
                1
            };
        } else if args[i] == "--start-s" {
            i += 1;
            if let Some(v) = args.get(i).and_then(|s| s.parse::<f64>().ok()) {
                ts_start_us = capture_start_us.saturating_add((v * 1e6) as u64);
            }
            i += 1;
        } else if args[i] == "--end-s" {
            i += 1;
            if let Some(v) = args.get(i).and_then(|s| s.parse::<f64>().ok()) {
                ts_end_us = capture_start_us.saturating_add((v * 1e6) as u64);
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    let filter_lc = filter.as_ref().map(|f| f.to_lowercase());

    let windowed: Vec<_> = if ts_start_us > 0 || ts_end_us != u64::MAX {
        profile
            .samples
            .iter()
            .filter(|s| s.ts >= ts_start_us && s.ts <= ts_end_us)
            .cloned()
            .collect()
    } else {
        profile.samples.clone()
    };
    if windowed.is_empty() {
        eprintln!("No CPU samples in the requested --start-s/--end-s window.");
        return;
    }

    let hz = profile.kernel.as_ref().map(|k| k.sample_hz());
    let rated = profile.kernel.as_ref().and_then(|k| k.rated_freq_mhz());
    let bins = compute_cpu_usage_bins(&windowed, 1, hz, rated);
    if bins.is_empty() {
        eprintln!("No CPU samples to bin.");
        return;
    }

    // (cpu_idx, tid) -> busy sample count, plus per-cpu busy total for
    // the "% of this CPU's busy time" denominator below.
    let mut by_cpu_tid: FxHashMap<(u16, u32), u64> = FxHashMap::default();
    let mut cpu_busy_total: FxHashMap<u16, u64> = FxHashMap::default();
    for s in &windowed {
        if (s.flags & CPU_SAMPLE_FLAG_IDLE) != 0 {
            continue;
        }
        *by_cpu_tid.entry((s.cpu, s.tid)).or_insert(0) += 1;
        *cpu_busy_total.entry(s.cpu).or_insert(0) += 1;
    }

    let window_scale = if freq_scale {
        bins.freq_scale_for(0) as f64
    } else {
        1.0
    };
    println!(
        "=== Per-CPU busy % ({}) + top thread per CPU ===",
        if ts_start_us > 0 || ts_end_us != u64::MAX {
            format!(
                "t={:.1}s..{:.1}s",
                ts_start_us.saturating_sub(capture_start_us) as f64 / 1e6,
                if ts_end_us == u64::MAX {
                    f64::INFINITY
                } else {
                    ts_end_us.saturating_sub(capture_start_us) as f64 / 1e6
                }
            )
        } else {
            "whole capture".to_string()
        }
    );
    println!(
        "{:>5} {:>8}  {:<20} {:>10} {:>8}",
        "cpu", "busy%", "top thread (tdname)", "samples", "% of cpu"
    );
    println!("{}", "-".repeat(60));
    for (idx, &cpu_id) in bins.cpu_ids.iter().enumerate() {
        let busy_pct = bins.cpu_busy_fraction(0, idx) * 100.0 * window_scale;
        let total_busy = cpu_busy_total.get(&cpu_id).copied().unwrap_or(0);

        // Top tid on this cpu by busy sample count.
        let mut top: Option<(u32, u64)> = None;
        for (&(c, tid), &n) in &by_cpu_tid {
            if c != cpu_id {
                continue;
            }
            if top.is_none_or(|(_, best)| n > best) {
                top = Some((tid, n));
            }
        }
        let (name, samples, of_cpu) = match top {
            Some((tid, n)) => {
                let name = profile
                    .threads
                    .get(&tid)
                    .map(|t| format!("{} (tid {tid})", t.tdname))
                    .unwrap_or_else(|| format!("<unknown> (tid {tid})"));
                let of_cpu = if total_busy > 0 {
                    n as f64 / total_busy as f64 * 100.0
                } else {
                    0.0
                };
                (name, n, of_cpu)
            }
            None => ("<idle only>".to_string(), 0, 0.0),
        };
        println!(
            "{:>5} {:>7.1}%  {:<20} {:>10} {:>7.1}%",
            cpu_id, busy_pct, name, samples, of_cpu
        );
    }

    if let Some(n) = timeline_bins {
        let tl = compute_cpu_usage_bins(&windowed, n, hz, rated);
        println!();
        println!(
            "=== Aggregate CPUs-busy timeline ({} bins x {} ms, {} cores total, \
             peak_samples_per_bin={}, nominal_hz={:?}{}) ===",
            tl.bin_count,
            tl.bin_us as f64 / 1000.0,
            tl.ncpus(),
            tl.peak_samples_per_bin,
            hz,
            if freq_scale {
                format!(", freq_scale ref_mhz={:?}", tl.reference_freq_mhz)
            } else {
                String::new()
            }
        );
        for b in 0..tl.bin_count {
            let t_s = (tl.t0_us + tl.bin_us * b as u64) as f64 / 1_000_000.0;
            let scale = if freq_scale {
                tl.freq_scale_for(b) as f64
            } else {
                1.0
            };
            let busy_cpus = tl.cpus_busy(b) * scale;
            let pct = busy_cpus / tl.ncpus() as f64 * 100.0;
            let bar_len = (pct / 2.0).round() as usize;
            println!(
                "  t={t_s:>8.1}s  {busy_cpus:>5.1}/{} cpus  {pct:>5.1}%  {}",
                tl.ncpus(),
                "#".repeat(bar_len.min(50))
            );
        }
    }

    let Some(needle) = filter_lc else { return };
    println!();
    println!("=== Threads matching {needle:?} (comm or tdname) ===");
    println!(
        "{:>6} {:>5} {:<24} {:<20} {:>10} {:>8}",
        "tid", "cpu", "comm", "tdname", "samples", "% of cpu"
    );
    println!("{}", "-".repeat(80));
    // tid -> (best cpu, samples on that cpu) for threads that migrate;
    // report every (tid, cpu) pair they actually appeared on so a
    // thread that hops cores (not bound) is visible as such.
    let mut rows: Vec<(u32, u16, u64)> = by_cpu_tid
        .iter()
        .filter(|(&(_, tid), _)| {
            profile
                .threads
                .get(&tid)
                .map(|t| {
                    t.comm.to_lowercase().contains(&needle)
                        || t.tdname.to_lowercase().contains(&needle)
                })
                .unwrap_or(false)
        })
        .map(|(&(cpu, tid), &n)| (tid, cpu, n))
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.2));
    if rows.is_empty() {
        println!("(no threads matched)");
        return;
    }
    for (tid, cpu, n) in rows {
        let (comm, tdname) = profile
            .threads
            .get(&tid)
            .map(|t| (t.comm.clone(), t.tdname.clone()))
            .unwrap_or_else(|| ("<unknown>".to_string(), "<unknown>".to_string()));
        let of_cpu = cpu_busy_total
            .get(&cpu)
            .filter(|&&t| t > 0)
            .map(|&t| n as f64 / t as f64 * 100.0)
            .unwrap_or(0.0);
        println!("{tid:>6} {cpu:>5} {comm:<24} {tdname:<20} {n:>10} {of_cpu:>7.1}%");
    }
}
