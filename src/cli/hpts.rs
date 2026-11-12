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

fn hpts_payload_detail(payload: &HptsWheelPayload) -> String {
    match payload {
        HptsWheelPayload::Init {
            num_hptss,
            bind_type,
            num_slots,
            usecs_per_slot,
            total_slot_time,
        } => {
            format!(
                "num_hptss={num_hptss} bind_type={} num_slots={num_slots} \
                 usecs_per_slot={usecs_per_slot} total_slot_time={total_slot_time}",
                hpts_bind_type_name(*bind_type),
            )
        }
        HptsWheelPayload::InitHptsEntry => "(marker)".to_string(),
        HptsWheelPayload::InitTimingParams {
            usecs_per_slot,
            total_slot_time,
            num_slots,
            wheel_mask,
        } => {
            format!(
                "usecs_per_slot={usecs_per_slot} total_slot_time={total_slot_time} \
                 num_slots={num_slots} wheel_mask={wheel_mask:#x}"
            )
        }
        HptsWheelPayload::StateActive {
            from_callout,
            cur_slot,
            tp_cur_count,
        } => {
            format!("from_callout={from_callout} cur_slot={cur_slot} tp_cur_count={tp_cur_count}")
        }
        HptsWheelPayload::StateInactive { slots_ran } => format!("slots_ran={slots_ran}"),
        HptsWheelPayload::Sleep { sleep_time } => format!("sleep_time={sleep_time}us"),
        HptsWheelPayload::ConnInsert {
            gencnt,
            slot,
            usecs,
            tp_cur_count,
        } => {
            format!("gencnt={gencnt} slot={slot} usecs={usecs} tp_cur_count={tp_cur_count}")
        }
        HptsWheelPayload::ConnRemove {
            gencnt,
            slot,
            tp_cur_count,
        } => format!("gencnt={gencnt} slot={slot} tp_cur_count={tp_cur_count}"),
        HptsWheelPayload::ConnMove { gencnt, old_slot } => {
            format!("gencnt={gencnt} old_slot={old_slot}")
        }
        HptsWheelPayload::WheelWrap {
            wrap_loop_cnt,
            slots_to_run,
            prev_slot,
            cur_slot,
        } => {
            format!(
                "wrap_loop_cnt={wrap_loop_cnt} slots_to_run={slots_to_run} \
                 prev_slot={prev_slot} cur_slot={cur_slot}"
            )
        }
        HptsWheelPayload::Processing {
            from_callout,
            prev_slot,
            cur_slot,
            slots_to_run,
            tp_cur_count,
        } => {
            format!(
                "from_callout={from_callout} prev_slot={prev_slot} cur_slot={cur_slot} \
                 slots_to_run={slots_to_run} tp_cur_count={tp_cur_count}"
            )
        }
        HptsWheelPayload::PerfProcessing {
            from_callout,
            slots_processed,
            tps_processed,
            runtime_usec,
            loop_cnt,
        } => {
            format!(
                "from_callout={from_callout} slots_processed={slots_processed} \
                 tps_processed={tps_processed} runtime_usec={runtime_usec} loop_cnt={loop_cnt}"
            )
        }
        HptsWheelPayload::PerfLateness {
            gencnt,
            lateness_slots,
            lateness_usec,
            requested_time,
            actual_time,
        } => {
            format!(
                "gencnt={gencnt} lateness_slots={lateness_slots} lateness_usec={lateness_usec} \
                 requested_time={requested_time} actual_time={actual_time}"
            )
        }
        HptsWheelPayload::LatenessSample { buckets } => {
            format!("buckets={}", format_hist_buckets(buckets))
        }
        HptsWheelPayload::TpBatchSample { buckets } => {
            format!("buckets={}", format_hist_buckets(buckets))
        }
        HptsWheelPayload::SlotGateSample {
            fires,
            diverts,
            bytes,
            rollover_max,
            walks,
            closed,
            default_bytes,
            rack_bytes,
            bbr_bytes,
            closed_tcp_bytes,
            caller0,
            caller0_bytes,
            caller1,
            caller1_bytes,
            caller_other_bytes,
        } => {
            format!(
                "fires={fires} diverts={diverts} bytes={bytes} \
                 rollover_max={rollover_max} walks={walks} closed={closed} \
                 default_bytes={default_bytes} rack_bytes={rack_bytes} \
                 bbr_bytes={bbr_bytes} closed_tcp_bytes={closed_tcp_bytes} \
                 caller0={caller0:#x} caller0_bytes={caller0_bytes} \
                 caller1={caller1:#x} caller1_bytes={caller1_bytes} \
                 caller_other_bytes={caller_other_bytes}"
            )
        }
        HptsWheelPayload::OpporSample {
            oppor_runs,
            oppor_noop,
            buckets,
        } => {
            format!(
                "oppor_runs={oppor_runs} oppor_noop={oppor_noop} buckets={}",
                format_hist_buckets(buckets)
            )
        }
        HptsWheelPayload::NoRunSlotSkip {
            prev_slot,
            new_prev_slot,
            skipped_slots,
            tp_cur_count,
        } => {
            format!(
                "prev_slot={prev_slot} new_prev_slot={new_prev_slot} \
                 skipped_slots={skipped_slots} tp_cur_count={tp_cur_count}"
            )
        }
    }
}

