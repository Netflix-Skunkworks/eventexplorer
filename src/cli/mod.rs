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

mod summary;
pub use summary::*;
mod rtt;
pub use rtt::*;
mod goodput;
pub use goodput::*;
mod events;
pub use events::*;
mod interfaces;
pub use interfaces::*;
mod flame;
pub use flame::*;
mod stars;
pub use stars::*;
mod rate_collapse;
mod stars_pace;
pub use rate_collapse::*;
pub use stars_pace::*;
mod percore;
pub use percore::*;
mod hpts;
pub use hpts::*;
mod httpreqs;
pub use httpreqs::*;
mod ella;
pub use ella::*;
mod nginx;
pub use nginx::*;
mod mem;
pub use mem::*;
mod site;
pub use site::*;
mod origin;
pub use origin::*;
mod sizes;
pub use sizes::*;
mod upload;
pub use upload::*;

pub(crate) use crate::analysis::*;
pub(crate) use crate::elog_parser::*;
pub(crate) use std::env;
pub(crate) use std::process;

fn print_usage() {
    eprintln!("Usage: elog_cli <file.elog[.gz]>... [command]");
    eprintln!("       elog_cli site show                 Effective site config and its sources");
    eprintln!("       elog_cli site install <file|url>   Install a site file as the user config");
    eprintln!();
    eprintln!("Several files are treated as one continuous capture, so a rolled set");
    eprintln!("can be analyzed in one pass: `elog_cli capture-*.elog summary`. They may");
    eprintln!("be given in any order but must come from the same elog run.");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  summary [--limiter]  Session summaries (default).  With --limiter, appends");
    eprintln!("                       %pacing/%cwnd/%rwnd/%app/%idle/retx_count columns.");
    eprintln!("  limiter <session>    Print per-session limiter summary and timeline");
    eprintln!("  stats                Event type distribution and memory breakdown");
    eprintln!("  sizes [--fields N]   On-disk bytes per (provider, event); --fields N adds");
    eprintln!("                       a per-field byte breakdown for the top N shapes");
    eprintln!("  memtest              Measure RSS at each loading phase");
    eprintln!("  rtt [session_hex]    Dump raw RTT events (all or one session)");
    eprintln!("  rttanalysis          Analyze divergence of srtt / gp_srtt / path_rtt");
    eprintln!("  underflow            Analyze RTT_UNDERFLOW diagnostic events");
    eprintln!("  goodput [session_hex] Dump goodput events with span analysis");
    eprintln!("  ackdump <session_hex> <start_s> <end_s>  Dump ACK/goodput events in time range");
    eprintln!("  gpsim <session_hex> [start_s] [end_s]   Simulate goodput window state per ACK");
    eprintln!("  ackgaps             Wall-clock vs NIC inter-ACK gaps by SRTT (idle-cutoff check)");
    eprintln!("  gpcheck <session_hex> [start_s] [end_s] Dump GP_CHECK diagnostic events");
    eprintln!("  events [session_hex] [--since s] [--until s]  Dump events for a session");
    eprintln!("                       (or all sessions). --since/--until are capture-start-");
    eprintln!("                       relative seconds, same convention as starspace/httpreqs,");
    eprintln!("                       so a stalled request's ts_s can be pasted straight in.");
    eprintln!("  interfaces           List interfaces seen by the 'if' provider");
    eprintln!(
        "  ifsamples [ifindex] [--window-s N]  Dump per-interface counter samples + derived rates"
    );
    eprintln!("                       --window-s N: aggregate over N-second windows instead of");
    eprintln!("                       raw per-tick deltas (some drivers refresh byte counters");
    eprintln!("                       only every other tick, making raw per-tick rates alternate");
    eprintln!("                       between ~0 and ~2x true rate)");
    eprintln!("  ifevents             Dump the 'if' provider lifecycle/link/config timeline");
    eprintln!("  tcpsamples           Dump per-stack TCP aggregate samples (state/rps/goodput)");
    eprintln!("  ella                 List ELLA channel sessions (`fanout_node`)");
    eprintln!("  ellastats            Dump CHANNEL_COUNT / CHANNEL_COUNT_TOTAL samples");
    eprintln!("  ellaevents [session] Dump ELLA channel-layer events (all, or one session)");
    eprintln!("  nginx                List nginx HTTP requests (session/request/method/status)");
    eprintln!("  nginxstats           Dump nginx completion Gb/s and status mix (250 bins)");
    eprintln!("  nginxevents [session] Dump nginx lifecycle events (all, or one session)");
    eprintln!("  nginxconns           List nginx connections (close reason, TLS, disk counters)");
    eprintln!("  nginxdiag            Summarize nginx TLS, cache, upstream, disk and stall events");
    eprintln!("  mem                  Dump mem provider MEM_INFO and SYS_SAMPLE page states (GiB)");
    eprintln!("  upload [--cache NAME] [--note TEXT] [--open]  Upload the capture to");
    eprintln!("                       the site's share server (presigned PUT) and print a");
    eprintln!("                       shareable EventExplorer link. The cache name defaults");
    eprintln!("                       to the appliance-style prefix of the file name");
    eprintln!("                       (cNNN.siteNNN.dev-*); --open opens the link in a");
    eprintln!("                       browser. Needs uploads_url (docs/site-config.md).");
    eprintln!("  starsagg             Dump STARS_AGG_SAMPLE timeline (pressure/tokens/slot-gate");
    eprintln!("                       budget+peak-burst/denies+admits) plus a required-vs-");
    eprintln!("                       configured slot-gate-budget headroom check");
    eprintln!("  starspace [--min-samples N] [--since S] [--until S]  Aggregate per-connection");
    eprintln!("                       STARS_PACE events (natural/pre-squeeze vs scaled/post-");
    eprintln!("                       squeeze rate + the pressure in effect) into a natural-rate-");
    eprintln!(
        "                       tier -> resulting-scaled-rate-tier breakdown, split into the"
    );
    eprintln!(
        "                       synthetic no-rate-estimate placeholder (natural_bps == 1<<24)"
    );
    eprintln!("                       vs real goodput-based estimates -- the direct, ground-truth");
    eprintln!("                       answer to whether pressure actually squeezes high-natural-");
    eprintln!("                       rate flows toward the fair-share tier, no formula replay or");
    eprintln!("                       synthetic natural_bps assumption needed (unlike");
    eprintln!("                       pressure-squeeze-predict in elog.py, which predates real");
    eprintln!("                       per-connection STARS_PACE capture support). --min-samples N");
    eprintln!("                       (default 20) hides natural-rate tiers with too few samples");
    eprintln!("                       to be a meaningful distribution. --since/--until (seconds");
    eprintln!("                       since capture start, same convention as starsagg's t(s))");
    eprintln!("                       isolate a specific window, e.g. just the pressure \"hold\"");
    eprintln!("                       plateau of one overload episode. --raw dumps one TSV line");
    eprintln!(
        "                       per sample (ts_s/natural_bps/scaled_bps/pressure/session_id/"
    );
    eprintln!("                       len) instead of the aggregated breakdown, for offline");
    eprintln!("                       what-if analysis.");
    eprintln!(
        "  ratecollapse [--threshold-bps N] [--min-duration-s F] [--since S] [--until S] [--raw]"
    );
    eprintln!("                       Finds per-session runs of consecutive STARS_PACE samples");
    eprintln!(
        "                       where natural_bps (PRE-squeeze -- not the STARS controller's"
    );
    eprintln!("                       doing) stays below --threshold-bps (default 500000 B/s) for");
    eprintln!("                       at least --min-duration-s (default 1.0s). This is the");
    eprintln!("                       signature of rack_get_lt_bw() being substituted in after an");
    eprintln!("                       RTO collapses cwnd, then staying stuck (idle-diluted lt-bw)");
    eprintln!("                       until a fresh rate measurement completes -- i.e. a real,");
    eprintln!("                       enforced stall with no bandwidth/CPU limit behind it. --raw");
    eprintln!("                       dumps one line per episode instead of the summary.");
    eprintln!("  httpreqs [--bin-s N] [--raw]  Per-connection HTTP response delivery rate,");
    eprintln!("                       computed directly from HTTP_REQ_FREED's own byte range");
    eprintln!("                       and first_send/last_ack_us timestamps -- ground truth for");
    eprintln!("                       whether responses are actually being delivered slower, not");
    eprintln!("                       an aggregate CPU/HPTS-volume proxy. --bin-s (default 60)");
    eprintln!("                       sets the time-binned breakdown's bucket width in seconds.");
    eprintln!("                       --raw dumps one TSV line per completed request instead.");
    eprintln!("                       --min-bytes N filters out small (manifest/heartbeat-sized)");
    eprintln!("                       requests so the breakdown reflects real segment fetches.");
    eprintln!("                       --stall-report [secs] (default 5) reports whether slow");
    eprintln!("                       (>= secs) requests are spread across many sessions or");
    eprintln!("                       concentrated onto a repeat-offender few.");
    eprintln!("  starsdiag [flame opts]  interfaces + starsagg + flame in one parse pass --");
    eprintln!("                       use instead of 3 separate invocations on large captures");
    eprintln!("                       (each invocation loads the whole file into memory)");
    eprintln!("  hptsevents [filter]  Dump the hpts timing-wheel scheduler trace (ids 0-14,");
    eprintln!("                       v2 captures only); optional filter matches one event");
    eprintln!("                       name (e.g. `PERF_LATENESS`), case-insensitive");
    eprintln!("  hptslateness [--start-s S] [--end-s S]  Summarize PERF_LATENESS (hpts callout");
    eprintln!("                       delay) events, plus the periodic per-CPU");
    eprintln!("                       LATENESS_SAMPLE/TP_BATCH_SAMPLE hists. --start-s/--end-s");
    eprintln!("                       (trace-relative seconds) restrict both to a window, so a");
    eprintln!("                       short bad episode isn't diluted by a long healthy capture");
    eprintln!("  hptsgaps [exclude_last_secs]  Cross-check PROCESSING slot continuity for");
    eprintln!("                       silently-skipped wheel slots");
    eprintln!("  percore [--filter S] [--timeline [N]] [--freq-scale] [--start-s S] [--end-s S]  Per-CPU busy%");
    eprintln!(
        "                       (whole capture, or windowed) + top thread per CPU. --filter S"
    );
    eprintln!(
        "                       lists every (tid,cpu) whose comm/tdname contains S -- checks"
    );
    eprintln!("                       whether a specific thread (e.g. a bound HPTS instance) pegs");
    eprintln!("                       its assigned core even when aggregate CPU% looks fine.");
    eprintln!("                       --timeline [N] (default N=60) prints an N-bin aggregate");
    eprintln!("                       CPUs-busy time series to check whether saturation was");
    eprintln!("                       sustained or bursty. --freq-scale multiplies busy% by the");
    eprintln!(
        "                       same per-bin frequency scale the GUI uses. --start-s/--end-s"
    );
    eprintln!(
        "                       (trace-relative seconds) restrict every stat to that window."
    );
    eprintln!("  hexdump <offset> [n] Hex dump n bytes at offset (default 64)");
    eprintln!("  flame [opts]         Emit cpu-provider stacks in folded-stack format");
    eprintln!("                       (consumable by flamegraph.pl). Options:");
    eprintln!("                         --cpu N[,M,...]     restrict to listed cpu ids");
    eprintln!("                         --pid N[,M,...]     restrict to listed pids");
    eprintln!("                         --comm S[,T,...]    restrict to comm/tdname prefixes");
    eprintln!("                         --start-s S         restrict to trace-relative seconds");
    eprintln!(
        "                         --end-s S           >= S / <= S (matches the GUI's time axis)"
    );
    eprintln!("                         --rel-text          format unsymbolised frames as");
    eprintln!("                                             kernel+0xOFFSET (kept on by default;");
    eprintln!("                                             pass --raw-addrs to disable)");
    eprintln!(
        "                         --raw-addrs         show 0xADDR instead of kernel+0xOFFSET"
    );
    eprintln!("                                             for unsymbolised frames");
    eprintln!("                         --show-module       prefix symbols with their owning");
    eprintln!("                                             linker file or DSO basename");
    eprintln!("                                             (e.g. kernel:tcp_input,");
    eprintln!("                                             libc.so.7:recvfrom)");
    eprintln!("                         --by-symbol         collapse PCs that resolve to the same");
    eprintln!(
        "                                             symbol (kernel or user-space) into one"
    );
    eprintln!("                                             folded-stack leaf (classic");
    eprintln!(
        "                                             stackcollapse-perf.pl style; default when"
    );
    eprintln!(
        "                                             SYMBOL_RANGE / USER_SYMBOL_RANGE events"
    );
    eprintln!(
        "                                             are available, otherwise falls back to"
    );
    eprintln!("                                             per-PC keying)");
    eprintln!("                         --by-pc             one folded-stack leaf per unique PC");
    eprintln!(
        "                                             (opt-out from --by-symbol; useful when"
    );
    eprintln!("                                             you specifically want to spot tight");
    eprintln!("                                             loops as multiple adjacent leaves)");
    eprintln!("                         --split-user-by-pid keep resolved user-mode frames scoped");
    eprintln!("                                             to their pid (legacy). Default is to");
    eprintln!(
        "                                             fold matching user-mode symbols across"
    );
    eprintln!("                                             pids so N forks of the same binary");
    eprintln!(
        "                                             collapse into one tower. Effective only"
    );
    eprintln!("                                             with --by-symbol.");
    eprintln!("  origin <segments.csv> [more.csv ...] [--raw]");
    eprintln!("                       Align origin video segments ground-truth to the");
    eprintln!("                       capture's timeline and report bitrate overlap stats.");
    eprintln!("                       Rows outside the capture are skipped. --raw dumps");
    eprintln!("                       one line per overlapping segment (t_s/enc1/enc2/size).");
    eprintln!("                         --group-by-process  prefix each stack with the process");
    eprintln!(
        "                                             comm (or `kernel` for kernel threads)."
    );
    eprintln!("                                             Off by default so folded output stays");
    eprintln!("                                             stackcollapse-perf.pl shaped.");
}

