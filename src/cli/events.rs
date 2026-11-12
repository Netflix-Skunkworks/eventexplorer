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

/// `since`/`until` are seconds relative to `capture_start` -- the same
/// convention `starspace`/`starsagg`/`httpreqs`/`hptslateness` all use --
/// so a window found via one of those commands (e.g. a stalled request's
/// `ts_s` from `httpreqs --raw`) can be pasted straight in here to see
/// what else was happening on that session at the same moment.
pub fn cmd_events(
    sessions: &Sessions,
    capture_start: u64,
    filter: Option<u64>,
    since: Option<f64>,
    until: Option<f64>,
) {
    let mut session_list: Vec<_> = sessions.iter().collect();
    session_list.sort_by_key(|(sid, _)| *sid);

    for (&sid, events) in &session_list {
        if let Some(f) = filter {
            if sid != f {
                continue;
            }
        }
        let in_window = |ts: u64| -> bool {
            let t = ts.saturating_sub(capture_start) as f64 / 1e6;
            since.is_none_or(|lo| t >= lo) && until.is_none_or(|hi| t <= hi)
        };
        let shown: Vec<&Event> = events.iter().filter(|e| in_window(e.ts)).collect();
        if shown.is_empty() {
            continue;
        }

        println!(
            "=== Events for session {sid:#x} ({} of {} events shown) ===",
            shown.len(),
            events.len()
        );
        for e in shown {
            let t = e.ts.saturating_sub(capture_start) as f64 / 1_000_000.0;
            let name = event_name(e.event_id);
            let detail: String = match &e.payload {
                EventPayload::Dsack {
                    dsack_round_seen,
                    dsack_round_end,
                    num_dsack,
                    dsack_persist,
                    round_mod,
                    round,
                } => {
                    format!(
                        "round={round} num_dsack={num_dsack} persist={dsack_persist} \
                         (round_seen={dsack_round_seen} round_end={dsack_round_end} mod={round_mod})"
                    )
                }
                EventPayload::Rto {
                    timeout_num,
                    min_rtt,
                    rtt,
                    segment_len,
                    inflight,
                    pkts_out,
                } => {
                    format!(
                        "timeout#{timeout_num} rtt={rtt}us min_rtt={min_rtt}us \
                         seg={segment_len}B inflight={inflight}B pkts_out={pkts_out}"
                    )
                }
                EventPayload::WindowCollapse {
                    count,
                    direction,
                    inflight,
                    has_collapsed,
                } => {
                    format!(
                        "{count} segments, direction={} inflight={inflight}B has_collapsed={has_collapsed}",
                        collapse_dir_name(*direction)
                    )
                }
                EventPayload::ConnParamsV2 {
                    iss,
                    irs,
                    mss,
                    snd_wscale,
                    rcv_wscale,
                    flags,
                    cc_algo,
                    tcp_stack,
                } => {
                    let cc = if cc_algo.is_empty() {
                        "(unset)"
                    } else {
                        cc_algo.as_str()
                    };
                    let stack = if tcp_stack.is_empty() {
                        "(unset)"
                    } else {
                        tcp_stack.as_str()
                    };
                    format!(
                        "iss={iss} irs={irs} mss={mss} snd_wscale={snd_wscale} \
                         rcv_wscale={rcv_wscale} flags=0x{flags:02x} ({}) \
                         cc_algo={cc} tcp_stack={stack}",
                        format_conn_params_flags(*flags),
                    )
                }
                EventPayload::ConnOwner { pid } => format!("pid={pid}"),
                EventPayload::TcpStackChange { tcp_stack } => {
                    format!("tcp_stack={tcp_stack}")
                }
                EventPayload::CcAlgoChange { cc_algo } => {
                    format!("cc_algo={cc_algo}")
                }
                EventPayload::UserSend {
                    error,
                    flags,
                    state,
                    sndbuf_used,
                    sndbuf_avail,
                    send_len,
                } => {
                    format!(
                        "error={error} flags=0x{flags:08x} state={state} \
                         sndbuf_used={sndbuf_used} sndbuf_avail={sndbuf_avail} \
                         send_len={send_len}",
                    )
                }
                EventPayload::SendFile {
                    offset,
                    length,
                    flags,
                } => {
                    format!("offset={offset} length={length} flags=0x{flags:08x}")
                }
                _ => format!("{:?}", e.payload),
            };
            println!(
                "{:>10.3}s  cpu={:<3} tid={:<6} {:16} {}",
                t, e.cpu, e.tid, name, detail
            );
        }
        println!();
    }
}

pub fn cmd_hexdump(filepath: &str, offset: usize, count: usize) {
    use std::fs;
    let data = match fs::read(filepath) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error reading {filepath}: {e}");
            return;
        }
    };
    let end = (offset + count).min(data.len());
    if offset >= data.len() {
        eprintln!("Offset {offset} is past end of file ({})", data.len());
        return;
    }
    println!(
        "=== Hex dump: offset {offset:#x} ({offset}), {} bytes ===",
        end - offset
    );
    for row_start in (offset..end).step_by(16) {
        print!("{row_start:08x}  ");
        let row_end = (row_start + 16).min(end);
        for col in 0..16 {
            let i = row_start + col;
            if i < row_end {
                print!("{:02x} ", data[i]);
            } else {
                print!("   ");
            }
            if col == 7 {
                print!(" ");
            }
        }
        print!(" |");
        for &b in &data[row_start..row_end] {
            if b.is_ascii_graphic() || b == b' ' {
                print!("{}", b as char);
            } else {
                print!(".");
            }
        }
        println!("|");
    }
}
