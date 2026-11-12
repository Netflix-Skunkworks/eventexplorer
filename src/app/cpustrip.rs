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

/// View-mode toggle for the CPU-usage time-series strip in the flame
/// tab. Aggregate is the default because it answers the most common
/// question ("when was the box busy?") in one glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuUsageViewMode {
    /// One line summing busy fractions across every CPU. Y-axis runs
    /// 0..ncpu (so an 8-CPU box maxes out around 8.0). Reads as
    /// "total CPUs busy".
    Aggregate,
    /// One stacked area per CPU, colour-coded by CPU id. Y-axis runs
    /// 0..ncpu. Useful for spotting per-CPU imbalances (one CPU
    /// pegged while the others sleep).
    PerCpu,
}

/// Self-contained, view-agnostic CPU-busy time-series strip.
///
/// Owns its data (`CpuUsageBins`), view-mode toggle, and in-progress
/// drag-to-select state. Renders into any host `Ui` and emits
/// [`SelectionChange`] events when the user drags a new selection or
/// right-clicks to clear; the host decides what the selection *means*
/// (a flame-tab time-range filter, a per-session `view_x`, an
/// aggregate-tab time slice, etc.). This is the first concrete
/// "context strip" — see Path A of the multi-provider integration
/// plan in `docs/architecture.md`.
pub struct CpuBusyStripView {
    /// Bins across the *whole* capture, at `target_bins` resolution.
    /// Used as-is when the host's view covers the full capture; source
    /// data for [`Self::effective_bins`]'s zoomed re-bin otherwise.
    pub(crate) bins: analysis::CpuUsageBins,
    /// Backing samples for [`Self::bins`], kept around so zooming in
    /// can re-bin just the visible window instead of stretching the
    /// same full-capture bins wider. Arc'd (shared with the app's
    /// `cpu_profile`) so this doesn't clone tens of millions of
    /// samples per strip.
    pub(crate) profile: Arc<CpuProfile>,
    /// CPU allowlist shared with Flame; empty means all CPUs.
    cpus: Vec<u16>,
    /// `target_bins` this strip was built with — reused for the
    /// zoomed re-bin so resolution stays consistent with the
    /// full-capture view.
    pub(crate) target_bins: usize,
    /// Cached zoomed re-bin: `(view_us, bins)`. Recomputed only when
    /// the host's view range moves by more than ~1% (see
    /// `effective_bins`), so a static view doesn't re-bin every frame.
    pub(crate) zoom_cache: Option<((u64, u64), analysis::CpuUsageBins)>,
    /// Aggregate (sum across CPUs) vs per-CPU stacking on the
    /// time-series plot. Affects only visualisation — the host's
    /// time-range selection is independent of this toggle.
    pub(crate) view_mode: CpuUsageViewMode,
    /// Active drag on the strip. Resize is a new time window; Translate
    /// is Shift+drag sliding the current window.
    pub(crate) drag: Option<StripTimeDrag>,
    /// Whether `drag` is editing the alt (region A) band: Alt was held
    /// when the drag started.
    pub(crate) drag_is_alt: bool,
    /// Last-frame cursor-tracking metadata. Saved at the end of
    /// `draw()` so the host (today: per-session view) can paint
    /// a shared cursor line over the strip, paint a tracking dot
    /// on the strip’s data line, and add the strip’s value to
    /// the shared tooltip — the same UX the per-session panels
    /// already provide for the RTT / Rate / Send State series.
    /// `None` until the strip has been rendered at least once.
    pub(crate) last_rect: Option<egui::Rect>,
    pub(crate) last_transform: Option<egui_plot::PlotTransform>,
    /// Strip data origin in host-relative seconds (the screen
    /// position where bin 0 starts). Lets `cursor_lookup` map a
    /// host-relative `data_x_s` back to a bin index without re-
    /// receiving `host_t0_us`.
    pub(crate) last_strip_data_lo_s: Option<f64>,
    /// Plot height in logical px. Defaults to the compact flame-tab
    /// strip height; the Aggregate tab overrides it a little taller.
    pub(crate) plot_height: f32,
    /// Rotated y-axis title. Defaults to "CPUs busy"; the Aggregate
    /// tab shows utilisation as a percentage and labels it "% busy".
    pub(crate) y_label: String,
    /// When true, plot utilisation as a percentage of the box's total
    /// CPU capacity (`cpus_busy / ncpu * 100`) instead of a raw
    /// CPUs-busy count. Off for the flame / per-session strips.
    pub(crate) as_percent: bool,
    /// X-axis title. `Some` (default) shows a built-in "Time (seconds)"
    /// caption; `None` (Aggregate tab) drops the title and uses the
    /// shared [`agg_x_time_axis`] tick style so the strip matches the
    /// other Aggregate panels (no title, no flush-left "0").
    pub(crate) x_axis_title: Option<String>,
    /// Mirror of the app-wide "scale CPU usage by frequency" toggle,
    /// synced by the host right before each `draw()` / `cursor_lookup()`
    /// call. When true, every busy value read from [`Self::bins`] is
    /// multiplied by [`analysis::CpuUsageBins::freq_scale_for`] for that
    /// bin -- a no-op (multiplier `1.0`) on captures with no frequency
    /// data. Off by default: the underlying counts are already raw/
    /// unscaled, this only changes what gets *displayed*.
    pub(crate) scale_by_freq: bool,
    /// Font size of the rotated y-axis label. `max_digits`/`y_axis_width`
    /// alone don't guarantee the strip's plot area lines up with the
    /// panels stacked below it — the label's font size also sets the
    /// gutter's width, so it must match whatever those panels use for
    /// their own y-axis label (12.0 for the Aggregate tab's `agg_y_axis`
    /// panels, 14.0 for the per-session RTT/Rate/etc panels).
    pub(crate) y_axis_label_size: f32,
    pub(crate) show_legends: bool,
    /// After the 8px top inset, size the Plot to leftover height
    /// instead of [`Self::plot_height`]. Used when the catalog row is
    /// maximized so the inset cannot overflow a fill-remaining slot.
    pub(crate) fill_remaining: bool,
    /// egui_plot widget id; the host sets this per tab before `draw`.
    pub(crate) plot_id: String,
}

/// Result of a frame's user interaction with a [`CpuBusyStripView`]
/// (or any future [`ContextStrip`]). The host applies the change to
/// whichever piece of state the selection drives in that view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionChange {
    /// User finished a drag-to-select and committed a new
    /// `(start_us, end_us)` time range, in absolute capture
    /// timestamps.
    Set(u64, u64),
    /// User right-clicked the strip; clear any active selection.
    Clear,
}

/// Band fills. Two-band mode is the Flame tab's same-file region
/// compare: A (Alt+drag) is orange, B (plain drag) is blue.
/// A single band keeps the historical blue.
const REGION_A_FILL: Color32 = Color32::from_rgba_premultiplied(235, 100, 45, 46);
const REGION_B_FILL: Color32 = Color32::from_rgba_premultiplied(70, 130, 235, 46);
const SINGLE_BAND_FILL: Color32 = Color32::from_rgba_premultiplied(100, 100, 255, 40);

/// Selection bands for a strip draw, in absolute capture microseconds:
/// the host's primary time-range selection, plus an optional
/// comparison ("alt") band — region A of the Flame tab's same-file
/// region compare. The alt band is edited by Alt+drag /
/// Alt+right-click; the primary band keeps plain-drag / right-click.
#[derive(Clone, Copy, Default)]
pub(crate) struct StripSelections {
    pub(crate) primary: Option<(u64, u64)>,
    pub(crate) alt: Option<(u64, u64)>,
}

#[derive(Clone, Copy)]
pub(crate) enum StripTimeDrag {
    Resize {
        anchor: f64,
        current: f64,
    },
    Translate {
        orig_lo: f64,
        orig_hi: f64,
        press_x: f64,
    },
}

fn strip_drag_band(drag: StripTimeDrag, pointer_x: f64, view_lo: f64, view_hi: f64) -> (f64, f64) {
    match drag {
        StripTimeDrag::Resize { anchor, current } => (anchor.min(current), anchor.max(current)),
        StripTimeDrag::Translate {
            orig_lo,
            orig_hi,
            press_x,
        } => {
            let [lo, hi] =
                translate_x_range(orig_lo, orig_hi, pointer_x - press_x, view_lo, view_hi);
            (lo, hi)
        }
    }
}

fn commit_strip_drag(
    drag: StripTimeDrag,
    pointer_x: f64,
    view_lo: f64,
    view_hi: f64,
    host_t0_us: u64,
    min_width: f64,
) -> Option<SelectionChange> {
    let (lo, hi) = strip_drag_band(drag, pointer_x, view_lo, view_hi);
    let lo_c = lo.max(view_lo).min(view_hi);
    let hi_c = hi.max(view_lo).min(view_hi);
    if hi_c - lo_c < min_width {
        return None;
    }
    let lo_us = (host_t0_us as i64 + (lo_c * 1_000_000.0).round() as i64).max(0) as u64;
    let hi_us = (host_t0_us as i64 + (hi_c * 1_000_000.0).round() as i64).max(0) as u64;
    Some(SelectionChange::Set(lo_us, hi_us))
}

fn begin_strip_drag(
    shift: bool,
    pointer_x: f64,
    active: Option<(f64, f64)>,
    view_lo: f64,
    view_hi: f64,
) -> StripTimeDrag {
    let movable =
        active.filter(|(lo, hi)| (lo - view_lo).abs() >= 1e-6 || (hi - view_hi).abs() >= 1e-6);
    if shift {
        if let Some((lo, hi)) = movable {
            return StripTimeDrag::Translate {
                orig_lo: lo,
                orig_hi: hi,
                press_x: pointer_x,
            };
        }
    }
    StripTimeDrag::Resize {
        anchor: pointer_x,
        current: pointer_x,
    }
}

impl CpuBusyStripView {
    pub(crate) fn new(
        bins: analysis::CpuUsageBins,
        profile: Arc<CpuProfile>,
        target_bins: usize,
    ) -> Self {
        Self {
            bins,
            profile,
            cpus: Vec::new(),
            target_bins,
            zoom_cache: None,
            view_mode: CpuUsageViewMode::Aggregate,
            drag: None,
            drag_is_alt: false,
            last_rect: None,
            last_transform: None,
            last_strip_data_lo_s: None,
            plot_height: 110.0,
            y_label: "CPUs busy".to_string(),
            as_percent: false,
            x_axis_title: Some("Time (seconds)".to_string()),
            scale_by_freq: false,
            y_axis_label_size: 12.0,
            show_legends: true,
            fill_remaining: false,
            plot_id: "cpu_usage_strip".to_string(),
        }
    }

    /// Percentage-scale factor applied to CPUs-busy values when
    /// `as_percent` is set (`100 / ncpu`), or `1.0` otherwise.
    fn y_scale(&self) -> f64 {
        if self.as_percent {
            100.0 / self.bins.ncpus().max(1) as f64
        } else {
            1.0
        }
    }

    /// [`analysis::CpuUsageBins::cpus_busy`] for `bin`, additionally
    /// scaled by [`analysis::CpuUsageBins::freq_scale_for`] when
    /// [`Self::scale_by_freq`] is on.
    fn scaled_cpus_busy(&self, bins: &analysis::CpuUsageBins, bin: usize) -> f64 {
        let v = bins.cpus_busy(bin);
        if self.scale_by_freq {
            v * bins.freq_scale_for(bin) as f64
        } else {
            v
        }
    }

