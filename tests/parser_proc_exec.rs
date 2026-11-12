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

//! Integration coverage for `PROC_EXEC`: the parser records the image
//! name after `execve`, and sample-time lookup follows that history
//! instead of the fork-time `THREAD_CREATE` `comm`.

mod support;

use event_explorer::elog_parser::*;
use support::{synthetic_elog_proc_exec, CAPTURE_START};

fn parse_fixture() -> CpuProfile {
    let bytes = synthetic_elog_proc_exec();
    let progress = LoadProgress::default();
    let (_s, _nic, _pace, cpu, _if, _tcp, _ella, _nginx, _fi, _providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse proc-exec fixture");
    cpu
}

#[test]
fn proc_exec_renames_process_comm_at_sample_time() {
    let cpu = parse_fixture();

    assert_eq!(cpu.samples.len(), 2);
    assert_eq!(cpu.threads.get(&1).map(|t| t.comm.as_str()), Some("sh"));
    assert_eq!(
        cpu.proc_execs.get(&100).map(|v| v.len()),
        Some(1),
        "PROC_EXEC should be recorded"
    );
    assert_eq!(
        cpu.process_comm_at(100, Some(1), CAPTURE_START + 50),
        Some("sh")
    );
    assert_eq!(
        cpu.process_comm_at(100, Some(1), CAPTURE_START + 150),
        Some("perl")
    );
    assert_eq!(cpu.process_comm_latest(100), Some("perl"));
}
