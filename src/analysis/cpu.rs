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

use super::*;
use crate::elog_parser::*;

// ---------------------------------------------------------------------------
// CPU usage time-series (cpu provider)
// ---------------------------------------------------------------------------

/// Per-CPU CPU-utilisation time series, binned at a fixed width.
///
/// The kernel's `cpu` provider takes one sample per CPU per profiler
/// tick (typically 1000 Hz via hwpmc). Current kernels skip idle-thread
/// ticks entirely, so every emitted sample is *busy* and `busy / total`
/// on post-2026-05 captures collapses to 1.0 everywhere a sample
/// landed. Older captures still carry depth-0 idle samples tagged
/// [`CPU_SAMPLE_FLAG_IDLE`]; those are counted toward `total` but not
/// toward `busy` so this series continues to report a meaningful
/// utilisation fraction on archival captures.
///
/// Stored in column-major form (`[bin][cpu_idx]`) because the GUI
/// reads each CPU's series in turn for plotting; that layout is also
/// efficient for computing the aggregate (sum across CPUs per bin)
/// without re-walking the whole structure. CPU-idx is an index into
/// [`Self::cpu_ids`]; the kernel can leave CPU IDs sparse (e.g.
/// CPUs 0,1,4,5 on a NUMA-pinned config) so we don't assume `cpu_id
/// == cpu_idx`.
///
/// Bin width is picked once at construction time based on the total
/// capture duration and the requested bin count — typically a few
/// dozen ms on a 1 s capture, a few hundred ms on a 30 s one. The
/// GUI keeps a single instance cached on the [`FlameViewState`] and
/// rebuilds it only if the bin-count target changes (which it
/// currently doesn't — the target is a constant).
#[derive(Debug, Clone, Default)]
pub struct CpuUsageBins {
    /// Sorted, unique list of CPU IDs that produced samples in the
    /// capture. Index into this when reading [`Self::busy`] /
    /// [`Self::total`].
    pub cpu_ids: Vec<u16>,
    /// Number of bins on the time axis.
    pub bin_count: usize,
    /// Bin width in microseconds. Always > 0.
    pub bin_us: u64,
    /// Capture origin (timestamp of the first sample, in us). The
    /// midpoint of bin `i` is `t0_us + bin_us * (i as u64) +
    /// bin_us / 2`.
    pub t0_us: u64,
    /// Per-bin × per-cpu busy sample count (non-idle samples).
    /// `busy[bin * cpu_ids.len() + cpu_idx]`.
    pub busy: Vec<u32>,
    /// Per-bin × per-cpu total sample count (busy + idle on
    /// archival captures; busy-only on idle-skipping kernels). Same
    /// indexing as [`Self::busy`]; lets the renderer report
    /// utilisation as a fraction without assuming a perfect
    /// `kernel.hz` × `bin_us` rate (which is wrong by ~1% even on
    /// quiet hardware due to clock jitter and dropped ticks).
    pub total: Vec<u32>,
    /// "Samples per CPU per bin when 100 % busy" — the denominator
    /// in [`Self::cpus_busy`] / [`Self::cpu_busy_fraction`]. Derived
    /// from the kernel's reported sampler frequency
    /// (`kernel_hz * bin_us`) when [`compute_cpu_usage_bins`] was
    /// given one; only falls back to the max of `total[cell]` across
    /// every (bin, cpu) pair for captures that lack it. Either way
    /// this is what makes utilisation come out right regardless of
    /// whether the producer emits idle samples (archival firmware,
    /// where `total` includes idle ticks) or skips them (current
    /// firmware, where `busy == total` for every sampled cell) —
    /// without a real denominator, `busy / total` pins to 100 % on
    /// idle-skipping kernels because every cell satisfies
    /// `busy == total` by construction.
    pub peak_samples_per_bin: u32,
    /// Per-bin frequency-scaling multiplier in `(0.0, 1.0]`, for the
    /// optional "scale CPU usage by frequency" display mode. `None`
    /// when no sample in the capture carries a valid `freq_mhz`
    /// (captures predating that field, or hardware without a working
    /// APERF/MPERF pair). When `Some`, each bin is the average
    /// `freq_mhz` of that bin's busy, freq-valid samples divided by a
    /// `reference_freq_mhz` (see that field). Clamped to `1.0` so a
    /// core's rare single-core-turbo excursions above the reference
    /// never make the scaled series exceed the raw one. A bin with
    /// no freq-valid samples of its own defaults to `1.0`.
    pub freq_scale: Option<Vec<f32>>,
    /// The reference frequency (MHz) `freq_scale` is normalised
    /// against. Prefer `CPU_INFO.freq_max_mhz` from the cpu provider
    /// when the capture has it. Otherwise the mean of the highest
    /// 50 MHz bucket that still holds at least 0.1 % of the
    /// capture's busy, freq-valid samples (a stand-in for rated max
    /// on older captures). `None` iff `freq_scale` is `None`.
    pub reference_freq_mhz: Option<u16>,
}

impl CpuUsageBins {
    /// Frequency-scaling multiplier for `bin` (see [`Self::freq_scale`]).
    /// `1.0` (no scaling) when the capture has no frequency data at
    /// all or `bin` is out of range.
    pub fn freq_scale_for(&self, bin: usize) -> f32 {
        self.freq_scale
            .as_ref()
            .and_then(|v| v.get(bin))
            .copied()
            .unwrap_or(1.0)
    }

    /// Re-express [`Self::freq_scale`] against `new_ref` MHz. A zoomed
    /// re-bin computes its own reference from the visible window; the
    /// plot should keep using the capture-wide ceiling so scale-by-freq
    /// stays consistent at every zoom.
    pub fn retarget_freq_reference(&mut self, new_ref: u16) {
        if new_ref == 0 {
            return;
        }
        let Some(old_ref) = self.reference_freq_mhz else {
            return;
        };
        if old_ref == new_ref {
            return;
        }
        if let Some(scale) = self.freq_scale.as_mut() {
            let ratio = old_ref as f32 / new_ref as f32;
            for v in scale.iter_mut() {
                *v = (*v * ratio).min(1.0);
            }
        }
        self.reference_freq_mhz = Some(new_ref);
    }

    /// Convenience: number of CPUs (length of [`Self::cpu_ids`]).
    pub fn ncpus(&self) -> usize {
        self.cpu_ids.len()
    }

    /// True when there are no bins or no CPUs to display.
    pub fn is_empty(&self) -> bool {
        self.bin_count == 0 || self.cpu_ids.is_empty()
    }

    /// Read `(busy, total)` for `(bin, cpu_idx)`. Returns `(0, 0)`
    /// when out of range — saves the renderer a `get`-and-default
    /// dance per cell.
    pub fn cell(&self, bin: usize, cpu_idx: usize) -> (u32, u32) {
        if bin >= self.bin_count || cpu_idx >= self.cpu_ids.len() {
            return (0, 0);
        }
        let i = bin * self.cpu_ids.len() + cpu_idx;
        (self.busy[i], self.total[i])
    }

    /// Sum of `busy` across all CPUs for `bin`. Useful for the
    /// aggregate-mode line.
    pub fn aggregate_busy(&self, bin: usize) -> u64 {
        if bin >= self.bin_count {
            return 0;
        }
        let off = bin * self.cpu_ids.len();
        self.busy[off..off + self.cpu_ids.len()]
            .iter()
            .map(|&v| v as u64)
            .sum()
    }

