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

//! Interfaces, per-stack TCP, STARS controller, and HPTS wheel-callout
//! panels (`interfaces_section`,
//! `http_rps_section`, `tcp_goodput_section`, `tcp_send_section`,
//! `tcp_lro_flush_section`, `stars_agg_section`, `hpts_hist_section`),
//! backed by the `if`, `tcp`, and `hpts` periodic sample providers. Like
//! the other aggregate panels these are static (no built-in zoom/scroll)
//! and share the [`agg_y_axis`] left-aligned gutter; they link their
//! stacked panels on a common x group so the cursor stays synchronised.

#[allow(unused_imports)]
use super::*;

// ---------------------------------------------------------------------------
// Shared plot helper
// ---------------------------------------------------------------------------

/// Number of frames to keep force-repainting after a view is (re)built.
/// egui is event-driven — without an input event it won't render another
/// frame, so egui_plot's first-frame auto-fit (and the linked-axis
/// propagation across the stacked panels) would otherwise stall until the
/// user moves the mouse. A short burst of repaints lets the bounds settle
/// on their own, then the tab goes idle again.
pub(crate) const SETTLE_FRAMES: u8 = 4;

/// Fixed height for every time-series / distribution panel on the
/// consolidated Aggregate tab. A uniform height (rather than
/// stretch-to-fill) keeps the long scrolling dashboard visually
/// even and predictable no matter how many providers contributed.
pub(crate) const AGG_PANEL_H: f32 = 260.0;

/// Format an axis tick: integers without a decimal point, fractionals
/// trimmed to at most three decimals. Shared by both aggregate axes so
/// every panel prints numbers the same way the CPU strip does.
fn fmt_axis_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// Shared y-axis for the Aggregate tab's stacked panels. Every panel
/// reserves the same 6-digit gutter so their plot areas share one left
/// edge and print their y tick numbers there (just like the CPU strip
/// up top), with the flush-left "0" tick suppressed. Throughput-style
/// panels pin their lower bound to zero via `include_y(0.0)`; duration-
/// ratio plots scale to the visible series instead.
pub(crate) fn agg_y_axis(label: &str) -> AxisHints {
    agg_y_axis_scaled(label, 1.0)
}

/// Digit columns reserved for the throughput/packets tick labels.
const MAIN_Y_DIGITS: usize = 6;
const Y_AXIS_LINE_H: f32 = 12.0;

/// [`agg_y_axis`] that prints `mark * scale`. `scale` is 1 except for a
/// Gb/s series redrawn in Mb/s, Kb/s, or b/s.
fn agg_y_axis_scaled(label: &str, scale: f64) -> AxisHints {
    AxisHints::new_y()
        .label(RichText::new(label).size(12.0))
        .max_digits(MAIN_Y_DIGITS)
        .label_spacing(egui::Rangef::new(16.0, 48.0))
        .formatter(move |mark, _digits, _range| {
            let shown = mark.value * scale;
            if shown.abs() < 1e-9 {
                String::new()
            } else {
                fmt_axis_num(shown)
            }
        })
}

/// Display unit for a series stored in Gb/s. The coarsest unit in which
/// `peak_gbps` is at least 1.
fn throughput_display(peak_gbps: f64) -> (f64, &'static str) {
    if peak_gbps >= 1.0 {
        (1.0, "Gb/s")
    } else if peak_gbps >= 1e-3 {
        (1e3, "Mb/s")
    } else if peak_gbps >= 1e-6 {
        (1e6, "Kb/s")
    } else if peak_gbps > 0.0 {
        (1e9, "b/s")
    } else {
        (1.0, "Gb/s")
    }
}

/// Shared time x-axis for the Aggregate tab's stacked panels: no axis
/// title, integer-second tick labels, and the flush-left "0" tick
/// suppressed — so every panel's x-axis reads the same as the CPU strip
/// up top, which omits the "0". Number labels are otherwise unchanged.
///
/// `utc_origin` is `Some(start_utc_us)` when the "UTC time labels"
/// toggle is on (see [`utc_axis_origin`]); ticks then render as
/// wall-clock time-of-day instead of elapsed seconds, and the tick at
/// the view's left edge (rather than being blanked) carries the one
/// `UTC+` anchor for the whole row — see [`format_time_axis_tick`].
/// Pair with [`utc_x_grid_spacer`] (via `Plot::x_grid_spacer`) so that
/// anchor tick actually exists to format.
pub(crate) fn agg_x_time_axis(utc_origin: Option<u64>) -> AxisHints {
    AxisHints::new_x()
        .label_spacing(egui::Rangef::new(36.0, 40.0))
        .formatter(move |mark, _digits, range| match utc_origin {
            Some(origin) => format_time_axis_tick(mark.value, mark.step_size, origin, range),
            None if mark.value.abs() < 1e-9 => String::new(),
            None => fmt_axis_num(mark.value),
        })
}

/// Custom x grid spacer for UTC-mode time axes: the normal
/// `log_grid_spacer(10)` picks, plus one extra mark pinned to the
/// current view's left edge with a `step_size` of `f64::INFINITY` so
/// egui_plot's per-tick width check (which skips any label wider than
/// `dpos_dvalue * step_size`) never culls it. That guarantees the axis
/// always has a mark at the left edge for [`format_time_axis_tick`] to
/// render its `UTC+` anchor on, even when the view doesn't start on a
/// "nice" round number (a zoomed window, or a strip whose data starts a
/// beat after the shared `t=0`). No-op when not in UTC mode — callers
/// only attach this via `Plot::x_grid_spacer` when `utc_origin.is_some()`.
pub(crate) fn utc_x_grid_spacer(input: GridInput) -> Vec<GridMark> {
    let (lo, hi) = input.bounds;
    let mut marks = log_grid_spacer(10)(input);
    let eps = (hi - lo).abs().max(1.0) * 1e-6;
    if !marks.iter().any(|m| (m.value - lo).abs() < eps) {
        marks.push(GridMark {
            value: lo,
            step_size: f64::INFINITY,
        });
    }
    marks
}

// ---------------------------------------------------------------------------
// Shared hover cursor (System tab)
// ---------------------------------------------------------------------------

/// One registered panel's worth of hover state: its plot rect, the data x
/// under the cursor, and the tracking dots + tooltip rows sampled from its
/// series at that x.
struct PanelCursor {
    rect: egui::Rect,
    data_x: f64,
    /// This panel's visible x-range width (seconds), used to pick the
    /// tooltip's UTC millisecond cutoff to match the axis ticks (see
    /// [`format_axis_utc`]).
    view_width_secs: f64,
    dots: Vec<(egui::Pos2, Color32)>,
    entries: Vec<(String, Color32)>,
}

/// Per-frame accumulator for the System tab's shared hover cursor.
///
/// The System tab stacks many independently-rendered time panels (the CPU
/// strip, STARS pressure/deny + budget/peak, session count, Network
/// TX/RX, HTTP Requests per Second, TCP Connections by State, TCP
/// Connection Rate, and the TCP
/// Goodput stacked area).
/// Each is its own `Plot`, so egui_plot's built-in hover readout only ever
/// covers the one panel under the pointer. This scratch reproduces the
/// per-session plots' shared cursor: every panel registers its rect (and,
/// where it has line series, tracking dots + tooltip rows) at the hovered
/// x. A single vertical cursor line is then painted down every panel (so
/// the aligned time is highlighted across the whole tab), but the tracking
/// dots and value tooltip are drawn only for the panel directly under the
/// pointer — so the tooltip stays focused and changes as you move between
/// panels rather than listing every series at once.
///
/// Every panel shares the same left-aligned y gutter and spans the full
/// width, so one screen x maps to (very nearly) the same capture time in
/// each; each panel converts that screen x to its own data x via its own
/// transform, so tiny per-panel x-range differences don't skew the readout.
#[derive(Default)]
pub(crate) struct SysCursor {
    pointer: Option<egui::Pos2>,
    cursor_x: f32,
    y_lo: f32,
    y_hi: f32,
    panels: Vec<PanelCursor>,
    /// Geometry of every stacked time panel this frame, including CPU
    /// Usage. Fed to the [`TimeAxisController`] gesture pass after all
    /// panels draw.
    geoms: Vec<super::PanelGeom>,
    /// UTC anchor for the "t = …" tooltip line and every stacked panel's
    /// x-axis ticks (see [`agg_x_time_axis`]); `None` shows plain
    /// seconds-since-capture as before.
    pub(crate) utc_origin: Option<u64>,
    /// Height of the catalog plot currently being drawn.
    pub(crate) panel_h: f32,
    /// When set, [`Self::plot_h`] uses the space left *after* titles
    /// and other chrome so a maximized plot fits the tab without scroll.
    pub(crate) fill_remaining: bool,
    pub(crate) show_legends: bool,
    pub(crate) last_plot_rect: Option<egui::Rect>,
    /// Equal-time buckets for plots that bin the visible window.
    pub(crate) plot_bins: usize,
}

impl SysCursor {
    pub(crate) fn new(ui: &egui::Ui, utc_origin: Option<u64>, plot_bins: usize) -> Self {
        Self {
            pointer: ui.input(|i| i.pointer.latest_pos()),
            utc_origin,
            panel_h: AGG_PANEL_H,
            fill_remaining: false,
            show_legends: true,
            last_plot_rect: None,
            plot_bins: plot_bins.clamp(PLOT_BINS_MIN, PLOT_BINS_MAX),
            ..Default::default()
        }
    }

    /// Geometry of the gesture-eligible time panels registered this frame.
    pub(crate) fn geoms(&self) -> &[super::PanelGeom] {
        &self.geoms
    }

    /// Plot widget height: stored catalog height, or whatever is left
    /// in `ui` when this row is maximized (call after drawing titles).
    pub(crate) fn plot_h(&self, ui: &egui::Ui) -> f32 {
        if self.fill_remaining {
            ui.available_height().max(PLOT_H_MIN)
        } else {
            self.panel_h
        }
    }

    /// Register a time panel spanning `rect` with x-transform `transform`.
    /// When `gesture` is true the panel joins the shared zoom/pan gesture
    /// union. When the pointer is
    /// horizontally within the panel column, records the panel (so the
    /// cursor line spans it) and returns the capture time (data x, seconds)
    /// under the cursor so the caller can sample its series; returns `None`
    /// otherwise. Subsequent [`add`] calls attach to this panel.
    pub(crate) fn panel(
        &mut self,
        rect: egui::Rect,
        transform: &egui_plot::PlotTransform,
        gesture: bool,
    ) -> Option<f64> {
        if gesture {
            self.geoms.push((rect, *transform));
        }
        let pos = self.pointer?;
        if pos.x < rect.left() || pos.x > rect.right() {
            return None;
        }
        if self.panels.is_empty() {
            self.y_lo = rect.top();
            self.y_hi = rect.bottom();
        } else {
            self.y_lo = self.y_lo.min(rect.top());
            self.y_hi = self.y_hi.max(rect.bottom());
        }
        self.cursor_x = pos.x;
        let data_x = transform.value_from_position(pos).x;
        self.panels.push(PanelCursor {
            rect,
            data_x,
            view_width_secs: transform.bounds().width(),
            dots: Vec::new(),
            entries: Vec::new(),
        });
        Some(data_x)
    }

    /// Add a tracking dot + tooltip row for one series value at the cursor,
    /// attached to the most recently registered panel. `transform` is that
    /// panel's transform (used to place the dot at the series' y); the
    /// dot's x is pinned to the shared cursor line.
    pub(crate) fn add(
        &mut self,
        transform: &egui_plot::PlotTransform,
        data_x: f64,
        y: f64,
        text: String,
        color: Color32,
    ) {
        let dot_y = transform.position_from_point(&PlotPoint { x: data_x, y }).y;
        let cursor_x = self.cursor_x;
        if let Some(p) = self.panels.last_mut() {
            p.dots.push((egui::pos2(cursor_x, dot_y), color));
            p.entries.push((text, color));
        }
    }

    /// Tooltip row without a tracking dot (stacked Total).
    pub(crate) fn add_text(&mut self, text: String, color: Color32) {
        if let Some(p) = self.panels.last_mut() {
            p.entries.push((text, color));
        }
    }

    /// Layers from [`CpuUsagePanel::cursor_series`]: Total is text-only
    /// so it does not stack a second dot on the top band.
    pub(crate) fn add_cpu_series(
        &mut self,
        transform: &egui_plot::PlotTransform,
        data_x: f64,
        series: &[(String, Color32, f64)],
    ) {
        for (label, color, y) in series {
            if label.starts_with("Total:") {
                self.add_text(label.clone(), *color);
            } else {
                self.add(transform, data_x, *y, label.clone(), *color);
            }
        }
    }

