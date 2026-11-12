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

//! Shared test support: a fully synthetic `.elog` byte generator plus a
//! golden-snapshot helper.
//!
//! The fixture is built entirely in code (no real captures) so it carries no
//! private data and is byte-for-byte deterministic. It exercises the tcp,
//! hpts, cpu, and if providers across the event catalog so the golden
//! snapshots are sensitive to parser/analysis regressions during refactors.
//!
//! On-disk layout mirrors `src/elog_parser.rs` (`parse_elog_core`):
//!   - file header: 40 bytes
//!   - provider table: u32 count, then 34 (v1) or 50 (v2, +schema_id[16])
//!     bytes per provider
//!   - v2 only: decode-blob section (u32 blob_count, then per blob
//!     schema_id[16] + u32 size + raw bytes)
//!   - events: 32-byte header + payload, little-endian
//!
//! Run tests with `UPDATE_GOLDEN=1` to (re)write the golden files.

#![allow(dead_code)]

use std::path::PathBuf;

// Provider ids.
pub const TCP_PID: u16 = 1;
pub const HPTS_PID: u16 = 2;
pub const CPU_PID: u16 = 3;
pub const IF_PID: u16 = 4;
pub const PROC_PID: u16 = 5;
pub const ELLA_RELAY_PID: u16 = 11;
pub const ELLA_RELAY_SESSION: u64 = 1;

// Capture-wide constants.
pub const CAPTURE_START: u64 = 1_000_000;
pub const START_UTC_US: u64 = 1_700_000_000_000_000;
/// Idle span between the drained backlog and the live capture in
/// [`synthetic_elog_fanout_backlog`], comfortably past the parser's
/// 60s detection floor.
pub const BACKLOG_GAP_US: u64 = 1_500_000_000;

// Session ids (hex strings used as CLI args).
pub const SESSION_A: u64 = 0x1001;
pub const SESSION_B: u64 = 0x2002;
pub const TCP_GLOBAL_SESSION: u64 = 0x9000;
pub const CPU0: u64 = 0;
pub const CPU1: u64 = 1;
pub const VNET: u64 = 0x7000;

pub const KERNEL_BASE: u64 = 0xffff_ffff_8000_0000;

// tcp provider event ids (mirrors elog_parser constants).
const EVT_CONN_SET_IP_V4: u32 = 0;
const EVT_CONN_OWNER: u32 = 5;
const EVT_CONN_PARAMS_V2: u32 = 6;
const EVT_TCP_STACK_CHANGE: u32 = 7;
const EVT_CC_ALGO_CHANGE: u32 = 8;
const EVT_OUT: u32 = 101;
const EVT_ACK: u32 = 102;
const EVT_SACK: u32 = 103;
const EVT_USER_SEND: u32 = 104;
const EVT_CWND: u32 = 200;
const EVT_ENTER_RECOVERY: u32 = 202;
const EVT_EXIT_RECOVERY: u32 = 203;
const EVT_WINDOW_COLLAPSE: u32 = 205;
const EVT_GOODPUT: u32 = 207;
const EVT_GP_CHECK: u32 = 208;
const EVT_RTT: u32 = 300;
const EVT_RTT_UNDERFLOW: u32 = 302;
const EVT_RTO: u32 = 400;
const EVT_DSACK: u32 = 402;
const EVT_PACING_CALC_V2: u32 = 603;
const EVT_PACE_STATE: u32 = 601;
const EVT_STARS_PACE: u32 = 604;
const EVT_TCP_SAMPLE: u32 = 900;
const EVT_TCP_GP_SAMPLE: u32 = 901;
const EVT_TCP_STACK_INFO: u32 = 902;
const EVT_TCP_RPS_SAMPLE: u32 = 903;
const EVT_SESSION_CREATE: u32 = 0xFFFF_FFFE;

// hpts.
const EVT_HPTS_STARS_AGG_SAMPLE: u32 = 16;

// cpu.
const EVT_CPU_KERNEL_INFO: u32 = 0;
const EVT_CPU_THREAD_CREATE: u32 = 1;
const EVT_CPU_SAMPLE: u32 = 3;
const EVT_CPU_LINKER_FILE: u32 = 5;
const EVT_CPU_SYMBOL_RANGE: u32 = 6;

// process (split-schema namespace).
const EVT_PROC_THREAD_CREATE: u32 = 0;
const EVT_PROC_EXEC: u32 = 2;
const EVT_PROC_MAP: u32 = 4;
const EVT_PROC_FORK: u32 = 5;
// cpu provider, split-schema SAMPLE id (legacy THREAD_CREATE is also 1).
const EVT_CPUX_SAMPLE: u32 = 1;

// if.
const EVT_IF_INFO: u32 = 0;
const EVT_IF_ATTACH: u32 = 1;
const EVT_IF_LINK_STATE: u32 = 4;
const EVT_IF_SAMPLE: u32 = 20;
const EVT_IF_SAMPLE_BYTES: u32 = 21;
const EVT_IF_SAMPLE_PACKETS: u32 = 22;

/// Pacing cap source codes.
const PACING_CAP_STARS: u8 = 1;
const PACING_CAP_BW_RATE_CAP: u8 = 2;

struct Writer {
    events: Vec<u8>,
    count: u64,
}

impl Writer {
    fn new() -> Self {
        Writer {
            events: Vec::new(),
            count: 0,
        }
    }

    /// Append one event record (32-byte header + payload).
    #[allow(clippy::too_many_arguments)]
    fn event(
        &mut self,
        provider: u16,
        cpu: u16,
        ts: u64,
        session: u64,
        event_id: u32,
        tid: i32,
        payload: &[u8],
    ) {
        let len = (32 + payload.len()) as u16;
        let e = &mut self.events;
        e.extend_from_slice(&len.to_le_bytes());
        e.extend_from_slice(&cpu.to_le_bytes());
        e.extend_from_slice(&provider.to_le_bytes());
        e.extend_from_slice(&[0u8, 0u8]); // pad
        e.extend_from_slice(&ts.to_le_bytes());
        e.extend_from_slice(&session.to_le_bytes());
        e.extend_from_slice(&event_id.to_le_bytes());
        e.extend_from_slice(&tid.to_le_bytes());
        e.extend_from_slice(payload);
        self.count += 1;
    }
}

fn cstr(s: &str, width: usize) -> Vec<u8> {
    let mut v = vec![0u8; width];
    let bytes = s.as_bytes();
    let n = bytes.len().min(width - 1).min(width);
    v[..n].copy_from_slice(&bytes[..n]);
    v
}

// ---- tcp payload encoders --------------------------------------------------

fn p_session_create(tp: u64) -> Vec<u8> {
    tp.to_le_bytes().to_vec()
}

fn p_conn_set_ip_v4(src: [u8; 4], sport: u16, dst: [u8; 4], dport: u16) -> Vec<u8> {
    let mut v = Vec::with_capacity(14);
    v.extend_from_slice(&src);
    v.extend_from_slice(&sport.to_be_bytes());
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(&dst);
    v.extend_from_slice(&dport.to_be_bytes());
    v
}

#[allow(clippy::too_many_arguments)]
fn p_conn_params_v2(
    iss: u32,
    irs: u32,
    mss: u32,
    snd_ws: u8,
    rcv_ws: u8,
    flags: u8,
    cc: &str,
    stack: &str,
) -> Vec<u8> {
    let mut v = Vec::with_capacity(63);
    v.extend_from_slice(&iss.to_le_bytes());
    v.extend_from_slice(&irs.to_le_bytes());
    v.extend_from_slice(&mss.to_le_bytes());
    v.push(snd_ws);
    v.push(rcv_ws);
    v.push(flags);
    v.extend_from_slice(&cstr(cc, 16));
    v.extend_from_slice(&cstr(stack, 32));
    v
}

fn p_out(seq: u32, ack: u32, win: u32, len: u32, flags: u16, cwnd: u32) -> Vec<u8> {
    let mut v = vec![0u8; 30];
    v[0..4].copy_from_slice(&seq.to_le_bytes());
    v[4..8].copy_from_slice(&ack.to_le_bytes());
    v[8..12].copy_from_slice(&win.to_le_bytes());
    v[12..16].copy_from_slice(&len.to_le_bytes());
    v[16..18].copy_from_slice(&flags.to_le_bytes());
    v[20..24].copy_from_slice(&cwnd.to_le_bytes());
    v
}

fn p_ack(acked: u32, snd_una: u32, inflight: u32, sacked: u32, nsegs: u32) -> Vec<u8> {
    let mut v = vec![0u8; 32];
    v[0..4].copy_from_slice(&acked.to_le_bytes());
    v[4..8].copy_from_slice(&snd_una.to_le_bytes());
    v[8..12].copy_from_slice(&inflight.to_le_bytes());
    v[12..16].copy_from_slice(&sacked.to_le_bytes());
    v[16..20].copy_from_slice(&nsegs.to_le_bytes());
    v
}

fn p_user_send(error: i32, flags: u32, state: u8, used: u32, avail: u32, send_len: u32) -> Vec<u8> {
    let mut v = vec![0u8; 24];
    v[0..4].copy_from_slice(&error.to_le_bytes());
    v[4..8].copy_from_slice(&flags.to_le_bytes());
    v[8] = state;
    v[12..16].copy_from_slice(&used.to_le_bytes());
    v[16..20].copy_from_slice(&avail.to_le_bytes());
    v[20..24].copy_from_slice(&send_len.to_le_bytes());
    v
}

fn p_u32_pair(a: u32, b: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(8);
    v.extend_from_slice(&a.to_le_bytes());
    v.extend_from_slice(&b.to_le_bytes());
    v
}

fn p_stars_pace(natural_bps: u64, scaled_bps: u64, pressure: u32, len: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(24);
    v.extend_from_slice(&natural_bps.to_le_bytes());
    v.extend_from_slice(&scaled_bps.to_le_bytes());
    v.extend_from_slice(&pressure.to_le_bytes());
    v.extend_from_slice(&len.to_le_bytes());
    v
}

fn p_goodput(goodput: u64, raw_bw: u64, utim: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(24);
    v.extend_from_slice(&goodput.to_le_bytes());
    v.extend_from_slice(&raw_bw.to_le_bytes());
    v.extend_from_slice(&utim.to_le_bytes());
    v
}

#[allow(clippy::too_many_arguments)]
fn p_gp_check(
    elapsed: u64,
    utim: u64,
    now: u64,
    start_ts: u64,
    last_ack: u64,
    srtt: u32,
    th_ack: u32,
    bytes: u32,
    min_bytes: u32,
    reason: u8,
) -> Vec<u8> {
    let mut v = vec![0u8; 57];
    v[0..8].copy_from_slice(&elapsed.to_le_bytes());
    v[8..16].copy_from_slice(&utim.to_le_bytes());
    v[16..24].copy_from_slice(&now.to_le_bytes());
    v[24..32].copy_from_slice(&start_ts.to_le_bytes());
    v[32..40].copy_from_slice(&last_ack.to_le_bytes());
    v[40..44].copy_from_slice(&srtt.to_le_bytes());
    v[44..48].copy_from_slice(&th_ack.to_le_bytes());
    v[48..52].copy_from_slice(&bytes.to_le_bytes());
    v[52..56].copy_from_slice(&min_bytes.to_le_bytes());
    v[56] = reason;
    v
}

fn p_rtt(
    rtt: u32,
    srtt: u32,
    path_rtt: u32,
    lowest: u32,
    gp_filter_min: u32,
    gp_srtt: u32,
) -> Vec<u8> {
    let mut v = Vec::with_capacity(24);
    for x in [rtt, srtt, path_rtt, lowest, gp_filter_min, gp_srtt] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v
}

fn p_rtt_underflow(us_rtt: u32, minuend: u32, subtrahend: u32, location: u8) -> Vec<u8> {
    let mut v = vec![0u8; 13];
    v[0..4].copy_from_slice(&us_rtt.to_le_bytes());
    v[4..8].copy_from_slice(&minuend.to_le_bytes());
    v[8..12].copy_from_slice(&subtrahend.to_le_bytes());
    v[12] = location;
    v
}

fn p_pacing_calc_v2(
    slot: u32,
    bw_est: u64,
    method: u8,
    quality: u8,
    in_ss: u8,
    cap: u8,
    inflight: u32,
) -> Vec<u8> {
    let mut v = vec![0u8; 24];
    v[0..4].copy_from_slice(&slot.to_le_bytes());
    v[8..16].copy_from_slice(&bw_est.to_le_bytes());
    v[16] = method;
    v[17] = quality;
    v[18] = in_ss;
    v[19] = cap;
    v[20..24].copy_from_slice(&inflight.to_le_bytes());
    v
}