/// Compact raw rendering of a cumulative histogram's nonzero buckets,
/// for the per-event detail line in `hptsevents` (e.g. `b0=12 b3=45`).
fn format_hist_buckets(buckets: &[u64]) -> String {
    let nonzero: Vec<String> = buckets
        .iter()
        .enumerate()
        .filter(|(_, &v)| v != 0)
        .map(|(b, v)| format!("b{b}={v}"))
        .collect();
    if nonzero.is_empty() {
        "(all zero)".to_string()
    } else {
        nonzero.join(" ")
    }
}

/// Diff a cumulative per-hpts-instance histogram stream into one
/// total-occupancy histogram over the whole capture. Keyed by
/// `session_id`, not `cpu`: each `tcp_hpts_entry` logs to its own fixed
/// `p_eventlog_session` for the life of the machine, but the *logging*
/// cpu in the event header is whichever cpu happened to be running
/// `tcp_hptsi()` for that trip (wheels are commonly processed
/// opportunistically from a foreign send path, not only from their own
/// bound callout) -- so `cpu` is not a stable per-instance key, and
/// grouping by it would interleave several instances' independent
/// cumulative counters. `events` need not be ts-sorted globally, but
/// *within* each session's own sequence they're taken in capture order
/// (the parser preserves event order).
///
/// This collapses to one grand total rather than a time series, so an
/// instance's first observed sample must NOT be counted at full value:
/// `hist_lateness_usec` / `hist_tp_batch_size` are lifetime-of-the-hpts-
/// instance counters, not per-capture ones, so a first sample taken well
/// after boot would otherwise dump hours/days of pre-capture history into
/// bucket totals. The first sample per instance is therefore only
/// recorded as a baseline (zero contribution); a per-bucket drop on later
/// samples is treated as a counter reset (the new value becomes the
/// delta) rather than going negative. `analysis::build_hpts_lateness_stack`
/// / `build_hpts_batch_stack` apply this same baseline-exclusion to the
/// GUI's time-series (rather than grand-total) rendering of the same
/// samples.
fn diff_cumulative_hist<'a>(events: impl Iterator<Item = (u64, &'a [u64])>) -> (usize, Vec<u64>) {
    let mut last: std::collections::HashMap<u64, Vec<u64>> = std::collections::HashMap::new();
    let mut nbuckets = 0usize;
    let mut total = Vec::new();
    for (session_id, buckets) in events {
        if buckets.len() > nbuckets {
            nbuckets = buckets.len();
            total.resize(nbuckets, 0);
        }
        let first_sample_for_instance = !last.contains_key(&session_id);
        let prev = last
            .entry(session_id)
            .or_insert_with(|| vec![0u64; nbuckets]);
        prev.resize(nbuckets, 0);
        if first_sample_for_instance {
            prev[..buckets.len()].copy_from_slice(buckets);
            continue;
        }
        for (b, &v) in buckets.iter().enumerate() {
            let delta = if v >= prev[b] { v - prev[b] } else { v };
            total[b] += delta;
            prev[b] = v;
        }
    }
    (nbuckets, total)
}

/// Print one diffed histogram as a bucket/count/percentage table, using
/// `label` to render each bucket index.
fn print_hist_summary(
    title: &str,
    nbuckets: usize,
    total: &[u64],
    label: impl Fn(usize, usize) -> String,
) {
    let grand_total: u64 = total.iter().sum();
    println!("=== {title} ({grand_total} samples across {nbuckets} buckets) ===");
    if grand_total == 0 {
        println!("  (all buckets zero)");
        return;
    }
    for (b, &count) in total.iter().enumerate() {
        if count == 0 {
            continue;
        }
        println!(
            "  {:>10}  {:>12}  {:>6.2}%",
            label(b, nbuckets),
            count,
            100.0 * count as f64 / grand_total as f64
        );
    }
    println!();
}

