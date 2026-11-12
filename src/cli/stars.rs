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

/// Dump `STARS_AGG_SAMPLE` (hpts provider, event 16) as a raw timeline: one
/// row per sample with the fields the System-tab GUI plots (pressure,
/// tokens, slot-gate budget/peak-burst, deny/admit deltas) plus the
/// configured `health_limit_bps` so a required-budget-per-slot figure can be
/// cross-checked against `slot_gate_budget_bytes` directly from the CLI,
/// without going through the GUI. Also includes the pacing-rollover backlog
/// (instantaneous queue depth, plus divert/admit deltas) so a low slot-gate
/// deny rate can be checked against whether connections are still piling up
/// in rollover rather than being denied outright.
pub fn cmd_stars_agg(samples: &[StarsAggSample], capture_start: u64) {
    if samples.is_empty() {
        println!("No STARS_AGG_SAMPLE events in this capture.");
        return;
    }
    println!("=== STARS_AGG_SAMPLE ({} samples) ===", samples.len());
    println!(
        "{:>10} {:>6} {:>12} {:>10} {:>14} {:>12} {:>10} {:>10} {:>12} {:>12} {:>14} {:>10} {:>10} {:>10} {:>10} {:>9} {:>9} {:>8} {:>7}",
        "t(s)",
        "press",
        "tokens",
        "per_slot",
        "gate_budget",
        "peak_burst",
        "denies",
        "admits",
        "denied_B",
        "admitB_B",
        "meter_obytes",
        "health_bps",
        "min_bps",
        "norate_d",
        "roll_q",
        "roll_div",
        "roll_adm",
        "headrm%",
        "rxt_d",
    );
    println!("{}", "-".repeat(215));
    let mut prev: Option<&StarsAggSample> = None;
    let mut total_norate_delta: u64 = 0;
    let mut total_denied_bytes: u64 = 0;
    let mut total_admitted_bytes: u64 = 0;
    let mut total_rollover_diverts: u64 = 0;
    let mut total_rollover_admits: u64 = 0;
    let mut max_rollover_queue_depth: u64 = 0;
    let mut min_headroom_pct: u64 = 100;
    let mut total_rxt_delta: u64 = 0;
    for s in samples {
        let t = (s.ts.saturating_sub(capture_start)) as f64 / 1e6;
        let (
            d_denies,
            d_admits,
            d_norate,
            d_denied_bytes,
            d_admitted_bytes,
            d_roll_div,
            d_roll_adm,
            d_rxt,
        ) = match prev {
            Some(p) => (
                s.slot_gate_denies.saturating_sub(p.slot_gate_denies),
                s.slot_gate_admits.saturating_sub(p.slot_gate_admits),
                s.synthetic_norate_hits
                    .saturating_sub(p.synthetic_norate_hits),
                s.slot_gate_denied_bytes
                    .saturating_sub(p.slot_gate_denied_bytes),
                s.slot_gate_admitted_bytes
                    .saturating_sub(p.slot_gate_admitted_bytes),
                s.rollover_diverts.saturating_sub(p.rollover_diverts),
                s.rollover_admits.saturating_sub(p.rollover_admits),
                s.rxt_no_pace_hits.saturating_sub(p.rxt_no_pace_hits),
            ),
            None => (
                s.slot_gate_denies,
                s.slot_gate_admits,
                0,
                s.slot_gate_denied_bytes,
                s.slot_gate_admitted_bytes,
                s.rollover_diverts,
                s.rollover_admits,
                0,
            ),
        };
        total_norate_delta += d_norate;
        total_rxt_delta += d_rxt;
        total_denied_bytes += d_denied_bytes;
        total_admitted_bytes += d_admitted_bytes;
        total_rollover_diverts += d_roll_div;
        total_rollover_admits += d_roll_adm;
        max_rollover_queue_depth = max_rollover_queue_depth.max(s.rollover_queue_depth);
        min_headroom_pct = min_headroom_pct.min(s.slot_gate_headroom_pct);
        println!(
            "{t:>10.3} {:>6} {:>12} {:>10} {:>14} {:>12} {:>10} {:>10} {:>12} {:>12} {:>14} {:>10} {:>10} {:>10} {:>10} {:>9} {:>9} {:>8} {:>7}",
            s.pressure,
            s.tokens,
            s.per_slot_bytes,
            s.slot_gate_budget_bytes,
            s.slot_bytes_peak,
            d_denies,
            d_admits,
            d_denied_bytes,
            d_admitted_bytes,
            s.meter_obytes,
            s.health_limit_bps,
            s.min_rate_bps,
            d_norate,
            s.rollover_queue_depth,
            d_roll_div,
            d_roll_adm,
            s.slot_gate_headroom_pct,
            d_rxt,
        );
        prev = Some(s);
    }
    println!();
    println!(
        "rollover: max queue depth (roll_q) {max_rollover_queue_depth} across the capture; \
         {total_rollover_diverts} total diverts, {total_rollover_admits} total admits over the \
         whole run. NOTE: diverts - admits is NOT a backlog estimate -- a single connection can \
         cycle through divert/admit repeatedly (denied again next slot after being admitted into \
         the batch), so both counters accumulate with churn, not just first-time entry. roll_q \
         (the instantaneous per-sample column above) is the only field that reflects actual \
         backlog depth at a point in time. Zero roll_q throughout means either no rollover \
         backlog built up, or this capture predates the rollover_* fields.",
    );
    let total_bytes = total_denied_bytes + total_admitted_bytes;
    println!(
        "slot-gate byte totals: denied={total_denied_bytes} admitted={total_admitted_bytes} \
         ({:.2}% of attempted bytes denied -- compare against the send-attempt-count deny%% \
         above; a gate that denies mostly large sends will show a much higher byte-deny%% than \
         count-deny%%)",
        if total_bytes > 0 {
            100.0 * total_denied_bytes as f64 / total_bytes as f64
        } else {
            0.0
        },
    );
    println!(
        "slot-gate headroom (headrm%%): min {min_headroom_pct}% over the capture (100% = auto \
         budget applied unreduced). Below 100% means IFCOUNTER_OQDROPS moved on the metered \
         interface and the self-tuning AIMD backed the budget off; 100% throughout means either \
         no drops were seen, headroom is disabled (stars_slot_gate_headroom_enable=0), or this \
         capture predates the headroom_pct field.",
    );
    let first_norate = samples
        .first()
        .map(|s| s.synthetic_norate_hits)
        .unwrap_or(0);
    let last_norate = samples.last().map(|s| s.synthetic_norate_hits).unwrap_or(0);
    println!(
        "synthetic_norate_hits: {} -> {} over the capture ({} total sends used the no-RTT-estimate \
         synthetic-rate path, ~{:.1}/sample avg)",
        first_norate,
        last_norate,
        total_norate_delta,
        total_norate_delta as f64 / samples.len().max(1) as f64,
    );
    let first_rxt = samples.first().map(|s| s.rxt_no_pace_hits).unwrap_or(0);
    let last_rxt = samples.last().map(|s| s.rxt_no_pace_hits).unwrap_or(0);
    if total_rxt_delta > 0 {
        println!(
            "rxt_no_pace_hits: {} -> {} over the capture ({} total sends were handed back for \
             inline transmission during RTO recovery, ~{:.1}/sample avg) -- the \
             stars_rxt_no_pace bypass was armed and firing.",
            first_rxt,
            last_rxt,
            total_rxt_delta,
            total_rxt_delta as f64 / samples.len().max(1) as f64,
        );
    } else {
        println!(
            "rxt_no_pace_hits: 0 over the capture -- the stars_rxt_no_pace bypass never fired. \
             Ambiguous on its own: the knob may be off, no connection may have entered RTO \
             recovery, or the capture may predate this field. Check the knob's state before \
             reading any A/B against it as a null result.",
        );
    }

    // Cross-check: bytes/slot required to sustain health_limit_bps at a
    // nominal 64us slot, vs. the actual configured slot-gate budget --
    // the slot gate is sized from the metered interface's *baudrate*, not
    // from health_limit_bps, so these can diverge (see tcp_hpts.c's
    // hpts_stars_slot_budget_from_baud()). A budget close to (or below) the
    // required figure leaves little/no headroom for the burstiness the
    // slot gate is supposed to tolerate.
    let last = samples.last().unwrap();
    if last.health_limit_bps > 0 {
        let required_per_slot_64us = (last.health_limit_bps / 8) as f64 * 64.0 / 1_000_000.0;
        println!(
            "Last sample: health_limit_bps={} => ~{:.0} bytes needed per 64us slot to sustain it; \
             configured slot_gate_budget_bytes={} ({:.1}x headroom)",
            last.health_limit_bps,
            required_per_slot_64us,
            last.slot_gate_budget_bytes,
            last.slot_gate_budget_bytes as f64 / required_per_slot_64us.max(1.0),
        );
    }
}
