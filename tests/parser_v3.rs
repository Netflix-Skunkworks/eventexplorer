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

//! Integration coverage for EventLog v3 (header counter array): the fixed
//! header shrinks from 40 to 36 bytes and the old single `dropped_events`
//! field is replaced by `counter_count` + that many trailing `u64`
//! counters. See `support::synthetic_elog_v3` for the fixture's exact
//! layout.

mod support;

use event_explorer::elog_parser::*;
use support::synthetic_elog_v3;

#[test]
fn v3_header_counters_decode_into_file_info() {
    let bytes = synthetic_elog_v3(&[42, 7, 3, 1]);
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, _ella, _nginx, file_info, providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse v3 fixture");

    assert_eq!(file_info.version, 3);
    assert_eq!(
        file_info.dropped, 42,
        "index 0 = ELOG_COUNTER_KERNEL_DROPPED"
    );
    assert_eq!(file_info.user_dropped_full, 7);
    assert_eq!(file_info.user_dropped_prejoin, 3);
    assert_eq!(file_info.user_dropped_late, 1);
    assert_eq!(providers.len(), 4, "provider table should still resolve");
}

#[test]
fn v3_shorter_counter_array_reads_missing_trailing_indices_as_zero() {
    // Append-only design: an older-minor v3 writer emitting fewer
    // counters than this reader knows about must not desync the rest of
    // the header/provider-table offsets, and unknown trailing indices
    // read as 0 rather than erroring.
    let bytes = synthetic_elog_v3(&[99]);
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, _ella, _nginx, file_info, providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse short-counter fixture");

    assert_eq!(file_info.dropped, 99);
    assert_eq!(file_info.user_dropped_full, 0);
    assert_eq!(file_info.user_dropped_prejoin, 0);
    assert_eq!(file_info.user_dropped_late, 0);
    assert_eq!(providers.len(), 4);
}

#[test]
fn v3_zero_counters_is_valid() {
    let bytes = synthetic_elog_v3(&[]);
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, _ella, _nginx, file_info, ..) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse zero-counter fixture");

    assert_eq!(file_info.dropped, 0);
    assert_eq!(file_info.user_dropped_full, 0);
}

#[test]
fn v3_truncated_counter_array_is_rejected() {
    let mut bytes = synthetic_elog_v3(&[1, 2, 3, 4]);
    // Claim counter_count = 4 (unchanged) but chop off the file partway
    // through the counter array -- must be a hard parse error, not a
    // silent short-read that desyncs every offset after it.
    bytes.truncate(36 + 8); // header + only 1 of 4 claimed counters
    let progress = LoadProgress::default();
    let err = parse_elog_bytes_with_progress(&bytes, &progress)
        .expect_err("truncated v3 header counters must not parse");
    assert!(
        err.to_string().contains("header counters"),
        "unexpected error: {err}"
    );
}

#[test]
fn v3_absurd_counter_count_is_rejected_outright() {
    // A corrupt/hostile counter_count must not be trusted as a read size
    // (it could otherwise desync every offset after it onto whatever
    // event bytes happen to follow) -- exceeding the sane-max bound is a
    // hard error, matching the producer's own `errx` on the same check.
    let mut bytes = synthetic_elog_v3(&[1, 2, 3, 4]);
    let huge = (u32::MAX / 2).to_le_bytes();
    bytes[32..36].copy_from_slice(&huge);
    let progress = LoadProgress::default();
    let err = parse_elog_bytes_with_progress(&bytes, &progress)
        .expect_err("an absurd counter_count must not parse successfully");
    assert!(
        err.to_string().contains("Invalid header counter count"),
        "unexpected error: {err}"
    );
}
