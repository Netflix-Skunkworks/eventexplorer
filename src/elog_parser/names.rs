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

/// Short human-readable name for a PACING_CAP_* value.
pub fn pacing_cap_name(cap: u8) -> &'static str {
    match cap {
        PACING_CAP_NONE => "none",
        PACING_CAP_STARS => "STARS",
        PACING_CAP_BW_RATE_CAP => "bw_rate_cap",
        PACING_CAP_FIXED => "fixed",
        PACING_CAP_UNKNOWN => "unknown",
        _ => "?",
    }
}

/// Pretty-print a `CONN_PARAMS_V2.flags` bitmask as a `+`-separated
/// list (e.g. `"SACK+TS+ECN+WSCALE"`), or `"none"` when no bits are set.
pub fn format_conn_params_flags(flags: u8) -> String {
    let names: [(u8, &str); 7] = [
        (CP_FLAG_SACK_PERMIT, "SACK"),
        (CP_FLAG_TIMESTAMPS, "TS"),
        (CP_FLAG_ECN, "ECN"),
        (CP_FLAG_ACE, "ACE"),
        (CP_FLAG_FASTOPEN, "FASTOPEN"),
        (CP_FLAG_SIGNATURE, "SIGNATURE"),
        (CP_FLAG_WINDOW_SCALE, "WSCALE"),
    ];
    let parts: Vec<&str> = names
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, n)| *n)
        .collect();
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join("+")
    }
}

/// Pretty-print a `FLAG tcp_flags` bitmask (from `tcp_eventlog_schema.src`,
/// carried by `IN` and `OUT` events) as a `+`-separated list (e.g.
/// `"ACK+PUSH"` or `"ACK+RETRANSMIT"`), or `"none"` when no bits are set.
pub fn format_tcp_flags(flags: u16) -> String {
    let names: [(u16, &str); 12] = [
        (TCP_FLAG_FIN, "FIN"),
        (TCP_FLAG_SYN, "SYN"),
        (TCP_FLAG_RST, "RST"),
        (TCP_FLAG_PUSH, "PUSH"),
        (TCP_FLAG_ACK, "ACK"),
        (TCP_FLAG_URG, "URG"),
        (TCP_FLAG_ECE, "ECE"),
        (TCP_FLAG_CWR, "CWR"),
        (TCP_FLAG_AE, "AE"),
        (TCP_FLAG_PACED, "PACED"),
        (TCP_FLAG_TLP, "TLP"),
        (TCP_FLAG_RETRANSMIT, "RETRANSMIT"),
    ];
    let parts: Vec<&str> = names
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, n)| *n)
        .collect();
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join("+")
    }
}

/// Pretty-print an HTTP request-tracking `track_flags` bitmask as a
/// `+`-separated list (e.g. `"USED+OPEN+SEQV"`), or `"none"` when no
/// bits are set.
pub fn format_http_req_flags(flags: u32) -> String {
    let names: [(u32, &str); 6] = [
        (HTTP_REQ_FLG_USED, "USED"),
        (HTTP_REQ_FLG_OPEN, "OPEN"),
        (HTTP_REQ_FLG_SEQV, "SEQV"),
        (HTTP_REQ_FLG_COMP, "COMP"),
        (HTTP_REQ_FLG_FSND, "FSND"),
        (HTTP_REQ_FLG_LSND, "LSND"),
    ];
    let parts: Vec<&str> = names
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, n)| *n)
        .collect();
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join("+")
    }
}

/// Short human-readable name for a TCP connection state
/// (`ENUM tcp_state` in the `tcp` provider's embedded v2 schema — verified
/// against `/tmp/hpts-delay.elog`). Used by [`EventPayload::In`]'s `state`.
pub fn tcp_state_name(state: u8) -> &'static str {
    match state {
        0 => "CLOSED",
        1 => "LISTEN",
        2 => "SYN_SENT",
        3 => "SYN_RECEIVED",
        4 => "ESTABLISHED",
        5 => "CLOSE_WAIT",
        6 => "FIN_WAIT_1",
        7 => "CLOSING",
        8 => "LAST_ACK",
        9 => "FIN_WAIT_2",
        10 => "TIME_WAIT",
        _ => "?",
    }
}

