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

//! Integration coverage for in-band provider announcements
//! (`EVT_PROVIDER_INFO` / `EVT_PROVIDER_BLOB`): a provider that registers
//! after the file header was written, so its row is absent from the
//! header's provider table and its decode blob (if any) is absent from
//! the header's blob table. See `support::synthetic_elog_v3_late_provider`
//! for the fixture's exact layout, and elog.5's "Provider Announcement
//! Records" for the on-disk format.

mod support;

use event_explorer::elog_parser::*;
use support::synthetic_elog_v3_late_provider;

#[test]
fn late_provider_is_routed_and_decoded_via_its_in_band_announcement() {
    let (bytes, late_session, late_pid) = synthetic_elog_v3_late_provider();
    let progress = LoadProgress::default();
    let (sessions, _nic, _pace, _cpu, _if, _tcp, _ella, _nginx, file_info, providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse late-provider fixture");

    assert_eq!(file_info.version, 3);

    let late_provider = providers
        .iter()
        .find(|p| p.id == late_pid)
        .expect("late-announced provider should appear in the provider list");
    assert_eq!(late_provider.name, "tcp");
    assert_eq!(
        late_provider.event_count, 1,
        "only the real IN event should count -- the INFO/BLOB announcements themselves must not"
    );

    let events = sessions
        .get(&late_session)
        .expect("late provider's session should have decoded events");
    assert_eq!(events.len(), 1);
    match &events[0].payload {
        EventPayload::In {
            seq,
            ack,
            win,
            len,
            flags,
            state,
            ..
        } => {
            assert_eq!(*seq, 5000);
            assert_eq!(*ack, 6000);
            assert_eq!(*win, 32768);
            assert_eq!(*len, 1460);
            assert_eq!(*flags, 0x18);
            assert_eq!(*state, 4);
        }
        other => panic!("expected EventPayload::In, got {other:?}"),
    }
}

#[test]
fn late_provider_schema_from_in_band_blob_is_validated_like_a_header_time_one() {
    let (bytes, _late_session, late_pid) = synthetic_elog_v3_late_provider();
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, _ella, _nginx, _file_info, providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse late-provider fixture");

    let late_provider = providers
        .iter()
        .find(|p| p.id == late_pid)
        .expect("late-announced provider should appear in the provider list");
    assert_eq!(
        late_provider.schema_name.as_deref(),
        Some("tcp"),
        "the in-band EVT_PROVIDER_BLOB should resolve into Provider.schema_name exactly as a \
         header-time blob table entry would"
    );
    assert_eq!(late_provider.schema_event_count, 1);
    assert!(
        late_provider.schema_divergent_events.is_empty(),
        "the fixture's schema matches the hardcoded IN decoder exactly: {:?}",
        late_provider.schema_divergent_events
    );
    assert!(
        late_provider.schema_new_fields.is_empty(),
        "the fixture's schema has no additive fields: {:?}",
        late_provider.schema_new_fields
    );
}
