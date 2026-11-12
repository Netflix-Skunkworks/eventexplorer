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

//! Shared, reusable time-axis interaction model for stacked time-series
//! panels. Owns one zoom window (`view_x`, trace-relative seconds) plus a
//! zoom-history undo stack and the transient box-zoom / Δt-measurement drag
//! state, and drives the same custom gestures the per-session plots use:
//! left-drag box zoom, Shift+left-drag to slide a zoomed window
//! without changing its width, middle-click reset, double-click
//! undo, and a right-drag measurement highlight (Shift+right-drag
//! slides that band). Middle-drag does not
//! pan (the window may still scroll). egui_plot's built-in zoom/scroll stays
//! disabled — `view_x` is the single source of truth for the shared
//! axis, mirroring the "Custom zoom only" rule in AGENTS.md.
//!
//! Both time-series surfaces share this: the System tab owns one
//! (`agg_time_axis`), and each open session's `PlotViewState` owns one for
//! its stacked RTT/rate/cwnd/sizes panels.

use super::*;
use egui_plot::PlotTransform;

/// One geometry sample for the gesture pass: a panel's on-screen rect and
/// the transform mapping screen x to data x (capture seconds). Every
/// stacked time panel contributes one per frame (CPU Usage included).
pub(crate) type PanelGeom = (egui::Rect, PlotTransform);

/// Slide `[lo, hi]` by `dx`, keeping width, clamped to `[bound_lo, bound_hi]`.
pub(crate) fn translate_x_range(
    lo: f64,
    hi: f64,
    dx: f64,
    bound_lo: f64,
    bound_hi: f64,
) -> [f64; 2] {
    let width = (hi - lo).max(0.0);
    let span = bound_hi - bound_lo;
    if width <= 0.0 || span <= 0.0 {
        return [lo, hi];
    }
    if width >= span {
        return [bound_lo, bound_hi];
    }
    let new_lo = (lo + dx).clamp(bound_lo, bound_hi - width);
    [new_lo, new_lo + width]
}

/// What a [`TimeAxisController::handle`] pass changed, so the caller can
/// re-fit the cached plot bounds it owns.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimeAxisOutcome {
    /// Nothing that needs the caller to touch cached plot bounds.
    Idle,
    /// The view returned to the full (un-zoomed) range. `hard` marks the
    /// middle-click "reset everything" gesture; a plain double-click undo
    /// that happened to pop back to full reports `hard = false`. Callers
    /// that also keep per-plot y-max overrides clear those on `hard`.
    Reset { hard: bool },
}

#[derive(Clone, Copy)]
struct GrabDrag {
    press_sx: f32,
    orig: [f64; 2],
    x0: f32,
    x1: f32,
}

impl GrabDrag {
    fn from_press(pos_x: f32, orig: [f64; 2], transform: PlotTransform) -> Self {
        let [lo, hi] = orig;
        Self {
            press_sx: pos_x,
            orig,
            x0: transform.position_from_point_x(lo),
            x1: transform.position_from_point_x(hi),
        }
    }

    fn dx_data(self, pos_x: f32) -> Option<f64> {
        let span_px = (self.x1 - self.x0) as f64;
        if span_px.abs() <= 1.0 {
            return None;
        }
        let [lo, hi] = self.orig;
        Some((pos_x - self.press_sx) as f64 / span_px * (hi - lo))
    }
}

#[derive(Default)]
pub(crate) struct TimeAxisController {
    /// Shared zoom window in trace-relative seconds. `None` = full range.
    pub(crate) view_x: Option<[f64; 2]>,
    /// Undo stack of prior `view_x` values (double-click pops one).
    zoom_history: Vec<Option<[f64; 2]>>,
    /// In-progress left-drag box zoom (data x of the press).
    drag_start_x: Option<f64>,
    /// Shift+left-drag: move the zoom window (screen x + original bounds).
    pan_start: Option<GrabDrag>,
    /// Shift+right-drag: move the Δt band.
    measure_move: Option<GrabDrag>,
    /// In-progress / committed right-drag Δt measurement (data x).
    measure_start_x: Option<f64>,
    measure_range: Option<[f64; 2]>,
}

impl TimeAxisController {
    /// Drop all zoom + measurement state (used on new-file load).
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Drop an unfinished box-zoom or Δt drag. The tab bar calls this
    /// on switch; each tab keeps its own committed `view_x`.
    pub(crate) fn cancel_gesture(&mut self) {
        self.drag_start_x = None;
        self.pan_start = None;
        self.measure_move = None;
        self.measure_start_x = None;
    }