fn hpts_payload_event_id(payload: &HptsWheelPayload) -> u32 {
    match payload {
        HptsWheelPayload::Init { .. } => EVT_HPTS_INIT,
        HptsWheelPayload::InitHptsEntry => EVT_HPTS_INIT_HPTS_ENTRY,
        HptsWheelPayload::InitTimingParams { .. } => EVT_HPTS_INIT_TIMING_PARAMS,
        HptsWheelPayload::StateActive { .. } => EVT_HPTS_STATE_ACTIVE,
        HptsWheelPayload::StateInactive { .. } => EVT_HPTS_STATE_INACTIVE,
        HptsWheelPayload::Sleep { .. } => EVT_HPTS_SLEEP,
        HptsWheelPayload::ConnInsert { .. } => EVT_HPTS_CONN_INSERT,
        HptsWheelPayload::ConnRemove { .. } => EVT_HPTS_CONN_REMOVE,
        HptsWheelPayload::ConnMove { .. } => EVT_HPTS_CONN_MOVE,
        HptsWheelPayload::WheelWrap { .. } => EVT_HPTS_WHEEL_WRAP,
        HptsWheelPayload::Processing { .. } => EVT_HPTS_PROCESSING,
        HptsWheelPayload::PerfProcessing { .. } => EVT_HPTS_PERF_PROCESSING,
        HptsWheelPayload::PerfLateness { .. } => EVT_HPTS_PERF_LATENESS,
        HptsWheelPayload::LatenessSample { .. } => EVT_HPTS_LATENESS_SAMPLE,
        HptsWheelPayload::TpBatchSample { .. } => EVT_HPTS_TP_BATCH_SAMPLE,
        HptsWheelPayload::SlotGateSample { .. } => EVT_HPTS_SLOT_GATE_SAMPLE,
        HptsWheelPayload::OpporSample { .. } => EVT_HPTS_OPPOR_SAMPLE,
        HptsWheelPayload::NoRunSlotSkip { .. } => EVT_HPTS_NO_RUN_SLOT_SKIP,
    }
}

/// Dump the `hpts` provider's timing-wheel scheduler trace (ids 0-14 --
/// a separate schema from `STARS_AGG_SAMPLE`). `filter`, when given,
/// restricts output to one event type by name (case-insensitive,
/// e.g. `"PERF_LATENESS"`); see `hpts_wheel_event_name` for the full list.
pub fn cmd_hpts_events(events: &[HptsWheelEvent], capture_start: u64, filter: Option<&str>) {
    if events.is_empty() {
        println!(
            "No hpts timing-wheel scheduler events in this capture \
             (only STARS_AGG_SAMPLE, if present -- see `starsagg`)."
        );
        return;
    }
    let filter = filter.map(|f| f.to_ascii_uppercase());
    println!(
        "=== hpts timing-wheel scheduler trace ({} events{}) ===",
        events.len(),
        filter
            .as_deref()
            .map(|f| format!(", filtered to {f}"))
            .unwrap_or_default(),
    );
    println!(
        "{:>10}  {:<6} {:>18} {:<16} detail",
        "t(s)", "cpu", "session_id", "event"
    );
    println!("{}", "-".repeat(100));
    for e in events {
        let id = hpts_payload_event_id(&e.payload);
        let name = hpts_wheel_event_name(id);
        if let Some(f) = &filter {
            if name != f {
                continue;
            }
        }
        let t = (e.ts.saturating_sub(capture_start)) as f64 / 1e6;
        println!(
            "{:>10.3}  {:<6} {:>#18x} {:<16} {}",
            t,
            e.cpu,
            e.session_id,
            name,
            hpts_payload_detail(&e.payload),
        );
    }
}

