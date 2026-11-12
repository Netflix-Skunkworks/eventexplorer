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

// ---------------------------------------------------------------------------
// mem provider (`mem_eventlog_schema.src`). Fields are read by schema name
// when the capture embeds a blob; the fallback offsets are the kernel's
// packed layout.
// ---------------------------------------------------------------------------

/// Decode one `mem` event into `prof`. Unknown ids and short v1 payloads
/// are ignored.
pub fn parse_mem_event(
    prof: &mut MemProfile,
    ts: u64,
    event_id: u32,
    data: &[u8],
    off: usize,
    size: usize,
    schema: Option<&ProviderSchema>,
) {
    let r = PayloadReader::new(schema, event_id, data, off, size);
    match event_id {
        EVT_MEM_INFO if r.typed() || size >= 34 => prof.infos.push(MemInfo {
            ts,
            page_size: r.u32("page_size", 0),
            ndomains: r.u16("ndomains", 4),
            sample_period_us: r.u32("sample_period_us", 6),
            physmem: r.u64("physmem", 10),
            usable: r.u64("usable", 18),
            swap_total: r.u64("swap_total", 26),
        }),
        EVT_MEM_SYS_SAMPLE if r.typed() || size >= 72 => prof.samples.push(MemSample {
            ts,
            free: r.u64("free", 0),
            active: r.u64("active", 8),
            inactive: r.u64("inactive", 16),
            laundry: r.u64("laundry", 24),
            wired: r.u64("wired", 32),
            free_target: r.u64("free_target", 40),
            free_min: r.u64("free_min", 48),
            free_severe: r.u64("free_severe", 56),
            swap_used: r.u64("swap_used", 64),
        }),
        _ => {}
    }
}