    /// `(x_lo, x_hi, x_auto)` for seeding a plot's initial x-bounds:
    /// the zoom window when zoomed (x auto off), else the full data range
    /// (x auto on so egui_plot fits to the data ∪ `[x_min, x_max]`).
    pub(crate) fn seed(&self, x_min: f64, x_max: f64) -> (f64, f64, bool) {
        match self.view_x {
            Some([lo, hi]) => (lo, hi, false),
            None => (x_min, x_max, true),
        }
    }

    /// Force one plot's cached `PlotMemory` x-bounds to the zoom window.
    /// egui_plot 0.27 ignores `include_x` once `auto_bounds.x` is off, so a
    /// zoomed panel needs its bounds written directly every frame; a no-op
    /// when not zoomed. Call before the plot's `.show`.
    pub(crate) fn apply(&self, ctx: &egui::Context, id: &str) {
        if let Some([x0, x1]) = self.view_x {
            let plot_id = egui::Id::new(id);
            if let Some(mut mem) = PlotMemory::load(ctx, plot_id) {
                let mut bounds = *mem.bounds();
                bounds.set_x(&PlotBounds::from_min_max(
                    [x0.max(0.0), bounds.min()[1]],
                    [x1, bounds.max()[1]],
                ));
                mem.set_bounds(bounds);
                mem.auto_bounds = Vec2b::new(false, mem.auto_bounds.y);
                mem.store(ctx, plot_id);
            }
        }
    }

    /// Trace-relative seconds for an absolute window. `(0, u64::MAX)`
    /// is the un-zoomed capture (`None`).
    pub(crate) fn view_x_for_window(
        capture_start_us: u64,
        lo_us: u64,
        hi_us: u64,
    ) -> Option<[f64; 2]> {
        if lo_us == 0 && hi_us == u64::MAX {
            return None;
        }
        let lo = (lo_us as i64 - capture_start_us as i64) as f64 / 1_000_000.0;
        let hi = (hi_us as i64 - capture_start_us as i64) as f64 / 1_000_000.0;
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        if (hi - lo).abs() < 1e-9 {
            return None;
        }
        Some([lo.max(0.0), hi])
    }

    /// Replace the zoom window without touching the undo stack.
    pub(crate) fn set_view_x(&mut self, view: Option<[f64; 2]>) {
        self.view_x = view;
    }

    /// Absolute-µs window for the CPU strips (which zoom via `view_us`),
    /// given the capture's start timestamp. `None` when not zoomed.
    pub(crate) fn window_us(&self, capture_start_us: u64) -> Option<(u64, u64)> {
        let [lo, hi] = self.view_x?;
        let lo_us = capture_start_us as i64 + (lo * 1_000_000.0).round() as i64;
        let hi_us = capture_start_us as i64 + (hi * 1_000_000.0).round() as i64;
        Some((lo_us.max(0) as u64, hi_us.max(0) as u64))
    }

    /// Zoom to an absolute-µs range, pushing the prior window onto the
    /// undo stack. Used when the System tab takes the Flame time filter.
    pub(crate) fn set_window_us(&mut self, capture_start_us: u64, lo_us: u64, hi_us: u64) {
        let lo = (lo_us as i64 - capture_start_us as i64) as f64 / 1_000_000.0;
        let hi = (hi_us as i64 - capture_start_us as i64) as f64 / 1_000_000.0;
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        if (hi - lo).abs() < 1e-9 {
            return;
        }
        self.zoom_history.push(self.view_x);
        self.view_x = Some([lo.max(0.0), hi]);
    }

    /// Clear the zoom (hard reset, or Flame filter back to the full
    /// capture copied onto System).
    pub(crate) fn clear_zoom(&mut self) {
        self.view_x = None;
        self.zoom_history.clear();
        self.drag_start_x = None;
        self.pan_start = None;
    }

    /// Re-enable x auto-bounds on the given plots. Needed when returning to
    /// the un-zoomed state: `apply` left `auto_bounds.x = false` in memory,
    /// so without this the panels would stay pinned to the last window.
    /// The caller invokes this in response to [`TimeAxisOutcome::Reset`].
    pub(crate) fn reset_plots(&self, ctx: &egui::Context, ids: &[&str]) {
        for id in ids {
            let plot_id = egui::Id::new(*id);
            if let Some(mut mem) = PlotMemory::load(ctx, plot_id) {
                mem.auto_bounds = Vec2b::new(true, mem.auto_bounds.y);
                mem.store(ctx, plot_id);
            }
        }
    }

