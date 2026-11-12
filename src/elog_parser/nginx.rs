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

/// Schema enum name for `value`, else the firmware's name at the time
/// this decoder was written (`nginx_eventlog_schema.src`), else the number.
fn enum_str(r: &PayloadReader, field: &str, value: u32, fallback: &[&str]) -> String {
    if let Some(name) = r.enum_name(field, value) {
        return name.to_string();
    }
    fallback
        .get(value as usize)
        .map(|s| s.to_string())
        .unwrap_or_else(|| value.to_string())
}

const CLOSE_REASONS: &[&str] = &[
    "CLOSED",
    "ERROR",
    "TIMEOUT",
    "IDLE_TIMEOUT",
    "HEADER_TIMEOUT",
    "SEND_TIMEOUT",
    "KEEPALIVE_TIMEOUT",
    "CLIENT_CLOSED",
    "LINGERING_TIMEOUT",
    "TLS_FAILED",
    "WORKER_EXIT",
];
const TLS_FAIL_KINDS: &[&str] = &["ERROR", "EOF", "REJECTED", "OCSP", "TIMEOUT"];
const DISK_ERROR_KINDS: &[&str] = &["ERROR", "TRUNCATED"];
const FAS_STATES: &[&str] = &["NONE", "NONEED", "DONE", "ENABLE", "READ", "HEADER", "BODY"];
const CONNECT_RCS: &[&str] = &[
    "OK",
    "ERROR",
    "AGAIN",
    "BUSY",
    "DONE",
    "DECLINED",
    "ABORT",
    "NO_STRIPING",
];

