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

#[allow(unused_imports)]
use super::*;

fn payload_detail(p: &EllaPayload) -> String {
    match p {
        EllaPayload::InstanceAttach { instance_id, side } => {
            format!("instance={instance_id} side={}", ella_side_name(*side))
        }
        EllaPayload::NodeInfo { role } => format!("role={}", ella_role_name(*role)),
        EllaPayload::SessionCreate {
            channel_name,
            version,
            instance_id,
        } => format!("channel={channel_name} v{version} instance={instance_id}"),
        EllaPayload::SessionDestroy => String::new(),
        EllaPayload::ChannelState {
            state,
            reason,
            instance_id,
            subscribers,
        } => format!(
            "{} reason={} instance={instance_id} subs={subscribers}",
            ella_lifecycle_state_name(*state),
            ella_lifecycle_reason_name(*reason)
        ),
        EllaPayload::ChannelDestroyed {
            reason,
            instance_id,
            subscribers,
        } => format!(
            "reason={} instance={instance_id} subs={subscribers}",
            ella_destroy_reason_name(*reason)
        ),
        EllaPayload::ChannelIdAssigned {
            instance_id,
            channel_id,
        } => format!("instance={instance_id} channel_id={channel_id}"),
        EllaPayload::ChannelSubscriber {
            state,
            reason,
            instance_id,
            subscriber_address,
            subscriber_port,
            subscribers,
        } => format!(
            "{} reason={} instance={instance_id} {subscriber_address}:{subscriber_port} subs={subscribers}",
            ella_subscriber_state_name(*state),
            ella_subscriber_reason_name(*reason)
        ),
        EllaPayload::SubscriberIdentity {
            instance_id,
            request_id,
            subscriber_address,
            subscriber_port,
            pbcid,
        } => format!(
            "instance={instance_id} request={request_id} {subscriber_address}:{subscriber_port} pbcid={pbcid}"
        ),
        EllaPayload::ChannelError {
            code,
            instance_id,
            detail,
        } => format!(
            "{} instance={instance_id} detail={detail}",
            ella_error_code_name(*code)
        ),
        EllaPayload::ObjectState {
            state,
            instance_id,
            boundary_index,
            first_packet,
            packets,
            bytes,
        } => format!(
            "{} instance={instance_id} slot={boundary_index} first_pn={first_packet} packets={packets} bytes={bytes}",
            ella_object_state_name(*state)
        ),
        EllaPayload::ObjectTiming {
            instance_id,
            object_id,
            first_packet,
            packets_sent,
            packets_total,
            packets_unsent,
            start_lateness_us,
            signature_duration_us,
            encode_duration_us,
            enqueue_duration_us,
        } => format!(
            "instance={instance_id} object={object_id} first_pn={first_packet} sent={packets_sent}/{packets_total} unsent={packets_unsent} late_us={start_lateness_us} sign_us={signature_duration_us} encode_us={encode_duration_us} enqueue_us={enqueue_duration_us}"
        ),
        EllaPayload::FrameSent {
            instance_id,
            object_id,
            frame_index,
            first_packet,
            packets,
            is_probe,
            toggle,
            frame_len,
            planned_send_us,
            actual_send_us,
            start_lateness_us,
        } => format!(
            "instance={instance_id} object={object_id} frame={frame_index} first_pn={first_packet} packets={packets} probe={} toggle={} len={frame_len} planned_us={planned_send_us} actual_us={actual_send_us} lateness_us={start_lateness_us}",
            *is_probe as u8, *toggle as u8
        ),
        EllaPayload::FrameRelay {
            instance_id,
            first_packet,
            packets,
            toggle,
            boundary_index,
            frame_len,
            wire_send_us,
            recv_us,
            fwd_us,
            residency_us,
        } => format!(
            "instance={instance_id} first_pn={first_packet} packets={packets} toggle={} slot={boundary_index} len={frame_len} wire_us={wire_send_us} recv_us={recv_us} fwd_us={fwd_us} residency_us={residency_us}",
            *toggle as u8
        ),
        EllaPayload::RelayError {
            code,
            instance_id,
            channel_id,
            channel_id_known,
            detail,
            message_type,
            subscriber_address,
            subscriber_port,
        } => {
            let mut out = format!(
                "{} instance={instance_id} channel_id={channel_id} known={} detail={detail}",
                ella_relay_error_code_name(*code),
                *channel_id_known as u8
            );
            if *message_type != 0 {
                out.push_str(&format!(
                    " msg={}({message_type:#04x})",
                    ella_fanout_message_type_name(*message_type)
                ));
            }
            if !subscriber_address.is_empty() {
                out.push_str(&format!(" peer={subscriber_address}:{subscriber_port}"));
            }
            out
        }
    }
}