    /// Per-CPU analogue of [`Self::scaled_cpus_busy`].
    fn scaled_cpu_busy_fraction(
        &self,
        bins: &analysis::CpuUsageBins,
        bin: usize,
        cpu_idx: usize,
    ) -> f64 {
        let v = bins.cpu_busy_fraction(bin, cpu_idx);
        if self.scale_by_freq {
            v * bins.freq_scale_for(bin) as f64
        } else {
            v
        }
    }

    /// Look up the strip’s value at `data_x_s` (host-relative
    /// seconds, same units the per-session panels use). Returns
    /// `(label, color, value)` for the host to paint a tracking
    /// dot at `(cursor_x, transform.position_from_point(value).y)`
    /// and to push as an entry on the shared tooltip. Returns
    /// `None` when `data_x_s` is outside the strip’s data range,
    /// when the strip hasn’t been drawn yet, or when there are
    /// no bins.
    pub(crate) fn cursor_lookup(&self, data_x_s: f64) -> Option<(String, Color32, f64)> {
        let strip_data_lo_s = self.last_strip_data_lo_s?;
        // Read whatever bins the last `draw()` actually rendered (the
        // zoomed re-bin when zoomed in) so the tooltip value matches
        // the line on screen.
        let bins = self.current_bins();
        if bins.is_empty() {
            return None;
        }
        let bin_s = bins.bin_us as f64 / 1_000_000.0;
        if bin_s <= 0.0 {
            return None;
        }
        let bin_f = (data_x_s - strip_data_lo_s) / bin_s;
        if !bin_f.is_finite() || bin_f < 0.0 {
            return None;
        }
        let bin = bin_f as usize;
        if bin >= bins.bin_count {
            return None;
        }
        let v = self.scaled_cpus_busy(bins, bin) * self.y_scale();
        // On the System tab the strip is percentage-scaled ("% busy"), so the
        // aggregate readout is just "N% busy". The flame / per-session strips
        // show a raw CPUs-busy count and keep the "<label>: N" form.
        let label = if self.as_percent {
            format!("{v:.1}% busy")
        } else {
            format!("{}: {v:.1}", self.y_label)
        };
        Some((label, Color32::from_rgb(120, 200, 200), v))
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.bins.is_empty()
    }

    pub(crate) fn bins(&self) -> &analysis::CpuUsageBins {
        &self.bins
    }

    /// Bins backing the most recent `draw()` call: the zoomed re-bin
    /// when the last-drawn view was zoomed in, or [`Self::bins`]
    /// otherwise. Used by [`Self::cursor_lookup`] so the tooltip
    /// value matches what's on screen.
    fn current_bins(&self) -> &analysis::CpuUsageBins {
        self.zoom_cache
            .as_ref()
            .map(|(_, b)| b)
            .unwrap_or(&self.bins)
    }

    /// Bins to render for `view_us` (host-relative absolute capture
    /// microseconds, or `None`/full-capture for the un-zoomed case).
    /// Zoomed out to (or past) the full capture returns
    /// [`Self::bins`] unchanged. Zoomed in, re-bins just the
    /// visible slice (found via binary search on the sorted `ts`
    /// field) at the same `target_bins` resolution, so the strip gets
    /// more detailed as the user zooms in rather than just stretching
    /// the same coarse bins wider. The re-bin is cached and only
    /// redone when the view has moved by more than ~1% of its width,
    /// so a static view doesn't re-bin every frame.
    pub(crate) fn effective_bins(
        &mut self,
        view_us: Option<(u64, u64)>,
    ) -> &analysis::CpuUsageBins {
        let full_lo = self.bins.t0_us;
        let full_hi = full_lo + self.bins.bin_us * self.bins.bin_count as u64;
        let Some((lo_us, hi_us)) = view_us else {
            self.zoom_cache = None;
            return &self.bins;
        };
        if lo_us <= full_lo && hi_us >= full_hi {
            self.zoom_cache = None;
            return &self.bins;
        }
        let stale = match &self.zoom_cache {
            Some(((cached_lo, cached_hi), _)) => {
                let tol = (hi_us.saturating_sub(lo_us)).max(1) as f64 * 0.01;
                (*cached_lo as f64 - lo_us as f64).abs() > tol
                    || (*cached_hi as f64 - hi_us as f64).abs() > tol
            }
            None => true,
        };
        if stale {
            let samples = &self.profile.samples;
            let lo_idx = samples.partition_point(|s| s.ts < lo_us);
            let hi_idx = samples.partition_point(|s| s.ts <= hi_us);
            let hz = self.profile.kernel.as_ref().map(|k| k.sample_hz());
            let rated = self
                .profile
                .kernel
                .as_ref()
                .and_then(|k| k.rated_freq_mhz());
            let mut rebinned = analysis::compute_cpu_usage_bins_for_cpus(
                &samples[lo_idx..hi_idx],
                self.target_bins,
                hz,
                rated,
                &self.cpus,
            );
            if let Some(reference) = self.bins.reference_freq_mhz {
                rebinned.retarget_freq_reference(reference);
            }
            self.zoom_cache = Some(((lo_us, hi_us), rebinned));
        }
        &self.zoom_cache.as_ref().unwrap().1
    }
}

/// Display mode for a [`CpuUsagePanel`]. `Aggregate`/`PerCpu` are
/// rendered by the panel's inner [`CpuBusyStripView`] (mapped onto
/// that struct's own [`CpuUsageViewMode`]); `PerProcess`/`KernelUser`
/// are rendered by one of its two inner [`TopProcessesStripView`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuStripMode {
    /// One line summing busy fractions across every CPU. Reads as
    /// "total CPUs busy".
    Aggregate,
    /// One stacked area per CPU, colour-coded by CPU id.
    PerCpu,
    /// Stacked top-N processes by on-CPU time, plus an "Other" cap
    /// for the long tail.
    PerProcess,
    /// Stacked kernel-mode vs. user-mode busy time
    /// ([`CPU_SAMPLE_FLAG_USER_MODE`]). Default on first open.
    KernelUser,
}

impl CpuStripMode {
    const ALL: [CpuStripMode; 4] = [
        Self::Aggregate,
        Self::PerCpu,
        Self::PerProcess,
        Self::KernelUser,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Aggregate => "Aggregate",
            Self::PerCpu => "Per-CPU",
            Self::PerProcess => "Per-Process",
            Self::KernelUser => "Kernel vs User",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Aggregate => {
                "One line summing busy fractions across every CPU. Y-axis \
                 runs 0..ncpu (an 8-CPU box maxes out near 8.0). Reads as \
                 \"total CPUs busy\"."
            }
            Self::PerCpu => {
                "Stacked one line per CPU, colour-coded by CPU id. Useful \
                 for spotting per-CPU imbalances (one CPU pegged while \
                 others sleep)."
            }
            Self::PerProcess => {
                "Stacked top-N processes by on-CPU time, plus an \"Other\" \
                 cap for the long tail."
            }
            Self::KernelUser => "Stacked kernel-mode vs. user-mode busy time.",
        }
    }
}

/// Style knobs a host passes to [`CpuUsagePanel::build`] so the three
/// hosts (Aggregate, per-session, Flame) can each keep their existing
/// cosmetic differences (strip height, x-axis caption) while sharing
/// every other line of plot/gear/cursor code.
pub struct CpuUsagePanelStyle {
    pub plot_height: f32,
    pub x_axis_title: Option<String>,
    /// See [`CpuBusyStripView::y_axis_label_size`] — must match the
    /// y-axis label font size of whatever panels this host stacks
    /// below the CPU panel, or their plot areas won't line up.
    pub y_axis_label_size: f32,
}

/// Unified "CPU Usage" plot: one [`CpuBusyStripView`] (Aggregate /
/// Per-CPU) plus two [`TopProcessesStripView`]s (Per-Process, Kernel
/// vs User — the latter fed two synthetic "Kernel"/"User" rows from
/// [`analysis::compute_kernel_user_usage_bins`]), switched by a single
/// gear-menu dropdown. Replaces the old pair of `Option<...>` strip
/// fields + `show_per_process_breakdown: bool` that each host used to
/// carry independently.
pub struct CpuUsagePanel {
    pub(crate) mode: CpuStripMode,
    /// Capture-wide frequency ceiling, independent of the CPU allowlist.
    reference_freq_mhz: Option<u16>,
    pub(crate) cpu_strip: CpuBusyStripView,
    pub(crate) process_strip: TopProcessesStripView,
    pub(crate) kernel_user_strip: TopProcessesStripView,
    /// Mirror of the app-wide "scale CPU usage by frequency" toggle;
    /// see [`CpuBusyStripView::scale_by_freq`]. Applied to whichever
    /// of the three inner strips is active right before each
    /// [`Self::draw`] call.
    pub(crate) scale_by_freq: bool,
    pub(crate) show_legends: bool,
    pub(crate) fill_remaining: bool,
}

impl CpuUsagePanel {
    /// Build a panel from `profile`, or `None` when the capture has
    /// no `cpu`-provider samples (mirrors the old
    /// `with_cpu_strips`/flame-build early-outs). `style` carries the
    /// per-host cosmetic knobs; `as_percent`/`y_label` are now always
    /// "% busy" on every host, so they're fixed here rather than in
    /// `style`.
    pub(crate) fn build(profile: &Arc<CpuProfile>, style: CpuUsagePanelStyle) -> Option<Self> {
        if profile.samples.is_empty() {
            return None;
        }
        // First paint uses the default bucket count. `sync_bins`
        // replaces these bins when the gear slider differs.
        const TOP_PROCESSES_N: usize = 10;

        let hz = profile.kernel.as_ref().map(|k| k.sample_hz());
        let rated = profile.kernel.as_ref().and_then(|k| k.rated_freq_mhz());
        let cpu_bins = analysis::compute_cpu_usage_bins(&profile.samples, STACK_BINS, hz, rated);
        if cpu_bins.is_empty() {
            return None;
        }
        let ncpus = cpu_bins.ncpus();
        let reference_freq_mhz = cpu_bins.reference_freq_mhz;
        let process_bins = analysis::compute_process_usage_bins(
            &profile.samples,
            profile,
            &cpu_bins,
            TOP_PROCESSES_N,
        );
        let kernel_user_bins =
            analysis::compute_kernel_user_usage_bins(&profile.samples, &cpu_bins);

        let mut cpu_strip = CpuBusyStripView::new(cpu_bins, Arc::clone(profile), STACK_BINS);
        cpu_strip.plot_height = style.plot_height;
        cpu_strip.y_label = "% busy".to_string();
        cpu_strip.as_percent = true;
        cpu_strip.x_axis_title = style.x_axis_title.clone();
        cpu_strip.y_axis_label_size = style.y_axis_label_size;

        let mut process_strip = TopProcessesStripView::new(
            process_bins,
            Arc::clone(profile),
            STACK_BINS,
            TOP_PROCESSES_N,
            reference_freq_mhz,
        );
        process_strip.plot_height = style.plot_height;
        process_strip.x_axis_title = style.x_axis_title.clone();
        process_strip.as_percent = true;
        process_strip.ncpus = ncpus;
        process_strip.y_label = "% busy".to_string();
        process_strip.y_axis_label_size = style.y_axis_label_size;

        let mut kernel_user_strip = TopProcessesStripView::new(
            kernel_user_bins,
            Arc::clone(profile),
            STACK_BINS,
            TOP_PROCESSES_N,
            reference_freq_mhz,
        );
        kernel_user_strip.plot_height = style.plot_height;
        kernel_user_strip.x_axis_title = style.x_axis_title;
        kernel_user_strip.as_percent = true;
        kernel_user_strip.ncpus = ncpus;
        kernel_user_strip.y_label = "% busy".to_string();
        kernel_user_strip.is_kernel_user = true;
        kernel_user_strip.y_axis_label_size = style.y_axis_label_size;

        Some(Self {
            mode: CpuStripMode::KernelUser,
            reference_freq_mhz,
            cpu_strip,
            process_strip,
            kernel_user_strip,
            scale_by_freq: false,
            show_legends: true,
            fill_remaining: false,
        })
    }