/// Decode and ingest one `nginx` event into `prof`. Envelope `session_id`
/// is the TCP connection (or a special worker/sample session); `request`
/// in the payload is the HTTP/1 serial or HTTP/2 stream id.
///
/// Fallback offsets are for schema-less captures: the pre-release natural
/// alignment for the original four events, packed layout for the rest.
#[allow(clippy::too_many_arguments)]
pub fn ingest_nginx(
    prof: &mut NginxProfile,
    ts: u64,
    cpu: u16,
    tid: i32,
    provider_id: u16,
    session_id: u64,
    event_id: u32,
    data: &[u8],
    off: usize,
    size: usize,
    schema: Option<&ProviderSchema>,
) -> bool {
    let r = PayloadReader::new(schema, event_id, data, off, size);
    let payload = match event_id {
        // request u32, pad, range_start i64, range_end i64,
        // method char[16], uri char[256] — 296 bytes.
        EVT_NGINX_REQUEST_START if r.typed() || size >= 296 => NginxPayload::RequestStart {
            request: r.u32("request", 0),
            range_start: r.i64("range_start", 8),
            range_end: r.i64("range_end", 16),
            method: r.cstr("method", 24, 16),
            uri: r.text("uri", 40, 256),
        },
        // request u32, status u16, pad, content_length i64 — 16 bytes.
        EVT_NGINX_RESPONSE_HEADERS if r.typed() || size >= 16 => NginxPayload::ResponseHeaders {
            request: r.u32("request", 0),
            status: r.u16("status", 4),
            content_length: r.i64("content_length", 8),
            dev: r.has("dev").then(|| r.u64("dev", 14)),
        },
        // request u32, status u16, pad, bytes_sent u64 — 16 bytes.
        EVT_NGINX_REQUEST_DONE if r.typed() || size >= 16 => NginxPayload::RequestDone {
            request: r.u32("request", 0),
            status: r.u16("status", 4),
            bytes_sent: r.u64("bytes_sent", 8),
            detail: r.has("flags").then(|| NginxDoneDetail {
                flags: r.u16("flags", 14),
                err_status: r.u16("err_status", 16),
                content_length: r.i64("content_length", 18),
                request_time_ms: r.u32("request_time_ms", 26),
                cache_status: r.u8("cache_status", 30),
                upstream_tries: r.u8("upstream_tries", 31),
                busy_events: r.u32("busy_events", 32),
                busy_defer_ms: r.u32("busy_defer_ms", 36),
                aio_reads: r.u32("aio_reads", 40),
                aio_wait_ms: r.u32("aio_wait_ms", 44),
                aio_max_ms: r.u32("aio_max_ms", 48),
            }),
        },
        EVT_NGINX_REQUEST_MESSAGE if r.typed() || size >= 6 => NginxPayload::RequestMessage {
            request: r.u32("request", 0),
            msg: r.text("msg", 6, 256),
        },
        // resp_1xx..resp_other u64 x6, bytes_sent u64 — 56 bytes.
        EVT_NGINX_SAMPLE if r.typed() || size >= 56 => NginxPayload::Sample {
            resp_1xx: r.u64("resp_1xx", 0),
            resp_2xx: r.u64("resp_2xx", 8),
            resp_3xx: r.u64("resp_3xx", 16),
            resp_4xx: r.u64("resp_4xx", 24),
            resp_5xx: r.u64("resp_5xx", 32),
            resp_other: r.u64("resp_other", 40),
            bytes_sent: r.u64("bytes_sent", 48),
        },
        EVT_NGINX_CONN_START if r.typed() || size >= 3 => NginxPayload::ConnStart {
            server_port: r.u16("server_port", 0),
            flags: r.u8("flags", 2),
        },
        EVT_NGINX_CONN_END if r.typed() || size >= 17 => NginxPayload::ConnEnd {
            reason: enum_str(&r, "reason", r.u8("reason", 0) as u32, CLOSE_REASONS),
            requests: r.u32("requests", 1),
            bytes_sent: r.u64("bytes_sent", 5),
            duration_ms: r.u32("duration_ms", 13),
        },
        EVT_NGINX_TLS_HANDSHAKE_DONE if r.typed() || size >= 71 => {
            NginxPayload::TlsHandshakeDone(NginxTlsInfo {
                version: r.u16("version", 0),
                flags: r.u8("flags", 2),
                handshake_ms: r.u32("handshake_ms", 3),
                cipher: r.cstr("cipher", 7, 48),
                alpn: r.cstr("alpn", 55, 16),
            })
        }
        EVT_NGINX_TLS_HANDSHAKE_FAIL if r.typed() || size >= 17 => {
            NginxPayload::TlsHandshakeFail(NginxTlsFail {
                kind: enum_str(&r, "kind", r.u8("kind", 0) as u32, TLS_FAIL_KINDS),
                ssl_error: r.i32("ssl_error", 1),
                reason: r.u32("reason", 5),
                sys_errno: r.i32("sys_errno", 9),
                handshake_ms: r.u32("handshake_ms", 13),
            })
        }
        EVT_NGINX_SENDFILE_BUSY_DEFER if r.typed() || size >= 19 => {
            NginxPayload::SendfileBusyDefer {
                delay_ms: r.u16("delay_ms", 0),
                file_pos: r.i64("file_pos", 2),
                dev: r.u64("dev", 10),
                ktls: r.u8("ktls", 18) != 0,
            }
        }
        EVT_NGINX_SENDFILE_ERROR if r.typed() || size >= 22 => NginxPayload::SendfileError {
            kind: enum_str(&r, "kind", r.u8("kind", 0) as u32, DISK_ERROR_KINDS),
            sys_errno: r.i32("sys_errno", 1),
            file_pos: r.i64("file_pos", 5),
            dev: r.u64("dev", 13),
            ktls: r.u8("ktls", 21) != 0,
        },
        EVT_NGINX_FILE_OPEN_FAIL if r.typed() || size >= 26 => NginxPayload::FileOpenFail {
            request: r.u32("request", 0),
            status: r.u16("status", 4),
            sys_errno: r.i32("sys_errno", 6),
            syscall: r.cstr("syscall", 10, 16),
        },
        EVT_NGINX_CACHE_LOOKUP if r.typed() || size >= 11 => {
            NginxPayload::CacheLookup(NginxCacheLookup {
                request: r.u32("request", 0),
                subrequest: r.u8("subrequest", 4) != 0,
                cache_status: r.u8("cache_status", 5),
                fas_state: enum_str(&r, "fas_state", r.u8("fas_state", 6) as u32, FAS_STATES),
                lock_wait_ms: r.u32("lock_wait_ms", 7),
            })
        }
        EVT_NGINX_UPSTREAM_ATTEMPT_START if r.typed() || size >= 40 => {
            NginxPayload::UpstreamAttemptStart {
                request: r.u32("request", 0),
                subrequest: r.u8("subrequest", 4) != 0,
                attempt: r.u16("attempt", 5),
                connect_rc: enum_str(&r, "connect_rc", r.u8("connect_rc", 7) as u32, CONNECT_RCS),
                local_port: r.u16("local_port", 8),
                dns_ms: r.i32("dns_ms", 10),
                upstream_session: r.u64("upstream_session", 14),
                target_type: r.cstr("target_type", 22, 16),
                peer: r.text("peer", 40, 128),
            }
        }
        EVT_NGINX_UPSTREAM_ATTEMPT_END if r.typed() || size >= 33 => {
            NginxPayload::UpstreamAttemptEnd {
                request: r.u32("request", 0),
                subrequest: r.u8("subrequest", 4) != 0,
                attempt: r.u16("attempt", 5),
                ft: r.u32("ft", 7),
                status: r.u16("status", 11),
                connect_ms: r.i32("connect_ms", 13),
                header_ms: r.i32("header_ms", 17),
                elapsed_ms: r.u32("elapsed_ms", 21),
                bytes_received: r.u64("bytes_received", 25),
            }
        }
        EVT_NGINX_WORKER_STALL if r.typed() || size >= 172 => NginxPayload::WorkerStall {
            stall_ms: r.u32("stall_ms", 0),
            line: r.u32("line", 4),
            prev_line: r.u32("prev_line", 8),
            file: r.cstr("file", 12, 48),
            prev_file: r.cstr("prev_file", 60, 48),
            func: r.cstr("func", 108, 64),
        },
        EVT_NGINX_AIO_SYNC_FALLBACK if r.typed() || size >= 28 => NginxPayload::AioSyncFallback {
            offset: r.i64("offset", 0),
            size: r.u64("size", 8),
            read_us: r.u32("read_us", 16),
            dev: r.u64("dev", 20),
        },
        _ => return false,
    };
    apply_to_request(prof, provider_id, session_id, ts, &payload);
    apply_to_conn(prof, provider_id, session_id, ts, &payload);
    prof.events.push(NginxEvent {
        ts,
        cpu,
        tid,
        provider_id,
        session_id,
        event_id,
        payload,
    });
    true
}