/// List ELLA channel sessions seen in the capture.
pub fn cmd_ella(prof: &EllaProfile, _capture_start: u64) {
    if prof.is_empty() {
        println!("No ELLA (`fanout_node`) data in this capture.");
        return;
    }
    let rows = build_ella_channel_list(prof);
    println!(
        "=== ELLA channels ({} titles, {} sessions) ===",
        rows.len(),
        prof.channels.len()
    );
    println!(
        "{:<15} {:<12} {:<18} {:<48} {:>8} {:>6} {:>10} {:>10} {:>8} {:>5} {:>6} {:>10}",
        "node",
        "ids",
        "channel",
        "name",
        "dur_s",
        "peak",
        "rate",
        "frame",
        "stretch",
        "inst",
        "errors",
        "events"
    );
    for r in rows {
        let name = if r.channel_name.is_empty() {
            "-"
        } else {
            &r.channel_name
        };
        println!(
            "{:<15} {:<12} {:<18} {:<48} {:>8.3} {:>6} {:>10} {:>10} {:>8} {:>5} {:>6} {:>10}",
            r.node,
            r.ids_label,
            if r.short_label.is_empty() {
                "-"
            } else {
                &r.short_label
            },
            name,
            r.duration_s,
            r.peak_subscribers,
            format_ella_rx_rate(r.rx_rate_mbps),
            format_ella_rx_rate(r.frame_rate_mbps),
            format_ella_stretch(r.stretch_frac),
            r.instance_count,
            r.error_count,
            r.event_count
        );
    }
}

