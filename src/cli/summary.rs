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

pub fn cmd_stats(sessions: &Sessions) {
    use std::collections::HashMap;
    let mut by_type: HashMap<&str, usize> = HashMap::new();
    let mut total = 0usize;
    for events in sessions.values() {
        for e in events {
            let name = match &e.payload {
                EventPayload::ConnSetIpV4 { .. } => "ConnSetIpV4",
                EventPayload::ConnSetIpV6 { .. } => "ConnSetIpV6",
                EventPayload::LogId { .. } => "LogId",
                EventPayload::In { .. } => "In",
                EventPayload::Out { .. } => "Out",
                EventPayload::Ack { .. } => "Ack",
                EventPayload::Cwnd { .. } => "Cwnd",
                EventPayload::EnterRecovery { .. } => "EnterRecovery",
                EventPayload::ExitRecovery { .. } => "ExitRecovery",
                EventPayload::Goodput { .. } => "Goodput",
                EventPayload::GpCheck { .. } => "GpCheck",
                EventPayload::Rtt { .. } => "Rtt",
                EventPayload::PacingCalc { .. } => "PacingCalc",
                EventPayload::RttUnderflow { .. } => "RttUnderflow",
                EventPayload::PaceState { .. } => "PaceState",
                EventPayload::StarsPace { .. } => "StarsPace",
                EventPayload::SessionCreate { .. } => "SessionCreate",
                EventPayload::ConnOwner { .. } => "ConnOwner",
                EventPayload::ConnParamsV2 { .. } => "ConnParamsV2",
                EventPayload::TcpStackChange { .. } => "TcpStackChange",
                EventPayload::CcAlgoChange { .. } => "CcAlgoChange",
                EventPayload::UserSend { .. } => "UserSend",
                EventPayload::SendFile { .. } => "SendFile",
                EventPayload::Dsack { .. } => "Dsack",
                EventPayload::HttpReqNew { .. } => "HttpReqNew",
                EventPayload::HttpReqFreed { .. } => "HttpReqFreed",
                EventPayload::HttpReqStale { .. } => "HttpReqStale",
                EventPayload::HttpReqAllocfail { .. } => "HttpReqAllocfail",
                EventPayload::HttpReqForcefree { .. } => "HttpReqForcefree",
                EventPayload::Rto { .. } => "Rto",
                EventPayload::WindowCollapse { .. } => "WindowCollapse",
                EventPayload::Other => "Other",
            };
            *by_type.entry(name).or_insert(0) += 1;
            total += 1;
        }
    }
    let event_size = std::mem::size_of::<Event>();
    println!("=== Event Type Distribution ({total} events, {event_size} bytes/event) ===");
    let mut sorted: Vec<_> = by_type.into_iter().collect();
    // Sort by count desc, then by name for a stable order on ties (the
    // source map is a std HashMap, so a count-only sort would otherwise
    // vary run to run).
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    println!("{:>20}  {:>10}  {:>6}  {:>10}", "type", "count", "%", "MB");
    println!("{}", "-".repeat(52));
    for (name, count) in &sorted {
        let pct = *count as f64 / total as f64 * 100.0;
        let mb = *count as f64 * event_size as f64 / 1e6;
        println!("{:>20}  {:>10}  {:>5.1}%  {:>8.1}", name, count, pct, mb);
    }
    let plot_types = [
        "Rtt",
        "Goodput",
        "PacingCalc",
        "Cwnd",
        "Ack",
        "EnterRecovery",
        "ExitRecovery",
    ];
    let plot_count: usize = sorted
        .iter()
        .filter(|(name, _)| plot_types.contains(name))
        .map(|(_, count)| count)
        .sum();
    let plot_mb = plot_count as f64 * event_size as f64 / 1e6;
    println!("{}", "-".repeat(52));
    println!(
        "{:>20}  {:>10}  {:>5.1}%  {:>8.1}",
        "PLOT-ESSENTIAL",
        plot_count,
        plot_count as f64 / total as f64 * 100.0,
        plot_mb
    );
    println!(
        "{:>20}  {:>10}  {:>5.1}%  {:>8.1}",
        "DISCARDABLE",
        total - plot_count,
        (total - plot_count) as f64 / total as f64 * 100.0,
        (total - plot_count) as f64 * event_size as f64 / 1e6
    );
}