    /// Paint the shared cursor line down every registered panel, plus the
    /// tracking dots and value tooltip for the single panel under the
    /// pointer. No-op unless the pointer is inside the vertical span of the
    /// registered panels.
    pub(crate) fn draw(&self, ui: &egui::Ui) {
        let Some(pos) = self.pointer else { return };
        if self.panels.is_empty() {
            return;
        }
        if pos.y < self.y_lo || pos.y > self.y_hi {
            return;
        }
        let painter = ui.painter();
        let stroke = Stroke::new(1.0_f32, Color32::from_gray(180));
        for p in &self.panels {
            painter.vline(self.cursor_x, p.rect.top()..=p.rect.bottom(), stroke);
        }

        // Only the panel directly under the pointer gets dots + a tooltip.
        let Some(hovered) = self.panels.iter().find(|p| p.rect.contains(pos)) else {
            return;
        };
        for (dot, color) in &hovered.dots {
            if hovered.rect.contains(*dot) {
                painter.circle_filled(*dot, 4.5, Color32::from_gray(20));
                painter.circle_filled(*dot, 3.0, *color);
            }
        }

        let font = egui::FontId::proportional(12.0);
        let padding = 6.0_f32;
        let line_spacing = 2.0_f32;
        let mut rows: Vec<(String, Color32)> = Vec::with_capacity(hovered.entries.len() + 1);
        let time_label = match self.utc_origin {
            Some(origin) => {
                format_tooltip_utc(hovered.data_x, origin, hovered.view_width_secs / 6.0)
            }
            None => format!("t = {:.2}s", hovered.data_x),
        };
        rows.push((time_label, Color32::from_gray(220)));
        rows.extend(hovered.entries.iter().cloned());

        let galleys: Vec<_> = rows
            .iter()
            .map(|(text, color)| painter.layout_no_wrap(text.clone(), font.clone(), *color))
            .collect();
        let box_w = galleys.iter().map(|g| g.size().x).fold(0.0_f32, f32::max) + padding * 2.0;
        let box_h = galleys.iter().map(|g| g.size().y).sum::<f32>()
            + (galleys.len().saturating_sub(1) as f32) * line_spacing
            + padding * 2.0;

        let box_x = if self.cursor_x + 14.0 + box_w < hovered.rect.right() {
            self.cursor_x + 14.0
        } else {
            self.cursor_x - 14.0 - box_w
        }
        .max(hovered.rect.left());
        let box_y = (pos.y - box_h * 0.5).clamp(
            hovered.rect.top(),
            (hovered.rect.bottom() - box_h).max(hovered.rect.top()),
        );
        let box_rect =
            egui::Rect::from_min_size(egui::pos2(box_x, box_y), egui::vec2(box_w, box_h));
        painter.rect_filled(
            box_rect,
            4.0,
            Color32::from_rgba_premultiplied(25, 25, 30, 230),
        );
        painter.rect_stroke(box_rect, 4.0, Stroke::new(1.0_f32, Color32::from_gray(70)));
        let mut text_y = box_y + padding;
        for galley in galleys {
            let h = galley.size().y;
            painter.galley(
                egui::pos2(box_x + padding, text_y),
                galley,
                Color32::TRANSPARENT,
            );
            text_y += h + line_spacing;
        }
    }
}

/// Format a series value for the shared-cursor tooltip: fewer decimals as
/// the magnitude grows, with trailing zeros trimmed.
fn fmt_cursor_val(v: f64) -> String {
    let s = if v.abs() >= 100.0 {
        format!("{v:.0}")
    } else if v.abs() >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    };
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Hide TCP on the HTTP responses plot once egui has stored the
/// legend. Returns false until that memory exists.
fn hide_http_rps_legend(ctx: &egui::Context) -> bool {
    let id = egui::Id::new("http_rps_plot");
    let Some(mut mem) = PlotMemory::load(ctx, id) else {
        return false;
    };
    let changed = mem.hidden_items.insert("TCP".to_string());
    if changed {
        mem.store(ctx, id);
    }
    true
}

/// One named line for [`draw_series_plot`].
pub(crate) struct PlotLine<'a> {
    pub(crate) name: String,
    pub(crate) color: Color32,
    pub(crate) data: &'a [[f64; 2]],
}

pub(crate) struct PlotMarker<'a> {
    pub(crate) name: &'a str,
    pub(crate) color: Color32,
    pub(crate) shape: MarkerShape,
    pub(crate) data: &'a [[f64; 2]],
}

/// Hover/caption cutoff past the last sample (seconds). 1 Hz STATS
/// interpolates between ticks; past this window the cursor omits the series.
pub(crate) const SERIES_STALE_AFTER_S: f64 = 2.0;

struct SecondaryDraw<'a> {
    name: &'a str,
    color: Color32,
    raw: &'a [[f64; 2]],
    mapped: Vec<[f64; 2]>,
}

/// Map `lines` onto `0..plot_top` using one shared scale (the visible
/// peak, plus the usual 8% headroom). Hidden and empty series are left
/// out of both the scale and the draw list.
fn layout_secondary<'a>(
    lines: &'a [PlotLine<'a>],
    hidden: impl Fn(&str) -> bool,
    view: [f64; 2],
    plot_top: f64,
) -> Option<(f64, f64, Vec<SecondaryDraw<'a>>)> {
    if plot_top <= 0.0 {
        return None;
    }
    let mut prepared = Vec::new();
    let mut peak = 0.0_f64;
    for line in lines {
        if line.data.is_empty() || hidden(&line.name) {
            continue;
        }
        let ds = downsample_mean(line.data, view, MAX_DISPLAY_POINTS);
        for p in &ds {
            peak = peak.max(p[1]);
        }
        prepared.push((line, ds));
    }
    if prepared.is_empty() {
        return None;
    }
    let secondary_top = y_axis_top(peak);
    let drawn = prepared
        .into_iter()
        .map(|(line, ds)| SecondaryDraw {
            name: line.name.as_str(),
            color: line.color,
            raw: line.data,
            mapped: ds
                .into_iter()
                .map(|p| [p[0], p[1] * plot_top / secondary_top])
                .collect(),
        })
        .collect();
    Some((peak, secondary_top, drawn))
}

fn fmt_drop_axis(v: f64) -> String {
    let av = v.abs();
    if av >= 1_000_000.0 {
        fmt_compact_unit(v / 1_000_000.0, "M")
    } else if av >= 1_000.0 {
        fmt_compact_unit(v / 1_000.0, "k")
    } else {
        fmt_axis_num(v)
    }
}

fn fmt_compact_unit(shown: f64, suffix: &str) -> String {
    if (shown - shown.round()).abs() < 0.05 {
        format!("{}{suffix}", shown.round() as i64)
    } else {
        format!("{shown:.1}{suffix}")
    }
}

/// Drops/s name on the throughput axis name's line, and each drops tick
/// label directly under that throughput tick. The gutter stays the
/// normal 6-digit width.
fn paint_drop_axis(
    ui: &egui::Ui,
    transform: &egui_plot::PlotTransform,
    plot_rect: egui::Rect,
    plot_top: f64,
    drop_peak: f64,
    drop_top: f64,
) {
    if drop_peak <= 0.0 || drop_top <= 0.0 || plot_top <= 0.0 {
        return;
    }
    let axis_w = (MAIN_Y_DIGITS as f32 + 1.0) * Y_AXIS_LINE_H;
    let gutter = egui::Rect::from_min_max(
        egui::pos2(plot_rect.left() - axis_w, plot_rect.top()),
        egui::pos2(plot_rect.left(), plot_rect.bottom()),
    );
    let painter = ui.painter().with_clip_rect(gutter);
    let color = COLOR_SSTHRESH;
    let unit = painter.layout_no_wrap(
        "drops/s".to_string(),
        egui::FontId::proportional(12.0),
        color,
    );
    // Gb/s is rotated on the gutter's left edge. drops/s shares that
    // line, one text-height to the right.
    let unit_pos = egui::pos2(
        gutter.left() + Y_AXIS_LINE_H,
        gutter.center().y + unit.size().x / 2.0,
    );
    painter.add(egui::Shape::Text(
        egui::epaint::TextShape::new(unit_pos, unit, color)
            .with_angle(-std::f32::consts::FRAC_PI_2),
    ));

    let marks = nice_y_grid_spacer(egui_plot::GridInput {
        bounds: (0.0, plot_top),
        base_step_size: plot_top / 4.0,
    });
    let drops_per_plot = drop_top / plot_top;
    let font = egui::TextStyle::Body.resolve(ui.style());
    for mark in marks {
        if mark.value.abs() < 1e-9 {
            continue;
        }
        let screen_y = transform
            .position_from_point(&PlotPoint {
                x: 0.0,
                y: mark.value,
            })
            .y;
        let galley = painter.layout_no_wrap(
            fmt_drop_axis(mark.value * drops_per_plot),
            font.clone(),
            color,
        );
        // Primary tick text is centered on the tick and right-aligned to
        // the plot edge. This one starts at that text's bottom.
        let pos = egui::pos2(
            plot_rect.left() - galley.size().x,
            screen_y + galley.size().y / 2.0,
        );
        if pos.x < unit_pos.x + Y_AXIS_LINE_H {
            continue;
        }
        painter.galley(pos, galley, color);
    }
}

