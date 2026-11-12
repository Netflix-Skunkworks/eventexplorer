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

//! Integration coverage for the process provider's `PROC_FORK` edge:
//! fork-without-exec (nginx reload) children inherit the parent's
//! address space, so the parser copies the parent's `PROC_MAP` set onto
//! the child pid. Pids with no edge keep the pre-`PROC_FORK`
//! same-`comm` heuristic as fallback. See
//! `support::synthetic_elog_proc_fork` for the fixture layout.

mod support;

use event_explorer::elog_parser::*;
use support::synthetic_elog_proc_fork;

fn parse_fixture() -> CpuProfile {
    let bytes = synthetic_elog_proc_fork();
    let progress = LoadProgress::default();
    let (_s, _nic, _pace, cpu, _if, _tcp, _ella, _nginx, _fi, _providers, _schema) =
        parse_elog_bytes_with_progress(&bytes, &progress).expect("parse proc-fork fixture");
    cpu
}

#[test]
fn proc_fork_edge_copies_parent_proc_maps_onto_child() {
    let cpu = parse_fixture();

    let parent = cpu.proc_maps.get(&100).expect("parent maps");
    assert_eq!(parent.len(), 1);
    assert_eq!(parent[0].pid, 100);

    let child = cpu
        .proc_maps
        .get(&200)
        .expect("child inherits via PROC_FORK");
    assert_eq!(child.len(), 1);
    assert_eq!(child[0].pid, 200);
    assert_eq!(child[0].user_file_id, 58);
    assert_eq!(child[0].base, 0x37dd_f7a9_0000);
    assert_eq!(child[0].size, 0x1a8000);
    assert_eq!(child[0].offset, 0x90000);
}

#[test]
fn same_comm_heuristic_still_covers_pids_without_a_fork_edge() {
    let cpu = parse_fixture();

    let inherited = cpu.proc_maps.get(&300).expect("heuristic fallback");
    assert_eq!(inherited.len(), 1);
    assert_eq!(inherited[0].pid, 300);
    assert_eq!(inherited[0].base, 0x37dd_f7a9_0000);
}