/// Summarize `PERF_LATENESS` events (how late a TCP connection's wheel
/// callout fired vs. its requested slot) -- the primary "hpts delay"
/// diagnostic. Reports percentiles of `lateness_usec` plus the worst
/// offenders, both overall and broken down by (cpu, session_id) so a
/// specific stuck/overloaded hpts instance stands out.
///
/// Also reports the periodic per-CPU `HPTS_LATENESS_SAMPLE` /
/// `HPTS_TP_BATCH_SAMPLE` histograms (ids 13-14), if present: a
/// capture-wide view of the same lateness distribution plus how many
/// connections each wheel callout processed per batch, independent of
/// (and cheaper to keep enabled long-term than) per-connection
/// `PERF_LATENESS` events.
///
/// `--start-s`/`--end-s` (capture-relative seconds, same convention as
/// `flame`/`percore`) restrict both the `PERF_LATENESS` summary and the
/// histogram diffs to that window -- a capture-wide batch-size
/// histogram can be dominated by a long healthy majority and hide
/// whether batches actually saturate at the slot-gate's per-slot cap
/// during a specific bad window.
pub fn cmd_hpts_lateness(events: &[HptsWheelEvent], capture_start: u64, args: &[String]) {
    let mut ts_start_us: u64 = 0;
    let mut ts_end_us: u64 = u64::MAX;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--start-s" {
            i += 1;
            if let Some(v) = args.get(i).and_then(|s| s.parse::<f64>().ok()) {
                ts_start_us = capture_start.saturating_add((v * 1e6) as u64);
            }
        } else if args[i] == "--end-s" {
            i += 1;
            if let Some(v) = args.get(i).and_then(|s| s.parse::<f64>().ok()) {
                ts_end_us = capture_start.saturating_add((v * 1e6) as u64);
            }
        }
        i += 1;
    }
    let windowed: Vec<&HptsWheelEvent> = if ts_start_us > 0 || ts_end_us != u64::MAX {
        events
            .iter()
            .filter(|e| e.ts >= ts_start_us && e.ts <= ts_end_us)
            .collect()
    } else {
        events.iter().collect()
    };
    let events: Vec<HptsWheelEvent> = windowed.into_iter().cloned().collect();
    let events = events.as_slice();

    let lateness: Vec<(&HptsWheelEvent, u32, u32)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            HptsWheelPayload::PerfLateness {
                lateness_slots,
                lateness_usec,
                ..
            } => Some((e, *lateness_slots, *lateness_usec)),
            _ => None,
        })
        .collect();

    if lateness.is_empty() {
        println!(
            "No PERF_LATENESS events in this capture (only present on kernels emitting the \
             newer hpts timing-wheel schema -- see `hptsevents`)."
        );
    } else {
        print_perf_lateness_summary(&lateness, capture_start);
    }

    print_lateness_and_batch_histograms(events);
}

fn print_perf_lateness_summary(lateness: &[(&HptsWheelEvent, u32, u32)], capture_start: u64) {
    let mut usecs: Vec<f64> = lateness.iter().map(|(_, _, us)| *us as f64).collect();
    usecs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = usecs.len();
    let sum: f64 = usecs.iter().sum();
    let zero_count = usecs.iter().filter(|&&v| v == 0.0).count();

    println!("=== hpts PERF_LATENESS summary ({n} events) ===");
    println!(
        "  on-time (0 usec): {zero_count} ({:.1}%)",
        100.0 * zero_count as f64 / n as f64
    );
    println!("  mean:   {:>10.1} usec", sum / n as f64);
    println!("  p50:    {:>10.1} usec", percentile(&usecs, 50.0));
    println!("  p90:    {:>10.1} usec", percentile(&usecs, 90.0));
    println!("  p99:    {:>10.1} usec", percentile(&usecs, 99.0));
    println!("  p99.9:  {:>10.1} usec", percentile(&usecs, 99.9));
    println!("  max:    {:>10.1} usec", usecs[n - 1]);
    println!();

    // Break down by (cpu, session_id) -- each is one hpts instance --
    // so a single overloaded/stuck instance stands out from the rest.
    let mut by_instance: std::collections::BTreeMap<(u16, u64), Vec<f64>> =
        std::collections::BTreeMap::new();
    for (e, _, us) in lateness {
        by_instance
            .entry((e.cpu, e.session_id))
            .or_default()
            .push(*us as f64);
    }
    if by_instance.len() > 1 {
        println!(
            "=== by hpts instance (cpu, session_id) -- {} instances ===",
            by_instance.len()
        );
        println!(
            "{:>6} {:>18}  {:>10} {:>12} {:>12} {:>12}",
            "cpu", "session_id", "count", "mean_us", "p99_us", "max_us"
        );
        println!("{}", "-".repeat(80));
        let mut rows: Vec<_> = by_instance.into_iter().collect();
        // Worst offenders first: sort by max lateness descending.
        rows.sort_by(|a, b| {
            let max_a = a.1.iter().cloned().fold(0.0_f64, f64::max);
            let max_b = b.1.iter().cloned().fold(0.0_f64, f64::max);
            max_b.partial_cmp(&max_a).unwrap()
        });
        for ((cpu, sid), mut vs) in rows {
            vs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mean = vs.iter().sum::<f64>() / vs.len() as f64;
            let max = vs[vs.len() - 1];
            println!(
                "{:>6} {:>#18x}  {:>10} {:>12.1} {:>12.1} {:>12.1}",
                cpu,
                sid,
                vs.len(),
                mean,
                percentile(&vs, 99.0),
                max,
            );
        }
        println!();
    }

    println!("=== worst 20 PERF_LATENESS events ===");
    println!(
        "{:>10}  {:>6} {:>18}  {:>10} {:>12}",
        "t(s)", "cpu", "session_id", "slots", "usec"
    );
    println!("{}", "-".repeat(70));
    let mut worst: Vec<_> = lateness.iter().collect();
    worst.sort_by_key(|(_, _, us)| std::cmp::Reverse(*us));
    for (e, slots, us) in worst.into_iter().take(20) {
        let t = (e.ts.saturating_sub(capture_start)) as f64 / 1e6;
        println!(
            "{:>10.3}  {:>6} {:>#18x}  {:>10} {:>12}",
            t, e.cpu, e.session_id, slots, us
        );
    }
}

