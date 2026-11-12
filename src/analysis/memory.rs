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

//! `mem` provider time series for the System-tab Memory / Swap panels
//! and `elog_cli mem`.

use crate::analysis::step_series_bins;
use crate::elog_parser::MemProfile;

const GIB: f64 = (1u64 << 30) as f64;

/// Indexed like [`MemSeries::states`]: Free, Inactive, Laundry, Active, Wired.
pub const MEM_STATE_LABELS: [&str; 5] = ["Free", "Inactive", "Laundry", "Active", "Wired"];

/// Bottom to top. `None` is Other, usable minus the five states.
pub const MEM_STACK: [Option<usize>; 6] = [Some(4), Some(3), Some(1), Some(2), None, Some(0)];

/// Plottable `mem` series: capture-relative seconds vs GiB. Each state
/// is a step series (a sample holds until the next one).
#[derive(Debug, Clone, Default)]
pub struct MemSeries {
    /// Indexed like [`MEM_STATE_LABELS`].
    pub states: [Vec<[f64; 2]>; 5],
    pub free_target: Vec<[f64; 2]>,
    pub free_min: Vec<[f64; 2]>,
    pub free_severe: Vec<[f64; 2]>,
    pub swap_used: Vec<[f64; 2]>,
    /// From the latest `MEM_INFO`; 0 when the capture has none.
    pub usable_gib: f64,
    pub swap_total_gib: f64,
}

impl MemSeries {
    pub fn is_empty(&self) -> bool {
        self.states[0].is_empty()
    }

    pub fn last_t(&self) -> Option<f64> {
        self.states[0].last().map(|p| p[0])
    }

    /// Any swap configured or used during the capture.
    pub fn has_swap(&self) -> bool {
        self.swap_total_gib > 0.0 || self.swap_used.iter().any(|p| p[1] > 0.0)
    }
}

pub fn build_mem_series(prof: &MemProfile, t0_us: u64) -> MemSeries {
    let n = prof.samples.len();
    let mut s = MemSeries {
        states: std::array::from_fn(|_| Vec::with_capacity(n)),
        free_target: Vec::with_capacity(n),
        free_min: Vec::with_capacity(n),
        free_severe: Vec::with_capacity(n),
        swap_used: Vec::with_capacity(n),
        usable_gib: 0.0,
        swap_total_gib: 0.0,
    };
    for m in &prof.samples {
        let t = m.ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        let g = |b: u64| b as f64 / GIB;
        for (k, v) in [m.free, m.inactive, m.laundry, m.active, m.wired]
            .into_iter()
            .enumerate()
        {
            s.states[k].push([t, g(v)]);
        }
        s.free_target.push([t, g(m.free_target)]);
        s.free_min.push([t, g(m.free_min)]);
        s.free_severe.push([t, g(m.free_severe)]);
        s.swap_used.push([t, g(m.swap_used)]);
    }
    if let Some(info) = prof.info() {
        s.usable_gib = info.usable as f64 / GIB;
        s.swap_total_gib = info.swap_total as f64 / GIB;
    }
    s
}

/// GiB of `usable_gib` outside the five sampled states. Zero when
/// usable is unknown or the states already cover it.
pub fn mem_other_gib(accounted_gib: f64, usable_gib: f64) -> f64 {
    if usable_gib <= 0.0 {
        0.0
    } else {
        (usable_gib - accounted_gib).max(0.0)
    }
}

/// Bin each state onto `nbins` equal buckets over `[vx0, vx1]`
/// (time-weighted, see [`step_series_bins`]) and accumulate in
/// [`MEM_STACK`] order. Returns bin centers and, per stack slot, the
/// cumulative band top.
pub fn mem_stack_bins(
    states: &[Vec<[f64; 2]>; 5],
    usable_gib: f64,
    vx0: f64,
    vx1: f64,
    nbins: usize,
) -> (Vec<f64>, [Vec<f64>; 6]) {
    let binned: [Vec<[f64; 2]>; 5] =
        std::array::from_fn(|k| step_series_bins(&states[k], vx0, vx1, nbins));
    let centers: Vec<f64> = binned[0].iter().map(|p| p[0]).collect();
    let mut tops: [Vec<f64>; 6] = std::array::from_fn(|_| Vec::with_capacity(centers.len()));
    for i in 0..centers.len() {
        let height = |k: usize| binned[k].get(i).map_or(0.0, |p| p[1]);
        let accounted: f64 = (0..5).map(height).sum();
        let other = mem_other_gib(accounted, usable_gib);
        let mut acc = 0.0;
        for (slot, band) in MEM_STACK.iter().enumerate() {
            acc += match band {
                Some(k) => height(*k),
                None => other,
            };
            tops[slot].push(acc);
        }
    }
    (centers, tops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elog_parser::{MemInfo, MemSample};

    fn sample(ts: u64, free: u64, wired: u64) -> MemSample {
        MemSample {
            ts,
            free,
            wired,
            ..Default::default()
        }
    }

    #[test]
    fn stack_puts_wired_under_free() {
        let prof = MemProfile {
            infos: Vec::new(),
            samples: vec![
                sample(1_000_000, 2 << 30, 1 << 30),
                sample(2_000_000, 1 << 30, 3 << 30),
                sample(3_000_000, 1 << 30, 3 << 30),
            ],
        };
        let s = build_mem_series(&prof, 0);
        // Two bins: [1,2) holds the first sample, [2,3) the second.
        let (centers, tops) = mem_stack_bins(&s.states, 0.0, 1.0, 3.0, 2);
        assert_eq!(centers, vec![1.5, 2.5]);
        assert_eq!(tops[0], vec![1.0, 3.0], "wired is the bottom band");
        assert_eq!(tops[5], vec![3.0, 4.0], "free tops the stack");
    }

    #[test]
    fn inactive_sits_between_active_and_laundry() {
        let one = 1u64 << 30;
        let sample = |ts| MemSample {
            ts,
            free: one,
            active: one,
            inactive: one,
            laundry: one,
            wired: one,
            ..Default::default()
        };
        let prof = MemProfile {
            infos: vec![MemInfo {
                usable: 10 * one,
                ..Default::default()
            }],
            samples: vec![sample(1_000_000), sample(3_000_000)],
        };
        let s = build_mem_series(&prof, 0);
        let (_, tops) = mem_stack_bins(&s.states, s.usable_gib, 1.0, 3.0, 1);
        assert_eq!(tops[0], vec![1.0], "wired");
        assert_eq!(tops[1], vec![2.0], "active");
        assert_eq!(tops[2], vec![3.0], "inactive");
        assert_eq!(tops[3], vec![4.0], "laundry");
        assert_eq!(tops[4], vec![9.0], "other");
        assert_eq!(tops[5], vec![10.0], "free");
    }

    #[test]
    fn other_is_the_gap_under_usable() {
        assert_eq!(mem_other_gib(110.0, 126.0), 16.0);
        assert_eq!(mem_other_gib(126.0, 125.0), 0.0);
        assert_eq!(mem_other_gib(110.0, 0.0), 0.0);
    }
}
