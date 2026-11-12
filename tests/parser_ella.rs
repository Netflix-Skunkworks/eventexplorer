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

use event_explorer::analysis::{
    build_ella_channel_list, build_ella_channel_plot, build_ella_relay_plot,
};
use event_explorer::elog_parser::*;
use support::{
    synthetic_elog_ella, synthetic_elog_fanout_backlog, synthetic_elog_fanout_node,
    synthetic_elog_fanout_node_info, synthetic_elog_fanout_stats_only, BACKLOG_GAP_US,
    CAPTURE_START, ELLA_RELAY_PID, ELLA_RELAY_SESSION,
};

#[test]
fn ella_synthetic_decodes_channel_layer() {
    let bytes = synthetic_elog_ella();
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, ella, _nginx, file_info, providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse ELLA fixture");

    assert_eq!(file_info.skipped_other_providers, 0);
    assert!(
        providers
            .iter()
            .any(|p| p.name == "ella_relay" && p.id == ELLA_RELAY_PID),
        "ella_relay provider missing"
    );

    assert_eq!(ella.channels.len(), 1);
    let ch = &ella.channels[&(ELLA_RELAY_PID, ELLA_RELAY_SESSION)];
    assert_eq!(ch.channel_name, "live/1");
    assert_eq!(ch.version, 2);
    assert_eq!(ch.channel_ids.get(&1), Some(&7));
    assert_eq!(ch.last_subscribers, 0);
    assert_eq!(ch.peak_subscribers, 1);
    assert_eq!(ch.last_state, Some(ELLA_LIFECYCLE_CREATED));
    assert_eq!(ch.destroy_reason, Some(1));
    assert_eq!(ch.end_state_name(), "DESTROYED");

    assert_eq!(
        ella.instances.get(&(ELLA_RELAY_PID, 1)),
        Some(&ELLA_SIDE_UPSTREAM)
    );
    assert_eq!(
        ella.instances.get(&(ELLA_RELAY_PID, 2)),
        Some(&ELLA_SIDE_DOWNSTREAM)
    );

    assert_eq!(ella.counts.len(), 2);
    assert_eq!(ella.counts[0].instance_id, 1);
    assert_eq!(ella.counts[0].channels, 1);
    assert_eq!(ella.counts[1].subscribers, 3);
    assert_eq!(ella.counts[1].channels_pending_leave, 1);

    assert!(ella
        .events
        .iter()
        .any(|e| matches!(e.payload, EllaPayload::SessionCreate { .. })));
    assert!(ella
        .events
        .iter()
        .any(|e| matches!(e.payload, EllaPayload::ChannelSubscriber { .. })));
    assert!(ella
        .events
        .iter()
        .any(|e| matches!(e.payload, EllaPayload::RelayError { .. })));
    assert!(ella
        .events
        .iter()
        .any(|e| e.session_id == ELLA_RELAY_GLOBAL_SESSION
            && matches!(e.payload, EllaPayload::InstanceAttach { .. })));

    let series = build_ella_relay_plot(&ella, file_info.capture_start);
    assert!(!series.aggregated);
    assert_eq!(series.subscribers.len(), 1);
    assert_eq!(series.subscribers[0].data.len(), 2);

    let plot = build_ella_channel_plot(
        &ella,
        ELLA_RELAY_PID,
        ELLA_RELAY_SESSION,
        file_info.capture_start,
    );
    assert_eq!(plot.subscribers.len(), 2);
    assert!(
        plot.subscribers
            .iter()
            .any(|s| s.data.iter().any(|p| p[1] >= 1.0)),
        "SUBSCRIBED should raise occupancy"
    );
    assert!(!plot.markers.is_empty());

    let rows = build_ella_channel_list(&ella);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].peak_subscribers, 1);
    assert_eq!(rows[0].short_label, "1");
    assert_eq!(rows[0].error_count, 1);
}

#[test]
fn node_info_declares_the_role_over_the_attach_set() {
    for (role, expected) in [
        (ELLA_ROLE_ORIGIN, EllaNodeRole::Server),
        (ELLA_ROLE_RELAY, EllaNodeRole::Relay),
    ] {
        let bytes = synthetic_elog_fanout_node_info(role);
        let progress = LoadProgress::default();
        let (
            _sessions,
            _nic,
            _pace,
            _cpu,
            _if,
            _tcp,
            ella,
            _nginx,
            _file_info,
            _providers,
            _schema,
        ) = parse_elog_bytes_with_progress(&bytes, &progress).expect("parse NODE_INFO fixture");

        assert_eq!(ella.roles.get(&ELLA_RELAY_PID), Some(&role));
        assert_eq!(ella.node_role(ELLA_RELAY_PID), expected);
        assert!(ella
            .events
            .iter()
            .any(|e| matches!(e.payload, EllaPayload::NodeInfo { role: r } if r == role)));
    }
}