    /// Sum of `total` across all CPUs for `bin`. Used as the
    /// denominator when normalising aggregate-mode utilisation on
    /// archival captures; the newer [`Self::cpus_busy`] is
    /// preferred for current captures because it tolerates
    /// idle-skipping producers.
    pub fn aggregate_total(&self, bin: usize) -> u64 {
        if bin >= self.bin_count {
            return 0;
        }
        let off = bin * self.cpu_ids.len();
        self.total[off..off + self.cpu_ids.len()]
            .iter()
            .map(|&v| v as u64)
            .sum()
    }

    /// Aggregate "CPUs busy" for `bin`, computed from the
    /// data-derived peak sample rate (see
    /// [`Self::peak_samples_per_bin`]). Saturates at the visible
    /// CPU count so a single noisy bin can't push the value above
    /// the physical core count. Returns `0.0` when the capture is
    /// effectively empty (no samples at all → no peak rate to
    /// normalise against).
    ///
    /// Correct on both archival captures (where `total` includes
    /// idle ticks, so `peak_samples_per_bin` equals the nominal
    /// stathz × bin_us) and current idle-skipping captures (where
    /// `peak_samples_per_bin` equals hwpmc's nominal rate × bin_us
    /// because a fully-busy CPU emits one sample per hwpmc tick,
    /// regardless of thread state). The `busy_sum / (total_sum /
    /// ncpu)` formulation that preceded this method pinned to
    /// `ncpu` on idle-skipping captures because every cell had
    /// `busy == total`.
    pub fn cpus_busy(&self, bin: usize) -> f64 {
        if self.peak_samples_per_bin == 0 {
            return 0.0;
        }
        let busy_sum = self.aggregate_busy(bin) as f64;
        let denom = self.peak_samples_per_bin as f64;
        let raw = busy_sum / denom;
        let cap = self.cpu_ids.len() as f64;
        if raw > cap {
            cap
        } else {
            raw
        }
    }

    /// Per-CPU "fraction busy" for `(bin, cpu_idx)`, computed
    /// from the data-derived peak sample rate. Returns a value in
    /// `[0.0, 1.0]`. See [`Self::cpus_busy`] for why we use
    /// `peak_samples_per_bin` instead of `busy / total`.
    pub fn cpu_busy_fraction(&self, bin: usize, cpu_idx: usize) -> f64 {
        if self.peak_samples_per_bin == 0 {
            return 0.0;
        }
        let (busy, _total) = self.cell(bin, cpu_idx);
        let f = busy as f64 / self.peak_samples_per_bin as f64;
        if f > 1.0 {
            1.0
        } else {
            f
        }
    }
}

/// Compute a binned CPU-utilisation time series from `samples`.
///
/// `target_bins` is a *target* — the actual bin count is at most
/// that, and may be smaller for very short captures (e.g. only 30
/// bins on a 30 ms capture at 1 ms/bin). Bin width is rounded *up*
/// from `duration / target_bins` to a tidy multiple (1, 2, 5, 10,
/// 20, 50, 100, 200, 500 ms, …) so on-screen tick spacing stays
/// readable at any zoom.
///
/// `kernel_hz` is the actual sampler frequency -- [`KernelInfo::sample_hz`],
/// NOT the raw `hz` field (which is the kernel tick rate, not the hwpmc
/// sampler rate on captures where `pmchz` is set independently) -- used
/// to derive [`CpuUsageBins::peak_samples_per_bin`] as `hz * bin_us`
/// when known (see that field's doc for why this replaced a purely
/// data-derived empirical max: the max-of-many-cells approach is
/// biased upward by 10-25%+ at typical GUI zoom levels, worse the
/// finer the bins, because it's an order statistic over
/// `bin_count * ncpu` noisy cells rather than a physical constant).
/// Pass `None` for captures that pre-date `KERNEL_INFO` carrying
/// `hz`, which falls back to the empirical max.
///
/// Idle samples (the per-CPU idle thread running) are still counted
/// in `total` so callers that lack `kernel_hz` still get a usable
/// (if biased) denominator.
///
/// O(samples) — single pass, no sort. The samples vector is already
/// in timestamp order (`parse_elog_core` sorts after merging
/// per-CPU sessions), so we can short-circuit `t0` to `samples[0].ts`
/// and treat any out-of-order sample defensively.
pub fn compute_cpu_usage_bins(
    samples: &[CpuStackSample],
    target_bins: usize,
    kernel_hz: Option<u32>,
    rated_freq_mhz: Option<u16>,
) -> CpuUsageBins {
    compute_cpu_usage_bins_for_cpus(samples, target_bins, kernel_hz, rated_freq_mhz, &[])
}

