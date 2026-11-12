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

//! Coverage for `SYMBOL_PACK_REF` decode, the packed/legacy `SYMBOL_RANGE`
//! fallback, and `install_packs_with` filling ranges from a fetched pack.
//! Layouts are the kernel's `__packed` wire structs (see
//! `symbols_eventlog_schema.src`).

use event_explorer::elog_parser::*;

const PACK_ID: &str = "c6ebd3c672f33ee93784de197c4b1a5e54f27580";
const BASE: u64 = 0xFFFFFFFF81000000;
const KMOD_SIZE: u64 = 0x100000;

fn cstr(s: &str, n: usize) -> Vec<u8> {
    let mut b = s.as_bytes().to_vec();
    b.truncate(n - 1);
    b.resize(n, 0);
    b
}

fn ev(provider: u16, event_id: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(32 + payload.len());
    v.extend_from_slice(&(32u16 + payload.len() as u16).to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes()); // cpu
    v.extend_from_slice(&provider.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes()); // reserved
    v.extend_from_slice(&1000u64.to_le_bytes()); // timestamp
    v.extend_from_slice(&0u64.to_le_bytes()); // session
    v.extend_from_slice(&event_id.to_le_bytes());
    v.extend_from_slice(&0i32.to_le_bytes()); // thread
    v.extend_from_slice(payload);
    v
}

fn elog(providers: &[(u16, &str)], events: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&3u32.to_le_bytes()); // version
    out.extend_from_slice(&0u64.to_le_bytes()); // capture_start
    out.extend_from_slice(&0u64.to_le_bytes()); // start_utc
    out.extend_from_slice(&1u64.to_le_bytes()); // event_count (unused)
    out.extend_from_slice(&0u32.to_le_bytes()); // counter_count
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
        out.extend_from_slice(&[0u8; 16]); // schema_id: none
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // blob_count
    out.extend_from_slice(&events);
    out
}

/// The pack: symbols provider + packed `SYMBOL_RANGE`s at file_id 1.
fn pack_bytes(ranges: &[(u64, u32, &str)]) -> Vec<u8> {
    let mut events = Vec::new();
    for (off, size, name) in ranges {
        let mut p = Vec::with_capacity(112);
        p.extend_from_slice(&1u32.to_le_bytes()); // file_id (pack convention)
        p.extend_from_slice(&off.to_le_bytes());
        p.extend_from_slice(&size.to_le_bytes());
        p.extend_from_slice(&cstr(name, 96));
        events.extend(ev(1, EVT_SYM_SYMBOL_RANGE, &p));
    }
    elog(&[(1, "symbols")], events)
}

fn linker_file(file_id: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(257);
    p.extend_from_slice(&file_id.to_le_bytes());
    p.extend_from_slice(&BASE.to_le_bytes());
    p.extend_from_slice(&KMOD_SIZE.to_le_bytes());
    p.extend_from_slice(&1u32.to_le_bytes()); // flags: LINKED
    p.extend_from_slice(&cstr(PACK_ID, 41));
    p.extend_from_slice(&cstr("tcp_rack.ko", 64));
    p.extend_from_slice(&cstr("/boot/modules/tcp_rack.ko", 128));
    assert_eq!(p.len(), 257);
    p
}

fn pack_ref(kind: u8, file_id: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(71);
    p.push(kind);
    p.push(0); // id_kind: GNU build-id
    p.extend_from_slice(&file_id.to_le_bytes());
    p.extend_from_slice(&cstr(PACK_ID, 65));
    assert_eq!(p.len(), 71);
    p
}

fn cpu_sample(frames: &[u64]) -> Vec<u8> {
    let mut p = Vec::with_capacity(16 + 8 * frames.len());
    p.extend_from_slice(&100u32.to_le_bytes()); // tid
    p.extend_from_slice(&0u32.to_le_bytes()); // pid (kernel)
    p.push(0); // type
    p.push(0); // flags
    p.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    p.extend_from_slice(&0u16.to_le_bytes()); // freq
    p.extend_from_slice(&0u16.to_le_bytes()); // pad
    for f in frames {
        p.extend_from_slice(&f.to_le_bytes());
    }
    p
}

