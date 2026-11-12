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

//! Unit tests for pure, public formatting/parsing helpers in the parser
//! library. These run fast and pin behavior that the refactor must preserve.

use event_explorer::elog_parser::*;

#[test]
fn pacing_cap_names() {
    assert_eq!(pacing_cap_name(0), "none");
    assert_eq!(pacing_cap_name(1), "STARS");
    assert_eq!(pacing_cap_name(2), "bw_rate_cap");
    assert_eq!(pacing_cap_name(200), "?");
}

#[test]
fn conn_params_flag_formatting() {
    assert_eq!(format_conn_params_flags(0), "none");
    // 0x43 = WINDOW_SCALE | TIMESTAMPS | SACK_PERMIT.
    assert_eq!(format_conn_params_flags(0x43), "SACK+TS+WSCALE");
}

#[test]
fn http_req_flag_formatting() {
    assert_eq!(format_http_req_flags(0), "none");
    assert_eq!(format_http_req_flags(0x01 | 0x02), "USED+OPEN");
}

#[test]
fn time_span_carries_rounded_seconds_into_minutes() {
    use event_explorer::app::format_time_span;
    assert_eq!(format_time_span(90.0), "1m 30.0s");
    // A rolled capture's boundary lands one sample short of an exact
    // minute, which must not render as "59m 60.0s".
    assert_eq!(format_time_span(3599.999), "60m 0.0s");
    assert_eq!(format_time_span(10799.999), "180m 0.0s");
    assert_eq!(format_time_span(3600.0), "60m 0.0s");
    // Sub-minute spans keep their own units.
    assert_eq!(format_time_span(59.9), "59.900 s");
}

#[test]
fn remote_path_detection() {
    assert_eq!(
        is_remote_path("host:/var/log/x.elog"),
        Some(("host", "/var/log/x.elog"))
    );
    assert_eq!(
        is_remote_path("user@host:path"),
        Some(("user@host", "path"))
    );
    // Plain local paths are not remote.
    assert_eq!(is_remote_path("/var/log/x.elog"), None);
    assert_eq!(is_remote_path("relative/x.elog"), None);
    // A single-letter host looks like a Windows drive, so it's rejected.
    assert_eq!(is_remote_path("C:file"), None);
}