/// Dump `CHANNEL_COUNT`, `CHANNEL_COUNT_TOTAL`, and `CHANNEL_SAMPLE`.
pub fn cmd_ella_stats(prof: &EllaProfile, capture_start: u64) {
    if prof.counts.is_empty()
        && prof.counts_total.is_empty()
        && prof.channel_samples.is_empty()
        && prof.frame_samples.is_empty()
        && prof.offload_samples.is_empty()
    {
        println!(
            "No CHANNEL_COUNT / CHANNEL_COUNT_TOTAL / CHANNEL_SAMPLE samples in this capture."
        );
        return;
    }
    println!(
        "ella samples: {} channel, {} frame",
        prof.channel_samples.len(),
        prof.frame_samples.len()
    );
    if !prof.counts.is_empty() {
        println!("=== CHANNEL_COUNT ({} samples) ===", prof.counts.len());
        println!(
            "{:>10} {:<8} {:<22} {:>6} {:>6} {:>7} {:>7} {:>6}",
            "t(s)", "node", "instance", "chan", "subs", "leave", "retire", "sess"
        );
        for s in &prof.counts {
            let t = s.ts.saturating_sub(capture_start) as f64 / 1_000_000.0;
            println!(
                "{:>10.3} {:<8} {:<22} {:>6} {:>6} {:>7} {:>7} {:>6}",
                t,
                prof.node_cli_prefix(s.provider_id),
                prof.instance_label(s.provider_id, s.instance_id),
                s.channels,
                s.subscribers,
                s.channels_pending_leave,
                s.channels_pending_retire,
                s.sessions
            );
        }
    }
    if !prof.counts_total.is_empty() {
        if !prof.counts.is_empty() {
            println!();
        }
        println!(
            "=== CHANNEL_COUNT_TOTAL ({} samples) ===",
            prof.counts_total.len()
        );
        println!(
            "{:>10} {:<8} {:>6} {:>6} {:>7} {:>7} {:>10} {:>10} {:>7} {:>7} {:>8} {:>10} {:>8} {:>7} {:>7} {:>9}",
            "t(s)",
            "node",
            "ch_up",
            "ch_dn",
            "sub_up",
            "sub_dn",
            "pkts_up",
            "pkts_dn",
            "jn_up",
            "jn_dn",
            "jn_dlvr",
            "bytes_dn",
            "new_cli",
            "cli_up",
            "cli_dn",
            "subs_dlvr"
        );
        for s in &prof.counts_total {
            let t = s.ts.saturating_sub(capture_start) as f64 / 1_000_000.0;
            println!(
                "{:>10.3} {:<8} {:>6} {:>6} {:>7} {:>7} {:>10} {:>10} {:>7} {:>7} {:>8} {:>10} {:>8} {:>7} {:>7} {:>9}",
                t,
                prof.node_cli_prefix(s.provider_id),
                s.channels_up,
                s.channels_dn,
                s.subscribers_up,
                s.subscribers_dn,
                s.packets_up,
                s.packets_dn,
                s.joins_up,
                s.joins_dn,
                s.joins_dlvr,
                s.bytes_dn.max(s.bytes_up),
                s.new_clients,
                s.clients_up,
                s.clients_dn,
                s.subscribers_dlvr
            );
        }
    }
    if !prof.channel_samples.is_empty() {
        if !prof.counts.is_empty() || !prof.counts_total.is_empty() {
            println!();
        }
        println!(
            "=== CHANNEL_SAMPLE ({} samples) ===",
            prof.channel_samples.len()
        );
        println!(
            "{:>10} {:<8} {:<24} {:<22} {:>6} {:>7} {:>7} {:>7} {:>9} {:>8} {:>8} {:>8} {:>8}",
            "t(s)",
            "node",
            "channel",
            "instance",
            "subs",
            "pending",
            "leaving",
            "objs",
            "bytes",
            "packets",
            "attached",
            "no_ring",
            "drn_skip"
        );
        for s in &prof.channel_samples {
            let t = s.ts.saturating_sub(capture_start) as f64 / 1_000_000.0;
            println!(
                "{:>10.3} {:<8} {:<24} {:<22} {:>6} {:>7} {:>7} {:>7} {:>9} {:>8} {:>8} {:>8} {:>8}",
                t,
                prof.node_cli_prefix(s.provider_id),
                prof.channel_label(s.provider_id, s.session_id),
                prof.instance_label(s.provider_id, s.instance_id),
                s.subscribers,
                s.subscribers_pending,
                s.subscribers_leaving,
                s.objects_completed,
                s.object_bytes,
                s.packets,
                s.inproc_attached,
                s.publish_no_ring,
                s.drain_skipped_pending
            );
        }
    }
    if !prof.frame_samples.is_empty() {
        if !prof.counts.is_empty()
            || !prof.counts_total.is_empty()
            || !prof.channel_samples.is_empty()
        {
            println!();
        }
        println!(
            "=== FRAME_SAMPLE ({} samples) ===",
            prof.frame_samples.len()
        );
        println!(
            "{:>10} {:<8} {:<24} {:<22} {:>6} {:>9} {:>10} {:>10} {:>10} {:>10}",
            "t(s)",
            "node",
            "channel",
            "instance",
            "frames",
            "bytes",
            "plan_mbps",
            "act_mbps",
            "fwd_mbps",
            "over_us"
        );
        for s in &prof.frame_samples {
            let t = s.ts.saturating_sub(capture_start) as f64 / 1_000_000.0;
            let mbps = |us: u32| {
                if us == 0 {
                    0.0
                } else {
                    s.sum_frame_len as f64 * 8.0 / us as f64
                }
            };
            println!(
                "{:>10.3} {:<8} {:<24} {:<22} {:>6} {:>9} {:>10.2} {:>10.2} {:>10.2} {:>10}",
                t,
                prof.node_cli_prefix(s.provider_id),
                prof.channel_label(s.provider_id, s.session_id),
                prof.instance_label(s.provider_id, s.instance_id),
                s.frames,
                s.sum_frame_len,
                mbps(s.sum_planned_us),
                mbps(s.sum_actual_us),
                mbps(s.sum_fwd_us),
                s.max_overshoot_us
            );
        }
    }
    if !prof.offload_samples.is_empty() {
        println!();
        println!(
            "=== OFFLOAD_STATS ({} samples) ===",
            prof.offload_samples.len()
        );
        println!(
            "rx_open: 0 not attempted, 1 opened, 2 no socket, 3 port unbound, \
             4 catch-all refused, 5 region too big, 6 socket failed, \
             7 init failed, 8 evloop failed"
        );
        println!(
            "{:>10} {:<8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>7} {:>6} {:>6} {:>8}",
            "t(s)",
            "node",
            "seq_ok",
            "seq_stale",
            "routed",
            "else_gate",
            "else_unreg",
            "malformed",
            "nocls",
            "overwrite",
            "drop_unreg",
            "rx_open",
            "port",
            "errno",
            "region"
        );
        for s in &prof.offload_samples {
            let t = s.ts.saturating_sub(capture_start) as f64 / 1_000_000.0;
            println!(
                "{:>10.3} {:<8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>7} {:>6} {:>6} {:>8}",
                t,
                prof.node_cli_prefix(s.provider_id),
                s.send_seq_ok,
                s.send_seq_stale,
                s.rxring_routed,
                s.rxring_else_gate,
                s.rxring_else_unreg,
                s.rxring_else_malformed,
                s.rxring_else_nocls,
                s.rxring_overwrites,
                s.rxring_dropped_unreg,
                s.rxring_result,
                s.rxring_port,
                s.rxring_errno,
                s.rxring_region_bytes
            );
        }
    }
}

/// Dump ELLA lifecycle/subscriber/object events, optionally filtered to one session.
pub fn cmd_ella_events(prof: &EllaProfile, capture_start: u64, session: Option<u64>) {
    if prof.events.is_empty() {
        println!("No ELLA channel-layer events in this capture.");
        return;
    }
    let mut n = 0usize;
    for e in &prof.events {
        if let Some(sid) = session {
            if e.session_id != sid {
                continue;
            }
        }
        n += 1;
        let t = e.ts.saturating_sub(capture_start) as f64 / 1_000_000.0;
        let detail = payload_detail(&e.payload);
        println!(
            "{:>10.6}  {:<8}  {:#x}  {}  {}",
            t,
            prof.node_cli_prefix(e.provider_id),
            e.session_id,
            ella_relay_event_name(e.event_id),
            detail
        );
    }
    if n == 0 {
        println!("No matching ELLA events.");
    }
}
