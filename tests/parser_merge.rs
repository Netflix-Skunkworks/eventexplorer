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

//! Integration coverage for loading a rolled capture set as one
//! continuous capture (`parse_elogs` / `merge_captures`).
//!
//! The fixture set is three files an hour apart, built by
//! `support::synthetic_elog_roll`: one session runs through all three,
//! each file also has a session of its own, and the global `hpts` / `if`
//! streams plus a boundary identity re-dump exercise the append and
//! upsert halves of the merge.

mod support;

use event_explorer::elog_parser::*;
use support::{
    synthetic_elog_roll, write_temp_elog, CAPTURE_START, HPTS_PID, IF_PID, SESSION_A, SESSION_B,
    TCP_PID,
};

const HOUR_US: u64 = 3_600_000_000;

/// The three-file rolled set, as paths in argv order.
fn rolled_set(tag: &str) -> Vec<String> {
    (0..3u64)
        .map(|roll| {
            let bytes = synthetic_elog_roll(roll, roll * HOUR_US, "tcp");
            write_temp_elog(&format!("merge_{tag}_{roll}.elog"), &bytes)
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn parse_one(path: &str) -> ParsedCapture {
    parse_elog(path).expect("parse single fixture")
}

#[test]
fn rolled_set_merges_into_one_timeline() {
    let paths = rolled_set("timeline");
    let (
        sessions,
        agg,
        _pace,
        _cpu,
        if_profile,
        _tcp,
        _ella,
        _nginx,
        file_info,
        providers,
        _schema,
    ) = parse_elogs(&paths).expect("merge rolled set");

    // Origin comes from the earliest file; the span reaches the last.
    assert_eq!(file_info.capture_start, CAPTURE_START);
    assert_eq!(file_info.start_utc_us, support::START_UTC_US);
    assert!(
        file_info.capture_end > CAPTURE_START + 2 * HOUR_US,
        "merged capture must extend into the last file, got {}",
        file_info.capture_end
    );

    // Every file is named, in time order.
    assert_eq!(file_info.sources.len(), 3);
    let names: Vec<&str> = file_info.sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "merge_timeline_0.elog",
            "merge_timeline_1.elog",
            "merge_timeline_2.elog"
        ]
    );
    assert_eq!(file_info.sources[1].capture_start, CAPTURE_START + HOUR_US);

    // Header totals are the sum of the parts.
    let per_file: Vec<ParsedCapture> = paths.iter().map(|p| parse_one(p)).collect();
    let expected_events: u64 = per_file.iter().map(|c| c.8.event_count).sum();
    let expected_parsed: usize = per_file.iter().map(|c| c.8.parsed).sum();
    assert_eq!(file_info.event_count, expected_events);
    assert_eq!(file_info.parsed, expected_parsed);

    // The shared session is one series spanning all three files; the
    // per-file sessions come along as their own entries.
    assert_eq!(sessions.len(), 4, "1 shared + 3 per-file sessions");
    let shared = &sessions[&SESSION_A];
    let per_file_shared: usize = per_file.iter().map(|c| c.0[&SESSION_A].len()).sum();
    assert_eq!(shared.len(), per_file_shared);
    assert!(
        shared.is_sorted_by_key(|e| e.ts),
        "merged session events must stay in timestamp order"
    );
    assert!(shared.last().unwrap().ts > CAPTURE_START + 2 * HOUR_US);
    for roll in 0..3u64 {
        assert!(
            sessions.contains_key(&(SESSION_B + roll)),
            "session unique to file {roll} must survive the merge"
        );
    }

    // Global streams append and stay ordered.
    let expected_agg: usize = per_file.iter().map(|c| c.1.len()).sum();
    assert_eq!(agg.len(), expected_agg);
    assert!(agg.is_sorted_by_key(|s| s.ts));
    let expected_if: usize = per_file.iter().map(|c| c.4.samples.len()).sum();
    assert_eq!(if_profile.samples.len(), expected_if);
    assert!(if_profile.samples.is_sorted_by_key(|s| s.ts));

    // The boundary identity re-dump upserts rather than duplicating.
    assert_eq!(if_profile.interfaces.len(), 1);
    assert_eq!(if_profile.name_for(1), "cxl0");

    // Provider rows fold: counts sum, spans widen.
    assert_eq!(providers.len(), 3);
    for provider in &providers {
        let expected: u64 = per_file
            .iter()
            .flat_map(|c| c.9.iter())
            .filter(|p| p.id == provider.id)
            .map(|p| p.event_count)
            .sum();
        assert_eq!(
            provider.event_count, expected,
            "provider {} event_count",
            provider.name
        );
    }
    let tcp = providers.iter().find(|p| p.id == TCP_PID).unwrap();
    assert_eq!(tcp.first_ts, Some(CAPTURE_START));
    assert!(tcp.last_ts.unwrap() > CAPTURE_START + 2 * HOUR_US);
    assert!(providers.iter().any(|p| p.id == HPTS_PID));
    assert!(providers.iter().any(|p| p.id == IF_PID));
}

/// Files may be listed in any order — the merge sorts them onto the time
/// axis itself, so a shell glob's order (or the file dialog's selection
/// order) doesn't matter.
#[test]
fn file_order_does_not_matter() {
    let paths = rolled_set("order");
    let forward = parse_elogs(&paths).expect("merge forward");
    let mut shuffled = vec![paths[2].clone(), paths[0].clone(), paths[1].clone()];
    let reversed = parse_elogs(&shuffled).expect("merge shuffled");

    assert_eq!(forward.8.capture_start, reversed.8.capture_start);
    assert_eq!(forward.8.capture_end, reversed.8.capture_end);
    assert_eq!(forward.8.event_count, reversed.8.event_count);
    assert_eq!(forward.0[&SESSION_A].len(), reversed.0[&SESSION_A].len());
    let forward_names: Vec<&str> = forward.8.sources.iter().map(|s| s.name.as_str()).collect();
    let reversed_names: Vec<&str> = reversed.8.sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(forward_names, reversed_names, "sources are time-ordered");

    // A repeated path is loaded once, so nothing is double-counted.
    shuffled.push(paths[0].clone());
    let with_dupe = parse_elogs(&shuffled).expect("merge with duplicate path");
    assert_eq!(with_dupe.8.event_count, forward.8.event_count);
    assert_eq!(with_dupe.8.sources.len(), 3);
}

/// A single path through the multi-file loader is the single-file load.
#[test]
fn single_file_is_unchanged() {
    let paths = rolled_set("single");
    let via_set = parse_elogs(&paths[..1]).expect("merge one file");
    let direct = parse_one(&paths[0]);

    assert_eq!(via_set.8.capture_start, direct.8.capture_start);
    assert_eq!(via_set.8.event_count, direct.8.event_count);
    assert_eq!(via_set.0.len(), direct.0.len());
    assert_eq!(via_set.8.sources.len(), 1);
}

/// Timestamps from either side of a reboot are numerically comparable
/// but unrelated, so a set whose wall-clock anchors disagree is refused
/// instead of being spliced into a bogus timeline.
#[test]
fn rejects_captures_from_different_boots() {
    let first = write_temp_elog("merge_boot_0.elog", &synthetic_elog_roll(0, 0, "tcp"));
    // Same monotonic span, wall clock a day off: a different boot.
    let mut rebooted = synthetic_elog_roll(1, HOUR_US, "tcp");
    let shifted = support::START_UTC_US + HOUR_US + 86_400_000_000;
    rebooted[16..24].copy_from_slice(&shifted.to_le_bytes());
    let second = write_temp_elog("merge_boot_1.elog", &rebooted);

    let paths = vec![
        first.to_string_lossy().into_owned(),
        second.to_string_lossy().into_owned(),
    ];
    let err = parse_elogs(&paths).expect_err("must refuse a cross-boot set");
    let msg = err.to_string();
    assert!(
        msg.contains(
            "These captures are from different boots of the same machine. \
             Open each file individually instead."
        ),
        "error should name the cause, got: {msg}"
    );
}

/// Provider ids are per-file, and the sample/event streams key on them,
/// so a set that numbers providers differently can't be merged by id.
#[test]
fn rejects_conflicting_provider_tables() {
    let first = write_temp_elog("merge_conflict_0.elog", &synthetic_elog_roll(0, 0, "tcp"));
    let second = write_temp_elog(
        "merge_conflict_1.elog",
        &synthetic_elog_roll(1, HOUR_US, "cpu"),
    );

    let paths = vec![
        first.to_string_lossy().into_owned(),
        second.to_string_lossy().into_owned(),
    ];
    let err = parse_elogs(&paths).expect_err("must refuse conflicting provider tables");
    let msg = err.to_string();
    assert!(
        msg.contains("not from the same elog run"),
        "error should name the cause, got: {msg}"
    );
    assert!(
        msg.contains("provider id"),
        "error should name the conflicting id, got: {msg}"
    );
}

/// Overlapping spans (a set assembled by hand, or captures from two
/// concurrent `elog` runs) still produce a correctly ordered timeline —
/// the merge re-sorts rather than trusting the file boundaries.
#[test]
fn interleaved_spans_are_reordered() {
    // Second file starts one tenth of the way into the first.
    let first = write_temp_elog("merge_overlap_0.elog", &synthetic_elog_roll(0, 0, "tcp"));
    let second = write_temp_elog(
        "merge_overlap_1.elog",
        &synthetic_elog_roll(1, 50_000, "tcp"),
    );
    let paths = vec![
        first.to_string_lossy().into_owned(),
        second.to_string_lossy().into_owned(),
    ];

    let (sessions, agg, _pace, _cpu, if_profile, _tcp, _ella, _nginx, _info, _providers, _schema) =
        parse_elogs(&paths).expect("merge overlapping set");

    assert!(
        sessions[&SESSION_A].is_sorted_by_key(|e| e.ts),
        "interleaved session events must be re-sorted"
    );
    assert!(agg.is_sorted_by_key(|s| s.ts));
    assert!(if_profile.samples.is_sorted_by_key(|s| s.ts));
}

/// `elog` has also written rolled sets whose later files repeat the
/// first file's header anchor verbatim instead of advancing it (see
/// "Drained ring backlog" in `docs/file-format.md`). The headers then
/// say all three files start at the same instant while the events say
/// they're an hour apart — order and validate off the events.
#[test]
fn repeated_header_anchor_still_orders_the_set() {
    let paths: Vec<String> = [2u64, 0, 1]
        .iter()
        .map(|&roll| {
            let bytes = support::synthetic_elog_roll_anchored(
                roll,
                roll * HOUR_US,
                CAPTURE_START,
                support::START_UTC_US,
            );
            write_temp_elog(&format!("merge_anchor_{roll}.elog"), &bytes)
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    let (sessions, _agg, _pace, _cpu, _if, _tcp, _ella, _nginx, info, _providers, _schema) =
        parse_elogs(&paths).expect("merge a set with repeated header anchors");

    assert_eq!(info.capture_start, CAPTURE_START);
    assert!(info.capture_end > CAPTURE_START + 2 * HOUR_US);
    let names: Vec<&str> = info.sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "merge_anchor_0.elog",
            "merge_anchor_1.elog",
            "merge_anchor_2.elog"
        ],
        "files are ordered by their events, not their identical headers"
    );
    let starts: Vec<u64> = info.sources.iter().map(|s| s.capture_start).collect();
    assert_eq!(
        starts,
        vec![
            CAPTURE_START,
            CAPTURE_START + HOUR_US,
            CAPTURE_START + 2 * HOUR_US
        ],
        "each source reports where its own events start"
    );
    assert!(sessions[&SESSION_A].is_sorted_by_key(|e| e.ts));
}

/// A first file that opened with drained ring backlog has its origin
/// trimmed forward, leaving its header and live anchors a provider's
/// whole uptime apart. That's one clock domain, not two, and the trimmed
/// origin is still the merged capture's origin.
#[test]
fn backlog_in_the_first_file_still_merges() {
    // The live capture opens a backlog span past the header origin, and
    // the later files roll off that live start.
    let live = support::BACKLOG_GAP_US;
    let paths: Vec<String> = (0..3u64)
        .map(|roll| {
            let bytes = match roll {
                0 => support::synthetic_elog_roll_with_backlog(0, live),
                _ => synthetic_elog_roll(roll, live + roll * HOUR_US, "tcp"),
            };
            write_temp_elog(&format!("merge_backlog_{roll}.elog"), &bytes)
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    let (_sessions, _agg, _pace, _cpu, _if, _tcp, _ella, _nginx, info, _providers, _schema) =
        parse_elogs(&paths).expect("merge a set whose first file has backlog");

    assert_eq!(
        info.capture_start,
        CAPTURE_START + live,
        "merged origin is the trimmed live start, not the backlog record"
    );
    assert_eq!(info.header_capture_start, CAPTURE_START);
    assert_eq!(info.backlog_events, 1);
    assert_eq!(info.sources.len(), 3);
    assert_eq!(info.sources[0].capture_start, CAPTURE_START + live);
    assert!(info.capture_end > CAPTURE_START + live + 2 * HOUR_US);
}

#[test]
fn merging_nothing_is_an_error() {
    assert!(merge_captures(Vec::new()).is_err());
    let empty: Vec<String> = Vec::new();
    assert!(parse_elogs(&empty).is_err());
}