/// `STRUCT DSACK` (dsack_round_seen, dsack_round_end, num_dsack,
/// dsack_persist, mod, round) — verified against
/// `/tmp/hpts-delay.elog`'s embedded `tcp` schema.
fn p_dsack(
    dsack_round_seen: u8,
    dsack_round_end: u32,
    num_dsack: u32,
    dsack_persist: u32,
    round_mod: u8,
    round: u32,
) -> Vec<u8> {
    let mut v = vec![0u8; 24];
    v[0] = dsack_round_seen;
    v[4..8].copy_from_slice(&dsack_round_end.to_le_bytes());
    v[8..12].copy_from_slice(&num_dsack.to_le_bytes());
    v[12..16].copy_from_slice(&dsack_persist.to_le_bytes());
    v[16] = round_mod;
    v[20..24].copy_from_slice(&round.to_le_bytes());
    v
}

/// `STRUCT RTO` (timeout_num, min_rtt, rtt, segment_len, inflight,
/// pkts_out) — verified against `/tmp/hpts-delay.elog`'s embedded `tcp`
/// schema.
fn p_rto(
    timeout_num: i32,
    min_rtt: u32,
    rtt: u32,
    segment_len: u32,
    inflight: u32,
    pkts_out: u32,
) -> Vec<u8> {
    let mut v = vec![0u8; 24];
    v[0..4].copy_from_slice(&timeout_num.to_le_bytes());
    v[4..8].copy_from_slice(&min_rtt.to_le_bytes());
    v[8..12].copy_from_slice(&rtt.to_le_bytes());
    v[12..16].copy_from_slice(&segment_len.to_le_bytes());
    v[16..20].copy_from_slice(&inflight.to_le_bytes());
    v[20..24].copy_from_slice(&pkts_out.to_le_bytes());
    v
}

/// `STRUCT WINDOW_COLLAPSE` (count, direction, inflight, has_collapsed)
/// — verified against `/tmp/hpts-delay.elog`'s embedded `tcp` schema.
fn p_window_collapse(count: u32, direction: u8, inflight: u32, has_collapsed: u8) -> Vec<u8> {
    let mut v = vec![0u8; 13];
    v[0..4].copy_from_slice(&count.to_le_bytes());
    v[4] = direction;
    v[8..12].copy_from_slice(&inflight.to_le_bytes());
    v[12] = has_collapsed;
    v
}

// ---- hpts / cpu / if encoders ---------------------------------------------

/// 20 fixed `uint64_t` fields, in the exact order of `STRUCT
/// STARS_AGG_SAMPLE` (hpts_eventlog_schema.src): pressure, tokens (i64
/// bit-cast to u64), per_slot_bytes, slot_gate_budget_bytes,
/// slot_bytes_peak, slot_gate_denies, slot_gate_admits,
/// slot_gate_denied_bytes, slot_gate_admitted_bytes, meter_obytes,
/// pressure_sum, pressure_ticks, synthetic_norate_hits, slot_gate_enable,
/// health_limit_bps, min_rate_bps, rollover_queue_depth,
/// rollover_diverts, rollover_admits, slot_gate_headroom_pct.
#[allow(clippy::too_many_arguments)]
fn p_stars_agg_sample(
    pressure: u64,
    tokens: i64,
    per_slot_bytes: u64,
    slot_gate_budget_bytes: u64,
    slot_bytes_peak: u64,
    slot_gate_denies: u64,
    slot_gate_admits: u64,
    slot_gate_denied_bytes: u64,
    slot_gate_admitted_bytes: u64,
    meter_obytes: u64,
    pressure_sum: u64,
    pressure_ticks: u64,
    synthetic_norate_hits: u64,
    slot_gate_enable: u64,
    health_limit_bps: u64,
    min_rate_bps: u64,
    rollover_queue_depth: u64,
    rollover_diverts: u64,
    rollover_admits: u64,
    slot_gate_headroom_pct: u64,
) -> Vec<u8> {
    let mut v = Vec::with_capacity(160);
    for x in [
        pressure,
        tokens as u64,
        per_slot_bytes,
        slot_gate_budget_bytes,
        slot_bytes_peak,
        slot_gate_denies,
        slot_gate_admits,
        slot_gate_denied_bytes,
        slot_gate_admitted_bytes,
        meter_obytes,
        pressure_sum,
        pressure_ticks,
        synthetic_norate_hits,
        slot_gate_enable,
        health_limit_bps,
        min_rate_bps,
        rollover_queue_depth,
        rollover_diverts,
        rollover_admits,
        slot_gate_headroom_pct,
    ] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v
}

fn p_kernel_info(text_base: u64, text_size: u64, ncpus: u32, hz: u32, ident: &str) -> Vec<u8> {
    let mut v = vec![0u8; 88];
    v[0..8].copy_from_slice(&text_base.to_le_bytes());
    v[8..16].copy_from_slice(&text_size.to_le_bytes());
    v[16..20].copy_from_slice(&ncpus.to_le_bytes());
    v[20..24].copy_from_slice(&hz.to_le_bytes());
    let id = cstr(ident, 64);
    v[24..88].copy_from_slice(&id);
    v
}

fn p_thread_create(tid: u32, pid: u32, comm: &str, tdname: &str) -> Vec<u8> {
    let mut v = vec![0u8; 72];
    v[0..4].copy_from_slice(&tid.to_le_bytes());
    v[4..8].copy_from_slice(&pid.to_le_bytes());
    v[8..40].copy_from_slice(&cstr(comm, 32));
    v[40..72].copy_from_slice(&cstr(tdname, 32));
    v
}

fn p_cpu_sample(tid: u32, pid: u32, sample_type: u8, flags: u8, frames: &[u64]) -> Vec<u8> {
    let depth = frames.len().min(64);
    let mut v = vec![0u8; 16 + depth * 8];
    v[0..4].copy_from_slice(&tid.to_le_bytes());
    v[4..8].copy_from_slice(&pid.to_le_bytes());
    v[8] = sample_type;
    v[9] = flags;
    v[10] = depth as u8;
    for (i, f) in frames.iter().take(depth).enumerate() {
        let o = 16 + i * 8;
        v[o..o + 8].copy_from_slice(&f.to_le_bytes());
    }
    v
}

fn p_linker_file(
    file_id: u32,
    base: u64,
    size: u64,
    flags: u32,
    build_id: &str,
    filename: &str,
    pathname: &str,
) -> Vec<u8> {
    // 261 bytes of fields + 3 bytes trailing pad (struct is `__aligned(8)`
    // because of the trailing `uint64_t` fields, so its total size rounds
    // up to the next multiple of 8).
    let mut v = vec![0u8; 264];
    v[0..4].copy_from_slice(&file_id.to_le_bytes());
    v[8..16].copy_from_slice(&base.to_le_bytes());
    v[16..24].copy_from_slice(&size.to_le_bytes());
    v[24..28].copy_from_slice(&flags.to_le_bytes());
    v[28..69].copy_from_slice(&cstr(build_id, 41));
    v[69..133].copy_from_slice(&cstr(filename, 64));
    v[133..261].copy_from_slice(&cstr(pathname, 128));
    v
}

fn p_symbol_range(file_id: u32, offset: u64, size: u32, name: &str) -> Vec<u8> {
    let mut v = vec![0u8; 116];
    v[0..4].copy_from_slice(&file_id.to_le_bytes());
    v[8..16].copy_from_slice(&offset.to_le_bytes());
    v[16..20].copy_from_slice(&size.to_le_bytes());
    v[20..116].copy_from_slice(&cstr(name, 96));
    v
}

fn p_if_info(period_us: u32, nifs: u16) -> Vec<u8> {
    let mut v = vec![0u8; 18];
    v[0..4].copy_from_slice(&period_us.to_le_bytes());
    v[16..18].copy_from_slice(&nifs.to_le_bytes());
    v
}

fn p_if_attach(
    ifindex: u16,
    name: &str,
    if_type: u8,
    driver: &str,
    mtu: u32,
    baudrate: u64,
    hwaddr: &[u8],
) -> Vec<u8> {
    let mut v = vec![0u8; 49 + hwaddr.len()];
    v[0..2].copy_from_slice(&ifindex.to_le_bytes());
    v[2..18].copy_from_slice(&cstr(name, 16));
    v[18] = if_type;
    v[19..35].copy_from_slice(&cstr(driver, 16));
    v[36..40].copy_from_slice(&mtu.to_le_bytes());
    v[40..48].copy_from_slice(&baudrate.to_le_bytes());
    v[48] = hwaddr.len() as u8;
    v[49..49 + hwaddr.len()].copy_from_slice(hwaddr);
    v
}

fn p_if_link_state(ifindex: u16, state: u8, baudrate: u64) -> Vec<u8> {
    let mut v = vec![0u8; 16];
    v[0..2].copy_from_slice(&ifindex.to_le_bytes());
    v[2] = state;
    v[8..16].copy_from_slice(&baudrate.to_le_bytes());
    v
}

#[allow(clippy::too_many_arguments)]
fn p_if_sample(ifindex: u16, ibytes: u64, obytes: u64, ipackets: u64, opackets: u64) -> Vec<u8> {
    let mut v = vec![0u8; 96];
    v[0..2].copy_from_slice(&ifindex.to_le_bytes());
    v[8..16].copy_from_slice(&ibytes.to_le_bytes());
    v[16..24].copy_from_slice(&obytes.to_le_bytes());
    v[24..32].copy_from_slice(&ipackets.to_le_bytes());
    v[32..40].copy_from_slice(&opackets.to_le_bytes());
    v
}

#[allow(clippy::too_many_arguments)]
fn p_if_sample_bytes(
    ifindex: u16,
    ibytes: u64,
    obytes: u64,
    tcp_ibytes: u64,
    tcp_obytes: u64,
    udp_ibytes: u64,
    udp_obytes: u64,
) -> Vec<u8> {
    let mut v = vec![0u8; 56];
    v[0..2].copy_from_slice(&ifindex.to_le_bytes());
    v[8..16].copy_from_slice(&ibytes.to_le_bytes());
    v[16..24].copy_from_slice(&obytes.to_le_bytes());
    v[24..32].copy_from_slice(&tcp_ibytes.to_le_bytes());
    v[32..40].copy_from_slice(&tcp_obytes.to_le_bytes());
    v[40..48].copy_from_slice(&udp_ibytes.to_le_bytes());
    v[48..56].copy_from_slice(&udp_obytes.to_le_bytes());
    v
}

#[allow(clippy::too_many_arguments)]
fn p_if_sample_packets(
    ifindex: u16,
    ipackets: u64,
    opackets: u64,
    tcp_ipackets: u64,
    tcp_opackets: u64,
    udp_ipackets: u64,
    udp_opackets: u64,
) -> Vec<u8> {
    let mut v = vec![0u8; 112];
    v[0..2].copy_from_slice(&ifindex.to_le_bytes());
    v[8..16].copy_from_slice(&ipackets.to_le_bytes());
    v[16..24].copy_from_slice(&opackets.to_le_bytes());
    v[24..32].copy_from_slice(&tcp_ipackets.to_le_bytes());
    v[32..40].copy_from_slice(&tcp_opackets.to_le_bytes());
    v[40..48].copy_from_slice(&udp_ipackets.to_le_bytes());
    v[48..56].copy_from_slice(&udp_opackets.to_le_bytes());
    // ierrors..noproto (offsets 56-111) intentionally left 0 for the
    // synthetic fixture; tests that need them set can extend this.
    v
}

/// NCD-10070 shape: `IF_SAMPLE_BYTES` with the appended `other_ibytes`/
/// `other_obytes` fields (72 bytes total). Used to exercise the direct-
/// counter decode path, as opposed to `p_if_sample_bytes`'s 56-byte
/// shape which exercises the derive-from-subtraction fallback.
#[allow(clippy::too_many_arguments)]
fn p_if_sample_bytes_with_other(
    ifindex: u16,
    ibytes: u64,
    obytes: u64,
    tcp_ibytes: u64,
    tcp_obytes: u64,
    udp_ibytes: u64,
    udp_obytes: u64,
    other_ibytes: u64,
    other_obytes: u64,
) -> Vec<u8> {
    let mut v = p_if_sample_bytes(
        ifindex, ibytes, obytes, tcp_ibytes, tcp_obytes, udp_ibytes, udp_obytes,
    );
    v.resize(72, 0);
    v[56..64].copy_from_slice(&other_ibytes.to_le_bytes());
    v[64..72].copy_from_slice(&other_obytes.to_le_bytes());
    v
}

