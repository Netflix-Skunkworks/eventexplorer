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

//! Integration coverage for EventLog v2 (schema-verified decoding): parsing
//! the 50-byte provider rows + embedded `EDB1` blobs, and the three routing
//! outcomes the load-time validation guard can produce (verified,
//! divergent-generic, and schema-only-generic). See
//! `support::synthetic_elog_v2` for the fixture's exact layout.

mod support;

use event_explorer::elog_parser::*;
use support::{synthetic_elog_v2, synthetic_elog_v2_in_out_drifted};

#[test]
fn tcp_in_is_verified_and_decodes_via_hardcoded_path_with_additive_notice() {
    let (bytes, tcp_session) = synthetic_elog_v2();
    let progress = LoadProgress::default();
    let (sessions, _nic, _pace, _cpu, _if, _tcp, _ella, _nginx, file_info, providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse v2 fixture");

    assert_eq!(file_info.version, 2);

    let tcp_provider = providers
        .iter()
        .find(|p| p.name == "tcp")
        .expect("tcp provider present");
    assert_eq!(tcp_provider.schema_name.as_deref(), Some("tcp"));
    assert_eq!(tcp_provider.schema_event_count, 2);
    assert_eq!(
        tcp_provider.schema_new_fields.len(),
        1,
        "IN's additive 'future_flag' field should produce exactly one notice: {:?}",
        tcp_provider.schema_new_fields
    );
    assert!(tcp_provider.schema_new_fields[0].contains("future_flag"));

    let events = sessions
        .get(&tcp_session)
        .expect("tcp session should have events");
    assert_eq!(
        events.len(),
        1,
        "only the verified IN event should decode via the hardcoded path -- \
         the divergent DSACK must not appear here: {events:?}"
    );
    match &events[0].payload {
        EventPayload::In {
            seq,
            ack,
            win,
            len,
            flags,
            state,
            th_off,
            rcv_time_us,
            ..
        } => {
            assert_eq!(*seq, 1000);
            assert_eq!(*ack, 2000);
            assert_eq!(*win, 65535);
            assert_eq!(*len, 1460);
            assert_eq!(*flags, 0x10);
            assert_eq!(*state, 4);
            assert_eq!(*th_off, 5);
            assert_eq!(*rcv_time_us, 999_000);
        }
        other => panic!("expected EventPayload::In, got {other:?}"),
    }
}

#[test]
fn tcp_dsack_is_divergent_and_falls_back_to_generic_schema_decode() {
    let (bytes, _tcp_session) = synthetic_elog_v2();
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, _ella, _nginx, _file_info, providers, schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse v2 fixture");

    let tcp_provider = providers
        .iter()
        .find(|p| p.name == "tcp")
        .expect("tcp provider present");
    assert_eq!(
        tcp_provider.schema_divergent_events.len(),
        1,
        "DSACK's missing fields against TCP_EXPECTED should be flagged divergent: {:?}",
        tcp_provider.schema_divergent_events
    );
    assert!(tcp_provider.schema_divergent_events[0].contains("DSACK"));

    assert_eq!(
        schema.generic_events.len(),
        1,
        "the divergent DSACK event should be generically decoded: {:?}",
        schema.generic_events
    );
    let ev = &schema.generic_events[0];
    assert_eq!(ev.event_name, "DSACK");
    assert_eq!(ev.event_id, 402);
    assert_eq!(
        ev.formatted, "seen=1 round_end=12345",
        "generic decode should render the schema's real field values"
    );
}

#[test]
fn tcp_in_out_field_reorder_still_decodes_into_the_session() {
    let (bytes, tcp_session) = synthetic_elog_v2_in_out_drifted();
    let progress = LoadProgress::default();
    let (sessions, _nic, _pace, _cpu, _if, _tcp, _ella, _nginx, _file_info, providers, schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse drifted-schema fixture");

    let tcp_provider = providers
        .iter()
        .find(|p| p.name == "tcp")
        .expect("tcp provider present");
    assert!(
        tcp_provider.schema_divergent_events.is_empty(),
        "th_sum is optional, so the current kernel layout must not flag drift: {:?}",
        tcp_provider.schema_divergent_events
    );

    // Reordered IN/OUT must stay on the typed session path.
    assert!(
        schema.generic_events.is_empty(),
        "IN/OUT should decode via the schema-driven session path, not generic: {:?}",
        schema.generic_events
    );

    let events = sessions
        .get(&tcp_session)
        .expect("tcp session should have events");
    assert_eq!(events.len(), 2, "expected exactly IN + OUT: {events:?}");

    match &events[0].payload {
        EventPayload::In {
            seq,
            ack,
            win,
            len,
            flags,
            state,
            th_off,
            th_urp,
            rcv_time_us,
            ..
        } => {
            assert_eq!(*seq, 1000);
            assert_eq!(*ack, 2000);
            assert_eq!(
                *win, 54321,
                "win must come from its new offset (12), not the stale offset 8"
            );
            assert_eq!(
                *len, 1300,
                "len must come from its new offset (16), not the stale offset 12"
            );
            assert_eq!(*flags, 0x18);
            assert_eq!(*state, 6);
            assert_eq!(*th_off, 5);
            assert_eq!(*th_urp, 42);
            assert_eq!(*rcv_time_us, 888_000);
        }
        other => panic!("expected EventPayload::In, got {other:?}"),
    }

    match &events[1].payload {
        EventPayload::Out {
            seq,
            ack,
            win,
            len,
            flags,
            cwnd,
            th_off,
            th_urp,
            ..
        } => {
            assert_eq!(*seq, 3000);
            assert_eq!(*ack, 4000);
            assert_eq!(*win, 12345, "win must come from its new offset (12)");
            assert_eq!(*len, 1460, "len must come from its new offset (16)");
            assert_eq!(*flags, 0x10);
            assert_eq!(*cwnd, 11 * 1460);
            assert_eq!(*th_off, 5);
            assert_eq!(*th_urp, 7);
        }
        other => panic!("expected EventPayload::Out, got {other:?}"),
    }
}

#[test]
fn hpts_wheel_events_decode_from_v2_capture_and_provider_is_verified() {
    let (bytes, _tcp_session) = synthetic_elog_v2();
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, _ella, _nginx, _file_info, providers, schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse v2 fixture");

    let hpts_provider = providers
        .iter()
        .find(|p| p.name == "hpts")
        .expect("hpts provider present");
    assert_eq!(hpts_provider.schema_name.as_deref(), Some("hpts"));
    assert_eq!(hpts_provider.schema_event_count, 2);
    assert!(
        hpts_provider.schema_divergent_events.is_empty(),
        "hpts wheel schema matches hpts_wheel.rs exactly: {:?}",
        hpts_provider.schema_divergent_events
    );
    assert!(
        hpts_provider.schema_new_fields.is_empty(),
        "no additive fields in this fixture's hpts schema: {:?}",
        hpts_provider.schema_new_fields
    );

    assert_eq!(schema.hpts_wheel_events.len(), 2);
    match &schema.hpts_wheel_events[0].payload {
        HptsWheelPayload::Init {
            num_hptss,
            bind_type,
            num_slots,
            usecs_per_slot,
            total_slot_time,
        } => {
            assert_eq!(*num_hptss, 4);
            assert_eq!(*bind_type, HPTS_BIND_TYPE_CPU);
            assert_eq!(*num_slots, 4096);
            assert_eq!(*usecs_per_slot, 100);
            assert_eq!(*total_slot_time, 409_600);
        }
        other => panic!("expected HptsWheelPayload::Init, got {other:?}"),
    }
    match &schema.hpts_wheel_events[1].payload {
        HptsWheelPayload::PerfLateness {
            gencnt,
            lateness_slots,
            lateness_usec,
            requested_time,
            actual_time,
        } => {
            assert_eq!(*gencnt, 555);
            assert_eq!(*lateness_slots, 3);
            assert_eq!(*lateness_usec, 250);
            assert_eq!(*requested_time, 1_000);
            assert_eq!(*actual_time, 1_250);
        }
        other => panic!("expected HptsWheelPayload::PerfLateness, got {other:?}"),
    }
}

#[test]
fn tcp_out_with_retransmit_and_tlp_flags() {
    let (bytes, tcp_session) = synthetic_elog_v2_in_out_drifted();
    let progress = LoadProgress::default();
    let (sessions, _nic, _pace, _cpu, _if, _tcp, _ella, _nginx, _file_info, _providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse fixture");

    let events = sessions
        .get(&tcp_session)
        .expect("tcp session should have events");
    let out_ev = events
        .iter()
        .find(|e| matches!(e.payload, EventPayload::Out { .. }))
        .expect("should find OUT event");

    if let EventPayload::Out { flags, .. } = out_ev.payload {
        // Wire payload in synthetic_elog_v2_in_out_drifted sets flags = 0x10 (ACK)
        assert_eq!(flags, 0x10);
        assert!(!out_ev.payload.is_retransmit());
        assert!(!out_ev.payload.is_tlp());
    }

    // Now test with RETRANSMIT and TLP flags set on OUT
    let mut drifted_retx_bytes = bytes.clone();
    // In synthetic_elog_v2_in_out_drifted, the OUT payload starts after the header + providers + blobs + first event.
    // Let's modify the OUT event's flags field directly in the bytes:
    // Search for the sequence of ack=4000 (0x00000fa0) followed by flags (2 bytes)
    let ack_bytes = 4000u32.to_le_bytes();
    let pos = drifted_retx_bytes
        .windows(4)
        .position(|w| w == ack_bytes)
        .expect("should find ack=4000");
    let flags_pos = pos + 4;
    let new_flags = (TCP_FLAG_ACK | TCP_FLAG_RETRANSMIT | TCP_FLAG_TLP).to_le_bytes();
    drifted_retx_bytes[flags_pos..flags_pos + 2].copy_from_slice(&new_flags);

    let (sessions2, ..) =
        parse_elog_bytes_with_progress(&drifted_retx_bytes, &progress).expect("parse fixture 2");
    let events2 = sessions2.get(&tcp_session).unwrap();
    let out_ev2 = events2
        .iter()
        .find(|e| matches!(e.payload, EventPayload::Out { .. }))
        .unwrap();
    assert!(out_ev2.payload.is_retransmit());
    assert!(out_ev2.payload.is_tlp());
    if let EventPayload::Out { flags, .. } = out_ev2.payload {
        assert_eq!(flags, TCP_FLAG_ACK | TCP_FLAG_RETRANSMIT | TCP_FLAG_TLP);
    }
}