/// Short human-readable name for a `WINDOW_COLLAPSE.direction` value
/// (`ENUM collapse_dir` in the `tcp` provider's embedded v2 schema —
/// verified against `/tmp/hpts-delay.elog`).
pub fn collapse_dir_name(direction: u8) -> &'static str {
    match direction {
        0 => "UNCOLLAPSED",
        1 => "COLLAPSED",
        2 => "RSM_MARKED",
        3 => "SPLIT",
        _ => "?",
    }
}

/// Short human-readable name for an interface link state
/// (`ENUM link_state` in `if_eventlog_schema.src`).
pub fn if_link_state_name(state: u8) -> &'static str {
    match state {
        0 => "UNKNOWN",
        1 => "DOWN",
        2 => "UP",
        _ => "?",
    }
}

/// Name string for an `if`-provider event id (`if_eventlog_schema.src`).
/// The if provider's id namespace is independent of tcp's, so callers
/// must select this helper based on the event's provider.
pub fn if_event_name(id: u32) -> &'static str {
    match id {
        EVT_IF_INFO => "IF_INFO",
        EVT_IF_ATTACH => "IF_ATTACH",
        EVT_IF_DETACH => "IF_DETACH",
        EVT_IF_RENAME => "IF_RENAME",
        EVT_IF_LINK_STATE => "IF_LINK_STATE",
        EVT_IF_FLAGS_CHANGE => "IF_FLAGS_CHANGE",
        EVT_IF_MTU_CHANGE => "IF_MTU_CHANGE",
        EVT_IF_CAPABILITIES_CHANGE => "IF_CAPABILITIES_CHANGE",
        EVT_IF_ADDR_ADD => "IF_ADDR_ADD",
        EVT_IF_ADDR_REMOVE => "IF_ADDR_REMOVE",
        EVT_IF_SAMPLE => "IF_SAMPLE",
        EVT_IF_SAMPLE_BYTES => "IF_SAMPLE_BYTES",
        EVT_IF_SAMPLE_PACKETS => "IF_SAMPLE_PACKETS",
        EVT_SESSION_CREATE => "SESSION_CREATE",
        EVT_SESSION_END => "SESSION_END",
        _ => "UNKNOWN",
    }
}

/// Short human-readable name for a `CPU_SAMPLE.sample_type` value.
pub fn cpu_sample_type_name(t: u8) -> &'static str {
    match t {
        CPU_SAMPLE_TYPE_STATCLOCK => "statclock",
        CPU_SAMPLE_TYPE_HWPMC => "hwpmc",
        _ => "?",
    }
}

/// Pretty-print a `CPU_SAMPLE.flags` bitmask as a `+`-separated list
/// (e.g. `"IDLE+INTR"`), or `"none"` when no bits are set.
pub fn format_cpu_sample_flags(flags: u8) -> String {
    let names: [(u8, &str); 5] = [
        (CPU_SAMPLE_FLAG_USER_MODE, "USER"),
        (CPU_SAMPLE_FLAG_IDLE, "IDLE"),
        (CPU_SAMPLE_FLAG_INTR, "INTR"),
        (CPU_SAMPLE_FLAG_TRUNC, "TRUNC"),
        (CPU_SAMPLE_FLAG_FREQ_VALID, "FREQ_VALID"),
    ];
    let parts: Vec<&str> = names
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, n)| *n)
        .collect();
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join("+")
    }
}