/// Split the leading capture paths off the argument list. A leading
/// argument counts as a path while it names a readable file or a
/// `host:path` remote spec, so the command that follows them needs no
/// separator.
fn split_capture_paths(args: &[String]) -> (Vec<String>, &[String]) {
    let is_path = |a: &String| is_remote_path(a).is_some() || std::path::Path::new(a).is_file();
    let n = args.iter().take_while(|a| is_path(a)).count();
    (args[..n].to_vec(), &args[n..])
}

/// Whether `arg` was meant as a capture file rather than a command, used
/// to report a mistyped path as such instead of as an unknown command.
fn looks_like_path(arg: &str) -> bool {
    arg.contains('/') || arg.ends_with(".elog") || arg.ends_with(".gz")
}

/// Entry point for the `elog_cli` binary. Parses argv, loads the capture,
/// and dispatches to the matching `cmd_*` handler. Exits the process on
/// argument or load errors (this is the program's top-level driver).
pub fn run() {
    let argv: Vec<String> = env::args().collect();
    if argv.len() < 2 {
        print_usage();
        process::exit(1);
    }

    if argv[1] == "site" {
        cmd_site(&argv[2..]);
        return;
    }

    let (filepaths, tail) = split_capture_paths(&argv[1..]);
    if filepaths.is_empty() {
        let arg = &argv[1];
        if looks_like_path(arg) {
            eprintln!("Error: cannot read capture file {arg}");
        } else {
            eprintln!("Error: {arg} is not a readable capture file");
            print_usage();
        }
        process::exit(1);
    }

    // The dispatch below indexes argv positionally (`args[3..]` is a
    // command's own arguments), so collapse the path list back down to
    // the single-file shape it expects.
    let mut args = Vec::with_capacity(tail.len() + 2);
    args.push(argv[0].clone());
    args.push(filepaths[0].clone());
    args.extend(tail.iter().cloned());

    let filepath = &args[1];
    let command = args.get(2).map(|s| s.as_str()).unwrap_or("summary");

    // `sizes` walks event record headers only — skip the full parse so
    // multi-GB captures answer in seconds instead of minutes.
    if command == "sizes" {
        cmd_sizes(&filepaths, &args[3..]);
        return;
    }

    // `upload` ships the bytes to the share server unopened — no parse at all.
    if command == "upload" {
        cmd_upload(&filepaths, &args[3..]);
        return;
    }

    let (
        sessions,
        stars_agg_samples,
        stars_pace_samples,
        mut cpu_profile,
        if_profile,
        tcp_samples,
        ella_profile,
        nginx_profile,
        file_info,
        providers,
        schema,
    ) = match parse_elogs(&filepaths) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error parsing {}: {e}", filepaths.join(" "));
            process::exit(1);
        }
    };

    // Reference-mode captures name packs instead of carrying ranges;
    // fetch them only for commands that symbolize, so a plain `summary`
    // or `events` never touches the network.
    let mut pack_stats: Option<PackInstallStats> = None;
    let symbols = symbols_base_url().filter(|_| matches!(command, "flame" | "starsdiag"));
    if let Some(base) = symbols.filter(|_| !cpu_profile.pack_refs.is_empty()) {
        let stats = install_packs(&mut cpu_profile, &base);
        eprintln!(
            "# symbol packs: {} ref(s), {} already inline, {} installed, {} failed",
            stats.refs, stats.already_inline, stats.installed, stats.failed
        );
        pack_stats = Some(stats);
    }

    println!("=== File Info ===");
    println!("  version:         {}", file_info.version);
    if file_info.sources.len() > 1 {
        println!("  merged files:    {}", file_info.sources.len());
        for src in &file_info.sources {
            println!(
                "    {} ({:.3}s..{:.3}s, {} events)",
                src.name,
                src.capture_start.saturating_sub(file_info.capture_start) as f64 / 1_000_000.0,
                src.capture_end.saturating_sub(file_info.capture_start) as f64 / 1_000_000.0,
                src.event_count,
            );
        }
    }
    println!("  capture_start:   {} us", file_info.header_capture_start);
    if file_info.backlog_events > 0 {
        println!(
            "  live start:      {} us (skipped {} backlog event(s) spanning {:.3}s)",
            file_info.capture_start,
            file_info.backlog_events,
            file_info
                .capture_start
                .saturating_sub(file_info.header_capture_start) as f64
                / 1_000_000.0,
        );
    }
    println!("  start_utc_us:    {} us", file_info.start_utc_us);
    println!("  events (header): {}", file_info.event_count);
    println!("  events (parsed): {}", file_info.parsed);
    println!("  skipped:         {}", file_info.skipped_other_providers);
    println!("  dropped:         {}", file_info.dropped);
    if file_info.user_dropped_full > 0 {
        println!(
            "  user_dropped (ring full):  {}",
            file_info.user_dropped_full
        );
    }
    if file_info.user_dropped_prejoin > 0 {
        println!(
            "  user_dropped (prejoin):    {}",
            file_info.user_dropped_prejoin
        );
    }
    if file_info.user_dropped_late > 0 {
        println!(
            "  user_dropped (late):       {}",
            file_info.user_dropped_late
        );
    }
    println!("  providers:");
    for p in &providers {
        let rel = |ts: Option<u64>| match ts {
            Some(t) => format!(
                "{:.3}s",
                t.saturating_sub(file_info.capture_start) as f64 / 1_000_000.0
            ),
            None => "-".into(),
        };
        println!(
            "    id={} name={:?} events={} first={} last={}",
            p.id,
            p.name,
            p.event_count,
            rel(p.first_ts),
            rel(p.last_ts)
        );
        if let Some(schema_name) = &p.schema_name {
            println!(
                "      schema: {schema_name:?} ({} events defined)",
                p.schema_event_count
            );
            if p.schema_divergent_events.is_empty() && p.schema_new_fields.is_empty() {
                println!("      schema: hardcoded decoders verified, no drift detected");
            }
            for line in &p.schema_divergent_events {
                println!("      schema DIVERGENT (generic decode used): {line}");
            }
            for line in &p.schema_new_fields {
                println!("      schema notice: {line}");
            }
        }
    }
    if !schema.hpts_wheel_events.is_empty() {
        println!(
            "  hpts_wheel_events: {} (see `hptsevents`/`hptslateness`/`hptsgaps`)",
            schema.hpts_wheel_events.len()
        );
    }
    if !schema.generic_events.is_empty() {
        println!(
            "  generic_events:  {} (schema-decoded, no hardcoded decoder)",
            schema.generic_events.len()
        );
    }
    if !schema.mem.samples.is_empty() {
        println!(
            "  mem_samples:     {} (see `mem`)",
            schema.mem.samples.len()
        );
    }
    println!("  sessions:        {}", sessions.len());
    println!("  stars_agg_samples:{}", stars_agg_samples.len());
    println!("  stars_pace_samples:{}", stars_pace_samples.len());
    if !if_profile.is_empty() {
        println!(
            "  if_provider:     {} interfaces, {} samples, {} change events (period {} us)",
            if_profile.interfaces.len(),
            if_profile.samples.len(),
            if_profile.events.len(),
            if_profile.period_us,
        );
    }
    if !tcp_samples.is_empty() {
        println!(
            "  tcp_samples:     {} stacks, {} state, {} rps, {} goodput-hist, \
             {} pacing-rate-hist, {} realized-rate-hist samples",
            tcp_samples.stacks.len(),
            tcp_samples.state_samples.len(),
            tcp_samples.rps_samples.len(),
            tcp_samples.gp_samples.len(),
            tcp_samples.pacing_rate_samples.len(),
            tcp_samples.realized_rate_samples.len(),
        );
        if !tcp_samples.send_samples.is_empty()
            || !tcp_samples.lro_flush_samples.is_empty()
            || !tcp_samples.srtt_samples.is_empty()
        {
            println!(
                "  tcp_hist_samples: {} send-size, {} lro-flush, {} srtt",
                tcp_samples.send_samples.len(),
                tcp_samples.lro_flush_samples.len(),
                tcp_samples.srtt_samples.len(),
            );
        }
    }
    if !cpu_profile.is_empty() {
        println!(
            "  cpu_samples:     {} ({} threads, {} dropped)",
            cpu_profile.samples.len(),
            cpu_profile.threads.len(),
            cpu_profile.overflow_dropped,
        );
        let nlf = cpu_profile.linker_files.len();
        let nsym = cpu_profile.symbol_count();
        if nlf > 0 || nsym > 0 {
            println!(
                "  cpu_symbols:     {} symbols across {} linker files{}",
                nsym,
                nlf,
                if nsym == 0 {
                    "  (re-capture with `elog --embed-symbols` for symbols)"
                } else {
                    ""
                },
            );
        }
        let nuf = cpu_profile.user_files.len();
        let nusym = cpu_profile.user_symbol_count();
        let npm = cpu_profile.proc_map_count();
        let npid = cpu_profile.proc_maps.len();
        if nuf > 0 || nusym > 0 || npm > 0 {
            println!(
                "  cpu_user_syms:   {} symbols across {} user files \
                 / {} proc maps across {} pids{}",
                nusym,
                nuf,
                npm,
                npid,
                if nusym == 0 {
                    "  (re-capture with `elog -U` for user symbols)"
                } else {
                    ""
                },
            );
        }
        let nrefs = cpu_profile.pack_refs.len();
        if nrefs > 0 {
            let suffix = match pack_stats {
                Some(s) => format!(
                    "  ({} already inline, {} installed, {} failed)",
                    s.already_inline, s.installed, s.failed
                ),
                None => String::new(),
            };
            println!("  cpu_pack_refs:   {nrefs} symbol pack refs{suffix}");
        }
        let nfiles = cpu_profile.source_files.len();
        let nlines = cpu_profile.source_line_count();
        if nfiles > 0 || nlines > 0 {
            println!("  source_lines:    {nlines} lines across {nfiles} source files");
        }
        if let Some(k) = &cpu_profile.kernel {
            if !k.build_id.is_empty() {
                println!(
                    "  cpu_kernel:      {} build_id={}",
                    if k.ident.is_empty() {
                        "<unknown>"
                    } else {
                        &k.ident
                    },
                    k.build_id,
                );
            }
        }
    }
    println!();

    match command {
        "stats" => cmd_stats(&sessions),
        "memtest" => cmd_memtest(sessions),
        "summary" => {
            let with_limiter = args.iter().skip(3).any(|a| a == "--limiter");
            cmd_summary(&sessions, with_limiter);
        }
        "limiter" => {
            let sid = args.get(3).and_then(|s| parse_session_id(s));
            cmd_limiter(&sessions, sid);
        }
        "rtt" => {
            let filter = args.get(3).and_then(|s| parse_session_id(s));
            cmd_rtt(&sessions, filter);
        }
        "rttanalysis" => {
            cmd_rtt_analysis(&sessions);
        }
        "underflow" => {
            let filter = args.get(3).and_then(|s| parse_session_id(s));
            cmd_underflow(&sessions, filter);
        }
        "goodput" => {
            let filter = args.get(3).and_then(|s| parse_session_id(s));
            cmd_goodput(&sessions, filter);
        }
        "ackgaps" => {
            cmd_ackgaps(&sessions);
        }
        "gpsim" => {
            let sid = args
                .get(3)
                .and_then(|s| parse_session_id(s))
                .unwrap_or_else(|| {
                    eprintln!("gpsim requires a session_hex argument");
                    process::exit(1);
                });
            let start_s: f64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let end_s: f64 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(f64::MAX);
            cmd_gpsim(&sessions, sid, start_s, end_s);
        }
        "ackdump" => {
            let sid = args
                .get(3)
                .and_then(|s| parse_session_id(s))
                .unwrap_or_else(|| {
                    eprintln!("ackdump requires a session_hex argument");
                    process::exit(1);
                });
            let start_s: f64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let end_s: f64 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(f64::MAX);
            cmd_ackdump(&sessions, sid, start_s, end_s);
        }
        "gpcheck" => {
            let sid = args
                .get(3)
                .and_then(|s| parse_session_id(s))
                .unwrap_or_else(|| {
                    eprintln!("gpcheck requires a session_hex argument");
                    process::exit(1);
                });
            let start_s: f64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let end_s: f64 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(f64::MAX);
            cmd_gpcheck(&sessions, sid, start_s, end_s);
        }
        "events" => {
            let filter = args.get(3).and_then(|s| parse_session_id(s));
            let since: Option<f64> = args
                .iter()
                .position(|a| a == "--since")
                .and_then(|i| args.get(i + 1))
                .and_then(|s| s.parse().ok());
            let until: Option<f64> = args
                .iter()
                .position(|a| a == "--until")
                .and_then(|i| args.get(i + 1))
                .and_then(|s| s.parse().ok());
            cmd_events(&sessions, file_info.capture_start, filter, since, until);
        }
        "interfaces" => cmd_interfaces(&if_profile),
        "ifsamples" => {
            // The interface index is positional, not a flag. Reject an
            // unparseable one instead of silently dropping the filter and
            // dumping all interfaces blended together -- a wrong `--ifidx 12`
            // otherwise reads as a plausible single-interface answer.
            if let Some(a) = args.get(3) {
                if a != "--window-s" && a.parse::<u16>().is_err() {
                    eprintln!(
                        "ifsamples: '{a}' is not an interface index. The index is \
                         positional: `ifsamples 12`, not `--ifidx 12`. \
                         Run `interfaces` to list indices."
                    );
                    process::exit(1);
                }
            }
            let ifindex = args.get(3).and_then(|s| s.parse::<u16>().ok());
            let window_s: Option<f64> = args
                .iter()
                .position(|a| a == "--window-s")
                .and_then(|i| args.get(i + 1))
                .and_then(|s| s.parse().ok());
            cmd_if_samples(&if_profile, file_info.capture_start, ifindex, window_s);
        }
        "ifevents" => cmd_if_events(&if_profile, file_info.capture_start),
        "tcpsamples" => cmd_tcp_samples(&tcp_samples, file_info.capture_start),
        "ella" => cmd_ella(&ella_profile, file_info.capture_start),
        "ellastats" => cmd_ella_stats(&ella_profile, file_info.capture_start),
        "ellaevents" => {
            let filter = args.get(3).and_then(|s| parse_session_id(s));
            cmd_ella_events(&ella_profile, file_info.capture_start, filter);
        }
        "nginx" => cmd_nginx(&nginx_profile, file_info.capture_start),
        "nginxstats" => cmd_nginx_stats(&nginx_profile, file_info.capture_start),
        "nginxevents" => {
            let filter = args.get(3).and_then(|s| parse_session_id(s));
            cmd_nginx_events(&nginx_profile, file_info.capture_start, filter);
        }
        "nginxconns" => cmd_nginx_conns(&nginx_profile, file_info.capture_start),
        "nginxdiag" => cmd_nginx_diag(&nginx_profile, file_info.capture_start),
        "mem" => cmd_mem(&schema.mem, file_info.capture_start),
        "starsagg" => cmd_stars_agg(&stars_agg_samples, file_info.capture_start),
        "starspace" => cmd_stars_pace(&stars_pace_samples, file_info.capture_start, &args[3..]),
        "ratecollapse" => {
            cmd_rate_collapse(&stars_pace_samples, file_info.capture_start, &args[3..])
        }
        "httpreqs" => cmd_httpreqs(&sessions, file_info.capture_start, &args[3..]),
        "hptsevents" => {
            let filter = args.get(3).map(|s| s.as_str());
            cmd_hpts_events(&schema.hpts_wheel_events, file_info.capture_start, filter);
        }
        "hptslateness" => {
            cmd_hpts_lateness(
                &schema.hpts_wheel_events,
                file_info.capture_start,
                &args[3..],
            );
        }
        "hptsgaps" => {
            let exclude_last_secs: Option<f64> = args.get(3).and_then(|s| s.parse().ok());
            cmd_hpts_gaps(
                &schema.hpts_wheel_events,
                file_info.capture_start,
                exclude_last_secs,
            );
        }
        "hexdump" => {
            let offset: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let count: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(64);
            cmd_hexdump(filepath, offset, count);
        }
        "cpuinfo" => {
            // Machine-readable sampler metadata for the fleet fold:
            // ncpu and the actual sample rate, one key per line.
            if let Some(k) = &cpu_profile.kernel {
                println!("ncpus={}", k.ncpus);
                println!("sample_hz={}", k.sample_hz());
            } else {
                println!("ncpus=0");
                println!("sample_hz=0");
            }
        }
        "flame" => {
            cmd_flame(&cpu_profile, file_info.capture_start, &args[3..]);
        }
        "percore" => {
            cmd_percore(&cpu_profile, file_info.capture_start, &args[3..]);
        }
        "origin" => {
            cmd_origin(&file_info, &args[3..]);
        }
        "starsdiag" => {
            // Combines interfaces + starsagg + flame into one parse pass.
            // Large captures (multi-GB) load the whole file into memory per
            // invocation -- running these as 3 separate `elog_cli` processes
            // against the same file multiplies both parse time and peak
            // memory for no benefit, since every subcommand parses
            // everything regardless. Flame args after "starsdiag" are
            // forwarded as-is (e.g. `--comm nginx`).
            cmd_interfaces(&if_profile);
            println!();
            cmd_stars_agg(&stars_agg_samples, file_info.capture_start);
            println!();
            cmd_flame(&cpu_profile, file_info.capture_start, &args[3..]);
        }
        _ => {
            eprintln!("Unknown command: {command}");
            print_usage();
            process::exit(1);
        }
    }
}

fn parse_session_id(s: &str) -> Option<u64> {
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u64::from_str_radix(s, 16).ok()
}