/// Draw one stacked time-series panel on the System tab's shared time
/// axis. `seed` is `(x_lo, x_hi, x_auto)` from the [`TimeAxisController`]
/// (the caller also runs `controller.apply` before this so a zoomed
/// window's bounds are forced). Each series is downsampled to the
/// currently-visible x-range every frame so zooming in reveals real
/// resolution rather than a pre-baked decimation.
pub(crate) fn draw_series_plot(
    ui: &mut egui::Ui,
    plot_id: &str,
    y_label: &str,
    height: f32,
    lines: &[PlotLine<'_>],
    cursor: &mut SysCursor,
    seed: (f64, f64, bool),
) {
    draw_series_plot_marked(
        ui,
        plot_id,
        y_label,
        height,
        lines,
        &[],
        cursor,
        seed,
        false,
        true,
        &[],
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_series_plot_marked(
    ui: &mut egui::Ui,
    plot_id: &str,
    y_label: &str,
    height: f32,
    lines: &[PlotLine<'_>],
    markers: &[PlotMarker<'_>],
    cursor: &mut SysCursor,
    seed: (f64, f64, bool),
    hold_last: bool,
    pin_y0: bool,
    secondary: &[PlotLine<'_>],
) {
    let height = if cursor.fill_remaining {
        ui.available_height().max(PLOT_H_MIN)
    } else {
        height
    };
    // Whole-capture x extent of the data — the downsample fallback used
    // before egui_plot reports a valid window on the first frame.
    let mut data_lo = f64::INFINITY;
    let mut data_hi = f64::NEG_INFINITY;
    for line in lines {
        if let (Some(first), Some(last)) = (line.data.first(), line.data.last()) {
            data_lo = data_lo.min(first[0]);
            data_hi = data_hi.max(last[0]);
        }
    }
    for m in markers {
        if let (Some(first), Some(last)) = (m.data.first(), m.data.last()) {
            data_lo = data_lo.min(first[0]);
            data_hi = data_hi.max(last[0]);
        }
    }
    if !data_lo.is_finite() {
        data_lo = 0.0;
    }
    if !data_hi.is_finite() || data_hi <= data_lo {
        data_hi = data_lo + 1.0;
    }
    // Y from the drawn (mean-downsampled) points of series that are on in
    // the legend, plus 8% so a peak isn't clipped by the frame. Hidden
    // series are left out so toggling one off still rescales.
    let (x_lo, x_hi, _x_auto) = seed;
    let view = seed_view_x(x_lo, x_hi, data_lo, data_hi);
    let hidden = PlotMemory::load(ui.ctx(), egui::Id::new(plot_id))
        .map(|m| m.hidden_items)
        .unwrap_or_default();
    let mut drawn: Vec<(&str, Color32, Vec<[f64; 2]>)> = Vec::new();
    let mut y_peak = 0.0_f64;
    let mut y_floor = f64::INFINITY;
    for line in lines {
        if line.data.is_empty() {
            continue;
        }
        let mut ds = downsample_mean(line.data, view, MAX_DISPLAY_POINTS);
        if hold_last {
            hold_last_to_edge(&mut ds, x_hi);
        }
        if !hidden.contains(&line.name) {
            for p in &ds {
                y_peak = y_peak.max(p[1]);
                if pin_y0 || p[1] > 0.0 {
                    y_floor = y_floor.min(p[1]);
                }
            }
        }
        drawn.push((line.name.as_str(), line.color, ds));
    }
    for m in markers {
        if m.data.is_empty() || hidden.contains(m.name) {
            continue;
        }
        y_peak = y_peak.max(step_peak_in_view(m.data, view));
        if !pin_y0 {
            for p in m.data {
                if p[0] >= view[0] && p[0] <= view[1] {
                    y_floor = y_floor.min(p[1]);
                }
            }
        }
    }
    let (y_lo, y_hi) = if pin_y0 {
        (0.0, y_axis_top(y_peak))
    } else {
        y_axis_span(y_floor, y_peak)
    };
    let (y_scale, y_unit) = if y_label == "Gb/s" {
        throughput_display(y_peak)
    } else {
        (1.0, y_label)
    };
    let secondary_layout = if pin_y0 {
        layout_secondary(secondary, |name| hidden.contains(name), view, y_hi)
    } else {
        None
    };
    let y_axis = agg_y_axis_scaled(y_unit, y_scale);
    // Every Aggregate panel prints its own x tick labels (matching the CPU
    // strip up top), even the stacked ones, so each reads independently.
    let mut plot = Plot::new(plot_id)
        .id(egui::Id::new(plot_id))
        .height(height)
        .include_x(x_lo)
        .include_x(x_hi)
        .include_y(y_lo)
        .include_y(y_hi)
        .auto_bounds(egui::Vec2b::new(false, false))
        // No interior padding: data fills the frame edge-to-edge in x.
        // Most panels pin 0 to the bottom; duration-ratio plots do not.
        .set_margin_fraction(egui::vec2(0.0, 0.0))
        .custom_y_axes(vec![y_axis])
        .y_axis_width(MAIN_Y_DIGITS)
        .custom_x_axes(vec![agg_x_time_axis(cursor.utc_origin)])
        .y_grid_spacer(nice_y_grid_spacer)
        .show_x(false)
        .show_y(false)
        .allow_scroll(false)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false);
    plot = maybe_legend(plot, cursor.show_legends);
    if cursor.utc_origin.is_some() {
        plot = plot.x_grid_spacer(utc_x_grid_spacer);
    }
    let resp = plot.show(ui, |plot_ui| {
        plot_ui.set_plot_bounds(PlotBounds::from_min_max([x_lo, y_lo], [x_hi, y_hi]));
        plot_ui.set_auto_bounds(egui::Vec2b::new(false, false));
        for (name, color, ds) in drawn {
            plot_ui.line(
                Line::new(PlotPoints::from(ds))
                    .color(color)
                    .width(1.5_f32)
                    .name(name),
            );
        }
        for m in markers {
            if m.data.is_empty() {
                continue;
            }
            plot_ui.points(
                Points::new(PlotPoints::from(m.data.to_vec()))
                    .color(m.color)
                    .shape(m.shape)
                    .radius(4.5_f32)
                    .name(m.name),
            );
        }
        if let Some((_, _, drawn_sec)) = &secondary_layout {
            for s in drawn_sec {
                plot_ui.line(
                    Line::new(PlotPoints::from(s.mapped.clone()))
                        .color(s.color)
                        .width(2.0_f32)
                        .name(s.name),
                );
            }
        }
    });
    if let Some((drop_peak, drop_top, _)) = &secondary_layout {
        paint_drop_axis(
            ui,
            &resp.transform,
            resp.response.rect,
            y_hi,
            *drop_peak,
            *drop_top,
        );
    }

    // Shared-cursor contribution: register the panel and, at the hovered
    // x, sample each *visible* line off its raw data (not the downsampled
    // display copy) for the combined tooltip.
    if let Some(data_x) = cursor.panel(resp.response.rect, &resp.transform, true) {
        let hidden = PlotMemory::load(ui.ctx(), egui::Id::new(plot_id))
            .map(|m| m.hidden_items)
            .unwrap_or_default();
        for line in lines {
            if line.data.is_empty() || hidden.contains(&line.name) {
                continue;
            }
            if !hold_last {
                if let Some(last) = line.data.last() {
                    if data_x > last[0] + SERIES_STALE_AFTER_S {
                        continue;
                    }
                }
            }
            if let Some(y) = interpolate_line(line.data, data_x) {
                cursor.add(
                    &resp.transform,
                    data_x,
                    y,
                    format!("{}: {} {y_unit}", line.name, fmt_cursor_val(y * y_scale)),
                    line.color,
                );
            }
        }
        if let Some((_, drop_top, drawn_sec)) = &secondary_layout {
            for s in drawn_sec {
                if !hold_last {
                    if let Some(last) = s.raw.last() {
                        if data_x > last[0] + SERIES_STALE_AFTER_S {
                            continue;
                        }
                    }
                }
                if let Some(plot_y) = interpolate_line(&s.mapped, data_x) {
                    let y = if y_hi > 0.0 {
                        plot_y * *drop_top / y_hi
                    } else {
                        0.0
                    };
                    cursor.add(
                        &resp.transform,
                        data_x,
                        plot_y,
                        format!("{}: {} /s", s.name, fmt_cursor_val(y)),
                        s.color,
                    );
                }
            }
        }
    }
    cursor.last_plot_rect = Some(resp.response.rect);
}

/// Fixed TCP/UDP/Other colors for the interface protocol stack, indexed
/// like [`analysis::PROTO_STACK_LABELS`] and [`analysis::protocol_stack_bins`]'s
/// `tops` output (`[tcp, udp, other]`).
const PROTO_STACK_COLORS: [Color32; 3] = [COLOR_IF_TCP, COLOR_IF_UDP, COLOR_IF_OTHER];

/// Bottom-to-top indexes into [`analysis::PROTO_STACK_LABELS`]: Other, UDP, TCP.
const PROTO_DRAW_ORDER: [usize; 3] = [2, 1, 0];

/// Opacity of a protocol band. Matches `egui_plot`'s `Line::fill`.
const PROTO_BAND_FILL_ALPHA: f32 = 0.05;

fn proto_band_fill(color: Color32) -> Color32 {
    egui::Rgba::from(color)
        .to_opaque()
        .multiply(PROTO_BAND_FILL_ALPHA)
        .into()
}

/// Stacked-area plot of one interface's TCP/UDP/Other traffic
/// composition, in absolute units (Gb/s or pkts/s) -- the Single
/// Interface view's TX/RX panels. A Gb/s series whose peak is under 1
/// is labeled in Mb/s, Kb/s, or b/s.
///
/// Each protocol is a line at the top of its band and the same
/// translucent wash `Line::fill` uses, covering that band only.
/// Hiding a legend entry drops that protocol out and closes the
/// stack up.
#[allow(clippy::too_many_arguments)]
fn draw_protocol_stack(
    ui: &mut egui::Ui,
    plot_id: &str,
    cum: &[Vec<(u64, u64)>; 3],
    t0_us: u64,
    to_rate_per_sec: f64,
    y_label: &str,
    height: f32,
    cursor: &mut SysCursor,
    seed: (f64, f64, bool),
    drops: &[[f64; 2]],
) {
    let height = if cursor.fill_remaining {
        ui.available_height().max(PLOT_H_MIN)
    } else {
        height
    };
    let n = cum[0].len();
    if n < 2 {
        return;
    }
    let data_lo = cum[0][0].0.saturating_sub(t0_us) as f64 / 1_000_000.0;
    let mut data_hi = cum[0][n - 1].0.saturating_sub(t0_us) as f64 / 1_000_000.0;
    if data_hi <= data_lo {
        data_hi = data_lo + 1.0;
    }

    let (x_lo, x_hi, _x_auto) = seed;
    let [vx0, vx1] = seed_view_x(x_lo, x_hi, data_lo, data_hi);
    let nbins = cursor.plot_bins;
    let (centers, tops) = if vx1 > vx0 {
        analysis::protocol_stack_bins(cum, t0_us, vx0, vx1, nbins, to_rate_per_sec)
    } else {
        (Vec::new(), [Vec::new(), Vec::new(), Vec::new()])
    };
    let hidden = PlotMemory::load(ui.ctx(), egui::Id::new(plot_id))
        .map(|mem| mem.hidden_items.clone())
        .unwrap_or_default();
    let nbin = centers.len();
    let marginal: [Vec<f64>; 3] = std::array::from_fn(|k| {
        (0..nbin)
            .map(|i| {
                let below = if k == 0 { 0.0 } else { tops[k - 1][i] };
                tops[k][i] - below
            })
            .collect()
    });
    let mut floor = vec![0.0; nbin];
    let mut band_bottom: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
    let mut band_top: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
    for &k in &PROTO_DRAW_ORDER {
        if hidden.contains(analysis::PROTO_STACK_LABELS[k]) {
            continue;
        }
        let top: Vec<f64> = floor.iter().zip(&marginal[k]).map(|(b, m)| b + m).collect();
        band_bottom[k].clone_from(&floor);
        band_top[k].clone_from(&top);
        floor = top;
    }
    let y_peak = floor.iter().copied().fold(0.0_f64, f64::max);
    let y_top = y_axis_top(y_peak);
    let (y_scale, y_unit) = if y_label == "Gb/s" {
        throughput_display(y_peak)
    } else {
        (1.0, y_label)
    };
    let drop_line = [PlotLine {
        name: "Drops".to_string(),
        color: COLOR_SSTHRESH,
        data: drops,
    }];
    let view = [vx0, vx1];
    let secondary_layout = layout_secondary(&drop_line, |name| hidden.contains(name), view, y_top);
    let y_axis = agg_y_axis_scaled(y_unit, y_scale);
    let band_points: [Vec<[f64; 2]>; 3] = std::array::from_fn(|k| {
        let ys = if band_top[k].is_empty() {
            &tops[k]
        } else {
            &band_top[k]
        };
        centers.iter().zip(ys).map(|(&x, &y)| [x, y]).collect()
    });
    let mut plot = Plot::new(plot_id)
        .id(egui::Id::new(plot_id))
        .height(height)
        .include_x(x_lo)
        .include_x(x_hi)
        .include_y(0.0)
        .include_y(y_top)
        .auto_bounds(egui::Vec2b::new(false, false))
        .set_margin_fraction(egui::vec2(0.0, 0.0))
        .custom_y_axes(vec![y_axis])
        .custom_x_axes(vec![agg_x_time_axis(cursor.utc_origin)])
        .y_grid_spacer(nice_y_grid_spacer)
        .show_x(false)
        .show_y(false)
        .allow_scroll(false)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false);
    plot = maybe_legend(plot, cursor.show_legends);
    if cursor.utc_origin.is_some() {
        plot = plot.x_grid_spacer(utc_x_grid_spacer);
    }

    let resp = plot.show(ui, |plot_ui| {
        plot_ui.set_plot_bounds(PlotBounds::from_min_max([x_lo, 0.0], [x_hi, y_top]));
        plot_ui.set_auto_bounds(egui::Vec2b::new(false, false));
        for k in 0..3 {
            if band_points[k].is_empty() {
                continue;
            }
            plot_ui.line(
                Line::new(PlotPoints::from(band_points[k].clone()))
                    .color(PROTO_STACK_COLORS[k])
                    .name(analysis::PROTO_STACK_LABELS[k]),
            );
        }
        if let Some((_, _, drawn_sec)) = &secondary_layout {
            for s in drawn_sec {
                plot_ui.line(
                    Line::new(PlotPoints::from(s.mapped.clone()))
                        .color(s.color)
                        .width(2.0_f32)
                        .name(s.name),
                );
            }
        }
    });
    if let Some((drop_peak, drop_top, _)) = &secondary_layout {
        paint_drop_axis(
            ui,
            &resp.transform,
            resp.response.rect,
            y_top,
            *drop_peak,
            *drop_top,
        );
    }

    if centers.len() >= 2 {
        let clip = resp.transform.frame().intersect(ui.clip_rect());
        let painter = ui.painter().with_clip_rect(clip);
        let pt = |x: f64, y: f64| resp.transform.position_from_point(&PlotPoint { x, y });
        for &k in &PROTO_DRAW_ORDER {
            if band_top[k].len() < 2 {
                continue;
            }
            let color = proto_band_fill(PROTO_STACK_COLORS[k]);
            let mut mesh = egui::Mesh::default();
            for i in 0..centers.len() - 1 {
                let idx = mesh.vertices.len() as u32;
                mesh.colored_vertex(pt(centers[i], band_bottom[k][i]), color);
                mesh.colored_vertex(pt(centers[i], band_top[k][i]), color);
                mesh.colored_vertex(pt(centers[i + 1], band_top[k][i + 1]), color);
                mesh.colored_vertex(pt(centers[i + 1], band_bottom[k][i + 1]), color);
                mesh.add_triangle(idx, idx + 1, idx + 2);
                mesh.add_triangle(idx, idx + 2, idx + 3);
            }
            if !mesh.is_empty() {
                painter.add(egui::Shape::mesh(mesh));
            }
        }
    }

    if let Some(data_x) = cursor.panel(resp.response.rect, &resp.transform, true) {
        let shown: [Option<f64>; 3] = std::array::from_fn(|k| {
            if band_top[k].is_empty() {
                None
            } else {
                interpolate_line(&band_points[k], data_x)
            }
        });
        if let Some(total) = PROTO_DRAW_ORDER.iter().rev().find_map(|&k| shown[k]) {
            cursor.add(
                &resp.transform,
                data_x,
                total,
                format!("All: {} {y_unit}", fmt_cursor_val(total * y_scale)),
                Color32::from_gray(200),
            );
        }
        for &k in PROTO_DRAW_ORDER.iter().rev() {
            let Some(top) = shown[k] else { continue };
            let below = PROTO_DRAW_ORDER
                .iter()
                .copied()
                .take_while(|&j| j != k)
                .filter_map(|j| shown[j])
                .last()
                .unwrap_or(0.0);
            let val = top - below;
            if val <= 0.0 {
                continue;
            }
            cursor.add(
                &resp.transform,
                data_x,
                top,
                format!(
                    "{}: {} {y_unit}",
                    analysis::PROTO_STACK_LABELS[k],
                    fmt_cursor_val(val * y_scale)
                ),
                PROTO_STACK_COLORS[k],
            );
        }
        if let Some((_, drop_top, drawn_sec)) = &secondary_layout {
            for s in drawn_sec {
                if let Some(plot_y) = interpolate_line(&s.mapped, data_x) {
                    let y = if y_top > 0.0 {
                        plot_y * *drop_top / y_top
                    } else {
                        0.0
                    };
                    cursor.add(
                        &resp.transform,
                        data_x,
                        plot_y,
                        format!("Drops: {} /s", fmt_cursor_val(y)),
                        s.color,
                    );
                }
            }
        }
    }
    cursor.last_plot_rect = Some(resp.response.rect);
}

fn ella_stack_open_hover(
    label: &str,
    stats: &std::collections::HashMap<String, analysis::EllaStackLegendStats>,
) -> String {
    let s = stats.get(label).copied().unwrap_or_default();
    analysis::ella_stack_legend_hover(label, s)
}

fn draw_named_stack_key(
    ui: &mut egui::Ui,
    series: &[analysis::EllaNamedSeries],
    title_colors: &std::collections::HashMap<String, Color32>,
    stats: &std::collections::HashMap<String, analysis::EllaStackLegendStats>,
) -> Option<String> {
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        for s in series {
            let text = RichText::new(format!("■ {}", s.label))
                .size(12.0)
                .color(title_stack_color(title_colors, &s.label));
            if is_stack_other(&s.label) {
                ui.label(text);
            } else {
                let resp = ui
                    .add(egui::Label::new(text).sense(egui::Sense::click()))
                    .on_hover_text(ella_stack_open_hover(&s.label, stats));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    clicked = Some(s.label.clone());
                }
            }
            ui.add_space(8.0);
        }
    });
    clicked
}

/// Stacked occupancy: each band is `Line::fill(0)` in decreasing
/// cumulative order so shorter fills overpaint.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_named_stack(
    ui: &mut egui::Ui,
    plot_id: &str,
    y_label: &str,
    height: f32,
    series: &[analysis::EllaNamedSeries],
    cursor: &mut SysCursor,
    seed: (f64, f64, bool),
    title_colors: &std::collections::HashMap<String, Color32>,
    stats: &std::collections::HashMap<String, analysis::EllaStackLegendStats>,
) -> Option<String> {
    if series.is_empty() {
        return None;
    }
    let mut has_pts = false;
    for s in series {
        if !s.data.is_empty() {
            has_pts = true;
            break;
        }
    }
    if !has_pts {
        return None;
    }

    let height = if cursor.fill_remaining {
        ui.available_height().max(PLOT_H_MIN)
    } else {
        height
    };

    let clicked = draw_named_stack_key(ui, series, title_colors, stats);

    let y_axis = agg_y_axis(y_label);
    let (x_lo, x_hi, _x_auto) = seed;
    let n = series.len();
    let (centers, tops) = analysis::occupancy_stack_bins(series, x_lo, x_hi, cursor.plot_bins);
    let y_peak = tops
        .last()
        .map(|row| row.iter().copied().fold(0.0_f64, f64::max))
        .unwrap_or(0.0);
    let y_top = y_axis_top(y_peak);
    let band_points: Vec<Vec<[f64; 2]>> = (0..n)
        .map(|k| {
            centers
                .iter()
                .zip(&tops[k])
                .map(|(&x, &y)| [x, y])
                .collect()
        })
        .collect();
    let mut plot = Plot::new(plot_id)
        .id(egui::Id::new(plot_id))
        .height(height)
        .include_x(x_lo)
        .include_x(x_hi)
        .include_y(0.0)
        .include_y(y_top)
        .auto_bounds(egui::Vec2b::new(false, false))
        .set_margin_fraction(egui::vec2(0.0, 0.0))
        .custom_y_axes(vec![y_axis])
        .custom_x_axes(vec![agg_x_time_axis(cursor.utc_origin)])
        .y_grid_spacer(nice_y_grid_spacer)
        .show_x(false)
        .show_y(false)
        .allow_scroll(false)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false);
    if cursor.utc_origin.is_some() {
        plot = plot.x_grid_spacer(utc_x_grid_spacer);
    }

    let resp = plot.show(ui, |plot_ui| {
        plot_ui.set_plot_bounds(PlotBounds::from_min_max([x_lo, 0.0], [x_hi, y_top]));
        plot_ui.set_auto_bounds(egui::Vec2b::new(false, false));
        for k in (0..n).rev() {
            if band_points[k].is_empty() {
                continue;
            }
            plot_ui.line(
                Line::new(PlotPoints::from(band_points[k].clone()))
                    .color(title_stack_color(title_colors, &series[k].label))
                    .fill(0.0_f32)
                    .name(&series[k].label),
            );
        }
    });

    if let Some(data_x) = cursor.panel(resp.response.rect, &resp.transform, true) {
        let tops: Vec<Option<f64>> = band_points
            .iter()
            .map(|pts| interpolate_line(pts, data_x))
            .collect();
        if let Some(total) = tops.last().copied().flatten() {
            cursor.add(
                &resp.transform,
                data_x,
                total,
                format!("All: {} {y_label}", fmt_cursor_val(total)),
                Color32::from_gray(200),
            );
        }
        for k in 0..n {
            let Some(top) = tops[k] else { continue };
            let below = if k == 0 {
                0.0
            } else {
                tops[k - 1].unwrap_or(0.0)
            };
            let val = top - below;
            if val <= 0.0 {
                continue;
            }
            cursor.add(
                &resp.transform,
                data_x,
                top,
                format!("{}: {} {y_label}", series[k].label, fmt_cursor_val(val)),
                title_stack_color(title_colors, &series[k].label),
            );
        }
    }
    cursor.last_plot_rect = Some(resp.response.rect);
    clicked
}