pub fn event_name(id: u32) -> &'static str {
    match id {
        EVT_CONN_SET_IP_V4 => "CONN_SET_IP_V4",
        EVT_CONN_SET_IP_V6 => "CONN_SET_IP_V6",
        EVT_LOG_ID => "LOG_ID",
        EVT_CONN_PARAMS => "CONN_PARAMS",
        EVT_MSS => "MSS",
        EVT_CONN_OWNER => "CONN_OWNER",
        EVT_CONN_PARAMS_V2 => "CONN_PARAMS_V2",
        EVT_TCP_STACK_CHANGE => "TCP_STACK_CHANGE",
        EVT_CC_ALGO_CHANGE => "CC_ALGO_CHANGE",
        EVT_IN => "IN",
        EVT_OUT => "OUT",
        EVT_ACK => "ACK",
        EVT_SACK => "SACK",
        EVT_USER_SEND => "USER_SEND",
        EVT_SENDFILE => "SENDFILE",
        EVT_CWND => "CWND",
        EVT_CC_PARAMS => "CC_PARAMS",
        EVT_ENTER_RECOVERY => "ENTER_RECOVERY",
        EVT_EXIT_RECOVERY => "EXIT_RECOVERY",
        EVT_PRR_STATE => "PRR_STATE",
        EVT_WINDOW_COLLAPSE => "WINDOW_COLLAPSE",
        EVT_HYSTART => "HYSTART",
        EVT_GOODPUT => "GOODPUT",
        EVT_GP_CHECK => "GP_CHECK",
        EVT_RTT => "RTT",
        EVT_RTT_SAMPLE => "RTT_SAMPLE",
        EVT_RTT_UNDERFLOW => "RTT_UNDERFLOW",
        EVT_RTO => "RTO",
        EVT_DSACK => "DSACK",
        EVT_TIMER_START => "TIMER_START",
        EVT_TIMER_CANCEL => "TIMER_CANCEL",
        EVT_TIMER_FIRED => "TIMER_FIRED",
        EVT_SB_WAKE => "SB_WAKE",
        EVT_PACING_CALC => "PACING_CALC",
        EVT_PACE_STATE => "PACE_STATE",
        EVT_HW_PACE_RATE => "HW_PACE_RATE",
        EVT_PACING_CALC_V2 => "PACING_CALC_V2",
        EVT_STARS_PACE => "STARS_PACE",
        EVT_SOCKET_OPT_UINT32 => "SOCKET_OPT_UINT32",
        EVT_SOCKET_OPT_ERR => "SOCKET_OPT_ERR",
        EVT_OUTPUT_DECISION => "OUTPUT_DECISION",
        EVT_TCP_SAMPLE => "TCP_SAMPLE",
        EVT_TCP_GP_SAMPLE => "TCP_GP_SAMPLE",
        EVT_TCP_STACK_INFO => "TCP_STACK_INFO",
        EVT_TCP_RPS_SAMPLE => "TCP_RPS_SAMPLE",
        EVT_TCP_PACING_RATE_SAMPLE => "TCP_PACING_RATE_SAMPLE",
        EVT_TCP_REALIZED_RATE_SAMPLE => "TCP_REALIZED_RATE_SAMPLE",
        EVT_TCP_SEND_SAMPLE => "TCP_SEND_SAMPLE",
        EVT_TCP_LRO_FLUSH_SAMPLE => "TCP_LRO_FLUSH_SAMPLE",
        EVT_TCP_SRTT_SAMPLE => "TCP_SRTT_SAMPLE",
        EVT_HTTP_REQ_NEW => "HTTP_REQ_NEW",
        EVT_HTTP_REQ_FREED => "HTTP_REQ_FREED",
        EVT_HTTP_REQ_STALE => "HTTP_REQ_STALE",
        EVT_HTTP_REQ_ALLOCFAIL => "HTTP_REQ_ALLOCFAIL",
        EVT_HTTP_REQ_FORCEFREE => "HTTP_REQ_FORCEFREE",
        EVT_SESSION_CREATE => "SESSION_CREATE",
        EVT_SESSION_END => "SESSION_END",
        _ => "UNKNOWN",
    }
}

/// Name string for the `hpts` provider's timing-wheel scheduler trace
/// (ids 0-14; `hpts_eventlog_schema.src`). Distinct namespace from
/// `EVT_HPTS_STARS_AGG_SAMPLE` (id 16), which has no separate name
/// helper since it's the only event the legacy hpts decoder handles.
pub fn hpts_wheel_event_name(id: u32) -> &'static str {
    match id {
        EVT_HPTS_INIT => "INIT",
        EVT_HPTS_INIT_HPTS_ENTRY => "INIT_HPTS_ENTRY",
        EVT_HPTS_INIT_TIMING_PARAMS => "INIT_TIMING_PARAMS",
        EVT_HPTS_STATE_ACTIVE => "STATE_ACTIVE",
        EVT_HPTS_STATE_INACTIVE => "STATE_INACTIVE",
        EVT_HPTS_SLEEP => "SLEEP",
        EVT_HPTS_CONN_INSERT => "CONN_INSERT",
        EVT_HPTS_CONN_REMOVE => "CONN_REMOVE",
        EVT_HPTS_CONN_MOVE => "CONN_MOVE",
        EVT_HPTS_WHEEL_WRAP => "WHEEL_WRAP",
        EVT_HPTS_PROCESSING => "PROCESSING",
        EVT_HPTS_PERF_PROCESSING => "PERF_PROCESSING",
        EVT_HPTS_PERF_LATENESS => "PERF_LATENESS",
        EVT_HPTS_LATENESS_SAMPLE => "LATENESS_SAMPLE",
        EVT_HPTS_TP_BATCH_SAMPLE => "TP_BATCH_SAMPLE",
        EVT_HPTS_SLOT_GATE_SAMPLE => "SLOT_GATE_SAMPLE",
        EVT_HPTS_NO_RUN_SLOT_SKIP => "NO_RUN_SLOT_SKIP",
        EVT_HPTS_OPPOR_SAMPLE => "HPTS_OPPOR_SAMPLE",
        _ => "UNKNOWN",
    }
}