fn apply_to_request(
    prof: &mut NginxProfile,
    provider_id: u16,
    session_id: u64,
    ts: u64,
    payload: &NginxPayload,
) {
    let Some(request) = payload.request() else {
        return;
    };
    let entry = prof
        .requests
        .entry((provider_id, session_id, request))
        .or_insert_with(|| NginxRequestInfo::new(provider_id, session_id, request, ts));
    entry.event_count += 1;
    match payload {
        NginxPayload::RequestStart {
            range_start,
            range_end,
            method,
            uri,
            ..
        } => {
            entry.range_start = *range_start;
            entry.range_end = *range_end;
            entry.method = method.clone();
            entry.uri = uri.clone();
            entry.start_ts = ts;
        }
        NginxPayload::ResponseHeaders {
            status,
            content_length,
            dev,
            ..
        } => {
            entry.headers_ts = Some(ts);
            entry.status = Some(*status);
            entry.content_length = Some(*content_length);
            if dev.is_some() {
                entry.dev = *dev;
            }
        }
        NginxPayload::RequestDone {
            status,
            bytes_sent,
            detail,
            ..
        } => {
            entry.done_ts = Some(ts);
            entry.status = Some(*status);
            entry.bytes_sent = Some(*bytes_sent);
            if detail.is_some() {
                entry.detail = *detail;
            }
        }
        NginxPayload::RequestMessage { msg, .. } => entry.messages.push(msg.clone()),
        NginxPayload::FileOpenFail {
            status,
            sys_errno,
            syscall,
            ..
        } => entry.open_fail = Some((*status, *sys_errno, syscall.clone())),
        NginxPayload::CacheLookup(c) => {
            if !c.subrequest {
                entry.cache = Some(c.clone());
            }
        }
        NginxPayload::UpstreamAttemptStart {
            subrequest,
            attempt,
            connect_rc,
            upstream_session,
            target_type,
            peer,
            ..
        } => entry.upstream.push(NginxUpstreamAttempt {
            attempt: *attempt,
            subrequest: *subrequest,
            peer: peer.clone(),
            target_type: target_type.clone(),
            connect_rc: connect_rc.clone(),
            upstream_session: *upstream_session,
            start_ts: ts,
            ..Default::default()
        }),
        NginxPayload::UpstreamAttemptEnd {
            subrequest,
            attempt,
            ft,
            status,
            connect_ms,
            header_ms,
            bytes_received,
            ..
        } => {
            let a = match entry
                .upstream
                .iter()
                .rposition(|a| a.attempt == *attempt && a.subrequest == *subrequest)
            {
                Some(i) => &mut entry.upstream[i],
                None => {
                    // START predates the capture.
                    entry.upstream.push(NginxUpstreamAttempt {
                        attempt: *attempt,
                        subrequest: *subrequest,
                        start_ts: ts,
                        ..Default::default()
                    });
                    entry.upstream.last_mut().unwrap()
                }
            };
            a.end_ts = Some(ts);
            a.ft = *ft;
            a.status = *status;
            a.connect_ms = *connect_ms;
            a.header_ms = *header_ms;
            a.bytes_received = *bytes_received;
        }
        _ => {}
    }
}