// ---------------------------------------------------------------------------
// Interfaces section (Aggregate tab)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IfMetric {
    Throughput,
    Packets,
}

/// Which of the two Interfaces-panel layouts is shown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InterfacesViewMode {
    /// Multi-interface line comparison, all-protocol totals only --
    /// "which NICs are busy".
    Overview,
    /// One interface's TCP/UDP/Other composition as a stacked-area
    /// chart -- "how does this interface's traffic break down".
    SingleInterface,
}

pub struct InterfacesViewState {
    pub(crate) series: Vec<analysis::IfRateSeries>,
    /// Per-plot metric toggles: the Network TX and Network RX panels each
    /// carry their own Throughput/Packets selector.
    pub(crate) tx_metric: IfMetric,
    pub(crate) rx_metric: IfMetric,
    /// All Interfaces / Single Interface, chosen separately per plot.
    pub(crate) tx_view: InterfacesViewMode,
    pub(crate) rx_view: InterfacesViewMode,
    /// Single Interface ifindex for that plot. `None` picks the busiest
    /// classification-capable candidate.
    pub(crate) tx_ifindex: Option<u16>,
    pub(crate) rx_ifindex: Option<u16>,
    /// TX queue drops on the Network TX plot.
    pub(crate) show_tx_errors: bool,
    /// Frames left to force-repaint after (re)building so egui_plot's
    /// auto-fit settles without waiting for a mouse/keyboard event.
    pub(crate) settle_frames: u8,
}

impl InterfacesViewState {
    pub(crate) fn new(prof: &IfProfile, t0_us: u64) -> Self {
        Self {
            series: analysis::build_if_rate_series(prof, t0_us),
            tx_metric: IfMetric::Throughput,
            rx_metric: IfMetric::Throughput,
            tx_view: InterfacesViewMode::SingleInterface,
            rx_view: InterfacesViewMode::SingleInterface,
            tx_ifindex: None,
            rx_ifindex: None,
            show_tx_errors: true,
            settle_frames: SETTLE_FRAMES,
        }
    }
}

/// Y-axis unit label for an interface metric.
fn if_metric_label(m: IfMetric) -> &'static str {
    match m {
        IfMetric::Throughput => "Gb/s",
        IfMetric::Packets => "pkts/s",
    }
}

/// Title, All/Single, metric, and (in Single mode) the interface dropdown,
/// on one row.
#[allow(clippy::too_many_arguments)]
fn iface_rate_header(
    ui: &mut egui::Ui,
    title: &str,
    show_view: bool,
    view_mode: &mut InterfacesViewMode,
    metric: &mut IfMetric,
    combo_id: &str,
    series: &[analysis::IfRateSeries],
    candidates: &[usize],
    selected: &mut Option<u16>,
    show_errors: Option<&mut bool>,
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.strong(RichText::new(title).size(14.0));
        ui.add_space(12.0);
        if show_view {
            ui.selectable_value(
                view_mode,
                InterfacesViewMode::Overview,
                RichText::new("All Interfaces").size(13.0),
            );
            ui.selectable_value(
                view_mode,
                InterfacesViewMode::SingleInterface,
                RichText::new("Single Interface").size(13.0),
            );
            ui.add_space(12.0);
        }
        ui.selectable_value(
            metric,
            IfMetric::Throughput,
            RichText::new("Throughput").size(13.0),
        );
        ui.selectable_value(
            metric,
            IfMetric::Packets,
            RichText::new("Packets/s").size(13.0),
        );
        if show_view && *view_mode == InterfacesViewMode::SingleInterface {
            if !candidates
                .iter()
                .any(|&p| Some(series[p].ifindex) == *selected)
            {
                *selected = Some(series[candidates[0]].ifindex);
            }
            single_iface_picker(ui, combo_id, series, candidates, selected);
        }
        if let Some(show) = show_errors {
            ui.add_space(12.0);
            let enabled = tx_errors_present(series, show_view, *view_mode, *selected);
            let mut on = *show && enabled;
            if ui
                .add_enabled(
                    enabled,
                    egui::Checkbox::new(&mut on, RichText::new("Show Network Errors").size(13.0)),
                )
                .changed()
                && enabled
            {
                *show = on;
            }
        }
    });
}

/// True when the interfaces on this plot recorded input/output errors
/// or queue drops. Single Interface checks the selected NIC.
fn tx_errors_present(
    series: &[analysis::IfRateSeries],
    show_view: bool,
    view_mode: InterfacesViewMode,
    selected: Option<u16>,
) -> bool {
    let has = |s: &analysis::IfRateSeries| s.total_errs > 0 || s.total_drops > 0;
    if show_view && view_mode == InterfacesViewMode::SingleInterface {
        series.iter().any(|s| Some(s.ifindex) == selected && has(s))
    } else {
        series.iter().any(|s| !s.is_idle() && has(s))
    }
}

