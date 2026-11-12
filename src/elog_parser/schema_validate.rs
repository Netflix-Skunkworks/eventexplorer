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
// Schema verification: does a resolved v2 embedded schema agree with what
// this codebase's hardcoded decoders assume about a payload's byte
// layout? Run once per provider at load time (`validate_hardcoded_decoders`,
// called from `core.rs` right after schema resolution) rather than per
// event.
//
// `ExpectedField.name` MUST be the schema's own field name (from the
// provider's `*_eventlog_schema.src`), not whatever Rust calls the
// decoded struct member — the lookup is by schema name. Where our
// decoder's assumptions don't correspond to any real schema field (see
// the DSACK / IF_CAPABILITIES_CHANGE tables below),
// the mismatched name is deliberately left as our decoder's own guess:
// the lookup then correctly fails and marks the event divergent, which
// is exactly the outcome we want for a decoder that was never verified
// against a real kernel schema.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct ExpectedField {
    /// The schema's own field name (see module doc comment).
    pub name: &'static str,
    pub offset: u16,
    /// Expected byte width, or `None` for a trailing variable-length
    /// array field (`hwaddr` in `IF_ATTACH`, `addr` in `IF_ADDR`) —
    /// its runtime size is data-dependent (driven by a preceding
    /// length field), so only the starting offset is meaningful to
    /// verify.
    pub size: Option<u16>,
    /// The decoder reads this field by name and tolerates its absence
    /// (older or newer schemas), so a missing field is not drift.
    pub required: bool,
}

