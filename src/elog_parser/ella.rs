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

/// Decode and ingest one `fanout_node` / `ella_relay` channel-layer event
/// into `prof`.
#[allow(clippy::too_many_arguments)]
pub fn ingest_ella_relay(
    prof: &mut EllaProfile,
    ts: u64,
    cpu: u16,
    tid: i32,
    provider_id: u16,
    session_id: u64,
    event_id: u32,
    data: &[u8],
    off: usize,
    size: usize,
    schema: Option<&ProviderSchema>,
) -> bool {
    let r = PayloadReader::new(schema, event_id, data, off, size);
    match event_id {
        EVT_ELLA_RELAY_INSTANCE_ATTACH if r.typed() || size >= 5 => {
            let instance_id = r.u32("instance_id", 0);
            let side = r.u8("side", 4);
            prof.instances.insert((provider_id, instance_id), side);
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                EllaPayload::InstanceAttach { instance_id, side },
            );
            true
        }
        EVT_ELLA_RELAY_NODE_INFO if r.typed() || size >= 1 => {
            let role = r.u8("role", 0);
            prof.roles.insert(provider_id, role);
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                EllaPayload::NodeInfo { role },
            );
            true
        }
        EVT_ELLA_RELAY_RELAY_ERROR if r.typed() || size >= 18 => {
            let (subscriber_address, subscriber_port) = if r.typed() {
                (
                    r.ip_or_cstr("subscriber_address", 18, 48),
                    r.u16("subscriber_port", 13),
                )
            } else {
                let legacy = size == 20 || size >= 69;
                if size >= 69 {
                    (
                        read_cstr(data, off + 21, 48),
                        if size >= 72 {
                            read_u16(data, off + 70)
                        } else {
                            0
                        },
                    )
                } else if legacy {
                    (String::new(), 0)
                } else {
                    let len = data[off + 17] as usize;
                    let port = read_u16(data, off + 13);
                    if len > 0 && 18 + len <= size {
                        (format_ip_addr(&data[off + 18..off + 18 + len]), port)
                    } else {
                        (String::new(), port)
                    }
                }
            };
            let packed = r.typed() || !(size == 20 || size >= 69);
            let payload = EllaPayload::RelayError {
                code: r.u8("code", 0),
                instance_id: r.u32("instance_id", if packed { 1 } else { 4 }),
                channel_id: r.u32("channel_id", if packed { 5 } else { 8 }),
                channel_id_known: r.u8("channel_id_known", if packed { 15 } else { 12 }) != 0,
                detail: r.u32("detail", if packed { 9 } else { 16 }),
                message_type: r.u8("message_type", if packed { 16 } else { 20 }),
                subscriber_address,
                subscriber_port,
            };
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                payload,
            );
            true
        }
        EVT_ELLA_RELAY_SESSION_CREATE if r.typed() || size >= 69 => {
            let payload = EllaPayload::SessionCreate {
                channel_name: r.cstr("channel_name", 0, 64),
                version: r.u8("version", 64),
                instance_id: r.u32("instance_id", if size >= 72 { 68 } else { 65 }),
            };
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                payload,
            );
            true
        }
        EVT_ELLA_RELAY_SESSION_DESTROY => {
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                EllaPayload::SessionDestroy,
            );
            true
        }
        EVT_ELLA_RELAY_CHANNEL_STATE if r.typed() || size >= 10 => {
            let packed = r.typed() || size < 12;
            let payload = EllaPayload::ChannelState {
                state: r.u8("state", 0),
                reason: r.u8("reason", 1),
                instance_id: r.u32("instance_id", if packed { 2 } else { 4 }),
                subscribers: r.u32("subscribers", if packed { 6 } else { 8 }),
            };
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                payload,
            );
            true
        }
        EVT_ELLA_RELAY_CHANNEL_ID_ASSIGNED if r.typed() || size >= 8 => {
            let payload = EllaPayload::ChannelIdAssigned {
                instance_id: r.u32("instance_id", 0),
                channel_id: r.u32("channel_id", 4),
            };
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                payload,
            );
            true
        }
        EVT_ELLA_RELAY_CHANNEL_ERROR if r.typed() || size >= 9 => {
            let packed = r.typed() || size < 12;
            let payload = EllaPayload::ChannelError {
                code: r.u8("code", 0),
                instance_id: r.u32("instance_id", if packed { 1 } else { 4 }),
                detail: r.u32("detail", if packed { 5 } else { 8 }),
            };
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                payload,
            );
            true
        }
        EVT_ELLA_RELAY_CHANNEL_DESTROYED if r.typed() || size >= 9 => {
            let packed = r.typed() || size < 12;
            let payload = EllaPayload::ChannelDestroyed {
                reason: r.u8("reason", 0),
                instance_id: r.u32("instance_id", if packed { 1 } else { 4 }),
                subscribers: r.u32("subscribers", if packed { 5 } else { 8 }),
            };
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                payload,
            );
            true
        }
        EVT_ELLA_RELAY_CHANNEL_SUBSCRIBER if r.typed() || size >= 13 => {
            let (subscriber_address, subscriber_port, subscribers) = if r.typed() {
                (
                    r.ip_or_cstr("subscriber_address", 13, 48),
                    r.u16("subscriber_port", 10),
                    r.u32("subscribers", 6),
                )
            } else if size >= 64 {
                (
                    read_cstr(data, off + 8, 48),
                    read_u16(data, off + 56),
                    read_u32(data, off + 60),
                )
            } else {
                let subscribers = read_u32(data, off + 6);
                let port = read_u16(data, off + 10);
                let len = data[off + 12] as usize;
                let address = if len > 0 && 13 + len <= size {
                    format_ip_addr(&data[off + 13..off + 13 + len])
                } else {
                    String::new()
                };
                (address, port, subscribers)
            };
            let payload = EllaPayload::ChannelSubscriber {
                state: subscriber_state(&r, r.u8("state", 0)),
                reason: r.u8("reason", 1),
                instance_id: r.u32("instance_id", if r.typed() || size < 64 { 2 } else { 4 }),
                subscriber_address,
                subscriber_port,
                subscribers,
            };
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                payload,
            );
            true
        }
        EVT_ELLA_RELAY_SUBSCRIBER_IDENTITY if r.typed() || size >= 27 => {
            let addr = r.in6_addr("subscriber_address", 10);
            let pbcid_len = r.u8("pbcid_len", 26) as usize;
            let payload = EllaPayload::SubscriberIdentity {
                instance_id: r.u32("instance_id", 0),
                request_id: r.u32("request_id", 4),
                subscriber_address: addr
                    .to_ipv4_mapped()
                    .map_or_else(|| addr.to_string(), |a| a.to_string()),
                subscriber_port: r.u16("subscriber_port", 8),
                pbcid: r.text("pbcid", 27, pbcid_len),
            };
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                payload,
            );
            true
        }
        EVT_ELLA_RELAY_OBJECT_STATE if r.typed() || size >= 18 => {
            let packed = r.typed() || size < 24;
            let payload = EllaPayload::ObjectState {
                state: r.u8("state", 0),
                instance_id: r.u32("instance_id", if packed { 1 } else { 4 }),
                boundary_index: r.u8("boundary_index", if packed { 5 } else { 8 }),
                first_packet: r.u32("first_packet", if packed { 6 } else { 12 }),
                packets: r.u32("packets", if packed { 10 } else { 16 }),
                bytes: r.u32("bytes", if packed { 14 } else { 20 }),
            };
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                payload,
            );
            true
        }
        EVT_ELLA_RELAY_CHANNEL_COUNT if r.typed() || size >= 20 => {
            prof.counts.push(EllaChannelCount {
                ts,
                provider_id,
                instance_id: r.u32("instance_id", 0),
                channels: r.u32("channels", 4),
                channels_pending_leave: r.u32("channels_pending_leave", 8),
                channels_pending_retire: r.u32("channels_pending_retire", 12),
                subscribers: r.u32("subscribers", 16),
                sessions: r.u32("sessions", 20),
            });
            true
        }
        EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL if r.typed() || size >= 8 => {
            let (leave_up, leave_dn, retire_up, retire_dn, sub_up, sub_dn) =
                if r.typed() || size >= 32 {
                    (
                        r.u32("channels_pending_leave_up", 8),
                        r.u32("channels_pending_leave_dn", 12),
                        r.u32("channels_pending_retire_up", 16),
                        r.u32("channels_pending_retire_dn", 20),
                        r.u32("subscribers_up", 24),
                        r.u32("subscribers_dn", 28),
                    )
                } else if size >= 24 {
                    (
                        0,
                        read_u32(data, off + 8),
                        0,
                        read_u32(data, off + 12),
                        read_u32(data, off + 16),
                        read_u32(data, off + 20),
                    )
                } else {
                    (0, 0, 0, 0, 0, 0)
                };
            // objects_* land at 32/36. Current 40 B stops there (the
            // counters come in TRAFFIC_STATS / DELIVERED_STATS); older
            // captures carry them inline. Typed ingest reads by name;
            // untyped needs the packed 104-byte (64-bit bytes) or the
            // earlier 92-byte layout.
            let (objects_up, objects_dn) = if r.typed() || size >= 40 {
                (r.u32("objects_up", 32), r.u32("objects_dn", 36))
            } else {
                (0, 0)
            };
            let (
                packets_up,
                packets_dn,
                bytes_up,
                bytes_dn,
                joins_up,
                joins_dn,
                switches_up,
                switches_dn,
                packets_dlvr,
                bytes_dlvr,
                joins_dlvr,
                switches_dlvr,
                new_clients,
            ) = if r.typed() || size >= 104 {
                (
                    r.u32("packets_up", 40),
                    r.u32("packets_dn", 44),
                    r.u64("bytes_up", 48),
                    r.u64("bytes_dn", 56),
                    r.u32("joins_up", 64),
                    r.u32("joins_dn", 68),
                    r.u32("switches_up", 72),
                    r.u32("switches_dn", 76),
                    r.u32("packets_dlvr", 80),
                    r.u64("bytes_dlvr", 84),
                    r.u32("joins_dlvr", 92),
                    r.u32("switches_dlvr", 96),
                    r.u32("new_clients", 100),
                )
            } else if size >= 92 {
                (
                    r.u32("packets_up", 40),
                    r.u32("packets_dn", 44),
                    u64::from(r.u32("bytes_up", 48)),
                    u64::from(r.u32("bytes_dn", 52)),
                    r.u32("joins_up", 56),
                    r.u32("joins_dn", 60),
                    r.u32("switches_up", 64),
                    r.u32("switches_dn", 68),
                    r.u32("packets_dlvr", 72),
                    u64::from(r.u32("bytes_dlvr", 76)),
                    r.u32("joins_dlvr", 80),
                    r.u32("switches_dlvr", 84),
                    r.u32("new_clients", 88),
                )
            } else {
                (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
            };
            let (clients_up, clients_dn) = if r.typed() || size >= 112 {
                (r.u32("clients_up", 104), r.u32("clients_dn", 108))
            } else {
                (0, 0)
            };
            let subscribers_dlvr = if r.typed() || size >= 116 {
                r.u32("subscribers_dlvr", 112)
            } else {
                0
            };
            prof.counts_total.push(EllaChannelCountTotal {
                ts,
                provider_id,
                channels_up: r.u32("channels_up", 0),
                channels_dn: r.u32("channels_dn", 4),
                channels_pending_leave_up: leave_up,
                channels_pending_leave_dn: leave_dn,
                channels_pending_retire_up: retire_up,
                channels_pending_retire_dn: retire_dn,
                subscribers_up: sub_up,
                subscribers_dn: sub_dn,
                objects_up,
                objects_dn,
                packets_up,
                packets_dn,
                bytes_up,
                bytes_dn,
                joins_up,
                joins_dn,
                switches_up,
                switches_dn,
                leaves_up: 0,
                leaves_dn: 0,
                expiries_up: 0,
                expiries_dn: 0,
                rejects_up: 0,
                rejects_dn: 0,
                packets_dlvr,
                bytes_dlvr,
                joins_dlvr,
                switches_dlvr,
                new_clients,
                clients_up,
                clients_dn,
                subscribers_dlvr,
            });
            true
        }
        EVT_ELLA_RELAY_TRAFFIC_STATS if r.typed() || size >= 41 => {
            let side = r.u8("side", 0);
            let packets = r.u32("packets", 1);
            let bytes = r.u64("bytes", 5);
            let joins = r.u32("joins", 13);
            let switches = r.u32("switches", 17);
            let leaves = r.u32("leaves", 21);
            let expiries = r.u32("expiries", 25);
            let rejects = r.u32("rejects", 29);
            let new_clients = r.u32("new_clients", 33);
            let clients = r.u32("clients", 37);
            if let Some(t) = latest_total(prof, provider_id) {
                if side == ELLA_SIDE_UPSTREAM {
                    t.packets_up = packets;
                    t.bytes_up = bytes;
                    t.joins_up = joins;
                    t.switches_up = switches;
                    t.leaves_up = leaves;
                    t.expiries_up = expiries;
                    t.rejects_up = rejects;
                    t.clients_up = clients;
                } else {
                    t.packets_dn = packets;
                    t.bytes_dn = bytes;
                    t.joins_dn = joins;
                    t.switches_dn = switches;
                    t.leaves_dn = leaves;
                    t.expiries_dn = expiries;
                    t.rejects_dn = rejects;
                    t.clients_dn = clients;
                }
                t.new_clients = t.new_clients.saturating_add(new_clients);
            }
            true
        }
        EVT_ELLA_RELAY_DELIVERED_STATS if r.typed() || size >= 24 => {
            if let Some(t) = latest_total(prof, provider_id) {
                t.packets_dlvr = r.u32("packets", 0);
                t.bytes_dlvr = r.u64("bytes", 4);
                t.joins_dlvr = r.u32("joins", 12);
                t.switches_dlvr = r.u32("switches", 16);
                t.subscribers_dlvr = r.u32("subscriptions", 20);
            }
            true
        }
        EVT_ELLA_RELAY_CHANNEL_SAMPLE if r.typed() || size >= 24 => {
            let s = EllaChannelSample {
                ts,
                provider_id,
                session_id,
                instance_id: r.u32("instance_id", 0),
                subscribers: r.u32("subscribers", 4),
                subscribers_pending: r.u32("subscribers_pending", 8),
                subscribers_leaving: r.u32("subscribers_leaving", 12),
                objects_completed: r.u32("objects_completed", 16),
                object_bytes: r.u32("object_bytes", 20),
                packets: if r.typed() || size >= 28 {
                    r.u32("packets", 24)
                } else {
                    0
                },
                inproc_attached: if r.typed() || size >= 32 {
                    r.u32("inproc_attached", 28)
                } else {
                    0
                },
                publish_no_ring: if r.typed() || size >= 36 {
                    r.u32("publish_no_ring", 32)
                } else {
                    0
                },
                drain_skipped_pending: if r.typed() || size >= 40 {
                    r.u32("drain_skipped_pending", 36)
                } else {
                    0
                },
            };
            apply_channel_sample(prof, &s);
            prof.channel_samples.push(s);
            true
        }
        EVT_ELLA_RELAY_FRAME_SAMPLE if r.typed() || size >= 32 => {
            prof.frame_samples.push(EllaFrameSample {
                ts,
                provider_id,
                session_id,
                instance_id: r.u32("instance_id", 0),
                frames: r.u32("frames", 4),
                frames_stretched: r.u32("frames_stretched", 8),
                sum_frame_len: r.u32("sum_frame_len", 12),
                sum_planned_us: r.u32("sum_planned_us", 16),
                sum_actual_us: r.u32("sum_actual_us", 20),
                sum_fwd_us: r.u32("sum_fwd_us", 24),
                max_overshoot_us: r.u32("max_overshoot_us", 28),
                sum_packets: if r.typed() || size >= 36 {
                    r.u32("sum_packets", 32)
                } else {
                    0
                },
            });
            true
        }
        EVT_ELLA_RELAY_OFFLOAD_STATS if r.typed() || size >= 32 => {
            let tail = r.typed() || size >= 48;
            prof.offload_samples.push(EllaOffloadSample {
                ts,
                provider_id,
                send_seq_ok: r.u32("send_seq_ok", 0),
                send_seq_stale: r.u32("send_seq_stale", 4),
                rxring_routed: r.u32("rxring_routed", 8),
                rxring_else_gate: r.u32("rxring_else_gate", 12),
                rxring_else_unreg: r.u32("rxring_else_unreg", 16),
                rxring_else_malformed: r.u32("rxring_else_malformed", 20),
                rxring_overwrites: r.u32("rxring_overwrites", 24),
                rxring_dropped_unreg: r.u32("rxring_dropped_unreg", 28),
                rxring_result: if tail { r.u32("rxring_result", 32) } else { 0 },
                rxring_port: if tail { r.u32("rxring_port", 36) } else { 0 },
                rxring_errno: if tail { r.u32("rxring_errno", 40) } else { 0 },
                rxring_region_bytes: if tail {
                    r.u32("rxring_region_bytes", 44)
                } else {
                    0
                },
                rxring_else_nocls: if r.typed() || size >= 52 {
                    r.u32("rxring_else_nocls", 48)
                } else {
                    0
                },
            });
            true
        }
        EVT_ELLA_RELAY_OBJECT_TIMING if r.typed() || size >= 36 => {
            // Untyped 40 B carries first_packet after object_id.
            let o = if r.typed() || size < 40 { 0 } else { 4 };
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                EllaPayload::ObjectTiming {
                    instance_id: r.u32("instance_id", 0),
                    object_id: r.u32("object_id", 4),
                    first_packet: if r.typed() || o != 0 {
                        r.u32("first_packet", 8)
                    } else {
                        0
                    },
                    packets_sent: r.u32("packets_sent", 8 + o),
                    packets_total: r.u32("packets_total", 12 + o),
                    packets_unsent: r.u32("packets_unsent", 16 + o),
                    start_lateness_us: r.i32("start_lateness_us", 20 + o),
                    signature_duration_us: r.u32("signature_duration_us", 24 + o),
                    encode_duration_us: r.u32("encode_duration_us", 28 + o),
                    enqueue_duration_us: r.u32("enqueue_duration_us", 32 + o),
                },
            );
            true
        }
        EVT_ELLA_RELAY_FRAME_SENT if r.typed() || size >= 38 => {
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                EllaPayload::FrameSent {
                    instance_id: r.u32("instance_id", 0),
                    object_id: r.u32("object_id", 4),
                    frame_index: r.u32("frame_index", 8),
                    first_packet: r.u32("first_packet", 12),
                    packets: r.u32("packets", 16),
                    is_probe: r.u8("is_probe", 20) != 0,
                    toggle: r.u8("toggle", 21) != 0,
                    frame_len: r.u32("frame_len", 22),
                    planned_send_us: r.u32("planned_send_us", 26),
                    actual_send_us: r.u32("actual_send_us", 30),
                    start_lateness_us: r.i32("start_lateness_us", 34),
                },
            );
            true
        }
        EVT_ELLA_RELAY_FRAME_RELAY if r.typed() || size >= 34 => {
            push_event(
                prof,
                ts,
                cpu,
                tid,
                provider_id,
                session_id,
                event_id,
                EllaPayload::FrameRelay {
                    instance_id: r.u32("instance_id", 0),
                    first_packet: r.u32("first_packet", 4),
                    packets: r.u32("packets", 8),
                    toggle: r.u8("toggle", 12) != 0,
                    boundary_index: r.u8("boundary_index", 13),
                    frame_len: r.u32("frame_len", 14),
                    wire_send_us: r.u32("wire_send_us", 18),
                    recv_us: r.u32("recv_us", 22),
                    fwd_us: r.u32("fwd_us", 26),
                    residency_us: r.u32("residency_us", 30),
                },
            );
            true
        }
        _ => false,
    }
}

