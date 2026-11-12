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

fn fmt_bytes(n: Option<u64>) -> String {
    match n {
        Some(v) => format!("{v}"),
        None => "-".into(),
    }
}

fn fmt_dur(s: Option<f64>) -> String {
    match s {
        Some(v) => format!("{v:.3}"),
        None => "-".into(),
    }
}

/// List nginx HTTP requests seen in the capture.
pub fn cmd_nginx(prof: &NginxProfile, capture_start: u64) {
    if prof.is_empty() {
        println!("No nginx data in this capture.");
        return;
    }
    let rows = build_nginx_request_list(prof, capture_start);
    let done = rows.iter().filter(|r| r.duration_s.is_some()).count();
    println!(
        "=== nginx requests ({} requests, {} completed) ===",
        rows.len(),
        done
    );
    println!(
        "{:>10} {:>18} {:>8} {:<6} {:<6} {:>10} {:>12} {:>10} {:<12} {:>5} {:<12}  uri",
        "start_s",
        "session",
        "request",
        "method",
        "status",
        "dur_s",
        "ttfb_s",
        "bytes",
        "cache",
        "tries",
        "flags"
    );
    for r in rows {
        let dash = |s: &str| {
            if s.is_empty() {
                "-".to_string()
            } else {
                s.to_string()
            }
        };
        // Keepalive/header-sent are the normal case; show only the rest.
        let flags: Vec<&str> = r
            .flags
            .split('|')
            .filter(|f| !f.is_empty() && *f != "KEEPALIVE" && *f != "HEADER_SENT")
            .collect();
        println!(
            "{:>10.3} {:>18} {:>8} {:<6} {:<6} {:>10} {:>12} {:>10} {:<12} {:>5} {:<12}  {}",
            r.start_s,
            format!("{:#x}", r.session_id),
            r.request,
            r.method,
            r.status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".into()),
            fmt_dur(r.duration_s),
            fmt_dur(r.ttfb_s),
            fmt_bytes(r.bytes_sent),
            dash(&r.cache),
            if r.upstream_tries == 0 {
                "-".to_string()
            } else {
                r.upstream_tries.to_string()
            },
            dash(&flags.join("|")),
            r.uri,
        );
        for m in &r.messages {
            println!("{:>10}   message: {m}", "");
        }
    }
}

fn print_counts(title: &str, rows: &NginxCounts) {
    if rows.is_empty() {
        return;
    }
    println!("  {title}:");
    for (k, n) in rows {
        println!("    {n:>10}  {k}");
    }
}

/// Capture-wide rollup of nginx connection, TLS, disk, cache, upstream
/// and worker events.
pub fn cmd_nginx_diag(prof: &NginxProfile, capture_start: u64) {
    if prof.is_empty() {
        println!("No nginx data in this capture.");
        return;
    }
    let d = build_nginx_diag_summary(prof, capture_start);

    println!("=== nginx connections ({}) ===", d.connections);
    print_counts("close reasons", &d.close_reasons);
    if d.tls_handshakes > 0 {
        println!(
            "  TLS handshakes: {} ({} kTLS, {} resumed)",
            d.tls_handshakes, d.tls_ktls, d.tls_reused
        );
        if let Some((p50, p90, max)) = d.tls_handshake_ms {
            println!("  TLS handshake ms (accept->done): p50 {p50}  p90 {p90}  max {max}");
        }
        print_counts("TLS versions", &d.tls_versions);
    }
    print_counts("TLS handshake failures", &d.tls_failures);

    println!();
    println!("=== nginx requests ({}) ===", d.requests);
    print_counts("abnormal completions", &d.abnormal_requests);
    print_counts("cache status", &d.cache_status);
    if d.cache_lock_waits > 0 {
        println!(
            "  cache lock waits: {} (max {} ms)",
            d.cache_lock_waits, d.cache_lock_wait_ms_max
        );
    }
    if d.upstream_attempts > 0 {
        println!("  upstream attempts: {}", d.upstream_attempts);
    }
    print_counts("upstream attempt failures", &d.upstream_failures);
    print_counts("messages", &d.messages);

    println!();
    println!("=== nginx disk ===");
    println!(
        "  sendfile EBUSY deferrals: {} ({} ms of back-off)",
        d.busy_defers, d.busy_defer_ms
    );
    print_counts("sendfile errors", &d.sendfile_errors);
    print_counts("file open failures", &d.open_failures);
    if d.aio_sync_reads > 0 {
        println!(
            "  blocking aio reads: {} (total {:.1} ms, max {:.1} ms)",
            d.aio_sync_reads,
            d.aio_sync_read_us as f64 / 1000.0,
            d.aio_sync_read_us_max as f64 / 1000.0
        );
    }

    println!();
    println!("=== nginx worker stalls ({}) ===", d.stalls.len());
    for s in &d.stalls {
        println!(
            "  {:>10.3}  tid {:<8} {:>6} ms  {}  since {}",
            s.ts_s, s.tid, s.stall_ms, s.at, s.since
        );
    }
}

