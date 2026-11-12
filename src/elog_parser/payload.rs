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

// ---------------------------------------------------------------------------
// Payload parsers
//
// Without an embedded schema, struct formats mirror C natural alignment
// on amd64. With a schema, fields are read by name at the blob's offsets
// (packed or aligned). Size guards apply only to the hardcoded path.
// ---------------------------------------------------------------------------

pub fn parse_payload(
    event_id: u32,
    data: &[u8],
    off: usize,
    size: usize,
    schema: Option<&ProviderSchema>,
) -> EventPayload {
    let r = PayloadReader::new(schema, event_id, data, off, size);
    match event_id {
        // Packed 12 B (`<4sH4sH`) or padded 14 B (`<4sH2x4sH`); ports ntohs.
        EVT_CONN_SET_IP_V4 if r.typed() || size >= 12 => {
            let (dst_off, dport_off) = if size >= 14 { (8, 12) } else { (6, 10) };
            EventPayload::ConnSetIpV4 {
                src_addr: r.in_addr("src_addr", 0),
                src_port: r.port("src_port", 4),
                dst_addr: r.in_addr("dst_addr", dst_off),
                dst_port: r.port("dst_port", dport_off),
            }
        }
        EVT_CONN_SET_IP_V6 if r.typed() || size >= 36 => EventPayload::ConnSetIpV6 {
            src_addr: r.in6_addr("src_addr", 0),
            src_port: r.port("src_port", 16),
            dst_addr: r.in6_addr("dst_addr", 18),
            dst_port: r.port("dst_port", 34),
        },
        EVT_LOG_ID if r.typed() || size >= 64 => EventPayload::LogId {
            log_id: r.cstr("log_id", 0, 64),
        },
        EVT_IN if r.typed() || size >= 32 => EventPayload::In {
            seq: r.u32("seq", 0),
            ack: r.u32("ack", 4),
            win: r.u32("win", 8),
            len: r.u32("len", 12),
            flags: r.u16("flags", 16),
            state: r.u8("state", 18),
            th_off: r.u8("th_off", 19),
            th_sum: r.u16("th_sum", 20),
            th_urp: r.u16("th_urp", 22),
            rcv_time_us: r.u64("rcv_time_us", 24),
        },
        EVT_IN if size >= 18 => EventPayload::In {
            seq: r.u32("seq", 0),
            ack: r.u32("ack", 4),
            win: r.u32("win", 8),
            len: r.u32("len", 12),
            flags: r.u16("flags", 16),
            state: 0,
            th_off: 0,
            th_sum: 0,
            th_urp: 0,
            rcv_time_us: 0,
        },
        EVT_OUT if r.typed() || size >= 30 => EventPayload::Out {
            seq: r.u32("seq", 0),
            ack: r.u32("ack", 4),
            win: r.u32("win", 8),
            len: r.u32("len", 12),
            flags: r.u16("flags", 16),
            cwnd: r.u32("cwnd", 20),
            th_off: r.u8("th_off", 24),
            th_sum: r.u16("th_sum", 26),
            th_urp: r.u16("th_urp", 28),
        },
        EVT_OUT if size >= 26 => EventPayload::Out {
            seq: r.u32("seq", 0),
            ack: r.u32("ack", 4),
            win: r.u32("win", 8),
            len: r.u32("len", 12),
            flags: r.u16("flags", 16),
            cwnd: r.u32("cwnd", 20),
            th_off: 0,
            th_sum: 0,
            th_urp: 0,
        },
        EVT_USER_SEND if r.typed() || size >= 24 => EventPayload::UserSend {
            error: r.i32("error", 0),
            flags: r.u32("flags", 4),
            state: r.u8("state", 8),
            sndbuf_used: r.u32("sndbuf_used", 12),
            sndbuf_avail: r.u32("sndbuf_avail", 16),
            send_len: r.u32("send_len", 20),
        },
        EVT_SENDFILE if r.typed() || size >= 20 => EventPayload::SendFile {
            offset: r.u64("offset", 0),
            length: r.u64("length", 8),
            flags: r.u32("flags", 16),
        },
        EVT_ACK if r.typed() || size >= 32 => EventPayload::Ack {
            acked: r.u32("acked", 0),
            snd_una: r.u32("snd_una", 4),
            inflight: r.u32("inflight", 8),
            sacked: r.u32("sacked", 12),
            nsegs: r.u32("nsegs", 16),
            rcv_time_us: r.u64("rcv_time_us", 24),
            num_blocks: 0,
            blk1_start: 0,
            blk1_end: 0,
            blk2_start: 0,
            blk2_end: 0,
            blk3_start: 0,
            blk3_end: 0,
            blk4_start: 0,
            blk4_end: 0,
        },
        EVT_SACK if r.typed() || size >= 68 => EventPayload::Ack {
            acked: r.u32("acked", 0),
            snd_una: r.u32("snd_una", 4),
            inflight: r.u32("inflight", 8),
            sacked: r.u32("sacked", 12),
            nsegs: r.u32("nsegs", 16),
            rcv_time_us: r.u64("rcv_time_us", 24),
            num_blocks: r.u8("num_blocks", 32),
            blk1_start: r.u32("blk1_start", 36),
            blk1_end: r.u32("blk1_end", 40),
            blk2_start: r.u32("blk2_start", 44),
            blk2_end: r.u32("blk2_end", 48),
            blk3_start: r.u32("blk3_start", 52),
            blk3_end: r.u32("blk3_end", 56),
            blk4_start: r.u32("blk4_start", 60),
            blk4_end: r.u32("blk4_end", 64),
        },
        EVT_ACK | EVT_SACK if size >= 20 => EventPayload::Ack {
            acked: r.u32("acked", 0),
            snd_una: r.u32("snd_una", 4),
            inflight: r.u32("inflight", 8),
            sacked: r.u32("sacked", 12),
            nsegs: r.u32("nsegs", 16),
            rcv_time_us: 0,
            num_blocks: 0,
            blk1_start: 0,
            blk1_end: 0,
            blk2_start: 0,
            blk2_end: 0,
            blk3_start: 0,
            blk3_end: 0,
            blk4_start: 0,
            blk4_end: 0,
        },
        EVT_CWND if r.typed() || size >= 8 => EventPayload::Cwnd {
            cwnd: r.u32("cwnd", 0),
            ssthresh: r.u32("ssthresh", 4),
        },
        EVT_ENTER_RECOVERY if r.typed() || size >= 8 => EventPayload::EnterRecovery {
            cwnd: r.u32("cwnd", 0),
            ssthresh: r.u32("ssthresh", 4),
        },
        EVT_EXIT_RECOVERY if r.typed() || size >= 8 => EventPayload::ExitRecovery {
            cwnd: r.u32("cwnd", 0),
            ssthresh: r.u32("ssthresh", 4),
        },
        EVT_GOODPUT if r.typed() || size >= 32 => EventPayload::Goodput {
            goodput: r.u64("gp_bw", 0),
            raw_bw: r.u64("raw_bw", 8),
            utim: r.u64("utim", 16),
            gp_srtt: r.u32("gp_srtt", 24),
            lowest_rtt: r.u32("lowest_rtt", 28),
            pacing_bw: r.u64("pacing_bw", 32),
        },
        EVT_GOODPUT if size >= 24 => EventPayload::Goodput {
            goodput: r.u64("gp_bw", 0),
            raw_bw: r.u64("raw_bw", 8),
            utim: r.u64("utim", 16),
            gp_srtt: 0,
            lowest_rtt: 0,
            pacing_bw: 0,
        },
        EVT_GP_CHECK if r.typed() || size >= 57 => EventPayload::GpCheck {
            elapsed_us: r.u64("elapsed", 0),
            utim_us: r.u64("utim", 8),
            now_us: r.u64("now", 16),
            start_ts_us: r.u64("start_ts", 24),
            last_ack_us: r.u64("last_ack", 32),
            srtt_us: r.u32("srtt", 40),
            th_ack: r.u32("th_ack", 44),
            bytes: r.u32("bytes", 48),
            min_bytes: r.u32("min_bytes", 52),
            reason: r.u8("reason", 56),
        },
        EVT_RTT if r.typed() || size >= 24 => EventPayload::Rtt {
            rtt: r.u32("rtt", 0),
            srtt: r.u32("srtt", 4),
            path_rtt: r.u32("path_rtt", 8),
            lowest_rtt: r.u32("lowest_rtt", 12),
            gp_filter_min: r.u32("gp_filter_min", 16),
            gp_srtt: r.u32("gp_srtt", 20),
        },
        EVT_RTT_UNDERFLOW if r.typed() || size >= 13 => EventPayload::RttUnderflow {
            us_rtt: r.u32("us_rtt", 0),
            minuend: r.u32("minuend", 4),
            subtrahend: r.u32("subtrahend", 8),
            location: r.u8("where", 12),
        },
        EVT_PACING_CALC if r.typed() || size >= 24 => EventPayload::PacingCalc {
            slot: r.u32("slot", 0),
            bw_est: r.u64("bw_est", 8),
            method: r.u8("method", 16),
            quality: r.u8("quality", 17),
            in_slow_start: r.u8("in_slow_start", 18),
            cap_source: PACING_CAP_NONE,
            inflight: r.u32("inflight", 20),
        },
        EVT_PACING_CALC_V2 if r.typed() || size >= 24 => EventPayload::PacingCalc {
            slot: r.u32("slot", 0),
            bw_est: r.u64("bw_est", 8),
            method: r.u8("method", 16),
            quality: r.u8("quality", 17),
            in_slow_start: r.u8("in_slow_start", 18),
            cap_source: r.u8("cap_source", 19),
            inflight: r.u32("inflight", 20),
        },
        EVT_PACE_STATE if r.typed() || size >= 8 => EventPayload::PaceState {
            rc_pace_min_segs: r.u32("rc_pace_min_segs", 0),
            rc_pace_max_segs: r.u32("rc_pace_max_segs", 4),
        },
        EVT_STARS_PACE if r.typed() || size >= 24 => EventPayload::StarsPace {
            natural_bps: r.u64("natural_bps", 0),
            scaled_bps: r.u64("scaled_bps", 8),
            pressure: r.u32("pressure", 16),
            len: r.u32("len", 20),
        },
        EVT_SESSION_CREATE if r.typed() || size >= 8 => {
            EventPayload::SessionCreate { tp: r.u64("tp", 0) }
        }
        EVT_CONN_OWNER if r.typed() || size >= 4 => EventPayload::ConnOwner {
            pid: r.i32("pid", 0),
        },
        EVT_CONN_PARAMS_V2 if r.typed() || size >= 63 => EventPayload::ConnParamsV2 {
            iss: r.u32("iss", 0),
            irs: r.u32("irs", 4),
            mss: r.u32("mss", 8),
            snd_wscale: r.u8("snd_wscale", 12),
            rcv_wscale: r.u8("rcv_wscale", 13),
            flags: r.u8("flags", 14),
            cc_algo: r.cstr("cc_algo", 15, 16),
            tcp_stack: r.cstr("tcp_stack", 31, 32),
        },
        EVT_TCP_STACK_CHANGE if r.typed() || size >= 32 => EventPayload::TcpStackChange {
            tcp_stack: r.cstr("tcp_stack", 0, 32),
        },
        EVT_CC_ALGO_CHANGE if r.typed() || size >= 16 => EventPayload::CcAlgoChange {
            cc_algo: r.cstr("cc_algo", 0, 16),
        },
        EVT_HTTP_REQ_NEW if r.typed() || size >= 60 => EventPayload::HttpReqNew {
            slot: r.u8("slot", 0),
            track_flags: r.u32("track_flags", 4),
            start: r.u64("start", 8),
            end: r.u64("end", 16),
            start_seq: r.u32("start_seq", 24),
            end_seq: r.u32("end_seq", 28),
            timestamp: r.u64("timestamp", 32),
            localtime: r.u64("localtime", 40),
            first_rcv_us: r.u64("first_rcv_us", 48),
            playout_ms: r.u32("playout_ms", 56),
        },
        EVT_HTTP_REQ_FREED if r.typed() || size >= 88 => EventPayload::HttpReqFreed {
            slot: r.u8("slot", 0),
            track_flags: r.u32("track_flags", 4),
            start_seq: r.u32("start_seq", 8),
            end_seq: r.u32("end_seq", 12),
            start: r.u64("start", 16),
            end: r.u64("end", 24),
            timestamp: r.u64("timestamp", 32),
            localtime: r.u64("localtime", 40),
            first_rcv_us: r.u64("first_rcv_us", 48),
            first_send: r.u64("first_send", 56),
            last_ack_us: r.u64("last_ack_us", 64),
            sent_at_fs: r.u64("sent_at_fs", 72),
            sent_at_ls: r.u64("sent_at_ls", 80),
        },
        EVT_HTTP_REQ_STALE if r.typed() || size >= 56 => EventPayload::HttpReqStale {
            slot: r.u8("slot", 0),
            track_flags: r.u32("track_flags", 4),
            is_oldest: r.u8("is_oldest", 8),
            start_seq: r.u32("start_seq", 12),
            end_seq: r.u32("end_seq", 16),
            start: r.u64("start", 24),
            end: r.u64("end", 32),
            localtime: r.u64("localtime", 40),
            time_delta: r.u64("time_delta", 48),
        },
        EVT_HTTP_REQ_ALLOCFAIL if r.typed() || size >= 32 => EventPayload::HttpReqAllocfail {
            slot: r.u8("slot", 0),
            track_flags: r.u32("track_flags", 4),
            start_seq: r.u32("start_seq", 8),
            end_seq: r.u32("end_seq", 12),
            start: r.u64("start", 16),
            end: r.u64("end", 24),
        },
        EVT_HTTP_REQ_FORCEFREE if r.typed() || size >= 80 => EventPayload::HttpReqForcefree {
            slot: r.u8("slot", 0),
            track_flags: r.u32("track_flags", 4),
            start_seq: r.u32("start_seq", 8),
            end_seq: r.u32("end_seq", 12),
            start: r.u64("start", 16),
            end: r.u64("end", 24),
            timestamp: r.u64("timestamp", 32),
            localtime: r.u64("localtime", 40),
            first_rcv_us: r.u64("first_rcv_us", 48),
            first_send: r.u64("first_send", 56),
            sent_at_fs: r.u64("sent_at_fs", 64),
            sent_at_ls: r.u64("sent_at_ls", 72),
        },
        EVT_DSACK if r.typed() || size >= 24 => EventPayload::Dsack {
            dsack_round_seen: r.u8("dsack_round_seen", 0),
            dsack_round_end: r.u32("dsack_round_end", 4),
            num_dsack: r.u32("num_dsack", 8),
            dsack_persist: r.u32("dsack_persist", 12),
            round_mod: r.u8("mod", 16),
            round: r.u32("round", 20),
        },
        EVT_DSACK => EventPayload::Dsack {
            dsack_round_seen: 0,
            dsack_round_end: 0,
            num_dsack: 0,
            dsack_persist: 0,
            round_mod: 0,
            round: 0,
        },
        EVT_RTO if r.typed() || size >= 24 => EventPayload::Rto {
            timeout_num: r.i32("timeout_num", 0),
            min_rtt: r.u32("min_rtt", 4),
            rtt: r.u32("rtt", 8),
            segment_len: r.u32("segment_len", 12),
            inflight: r.u32("inflight", 16),
            pkts_out: r.u32("pkts_out", 20),
        },
        EVT_RTO => EventPayload::Rto {
            timeout_num: 0,
            min_rtt: 0,
            rtt: 0,
            segment_len: 0,
            inflight: 0,
            pkts_out: 0,
        },
        EVT_WINDOW_COLLAPSE if r.typed() || size >= 13 => EventPayload::WindowCollapse {
            count: r.u32("count", 0),
            direction: r.u8("direction", 4),
            inflight: r.u32("inflight", 8),
            has_collapsed: r.u8("has_collapsed", 12),
        },
        EVT_WINDOW_COLLAPSE => EventPayload::WindowCollapse {
            count: 0,
            direction: 0,
            inflight: 0,
            has_collapsed: 0,
        },
        _ => EventPayload::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elog_parser::schema::test_support::{build_blob, FieldSpec};

    #[test]
    fn schema_packed_in_matches_hardcoded_natural() {
        let aligned_schema = ProviderSchema::parse(&build_blob(
            "tcp",
            "IN",
            vec![
                FieldSpec::scalar("seq", DECODE_TYPE_U32, 0),
                FieldSpec::scalar("ack", DECODE_TYPE_U32, 4),
                FieldSpec::scalar("win", DECODE_TYPE_U32, 8),
                FieldSpec::scalar("len", DECODE_TYPE_U32, 12),
                FieldSpec::scalar("flags", DECODE_TYPE_U16, 16),
                FieldSpec::scalar("state", DECODE_TYPE_U8, 18),
                FieldSpec::scalar("th_off", DECODE_TYPE_U8, 19),
                FieldSpec::scalar("th_sum", DECODE_TYPE_U16, 20),
                FieldSpec::scalar("th_urp", DECODE_TYPE_U16, 22),
                FieldSpec::scalar("rcv_time_us", DECODE_TYPE_U64, 24),
            ],
            EVT_IN,
            "IN",
            "",
        ))
        .unwrap();
        let mut aligned = vec![0u8; 32];
        aligned[0..4].copy_from_slice(&1u32.to_le_bytes());
        aligned[4..8].copy_from_slice(&2u32.to_le_bytes());
        aligned[8..12].copy_from_slice(&3u32.to_le_bytes());
        aligned[12..16].copy_from_slice(&4u32.to_le_bytes());
        aligned[16..18].copy_from_slice(&0x18u16.to_le_bytes());
        aligned[18] = 4;
        aligned[19] = 8;
        aligned[20..22].copy_from_slice(&9u16.to_le_bytes());
        aligned[22..24].copy_from_slice(&10u16.to_le_bytes());
        aligned[24..32].copy_from_slice(&11u64.to_le_bytes());

        let packed_schema = ProviderSchema::parse(&build_blob(
            "tcp",
            "IN",
            vec![
                FieldSpec::scalar("rcv_time_us", DECODE_TYPE_U64, 0),
                FieldSpec::scalar("seq", DECODE_TYPE_U32, 8),
                FieldSpec::scalar("ack", DECODE_TYPE_U32, 12),
                FieldSpec::scalar("win", DECODE_TYPE_U32, 16),
                FieldSpec::scalar("len", DECODE_TYPE_U32, 20),
                FieldSpec::scalar("flags", DECODE_TYPE_U16, 24),
                FieldSpec::scalar("state", DECODE_TYPE_U8, 26),
                FieldSpec::scalar("th_off", DECODE_TYPE_U8, 27),
                FieldSpec::scalar("th_sum", DECODE_TYPE_U16, 28),
                FieldSpec::scalar("th_urp", DECODE_TYPE_U16, 30),
            ],
            EVT_IN,
            "IN",
            "",
        ))
        .unwrap();
        let mut packed = vec![0u8; 32];
        packed[0..8].copy_from_slice(&11u64.to_le_bytes());
        packed[8..12].copy_from_slice(&1u32.to_le_bytes());
        packed[12..16].copy_from_slice(&2u32.to_le_bytes());
        packed[16..20].copy_from_slice(&3u32.to_le_bytes());
        packed[20..24].copy_from_slice(&4u32.to_le_bytes());
        packed[24..26].copy_from_slice(&0x18u16.to_le_bytes());
        packed[26] = 4;
        packed[27] = 8;
        packed[28..30].copy_from_slice(&9u16.to_le_bytes());
        packed[30..32].copy_from_slice(&10u16.to_le_bytes());

        let from_aligned = parse_payload(EVT_IN, &aligned, 0, aligned.len(), Some(&aligned_schema));
        let from_packed = parse_payload(EVT_IN, &packed, 0, packed.len(), Some(&packed_schema));
        let from_v1 = parse_payload(EVT_IN, &aligned, 0, aligned.len(), None);
        for payload in [from_aligned, from_packed, from_v1] {
            match payload {
                EventPayload::In {
                    seq,
                    ack,
                    win,
                    len,
                    flags,
                    state,
                    th_off,
                    th_sum,
                    th_urp,
                    rcv_time_us,
                } => {
                    assert_eq!(
                        (
                            seq,
                            ack,
                            win,
                            len,
                            flags,
                            state,
                            th_off,
                            th_sum,
                            th_urp,
                            rcv_time_us
                        ),
                        (1, 2, 3, 4, 0x18, 4, 8, 9, 10, 11)
                    );
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn parse_goodput_pacing_bw_is_optional() {
        let mut new = vec![0u8; 40];
        new[0..8].copy_from_slice(&1_000_000u64.to_le_bytes()); // gp_bw
        new[32..40].copy_from_slice(&1_250_000u64.to_le_bytes()); // pacing_bw
        match parse_payload(EVT_GOODPUT, &new, 0, new.len(), None) {
            EventPayload::Goodput {
                goodput, pacing_bw, ..
            } => {
                assert_eq!(goodput, 1_000_000);
                assert_eq!(pacing_bw, 1_250_000);
            }
            other => panic!("unexpected {other:?}"),
        }
        let old = &new[..32];
        match parse_payload(EVT_GOODPUT, old, 0, old.len(), None) {
            EventPayload::Goodput {
                goodput, pacing_bw, ..
            } => {
                assert_eq!(goodput, 1_000_000);
                assert_eq!(pacing_bw, 0);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parse_out_retransmit_and_tlp_flags() {
        let mut aligned = vec![0u8; 30];
        aligned[0..4].copy_from_slice(&1000u32.to_le_bytes()); // seq
        aligned[4..8].copy_from_slice(&2000u32.to_le_bytes()); // ack
        aligned[8..12].copy_from_slice(&65535u32.to_le_bytes()); // win
        aligned[12..16].copy_from_slice(&1460u32.to_le_bytes()); // len
        aligned[16..18]
            .copy_from_slice(&(TCP_FLAG_ACK | TCP_FLAG_RETRANSMIT | TCP_FLAG_TLP).to_le_bytes()); // flags
        aligned[20..24].copy_from_slice(&14600u32.to_le_bytes()); // cwnd
        aligned[24] = 5; // th_off
        aligned[26..28].copy_from_slice(&0x1234u16.to_le_bytes()); // th_sum
        aligned[28..30].copy_from_slice(&0u16.to_le_bytes()); // th_urp

        let payload = parse_payload(EVT_OUT, &aligned, 0, aligned.len(), None);
        assert!(payload.is_retransmit());
        assert!(payload.is_tlp());
        if let EventPayload::Out {
            seq,
            ack,
            win,
            len,
            flags,
            cwnd,
            th_off,
            th_sum,
            th_urp,
        } = payload
        {
            assert_eq!(seq, 1000);
            assert_eq!(ack, 2000);
            assert_eq!(win, 65535);
            assert_eq!(len, 1460);
            assert_eq!(flags, TCP_FLAG_ACK | TCP_FLAG_RETRANSMIT | TCP_FLAG_TLP);
            assert_eq!(cwnd, 14600);
            assert_eq!(th_off, 5);
            assert_eq!(th_sum, 0x1234);
            assert_eq!(th_urp, 0);
        } else {
            panic!("expected EventPayload::Out, got {:?}", payload);
        }
    }

    #[test]
    fn packed_conn_set_ip_v4_without_schema() {
        // 12-byte `__packed` record from typical-hpts-only-2026-09-14.
        let data = [
            0xcf, 0x2d, 0x48, 0x01, 0x01, 0xbb, 0xd0, 0x53, 0xc1, 0xa2, 0xc3, 0x70,
        ];
        match parse_payload(EVT_CONN_SET_IP_V4, &data, 0, data.len(), None) {
            EventPayload::ConnSetIpV4 {
                src_addr,
                src_port,
                dst_addr,
                dst_port,
            } => {
                assert_eq!(src_addr, Ipv4Addr::new(207, 45, 72, 1));
                assert_eq!(src_port, 443);
                assert_eq!(dst_addr, Ipv4Addr::new(208, 83, 193, 162));
                assert_eq!(dst_port, 50032);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn padded_conn_set_ip_v4_without_schema() {
        let mut data = vec![0u8; 14];
        data[0..4].copy_from_slice(&[10, 0, 0, 2]);
        data[4..6].copy_from_slice(&12345u16.to_be_bytes());
        data[8..12].copy_from_slice(&[10, 0, 0, 1]);
        data[12..14].copy_from_slice(&443u16.to_be_bytes());
        match parse_payload(EVT_CONN_SET_IP_V4, &data, 0, data.len(), None) {
            EventPayload::ConnSetIpV4 {
                src_addr,
                src_port,
                dst_addr,
                dst_port,
            } => {
                assert_eq!(src_addr, Ipv4Addr::new(10, 0, 0, 2));
                assert_eq!(src_port, 12345);
                assert_eq!(dst_addr, Ipv4Addr::new(10, 0, 0, 1));
                assert_eq!(dst_port, 443);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn packed_conn_set_ip_v6_without_schema() {
        let data = [
            0x2a, 0x00, 0x86, 0xc0, 0x20, 0x28, 0x20, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x01, 0x53, 0xbb, 0x76, 0x26, 0x00, 0x1f, 0x14, 0x06, 0x2a, 0xde, 0x0e, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xc6, 0x8b, 0x1b, 0x68,
        ];
        match parse_payload(EVT_CONN_SET_IP_V6, &data, 0, data.len(), None) {
            EventPayload::ConnSetIpV6 {
                src_addr,
                src_port,
                dst_addr,
                dst_port,
            } => {
                assert_eq!(
                    src_addr,
                    "2a00:86c0:2028:2028::153".parse::<Ipv6Addr>().unwrap()
                );
                assert_eq!(src_port, 0xbb76);
                assert_eq!(
                    dst_addr,
                    "2600:1f14:62a:de0e::c68b".parse::<Ipv6Addr>().unwrap()
                );
                assert_eq!(dst_port, 0x1b68);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