/// CPU usage for an allowlist (empty means all CPUs). Keep the input
/// slice's time extent so filtering never shifts or shortens the plot.
pub fn compute_cpu_usage_bins_for_cpus(
    samples: &[CpuStackSample],
    target_bins: usize,
    kernel_hz: Option<u32>,
    rated_freq_mhz: Option<u16>,
    cpus: &[u16],
) -> CpuUsageBins {
    if samples.is_empty() || target_bins == 0 {
        return CpuUsageBins::default();
    }
    // Capture extents. The samples vector is already sorted by ts in
    // `parse_elog_core`; defensively take min/max anyway in case a
    // future caller hands us an unsorted slice.
    let mut t0 = samples[0].ts;
    let mut t_end = samples[0].ts;
    for s in samples {
        if s.ts < t0 {
            t0 = s.ts;
        }
        if s.ts > t_end {
            t_end = s.ts;
        }
    }
    // Make sure end > start so the duration is at least 1us. Captures
    // with exactly one sample (or zero spread) get a single 1us bin.
    let duration = t_end.saturating_sub(t0).max(1);

    // Choose a bin width that's at most `duration / target_bins`
    // rounded up to a tidy 1/2/5 × 10^k multiple. This keeps
    // on-screen labels readable as the user resizes the window.
    let raw = (duration as f64) / (target_bins as f64);
    let mut bin_us = nice_bin_us(raw).max(1);
    // Never bin finer than the sampler's own native resolution
    // (1/kernel_hz). Below that period a bin can only ever contain 0
    // or 1 real sample, so its "% busy" is really answering "did a
    // sample happen to land here", not "was the cpu busy for this
    // slice of time" -- e.g. at a 100 Hz sampler (10ms period), 2ms
    // bins have a true expected count of 0.2 samples/bin, but nothing
    // downstream can represent a sub-1 peak, so every bin reads as
    // either 0% or (clamped) 100% and the *average* over many bins
    // collapses toward bin_width / period (~20% here) regardless of
    // how busy the cpu actually was. Flooring bin_us at the sample
    // period keeps zoomed-in views pinned at the sampler's real
    // resolution instead of fabricating sub-sample-period noise. Only
    // applies when we know the real rate; the empirical-max fallback
    // path (no kernel_hz) has no period to floor against.
    if let Some(hz) = kernel_hz.filter(|&hz| hz > 0) {
        let period_us = nice_bin_us((1_000_000.0 / hz as f64).max(1.0));
        bin_us = bin_us.max(period_us);
    }
    let bin_count = duration.div_ceil(bin_us) as usize;

    // Discover the set of CPU IDs that actually appear (sparse on
    // NUMA-pinned configs). Use a u16 sieve since CPU IDs are small.
    let mut seen = [false; 256];
    let mut max_cpu = 0u16;
    for s in samples {
        if (s.cpu as usize) < seen.len() {
            seen[s.cpu as usize] = true;
        }
        if s.cpu > max_cpu {
            max_cpu = s.cpu;
        }
    }
    let cpu_ids: Vec<u16> = (0..=max_cpu)
        .filter(|&c| (c as usize) < seen.len() && seen[c as usize])
        .filter(|c| cpus.is_empty() || cpus.contains(c))
        .collect();
    if cpu_ids.is_empty() {
        // No sampled CPUs matched the selection (or all IDs exceeded
        // MAXCPU). Preserve the time extent without any CPU columns.
        return CpuUsageBins {
            cpu_ids: Vec::new(),
            bin_count,
            bin_us,
            t0_us: t0,
            busy: Vec::new(),
            total: Vec::new(),
            peak_samples_per_bin: 0,
            freq_scale: None,
            reference_freq_mhz: None,
        };
    }
    // Reverse lookup: cpu_id -> cpu_idx into cpu_ids. Tiny since
    // cpu_ids.len() <= MAXCPU.
    let mut cpu_idx = [u16::MAX; 256];
    for (idx, &cid) in cpu_ids.iter().enumerate() {
        cpu_idx[cid as usize] = idx as u16;
    }

    let ncpu = cpu_ids.len();
    let cells = bin_count * ncpu;
    let mut busy = vec![0u32; cells];
    let mut total = vec![0u32; cells];
    // Per-bin freq_mhz accumulators for the optional frequency-scaling
    // display mode (see CpuUsageBins::freq_scale). Only busy,
    // freq-valid samples contribute; a bin's average is independent
    // of which CPU(s) it came from.
    let mut freq_sum = vec![0.0f64; bin_count];
    let mut freq_count = vec![0u32; bin_count];
    // Histogram of busy, freq-valid samples in 50 MHz buckets, used to
    // pick `reference_freq_mhz` when `CPU_INFO.freq_max_mhz` is absent.
    const FREQ_BUCKET_MHZ: u32 = 50;
    const FREQ_REF_MIN_BUCKET_SHARE: f64 = 0.001;
    let mut freq_hist: std::collections::HashMap<u32, (u32, u64)> =
        std::collections::HashMap::new();
    for s in samples {
        let off_us = s.ts.saturating_sub(t0);
        let bin = (off_us / bin_us) as usize;
        let bin = bin.min(bin_count - 1);
        let idx = match cpu_idx.get(s.cpu as usize) {
            Some(&v) if v != u16::MAX => v as usize,
            _ => continue,
        };
        let cell = bin * ncpu + idx;
        total[cell] += 1;
        let is_busy = (s.flags & CPU_SAMPLE_FLAG_IDLE) == 0;
        if is_busy {
            busy[cell] += 1;
        }
        if is_busy && (s.flags & CPU_SAMPLE_FLAG_FREQ_VALID) != 0 {
            if let Some(freq_mhz) = s.freq_mhz {
                freq_sum[bin] += freq_mhz as f64;
                freq_count[bin] += 1;
                let bucket = freq_mhz as u32 / FREQ_BUCKET_MHZ;
                let entry = freq_hist.entry(bucket).or_insert((0, 0));
                entry.0 += 1;
                entry.1 += freq_mhz as u64;
            }
        }
    }
    // Reference = mean of the highest 50 MHz bucket that still holds
    // ≥0.1 % of busy freq-valid samples (the capture's sustained
    // ceiling). If every bucket is below the floor, use the highest
    // bucket that exists.
    let hist_reference: Option<u16> = if freq_hist.is_empty() {
        None
    } else {
        let total_n: u32 = freq_hist.values().map(|&(c, _)| c).sum();
        let min_count = ((total_n as f64) * FREQ_REF_MIN_BUCKET_SHARE)
            .ceil()
            .max(1.0) as u32;
        freq_hist
            .iter()
            .filter(|(_, &(c, _))| c >= min_count)
            .max_by_key(|(&bucket, _)| bucket)
            .or_else(|| freq_hist.iter().max_by_key(|(&bucket, _)| bucket))
            .map(|(_, &(count, sum))| (sum / count as u64) as u16)
    };
    let reference_freq_mhz = rated_freq_mhz.filter(|&f| f > 0).or(hist_reference);
    let freq_scale = reference_freq_mhz.map(|reference| {
        (0..bin_count)
            .map(|bin| {
                if freq_count[bin] > 0 {
                    (((freq_sum[bin] / freq_count[bin] as f64) / reference as f64) as f32).min(1.0)
                } else {
                    1.0
                }
            })
            .collect()
    });

    // "Samples per CPU per bin when 100% busy" -- the denominator
    // every other cell is normalised against. Prefer the kernel's
    // own reported sampler frequency (physical, bin-width-independent)
    // over an empirical max across `total`: the max is an order
    // statistic over `bin_count * ncpu` cells, so it trends upward
    // as bins get smaller/more numerous even though nothing about
    // the real sampling rate changed -- measured at 10-25%+ inflation
    // at typical GUI zoom widths (1-2s bins) vs. a coarse whole-capture
    // bin, which silently caps the displayed "% busy" well below 100%
    // for a genuinely saturated CPU. Round to the nearest sample
    // rather than truncating so a bin width that's an exact multiple
    // of the sample period doesn't get shorted by integer division.
    let peak_samples_per_bin = match kernel_hz {
        Some(hz) if hz > 0 => (((hz as u64) * bin_us + 500_000) / 1_000_000)
            .max(1)
            .min(u32::MAX as u64) as u32,
        // No hz on record (older captures pre-dating that field) --
        // fall back to the empirical max; still directionally useful,
        // just biased at fine zoom as described above.
        _ => total.iter().copied().max().unwrap_or(0),
    };
    CpuUsageBins {
        cpu_ids,
        bin_count,
        bin_us,
        t0_us: t0,
        busy,
        total,
        peak_samples_per_bin,
        freq_scale,
        reference_freq_mhz,
    }
}

