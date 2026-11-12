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

mod support;

use event_explorer::analysis::build_mem_series;
use event_explorer::elog_parser::*;
use support::{synthetic_elog_mem, CAPTURE_START};

const GIB: u64 = 1 << 30;

#[test]
fn mem_provider_decodes_info_and_samples() {
    let bytes = synthetic_elog_mem();
    let progress = LoadProgress::default();
    let (_s, _n, _p, _c, _i, _t, _e, _ng, file_info, _pr, schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse mem fixture");

    assert_eq!(file_info.skipped_other_providers, 0);
    assert!(
        schema.generic_events.is_empty(),
        "mem must not fall back to generic"
    );

    let mem = &schema.mem;
    assert_eq!(
        mem.infos,
        vec![MemInfo {
            ts: CAPTURE_START,
            page_size: 16384,
            ndomains: 2,
            sample_period_us: 1_000_000,
            physmem: 64 * GIB,
            usable: 62 * GIB,
            swap_total: 8 * GIB,
        }]
    );
    assert_eq!(mem.samples.len(), 2);
    let s = mem.samples[1];
    assert_eq!(
        (s.free, s.active, s.inactive, s.laundry, s.wired),
        (9 * GIB, 21 * GIB, 25 * GIB, GIB, 6 * GIB)
    );
    assert_eq!(
        (s.free_target, s.free_min, s.free_severe),
        (GIB, GIB / 2, GIB / 4)
    );
    assert_eq!(s.swap_used, GIB);

    let series = build_mem_series(mem, file_info.capture_start);
    assert_eq!(
        series.states[0],
        vec![[0.001, 10.0], [1.001, 9.0]],
        "free, GiB"
    );
    assert_eq!(series.usable_gib, 62.0);
    assert!(series.has_swap());
}