/// Interface dropdown for the Single Interface metric row.
fn single_iface_picker(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    series: &[analysis::IfRateSeries],
    candidates: &[usize],
    selected: &mut Option<u16>,
) {
    ui.add_space(16.0);
    ui.label(RichText::new("Interface").size(13.0));
    ui.add_space(8.0);
    let current_name = candidates
        .iter()
        .find(|&&p| Some(series[p].ifindex) == *selected)
        .map(|&p| series[p].name.clone())
        .unwrap_or_default();
    egui::ComboBox::new(id, "")
        .selected_text(current_name)
        .show_ui(ui, |ui| {
            for &p in candidates {
                let ix = series[p].ifindex;
                let name = series[p].name.clone();
                ui.selectable_value(selected, Some(ix), name);
            }
        });
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum IfCatalogPanel {
    Tx,
    Rx,
}

impl EventExplorerApp {
    fn interfaces_seed_and_state(
        &mut self,
        ui: &mut egui::Ui,
        plot_ids: &[String],
        seed: Option<(f64, f64, bool)>,
    ) -> ((f64, f64, bool), u64) {
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = seed.unwrap_or_else(|| {
            let seed = self.agg_time_axis.seed(0.0, x_max);
            for id in plot_ids {
                self.agg_time_axis.apply(ui.ctx(), id);
            }
            seed
        });
        let t0_us = self
            .file_info
            .as_ref()
            .map(|f| f.capture_start)
            .unwrap_or(0);
        if self.interfaces_view.is_none() {
            self.interfaces_view = Some(InterfacesViewState::new(&self.if_profile, t0_us));
        }
        let state = self.interfaces_view.as_mut().unwrap();
        if state.settle_frames > 0 {
            state.settle_frames -= 1;
            ui.ctx().request_repaint();
        }
        (seed, t0_us)
    }

    fn interfaces_active_indices(state: &InterfacesViewState) -> Vec<usize> {
        state
            .series
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_idle())
            .map(|(i, _)| i)
            .collect()
    }

    /// System-tab NIC TX or RX. Each keeps its own
    /// All Interfaces / Single Interface choice.
    pub(crate) fn show_nic_plots(
        &mut self,
        ui: &mut egui::Ui,
        show_tx: bool,
        show_rx: bool,
        tx_h: f32,
        rx_h: f32,
        id_prefix: &str,
    ) {
        if self.if_profile.is_empty() || (!show_tx && !show_rx) {
            return;
        }
        let capture_start = self
            .file_info
            .as_ref()
            .map(|info| info.capture_start)
            .unwrap_or(0);
        let origin = utc_axis_origin(self.utc_time_labels, self.file_info.as_ref(), capture_start);
        let mut cursor = SysCursor::new(ui, origin, self.plot_bins);
        if show_tx {
            cursor.panel_h = tx_h;
            self.interfaces_section_ex(ui, &mut cursor, IfCatalogPanel::Tx, None, id_prefix);
        }
        if show_rx {
            cursor.panel_h = rx_h;
            self.interfaces_section_ex(ui, &mut cursor, IfCatalogPanel::Rx, None, id_prefix);
        }
    }

    pub(crate) fn interfaces_section(
        &mut self,
        ui: &mut egui::Ui,
        cursor: &mut SysCursor,
        panel: IfCatalogPanel,
    ) {
        self.interfaces_section_ex(ui, cursor, panel, None, "");
    }

    pub(crate) fn interfaces_section_ex(
        &mut self,
        ui: &mut egui::Ui,
        cursor: &mut SysCursor,
        panel: IfCatalogPanel,
        seed: Option<(f64, f64, bool)>,
        id_prefix: &str,
    ) {
        let pid = |base: &str| format!("{id_prefix}{base}");
        let (seed, t0_us) = self.interfaces_seed_and_state(
            ui,
            &[
                pid("if_tx_plot"),
                pid("if_rx_plot"),
                pid("if_single_tx_plot"),
                pid("if_single_rx_plot"),
            ],
            seed,
        );
        let state = self.interfaces_view.as_mut().unwrap();
        let visible = Self::interfaces_active_indices(state);
        if visible.is_empty() {
            return;
        }

        let panel_h = cursor.panel_h;

        let mut candidates: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|&i| state.series[i].has_protocol_classification)
            .collect();
        candidates.sort_by(|&a, &b| {
            state.series[b]
                .peak_tx_gbps
                .total_cmp(&state.series[a].peak_tx_gbps)
        });

        let show_view = !candidates.is_empty();
        match panel {
            IfCatalogPanel::Tx => {
                iface_rate_header(
                    ui,
                    "Network TX",
                    show_view,
                    &mut state.tx_view,
                    &mut state.tx_metric,
                    &pid("if_single_tx_combo"),
                    &state.series,
                    &candidates,
                    &mut state.tx_ifindex,
                    Some(&mut state.show_tx_errors),
                );
                let mode = if show_view {
                    state.tx_view
                } else {
                    InterfacesViewMode::Overview
                };
                if mode == InterfacesViewMode::SingleInterface {
                    let sel = candidates
                        .iter()
                        .copied()
                        .find(|&p| Some(state.series[p].ifindex) == state.tx_ifindex)
                        .unwrap_or(candidates[0]);
                    let iface = &state.series[sel];
                    let proto_cum = iface.proto_cum.as_ref().unwrap();
                    let (cum, to_rate_per_sec, y_label) = match state.tx_metric {
                        IfMetric::Throughput => {
                            (&proto_cum.tx_bytes, analysis::BYTES_PER_S_TO_GBPS, "Gb/s")
                        }
                        IfMetric::Packets => (&proto_cum.tx_packets, 1.0, "pkts/s"),
                    };
                    let drops: &[[f64; 2]] = if state.show_tx_errors {
                        &iface.oqdrop_rate
                    } else {
                        &[]
                    };
                    draw_protocol_stack(
                        ui,
                        &pid("if_single_tx_plot"),
                        cum,
                        t0_us,
                        to_rate_per_sec,
                        y_label,
                        panel_h,
                        cursor,
                        seed,
                        drops,
                    );
                    return;
                }
                let tx_lines: Vec<PlotLine> = visible
                    .iter()
                    .map(|&i| PlotLine {
                        name: state.series[i].name.clone(),
                        color: series_color(i),
                        data: match state.tx_metric {
                            IfMetric::Throughput => {
                                state.series[i].tx_gbps_for(analysis::IfProtocol::All)
                            }
                            IfMetric::Packets => {
                                state.series[i].tx_pps_for(analysis::IfProtocol::All)
                            }
                        },
                    })
                    .collect();
                let drop_lines: Vec<PlotLine> = if state.show_tx_errors {
                    visible
                        .iter()
                        .filter_map(|&i| {
                            let s = &state.series[i];
                            if s.oqdrop_rate.is_empty() {
                                None
                            } else {
                                Some(PlotLine {
                                    name: format!("{} drops", s.name),
                                    color: COLOR_SSTHRESH,
                                    data: &s.oqdrop_rate,
                                })
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                draw_series_plot_marked(
                    ui,
                    &pid("if_tx_plot"),
                    if_metric_label(state.tx_metric),
                    panel_h,
                    &tx_lines,
                    &[],
                    cursor,
                    seed,
                    false,
                    true,
                    &drop_lines,
                );
            }
            IfCatalogPanel::Rx => {
                iface_rate_header(
                    ui,
                    "Network RX",
                    show_view,
                    &mut state.rx_view,
                    &mut state.rx_metric,
                    &pid("if_single_rx_combo"),
                    &state.series,
                    &candidates,
                    &mut state.rx_ifindex,
                    None,
                );
                let mode = if show_view {
                    state.rx_view
                } else {
                    InterfacesViewMode::Overview
                };
                if mode == InterfacesViewMode::SingleInterface {
                    let sel = candidates
                        .iter()
                        .copied()
                        .find(|&p| Some(state.series[p].ifindex) == state.rx_ifindex)
                        .unwrap_or(candidates[0]);
                    let proto_cum = state.series[sel].proto_cum.as_ref().unwrap();
                    let (cum, to_rate_per_sec, y_label) = match state.rx_metric {
                        IfMetric::Throughput => {
                            (&proto_cum.rx_bytes, analysis::BYTES_PER_S_TO_GBPS, "Gb/s")
                        }
                        IfMetric::Packets => (&proto_cum.rx_packets, 1.0, "pkts/s"),
                    };
                    draw_protocol_stack(
                        ui,
                        &pid("if_single_rx_plot"),
                        cum,
                        t0_us,
                        to_rate_per_sec,
                        y_label,
                        panel_h,
                        cursor,
                        seed,
                        &[],
                    );
                    return;
                }
                let rx_lines: Vec<PlotLine> = visible
                    .iter()
                    .map(|&i| PlotLine {
                        name: state.series[i].name.clone(),
                        color: series_color(i),
                        data: match state.rx_metric {
                            IfMetric::Throughput => {
                                state.series[i].rx_gbps_for(analysis::IfProtocol::All)
                            }
                            IfMetric::Packets => {
                                state.series[i].rx_pps_for(analysis::IfProtocol::All)
                            }
                        },
                    })
                    .collect();
                draw_series_plot(
                    ui,
                    &pid("if_rx_plot"),
                    if_metric_label(state.rx_metric),
                    panel_h,
                    &rx_lines,
                    cursor,
                    seed,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TCP stack section (Aggregate tab)
// ---------------------------------------------------------------------------

/// Which goodput visualization the TCP stack section shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GpView {
    /// Stacked-area composition (% of each log2 bucket) over time.
    OverTime,
    /// Whole-capture bucket histogram (bar chart).
    Histogram,
}

pub struct TcpSamplesViewState {
    pub(crate) rps: Vec<analysis::TcpRpsSeries>,
    pub(crate) goodput: analysis::GoodputHist,
    pub(crate) gp_stack: analysis::GoodputStack,
    pub(crate) gp_view: GpView,
    pub(crate) srtt: analysis::GoodputHist,
    pub(crate) srtt_stack: analysis::GoodputStack,
    pub(crate) srtt_view: GpView,
    pub(crate) send_stack: analysis::GoodputStack,
    pub(crate) lro_flush_stack: analysis::GoodputStack,
    /// See [`InterfacesViewState::settle_frames`].
    pub(crate) settle_frames: u8,
}

impl TcpSamplesViewState {
    pub(crate) fn new(prof: &TcpSampleProfile, t0_us: u64) -> Self {
        Self {
            rps: analysis::build_tcp_rps_series(prof, t0_us),
            goodput: analysis::build_goodput_hist(prof),
            gp_stack: analysis::build_goodput_stack(prof, t0_us),
            gp_view: GpView::OverTime,
            srtt: analysis::build_srtt_hist(prof),
            srtt_stack: analysis::build_hist_stack(prof, &prof.srtt_samples, t0_us, 14),
            srtt_view: GpView::OverTime,
            send_stack: analysis::build_hist_stack(prof, &prof.send_samples, t0_us, 64),
            lro_flush_stack: analysis::build_hist_stack(prof, &prof.lro_flush_samples, t0_us, 64),
            settle_frames: SETTLE_FRAMES,
        }
    }
}

impl EventExplorerApp {
    /// Lazily build the per-stack TCP sample view and tick its settle
    /// counter (a short burst of repaints so egui_plot's linked axes
    /// settle without waiting for input). Returns the view handle.
    fn ensure_tcp_samples_view(&mut self, ui: &mut egui::Ui) -> &mut TcpSamplesViewState {
        if self.tcp_samples_view.is_none() {
            let t0 = self
                .file_info
                .as_ref()
                .map(|f| f.capture_start)
                .unwrap_or(0);
            self.tcp_samples_view = Some(TcpSamplesViewState::new(&self.tcp_samples, t0));
        }
        let state = self.tcp_samples_view.as_mut().unwrap();
        if state.settle_frames > 0 {
            state.settle_frames -= 1;
            ui.ctx().request_repaint();
        }
        state
    }

    /// HTTP responses/sec on the Aggregate tab, one line per status
    /// class. TCP is TCP_RPS_SAMPLE and starts hidden when response
    /// lines are drawn.
    pub(crate) fn http_rps_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        // Seed + force onto the shared time axis before the (whole-self)
        // sample-view borrow taken by `ensure_tcp_samples_view`.
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.agg_time_axis.apply(ui.ctx(), "http_rps_plot");
        self.ensure_tcp_samples_view(ui);
        if !self.nginx_profile.is_empty() {
            self.ensure_nginx_samples_view(ui);
        }

        let has_tcp = self
            .tcp_samples_view
            .as_ref()
            .is_some_and(|s| s.rps.iter().any(|r| !r.samples.is_empty()));
        let has_nginx = self
            .nginx_samples_view
            .as_ref()
            .is_some_and(|s| !s.agg.completions.is_empty() || !s.agg.samples.is_empty());

        ui.add_space(6.0);
        ui.strong(RichText::new("HTTP Responses Per Second").size(14.0));
        if !has_tcp && !has_nginx {
            ui.label(
                RichText::new("No HTTP responses in this capture.")
                    .size(12.0)
                    .color(Color32::from_gray(180)),
            );
        }

        let tcp_series = self
            .tcp_samples_view
            .as_ref()
            .map(|s| s.rps.as_slice())
            .unwrap_or(&[]);
        let nginx_completions = self
            .nginx_samples_view
            .as_ref()
            .map(|s| s.agg.completions.as_slice())
            .unwrap_or(&[]);
        let nginx_samples = self
            .nginx_samples_view
            .as_ref()
            .map(|s| s.agg.samples.as_slice())
            .unwrap_or(&[]);

        let mut data_lo = f64::INFINITY;
        let mut data_hi = f64::NEG_INFINITY;
        if has_tcp {
            for s in tcp_series {
                if let (Some(first), Some(last)) = (s.samples.first(), s.samples.last()) {
                    data_lo = data_lo.min(first.0);
                    data_hi = data_hi.max(last.0);
                }
            }
        }
        if let (Some(first), Some(last)) = (nginx_completions.first(), nginx_completions.last()) {
            data_lo = data_lo.min(first.ts_s);
            data_hi = data_hi.max(last.ts_s);
        }
        if let (Some(first), Some(last)) = (nginx_samples.first(), nginx_samples.last()) {
            data_lo = data_lo.min(first.ts_s);
            data_hi = data_hi.max(last.ts_s);
        }
        if !data_lo.is_finite() {
            data_lo = 0.0;
        }
        if !data_hi.is_finite() || data_hi <= data_lo {
            data_hi = data_lo + 1.0;
        }
        let [vx0, vx1] = seed_view_x(seed.0, seed.1, data_lo, data_hi);
        let tcp_binned = if has_tcp {
            analysis::tcp_rps_bins(tcp_series, vx0, vx1, cursor.plot_bins)
        } else {
            Vec::new()
        };
        let nginx_binned = self
            .nginx_samples_view
            .as_ref()
            .map(|s| analysis::nginx_status_bins(&s.agg, vx0, vx1, cursor.plot_bins))
            .unwrap_or_default();

        let mut lines: Vec<PlotLine> = Vec::new();
        let tcp_on = tcp_binned.iter().any(|p| p[1] > 0.0);
        let mut i = 0usize;
        let mut has_response_lines = false;
        for s in &nginx_binned {
            if s.data.iter().all(|p| p[1] == 0.0) {
                continue;
            }
            has_response_lines = true;
            // SERIES_PALETTE[1] is COLOR_TCP_RPS; skip it when the TCP
            // line is on this plot so a status line doesn't share its orange.
            let color = if tcp_on {
                series_color(if i >= 1 { i + 1 } else { i })
            } else {
                series_color(i)
            };
            lines.push(PlotLine {
                name: s.label.clone(),
                color,
                data: &s.data,
            });
            i += 1;
        }
        if tcp_on {
            lines.push(PlotLine {
                name: "TCP".to_string(),
                color: COLOR_TCP_RPS,
                data: &tcp_binned,
            });
        }
        draw_series_plot(
            ui,
            "http_rps_plot",
            "resp/s",
            cursor.panel_h,
            &lines,
            cursor,
            seed,
        );
        if has_response_lines {
            if let Some(state) = self.nginx_samples_view.as_mut() {
                if state.rps_legend_init && hide_http_rps_legend(ui.ctx()) {
                    state.rps_legend_init = false;
                    ui.ctx().request_repaint();
                }
            }
        }
    }

    /// Per-stack TCP goodput-distribution panel of the Aggregate tab
    /// ("TCP Goodput"): the stacked per-bucket composition over time, or
    /// the whole-capture histogram. The contributing stack(s) name is a
    /// subtitle. No-op when the capture has no goodput samples.
    pub(crate) fn tcp_goodput_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        // Seed + force the stacked-area panel onto the shared time axis
        // before the (whole-self) sample-view borrow. The histogram view is
        // categorical (buckets, not time) and stays off the shared axis.
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.agg_time_axis.apply(ui.ctx(), "tcp_gp_stack");
        let state = self.ensure_tcp_samples_view(ui);
        if state.goodput.is_empty() {
            return;
        }
        let gp_time_ok = !state.gp_stack.is_empty();
        let show_gp_time = gp_time_ok && state.gp_view == GpView::OverTime;

        // Goodput is a per-stack event; name the contributing stack(s) so
        // it isn't confused with the aggregate PCB-state panel.
        let gp_sub = state.goodput.labels.join(", ");

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.strong(RichText::new("TCP Goodput").size(14.0));
            if !gp_sub.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(gp_sub.clone())
                        .size(12.0)
                        .color(Color32::from_gray(180)),
                );
            }
            if gp_time_ok {
                ui.separator();
                ui.selectable_value(
                    &mut state.gp_view,
                    GpView::OverTime,
                    RichText::new("Distribution Over Time").size(14.0),
                );
                ui.selectable_value(
                    &mut state.gp_view,
                    GpView::Histogram,
                    RichText::new("Whole-Capture Histogram").size(14.0),
                );
            }
        });

        if show_gp_time {
            draw_bucket_key(
                ui,
                &state.gp_stack,
                false,
                true,
                goodput_display_label,
                goodput_speed_color,
                seed.0,
                seed.1,
                cursor.plot_bins,
            );
            draw_bucket_stack(
                ui,
                "tcp_gp_stack",
                &state.gp_stack,
                cursor.panel_h,
                "% of samples",
                false,
                cursor,
                seed,
                goodput_display_label,
                goodput_speed_color,
            );
        } else {
            ui.add_space(6.0);
            ui.label(
                RichText::new("Per-connection goodput distribution (whole capture)")
                    .size(12.0)
                    .color(Color32::from_gray(170)),
            );
            draw_goodput_hist(ui, &state.goodput, cursor);
        }
    }

    /// Per-stack smoothed RTT histogram on the Aggregate tab
    /// ("TCP Smoothed RTT"). No-op when the capture has no SRTT samples.
    pub(crate) fn tcp_srtt_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.agg_time_axis.apply(ui.ctx(), "tcp_srtt_stack");
        let state = self.ensure_tcp_samples_view(ui);
        if state.srtt.is_empty() {
            return;
        }
        let srtt_time_ok = !state.srtt_stack.is_empty();
        let show_time = srtt_time_ok && state.srtt_view == GpView::OverTime;
        let srtt_sub = state.srtt.labels.join(", ");

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.strong(RichText::new("TCP Smoothed RTT").size(14.0));
            if !srtt_sub.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(srtt_sub)
                        .size(12.0)
                        .color(Color32::from_gray(180)),
                );
            }
            if srtt_time_ok {
                ui.separator();
                ui.selectable_value(
                    &mut state.srtt_view,
                    GpView::OverTime,
                    RichText::new("Distribution Over Time").size(14.0),
                );
                ui.selectable_value(
                    &mut state.srtt_view,
                    GpView::Histogram,
                    RichText::new("Whole-Capture Histogram").size(14.0),
                );
            }
        });

        if show_time {
            draw_bucket_key(
                ui,
                &state.srtt_stack,
                false,
                true,
                analysis::srtt_bucket_label,
                srtt_speed_color,
                seed.0,
                seed.1,
                cursor.plot_bins,
            );
            draw_bucket_stack(
                ui,
                "tcp_srtt_stack",
                &state.srtt_stack,
                cursor.panel_h,
                "% of connections",
                false,
                cursor,
                seed,
                analysis::srtt_bucket_label,
                srtt_speed_color,
            );
        } else {
            ui.add_space(6.0);
            ui.label(
                RichText::new("Smoothed RTT distribution (whole capture)")
                    .size(12.0)
                    .color(Color32::from_gray(170)),
            );
            draw_srtt_hist(ui, &state.srtt, cursor);
        }
    }

    /// Per-stack TCP send-size histogram on the Aggregate tab
    /// ("TCP Send Size"): byte-weighted segments-per-burst composition
    /// over time. No-op when the capture has fewer than two send samples.
    pub(crate) fn tcp_send_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.agg_time_axis.apply(ui.ctx(), "tcp_send_stack");
        let state = self.ensure_tcp_samples_view(ui);
        if state.send_stack.is_empty() {
            return;
        }
        let send_sub = state.send_stack.labels.join(", ");

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.strong(RichText::new("TCP Send Size").size(14.0));
            if !send_sub.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(send_sub)
                        .size(12.0)
                        .color(Color32::from_gray(180)),
                );
            }
            ui.label(
                RichText::new("Segments per burst (byte-weighted)")
                    .size(12.0)
                    .color(Color32::from_gray(180)),
            );
        });
        draw_bucket_key(
            ui,
            &state.send_stack,
            false,
            false,
            analysis::lin_hist_bucket_label,
            goodput_bucket_color,
            seed.0,
            seed.1,
            cursor.plot_bins,
        );
        draw_bucket_stack(
            ui,
            "tcp_send_stack",
            &state.send_stack,
            cursor.panel_h,
            "% of bytes",
            false,
            cursor,
            seed,
            analysis::lin_hist_bucket_label,
            goodput_bucket_color,
        );
    }

    /// Machine-wide LRO flush-size histogram on the Aggregate tab
    /// ("TCP LRO Flush Size"): packets coalesced per `tcp_lro_flush()`.
    /// No-op when the capture has fewer than two LRO samples.
    pub(crate) fn tcp_lro_flush_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.agg_time_axis.apply(ui.ctx(), "tcp_lro_flush_stack");
        let state = self.ensure_tcp_samples_view(ui);
        if state.lro_flush_stack.is_empty() {
            return;
        }

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.strong(RichText::new("TCP LRO Flush Size").size(14.0));
            ui.label(
                RichText::new("Packets coalesced per flush")
                    .size(12.0)
                    .color(Color32::from_gray(180)),
            );
        });
        draw_bucket_key(
            ui,
            &state.lro_flush_stack,
            false,
            false,
            analysis::lin_hist_bucket_label,
            goodput_bucket_color,
            seed.0,
            seed.1,
            cursor.plot_bins,
        );
        draw_bucket_stack(
            ui,
            "tcp_lro_flush_stack",
            &state.lro_flush_stack,
            cursor.panel_h,
            "% of flushes",
            false,
            cursor,
            seed,
            analysis::lin_hist_bucket_label,
            goodput_bucket_color,
        );
    }
}