/// Diff and print the periodic per-CPU `HPTS_LATENESS_SAMPLE` /
/// `HPTS_TP_BATCH_SAMPLE` histograms (ids 13-14) as capture-wide
/// occupancy tables. Prints nothing (beyond a note) for whichever of
/// the two is absent -- older kernels only emit `PERF_LATENESS`.
fn print_lateness_and_batch_histograms(events: &[HptsWheelEvent]) {
    let lateness_hist = diff_cumulative_hist(events.iter().filter_map(|e| match &e.payload {
        HptsWheelPayload::LatenessSample { buckets } => Some((e.session_id, buckets.as_slice())),
        _ => None,
    }));
    let batch_hist = diff_cumulative_hist(events.iter().filter_map(|e| match &e.payload {
        HptsWheelPayload::TpBatchSample { buckets } => Some((e.session_id, buckets.as_slice())),
        _ => None,
    }));

    if lateness_hist.0 == 0 && batch_hist.0 == 0 {
        println!(
            "No HPTS_LATENESS_SAMPLE/HPTS_TP_BATCH_SAMPLE events in this capture (only present \
             on kernels emitting the per-CPU histogram sample schema -- see `hptsevents`)."
        );
        return;
    }
    if lateness_hist.0 > 0 {
        print_hist_summary(
            "hpts LATENESS_SAMPLE occupancy (all hpts instances, capture-wide)",
            lateness_hist.0,
            &lateness_hist.1,
            hpts_lateness_bucket_label,
        );
    }
    if batch_hist.0 > 0 {
        print_hist_summary(
            "hpts TP_BATCH_SAMPLE occupancy (all hpts instances, capture-wide)",
            batch_hist.0,
            &batch_hist.1,
            hpts_batch_bucket_label,
        );
    }

    let oppor_hist = diff_cumulative_hist(events.iter().filter_map(|e| match &e.payload {
        HptsWheelPayload::OpporSample { buckets, .. } => Some((e.session_id, buckets.as_slice())),
        _ => None,
    }));
    if oppor_hist.0 == 0 {
        return;
    }
    print_hist_summary(
        "hpts HPTS_OPPOR_SAMPLE occupancy -- lro-driven runs only \
         (all hpts instances, capture-wide)",
        oppor_hist.0,
        &oppor_hist.1,
        hpts_batch_bucket_label,
    );
    // TP_BATCH_SAMPLE counts every run, so subtracting the lro-driven ones
    // leaves the callout-driven remainder. Comparing the two tails is the
    // point of the split: a heavier tail on the lro side implicates
    // tcp_choose_hpts_to_run()'s most-backlogged-wheel selection as what
    // concentrates connections onto a tick.
    if batch_hist.0 > 0 {
        let n = batch_hist.0.max(oppor_hist.0);
        let callout: Vec<u64> = (0..n)
            .map(|b| {
                let all = batch_hist.1.get(b).copied().unwrap_or(0);
                let opp = oppor_hist.1.get(b).copied().unwrap_or(0);
                all.saturating_sub(opp)
            })
            .collect();
        print_hist_summary(
            "hpts batch occupancy -- callout-driven runs only \
             (TP_BATCH_SAMPLE minus HPTS_OPPOR_SAMPLE, derived)",
            n,
            &callout,
            hpts_batch_bucket_label,
        );
    }
    print_oppor_attempt_counters(events);
}