    /// Run one gesture pass over the union of the supplied panel geometries
    /// (drawn this frame). Handles box zoom, middle-click reset, undo, and
    /// the Δt measurement overlay, mutating `view_x` for the next frame's
    /// `apply`. `x_min`/`x_max` are the full data range. Returns a
    /// [`TimeAxisOutcome`]; on a `Reset` the caller re-fits the cached
    /// x-bounds of the plots it owns (via [`Self::reset_plots`] or its own
    /// per-plot reset).
    pub(crate) fn handle(
        &mut self,
        ui: &egui::Ui,
        geoms: &[PanelGeom],
        x_min: f64,
        x_max: f64,
    ) -> TimeAxisOutcome {
        let Some((first_rect, first_tf)) = geoms.first() else {
            return TimeAxisOutcome::Idle;
        };
        let [vis_lo, vis_hi] = self.view_x.unwrap_or([x_min, x_max]);
        let data_x_at = |pos: egui::Pos2, transform: PlotTransform| {
            let left = geoms
                .iter()
                .map(|(r, _)| r.left())
                .fold(f32::INFINITY, f32::min);
            let right = geoms
                .iter()
                .map(|(r, _)| r.right())
                .fold(f32::NEG_INFINITY, f32::max);
            let sx = pos.x.clamp(left, right);
            transform
                .value_from_position(egui::pos2(sx, pos.y))
                .x
                .clamp(vis_lo, vis_hi)
        };

        let pointer_pos = ui.input(|i| i.pointer.latest_pos());
        // A press on a higher layer (the tab-bar gear) stays with that widget.
        let covered = pointer_pos.is_some_and(|p| {
            ui.ctx()
                .layer_id_at(p)
                .is_some_and(|id| id != ui.layer_id())
        });
        let pointer_over =
            !covered && pointer_pos.is_some_and(|p| geoms.iter().any(|(r, _)| r.contains(p)));
        let transform = pointer_pos
            .and_then(|p| geoms.iter().find(|(r, _)| r.contains(p)).map(|(_, t)| *t))
            .unwrap_or(*first_tf);
        let plots_rect = geoms.iter().fold(*first_rect, |acc, (r, _)| acc.union(*r));
        let mut changed = false;
        let mut outcome = TimeAxisOutcome::Idle;

        if pointer_over {
            // Double-click primary: undo the last zoom.
            if ui.input(|i| {
                i.pointer
                    .button_double_clicked(egui::PointerButton::Primary)
            }) {
                self.drag_start_x = None;
                self.pan_start = None;
                self.measure_move = None;
                self.view_x = self.zoom_history.pop().unwrap_or(None);
                if self.view_x.is_none() {
                    outcome = TimeAxisOutcome::Reset { hard: false };
                }
                changed = true;
            }

            // Primary press (not the second click of a double-click):
            // Shift pans a zoomed window; otherwise start a box-zoom drag.
            if ui.input(|i| i.pointer.primary_pressed())
                && !ui.input(|i| {
                    i.pointer
                        .button_double_clicked(egui::PointerButton::Primary)
                })
            {
                if let Some(pos) = pointer_pos {
                    let x = data_x_at(pos, transform);
                    if ui.input(|i| i.modifiers.shift) {
                        if let Some([lo, hi]) = self.view_x {
                            self.pan_start = Some(GrabDrag::from_press(pos.x, [lo, hi], transform));
                        } else {
                            self.drag_start_x = Some(x);
                        }
                    } else {
                        self.drag_start_x = Some(x);
                    }
                }
            }

            // Right press: Shift slides an existing Δt band; otherwise
            // start a new measurement.
            if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary)) {
                if let Some(pos) = pointer_pos {
                    let shift = ui.input(|i| i.modifiers.shift);
                    if shift {
                        if let Some([lo, hi]) = self.measure_range {
                            self.measure_move =
                                Some(GrabDrag::from_press(pos.x, [lo, hi], transform));
                            self.measure_start_x = None;
                        } else {
                            self.measure_start_x = Some(data_x_at(pos, transform));
                            self.measure_range = None;
                        }
                    } else {
                        self.measure_move = None;
                        self.measure_start_x = Some(data_x_at(pos, transform));
                        self.measure_range = None;
                    }
                }
            }