/// NCD-10070 shape: `IF_SAMPLE_PACKETS` with the appended
/// `other_ipackets`/`other_opackets` fields (128 bytes total). See
/// `p_if_sample_bytes_with_other`.
#[allow(clippy::too_many_arguments)]
fn p_if_sample_packets_with_other(
    ifindex: u16,
    ipackets: u64,
    opackets: u64,
    tcp_ipackets: u64,
    tcp_opackets: u64,
    udp_ipackets: u64,
    udp_opackets: u64,
    other_ipackets: u64,
    other_opackets: u64,
) -> Vec<u8> {
    let mut v = p_if_sample_packets(
        ifindex,
        ipackets,
        opackets,
        tcp_ipackets,
        tcp_opackets,
        udp_ipackets,
        udp_opackets,
    );
    v.resize(128, 0);
    v[112..120].copy_from_slice(&other_ipackets.to_le_bytes());
    v[120..128].copy_from_slice(&other_opackets.to_le_bytes());
    v
}

/// Emit a connection-oriented tcp session's event stream.
#[allow(clippy::too_many_arguments)]
fn emit_session(
    w: &mut Writer,
    sid: u64,
    src: [u8; 4],
    sport: u16,
    dst: [u8; 4],
    dport: u16,
    cc: &str,
    stack: &str,
    base_ts: u64,
) {
    let t = |ms: u64| base_ts + ms * 1000;
    w.event(
        TCP_PID,
        0,
        t(0),
        sid,
        EVT_SESSION_CREATE,
        100,
        &p_session_create(0xdead_beef),
    );
    w.event(
        TCP_PID,
        0,
        t(0),
        sid,
        EVT_CONN_SET_IP_V4,
        100,
        &p_conn_set_ip_v4(src, sport, dst, dport),
    );
    w.event(
        TCP_PID,
        0,
        t(0),
        sid,
        EVT_CONN_PARAMS_V2,
        100,
        &p_conn_params_v2(1000, 2000, 1460, 7, 8, 0x43, cc, stack),
    );
    w.event(
        TCP_PID,
        0,
        t(0),
        sid,
        EVT_CONN_OWNER,
        100,
        &p_session_create(4242)[..4],
    );

    let mut snd_una: u32 = 1000;
    let mut cwnd: u32 = 10 * 1460;
    for i in 0..20u64 {
        let ts = t(10 + i * 10);
        let seq = snd_una + (i as u32) * 1460;
        w.event(
            TCP_PID,
            0,
            ts,
            sid,
            EVT_OUT,
            100,
            &p_out(seq, 2000, 65535, 1460, 0x10, cwnd),
        );
        w.event(
            TCP_PID,
            0,
            ts + 500,
            sid,
            EVT_ACK,
            100,
            &p_ack(1460, snd_una, 5 * 1460, 0, 2),
        );
        snd_una += 1460;
        cwnd += 1460;
        w.event(
            TCP_PID,
            0,
            ts + 600,
            sid,
            EVT_RTT,
            100,
            &p_rtt(
                20_000 + (i as u32) * 100,
                21_000,
                22_000,
                19_500,
                19_000,
                21_500,
            ),
        );
        w.event(
            TCP_PID,
            0,
            ts + 700,
            sid,
            EVT_CWND,
            100,
            &p_u32_pair(cwnd, 40 * 1460),
        );
        let cap = if i % 2 == 0 {
            PACING_CAP_STARS
        } else {
            PACING_CAP_BW_RATE_CAP
        };
        w.event(
            TCP_PID,
            0,
            ts + 800,
            sid,
            EVT_PACING_CALC_V2,
            100,
            &p_pacing_calc_v2(1000, 25_000_000, 1, 2, 0, cap, 5 * 1460),
        );
        if i % 5 == 0 {
            w.event(
                TCP_PID,
                0,
                ts + 900,
                sid,
                EVT_GOODPUT,
                100,
                &p_goodput(24_000_000, 26_000_000, 200_000),
            );
            w.event(
                TCP_PID,
                0,
                ts + 950,
                sid,
                EVT_GP_CHECK,
                100,
                &p_gp_check(
                    200_000,
                    180_000,
                    ts + 950,
                    ts,
                    ts + 500,
                    21_000,
                    snd_una,
                    50_000,
                    16_000,
                    1,
                ),
            );
        }
        if i == 3 {
            w.event(
                TCP_PID,
                0,
                ts + 100,
                sid,
                EVT_USER_SEND,
                100,
                &p_user_send(0, 0, 4, 32_000, 96_000, 64_000),
            );
        }
    }

    // A recovery episode with loss signals in the middle of the trace.
    let rt = t(80);
    w.event(
        TCP_PID,
        0,
        rt,
        sid,
        EVT_ENTER_RECOVERY,
        100,
        &p_u32_pair(8 * 1460, 20 * 1460),
    );
    w.event(
        TCP_PID,
        0,
        rt + 200,
        sid,
        EVT_RTO,
        100,
        &p_rto(0, 19_500, 20_000, 1460, 8 * 1460, 6),
    );
    w.event(
        TCP_PID,
        0,
        rt + 300,
        sid,
        EVT_DSACK,
        100,
        &p_dsack(1, snd_una + 1460, 1, 3, 0, 1),
    );
    w.event(
        TCP_PID,
        0,
        rt + 400,
        sid,
        EVT_WINDOW_COLLAPSE,
        100,
        &p_window_collapse(2, 1, 8 * 1460, 1),
    );
    w.event(
        TCP_PID,
        0,
        rt + 5_000,
        sid,
        EVT_EXIT_RECOVERY,
        100,
        &p_u32_pair(10 * 1460, 20 * 1460),
    );

    // A couple of diagnostic / state-change events.
    w.event(
        TCP_PID,
        0,
        t(120),
        sid,
        EVT_RTT_UNDERFLOW,
        100,
        &p_rtt_underflow(100, 200, 300, 2),
    );
    w.event(
        TCP_PID,
        0,
        t(121),
        sid,
        EVT_PACE_STATE,
        100,
        &p_u32_pair(2, 16),
    );
    w.event(
        TCP_PID,
        0,
        t(122),
        sid,
        EVT_TCP_STACK_CHANGE,
        100,
        &cstr(stack, 32),
    );
    w.event(
        TCP_PID,
        0,
        t(123),
        sid,
        EVT_CC_ALGO_CHANGE,
        100,
        &cstr(cc, 16),
    );
    w.event(
        TCP_PID,
        0,
        t(124),
        sid,
        EVT_SACK,
        100,
        &p_ack(1460, snd_una, 4 * 1460, 1460, 1),
    );
}

/// Build the shared event stream used by both [`synthetic_elog`] (v1) and
/// [`synthetic_elog_v2`] — everything except the file header / provider
/// table, which differ by version.
fn build_events() -> Writer {
    let mut w = Writer::new();

    emit_session(
        &mut w,
        SESSION_A,
        [10, 0, 0, 2],
        12345,
        [10, 0, 0, 1],
        443,
        "cubic",
        "rack",
        CAPTURE_START + 10_000,
    );
    emit_session(
        &mut w,
        SESSION_B,
        [10, 0, 0, 3],
        23456,
        [10, 0, 0, 1],
        443,
        "newreno",
        "freebsd",
        CAPTURE_START + 20_000,
    );

    // tcp per-stack global samples.
    w.event(
        TCP_PID,
        0,
        CAPTURE_START,
        TCP_GLOBAL_SESSION,
        EVT_TCP_STACK_INFO,
        0,
        &{
            let mut v = cstr("rack", 32);
            v.extend_from_slice(&100_000u32.to_le_bytes());
            v
        },
    );
    for i in 0..5u64 {
        let ts = CAPTURE_START + i * 100_000;
        w.event(TCP_PID, 0, ts, TCP_GLOBAL_SESSION, EVT_TCP_SAMPLE, 0, &{
            // 11 contiguous host-order u64 PCB-state counts, matching
            // `STRUCT TCP_SAMPLE` in tcp_eventlog_schema.src:
            // closed, listen, syn_sent, syn_rcvd, estab, close_wait,
            // fin_wait_1, closing, last_ack, fin_wait_2, time_wait.
            let fields: [u64; 11] = [
                0,       // closed
                1,       // listen
                0,       // syn_sent
                0,       // syn_rcvd
                100 + i, // estab
                2,       // close_wait
                0,       // fin_wait_1
                0,       // closing
                0,       // last_ack
                0,       // fin_wait_2
                i,       // time_wait
            ];
            let mut v = Vec::with_capacity(88);
            for f in fields {
                v.extend_from_slice(&f.to_le_bytes());
            }
            v
        });
        w.event(
            TCP_PID,
            0,
            ts,
            TCP_GLOBAL_SESSION,
            EVT_TCP_RPS_SAMPLE,
            0,
            &(1000u64 + i * 50).to_le_bytes(),
        );
        w.event(TCP_PID, 0, ts, TCP_GLOBAL_SESSION, EVT_TCP_GP_SAMPLE, 0, &{
            let mut v = vec![0u8; 8 + 4 * 8];
            v[0] = 4;
            for (j, val) in [10u64, 20, 30, 40].iter().enumerate() {
                let o = 8 + j * 8;
                v[o..o + 8].copy_from_slice(&val.to_le_bytes());
            }
            v
        });
    }

    // hpts STARS aggregate samples.
    for i in 0..8u64 {
        let ts = CAPTURE_START + i * 50_000;
        w.event(
            HPTS_PID,
            0,
            ts,
            0,
            EVT_HPTS_STARS_AGG_SAMPLE,
            0,
            &p_stars_agg_sample(
                200 + i * 50,
                -1_000 + i as i64 * 100,
                3_200_000,
                3_200_000,
                100_000 + i * 5_000,
                i * 3,
                1_000 + i * 10,
                i * 1_500,
                500_000 + i * 20_000,
                20_000_000_000 + i * 1_000_000_000,
                500 + i * 50,
                10 + i,
                i,
                1,
                50_000_000_000,
                4_000_000,
                i * 7,
                i * 20,
                i * 18,
                100 - i.min(50) * 5,
            ),
        );
    }

    // cpu provider: kernel info, symbols, threads, samples.
    w.event(
        CPU_PID,
        0,
        CAPTURE_START,
        CPU0,
        EVT_CPU_KERNEL_INFO,
        0,
        &p_kernel_info(KERNEL_BASE, 0x0100_0000, 4, 1000, "FreeBSD test kernel"),
    );
    w.event(
        CPU_PID,
        0,
        CAPTURE_START,
        CPU0,
        EVT_CPU_LINKER_FILE,
        0,
        &p_linker_file(
            0,
            KERNEL_BASE,
            0x0100_0000,
            0,
            "",
            "kernel",
            "/boot/kernel/kernel",
        ),
    );
    w.event(
        CPU_PID,
        0,
        CAPTURE_START,
        CPU0,
        EVT_CPU_SYMBOL_RANGE,
        0,
        &p_symbol_range(0, 0x1000, 0x200, "tcp_input"),
    );
    w.event(
        CPU_PID,
        0,
        CAPTURE_START,
        CPU0,
        EVT_CPU_SYMBOL_RANGE,
        0,
        &p_symbol_range(0, 0x2000, 0x200, "tcp_output"),
    );
    w.event(
        CPU_PID,
        0,
        CAPTURE_START,
        CPU0,
        EVT_CPU_SYMBOL_RANGE,
        0,
        &p_symbol_range(0, 0x3000, 0x200, "ip_output"),
    );
    w.event(
        CPU_PID,
        0,
        CAPTURE_START,
        CPU0,
        EVT_CPU_THREAD_CREATE,
        0,
        &p_thread_create(100, 100, "nginx", "-"),
    );
    w.event(
        CPU_PID,
        0,
        CAPTURE_START,
        CPU1,
        EVT_CPU_THREAD_CREATE,
        0,
        &p_thread_create(200, 0, "kernel", "swi1: netisr"),
    );
    for i in 0..12u64 {
        let ts = CAPTURE_START + i * 20_000;
        let cpu_sess = if i % 2 == 0 { CPU0 } else { CPU1 };
        let (tid, pid) = if i % 2 == 0 { (100, 100) } else { (200, 0) };
        let frames = if i % 3 == 0 {
            vec![
                KERNEL_BASE + 0x3050,
                KERNEL_BASE + 0x2080,
                KERNEL_BASE + 0x1010,
            ]
        } else {
            vec![KERNEL_BASE + 0x2090, KERNEL_BASE + 0x1040]
        };
        w.event(
            CPU_PID,
            cpu_sess as u16,
            ts,
            cpu_sess,
            EVT_CPU_SAMPLE,
            0,
            &p_cpu_sample(tid, pid, 0, 0, &frames),
        );
    }

    // if provider: info, attach, link, samples.
    w.event(
        IF_PID,
        0,
        CAPTURE_START,
        VNET,
        EVT_IF_INFO,
        0,
        &p_if_info(100_000, 2),
    );
    w.event(
        IF_PID,
        0,
        CAPTURE_START,
        VNET,
        EVT_IF_ATTACH,
        0,
        &p_if_attach(
            1,
            "cxl0",
            6,
            "cxl",
            1500,
            100_000_000_000,
            &[0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
        ),
    );
    w.event(
        IF_PID,
        0,
        CAPTURE_START + 1000,
        VNET,
        EVT_IF_LINK_STATE,
        0,
        &p_if_link_state(1, 2, 100_000_000_000),
    );
    for i in 0..6u64 {
        let ts = CAPTURE_START + i * 100_000;
        w.event(
            IF_PID,
            0,
            ts,
            VNET,
            EVT_IF_SAMPLE,
            0,
            &p_if_sample(
                1,
                1_000_000 * (i + 1),
                5_000_000 * (i + 1),
                1000 * (i + 1),
                4000 * (i + 1),
            ),
        );
    }

    w
}

const PROVIDERS: [(u16, &str); 4] = [
    (TCP_PID, "tcp"),
    (HPTS_PID, "hpts"),
    (CPU_PID, "cpu"),
    (IF_PID, "if"),
];

/// Build the full synthetic capture as a v1 `.elog` byte vector (no
/// schema_id, no decode-blob section).
pub fn synthetic_elog() -> Vec<u8> {
    let w = build_events();

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&1u32.to_le_bytes()); // version
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes()); // event_count
    out.extend_from_slice(&0u64.to_le_bytes()); // dropped

    out.extend_from_slice(&(PROVIDERS.len() as u32).to_le_bytes());
    for (id, name) in PROVIDERS {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
    }

    out.extend_from_slice(&w.events);
    out
}

