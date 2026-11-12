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

//! Decode coverage for `STARS_AGG_SAMPLE` (id 16) across the four payload
//! widths it has had. The record grows by appending u64 fields, so every
//! older capture on disk is a valid short payload the decoder still has to
//! read. A missed width silently yields zeroed telemetry that reads as a
//! real measurement rather than as an error.

mod support;

use event_explorer::elog_parser::*;
use support::{synthetic_elog_stars_agg_truncations, STARS_AGG_FIELD_VALUES};

#[test]
fn stars_agg_decodes_every_historical_payload_width() {
    let bytes = synthetic_elog_stars_agg_truncations();
    let progress = LoadProgress::default();
    let (_, stars_agg, ..) = parse_elog_bytes_with_progress(&bytes, &progress)
        .expect("parse STARS_AGG_SAMPLE truncation fixture");

    assert_eq!(
        stars_agg.len(),
        4,
        "all four payload widths (128/152/160/168) must decode, not just the current one"
    );

    let v = STARS_AGG_FIELD_VALUES;
    // The 16 fields present at every width, checked on the shortest
    // payload: if any offset were wrong these would pick up a neighbour.
    let s = &stars_agg[0];
    assert_eq!(s.pressure, v[0]);
    assert_eq!(s.tokens, v[1] as i64);
    assert_eq!(s.per_slot_bytes, v[2]);
    assert_eq!(s.slot_gate_budget_bytes, v[3]);
    assert_eq!(s.slot_bytes_peak, v[4]);
    assert_eq!(s.slot_gate_denies, v[5]);
    assert_eq!(s.slot_gate_admits, v[6]);
    assert_eq!(s.slot_gate_denied_bytes, v[7]);
    assert_eq!(s.slot_gate_admitted_bytes, v[8]);
    assert_eq!(s.meter_obytes, v[9]);
    assert_eq!(s.pressure_sum, v[10]);
    assert_eq!(s.pressure_ticks, v[11]);
    assert_eq!(s.synthetic_norate_hits, v[12]);
    assert_eq!(s.slot_gate_enable, v[13]);
    assert_eq!(s.health_limit_bps, v[14]);
    assert_eq!(s.min_rate_bps, v[15]);

    // 128 bytes: everything appended later takes its no-op default.
    assert_eq!(s.rollover_queue_depth, 0);
    assert_eq!(s.rollover_diverts, 0);
    assert_eq!(s.rollover_admits, 0);
    assert_eq!(s.slot_gate_headroom_pct, 100, "100% = budget unreduced");
    assert_eq!(s.rxt_no_pace_hits, 0);

    // 152 bytes: rollover_* present, headroom and rxt_no_pace defaulted.
    let s = &stars_agg[1];
    assert_eq!(s.rollover_queue_depth, v[16]);
    assert_eq!(s.rollover_diverts, v[17]);
    assert_eq!(s.rollover_admits, v[18]);
    assert_eq!(s.slot_gate_headroom_pct, 100);
    assert_eq!(s.rxt_no_pace_hits, 0);

    // 160 bytes: headroom present, rxt_no_pace still defaulted.
    let s = &stars_agg[2];
    assert_eq!(s.rollover_admits, v[18]);
    assert_eq!(s.slot_gate_headroom_pct, v[19]);
    assert_eq!(s.rxt_no_pace_hits, 0);

    // 168 bytes: the current width, every field carried.
    let s = &stars_agg[3];
    assert_eq!(s.slot_gate_headroom_pct, v[19]);
    assert_eq!(
        s.rxt_no_pace_hits, v[20],
        "rxt_no_pace_hits must be read at offset 160, not defaulted to 0 -- \
         a 0 here is indistinguishable from the bypass never firing"
    );
}
