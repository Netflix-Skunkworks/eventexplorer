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

/// List the interfaces the `if` provider attached, with their metadata.
pub fn cmd_interfaces(prof: &IfProfile) {
    if prof.is_empty() {
        println!("No 'if' provider data in this capture.");
        return;
    }
    let mut ifs: Vec<_> = prof.interfaces.values().collect();
    ifs.sort_by_key(|i| i.ifindex);
    println!(
        "=== Network Interfaces ({}, sample period {} us, nifs={}) ===",
        ifs.len(),
        prof.period_us,
        prof.nifs,
    );
    println!(
        "{:>6}  {:<12} {:<10} {:>6} {:>14} {:<8} {:<18} state",
        "ifidx", "name", "driver", "mtu", "baudrate", "link", "hwaddr"
    );
    println!("{}", "-".repeat(92));
    for i in ifs {
        let hw = if i.hwaddr.is_empty() {
            "-".to_string()
        } else {
            i.hwaddr
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":")
        };
        println!(
            "{:>6}  {:<12} {:<10} {:>6} {:>14} {:<8} {:<18} {}",
            i.ifindex,
            i.name,
            i.driver,
            i.mtu,
            i.baudrate,
            if_link_state_name(i.link_state),
            hw,
            if i.detached { "detached" } else { "attached" },
        );
    }
}

