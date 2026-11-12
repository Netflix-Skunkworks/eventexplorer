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
// hpts provider: timing-wheel scheduler trace (ids 0-14).
//
// A separate schema from EVT_HPTS_STARS_AGG_SAMPLE (id 16, decoded in
// core.rs's is_hpts block). Offsets below are C-natural-alignment layouts
// transcribed from `hpts_eventlog_schema.src`'s STRUCT definitions and
// cross-checked field-for-field against a real v2 capture's embedded
// schema; see `schema_validate::HPTS_EXPECTED` for the ExpectedField
// tables that verify this against the live schema once per file.
// ---------------------------------------------------------------------------

/// Decode one `hpts` timing-wheel event (ids 0-14). Returns `None` for
/// any other event id (including `EVT_HPTS_STARS_AGG_SAMPLE`, decoded
/// separately in `core.rs`) or when `size` is too small for the id's
/// struct.
pub fn parse_hpts_wheel_payload(
    event_id: u32,
    data: &[u8],
    off: usize,
    size: usize,
    schema: Option<&ProviderSchema>,
) -> Option<HptsWheelPayload> {
    let r = PayloadReader::new(schema, event_id, data, off, size);
    match event_id {
        EVT_HPTS_INIT if r.typed() || size >= 20 => Some(HptsWheelPayload::Init {
            num_hptss: r.u32("num_hptss", 0),
            bind_type: r.u8("bind_type", 4),
            num_slots: r.u32("num_slots", 8),
            usecs_per_slot: r.u32("usecs_per_slot", 12),
            total_slot_time: r.u32("total_slot_time", 16),
        }),
        EVT_HPTS_INIT_HPTS_ENTRY => Some(HptsWheelPayload::InitHptsEntry),
        EVT_HPTS_INIT_TIMING_PARAMS if r.typed() || size >= 16 => {
            Some(HptsWheelPayload::InitTimingParams {
                usecs_per_slot: r.u32("usecs_per_slot", 0),
                total_slot_time: r.u32("total_slot_time", 4),
                num_slots: r.u32("num_slots", 8),
                wheel_mask: r.u32("wheel_mask", 12),
            })
        }
        EVT_HPTS_STATE_ACTIVE if r.typed() || size >= 12 => Some(HptsWheelPayload::StateActive {
            from_callout: r.u8("from_callout", 0),
            cur_slot: r.u32("cur_slot", 4),
            tp_cur_count: r.u32("tp_cur_count", 8),
        }),
        EVT_HPTS_STATE_INACTIVE if r.typed() || size >= 4 => {
            Some(HptsWheelPayload::StateInactive {
                slots_ran: r.u32("slots_ran", 0),
            })
        }
        EVT_HPTS_SLEEP if r.typed() || size >= 4 => Some(HptsWheelPayload::Sleep {
            sleep_time: r.u32("sleep_time", 0),
        }),
        EVT_HPTS_CONN_INSERT if r.typed() || size >= 16 => Some(HptsWheelPayload::ConnInsert {
            gencnt: r.u32("gencnt", 0),
            slot: r.u32("slot", 4),
            usecs: r.u32("usecs", 8),
            tp_cur_count: r.u32("tp_cur_count", 12),
        }),
        EVT_HPTS_CONN_REMOVE if r.typed() || size >= 12 => Some(HptsWheelPayload::ConnRemove {
            gencnt: r.u32("gencnt", 0),
            slot: r.u32("slot", 4),
            tp_cur_count: r.u32("tp_cur_count", 8),
        }),
        EVT_HPTS_CONN_MOVE if r.typed() || size >= 8 => Some(HptsWheelPayload::ConnMove {
            gencnt: r.u32("gencnt", 0),
            old_slot: r.u32("old_slot", 4),
        }),
        EVT_HPTS_WHEEL_WRAP if r.typed() || size >= 16 => Some(HptsWheelPayload::WheelWrap {
            wrap_loop_cnt: r.u32("wrap_loop_cnt", 0),
            slots_to_run: r.u32("slots_to_run", 4),
            prev_slot: r.u32("prev_slot", 8),
            cur_slot: r.u32("cur_slot", 12),
        }),
        EVT_HPTS_PROCESSING if r.typed() || size >= 20 => Some(HptsWheelPayload::Processing {
            from_callout: r.u8("from_callout", 0),
            prev_slot: r.u32("prev_slot", 4),
            cur_slot: r.u32("cur_slot", 8),
            slots_to_run: r.u32("slots_to_run", 12),
            tp_cur_count: r.u32("tp_cur_count", 16),
        }),
        EVT_HPTS_PERF_PROCESSING if r.typed() || size >= 20 => {
            Some(HptsWheelPayload::PerfProcessing {
                from_callout: r.u8("from_callout", 0),
                slots_processed: r.u32("slots_processed", 4),
                tps_processed: r.u32("tps_processed", 8),
                runtime_usec: r.u32("runtime_usec", 12),
                loop_cnt: r.u32("loop_cnt", 16),
            })
        }
        EVT_HPTS_PERF_LATENESS if r.typed() || size >= 20 => Some(HptsWheelPayload::PerfLateness {
            gencnt: r.u32("gencnt", 0),
            lateness_slots: r.u32("lateness_slots", 4),
            lateness_usec: r.u32("lateness_usec", 8),
            requested_time: r.u32("requested_time", 12),
            actual_time: r.u32("actual_time", 16),
        }),
        EVT_HPTS_LATENESS_SAMPLE | EVT_HPTS_TP_BATCH_SAMPLE if r.typed() || size >= 1 => {
            let buckets = r.hist_u64s(16, 0, 8);
            Some(if event_id == EVT_HPTS_LATENESS_SAMPLE {
                HptsWheelPayload::LatenessSample { buckets }
            } else {
                HptsWheelPayload::TpBatchSample { buckets }
            })
        }
        EVT_HPTS_SLOT_GATE_SAMPLE if r.typed() || size >= 28 => {
            Some(HptsWheelPayload::SlotGateSample {
                fires: r.u32("fires", 0),
                diverts: r.u32("diverts", 4),
                bytes: r.u64("bytes", 8),
                rollover_max: r.u32("rollover_max", 16),
                walks: r.u32("walks", 20),
                closed: r.u32("closed", 24),
                default_bytes: r.u64("default_bytes", 28),
                rack_bytes: r.u64("rack_bytes", 36),
                bbr_bytes: r.u64("bbr_bytes", 44),
                closed_tcp_bytes: r.u64("closed_tcp_bytes", 52),
                caller0: r.u64("caller0", 60),
                caller0_bytes: r.u64("caller0_bytes", 68),
                caller1: r.u64("caller1", 76),
                caller1_bytes: r.u64("caller1_bytes", 84),
                caller_other_bytes: r.u64("caller_other_bytes", 92),
            })
        }
        EVT_HPTS_OPPOR_SAMPLE if r.typed() || size >= 17 => {
            let buckets = r.hist_u64s(16, 16, 24);
            Some(HptsWheelPayload::OpporSample {
                oppor_runs: r.u64("oppor_runs", 0),
                oppor_noop: r.u64("oppor_noop", 8),
                buckets,
            })
        }
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Anything this module can decode must also be listed in
    /// `hpts_has_hardcoded_decoder`. `core.rs` decodes generically and
    /// `continue`s before it reaches the `is_hpts` block, so an id that is
    /// missing there never reaches this module at all -- it lands in
    /// `generic_events` and every consumer silently sees zero of them. That
    /// is exactly how `OPPOR_SAMPLE` shipped: decoder written and unit
    /// tested, but absent from the registry, so a real capture carrying
    /// 28755 of them decoded none.
    #[test]
    fn every_decodable_id_is_registered_as_hardcoded() {
        use crate::elog_parser::schema_validate::hpts_has_hardcoded_decoder;

        // Generous zero-filled payload so no arm bails on its size guard.
        let data = vec![0u8; 512];
        for event_id in 0u32..=18 {
            if parse_hpts_wheel_payload(event_id, &data, 0, data.len(), None).is_some() {
                assert!(
                    hpts_has_hardcoded_decoder(event_id),
                    "event id {event_id} decodes here but is missing from \
                     hpts_has_hardcoded_decoder, so core.rs will route it \
                     to generic_events instead"
                );
            }
        }
    }

    fn hist_bytes(nbuckets: u8, buckets: &[u64]) -> Vec<u8> {
        let mut buf = vec![0u8; 8 + buckets.len() * 8];
        buf[0] = nbuckets;
        for (i, v) in buckets.iter().enumerate() {
            buf[8 + i * 8..8 + i * 8 + 8].copy_from_slice(&v.to_le_bytes());
        }
        buf
    }

    #[test]
    fn lateness_sample_decodes_cumulative_buckets() {
        let data = hist_bytes(3, &[10, 20, 30]);
        let payload =
            parse_hpts_wheel_payload(EVT_HPTS_LATENESS_SAMPLE, &data, 0, data.len(), None)
                .expect("should decode");
        match payload {
            HptsWheelPayload::LatenessSample { buckets } => {
                assert_eq!(buckets, vec![10, 20, 30]);
            }
            other => panic!("expected LatenessSample, got {other:?}"),
        }
    }

    #[test]
    fn tp_batch_sample_decodes_cumulative_buckets() {
        let data = hist_bytes(2, &[100, 200]);
        let payload =
            parse_hpts_wheel_payload(EVT_HPTS_TP_BATCH_SAMPLE, &data, 0, data.len(), None)
                .expect("should decode");
        match payload {
            HptsWheelPayload::TpBatchSample { buckets } => {
                assert_eq!(buckets, vec![100, 200]);
            }
            other => panic!("expected TpBatchSample, got {other:?}"),
        }
    }

    /// `struct __aligned(8) { u64; u64; u8; }` is 24 bytes, so the buckets
    /// array starts at 24, not at 17.
    fn oppor_bytes(runs: u64, noop: u64, nbuckets: u8, buckets: &[u64]) -> Vec<u8> {
        let mut buf = vec![0u8; 24 + buckets.len() * 8];
        buf[0..8].copy_from_slice(&runs.to_le_bytes());
        buf[8..16].copy_from_slice(&noop.to_le_bytes());
        buf[16] = nbuckets;
        for (i, v) in buckets.iter().enumerate() {
            buf[24 + i * 8..24 + i * 8 + 8].copy_from_slice(&v.to_le_bytes());
        }
        buf
    }

    #[test]
    fn oppor_sample_decodes_counters_and_buckets() {
        let data = oppor_bytes(4_242, 91_000, 3, &[7, 8, 9]);
        let payload = parse_hpts_wheel_payload(EVT_HPTS_OPPOR_SAMPLE, &data, 0, data.len(), None)
            .expect("should decode");
        match payload {
            HptsWheelPayload::OpporSample {
                oppor_runs,
                oppor_noop,
                buckets,
            } => {
                assert_eq!(oppor_runs, 4_242);
                assert_eq!(oppor_noop, 91_000);
                assert_eq!(buckets, vec![7, 8, 9]);
            }
            other => panic!("expected OpporSample, got {other:?}"),
        }
    }

    #[test]
    fn oppor_sample_truncated_buckets_yield_counters_only() {
        let mut data = oppor_bytes(1, 2, 3, &[7, 8, 9]);
        data.truncate(30);
        let payload = parse_hpts_wheel_payload(EVT_HPTS_OPPOR_SAMPLE, &data, 0, data.len(), None)
            .expect("should still decode the fixed head");
        match payload {
            HptsWheelPayload::OpporSample {
                oppor_runs,
                oppor_noop,
                buckets,
            } => {
                assert_eq!((oppor_runs, oppor_noop), (1, 2));
                assert!(buckets.is_empty());
            }
            other => panic!("expected OpporSample, got {other:?}"),
        }
    }

    #[test]
    fn hist_sample_nbuckets_is_clamped_to_16() {
        // nbuckets=20 in the header, but only 16 buckets actually follow --
        // matches the schema's `uint64_t[nbuckets:16]` compile-time bound.
        let mut data = hist_bytes(16, &[1; 16]);
        data[0] = 20;
        let payload =
            parse_hpts_wheel_payload(EVT_HPTS_LATENESS_SAMPLE, &data, 0, data.len(), None)
                .expect("should decode");
        match payload {
            HptsWheelPayload::LatenessSample { buckets } => {
                assert_eq!(buckets.len(), 16);
            }
            other => panic!("expected LatenessSample, got {other:?}"),
        }
    }

    #[test]
    fn hist_sample_with_truncated_payload_yields_empty_buckets() {
        // Header claims 3 buckets but the payload is cut short -- the
        // out-of-bounds guard should yield an empty (not garbage/panicking)
        // bucket vec rather than reading past `size`.
        let data = hist_bytes(3, &[10, 20, 30]);
        let truncated = &data[..10];
        let payload = parse_hpts_wheel_payload(
            EVT_HPTS_LATENESS_SAMPLE,
            truncated,
            0,
            truncated.len(),
            None,
        )
        .expect("should still decode (nbuckets header is present)");
        match payload {
            HptsWheelPayload::LatenessSample { buckets } => {
                assert!(buckets.is_empty());
            }
            other => panic!("expected LatenessSample, got {other:?}"),
        }
    }

    #[test]
    fn slot_gate_sample_decodes_interval_counters() {
        let mut data = vec![0u8; 28];
        data[0..4].copy_from_slice(&10u32.to_le_bytes());
        data[4..8].copy_from_slice(&2u32.to_le_bytes());
        data[8..16].copy_from_slice(&1_000u64.to_le_bytes());
        data[16..20].copy_from_slice(&7u32.to_le_bytes());
        data[20..24].copy_from_slice(&4u32.to_le_bytes());
        data[24..28].copy_from_slice(&1u32.to_le_bytes());
        let payload =
            parse_hpts_wheel_payload(EVT_HPTS_SLOT_GATE_SAMPLE, &data, 0, data.len(), None)
                .expect("28-byte slot-gate sample");
        match payload {
            HptsWheelPayload::SlotGateSample {
                fires,
                diverts,
                bytes,
                rollover_max,
                walks,
                closed,
                default_bytes,
                rack_bytes,
                bbr_bytes,
                closed_tcp_bytes,
                caller0,
                ..
            } => {
                assert_eq!(fires, 10);
                assert_eq!(diverts, 2);
                assert_eq!(bytes, 1_000);
                assert_eq!(rollover_max, 7);
                assert_eq!(walks, 4);
                assert_eq!(closed, 1);
                assert_eq!(default_bytes, 0);
                assert_eq!(closed_tcp_bytes, 0);
                assert_eq!(caller0, 0);
                assert_eq!(rack_bytes, 0);
                assert_eq!(bbr_bytes, 0);
            }
            other => panic!("expected SlotGateSample, got {other:?}"),
        }
    }

    #[test]
    fn unrelated_event_id_is_not_decoded_as_hist_sample() {
        let data = hist_bytes(3, &[10, 20, 30]);
        assert!(
            parse_hpts_wheel_payload(EVT_HPTS_NO_RUN_SLOT_SKIP, &data, 0, data.len(), None)
                .is_none()
        );
    }
}
