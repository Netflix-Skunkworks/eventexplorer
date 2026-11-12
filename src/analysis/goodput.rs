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

use crate::elog_parser::*;

/// Accumulating goodput measurement (active ACK time, excludes idle gaps).
pub fn compute_kernel_goodput(
    events: &[Event],
    idle_thresh_us: u64,
    t0: u64,
    min_active_us: u64,
    max_wall_us: u64,
) -> Vec<(f64, f64)> {
    let mut ack_pairs: Vec<(u64, u32)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::Ack { acked, .. } if *acked > 0 => Some((e.ts, *acked)),
            _ => None,
        })
        .collect();
    ack_pairs.sort_by_key(|x| x.0);

    let n = ack_pairs.len();
    if n < 2 {
        return Vec::new();
    }

    let mut points = Vec::new();
    let mut win_start_us = ack_pairs[0].0;
    let mut total_bytes = ack_pairs[0].1 as u64;
    let mut active_time: u64 = 0;

    for i in 1..n {
        let ts = ack_pairs[i].0;
        let nbytes = ack_pairs[i].1 as u64;
        total_bytes += nbytes;

        let gap = ts.saturating_sub(ack_pairs[i - 1].0);
        if gap <= idle_thresh_us {
            active_time += gap;
        }

        let elapsed = ts.saturating_sub(win_start_us);
        if active_time >= min_active_us || (elapsed >= max_wall_us && active_time > 0) {
            let rate = if active_time > 0 {
                total_bytes as f64 * USEC_IN_SEC / active_time as f64
            } else {
                0.0
            };
            points.push(((ts - t0) as f64 / USEC_IN_SEC, rate));
            win_start_us = ts;
            total_bytes = 0;
            active_time = 0;
        }
    }

    points
}