    pub(crate) fn set_plot_height(&mut self, h: f32) {
        self.cpu_strip.plot_height = h;
        self.process_strip.plot_height = h;
        self.kernel_user_strip.plot_height = h;
    }

    /// Rebuild full-capture bins and invalidate zoom caches when the
    /// Time buckets count or shared CPU selection changes.
    pub(crate) fn sync_bins(&mut self, target_bins: usize, cpus: &[u16]) {
        if target_bins == 0
            || (self.cpu_strip.target_bins == target_bins && self.cpu_strip.cpus == cpus)
        {
            return;
        }
        let profile = Arc::clone(&self.cpu_strip.profile);
        let hz = profile.kernel.as_ref().map(|k| k.sample_hz());
        let rated = self.reference_freq_mhz;
        let cpu_bins = analysis::compute_cpu_usage_bins_for_cpus(
            &profile.samples,
            target_bins,
            hz,
            rated,
            cpus,
        );
        let ncpus = cpu_bins.ncpus();
        let reference_freq_mhz = cpu_bins.reference_freq_mhz;
        let top_n = self.process_strip.top_n;
        let process_bins =
            analysis::compute_process_usage_bins(&profile.samples, &profile, &cpu_bins, top_n);
        let kernel_user_bins =
            analysis::compute_kernel_user_usage_bins(&profile.samples, &cpu_bins);
        self.cpu_strip.bins = cpu_bins;
        self.cpu_strip.cpus = cpus.to_vec();
        self.cpu_strip.target_bins = target_bins;
        self.cpu_strip.zoom_cache = None;
        self.process_strip.bins = process_bins;
        self.process_strip.cpus = cpus.to_vec();
        self.process_strip.target_bins = target_bins;
        self.process_strip.reference_freq_mhz = reference_freq_mhz;
        self.process_strip.ncpus = ncpus;
        self.process_strip.zoom_cache = None;
        self.kernel_user_strip.bins = kernel_user_bins;
        self.kernel_user_strip.cpus = cpus.to_vec();
        self.kernel_user_strip.target_bins = target_bins;
        self.kernel_user_strip.reference_freq_mhz = reference_freq_mhz;
        self.kernel_user_strip.ncpus = ncpus;
        self.kernel_user_strip.zoom_cache = None;
    }

    /// egui_plot widget ids for this panel's strips (per-tab).
    pub(crate) fn set_plot_ids(&mut self, plot_id: &str) {
        self.cpu_strip.plot_id = plot_id.to_string();
        self.process_strip.plot_id = format!("{plot_id}_proc");
        self.kernel_user_strip.plot_id = format!("{plot_id}_ku");
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.cpu_strip.is_empty()
    }

    /// Capture-relative origin of the underlying bins, for hosts
    /// (Flame) that derive their own `host_t0_us` / UTC axis origin
    /// per draw rather than sharing one from the caller.
    pub(crate) fn t0_us(&self) -> u64 {
        self.cpu_strip.bins().t0_us
    }

    /// CPU-busy bins, for the info popups that report kernel ident /
    /// bin width regardless of which mode is currently selected.
    pub(crate) fn cpu_bins(&self) -> &analysis::CpuUsageBins {
        self.cpu_strip.bins()
    }

    /// Render the `"CPU Usage"` title + gear-icon button on one line.
    /// The gear popup holds the mode dropdown, frequency scaling, and
    /// the zoom-to-window switch. `id_salt` must be unique per panel
    /// instance on screen at once (e.g. a session id) so two open
    /// panels' gear popups don't share open/closed state.
    pub(crate) fn show_header(
        &mut self,
        ui: &mut egui::Ui,
        id_salt: impl std::hash::Hash + Copy,
        scale_cpu_by_freq: &mut bool,
        zoom_to_window: &mut bool,
    ) {
        self.show_titled_header(ui, id_salt, scale_cpu_by_freq, zoom_to_window, "CPU Usage");
    }

    /// [`Self::show_header`] with a caller-supplied title, so a second
    /// strip (the compare baseline) can name its capture.
    pub(crate) fn show_titled_header(
        &mut self,
        ui: &mut egui::Ui,
        id_salt: impl std::hash::Hash + Copy,
        scale_cpu_by_freq: &mut bool,
        zoom_to_window: &mut bool,
        title: &str,
    ) {
        ui.horizontal(|ui| {
            ui.strong(RichText::new(title).size(14.0));
            ui.add_space(6.0);
            self.show_gear(ui, id_salt, scale_cpu_by_freq, zoom_to_window);
        });
    }

    pub(crate) fn show_gear(
        &mut self,
        ui: &mut egui::Ui,
        id_salt: impl std::hash::Hash + Copy,
        scale_cpu_by_freq: &mut bool,
        zoom_to_window: &mut bool,
    ) {
        // Same custom-painted-glyph + Area/Frame::popup pattern as
        // the per-session and flame gear menus (see
        // `show_flame_gear_menu`) -- reused verbatim so all of the
        // app's gear buttons read as the same widget.
        let popup_id = ui.make_persistent_id(("cpu_usage_panel_gear", id_salt));
        let is_open = ui
            .memory(|m| m.data.get_temp::<bool>(popup_id))
            .unwrap_or(false);

        let button_h = ui.spacing().interact_size.y;
        let button_size = egui::vec2(button_h, button_h);
        let (rect, response) = ui.allocate_exact_size(button_size, egui::Sense::click());
        let visuals = if is_open {
            ui.visuals().widgets.open
        } else {
            *ui.style().interact(&response)
        };
        ui.painter().rect(
            rect.expand(visuals.expansion),
            visuals.rounding,
            visuals.bg_fill,
            visuals.bg_stroke,
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{2699}",
            egui::FontId::proportional(14.0),
            visuals.text_color(),
        );
        let response = response.on_hover_text("CPU usage plot options");
        if response.clicked() {
            ui.memory_mut(|m| m.data.insert_temp(popup_id, !is_open));
        }

        if !is_open {
            return;
        }

        const POPUP_W: f32 = 260.0;
        let area = egui::Area::new(popup_id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_bottom() + egui::vec2(0.0, 4.0))
            .pivot(egui::Align2::LEFT_TOP)
            .constrain_to(ui.ctx().screen_rect());
        let area_resp = area.show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(POPUP_W);
                ui.set_max_width(POPUP_W);

                let mut mode = self.mode;
                let prev_mode = mode;
                let cb = egui::ComboBox::from_id_source(("cpu_usage_panel_mode_combo", id_salt))
                    .selected_text(mode.label())
                    .show_ui(ui, |ui| {
                        for candidate in CpuStripMode::ALL {
                            ui.selectable_value(&mut mode, candidate, candidate.label())
                                .on_hover_text(candidate.description());
                        }
                    });
                // `show_ui`'s `InnerResponse::inner` is only `Some` when the
                // dropdown actually rendered its contents this frame --
                // i.e. exactly when the popup is open. Simpler and more
                // reliable than re-deriving the popup's `Id` and checking
                // `Memory::is_popup_open` ourselves.
                let combo_popup_open = cb.inner.is_some();
                if !combo_popup_open {
                    cb.response.on_hover_text(
                        "CPU usage plot view.\n\n\
                         • Aggregate: total CPUs busy across the box.\n\
                         • Per-CPU: stacked one line per CPU.\n\
                         • Per-Process: stacked top-N processes.\n\
                         • Kernel vs User: stacked kernel/user busy time.",
                    );
                }
                if mode != prev_mode {
                    self.mode = mode;
                }

                let has_freq = self.cpu_strip.bins.freq_scale.is_some();
                let freq_cb = ui.add_enabled(
                    has_freq,
                    egui::Checkbox::new(
                        scale_cpu_by_freq,
                        RichText::new("Scale by CPU frequency").size(12.0),
                    ),
                );
                if has_freq {
                    let mhz = self.cpu_strip.bins.reference_freq_mhz.unwrap_or(0);
                    freq_cb.on_hover_text(format!(
                        "Multiply displayed busy% by each bin's effective \
                         CPU frequency relative to this capture's sustained \
                         peak ({mhz} MHz), for comparison against \
                         frequency-scaled external dashboards. On by default."
                    ));
                } else {
                    freq_cb.on_disabled_hover_text(
                        "This capture has no per-sample CPU frequency data.",
                    );
                }

                ui.checkbox(
                    zoom_to_window,
                    RichText::new("Zoom to time window").size(12.0),
                )
                .on_hover_text(
                    "Show only the current time window, re-binned, and \
                     zoom it with box zoom, Shift-drag, double-click \
                     undo, and middle-click reset. Off shows the whole \
                     capture and highlights that window; drag sets it.",
                );
            });
        });

        let popup_rect = area_resp.response.rect;
        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.memory_mut(|m| m.data.insert_temp(popup_id, false));
        }
        if ui.ctx().input(|i| i.pointer.any_click()) {
            if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
                if !rect.contains(pos) && !popup_rect.contains(pos) {
                    ui.memory_mut(|m| m.data.insert_temp(popup_id, false));
                }
            }
        }
    }

    /// Render whichever inner strip `self.mode` selects, with a single
    /// selection band. `own_gestures` is true on the Flame tab, where
    /// drag-select sets the time filter.
    pub(crate) fn draw(
        &mut self,
        ui: &mut egui::Ui,
        host_t0_us: u64,
        current_selection_us: Option<(u64, u64)>,
        view_us: Option<(u64, u64)>,
        utc_axis_origin: Option<u64>,
        own_gestures: bool,
    ) -> Option<SelectionChange> {
        self.draw_with_alt(
            ui,
            host_t0_us,
            StripSelections {
                primary: current_selection_us,
                alt: None,
            },
            view_us,
            utc_axis_origin,
            own_gestures,
        )
        .0
    }

    /// [`Self::draw`] with a second ("alt") selection band, forwarded
    /// to whichever inner strip is active; see
    /// [`CpuBusyStripView::draw_with_alt`].
    pub(crate) fn draw_with_alt(
        &mut self,
        ui: &mut egui::Ui,
        host_t0_us: u64,
        selections: StripSelections,
        view_us: Option<(u64, u64)>,
        utc_axis_origin: Option<u64>,
        own_gestures: bool,
    ) -> (Option<SelectionChange>, Option<SelectionChange>) {
        self.cpu_strip.scale_by_freq = self.scale_by_freq;
        self.process_strip.scale_by_freq = self.scale_by_freq;
        self.kernel_user_strip.scale_by_freq = self.scale_by_freq;
        self.cpu_strip.show_legends = self.show_legends;
        self.process_strip.show_legends = self.show_legends;
        self.kernel_user_strip.show_legends = self.show_legends;
        self.cpu_strip.fill_remaining = self.fill_remaining;
        self.process_strip.fill_remaining = self.fill_remaining;
        self.kernel_user_strip.fill_remaining = self.fill_remaining;
        match self.mode {
            CpuStripMode::Aggregate => {
                self.cpu_strip.view_mode = CpuUsageViewMode::Aggregate;
                self.cpu_strip.draw_with_alt(
                    ui,
                    host_t0_us,
                    selections,
                    view_us,
                    utc_axis_origin,
                    own_gestures,
                )
            }
            CpuStripMode::PerCpu => {
                self.cpu_strip.view_mode = CpuUsageViewMode::PerCpu;
                self.cpu_strip.draw_with_alt(
                    ui,
                    host_t0_us,
                    selections,
                    view_us,
                    utc_axis_origin,
                    own_gestures,
                )
            }
            CpuStripMode::PerProcess => self.process_strip.draw_with_alt(
                ui,
                host_t0_us,
                selections,
                view_us,
                utc_axis_origin,
                own_gestures,
            ),
            CpuStripMode::KernelUser => self.kernel_user_strip.draw_with_alt(
                ui,
                host_t0_us,
                selections,
                view_us,
                utc_axis_origin,
                own_gestures,
            ),
        }
    }

    /// `(last_rect, last_transform)` of whichever inner strip is
    /// active, for the host's shared-cursor overlay. `None` until
    /// that strip has rendered at least once.
    pub(crate) fn last_rect_transform(&self) -> Option<(egui::Rect, egui_plot::PlotTransform)> {
        let (r, t) = match self.mode {
            CpuStripMode::Aggregate | CpuStripMode::PerCpu => {
                (self.cpu_strip.last_rect, self.cpu_strip.last_transform)
            }
            CpuStripMode::PerProcess => (
                self.process_strip.last_rect,
                self.process_strip.last_transform,
            ),
            CpuStripMode::KernelUser => (
                self.kernel_user_strip.last_rect,
                self.kernel_user_strip.last_transform,
            ),
        };
        Some((r?, t?))
    }

    /// Multi-row cursor readout at `data_x_s`: one entry per visible
    /// layer for the stacked modes (Per-Process, Kernel vs User), plus
    /// a Total row, or a single-element vec for Aggregate/Per-CPU.
    pub(crate) fn cursor_series(&self, data_x_s: f64) -> Vec<(String, Color32, f64)> {
        match self.mode {
            CpuStripMode::PerProcess => self.process_strip.cursor_series(data_x_s),
            CpuStripMode::KernelUser => self.kernel_user_strip.cursor_series(data_x_s),
            CpuStripMode::Aggregate | CpuStripMode::PerCpu => {
                match self.cpu_strip.cursor_lookup(data_x_s) {
                    Some(entry) => vec![entry],
                    None => Vec::new(),
                }
            }
        }
    }
}

