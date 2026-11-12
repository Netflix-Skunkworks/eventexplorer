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

//! Decode coverage for `STARS_PACE` (id 604): the per-connection
//! diagnostic logging the natural (pre-squeeze) pacing rate alongside
//! `hpts_stars_pace_rate()`'s result and the global pressure that produced
//! it. See `support::synthetic_elog_stars_pace` for the fixture's exact
//! layout.

mod support;

use event_explorer::elog_parser::*;
use support::{synthetic_elog_stars_pace, SESSION_A};

#[test]
fn stars_pace_decodes_squeezed_and_unsqueezed_shapes() {
    let bytes = synthetic_elog_stars_pace();
    let progress = LoadProgress::default();
    let (sessions, ..) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse STARS_PACE fixture");

    let events = sessions.get(&SESSION_A).expect("session A present");
    let stars_pace: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::StarsPace {
                natural_bps,
                scaled_bps,
                pressure,
                len,
            } => Some((*natural_bps, *scaled_bps, *pressure, *len)),
            _ => None,
        })
        .collect();
    assert_eq!(stars_pace.len(), 2, "both STARS_PACE events should decode");

    // Common-return scaler: squeezed below natural.
    assert_eq!(stars_pace[0], (100_000_000, 25_000_000, 768, 1_460));
    // No-rate-estimate placeholder: natural_bps == 1<<24, unsqueezed
    // (pressure 0 here) since that's the synthetic rate the kernel feeds in
    // when it has no real estimate for this connection yet.
    assert_eq!(stars_pace[1], (1 << 24, 1 << 24, 0, 2_920));
}

#[test]
fn stars_pace_has_a_hardcoded_decoder() {
    assert!(
        tcp_has_hardcoded_decoder(EVT_STARS_PACE),
        "EVT_STARS_PACE must be in the hardcoded-decoder allowlist now that \
         payload.rs has an arm for it, or core.rs's routing guard will \
         double-decode it generically"
    );
}
