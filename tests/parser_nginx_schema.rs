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

//! nginx captures carrying the real firmware decode blobs.

mod support;

use event_explorer::analysis::{
    build_nginx_agg, build_nginx_diag_summary, nginx_diag_bins, nginx_session_spans, NginxDiagKind,
};
use event_explorer::elog_parser::*;
use support::*;

const CONN: u64 = 0xAAAA;
const WORKER: u64 = (1 << 63) | 1;

fn parse(bytes: &[u8]) -> (NginxProfile, Vec<Provider>) {
    let progress = LoadProgress::default();
    let (_s, _n, _p, _c, _i, _t, _e, nginx, _f, providers, _sc) =
        parse_elog_bytes_with_progress(bytes, &progress).expect("parse nginx capture");
    (nginx, providers)
}

fn request_start(request: u32, uri: &str) -> Vec<u8> {
    Packed::new()
        .u32(request)
        .i64(0)
        .i64(-1)
        .chars("GET", 16)
        .varlen(uri)
        .done()
}

#[test]
fn v1_request_start_decodes_varlen_uri() {
    let bytes = synthetic_elog_nginx_schema(
        NGINX_BLOB_V1,
        &[
            (1_000, CONN, 0, request_start(1, "/f/fa/2051805908.ismv")),
            (
                2_000,
                CONN,
                2,
                Packed::new().u32(1).u16(206).u64(4096).done(),
            ),
        ],
    );
    let (nginx, providers) = parse(&bytes);
    let p = providers.iter().find(|p| p.name == "nginx").unwrap();
    assert!(
        p.schema_divergent_events.is_empty(),
        "{:?}",
        p.schema_divergent_events
    );
    let req = &nginx.requests[&(NGINX_SCHEMA_PID, CONN, 1)];
    assert_eq!(req.method, "GET");
    assert_eq!(req.uri, "/f/fa/2051805908.ismv");
    assert_eq!(req.range_end, -1);
    assert_eq!(req.bytes_sent, Some(4096));
    assert!(req.detail.is_none());
}

/// Every connection-diagnostics event, packed as the firmware emits it.
fn v2_events() -> Vec<NginxSchemaEvent> {
    vec![
        (500, CONN, 200, Packed::new().u16(443).u8(1).done()),
        (
            800,
            CONN,
            300,
            Packed::new()
                .u16(0x0304)
                .u8(0x03)
                .u32(7)
                .chars("TLS_AES_128_GCM_SHA256", 48)
                .chars("http/1.1", 16)
                .done(),
        ),
        (1_000, CONN, 0, request_start(1, "/range/abc?o=1")),
        (
            1_100,
            CONN,
            500,
            Packed::new().u32(1).u8(0).u8(1).u8(3).u32(40).done(),
        ),
        (
            1_200,
            CONN,
            501,
            Packed::new()
                .u32(1)
                .u8(0)
                .u16(1)
                .u8(2)
                .u16(51234)
                .i32(-1)
                .u64(0x1234)
                .chars("peer", 16)
                .varlen("10.0.0.1:80")
                .done(),
        ),
        (
            1_300,
            CONN,
            502,
            Packed::new()
                .u32(1)
                .u8(0)
                .u16(1)
                .u32(0x0100_0000)
                .u16(0)
                .i32(-1)
                .i32(-1)
                .u32(3000)
                .u64(0)
                .done(),
        ),
        (
            1_400,
            CONN,
            3,
            Packed::new().u32(1).varlen("out of fill targets").done(),
        ),
        (
            1_500,
            CONN,
            1,
            Packed::new().u32(1).u16(200).i64(100).u64(0x42).done(),
        ),
        (
            1_600,
            CONN,
            400,
            Packed::new().u16(4).i64(65536).u64(0x42).u8(0).done(),
        ),
        (
            1_700,
            CONN,
            401,
            Packed::new().u8(0).i32(5).i64(65536).u64(0x42).u8(1).done(),
        ),
        (
            2_000,
            CONN,
            2,
            Packed::new()
                .u32(1)
                .u16(200)
                .u64(100)
                .u16(0x1a) // TIMEDOUT|HEADER_SENT|KEEPALIVE
                .u16(0)
                .i64(100)
                .u32(12)
                .u8(1)
                .u8(2)
                .u32(3)
                .u32(8)
                .u32(4)
                .u32(20)
                .u32(9)
                .done(),
        ),
        (
            2_100,
            CONN,
            402,
            Packed::new()
                .u32(2)
                .u16(404)
                .i32(2)
                .chars("open()", 16)
                .done(),
        ),
        (
            3_000,
            CONN,
            201,
            Packed::new().u8(7).u32(2).u64(2000).u32(1500).done(),
        ),
        (
            3_100,
            0xBBBB,
            301,
            Packed::new().u8(4).i32(0).u32(0).i32(60).u32(60000).done(),
        ),
        (
            4_000,
            WORKER,
            600,
            Packed::new()
                .u32(300)
                .u32(190)
                .u32(150)
                .chars("ngx_freebsd_sendfile_chain.c", 48)
                .chars("ngx_http_request.c", 48)
                .chars("ngx_freebsd_sendfile_chain", 64)
                .done(),
        ),
        (
            4_100,
            WORKER,
            601,
            Packed::new()
                .i64(4096)
                .u64(65536)
                .u32(1200)
                .u64(0x42)
                .done(),
        ),
    ]
}