fn apply_to_conn(
    prof: &mut NginxProfile,
    provider_id: u16,
    session_id: u64,
    ts: u64,
    payload: &NginxPayload,
) {
    if !payload.is_conn_event() {
        return;
    }
    let c = prof
        .conns
        .entry((provider_id, session_id))
        .or_insert_with(|| NginxConnInfo {
            provider_id,
            session_id,
            ..Default::default()
        });
    match payload {
        NginxPayload::ConnStart { server_port, flags } => {
            // Replays (enable, dump_state) repeat CONN_START; keep the first.
            c.start_ts.get_or_insert(ts);
            c.server_port = *server_port;
            c.flags = *flags;
        }
        NginxPayload::ConnEnd {
            reason,
            requests,
            bytes_sent,
            duration_ms,
        } => {
            c.end_ts = Some(ts);
            c.close_reason = Some(reason.clone());
            c.requests = *requests;
            c.bytes_sent = *bytes_sent;
            c.duration_ms = *duration_ms;
        }
        NginxPayload::TlsHandshakeDone(t) => c.tls = Some(t.clone()),
        NginxPayload::TlsHandshakeFail(f) => c.tls_fail = Some(f.clone()),
        NginxPayload::SendfileBusyDefer { delay_ms, .. } => {
            c.busy_defers += 1;
            c.busy_defer_ms += *delay_ms as u64;
        }
        NginxPayload::SendfileError { .. } => c.sendfile_errors += 1,
        _ => {}
    }
}

/// `SSL_version()` as a display string.
pub fn nginx_tls_version_name(v: u16) -> String {
    match v {
        0x0300 => "SSLv3".into(),
        0x0301 => "TLSv1.0".into(),
        0x0302 => "TLSv1.1".into(),
        0x0303 => "TLSv1.2".into(),
        0x0304 => "TLSv1.3".into(),
        _ => format!("{v:#06x}"),
    }
}

/// `NGX_HTTP_CACHE_*` as a display string ("" when no cache was involved).
pub fn nginx_cache_status_name(v: u8) -> &'static str {
    match v {
        0 => "",
        1 => "MISS",
        2 => "BYPASS",
        3 => "EXPIRED",
        4 => "STALE",
        5 => "UPDATING",
        6 => "REVALIDATED",
        7 => "HIT",
        8 => "SCARCE",
        9 => "CACHELOCKEXP",
        _ => "?",
    }
}