/// A "cpu"-only capture (no "symbols"/"process" providers, mirroring
/// `oca.py set logging on`) whose stack-sample events use the split-schema
/// id (1) rather than the legacy id (3). Regression coverage for
/// split-schema detection needing more than "did this file register a
/// symbols/process provider" — some kernels emit split-schema cpu-provider
/// ids unconditionally, even without those two providers (see
/// `workstreams/stars/LOG.md` 2026-07-29 in the `work` repo). Every event
/// here uses a SAMPLE-shaped payload under id 1, which collides with the
/// legacy THREAD_CREATE id; a parser that trusts provider presence alone
/// would decode all of them as bogus threads instead of samples.
pub fn synthetic_elog_cpu_only_split_schema(n_samples: u64) -> Vec<u8> {
    let mut w = Writer::new();
    for i in 0..n_samples {
        let ts = CAPTURE_START + i * 20_000;
        let frames = vec![KERNEL_BASE + 0x2090, KERNEL_BASE + 0x1040];
        w.event(
            CPU_PID,
            0,
            ts,
            CPU0,
            EVT_CPU_THREAD_CREATE, // numerically == split-schema SAMPLE id (1)
            0,
            &p_cpu_sample(100 + i as u32, 100, 0, 0, &frames),
        );
    }

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let providers: [(u16, &str); 1] = [(CPU_PID, "cpu")];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
    }

    out.extend_from_slice(&w.events);
    out
}

// ---- process provider payload encoders (C natural alignment) -------------

fn p_proc_thread_create(tid: u32, pid: u32, comm: &str) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&tid.to_le_bytes());
    b.extend_from_slice(&pid.to_le_bytes());
    b.extend_from_slice(&cstr(comm, 32));
    b.extend_from_slice(&cstr(comm, 32)); // tdname
    b
}

fn p_proc_map(pid: u32, user_file_id: u32, base: u64, size: u64, offset: u64) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&pid.to_le_bytes());
    b.extend_from_slice(&user_file_id.to_le_bytes());
    b.extend_from_slice(&base.to_le_bytes());
    b.extend_from_slice(&size.to_le_bytes());
    b.extend_from_slice(&offset.to_le_bytes());
    b
}

fn p_proc_exec(pid: u32, ppid: u32, start_sec: u64, comm: &str) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&pid.to_le_bytes());
    b.extend_from_slice(&ppid.to_le_bytes());
    b.extend_from_slice(&start_sec.to_le_bytes());
    b.extend_from_slice(&cstr(comm, 32));
    b
}

/// Split-schema "process"-provider capture covering `PROC_FORK`: pid 100
/// (nginx) has a `PROC_MAP` (as dump_state would emit), then forks pid
/// 200 without exec — the edge must copy the parent's map set onto the
/// child. pid 300 is a same-`comm` pid with no edge at all, so the
/// pre-`PROC_FORK` `inherit_fork_proc_maps` heuristic must still cover
/// it as fallback.
pub fn synthetic_elog_proc_fork() -> Vec<u8> {
    let mut w = Writer::new();
    w.event(
        PROC_PID,
        0,
        CAPTURE_START,
        VNET,
        EVT_PROC_THREAD_CREATE,
        0,
        &p_proc_thread_create(1, 100, "nginx"),
    );
    w.event(
        PROC_PID,
        0,
        CAPTURE_START,
        VNET,
        EVT_PROC_MAP,
        0,
        &p_proc_map(100, 58, 0x37dd_f7a9_0000, 0x1a8000, 0x90000),
    );
    w.event(
        PROC_PID,
        0,
        CAPTURE_START + 1,
        VNET,
        EVT_PROC_FORK,
        0,
        &[200u32.to_le_bytes(), 100u32.to_le_bytes()].concat(),
    );
    w.event(
        PROC_PID,
        0,
        CAPTURE_START + 1,
        VNET,
        EVT_PROC_THREAD_CREATE,
        0,
        &p_proc_thread_create(2, 200, "nginx"),
    );
    w.event(
        PROC_PID,
        0,
        CAPTURE_START + 2,
        VNET,
        EVT_PROC_THREAD_CREATE,
        0,
        &p_proc_thread_create(3, 300, "nginx"),
    );

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let providers: [(u16, &str); 1] = [(PROC_PID, "process")];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
    }

    out.extend_from_slice(&w.events);
    out
}

/// Shell forks pid 100 (`THREAD_CREATE` comm `"sh"`), a sample lands
/// before `execve`, then `PROC_EXEC` renames the image to `"perl"` and
/// a later sample lands under the new name. Covers the flame-group
/// rename that `THREAD_CREATE` alone cannot express.
pub fn synthetic_elog_proc_exec() -> Vec<u8> {
    let mut w = Writer::new();
    let frames = vec![0x4000_0000u64];
    w.event(
        PROC_PID,
        0,
        CAPTURE_START,
        VNET,
        EVT_PROC_THREAD_CREATE,
        0,
        &p_proc_thread_create(1, 100, "sh"),
    );
    w.event(
        CPU_PID,
        0,
        CAPTURE_START + 50,
        CPU0,
        EVT_CPUX_SAMPLE,
        0,
        &p_cpu_sample(1, 100, 0, 0x01, &frames),
    );
    w.event(
        PROC_PID,
        0,
        CAPTURE_START + 100,
        VNET,
        EVT_PROC_EXEC,
        0,
        &p_proc_exec(100, 1, 1_700_000_000, "perl"),
    );
    w.event(
        CPU_PID,
        0,
        CAPTURE_START + 150,
        CPU0,
        EVT_CPUX_SAMPLE,
        0,
        &p_cpu_sample(1, 100, 0, 0x01, &frames),
    );

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let providers: [(u16, &str); 2] = [(CPU_PID, "cpu"), (PROC_PID, "process")];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
    }

    out.extend_from_slice(&w.events);
    out
}

/// NCD-10070 regression: an "if"-only capture using the appended
/// `other_ibytes`/`other_obytes`/`other_ipackets`/`other_opackets`
/// fields, with `other` deliberately set to a value the old derive-by-
/// subtraction formula (`total - tcp - udp`) would NOT produce (55/45
/// instead of the naive 100/100) -- proving the parser reads the direct
/// counter rather than silently re-deriving it. This is the scenario a
/// multi-port aggregate interface (lagg) hits in practice: `total` is a
/// separately-tracked, asynchronously-updated driver counter that need
/// not equal tcp+udp+other on any given tick.
pub fn synthetic_elog_if_sample_other_direct() -> Vec<u8> {
    let mut w = Writer::new();
    w.event(
        IF_PID,
        0,
        CAPTURE_START,
        VNET,
        EVT_IF_SAMPLE_BYTES,
        0,
        &p_if_sample_bytes_with_other(1, 1000, 900, 600, 500, 300, 300, 55, 45),
    );
    w.event(
        IF_PID,
        0,
        CAPTURE_START,
        VNET,
        EVT_IF_SAMPLE_PACKETS,
        0,
        &p_if_sample_packets_with_other(1, 10, 9, 6, 5, 3, 3, 1, 1),
    );

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let providers: [(u16, &str); 1] = [(IF_PID, "if")];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
    }

    out.extend_from_slice(&w.events);
    out
}

fn ella_pad_align(buf: &mut Vec<u8>, align: usize) {
    while !buf.len().is_multiple_of(align) {
        buf.push(0);
    }
}

fn ella_put_u32(buf: &mut Vec<u8>, v: u32) {
    ella_pad_align(buf, 4);
    buf.extend_from_slice(&v.to_le_bytes());
}

fn ella_put_u16(buf: &mut Vec<u8>, v: u16) {
    ella_pad_align(buf, 2);
    buf.extend_from_slice(&v.to_le_bytes());
}

fn ella_put_u8(buf: &mut Vec<u8>, v: u8) {
    buf.push(v);
}

fn ella_put_cstr(buf: &mut Vec<u8>, s: &str, width: usize) {
    let mut v = vec![0u8; width];
    let n = s.len().min(width);
    v[..n].copy_from_slice(&s.as_bytes()[..n]);
    buf.extend_from_slice(&v);
}

// ---- nginx payload encoders (C natural alignment) -------------------------

fn nginx_put_u64(buf: &mut Vec<u8>, v: u64) {
    ella_pad_align(buf, 8);
    buf.extend_from_slice(&v.to_le_bytes());
}

fn p_nginx_request_start(
    request: u32,
    range_start: i64,
    range_end: i64,
    method: &str,
    uri: &str,
) -> Vec<u8> {
    let mut b = Vec::new();
    ella_put_u32(&mut b, request);
    nginx_put_u64(&mut b, range_start as u64);
    nginx_put_u64(&mut b, range_end as u64);
    ella_put_cstr(&mut b, method, 16);
    ella_put_cstr(&mut b, uri, 256);
    b
}

fn p_nginx_response_headers(request: u32, status: u16, content_length: i64) -> Vec<u8> {
    let mut b = Vec::new();
    ella_put_u32(&mut b, request);
    ella_put_u16(&mut b, status);
    nginx_put_u64(&mut b, content_length as u64);
    b
}

fn p_nginx_request_done(request: u32, status: u16, bytes_sent: u64) -> Vec<u8> {
    let mut b = Vec::new();
    ella_put_u32(&mut b, request);
    ella_put_u16(&mut b, status);
    nginx_put_u64(&mut b, bytes_sent);
    b
}

/// Two HTTP requests on one connection session: one completed
/// (START → HEADERS → DONE) and one left open (START only).
pub fn synthetic_elog_nginx() -> Vec<u8> {
    const NGINX_PID: u16 = 30;
    const CONN: u64 = 0xAAAA;
    const REQ_A: u32 = 1;
    const REQ_B: u32 = 2;

    let mut w = Writer::new();
    w.event(
        NGINX_PID,
        0,
        CAPTURE_START + 1_000,
        CONN,
        0, // REQUEST_START
        0,
        &p_nginx_request_start(REQ_A, 0, -1, "GET", "/f/fa/2051805908.ismv"),
    );
    w.event(
        NGINX_PID,
        0,
        CAPTURE_START + 2_000,
        CONN,
        1, // RESPONSE_HEADERS
        0,
        &p_nginx_response_headers(REQ_A, 206, 41943317),
    );
    w.event(
        NGINX_PID,
        0,
        CAPTURE_START + 20_000,
        CONN,
        2, // REQUEST_DONE
        0,
        &p_nginx_request_done(REQ_A, 206, 41943317),
    );
    w.event(
        NGINX_PID,
        0,
        CAPTURE_START + 30_000,
        CONN,
        0, // REQUEST_START (left open)
        0,
        &p_nginx_request_start(REQ_B, 671744, -1, "GET", "/44/11/599505352.ismv"),
    );

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let providers: [(u16, &str); 1] = [(NGINX_PID, "nginx")];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
    }

    out.extend_from_slice(&w.events);
    out
}

fn p_ella_instance_attach(instance_id: u32, side: u8) -> Vec<u8> {
    let mut b = Vec::new();
    ella_put_u32(&mut b, instance_id);
    ella_put_u8(&mut b, side);
    b
}