fn latest_total(prof: &mut EllaProfile, provider_id: u16) -> Option<&mut EllaChannelCountTotal> {
    prof.counts_total
        .iter_mut()
        .rev()
        .find(|s| s.provider_id == provider_id)
}

/// After samples are time-sorted, peak occupancy is the max sum of
/// live `subscribers` across an envelope session's instances.
pub fn apply_channel_sample_peaks(prof: &mut EllaProfile) {
    if prof.channel_samples.is_empty() {
        return;
    }
    let mut last: FxHashMap<(u16, u64, u32), u32> = FxHashMap::default();
    let mut sess_sum: FxHashMap<(u16, u64), u32> = FxHashMap::default();
    let mut peak: FxHashMap<(u16, u64), u32> = FxHashMap::default();
    for s in &prof.channel_samples {
        if s.session_id == ELLA_RELAY_GLOBAL_SESSION {
            continue;
        }
        let key = (s.provider_id, s.session_id, s.instance_id);
        let old = last.insert(key, s.subscribers).unwrap_or(0);
        let sum = sess_sum.entry((s.provider_id, s.session_id)).or_insert(0);
        *sum = sum.saturating_sub(old).saturating_add(s.subscribers);
        let p = peak.entry((s.provider_id, s.session_id)).or_insert(0);
        if *sum > *p {
            *p = *sum;
        }
    }
    for ((pid, sid), n) in peak {
        if let Some(ch) = prof.channels.get_mut(&(pid, sid)) {
            ch.peak_subscribers = n;
            ch.last_subscribers = sess_sum.get(&(pid, sid)).copied().unwrap_or(0);
        }
    }
}

