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

const GIB: f64 = (1u64 << 30) as f64;

/// Dump the `mem` provider's MEM_INFO records and SYS_SAMPLE series (GiB).
pub fn cmd_mem(prof: &MemProfile, capture_start: u64) {
    if prof.infos.is_empty() && prof.samples.is_empty() {
        println!("No mem provider events in this capture.");
        return;
    }
    let rel = |ts: u64| ts.saturating_sub(capture_start) as f64 / 1_000_000.0;
    let g = |b: u64| b as f64 / GIB;
    for i in &prof.infos {
        println!(
            "MEM_INFO t={:.3}s page_size={} domains={} period_us={} physmem={:.2}GiB usable={:.2}GiB swap={:.2}GiB",
            rel(i.ts),
            i.page_size,
            i.ndomains,
            i.sample_period_us,
            g(i.physmem),
            g(i.usable),
            g(i.swap_total)
        );
    }
    if prof.samples.is_empty() {
        return;
    }
    println!(
        "{:>10}  {:>9} {:>9} {:>9} {:>9} {:>9}  {:>9} {:>9} {:>9}  {:>9}",
        "t(s)",
        "free",
        "inactive",
        "laundry",
        "active",
        "wired",
        "target",
        "min",
        "severe",
        "swap_used"
    );
    for s in &prof.samples {
        println!(
            "{:>10.3}  {:>9.3} {:>9.3} {:>9.3} {:>9.3} {:>9.3}  {:>9.3} {:>9.3} {:>9.3}  {:>9.3}",
            rel(s.ts),
            g(s.free),
            g(s.inactive),
            g(s.laundry),
            g(s.active),
            g(s.wired),
            g(s.free_target),
            g(s.free_min),
            g(s.free_severe),
            g(s.swap_used)
        );
    }
}