/// Per-process binned CPU-sample histogram, the data behind the
/// "Top processes" context strip. Shares its bin layout with a
/// [`CpuUsageBins`] passed at construction time, so the strip
/// stacks pixel-aligned with the CPUs-busy strip rendered above it.
///
/// The "fraction of CPUs busy by pid" interpretation comes from
/// dividing each pid's per-bin sample count by the same
/// `peak_samples_per_bin` denominator [`CpuUsageBins`] uses for
/// `cpus_busy()` — so a stacked-area total of 4 means "4 CPUs
/// worth of work in this bin", and the sum of all process layers
/// approximates the aggregate `CpuUsageBins::cpus_busy(bin)` line.
///
/// Only the top-N pids by total sample count are kept; the rest
/// fold into `other_per_bin` so the strip's stacked area always
/// represents 100 % of busy samples without leaking memory on
/// captures with thousands of short-lived pids.
#[derive(Debug, Clone, Default)]
pub struct ProcessUsageBins {
    /// Capture origin (timestamp of the first sample, in us).
    /// Identical to the matching `CpuUsageBins::t0_us`.
    pub t0_us: u64,
    /// Bin width in microseconds. Identical to the matching
    /// `CpuUsageBins::bin_us`.
    pub bin_us: u64,
    /// Number of bins on the time axis. Identical to the matching
    /// `CpuUsageBins::bin_count`.
    pub bin_count: usize,
    /// Top-N processes by total sample count, sorted descending by
    /// `total_samples` so the renderer can stack the largest at the
    /// bottom (most readable order: the dominant process anchors
    /// the stack base).
    pub processes: Vec<ProcessUsageInfo>,
    /// Per-bin sum of busy samples for *all other* pids that didn't
    /// make the top-N cut. Length = [`Self::bin_count`]. Rendered
    /// as a single grey "Other" stack layer so the visualisation
    /// always represents 100 % of samples without growing
    /// unboundedly with pid count.
    pub other_per_bin: Vec<u32>,
    /// Same `peak_samples_per_bin` as the matching `CpuUsageBins`.
    /// Used as the denominator when converting raw counts to
    /// "CPUs busy" so the strip's y-axis matches the CPUs-busy
    /// strip's y-axis exactly.
    pub peak_samples_per_bin: u32,
    /// Copy of the matching `CpuUsageBins::freq_scale`, so the "Scale
    /// by CPU frequency" toggle applies uniformly to this strip too —
    /// it's per-bin, not per-pid, so every process layer in a bin
    /// gets the same multiplier the aggregate CPUs-busy strip uses.
    pub freq_scale: Option<Vec<f32>>,
}

/// Per-process metadata + per-bin sample histogram. One entry per
/// pid in [`ProcessUsageBins::processes`].
#[derive(Debug, Clone)]
pub struct ProcessUsageInfo {
    pub pid: u32,
    /// Best-effort `comm` (process command name) from any
    /// [`THREAD_CREATE`](EVT_CPU_THREAD_CREATE) event for any tid
    /// of this pid. Empty when the capture started after every
    /// thread of this pid was already running and `elog` didn't
    /// re-snapshot — common on idle-skipping captures of long-
    /// running daemons.
    pub comm: String,
    /// Total busy sample count across the whole capture for this
    /// pid. Used both for top-N selection and for the strip's
    /// hover tooltip ("12,345 samples, 18% of capture").
    pub total_samples: u64,
    /// Per-bin busy sample count for this pid. Length =
    /// [`ProcessUsageBins::bin_count`].
    pub counts_per_bin: Vec<u32>,
}

impl ProcessUsageBins {
    /// True when there are no bins or no processes to display.
    pub fn is_empty(&self) -> bool {
        self.bin_count == 0
            || (self.processes.is_empty() && self.other_per_bin.iter().all(|&v| v == 0))
    }

    /// "CPUs busy by pid" for `(bin, process_idx)`. Returns 0.0
    /// when out of range or when the capture is effectively empty
    /// (no peak rate to normalise against).
    pub fn cpus_busy_by_pid(&self, bin: usize, process_idx: usize) -> f64 {
        if self.peak_samples_per_bin == 0
            || bin >= self.bin_count
            || process_idx >= self.processes.len()
        {
            return 0.0;
        }
        let count = self.processes[process_idx].counts_per_bin[bin] as f64;
        count / self.peak_samples_per_bin as f64
    }

    /// "CPUs busy" for the catch-all "Other" layer. Returns 0.0
    /// when out of range or when the capture is empty.
    pub fn cpus_busy_other(&self, bin: usize) -> f64 {
        if self.peak_samples_per_bin == 0 || bin >= self.bin_count {
            return 0.0;
        }
        self.other_per_bin[bin] as f64 / self.peak_samples_per_bin as f64
    }

    /// Frequency-scaling multiplier for `bin` (see
    /// [`CpuUsageBins::freq_scale_for`]). `1.0` (no scaling) when the
    /// capture has no frequency data or `bin` is out of range.
    pub fn freq_scale_for(&self, bin: usize) -> f32 {
        self.freq_scale
            .as_ref()
            .and_then(|v| v.get(bin))
            .copied()
            .unwrap_or(1.0)
    }
}

/// Compute a per-process binned sample histogram aligned to `grid`.
///
/// The bin layout (t0_us, bin_us, bin_count) and normalisation
/// denominator (peak_samples_per_bin) are taken from `grid` so the
/// resulting strip stacks pixel-aligned with the [`CpuUsageBins`]
/// strip rendered above it. Only the top `top_n` pids (by total
/// busy sample count) get their own per-bin layer; everything else
/// folds into `other_per_bin`.
///
/// Idle samples (`CPU_SAMPLE_FLAG_IDLE`) are excluded from the
/// counts — the strip is "which process was burning CPU when?",
/// and an idle thread per CPU isn't an interesting process here.
/// O(samples + pids × bins).
pub fn compute_process_usage_bins(
    samples: &[CpuStackSample],
    threads: &FxHashMap<u32, ThreadInfo>,
    grid: &CpuUsageBins,
    top_n: usize,
) -> ProcessUsageBins {
    if samples.is_empty() || grid.is_empty() || top_n == 0 {
        return ProcessUsageBins::default();
    }
    let mut selected = vec![false; u16::MAX as usize + 1];
    for &cpu in &grid.cpu_ids {
        selected[cpu as usize] = true;
    }

    // Pass 1: total busy samples per pid (decides top-N).
    let mut totals: FxHashMap<u32, u64> = FxHashMap::default();
    for s in samples {
        if (s.flags & CPU_SAMPLE_FLAG_IDLE) != 0 || !selected[s.cpu as usize] {
            continue;
        }
        *totals.entry(s.pid).or_insert(0) += 1;
    }
    if totals.is_empty() {
        return ProcessUsageBins::default();
    }

    // Top-N by total samples (desc). Tie-break by pid for
    // determinism — important so the same capture renders the
    // same colour bands on every reload.
    let mut ranked: Vec<(u32, u64)> = totals.iter().map(|(&p, &t)| (p, t)).collect();
    ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let kept_n = ranked.len().min(top_n);
    let kept_pids: Vec<u32> = ranked[..kept_n].iter().map(|(p, _)| *p).collect();
    let kept_totals: Vec<u64> = ranked[..kept_n].iter().map(|(_, t)| *t).collect();
    let mut pid_idx: FxHashMap<u32, usize> = FxHashMap::default();
    for (i, &p) in kept_pids.iter().enumerate() {
        pid_idx.insert(p, i);
    }

    // Pass 2: per-bin sample counts.
    let mut counts_per_bin: Vec<Vec<u32>> =
        (0..kept_n).map(|_| vec![0u32; grid.bin_count]).collect();
    let mut other_per_bin = vec![0u32; grid.bin_count];
    for s in samples {
        if (s.flags & CPU_SAMPLE_FLAG_IDLE) != 0 || !selected[s.cpu as usize] {
            continue;
        }
        let off_us = s.ts.saturating_sub(grid.t0_us);
        let bin = ((off_us / grid.bin_us) as usize).min(grid.bin_count - 1);
        match pid_idx.get(&s.pid) {
            Some(&idx) => counts_per_bin[idx][bin] += 1,
            None => other_per_bin[bin] += 1,
        }
    }

    // Lookup comm for each kept pid: first matching ThreadInfo wins.
    // ThreadInfo is keyed by tid, so we scan once.
    let mut comm_by_pid: FxHashMap<u32, &str> = FxHashMap::default();
    for ti in threads.values() {
        if pid_idx.contains_key(&ti.pid) && !ti.comm.is_empty() {
            comm_by_pid.entry(ti.pid).or_insert(ti.comm.as_str());
        }
    }

    let processes: Vec<ProcessUsageInfo> = kept_pids
        .into_iter()
        .zip(kept_totals)
        .zip(counts_per_bin)
        .map(|((pid, total), counts)| ProcessUsageInfo {
            pid,
            comm: comm_by_pid
                .get(&pid)
                .map(|s| s.to_string())
                .unwrap_or_default(),
            total_samples: total,
            counts_per_bin: counts,
        })
        .collect();

    ProcessUsageBins {
        t0_us: grid.t0_us,
        bin_us: grid.bin_us,
        bin_count: grid.bin_count,
        processes,
        other_per_bin,
        peak_samples_per_bin: grid.peak_samples_per_bin,
        freq_scale: grid.freq_scale.clone(),
    }
}