/// Build a strip's x-axis hints: a plain default axis with `x_axis_title`
/// as its caption (per-session / Flame strips), or the shared
/// [`agg_x_time_axis`] tick style with no caption (Aggregate tab) —
/// unchanged from before this existed. When `utc_axis_origin` is `Some`
/// (the "UTC time labels" toggle is on), both variants switch to
/// wall-clock tick labels instead, and a titled strip's caption becomes
/// "Time (UTC)". The tick at the view's left edge carries the row's one
/// `UTC+` anchor (see [`format_time_axis_tick`]) instead of being
/// blanked; callers pair this with [`utc_x_grid_spacer`] so that tick
/// actually exists even when the strip's own data starts a beat after
/// the shared `t=0`.
fn strip_x_axis_hints(x_axis_title: &Option<String>, utc_axis_origin: Option<u64>) -> AxisHints {
    match utc_axis_origin {
        Some(origin) => {
            let mut axis = AxisHints::new_x()
                .label_spacing(egui::Rangef::new(36.0, 40.0))
                .formatter(
                    move |mark: GridMark, _digits: usize, range: &std::ops::RangeInclusive<f64>| {
                        format_time_axis_tick(mark.value, mark.step_size, origin, range)
                    },
                );
            if x_axis_title.is_some() {
                axis = axis.label("Time (UTC)");
            }
            axis
        }
        None => match x_axis_title {
            Some(t) => AxisHints::new_x()
                .label_spacing(egui::Rangef::new(36.0, 40.0))
                .label(t.clone()),
            None => agg_x_time_axis(None),
        },
    }
}

