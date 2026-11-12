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

mod support;

use event_explorer::analysis::{
    build_nginx_agg, build_nginx_request_list, nginx_bytes_gbps_bins, nginx_latency_mean_bins,
    nginx_status_bins,
};
use event_explorer::elog_parser::*;
use support::{synthetic_elog_nginx, CAPTURE_START};

#[test]
fn nginx_synthetic_decodes_request_layer() {
    let bytes = synthetic_elog_nginx();
    let progress = LoadProgress::default();
    let (_sessions, _nic, _pace, _cpu, _if, _tcp, _ella, nginx, file_info, providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse nginx fixture");

    assert_eq!(file_info.skipped_other_providers, 0);
    assert!(
        providers.iter().any(|p| p.name == "nginx"),
        "nginx provider missing"
    );

    // Two HTTP requests on one connection session.
    assert_eq!(nginx.requests.len(), 2);
    assert_eq!(nginx.events.len(), 4);

    let req_a = nginx
        .requests
        .values()
        .find(|r| r.uri == "/f/fa/2051805908.ismv")
        .expect("request A present");
    assert_eq!(req_a.session_id, 0xAAAA);
    assert_eq!(req_a.request, 1);
    assert_eq!(req_a.method, "GET");
    assert_eq!(req_a.range_start, 0);
    assert_eq!(req_a.range_end, -1);
    assert_eq!(req_a.status, Some(206));
    assert_eq!(req_a.content_length, Some(41943317));
    assert_eq!(req_a.bytes_sent, Some(41943317));
    assert!(req_a.headers_ts.is_some());
    assert!(req_a.done_ts.is_some());
    assert_eq!(req_a.event_count, 3);

    let req_b = nginx
        .requests
        .values()
        .find(|r| r.uri == "/44/11/599505352.ismv")
        .expect("request B present");
    assert_eq!(req_b.session_id, 0xAAAA);
    assert_eq!(req_b.request, 2);
    assert!(req_b.done_ts.is_none(), "request B is still open");
    assert!(req_b.headers_ts.is_none());
}

#[test]
fn nginx_request_list_rows() {
    let bytes = synthetic_elog_nginx();
    let progress = LoadProgress::default();
    let (_s, _n, _p, _c, _i, _t, _e, nginx, _f, _pr, _sc) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse nginx fixture");

    let rows = build_nginx_request_list(&nginx, CAPTURE_START);
    assert_eq!(rows.len(), 2);
    // Sorted by start: request A first.
    assert_eq!(rows[0].uri, "/f/fa/2051805908.ismv");
    assert_eq!(rows[0].status, Some(206));
    assert!(rows[0].duration_s.is_some());
    assert!(rows[0].ttfb_s.is_some());
    assert_eq!(rows[1].uri, "/44/11/599505352.ismv");
    assert!(rows[1].duration_s.is_none());
}

#[test]
fn nginx_agg_series() {
    let bytes = synthetic_elog_nginx();
    let progress = LoadProgress::default();
    let (_s, _n, _p, _c, _i, _t, _e, nginx, _f, _pr, _sc) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse nginx fixture");

    let agg = build_nginx_agg(&nginx, CAPTURE_START);
    assert_eq!(agg.completions.len(), 1);
    assert_eq!(agg.completions[0].status, 206);
    let classes = nginx_status_bins(&agg, 0.0, 1.0, 250);
    let labels: Vec<&str> = classes.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, vec!["2xx"]);
    let total_rate: f64 = classes[0].data.iter().map(|p| p[1]).sum();
    assert!(total_rate > 0.0);
    let gbps = nginx_bytes_gbps_bins(&agg, 0.0, 1.0, 250);
    assert!(gbps.iter().any(|p| p[1] > 0.0));
    assert_eq!(agg.latencies.len(), 1);
    let lat = nginx_latency_mean_bins(&agg, 0.0, 1.0, 250);
    assert!(lat.iter().any(|s| s.data.iter().any(|p| p[1] > 0.0)));
}

#[test]
fn write_fixture_for_cli() {
    let Ok(path) = std::env::var("NGINX_FIXTURE_OUT") else {
        return;
    };
    std::fs::write(&path, synthetic_elog_nginx()).unwrap();
}
