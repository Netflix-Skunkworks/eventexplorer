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

#![allow(dead_code)]

mod progress;
pub use progress::*;
mod constants;
pub use constants::*;
mod names;
pub use names::*;
mod types;
pub use types::*;
mod bytes;
pub use bytes::*;
mod payload;
pub use payload::*;
mod schema;
pub use schema::*;
mod schema_validate;
pub use schema_validate::*;
mod hpts_wheel;
pub use hpts_wheel::*;
mod ella;
pub use ella::*;
mod nginx;
pub use nginx::*;
mod mem;
pub use mem::*;
mod core;
pub use core::*;
mod merge;
pub use merge::*;
mod packs;
pub use packs::*;
mod uploads;
pub use uploads::*;
mod io;
pub use io::*;
#[cfg(target_arch = "wasm32")]
pub(crate) use io::{
    member_display_name, open_gzip_member, unique_part_name, GzipMember, GzipSplit,
};

pub(crate) use std::io::Result;
pub(crate) use std::net::{Ipv4Addr, Ipv6Addr};
pub(crate) use std::sync::atomic::{AtomicU32, Ordering};
pub(crate) use std::sync::Mutex;

pub(crate) use flate2::read::GzDecoder;
pub use rustc_hash::{FxHashMap, FxHashSet};