            // Middle-click (no drag) resets zoom. Middle-drag is left
            // unused so the window can still scroll.
            if ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Middle)) {
                self.view_x = None;
                self.zoom_history.clear();
                self.drag_start_x = None;
                self.pan_start = None;
                outcome = TimeAxisOutcome::Reset { hard: true };
                changed = true;
            }
        }

        // Shift+drag: keep the press time under the cursor (screen delta
        // against the press-frame window).
        if let (Some(pan), Some(pos)) = (self.pan_start, pointer_pos) {
            if ui.input(|i| i.pointer.primary_down()) {
                if let Some(dx_data) = pan.dx_data(pos.x) {
                    let [lo, hi] = pan.orig;
                    let next = translate_x_range(lo, hi, -dx_data, x_min, x_max);
                    if self.view_x != Some(next) {
                        self.view_x = Some(next);
                        changed = true;
                    }
                }
            }
        }

        if let (Some(grab), Some(pos)) = (self.measure_move, pointer_pos) {
            if ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary)) {
                if let Some(dx_data) = grab.dx_data(pos.x) {
                    let [lo, hi] = grab.orig;
                    self.measure_range = Some(translate_x_range(lo, hi, dx_data, vis_lo, vis_hi));
                }
            }
        }

        // Primary drag: paint the box-zoom highlight across every panel.
        if self.pan_start.is_none() {
            if let (Some(start_x), Some(pos)) = (self.drag_start_x, pointer_pos) {
                if ui.input(|i| i.pointer.primary_down()) {
                    let current_x = data_x_at(pos, transform);
                    self.fill_x_band(
                        ui,
                        geoms,
                        start_x,
                        current_x,
                        Color32::from_rgba_premultiplied(100, 100, 255, 40),
                    );
                }
            }
        }

        // Primary release: commit box zoom, or finish a shift-pan.
        if ui.input(|i| i.pointer.primary_released()) {
            if let Some(pan) = self.pan_start.take() {
                self.drag_start_x = None;
                if self.view_x.is_some_and(|v| v != pan.orig) {
                    self.zoom_history.push(Some(pan.orig));
                    changed = true;
                }
            } else if let Some(start_x) = self.drag_start_x.take() {
                let end_x = match pointer_pos {
                    Some(pos) => data_x_at(pos, transform),
                    None if start_x - vis_lo < vis_hi - start_x => vis_hi,
                    None => vis_lo,
                };
                let start_x = start_x.clamp(vis_lo, vis_hi);
                if (end_x - start_x).abs() > 1e-9 {
                    let (lo, hi) = if start_x < end_x {
                        (start_x, end_x)
                    } else {
                        (end_x, start_x)
                    };
                    self.zoom_history.push(self.view_x);
                    self.view_x = Some([lo, hi]);
                    changed = true;
                }
            }
        }

        // Right release: finalize the measurement range.
        if ui.input(|i| i.pointer.button_released(egui::PointerButton::Secondary)) {
            if self.measure_move.take().is_some() {
                self.measure_start_x = None;
            } else if let Some(start_x) = self.measure_start_x.take() {
                let end_x = match pointer_pos {
                    Some(pos) => data_x_at(pos, transform),
                    None if start_x - vis_lo < vis_hi - start_x => vis_hi,
                    None => vis_lo,
                };
                let start_x = start_x.clamp(vis_lo, vis_hi);
                let lo = start_x.min(end_x);
                let hi = start_x.max(end_x);
                if (hi - lo) > 0.001 {
                    self.measure_range = Some([lo, hi]);
                }
            }
        }

        // Measurement overlay: the live drag or the persisted range.
        let active_measure = if self.measure_start_x.is_some()
            && ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary))
        {
            pointer_pos.map(|pos| {
                let start_x = self.measure_start_x.unwrap().clamp(vis_lo, vis_hi);
                let current_x = data_x_at(pos, transform);
                [start_x.min(current_x), start_x.max(current_x)]
            })
        } else {
            self.measure_range
        };
        if let Some([lo, hi]) = active_measure {
            if (hi - lo) > 0.0001 {
                self.draw_measure(ui, geoms, plots_rect, lo, hi);
            }
        }

        if changed {
            ui.ctx().request_repaint();
        }
        outcome
    }

    /// Paint the orange measurement band + edges + Δt label. Each panel
    /// maps data-x through its own transform so timeline gutters stay aligned.
    fn draw_measure(
        &self,
        ui: &egui::Ui,
        geoms: &[PanelGeom],
        plots_rect: egui::Rect,
        lo: f64,
        hi: f64,
    ) {
        let measure_color = Color32::from_rgb(255, 165, 0);
        self.fill_x_band(
            ui,
            geoms,
            lo,
            hi,
            Color32::from_rgba_premultiplied(255, 165, 0, 25),
        );
        self.vline_x(ui, geoms, lo, Stroke::new(1.0_f32, measure_color));
        self.vline_x(ui, geoms, hi, Stroke::new(1.0_f32, measure_color));

        let clip = ui.clip_rect();
        let visible = plots_rect.intersect(clip);
        let label = format!("Δt = {}", format_time_span(hi - lo));
        let font = egui::FontId::proportional(14.0);
        let x0_screen = geoms
            .first()
            .map(|(_, t)| t.position_from_point_x(lo))
            .unwrap_or(0.0);
        let x1_screen = geoms
            .first()
            .map(|(_, t)| t.position_from_point_x(hi))
            .unwrap_or(0.0);
        let paint_label = |painter: &egui::Painter, top: f32, bottom: f32| {
            let galley = painter.layout_no_wrap(
                label.clone(),
                font.clone(),
                Color32::from_rgb(255, 200, 100),
            );
            let box_h = galley.size().y + 4.0;
            let label_x = (x0_screen + x1_screen) / 2.0 - galley.size().x / 2.0;
            let label_y = (top + 5.0).min((bottom - box_h).max(top));
            let label_rect = egui::Rect::from_min_size(
                egui::pos2(label_x - 4.0, label_y - 2.0),
                egui::vec2(galley.size().x + 8.0, box_h),
            );
            painter.rect_filled(
                label_rect,
                3.0,
                Color32::from_rgba_premultiplied(30, 30, 35, 220),
            );
            painter.rect_stroke(
                label_rect,
                3.0,
                Stroke::new(1.0_f32, Color32::from_gray(70)),
            );
            painter.galley(egui::pos2(label_x, label_y), galley, Color32::TRANSPARENT);
        };
        if visible.is_positive() {
            paint_label(ui.painter(), visible.top(), visible.bottom());
        } else {
            paint_label(
                &ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("time_axis_overlay"),
                )),
                plots_rect.top(),
                plots_rect.bottom(),
            );
        }
    }

    /// Horizontal time-band fill, mapped per panel so gutters can differ.
    fn fill_x_band(&self, ui: &egui::Ui, geoms: &[PanelGeom], x0: f64, x1: f64, fill: Color32) {
        let clip = ui.clip_rect();
        let overlay = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("time_axis_overlay"),
        ));
        for (r, tf) in geoms {
            if !r.is_positive() {
                continue;
            }
            let lo = tf.position_from_point_x(x0.min(x1));
            let hi = tf.position_from_point_x(x0.max(x1));
            let band = egui::Rect::from_x_y_ranges(lo..=hi, r.top()..=r.bottom());
            if clip.intersects(*r) {
                ui.painter().rect_filled(band, 0.0, fill);
            } else {
                overlay.rect_filled(band, 0.0, fill);
            }
        }
    }

    fn vline_x(&self, ui: &egui::Ui, geoms: &[PanelGeom], x: f64, stroke: Stroke) {
        let clip = ui.clip_rect();
        let overlay = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("time_axis_overlay"),
        ));
        for (r, tf) in geoms {
            if !r.is_positive() {
                continue;
            }
            let sx = tf.position_from_point_x(x);
            if clip.intersects(*r) {
                ui.painter().vline(sx, r.top()..=r.bottom(), stroke);
            } else {
                overlay.vline(sx, r.top()..=r.bottom(), stroke);
            }
        }
    }
}

#[cfg(test)]
mod translate_x_range_tests {
    use super::translate_x_range;

    #[test]
    fn keeps_width_and_clamps_to_bounds() {
        assert_eq!(translate_x_range(10.0, 20.0, 5.0, 0.0, 100.0), [15.0, 25.0]);
        assert_eq!(
            translate_x_range(10.0, 20.0, -50.0, 0.0, 100.0),
            [0.0, 10.0]
        );
        assert_eq!(
            translate_x_range(80.0, 95.0, 50.0, 0.0, 100.0),
            [85.0, 100.0]
        );
    }

    #[test]
    fn full_span_stays_pinned_to_bounds() {
        assert_eq!(
            translate_x_range(0.0, 100.0, 10.0, 0.0, 100.0),
            [0.0, 100.0]
        );
    }
}