/// Sentinel "pid" values used by [`compute_kernel_user_usage_bins`] to
/// label its two synthetic rows, so the renderer can special-case their
/// colours instead of hashing them like a real pid. Chosen from the top
/// of the `u32` range, far above any real pid.
pub const KERNEL_PSEUDO_PID: u32 = u32::MAX;
pub const USER_PSEUDO_PID: u32 = u32::MAX - 1;

/// Compute a kernel-vs-user binned sample histogram aligned to `grid`,
/// shaped as a [`ProcessUsageBins`] with exactly two synthetic rows
/// (`KERNEL_PSEUDO_PID` / `USER_PSEUDO_PID`) so it can reuse
/// [`ProcessUsageBins`]'s existing stacked-area rendering instead of a
/// new plot type.
///
/// Idle samples are excluded, same as [`compute_process_usage_bins`];
/// every remaining sample is bucketed into "user" or "kernel" via
/// [`CPU_SAMPLE_FLAG_USER_MODE`]. Kernel + user always covers 100% of
/// busy samples, so `other_per_bin` stays all zero.
pub fn compute_kernel_user_usage_bins(
    samples: &[CpuStackSample],
    grid: &CpuUsageBins,
) -> ProcessUsageBins {
    if samples.is_empty() || grid.is_empty() {
        return ProcessUsageBins::default();
    }
    let mut selected = vec![false; u16::MAX as usize + 1];
    for &cpu in &grid.cpu_ids {
        selected[cpu as usize] = true;
    }

    let mut kernel_per_bin = vec![0u32; grid.bin_count];
    let mut user_per_bin = vec![0u32; grid.bin_count];
    let mut kernel_total = 0u64;
    let mut user_total = 0u64;
    for s in samples {
        if (s.flags & CPU_SAMPLE_FLAG_IDLE) != 0 || !selected[s.cpu as usize] {
            continue;
        }
        let off_us = s.ts.saturating_sub(grid.t0_us);
        let bin = ((off_us / grid.bin_us) as usize).min(grid.bin_count - 1);
        if (s.flags & CPU_SAMPLE_FLAG_USER_MODE) != 0 {
            user_per_bin[bin] += 1;
            user_total += 1;
        } else {
            kernel_per_bin[bin] += 1;
            kernel_total += 1;
        }
    }

    ProcessUsageBins {
        t0_us: grid.t0_us,
        bin_us: grid.bin_us,
        bin_count: grid.bin_count,
        processes: vec![
            ProcessUsageInfo {
                pid: KERNEL_PSEUDO_PID,
                comm: "Kernel".to_string(),
                total_samples: kernel_total,
                counts_per_bin: kernel_per_bin,
            },
            ProcessUsageInfo {
                pid: USER_PSEUDO_PID,
                comm: "User".to_string(),
                total_samples: user_total,
                counts_per_bin: user_per_bin,
            },
        ],
        other_per_bin: vec![0u32; grid.bin_count],
        peak_samples_per_bin: grid.peak_samples_per_bin,
        freq_scale: grid.freq_scale.clone(),
    }
}

/// Per-process rollup used by the Processes tab — one row per pid
/// observed in the cpu provider's sample stream. All fields are
/// derived from `CpuProfile`'s already-parsed data; no rescan of
/// the original event stream is needed.
#[derive(Debug, Clone)]
pub struct ProcessSummary {
    pub pid: u32,
    /// Best-effort `comm` from any `THREAD_CREATE` event of any
    /// tid of this pid. Empty when the capture started after every
    /// thread of this pid was already running.
    pub comm: String,
    /// Total CPU samples (busy + idle) attributed to this pid.
    pub sample_count: u64,
    /// Busy-only sample count (excludes `CPU_SAMPLE_FLAG_IDLE`).
    /// Used as the basis for [`Self::pct_cpu`].
    pub busy_sample_count: u64,
    /// Share of all post-filter busy samples that belong to this
    /// pid, expressed as a percentage in `[0, 100]`. Read as
    /// "what fraction of CPU time on the box was this pid?".
    pub pct_cpu: f64,
    /// Distinct tids observed for this pid (across the whole
    /// capture, from samples + `THREAD_CREATE`).
    pub thread_count: usize,
    /// Distinct DSOs (user-space modules) referenced by this pid's
    /// `PROC_MAP` entries. `0` for kernel-only pids and for pids
    /// where `elog -U` didn't snapshot the address space in time.
    pub module_count: usize,
    /// Timestamp (us) of the first observed sample for this pid.
    pub first_ts_us: u64,
    /// Timestamp (us) of the last observed sample for this pid.
    pub last_ts_us: u64,
    /// TCP session ids owned by this pid. Populated only for pids
    /// matching a [`SessionSummary::owner_pid`] in the loaded
    /// capture; empty when no session was attributed to this pid
    /// (which is every pid until the kernel-side
    /// `EVT_CONN_OWNER_PID` event lands and the parser fills
    /// `owner_pid` on the matching summaries).
    pub owned_session_ids: Vec<u64>,
}

/// Compute a per-process rollup from a [`CpuProfile`]. Sorted
/// descending by `busy_sample_count` (so the largest CPU
/// consumers come first), tie-broken on `pid` for deterministic
/// ordering across reloads.
///
/// `summaries` is the optional TCP session list. When provided,
/// each [`ProcessSummary::owned_session_ids`] is populated from
/// the matching [`SessionSummary::owner_pid`] entries. Pass an
/// empty slice when no TCP data is available (cpu-only captures)
/// or when the cross-reference isn't needed.
///
/// O(samples + threads + maps + summaries) — single pass over
/// each input vector. Cheap enough to recompute on demand from
/// the GUI.
pub fn compute_process_summaries(
    profile: &CpuProfile,
    summaries: &[SessionSummary],
) -> Vec<ProcessSummary> {
    compute_process_summaries_between(profile, summaries, 0, u64::MAX)
}