/// Short human-readable name for an hpts `ENUM bind_type` value
/// (`hpts_eventlog_schema.src`).
pub fn hpts_bind_type_name(bind_type: u8) -> &'static str {
    match bind_type {
        HPTS_BIND_TYPE_NONE => "NONE",
        HPTS_BIND_TYPE_CPU => "CPU",
        HPTS_BIND_TYPE_NUMA => "NUMA",
        _ => "?",
    }
}

/// Like [`event_name`], but for the `cpu` provider's event-id namespace.
/// The cpu provider's ids overlap with TCP's (id 0/1/2/3 mean different
/// things), so callers must pick the right helper based on the event's
/// provider. Returned strings are matched to `cpu_eventlog_schema.src`.
pub fn cpu_event_name(id: u32) -> &'static str {
    match id {
        EVT_CPU_KERNEL_INFO => "KERNEL_INFO",
        EVT_CPU_THREAD_CREATE => "THREAD_CREATE",
        EVT_CPU_THREAD_EXIT => "THREAD_EXIT",
        EVT_CPU_SAMPLE => "CPU_SAMPLE",
        EVT_CPU_OVERFLOW => "OVERFLOW",
        EVT_CPU_LINKER_FILE => "LINKER_FILE",
        EVT_CPU_SYMBOL_RANGE => "SYMBOL_RANGE",
        EVT_CPU_USER_FILE => "USER_FILE",
        EVT_CPU_PROC_MAP => "PROC_MAP",
        EVT_CPU_USER_SYMBOL_RANGE => "USER_SYMBOL_RANGE",
        EVT_CPU_PROC_EXEC => "PROC_EXEC",
        EVT_CPU_PROC_EXEC_MAP => "PROC_EXEC_MAP",
        EVT_CPU_PROC_MMAP => "PROC_MMAP",
        EVT_CPU_SYMBOL_PACK_REF => "SYMBOL_PACK_REF",
        EVT_SESSION_CREATE => "SESSION_CREATE",
        EVT_SESSION_END => "SESSION_END",
        _ => "UNKNOWN",
    }
}

pub fn ella_relay_event_name(id: u32) -> &'static str {
    match id {
        EVT_ELLA_RELAY_INSTANCE_ATTACH => "INSTANCE_ATTACH",
        EVT_ELLA_RELAY_NODE_ERROR => "NODE_ERROR",
        EVT_ELLA_RELAY_NODE_INFO => "NODE_INFO",
        EVT_ELLA_RELAY_SESSION_CREATE => "SESSION_CREATE",
        EVT_ELLA_RELAY_SESSION_DESTROY => "SESSION_DESTROY",
        EVT_ELLA_RELAY_CHANNEL_STATE => "CHANNEL_STATE",
        EVT_ELLA_RELAY_CHANNEL_ID_ASSIGNED => "CHANNEL_ID_ASSIGNED",
        EVT_ELLA_RELAY_CHANNEL_ERROR => "CHANNEL_ERROR",
        EVT_ELLA_RELAY_CHANNEL_DESTROYED => "CHANNEL_DESTROYED",
        EVT_ELLA_RELAY_CHANNEL_SUBSCRIBER => "CHANNEL_SUBSCRIBER",
        EVT_ELLA_RELAY_SUBSCRIBER_IDENTITY => "SUBSCRIBER_IDENTITY",
        EVT_ELLA_RELAY_CHANNEL_COUNT => "CHANNEL_COUNT",
        EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL => "CHANNEL_COUNT_TOTAL",
        EVT_ELLA_RELAY_CHANNEL_SAMPLE => "CHANNEL_SAMPLE",
        EVT_ELLA_RELAY_FRAME_SAMPLE => "FRAME_SAMPLE",
        EVT_ELLA_RELAY_OFFLOAD_STATS => "OFFLOAD_STATS",
        EVT_ELLA_RELAY_TRAFFIC_STATS => "TRAFFIC_STATS",
        EVT_ELLA_RELAY_DELIVERED_STATS => "DELIVERED_STATS",
        EVT_ELLA_RELAY_OBJECT_STATE => "OBJECT_STATE",
        EVT_ELLA_RELAY_OBJECT_TIMING => "OBJECT_TIMING",
        EVT_ELLA_RELAY_FRAME_SENT => "FRAME_SENT",
        EVT_ELLA_RELAY_FRAME_RELAY => "FRAME_RELAY",
        _ => "UNKNOWN",
    }
}