/// Report how often the lro path found a wheel worth running. `oppor_noop`
/// counts attempts that bailed because the wheel was already active or the
/// trylock failed, so a high share of them is the thundering herd every CPU
/// converging on the same most-backlogged wheel.
fn print_oppor_attempt_counters(events: &[HptsWheelEvent]) {
    let mut first: std::collections::HashMap<u64, (u64, u64)> = std::collections::HashMap::new();
    let mut last: std::collections::HashMap<u64, (u64, u64)> = std::collections::HashMap::new();
    for e in events {
        if let HptsWheelPayload::OpporSample {
            oppor_runs,
            oppor_noop,
            ..
        } = &e.payload
        {
            let v = (*oppor_runs, *oppor_noop);
            first.entry(e.session_id).or_insert(v);
            last.insert(e.session_id, v);
        }
    }
    let (mut runs, mut noop) = (0u64, 0u64);
    for (sid, (lr, ln)) in &last {
        let (fr, fn_) = first[sid];
        runs += lr.saturating_sub(fr);
        noop += ln.saturating_sub(fn_);
    }
    let attempts = runs + noop;
    if attempts == 0 {
        return;
    }
    println!();
    println!("hpts opportunistic (lro-driven) attempts, capture-wide");
    println!("  ran a wheel     {runs:>16}");
    println!(
        "  found nothing   {noop:>16}  ({:.1}% of {attempts} attempts)",
        100.0 * noop as f64 / attempts as f64
    );
}