#[cfg(not(target_os = "macos"))]
pub fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<f64>().ok())
        })
        .unwrap_or(0.0)
        / 1024.0
}

#[cfg(target_os = "macos")]
pub fn rss_mb() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v0>::uninit();
    // SAFETY: the buffer has the layout and size required by RUSAGE_INFO_V0.
    // proc_pid_rusage writes it synchronously; only read it on success.
    let result = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as libc::pid_t,
            libc::RUSAGE_INFO_V0,
            usage.as_mut_ptr().cast(),
        )
    };
    if result == 0 {
        // SAFETY: the successful call initialized the complete structure.
        unsafe { usage.assume_init() }.ri_resident_size as f64 / (1024.0 * 1024.0)
    } else {
        0.0
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    #[test]
    fn reports_current_process_memory() {
        assert!(super::rss_mb() > 0.0);
    }
}

pub fn cmd_memtest(mut sessions: Sessions) {
    let event_size = std::mem::size_of::<Event>() as f64;
    let event_count: usize = sessions.values().map(|v| v.len()).sum();
    eprintln!(
        "After parse:    {:>7.1} MB RSS,  {:>10} events ({:.0} MB event data)",
        rss_mb(),
        event_count,
        event_count as f64 * event_size / 1e6
    );

    let _summaries = session_summaries(&sessions);
    eprintln!("After summary:  {:>7.1} MB RSS", rss_mb());

    let _aux = retain_plot_events(&mut sessions);
    let event_count: usize = sessions.values().map(|v| v.len()).sum();
    eprintln!(
        "After retain:   {:>7.1} MB RSS,  {:>10} events ({:.0} MB event data)",
        rss_mb(),
        event_count,
        event_count as f64 * event_size / 1e6
    );
}

pub fn cmd_summary(sessions: &Sessions, with_limiter: bool) {
    let summaries = session_summaries(sessions);
    println!("=== Session Summaries ({} sessions) ===", summaries.len());
    if !with_limiter {
        println!(
            "{:>18}  {:>22}  {:>8}  {:>12}  {:>12}  {:>12}  {:>7}  {:>12}",
            "session_id",
            "remote",
            "events",
            "duration",
            "bytes_ack",
            "avg_rtt",
            "goodput",
            "avg_pacing"
        );
        println!("{}", "-".repeat(120));
        for s in &summaries {
            println!(
                "{:#18x}  {:>22}  {:>8}  {:>10.1}s  {:>12}  {:>9.1} ms  {:>5.1} Mb/s  {:>8.1} Mb/s",
                s.session_id,
                if s.remote.is_empty() {
                    "(unknown)".to_string()
                } else {
                    s.remote.clone()
                },
                s.event_count,
                s.duration_sec,
                s.total_bytes_acked,
                s.avg_srtt_ms,
                s.avg_goodput_mbps,
                s.avg_pacing_rate_mbps,
            );
        }
        return;
    }

    println!(
        "{:>18}  {:>22}  {:>8}  {:>10}  {:>10}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}  {:>6}",
        "session_id",
        "remote",
        "events",
        "duration",
        "bytes_ack",
        "goodput",
        "%pacing",
        "%cwnd",
        "%rwnd",
        "%app",
        "%idle",
        "retx"
    );
    println!("{}", "-".repeat(138));
    let mut any_unscaled = false;
    for s in &summaries {
        let events = sessions.get(&s.session_id);
        let (summary, retx_count, unscaled) = match events {
            Some(evs) => {
                let data = extract_overview_data(evs, None, None);
                let retx: usize = evs
                    .iter()
                    .filter(|e| match &e.payload {
                        EventPayload::Out { flags, len, .. } => {
                            *len > 0 && (flags & TCP_FLAG_RETRANSMIT) != 0
                        }
                        _ => false,
                    })
                    .count();
                (data.limiter_summary, retx, data.peer_rwnd_unscaled)
            }
            None => (LimiterSummary::default(), 0, true),
        };
        if unscaled {
            any_unscaled = true;
        }
        // Pre-CONN_PARAMS_V2 captures still report blank %rwnd
        // because the classifier can't run without snd_wscale.
        let rwnd_col = if unscaled {
            "n/a".to_string()
        } else {
            format!("{:.1}%", summary.rwnd_pct)
        };
        println!(
            "{:#18x}  {:>22}  {:>8}  {:>8.1}s  {:>10}  {:>4.1} Mb/s  {:>6.1}%  {:>6.1}%  {:>7}  {:>6.1}%  {:>6.1}%  {:>6}",
            s.session_id,
            if s.remote.is_empty() { "(unknown)".to_string() } else { s.remote.clone() },
            s.event_count,
            s.duration_sec,
            s.total_bytes_acked,
            s.avg_goodput_mbps,
            summary.pacing_pct,
            summary.cwnd_pct,
            rwnd_col,
            summary.app_pct,
            summary.idle_pct + summary.unknown_pct,
            retx_count,
        );
    }
    if any_unscaled {
        println!();
        println!(
            "  note: %rwnd is reported as n/a for sessions captured by older \
             kernels that don't emit CONN_PARAMS_V2 (no snd_wscale, so the \
             peer-advertised RWND can't be reconstructed)."
        );
    }
}