pub fn ella_side_name(v: u8) -> &'static str {
    match v {
        ELLA_SIDE_UPSTREAM => "UPSTREAM",
        ELLA_SIDE_DOWNSTREAM => "DOWNSTREAM",
        _ => "?",
    }
}

pub fn ella_role_name(v: u8) -> &'static str {
    match v {
        ELLA_ROLE_ORIGIN => "ORIGIN",
        ELLA_ROLE_RELAY => "RELAY",
        _ => "?",
    }
}

pub fn ella_lifecycle_state_name(v: u8) -> &'static str {
    match v {
        ELLA_LIFECYCLE_REQUESTED => "REQUESTED",
        ELLA_LIFECYCLE_CREATED => "CREATED",
        ELLA_LIFECYCLE_CREATE_FAILED => "CREATE_FAILED",
        ELLA_LIFECYCLE_CREATE_ROLLED_BACK => "CREATE_ROLLED_BACK",
        ELLA_LIFECYCLE_CREATE_TIMED_OUT => "CREATE_TIMED_OUT",
        ELLA_LIFECYCLE_CREATE_REJECTED => "CREATE_REJECTED",
        ELLA_LIFECYCLE_RETIRE_ARMED => "RETIRE_ARMED",
        ELLA_LIFECYCLE_LEAVE_DEFERRED => "LEAVE_DEFERRED",
        ELLA_LIFECYCLE_CANCELLED => "CANCELLED",
        _ => "?",
    }
}

pub fn ella_lifecycle_reason_name(v: u8) -> &'static str {
    match v {
        0 => "UNSPECIFIED",
        1 => "DOWNSTREAM_JOIN",
        2 => "CONFIGURED_CHANNEL",
        3 => "SWITCH",
        4 => "JOIN_NEXT",
        5 => "NO_SUBSCRIBERS",
        6 => "SUBSCRIBER_REJOINED",
        7 => "UPSTREAM_TIMEOUT",
        8 => "UPSTREAM_REJECTED",
        _ => "?",
    }
}

pub fn ella_destroy_reason_name(v: u8) -> &'static str {
    match v {
        0 => "UNSPECIFIED",
        1 => "LEAVE_COMPLETE",
        2 => "LEAVE_DELAY_EXPIRED",
        3 => "BOUNDARY_RETIRE",
        4 => "BOUNDARY_RETIRE_FALLBACK",
        5 => "SWITCH_OLD_DRAINED",
        6 => "SOURCE_ENDED",
        7 => "ALLOC_FAILED",
        8 => "SHUTDOWN",
        _ => "?",
    }
}

pub fn ella_subscriber_state_name(v: u8) -> &'static str {
    match v {
        ELLA_SUBSCRIBER_SUBSCRIBING => "SUBSCRIBING",
        ELLA_SUBSCRIBER_SUBSCRIBED => "SUBSCRIBED",
        ELLA_SUBSCRIBER_UNSUBSCRIBING => "UNSUBSCRIBING",
        ELLA_SUBSCRIBER_UNSUBSCRIBED => "UNSUBSCRIBED",
        _ => "?",
    }
}