impl CpuBusyStripView {
    /// Render the strip into `ui`. `selections.primary` is the host's
    /// current time-range selection (if any); the strip paints it as
    /// the shaded committed-selection band. `own_gestures` lets this
    /// strip drag-select a time range (Flame). Returns
    /// `(primary_change, alt_change)`.
    pub(crate) fn draw_with_alt(
        &mut self,
        ui: &mut egui::Ui,
        host_t0_us: u64,
        selections: StripSelections,
        view_us: Option<(u64, u64)>,
        utc_axis_origin: Option<u64>,
        own_gestures: bool,
    ) -> (Option<SelectionChange>, Option<SelectionChange>) {
        let current_selection_us = selections.primary;
        let alt_selection_us = selections.alt;
        if self.bins.is_empty() {
            return (None, None);
        }
        let bins = self.effective_bins(view_us).clone();
        if bins.is_empty() {
            return (None, None);
        }
        let y_scale = self.y_scale();

        // The strip’s on-screen x-axis means "seconds since host_t0_us",
        // which lets a per-session host align the strip to its session-
        // relative panels (host_t0_us = session.start_ts_us). The System
        // tab passes capture_start plus a view covering 0..time_max so
        // the strip shares that axis with the panels below. Flame passes
        // bins.t0_us (first CPU sample) as both origin and data start.
        let bin_s = bins.bin_us as f64 / 1_000_000.0;
        // strip_data_lo/hi is the strip’s data range in host-relative
        // seconds (can be negative when host_t0 > bins.t0).
        let strip_data_lo_s = (bins.t0_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
        let strip_data_hi_s = strip_data_lo_s + bin_s * bins.bin_count as f64;

        // Visible x-axis range in host-relative seconds. When `view_us`
        // is Some, we use that window so the strip follows the host’s
        // zoom (per-session: matches view_x). When None, we show the
        // full strip-data range.
        let (view_lo_s, view_hi_s) = if let Some((lo_us, hi_us)) = view_us {
            let lo = (lo_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            let hi = (hi_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            (lo, hi.max(lo + 1e-6))
        } else {
            (strip_data_lo_s, strip_data_hi_s.max(strip_data_lo_s + 1e-6))
        };

        // active_selection (shaded band overlay) in host-relative seconds.
        let mut emitted: Option<SelectionChange> = None;
        let mut emitted_alt: Option<SelectionChange> = None;
        let active_selection = current_selection_us.map(|(start_us, end_us)| {
            let lo = (start_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            let hi = (end_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            (lo, hi)
        });
        let alt_active_selection = alt_selection_us.map(|(start_us, end_us)| {
            let lo = (start_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            let hi = (end_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            (lo, hi)
        });
        // Two-band mode keeps the plain-drag band blue and paints the
        // Alt+drag band orange.
        let band_b_fill = if alt_selection_us.is_some() {
            REGION_B_FILL
        } else {
            SINGLE_BAND_FILL
        };

        // ---- Dynamic y-axis ---------------------------------------------
        // Clamp the y-axis to the actual peak utilisation (CPUs busy at
        // once) plus a small headroom rather than the static `ncpu`.
        // On a 96-CPU box where peak usage is ~8 CPUs, a fixed 0..96
        // axis flattens the line into the bottom strip; the dynamic max
        // gives the eye 12× the vertical resolution where it matters.
        // `cpus_busy()` normalises against the data-derived peak per-CPU
        // sample rate (`CpuUsageBins::peak_samples_per_bin`), so the
        // result is right on both archival captures and idle-skipping
        // kernels (newer firmware); the previous `busy_sum / (total_sum
        // / ncpu)` formulation pinned to `ncpu` whenever `busy == total`
        // for every cell (i.e. every idle-skipping capture).
        let mut peak_cpus_busy = 0.0f64;
        for bin in 0..bins.bin_count {
            let v = self.scaled_cpus_busy(&bins, bin) * y_scale;
            if v > peak_cpus_busy {
                peak_cpus_busy = v;
            }
        }
        // 8% headroom plus a 1-CPU floor (so a single-CPU peak doesn't
        // sit flush against the top edge), and never above ncpu (the
        // theoretical hard cap so the user's eye still gets the "how
        // close to fully saturated?" reference).
        // (Note: the previous `peak * 1.08` headroom calculation was
        // replaced by `nice_ceiling(peak)` further down — kept this
        // line removed to make the intent obvious.)

        // ---- The plot itself --------------------------------------------
        // Single-axis plot. Disable plot pan/zoom — drag is reserved for
        // selection. `Sense::click_and_drag()` makes the plot's response
        // report drag events (otherwise allow_drag(false) suppresses
        // them too). The plot is short (110 px) so it doesn't crowd the
        // flame canvas.
        //
        // CRITICAL: `auto_bounds(false, false)` is mandatory. The
        // selection overlay polygon places vertices at `bounds.max[1]`;
        // with y-auto-bounds on plus a 5% y margin, each frame's
        // polygon would push the upper bound outward by 5% → runaway
        // exponential growth ("y-axis goes to infinite when zooming").
        // We pin both axes to the include_x / include_y range and
        // accept that the user can't pan/zoom the strip — that's fine,
        // the strip's only job is selecting a time range.
        // Snap y_max up to a "nice" round number ({1, 2, 5} × 10ⁿ) so
        // the tick spacer below has a clean endpoint. Percentage plots
        // never go above 100.
        let y_max_nice = if self.as_percent {
            percent_y_axis_top(peak_cpus_busy)
        } else {
            nice_ceiling(peak_cpus_busy.max(1.0))
        };
        // CRITICAL: egui_plot's default `log_grid_spacer(10)` rounds
        // its smallest tick spacing UP to the next power of 10 of
        // `dvalue_dpos * grid_spacing.min` (8 px by default). On the
        // 110 px-tall strip the X-axis label + ticks eat ~42 px, so
        // the data area is ~68 logical px. For y_range = 96 (the 96-CPU
        // peak), `dvalue_dpos = 96 / 68 ≈ 1.41`, base_step_size = 11.3,
        // and `next_power(11.3, 10) = 100`. Smallest tick = 100 → the
        // ONLY mark inside `[0, 96]` is `value=0`, and even that is
        // suppressed by our formatter. Result: a y-axis with no labels
        // at all. The fix is a custom spacer that picks a {1, 2, 5} ×
        // 10ⁿ step targeted at ~4 ticks across the visible range,
        // independent of pixel density.
        let y_grid_spacer = move |input: egui_plot::GridInput| -> Vec<GridMark> {
            let (lo, hi) = input.bounds;
            let range = hi - lo;
            if range <= 0.0 {
                return vec![];
            }
            let rough_step = range / 4.0;
            let magnitude = 10.0_f64.powf(rough_step.log10().floor());
            let normalized = rough_step / magnitude;
            let nice_step: f64 = if normalized <= 1.5 {
                magnitude
            } else if normalized <= 3.5 {
                2.0 * magnitude
            } else if normalized <= 7.5 {
                5.0 * magnitude
            } else {
                10.0 * magnitude
            };
            let start = (lo / nice_step).ceil() as i64;
            let end = (hi / nice_step).floor() as i64;
            (start..=end)
                .map(|i| GridMark {
                    value: i as f64 * nice_step,
                    step_size: nice_step,
                })
                .collect()
        };
        // Suppress the y=0 label (it would visually stack with the
        // x-axis "0" at the bottom-left corner) and any sub-integer
        // ticks (CPU counts are always whole numbers).
        let y_formatter =
            |mark: GridMark, _digits: usize, _range: &std::ops::RangeInclusive<f64>| {
                if mark.value.abs() < 1e-9 {
                    String::new()
                } else if (mark.value - mark.value.round()).abs() < 1e-9 {
                    format!("{}", mark.value.round() as i64)
                } else {
                    String::new()
                }
            };
        // `max_digits(6)` matches the per-session panels’ AxisHints,
        // so the strip’s y-axis gutter is the same width and the
        // strip’s plot area aligns horizontally with the panels’
        // plot areas below. The strip never displays 6-digit numbers
        // (CPUs busy max ~100), but the reservation costs only a few
        // pixels of horizontal label space and is the cleanest way
        // to keep all stacked time-series visually columnated.
        let y_axis = AxisHints::new_y()
            .label(RichText::new(self.y_label.clone()).size(self.y_axis_label_size))
            .max_digits(6)
            .label_spacing(egui::Rangef::new(5.0, 6.0))
            .formatter(y_formatter);
        // Top inset: gives the topmost y-tick label a row of pixels
        // above the plot frame so it doesn't get clipped against the
        // controls row.
        ui.add_space(8.0);
        let plot_h = if self.fill_remaining {
            ui.available_height().max(PLOT_H_MIN)
        } else {
            self.plot_height
        };
        // The strip is built once per frame; bounds are forced via
        // `plot_ui.set_plot_bounds` inside the show closure (see below).
        // We tried two earlier approaches — mutating PlotMemory.bounds
        // before show, and `data.remove::<PlotMemory>` to clear cached
        // bounds — neither reliably propagated the new view range to
        // the rendered transform on egui_plot 0.27. set_plot_bounds is
        // the documented runtime knob and works.
        let mut plot = Plot::new(&self.plot_id)
            .height(plot_h)
            // Match the per-session panels’ y-axis gutter so the
            // strip’s plot area aligns horizontally with the panels.
            .y_axis_width(6)
            // Vertical margin (5%) so the topmost data point doesn't
            // sit flush against the plot's top edge.
            .set_margin_fraction(egui::vec2(0.005, 0.05))
            .include_y(0.0)
            .include_y(y_max_nice)
            .auto_bounds(Vec2b::new(false, false))
            .y_grid_spacer(y_grid_spacer)
            .show_x(false)
            .show_y(false)
            .allow_scroll(false)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .custom_y_axes(vec![y_axis])
            .sense(egui::Sense::click_and_drag());
        plot = maybe_legend(plot, self.show_legends);
        plot = plot.custom_x_axes(vec![strip_x_axis_hints(
            &self.x_axis_title,
            utc_axis_origin,
        )]);
        if utc_axis_origin.is_some() {
            plot = plot.x_grid_spacer(utc_x_grid_spacer);
        }

        // Capture the fixed y-bounds for the overlay polygon so we never
        // place vertices at `plot_bounds().max[1]` (which is the
        // feedback-loop trap that previously blew the y-axis up to
        // infinity). Even though we've pinned auto_bounds off, drawing
        // strictly inside the requested range is the belt-and-braces
        // version.
        let band_y_lo = 0.0_f64;
        let band_y_hi = y_max_nice;

        let inner = plot.show(ui, |plot_ui| {
            // Force the plot bounds for this frame to the host’s view
            // window. egui_plot 0.27 ignores include_x() when
            // auto_bounds is off, and PlotMemory caching keeps the
            // initial bounds across renders, so we have to call this
            // every frame to make a host view_x change actually take
            // effect on the strip. Y bounds are pinned to [0,
            // y_max_nice] (no feedback loop because the polygon
            // overlay vertices use these constants too).
            plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                [view_lo_s, 0.0],
                [view_hi_s, y_max_nice],
            ));

            // Lines (per-cpu stack vs aggregate).
            match self.view_mode {
                CpuUsageViewMode::Aggregate => {
                    let mut pts: Vec<[f64; 2]> = Vec::with_capacity(bins.bin_count);
                    for bin in 0..bins.bin_count {
                        let t = strip_data_lo_s + bin as f64 * bin_s + bin_s * 0.5;
                        pts.push([t, self.scaled_cpus_busy(&bins, bin) * y_scale]);
                    }
                    plot_ui.line(
                        Line::new(PlotPoints::new(pts))
                            .color(Color32::from_rgb(120, 200, 200))
                            .width(1.5_f32)
                            .name(self.y_label.as_str()),
                    );
                }
                CpuUsageViewMode::PerCpu => {
                    // Stacked area approximation: cumulative per-CPU
                    // utilisation. Drawn as overlapping lines (top of
                    // CPU N stacks above CPU N-1) so the viewer sees
                    // both each CPU's individual contribution and the
                    // aggregate envelope at the top. Colour cycle is
                    // hash-based so adjacent CPU IDs stay
                    // distinguishable.
                    let n = bins.ncpus();
                    // Pre-compute the running cumulative line per bin
                    // so each CPU's polyline is just `cumulative[bin]`.
                    // O(bins × ncpu); cheap.
                    let mut cum = vec![0.0f64; bins.bin_count];
                    for cpu_idx in 0..n {
                        let cpu_id = bins.cpu_ids[cpu_idx];
                        let mut pts: Vec<[f64; 2]> = Vec::with_capacity(bins.bin_count);
                        for (bin, c) in cum.iter_mut().enumerate() {
                            *c += self.scaled_cpu_busy_fraction(&bins, bin, cpu_idx) * y_scale;
                            let t = strip_data_lo_s + bin as f64 * bin_s + bin_s * 0.5;
                            pts.push([t, *c]);
                        }
                        let color = per_cpu_color(cpu_id);
                        plot_ui.line(
                            Line::new(PlotPoints::new(pts))
                                .color(color)
                                .width(1.0_f32)
                                .name(format!("CPU {cpu_id}")),
                        );
                    }
                }
            }

            // Flame time-filter drag: live preview + committed band.
            if own_gestures {
                let plot_resp = plot_ui.response();
                let pointer = plot_ui.pointer_coordinate();
                let shift = plot_ui.ctx().input(|i| i.modifiers.shift);
                // Alt routes the drag to region A (orange). A plain drag
                // edits region B (blue), the primary time window.
                let alt = plot_ui.ctx().input(|i| i.modifiers.alt);

                if plot_resp.drag_started_by(egui::PointerButton::Primary) {
                    if let Some(p) = pointer {
                        let active = if alt {
                            alt_active_selection
                        } else {
                            active_selection
                        };
                        self.drag =
                            Some(begin_strip_drag(shift, p.x, active, view_lo_s, view_hi_s));
                        self.drag_is_alt = alt;
                    }
                }
                if plot_resp.dragged_by(egui::PointerButton::Primary) {
                    if let Some(p) = pointer {
                        if let Some(StripTimeDrag::Resize { anchor, .. }) = self.drag {
                            self.drag = Some(StripTimeDrag::Resize {
                                anchor,
                                current: p.x,
                            });
                        }
                    }
                }
                if plot_resp.drag_stopped_by(egui::PointerButton::Primary) {
                    if let Some(drag) = self.drag.take() {
                        let x = pointer.map(|p| p.x).unwrap_or(match drag {
                            StripTimeDrag::Resize { current, .. } => current,
                            StripTimeDrag::Translate { press_x, .. } => press_x,
                        });
                        let change = commit_strip_drag(
                            drag,
                            x,
                            view_lo_s,
                            view_hi_s,
                            host_t0_us,
                            bin_s * 0.5,
                        );
                        if self.drag_is_alt {
                            emitted_alt = change;
                        } else {
                            emitted = change;
                        }
                    }
                }

                let preview_x = pointer.map(|p| p.x);
                if let Some(drag) = self.drag {
                    let x = preview_x.unwrap_or(match drag {
                        StripTimeDrag::Resize { current, .. } => current,
                        StripTimeDrag::Translate { press_x, .. } => press_x,
                    });
                    let (lo, hi) = strip_drag_band(drag, x, view_lo_s, view_hi_s);
                    let fill = if self.drag_is_alt {
                        REGION_A_FILL
                    } else {
                        band_b_fill
                    };
                    draw_x_band(plot_ui, lo, hi, band_y_lo, band_y_hi, fill);
                } else {
                    if let Some((lo, hi)) = alt_active_selection {
                        let band_covers_view =
                            (lo - view_lo_s).abs() < 1e-6 && (hi - view_hi_s).abs() < 1e-6;
                        if !band_covers_view {
                            draw_x_band(plot_ui, lo, hi, band_y_lo, band_y_hi, REGION_A_FILL);
                        }
                    }
                    if let Some((lo, hi)) = active_selection {
                        let band_covers_view =
                            (lo - view_lo_s).abs() < 1e-6 && (hi - view_hi_s).abs() < 1e-6;
                        if !band_covers_view {
                            draw_x_band(plot_ui, lo, hi, band_y_lo, band_y_hi, band_b_fill);
                        }
                    }
                }
            }
        });

        if own_gestures && inner.response.secondary_clicked() && self.drag.is_none() {
            let alt = ui.input(|i| i.modifiers.alt);
            if alt {
                if alt_selection_us.is_some() {
                    emitted_alt = Some(SelectionChange::Clear);
                }
            } else if active_selection.is_some() {
                emitted = Some(SelectionChange::Clear);
            }
        }

        // Save cursor-tracking metadata so the host can paint a
        // shared cursor line + dot + tooltip entry over the strip.
        self.last_rect = Some(inner.response.rect);
        self.last_transform = Some(inner.transform);
        self.last_strip_data_lo_s = Some(strip_data_lo_s);

        (emitted, emitted_alt)
    }
}

/// "Top processes by on-CPU time" context strip. Stacks the busy
/// sample share of each top-N pid as cumulative areas so the
/// reader can see at a glance which processes dominated CPU at
/// each moment in the capture. Shares its bin layout and
/// `peak_samples_per_bin` denominator with the CPU-busy strip
/// (same `target_bins` zoom re-bin), so the stacked total tracks
/// the aggregate `% busy` line.
///
/// Drag-to-select on this strip drives the same time-range
/// selection as the CPU-busy strip — both emit
/// [`SelectionChange`] events the host applies to its own
/// time-range filter (today: the flame tab's `FlameFilter`).
pub struct TopProcessesStripView {
    pub(crate) bins: analysis::ProcessUsageBins,
    /// Backing samples for zoomed re-bin; see [`CpuBusyStripView::profile`].
    pub(crate) profile: Arc<CpuProfile>,
    /// CPU allowlist shared with the sibling CPU-busy strip.
    cpus: Vec<u16>,
    /// Same `target_bins` as the sibling [`CpuBusyStripView`].
    pub(crate) target_bins: usize,
    /// Top-N kept when re-binning Per-Process.
    pub(crate) top_n: usize,
    /// Full-capture frequency reference, applied to zoomed grids so
    /// scale-by-freq matches the aggregate strip.
    pub(crate) reference_freq_mhz: Option<u16>,
    /// Cached zoomed re-bin; see [`CpuBusyStripView::zoom_cache`].
    pub(crate) zoom_cache: Option<((u64, u64), analysis::ProcessUsageBins)>,
    /// Active drag on the plot; see [`CpuBusyStripView::drag`].
    pub(crate) drag: Option<StripTimeDrag>,
    /// Whether `drag` is editing the alt band; see
    /// [`CpuBusyStripView::drag_is_alt`].
    pub(crate) drag_is_alt: bool,
    /// Last-frame cursor-tracking metadata; see CpuBusyStripView
    /// for the shared docstring.
    pub(crate) last_rect: Option<egui::Rect>,
    pub(crate) last_transform: Option<egui_plot::PlotTransform>,
    pub(crate) last_strip_data_lo_s: Option<f64>,
    /// Plot height in logical px; see [`CpuBusyStripView::plot_height`].
    pub(crate) plot_height: f32,
    /// X-axis title; see [`CpuBusyStripView::x_axis_title`].
    pub(crate) x_axis_title: Option<String>,
    /// When true, plot each layer as a percentage of the box's total CPU
    /// capacity ("% busy") instead of a raw CPUs-busy count, matching the
    /// aggregate CPU strip on the System tab. Off for the flame / per-
    /// session strips.
    pub(crate) as_percent: bool,
    /// CPU count for the `as_percent` scale (`100 / ncpus`). Set from the
    /// matching [`CpuBusyStripView`]'s bins; `0` disables percentage mode.
    pub(crate) ncpus: usize,
    /// Rotated y-axis title. Defaults to "By process"; the System tab
    /// shows "% busy" to match the aggregate strip.
    pub(crate) y_label: String,
    /// Mirror of the app-wide "scale CPU usage by frequency" toggle;
    /// see `CpuBusyStripView::scale_by_freq`. Synced by the host right
    /// before each `draw()` / `cursor_series()` / `cursor_lookup()`
    /// call.
    pub(crate) scale_by_freq: bool,
    /// Set only on the `kernel_user_strip` instance built by
    /// [`CpuUsagePanel::build`]. Switches `draw()` from the generic
    /// per-pid line stack to a two-band filled stacked-area render, and
    /// drops the "(pid N)" suffix from labels (the sentinel pids in
    /// [`analysis::compute_kernel_user_usage_bins`]'s output aren't
    /// meaningful to show).
    pub(crate) is_kernel_user: bool,
    /// Font size of the rotated y-axis label; see
    /// [`CpuBusyStripView::y_axis_label_size`].
    pub(crate) y_axis_label_size: f32,
    pub(crate) show_legends: bool,
    /// After the 8px top inset, size the Plot to leftover height
    /// instead of [`Self::plot_height`]. Used when the catalog row is
    /// maximized so the inset cannot overflow a fill-remaining slot.
    pub(crate) fill_remaining: bool,
    /// egui_plot widget id; the host sets this per tab before `draw`.
    pub(crate) plot_id: String,
}

impl TopProcessesStripView {
    pub(crate) fn new(
        bins: analysis::ProcessUsageBins,
        profile: Arc<CpuProfile>,
        target_bins: usize,
        top_n: usize,
        reference_freq_mhz: Option<u16>,
    ) -> Self {
        Self {
            bins,
            profile,
            target_bins,
            top_n,
            cpus: Vec::new(),
            reference_freq_mhz,
            zoom_cache: None,
            drag: None,
            drag_is_alt: false,
            last_rect: None,
            last_transform: None,
            last_strip_data_lo_s: None,
            plot_height: 110.0,
            x_axis_title: Some("Time (seconds)".to_string()),
            as_percent: false,
            ncpus: 0,
            y_label: "By process".to_string(),
            scale_by_freq: false,
            is_kernel_user: false,
            y_axis_label_size: 12.0,
            show_legends: true,
            fill_remaining: false,
            plot_id: "cpu_usage_strip_proc".to_string(),
        }
    }

    /// Bins backing the most recent `draw()`: the zoomed re-bin when
    /// the last-drawn view was zoomed in, otherwise [`Self::bins`].
    fn current_bins(&self) -> &analysis::ProcessUsageBins {
        self.zoom_cache
            .as_ref()
            .map(|(_, b)| b)
            .unwrap_or(&self.bins)
    }

    /// Same zoom re-bin as [`CpuBusyStripView::effective_bins`]: full
    /// capture uses the precomputed bins; a tighter view re-bins the
    /// visible slice at `target_bins` so Kernel vs User / Per-Process
    /// keep the same resolution as Aggregate.
    fn effective_bins(&mut self, view_us: Option<(u64, u64)>) -> &analysis::ProcessUsageBins {
        let full_lo = self.bins.t0_us;
        let full_hi = full_lo + self.bins.bin_us * self.bins.bin_count as u64;
        let Some((lo_us, hi_us)) = view_us else {
            self.zoom_cache = None;
            return &self.bins;
        };
        if lo_us <= full_lo && hi_us >= full_hi {
            self.zoom_cache = None;
            return &self.bins;
        }
        let stale = match &self.zoom_cache {
            Some(((cached_lo, cached_hi), _)) => {
                let tol = (hi_us.saturating_sub(lo_us)).max(1) as f64 * 0.01;
                (*cached_lo as f64 - lo_us as f64).abs() > tol
                    || (*cached_hi as f64 - hi_us as f64).abs() > tol
            }
            None => true,
        };
        if stale {
            let samples = &self.profile.samples;
            let lo_idx = samples.partition_point(|s| s.ts < lo_us);
            let hi_idx = samples.partition_point(|s| s.ts <= hi_us);
            let hz = self.profile.kernel.as_ref().map(|k| k.sample_hz());
            let rated = self
                .profile
                .kernel
                .as_ref()
                .and_then(|k| k.rated_freq_mhz());
            let mut grid = analysis::compute_cpu_usage_bins_for_cpus(
                &samples[lo_idx..hi_idx],
                self.target_bins,
                hz,
                rated,
                &self.cpus,
            );
            if let Some(reference) = self.reference_freq_mhz {
                grid.retarget_freq_reference(reference);
            }
            let rebinned = if self.is_kernel_user {
                analysis::compute_kernel_user_usage_bins(&samples[lo_idx..hi_idx], &grid)
            } else {
                analysis::compute_process_usage_bins(
                    &samples[lo_idx..hi_idx],
                    &self.profile,
                    &grid,
                    self.top_n,
                )
            };
            self.zoom_cache = Some(((lo_us, hi_us), rebinned));
        }
        &self.zoom_cache.as_ref().unwrap().1
    }

    /// `bins.cpus_busy_by_pid(bin, process_idx)`, scaled by the bin's
    /// frequency multiplier when `scale_by_freq` is set. See
    /// `CpuBusyStripView::scaled_cpus_busy`.
    fn scaled_cpus_busy_by_pid(
        &self,
        bins: &analysis::ProcessUsageBins,
        bin: usize,
        process_idx: usize,
    ) -> f64 {
        let v = bins.cpus_busy_by_pid(bin, process_idx);
        if self.scale_by_freq {
            v * bins.freq_scale_for(bin) as f64
        } else {
            v
        }
    }

    /// `bins.cpus_busy_other(bin)`, scaled by the bin's frequency
    /// multiplier when `scale_by_freq` is set.
    fn scaled_cpus_busy_other(&self, bins: &analysis::ProcessUsageBins, bin: usize) -> f64 {
        let v = bins.cpus_busy_other(bin);
        if self.scale_by_freq {
            v * bins.freq_scale_for(bin) as f64
        } else {
            v
        }
    }

    /// Percentage-scale factor applied to CPUs-busy values when
    /// `as_percent` is set (`100 / ncpus`), or `1.0` otherwise.
    fn y_scale(&self) -> f64 {
        if self.as_percent && self.ncpus > 0 {
            100.0 / self.ncpus as f64
        } else {
            1.0
        }
    }

    /// Per-process cursor readout at `data_x_s` (host-relative seconds):
    /// one `(label, color, cumulative_y)` per top process (plus "Other")
    /// whose contribution is non-negligible at this instant, plus a
    /// Total row for the full stack (including skipped tiny layers).
    /// carries the process's *individual* value ("comm (pid N): X% busy");
    /// `cumulative_y` is the running stacked total through that layer, so
    /// the host can place the tracking dot on the layer's top edge — the
    /// same place the stacked area draws it. Empty when out of range.
    pub(crate) fn cursor_series(&self, data_x_s: f64) -> Vec<(String, Color32, f64)> {
        let mut out = Vec::new();
        let Some(strip_data_lo_s) = self.last_strip_data_lo_s else {
            return out;
        };
        let bins = self.current_bins();
        if bins.is_empty() {
            return out;
        }
        let bin_s = bins.bin_us as f64 / 1_000_000.0;
        if bin_s <= 0.0 {
            return out;
        }
        let bin_f = (data_x_s - strip_data_lo_s) / bin_s;
        if !bin_f.is_finite() || bin_f < 0.0 {
            return out;
        }
        let bin = bin_f as usize;
        if bin >= bins.bin_count {
            return out;
        }
        let scale = self.y_scale();
        let fmt = |name: String, v: f64| -> String {
            if self.as_percent {
                format!("{name}: {v:.1}% busy")
            } else {
                format!("{name}: {v:.1} CPUs busy")
            }
        };
        let mut cum = 0.0_f64;
        for (i, p) in bins.processes.iter().enumerate() {
            let indiv = self.scaled_cpus_busy_by_pid(bins, bin, i) * scale;
            cum += indiv;
            if indiv < 0.05 {
                continue;
            }
            let name = if self.is_kernel_user {
                p.comm.clone()
            } else if p.comm.is_empty() {
                format!("pid {}", p.pid)
            } else {
                format!("{} (pid {})", p.comm, p.pid)
            };
            out.push((fmt(name, indiv), per_pid_color(p.pid), cum));
        }
        let other = self.scaled_cpus_busy_other(bins, bin) * scale;
        cum += other;
        if other >= 0.05 {
            out.push((
                fmt("Other".to_string(), other),
                Color32::from_rgb(150, 150, 160),
                cum,
            ));
        }
        if cum >= 0.05 {
            out.push((fmt("Total".to_string(), cum), Color32::from_gray(200), cum));
        }
        out
    }

    /// True when the underlying bins are empty (no processes
    /// survived top-N selection or the capture is empty). Mirrors
    /// `CpuBusyStripView::is_empty` for symmetry; will be consumed
    /// by step-4 (wire strips into Per-Session / Aggregate views)
    /// to gate strip rendering when there's no data to show.
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.bins.is_empty()
    }

    /// Render the strip into `ui`; see
    /// [`CpuBusyStripView::draw_with_alt`]. `own_gestures` lets this
    /// strip drag-select a time range (Flame). Returns
    /// `(primary_change, alt_change)`.
    pub(crate) fn draw_with_alt(
        &mut self,
        ui: &mut egui::Ui,
        host_t0_us: u64,
        selections: StripSelections,
        view_us: Option<(u64, u64)>,
        utc_axis_origin: Option<u64>,
        own_gestures: bool,
    ) -> (Option<SelectionChange>, Option<SelectionChange>) {
        let current_selection_us = selections.primary;
        let alt_selection_us = selections.alt;
        if self.bins.is_empty() {
            return (None, None);
        }
        let bins = self.effective_bins(view_us).clone();
        if bins.is_empty() {
            return (None, None);
        }
        let scale = self.y_scale();
        let y_label = self.y_label.clone();
        let scale_by_freq = self.scale_by_freq;
        let is_kernel_user = self.is_kernel_user;
        // Local, `self`-free equivalents of `scaled_cpus_busy_by_pid` /
        // `scaled_cpus_busy_other` for use inside the `plot.show`
        // closure below, which also needs a mutable borrow of `self`
        // (for `self.drag`) and so can't call `&self` methods too.
        let scaled_by_pid = |bin: usize, i: usize| -> f64 {
            let v = bins.cpus_busy_by_pid(bin, i);
            if scale_by_freq {
                v * bins.freq_scale_for(bin) as f64
            } else {
                v
            }
        };
        let scaled_other = |bin: usize| -> f64 {
            let v = bins.cpus_busy_other(bin);
            if scale_by_freq {
                v * bins.freq_scale_for(bin) as f64
            } else {
                v
            }
        };
        let bin_s = bins.bin_us as f64 / 1_000_000.0;
        let strip_data_lo_s = (bins.t0_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
        let strip_data_hi_s = strip_data_lo_s + bin_s * bins.bin_count as f64;

        // Visible x-axis range — see CpuBusyStripView::draw for
        // the explanation. host_t0_us is the host’s x=0 reference.
        let (view_lo_s, view_hi_s) = if let Some((lo_us, hi_us)) = view_us {
            let lo = (lo_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            let hi = (hi_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            (lo, hi.max(lo + 1e-6))
        } else {
            (strip_data_lo_s, strip_data_hi_s.max(strip_data_lo_s + 1e-6))
        };

        let mut emitted: Option<SelectionChange> = None;
        let mut emitted_alt: Option<SelectionChange> = None;
        let active_selection = current_selection_us.map(|(start_us, end_us)| {
            let lo = (start_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            let hi = (end_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            (lo, hi)
        });
        let alt_active_selection = alt_selection_us.map(|(start_us, end_us)| {
            let lo = (start_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            let hi = (end_us as i64 - host_t0_us as i64) as f64 / 1_000_000.0;
            (lo, hi)
        });
        let band_b_fill = if alt_selection_us.is_some() {
            REGION_B_FILL
        } else {
            SINGLE_BAND_FILL
        };

        // Y-axis: stack of "CPUs busy by pid" + "Other". Pick the
        // largest stacked-total across all bins as the dynamic ceiling
        // (with a floor of 1.0 so a quiet capture doesn't render
        // pancake-flat).
        let mut peak_stack = 0.0f64;
        for bin in 0..bins.bin_count {
            let mut sum = 0.0;
            for i in 0..bins.processes.len() {
                sum += scaled_by_pid(bin, i);
            }
            sum += scaled_other(bin);
            sum *= scale;
            if sum > peak_stack {
                peak_stack = sum;
            }
        }
        let y_max_nice = if self.as_percent {
            percent_y_axis_top(peak_stack)
        } else {
            nice_ceiling(peak_stack.max(1.0))
        };

        // Y-grid spacer: same {1, 2, 5} × 10ⁿ mechanism the CPU-busy
        // strip uses, picking ~4 ticks across the visible range.
        let y_grid_spacer = move |input: egui_plot::GridInput| -> Vec<GridMark> {
            let (lo, hi) = input.bounds;
            let range = hi - lo;
            if range <= 0.0 {
                return vec![];
            }
            let rough_step = range / 4.0;
            let magnitude = 10.0_f64.powf(rough_step.log10().floor());
            let normalized = rough_step / magnitude;
            let nice_step: f64 = if normalized <= 1.5 {
                magnitude
            } else if normalized <= 3.5 {
                2.0 * magnitude
            } else if normalized <= 7.5 {
                5.0 * magnitude
            } else {
                10.0 * magnitude
            };
            let start = (lo / nice_step).ceil() as i64;
            let end = (hi / nice_step).floor() as i64;
            (start..=end)
                .map(|i| GridMark {
                    value: i as f64 * nice_step,
                    step_size: nice_step,
                })
                .collect()
        };
        let y_formatter =
            |mark: GridMark, _digits: usize, _range: &std::ops::RangeInclusive<f64>| {
                if mark.value.abs() < 1e-9 {
                    String::new()
                } else if (mark.value - mark.value.round()).abs() < 1e-9 {
                    format!("{}", mark.value.round() as i64)
                } else {
                    String::new()
                }
            };
        // max_digits(6) matches CpuBusyStripView and the per-session
        // panels for column alignment.
        let y_axis = AxisHints::new_y()
            .label(RichText::new(y_label).size(self.y_axis_label_size))
            .max_digits(6)
            .label_spacing(egui::Rangef::new(5.0, 6.0))
            .formatter(y_formatter);

        ui.add_space(8.0);
        let plot_h = if self.fill_remaining {
            ui.available_height().max(PLOT_H_MIN)
        } else {
            self.plot_height
        };
        let mut plot = Plot::new(&self.plot_id)
            .height(plot_h)
            .y_axis_width(6)
            .set_margin_fraction(egui::vec2(0.005, 0.05))
            .include_y(0.0)
            .include_y(y_max_nice)
            .auto_bounds(Vec2b::new(false, false))
            .y_grid_spacer(y_grid_spacer)
            .show_x(false)
            .show_y(false)
            .allow_scroll(false)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .custom_y_axes(vec![y_axis])
            .sense(egui::Sense::click_and_drag());
        plot = maybe_legend(plot, self.show_legends);
        plot = plot.custom_x_axes(vec![strip_x_axis_hints(
            &self.x_axis_title,
            utc_axis_origin,
        )]);
        if utc_axis_origin.is_some() {
            plot = plot.x_grid_spacer(utc_x_grid_spacer);
        }

        let band_y_lo = 0.0_f64;
        let band_y_hi = y_max_nice;

        let inner = plot.show(ui, |plot_ui| {
            // Force the plot bounds for this frame — see the
            // matching comment in CpuBusyStripView::draw.
            plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                [view_lo_s, 0.0],
                [view_hi_s, y_max_nice],
            ));

            // Stack order: largest pid at the bottom, "Other"
            // overlay sits on top of the per-pid stack so the
            // reader sees the dominant pid first and the long tail
            // as a single grey cap. The cumulative line at index N
            // is the sum of layers 0..=N for each bin.
            let n = bins.processes.len();
            let mut cum = vec![0.0f64; bins.bin_count];
            if is_kernel_user && n == 2 {
                // Kernel vs User: a proper two-band filled stacked
                // area (rather than the generic bare-line stack below)
                // since there are always exactly two, well-known bands.
                let mut kernel_pts: Vec<[f64; 2]> = Vec::with_capacity(bins.bin_count);
                let mut total_pts: Vec<[f64; 2]> = Vec::with_capacity(bins.bin_count);
                for (bin, cum_slot) in cum.iter_mut().enumerate() {
                    let t = strip_data_lo_s + bin as f64 * bin_s + bin_s * 0.5;
                    let kernel_v = scaled_by_pid(bin, 0) * scale;
                    let total_v = kernel_v + scaled_by_pid(bin, 1) * scale;
                    kernel_pts.push([t, kernel_v]);
                    total_pts.push([t, total_v]);
                    *cum_slot = total_v;
                }
                let kernel_color = per_pid_color(bins.processes[0].pid);
                let user_color = per_pid_color(bins.processes[1].pid);
                draw_filled_band(plot_ui, &kernel_pts, None, kernel_color);
                draw_filled_band(plot_ui, &total_pts, Some(&kernel_pts), user_color);
                plot_ui.line(
                    Line::new(PlotPoints::new(kernel_pts))
                        .color(kernel_color)
                        .width(1.0_f32)
                        .name(bins.processes[0].comm.clone()),
                );
                plot_ui.line(
                    Line::new(PlotPoints::new(total_pts))
                        .color(user_color)
                        .width(1.0_f32)
                        .name(bins.processes[1].comm.clone()),
                );
            } else {
                // Per-pid stacked layers.
                for i in 0..n {
                    let mut pts: Vec<[f64; 2]> = Vec::with_capacity(bins.bin_count);
                    for (bin, c) in cum.iter_mut().enumerate() {
                        *c += scaled_by_pid(bin, i) * scale;
                        let t = strip_data_lo_s + bin as f64 * bin_s + bin_s * 0.5;
                        pts.push([t, *c]);
                    }
                    let pid = bins.processes[i].pid;
                    let comm = &bins.processes[i].comm;
                    let label = if comm.is_empty() {
                        format!("pid {pid}")
                    } else {
                        format!("{comm} (pid {pid})")
                    };
                    plot_ui.line(
                        Line::new(PlotPoints::new(pts))
                            .color(per_pid_color(pid))
                            .width(1.0_f32)
                            .name(label),
                    );
                }
            }
            // "Other" cap layer if it carries any samples.
            let mut other_total: u64 = 0;
            for &v in &bins.other_per_bin {
                other_total += v as u64;
            }
            if other_total > 0 {
                let mut pts: Vec<[f64; 2]> = Vec::with_capacity(bins.bin_count);
                for (bin, c) in cum.iter_mut().enumerate() {
                    *c += scaled_other(bin) * scale;
                    let t = strip_data_lo_s + bin as f64 * bin_s + bin_s * 0.5;
                    pts.push([t, *c]);
                }
                plot_ui.line(
                    Line::new(PlotPoints::new(pts))
                        .color(Color32::from_rgb(150, 150, 160))
                        .width(0.8_f32)
                        .name("Other"),
                );
            }

            if own_gestures {
                let plot_resp = plot_ui.response();
                let pointer = plot_ui.pointer_coordinate();
                let shift = plot_ui.ctx().input(|i| i.modifiers.shift);
                let alt = plot_ui.ctx().input(|i| i.modifiers.alt);
                if plot_resp.drag_started_by(egui::PointerButton::Primary) {
                    if let Some(p) = pointer {
                        let active = if alt {
                            alt_active_selection
                        } else {
                            active_selection
                        };
                        self.drag =
                            Some(begin_strip_drag(shift, p.x, active, view_lo_s, view_hi_s));
                        self.drag_is_alt = alt;
                    }
                }
                if plot_resp.dragged_by(egui::PointerButton::Primary) {
                    if let Some(p) = pointer {
                        if let Some(StripTimeDrag::Resize { anchor, .. }) = self.drag {
                            self.drag = Some(StripTimeDrag::Resize {
                                anchor,
                                current: p.x,
                            });
                        }
                    }
                }
                if plot_resp.drag_stopped_by(egui::PointerButton::Primary) {
                    if let Some(drag) = self.drag.take() {
                        let x = pointer.map(|p| p.x).unwrap_or(match drag {
                            StripTimeDrag::Resize { current, .. } => current,
                            StripTimeDrag::Translate { press_x, .. } => press_x,
                        });
                        let change = commit_strip_drag(
                            drag,
                            x,
                            view_lo_s,
                            view_hi_s,
                            host_t0_us,
                            bin_s * 0.5,
                        );
                        if self.drag_is_alt {
                            emitted_alt = change;
                        } else {
                            emitted = change;
                        }
                    }
                }

                let preview_x = pointer.map(|p| p.x);
                if let Some(drag) = self.drag {
                    let x = preview_x.unwrap_or(match drag {
                        StripTimeDrag::Resize { current, .. } => current,
                        StripTimeDrag::Translate { press_x, .. } => press_x,
                    });
                    let (lo, hi) = strip_drag_band(drag, x, view_lo_s, view_hi_s);
                    let fill = if self.drag_is_alt {
                        REGION_A_FILL
                    } else {
                        band_b_fill
                    };
                    draw_x_band(plot_ui, lo, hi, band_y_lo, band_y_hi, fill);
                } else {
                    if let Some((lo, hi)) = alt_active_selection {
                        let band_covers_view =
                            (lo - view_lo_s).abs() < 1e-6 && (hi - view_hi_s).abs() < 1e-6;
                        if !band_covers_view {
                            draw_x_band(plot_ui, lo, hi, band_y_lo, band_y_hi, REGION_A_FILL);
                        }
                    }
                    if let Some((lo, hi)) = active_selection {
                        let band_covers_view =
                            (lo - view_lo_s).abs() < 1e-6 && (hi - view_hi_s).abs() < 1e-6;
                        if !band_covers_view {
                            draw_x_band(plot_ui, lo, hi, band_y_lo, band_y_hi, band_b_fill);
                        }
                    }
                }
            }
        });

        if own_gestures && inner.response.secondary_clicked() && self.drag.is_none() {
            let alt = ui.input(|i| i.modifiers.alt);
            if alt {
                if alt_selection_us.is_some() {
                    emitted_alt = Some(SelectionChange::Clear);
                }
            } else if active_selection.is_some() {
                emitted = Some(SelectionChange::Clear);
            }
        }

        // Save cursor-tracking metadata — see CpuBusyStripView::draw.
        self.last_rect = Some(inner.response.rect);
        self.last_transform = Some(inner.transform);
        self.last_strip_data_lo_s = Some(strip_data_lo_s);

        (emitted, emitted_alt)
    }
}

/// Hash-based stable colour for a pid. Same trick as
/// [`per_cpu_color`] but seeded with a different multiplier so
/// adjacent pid ↔ cpu mappings don't accidentally produce the
/// same colour on the same canvas.
///
/// Special-cases [`analysis::KERNEL_PSEUDO_PID`] /
/// [`analysis::USER_PSEUDO_PID`] (the synthetic rows
/// [`analysis::compute_kernel_user_usage_bins`] emits) to two fixed,
/// meaningful colours instead of hashing them like a real pid.
pub fn per_pid_color(pid: u32) -> Color32 {
    if pid == analysis::KERNEL_PSEUDO_PID {
        return Color32::from_rgb(210, 130, 90);
    }
    if pid == analysis::USER_PSEUDO_PID {
        return Color32::from_rgb(100, 170, 220);
    }
    let h = pid.wrapping_mul(0x85EB_CA77).wrapping_add(0x9E37_79B1);
    let r = ((h >> 16) & 0xFF) as u8;
    let g = ((h >> 8) & 0xFF) as u8;
    let b = (h & 0xFF) as u8;
    Color32::from_rgb(r.max(120), g.max(120), b.max(120))
}

/// Fill the band under `top` (or between `top` and `bottom`, if given)
/// with one small convex quad per bin-to-bin segment, rather than one
/// polygon spanning the whole curve. `top`/`bottom` are same-length,
/// same-x point lists; each quad's two "vertical" edges both run from
/// `bottom` up to `top`, so every quad is convex regardless of how
/// jagged the curves are (unlike a naive whole-curve polygon, which
/// egui_plot's fan triangulation can render incorrectly for a
/// non-star-shaped area). `Line::fill()` would also work but caps out
/// around 5-10% opacity, too faint for a two-tone stacked area.
fn draw_filled_band(
    plot_ui: &mut egui_plot::PlotUi,
    top: &[[f64; 2]],
    bottom: Option<&[[f64; 2]]>,
    color: Color32,
) {
    if top.len() < 2 {
        return;
    }
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 190);
    for i in 0..top.len() - 1 {
        let (x0, top0) = (top[i][0], top[i][1]);
        let (x1, top1) = (top[i + 1][0], top[i + 1][1]);
        let (bot0, bot1) = match bottom {
            Some(b) => (b[i][1], b[i + 1][1]),
            None => (0.0, 0.0),
        };
        let pts = PlotPoints::new(vec![[x0, bot0], [x0, top0], [x1, top1], [x1, bot1]]);
        plot_ui.polygon(
            egui_plot::Polygon::new(pts)
                .fill_color(fill)
                .stroke(Stroke::NONE),
        );
    }
}

/// Hash-based palette for per-CPU lines. Keep adjacent CPU ids
/// visually distinct without forcing the user to read a legend on
/// every glance — the eye picks colour difference faster than CPU
/// numbers in a thin strip plot.
pub fn per_cpu_color(cpu_id: u16) -> Color32 {
    // Cheap mix; keep the palette saturated and bright enough to
    // read on the dark background.
    let h = (cpu_id as u32).wrapping_mul(2654435761) ^ 0x9E3779B9;
    let r = ((h >> 16) & 0xFF) as u8;
    let g = ((h >> 8) & 0xFF) as u8;
    let b = (h & 0xFF) as u8;
    // Lift each channel to keep it readable on a dark plot
    // background — pure low-channel hashes can come out as near-black.
    Color32::from_rgb(r.max(120), g.max(120), b.max(120))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elog_parser::{
        CpuStackSample, KernelInfo, CPU_SAMPLE_FLAG_FREQ_VALID, CPU_SAMPLE_FLAG_IDLE,
        CPU_SAMPLE_FLAG_USER_MODE,
    };

    fn cpu_panel() -> CpuUsagePanel {
        let mut samples = Vec::new();
        for i in 0..1_000 {
            for cpu in [0, 8] {
                samples.push(CpuStackSample {
                    ts: 1_000_000 + i * 1_000,
                    cpu,
                    pid: cpu as u32 + 1,
                    tid: cpu as u32 + 1,
                    sample_type: 1,
                    flags: CPU_SAMPLE_FLAG_FREQ_VALID
                        | if cpu == 0 {
                            CPU_SAMPLE_FLAG_USER_MODE
                                | if i % 5 == 0 { 0 } else { CPU_SAMPLE_FLAG_IDLE }
                        } else {
                            0
                        },
                    freq_mhz: Some(if cpu == 0 { 1_000 } else { 2_000 }),
                    frames: Vec::new(),
                });
            }
        }
        let profile = Arc::new(CpuProfile {
            samples,
            kernel: Some(KernelInfo {
                hz: 1_000,
                ..Default::default()
            }),
            ..Default::default()
        });
        CpuUsagePanel::build(
            &profile,
            CpuUsagePanelStyle {
                plot_height: 110.0,
                x_axis_title: None,
                y_axis_label_size: 12.0,
            },
        )
        .unwrap()
    }

    #[test]
    fn cpu_selection_scales_every_usage_mode_to_selected_capacity() {
        let mut panel = cpu_panel();
        for (cpus, percent) in [(&[0][..], 20.0), (&[8][..], 100.0), (&[0, 8][..], 60.0)] {
            panel.sync_bins(20, cpus);
            let grid = &panel.cpu_strip.bins;
            assert_eq!(grid.cpu_ids, cpus);
            assert_eq!(grid.t0_us, 1_000_000);
            let bin = 1;
            assert_eq!(grid.cpus_busy(bin) * panel.cpu_strip.y_scale(), percent);
            for strip in [&panel.process_strip, &panel.kernel_user_strip] {
                let total: f64 = (0..strip.bins.processes.len())
                    .map(|i| strip.bins.cpus_busy_by_pid(bin, i))
                    .sum::<f64>()
                    + strip.bins.cpus_busy_other(bin);
                assert_eq!(total * strip.y_scale(), percent);
                assert_eq!(strip.ncpus, cpus.len());
            }
        }
        panel.sync_bins(20, &[0]);
        assert_eq!(panel.process_strip.bins.processes.len(), 1);
        assert_eq!(panel.process_strip.bins.processes[0].pid, 1);
        assert_eq!(panel.kernel_user_strip.bins.processes[0].total_samples, 0);
        panel.cpu_strip.scale_by_freq = true;
        assert_eq!(
            panel.cpu_strip.scaled_cpus_busy(&panel.cpu_strip.bins, 1) * panel.cpu_strip.y_scale(),
            10.0
        );
    }

    #[test]
    fn cpu_selection_survives_zoom_and_invalidates_cached_bins() {
        let mut panel = cpu_panel();
        let zoom = Some((1_200_000, 1_800_000));
        panel.sync_bins(20, &[8]);
        panel.cpu_strip.effective_bins(zoom);
        panel.process_strip.effective_bins(zoom);
        panel.kernel_user_strip.effective_bins(zoom);

        panel.sync_bins(20, &[0]);
        assert!(panel.cpu_strip.zoom_cache.is_none());
        assert!(panel.process_strip.zoom_cache.is_none());
        assert!(panel.kernel_user_strip.zoom_cache.is_none());
        let grid = panel.cpu_strip.effective_bins(zoom).clone();
        assert_eq!(grid.cpu_ids, [0]);
        assert_eq!(grid.cpus_busy(1), 0.2);
        let processes = panel.process_strip.effective_bins(zoom);
        assert_eq!(processes.processes.len(), 1);
        assert_eq!(processes.processes[0].pid, 1);
        let ku = panel.kernel_user_strip.effective_bins(zoom);
        assert_eq!(ku.processes[0].total_samples, 0);
        assert_eq!(ku.cpus_busy_by_pid(1, 1), grid.cpus_busy(1));
    }

    #[test]
    fn empty_cpu_selection_results_clear_and_all_cpus_restore() {
        let mut panel = cpu_panel();
        panel.sync_bins(20, &[7]);
        assert!(panel.cpu_strip.bins.is_empty());
        assert!(panel.process_strip.bins.is_empty());
        assert!(panel.kernel_user_strip.bins.is_empty());
        panel.sync_bins(20, &[]);
        assert_eq!(panel.cpu_strip.bins.cpu_ids, [0, 8]);
        assert_eq!(panel.process_strip.ncpus, 2);
        assert_eq!(panel.kernel_user_strip.ncpus, 2);
        assert_eq!(
            panel.cpu_strip.bins.cpus_busy(1) * panel.cpu_strip.y_scale(),
            60.0
        );
    }
}