/// Dump per-interface counter samples, deriving per-period rates from the
/// cumulative counters (the kernel emits cumulative values by design).
///
/// `rx_Bps`/`tx_Bps` are the link-level totals. On captures with the
/// TCP/UDP/other split (`IF_SAMPLE_BYTES`/`IF_SAMPLE_PACKETS`, ids 21/22)
/// three more column pairs break that total down into `tcp_*`/`udp_*`/
/// `other_*`. On a schema new enough to carry it (NCD-10070),
/// `other_rx_Bps`/`other_tx_Bps` is a direct kernel counter, bumped at
/// the same IP-dispatch call site as tcp/udp -- NOT `rx_Bps -
/// tcp_rx_Bps - udp_rx_Bps`. `tcp+udp+other` is self-consistent by
/// construction but is NOT guaranteed to sum to `rx_Bps`/`tx_Bps`: on a
/// multi-port aggregate interface (lagg) those totals are a live sum of
/// member-port driver counters, asynchronous from the IP-dispatch split,
/// and the two are only expected to reconcile over multi-second windows.
/// On an older schema, `other_*` falls back to that same subtraction.
/// Older captures still (deprecated `IF_SAMPLE`, id 20) have no protocol
/// data at all, so tcp/udp read 0 and other reads the same as the total.
/// `oerrors`/`odrops` are the TX-side counters (`IFCOUNTER_OERRORS`/
/// `IFCOUNTER_OQDROPS`) -- `odrops` in particular is what a driver bumps
/// when it can't queue a packet for transmission because its ring is
/// full, i.e. the host is trying to send faster than the link can drain.
pub fn cmd_if_samples(
    prof: &IfProfile,
    capture_start: u64,
    ifindex: Option<u16>,
    window_s: Option<f64>,
) {
    if prof.samples.is_empty() {
        println!("No IF_SAMPLE events in this capture.");
        return;
    }
    // Group sample indices by ifindex (samples are globally ts-sorted).
    let mut by_if: std::collections::BTreeMap<u16, Vec<&IfSample>> =
        std::collections::BTreeMap::new();
    for s in &prof.samples {
        if let Some(f) = ifindex {
            if s.ifindex != f {
                continue;
            }
        }
        by_if.entry(s.ifindex).or_default().push(s);
    }
    if let Some(w) = window_s {
        return print_if_samples_windowed(prof, capture_start, by_if, w);
    }
    for (idx, samples) in by_if {
        let name = prof.name_for(idx);
        println!(
            "=== IF_SAMPLE for {name} (ifindex {idx}, {} samples) ===",
            samples.len()
        );
        println!(
            "{:>10}  {:>14} {:>14} {:>14} {:>14} {:>14} {:>14} {:>14} {:>14} {:>12} {:>12} {:>10} {:>10} {:>10} {:>10}",
            "t(s)",
            "rx_Bps",
            "tx_Bps",
            "tcp_rx_Bps",
            "tcp_tx_Bps",
            "udp_rx_Bps",
            "udp_tx_Bps",
            "other_rx_Bps",
            "other_tx_Bps",
            "rx_pps",
            "tx_pps",
            "ierrors",
            "idrops",
            "oerrors",
            "odrops"
        );
        println!("{}", "-".repeat(214));
        let mut prev: Option<&IfSample> = None;
        for s in samples {
            let t = (s.ts.saturating_sub(capture_start)) as f64 / 1e6;
            let rate = |cur: u64, prev_val: u64, dt: f64| -> f64 {
                cur.saturating_sub(prev_val) as f64 / dt
            };
            let (
                rxbps,
                txbps,
                tcp_rxbps,
                tcp_txbps,
                udp_rxbps,
                udp_txbps,
                other_rxbps,
                other_txbps,
                rxpps,
                txpps,
            ) = match prev {
                Some(p) => {
                    let dt = (s.ts.saturating_sub(p.ts)) as f64 / 1e6;
                    if dt > 0.0 {
                        (
                            rate(s.ibytes, p.ibytes, dt),
                            rate(s.obytes, p.obytes, dt),
                            rate(s.tcp_ibytes, p.tcp_ibytes, dt),
                            rate(s.tcp_obytes, p.tcp_obytes, dt),
                            rate(s.udp_ibytes, p.udp_ibytes, dt),
                            rate(s.udp_obytes, p.udp_obytes, dt),
                            rate(s.other_ibytes, p.other_ibytes, dt),
                            rate(s.other_obytes, p.other_obytes, dt),
                            rate(s.ipackets, p.ipackets, dt),
                            rate(s.opackets, p.opackets, dt),
                        )
                    } else {
                        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
                    }
                }
                None => (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
            };
            println!(
                "{:>10.3}  {:>14.0} {:>14.0} {:>14.0} {:>14.0} {:>14.0} {:>14.0} {:>14.0} {:>14.0} {:>12.0} {:>12.0} {:>10} {:>10} {:>10} {:>10}",
                t, rxbps, txbps, tcp_rxbps, tcp_txbps, udp_rxbps, udp_txbps, other_rxbps,
                other_txbps, rxpps, txpps, s.ierrors, s.iqdrops, s.oerrors, s.oqdrops
            );
            prev = Some(s);
        }
        println!();
    }
}

/// Same columns as `cmd_if_samples`, but rates are computed from the total
/// byte/packet delta across a whole `window_s`-second span (first sample at
/// or after each window boundary vs. the last one before the next), instead
/// of from consecutive raw samples. Some drivers only refresh their byte
/// counters roughly every other sample tick, which makes the raw per-tick
/// rate alternate between ~0 and ~2x the true rate (and can make it look
/// like line rate was exceeded, on a link or even a single physical member
/// port, when it wasn't) -- widening the window averages that artifact out.
fn print_if_samples_windowed(
    prof: &IfProfile,
    capture_start: u64,
    by_if: std::collections::BTreeMap<u16, Vec<&IfSample>>,
    window_s: f64,
) {
    for (idx, samples) in by_if {
        let name = prof.name_for(idx);
        println!(
            "=== IF_SAMPLE for {name} (ifindex {idx}, {} raw samples, {window_s:.3}s windows) ===",
            samples.len()
        );
        println!(
            "{:>10}  {:>14} {:>14} {:>14} {:>14} {:>14} {:>14} {:>14} {:>14} {:>12} {:>12} {:>10} {:>10} {:>10} {:>10}",
            "t(s)",
            "rx_Bps",
            "tx_Bps",
            "tcp_rx_Bps",
            "tcp_tx_Bps",
            "udp_rx_Bps",
            "udp_tx_Bps",
            "other_rx_Bps",
            "other_tx_Bps",
            "rx_pps",
            "tx_pps",
            "ierrors",
            "idrops",
            "oerrors",
            "odrops"
        );
        println!("{}", "-".repeat(214));
        let window_us = (window_s * 1e6) as u64;
        let mut window_start_idx = 0usize;
        while window_start_idx < samples.len() {
            let window_start_ts = samples[window_start_idx].ts;
            let mut end_idx = window_start_idx;
            while end_idx + 1 < samples.len()
                && samples[end_idx + 1].ts.saturating_sub(window_start_ts) < window_us
            {
                end_idx += 1;
            }
            if end_idx == window_start_idx {
                // Lone sample with nothing else in range -- nothing to diff against yet.
                window_start_idx += 1;
                continue;
            }
            let a = samples[window_start_idx];
            let b = samples[end_idx];
            let dt = b.ts.saturating_sub(a.ts) as f64 / 1e6;
            let t = (a.ts.saturating_sub(capture_start)) as f64 / 1e6;
            let rate = |cur: u64, prev_val: u64| -> f64 {
                if dt > 0.0 {
                    cur.saturating_sub(prev_val) as f64 / dt
                } else {
                    0.0
                }
            };
            println!(
                "{:>10.3}  {:>14.0} {:>14.0} {:>14.0} {:>14.0} {:>14.0} {:>14.0} {:>14.0} {:>14.0} {:>12.0} {:>12.0} {:>10} {:>10} {:>10} {:>10}",
                t,
                rate(b.ibytes, a.ibytes),
                rate(b.obytes, a.obytes),
                rate(b.tcp_ibytes, a.tcp_ibytes),
                rate(b.tcp_obytes, a.tcp_obytes),
                rate(b.udp_ibytes, a.udp_ibytes),
                rate(b.udp_obytes, a.udp_obytes),
                rate(b.other_ibytes, a.other_ibytes),
                rate(b.other_obytes, a.other_obytes),
                rate(b.ipackets, a.ipackets),
                rate(b.opackets, a.opackets),
                b.ierrors,
                b.iqdrops,
                b.oerrors,
                b.oqdrops,
            );
            window_start_idx = end_idx + 1;
        }
        println!();
    }
}

/// Dump the `if` provider's discrete lifecycle / link / config timeline.
pub fn cmd_if_events(prof: &IfProfile, capture_start: u64) {
    if prof.events.is_empty() {
        println!("No 'if' provider change events in this capture.");
        return;
    }
    println!(
        "=== if provider event timeline ({} events) ===",
        prof.events.len()
    );
    for e in &prof.events {
        let t = (e.ts.saturating_sub(capture_start)) as f64 / 1e6;
        let name = prof.name_for(e.ifindex);
        println!(
            "{:>10.3}s  {:<22} ifidx={:<4} {:<10} {}",
            t,
            if_event_name(e.event_id),
            e.ifindex,
            name,
            e.detail,
        );
    }
}

/// Dump per-stack TCP aggregate samples (PCB-state gauges, requests/sec,
/// goodput / send-size / LRO-flush histograms). Requests/sec is derived
/// from the cumulative TCP_RPS_SAMPLE counter.
pub fn cmd_tcp_samples(prof: &TcpSampleProfile, capture_start: u64) {
    if prof.is_empty() {
        println!("No TCP sample data in this capture.");
        return;
    }
    // Stack identity table. Keyed by (provider_id, session_id): every
    // "tcp" provider (one per loaded stack) reuses the same stack-global
    // session id, so the provider id is what tells the stacks apart.
    let mut stacks: Vec<_> = prof.stacks.iter().collect();
    stacks.sort_by_key(|(key, _)| **key);
    println!("=== TCP stacks ({}) ===", stacks.len());
    for ((pid, sid), info) in &stacks {
        println!(
            "  provider={:<3} session={:#018x}  stack={:<16} sample_period_us={}",
            pid, sid, info.stack_name, info.sample_period_us
        );
    }
    println!();

    let stack_label = |pid: u16, sid: u64| -> String { prof.stack_label(pid, sid) };

    if !prof.state_samples.is_empty() {
        println!(
            "=== TCP_SAMPLE state gauges ({} samples) ===",
            prof.state_samples.len()
        );
        // Columns follow the canonical TCP FSM order so every one of the
        // eleven kernel states is shown and `total` reconciles from them.
        println!(
            "{:>10}  {:<14} {:>7} {:>7} {:>8} {:>8} {:>7} {:>8} {:>7} {:>7} {:>8} {:>7} {:>7} {:>9}",
            "t(s)",
            "stack",
            "closed",
            "listen",
            "syn_sent",
            "syn_rcvd",
            "estab",
            "close_w",
            "fin_w1",
            "closing",
            "last_ack",
            "fin_w2",
            "time_w",
            "total"
        );
        println!("{}", "-".repeat(130));
        for s in &prof.state_samples {
            let t = (s.ts.saturating_sub(capture_start)) as f64 / 1e6;
            // TCP_SAMPLE gauges are the subsystem-wide aggregate (across
            // every stack and VNET), riding the aggregate session rather
            // than any one stack's; label those "(all)".
            let label = prof
                .stacks
                .get(&(s.provider_id, s.session_id))
                .map(|i| i.stack_name.clone())
                .unwrap_or_else(|| "(all)".to_string());
            println!(
                "{:>10.3}  {:<14} {:>7} {:>7} {:>8} {:>8} {:>7} {:>8} {:>7} {:>7} {:>8} {:>7} {:>7} {:>9}",
                t,
                label,
                s.closed,
                s.listen,
                s.syn_sent,
                s.syn_rcvd,
                s.estab,
                s.close_wait,
                s.fin_wait_1,
                s.closing,
                s.last_ack,
                s.fin_wait_2,
                s.time_wait,
                s.total()
            );
        }
        println!();
    }

    if !prof.rps_samples.is_empty() {
        println!(
            "=== TCP_RPS_SAMPLE (requests/sec, {} samples) ===",
            prof.rps_samples.len()
        );
        println!(
            "{:>10}  {:<14} {:>16} {:>12}",
            "t(s)", "stack", "cum_requests", "req/s"
        );
        println!("{}", "-".repeat(60));
        let mut prev: std::collections::HashMap<(u16, u64), &TcpRpsSample> =
            std::collections::HashMap::new();
        for s in &prof.rps_samples {
            let t = (s.ts.saturating_sub(capture_start)) as f64 / 1e6;
            // The stream interleaves a zero priming sample with the real
            // cumulative count at (almost) the same tick. Diff only against
            // the last *monotonic* sample: a count that drops (the reset)
            // is skipped without becoming the new baseline, so real
            // rates still compute across the zeros. A sub-ms gap can't
            // yield a meaningful rate, so it prints "-" but still advances.
            let key = (s.provider_id, s.session_id);
            let (rps, advance) = match prev.get(&key) {
                Some(p) if s.req_count >= p.req_count => {
                    let dt = (s.ts.saturating_sub(p.ts)) as f64 / 1e6;
                    if dt >= 1e-3 {
                        (
                            format!("{:.1}", (s.req_count - p.req_count) as f64 / dt),
                            true,
                        )
                    } else {
                        ("-".to_string(), true)
                    }
                }
                Some(_) => ("-".to_string(), false),
                None => ("-".to_string(), true),
            };
            println!(
                "{:>10.3}  {:<14} {:>16} {:>12}",
                t,
                stack_label(s.provider_id, s.session_id),
                s.req_count,
                rps
            );
            if advance {
                prev.insert(key, s);
            }
        }
        println!();
    }

    if !prof.gp_samples.is_empty() {
        println!(
            "=== TCP_GP_SAMPLE goodput histogram ({} samples) ===",
            prof.gp_samples.len()
        );
        for s in &prof.gp_samples {
            let t = (s.ts.saturating_sub(capture_start)) as f64 / 1e6;
            let buckets: Vec<String> = s.hist.iter().map(|v| v.to_string()).collect();
            println!(
                "{:>10.3}s  {:<14} [{}]",
                t,
                stack_label(s.provider_id, s.session_id),
                buckets.join(", ")
            );
        }
        println!();
    }

    if !prof.pacing_rate_samples.is_empty() {
        println!(
            "=== TCP_PACING_RATE_SAMPLE commanded-rate histogram ({} samples) ===",
            prof.pacing_rate_samples.len()
        );
        for s in &prof.pacing_rate_samples {
            let t = (s.ts.saturating_sub(capture_start)) as f64 / 1e6;
            let buckets: Vec<String> = s.hist.iter().map(|v| v.to_string()).collect();
            println!(
                "{:>10.3}s  {:<14} [{}]",
                t,
                stack_label(s.provider_id, s.session_id),
                buckets.join(", ")
            );
        }
        println!();
    }

    if !prof.realized_rate_samples.is_empty() {
        println!(
            "=== TCP_REALIZED_RATE_SAMPLE wall-clock-rate histogram ({} samples) ===",
            prof.realized_rate_samples.len()
        );
        for s in &prof.realized_rate_samples {
            let t = (s.ts.saturating_sub(capture_start)) as f64 / 1e6;
            let buckets: Vec<String> = s.hist.iter().map(|v| v.to_string()).collect();
            println!(
                "{:>10.3}s  {:<14} [{}]",
                t,
                stack_label(s.provider_id, s.session_id),
                buckets.join(", ")
            );
        }
        println!();
    }

    if !prof.send_samples.is_empty() {
        println!(
            "=== TCP_SEND_SAMPLE send-size histogram ({} samples) ===",
            prof.send_samples.len()
        );
        for s in &prof.send_samples {
            let t = (s.ts.saturating_sub(capture_start)) as f64 / 1e6;
            let buckets: Vec<String> = s.hist.iter().map(|v| v.to_string()).collect();
            println!(
                "{:>10.3}s  {:<14} [{}]",
                t,
                stack_label(s.provider_id, s.session_id),
                buckets.join(", ")
            );
        }
        println!();
    }

    if !prof.srtt_samples.is_empty() {
        println!(
            "=== TCP_SRTT_SAMPLE smoothed RTT histogram ({} samples) ===",
            prof.srtt_samples.len()
        );
        for s in &prof.srtt_samples {
            let t = (s.ts.saturating_sub(capture_start)) as f64 / 1e6;
            let buckets: Vec<String> = s.hist.iter().map(|v| v.to_string()).collect();
            println!(
                "{:>10.3}s  {:<14} [{}]",
                t,
                stack_label(s.provider_id, s.session_id),
                buckets.join(", ")
            );
        }
        println!();
    }

    if !prof.lro_flush_samples.is_empty() {
        println!(
            "=== TCP_LRO_FLUSH_SAMPLE LRO flush-size histogram ({} samples) ===",
            prof.lro_flush_samples.len()
        );
        for s in &prof.lro_flush_samples {
            let t = (s.ts.saturating_sub(capture_start)) as f64 / 1e6;
            let buckets: Vec<String> = s.hist.iter().map(|v| v.to_string()).collect();
            println!("{:>10.3}s  {:<14} [{}]", t, "(all)", buckets.join(", "));
        }
        println!();
    }
}