/// One row per nginx connection with its lifecycle and TLS details.
pub fn cmd_nginx_conns(prof: &NginxProfile, capture_start: u64) {
    let conns = nginx_conn_list(prof);
    if conns.is_empty() {
        println!("No nginx connection events in this capture.");
        return;
    }
    let rel = |ts: Option<u64>| {
        ts.map(|t| format!("{:.3}", t.saturating_sub(capture_start) as f64 / 1e6))
            .unwrap_or_else(|| "-".into())
    };
    println!("=== nginx connections ({}) ===", conns.len());
    println!(
        "{:>18} {:>10} {:>10} {:>5} {:<18} {:>5} {:>12} {:>8} {:>6} {:>6}  tls",
        "session", "start_s", "end_s", "port", "close", "reqs", "bytes", "dur_ms", "busy", "sferr"
    );
    for c in conns {
        let tls = match (&c.tls, &c.tls_fail) {
            (Some(t), _) => {
                let flags = nginx_tls_flag_names(t.flags);
                format!(
                    "{} {} alpn={} {}ms{}",
                    nginx_tls_version_name(t.version),
                    t.cipher,
                    if t.alpn.is_empty() { "-" } else { &t.alpn },
                    t.handshake_ms,
                    if flags.is_empty() {
                        String::new()
                    } else {
                        format!(" {flags}")
                    }
                )
            }
            (None, Some(f)) => format!(
                "FAILED {} ssl_error={} reason={} errno={} after {}ms",
                f.kind, f.ssl_error, f.reason, f.sys_errno, f.handshake_ms
            ),
            (None, None) if c.flags & NGINX_CONN_SSL != 0 => "(ssl)".into(),
            (None, None) => "-".into(),
        };
        println!(
            "{:>18} {:>10} {:>10} {:>5} {:<18} {:>5} {:>12} {:>8} {:>6} {:>6}  {}",
            format!("{:#x}", c.session_id),
            rel(c.start_ts),
            rel(c.end_ts),
            if c.server_port == 0 {
                "-".to_string()
            } else {
                c.server_port.to_string()
            },
            c.close_reason.as_deref().unwrap_or("-"),
            c.requests,
            c.bytes_sent,
            c.duration_ms,
            c.busy_defers,
            c.sendfile_errors,
            tls,
        );
    }
}

/// Dump the System-tab aggregate series (bytes/sec, status mix) as TSV.
pub fn cmd_nginx_stats(prof: &NginxProfile, capture_start: u64) {
    if prof.is_empty() {
        println!("No nginx data in this capture.");
        return;
    }
    let agg = build_nginx_agg(prof, capture_start);
    if agg.completions.is_empty() && agg.samples.is_empty() {
        println!("No completed nginx requests in this capture.");
        return;
    }
    let vx0 = 0.0;
    let mut vx1 = agg
        .samples
        .last()
        .map(|s| s.ts_s)
        .or_else(|| agg.completions.last().map(|c| c.ts_s))
        .unwrap_or(1.0);
    if vx1 <= vx0 {
        vx1 = 1.0;
    }
    let gbps = nginx_bytes_gbps_bins(&agg, vx0, vx1, crate::app::STACK_BINS);
    println!("=== nginx completion Gb/s ===");
    for p in &gbps {
        println!("{:>10.3}  {:>14.3}", p[0], p[1]);
    }
    let status = nginx_status_bins(&agg, vx0, vx1, crate::app::STACK_BINS);
    if !status.is_empty() {
        println!();
        println!("=== nginx status mix (responses/sec) ===");
        print!("{:>10}", "t(s)");
        for s in &status {
            print!("  {:>10}", s.label);
        }
        println!();
        let n = status.first().map(|s| s.data.len()).unwrap_or(0);
        for i in 0..n {
            print!("{:>10.3}", status[0].data[i][0]);
            for s in &status {
                print!("  {:>10.1}", s.data[i][1]);
            }
            println!();
        }
    }
    let lat = nginx_latency_mean_bins(&agg, vx0, vx1, crate::app::STACK_BINS);
    if !lat.is_empty() {
        println!();
        println!("=== nginx latency mean (ms) ===");
        print!("{:>10}", "t(s)");
        for s in &lat {
            print!("  {:>10}", s.label);
        }
        println!();
        let n = lat.first().map(|s| s.data.len()).unwrap_or(0);
        for i in 0..n {
            print!("{:>10.3}", lat[0].data[i][0]);
            for s in &lat {
                print!("  {:>10.1}", s.data[i][1]);
            }
            println!();
        }
    }
}