fn p_ella_session_create(name: &str, version: u8, instance_id: u32) -> Vec<u8> {
    let mut b = Vec::new();
    ella_put_cstr(&mut b, name, 64);
    ella_put_u8(&mut b, version);
    ella_put_u32(&mut b, instance_id);
    b
}

fn p_ella_channel_state(state: u8, reason: u8, instance_id: u32, subscribers: u32) -> Vec<u8> {
    let mut b = Vec::new();
    ella_put_u8(&mut b, state);
    ella_put_u8(&mut b, reason);
    ella_put_u32(&mut b, instance_id);
    ella_put_u32(&mut b, subscribers);
    b
}

fn p_ella_channel_id_assigned(instance_id: u32, channel_id: u32) -> Vec<u8> {
    let mut b = Vec::new();
    ella_put_u32(&mut b, instance_id);
    ella_put_u32(&mut b, channel_id);
    b
}

fn p_ella_channel_subscriber(
    state: u8,
    reason: u8,
    instance_id: u32,
    addr: &str,
    port: u16,
    subscribers: u32,
) -> Vec<u8> {
    let mut b = Vec::new();
    ella_put_u8(&mut b, state);
    ella_put_u8(&mut b, reason);
    ella_put_u32(&mut b, instance_id);
    ella_put_cstr(&mut b, addr, 48);
    ella_put_u16(&mut b, port);
    ella_put_u32(&mut b, subscribers);
    b
}

fn p_ella_channel_error(code: u8, instance_id: u32, detail: u32) -> Vec<u8> {
    let mut b = Vec::new();
    ella_put_u8(&mut b, code);
    ella_put_u32(&mut b, instance_id);
    ella_put_u32(&mut b, detail);
    b
}

fn p_ella_channel_destroyed(reason: u8, instance_id: u32, subscribers: u32) -> Vec<u8> {
    let mut b = Vec::new();
    ella_put_u8(&mut b, reason);
    ella_put_u32(&mut b, instance_id);
    ella_put_u32(&mut b, subscribers);
    b
}

fn p_ella_relay_error(
    code: u8,
    instance_id: u32,
    channel_id: u32,
    channel_id_known: u8,
    detail: u32,
) -> Vec<u8> {
    let mut b = Vec::new();
    ella_put_u8(&mut b, code);
    ella_put_u32(&mut b, instance_id);
    ella_put_u32(&mut b, channel_id);
    ella_put_u8(&mut b, channel_id_known);
    ella_put_u32(&mut b, detail);
    b
}

fn p_ella_channel_count(
    instance_id: u32,
    channels: u32,
    leave: u32,
    retire: u32,
    subscribers: u32,
    sessions: u32,
) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [instance_id, channels, leave, retire, subscribers, sessions] {
        ella_put_u32(&mut b, v);
    }
    b
}

/// An ELLA-only capture of `ella_relay` channel-layer events (corp/ella
/// PR #456): instance attach, one channel session, subscriber, sampled
/// CHANNEL_COUNT, plus a global RELAY_ERROR. Isolated from the shared
/// golden fixture so CLI goldens stay stable.
pub fn synthetic_elog_ella() -> Vec<u8> {
    let mut w = Writer::new();
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START,
        0,
        0,
        0,
        &p_ella_instance_attach(1, 0),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START,
        0,
        0,
        0,
        &p_ella_instance_attach(2, 1),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 1_000,
        ELLA_RELAY_SESSION,
        50,
        0,
        &p_ella_session_create("live/1", 2, 1),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 2_000,
        ELLA_RELAY_SESSION,
        52,
        0,
        &p_ella_channel_state(0, 1, 1, 0),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 3_000,
        ELLA_RELAY_SESSION,
        53,
        0,
        &p_ella_channel_id_assigned(1, 7),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 4_000,
        ELLA_RELAY_SESSION,
        52,
        0,
        &p_ella_channel_state(1, 1, 1, 1),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 10_000,
        ELLA_RELAY_SESSION,
        100,
        0,
        &p_ella_channel_subscriber(1, 0, 2, "10.0.0.1", 5000, 1),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START,
        0,
        150,
        0,
        &p_ella_channel_count(1, 1, 0, 0, 1, 1),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 100_000,
        0,
        150,
        0,
        &p_ella_channel_count(1, 2, 1, 0, 3, 2),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 50_000,
        ELLA_RELAY_SESSION,
        54,
        0,
        &p_ella_channel_error(4, 1, 16),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 110_000,
        ELLA_RELAY_SESSION,
        55,
        0,
        &p_ella_channel_destroyed(1, 1, 0),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 111_000,
        ELLA_RELAY_SESSION,
        51,
        0,
        &[],
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 20_000,
        0,
        1,
        0,
        &p_ella_relay_error(1, 1, 9, 1, 0),
    );

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let providers: [(u16, &str); 1] = [(ELLA_RELAY_PID, "ella_relay")];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
    }

    out.extend_from_slice(&w.events);
    out
}

fn p_ella_channel_count_v5(
    instance_id: u32,
    channels: u32,
    leave: u32,
    retire: u32,
    subscribers: u32,
) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [instance_id, channels, leave, retire, subscribers] {
        ella_put_u32(&mut b, v);
    }
    b
}

#[allow(clippy::too_many_arguments)]
fn p_ella_channel_count_total(
    channels_up: u32,
    channels_dn: u32,
    leave_up: u32,
    leave_dn: u32,
    retire_up: u32,
    retire_dn: u32,
    subscribers_up: u32,
    subscribers_dn: u32,
) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [
        channels_up,
        channels_dn,
        leave_up,
        leave_dn,
        retire_up,
        retire_dn,
        subscribers_up,
        subscribers_dn,
    ] {
        ella_put_u32(&mut b, v);
    }
    b
}

fn finish_elog(w: Writer, providers: &[(u16, &str)]) -> Vec<u8> {
    finish_elog_at(w, providers, CAPTURE_START, START_UTC_US)
}

/// [`finish_elog`] with an explicit header anchor, for building the
/// successive files of a rolled capture. Real `elog --roll-*` output
/// advances both fields by the same amount, keeping
/// `start_utc_us - capture_start` (the boot fingerprint the merge
/// validates) constant across the set.
fn finish_elog_at(
    w: Writer,
    providers: &[(u16, &str)],
    capture_start: u64,
    start_utc_us: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&capture_start.to_le_bytes());
    out.extend_from_slice(&start_utc_us.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
    }
    out.extend_from_slice(&w.events);
    out
}

/// One file of a synthetic rolled capture, covering
/// `[CAPTURE_START + offset_us, …)`. Carries:
///   - `SESSION_A`, which runs through every file of the set (the
///     connection-spans-a-boundary case the session merge has to
///     concatenate rather than clobber),
///   - a session unique to this file (`SESSION_B + roll`), so merging
///     is seen to union sessions, not just extend shared ones,
///   - `hpts` STARS aggregate samples and an `if` sample series, the
///     global streams that merge by append,
///   - a re-dumped `IF_ATTACH` for ifindex 1, standing in for the
///     dump-state replay `elog` re-triggers at every roll boundary.
///
/// `provider_names` lets a caller build a file whose provider table
/// conflicts with its siblings'.
pub fn synthetic_elog_roll(roll: u64, offset_us: u64, tcp_provider_name: &str) -> Vec<u8> {
    let w = roll_events(roll, offset_us);
    finish_elog_at(
        w,
        &[
            (TCP_PID, tcp_provider_name),
            (HPTS_PID, "hpts"),
            (IF_PID, "if"),
        ],
        CAPTURE_START + offset_us,
        START_UTC_US + offset_us,
    )
}

/// [`synthetic_elog_roll`] with an explicit header anchor, for building
/// a set whose later files repeat the first file's pair verbatim instead
/// of advancing it (the convention `docs/file-format.md` describes).
pub fn synthetic_elog_roll_anchored(
    roll: u64,
    offset_us: u64,
    capture_start: u64,
    start_utc_us: u64,
) -> Vec<u8> {
    finish_elog_at(
        roll_events(roll, offset_us),
        &[(TCP_PID, "tcp"), (HPTS_PID, "hpts"), (IF_PID, "if")],
        capture_start,
        start_utc_us,
    )
}

/// A rolled file that opens with drained ring backlog: one identity
/// record from `BACKLOG_GAP_US` before the live capture, which backdates
/// the header's `capture_start` while `start_utc_us` still names the
/// live start. The parser trims the origin forward
/// (`effective_capture_start`), so the merge sees a file whose header
/// and live anchors disagree by the backlog span. `offset_us` places the
/// live start and must be at least `BACKLOG_GAP_US`.
pub fn synthetic_elog_roll_with_backlog(roll: u64, offset_us: u64) -> Vec<u8> {
    let live = CAPTURE_START + offset_us;
    let backlog_ts = live - BACKLOG_GAP_US;
    let mut w = Writer::new();
    w.event(
        IF_PID,
        0,
        backlog_ts,
        VNET,
        EVT_IF_INFO,
        0,
        &p_if_info(100_000, 2),
    );
    let live_events = roll_events(roll, offset_us);
    w.events.extend_from_slice(&live_events.events);
    w.count += live_events.count;
    finish_elog_at(
        w,
        &[(TCP_PID, "tcp"), (HPTS_PID, "hpts"), (IF_PID, "if")],
        backlog_ts,
        START_UTC_US + offset_us,
    )
}

fn roll_events(roll: u64, offset_us: u64) -> Writer {
    let base = CAPTURE_START + offset_us;
    let mut w = Writer::new();

    // The long-lived session, present in every file of the set.
    for i in 0..5u64 {
        let ts = base + i * 100_000;
        w.event(
            TCP_PID,
            0,
            ts,
            SESSION_A,
            EVT_RTT,
            100,
            &p_rtt(20_000 + roll as u32, 21_000, 22_000, 19_500, 19_000, 21_500),
        );
    }
    // A session that only exists in this file.
    w.event(
        TCP_PID,
        0,
        base + 10_000,
        SESSION_B + roll,
        EVT_RTT,
        100,
        &p_rtt(30_000, 31_000, 32_000, 29_500, 29_000, 31_500),
    );

    for i in 0..4u64 {
        let ts = base + i * 50_000;
        w.event(
            HPTS_PID,
            0,
            ts,
            0,
            EVT_HPTS_STARS_AGG_SAMPLE,
            0,
            &p_stars_agg_sample(
                100 + roll * 10 + i,
                0,
                3_200_000,
                3_200_000,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                1,
                50_000_000_000,
                4_000_000,
                0,
                0,
                0,
                100,
            ),
        );
    }

    // Identity re-dump at the roll boundary.
    w.event(
        IF_PID,
        0,
        base,
        VNET,
        EVT_IF_ATTACH,
        0,
        &p_if_attach(
            1,
            "cxl0",
            6,
            "cxl",
            1500,
            100_000_000_000,
            &[0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
        ),
    );
    for i in 0..3u64 {
        let ts = base + i * 100_000;
        w.event(
            IF_PID,
            0,
            ts,
            VNET,
            EVT_IF_SAMPLE,
            0,
            &p_if_sample(
                1,
                1_000_000 * (roll + 1),
                5_000_000 * (roll + 1),
                1000,
                4000,
            ),
        );
    }

    w
}

/// Write `contents` to the test temp dir under `name` and return the
/// path, for tests that need real files (the `parse_elogs` path takes
/// paths, not bytes).
pub fn write_temp_elog(name: &str, contents: &[u8]) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, contents).expect("write temp fixture");
    path
}

/// Current `fanout_node` catalog: five-u32 `CHANNEL_COUNT` plus
/// `CHANNEL_COUNT_TOTAL`, provider name `fanout_node`.
pub fn synthetic_elog_fanout_node() -> Vec<u8> {
    let mut w = Writer::new();
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START,
        0,
        0,
        0,
        &p_ella_instance_attach(1, 0),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START,
        0,
        0,
        0,
        &p_ella_instance_attach(2, 1),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 1_000,
        0,
        150,
        0,
        &p_ella_channel_count_v5(1, 2, 0, 1, 4),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 1_000,
        0,
        151,
        0,
        &p_ella_channel_count_total(2, 10, 1, 5, 3, 7, 4, 40),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 20_000,
        0,
        1,
        0,
        &p_ella_relay_error(1, 1, 9, 1, 0),
    );
    finish_elog(w, &[(ELLA_RELAY_PID, "fanout_node")])
}