// Desktop-only IO. On wasm32 the parser only ever sees an in-memory byte
// slice (the user uploads the file via <input type="file">), so the path
// loaders, mmap, and the SSH remote-fetch path are all gated out.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use memmap2::Mmap;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use rayon::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use std::fs::File;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use std::io::Read as _;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use std::process::{Command, Stdio};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::*;

    /// Pack ranges live in a table shared by `Arc` clones. The CPU
    /// strips keep a clone for the whole capture, so an install after
    /// that clone exists has to be visible through it.
    #[test]
    fn pack_symbols_resolve_through_profile_clones() {
        let mut profile = CpuProfile::default();
        profile.linker_files.insert(
            1,
            LinkerFile {
                file_id: 1,
                base: 0x1000,
                size: 0x1000,
                flags: LINKER_FILE_FLAG_KERNEL,
                build_id: String::new(),
                filename: "kernel".to_string(),
                pathname: String::new(),
            },
        );
        let profile = std::sync::Arc::new(profile);
        let clone = std::sync::Arc::clone(&profile);
        profile.pack_symbols.insert(
            1,
            vec![SymbolRange {
                offset: 0x10,
                size: 0x20,
                name: "leaf".to_string(),
            }],
        );
        let hit = clone.symbolize_kernel(0x1014).expect("pack symbol");
        assert_eq!(hit.name(), "leaf");
        assert!(profile.symbols.is_empty());
    }

    /// Manual fill-in of zero-sized symbol ranges, matching the pass
    /// the real parser runs at the end of `parse_elog_core`. Lifted
    /// out so unit tests can build a `CpuProfile` by hand without
    /// going through a synthetic `.elog` round-trip.
    fn fill_in_zero_sized_symbols(profile: &mut CpuProfile) {
        for (file_id, syms) in profile.symbols.iter_mut() {
            syms.sort_unstable_by_key(|s| s.offset);
            syms.dedup_by(|a, b| a.offset == b.offset);
            let file_end = profile
                .linker_files
                .get(file_id)
                .map(|lf| lf.size)
                .unwrap_or(u64::MAX);
            for i in 0..syms.len() {
                if syms[i].size != 0 {
                    continue;
                }
                let next = syms.get(i + 1).map(|s| s.offset).unwrap_or(file_end);
                let span = next.saturating_sub(syms[i].offset);
                syms[i].size = span.min(u32::MAX as u64) as u32;
            }
        }
    }

    /// Regression test for the "asm entry point with `st_size == 0`
    /// shows up as `kernel+0xOFFSET`" bug. Builds a one-file profile
    /// where the middle symbol mimics `Xtimerint` (no `.size`
    /// directive, so `SymbolRange { size: 0, .. }`) and asserts that
    /// the post-parse fill-in pass extends its range up to the next
    /// symbol's offset, letting `symbolize` resolve a PC inside it.
    #[test]
    fn zero_sized_symbol_resolves_after_fill_in() {
        let mut profile = CpuProfile::default();
        let file_id = 1u32;
        profile.linker_files.insert(
            file_id,
            LinkerFile {
                file_id,
                base: 0x1000,
                size: 0x1000,
                flags: LINKER_FILE_FLAG_KERNEL | LINKER_FILE_FLAG_LINKED,
                build_id: String::new(),
                filename: "kernel".to_string(),
                pathname: "/boot/kernel/kernel".to_string(),
            },
        );
        profile.symbols.insert(
            file_id,
            vec![
                SymbolRange {
                    offset: 0x000,
                    size: 0x10,
                    name: "foo".to_string(),
                },
                SymbolRange {
                    offset: 0x040,
                    size: 0,
                    name: "Xtimerint".to_string(),
                },
                SymbolRange {
                    offset: 0x100,
                    size: 0x20,
                    name: "bar".to_string(),
                },
            ],
        );

        // Before the fill-in pass the binary search lands on `bar`
        // (offset 0x100 > 0x80) and `bar`'s range doesn't cover
        // 0x80, so the lookup misses entirely.
        assert!(
            profile.symbolize(0x1080, None).is_none(),
            "without fill-in, PCs inside a zero-sized symbol must miss"
        );

        fill_in_zero_sized_symbols(&mut profile);

        // After the pass `Xtimerint` spans `0x40..0x100` and the PC
        // at offset 0x80 (= file PC 0x1080) resolves to it with an
        // inner offset of 0x40.
        let hit = profile
            .symbolize(0x1080, None)
            .expect("Xtimerint should now cover 0x40..0x100");
        assert_eq!(hit.name(), "Xtimerint");
        assert_eq!(hit.inner_offset(), 0x40);
        assert_eq!(hit.label(), "Xtimerint+0x40");

        // The trailing symbol's own range is left untouched.
        let bar_hit = profile
            .symbolize(0x1100 + 0x10, None)
            .expect("bar still resolves at its own offsets");
        assert_eq!(bar_hit.name(), "bar");
        assert_eq!(bar_hit.inner_offset(), 0x10);

        // And the head symbol's bounds are still respected — a PC
        // past `foo`'s end but before `Xtimerint`'s start is *not*
        // attributed to either: the gap stays a gap.
        assert!(
            profile.symbolize(0x1020, None).is_none(),
            "gap between foo and Xtimerint must not be covered by the fill-in"
        );

        // Sanity: a PC past the last symbol but still inside the
        // linker file gets no hit (no symbol covers it).
        assert!(profile.symbolize(0x1FF0, None).is_none());
    }

    /// Last symbol in the table is zero-sized: the fill-in must use
    /// the owning `LinkerFile::size` as the cap rather than leaving
    /// it at zero.
    #[test]
    fn trailing_zero_sized_symbol_uses_linker_file_end() {
        let mut profile = CpuProfile::default();
        let file_id = 7u32;
        profile.linker_files.insert(
            file_id,
            LinkerFile {
                file_id,
                base: 0x2000,
                size: 0x100,
                flags: LINKER_FILE_FLAG_KERNEL | LINKER_FILE_FLAG_LINKED,
                build_id: String::new(),
                filename: "kernel".to_string(),
                pathname: "/boot/kernel/kernel".to_string(),
            },
        );
        profile.symbols.insert(
            file_id,
            vec![SymbolRange {
                offset: 0x80,
                size: 0,
                name: "calltrap".to_string(),
            }],
        );

        fill_in_zero_sized_symbols(&mut profile);

        let hit = profile
            .symbolize(0x2080 + 0x40, None)
            .expect("calltrap should span up to LinkerFile end");
        assert_eq!(hit.name(), "calltrap");
        assert_eq!(hit.inner_offset(), 0x40);

        // PC past the linker-file end is outside any LinkerFile
        // range and so misses at the file-selection step, not the
        // symbol-range check.
        assert!(profile.symbolize(0x2100, None).is_none());
    }

    /// Manual fill-in of zero-sized **user**-symbol ranges.
    /// Mirrors the kernel-side helper above, parameterised on
    /// `user_symbols` instead of `symbols`. Uses `u64::MAX` as the
    /// "no next entry" cap because user ELFs don't carry a single
    /// global file size we can hand the resolver — any PROC_MAP
    /// referencing the file already bounds the lookup.
    fn fill_in_zero_sized_user_symbols(profile: &mut CpuProfile) {
        for syms in profile.user_symbols.values_mut() {
            syms.sort_unstable_by_key(|s| s.offset);
            syms.dedup_by(|a, b| a.offset == b.offset);
            for i in 0..syms.len() {
                if syms[i].size != 0 {
                    continue;
                }
                let next = syms.get(i + 1).map(|s| s.offset).unwrap_or(u64::MAX);
                let span = next.saturating_sub(syms[i].offset);
                syms[i].size = span.min(u32::MAX as u64) as u32;
            }
        }
    }

    /// End-to-end check of the user-mode resolver: build a
    /// `CpuProfile` with a single shared library (PIE / `ET_DYN`,
    /// file offset `0x1000`) mapped into pid 42, two symbols (one
    /// real, one `Xtimerint`-style with `size == 0`), and assert
    /// that:
    ///   * the small symbol resolves at its own offset,
    ///   * the zero-sized symbol resolves *after* the fill-in pass
    ///     (and would miss before it),
    ///   * a sample with the wrong pid does not borrow another
    ///     pid's mapping.
    #[test]
    fn user_symbol_resolves_after_fill_in() {
        let mut profile = CpuProfile::default();
        let user_file_id = 1u32;
        profile.user_files.insert(
            user_file_id,
            UserFile {
                user_file_id,
                build_id: "abcd".to_string(),
                pathname: "/lib/libfoo.so".to_string(),
            },
        );
        profile.proc_maps.insert(
            42,
            vec![ProcMap {
                pid: 42,
                user_file_id,
                base: 0x4000_0000,
                size: 0x1_0000,
                offset: 0x1000,
            }],
        );
        profile.user_symbols.insert(
            user_file_id,
            vec![
                UserSymbolRange {
                    user_file_id,
                    offset: 0x1000,
                    size: 0x20,
                    name: "foo".to_string(),
                },
                UserSymbolRange {
                    user_file_id,
                    offset: 0x1020,
                    size: 0,
                    name: "bar_asm".to_string(),
                },
            ],
        );

        // First sym resolves immediately — no fill-in needed for
        // a non-zero-size entry. PC == map.base hits foo at
        // inner_offset 0 (because map.base translates to file
        // offset 0x1000, which is exactly foo's start).
        let foo = profile
            .symbolize(0x4000_0000, Some(42))
            .expect("foo should resolve at map.base");
        assert_eq!(foo.name(), "foo");
        assert_eq!(foo.inner_offset(), 0);
        assert!(matches!(foo, SymbolHit::User { .. }));

        // Before the fill-in: a PC at runtime VA 0x4000_0040 maps
        // to file offset 0x1040, which is past foo's range (0x1000 +
        // 0x20 == 0x1020) and inside what *should* be bar_asm —
        // but bar_asm.size is 0, so the half-open check rejects.
        // The PC *does* fall in pid 42's PROC_MAP though, so the
        // module-only fallback fires and surfaces the DSO basename
        // + runtime offset (0x4000_0040 - 0x4000_0000 == 0x40).
        let pre_fillin = profile
            .symbolize(0x4000_0040, Some(42))
            .expect("module-only fallback should kick in");
        assert!(matches!(pre_fillin, SymbolHit::UserModuleOnly { .. }));
        assert_eq!(pre_fillin.label(), "libfoo.so+0x40");

        fill_in_zero_sized_user_symbols(&mut profile);

        // After the fill-in: bar_asm spans [0x1020, u64::MAX) (no
        // next entry, no per-file cap), so the same PC resolves
        // to bar_asm at inner_offset 0x20.
        let bar = profile
            .symbolize(0x4000_0040, Some(42))
            .expect("bar_asm should resolve after the fill-in");
        assert!(matches!(bar, SymbolHit::User { .. }));
        assert_eq!(bar.name(), "bar_asm");
        assert_eq!(bar.inner_offset(), 0x20);
        assert_eq!(bar.label(), "bar_asm+0x20");

        // Wrong pid: there's no PROC_MAP for pid 43, so even with
        // a runtime VA that *would* match pid 42's mapping the
        // lookup must miss entirely (no fallback either — without
        // a PROC_MAP we can't even tell which DSO the PC came
        // from). This is the correctness gate that prevents pid
        // 100's `0x4000_0000` from accidentally borrowing pid
        // 200's symbol table.
        assert!(
            profile.symbolize(0x4000_0040, Some(43)).is_none(),
            "wrong pid must not borrow another pid's mapping"
        );

        // No pid: kernel path is the only one tried, and there
        // are no LinkerFiles in this profile, so the lookup
        // misses. Confirms `symbolize(.., None)` doesn't reach
        // for the user table on its own.
        assert!(profile.symbolize(0x4000_0040, None).is_none());

        // The label-with-file form surfaces the DSO basename, not
        // the full path. This is what the GUI's "show module
        // prefix" toggle and the CLI's `--show-module` flag
        // produce, and it has to keep working uniformly across
        // kernel and user hits.
        assert_eq!(bar.label_with_file(), "libfoo.so:bar_asm+0x20");
    }

    /// CPU-usage binning correctly bucketises per-CPU busy / total
    /// counts and rounds the bin width to a tidy 1/2/5 × 10^k
    /// multiple. Covers the three things that have to be right for
    /// the GUI's time-series strip to read sensibly:
    ///   * `cpu_ids` is the sorted, *sparse* set actually present
    ///     in the data — CPUs the kernel never sampled don't show
    ///     up as empty rows;
    ///   * `busy` and `total` separate idle vs non-idle correctly
    ///     so the renderer's `busy / total` ratio = utilisation;
    ///   * out-of-range bins clamp to the last bin instead of
    ///     panicking on the very last sample.
    #[test]
    fn cpu_usage_bins_buckets_and_rounding() {
        // Eight samples across 9000 us, two CPUs (1 and 4 — sparse to
        // exercise the cpu_ids dedup), one idle each.
        let mk = |ts: u64, cpu: u16, idle: bool| CpuStackSample {
            ts,
            cpu,
            tid: 0,
            pid: 0,
            sample_type: 0,
            flags: if idle { CPU_SAMPLE_FLAG_IDLE } else { 0 },
            freq_mhz: None,
            frames: Vec::new(),
        };
        let samples = vec![
            mk(0, 1, false),
            mk(1_000, 1, false),
            mk(2_500, 1, true), // CPU1 idle in bin 1
            mk(4_000, 1, false),
            mk(0, 4, true), // CPU4 idle in bin 0
            mk(2_500, 4, false),
            mk(8_000, 4, false),
            mk(9_000, 4, false), // last-sample boundary check
        ];
        // target_bins = 4, raw bin width = 9000/4 = 2250 us → round
        // up to 5000 us (next 1/2/5 × 10^k step).
        let bins = compute_cpu_usage_bins(&samples, 4, None, None);
        assert_eq!(bins.bin_us, 5_000);
        // duration / bin_us rounded up == 2 bins.
        assert_eq!(bins.bin_count, 2);
        assert_eq!(bins.cpu_ids, vec![1u16, 4]);
        assert_eq!(bins.t0_us, 0);

        // Bin 0 covers ts in [0, 5000); bin 1 covers [5000, 9000].
        // CPU 1 (idx 0): bin 0 has samples at 0, 1000, 2500, 4000 →
        //   total 4, busy 3 (the 2500 sample is idle).
        //   bin 1 has no CPU 1 samples → total 0, busy 0.
        assert_eq!(bins.cell(0, 0), (3, 4));
        assert_eq!(bins.cell(1, 0), (0, 0));
        // CPU 4 (idx 1): bin 0 has 0 (idle) and 2500 → total 2, busy 1.
        //   bin 1 has 8000 and 9000 (both busy) → total 2, busy 2.
        // The 9000-us sample lands exactly on the boundary; the
        // clamp-to-last-bin path puts it in bin 1.
        assert_eq!(bins.cell(0, 1), (1, 2));
        assert_eq!(bins.cell(1, 1), (2, 2));

        // Aggregate sanity checks.
        assert_eq!(bins.aggregate_busy(0), 4);
        assert_eq!(bins.aggregate_total(0), 6);
        assert_eq!(bins.aggregate_busy(1), 2);
        assert_eq!(bins.aggregate_total(1), 2);

        // Peak per-cell total = 4 (CPU1, bin 0). This is the
        // data-derived "samples per CPU per bin when 100 % busy"
        // denominator that the GUI uses for cpus_busy /
        // cpu_busy_fraction.
        assert_eq!(bins.peak_samples_per_bin, 4);

        // cpus_busy = busy_sum / peak_samples_per_bin, capped at
        // ncpu. Bin 0: 4 / 4 = 1.0 CPUs. Bin 1: 2 / 4 = 0.5 CPUs.
        assert!((bins.cpus_busy(0) - 1.0).abs() < 1e-9);
        assert!((bins.cpus_busy(1) - 0.5).abs() < 1e-9);

        // Per-cpu busy fraction. CPU1 bin 0: busy=3 / peak=4 =
        // 0.75. CPU4 bin 1: busy=2 / peak=4 = 0.5.
        assert!((bins.cpu_busy_fraction(0, 0) - 0.75).abs() < 1e-9);
        assert!((bins.cpu_busy_fraction(1, 1) - 0.5).abs() < 1e-9);

        // Out-of-range cells return zero rather than panicking.
        assert_eq!(bins.cell(99, 99), (0, 0));
        assert_eq!(bins.aggregate_busy(99), 0);
        assert_eq!(bins.cpus_busy(99), 0.0);

        // Empty input produces an empty bins struct.
        let empty = compute_cpu_usage_bins(&[], 100, None, None);
        assert!(empty.is_empty());
        assert_eq!(empty.peak_samples_per_bin, 0);
        assert_eq!(empty.cpus_busy(0), 0.0);
    }

    /// Idle-skipping kernels (post May 2026) emit no samples at all
    /// for idle CPUs, so `busy == total` for every cell. The old
    /// `busy_sum / (total_sum / ncpu)` formulation pinned the
    /// aggregate "CPUs busy" line to `ncpu` regardless of actual
    /// utilisation. The peak-rate denominator fixes that: a CPU
    /// that's busy at half the rate of the busiest CPU should
    /// register as 0.5 CPUs busy, not as a full CPU.
    #[test]
    fn cpus_busy_robust_to_idle_skipping_kernel() {
        let mk = |ts: u64, cpu: u16| CpuStackSample {
            ts,
            tid: 1,
            pid: 1,
            cpu,
            flags: 0, // never set IDLE — simulates idle-skipping kernel
            sample_type: 0,
            freq_mhz: None,
            frames: Vec::new(),
        };
        // 10 ms capture, 2 CPUs.
        //   CPU 0: 4 samples in bin 0, 4 in bin 1 (fully busy both bins)
        //   CPU 1: 2 samples in bin 0, 0 in bin 1 (half-busy bin 0,
        //          idle in bin 1)
        let samples = vec![
            mk(0, 0),
            mk(1_000, 0),
            mk(2_000, 0),
            mk(3_000, 0),
            mk(5_000, 0),
            mk(6_000, 0),
            mk(7_000, 0),
            mk(8_000, 0),
            mk(0, 1),
            mk(2_000, 1),
        ];
        let bins = compute_cpu_usage_bins(&samples, 2, None, None);
        // Expect bin_us = 5000 (raw 5000 rounds up to itself) and
        // 2 bins covering [0, 10000).
        assert_eq!(bins.bin_count, 2);
        assert_eq!(bins.peak_samples_per_bin, 4);
        // Bin 0: CPU0 busy 4/4, CPU1 busy 2/4 → 1.5 CPUs busy.
        assert!((bins.cpus_busy(0) - 1.5).abs() < 1e-9);
        // Bin 1: CPU0 busy 4/4, CPU1 busy 0/4 → 1.0 CPU busy.
        assert!((bins.cpus_busy(1) - 1.0).abs() < 1e-9);
        // Sanity: the old formulation would have pinned both bins
        // to `ncpu = 2` (because every sample is a busy sample →
        // busy_sum == total_sum, and total_per_cpu_avg ==
        // total_sum / ncpu → ratio == ncpu for any non-empty bin).
    }

    /// The empirical-max fallback (`kernel_hz = None`) is an order
    /// statistic over every (bin, cpu) cell, so a single noisy outlier
    /// cell corrupts the calibration for *every other* cell — a real
    /// bug found via EventExplorer's GUI reading ~88% CPU busy for a
    /// window independently confirmed (via flame graph + STARS
    /// slot-gate deny rate) to be at or near 100%. Passing the real
    /// `kernel_hz` fixes this: the denominator becomes a physical
    /// constant, immune to any one cell's jitter.
    #[test]
    fn cpu_usage_bins_hz_denominator_immune_to_outlier_cell() {
        let mk = |ts: u64, cpu: u16| CpuStackSample {
            ts,
            tid: 1,
            pid: 1,
            cpu,
            flags: 0,
            sample_type: 0,
            freq_mhz: None,
            frames: Vec::new(),
        };
        // hz=1000, 10ms bins → 10 samples/bin is "100% busy". CPU 0 is
        // fully busy every bin except one where it happens to emit 12
        // (simulating sampler jitter); CPU 1 is steadily half-busy.
        let mut samples = Vec::new();
        for bin in 0..5u64 {
            let base = bin * 10_000;
            let cpu0_count = if bin == 2 { 12 } else { 10 };
            for i in 0..cpu0_count {
                samples.push(mk(base + i * (10_000 / cpu0_count), 0));
            }
            for i in 0..5 {
                samples.push(mk(base + i * 2_000, 1));
            }
        }

        // With the real hz on record, the outlier bin can't drag down
        // every other bin's reading.
        let bins = compute_cpu_usage_bins(&samples, 5, Some(1000), None);
        assert_eq!(bins.bin_us, 10_000);
        assert_eq!(bins.peak_samples_per_bin, 10);
        for bin in [0, 1, 3, 4] {
            assert!((bins.cpu_busy_fraction(bin, 0) - 1.0).abs() < 1e-9);
            assert!((bins.cpu_busy_fraction(bin, 1) - 0.5).abs() < 1e-9);
        }
        // The outlier bin itself clamps at 1.0 rather than reading 1.2.
        assert!((bins.cpu_busy_fraction(2, 0) - 1.0).abs() < 1e-9);

        // Without hz, the one outlier bin (12 samples) becomes the
        // denominator for the whole timeline, so CPU 0's genuinely
        // fully-busy bins read 10/12 ≈ 83% instead of 100% -- this is
        // the bug.
        let legacy = compute_cpu_usage_bins(&samples, 5, None, None);
        assert_eq!(legacy.peak_samples_per_bin, 12);
        assert!((legacy.cpu_busy_fraction(0, 0) - 10.0 / 12.0).abs() < 1e-9);
    }

    /// Module-only fallback fires when a user PC lands inside a
    /// known [`ProcMap`] but no [`UserSymbolRange`] covers the
    /// resulting file offset — e.g. a `PROC_MAP` whose `offset`
    /// doesn't line up with the `PT_LOAD` the symbol table was
    /// computed against, so the translated offset lands miles below
    /// every entry. Verifies that we still surface the DSO basename
    /// and runtime offset rather than dropping to raw hex, and that
    /// the `BySymbol` aggregation collapses every PC in the DSO to
    /// a single `(map.base, pid)` key.
    #[test]
    fn module_only_fallback_when_symbol_table_missed() {
        let mut profile = CpuProfile::default();
        let user_file_id = 7u32;
        profile.user_files.insert(
            user_file_id,
            UserFile {
                user_file_id,
                build_id: String::new(),
                pathname: "/usr/local/lib/libcksum32.so.0".to_string(),
            },
        );
        // Mirror the mismatch case: this mapping's `offset` is 0,
        // but the symbol table below was computed against a
        // `PT_LOAD` starting at file offset 0x203000.
        profile.proc_maps.insert(
            4456,
            vec![ProcMap {
                pid: 4456,
                user_file_id,
                base: 0x195e_93c7_0000,
                size: 0x8000,
                offset: 0,
            }],
        );
        // Symbol table lives at the actual PT_LOAD's file offset —
        // far above what the mismatched translation produces.
        profile.user_symbols.insert(
            user_file_id,
            vec![UserSymbolRange {
                user_file_id,
                offset: 0x0020_3460,
                size: 0xb9,
                name: "cksum32_find".to_string(),
            }],
        );

        // Translation: pc - base + offset = 0x43d9. Symbol
        // table starts at 0x203460, so partition_point returns 0
        // and the regular User path bails. Our fallback sees the
        // PC is still inside pid 4456's PROC_MAP and surfaces
        // the DSO basename + runtime offset.
        let pc = 0x195e_93c7_43d9u64;
        let hit = profile
            .symbolize(pc, Some(4456))
            .expect("module-only fallback should fire");
        assert!(matches!(hit, SymbolHit::UserModuleOnly { .. }));
        assert_eq!(hit.file_short(), "libcksum32.so.0");
        assert_eq!(hit.inner_offset(), 0x43d9);
        assert_eq!(hit.label(), "libcksum32.so.0+0x43d9");
        // label_with_file collapses to label() for module-only
        // hits — we don't want `libfoo.so:libfoo.so+0x...`.
        assert_eq!(hit.label_with_file(), "libcksum32.so.0+0x43d9");

        // PC outside every known mapping for the pid → fallback
        // must not fire. We can't tell which DSO it came from.
        assert!(profile.symbolize(0x1234_5678_9000, Some(4456)).is_none());

        // Pid with no PROC_MAP at all → fallback can't fire
        // either; we can't even tell which binary the PC belongs
        // to, so callers fall through to the raw-hex tier.
        assert!(profile.symbolize(pc, Some(99999)).is_none());
    }

    /// Kernel resolution wins over user resolution when both
    /// could match — we look up the kernel table first because
    /// most samples on a busy box are kernel-mode and the user
    /// fallback only matters for `USER_MODE`-flagged or
    /// kernel-missed PCs. This test wires up a profile where pid
    /// 1's PROC_MAP would *also* cover the kernel PC if it ran
    /// first, and checks the kernel hit wins.
    #[test]
    fn kernel_resolution_takes_precedence_over_user() {
        let mut profile = CpuProfile::default();
        profile.linker_files.insert(
            7,
            LinkerFile {
                file_id: 7,
                base: 0x8000,
                size: 0x1000,
                flags: LINKER_FILE_FLAG_KERNEL | LINKER_FILE_FLAG_LINKED,
                build_id: String::new(),
                filename: "kernel".to_string(),
                pathname: "/boot/kernel/kernel".to_string(),
            },
        );
        profile.symbols.insert(
            7,
            vec![SymbolRange {
                offset: 0x100,
                size: 0x40,
                name: "tcp_input".to_string(),
            }],
        );
        // A pathological user mapping that happens to cover the
        // same VA range as the kernel image. In practice ASLR
        // keeps these worlds far apart, but the test makes the
        // ordering invariant explicit.
        profile.user_files.insert(
            1,
            UserFile {
                user_file_id: 1,
                build_id: String::new(),
                pathname: "/usr/local/bin/decoy".to_string(),
            },
        );
        profile.proc_maps.insert(
            1,
            vec![ProcMap {
                pid: 1,
                user_file_id: 1,
                base: 0x8000,
                size: 0x1000,
                offset: 0,
            }],
        );
        profile.user_symbols.insert(
            1,
            vec![UserSymbolRange {
                user_file_id: 1,
                offset: 0x100,
                size: 0x40,
                name: "decoy_user_fn".to_string(),
            }],
        );

        let hit = profile
            .symbolize(0x8120, Some(1))
            .expect("kernel range covers 0x8120");
        assert_eq!(hit.name(), "tcp_input");
        assert!(matches!(hit, SymbolHit::Kernel { .. }));
    }
}