/// Dump nginx lifecycle events, optionally filtered to one session.
pub fn cmd_nginx_events(prof: &NginxProfile, capture_start: u64, session: Option<u64>) {
    if prof.events.is_empty() {
        println!("No nginx events in this capture.");
        return;
    }
    let mut n = 0usize;
    for e in &prof.events {
        if let Some(sid) = session {
            if e.session_id != sid {
                continue;
            }
        }
        n += 1;
        let t = e.ts.saturating_sub(capture_start) as f64 / 1_000_000.0;
        let detail = nginx_event_detail(&e.payload);
        println!(
            "{:>10.6}  {:#x}  {}  {}",
            t,
            e.session_id,
            nginx_event_name(e.event_id),
            detail
        );
    }
    if n == 0 {
        println!("No matching nginx events.");
    }
}

/// One-line `key=value` rendering of an nginx event payload.
pub fn nginx_event_detail(p: &NginxPayload) -> String {
    match p {
        NginxPayload::RequestStart {
            request,
            range_start,
            range_end,
            method,
            uri,
        } => format!("{method} {uri} request={request} range={range_start}-{range_end}"),
        NginxPayload::ResponseHeaders {
            request,
            status,
            content_length,
            dev,
        } => {
            let mut s = format!("request={request} status={status} content_length={content_length}");
            if let Some(dev) = dev {
                s.push_str(&format!(" dev={dev}"));
            }
            s
        }
        NginxPayload::RequestDone {
            request,
            status,
            bytes_sent,
            detail,
        } => {
            let mut s = format!("request={request} status={status} bytes_sent={bytes_sent}");
            if let Some(d) = detail {
                s.push_str(&format!(
                    " flags={} err_status={} content_length={} time={}ms cache={} tries={} busy={}/{}ms aio={}/{}ms/max {}ms",
                    nginx_done_flag_names(d.flags),
                    d.err_status,
                    d.content_length,
                    d.request_time_ms,
                    nginx_cache_status_name(d.cache_status),
                    d.upstream_tries,
                    d.busy_events,
                    d.busy_defer_ms,
                    d.aio_reads,
                    d.aio_wait_ms,
                    d.aio_max_ms
                ));
            }
            s
        }
        NginxPayload::RequestMessage { request, msg } => format!("request={request} {msg}"),
        NginxPayload::Sample {
            resp_1xx,
            resp_2xx,
            resp_3xx,
            resp_4xx,
            resp_5xx,
            resp_other,
            bytes_sent,
        } => format!(
            "1xx={resp_1xx} 2xx={resp_2xx} 3xx={resp_3xx} 4xx={resp_4xx} 5xx={resp_5xx} other={resp_other} bytes_sent={bytes_sent}"
        ),
        NginxPayload::ConnStart { server_port, flags } => {
            let mut f = Vec::new();
            if flags & NGINX_CONN_SSL != 0 {
                f.push("SSL");
            }
            if flags & NGINX_CONN_PROXY_PROTOCOL != 0 {
                f.push("PROXY_PROTOCOL");
            }
            format!("port={server_port} flags={}", f.join("|"))
        }
        NginxPayload::ConnEnd {
            reason,
            requests,
            bytes_sent,
            duration_ms,
        } => format!(
            "reason={reason} requests={requests} bytes_sent={bytes_sent} duration={duration_ms}ms"
        ),
        NginxPayload::TlsHandshakeDone(t) => format!(
            "{} {} alpn={} flags={} handshake={}ms",
            nginx_tls_version_name(t.version),
            t.cipher,
            t.alpn,
            nginx_tls_flag_names(t.flags),
            t.handshake_ms
        ),
        NginxPayload::TlsHandshakeFail(f) => format!(
            "kind={} ssl_error={} reason={} errno={} after={}ms",
            f.kind, f.ssl_error, f.reason, f.sys_errno, f.handshake_ms
        ),
        NginxPayload::SendfileBusyDefer {
            delay_ms,
            file_pos,
            dev,
            ktls,
        } => format!("delay={delay_ms}ms offset={file_pos} dev={dev} ktls={ktls}"),
        NginxPayload::SendfileError {
            kind,
            sys_errno,
            file_pos,
            dev,
            ktls,
        } => format!("kind={kind} errno={sys_errno} offset={file_pos} dev={dev} ktls={ktls}"),
        NginxPayload::FileOpenFail {
            request,
            status,
            sys_errno,
            syscall,
        } => format!("request={request} {syscall} errno={sys_errno} status={status}"),
        NginxPayload::CacheLookup(c) => format!(
            "request={} cache={} fas={} lock_wait={}ms subrequest={}",
            c.request,
            nginx_cache_status_name(c.cache_status),
            c.fas_state,
            c.lock_wait_ms,
            c.subrequest
        ),
        NginxPayload::UpstreamAttemptStart {
            request,
            subrequest,
            attempt,
            connect_rc,
            local_port,
            dns_ms,
            upstream_session,
            target_type,
            peer,
        } => format!(
            "request={request} attempt={attempt} peer={peer} target={target_type} connect={connect_rc} lport={local_port} dns={dns_ms}ms tcp_session={upstream_session:#x} subrequest={subrequest}"
        ),
        NginxPayload::UpstreamAttemptEnd {
            request,
            subrequest,
            attempt,
            ft,
            status,
            connect_ms,
            header_ms,
            elapsed_ms,
            bytes_received,
        } => format!(
            "request={request} attempt={attempt} ft={} status={status} connect={connect_ms}ms header={header_ms}ms elapsed={elapsed_ms}ms rx={bytes_received} subrequest={subrequest}",
            nginx_upstream_ft_names(*ft)
        ),
        NginxPayload::WorkerStall {
            stall_ms,
            file,
            line,
            prev_file,
            prev_line,
            func,
        } => format!("stall={stall_ms}ms at {file}:{line} ({func}) since {prev_file}:{prev_line}"),
        NginxPayload::AioSyncFallback {
            offset,
            size,
            read_us,
            dev,
        } => format!("size={size} offset={offset} read={read_us}us dev={dev}"),
    }
}