#[test]
fn v2_events_decode_and_fold() {
    let bytes = synthetic_elog_nginx_schema(NGINX_BLOB_V2, &v2_events());
    let (nginx, providers) = parse(&bytes);
    let p = providers.iter().find(|p| p.name == "nginx").unwrap();
    assert!(
        p.schema_divergent_events.is_empty(),
        "{:?}",
        p.schema_divergent_events
    );
    assert!(p.schema_new_fields.is_empty(), "{:?}", p.schema_new_fields);
    assert_eq!(nginx.events.len(), v2_events().len());

    let req = &nginx.requests[&(NGINX_SCHEMA_PID, CONN, 1)];
    assert_eq!(req.uri, "/range/abc?o=1");
    assert_eq!(req.dev, Some(0x42));
    let d = req.detail.expect("REQUEST_DONE detail");
    assert_eq!(d.flags, 0x1a);
    assert_eq!(d.request_time_ms, 12);
    assert_eq!((d.cache_status, d.upstream_tries), (1, 2));
    assert_eq!((d.busy_events, d.busy_defer_ms), (3, 8));
    assert_eq!((d.aio_reads, d.aio_wait_ms, d.aio_max_ms), (4, 20, 9));
    assert_eq!(req.messages, vec!["out of fill targets".to_string()]);
    let cache = req.cache.as_ref().expect("cache lookup");
    assert_eq!(cache.fas_state, "ENABLE");
    assert_eq!(cache.lock_wait_ms, 40);
    assert_eq!(req.upstream.len(), 1);
    let a = &req.upstream[0];
    assert_eq!(a.peer, "10.0.0.1:80");
    assert_eq!(a.target_type, "peer");
    assert_eq!(a.connect_rc, "AGAIN");
    assert_eq!(a.upstream_session, 0x1234);
    assert_eq!(a.ft, 0x0100_0000);
    assert!(a.end_ts.is_some());

    let missing = &nginx.requests[&(NGINX_SCHEMA_PID, CONN, 2)];
    assert_eq!(missing.open_fail, Some((404, 2, "open()".to_string())));

    let conn = &nginx.conns[&(NGINX_SCHEMA_PID, CONN)];
    assert_eq!(conn.server_port, 443);
    assert_eq!(conn.flags, NGINX_CONN_SSL);
    assert_eq!(conn.close_reason.as_deref(), Some("CLIENT_CLOSED"));
    assert_eq!((conn.requests, conn.bytes_sent), (2, 2000));
    let tls = conn.tls.as_ref().expect("tls");
    assert_eq!(tls.version, 0x0304);
    assert_eq!(tls.cipher, "TLS_AES_128_GCM_SHA256");
    assert_eq!(tls.alpn, "http/1.1");
    assert_eq!(tls.flags, NGINX_TLS_REUSED | NGINX_TLS_KTLS_TX);
    assert_eq!((conn.busy_defers, conn.busy_defer_ms), (1, 4));
    assert_eq!(conn.sendfile_errors, 1);

    let failed = &nginx.conns[&(NGINX_SCHEMA_PID, 0xBBBB)];
    assert_eq!(failed.tls_fail.as_ref().unwrap().kind, "TIMEOUT");

    assert!(nginx.events.iter().any(|e| matches!(
        &e.payload,
        NginxPayload::WorkerStall { stall_ms: 300, func, .. } if func == "ngx_freebsd_sendfile_chain"
    )));
}

#[test]
fn v2_diag_summary_and_series() {
    let bytes = synthetic_elog_nginx_schema(NGINX_BLOB_V2, &v2_events());
    let (nginx, _) = parse(&bytes);

    let d = build_nginx_diag_summary(&nginx, CAPTURE_START);
    assert_eq!(d.connections, 2);
    assert_eq!(d.close_reasons, vec![("CLIENT_CLOSED".to_string(), 1)]);
    assert_eq!((d.tls_handshakes, d.tls_ktls, d.tls_reused), (1, 1, 1));
    assert_eq!(d.tls_failures, vec![("TIMEOUT".to_string(), 1)]);
    assert_eq!(d.abnormal_requests, vec![("TIMEDOUT".to_string(), 1)]);
    assert_eq!(d.cache_status, vec![("MISS".to_string(), 1)]);
    assert_eq!(d.upstream_failures, vec![("TIMEOUT_CONN".to_string(), 1)]);
    assert_eq!((d.busy_defers, d.busy_defer_ms), (1, 4));
    assert_eq!(d.aio_sync_reads, 1);
    assert_eq!(d.stalls.len(), 1);
    assert_eq!(d.messages, vec![("out of fill targets".to_string(), 1)]);

    let agg = build_nginx_agg(&nginx, CAPTURE_START);
    let kinds: Vec<NginxDiagKind> = nginx_diag_bins(&agg, 0.0, 0.01, 10)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(kinds, NginxDiagKind::ALL.to_vec());

    let spans = nginx_session_spans(&nginx, CONN, CAPTURE_START);
    let span = spans.iter().find(|s| s.request == 1).unwrap();
    assert_eq!(
        span.detail(),
        "cache MISS · 2 upstream tries · disk busy 8 ms · aio 20 ms · TIMEDOUT · 1 message"
    );
}