/// [`compute_process_summaries`] restricted to samples whose `ts`
/// falls in `[ts_start_us, ts_end_us]` (inclusive). `ts_end_us` of
/// `u64::MAX` runs through the end of the capture. The full-range
/// call also folds in `THREAD_CREATE` tids and session-owner pids
/// that have no samples.
pub fn compute_process_summaries_between(
    profile: &CpuProfile,
    summaries: &[SessionSummary],
    ts_start_us: u64,
    ts_end_us: u64,
) -> Vec<ProcessSummary> {
    if profile.samples.is_empty() {
        return Vec::new();
    }
    let full_range = ts_start_us == 0 && ts_end_us == u64::MAX;

    // Per-pid sample counters + first/last timestamps.
    struct Acc {
        sample_count: u64,
        busy_sample_count: u64,
        first_ts_us: u64,
        last_ts_us: u64,
        tids: FxHashSet<u32>,
    }
    let mut by_pid: FxHashMap<u32, Acc> = FxHashMap::default();
    let mut total_busy: u64 = 0;
    for s in &profile.samples {
        if s.ts < ts_start_us || s.ts > ts_end_us {
            continue;
        }
        let acc = by_pid.entry(s.pid).or_insert_with(|| Acc {
            sample_count: 0,
            busy_sample_count: 0,
            first_ts_us: s.ts,
            last_ts_us: s.ts,
            tids: FxHashSet::default(),
        });
        acc.sample_count += 1;
        acc.tids.insert(s.tid);
        if s.ts < acc.first_ts_us {
            acc.first_ts_us = s.ts;
        }
        if s.ts > acc.last_ts_us {
            acc.last_ts_us = s.ts;
        }
        if (s.flags & CPU_SAMPLE_FLAG_IDLE) == 0 {
            acc.busy_sample_count += 1;
            total_busy += 1;
        }
    }

    // Bring in tids from THREAD_CREATE for pids we already saw — a
    // pid that only had idle samples in the window still gets its
    // full thread roster reflected in thread_count. A caller-supplied
    // time window counts threads that were actually sampled in it.
    if full_range {
        for ti in profile.threads.values() {
            if let Some(acc) = by_pid.get_mut(&ti.pid) {
                acc.tids.insert(ti.tid);
            }
        }
    }

    // Best-effort comm lookup: first matching THREAD_CREATE wins.
    let mut comm_by_pid: FxHashMap<u32, &str> = FxHashMap::default();
    for ti in profile.threads.values() {
        if by_pid.contains_key(&ti.pid) && !ti.comm.is_empty() {
            comm_by_pid.entry(ti.pid).or_insert(ti.comm.as_str());
        }
    }

    // Build pid -> Vec<session_id> from the SessionSummary list
    // when callers pass one. Today every owner_pid is `None`
    // (kernel-side event not implemented); this loop is a no-op
    // until the producer lands the change.
    let mut owned_by_pid: FxHashMap<u32, Vec<u64>> = FxHashMap::default();
    for s in summaries {
        if let Some(pid) = s.owner_pid {
            owned_by_pid.entry(pid).or_default().push(s.session_id);
        }
    }
    // Some pids own sessions but never showed up in the cpu-sample
    // stream (e.g. a TCP daemon that was idle the whole capture).
    // Fold those in as zero-sample rows so the Processes tab can
    // still show their session ownership. A time window only lists
    // pids that were sampled inside it.
    if full_range {
        for &pid in owned_by_pid.keys() {
            by_pid.entry(pid).or_insert_with(|| Acc {
                sample_count: 0,
                busy_sample_count: 0,
                first_ts_us: 0,
                last_ts_us: 0,
                tids: FxHashSet::default(),
            });
        }
    }

    let total_busy_f = total_busy.max(1) as f64;
    let mut rows: Vec<ProcessSummary> = by_pid
        .into_iter()
        .map(|(pid, acc)| {
            let module_count = profile.proc_maps.get(&pid).map(|m| m.len()).unwrap_or(0);
            let mut owned = owned_by_pid.remove(&pid).unwrap_or_default();
            owned.sort_unstable();
            ProcessSummary {
                pid,
                comm: comm_by_pid
                    .get(&pid)
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                sample_count: acc.sample_count,
                busy_sample_count: acc.busy_sample_count,
                pct_cpu: 100.0 * acc.busy_sample_count as f64 / total_busy_f,
                thread_count: acc.tids.len(),
                module_count,
                first_ts_us: acc.first_ts_us,
                last_ts_us: acc.last_ts_us,
                owned_session_ids: owned,
            }
        })
        .collect();
    rows.sort_unstable_by(|a, b| {
        b.busy_sample_count
            .cmp(&a.busy_sample_count)
            .then(a.pid.cmp(&b.pid))
    });
    rows
}

/// One Processes-tab grouped row. Same-name pids share a row; empty
/// `comm` folds into one "(unknown)" group.
#[derive(Debug, Clone)]
pub struct ProcessGroupSummary {
    pub comm: String,
    pub pids: Vec<u32>,
    pub sample_count: u64,
    pub busy_sample_count: u64,
    pub pct_cpu: f64,
    pub thread_count: usize,
    pub module_count: usize,
    pub first_ts_us: u64,
    pub last_ts_us: u64,
    pub owned_session_ids: Vec<u64>,
}

/// Fold per-pid [`ProcessSummary`] rows onto one row per `comm`.
/// `module_count` is the number of distinct `user_file_id`s across
/// the group's `PROC_MAP`s. Sorted descending by busy samples,
/// then `comm`.
pub fn group_process_summaries_by_comm(
    rows: &[ProcessSummary],
    profile: &CpuProfile,
) -> Vec<ProcessGroupSummary> {
    struct Acc {
        pids: Vec<u32>,
        sample_count: u64,
        busy_sample_count: u64,
        thread_count: usize,
        first_ts_us: u64,
        last_ts_us: u64,
        owned_session_ids: Vec<u64>,
    }
    let mut by_comm: FxHashMap<String, Acc> = FxHashMap::default();
    for r in rows {
        let acc = by_comm.entry(r.comm.clone()).or_insert_with(|| Acc {
            pids: Vec::new(),
            sample_count: 0,
            busy_sample_count: 0,
            thread_count: 0,
            first_ts_us: r.first_ts_us,
            last_ts_us: r.last_ts_us,
            owned_session_ids: Vec::new(),
        });
        acc.pids.push(r.pid);
        acc.sample_count += r.sample_count;
        acc.busy_sample_count += r.busy_sample_count;
        acc.thread_count += r.thread_count;
        if r.first_ts_us > 0 && (acc.first_ts_us == 0 || r.first_ts_us < acc.first_ts_us) {
            acc.first_ts_us = r.first_ts_us;
        }
        if r.last_ts_us > acc.last_ts_us {
            acc.last_ts_us = r.last_ts_us;
        }
        acc.owned_session_ids
            .extend_from_slice(&r.owned_session_ids);
    }
    let total_busy: u64 = rows.iter().map(|r| r.busy_sample_count).sum();
    let total_busy_f = total_busy.max(1) as f64;
    let mut groups: Vec<ProcessGroupSummary> = by_comm
        .into_iter()
        .map(|(comm, mut acc)| {
            acc.pids.sort_unstable();
            acc.owned_session_ids.sort_unstable();
            acc.owned_session_ids.dedup();
            let mut files = FxHashSet::default();
            for &pid in &acc.pids {
                if let Some(maps) = profile.proc_maps.get(&pid) {
                    for m in maps {
                        files.insert(m.user_file_id);
                    }
                }
            }
            ProcessGroupSummary {
                comm,
                sample_count: acc.sample_count,
                busy_sample_count: acc.busy_sample_count,
                pct_cpu: 100.0 * acc.busy_sample_count as f64 / total_busy_f,
                thread_count: acc.thread_count,
                module_count: files.len(),
                first_ts_us: acc.first_ts_us,
                last_ts_us: acc.last_ts_us,
                owned_session_ids: acc.owned_session_ids,
                pids: acc.pids,
            }
        })
        .collect();
    groups.sort_unstable_by(|a, b| {
        b.busy_sample_count
            .cmp(&a.busy_sample_count)
            .then(a.comm.cmp(&b.comm))
    });
    groups
}