/// Goodput bucket 0 is a truncated-to-<1Mbps estimate, not a literal
/// zero rate, so the stacked plot's legend/tooltip spell it out rather
/// than showing the bare "0" [`analysis::goodput_bucket_label`] gives
/// every other bucket-histogram consumer (e.g. the HPTS lateness/batch
/// stacks, where bucket 0 *is* a literal "0us"/"0 conns").
fn goodput_display_label(b: usize, nbuckets: usize) -> String {
    if b == 0 {
        "≈0 (<1M)".to_string()
    } else {
        analysis::goodput_bucket_label(b, nbuckets)
    }
}

/// Stacked-area plot of a [`analysis::GoodputStack`]'s bucket distribution
/// over time (share (%) of the per-window sample count) -- used for the
/// goodput histogram and, reusing the same generic "diffed cumulative
/// histogram" shape, the HPTS lateness/batch histograms. The visible
/// x-range is binned into columns; each column diffs the cumulative
/// histogram at its edges, so zooming in re-resolves the composition. Each
/// band is painted as a single screen-space `Mesh` (two triangles per
/// column) after the plot lays out its axes. Emitting the bands as
/// ~nbins×nbuckets individual `Polygon` plot-items instead cost thousands
/// of plot-items and a tessellation spike every time the panel scrolled
/// into view. The shared cursor contributes tooltip rows for buckets that are at
/// least [`BUCKET_SHARE_MIN_PCT`] of the hovered column, largest share
/// first. `label` formats a bucket index into its display string
/// (e.g. [`analysis::goodput_bucket_label`], [`analysis::hpts_lateness_bucket_label`]).
/// `color` paints bucket `b` of `nbuckets`.
#[allow(clippy::too_many_arguments)]
fn draw_bucket_stack(
    ui: &mut egui::Ui,
    plot_id: &str,
    stack: &analysis::GoodputStack,
    height: f32,
    y_unit: &str,
    hide_zero: bool,
    cursor: &mut SysCursor,
    seed: (f64, f64, bool),
    label: impl Fn(usize, usize) -> String,
    color_of: impl Fn(usize, usize) -> Color32,
) {
    let height = if cursor.fill_remaining {
        ui.available_height().max(PLOT_H_MIN)
    } else {
        height
    };
    let n = stack.times.len();
    if n < 2 {
        return;
    }
    // Whole-capture x extent, used to clamp the per-column diff window.
    let data_lo = stack.times[0];
    let data_hi = stack.times[n - 1];
    let nb = stack.nbuckets;
    let first_bucket = if hide_zero { 1.min(nb) } else { 0 };

    let y_axis = agg_y_axis(y_unit);
    let (x_lo, x_hi, _x_auto) = seed;
    let mut plot = Plot::new(plot_id)
        .id(egui::Id::new(plot_id))
        .height(height)
        .include_x(x_lo)
        .include_x(x_hi)
        .include_y(0.0)
        .include_y(100.0)
        .auto_bounds(egui::Vec2b::new(false, false))
        .set_margin_fraction(egui::vec2(0.0, 0.0))
        .custom_y_axes(vec![y_axis])
        .custom_x_axes(vec![agg_x_time_axis(cursor.utc_origin)])
        .show_x(false)
        .show_y(false)
        .allow_scroll(false)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false);
    plot = maybe_legend(plot, cursor.show_legends);
    if cursor.utc_origin.is_some() {
        plot = plot.x_grid_spacer(utc_x_grid_spacer);
    }
    // The plot only lays out the axes and reports the transform; the bands
    // are painted as one mesh per bucket below.
    let resp = plot.show(ui, |plot_ui| {
        plot_ui.set_plot_bounds(PlotBounds::from_min_max([x_lo, 0.0], [x_hi, 100.0]));
        plot_ui.set_auto_bounds(egui::Vec2b::new(false, false));
    });

    // Register the panel first so the shared cursor line spans it and we learn
    // the hovered capture time (data x) to build the tooltip from below.
    let hover_x = cursor.panel(resp.response.rect, &resp.transform, true);

    let transform = &resp.transform;
    let [vx0, vx1] = seed_view_x(x_lo, x_hi, data_lo, data_hi);
    let ndrawn = nb - first_bucket;
    if vx1 > vx0 && ndrawn > 0 {
        let nbins = cursor.plot_bins;
        // tops[pos][bin] = cumulative % through drawn bucket `pos`.
        let mut tops: Vec<Vec<f64>> = vec![Vec::with_capacity(nbins); ndrawn];
        let mut centers: Vec<f64> = Vec::with_capacity(nbins);
        for_each_column_share(stack, first_bucket, vx0, vx1, nbins, |center, frac| {
            centers.push(center);
            let mut acc = 0.0;
            for pos in 0..ndrawn {
                acc += frac[pos];
                tops[pos].push(acc);
            }
        });
        // One mesh per band (bottom band first), painted in screen space and
        // clipped to the visible plot frame so an off-screen panel costs
        // nothing to paint.
        let clip = transform.frame().intersect(ui.clip_rect());
        let painter = ui.painter().with_clip_rect(clip);
        let pt = |x: f64, y: f64| transform.position_from_point(&PlotPoint { x, y });
        for pos in 0..ndrawn {
            let color = color_of(first_bucket + pos, nb);
            let mut mesh = egui::Mesh::default();
            for i in 0..centers.len().saturating_sub(1) {
                let (x0, x1) = (centers[i], centers[i + 1]);
                let (top0, top1) = (tops[pos][i], tops[pos][i + 1]);
                let (bot0, bot1) = if pos == 0 {
                    (0.0, 0.0)
                } else {
                    (tops[pos - 1][i], tops[pos - 1][i + 1])
                };
                let idx = mesh.vertices.len() as u32;
                mesh.colored_vertex(pt(x0, bot0), color);
                mesh.colored_vertex(pt(x0, top0), color);
                mesh.colored_vertex(pt(x1, top1), color);
                mesh.colored_vertex(pt(x1, bot1), color);
                mesh.add_triangle(idx, idx + 1, idx + 2);
                mesh.add_triangle(idx, idx + 2, idx + 3);
            }
            if !mesh.is_empty() {
                painter.add(egui::Shape::mesh(mesh));
            }
        }

        // Shared-cursor tooltip: at the hovered x, report each bucket's share
        // (%) of that column's composition, with a dot on the band's
        // cumulative top so the rows line up with the stacked boundaries.
        if let Some(dx) = hover_x {
            let bin = (((dx - vx0) / (vx1 - vx0) * nbins as f64).floor() as isize)
                .clamp(0, nbins as isize - 1) as usize;
            let mut rows: Vec<(usize, f64, f64)> = Vec::new();
            for pos in 0..ndrawn {
                let top = tops[pos][bin];
                let below = if pos == 0 { 0.0 } else { tops[pos - 1][bin] };
                let share = top - below;
                if share >= BUCKET_SHARE_MIN_PCT {
                    rows.push((first_bucket + pos, share, top));
                }
            }
            rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            for (bkt, share, top) in rows {
                cursor.add(
                    transform,
                    dx,
                    top,
                    format!("{}: {share:.1}%", label(bkt, nb)),
                    color_of(bkt, nb),
                );
            }
        }
    }
    cursor.last_plot_rect = Some(resp.response.rect);
}

/// Compact color key for a stacked [`draw_bucket_stack`] plot. A bucket
/// is listed when it reaches [`BUCKET_SHARE_MIN_PCT`] of any time column
/// in the visible window. `by_value` lists them from the smallest bucket
/// to the largest; otherwise the largest average share comes first.
#[allow(clippy::too_many_arguments)]
fn draw_bucket_key(
    ui: &mut egui::Ui,
    stack: &analysis::GoodputStack,
    hide_zero: bool,
    by_value: bool,
    label: impl Fn(usize, usize) -> String,
    color_of: impl Fn(usize, usize) -> Color32,
    t_lo: f64,
    t_hi: f64,
    nbins: usize,
) {
    let nb = stack.nbuckets;
    let first = if hide_zero { 1.min(nb) } else { 0 };
    let n = stack.times.len();
    if n < 2 {
        return;
    }
    let [vx0, vx1] = seed_view_x(t_lo, t_hi, stack.times[0], stack.times[n - 1]);
    let shares = order_bucket_shares(stack_bucket_shares(stack, first, vx0, vx1, nbins), by_value);
    ui.horizontal_wrapped(|ui| {
        for (bkt, _) in shares {
            let color = color_of(bkt, nb);
            let label = label(bkt, nb);
            ui.label(RichText::new(format!("■ {label}")).size(12.0).color(color));
            ui.add_space(2.0);
        }
    });
}

/// Omit buckets below this share (%) from the color key and hover list.
const BUCKET_SHARE_MIN_PCT: f64 = 1.0;

fn order_bucket_shares(mut shares: Vec<(usize, f64)>, by_value: bool) -> Vec<(usize, f64)> {
    if by_value {
        shares.sort_by_key(|(bkt, _)| *bkt);
    }
    shares
}