/// A `CHANNEL_SAMPLE` is the producer's own occupancy reading for one
/// instance; trust it over the SUBSCRIBED-minus-UNSUBSCRIBED delta.
/// Identity still comes from SESSION_CREATE / lifecycle: a sample on a
/// session the capture never opened is leftover ring data, not a title.
fn apply_channel_sample(prof: &mut EllaProfile, s: &EllaChannelSample) {
    if s.session_id == ELLA_RELAY_GLOBAL_SESSION {
        return;
    }
    let Some(entry) = prof.channels.get_mut(&(s.provider_id, s.session_id)) else {
        return;
    };
    entry.event_count += 1;
    if s.ts < entry.first_ts {
        entry.first_ts = s.ts;
    }
    if s.ts > entry.last_ts {
        entry.last_ts = s.ts;
    }
    entry.occ_by_inst.insert(s.instance_id, s.subscribers);
    let total: u32 = entry.occ_by_inst.values().copied().sum();
    if total > entry.peak_subscribers {
        entry.peak_subscribers = total;
    }
}

/// Canonical `subscriber_state` value for the raw one on the wire.
/// ella#460 inserted `UNSUBSCRIBING` at 2 and moved the terminal
/// `UNSUBSCRIBED` to 3, so the blob's enum table decides which a value
/// is; a capture that carries no table for the field predates the
/// insert, where 2 was terminal.
fn subscriber_state(r: &PayloadReader, raw: u8) -> u8 {
    match r.enum_name("state", raw as u32) {
        Some("SUBSCRIBING") => ELLA_SUBSCRIBER_SUBSCRIBING,
        Some("SUBSCRIBED") => ELLA_SUBSCRIBER_SUBSCRIBED,
        Some("UNSUBSCRIBING") => ELLA_SUBSCRIBER_UNSUBSCRIBING,
        Some("UNSUBSCRIBED") => ELLA_SUBSCRIBER_UNSUBSCRIBED,
        Some(_) => raw,
        None if raw == 2 => ELLA_SUBSCRIBER_UNSUBSCRIBED,
        None => raw,
    }
}