/// Display name for an nginx event id.
pub fn nginx_event_name(event_id: u32) -> &'static str {
    match event_id {
        EVT_NGINX_REQUEST_START => "REQUEST_START",
        EVT_NGINX_RESPONSE_HEADERS => "RESPONSE_HEADERS",
        EVT_NGINX_REQUEST_DONE => "REQUEST_DONE",
        EVT_NGINX_REQUEST_MESSAGE => "REQUEST_MESSAGE",
        EVT_NGINX_SAMPLE => "NGINX_SAMPLE",
        EVT_NGINX_CONN_START => "CONN_START",
        EVT_NGINX_CONN_END => "CONN_END",
        EVT_NGINX_TLS_HANDSHAKE_DONE => "TLS_HANDSHAKE_DONE",
        EVT_NGINX_TLS_HANDSHAKE_FAIL => "TLS_HANDSHAKE_FAIL",
        EVT_NGINX_SENDFILE_BUSY_DEFER => "SENDFILE_BUSY_DEFER",
        EVT_NGINX_SENDFILE_ERROR => "SENDFILE_ERROR",
        EVT_NGINX_FILE_OPEN_FAIL => "FILE_OPEN_FAIL",
        EVT_NGINX_CACHE_LOOKUP => "CACHE_LOOKUP",
        EVT_NGINX_UPSTREAM_ATTEMPT_START => "UPSTREAM_ATTEMPT_START",
        EVT_NGINX_UPSTREAM_ATTEMPT_END => "UPSTREAM_ATTEMPT_END",
        EVT_NGINX_WORKER_STALL => "WORKER_STALL",
        EVT_NGINX_AIO_SYNC_FALLBACK => "AIO_SYNC_FALLBACK",
        _ => "UNKNOWN",
    }
}