#[test]
fn pack_ref_decodes_and_install_fills_ranges() {
    let ref_cap = elog(
        &[(1, "cpu"), (2, "symbols")],
        [
            ev(2, EVT_SYM_LINKER_FILE, &linker_file(7)),
            ev(2, EVT_SYM_SYMBOL_PACK_REF, &pack_ref(0, 7)),
            ev(1, 1, &cpu_sample(&[BASE + 0x1015, BASE + 0x2005])),
        ]
        .concat(),
    );
    let mut profile = parse_elog_core(&ref_cap, None)
        .expect("parse ref capture")
        .3;
    assert_eq!(profile.pack_refs.len(), 1);
    let r = &profile.pack_refs[0];
    assert_eq!((r.kind, r.id_kind, r.file_id), (0, 0, 7));
    assert_eq!(r.pack_id, PACK_ID);
    assert!(!profile.symbols.contains_key(&7), "no in-file ranges yet");

    let pack = pack_bytes(&[(0x1010, 0x40, "rack_fn_a"), (0x2000, 0x80, "rack_fn_b")]);
    let stats = install_packs_with(&mut profile, &mut |id| {
        assert_eq!(id, PACK_ID);
        Some(pack.clone())
    });
    assert_eq!(
        (
            stats.refs,
            stats.already_inline,
            stats.installed,
            stats.failed
        ),
        (1, 0, 1, 0)
    );

    assert!(
        !profile.symbols.contains_key(&7),
        "pack ranges stay out of the in-file table"
    );
    let hit = profile.symbolize(BASE + 0x1015, None);
    assert_eq!(hit.as_ref().map(|h| h.name()), Some("rack_fn_a"));
    let hit = profile.symbolize(BASE + 0x2004, None);
    assert_eq!(hit.as_ref().map(|h| h.name()), Some("rack_fn_b"));
}

#[test]
fn in_file_ranges_win_over_pack() {
    let mut events = Vec::new();
    events.extend(ev(2, EVT_SYM_LINKER_FILE, &linker_file(7)));
    events.extend(ev(2, EVT_SYM_SYMBOL_PACK_REF, &pack_ref(0, 7)));
    let mut inline = Vec::with_capacity(112);
    inline.extend_from_slice(&7u32.to_le_bytes());
    inline.extend_from_slice(&0x3000u64.to_le_bytes());
    inline.extend_from_slice(&0x10u32.to_le_bytes());
    inline.extend_from_slice(&cstr("inline_fn", 96));
    events.extend(ev(2, EVT_SYM_SYMBOL_RANGE, &inline));
    let ref_cap = elog(&[(1, "cpu"), (2, "symbols")], events);

    let mut profile = parse_elog_core(&ref_cap, None).expect("parse").3;
    let stats = install_packs_with(&mut profile, &mut |_| {
        panic!("pack must not be fetched when in-file ranges exist")
    });
    assert_eq!((stats.installed, stats.already_inline), (0, 1));
    let hit = profile.symbolize(BASE + 0x3001, None);
    assert_eq!(hit.as_ref().map(|h| h.name()), Some("inline_fn"));
}

#[test]
fn legacy_padded_range_still_decodes() {
    // Naturally-aligned 116 B SYMBOL_RANGE (pre-packing captures and
    // early PoC packs) must keep decoding via the legacy offsets.
    let mut p = Vec::with_capacity(116);
    p.extend_from_slice(&1u32.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes()); // alignment pad
    p.extend_from_slice(&0x4000u64.to_le_bytes());
    p.extend_from_slice(&0x20u32.to_le_bytes());
    p.extend_from_slice(&cstr("legacy_fn", 96));
    assert_eq!(p.len(), 116);
    let cap = elog(&[(1, "symbols")], ev(1, EVT_SYM_SYMBOL_RANGE, &p));

    let profile = parse_elog_core(&cap, None).expect("parse").3;
    let ranges = &profile.symbols[&1];
    assert_eq!(ranges.len(), 1);
    assert_eq!((ranges[0].offset, ranges[0].size), (0x4000, 0x20));
    assert_eq!(ranges[0].name, "legacy_fn");
}