/// Cross-check the "HPTS silently skips a slot" hypothesis directly
/// against the `PROCESSING` (id 10) event stream: for consecutive
/// `PROCESSING` runs on the same wheel instance, does the next run's
/// `prev_slot` exactly equal the previous run's `cur_slot`? A mismatch
/// means some slots were never covered by *any* processing run -- they
/// were silently skipped -- independent of whether `NO_RUN_SLOT_SKIP`
/// (id 15) instrumentation happened to be armed for that code path.
/// Checks both `session_id` (the wheel's own eventlog identity) and
/// `cpu` as the grouping key, since it isn't a priori obvious which one
/// uniquely identifies "the same wheel" in this capture, and
/// cross-references each mismatch against `NO_RUN_SLOT_SKIP` /
/// `INSERT_ARC_VIOLATION` events from the same session to help
/// attribute the cause.
pub fn cmd_hpts_gaps(
    events: &[HptsWheelEvent],
    capture_start: u64,
    exclude_last_secs: Option<f64>,
) {
    #[derive(Clone, Copy)]
    struct Proc {
        ts: u64,
        cpu: u16,
        session_id: u64,
        prev_slot: u32,
        cur_slot: u32,
    }
    let mut procs: Vec<Proc> = events
        .iter()
        .filter_map(|e| match &e.payload {
            HptsWheelPayload::Processing {
                prev_slot,
                cur_slot,
                ..
            } => Some(Proc {
                ts: e.ts,
                cpu: e.cpu,
                session_id: e.session_id,
                prev_slot: *prev_slot,
                cur_slot: *cur_slot,
            }),
            _ => None,
        })
        .collect();

    if procs.is_empty() {
        println!("No PROCESSING events in this capture -- see `hptsevents` for what is present.");
        return;
    }
    procs.sort_by_key(|p| p.ts);

    let capture_end = procs.last().map(|p| p.ts).unwrap_or(capture_start);
    if let Some(secs) = exclude_last_secs {
        let cutoff = capture_end.saturating_sub((secs * 1e6) as u64);
        let before = procs.len();
        procs.retain(|p| p.ts < cutoff);
        println!(
            "Excluding last {secs:.1}s of capture (ts >= {:.3}s): dropped {} of {before} \
             PROCESSING events, {} remain.\n",
            (cutoff.saturating_sub(capture_start)) as f64 / 1e6,
            before - procs.len(),
            procs.len()
        );
        if procs.is_empty() {
            println!("No PROCESSING events remain after exclusion.");
            return;
        }
    }

    // NO_RUN_SLOT_SKIP events, grouped by session and sorted by ts, so
    // each continuity mismatch can be matched against a self-reported
    // skip that happened in the same window on the same wheel.
    let mut skips: std::collections::BTreeMap<u64, Vec<(u64, u32, u32)>> =
        std::collections::BTreeMap::new();
    for e in events {
        if let HptsWheelPayload::NoRunSlotSkip {
            prev_slot,
            new_prev_slot,
            ..
        } = &e.payload
        {
            skips
                .entry(e.session_id)
                .or_default()
                .push((e.ts, *prev_slot, *new_prev_slot));
        }
    }
    for v in skips.values_mut() {
        v.sort_by_key(|(ts, ..)| *ts);
    }

    struct Gap {
        session_id: u64,
        cpu_before: u16,
        cpu_after: u16,
        ts_before: u64,
        ts_after: u64,
        cur_slot_before: u32,
        prev_slot_after: u32,
        matched_skip: bool,
    }

    // `procs` is globally ts-sorted, so appending in iteration order
    // keeps each per-key bucket ts-sorted too -- no per-group re-sort.
    let group_and_check = |key_by_cpu: bool| -> (usize, usize, Vec<Gap>) {
        let mut by_key: std::collections::BTreeMap<u64, Vec<Proc>> =
            std::collections::BTreeMap::new();
        for p in &procs {
            let key = if key_by_cpu {
                p.cpu as u64
            } else {
                p.session_id
            };
            by_key.entry(key).or_default().push(*p);
        }
        let mut total_pairs = 0usize;
        let mut mismatches = 0usize;
        let mut gaps = Vec::new();
        for group in by_key.values() {
            for w in group.windows(2) {
                let (a, b) = (w[0], w[1]);
                total_pairs += 1;
                if a.cur_slot != b.prev_slot {
                    mismatches += 1;
                    let matched_skip = !key_by_cpu
                        && skips
                            .get(&a.session_id)
                            .map(|v| {
                                v.iter().any(|(ts, ps, nps)| {
                                    *ts >= a.ts
                                        && *ts <= b.ts
                                        && *ps == a.cur_slot
                                        && *nps == b.prev_slot
                                })
                            })
                            .unwrap_or(false);
                    gaps.push(Gap {
                        session_id: a.session_id,
                        cpu_before: a.cpu,
                        cpu_after: b.cpu,
                        ts_before: a.ts,
                        ts_after: b.ts,
                        cur_slot_before: a.cur_slot,
                        prev_slot_after: b.prev_slot,
                        matched_skip,
                    });
                }
            }
        }
        (total_pairs, mismatches, gaps)
    };

    let (pairs_by_session, mismatches_by_session, mut gaps_by_session) = group_and_check(false);
    let (pairs_by_cpu, mismatches_by_cpu, _gaps_by_cpu) = group_and_check(true);

    println!(
        "=== PROCESSING slot continuity ({} PROCESSING events) ===",
        procs.len()
    );
    println!(
        "  grouped by session_id (hpts instance): {mismatches_by_session}/{pairs_by_session} \
         consecutive pairs do NOT line up (next.prev_slot != prev.cur_slot)"
    );
    println!(
        "  grouped by cpu:                        {mismatches_by_cpu}/{pairs_by_cpu} \
         consecutive pairs do NOT line up"
    );
    println!();

    if gaps_by_session.is_empty() {
        println!(
            "Every PROCESSING run's prev_slot lines up exactly with the previous run's \
             cur_slot (grouped by session_id) -- no skipped slots detected via this signal."
        );
        return;
    }

    gaps_by_session.sort_by_key(|g| {
        std::cmp::Reverse((g.prev_slot_after as i64 - g.cur_slot_before as i64).abs())
    });
    let matched_count = gaps_by_session.iter().filter(|g| g.matched_skip).count();
    let small_unmatched = gaps_by_session
        .iter()
        .filter(|g| {
            !g.matched_skip && (g.prev_slot_after as i64 - g.cur_slot_before as i64).abs() <= 3
        })
        .count();
    let big_unmatched = gaps_by_session.len() - matched_count - small_unmatched;
    println!(
        "  {matched_count} match a NO_RUN_SLOT_SKIP event on the same session in the same \
         window (the known insert-race fast-path bug)"
    );
    println!(
        "  {small_unmatched} are small (<=3 slots) but unmatched (no NO_RUN_SLOT_SKIP recorded \
         -- likely the same bug on a code path not covered by that instrumentation, or a \
         benign 1-slot boundary race)"
    );
    println!("  {big_unmatched} are large (>3 slots) and unmatched -- see the burst below");
    println!();

    // One-second timeline buckets: how many distinct wheel instances saw a
    // continuity gap in each second of the capture. Isolated (all-1s)
    // gaps vs. bursts hitting many instances in the same window look very
    // different here and help distinguish per-connection races from a
    // machine-wide stall.
    let mut by_second: std::collections::BTreeMap<u64, usize> = std::collections::BTreeMap::new();
    for g in &gaps_by_session {
        let sec = g.ts_before.saturating_sub(capture_start) / 1_000_000;
        *by_second.entry(sec).or_default() += 1;
    }
    println!("=== gaps per second of capture ===");
    for (sec, count) in &by_second {
        println!("  t={sec:>4}s: {count}");
    }
    println!();

    println!("=== continuity gaps (grouped by session_id), worst first ===");
    println!(
        "{:>18} {:>10} {:>10} {:>6} {:>6} {:>10} {:>10} {:>10} {:>8}",
        "session_id",
        "t_before",
        "t_after",
        "cpuB",
        "cpuA",
        "cur(A)",
        "prev(B)",
        "gap_slots",
        "no_run?"
    );
    println!("{}", "-".repeat(110));
    for g in gaps_by_session.iter() {
        let tb = (g.ts_before.saturating_sub(capture_start)) as f64 / 1e6;
        let ta = (g.ts_after.saturating_sub(capture_start)) as f64 / 1e6;
        let gap = g.prev_slot_after as i64 - g.cur_slot_before as i64;
        println!(
            "{:>#18x} {tb:>10.3} {ta:>10.3} {:>6} {:>6} {:>10} {:>10} {gap:>10} {:>8}",
            g.session_id,
            g.cpu_before,
            g.cpu_after,
            g.cur_slot_before,
            g.prev_slot_after,
            if g.matched_skip { "yes" } else { "no" },
        );
    }
}