/// Fold per-pid rows the same way the flame's **Group by process**
/// bar does: user pids group by `comm`, kernel threads (idle,
/// ithreads, swi, pid 0) collapse onto one [`crate::analysis::KERNEL_GROUP_LABEL`]
/// row.
pub fn group_process_summaries_by_flame(
    rows: &[ProcessSummary],
    profile: &CpuProfile,
) -> Vec<ProcessGroupSummary> {
    // Same grouping tables as `build_flame_tree`, one pass over the
    // sample stream for every pid in the table.
    let user_pids = super::flame::collect_user_pids(profile);
    let relabeled: Vec<ProcessSummary> = rows
        .iter()
        .cloned()
        .map(|mut r| {
            let mut label =
                super::flame::process_group_label_for_pid(r.pid, None, &user_pids, profile);
            if label == format!("pid {}", r.pid) {
                label = crate::analysis::KERNEL_GROUP_LABEL.to_string();
            }
            r.comm = label;
            r
        })
        .collect();
    group_process_summaries_by_comm(&relabeled, profile)
}

/// Round `raw_us` up to a tidy 1/2/5 × 10^k value (in microseconds).
/// Used by [`compute_cpu_usage_bins`] to pick a bin width whose
/// boundaries land on round numbers, so the on-screen x-axis ticks
/// read as 0.5 s / 1 s / 5 s / … rather than 0.467 s / 0.934 s.
pub fn nice_bin_us(raw_us: f64) -> u64 {
    if !raw_us.is_finite() || raw_us <= 0.0 {
        return 1;
    }
    let exp = raw_us.log10().floor() as i32;
    let scale = 10f64.powi(exp);
    let frac = raw_us / scale;
    let nice = if frac <= 1.0 {
        1.0
    } else if frac <= 2.0 {
        2.0
    } else if frac <= 5.0 {
        5.0
    } else {
        10.0
    };
    (nice * scale).ceil() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        pid: u32,
        comm: &str,
        busy: u64,
        threads: usize,
        first: u64,
        last: u64,
    ) -> ProcessSummary {
        ProcessSummary {
            pid,
            comm: comm.to_string(),
            sample_count: busy,
            busy_sample_count: busy,
            pct_cpu: 0.0,
            thread_count: threads,
            module_count: 0,
            first_ts_us: first,
            last_ts_us: last,
            owned_session_ids: Vec::new(),
        }
    }

    #[test]
    fn process_summaries_time_window_drops_samples_outside() {
        let mut early = busy_sample(100, 0, 0);
        early.pid = 1;
        early.tid = 10;
        let mut mid = busy_sample(200, 0, 0);
        mid.pid = 1;
        mid.tid = 11;
        let mut late = busy_sample(500, 0, 0);
        late.pid = 2;
        late.tid = 20;
        let mut profile = CpuProfile {
            samples: vec![early, mid, late],
            ..CpuProfile::default()
        };
        profile.threads.insert(
            99,
            ThreadInfo {
                tid: 99,
                pid: 1,
                comm: "perl".to_string(),
                tdname: String::new(),
            },
        );
        let all = compute_process_summaries(&profile, &[]);
        assert_eq!(all[0].pid, 1);
        assert_eq!(all[0].busy_sample_count, 2);
        assert_eq!(all[0].thread_count, 3);
        let window = compute_process_summaries_between(&profile, &[], 150, 200);
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].pid, 1);
        assert_eq!(window[0].busy_sample_count, 1);
        assert_eq!(window[0].thread_count, 1);
        assert_eq!(window[0].first_ts_us, 200);
        assert_eq!(window[0].last_ts_us, 200);
        assert!(compute_process_summaries_between(&profile, &[], 201, 499).is_empty());
    }

    #[test]
    fn group_by_comm_merges_same_name() {
        let rows = vec![
            row(100, "perl", 10, 1, 1000, 2000),
            row(200, "perl", 5, 1, 500, 3000),
            row(300, "nginx", 20, 2, 0, 4000),
        ];
        let groups = group_process_summaries_by_comm(&rows, &CpuProfile::default());
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].comm, "nginx");
        assert_eq!(groups[0].busy_sample_count, 20);
        assert_eq!(groups[1].comm, "perl");
        assert_eq!(groups[1].pids, vec![100, 200]);
        assert_eq!(groups[1].busy_sample_count, 15);
        assert_eq!(groups[1].thread_count, 2);
        assert_eq!(groups[1].first_ts_us, 500);
        assert_eq!(groups[1].last_ts_us, 3000);
    }

    #[test]
    fn group_by_comm_folds_empty_comm() {
        let rows = vec![row(1, "", 3, 1, 1, 2), row(2, "", 1, 1, 3, 4)];
        let groups = group_process_summaries_by_comm(&rows, &CpuProfile::default());
        assert_eq!(groups.len(), 1);
        assert!(groups[0].comm.is_empty());
        assert_eq!(groups[0].pids, vec![1, 2]);
        assert_eq!(groups[0].busy_sample_count, 4);
    }

    #[test]
    fn group_by_flame_merges_kernel_threads() {
        let rows = vec![
            row(11, "idle", 10, 4, 0, 100),
            row(12, "intr", 5, 8, 0, 100),
            row(13, "clock", 2, 1, 0, 50),
            row(99, "", 3, 1, 0, 40),
            row(9104, "ella_server", 7, 2, 0, 100),
        ];
        let profile = CpuProfile {
            samples: vec![crate::elog_parser::CpuStackSample {
                ts: 0,
                cpu: 0,
                tid: 1,
                pid: 9104,
                sample_type: 0,
                flags: crate::elog_parser::CPU_SAMPLE_FLAG_USER_MODE,
                freq_mhz: None,
                frames: vec![0x4000_0000],
            }],
            threads: [(
                1,
                crate::elog_parser::ThreadInfo {
                    tid: 1,
                    pid: 9104,
                    comm: "ella_server".to_string(),
                    tdname: String::new(),
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let groups = group_process_summaries_by_flame(&rows, &profile);
        let labels: Vec<&str> = groups.iter().map(|g| g.comm.as_str()).collect();
        assert!(labels.contains(&crate::analysis::KERNEL_GROUP_LABEL));
        assert!(labels.contains(&"ella_server"));
        assert!(!labels.contains(&"idle"));
        assert!(!labels.contains(&"intr"));
        let kern = groups
            .iter()
            .find(|g| g.comm == crate::analysis::KERNEL_GROUP_LABEL)
            .unwrap();
        assert_eq!(kern.busy_sample_count, 20);
        assert_eq!(kern.pids, vec![11, 12, 13, 99]);
    }

    #[test]
    fn group_by_flame_one_pass_over_samples() {
        // 4k pids and 20k samples share one sample-stream scan.
        let n_pid = 4_000u32;
        let n_samp = 20_000usize;
        let frames: Vec<u64> = (0..8).map(|i| 0x1000 + i * 0x10).collect();
        let samples: Vec<crate::elog_parser::CpuStackSample> = (0..n_samp)
            .map(|i| {
                let pid = 1_000 + (i as u32 % n_pid);
                crate::elog_parser::CpuStackSample {
                    ts: i as u64,
                    cpu: 0,
                    tid: pid,
                    pid,
                    sample_type: 0,
                    flags: 0,
                    freq_mhz: None,
                    frames: frames.clone(),
                }
            })
            .collect();
        let rows: Vec<ProcessSummary> = (0..n_pid)
            .map(|i| row(1_000 + i, "worker", 1, 1, 0, 1))
            .collect();
        let threads = (0..n_pid)
            .map(|i| {
                let pid = 1_000 + i;
                (
                    pid,
                    crate::elog_parser::ThreadInfo {
                        tid: pid,
                        pid,
                        comm: "worker".into(),
                        tdname: String::new(),
                    },
                )
            })
            .collect();
        let profile = CpuProfile {
            samples,
            threads,
            ..Default::default()
        };
        let t0 = std::time::Instant::now();
        let groups = group_process_summaries_by_flame(&rows, &profile);
        assert!(
            t0.elapsed().as_secs() < 2,
            "Processes grouping took {:?}",
            t0.elapsed()
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].comm, "worker");
        assert_eq!(groups[0].pids.len(), n_pid as usize);
    }

    fn freq_sample(ts: u64, freq_mhz: u16) -> CpuStackSample {
        CpuStackSample {
            ts,
            cpu: 0,
            tid: 1,
            pid: 1,
            sample_type: 1,
            flags: CPU_SAMPLE_FLAG_FREQ_VALID,
            freq_mhz: Some(freq_mhz),
            frames: Vec::new(),
        }
    }

    #[test]
    fn freq_scale_uses_sustained_peak_not_mode() {
        // ELLA-like: 99% at 1500 MHz (the mode), 1% at 2200 MHz.
        // 1% clears the 0.1% floor, so 2200 is the ceiling.
        let mut samples = Vec::with_capacity(10_000);
        for i in 0..9_900 {
            samples.push(freq_sample(i as u64 * 1_000, 1500));
        }
        for i in 9_900..10_000 {
            samples.push(freq_sample(i as u64 * 1_000, 2200));
        }
        let bins = compute_cpu_usage_bins(&samples, 1, Some(1000), None);
        assert_eq!(bins.reference_freq_mhz, Some(2200));
        let mean = (9_900.0 * 1500.0 + 100.0 * 2200.0) / 10_000.0;
        let expected = (mean / 2200.0) as f32;
        assert!(
            (bins.freq_scale_for(0) - expected).abs() < 1e-4,
            "scale={} expected={}",
            bins.freq_scale_for(0),
            expected
        );
    }

    #[test]
    fn freq_scale_ignores_tiny_turbo_tail() {
        // 99.95% at 2800, 0.05% at 3400 — below the 0.1% floor.
        let mut samples = Vec::with_capacity(10_000);
        for i in 0..9_995 {
            samples.push(freq_sample(i as u64 * 1_000, 2800));
        }
        for i in 9_995..10_000 {
            samples.push(freq_sample(i as u64 * 1_000, 3400));
        }
        let bins = compute_cpu_usage_bins(&samples, 1, Some(1000), None);
        assert_eq!(bins.reference_freq_mhz, Some(2800));
        assert!((bins.freq_scale_for(0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn retarget_freq_reference_rescales_bins() {
        let samples: Vec<_> = (0..100)
            .map(|i| freq_sample(i as u64 * 1_000, 1500))
            .collect();
        let mut bins = compute_cpu_usage_bins(&samples, 1, Some(1000), None);
        assert_eq!(bins.reference_freq_mhz, Some(1500));
        assert!((bins.freq_scale_for(0) - 1.0).abs() < 1e-4);
        bins.retarget_freq_reference(3000);
        assert_eq!(bins.reference_freq_mhz, Some(3000));
        assert!((bins.freq_scale_for(0) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn freq_scale_uses_cpu_info_max_when_present() {
        // Quiet capture: every sample at 1500 MHz. Histogram reference
        // would be 1500 (scale 1.0); rated max 3000 must win.
        let samples: Vec<_> = (0..100)
            .map(|i| freq_sample(i as u64 * 1_000, 1500))
            .collect();
        let bins = compute_cpu_usage_bins(&samples, 1, Some(1000), Some(3000));
        assert_eq!(bins.reference_freq_mhz, Some(3000));
        assert!((bins.freq_scale_for(0) - 0.5).abs() < 1e-4);
    }

    fn busy_sample(ts: u64, cpu: u16, flags: u8) -> CpuStackSample {
        CpuStackSample {
            ts,
            cpu,
            tid: 1,
            pid: 1,
            sample_type: 1,
            flags,
            freq_mhz: None,
            frames: Vec::new(),
        }
    }

    #[test]
    fn kernel_user_bins_sum_to_aggregate_cpus_busy() {
        let mut samples = Vec::new();
        for i in 0..200 {
            let ts = i as u64 * 1_000;
            let idle = if i % 5 == 0 { CPU_SAMPLE_FLAG_IDLE } else { 0 };
            let user = if i % 3 == 0 {
                CPU_SAMPLE_FLAG_USER_MODE
            } else {
                0
            };
            samples.push(busy_sample(ts, 0, idle | user));
            samples.push(busy_sample(
                ts,
                1,
                if i % 4 == 0 { CPU_SAMPLE_FLAG_IDLE } else { 0 },
            ));
        }
        let grid = compute_cpu_usage_bins(&samples, 20, Some(1000), None);
        let ku = compute_kernel_user_usage_bins(&samples, &grid);
        assert_eq!(ku.bin_count, grid.bin_count);
        assert_eq!(ku.bin_us, grid.bin_us);
        for bin in 0..grid.bin_count {
            let stacked = ku.cpus_busy_by_pid(bin, 0) + ku.cpus_busy_by_pid(bin, 1);
            let agg = grid.cpus_busy(bin);
            assert!(
                (stacked - agg).abs() < 1e-9,
                "bin {bin}: kernel+user={stacked} aggregate={agg}"
            );
        }
    }

    #[test]
    fn zoomed_kernel_user_follows_zoomed_cpu_grid() {
        let samples: Vec<_> = (0..10_000)
            .map(|i| {
                let flags = if i % 2 == 0 {
                    CPU_SAMPLE_FLAG_USER_MODE
                } else {
                    0
                };
                busy_sample(i as u64 * 1_000, 0, flags)
            })
            .collect();
        let full = compute_cpu_usage_bins(&samples, 20, Some(1000), None);
        let slice = &samples[4_000..5_000];
        let zoomed = compute_cpu_usage_bins(slice, 20, Some(1000), None);
        assert!(zoomed.bin_us < full.bin_us);
        let ku = compute_kernel_user_usage_bins(slice, &zoomed);
        assert_eq!(ku.bin_us, zoomed.bin_us);
        assert_eq!(ku.bin_count, zoomed.bin_count);
    }
}