#[cfg(test)]
fn notable_bucket_shares(shares: impl IntoIterator<Item = (usize, f64)>) -> Vec<(usize, f64)> {
    let mut out: Vec<(usize, f64)> = shares
        .into_iter()
        .filter(|(_, s)| *s >= BUCKET_SHARE_MIN_PCT)
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Walk `[t0, t1)` in `nbins` equal time columns. `frac` is each drawn
/// bucket's share of that column, in percent. A column with no new
/// samples keeps the previous mix.
fn for_each_column_share(
    stack: &analysis::GoodputStack,
    first_bucket: usize,
    t0: f64,
    t1: f64,
    nbins: usize,
    mut f: impl FnMut(f64, &[f64]),
) {
    let nbins = nbins.max(1);
    let ndrawn = stack.nbuckets.saturating_sub(first_bucket);
    if ndrawn == 0 || t1 <= t0 {
        return;
    }
    let row_at = |t: f64| -> Option<usize> {
        let c = stack.times.partition_point(|&x| x <= t);
        (c > 0).then(|| c - 1)
    };
    let mut frac = vec![0.0f64; ndrawn];
    for i in 0..nbins {
        let e0 = t0 + (i as f64) * (t1 - t0) / nbins as f64;
        let e1 = t0 + ((i + 1) as f64) * (t1 - t0) / nbins as f64;
        let (r0, r1) = (row_at(e0), row_at(e1));
        let mut total = 0.0f64;
        let mut delta = vec![0.0f64; ndrawn];
        for (pos, d_slot) in delta.iter_mut().enumerate() {
            let bkt = first_bucket + pos;
            let v1 = r1.map(|r| stack.at(r, bkt)).unwrap_or(0);
            let v0 = r0.map(|r| stack.at(r, bkt)).unwrap_or(0);
            let d = v1.saturating_sub(v0) as f64;
            *d_slot = d;
            total += d;
        }
        if total > 0.0 {
            for pos in 0..ndrawn {
                frac[pos] = delta[pos] / total * 100.0;
            }
        }
        f((e0 + e1) * 0.5, &frac);
    }
}

/// Buckets that reach [`BUCKET_SHARE_MIN_PCT`] of any visible time column.
/// The stored share is the average across columns that have a mix, so a
/// band that is large on the chart stays listed when it is a small
/// fraction of the window's sample count.
fn stack_bucket_shares(
    stack: &analysis::GoodputStack,
    first_bucket: usize,
    t0: f64,
    t1: f64,
    nbins: usize,
) -> Vec<(usize, f64)> {
    let ndrawn = stack.nbuckets.saturating_sub(first_bucket);
    let mut sum = vec![0.0f64; ndrawn];
    let mut peak = vec![0.0f64; ndrawn];
    let mut columns = 0usize;
    let mut seen = false;
    for_each_column_share(stack, first_bucket, t0, t1, nbins, |_center, frac| {
        if frac.iter().any(|p| *p > 0.0) {
            seen = true;
        }
        if !seen {
            return;
        }
        columns += 1;
        for (pos, p) in frac.iter().enumerate() {
            sum[pos] += *p;
            peak[pos] = peak[pos].max(*p);
        }
    });
    if columns == 0 {
        return Vec::new();
    }
    let mut out: Vec<(usize, f64)> = (0..ndrawn)
        .filter(|&pos| peak[pos] >= BUCKET_SHARE_MIN_PCT)
        .map(|pos| (first_bucket + pos, sum[pos] / columns as f64))
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Bar chart of the aggregate goodput histogram. X is the log2 Mbps
/// bucket (labelled with the human range); Y is the sample count.
fn draw_goodput_hist(ui: &mut egui::Ui, hist: &analysis::GoodputHist, cursor: &mut SysCursor) {
    let height = if cursor.fill_remaining {
        ui.available_height().max(PLOT_H_MIN)
    } else {
        cursor.panel_h
    };
    let n = hist.buckets.len();
    let max_count = hist.buckets.iter().copied().max().unwrap_or(0);
    let bars: Vec<egui_plot::Bar> = hist
        .buckets
        .iter()
        .enumerate()
        .map(|(b, &count)| {
            egui_plot::Bar::new(b as f64, count as f64)
                .width(0.8)
                .fill(goodput_speed_color(b, n))
                .name(analysis::goodput_bucket_label(b, n))
        })
        .collect();

    let chart = egui_plot::BarChart::new(bars).element_formatter(Box::new(
        move |bar: &egui_plot::Bar, _| {
            format!(
                "{}: {} samples",
                bar.name,
                format_count(bar.value.round().max(0.0) as u64)
            )
        },
    ));

    let x_axis = AxisHints::new_x()
        .label(RichText::new("goodput (Mbps, log2 buckets)").size(12.0))
        .formatter(move |mark, _, _| {
            let b = mark.value.round();
            if b < 0.0 || b as usize >= n || (mark.value - b).abs() > 1e-6 {
                String::new()
            } else {
                analysis::goodput_bucket_label(b as usize, n)
            }
        });
    let y_axis = agg_y_axis("samples");

    let resp = Plot::new("tcp_goodput_hist")
        .height(height)
        .custom_x_axes(vec![x_axis])
        .custom_y_axes(vec![y_axis])
        .include_x(-0.5)
        .include_x(n as f64 - 0.5)
        .include_y(0.0)
        .include_y(max_count as f64 * 1.08)
        .set_margin_fraction(egui::vec2(0.0, 0.0))
        .allow_scroll(false)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_boxed_zoom(false)
        .show_x(false)
        .show(ui, |plot_ui| {
            plot_ui.bar_chart(chart);
        });
    cursor.last_plot_rect = Some(resp.response.rect);
}

/// Bar chart of the aggregate smoothed-RTT histogram. X is the custom
/// usec bucket; Y is connection-seconds. Bar color runs green (low RTT)
/// through yellow to red (high RTT), the same scale as the stacked view.
fn draw_srtt_hist(ui: &mut egui::Ui, hist: &analysis::GoodputHist, cursor: &mut SysCursor) {
    let height = if cursor.fill_remaining {
        ui.available_height().max(PLOT_H_MIN)
    } else {
        cursor.panel_h
    };
    let n = hist.buckets.len();
    let max_count = hist.buckets.iter().copied().max().unwrap_or(0);
    let bars: Vec<egui_plot::Bar> = hist
        .buckets
        .iter()
        .enumerate()
        .map(|(b, &count)| {
            egui_plot::Bar::new(b as f64, count as f64)
                .width(0.8)
                .fill(srtt_speed_color(b, n))
                .name(analysis::srtt_bucket_label(b, n))
        })
        .collect();

    let chart = egui_plot::BarChart::new(bars).element_formatter(Box::new(
        move |bar: &egui_plot::Bar, _| {
            format!(
                "{}: {} connection-seconds",
                bar.name,
                format_count(bar.value.round().max(0.0) as u64)
            )
        },
    ));

    let x_axis = AxisHints::new_x()
        .label(RichText::new("smoothed RTT").size(12.0))
        .formatter(move |mark, _, _| {
            let b = mark.value.round();
            if b < 0.0 || b as usize >= n || (mark.value - b).abs() > 1e-6 {
                String::new()
            } else {
                analysis::srtt_bucket_label(b as usize, n)
            }
        });
    let y_axis = agg_y_axis("conn-s");

    let resp = Plot::new("tcp_srtt_hist")
        .height(height)
        .custom_x_axes(vec![x_axis])
        .custom_y_axes(vec![y_axis])
        .include_x(-0.5)
        .include_x(n as f64 - 0.5)
        .include_y(0.0)
        .include_y(max_count as f64 * 1.08)
        .set_margin_fraction(egui::vec2(0.0, 0.0))
        .allow_scroll(false)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_boxed_zoom(false)
        .show_x(false)
        .show(ui, |plot_ui| {
            plot_ui.bar_chart(chart);
        });
    cursor.last_plot_rect = Some(resp.response.rect);
}

// ---------------------------------------------------------------------------
// STARS aggregate-sample section (Aggregate tab)
// ---------------------------------------------------------------------------

pub struct StarsAggViewState {
    pub(crate) series: analysis::StarsAggSeries,
    /// See [`InterfacesViewState::settle_frames`].
    pub(crate) settle_frames: u8,
}

impl StarsAggViewState {
    pub(crate) fn new(samples: &[StarsAggSample], t0_us: u64) -> Self {
        Self {
            series: analysis::build_stars_agg_series(samples, t0_us),
            settle_frames: SETTLE_FRAMES,
        }
    }
}

impl EventExplorerApp {
    /// Lazily build the STARS view and tick its settle counter. See
    /// [`Self::ensure_tcp_samples_view`].
    fn ensure_stars_agg_view(&mut self, ui: &mut egui::Ui) -> &mut StarsAggViewState {
        if self.stars_agg_view.is_none() {
            let t0 = self
                .file_info
                .as_ref()
                .map(|f| f.capture_start)
                .unwrap_or(0);
            self.stars_agg_view = Some(StarsAggViewState::new(&self.stars_agg_samples, t0));
        }
        let state = self.stars_agg_view.as_mut().unwrap();
        if state.settle_frames > 0 {
            state.settle_frames -= 1;
            ui.ctx().request_repaint();
        }
        state
    }

    /// STARS controller section of the Aggregate tab: the machine-wide
    /// adaptive egress-rate-squeeze controller's throttle gauge and
    /// slot-gate deny rate ("STARS Pressure & Slot-Gate Denies"), plus
    /// secondary burst-size context ("STARS Slot-Gate Budget / Peak
    /// Burst"). The config values (health limit, min rate, gate enabled)
    /// rarely change within a capture, so they're surfaced as a header
    /// line rather than a time series. No-op when the capture has no
    /// `STARS_AGG_SAMPLE` data.
    pub(crate) fn stars_agg_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        if self.stars_agg_samples.is_empty() {
            return;
        }
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.agg_time_axis.apply(ui.ctx(), "stars_pressure_plot");
        self.agg_time_axis.apply(ui.ctx(), "stars_budget_plot");
        self.ensure_stars_agg_view(ui);
        let (pressure_peak, deny_peak, denied_s, gate_note, health, min_rate) = {
            let series = &self.stars_agg_view.as_ref().unwrap().series;
            let pressure_peak = series.pressure_pct.iter().fold(0.0_f64, |m, p| m.max(p[1]));
            let deny_peak = series.deny_pct.iter().fold(0.0_f64, |m, p| m.max(p[1]));
            let denied_peak = series.denied_mbps.iter().fold(0.0_f64, |m, p| m.max(p[1]));
            let gate_note = if series.gate_enabled {
                "enabled"
            } else {
                "disabled"
            };
            (
                pressure_peak,
                deny_peak,
                format_goodput(denied_peak),
                gate_note,
                format_goodput(series.health_limit_bps as f64 / 1_000_000.0),
                format_goodput(series.min_rate_bps as f64 / 1_000_000.0),
            )
        };

        let catalog = PlotId::Stars;
        if self.system_layout.slot_is_shown("stars_pressure_plot") {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.strong(RichText::new("STARS Pressure & Slot-Gate Denies").size(14.0));
                ui.add_space(16.0);
                ui.label(
                    RichText::new(format!(
                        "Pressure peak: {pressure_peak:.1}%  |  Deny peak: {deny_peak:.1}%  |  \
                         Denied peak: {denied_s}  |  Slot gate: {gate_note}  |  Health limit: {health}  |  \
                         Min rate: {min_rate}"
                    ))
                    .size(12.0)
                    .color(Color32::from_gray(180)),
                );
            });
            prepare_plot_slot(
                &self.system_layout,
                catalog,
                "stars_pressure_plot",
                cursor,
                ui.available_height(),
            );
            let series = &self.stars_agg_view.as_ref().unwrap().series;
            let lines = [
                PlotLine {
                    name: "Pressure".to_string(),
                    color: COLOR_STARS_PRESSURE,
                    data: &series.pressure_pct,
                },
                PlotLine {
                    name: "Slot-gate deny rate".to_string(),
                    color: COLOR_STARS_DENY,
                    data: &series.deny_pct,
                },
            ];
            draw_series_plot(
                ui,
                "stars_pressure_plot",
                "%",
                cursor.panel_h,
                &lines,
                cursor,
                seed,
            );
            finish_plot_slot(
                ui,
                &mut self.system_layout,
                &mut self.system_draft,
                catalog,
                "stars_pressure_plot",
                cursor.last_plot_rect,
            );
        }

        if self.system_layout.slot_is_shown("stars_budget_plot") {
            ui.add_space(6.0);
            ui.strong(RichText::new("STARS Slot-Gate Budget / Peak Burst").size(14.0));
            prepare_plot_slot(
                &self.system_layout,
                catalog,
                "stars_budget_plot",
                cursor,
                ui.available_height(),
            );
            let series = &self.stars_agg_view.as_ref().unwrap().series;
            let lines = [
                PlotLine {
                    name: "Peak burst (since last sample)".to_string(),
                    color: COLOR_STARS_PEAK,
                    data: &series.slot_peak_kb,
                },
                PlotLine {
                    name: "Configured budget".to_string(),
                    color: COLOR_STARS_BUDGET,
                    data: &series.slot_budget_kb,
                },
            ];
            draw_series_plot(
                ui,
                "stars_budget_plot",
                "KB",
                cursor.panel_h,
                &lines,
                cursor,
                seed,
            );
            finish_plot_slot(
                ui,
                &mut self.system_layout,
                &mut self.system_draft,
                catalog,
                "stars_budget_plot",
                cursor.last_plot_rect,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// HPTS wheel callout histogram sections (Aggregate tab)
// ---------------------------------------------------------------------------

pub struct HptsHistViewState {
    pub(crate) lateness: analysis::GoodputStack,
    pub(crate) batch: analysis::GoodputStack,
    /// See [`InterfacesViewState::settle_frames`].
    pub(crate) settle_frames: u8,
}

impl HptsHistViewState {
    pub(crate) fn new(events: &[HptsWheelEvent], t0_us: u64) -> Self {
        Self {
            lateness: analysis::build_hpts_lateness_stack(events, t0_us),
            batch: analysis::build_hpts_batch_stack(events, t0_us),
            settle_frames: SETTLE_FRAMES,
        }
    }
}

impl EventExplorerApp {
    /// Lazily build the hpts-histogram view and tick its settle counter.
    /// See [`Self::ensure_tcp_samples_view`].
    fn ensure_hpts_hist_view(&mut self, ui: &mut egui::Ui) -> &mut HptsHistViewState {
        if self.hpts_hist_view.is_none() {
            let t0 = self
                .file_info
                .as_ref()
                .map(|f| f.capture_start)
                .unwrap_or(0);
            self.hpts_hist_view = Some(HptsHistViewState::new(&self.hpts_wheel_events, t0));
        }
        let state = self.hpts_hist_view.as_mut().unwrap();
        if state.settle_frames > 0 {
            state.settle_frames -= 1;
            ui.ctx().request_repaint();
        }
        state
    }

    /// HPTS wheel callout histogram sections of the Aggregate tab: how
    /// late each wheel callout fired relative to its requested slot
    /// ("HPTS Callout Lateness", from `HPTS_LATENESS_SAMPLE`) and how
    /// many connections each callout processed ("HPTS Batch Size", from
    /// `HPTS_TP_BATCH_SAMPLE`), both as a stacked distribution over time
    /// (diffing cumulative per-hpts-instance histogram snapshots -- see
    /// `analysis::build_hpts_lateness_stack`). Each histogram is its own
    /// catalog row. No-op when the capture predates these events (ids 13/14).
    pub(crate) fn hpts_hist_section(
        &mut self,
        ui: &mut egui::Ui,
        cursor: &mut SysCursor,
        which: PlotId,
    ) {
        if self.hpts_wheel_events.is_empty() {
            return;
        }
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.agg_time_axis.apply(ui.ctx(), which.chrome_slot());
        self.ensure_hpts_hist_view(ui);
        let lateness = which == PlotId::HptsLateness;
        let empty = self.hpts_hist_view.as_ref().is_some_and(|s| {
            if lateness {
                s.lateness.is_empty()
            } else {
                s.batch.is_empty()
            }
        });
        if empty {
            return;
        }

        ui.add_space(6.0);
        if lateness {
            ui.strong(RichText::new("HPTS Callout Lateness").size(14.0));
            ui.label(
                RichText::new(
                    "Share of HPTS_LATENESS_SAMPLE occupancy per usec-late band (log2 buckets)",
                )
                .size(12.0)
                .color(Color32::from_gray(180)),
            );
            draw_bucket_key(
                ui,
                &self.hpts_hist_view.as_ref().unwrap().lateness,
                false,
                false,
                analysis::hpts_lateness_bucket_label,
                goodput_bucket_color,
                seed.0,
                seed.1,
                cursor.plot_bins,
            );
            draw_bucket_stack(
                ui,
                which.chrome_slot(),
                &self.hpts_hist_view.as_ref().unwrap().lateness,
                cursor.panel_h,
                "% of samples",
                false,
                cursor,
                seed,
                analysis::hpts_lateness_bucket_label,
                goodput_bucket_color,
            );
        } else {
            ui.strong(RichText::new("HPTS Batch Size").size(14.0));
            ui.label(
                RichText::new(
                    "Connections processed per wheel callout (HPTS_TP_BATCH_SAMPLE occupancy)",
                )
                .size(12.0)
                .color(Color32::from_gray(180)),
            );
            draw_bucket_key(
                ui,
                &self.hpts_hist_view.as_ref().unwrap().batch,
                false,
                false,
                analysis::hpts_batch_bucket_label,
                goodput_bucket_color,
                seed.0,
                seed.1,
                cursor.plot_bins,
            );
            draw_bucket_stack(
                ui,
                which.chrome_slot(),
                &self.hpts_hist_view.as_ref().unwrap().batch,
                cursor.panel_h,
                "% of samples",
                false,
                cursor,
                seed,
                analysis::hpts_batch_bucket_label,
                goodput_bucket_color,
            );
        }
    }

    fn ensure_hpts_slot_gate_view(&mut self, ui: &mut egui::Ui) -> &mut HptsSlotGateViewState {
        if self.hpts_slot_gate_view.is_none() {
            let t0 = self
                .file_info
                .as_ref()
                .map(|f| f.capture_start)
                .unwrap_or(0);
            self.hpts_slot_gate_view =
                Some(HptsSlotGateViewState::new(&self.hpts_wheel_events, t0));
        }
        let state = self.hpts_slot_gate_view.as_mut().unwrap();
        if state.settle_frames > 0 {
            state.settle_frames -= 1;
            ui.ctx().request_repaint();
        }
        state
    }

    /// One Slot Gate catalog row. One point per second, summed across
    /// CPUs, from `HPTS_SLOT_GATE_SAMPLE`, then time-weighted into the
    /// Time buckets count over the visible window.
    pub(crate) fn hpts_slot_gate_section(
        &mut self,
        ui: &mut egui::Ui,
        cursor: &mut SysCursor,
        which: PlotId,
    ) {
        if !self.hpts_has_slot_gate {
            return;
        }
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.ensure_hpts_slot_gate_view(ui);
        let spec = match which {
            PlotId::HptsSlotGateOutput => SlotGatePlot {
                slot: which.chrome_slot(),
                title: "Slot Gate Output",
                subtitle: "Bytes the gate counted, summed across CPUs",
                y_label: "Gbps",
                lines: &[("Output", COLOR_SLOT_BYTES, SlotGateLine::Bytes)],
                sum_toggle: false,
            },
            PlotId::HptsSlotGateFires => SlotGatePlot {
                slot: which.chrome_slot(),
                title: "Slot Gate Fires",
                subtitle: "Completed fires and rollover diverts per second",
                y_label: "/s",
                lines: &[
                    ("Fires", COLOR_SLOT_FIRES, SlotGateLine::Fires),
                    ("Diverts", COLOR_SLOT_DIVERTS, SlotGateLine::Diverts),
                ],
                sum_toggle: false,
            },
            PlotId::HptsSlotGateChecks => SlotGatePlot {
                slot: which.chrome_slot(),
                title: "Slot Gate Checks",
                subtitle: "Counter walks and closed publishes per second",
                y_label: "/s",
                lines: &[
                    ("Walks", COLOR_SLOT_WALKS, SlotGateLine::Walks),
                    ("Closed", COLOR_SLOT_CLOSED, SlotGateLine::Closed),
                ],
                sum_toggle: false,
            },
            PlotId::HptsSlotGateRollover => SlotGatePlot {
                slot: which.chrome_slot(),
                title: "Slot Gate Rollover",
                subtitle: "Deepest rollover list on any CPU",
                y_label: "flows",
                lines: &[("Rollover max", COLOR_SLOT_ROLLOVER, SlotGateLine::Rollover)],
                sum_toggle: true,
            },
            _ => return,
        };
        self.slot_gate_plot(ui, cursor, seed, spec);
    }

    fn slot_gate_plot(
        &mut self,
        ui: &mut egui::Ui,
        cursor: &mut SysCursor,
        seed: (f64, f64, bool),
        spec: SlotGatePlot<'_>,
    ) {
        self.agg_time_axis.apply(ui.ctx(), spec.slot);
        ui.add_space(6.0);
        let mut sum_cpus = spec.sum_toggle
            && self
                .hpts_slot_gate_view
                .as_ref()
                .map(|v| v.rollover_sum_cpus)
                .unwrap_or(false);
        ui.horizontal_wrapped(|ui| {
            ui.strong(RichText::new(spec.title).size(14.0));
            ui.add_space(16.0);
            let subtitle = if spec.sum_toggle && sum_cpus {
                "Rollover lists summed across CPUs"
            } else if spec.sum_toggle {
                "Deepest rollover list on any CPU"
            } else {
                spec.subtitle
            };
            ui.label(
                RichText::new(subtitle)
                    .size(12.0)
                    .color(Color32::from_gray(180)),
            );
            if spec.sum_toggle {
                ui.add_space(12.0);
                ui.checkbox(&mut sum_cpus, RichText::new("Sum of CPUs").size(13.0));
            }
        });
        if spec.sum_toggle {
            self.hpts_slot_gate_view.as_mut().unwrap().rollover_sum_cpus = sum_cpus;
        }
        let series = &self.hpts_slot_gate_view.as_ref().unwrap().series;
        let lines: Vec<PlotLine<'_>> = spec
            .lines
            .iter()
            .map(|(name, color, line)| {
                let summed = spec.sum_toggle && sum_cpus && matches!(line, SlotGateLine::Rollover);
                PlotLine {
                    name: if summed {
                        "Rollover sum".to_string()
                    } else {
                        (*name).to_string()
                    },
                    color: *color,
                    data: if summed {
                        series.rollover_sum.as_slice()
                    } else {
                        line.series(series)
                    },
                }
            })
            .collect();
        let mut data_lo = f64::INFINITY;
        let mut data_hi = f64::NEG_INFINITY;
        for line in &lines {
            if let (Some(first), Some(last)) = (line.data.first(), line.data.last()) {
                data_lo = data_lo.min(first[0]);
                data_hi = data_hi.max(last[0]);
            }
        }
        if !data_lo.is_finite() {
            data_lo = 0.0;
        }
        if !data_hi.is_finite() || data_hi <= data_lo {
            data_hi = data_lo + 1.0;
        }
        let view = seed_view_x(seed.0, seed.1, data_lo, data_hi);
        let nbins = cursor.plot_bins;
        let binned: Vec<(String, Color32, Vec<[f64; 2]>)> = lines
            .iter()
            .map(|line| {
                (
                    line.name.clone(),
                    line.color,
                    slot_gate_bins(line.data, view, nbins),
                )
            })
            .collect();
        let drawn: Vec<PlotLine<'_>> = binned
            .iter()
            .map(|(name, color, data)| PlotLine {
                name: name.clone(),
                color: *color,
                data: data.as_slice(),
            })
            .collect();
        draw_series_plot(
            ui,
            spec.slot,
            spec.y_label,
            cursor.panel_h,
            &drawn,
            cursor,
            seed,
        );
    }
}

/// Hold each 1 Hz slot-gate sample across its second, then time-weight
/// into `nbins` equal bins on `view`.
fn slot_gate_bins(data: &[[f64; 2]], view: [f64; 2], nbins: usize) -> Vec<[f64; 2]> {
    if data.is_empty() {
        return Vec::new();
    }
    let mut closed = Vec::with_capacity(data.len() + 1);
    closed.extend_from_slice(data);
    let last = data[data.len() - 1];
    closed.push([last[0] + 1.0, last[1]]);
    analysis::step_series_bins(&closed, view[0], view[1], nbins)
}

struct SlotGatePlot<'a> {
    slot: &'a str,
    title: &'a str,
    subtitle: &'a str,
    y_label: &'a str,
    lines: &'a [(&'a str, Color32, SlotGateLine)],
    sum_toggle: bool,
}