/// A node that declares itself via `NODE_INFO` while its attach set says
/// otherwise, so the two role sources can be told apart.
pub fn synthetic_elog_fanout_node_info(role: u8) -> Vec<u8> {
    let mut w = Writer::new();
    w.event(ELLA_RELAY_PID, 0, CAPTURE_START, 0, 2, 0, &[role]);
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START,
        0,
        0,
        0,
        &p_ella_instance_attach(0, 1),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 1_000,
        ELLA_RELAY_SESSION,
        50,
        0,
        &p_ella_session_create("live/1", 2, 0),
    );
    finish_elog(w, &[(ELLA_RELAY_PID, "fanout_node")])
}

/// `STATS` keyword only: `CHANNEL_COUNT_TOTAL`, no per-instance samples.
pub fn synthetic_elog_fanout_stats_only() -> Vec<u8> {
    let mut w = Writer::new();
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START,
        0,
        0,
        0,
        &p_ella_instance_attach(1, 0),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START,
        0,
        0,
        0,
        &p_ella_instance_attach(2, 1),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 1_000_000,
        0,
        151,
        0,
        &p_ella_channel_count_total(2, 10, 1, 4, 0, 0, 4, 40),
    );
    w.event(
        ELLA_RELAY_PID,
        0,
        CAPTURE_START + 2_000_000,
        0,
        151,
        0,
        &p_ella_channel_count_total(3, 20, 2, 6, 1, 3, 5, 80),
    );
    finish_elog(w, &[(ELLA_RELAY_PID, "fanout_node")])
}

/// A capture that opens with drained ring backlog: one `NODE_INFO`
/// identity record from long before `elog(1)` attached (so the header's
/// `capture_start` names it, while `START_UTC_US` names the live start),
/// then `BACKLOG_GAP_US` of silence, then the real capture body.
pub fn synthetic_elog_fanout_backlog() -> Vec<u8> {
    let mut w = Writer::new();
    w.event(ELLA_RELAY_PID, 0, CAPTURE_START, 0, 2, 0, &[1]);
    let live = CAPTURE_START + BACKLOG_GAP_US;
    w.event(
        ELLA_RELAY_PID,
        0,
        live,
        0,
        0,
        0,
        &p_ella_instance_attach(1, 0),
    );
    for i in 0..20u64 {
        w.event(
            ELLA_RELAY_PID,
            0,
            live + i * 1_000_000,
            0,
            150,
            0,
            &p_ella_channel_count_v5(1, 2, 0, 1, 4),
        );
    }
    finish_elog(w, &[(ELLA_RELAY_PID, "fanout_node")])
}

/// An "if"-only capture exercising the `IF_SAMPLE_BYTES`/`IF_SAMPLE_PACKETS`
/// pair (ids 21/22) that replaced the deprecated `IF_SAMPLE` (id 20).
/// Covers, in order:
///   - ifindex 1: two clean ticks, each a normal BYTES-then-PACKETS pair —
///     the common case the kernel actually emits.
///   - ifindex 2: a PACKETS event with no preceding BYTES for that
///     ifindex (simulates a capture starting mid-tick) — should decode as
///     a byte-fields-zero record rather than being dropped.
///   - ifindex 1 again: a BYTES event with no follow-up PACKETS before
///     the capture ends (simulates truncation) — should be flushed
///     standalone by the end-of-parse pending flush, not lost.
pub fn synthetic_elog_if_sample_split() -> Vec<u8> {
    let mut w = Writer::new();
    // ifindex 1, tick 1: total 1000/900 bytes, tcp 600/500, udp 300/300
    // -> other = 100/100.
    w.event(
        IF_PID,
        0,
        CAPTURE_START,
        VNET,
        EVT_IF_SAMPLE_BYTES,
        0,
        &p_if_sample_bytes(1, 1000, 900, 600, 500, 300, 300),
    );
    w.event(
        IF_PID,
        0,
        CAPTURE_START,
        VNET,
        EVT_IF_SAMPLE_PACKETS,
        0,
        &p_if_sample_packets(1, 10, 9, 6, 5, 3, 3),
    );
    // ifindex 1, tick 2: cumulative counters advance.
    w.event(
        IF_PID,
        0,
        CAPTURE_START + 100_000,
        VNET,
        EVT_IF_SAMPLE_BYTES,
        0,
        &p_if_sample_bytes(1, 2000, 1800, 1200, 1000, 600, 600),
    );
    w.event(
        IF_PID,
        0,
        CAPTURE_START + 100_000,
        VNET,
        EVT_IF_SAMPLE_PACKETS,
        0,
        &p_if_sample_packets(1, 20, 18, 12, 10, 6, 6),
    );
    // ifindex 2: packets-only (no preceding bytes half for this ifindex).
    w.event(
        IF_PID,
        0,
        CAPTURE_START + 100_000,
        VNET,
        EVT_IF_SAMPLE_PACKETS,
        0,
        &p_if_sample_packets(2, 5, 4, 1, 1, 1, 1),
    );
    // ifindex 1: an orphaned bytes half at the very end of the capture,
    // never followed by a packets half.
    w.event(
        IF_PID,
        0,
        CAPTURE_START + 200_000,
        VNET,
        EVT_IF_SAMPLE_BYTES,
        0,
        &p_if_sample_bytes(1, 3000, 2700, 1800, 1500, 900, 900),
    );

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let providers: [(u16, &str); 1] = [(IF_PID, "if")];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
    }

    out.extend_from_slice(&w.events);
    out
}

/// A minimal "tcp"-only capture with a couple of `STARS_PACE` (id 604)
/// events on one session: one from the common-return scaler (squeezed
/// below natural) and one from the no-rate-estimate placeholder path
/// (natural_bps == 1<<24, unsqueezed since pressure is 0 here) -- the two
/// shapes NCD-9761's `TCP_EVENTLOG_STARS_PACE_LOG` call sites emit.
pub fn synthetic_elog_stars_pace() -> Vec<u8> {
    let mut w = Writer::new();
    w.event(
        TCP_PID,
        0,
        CAPTURE_START,
        SESSION_A,
        EVT_STARS_PACE,
        100,
        &p_stars_pace(100_000_000, 25_000_000, 768, 1_460),
    );
    w.event(
        TCP_PID,
        0,
        CAPTURE_START + 1_000,
        SESSION_A,
        EVT_STARS_PACE,
        100,
        &p_stars_pace(1 << 24, 1 << 24, 0, 2_920),
    );

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let providers: [(u16, &str); 1] = [(TCP_PID, "tcp")];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
    }

    out.extend_from_slice(&w.events);
    out
}

/// The 21 `STARS_AGG_SAMPLE` field values the truncation fixture writes,
/// in schema order. Each is distinct so a test can tell a correctly read
/// field from a neighbour read at the wrong offset.
pub const STARS_AGG_FIELD_VALUES: [u64; 21] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
];

/// One `STARS_AGG_SAMPLE` capture per historical payload width: 128 bytes
/// (pre-rollover), 152 (pre-headroom), 160 (pre-rxt_no_pace) and 168 (all
/// fields). The record has grown by append four times, and each older width
/// is still present in captures on disk, so the decoder has to keep reading
/// every one of them rather than rejecting short payloads.
pub fn synthetic_elog_stars_agg_truncations() -> Vec<u8> {
    let mut w = Writer::new();
    let mut full = Vec::with_capacity(168);
    for x in STARS_AGG_FIELD_VALUES {
        full.extend_from_slice(&x.to_le_bytes());
    }
    for (i, nbytes) in [128usize, 152, 160, 168].iter().enumerate() {
        w.event(
            HPTS_PID,
            0,
            CAPTURE_START + i as u64 * 1_000,
            0,
            EVT_HPTS_STARS_AGG_SAMPLE,
            100,
            &full[..*nbytes],
        );
    }

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let providers: [(u16, &str); 1] = [(HPTS_PID, "hpts")];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
    }

    out.extend_from_slice(&w.events);
    out
}

// ---------------------------------------------------------------------------
// EventLog v2 support: a hand-rolled `EDB1` decode-blob builder + a small
// v2 synthetic capture, independent of `src/elog_parser/schema.rs`'s own
// `#[cfg(test)]` builder (which integration tests can't see — it isn't
// compiled into the library integration tests link against). Encodes the
// wire format exactly as documented in `docs/file-format.md`'s "Version 2"
// section, so a bug in the real parser can't be masked by sharing broken
// serialization code with it.
// ---------------------------------------------------------------------------

pub const EDB_TYPE_U8: u8 = 1;
pub const EDB_TYPE_U16: u8 = 2;
pub const EDB_TYPE_U32: u8 = 3;
pub const EDB_TYPE_U64: u8 = 4;

pub struct EdbField {
    pub name: &'static str,
    pub type_: u8,
    pub offset: u16,
}

impl EdbField {
    pub fn scalar(name: &'static str, type_: u8, offset: u16) -> Self {
        EdbField {
            name,
            type_,
            offset,
        }
    }
}

pub struct EdbStruct {
    pub name: &'static str,
    pub fields: Vec<EdbField>,
}

pub struct EdbEvent {
    pub id: u32,
    pub name: &'static str,
    /// 1-based index into the blob's struct list; `0` means no payload.
    pub struct_index: u16,
    pub format: &'static str,
}

fn edb_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Build a raw `EDB1` decode blob: no keywords/enums/flags, just the given
/// structs and events (in order, so `EdbEvent::struct_index == 1` refers
/// to `structs[0]`). Every field is a plain scalar (no annotation, no
/// varlen) — sufficient for the divergence/verification tests, which only
/// need offset+size comparisons and simple positional format rendering.
pub fn build_edb1(provider_name: &str, structs: &[EdbStruct], events: &[EdbEvent]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"EDB1");
    out.extend_from_slice(&1u16.to_le_bytes()); // version
    out.push(8); // word_size
    out.push(0); // endianness = LE
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    edb_str(&mut out, provider_name);
    out.extend_from_slice(&0u16.to_le_bytes()); // nkeywords
    out.extend_from_slice(&0u16.to_le_bytes()); // nenums
    out.extend_from_slice(&0u16.to_le_bytes()); // nflags
    out.extend_from_slice(&(structs.len() as u16).to_le_bytes());
    out.extend_from_slice(&(events.len() as u16).to_le_bytes());

    for s in structs {
        edb_str(&mut out, s.name);
        out.extend_from_slice(&(s.fields.len() as u16).to_le_bytes());
        for f in &s.fields {
            edb_str(&mut out, f.name);
            out.push(f.type_);
            out.push(0); // annotation = none
            out.extend_from_slice(&0u16.to_le_bytes()); // annot_index
            out.extend_from_slice(&0u16.to_le_bytes()); // char_len
            out.extend_from_slice(&f.offset.to_le_bytes());
            out.push(0); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // varlen_count_index
            out.extend_from_slice(&0u16.to_le_bytes()); // varlen_max
        }
    }

    for e in events {
        out.extend_from_slice(&e.id.to_le_bytes());
        edb_str(&mut out, e.name);
        out.push(0); // level
        out.extend_from_slice(&0u32.to_le_bytes()); // keywords
        out.extend_from_slice(&e.struct_index.to_le_bytes());
        edb_str(&mut out, e.format);
    }

    out
}