impl ExpectedField {
    const fn new(name: &'static str, offset: u16, size: u16) -> Self {
        ExpectedField {
            name,
            offset,
            size: Some(size),
            required: true,
        }
    }
    const fn optional(name: &'static str, offset: u16, size: u16) -> Self {
        ExpectedField {
            name,
            offset,
            size: Some(size),
            required: false,
        }
    }
    const fn varlen(name: &'static str, offset: u16) -> Self {
        ExpectedField {
            name,
            offset,
            size: None,
            required: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExpectedEvent {
    pub event_id: u32,
    /// Every field this codebase's hardcoded decoder for `event_id`
    /// actually reads. Fields the schema has grown *beyond* this list
    /// are safe additive drift (see [`ValidationResult::new_fields`]),
    /// not a reason to distrust the decoder.
    pub fields: &'static [ExpectedField],
}

/// Result of validating one provider's hardcoded decoders against its
/// resolved schema.
#[derive(Debug, Clone, Default)]
pub struct ValidationResult {
    /// Event ids where the hardcoded decoder's assumed offset/size for
    /// some field it reads disagrees with the schema (missing field,
    /// or a field at a different offset/size) — unsafe drift. These
    /// ids must be decoded generically from the schema instead.
    pub divergent: FxHashSet<u32>,
    /// Human-readable notices (one per event, in `ExpectedEvent` order)
    /// for events where the decoder is still safe to trust but the
    /// schema has grown fields beyond what it decodes.
    pub new_fields: Vec<String>,
    /// Human-readable notices (one per divergent event) explaining
    /// *why* — surfaced alongside `divergent` so a developer doesn't
    /// have to go re-derive it.
    pub divergent_reasons: Vec<String>,
}

/// Validate `expected` (one provider kind's hardcoded-decoder field
/// assumptions) against `schema` (that provider's resolved v2 schema).
/// Called once per provider at load time — see the "Changes to existing
/// code" section of the v2 schema-verified-decoding plan for where this
/// is wired into `core.rs`.
pub fn validate_hardcoded_decoders(
    schema: &ProviderSchema,
    expected: &[ExpectedEvent],
) -> ValidationResult {
    let mut result = ValidationResult::default();
    let mut checked = FxHashSet::default();
    for exp in expected {
        if !checked.insert(exp.event_id) {
            continue;
        }
        // Not in schema: absence isn't evidence of drift, only presence
        // + mismatch is. Leave the decoder trusted, don't checking it.
        let Some(ev) = schema.find_event(exp.event_id) else {
            continue;
        };
        let Some(st) = schema.struct_for_event(ev) else {
            if !exp.fields.is_empty() {
                result.divergent.insert(exp.event_id);
                result.divergent_reasons.push(format!(
                    "{}: decoder expects a payload but schema defines none",
                    ev.name
                ));
            }
            continue;
        };

        let alternatives = expected
            .iter()
            .filter(|candidate| candidate.event_id == exp.event_id);
        let mut mismatch = None;
        let mut matched = None;
        for candidate in alternatives {
            match expected_layout_mismatch(st, candidate.fields) {
                Some(reason) => {
                    mismatch.get_or_insert(reason);
                }
                None => {
                    matched = Some(candidate);
                    break;
                }
            };
        }

        let Some(matched) = matched else {
            result.divergent.insert(exp.event_id);
            result
                .divergent_reasons
                .push(format!("{}: {}", ev.name, mismatch.unwrap_or_default()));
            continue;
        };

        let known: FxHashSet<&str> = matched.fields.iter().map(|f| f.name).collect();
        let extra: Vec<&str> = st
            .fields
            .iter()
            .filter(|sf| !known.contains(sf.name.as_str()))
            .map(|sf| sf.name.as_str())
            .collect();
        if !extra.is_empty() {
            result.new_fields.push(format!(
                "{}: {} undecoded field{} ({})",
                ev.name,
                extra.len(),
                if extra.len() == 1 { "" } else { "s" },
                extra.join(", ")
            ));
        }
    }
    result
}

fn expected_layout_mismatch(st: &DecodeStruct, fields: &[ExpectedField]) -> Option<String> {
    // Offset differences are packing/alignment/reorder; typed ingest
    // reads by schema name. Only a missing field or a size change is
    // unsafe for those builders.
    for f in fields {
        match st.fields.iter().find(|sf| sf.name == f.name) {
            None if !f.required => {}
            None => return Some(format!("field '{}' not present in schema", f.name)),
            Some(sf) => {
                if let Some(expected_size) = f.size {
                    let actual_size = field_byte_size(sf);
                    if actual_size != expected_size {
                        return Some(format!(
                            "field '{}' is {} bytes in schema, decoder assumes {}",
                            f.name, actual_size, expected_size
                        ));
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Per-provider "does a hardcoded decoder exist at all for this event id"
// predicates. Used by the routing guard in `core.rs`'s event loop to
// decide the "no hardcoded arm handles it, but the schema defines it"
// case — a schema-only event should be decoded generically even when
// it was never a candidate for divergence checking (e.g. it has never
// had a hardcoded decoder to validate). Kept in sync with the match
// arms in `payload.rs` / `core.rs` by hand; these lists change only
// when a hardcoded decoder is added or removed, not when its expected
// fields change.
// ---------------------------------------------------------------------------

/// Whether `parse_payload` (or one of `core.rs`'s tcp-sample /
/// session-marker special cases) has any arm for `event_id`, i.e.
/// whether this event id currently produces something other than
/// `EventPayload::Other`.
pub fn tcp_has_hardcoded_decoder(event_id: u32) -> bool {
    matches!(
        event_id,
        EVT_CONN_SET_IP_V4
            | EVT_CONN_SET_IP_V6
            | EVT_LOG_ID
            | EVT_IN
            | EVT_OUT
            | EVT_ACK
            | EVT_SACK
            | EVT_USER_SEND
            | EVT_SENDFILE
            | EVT_CWND
            | EVT_ENTER_RECOVERY
            | EVT_EXIT_RECOVERY
            | EVT_GOODPUT
            | EVT_GP_CHECK
            | EVT_RTT
            | EVT_RTT_UNDERFLOW
            | EVT_PACING_CALC
            | EVT_PACING_CALC_V2
            | EVT_PACE_STATE
            | EVT_STARS_PACE
            | EVT_SESSION_CREATE
            | EVT_CONN_OWNER
            | EVT_CONN_PARAMS_V2
            | EVT_TCP_STACK_CHANGE
            | EVT_CC_ALGO_CHANGE
            | EVT_HTTP_REQ_NEW
            | EVT_HTTP_REQ_FREED
            | EVT_HTTP_REQ_STALE
            | EVT_HTTP_REQ_ALLOCFAIL
            | EVT_HTTP_REQ_FORCEFREE
            | EVT_DSACK
            | EVT_RTO
            | EVT_WINDOW_COLLAPSE
            // Periodic per-stack samples, handled by core.rs's
            // is_tcp-sample block rather than parse_payload.
            | EVT_TCP_SAMPLE
            | EVT_TCP_GP_SAMPLE
            | EVT_TCP_STACK_INFO
            | EVT_TCP_RPS_SAMPLE
            | EVT_TCP_PACING_RATE_SAMPLE
            | EVT_TCP_REALIZED_RATE_SAMPLE
            | EVT_TCP_SEND_SAMPLE
            | EVT_TCP_LRO_FLUSH_SAMPLE
            | EVT_TCP_SRTT_SAMPLE
    )
}

/// TCP: field-level expectations, one entry per event id with at least
/// one hardcoded field read (`tcp_has_hardcoded_decoder` is a superset —
/// `EVT_GP_CHECK`/`EVT_RTT_UNDERFLOW`/`EVT_PACING_CALC_V2` aren't defined
/// in the reference `tcp_eventlog_schema.src` at all, so they're never
/// reachable via `ProviderSchema::find_event` regardless).
pub static TCP_EXPECTED: &[ExpectedEvent] = &[
    ExpectedEvent {
        event_id: EVT_CONN_SET_IP_V4,
        fields: &[
            ExpectedField::new("src_addr", 0, 4),
            ExpectedField::new("src_port", 4, 2),
            ExpectedField::new("dst_addr", 8, 4),
            ExpectedField::new("dst_port", 12, 2),
        ],
    },
    ExpectedEvent {
        event_id: EVT_CONN_SET_IP_V6,
        fields: &[
            ExpectedField::new("src_addr", 0, 16),
            ExpectedField::new("src_port", 16, 2),
            ExpectedField::new("dst_addr", 18, 16),
            ExpectedField::new("dst_port", 34, 2),
        ],
    },
    ExpectedEvent {
        event_id: EVT_LOG_ID,
        fields: &[ExpectedField::new("log_id", 0, 64)],
    },
    ExpectedEvent {
        // state/th_off/th_sum/th_urp/rcv_time_us verified against
        // `/tmp/hpts-delay.elog`'s embedded schema and now decoded by
        // `parse_payload`'s 32-byte tier.
        event_id: EVT_IN,
        fields: &[
            ExpectedField::new("seq", 0, 4),
            ExpectedField::new("ack", 4, 4),
            ExpectedField::new("win", 8, 4),
            ExpectedField::new("len", 12, 4),
            ExpectedField::new("flags", 16, 2),
            ExpectedField::new("state", 18, 1),
            ExpectedField::new("th_off", 19, 1),
            ExpectedField::optional("th_sum", 20, 2),
            ExpectedField::new("th_urp", 22, 2),
            ExpectedField::new("rcv_time_us", 24, 8),
        ],
    },
    ExpectedEvent {
        // th_off/th_sum/th_urp verified against
        // `/tmp/hpts-delay.elog`'s embedded schema and now decoded by
        // `parse_payload`'s 30-byte tier.
        event_id: EVT_OUT,
        fields: &[
            ExpectedField::new("seq", 0, 4),
            ExpectedField::new("ack", 4, 4),
            ExpectedField::new("win", 8, 4),
            ExpectedField::new("len", 12, 4),
            ExpectedField::new("flags", 16, 2),
            ExpectedField::new("cwnd", 20, 4),
            ExpectedField::new("th_off", 24, 1),
            ExpectedField::optional("th_sum", 26, 2),
            ExpectedField::new("th_urp", 28, 2),
        ],
    },
    ExpectedEvent {
        // rcv_time_us verified against `/tmp/hpts-delay.elog`'s embedded
        // schema and now decoded by `parse_payload`'s 32-byte tier.
        event_id: EVT_ACK,
        fields: &[
            ExpectedField::new("acked", 0, 4),
            ExpectedField::new("snd_una", 4, 4),
            ExpectedField::new("inflight", 8, 4),
            ExpectedField::new("sacked", 12, 4),
            ExpectedField::new("nsegs", 16, 4),
            ExpectedField::new("rcv_time_us", 24, 8),
        ],
    },
    ExpectedEvent {
        // SACK (103) shares ACK's Rust variant (`EventPayload::Ack`) but
        // decodes against STRUCT SACK, which starts with the same 5
        // fields at the same offsets as STRUCT ACK, then adds
        // rcv_time_us + up to 4 block ranges — all verified against
        // `/tmp/hpts-delay.elog`'s embedded schema and decoded by
        // `parse_payload`'s 68-byte tier.
        event_id: EVT_SACK,
        fields: &[
            ExpectedField::new("acked", 0, 4),
            ExpectedField::new("snd_una", 4, 4),
            ExpectedField::new("inflight", 8, 4),
            ExpectedField::new("sacked", 12, 4),
            ExpectedField::new("nsegs", 16, 4),
            ExpectedField::new("rcv_time_us", 24, 8),
            ExpectedField::new("num_blocks", 32, 1),
            ExpectedField::new("blk1_start", 36, 4),
            ExpectedField::new("blk1_end", 40, 4),
            ExpectedField::new("blk2_start", 44, 4),
            ExpectedField::new("blk2_end", 48, 4),
            ExpectedField::new("blk3_start", 52, 4),
            ExpectedField::new("blk3_end", 56, 4),
            ExpectedField::new("blk4_start", 60, 4),
            ExpectedField::new("blk4_end", 64, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_USER_SEND,
        fields: &[
            ExpectedField::new("error", 0, 4),
            ExpectedField::new("flags", 4, 4),
            ExpectedField::new("state", 8, 1),
            ExpectedField::new("sndbuf_used", 12, 4),
            ExpectedField::new("sndbuf_avail", 16, 4),
            ExpectedField::new("send_len", 20, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_SENDFILE,
        fields: &[
            ExpectedField::new("offset", 0, 8),
            ExpectedField::new("length", 8, 8),
            ExpectedField::new("flags", 16, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_CWND,
        fields: &[
            ExpectedField::new("cwnd", 0, 4),
            ExpectedField::new("ssthresh", 4, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ENTER_RECOVERY,
        fields: &[
            ExpectedField::new("cwnd", 0, 4),
            ExpectedField::new("ssthresh", 4, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_EXIT_RECOVERY,
        fields: &[
            ExpectedField::new("cwnd", 0, 4),
            ExpectedField::new("ssthresh", 4, 4),
        ],
    },
    ExpectedEvent {
        // Decoder's Rust field is named `goodput`; the schema's real
        // field name is `gp_bw` (see module doc comment on naming).
        // gp_srtt/lowest_rtt verified against `/tmp/hpts-delay.elog`'s
        // embedded schema and now decoded by `parse_payload`'s 32-byte
        // tier.
        event_id: EVT_GOODPUT,
        fields: &[
            ExpectedField::new("gp_bw", 0, 8),
            ExpectedField::new("raw_bw", 8, 8),
            ExpectedField::new("utim", 16, 8),
            ExpectedField::new("gp_srtt", 24, 4),
            ExpectedField::new("lowest_rtt", 28, 4),
            ExpectedField::optional("pacing_bw", 32, 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_RTT,
        fields: &[
            ExpectedField::new("rtt", 0, 4),
            ExpectedField::new("srtt", 4, 4),
            ExpectedField::new("path_rtt", 8, 4),
            ExpectedField::new("lowest_rtt", 12, 4),
            ExpectedField::new("gp_filter_min", 16, 4),
            ExpectedField::new("gp_srtt", 20, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_PACING_CALC,
        fields: &[
            ExpectedField::new("slot", 0, 4),
            ExpectedField::new("bw_est", 8, 8),
            ExpectedField::new("method", 16, 1),
            ExpectedField::new("quality", 17, 1),
            ExpectedField::new("in_slow_start", 18, 1),
            ExpectedField::new("inflight", 20, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_PACE_STATE,
        fields: &[
            ExpectedField::new("rc_pace_min_segs", 0, 4),
            ExpectedField::new("rc_pace_max_segs", 4, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_STARS_PACE,
        fields: &[
            ExpectedField::new("natural_bps", 0, 8),
            ExpectedField::new("scaled_bps", 8, 8),
            ExpectedField::new("pressure", 16, 4),
            ExpectedField::new("len", 20, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_SESSION_CREATE,
        fields: &[ExpectedField::new("tp", 0, 8)],
    },
    ExpectedEvent {
        event_id: EVT_CONN_OWNER,
        fields: &[ExpectedField::new("pid", 0, 4)],
    },
    ExpectedEvent {
        event_id: EVT_CONN_PARAMS_V2,
        fields: &[
            ExpectedField::new("iss", 0, 4),
            ExpectedField::new("irs", 4, 4),
            ExpectedField::new("mss", 8, 4),
            ExpectedField::new("snd_wscale", 12, 1),
            ExpectedField::new("rcv_wscale", 13, 1),
            ExpectedField::new("flags", 14, 1),
            ExpectedField::new("cc_algo", 15, 16),
            ExpectedField::new("tcp_stack", 31, 32),
        ],
    },
    ExpectedEvent {
        event_id: EVT_TCP_STACK_CHANGE,
        fields: &[ExpectedField::new("tcp_stack", 0, 32)],
    },
    ExpectedEvent {
        event_id: EVT_CC_ALGO_CHANGE,
        fields: &[ExpectedField::new("cc_algo", 0, 16)],
    },
    ExpectedEvent {
        event_id: EVT_HTTP_REQ_NEW,
        fields: &[
            ExpectedField::new("slot", 0, 1),
            ExpectedField::new("track_flags", 4, 4),
            ExpectedField::new("start", 8, 8),
            ExpectedField::new("end", 16, 8),
            ExpectedField::new("start_seq", 24, 4),
            ExpectedField::new("end_seq", 28, 4),
            ExpectedField::new("timestamp", 32, 8),
            ExpectedField::new("localtime", 40, 8),
            ExpectedField::new("first_rcv_us", 48, 8),
            ExpectedField::new("playout_ms", 56, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HTTP_REQ_FREED,
        fields: &[
            ExpectedField::new("slot", 0, 1),
            ExpectedField::new("track_flags", 4, 4),
            ExpectedField::new("start_seq", 8, 4),
            ExpectedField::new("end_seq", 12, 4),
            ExpectedField::new("start", 16, 8),
            ExpectedField::new("end", 24, 8),
            ExpectedField::new("timestamp", 32, 8),
            ExpectedField::new("localtime", 40, 8),
            ExpectedField::new("first_rcv_us", 48, 8),
            ExpectedField::new("first_send", 56, 8),
            ExpectedField::new("last_ack_us", 64, 8),
            ExpectedField::new("sent_at_fs", 72, 8),
            ExpectedField::new("sent_at_ls", 80, 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HTTP_REQ_STALE,
        fields: &[
            ExpectedField::new("slot", 0, 1),
            ExpectedField::new("track_flags", 4, 4),
            ExpectedField::new("is_oldest", 8, 1),
            ExpectedField::new("start_seq", 12, 4),
            ExpectedField::new("end_seq", 16, 4),
            ExpectedField::new("start", 24, 8),
            ExpectedField::new("end", 32, 8),
            ExpectedField::new("localtime", 40, 8),
            ExpectedField::new("time_delta", 48, 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HTTP_REQ_ALLOCFAIL,
        fields: &[
            ExpectedField::new("slot", 0, 1),
            ExpectedField::new("track_flags", 4, 4),
            ExpectedField::new("start_seq", 8, 4),
            ExpectedField::new("end_seq", 12, 4),
            ExpectedField::new("start", 16, 8),
            ExpectedField::new("end", 24, 8),
        ],
    },
    ExpectedEvent {
        // Verified against `/tmp/hpts-delay.elog`'s embedded schema. An
        // earlier version of this table listed a guessed
        // `start_seq`/`end_seq` layout that matched no real schema
        // field (see `git log` on this file) and always validated
        // divergent by design; `parse_payload` now decodes the real
        // (round-based) fields below.
        event_id: EVT_DSACK,
        fields: &[
            ExpectedField::new("dsack_round_seen", 0, 1),
            ExpectedField::new("dsack_round_end", 4, 4),
            ExpectedField::new("num_dsack", 8, 4),
            ExpectedField::new("dsack_persist", 12, 4),
            ExpectedField::new("mod", 16, 1),
            ExpectedField::new("round", 20, 4),
        ],
    },
    // RTO/WINDOW_COLLAPSE fields verified against
    // `/tmp/hpts-delay.elog`'s embedded schema and now decoded by
    // `parse_payload` (previously marker-only, no fields decoded).
    ExpectedEvent {
        event_id: EVT_RTO,
        fields: &[
            ExpectedField::new("timeout_num", 0, 4),
            ExpectedField::new("min_rtt", 4, 4),
            ExpectedField::new("rtt", 8, 4),
            ExpectedField::new("segment_len", 12, 4),
            ExpectedField::new("inflight", 16, 4),
            ExpectedField::new("pkts_out", 20, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_WINDOW_COLLAPSE,
        fields: &[
            ExpectedField::new("count", 0, 4),
            ExpectedField::new("direction", 4, 1),
            ExpectedField::new("inflight", 8, 4),
            ExpectedField::new("has_collapsed", 12, 1),
        ],
    },
];

/// `if` provider: whether `core.rs`'s `is_if` block has an arm for
/// `event_id`. The `if` provider is fully decoded today (every id the
/// schema defines, 0-9 and 20-22, has a hardcoded arm).
pub fn if_has_hardcoded_decoder(event_id: u32) -> bool {
    matches!(
        event_id,
        EVT_IF_INFO
            | EVT_IF_ATTACH
            | EVT_IF_DETACH
            | EVT_IF_RENAME
            | EVT_IF_LINK_STATE
            | EVT_IF_FLAGS_CHANGE
            | EVT_IF_MTU_CHANGE
            | EVT_IF_CAPABILITIES_CHANGE
            | EVT_IF_ADDR_ADD
            | EVT_IF_ADDR_REMOVE
            | EVT_IF_SAMPLE
            | EVT_IF_SAMPLE_BYTES
            | EVT_IF_SAMPLE_PACKETS
    )
}

pub static IF_EXPECTED: &[ExpectedEvent] = &[
    ExpectedEvent {
        // vnet_id (offset 8, size 8) isn't read by the decoder today.
        event_id: EVT_IF_INFO,
        fields: &[
            ExpectedField::new("period_us", 0, 4),
            ExpectedField::new("nifs", 16, 2),
        ],
    },
    ExpectedEvent {
        event_id: EVT_IF_ATTACH,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("name", 2, 16),
            ExpectedField::new("if_type", 18, 1),
            ExpectedField::new("driver", 19, 16),
            ExpectedField::new("mtu", 36, 4),
            ExpectedField::new("baudrate", 40, 8),
            ExpectedField::new("hwaddr_len", 48, 1),
            ExpectedField::varlen("hwaddr", 49),
        ],
    },
    ExpectedEvent {
        event_id: EVT_IF_DETACH,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("name", 2, 16),
        ],
    },
    ExpectedEvent {
        event_id: EVT_IF_RENAME,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("old_name", 2, 16),
            ExpectedField::new("new_name", 18, 16),
        ],
    },
    ExpectedEvent {
        event_id: EVT_IF_LINK_STATE,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("state", 2, 1),
            ExpectedField::new("baudrate", 8, 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_IF_FLAGS_CHANGE,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("old_flags", 4, 4),
            ExpectedField::new("new_flags", 8, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_IF_MTU_CHANGE,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("old_mtu", 4, 4),
            ExpectedField::new("new_mtu", 8, 4),
        ],
    },
    ExpectedEvent {
        // The kernel schema carries only old_caps/new_caps; caps2 is
        // read by name and shown only when the schema has it.
        event_id: EVT_IF_CAPABILITIES_CHANGE,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("old_caps", 4, 4),
            ExpectedField::new("new_caps", 8, 4),
            ExpectedField::optional("old_caps2", 12, 4),
            ExpectedField::optional("new_caps2", 16, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_IF_ADDR_ADD,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("family", 2, 2),
            ExpectedField::new("prefixlen", 4, 1),
            ExpectedField::new("addr_len", 5, 1),
            ExpectedField::varlen("addr", 6),
        ],
    },
    ExpectedEvent {
        event_id: EVT_IF_ADDR_REMOVE,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("family", 2, 2),
            ExpectedField::new("prefixlen", 4, 1),
            ExpectedField::new("addr_len", 5, 1),
            ExpectedField::varlen("addr", 6),
        ],
    },
    ExpectedEvent {
        // Deprecated; kept so old captures' embedded schema still
        // validates against a hardcoded decoder.
        event_id: EVT_IF_SAMPLE,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("ibytes", 8, 8),
            ExpectedField::new("obytes", 16, 8),
            ExpectedField::new("ipackets", 24, 8),
            ExpectedField::new("opackets", 32, 8),
            ExpectedField::new("ierrors", 40, 8),
            ExpectedField::new("oerrors", 48, 8),
            ExpectedField::new("iqdrops", 56, 8),
            ExpectedField::new("oqdrops", 64, 8),
            ExpectedField::new("imcasts", 72, 8),
            ExpectedField::new("omcasts", 80, 8),
            ExpectedField::new("noproto", 88, 8),
        ],
    },
    ExpectedEvent {
        // other_ibytes/obytes (NCD-10070): core.rs reads them only when the
        // schema has them, else derives them from the totals.
        event_id: EVT_IF_SAMPLE_BYTES,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("ibytes", 8, 8),
            ExpectedField::new("obytes", 16, 8),
            ExpectedField::new("tcp_ibytes", 24, 8),
            ExpectedField::new("tcp_obytes", 32, 8),
            ExpectedField::new("udp_ibytes", 40, 8),
            ExpectedField::new("udp_obytes", 48, 8),
            ExpectedField::optional("other_ibytes", 56, 8),
            ExpectedField::optional("other_obytes", 64, 8),
        ],
    },
    ExpectedEvent {
        // other_ipackets/opackets: read conditionally, as above.
        event_id: EVT_IF_SAMPLE_PACKETS,
        fields: &[
            ExpectedField::new("ifindex", 0, 2),
            ExpectedField::new("ipackets", 8, 8),
            ExpectedField::new("opackets", 16, 8),
            ExpectedField::new("tcp_ipackets", 24, 8),
            ExpectedField::new("tcp_opackets", 32, 8),
            ExpectedField::new("udp_ipackets", 40, 8),
            ExpectedField::new("udp_opackets", 48, 8),
            ExpectedField::new("ierrors", 56, 8),
            ExpectedField::new("oerrors", 64, 8),
            ExpectedField::new("iqdrops", 72, 8),
            ExpectedField::new("oqdrops", 80, 8),
            ExpectedField::new("imcasts", 88, 8),
            ExpectedField::new("omcasts", 96, 8),
            ExpectedField::new("noproto", 104, 8),
            ExpectedField::optional("other_ipackets", 112, 8),
            ExpectedField::optional("other_opackets", 120, 8),
        ],
    },
];

/// `cpu` provider: whether `core.rs`'s cpu-family block has an arm for
/// `event_id` (in the *legacy*, non-split-schema numbering — see
/// `ProviderKind::Cpu` / `split_schema` in `core.rs`).
pub fn cpu_has_hardcoded_decoder(event_id: u32) -> bool {
    matches!(
        event_id,
        EVT_CPU_KERNEL_INFO
            | EVT_CPU_THREAD_CREATE
            | EVT_CPU_THREAD_EXIT
            | EVT_CPU_SAMPLE
            | EVT_CPU_OVERFLOW
            | EVT_CPU_LINKER_FILE
            | EVT_CPU_SYMBOL_RANGE
            | EVT_CPU_USER_FILE
            | EVT_CPU_PROC_MAP
            | EVT_CPU_USER_SYMBOL_RANGE
    )
}

/// `cpu` provider: left empty. Split-schema captures use a different
/// event-id namespace (`CPU_INFO`/`CPU_SAMPLE` at ids 0/1) than the
/// unified `cpu` provider (`KERNEL_INFO`/`THREAD_CREATE`/`CPU_SAMPLE`
/// at 0/1/3). A single Expected table cannot name both without marking
/// one of them divergent and dumping stack samples into
/// `generic_events`. Schema-present CPU events still decode by field
/// name; populate this table only when the id spaces are unified or
/// the validator is split-schema aware.
pub static CPU_EXPECTED: &[ExpectedEvent] = &[];

/// `hpts` provider: whether a hardcoded decoder exists for `event_id`,
/// across *both* schemas this provider's id namespace carries (the
/// timing-wheel trace, ids 0-14 plus `OPPOR_SAMPLE` at 17, decoded by
/// `hpts_wheel.rs`; and `STARS_AGG_SAMPLE`, id 16, decoded in `core.rs`'s
/// `is_hpts` block).
/// `LATENESS_SAMPLE`/`TP_BATCH_SAMPLE`/`OPPOR_SAMPLE` (ids 13-14, 17) have
/// no `HPTS_EXPECTED`
/// entry below, same as `EVT_TCP_GP_SAMPLE` in `TCP_EXPECTED`: both are
/// the generic `nbuckets:u8 + nbuckets*u64` histogram shape rather than
/// a fixed field layout, so there's nothing offset-based to check.
///
/// Listing an id here is load-bearing, not bookkeeping: `core.rs` decodes
/// generically and `continue`s *before* it reaches the `is_hpts` block, so
/// an id missing from this match never reaches `hpts_wheel.rs` at all and
/// silently lands in `generic_events` instead.
pub fn hpts_has_hardcoded_decoder(event_id: u32) -> bool {
    matches!(
        event_id,
        EVT_HPTS_INIT
            | EVT_HPTS_INIT_HPTS_ENTRY
            | EVT_HPTS_INIT_TIMING_PARAMS
            | EVT_HPTS_STATE_ACTIVE
            | EVT_HPTS_STATE_INACTIVE
            | EVT_HPTS_SLEEP
            | EVT_HPTS_CONN_INSERT
            | EVT_HPTS_CONN_REMOVE
            | EVT_HPTS_CONN_MOVE
            | EVT_HPTS_WHEEL_WRAP
            | EVT_HPTS_PROCESSING
            | EVT_HPTS_PERF_PROCESSING
            | EVT_HPTS_PERF_LATENESS
            | EVT_HPTS_LATENESS_SAMPLE
            | EVT_HPTS_TP_BATCH_SAMPLE
            | EVT_HPTS_OPPOR_SAMPLE
            | EVT_HPTS_SLOT_GATE_SAMPLE
            | EVT_HPTS_STARS_AGG_SAMPLE
    )
}

/// hpts timing-wheel ids 0-12, transcribed 1:1 from `hpts_wheel.rs`'s
/// match arms (which were themselves transcribed from
/// `hpts_eventlog_schema.src`). Ids 13-14 (`LATENESS_SAMPLE`/
/// `TP_BATCH_SAMPLE`), `OPPOR_SAMPLE` (id 17), and `STARS_AGG_SAMPLE`
/// (id 16) have no entry here
/// — same reasoning as `CPU_EXPECTED` above, plus (for 13-14
/// specifically) the generic histogram shape has no per-field offsets
/// to check. If a resolved schema ever does define one of these,
/// `validate_hardcoded_decoders` simply won't check it (absence of an
/// `ExpectedEvent` entry, not absence from the schema), which is the
/// same safe default as any other unchecked event.
pub static HPTS_EXPECTED: &[ExpectedEvent] = &[
    ExpectedEvent {
        event_id: EVT_HPTS_INIT,
        fields: &[
            ExpectedField::new("num_hptss", 0, 4),
            ExpectedField::new("bind_type", 4, 1),
            ExpectedField::new("num_slots", 8, 4),
            ExpectedField::new("usecs_per_slot", 12, 4),
            ExpectedField::new("total_slot_time", 16, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_INIT_HPTS_ENTRY,
        fields: &[],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_INIT_TIMING_PARAMS,
        fields: &[
            ExpectedField::new("usecs_per_slot", 0, 4),
            ExpectedField::new("total_slot_time", 4, 4),
            ExpectedField::new("num_slots", 8, 4),
            ExpectedField::new("wheel_mask", 12, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_STATE_ACTIVE,
        fields: &[
            ExpectedField::new("from_callout", 0, 1),
            ExpectedField::new("cur_slot", 4, 4),
            ExpectedField::new("tp_cur_count", 8, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_STATE_INACTIVE,
        fields: &[ExpectedField::new("slots_ran", 0, 4)],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_SLEEP,
        fields: &[ExpectedField::new("sleep_time", 0, 4)],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_CONN_INSERT,
        fields: &[
            ExpectedField::new("gencnt", 0, 4),
            ExpectedField::new("slot", 4, 4),
            ExpectedField::new("usecs", 8, 4),
            ExpectedField::new("tp_cur_count", 12, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_CONN_REMOVE,
        fields: &[
            ExpectedField::new("gencnt", 0, 4),
            ExpectedField::new("slot", 4, 4),
            ExpectedField::new("tp_cur_count", 8, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_CONN_MOVE,
        fields: &[
            ExpectedField::new("gencnt", 0, 4),
            ExpectedField::new("old_slot", 4, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_WHEEL_WRAP,
        fields: &[
            ExpectedField::new("wrap_loop_cnt", 0, 4),
            ExpectedField::new("slots_to_run", 4, 4),
            ExpectedField::new("prev_slot", 8, 4),
            ExpectedField::new("cur_slot", 12, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_PROCESSING,
        fields: &[
            ExpectedField::new("from_callout", 0, 1),
            ExpectedField::new("prev_slot", 4, 4),
            ExpectedField::new("cur_slot", 8, 4),
            ExpectedField::new("slots_to_run", 12, 4),
            ExpectedField::new("tp_cur_count", 16, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_PERF_PROCESSING,
        fields: &[
            ExpectedField::new("from_callout", 0, 1),
            ExpectedField::new("slots_processed", 4, 4),
            ExpectedField::new("tps_processed", 8, 4),
            ExpectedField::new("runtime_usec", 12, 4),
            ExpectedField::new("loop_cnt", 16, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_PERF_LATENESS,
        fields: &[
            ExpectedField::new("gencnt", 0, 4),
            ExpectedField::new("lateness_slots", 4, 4),
            ExpectedField::new("lateness_usec", 8, 4),
            ExpectedField::new("requested_time", 12, 4),
            ExpectedField::new("actual_time", 16, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_LATENESS_SAMPLE,
        fields: &[
            ExpectedField::new("nbuckets", 0, 1),
            ExpectedField::varlen("buckets", 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_TP_BATCH_SAMPLE,
        fields: &[
            ExpectedField::new("nbuckets", 0, 1),
            ExpectedField::varlen("buckets", 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_SLOT_GATE_SAMPLE,
        fields: &[
            ExpectedField::new("fires", 0, 4),
            ExpectedField::new("diverts", 4, 4),
            ExpectedField::new("bytes", 8, 8),
            ExpectedField::new("rollover_max", 16, 4),
            ExpectedField::new("walks", 20, 4),
            ExpectedField::new("closed", 24, 4),
            ExpectedField::new("default_bytes", 28, 8),
            ExpectedField::new("rack_bytes", 36, 8),
            ExpectedField::new("bbr_bytes", 44, 8),
            ExpectedField::new("closed_tcp_bytes", 52, 8),
            ExpectedField::new("caller0", 60, 8),
            ExpectedField::new("caller0_bytes", 68, 8),
            ExpectedField::new("caller1", 76, 8),
            ExpectedField::new("caller1_bytes", 84, 8),
            ExpectedField::new("caller_other_bytes", 92, 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_SLOT_GATE_SAMPLE,
        fields: &[
            ExpectedField::new("fires", 0, 4),
            ExpectedField::new("diverts", 4, 4),
            ExpectedField::new("bytes", 8, 8),
            ExpectedField::new("rollover_max", 16, 4),
            ExpectedField::new("walks", 20, 4),
            ExpectedField::new("closed", 24, 4),
            ExpectedField::new("default_bytes", 28, 8),
            ExpectedField::new("rack_bytes", 36, 8),
            ExpectedField::new("bbr_bytes", 44, 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_HPTS_SLOT_GATE_SAMPLE,
        fields: &[
            ExpectedField::new("fires", 0, 4),
            ExpectedField::new("diverts", 4, 4),
            ExpectedField::new("bytes", 8, 8),
            ExpectedField::new("rollover_max", 16, 4),
            ExpectedField::new("walks", 20, 4),
            ExpectedField::new("closed", 24, 4),
        ],
    },
];

pub fn nginx_has_hardcoded_decoder(event_id: u32) -> bool {
    matches!(
        event_id,
        EVT_NGINX_REQUEST_START
            | EVT_NGINX_RESPONSE_HEADERS
            | EVT_NGINX_REQUEST_DONE
            | EVT_NGINX_REQUEST_MESSAGE
            | EVT_NGINX_SAMPLE
            | EVT_NGINX_CONN_START
            | EVT_NGINX_CONN_END
            | EVT_NGINX_TLS_HANDSHAKE_DONE
            | EVT_NGINX_TLS_HANDSHAKE_FAIL
            | EVT_NGINX_SENDFILE_BUSY_DEFER
            | EVT_NGINX_SENDFILE_ERROR
            | EVT_NGINX_FILE_OPEN_FAIL
            | EVT_NGINX_CACHE_LOOKUP
            | EVT_NGINX_UPSTREAM_ATTEMPT_START
            | EVT_NGINX_UPSTREAM_ATTEMPT_END
            | EVT_NGINX_WORKER_STALL
            | EVT_NGINX_AIO_SYNC_FALLBACK
    )
}

// Alternatives for one event id are tried in order; list the newest
// layout first so its extra fields don't read as undecoded.
pub static NGINX_EXPECTED: &[ExpectedEvent] = &[
    // Shipped firmware: packed, varlen `uri`.
    ExpectedEvent {
        event_id: EVT_NGINX_REQUEST_START,
        fields: &[
            ExpectedField::new("request", 0, 4),
            ExpectedField::new("range_start", 4, 8),
            ExpectedField::new("range_end", 12, 8),
            ExpectedField::new("method", 20, 16),
            ExpectedField::new("uri_len", 36, 2),
            ExpectedField::varlen("uri", 38),
        ],
    },
    // Pre-release: natural alignment, fixed `uri[256]`.
    ExpectedEvent {
        event_id: EVT_NGINX_REQUEST_START,
        fields: &[
            ExpectedField::new("request", 0, 4),
            ExpectedField::new("range_start", 8, 8),
            ExpectedField::new("range_end", 16, 8),
            ExpectedField::new("method", 24, 16),
            ExpectedField::new("uri", 40, 256),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_RESPONSE_HEADERS,
        fields: &[
            ExpectedField::new("request", 0, 4),
            ExpectedField::new("status", 4, 2),
            ExpectedField::new("content_length", 6, 8),
            ExpectedField::new("dev", 14, 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_RESPONSE_HEADERS,
        fields: &[
            ExpectedField::new("request", 0, 4),
            ExpectedField::new("status", 4, 2),
            ExpectedField::new("content_length", 8, 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_REQUEST_DONE,
        fields: &[
            ExpectedField::new("request", 0, 4),
            ExpectedField::new("status", 4, 2),
            ExpectedField::new("bytes_sent", 6, 8),
            ExpectedField::new("flags", 14, 2),
            ExpectedField::new("err_status", 16, 2),
            ExpectedField::new("content_length", 18, 8),
            ExpectedField::new("request_time_ms", 26, 4),
            ExpectedField::new("cache_status", 30, 1),
            ExpectedField::new("upstream_tries", 31, 1),
            ExpectedField::new("busy_events", 32, 4),
            ExpectedField::new("busy_defer_ms", 36, 4),
            ExpectedField::new("aio_reads", 40, 4),
            ExpectedField::new("aio_wait_ms", 44, 4),
            ExpectedField::new("aio_max_ms", 48, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_REQUEST_DONE,
        fields: &[
            ExpectedField::new("request", 0, 4),
            ExpectedField::new("status", 4, 2),
            ExpectedField::new("bytes_sent", 8, 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_REQUEST_MESSAGE,
        fields: &[
            ExpectedField::new("request", 0, 4),
            ExpectedField::new("msg_len", 4, 2),
            ExpectedField::varlen("msg", 6),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_SAMPLE,
        fields: &[
            ExpectedField::new("resp_1xx", 0, 8),
            ExpectedField::new("resp_2xx", 8, 8),
            ExpectedField::new("resp_3xx", 16, 8),
            ExpectedField::new("resp_4xx", 24, 8),
            ExpectedField::new("resp_5xx", 32, 8),
            ExpectedField::new("resp_other", 40, 8),
            ExpectedField::new("bytes_sent", 48, 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_CONN_START,
        fields: &[
            ExpectedField::new("server_port", 0, 2),
            ExpectedField::new("flags", 2, 1),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_CONN_END,
        fields: &[
            ExpectedField::new("reason", 0, 1),
            ExpectedField::new("requests", 1, 4),
            ExpectedField::new("bytes_sent", 5, 8),
            ExpectedField::new("duration_ms", 13, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_TLS_HANDSHAKE_DONE,
        fields: &[
            ExpectedField::new("version", 0, 2),
            ExpectedField::new("flags", 2, 1),
            ExpectedField::new("handshake_ms", 3, 4),
            ExpectedField::new("cipher", 7, 48),
            ExpectedField::new("alpn", 55, 16),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_TLS_HANDSHAKE_FAIL,
        fields: &[
            ExpectedField::new("kind", 0, 1),
            ExpectedField::new("ssl_error", 1, 4),
            ExpectedField::new("reason", 5, 4),
            ExpectedField::new("sys_errno", 9, 4),
            ExpectedField::new("handshake_ms", 13, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_SENDFILE_BUSY_DEFER,
        fields: &[
            ExpectedField::new("delay_ms", 0, 2),
            ExpectedField::new("file_pos", 2, 8),
            ExpectedField::new("dev", 10, 8),
            ExpectedField::new("ktls", 18, 1),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_SENDFILE_ERROR,
        fields: &[
            ExpectedField::new("kind", 0, 1),
            ExpectedField::new("sys_errno", 1, 4),
            ExpectedField::new("file_pos", 5, 8),
            ExpectedField::new("dev", 13, 8),
            ExpectedField::new("ktls", 21, 1),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_FILE_OPEN_FAIL,
        fields: &[
            ExpectedField::new("request", 0, 4),
            ExpectedField::new("status", 4, 2),
            ExpectedField::new("sys_errno", 6, 4),
            ExpectedField::new("syscall", 10, 16),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_CACHE_LOOKUP,
        fields: &[
            ExpectedField::new("request", 0, 4),
            ExpectedField::new("subrequest", 4, 1),
            ExpectedField::new("cache_status", 5, 1),
            ExpectedField::new("fas_state", 6, 1),
            ExpectedField::new("lock_wait_ms", 7, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_UPSTREAM_ATTEMPT_START,
        fields: &[
            ExpectedField::new("request", 0, 4),
            ExpectedField::new("subrequest", 4, 1),
            ExpectedField::new("attempt", 5, 2),
            ExpectedField::new("connect_rc", 7, 1),
            ExpectedField::new("local_port", 8, 2),
            ExpectedField::new("dns_ms", 10, 4),
            ExpectedField::new("upstream_session", 14, 8),
            ExpectedField::new("target_type", 22, 16),
            ExpectedField::new("peer_len", 38, 2),
            ExpectedField::varlen("peer", 40),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_UPSTREAM_ATTEMPT_END,
        fields: &[
            ExpectedField::new("request", 0, 4),
            ExpectedField::new("subrequest", 4, 1),
            ExpectedField::new("attempt", 5, 2),
            ExpectedField::new("ft", 7, 4),
            ExpectedField::new("status", 11, 2),
            ExpectedField::new("connect_ms", 13, 4),
            ExpectedField::new("header_ms", 17, 4),
            ExpectedField::new("elapsed_ms", 21, 4),
            ExpectedField::new("bytes_received", 25, 8),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_WORKER_STALL,
        fields: &[
            ExpectedField::new("stall_ms", 0, 4),
            ExpectedField::new("line", 4, 4),
            ExpectedField::new("prev_line", 8, 4),
            ExpectedField::new("file", 12, 48),
            ExpectedField::new("prev_file", 60, 48),
            ExpectedField::new("func", 108, 64),
        ],
    },
    ExpectedEvent {
        event_id: EVT_NGINX_AIO_SYNC_FALLBACK,
        fields: &[
            ExpectedField::new("offset", 0, 8),
            ExpectedField::new("size", 8, 8),
            ExpectedField::new("read_us", 16, 4),
            ExpectedField::new("dev", 20, 8),
        ],
    },
];

pub fn ella_relay_has_hardcoded_decoder(event_id: u32) -> bool {
    matches!(
        event_id,
        EVT_ELLA_RELAY_INSTANCE_ATTACH
            | EVT_ELLA_RELAY_NODE_INFO
            | EVT_ELLA_RELAY_RELAY_ERROR
            | EVT_ELLA_RELAY_SESSION_CREATE
            | EVT_ELLA_RELAY_SESSION_DESTROY
            | EVT_ELLA_RELAY_CHANNEL_STATE
            | EVT_ELLA_RELAY_CHANNEL_ID_ASSIGNED
            | EVT_ELLA_RELAY_CHANNEL_ERROR
            | EVT_ELLA_RELAY_CHANNEL_DESTROYED
            | EVT_ELLA_RELAY_CHANNEL_SUBSCRIBER
            | EVT_ELLA_RELAY_SUBSCRIBER_IDENTITY
            | EVT_ELLA_RELAY_CHANNEL_COUNT
            | EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL
            | EVT_ELLA_RELAY_CHANNEL_SAMPLE
            | EVT_ELLA_RELAY_FRAME_SAMPLE
            | EVT_ELLA_RELAY_OFFLOAD_STATS
            | EVT_ELLA_RELAY_TRAFFIC_STATS
            | EVT_ELLA_RELAY_DELIVERED_STATS
            | EVT_ELLA_RELAY_FRAME_SENT
            | EVT_ELLA_RELAY_FRAME_RELAY
            | EVT_ELLA_RELAY_OBJECT_STATE
            | EVT_ELLA_RELAY_OBJECT_TIMING
    )
}

pub static ELLA_RELAY_EXPECTED: &[ExpectedEvent] = &[
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_INSTANCE_ATTACH,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("side", 4, 1),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_NODE_INFO,
        fields: &[ExpectedField::new("role", 0, 1)],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_RELAY_ERROR,
        fields: &[
            ExpectedField::new("code", 0, 1),
            ExpectedField::new("instance_id", 1, 4),
            ExpectedField::new("channel_id", 5, 4),
            ExpectedField::new("detail", 9, 4),
            ExpectedField::new("subscriber_port", 13, 2),
            ExpectedField::new("channel_id_known", 15, 1),
            ExpectedField::new("message_type", 16, 1),
            ExpectedField::new("subscriber_address_len", 17, 1),
            ExpectedField::varlen("subscriber_address", 18),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_RELAY_ERROR,
        fields: &[
            ExpectedField::new("code", 0, 1),
            ExpectedField::new("instance_id", 4, 4),
            ExpectedField::new("channel_id", 8, 4),
            ExpectedField::new("channel_id_known", 12, 1),
            ExpectedField::new("detail", 16, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_SESSION_CREATE,
        fields: &[
            ExpectedField::new("channel_name", 0, 64),
            ExpectedField::new("version", 64, 1),
            ExpectedField::new("instance_id", 65, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_SESSION_CREATE,
        fields: &[
            ExpectedField::new("channel_name", 0, 64),
            ExpectedField::new("version", 64, 1),
            ExpectedField::new("instance_id", 68, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_SESSION_DESTROY,
        fields: &[],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_STATE,
        fields: &[
            ExpectedField::new("state", 0, 1),
            ExpectedField::new("reason", 1, 1),
            ExpectedField::new("instance_id", 2, 4),
            ExpectedField::new("subscribers", 6, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_STATE,
        fields: &[
            ExpectedField::new("state", 0, 1),
            ExpectedField::new("reason", 1, 1),
            ExpectedField::new("instance_id", 4, 4),
            ExpectedField::new("subscribers", 8, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_ID_ASSIGNED,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("channel_id", 4, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_ERROR,
        fields: &[
            ExpectedField::new("code", 0, 1),
            ExpectedField::new("instance_id", 1, 4),
            ExpectedField::new("detail", 5, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_ERROR,
        fields: &[
            ExpectedField::new("code", 0, 1),
            ExpectedField::new("instance_id", 4, 4),
            ExpectedField::new("detail", 8, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_DESTROYED,
        fields: &[
            ExpectedField::new("reason", 0, 1),
            ExpectedField::new("instance_id", 1, 4),
            ExpectedField::new("subscribers", 5, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_DESTROYED,
        fields: &[
            ExpectedField::new("reason", 0, 1),
            ExpectedField::new("instance_id", 4, 4),
            ExpectedField::new("subscribers", 8, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_SUBSCRIBER,
        fields: &[
            ExpectedField::new("state", 0, 1),
            ExpectedField::new("reason", 1, 1),
            ExpectedField::new("instance_id", 2, 4),
            ExpectedField::new("subscribers", 6, 4),
            ExpectedField::new("subscriber_port", 10, 2),
            ExpectedField::new("subscriber_address_len", 12, 1),
            ExpectedField::varlen("subscriber_address", 13),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_SUBSCRIBER,
        fields: &[
            ExpectedField::new("state", 0, 1),
            ExpectedField::new("reason", 1, 1),
            ExpectedField::new("instance_id", 4, 4),
            ExpectedField::new("subscriber_address", 8, 48),
            ExpectedField::new("subscriber_port", 56, 2),
            ExpectedField::new("subscribers", 60, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_SUBSCRIBER_IDENTITY,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("request_id", 4, 4),
            ExpectedField::new("subscriber_port", 8, 2),
            ExpectedField::new("subscriber_address", 10, 16),
            ExpectedField::new("pbcid_len", 26, 1),
            ExpectedField::varlen("pbcid", 27),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_COUNT,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("channels", 4, 4),
            ExpectedField::new("channels_pending_leave", 8, 4),
            ExpectedField::new("channels_pending_retire", 12, 4),
            ExpectedField::new("subscribers", 16, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
        fields: &[
            ExpectedField::new("channels_up", 0, 4),
            ExpectedField::new("channels_dn", 4, 4),
            ExpectedField::new("channels_pending_leave_up", 8, 4),
            ExpectedField::new("channels_pending_leave_dn", 12, 4),
            ExpectedField::new("channels_pending_retire_up", 16, 4),
            ExpectedField::new("channels_pending_retire_dn", 20, 4),
            ExpectedField::new("subscribers_up", 24, 4),
            ExpectedField::new("subscribers_dn", 28, 4),
            ExpectedField::new("objects_up", 32, 4),
            ExpectedField::new("objects_dn", 36, 4),
            ExpectedField::new("packets_up", 40, 4),
            ExpectedField::new("packets_dn", 44, 4),
            ExpectedField::new("bytes_up", 48, 8),
            ExpectedField::new("bytes_dn", 56, 8),
            ExpectedField::new("joins_up", 64, 4),
            ExpectedField::new("joins_dn", 68, 4),
            ExpectedField::new("switches_up", 72, 4),
            ExpectedField::new("switches_dn", 76, 4),
            ExpectedField::new("packets_dlvr", 80, 4),
            ExpectedField::new("bytes_dlvr", 84, 8),
            ExpectedField::new("joins_dlvr", 92, 4),
            ExpectedField::new("switches_dlvr", 96, 4),
            ExpectedField::new("new_clients", 100, 4),
            ExpectedField::new("clients_up", 104, 4),
            ExpectedField::new("clients_dn", 108, 4),
            ExpectedField::new("subscribers_dlvr", 112, 4),
        ],
    },
    // CDN-metrics CHANNEL_STATS before unique clients and subtree subscriptions.
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
        fields: &[
            ExpectedField::new("channels_up", 0, 4),
            ExpectedField::new("channels_dn", 4, 4),
            ExpectedField::new("channels_pending_leave_up", 8, 4),
            ExpectedField::new("channels_pending_leave_dn", 12, 4),
            ExpectedField::new("channels_pending_retire_up", 16, 4),
            ExpectedField::new("channels_pending_retire_dn", 20, 4),
            ExpectedField::new("subscribers_up", 24, 4),
            ExpectedField::new("subscribers_dn", 28, 4),
            ExpectedField::new("objects_up", 32, 4),
            ExpectedField::new("objects_dn", 36, 4),
            ExpectedField::new("packets_up", 40, 4),
            ExpectedField::new("packets_dn", 44, 4),
            ExpectedField::new("bytes_up", 48, 8),
            ExpectedField::new("bytes_dn", 56, 8),
            ExpectedField::new("joins_up", 64, 4),
            ExpectedField::new("joins_dn", 68, 4),
            ExpectedField::new("switches_up", 72, 4),
            ExpectedField::new("switches_dn", 76, 4),
            ExpectedField::new("packets_dlvr", 80, 4),
            ExpectedField::new("bytes_dlvr", 84, 8),
            ExpectedField::new("joins_dlvr", 92, 4),
            ExpectedField::new("switches_dlvr", 96, 4),
            ExpectedField::new("new_clients", 100, 4),
        ],
    },
    // CDN-metrics CHANNEL_STATS with 32-bit byte counters.
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
        fields: &[
            ExpectedField::new("channels_up", 0, 4),
            ExpectedField::new("channels_dn", 4, 4),
            ExpectedField::new("channels_pending_leave_up", 8, 4),
            ExpectedField::new("channels_pending_leave_dn", 12, 4),
            ExpectedField::new("channels_pending_retire_up", 16, 4),
            ExpectedField::new("channels_pending_retire_dn", 20, 4),
            ExpectedField::new("subscribers_up", 24, 4),
            ExpectedField::new("subscribers_dn", 28, 4),
            ExpectedField::new("objects_up", 32, 4),
            ExpectedField::new("objects_dn", 36, 4),
            ExpectedField::new("packets_up", 40, 4),
            ExpectedField::new("packets_dn", 44, 4),
            ExpectedField::new("bytes_up", 48, 4),
            ExpectedField::new("bytes_dn", 52, 4),
            ExpectedField::new("joins_up", 56, 4),
            ExpectedField::new("joins_dn", 60, 4),
            ExpectedField::new("switches_up", 64, 4),
            ExpectedField::new("switches_dn", 68, 4),
            ExpectedField::new("packets_dlvr", 72, 4),
            ExpectedField::new("bytes_dlvr", 76, 4),
            ExpectedField::new("joins_dlvr", 80, 4),
            ExpectedField::new("switches_dlvr", 84, 4),
            ExpectedField::new("new_clients", 88, 4),
        ],
    },
    // Current CHANNEL_STATS: gauges and objects; counters moved to
    // TRAFFIC_STATS / DELIVERED_STATS.
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
        fields: &[
            ExpectedField::new("channels_up", 0, 4),
            ExpectedField::new("channels_dn", 4, 4),
            ExpectedField::new("channels_pending_leave_up", 8, 4),
            ExpectedField::new("channels_pending_leave_dn", 12, 4),
            ExpectedField::new("channels_pending_retire_up", 16, 4),
            ExpectedField::new("channels_pending_retire_dn", 20, 4),
            ExpectedField::new("subscribers_up", 24, 4),
            ExpectedField::new("subscribers_dn", 28, 4),
            ExpectedField::new("objects_up", 32, 4),
            ExpectedField::new("objects_dn", 36, 4),
        ],
    },
    // Pre-CDN-metrics CHANNEL_STATS (gauges only).
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_COUNT_TOTAL,
        fields: &[
            ExpectedField::new("channels_up", 0, 4),
            ExpectedField::new("channels_dn", 4, 4),
            ExpectedField::new("channels_pending_leave_up", 8, 4),
            ExpectedField::new("channels_pending_leave_dn", 12, 4),
            ExpectedField::new("channels_pending_retire_up", 16, 4),
            ExpectedField::new("channels_pending_retire_dn", 20, 4),
            ExpectedField::new("subscribers_up", 24, 4),
            ExpectedField::new("subscribers_dn", 28, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_SAMPLE,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("subscribers", 4, 4),
            ExpectedField::new("subscribers_pending", 8, 4),
            ExpectedField::new("subscribers_leaving", 12, 4),
            ExpectedField::new("objects_completed", 16, 4),
            ExpectedField::new("object_bytes", 20, 4),
            ExpectedField::new("packets", 24, 4),
            ExpectedField::new("inproc_attached", 28, 4),
            ExpectedField::new("publish_no_ring", 32, 4),
            ExpectedField::new("drain_skipped_pending", 36, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_SAMPLE,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("subscribers", 4, 4),
            ExpectedField::new("subscribers_pending", 8, 4),
            ExpectedField::new("subscribers_leaving", 12, 4),
            ExpectedField::new("objects_completed", 16, 4),
            ExpectedField::new("object_bytes", 20, 4),
            ExpectedField::new("packets", 24, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_CHANNEL_SAMPLE,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("subscribers", 4, 4),
            ExpectedField::new("subscribers_pending", 8, 4),
            ExpectedField::new("subscribers_leaving", 12, 4),
            ExpectedField::new("objects_completed", 16, 4),
            ExpectedField::new("object_bytes", 20, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_FRAME_SAMPLE,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("frames", 4, 4),
            ExpectedField::new("frames_stretched", 8, 4),
            ExpectedField::new("sum_frame_len", 12, 4),
            ExpectedField::new("sum_planned_us", 16, 4),
            ExpectedField::new("sum_actual_us", 20, 4),
            ExpectedField::new("sum_fwd_us", 24, 4),
            ExpectedField::new("max_overshoot_us", 28, 4),
            ExpectedField::new("sum_packets", 32, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_FRAME_SAMPLE,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("frames", 4, 4),
            ExpectedField::new("frames_stretched", 8, 4),
            ExpectedField::new("sum_frame_len", 12, 4),
            ExpectedField::new("sum_planned_us", 16, 4),
            ExpectedField::new("sum_actual_us", 20, 4),
            ExpectedField::new("sum_fwd_us", 24, 4),
            ExpectedField::new("max_overshoot_us", 28, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_OFFLOAD_STATS,
        fields: &[
            ExpectedField::new("send_seq_ok", 0, 4),
            ExpectedField::new("send_seq_stale", 4, 4),
            ExpectedField::new("rxring_routed", 8, 4),
            ExpectedField::new("rxring_else_gate", 12, 4),
            ExpectedField::new("rxring_else_unreg", 16, 4),
            ExpectedField::new("rxring_else_malformed", 20, 4),
            ExpectedField::new("rxring_overwrites", 24, 4),
            ExpectedField::new("rxring_dropped_unreg", 28, 4),
            ExpectedField::new("rxring_result", 32, 4),
            ExpectedField::new("rxring_port", 36, 4),
            ExpectedField::new("rxring_errno", 40, 4),
            ExpectedField::new("rxring_region_bytes", 44, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_OFFLOAD_STATS,
        fields: &[
            ExpectedField::new("send_seq_ok", 0, 4),
            ExpectedField::new("send_seq_stale", 4, 4),
            ExpectedField::new("rxring_routed", 8, 4),
            ExpectedField::new("rxring_else_gate", 12, 4),
            ExpectedField::new("rxring_else_unreg", 16, 4),
            ExpectedField::new("rxring_else_malformed", 20, 4),
            ExpectedField::new("rxring_overwrites", 24, 4),
            ExpectedField::new("rxring_dropped_unreg", 28, 4),
            ExpectedField::new("rxring_result", 32, 4),
            ExpectedField::new("rxring_port", 36, 4),
            ExpectedField::new("rxring_errno", 40, 4),
            ExpectedField::new("rxring_region_bytes", 44, 4),
            ExpectedField::new("rxring_else_nocls", 48, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_OFFLOAD_STATS,
        fields: &[
            ExpectedField::new("send_seq_ok", 0, 4),
            ExpectedField::new("send_seq_stale", 4, 4),
            ExpectedField::new("rxring_routed", 8, 4),
            ExpectedField::new("rxring_else_gate", 12, 4),
            ExpectedField::new("rxring_else_unreg", 16, 4),
            ExpectedField::new("rxring_else_malformed", 20, 4),
            ExpectedField::new("rxring_overwrites", 24, 4),
            ExpectedField::new("rxring_dropped_unreg", 28, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_TRAFFIC_STATS,
        fields: &[
            ExpectedField::new("side", 0, 1),
            ExpectedField::new("packets", 1, 4),
            ExpectedField::new("bytes", 5, 8),
            ExpectedField::new("joins", 13, 4),
            ExpectedField::new("switches", 17, 4),
            ExpectedField::new("leaves", 21, 4),
            ExpectedField::new("expiries", 25, 4),
            ExpectedField::new("rejects", 29, 4),
            ExpectedField::new("new_clients", 33, 4),
            ExpectedField::new("clients", 37, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_DELIVERED_STATS,
        fields: &[
            ExpectedField::new("packets", 0, 4),
            ExpectedField::new("bytes", 4, 8),
            ExpectedField::new("joins", 12, 4),
            ExpectedField::new("switches", 16, 4),
            ExpectedField::new("subscriptions", 20, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_FRAME_SENT,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("object_id", 4, 4),
            ExpectedField::new("frame_index", 8, 4),
            ExpectedField::new("first_packet", 12, 4),
            ExpectedField::new("packets", 16, 4),
            ExpectedField::new("is_probe", 20, 1),
            ExpectedField::new("toggle", 21, 1),
            ExpectedField::new("frame_len", 22, 4),
            ExpectedField::new("planned_send_us", 26, 4),
            ExpectedField::new("actual_send_us", 30, 4),
            ExpectedField::new("start_lateness_us", 34, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_FRAME_RELAY,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("first_packet", 4, 4),
            ExpectedField::new("packets", 8, 4),
            ExpectedField::new("toggle", 12, 1),
            ExpectedField::new("boundary_index", 13, 1),
            ExpectedField::new("frame_len", 14, 4),
            ExpectedField::new("wire_send_us", 18, 4),
            ExpectedField::new("recv_us", 22, 4),
            ExpectedField::new("fwd_us", 26, 4),
            ExpectedField::new("residency_us", 30, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_OBJECT_STATE,
        fields: &[
            ExpectedField::new("state", 0, 1),
            ExpectedField::new("instance_id", 1, 4),
            ExpectedField::new("boundary_index", 5, 1),
            ExpectedField::new("first_packet", 6, 4),
            ExpectedField::new("packets", 10, 4),
            ExpectedField::new("bytes", 14, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_OBJECT_STATE,
        fields: &[
            ExpectedField::new("state", 0, 1),
            ExpectedField::new("instance_id", 4, 4),
            ExpectedField::new("boundary_index", 8, 1),
            ExpectedField::new("first_packet", 12, 4),
            ExpectedField::new("packets", 16, 4),
            ExpectedField::new("bytes", 20, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_OBJECT_TIMING,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("object_id", 4, 4),
            ExpectedField::new("first_packet", 8, 4),
            ExpectedField::new("packets_sent", 12, 4),
            ExpectedField::new("packets_total", 16, 4),
            ExpectedField::new("packets_unsent", 20, 4),
            ExpectedField::new("start_lateness_us", 24, 4),
            ExpectedField::new("signature_duration_us", 28, 4),
            ExpectedField::new("encode_duration_us", 32, 4),
            ExpectedField::new("enqueue_duration_us", 36, 4),
        ],
    },
    ExpectedEvent {
        event_id: EVT_ELLA_RELAY_OBJECT_TIMING,
        fields: &[
            ExpectedField::new("instance_id", 0, 4),
            ExpectedField::new("object_id", 4, 4),
            ExpectedField::new("packets_sent", 8, 4),
            ExpectedField::new("packets_total", 12, 4),
            ExpectedField::new("packets_unsent", 16, 4),
            ExpectedField::new("start_lateness_us", 20, 4),
            ExpectedField::new("signature_duration_us", 24, 4),
            ExpectedField::new("encode_duration_us", 28, 4),
            ExpectedField::new("enqueue_duration_us", 32, 4),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elog_parser::schema::test_support::{
        build_blob_multi, EventSpec, FieldSpec, StructSpec,
    };

    /// Packing/alignment that only moves offsets is safe: typed ingest
    /// reads by schema name. An extra field is additive (new_fields).
    #[test]
    fn packing_offset_shift_is_not_divergent() {
        let blob = build_blob_multi(
            "test",
            &[
                StructSpec {
                    name: "SHIFTED",
                    fields: vec![
                        FieldSpec::scalar("seq", DECODE_TYPE_U32, 0),
                        FieldSpec::scalar("inserted", DECODE_TYPE_U32, 4),
                        FieldSpec::scalar("len", DECODE_TYPE_U32, 8),
                    ],
                },
                StructSpec {
                    name: "STABLE",
                    fields: vec![
                        FieldSpec::scalar("a", DECODE_TYPE_U32, 0),
                        FieldSpec::scalar("b", DECODE_TYPE_U32, 4),
                    ],
                },
            ],
            &[
                EventSpec {
                    id: 500,
                    name: "SHIFTED",
                    struct_index: 1,
                    format: "",
                },
                EventSpec {
                    id: 501,
                    name: "STABLE",
                    struct_index: 2,
                    format: "",
                },
            ],
        );
        let schema = ProviderSchema::parse(&blob).unwrap();

        const EXPECTED: &[ExpectedEvent] = &[
            ExpectedEvent {
                event_id: 500,
                fields: &[
                    ExpectedField::new("seq", 0, 4),
                    ExpectedField::new("len", 4, 4),
                ],
            },
            ExpectedEvent {
                event_id: 501,
                fields: &[ExpectedField::new("a", 0, 4), ExpectedField::new("b", 4, 4)],
            },
        ];
        let result = validate_hardcoded_decoders(&schema, EXPECTED);

        assert!(
            result.divergent.is_empty(),
            "offset-only packing must not be divergence: {:?}",
            result.divergent
        );
        assert!(result.new_fields[0].contains("inserted"));
    }

    #[test]
    fn missing_field_is_divergent() {
        let blob = build_blob_multi(
            "test",
            &[StructSpec {
                name: "S",
                fields: vec![FieldSpec::scalar("seq", DECODE_TYPE_U32, 0)],
            }],
            &[EventSpec {
                id: 500,
                name: "S",
                struct_index: 1,
                format: "",
            }],
        );
        let schema = ProviderSchema::parse(&blob).unwrap();
        const EXPECTED: &[ExpectedEvent] = &[ExpectedEvent {
            event_id: 500,
            fields: &[
                ExpectedField::new("seq", 0, 4),
                ExpectedField::new("len", 4, 4),
            ],
        }];
        let result = validate_hardcoded_decoders(&schema, EXPECTED);
        assert_eq!(result.divergent, FxHashSet::from_iter([500]));
        assert!(
            result.divergent_reasons[0].contains("len"),
            "{:?}",
            result.divergent_reasons
        );
    }

    #[test]
    fn size_change_is_divergent() {
        let blob = build_blob_multi(
            "test",
            &[StructSpec {
                name: "S",
                fields: vec![FieldSpec::scalar("seq", DECODE_TYPE_U64, 0)],
            }],
            &[EventSpec {
                id: 500,
                name: "S",
                struct_index: 1,
                format: "",
            }],
        );
        let schema = ProviderSchema::parse(&blob).unwrap();
        const EXPECTED: &[ExpectedEvent] = &[ExpectedEvent {
            event_id: 500,
            fields: &[ExpectedField::new("seq", 0, 4)],
        }];
        let result = validate_hardcoded_decoders(&schema, EXPECTED);
        assert_eq!(result.divergent, FxHashSet::from_iter([500]));
        assert!(result.divergent_reasons[0].contains("seq"));
    }

    #[test]
    fn duplicate_event_entries_are_alternative_layouts() {
        let blob = build_blob_multi(
            "test",
            &[StructSpec {
                name: "PACKED",
                fields: vec![
                    FieldSpec::scalar("tag", DECODE_TYPE_U8, 0),
                    FieldSpec::scalar("value", DECODE_TYPE_U32, 1),
                ],
            }],
            &[EventSpec {
                id: 500,
                name: "PACKED",
                struct_index: 1,
                format: "",
            }],
        );
        let schema = ProviderSchema::parse(&blob).unwrap();
        const EXPECTED: &[ExpectedEvent] = &[
            ExpectedEvent {
                event_id: 500,
                fields: &[
                    ExpectedField::new("tag", 0, 1),
                    ExpectedField::new("value", 4, 4),
                ],
            },
            ExpectedEvent {
                event_id: 500,
                fields: &[
                    ExpectedField::new("tag", 0, 1),
                    ExpectedField::new("value", 1, 4),
                ],
            },
        ];

        let result = validate_hardcoded_decoders(&schema, EXPECTED);
        assert!(result.divergent.is_empty());
        assert!(result.divergent_reasons.is_empty());
    }

    /// Fields the schema grew *after* the ones the decoder reads (the
    /// real `IN` event's `state`/`th_off`/... additions) are safe,
    /// additive drift: reported as a notice, not divergence.
    #[test]
    fn additive_trailing_fields_are_a_notice_not_divergence() {
        let blob = build_blob_multi(
            "test",
            &[StructSpec {
                name: "IN",
                fields: vec![
                    FieldSpec::scalar("seq", DECODE_TYPE_U32, 0),
                    FieldSpec::scalar("ack", DECODE_TYPE_U32, 4),
                    FieldSpec::scalar("win", DECODE_TYPE_U32, 8),
                    FieldSpec::scalar("len", DECODE_TYPE_U32, 12),
                    FieldSpec::scalar("flags", DECODE_TYPE_U16, 16),
                    FieldSpec::scalar("state", DECODE_TYPE_U8, 18),
                    FieldSpec::scalar("extra", DECODE_TYPE_U32, 20),
                ],
            }],
            &[EventSpec {
                id: 100,
                name: "IN",
                struct_index: 1,
                format: "",
            }],
        );
        let schema = ProviderSchema::parse(&blob).unwrap();

        const EXPECTED: &[ExpectedEvent] = &[ExpectedEvent {
            event_id: 100,
            fields: &[
                ExpectedField::new("seq", 0, 4),
                ExpectedField::new("ack", 4, 4),
                ExpectedField::new("win", 8, 4),
                ExpectedField::new("len", 12, 4),
                ExpectedField::new("flags", 16, 2),
            ],
        }];
        let result = validate_hardcoded_decoders(&schema, EXPECTED);

        assert!(
            result.divergent.is_empty(),
            "additive fields must not be treated as divergence: {:?}",
            result.divergent
        );
        assert_eq!(result.new_fields.len(), 1);
        assert!(result.new_fields[0].contains("state"));
        assert!(result.new_fields[0].contains("extra"));
        assert!(result.new_fields[0].contains("2 undecoded fields"));
    }

    /// An `ExpectedEvent` for an id the schema doesn't define at all must
    /// be silently ignored (absence isn't evidence of drift) rather than
    /// treated as a missing-struct divergence.
    #[test]
    fn event_absent_from_schema_is_not_divergent() {
        let blob = build_blob_multi(
            "test",
            &[StructSpec {
                name: "S",
                fields: vec![FieldSpec::scalar("a", DECODE_TYPE_U32, 0)],
            }],
            &[EventSpec {
                id: 1,
                name: "S",
                struct_index: 1,
                format: "",
            }],
        );
        let schema = ProviderSchema::parse(&blob).unwrap();
        const EXPECTED: &[ExpectedEvent] = &[ExpectedEvent {
            event_id: 999, // not defined by this schema
            fields: &[ExpectedField::new("a", 0, 4)],
        }];
        let result = validate_hardcoded_decoders(&schema, EXPECTED);
        assert!(result.divergent.is_empty());
        assert!(result.new_fields.is_empty());
    }

    /// An optional field is neither drift when the schema lacks it nor
    /// an undecoded-field notice when the schema has it.
    #[test]
    fn optional_field_absent_or_present_is_clean() {
        const EXPECTED: &[ExpectedEvent] = &[ExpectedEvent {
            event_id: 1,
            fields: &[
                ExpectedField::new("a", 0, 4),
                ExpectedField::optional("b", 4, 4),
            ],
        }];
        for fields in [
            vec![FieldSpec::scalar("a", DECODE_TYPE_U32, 0)],
            vec![
                FieldSpec::scalar("a", DECODE_TYPE_U32, 0),
                FieldSpec::scalar("b", DECODE_TYPE_U32, 4),
            ],
        ] {
            let blob = build_blob_multi(
                "test",
                &[StructSpec { name: "S", fields }],
                &[EventSpec {
                    id: 1,
                    name: "S",
                    struct_index: 1,
                    format: "",
                }],
            );
            let schema = ProviderSchema::parse(&blob).unwrap();
            let result = validate_hardcoded_decoders(&schema, EXPECTED);
            assert!(
                result.divergent.is_empty(),
                "{:?}",
                result.divergent_reasons
            );
            assert!(result.new_fields.is_empty(), "{:?}", result.new_fields);
        }
    }

    #[test]
    fn tcp_has_hardcoded_decoder_matches_expected_table_coverage() {
        // Every event in TCP_EXPECTED must be one this codebase claims to
        // have a hardcoded decoder for -- otherwise the routing guard in
        // core.rs would treat it as schema-only even when it's really
        // divergent-checked.
        for exp in TCP_EXPECTED {
            assert!(
                tcp_has_hardcoded_decoder(exp.event_id),
                "TCP_EXPECTED has event {} but tcp_has_hardcoded_decoder doesn't",
                exp.event_id
            );
        }
    }

    #[test]
    fn hpts_wheel_ids_have_hardcoded_decoders() {
        for exp in HPTS_EXPECTED {
            assert!(
                hpts_has_hardcoded_decoder(exp.event_id),
                "HPTS_EXPECTED has event {} but hpts_has_hardcoded_decoder doesn't",
                exp.event_id
            );
        }
    }

    #[test]
    fn ella_relay_ids_have_hardcoded_decoders() {
        for exp in ELLA_RELAY_EXPECTED {
            assert!(
                ella_relay_has_hardcoded_decoder(exp.event_id),
                "ELLA_RELAY_EXPECTED has event {} but ella_relay_has_hardcoded_decoder doesn't",
                exp.event_id
            );
        }
    }
}