#[test]
fn fanout_node_provider_and_five_u32_count() {
    let bytes = synthetic_elog_fanout_node();
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, ella, _nginx, _file_info, providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse fanout_node fixture");

    assert!(
        providers
            .iter()
            .any(|p| p.name == "fanout_node" && p.id == ELLA_RELAY_PID),
        "fanout_node provider missing"
    );
    assert_eq!(ella.counts.len(), 1);
    assert_eq!(ella.counts[0].channels, 2);
    assert_eq!(ella.counts[0].sessions, 0);
    assert_eq!(ella.counts_total.len(), 1);
    assert_eq!(ella.counts_total[0].channels_dn, 10);
    assert_eq!(ella.counts_total[0].channels_pending_leave_up, 1);
    assert_eq!(ella.counts_total[0].channels_pending_leave_dn, 5);
    assert_eq!(ella.counts_total[0].subscribers_dn, 40);
    assert!(ella
        .events
        .iter()
        .any(|e| matches!(e.payload, EllaPayload::RelayError { .. })));

    let plot = build_ella_relay_plot(&ella, 0);
    assert!(!plot.aggregated);
}

#[test]
fn fanout_node_stats_only_plots_from_total() {
    let bytes = synthetic_elog_fanout_stats_only();
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, ella, _nginx, file_info, _providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse STATS-only fixture");

    assert!(ella.counts.is_empty());
    assert_eq!(ella.counts_total.len(), 2);
    assert_eq!(ella.counts_total[1].channels_pending_leave_up, 2);
    assert_eq!(ella.counts_total[1].channels_pending_leave_dn, 6);
    let plot = build_ella_relay_plot(&ella, file_info.capture_start);
    assert!(plot.aggregated);
    assert!(plot.dn_channels_max.is_empty());
    let peak = plot
        .dn_channels_sum
        .iter()
        .map(|p| p[1])
        .fold(0.0_f64, f64::max);
    assert!((peak - 20.0).abs() < 1e-6, "STATS-only dn peak {peak}");
    let leave_up = plot
        .pending
        .iter()
        .find(|s| s.label == "Upstream Leave")
        .unwrap();
    let leave_peak = leave_up.data.iter().map(|p| p[1]).fold(0.0_f64, f64::max);
    assert!(
        (leave_peak - 2.0).abs() < 1e-6,
        "STATS-only upstream leave {leave_peak}"
    );
    let dn_leave = plot
        .dn_leave_sum
        .iter()
        .map(|p| p[1])
        .fold(0.0_f64, f64::max);
    assert!(
        (dn_leave - 6.0).abs() < 1e-6,
        "STATS-only downstream leave {dn_leave}"
    );
}

#[test]
fn drained_backlog_does_not_backdate_the_capture_origin() {
    let bytes = synthetic_elog_fanout_backlog();
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, ella, _nginx, file_info, _providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse backlog fixture");

    assert_eq!(file_info.header_capture_start, CAPTURE_START);
    assert_eq!(file_info.backlog_events, 1);
    assert_eq!(file_info.capture_start, CAPTURE_START + BACKLOG_GAP_US);
    // The origin lands on the live start, so the capture reads as the 19s
    // of samples it holds rather than the 1500s the header implies.
    let duration_s = file_info
        .capture_end
        .saturating_sub(file_info.capture_start) as f64
        / 1_000_000.0;
    assert!(
        (duration_s - 19.0).abs() < 1e-6,
        "duration {duration_s}s should span only the live capture"
    );
    // The backlog NODE_INFO still applies -- the origin moved, the event
    // was not discarded.
    assert_eq!(ella.node_role(ELLA_RELAY_PID), EllaNodeRole::Relay);
}

#[test]
fn a_capture_without_backlog_keeps_its_header_origin() {
    let bytes = synthetic_elog_fanout_node();
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, _ella, _nginx, file_info, _providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse fanout fixture");

    assert_eq!(file_info.capture_start, file_info.header_capture_start);
    assert_eq!(file_info.backlog_events, 0);
}