pub fn cmd_limiter(sessions: &Sessions, filter: Option<u64>) {
    let mut session_list: Vec<_> = sessions.iter().collect();
    session_list.sort_by_key(|(sid, _)| *sid);

    for (&sid, events) in &session_list {
        if let Some(f) = filter {
            if sid != f {
                continue;
            }
        }
        if events.is_empty() {
            continue;
        }

        let data = extract_overview_data(events, None, None);
        let summary = data.limiter_summary;

        println!("=== Limiter summary for session {sid:#x} ===");
        if let Some(ref cp) = data.conn_params {
            let cc = if cp.cc_algo.is_empty() {
                "?".to_string()
            } else {
                cp.cc_algo.clone()
            };
            let stack_str = if cp.tcp_stack.is_empty() {
                String::new()
            } else {
                format!("  stack={}", cp.tcp_stack)
            };
            println!(
                "  cc={cc}{stack_str}  snd_wscale={}  rcv_wscale={}  opts={}",
                cp.snd_wscale,
                cp.rcv_wscale,
                format_conn_params_flags(cp.flags),
            );
        }
        if summary.is_empty() {
            println!("  (not enough signal to classify)");
            println!();
            continue;
        }
        if data.peer_rwnd_unscaled {
            println!(
                "  pacing={:.1}%  cwnd={:.1}%  rwnd=n/a  app={:.1}%  unknown={:.1}%  idle={:.1}%",
                summary.pacing_pct,
                summary.cwnd_pct,
                summary.app_pct,
                summary.unknown_pct,
                summary.idle_pct,
            );
            println!(
                "    (rwnd classification is disabled — this capture predates \
                 CONN_PARAMS_V2, so peer-advertised RWND cannot be reconstructed)"
            );
        } else {
            println!(
                "  pacing={:.1}%  cwnd={:.1}%  rwnd={:.1}%  app={:.1}%  unknown={:.1}%  idle={:.1}%",
                summary.pacing_pct, summary.cwnd_pct, summary.rwnd_pct,
                summary.app_pct, summary.unknown_pct, summary.idle_pct,
            );
        }
        println!("  compact: {}", summary.compact());
        println!();
        println!(
            "  {} spans (RLE, bucket = {}ms)",
            data.limiter_spans.len(),
            LIMITER_BUCKET_US / 1000
        );
        println!(
            "    {:>10}  {:>10}  {:>10}  {:>8}",
            "t_start_s", "t_end_s", "dur_s", "class"
        );
        println!("    {}", "-".repeat(44));
        for span in &data.limiter_spans {
            println!(
                "    {:>10.3}  {:>10.3}  {:>10.3}  {:>8}",
                span.t_start,
                span.t_end,
                span.t_end - span.t_start,
                span.class.short(),
            );
        }
        println!();

        if !data.idle_spans.is_empty() {
            println!(
                "  {} idle spans (gap > {}ms)",
                data.idle_spans.len(),
                IDLE_GAP_US / 1000
            );
            let mut total_idle = 0.0;
            for s in &data.idle_spans {
                total_idle += s[1] - s[0];
            }
            println!("    total idle time: {:.3}s", total_idle);
            println!();
        }

        let rto = data.rto_markers.len();
        let wc = data.window_collapse_markers.len();
        let dsack = data.dsack_markers.len();
        if rto + wc + dsack > 0 {
            println!("  markers: RTO={rto}  WindowCollapse={wc}  DSACK={dsack}");
            println!();
        }
    }
}