pub fn ella_subscriber_reason_name(v: u8) -> &'static str {
    match v {
        0 => "NONE",
        1 => "SUBSCRIBER_LEAVING",
        2 => "KEEPALIVE_TIMEOUT",
        3 => "JOIN_REJECTED",
        4 => "INCOMING_SUBSCRIBER",
        5 => "NO_CLIENT_SUBSCRIPTION",
        6 => "JOIN_TIMEOUT",
        7 => "SERVER_ACK",
        8 => "SHUTDOWN",
        _ => "?",
    }
}

pub fn ella_object_state_name(v: u8) -> &'static str {
    match v {
        ELLA_OBJECT_STARTED => "STARTED",
        ELLA_OBJECT_COMPLETED => "COMPLETED",
        ELLA_OBJECT_ABANDONED => "ABANDONED",
        _ => "?",
    }
}

pub fn ella_error_code_name(v: u8) -> &'static str {
    match v {
        0 => "UNKNOWN",
        1 => "SUBSCRIBE_FAILED",
        2 => "NO_UPSTREAM",
        3 => "DEFERRED_RESOLVE_FAILED",
        4 => "DATA_QUEUE_FULL",
        5 => "PACKET_VERIFY_FAILED",
        6 => "LEAVE_CHANNEL_NOT_FOUND",
        7 => "LEAVE_CLIENT_NOT_FOUND",
        _ => "?",
    }
}

pub fn ella_relay_error_code_name(v: u8) -> &'static str {
    match v {
        0 => "UNKNOWN",
        30 => "UNKNOWN_CHANNEL_DATA",
        31 => "CONTROL_DECRYPT_FAILED",
        32 => "VERSION_UNSUPPORTED",
        33 => "CAT_PACKET_TOO_SHORT",
        34 => "CAT_INVALID_MAGIC",
        35 => "CAT_BAD_VERSION",
        36 => "CAT_EXPIRED",
        37 => "CAT_IP_MISMATCH",
        38 => "CAT_LENGTH_INVALID",
        39 => "CAT_UNKNOWN_CLIENT",
        40 => "CATREK_NOT_FOUND",
        41 => "CAT_ALLOC_FAILED",
        42 => "CAT_DECRYPT_FAILED",
        43 => "CAT_INNER_TOO_SHORT",
        44 => "CAT_CRYPTO_CREATE_FAILED",
        45 => "CAT_KEY_SETUP_FAILED",
        46 => "CAT_BODY_DECRYPT_FAILED",
        47 => "CAT_CLIENT_ADD_FAILED",
        48 => "CAT_CLIENT_LOOKUP_FAILED",
        49 => "JOIN_BODY_TOO_SHORT",
        50 => "JOIN_NAME_INVALID",
        _ => "?",
    }
}

pub fn ella_fanout_message_type_name(v: u8) -> &'static str {
    let base = v & !0xc0;
    match if (0x05..=0x08).contains(&base) {
        base
    } else {
        v
    } {
        0 => "NONE",
        0x01 => "JOIN",
        0x02 => "JOIN_NEXT",
        0x03 => "SWITCH",
        0x05 => "DATA",
        0x06 => "DATA_2",
        0x07 => "DATA_3",
        0x08 => "DATA_4",
        0x11 => "LEAVE",
        0x12 => "KEEPALIVE",
        0x13 => "LEAVE_NEXT",
        0x20 => "RESPONSE",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tcp_flags_empty() {
        assert_eq!(format_tcp_flags(0), "none");
    }

    #[test]
    fn format_tcp_flags_single_and_combined() {
        assert_eq!(format_tcp_flags(TCP_FLAG_SYN), "SYN");
        assert_eq!(format_tcp_flags(TCP_FLAG_SYN | TCP_FLAG_ACK), "SYN+ACK");
        assert_eq!(format_tcp_flags(TCP_FLAG_ACK | TCP_FLAG_PUSH), "PUSH+ACK");
        assert_eq!(
            format_tcp_flags(TCP_FLAG_ACK | TCP_FLAG_RETRANSMIT),
            "ACK+RETRANSMIT"
        );
        assert_eq!(
            format_tcp_flags(TCP_FLAG_ACK | TCP_FLAG_RETRANSMIT | TCP_FLAG_TLP),
            "ACK+TLP+RETRANSMIT"
        );
    }
}
