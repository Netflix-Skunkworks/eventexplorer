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

use event_explorer::elog_parser::*;
use support::{
    synthetic_elog_cpu_only_split_schema, synthetic_elog_if_sample_other_direct,
    synthetic_elog_if_sample_split,
};

/// A "cpu"-only capture using split-schema sample ids must still decode
/// its samples as samples, not as legacy-id-1 THREAD_CREATE records, even
/// though it registers no "symbols"/"process" provider to signal that.
#[test]
fn cpu_only_split_schema_samples_not_misdecoded_as_threads() {
    let bytes = synthetic_elog_cpu_only_split_schema(50);
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, cpu, _if, _tcp, _ella, _nginx, file_info, _providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse synthetic fixture");

    assert_eq!(
        cpu.samples.len(),
        50,
        "all 50 split-schema-id samples should decode as CPU_SAMPLE"
    );
    assert_eq!(
        cpu.threads.len(),
        0,
        "no sample should be misdecoded as a bogus THREAD_CREATE"
    );
    assert_eq!(
        file_info.skipped_other_providers, 0,
        "no event should fall through unmatched"
    );
}

/// `IF_SAMPLE_BYTES` + `IF_SAMPLE_PACKETS` (ids 21/22) must merge into one
/// `IfSample` per tick, in both arrival orders, and orphaned halves (no
/// matching counterpart) must still be flushed rather than dropped.
#[test]
fn if_sample_bytes_and_packets_merge_into_one_record() {
    let bytes = synthetic_elog_if_sample_split();
    let progress = LoadProgress::default();
    let (
        _sessions,
        _stars,
        _pace,
        _cpu,
        if_profile,
        _tcp,
        _ella,
        _nginx,
        _file_info,
        _providers,
        _schema,
    ) = parse_elog_bytes_with_progress(&bytes, &progress).expect("parse synthetic fixture");

    // 2 merged ticks for ifindex 1 + 1 packets-only for ifindex 2 + 1
    // orphaned bytes-only for ifindex 1 flushed at end-of-parse = 4.
    assert_eq!(if_profile.samples.len(), 4);

    let mut by_if1: Vec<&IfSample> = if_profile
        .samples
        .iter()
        .filter(|s| s.ifindex == 1)
        .collect();
    by_if1.sort_by_key(|s| s.ts);
    assert_eq!(by_if1.len(), 3, "2 merged ticks + 1 orphaned bytes-only");

    // Tick 1: fully merged, both bytes and packets halves present.
    let t1 = by_if1[0];
    assert_eq!((t1.ibytes, t1.obytes), (1000, 900));
    assert_eq!((t1.tcp_ibytes, t1.tcp_obytes), (600, 500));
    assert_eq!((t1.udp_ibytes, t1.udp_obytes), (300, 300));
    // Legacy (56/112-byte) payload shape carries no direct other_* field,
    // so this must fall back to the derive-by-subtraction formula.
    assert_eq!((t1.other_ibytes, t1.other_obytes), (100, 100));
    assert_eq!((t1.ipackets, t1.opackets), (10, 9));
    assert_eq!((t1.tcp_ipackets, t1.tcp_opackets), (6, 5));
    assert_eq!((t1.udp_ipackets, t1.udp_opackets), (3, 3));
    assert_eq!((t1.other_ipackets, t1.other_opackets), (1, 1));

    // Tick 2: cumulative counters advanced, still fully merged.
    let t2 = by_if1[1];
    assert_eq!((t2.ibytes, t2.obytes), (2000, 1800));
    assert_eq!((t2.ipackets, t2.opackets), (20, 18));

    // Orphaned bytes-only record at the end of the capture: packet
    // fields must read 0 rather than carrying over the prior tick's.
    let orphan = by_if1[2];
    assert_eq!((orphan.ibytes, orphan.obytes), (3000, 2700));
    assert_eq!((orphan.ipackets, orphan.opackets), (0, 0));

    // ifindex 2: packets-only record (no preceding bytes half) --
    // byte fields must read 0, not be dropped.
    let if2: Vec<&IfSample> = if_profile
        .samples
        .iter()
        .filter(|s| s.ifindex == 2)
        .collect();
    assert_eq!(if2.len(), 1);
    assert_eq!((if2[0].ibytes, if2[0].obytes), (0, 0));
    assert_eq!((if2[0].ipackets, if2[0].opackets), (5, 4));
}

/// On a schema new enough to carry the appended other_ibytes/
/// obytes/ipackets/opackets fields, the parser must use that direct
/// counter as-is, not silently re-derive it as total - tcp - udp. The
/// fixture sets other to a value the derive formula would NOT produce,
/// so a regression back to always-derive would be caught here.
#[test]
fn if_sample_other_field_is_read_directly_not_rederived() {
    let bytes = synthetic_elog_if_sample_other_direct();
    let progress = LoadProgress::default();
    let (
        _sessions,
        _stars,
        _pace,
        _cpu,
        if_profile,
        _tcp,
        _ella,
        _nginx,
        _file_info,
        _providers,
        _schema,
    ) = parse_elog_bytes_with_progress(&bytes, &progress).expect("parse synthetic fixture");

    assert_eq!(if_profile.samples.len(), 1);
    let s = &if_profile.samples[0];
    assert_eq!((s.ibytes, s.obytes), (1000, 900));
    assert_eq!((s.tcp_ibytes, s.tcp_obytes), (600, 500));
    assert_eq!((s.udp_ibytes, s.udp_obytes), (300, 300));
    // The naive derive-by-subtraction formula would give (100, 100);
    // the fixture's direct counters are (55, 45).
    assert_eq!((s.other_ibytes, s.other_obytes), (55, 45));
    assert_eq!((s.other_ipackets, s.other_opackets), (1, 1));
}