pub struct HptsSlotGateViewState {
    pub(crate) series: analysis::HptsSlotGateSeries,
    pub(crate) settle_frames: u8,
    rollover_sum_cpus: bool,
}

impl HptsSlotGateViewState {
    pub(crate) fn new(events: &[HptsWheelEvent], t0_us: u64) -> Self {
        Self {
            series: analysis::build_hpts_slot_gate_series(events, t0_us),
            settle_frames: SETTLE_FRAMES,
            rollover_sum_cpus: false,
        }
    }
}

enum SlotGateLine {
    Bytes,
    Fires,
    Diverts,
    Walks,
    Closed,
    Rollover,
}

impl SlotGateLine {
    fn series<'a>(&self, s: &'a analysis::HptsSlotGateSeries) -> &'a [[f64; 2]] {
        match self {
            Self::Bytes => &s.bytes_gbps,
            Self::Fires => &s.fires,
            Self::Diverts => &s.diverts,
            Self::Walks => &s.walks,
            Self::Closed => &s.closed,
            Self::Rollover => &s.rollover_max,
        }
    }
}

#[cfg(test)]
mod slot_gate_bins_tests {
    use super::slot_gate_bins;

    #[test]
    fn slot_gate_bins_follow_the_bucket_count() {
        let data = [[0.0, 10.0], [1.0, 30.0]];
        let coarse = slot_gate_bins(&data, [0.0, 2.0], 1);
        assert_eq!(coarse.len(), 1);
        assert!((coarse[0][1] - 20.0).abs() < 1e-9);
        let fine = slot_gate_bins(&data, [0.0, 2.0], 4);
        assert_eq!(fine.len(), 4);
        assert!((fine[0][1] - 10.0).abs() < 1e-9);
        assert!((fine[2][1] - 30.0).abs() < 1e-9);
    }
}

#[cfg(test)]
mod throughput_display_tests {
    use super::throughput_display;

    #[test]
    fn stays_in_gbps_at_a_gigabit_and_above() {
        assert_eq!(throughput_display(2.5), (1.0, "Gb/s"));
        assert_eq!(throughput_display(1.0), (1.0, "Gb/s"));
        assert_eq!(throughput_display(0.0), (1.0, "Gb/s"));
    }

    #[test]
    fn drop_axis_compacts_thousands_and_millions() {
        assert_eq!(super::fmt_drop_axis(0.0), "0");
        assert_eq!(super::fmt_drop_axis(180.0), "180");
        assert_eq!(super::fmt_drop_axis(1_500.0), "1.5k");
        assert_eq!(super::fmt_drop_axis(180_000.0), "180k");
        assert_eq!(super::fmt_drop_axis(2_000_000.0), "2M");
        assert_eq!(super::fmt_drop_axis(1_200_000.0), "1.2M");
    }

    #[test]
    fn drops_a_unit_when_the_peak_is_under_one() {
        assert_eq!(throughput_display(0.4), (1e3, "Mb/s"));
        assert_eq!(throughput_display(1e-3), (1e3, "Mb/s"));
        assert_eq!(throughput_display(4e-4), (1e6, "Kb/s"));
        assert_eq!(throughput_display(1e-6), (1e6, "Kb/s"));
        assert_eq!(throughput_display(4e-7), (1e9, "b/s"));
    }
}

#[cfg(test)]
mod bucket_share_tests {
    use super::*;

    #[test]
    fn notable_bucket_shares_drops_sub_percent_and_orders_desc() {
        let got = notable_bucket_shares([(3, 0.4), (40, 51.0), (1, 2.0), (7, 1.0)]);
        assert_eq!(got, vec![(40, 51.0), (1, 2.0), (7, 1.0)]);
        assert_eq!(
            order_bucket_shares(got, true)
                .into_iter()
                .map(|(bkt, _)| bkt)
                .collect::<Vec<_>>(),
            vec![1, 7, 40]
        );
    }

    #[test]
    fn stack_bucket_shares_uses_window_delta() {
        let stack = analysis::GoodputStack {
            nbuckets: 4,
            times: vec![0.0, 1.0],
            cum: vec![0, 0, 0, 0, 10, 90, 0, 0],
            labels: vec![],
        };
        let got = stack_bucket_shares(&stack, 0, 0.0, 1.0, 4);
        assert_eq!(got, vec![(1, 90.0), (0, 10.0)]);
    }

    #[test]
    fn legend_keeps_a_band_that_is_large_on_the_chart_but_few_samples() {
        // First half is a huge count of bucket 1. Second half is 25/75
        // bucket 0/1, but only 100 samples. The sample-weighted total of
        // bucket 0 is 0.025%; its column share is 25%.
        let stack = analysis::GoodputStack {
            nbuckets: 2,
            times: vec![0.0, 0.5, 1.0],
            cum: vec![0, 0, 0, 100_000, 25, 100_075],
            labels: vec![],
        };
        let got = stack_bucket_shares(&stack, 0, 0.0, 1.0, 2);
        assert_eq!(got, vec![(1, 87.5), (0, 12.5)]);
    }
}
