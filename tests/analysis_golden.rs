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

//! Golden snapshots of the higher-level analysis/compute functions the GUI
//! drives, called directly through the library API (no CLI formatting in the
//! way). Complements `cli_golden.rs` by pinning the structured numeric output
//! of the rate/goodput/cpu analytics.

mod support;

use event_explorer::analysis::*;
use event_explorer::elog_parser::*;
use std::fmt::Write as _;
use support::{check_golden, synthetic_elog, SESSION_A};

fn parse() -> (Sessions, CpuProfile) {
    let bytes = synthetic_elog();
    let progress = LoadProgress::default();
    let (sessions, _nic, _pace, cpu, _if, _tcp, _ella, _nginx, _file, _providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse synthetic fixture");
    (sessions, cpu)
}

fn fmt_pairs(pairs: &[(f64, f64)]) -> String {
    let mut s = String::new();
    for (x, y) in pairs {
        writeln!(s, "{x:.3}\t{y:.3}").unwrap();
    }
    s
}

#[test]
fn session_summaries_golden() {
    let (sessions, _cpu) = parse();
    let summaries = session_summaries(&sessions);
    let mut s = String::new();
    for sum in &summaries {
        writeln!(
            s,
            "session={:#x} remote={} log_id={:?}",
            sum.session_id, sum.remote, sum.log_id
        )
        .unwrap();
        writeln!(
            s,
            "  events={} duration_sec={:.6}",
            sum.event_count, sum.duration_sec
        )
        .unwrap();
        writeln!(
            s,
            "  bytes_sent={} bytes_acked={}",
            sum.total_bytes_sent, sum.total_bytes_acked
        )
        .unwrap();
        writeln!(
            s,
            "  avg_goodput_mbps={:.6} avg_pacing_mbps={:.6} avg_kernel_goodput_mbps={:.6}",
            sum.avg_goodput_mbps, sum.avg_pacing_rate_mbps, sum.avg_kernel_goodput_mbps
        )
        .unwrap();
        writeln!(
            s,
            "  avg_srtt_ms={:.6} min_rtt_ms={:.6}",
            sum.avg_srtt_ms, sum.min_rtt_ms
        )
        .unwrap();
        writeln!(
            s,
            "  recovery_count={} owner_pid={:?}",
            sum.recovery_count, sum.owner_pid
        )
        .unwrap();
    }
    check_golden("analysis/session_summaries.txt", &s);
}

#[test]
fn send_ack_rate_golden() {
    let (sessions, _cpu) = parse();
    let events = sessions.get(&SESSION_A).expect("session A present");
    let t0 = events.first().map(|e| e.ts).unwrap_or(0);
    let send = compute_send_rate(events, 100_000, 50_000, t0);
    let ack = compute_ack_rate(events, 100_000, 50_000, t0);
    let kgp = compute_kernel_goodput(events, 500_000, t0, 50_000, 60_000_000);
    let mut s = String::new();
    s.push_str("== send_rate ==\n");
    s.push_str(&fmt_pairs(&send));
    s.push_str("== ack_rate ==\n");
    s.push_str(&fmt_pairs(&ack));
    s.push_str("== kernel_goodput ==\n");
    s.push_str(&fmt_pairs(&kgp));
    check_golden("analysis/send_ack_rate.txt", &s);
}

#[test]
fn cpu_usage_bins_golden() {
    let (_sessions, cpu) = parse();
    let hz = cpu.kernel.as_ref().map(|k| k.sample_hz());
    let rated = cpu.kernel.as_ref().and_then(|k| k.rated_freq_mhz());
    let bins = compute_cpu_usage_bins(&cpu.samples, 8, hz, rated);
    check_golden("analysis/cpu_usage_bins.txt", &format!("{bins:#?}\n"));
}