#[allow(clippy::too_many_arguments)]
fn push_event(
    prof: &mut EllaProfile,
    ts: u64,
    cpu: u16,
    tid: i32,
    provider_id: u16,
    session_id: u64,
    event_id: u32,
    payload: EllaPayload,
) {
    apply_to_channel(prof, provider_id, session_id, ts, &payload);
    prof.events.push(EllaEvent {
        ts,
        cpu,
        tid,
        provider_id,
        session_id,
        event_id,
        payload,
    });
}

fn apply_to_channel(
    prof: &mut EllaProfile,
    provider_id: u16,
    session_id: u64,
    ts: u64,
    payload: &EllaPayload,
) {
    if session_id == ELLA_RELAY_GLOBAL_SESSION {
        return;
    }
    let entry = prof
        .channels
        .entry((provider_id, session_id))
        .or_insert_with(|| EllaChannelInfo {
            provider_id,
            session_id,
            channel_name: String::new(),
            version: 0,
            first_ts: ts,
            last_ts: ts,
            event_count: 0,
            channel_ids: FxHashMap::default(),
            last_subscribers: 0,
            peak_subscribers: 0,
            occ_by_inst: FxHashMap::default(),
            last_state: None,
            destroy_reason: None,
        });
    entry.event_count += 1;
    if ts < entry.first_ts {
        entry.first_ts = ts;
    }
    if ts > entry.last_ts {
        entry.last_ts = ts;
    }
    match payload {
        EllaPayload::SessionCreate {
            channel_name,
            version,
            ..
        } => {
            if entry.channel_name.is_empty() && !channel_name.is_empty() {
                entry.channel_name = channel_name.clone();
            }
            entry.version = *version;
        }
        EllaPayload::ChannelState {
            state, subscribers, ..
        } => {
            entry.last_state = Some(*state);
            entry.last_subscribers = *subscribers;
        }
        EllaPayload::ChannelDestroyed {
            reason,
            instance_id,
            subscribers,
        } => {
            entry.destroy_reason = Some(*reason);
            entry.last_subscribers = *subscribers;
            entry.occ_by_inst.insert(*instance_id, 0);
        }
        EllaPayload::ChannelIdAssigned {
            instance_id,
            channel_id,
        } => {
            entry.channel_ids.insert(*instance_id, *channel_id);
        }
        EllaPayload::ChannelSubscriber {
            state,
            instance_id,
            subscribers,
            ..
        } => {
            entry.last_subscribers = *subscribers;
            let n = entry.occ_by_inst.entry(*instance_id).or_insert(0);
            match *state {
                ELLA_SUBSCRIBER_SUBSCRIBED => *n = n.saturating_add(1),
                ELLA_SUBSCRIBER_UNSUBSCRIBED => *n = n.saturating_sub(1),
                _ => {}
            }
            let total: u32 = entry.occ_by_inst.values().copied().sum();
            if total > entry.peak_subscribers {
                entry.peak_subscribers = total;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad_align(buf: &mut Vec<u8>, align: usize) {
        while !buf.len().is_multiple_of(align) {
            buf.push(0);
        }
    }

    fn put_u32(buf: &mut Vec<u8>, v: u32) {
        pad_align(buf, 4);
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn put_u16(buf: &mut Vec<u8>, v: u16) {
        pad_align(buf, 2);
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn put_u8(buf: &mut Vec<u8>, v: u8) {
        buf.push(v);
    }

    fn put_cstr(buf: &mut Vec<u8>, s: &str, width: usize) {
        let mut v = vec![0u8; width];
        let n = s.len().min(width);
        v[..n].copy_from_slice(&s.as_bytes()[..n]);
        buf.extend_from_slice(&v);
    }

    fn ingest(prof: &mut EllaProfile, session_id: u64, event_id: u32, payload: &[u8]) -> bool {
        ingest_ella_relay(
            prof,
            1,
            0,
            0,
            6,
            session_id,
            event_id,
            payload,
            0,
            payload.len(),
            None,
        )
    }

    #[test]
    fn session_create_offsets() {
        let mut b = Vec::new();
        put_cstr(&mut b, "live/1", 64);
        put_u8(&mut b, 2);
        put_u32(&mut b, 1);
        assert_eq!(b.len(), 72);
        let mut prof = EllaProfile::default();
        assert!(ingest(&mut prof, 7, EVT_ELLA_RELAY_SESSION_CREATE, &b));
        match &prof.events[0].payload {
            EllaPayload::SessionCreate {
                channel_name,
                version,
                instance_id,
            } => {
                assert_eq!(channel_name, "live/1");
                assert_eq!(*version, 2);
                assert_eq!(*instance_id, 1);
            }
            other => panic!("unexpected {other:?}"),
        }
        let ch = &prof.channels[&(6, 7)];
        assert_eq!(ch.channel_name, "live/1");
        assert_eq!(ch.version, 2);
    }

    #[test]
    fn channel_subscriber_offsets() {
        let mut b = vec![1, 0];
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&5000u16.to_le_bytes());
        b.push(4);
        assert_eq!(b.len(), 13);
        b.extend_from_slice(&[10, 0, 0, 1]);
        let mut prof = EllaProfile::default();
        assert!(ingest(&mut prof, 7, EVT_ELLA_RELAY_CHANNEL_SUBSCRIBER, &b));
        match &prof.events[0].payload {
            EllaPayload::ChannelSubscriber {
                state,
                instance_id,
                subscriber_address,
                subscriber_port,
                subscribers,
                ..
            } => {
                assert_eq!(*state, 1);
                assert_eq!(*instance_id, 2);
                assert_eq!(subscriber_address, "10.0.0.1");
                assert_eq!(*subscriber_port, 5000);
                assert_eq!(*subscribers, 4);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(prof.channels[&(6, 7)].last_subscribers, 4);
    }

    #[test]
    fn object_state_offsets() {
        let mut b = Vec::new();
        put_u8(&mut b, 1);
        put_u32(&mut b, 2);
        put_u8(&mut b, 3);
        put_u32(&mut b, 10);
        put_u32(&mut b, 8);
        put_u32(&mut b, 1000);
        assert_eq!(b.len(), 24);
        let mut prof = EllaProfile::default();
        assert!(ingest(&mut prof, 7, EVT_ELLA_RELAY_OBJECT_STATE, &b));
        match &prof.events[0].payload {
            EllaPayload::ObjectState {
                state,
                instance_id,
                boundary_index,
                first_packet,
                packets,
                bytes,
            } => {
                assert_eq!(*state, 1);
                assert_eq!(*instance_id, 2);
                assert_eq!(*boundary_index, 3);
                assert_eq!(*first_packet, 10);
                assert_eq!(*packets, 8);
                assert_eq!(*bytes, 1000);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(prof.channels[&(6, 7)].event_count, 1);
    }

    #[test]
    fn packed_payload_offsets() {
        let mut prof = EllaProfile::default();

        let mut session = vec![0; 64];
        session[..6].copy_from_slice(b"live/1");
        session.push(2);
        session.extend_from_slice(&3u32.to_le_bytes());
        assert!(ingest(
            &mut prof,
            7,
            EVT_ELLA_RELAY_SESSION_CREATE,
            &session
        ));

        let mut state = vec![ELLA_LIFECYCLE_CREATED, 0];
        state.extend_from_slice(&3u32.to_le_bytes());
        state.extend_from_slice(&11u32.to_le_bytes());
        assert!(ingest(&mut prof, 7, EVT_ELLA_RELAY_CHANNEL_STATE, &state));

        let mut error = vec![4];
        error.extend_from_slice(&3u32.to_le_bytes());
        error.extend_from_slice(&99u32.to_le_bytes());
        assert!(ingest(&mut prof, 7, EVT_ELLA_RELAY_CHANNEL_ERROR, &error));

        let mut object = vec![ELLA_OBJECT_COMPLETED];
        object.extend_from_slice(&3u32.to_le_bytes());
        object.push(2);
        object.extend_from_slice(&10u32.to_le_bytes());
        object.extend_from_slice(&8u32.to_le_bytes());
        object.extend_from_slice(&1000u32.to_le_bytes());
        assert!(ingest(&mut prof, 7, EVT_ELLA_RELAY_OBJECT_STATE, &object));

        let mut destroyed = vec![1];
        destroyed.extend_from_slice(&3u32.to_le_bytes());
        destroyed.extend_from_slice(&11u32.to_le_bytes());
        assert!(ingest(
            &mut prof,
            7,
            EVT_ELLA_RELAY_CHANNEL_DESTROYED,
            &destroyed
        ));

        assert!(matches!(
            prof.events[1].payload,
            EllaPayload::ChannelState {
                instance_id: 3,
                subscribers: 11,
                ..
            }
        ));
        assert!(matches!(
            prof.events[2].payload,
            EllaPayload::ChannelError {
                instance_id: 3,
                detail: 99,
                ..
            }
        ));
        assert!(matches!(
            prof.events[3].payload,
            EllaPayload::ObjectState {
                instance_id: 3,
                boundary_index: 2,
                first_packet: 10,
                packets: 8,
                bytes: 1000,
                ..
            }
        ));
        assert!(matches!(
            prof.events[4].payload,
            EllaPayload::ChannelDestroyed {
                instance_id: 3,
                subscribers: 11,
                ..
            }
        ));
    }

    #[test]
    fn channel_count_and_global_session_skip_channel_table() {
        let mut b = Vec::new();
        for v in [1u32, 3, 0, 1, 8, 2] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 24);
        let mut prof = EllaProfile::default();
        assert!(ingest(
            &mut prof,
            ELLA_RELAY_GLOBAL_SESSION,
            EVT_ELLA_RELAY_CHANNEL_COUNT,
            &b
        ));
        assert!(prof.channels.is_empty());
        assert_eq!(prof.counts.len(), 1);
        let s = &prof.counts[0];
        assert_eq!(s.instance_id, 1);
        assert_eq!(s.channels, 3);
        assert_eq!(s.subscribers, 8);
        assert_eq!(s.sessions, 2);
    }

    #[test]
    fn channel_count_five_u32s() {
        let mut b = Vec::new();
        for v in [1u32, 3, 0, 1, 8] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 20);
        let mut prof = EllaProfile::default();
        assert!(ingest(
            &mut prof,
            ELLA_RELAY_GLOBAL_SESSION,
            EVT_ELLA_RELAY_CHANNEL_COUNT,
            &b
        ));
        let s = &prof.counts[0];
        assert_eq!(s.instance_id, 1);
        assert_eq!(s.channels, 3);
        assert_eq!(s.subscribers, 8);
        assert_eq!(s.sessions, 0);
    }

    #[test]
    fn channel_sample_offsets() {
        let mut b = Vec::new();
        for v in [2u32, 5, 1, 3, 7, 1200] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 24);
        let mut prof = EllaProfile::default();
        assert!(ingest(&mut prof, 7, EVT_ELLA_RELAY_CHANNEL_SAMPLE, &b));
        assert_eq!(prof.channel_samples.len(), 1);
        let s = &prof.channel_samples[0];
        assert_eq!(s.session_id, 7);
        assert_eq!(s.instance_id, 2);
        assert_eq!(s.subscribers, 5);
        assert_eq!(s.subscribers_pending, 1);
        assert_eq!(s.subscribers_leaving, 3);
        assert_eq!(s.objects_completed, 7);
        assert_eq!(s.object_bytes, 1200);
        assert_eq!(s.packets, 0);
        assert!(prof.channels.is_empty());
    }

    #[test]
    fn channel_sample_with_packets() {
        let mut b = Vec::new();
        for v in [2u32, 5, 1, 3, 7, 1200, 4] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 28);
        let mut prof = EllaProfile::default();
        assert!(ingest(&mut prof, 7, EVT_ELLA_RELAY_CHANNEL_SAMPLE, &b));
        assert_eq!(prof.channel_samples[0].packets, 4);
    }

    #[test]
    fn channel_stats_cdn_counters() {
        let mut b = Vec::new();
        for v in [
            1u32, 2, 0, 0, 0, 0, 3, 4, 5, 6, 10, 20, 1000, 2000, 1, 2, 0, 0, 30, 3000, 4, 0, 7,
        ] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 92);
        let mut prof = EllaProfile::default();
        assert!(ingest(
            &mut prof,
            ELLA_RELAY_GLOBAL_SESSION,
            EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
            &b
        ));
        let s = &prof.counts_total[0];
        assert_eq!(s.objects_up, 5);
        assert_eq!(s.objects_dn, 6);
        assert_eq!(s.packets_up, 10);
        assert_eq!(s.packets_dn, 20);
        assert_eq!(s.bytes_up, 1000);
        assert_eq!(s.bytes_dn, 2000);
        assert_eq!(s.joins_up, 1);
        assert_eq!(s.joins_dn, 2);
        assert_eq!(s.packets_dlvr, 30);
        assert_eq!(s.bytes_dlvr, 3000);
        assert_eq!(s.joins_dlvr, 4);
        assert_eq!(s.new_clients, 7);
    }

    #[test]
    fn channel_stats_cdn_counters_64bit_bytes() {
        let mut b = Vec::new();
        for v in [1u32, 2, 0, 0, 0, 0, 3, 4, 5, 6, 10, 20] {
            put_u32(&mut b, v);
        }
        b.extend_from_slice(&5_000_000_000u64.to_le_bytes());
        b.extend_from_slice(&6_000_000_000u64.to_le_bytes());
        for v in [1u32, 2, 0, 0, 30] {
            put_u32(&mut b, v);
        }
        b.extend_from_slice(&7_000_000_000u64.to_le_bytes());
        for v in [4u32, 0, 7] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 104);
        let mut prof = EllaProfile::default();
        assert!(ingest(
            &mut prof,
            ELLA_RELAY_GLOBAL_SESSION,
            EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
            &b
        ));
        assert_eq!(prof.counts_total[0].bytes_dlvr, 7_000_000_000);
        assert_eq!(prof.counts_total[0].clients_dn, 0);
        for v in [64u32, 190_000, 400_000] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 116);
        let mut prof = EllaProfile::default();
        assert!(ingest(
            &mut prof,
            ELLA_RELAY_GLOBAL_SESSION,
            EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
            &b
        ));
        let s = &prof.counts_total[0];
        assert_eq!(s.packets_dn, 20);
        assert_eq!(s.bytes_up, 5_000_000_000);
        assert_eq!(s.bytes_dn, 6_000_000_000);
        assert_eq!(s.joins_dn, 2);
        assert_eq!(s.packets_dlvr, 30);
        assert_eq!(s.bytes_dlvr, 7_000_000_000);
        assert_eq!(s.joins_dlvr, 4);
        assert_eq!(s.new_clients, 7);
        assert_eq!(s.clients_up, 64);
        assert_eq!(s.clients_dn, 190_000);
        assert_eq!(s.subscribers_dlvr, 400_000);
    }

    #[test]
    fn object_timing_offsets() {
        let mut b = Vec::new();
        for v in [1u32, 42, 8, 10, 2] {
            put_u32(&mut b, v);
        }
        put_u32(&mut b, (-1500i32) as u32);
        for v in [100u32, 200, 50] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 36);
        let mut prof = EllaProfile::default();
        assert!(ingest(&mut prof, 9, EVT_ELLA_RELAY_OBJECT_TIMING, &b));
        assert_eq!(prof.events.len(), 1);
        match &prof.events[0].payload {
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
            } => {
                assert_eq!(*instance_id, 1);
                assert_eq!(*object_id, 42);
                assert_eq!(*first_packet, 0);
                assert_eq!(*packets_sent, 8);
                assert_eq!(*packets_total, 10);
                assert_eq!(*packets_unsent, 2);
                assert_eq!(*start_lateness_us, -1500);
                assert_eq!(*signature_duration_us, 100);
                assert_eq!(*encode_duration_us, 200);
                assert_eq!(*enqueue_duration_us, 50);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn object_timing_first_packet() {
        use crate::elog_parser::schema::test_support::{build_blob, FieldSpec};
        let mut b = Vec::new();
        for v in [1u32, 42, 7000, 8, 10, 2] {
            put_u32(&mut b, v);
        }
        put_u32(&mut b, (-1500i32) as u32);
        for v in [100u32, 200, 50] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 40);
        let names = [
            "instance_id",
            "object_id",
            "first_packet",
            "packets_sent",
            "packets_total",
            "packets_unsent",
            "start_lateness_us",
            "signature_duration_us",
            "encode_duration_us",
            "enqueue_duration_us",
        ];
        let fields = names
            .iter()
            .enumerate()
            .map(|(i, n)| FieldSpec::scalar(n, DECODE_TYPE_U32, 4 * i as u16))
            .collect();
        let schema = ProviderSchema::parse(&build_blob(
            "fanout_node",
            "OBJECT_TIMING",
            fields,
            EVT_ELLA_RELAY_OBJECT_TIMING,
            "OBJECT_TIMING",
            "",
        ))
        .unwrap();
        for s in [None, Some(&schema)] {
            let mut prof = EllaProfile::default();
            assert!(ingest_ella_relay(
                &mut prof,
                1,
                0,
                0,
                6,
                9,
                EVT_ELLA_RELAY_OBJECT_TIMING,
                &b,
                0,
                b.len(),
                s,
            ));
            assert!(matches!(
                prof.events[0].payload,
                EllaPayload::ObjectTiming {
                    object_id: 42,
                    first_packet: 7000,
                    packets_sent: 8,
                    packets_total: 10,
                    packets_unsent: 2,
                    start_lateness_us: -1500,
                    enqueue_duration_us: 50,
                    ..
                }
            ));
        }
    }

    #[test]
    fn traffic_and_delivered_stats_fold_into_channel_stats() {
        let mut stats = Vec::new();
        for v in [1u32, 2, 0, 0, 0, 0, 3, 4, 5, 6] {
            put_u32(&mut stats, v);
        }
        assert_eq!(stats.len(), 40);
        let traffic = |side: u8, base: u32| {
            let mut b = vec![side];
            b.extend_from_slice(&base.to_le_bytes());
            b.extend_from_slice(&(u64::from(base) * 1_000_000).to_le_bytes());
            for k in 1..=7u32 {
                b.extend_from_slice(&(base + k).to_le_bytes());
            }
            assert_eq!(b.len(), 41);
            b
        };
        let mut delivered = Vec::new();
        delivered.extend_from_slice(&30u32.to_le_bytes());
        delivered.extend_from_slice(&7_000_000_000u64.to_le_bytes());
        for v in [4u32, 1, 400] {
            delivered.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(delivered.len(), 24);

        let g = ELLA_RELAY_GLOBAL_SESSION;
        let mut prof = EllaProfile::default();
        assert!(ingest(
            &mut prof,
            g,
            EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
            &stats
        ));
        assert!(ingest(
            &mut prof,
            g,
            EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
            &stats
        ));
        assert!(ingest(
            &mut prof,
            g,
            EVT_ELLA_RELAY_TRAFFIC_STATS,
            &traffic(ELLA_SIDE_UPSTREAM, 10)
        ));
        assert!(ingest(
            &mut prof,
            g,
            EVT_ELLA_RELAY_TRAFFIC_STATS,
            &traffic(ELLA_SIDE_DOWNSTREAM, 20)
        ));
        assert!(ingest(
            &mut prof,
            g,
            EVT_ELLA_RELAY_DELIVERED_STATS,
            &delivered
        ));
        assert!(prof.events.is_empty());
        assert_eq!(prof.counts_total.len(), 2);
        assert_eq!(prof.counts_total[0].packets_up, 0);
        let s = &prof.counts_total[1];
        assert_eq!((s.objects_up, s.objects_dn), (5, 6));
        assert_eq!((s.packets_up, s.bytes_up), (10, 10_000_000));
        assert_eq!((s.joins_up, s.switches_up, s.leaves_up), (11, 12, 13));
        assert_eq!((s.expiries_up, s.rejects_up, s.clients_up), (14, 15, 17));
        assert_eq!((s.packets_dn, s.bytes_dn), (20, 20_000_000));
        assert_eq!((s.joins_dn, s.switches_dn, s.leaves_dn), (21, 22, 23));
        assert_eq!((s.expiries_dn, s.rejects_dn, s.clients_dn), (24, 25, 27));
        assert_eq!(s.new_clients, 16 + 26);
        assert_eq!((s.packets_dlvr, s.bytes_dlvr), (30, 7_000_000_000));
        assert_eq!((s.joins_dlvr, s.switches_dlvr), (4, 1));
        assert_eq!(s.subscribers_dlvr, 400);
        assert!(prof.has_cdn_counters());
    }

    #[test]
    fn subscriber_identity_v4_mapped_and_pbcid() {
        use crate::elog_parser::schema::test_support::{build_blob, FieldSpec};
        let schema = ProviderSchema::parse(&build_blob(
            "fanout_node",
            "SUBSCRIBER_IDENTITY",
            vec![
                FieldSpec::scalar("instance_id", DECODE_TYPE_U32, 0),
                FieldSpec::scalar("request_id", DECODE_TYPE_U32, 4),
                FieldSpec::scalar("subscriber_port", DECODE_TYPE_U16, 8),
                FieldSpec::scalar("subscriber_address", DECODE_TYPE_IN6_ADDR, 10),
                FieldSpec::scalar("pbcid_len", DECODE_TYPE_U8, 26),
                FieldSpec::varlen("pbcid", DECODE_TYPE_U8, 27, 5, 128),
            ],
            EVT_ELLA_RELAY_SUBSCRIBER_IDENTITY,
            "SUBSCRIBER_IDENTITY",
            "",
        ))
        .unwrap();
        let mut b = Vec::new();
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&77u32.to_le_bytes());
        b.extend_from_slice(&5000u16.to_le_bytes());
        b.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 192, 0, 2, 7]);
        b.push(6);
        b.extend_from_slice(b"pbc-42");
        assert_eq!(b.len(), 33);
        let mut prof = EllaProfile::default();
        assert!(ingest_ella_relay(
            &mut prof,
            1,
            0,
            0,
            6,
            7,
            EVT_ELLA_RELAY_SUBSCRIBER_IDENTITY,
            &b,
            0,
            b.len(),
            Some(&schema),
        ));
        match &prof.events[0].payload {
            EllaPayload::SubscriberIdentity {
                instance_id,
                request_id,
                subscriber_address,
                subscriber_port,
                pbcid,
            } => {
                assert_eq!((*instance_id, *request_id), (2, 77));
                assert_eq!(subscriber_address, "192.0.2.7");
                assert_eq!(*subscriber_port, 5000);
                assert_eq!(pbcid, "pbc-42");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(prof.channels[&(6, 7)].event_count, 1);
    }

    #[test]
    fn channel_sample_updates_existing_session() {
        let mut create = Vec::new();
        put_cstr(&mut create, "live/1", 64);
        put_u8(&mut create, 2);
        put_u32(&mut create, 1);
        let mut b = Vec::new();
        for v in [2u32, 5, 1, 3, 7, 1200] {
            put_u32(&mut b, v);
        }
        let mut prof = EllaProfile::default();
        assert!(ingest(&mut prof, 7, EVT_ELLA_RELAY_SESSION_CREATE, &create));
        assert!(ingest(&mut prof, 7, EVT_ELLA_RELAY_CHANNEL_SAMPLE, &b));
        let ch = &prof.channels[&(6, 7)];
        assert_eq!(ch.channel_name, "live/1");
        assert_eq!(ch.occ_by_inst[&2], 5);
        assert_eq!(ch.peak_subscribers, 5);
        apply_channel_sample_peaks(&mut prof);
        assert_eq!(prof.channels[&(6, 7)].peak_subscribers, 5);
    }

    #[test]
    fn frame_sample_offsets() {
        let mut b = Vec::new();
        for v in [2u32, 10, 3, 250_000, 1_000_000, 500_000, 0, 40] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 32);
        let mut prof = EllaProfile::default();
        assert!(ingest(&mut prof, 7, EVT_ELLA_RELAY_FRAME_SAMPLE, &b));
        assert_eq!(prof.frame_samples.len(), 1);
        let s = &prof.frame_samples[0];
        assert_eq!(s.session_id, 7);
        assert_eq!(s.instance_id, 2);
        assert_eq!(s.frames, 10);
        assert_eq!(s.frames_stretched, 3);
        assert_eq!(s.sum_frame_len, 250_000);
        assert_eq!(s.sum_planned_us, 1_000_000);
        assert_eq!(s.sum_actual_us, 500_000);
        assert_eq!(s.sum_fwd_us, 0);
        assert_eq!(s.max_overshoot_us, 40);
    }

    #[test]
    fn channel_sample_on_global_session_skips_channel_table() {
        let mut b = Vec::new();
        for v in [2u32, 5, 1, 3, 7, 1200] {
            put_u32(&mut b, v);
        }
        let mut prof = EllaProfile::default();
        assert!(ingest(
            &mut prof,
            ELLA_RELAY_GLOBAL_SESSION,
            EVT_ELLA_RELAY_CHANNEL_SAMPLE,
            &b
        ));
        assert_eq!(prof.channel_samples.len(), 1);
        assert!(prof.channels.is_empty());
    }

    #[test]
    fn channel_count_total_offsets() {
        let mut b = Vec::new();
        for v in [2u32, 10, 1, 5, 3, 7, 4, 40] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 32);
        let mut prof = EllaProfile::default();
        assert!(ingest(
            &mut prof,
            ELLA_RELAY_GLOBAL_SESSION,
            EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
            &b
        ));
        assert!(prof.counts.is_empty());
        let t = &prof.counts_total[0];
        assert_eq!(t.channels_up, 2);
        assert_eq!(t.channels_dn, 10);
        assert_eq!(t.channels_pending_leave_up, 1);
        assert_eq!(t.channels_pending_leave_dn, 5);
        assert_eq!(t.channels_pending_retire_up, 3);
        assert_eq!(t.channels_pending_retire_dn, 7);
        assert_eq!(t.subscribers_up, 4);
        assert_eq!(t.subscribers_dn, 40);
    }

    #[test]
    fn channel_count_total_summed_offsets() {
        let mut b = Vec::new();
        for v in [2u32, 10, 1, 3, 4, 40] {
            put_u32(&mut b, v);
        }
        assert_eq!(b.len(), 24);
        let mut prof = EllaProfile::default();
        assert!(ingest(
            &mut prof,
            ELLA_RELAY_GLOBAL_SESSION,
            EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
            &b
        ));
        let t = &prof.counts_total[0];
        assert_eq!(t.channels_up, 2);
        assert_eq!(t.channels_dn, 10);
        assert_eq!(t.channels_pending_leave_up, 0);
        assert_eq!(t.channels_pending_leave_dn, 1);
        assert_eq!(t.channels_pending_retire_up, 0);
        assert_eq!(t.channels_pending_retire_dn, 3);
        assert_eq!(t.subscribers_up, 4);
        assert_eq!(t.subscribers_dn, 40);
    }

    #[test]
    fn relay_error_offsets() {
        let mut b = Vec::new();
        put_u8(&mut b, 1);
        put_u32(&mut b, 3);
        put_u32(&mut b, 9);
        put_u8(&mut b, 1);
        put_u32(&mut b, 42);
        assert_eq!(b.len(), 20);
        let mut prof = EllaProfile::default();
        assert!(ingest(
            &mut prof,
            ELLA_RELAY_GLOBAL_SESSION,
            EVT_ELLA_RELAY_RELAY_ERROR,
            &b
        ));
        match &prof.events[0].payload {
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
                assert_eq!(*code, 1);
                assert_eq!(*instance_id, 3);
                assert_eq!(*channel_id, 9);
                assert!(*channel_id_known);
                assert_eq!(*detail, 42);
                assert_eq!(*message_type, 0);
                assert!(subscriber_address.is_empty());
                assert_eq!(*subscriber_port, 0);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(prof.channels.is_empty());
    }

    #[test]
    fn relay_error_extended_offsets() {
        let mut b = vec![42];
        b.extend_from_slice(&7u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&4242u16.to_le_bytes());
        b.push(0);
        b.push(0x12);
        b.push(16);
        assert_eq!(b.len(), 18);
        b.extend_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let mut prof = EllaProfile::default();
        assert!(ingest(
            &mut prof,
            ELLA_RELAY_GLOBAL_SESSION,
            EVT_ELLA_RELAY_RELAY_ERROR,
            &b
        ));
        match &prof.events[0].payload {
            EllaPayload::RelayError {
                code,
                instance_id,
                detail,
                message_type,
                subscriber_address,
                subscriber_port,
                ..
            } => {
                assert_eq!(*code, 42);
                assert_eq!(*instance_id, 7);
                assert_eq!(*detail, 0);
                assert_eq!(*message_type, 0x12);
                assert_eq!(subscriber_address, "2001:db8::1");
                assert_eq!(*subscriber_port, 4242);
                assert_eq!(ella_fanout_message_type_name(0xc6), "DATA_2");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn schema_packed_channel_state() {
        use crate::elog_parser::schema::test_support::{build_blob, FieldSpec};
        let schema = ProviderSchema::parse(&build_blob(
            "fanout_node",
            "CHANNEL_STATE",
            vec![
                FieldSpec::scalar("state", DECODE_TYPE_U8, 0),
                FieldSpec::scalar("reason", DECODE_TYPE_U8, 1),
                FieldSpec::scalar("instance_id", DECODE_TYPE_U32, 2),
                FieldSpec::scalar("subscribers", DECODE_TYPE_U32, 6),
            ],
            EVT_ELLA_RELAY_CHANNEL_STATE,
            "CHANNEL_STATE",
            "",
        ))
        .unwrap();
        let mut b = vec![3, 1];
        b.extend_from_slice(&9u32.to_le_bytes());
        b.extend_from_slice(&4u32.to_le_bytes());
        let mut prof = EllaProfile::default();
        assert!(ingest_ella_relay(
            &mut prof,
            1,
            0,
            0,
            6,
            7,
            EVT_ELLA_RELAY_CHANNEL_STATE,
            &b,
            0,
            b.len(),
            Some(&schema),
        ));
        match &prof.events[0].payload {
            EllaPayload::ChannelState {
                state,
                reason,
                instance_id,
                subscribers,
            } => {
                assert_eq!((*state, *reason, *instance_id, *subscribers), (3, 1, 9, 4));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    fn subscriber_payload(state: u8) -> Vec<u8> {
        let mut b = vec![state, 0];
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&5000u16.to_le_bytes());
        b.push(4);
        b.extend_from_slice(&[10, 0, 0, 1]);
        b
    }

    fn subscriber_schema(values: Vec<(u32, &'static str)>) -> ProviderSchema {
        use crate::elog_parser::schema::test_support::{
            build_blob_enums, EnumSpec, EventSpec, FieldSpec, StructSpec,
        };
        ProviderSchema::parse(&build_blob_enums(
            "fanout_node",
            &[EnumSpec {
                name: "subscriber_state",
                values,
            }],
            &[StructSpec {
                name: "SUBSCRIBER_STATE",
                fields: vec![
                    FieldSpec::enumerated("state", DECODE_TYPE_U8, 0, 1),
                    FieldSpec::scalar("reason", DECODE_TYPE_U8, 1),
                    FieldSpec::scalar("instance_id", DECODE_TYPE_U32, 2),
                    FieldSpec::scalar("subscribers", DECODE_TYPE_U32, 6),
                    FieldSpec::scalar("subscriber_port", DECODE_TYPE_U16, 10),
                    FieldSpec::scalar("subscriber_address_len", DECODE_TYPE_U8, 12),
                    FieldSpec::scalar("subscriber_address", DECODE_TYPE_U8, 13),
                ],
            }],
            &[EventSpec {
                id: EVT_ELLA_RELAY_CHANNEL_SUBSCRIBER,
                name: "SUBSCRIBER_STATE",
                struct_index: 1,
                format: "",
            }],
        ))
        .unwrap()
    }

    fn ingest_subscriber(prof: &mut EllaProfile, schema: Option<&ProviderSchema>, state: u8) {
        let b = subscriber_payload(state);
        assert!(ingest_ella_relay(
            prof,
            1,
            0,
            0,
            6,
            7,
            EVT_ELLA_RELAY_CHANNEL_SUBSCRIBER,
            &b,
            0,
            b.len(),
            schema,
        ));
    }

    /// ella#460 moved the terminal state to 3; the blob's enum table is
    /// what says so, and occupancy has to drop on it.
    #[test]
    fn subscriber_leave_is_the_enum_table_terminal_state() {
        let schema = subscriber_schema(vec![
            (0, "SUBSCRIBING"),
            (1, "SUBSCRIBED"),
            (2, "UNSUBSCRIBING"),
            (3, "UNSUBSCRIBED"),
        ]);
        let mut prof = EllaProfile::default();
        ingest_subscriber(&mut prof, Some(&schema), 1);
        ingest_subscriber(&mut prof, Some(&schema), 3);
        let states: Vec<u8> = prof
            .events
            .iter()
            .filter_map(|e| match e.payload {
                EllaPayload::ChannelSubscriber { state, .. } => Some(state),
                _ => None,
            })
            .collect();
        assert_eq!(
            states,
            vec![ELLA_SUBSCRIBER_SUBSCRIBED, ELLA_SUBSCRIBER_UNSUBSCRIBED]
        );
        assert_eq!(prof.channels[&(6, 7)].occ_by_inst[&2], 0);
        assert_eq!(prof.channels[&(6, 7)].peak_subscribers, 1);
    }

    /// An upstream subscription's `UNSUBSCRIBING` is a transition, not a
    /// leave — the terminal `UNSUBSCRIBED` still follows it.
    #[test]
    fn subscriber_unsubscribing_holds_occupancy() {
        let schema = subscriber_schema(vec![
            (0, "SUBSCRIBING"),
            (1, "SUBSCRIBED"),
            (2, "UNSUBSCRIBING"),
            (3, "UNSUBSCRIBED"),
        ]);
        let mut prof = EllaProfile::default();
        ingest_subscriber(&mut prof, Some(&schema), 1);
        ingest_subscriber(&mut prof, Some(&schema), 2);
        assert_eq!(prof.channels[&(6, 7)].occ_by_inst[&2], 1);
    }

    /// Pre-#460 captures numbered the terminal state 2, whether or not
    /// they carry a blob that names it.
    #[test]
    fn legacy_subscriber_leave_is_state_2() {
        let schema = subscriber_schema(vec![
            (0, "SUBSCRIBING"),
            (1, "SUBSCRIBED"),
            (2, "UNSUBSCRIBED"),
        ]);
        for s in [Some(&schema), None] {
            let mut prof = EllaProfile::default();
            ingest_subscriber(&mut prof, s, 1);
            ingest_subscriber(&mut prof, s, 2);
            match prof.events[1].payload {
                EllaPayload::ChannelSubscriber { state, .. } => {
                    assert_eq!(state, ELLA_SUBSCRIBER_UNSUBSCRIBED);
                }
                ref other => panic!("unexpected {other:?}"),
            }
            assert_eq!(prof.channels[&(6, 7)].occ_by_inst[&2], 0);
        }
    }
}