/// `SHA256(blob)[..16]` — mirrors `compute_schema_id` /
/// `EVENTLOG_DECODE_SCHEMA_ID_LEN`. Implemented independently (not by
/// calling into the crate under test) so this fixture doesn't validate
/// itself with the same code the parser uses to verify it.
pub fn edb_schema_id(blob: &[u8]) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(blob);
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// A small v2 capture exercising all three schema-routing outcomes at
/// once:
///   - `tcp` `IN` (id 100): decoder's 10 fields (seq/ack/win/len/flags/
///     state/th_off/th_sum/th_urp/rcv_time_us) match the schema exactly,
///     plus one additive trailing field (`future_flag`) the decoder
///     doesn't read — verified, decodes via the normal hardcoded path,
///     and produces a `schema_new_fields` notice.
///   - `tcp` `DSACK` (id 402): schema only defines the first two of
///     the six fields `schema_validate::TCP_EXPECTED` requires
///     (`dsack_round_seen`/`dsack_round_end`) — missing `num_dsack`
///     (among others) is a field-not-present mismatch — divergent,
///     falls back to generic schema decode.
///   - `hpts` timing-wheel ids 0 (`INIT`) and 12 (`PERF_LATENESS`): no
///     divergence tables apply route-wise (the hardcoded `hpts_wheel.rs`
///     decoder always handles these ids regardless of schema
///     resolution), but a matching schema is embedded anyway so
///     `Provider.schema_name`/`schema_event_count` are exercised and
///     confirmed verified (no drift).
pub fn synthetic_elog_v2() -> (Vec<u8>, u64) {
    let hpts_blob = build_edb1(
        "hpts",
        &[
            EdbStruct {
                name: "INIT",
                fields: vec![
                    EdbField::scalar("num_hptss", EDB_TYPE_U32, 0),
                    EdbField::scalar("bind_type", EDB_TYPE_U8, 4),
                    EdbField::scalar("num_slots", EDB_TYPE_U32, 8),
                    EdbField::scalar("usecs_per_slot", EDB_TYPE_U32, 12),
                    EdbField::scalar("total_slot_time", EDB_TYPE_U32, 16),
                ],
            },
            EdbStruct {
                name: "PERF_LATENESS",
                fields: vec![
                    EdbField::scalar("gencnt", EDB_TYPE_U32, 0),
                    EdbField::scalar("lateness_slots", EDB_TYPE_U32, 4),
                    EdbField::scalar("lateness_usec", EDB_TYPE_U32, 8),
                    EdbField::scalar("requested_time", EDB_TYPE_U32, 12),
                    EdbField::scalar("actual_time", EDB_TYPE_U32, 16),
                ],
            },
        ],
        &[
            EdbEvent {
                id: 0,
                name: "INIT",
                struct_index: 1,
                format: "",
            },
            EdbEvent {
                id: 12,
                name: "PERF_LATENESS",
                struct_index: 2,
                format: "",
            },
        ],
    );

    let tcp_blob = build_edb1(
        "tcp",
        &[
            EdbStruct {
                name: "IN",
                fields: vec![
                    EdbField::scalar("seq", EDB_TYPE_U32, 0),
                    EdbField::scalar("ack", EDB_TYPE_U32, 4),
                    EdbField::scalar("win", EDB_TYPE_U32, 8),
                    EdbField::scalar("len", EDB_TYPE_U32, 12),
                    EdbField::scalar("flags", EDB_TYPE_U16, 16),
                    EdbField::scalar("state", EDB_TYPE_U8, 18),
                    EdbField::scalar("th_off", EDB_TYPE_U8, 19),
                    EdbField::scalar("th_sum", EDB_TYPE_U16, 20),
                    EdbField::scalar("th_urp", EDB_TYPE_U16, 22),
                    EdbField::scalar("rcv_time_us", EDB_TYPE_U64, 24),
                    // Additive: not in `schema_validate::TCP_EXPECTED`,
                    // so this is the one field that should produce a
                    // `schema_new_fields` notice.
                    EdbField::scalar("future_flag", EDB_TYPE_U32, 32),
                ],
            },
            EdbStruct {
                name: "DSACK",
                fields: vec![
                    EdbField::scalar("dsack_round_seen", EDB_TYPE_U8, 0),
                    EdbField::scalar("dsack_round_end", EDB_TYPE_U32, 4),
                ],
            },
        ],
        &[
            EdbEvent {
                id: 100,
                name: "IN",
                struct_index: 1,
                format: "",
            },
            EdbEvent {
                id: 402,
                name: "DSACK",
                struct_index: 2,
                format: "seen=%1 round_end=%2",
            },
        ],
    );

    let hpts_schema_id = edb_schema_id(&hpts_blob);
    let tcp_schema_id = edb_schema_id(&tcp_blob);

    const V2_HPTS_PID: u16 = 10;
    const V2_TCP_PID: u16 = 20;
    let tcp_session: u64 = 0x5000;

    let mut w = Writer::new();
    w.event(
        V2_HPTS_PID,
        0,
        CAPTURE_START,
        0,
        0, /* EVT_HPTS_INIT */
        0,
        &{
            let mut v = vec![0u8; 20];
            v[0..4].copy_from_slice(&4u32.to_le_bytes()); // num_hptss
            v[4] = 1; // bind_type = CPU
            v[8..12].copy_from_slice(&4096u32.to_le_bytes()); // num_slots
            v[12..16].copy_from_slice(&100u32.to_le_bytes()); // usecs_per_slot
            v[16..20].copy_from_slice(&409_600u32.to_le_bytes()); // total_slot_time
            v
        },
    );
    w.event(
        V2_HPTS_PID,
        0,
        CAPTURE_START + 1_000,
        0,
        12, // EVT_HPTS_PERF_LATENESS
        0,
        &{
            let mut v = vec![0u8; 20];
            v[0..4].copy_from_slice(&555u32.to_le_bytes()); // gencnt
            v[4..8].copy_from_slice(&3u32.to_le_bytes()); // lateness_slots
            v[8..12].copy_from_slice(&250u32.to_le_bytes()); // lateness_usec
            v[12..16].copy_from_slice(&1_000u32.to_le_bytes()); // requested_time
            v[16..20].copy_from_slice(&1_250u32.to_le_bytes()); // actual_time
            v
        },
    );

    w.event(
        V2_TCP_PID,
        0,
        CAPTURE_START,
        tcp_session,
        100, // EVT_IN
        100,
        &{
            let mut v = vec![0u8; 36];
            v[0..4].copy_from_slice(&1000u32.to_le_bytes()); // seq
            v[4..8].copy_from_slice(&2000u32.to_le_bytes()); // ack
            v[8..12].copy_from_slice(&65535u32.to_le_bytes()); // win
            v[12..16].copy_from_slice(&1460u32.to_le_bytes()); // len
            v[16..18].copy_from_slice(&0x10u16.to_le_bytes()); // flags
            v[18] = 4; // state
            v[19] = 5; // th_off
            v[20..22].copy_from_slice(&0xabcdu16.to_le_bytes()); // th_sum
            v[22..24].copy_from_slice(&0u16.to_le_bytes()); // th_urp
            v[24..32].copy_from_slice(&999_000u64.to_le_bytes()); // rcv_time_us
            v[32..36].copy_from_slice(&7u32.to_le_bytes()); // future_flag -- additive, not read by the decoder
            v
        },
    );
    w.event(
        V2_TCP_PID,
        0,
        CAPTURE_START + 100,
        tcp_session,
        402, // EVT_DSACK
        100,
        &{
            let mut v = vec![0u8; 8];
            v[0] = 1; // dsack_round_seen
            v[4..8].copy_from_slice(&12345u32.to_le_bytes()); // dsack_round_end
            v
        },
    );

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&2u32.to_le_bytes()); // version = 2
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let providers: [(u16, &str, [u8; 16]); 2] = [
        (V2_HPTS_PID, "hpts", hpts_schema_id),
        (V2_TCP_PID, "tcp", tcp_schema_id),
    ];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name, sid) in &providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
        out.extend_from_slice(sid);
    }

    let blobs: [([u8; 16], &[u8]); 2] = [(hpts_schema_id, &hpts_blob), (tcp_schema_id, &tcp_blob)];
    out.extend_from_slice(&(blobs.len() as u32).to_le_bytes());
    for (sid, blob) in &blobs {
        out.extend_from_slice(sid);
        out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        out.extend_from_slice(blob);
    }

    out.extend_from_slice(&w.events);
    (out, tcp_session)
}

/// A v2 capture whose embedded `tcp` schema reflects a kernel that
/// reordered `STRUCT IN`/`STRUCT OUT` (`win` moved from offset 8 to 12,
/// with a new `win_raw` field taking its old slot, and `OUT` gaining a
/// `state` field IN already had) -- the same shape of drift
/// `schema_validate::TCP_EXPECTED`'s hardcoded offsets diverge from in
/// practice. Both events' wire payloads are filled with values that
/// only come out correct if the *new* offsets are honored; decoding
/// via the old hardcoded offsets would produce different numbers for
/// `win`/`len`/`state`/etc, so a regression back to "divergent event
/// dropped to generic" or "decoded with stale offsets" is caught either
/// way. Returns the byte vector and the session id carrying both events.
pub fn synthetic_elog_v2_in_out_drifted() -> (Vec<u8>, u64) {
    let tcp_blob = build_edb1(
        "tcp",
        &[
            EdbStruct {
                name: "IN",
                fields: vec![
                    EdbField::scalar("seq", EDB_TYPE_U32, 0),
                    EdbField::scalar("ack", EDB_TYPE_U32, 4),
                    EdbField::scalar("flags", EDB_TYPE_U16, 8),
                    EdbField::scalar("win_raw", EDB_TYPE_U16, 10),
                    EdbField::scalar("win", EDB_TYPE_U32, 12),
                    EdbField::scalar("len", EDB_TYPE_U32, 16),
                    EdbField::scalar("state", EDB_TYPE_U8, 20),
                    EdbField::scalar("th_off", EDB_TYPE_U8, 21),
                    EdbField::scalar("th_urp", EDB_TYPE_U16, 22),
                    EdbField::scalar("rcv_time_us", EDB_TYPE_U64, 24),
                ],
            },
            EdbStruct {
                name: "OUT",
                fields: vec![
                    EdbField::scalar("seq", EDB_TYPE_U32, 0),
                    EdbField::scalar("ack", EDB_TYPE_U32, 4),
                    EdbField::scalar("flags", EDB_TYPE_U16, 8),
                    EdbField::scalar("win_raw", EDB_TYPE_U16, 10),
                    EdbField::scalar("win", EDB_TYPE_U32, 12),
                    EdbField::scalar("len", EDB_TYPE_U32, 16),
                    EdbField::scalar("cwnd", EDB_TYPE_U32, 20),
                    EdbField::scalar("state", EDB_TYPE_U8, 24),
                    EdbField::scalar("th_off", EDB_TYPE_U8, 25),
                    EdbField::scalar("th_urp", EDB_TYPE_U16, 26),
                ],
            },
        ],
        &[
            EdbEvent {
                id: 100, // EVT_IN
                name: "IN",
                struct_index: 1,
                format: "",
            },
            EdbEvent {
                id: 101, // EVT_OUT
                name: "OUT",
                struct_index: 2,
                format: "",
            },
        ],
    );
    let tcp_schema_id = edb_schema_id(&tcp_blob);

    const V2_TCP_PID: u16 = 30;
    let tcp_session: u64 = 0x6000;

    let mut w = Writer::new();
    w.event(
        V2_TCP_PID,
        0,
        CAPTURE_START,
        tcp_session,
        100, // EVT_IN
        100,
        &{
            let mut v = vec![0u8; 32];
            v[0..4].copy_from_slice(&1000u32.to_le_bytes()); // seq
            v[4..8].copy_from_slice(&2000u32.to_le_bytes()); // ack
            v[8..10].copy_from_slice(&0x18u16.to_le_bytes()); // flags
            v[10..12].copy_from_slice(&999u16.to_le_bytes()); // win_raw
            v[12..16].copy_from_slice(&54321u32.to_le_bytes()); // win
            v[16..20].copy_from_slice(&1300u32.to_le_bytes()); // len
            v[20] = 6; // state
            v[21] = 5; // th_off
            v[22..24].copy_from_slice(&42u16.to_le_bytes()); // th_urp
            v[24..32].copy_from_slice(&888_000u64.to_le_bytes()); // rcv_time_us
            v
        },
    );
    w.event(
        V2_TCP_PID,
        0,
        CAPTURE_START + 100,
        tcp_session,
        101, // EVT_OUT
        100,
        &{
            let mut v = vec![0u8; 28];
            v[0..4].copy_from_slice(&3000u32.to_le_bytes()); // seq
            v[4..8].copy_from_slice(&4000u32.to_le_bytes()); // ack
            v[8..10].copy_from_slice(&0x10u16.to_le_bytes()); // flags
            v[10..12].copy_from_slice(&888u16.to_le_bytes()); // win_raw
            v[12..16].copy_from_slice(&12345u32.to_le_bytes()); // win
            v[16..20].copy_from_slice(&1460u32.to_le_bytes()); // len
            v[20..24].copy_from_slice(&(11 * 1460u32).to_le_bytes()); // cwnd
            v[24] = 4; // state
            v[25] = 5; // th_off
            v[26..28].copy_from_slice(&7u16.to_le_bytes()); // th_urp
            v
        },
    );

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&2u32.to_le_bytes()); // version = 2
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let providers: [(u16, &str, [u8; 16]); 1] = [(V2_TCP_PID, "tcp", tcp_schema_id)];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name, sid) in &providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
        out.extend_from_slice(sid);
    }

    let blobs: [([u8; 16], &[u8]); 1] = [(tcp_schema_id, &tcp_blob)];
    out.extend_from_slice(&(blobs.len() as u32).to_le_bytes());
    for (sid, blob) in &blobs {
        out.extend_from_slice(sid);
        out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        out.extend_from_slice(blob);
    }

    out.extend_from_slice(&w.events);
    (out, tcp_session)
}