fn flag_names(v: u64, table: &[(u64, &str)]) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut rest = v;
    for &(bit, name) in table {
        if v & bit != 0 {
            out.push(name);
            rest &= !bit;
        }
    }
    let mut s = out.join("|");
    if rest != 0 {
        if !s.is_empty() {
            s.push('|');
        }
        s.push_str(&format!("{rest:#x}"));
    }
    s
}

/// `REQUEST_DONE` flags as `A|B` ("" when none).
pub fn nginx_done_flag_names(v: u16) -> String {
    flag_names(
        v as u64,
        &[
            (NGINX_DONE_TERMINATED as u64, "TERMINATED"),
            (NGINX_DONE_TIMEDOUT as u64, "TIMEDOUT"),
            (NGINX_DONE_CONN_ERROR as u64, "CONN_ERROR"),
            (NGINX_DONE_HEADER_SENT as u64, "HEADER_SENT"),
            (NGINX_DONE_KEEPALIVE as u64, "KEEPALIVE"),
            (NGINX_DONE_NO_START as u64, "NO_START"),
        ],
    )
}

/// `TLS_HANDSHAKE_DONE` flags as `A|B`.
pub fn nginx_tls_flag_names(v: u8) -> String {
    flag_names(
        v as u64,
        &[
            (NGINX_TLS_REUSED as u64, "REUSED"),
            (NGINX_TLS_KTLS_TX as u64, "KTLS_TX"),
            (NGINX_TLS_EARLY_DATA as u64, "EARLY_DATA"),
        ],
    )
}