#[cfg(test)]
mod hist_diff_tests {
    use super::diff_cumulative_hist;

    #[test]
    fn first_sample_per_instance_is_a_baseline_not_a_delta() {
        // session 0's first sample already shows 1_000_000 in bucket 0
        // (hours of pre-capture history) -- it must contribute 0, not
        // 1_000_000.
        let samples: Vec<(u64, Vec<u64>)> =
            vec![(0, vec![1_000_000, 500]), (0, vec![1_000_010, 505])];
        let (nbuckets, total) =
            diff_cumulative_hist(samples.iter().map(|(sid, b)| (*sid, b.as_slice())));
        assert_eq!(nbuckets, 2);
        assert_eq!(total, vec![10, 5]);
    }

    #[test]
    fn deltas_are_summed_across_multiple_instances() {
        let samples: Vec<(u64, Vec<u64>)> = vec![
            (0, vec![100]),
            (1, vec![200]),
            (0, vec![110]),
            (1, vec![225]),
        ];
        let (nbuckets, total) =
            diff_cumulative_hist(samples.iter().map(|(sid, b)| (*sid, b.as_slice())));
        assert_eq!(nbuckets, 1);
        assert_eq!(total, vec![10 + 25]);
    }

    #[test]
    fn counter_drop_is_treated_as_a_reset() {
        let samples: Vec<(u64, Vec<u64>)> = vec![(0, vec![100]), (0, vec![150]), (0, vec![20])];
        let (_, total) = diff_cumulative_hist(samples.iter().map(|(sid, b)| (*sid, b.as_slice())));
        // 150-100=50, then a drop to 20 is a reset: delta = 20 (not negative).
        assert_eq!(total, vec![50 + 20]);
    }

    #[test]
    fn same_cpu_different_session_ids_do_not_cross_contaminate() {
        // Two hpts instances opportunistically logged from the same cpu
        // in quick succession (the real-world scenario this key choice
        // exists for) must be diffed independently, not merged.
        let samples: Vec<(u64, Vec<u64>)> = vec![
            (0xaaaa, vec![4_000_000]),
            (0xbbbb, vec![9_000_000]),
            (0xaaaa, vec![4_000_050]),
            (0xbbbb, vec![9_000_030]),
        ];
        let (_, total) = diff_cumulative_hist(samples.iter().map(|(sid, b)| (*sid, b.as_slice())));
        assert_eq!(total, vec![50 + 30]);
    }

    #[test]
    fn empty_input_yields_zero_buckets() {
        let (nbuckets, total) = diff_cumulative_hist(std::iter::empty());
        assert_eq!(nbuckets, 0);
        assert!(total.is_empty());
    }
}