/// A v3 capture where a second "tcp" stack (`LATE_TCP_PID`) registers
/// *after* the file header was written: its provider row is absent from
/// the header's provider table, and it's introduced mid-stream via an
/// `EVT_PROVIDER_INFO` announcement (id `0xFFFFFFFC`) followed by an
/// `EVT_PROVIDER_BLOB` announcement (id `0xFFFFFFFB`) carrying its `EDB1`
/// schema, exactly as `elog(1)`'s `write_provider_announcement()` emits
/// them for a provider discovered mid-capture. Returns the byte vector,
/// the late provider's own IN event's session id, and its provider id.
pub fn synthetic_elog_v3_late_provider() -> (Vec<u8>, u64, u16) {
    const EARLY_TCP_PID: u16 = 1;
    const LATE_TCP_PID: u16 = 99;
    let early_session: u64 = 0x1000;
    let late_session: u64 = 0x2000;

    let in_payload = |seq: u32, ack: u32, win: u32, len: u32, flags: u16, state: u8| -> Vec<u8> {
        let mut v = vec![0u8; 32];
        v[0..4].copy_from_slice(&seq.to_le_bytes());
        v[4..8].copy_from_slice(&ack.to_le_bytes());
        v[8..12].copy_from_slice(&win.to_le_bytes());
        v[12..16].copy_from_slice(&len.to_le_bytes());
        v[16..18].copy_from_slice(&flags.to_le_bytes());
        v[18] = state;
        v[19] = 5; // th_off
        v
    };

    // Matches `schema_validate::TCP_EXPECTED`'s IN entry exactly (same
    // fields, same offsets, nothing additive) so the late provider's
    // schema resolves as fully verified: no divergence, no new-fields
    // notice.
    let late_tcp_blob = build_edb1(
        "tcp",
        &[EdbStruct {
            name: "IN",
            fields: vec![
                EdbField::scalar("seq", EDB_TYPE_U32, 0),
                EdbField::scalar("ack", EDB_TYPE_U32, 4),
                EdbField::scalar("win", EDB_TYPE_U32, 8),
                EdbField::scalar("len", EDB_TYPE_U32, 12),
                EdbField::scalar("flags", EDB_TYPE_U16, 16),
                EdbField::scalar("state", EDB_TYPE_U8, 18),
                EdbField::scalar("th_off", EDB_TYPE_U8, 19),
                EdbField::scalar("th_sum", EDB_TYPE_U16, 20),
                EdbField::scalar("th_urp", EDB_TYPE_U16, 22),
                EdbField::scalar("rcv_time_us", EDB_TYPE_U64, 24),
            ],
        }],
        &[EdbEvent {
            id: 100, // EVT_IN
            name: "IN",
            struct_index: 1,
            format: "",
        }],
    );
    let late_tcp_schema_id = edb_schema_id(&late_tcp_blob);

    let mut w = Writer::new();
    w.event(
        EARLY_TCP_PID,
        0,
        CAPTURE_START,
        early_session,
        100, // EVT_IN
        100,
        &in_payload(1000, 2000, 65535, 1460, 0x10, 4),
    );
    w.event(
        LATE_TCP_PID,
        0,
        CAPTURE_START + 1_000,
        0,
        0xFFFF_FFFC, // EVT_PROVIDER_INFO
        -1,
        &{
            let mut v = vec![0u8; 50];
            v[0..2].copy_from_slice(&LATE_TCP_PID.to_le_bytes());
            v[2..34].copy_from_slice(&cstr("tcp", 32));
            v[34..50].copy_from_slice(&late_tcp_schema_id);
            v
        },
    );
    w.event(
        LATE_TCP_PID,
        0,
        CAPTURE_START + 1_000,
        0,
        0xFFFF_FFFB, // EVT_PROVIDER_BLOB
        -1,
        &{
            let mut v = Vec::with_capacity(16 + 4 + late_tcp_blob.len());
            v.extend_from_slice(&late_tcp_schema_id);
            v.extend_from_slice(&(late_tcp_blob.len() as u32).to_le_bytes());
            v.extend_from_slice(&late_tcp_blob);
            v
        },
    );
    w.event(
        LATE_TCP_PID,
        0,
        CAPTURE_START + 2_000,
        late_session,
        100, // EVT_IN
        200,
        &in_payload(5000, 6000, 32768, 1460, 0x18, 4),
    );

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&3u32.to_le_bytes()); // version
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes()); // event_count
    out.extend_from_slice(&0u32.to_le_bytes()); // counter_count

    let providers: [(u16, &str); 1] = [(EARLY_TCP_PID, "tcp")];
    out.extend_from_slice(&(providers.len() as u32).to_le_bytes());
    for (id, name) in providers {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
        out.extend_from_slice(&[0u8; 16]); // schema_id: none registered up front
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // blob_count: nothing embedded at header time

    out.extend_from_slice(&w.events);
    (out, late_session, LATE_TCP_PID)
}

/// Build the full synthetic capture as a v3 `.elog` byte vector: the same
/// events/providers as [`synthetic_elog`], but with the v3 header (36-byte
/// fixed part followed by a `counter_count`-sized trailing `u64` array)
/// instead of v1/v2's single `dropped_events:u64` at offset 32. Provider
/// rows are still the 50-byte v2+ shape (with an all-zero `schema_id`,
/// i.e. "no blob registered"), and an empty decode-blob section follows
/// the provider table, since `is_v2 = version >= 2` gates both of those on
/// version alone -- real v3 producers always write them. Exercising the
/// header layout together with a *non-empty* blob section isn't needed
/// here: that combination is already covered by `synthetic_elog_v2`, and
/// the two code paths (header parsing, blob-section parsing) are
/// independent.
pub fn synthetic_elog_v3(counters: &[u64]) -> Vec<u8> {
    let w = build_events();

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&3u32.to_le_bytes()); // version
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes()); // event_count
    out.extend_from_slice(&(counters.len() as u32).to_le_bytes()); // counter_count
    for c in counters {
        out.extend_from_slice(&c.to_le_bytes());
    }

    out.extend_from_slice(&(PROVIDERS.len() as u32).to_le_bytes());
    for (id, name) in PROVIDERS {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&cstr(name, 32));
        out.extend_from_slice(&[0u8; 16]); // schema_id: none registered
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // blob_count: no embedded decode blobs

    out.extend_from_slice(&w.events);
    out
}

/// Write the synthetic capture to the test temp dir and return its path.
///
/// Written exactly once per test binary (tests run in parallel threads of a
/// single process), so concurrent CLI readers never observe a partial file.
pub fn fixture_path() -> PathBuf {
    use std::sync::OnceLock;
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let path = dir.join("synthetic.elog");
        std::fs::write(&path, synthetic_elog()).expect("write fixture");
        path
    })
    .clone()
}

/// Compare `actual` against the golden file at `tests/golden/<name>`.
///
/// Set `UPDATE_GOLDEN=1` to (re)write the golden instead of asserting.
pub fn check_golden(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {} (run tests with UPDATE_GOLDEN=1 to create it)",
            path.display()
        )
    });
    assert_eq!(actual, expected, "golden mismatch for {name}");
}

const MEM_PID: u16 = 40;

fn p_mem_sys_sample(vals: [u64; 9]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// A v2 `mem`-only capture: one MEM_INFO then two SYS_SAMPLEs, with an
/// embedded schema matching the kernel's packed layout
/// (`mem_eventlog_schema.src`).
pub fn synthetic_elog_mem() -> Vec<u8> {
    let u64s = |names: &[&'static str], base: u16| -> Vec<EdbField> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| EdbField::scalar(n, EDB_TYPE_U64, base + 8 * i as u16))
            .collect()
    };
    let mut info_fields = vec![
        EdbField::scalar("page_size", EDB_TYPE_U32, 0),
        EdbField::scalar("ndomains", EDB_TYPE_U16, 4),
        EdbField::scalar("sample_period_us", EDB_TYPE_U32, 6),
    ];
    info_fields.extend(u64s(&["physmem", "usable", "swap_total"], 10));
    let blob = build_edb1(
        "mem",
        &[
            EdbStruct {
                name: "MEM_INFO",
                fields: info_fields,
            },
            EdbStruct {
                name: "SYS_SAMPLE",
                fields: u64s(
                    &[
                        "free",
                        "active",
                        "inactive",
                        "laundry",
                        "wired",
                        "free_target",
                        "free_min",
                        "free_severe",
                        "swap_used",
                    ],
                    0,
                ),
            },
        ],
        &[
            EdbEvent {
                id: 0,
                name: "MEM_INFO",
                struct_index: 1,
                format: "",
            },
            EdbEvent {
                id: 1,
                name: "SYS_SAMPLE",
                struct_index: 2,
                format: "",
            },
        ],
    );
    let sid = edb_schema_id(&blob);
    const GIB: u64 = 1 << 30;

    let mut w = Writer::new();
    let mut info = Vec::new();
    info.extend_from_slice(&16384u32.to_le_bytes());
    info.extend_from_slice(&2u16.to_le_bytes());
    info.extend_from_slice(&1_000_000u32.to_le_bytes());
    for v in [64 * GIB, 62 * GIB, 8 * GIB] {
        info.extend_from_slice(&v.to_le_bytes());
    }
    w.event(MEM_PID, 0, CAPTURE_START, 0, 0, 0, &info);
    w.event(
        MEM_PID,
        0,
        CAPTURE_START + 1_000,
        0,
        1,
        0,
        &p_mem_sys_sample([
            10 * GIB,
            20 * GIB,
            25 * GIB,
            GIB,
            6 * GIB,
            GIB,
            GIB / 2,
            GIB / 4,
            0,
        ]),
    );
    w.event(
        MEM_PID,
        0,
        CAPTURE_START + 1_001_000,
        0,
        1,
        0,
        &p_mem_sys_sample([
            9 * GIB,
            21 * GIB,
            25 * GIB,
            GIB,
            6 * GIB,
            GIB,
            GIB / 2,
            GIB / 4,
            GIB,
        ]),
    );

    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&MEM_PID.to_le_bytes());
    out.extend_from_slice(&cstr("mem", 32));
    out.extend_from_slice(&sid);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&sid);
    out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    out.extend_from_slice(&blob);
    out.extend_from_slice(&w.events);
    out
}

// ---------------------------------------------------------------------------
// nginx captures carrying the real ocafirmware decode blobs.
//
// `nginx_schema_ncd10174.edb` / `nginx_schema_ncd10222.edb` are the `EDB1`
// images `eventlog_gen.awk` emits for `nginx_eventlog_schema.src` at those
// firmware changes (packed structs, varlen `uri`/`msg`/`peer`). Payloads
// below are packed to match.
// ---------------------------------------------------------------------------

pub const NGINX_BLOB_NCD10174: &[u8] = include_bytes!("nginx_schema_ncd10174.edb");
pub const NGINX_BLOB_NCD10222: &[u8] = include_bytes!("nginx_schema_ncd10222.edb");
pub const NGINX_SCHEMA_PID: u16 = 30;

/// Little-endian packed payload builder (C `__packed` layout).
#[derive(Default)]
pub struct Packed(pub Vec<u8>);

impl Packed {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    pub fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i32(self, v: i32) -> Self {
        self.u32(v as u32)
    }
    pub fn u64(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i64(self, v: i64) -> Self {
        self.u64(v as u64)
    }
    pub fn chars(mut self, s: &str, width: usize) -> Self {
        let mut v = vec![0u8; width];
        let n = s.len().min(width - 1);
        v[..n].copy_from_slice(&s.as_bytes()[..n]);
        self.0.extend_from_slice(&v);
        self
    }
    /// `<name>_len:uint16_t` followed by the varlen bytes.
    pub fn varlen(mut self, s: &str) -> Self {
        self.0.extend_from_slice(&(s.len() as u16).to_le_bytes());
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    pub fn done(self) -> Vec<u8> {
        self.0
    }
}

/// One nginx event for [`synthetic_elog_nginx_schema`]:
/// `(ts offset from CAPTURE_START, session, event id, payload)`.
pub type NginxSchemaEvent = (u64, u64, u32, Vec<u8>);

/// A v2 capture with one `nginx` provider carrying `blob`.
pub fn synthetic_elog_nginx_schema(blob: &[u8], events: &[NginxSchemaEvent]) -> Vec<u8> {
    let mut w = Writer::new();
    for (dt, session, id, payload) in events {
        w.event(
            NGINX_SCHEMA_PID,
            0,
            CAPTURE_START + dt,
            *session,
            *id,
            0,
            payload,
        );
    }
    let sid = edb_schema_id(blob);
    let mut out = Vec::new();
    out.extend_from_slice(b"ELOG");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&CAPTURE_START.to_le_bytes());
    out.extend_from_slice(&START_UTC_US.to_le_bytes());
    out.extend_from_slice(&w.count.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&NGINX_SCHEMA_PID.to_le_bytes());
    out.extend_from_slice(&cstr("nginx", 32));
    out.extend_from_slice(&sid);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&sid);
    out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    out.extend_from_slice(blob);
    out.extend_from_slice(&w.events);
    out
}