/// `NGX_HTTP_UPSTREAM_FT_*` bits as `A|B` ("" for a clean final attempt).
pub fn nginx_upstream_ft_names(v: u32) -> String {
    flag_names(
        v as u64,
        &[
            (0x0000_0002, "ERROR"),
            (0x0000_0004, "TIMEOUT"),
            (0x0000_0008, "INVALID_HEADER"),
            (0x0000_0010, "HTTP_500"),
            (0x0000_0020, "HTTP_502"),
            (0x0000_0040, "HTTP_503"),
            (0x0000_0080, "HTTP_504"),
            (0x0000_0100, "HTTP_403"),
            (0x0000_0200, "HTTP_404"),
            (0x0000_0400, "HTTP_429"),
            (0x0000_0800, "UPDATING"),
            (0x0000_1000, "BUSY_LOCK"),
            (0x0000_2000, "MAX_WAITING"),
            (0x0000_4000, "NON_IDEMPOTENT"),
            (0x0010_0000, "ERROR_CONN"),
            (0x0020_0000, "ERROR_SEND"),
            (0x0040_0000, "ERROR_RECV"),
            (0x0100_0000, "TIMEOUT_CONN"),
            (0x0200_0000, "TIMEOUT_SEND"),
            (0x0400_0000, "TIMEOUT_RECV"),
            (0x4000_0000, "NOLIVE"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad_align(buf: &mut Vec<u8>, align: usize) {
        while !buf.len().is_multiple_of(align) {
            buf.push(0);
        }
    }

    fn put_u64(buf: &mut Vec<u8>, v: u64) {
        pad_align(buf, 8);
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn put_u32(buf: &mut Vec<u8>, v: u32) {
        pad_align(buf, 4);
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn put_u16(buf: &mut Vec<u8>, v: u16) {
        pad_align(buf, 2);
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn put_cstr(buf: &mut Vec<u8>, s: &str, width: usize) {
        let mut v = vec![0u8; width];
        let n = s.len().min(width);
        v[..n].copy_from_slice(&s.as_bytes()[..n]);
        buf.extend_from_slice(&v);
    }

    fn ingest(prof: &mut NginxProfile, session_id: u64, event_id: u32, payload: &[u8]) -> bool {
        ingest_nginx(
            prof,
            1,
            0,
            0,
            6,
            session_id,
            event_id,
            payload,
            0,
            payload.len(),
            None,
        )
    }

    #[test]
    fn request_start_offsets() {
        let mut b = Vec::new();
        put_u32(&mut b, 42); // request
        put_u64(&mut b, 100); // range_start
        put_u64(&mut b, u64::MAX); // range_end = -1
        put_cstr(&mut b, "GET", 16);
        put_cstr(&mut b, "/f/fa/2051805908.ismv", 256);
        assert_eq!(b.len(), 296);
        let mut prof = NginxProfile::default();
        assert!(ingest(&mut prof, 9, EVT_NGINX_REQUEST_START, &b));
        match &prof.events[0].payload {
            NginxPayload::RequestStart {
                request,
                range_start,
                range_end,
                method,
                uri,
            } => {
                assert_eq!(*request, 42);
                assert_eq!((*range_start, *range_end), (100, -1));
                assert_eq!(method, "GET");
                assert_eq!(uri, "/f/fa/2051805908.ismv");
            }
            other => panic!("unexpected {other:?}"),
        }
        let req = &prof.requests[&(6, 9, 42)];
        assert_eq!(req.method, "GET");
        assert_eq!(req.request, 42);
        assert_eq!(req.range_end, -1);
        assert_eq!(req.session_id, 9);
    }

    #[test]
    fn response_headers_and_done_fold_into_request() {
        let mut prof = NginxProfile::default();
        let mut start = Vec::new();
        put_u32(&mut start, 3);
        put_u64(&mut start, 0);
        put_u64(&mut start, u64::MAX);
        put_cstr(&mut start, "GET", 16);
        put_cstr(&mut start, "/x", 256);
        assert!(ingest(&mut prof, 5, EVT_NGINX_REQUEST_START, &start));

        let mut hdr = Vec::new();
        put_u32(&mut hdr, 3);
        put_u16(&mut hdr, 206);
        put_u64(&mut hdr, 41943317);
        assert_eq!(hdr.len(), 16);
        assert!(ingest(&mut prof, 5, EVT_NGINX_RESPONSE_HEADERS, &hdr));

        let mut done = Vec::new();
        put_u32(&mut done, 3);
        put_u16(&mut done, 206);
        put_u64(&mut done, 41943317);
        assert!(ingest(&mut prof, 5, EVT_NGINX_REQUEST_DONE, &done));

        let req = &prof.requests[&(6, 5, 3)];
        assert_eq!(req.status, Some(206));
        assert_eq!(req.content_length, Some(41943317));
        assert_eq!(req.bytes_sent, Some(41943317));
        assert!(req.headers_ts.is_some());
        assert!(req.done_ts.is_some());
        assert_eq!(req.event_count, 3);
    }

    #[test]
    fn two_requests_on_one_connection_stay_distinct() {
        let mut prof = NginxProfile::default();
        for req_id in [1u32, 2] {
            let mut start = Vec::new();
            put_u32(&mut start, req_id);
            put_u64(&mut start, 0);
            put_u64(&mut start, u64::MAX);
            put_cstr(&mut start, "GET", 16);
            put_cstr(&mut start, &format!("/{req_id}"), 256);
            assert!(ingest(&mut prof, 0xAAAA, EVT_NGINX_REQUEST_START, &start));
        }
        assert_eq!(prof.requests.len(), 2);
        assert_eq!(prof.requests[&(6, 0xAAAA, 1)].uri, "/1");
        assert_eq!(prof.requests[&(6, 0xAAAA, 2)].uri, "/2");
    }

    #[test]
    fn sample_decodes_and_skips_request_fold() {
        let mut b = Vec::new();
        put_u64(&mut b, 0); // resp_1xx
        put_u64(&mut b, 120); // resp_2xx
        put_u64(&mut b, 5); // resp_3xx
        put_u64(&mut b, 3); // resp_4xx
        put_u64(&mut b, 2); // resp_5xx
        put_u64(&mut b, 1); // resp_other
        put_u64(&mut b, 987654); // bytes_sent
        assert_eq!(b.len(), 56);
        let mut prof = NginxProfile::default();
        assert!(ingest(&mut prof, 0, EVT_NGINX_SAMPLE, &b));
        match &prof.events[0].payload {
            NginxPayload::Sample {
                resp_2xx,
                resp_other,
                bytes_sent,
                ..
            } => {
                assert_eq!(*resp_2xx, 120);
                assert_eq!(*resp_other, 1);
                assert_eq!(*bytes_sent, 987654);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(prof.requests.is_empty());
    }
}
