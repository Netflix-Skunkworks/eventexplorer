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

//! End-to-end golden snapshots of every `elog_cli` subcommand run against the
//! synthetic fixture. This is the primary regression net for the parser +
//! analysis + CLI-formatting pipeline: any behavior change during a refactor
//! shows up as a golden diff.

mod support;

use std::process::Command;
use support::{check_golden, fixture_path};

fn run(args: &[&str]) -> String {
    let fixture = fixture_path();
    let output = Command::new(env!("CARGO_BIN_EXE_elog_cli"))
        .arg(&fixture)
        .args(args)
        .output()
        .expect("run elog_cli");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Most subcommands write the report to stdout; a few diagnostics go to
    // stderr. Capture both so the snapshot is complete and stable.
    format!("--- stdout ---\n{stdout}--- stderr ---\n{stderr}")
}

macro_rules! golden_test {
    ($name:ident, $file:expr, [$($arg:expr),* $(,)?]) => {
        #[test]
        fn $name() {
            check_golden($file, &run(&[$($arg),*]));
        }
    };
}

golden_test!(summary, "cli/summary.txt", ["summary"]);
golden_test!(
    summary_limiter,
    "cli/summary_limiter.txt",
    ["summary", "--limiter"]
);
golden_test!(stats, "cli/stats.txt", ["stats"]);
golden_test!(limiter, "cli/limiter.txt", ["limiter", "1001"]);
golden_test!(rtt, "cli/rtt.txt", ["rtt", "1001"]);
golden_test!(rttanalysis, "cli/rttanalysis.txt", ["rttanalysis"]);
golden_test!(underflow, "cli/underflow.txt", ["underflow"]);
golden_test!(goodput, "cli/goodput.txt", ["goodput", "1001"]);
golden_test!(gpsim, "cli/gpsim.txt", ["gpsim", "1001"]);
golden_test!(ackdump, "cli/ackdump.txt", ["ackdump", "1001", "0", "1000"]);
golden_test!(gpcheck, "cli/gpcheck.txt", ["gpcheck", "1001"]);
golden_test!(events, "cli/events.txt", ["events", "1001"]);
golden_test!(interfaces, "cli/interfaces.txt", ["interfaces"]);
golden_test!(ifsamples, "cli/ifsamples.txt", ["ifsamples"]);
golden_test!(ifevents, "cli/ifevents.txt", ["ifevents"]);
golden_test!(tcpsamples, "cli/tcpsamples.txt", ["tcpsamples"]);
golden_test!(flame, "cli/flame.txt", ["flame"]);
golden_test!(flame_by_pc, "cli/flame_by_pc.txt", ["flame", "--by-pc"]);
golden_test!(
    flame_show_module,
    "cli/flame_show_module.txt",
    ["flame", "--show-module"]
);
