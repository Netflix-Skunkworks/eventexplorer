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

//! `mem` provider System-tab panels: page-state stack, free-page
//! pressure, and swap use.

#[allow(unused_imports)]
use super::*;

const MEM_OTHER_LABEL: &str = "Other";
const MEM_PLOT_ID: &str = "mem_states_plot";
const PRESSURE_PLOT_ID: &str = "mem_pressure_plot";
const SWAP_PLOT_ID: &str = "mem_swap_plot";

impl EventExplorerApp {
    /// Stacked page states in GiB, bottom to top: Wired, Active,
    /// Inactive, Laundry, Other, Free. Each band is an opaque mesh
    /// between its cumulative top and the band below. The top of the
    /// stack sits on the top edge of the frame.
    pub(crate) fn memory_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        let Some(agg) = self.aggregate.as_ref() else {
            return;
        };
        let series = &self.mem_series;
        if series.is_empty() {
            return;
        }
        let seed = self.agg_time_axis.seed(0.0, agg.time_max.max(1.0));
        self.agg_time_axis.apply(ui.ctx(), MEM_PLOT_ID);

        let data_lo = series.states[0].first().map_or(0.0, |p| p[0]);
        let mut data_hi = series.last_t().unwrap_or(data_lo);
        if data_hi <= data_lo {
            data_hi = data_lo + 1.0;
        }
        let (x_lo, x_hi, _) = seed;
        let [vx0, vx1] = seed_view_x(x_lo, x_hi, data_lo, data_hi);
        let (centers, tops) = if vx1 > vx0 {
            analysis::mem_stack_bins(
                &series.states,
                series.usable_gib,
                vx0,
                vx1,
                cursor.plot_bins,
            )
        } else {
            (Vec::new(), std::array::from_fn(|_| Vec::new()))
        };
        let band_points: [Vec<[f64; 2]>; 6] = std::array::from_fn(|k| {
            centers
                .iter()
                .zip(&tops[k])
                .map(|(&x, &y)| [x, y])
                .collect()
        });

        let latest = |k: usize| series.states[k].last().map_or(0.0, |p| p[1]);
        let last_other = analysis::mem_other_gib((0..5).map(latest).sum(), series.usable_gib);
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.strong(RichText::new("Memory").size(14.0));
            show_memory_help_button(ui);
            ui.add_space(8.0);
            let mut summary = format!(
                "Last: wired {:.1} · active {:.1} · inactive {:.1} · free {:.1}",
                latest(4),
                latest(3),
                latest(1),
                latest(0)
            );
            if series.usable_gib > 0.0 {
                summary.push_str(&format!(
                    " · other {last_other:.1}  |  usable {:.1} GiB",
                    series.usable_gib
                ));
            } else {
                summary.push_str(" GiB");
            }
            ui.label(
                RichText::new(summary)
                    .size(12.0)
                    .color(Color32::from_gray(180)),
            );
        });

        let y_peak = tops[5].iter().copied().fold(0.0_f64, f64::max);
        let y_top = if y_peak > 0.0 { y_peak } else { 1.0 };

        let mut plot = Plot::new(MEM_PLOT_ID)
            .id(egui::Id::new(MEM_PLOT_ID))
            .height(cursor.plot_h(ui))
            .include_x(x_lo)
            .include_x(x_hi)
            .include_y(0.0)
            .include_y(y_top)
            .auto_bounds(Vec2b::new(false, false))
            .set_margin_fraction(egui::vec2(0.0, 0.0))
            .custom_y_axes(vec![agg_y_axis("GiB")])
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
            plot_ui.set_auto_bounds(Vec2b::new(false, false));
        });
        paint_mem_stack(ui, &resp.transform, &centers, &tops);
        if cursor.show_legends {
            paint_mem_legend(ui, *resp.transform.frame(), series.usable_gib > 0.0);
        }

        if let Some(data_x) = cursor.panel(resp.response.rect, &resp.transform, true) {
            let at: [Option<f64>; 6] =
                std::array::from_fn(|k| interpolate_line(&band_points[k], data_x));
            if let Some(ceiling) = at[5] {
                cursor.add(
                    &resp.transform,
                    data_x,
                    ceiling,
                    format!("All: {ceiling:.2} GiB"),
                    Color32::from_gray(200),
                );
            }
            for k in (0..6).rev() {
                let Some(top) = at[k] else { continue };
                let below = if k == 0 {
                    0.0
                } else {
                    at[k - 1].unwrap_or(0.0)
                };
                let height = (top - below).max(0.0);
                let (label, color) = mem_band_style(analysis::MEM_STACK[k]);
                if analysis::MEM_STACK[k].is_none() && height <= 0.0 {
                    continue;
                }
                cursor.add(
                    &resp.transform,
                    data_x,
                    top,
                    format!("{label}: {height:.2} GiB"),
                    color,
                );
            }
        }
        cursor.last_plot_rect = Some(resp.response.rect);
    }

    /// Free pages against the pagedaemon free target / min / severe
    /// thresholds, on a scale that fits those lines.
    pub(crate) fn memory_pressure_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        let Some(agg) = self.aggregate.as_ref() else {
            return;
        };
        let series = &self.mem_series;
        if series.is_empty() {
            return;
        }
        let seed = self.agg_time_axis.seed(0.0, agg.time_max.max(1.0));
        self.agg_time_axis.apply(ui.ctx(), PRESSURE_PLOT_ID);

        let last_y = |data: &[[f64; 2]]| data.last().map_or(0.0, |p| p[1]);
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.strong(RichText::new("Memory Pressure").size(14.0));
            ui.add_space(16.0);
            ui.label(
                RichText::new(format!(
                    "Last: free {:.1} · target {:.1} · min {:.1} · severe {:.1} GiB",
                    last_y(&series.states[0]),
                    last_y(&series.free_target),
                    last_y(&series.free_min),
                    last_y(&series.free_severe)
                ))
                .size(12.0)
                .color(Color32::from_gray(180)),
            );
        });

        let lines: [(&str, Color32, bool, &[[f64; 2]]); 4] = [
            ("free", MEM_STATE_COLORS[0], false, &series.states[0]),
            (
                "free target",
                COLOR_MEM_FREE_TARGET,
                true,
                &series.free_target,
            ),
            ("free min", COLOR_MEM_FREE_MIN, true, &series.free_min),
            (
                "free severe",
                COLOR_MEM_FREE_SEVERE,
                true,
                &series.free_severe,
            ),
        ];
        let data_lo = series.states[0].first().map_or(0.0, |p| p[0]);
        let mut data_hi = series.last_t().unwrap_or(data_lo);
        if data_hi <= data_lo {
            data_hi = data_lo + 1.0;
        }
        let (x_lo, x_hi, _) = seed;
        let view = seed_view_x(x_lo, x_hi, data_lo, data_hi);
        let hidden = PlotMemory::load(ui.ctx(), egui::Id::new(PRESSURE_PLOT_ID))
            .map(|m| m.hidden_items)
            .unwrap_or_default();
        let mut y_peak = 0.0_f64;
        let mut drawn: Vec<(&str, Color32, bool, Vec<[f64; 2]>)> = Vec::with_capacity(lines.len());
        for (name, color, dashed, data) in lines {
            let ds = downsample_mean(data, view, MAX_DISPLAY_POINTS);
            if !hidden.contains(name) {
                y_peak = y_peak.max(step_peak_in_view(data, view));
            }
            drawn.push((name, color, dashed, ds));
        }
        let y_top = y_axis_top(y_peak);

        let mut plot = Plot::new(PRESSURE_PLOT_ID)
            .id(egui::Id::new(PRESSURE_PLOT_ID))
            .height(cursor.plot_h(ui))
            .include_x(x_lo)
            .include_x(x_hi)
            .include_y(0.0)
            .include_y(y_top)
            .auto_bounds(Vec2b::new(false, false))
            .set_margin_fraction(egui::vec2(0.0, 0.0))
            .custom_y_axes(vec![agg_y_axis("GiB")])
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
            plot_ui.set_auto_bounds(Vec2b::new(false, false));
            for (name, color, dashed, data) in &drawn {
                let mut line = Line::new(PlotPoints::from(data.clone()))
                    .color(*color)
                    .width(1.5_f32)
                    .name(*name);
                if *dashed {
                    line = line.style(egui_plot::LineStyle::dashed_loose());
                }
                plot_ui.line(line);
            }
        });

        if let Some(data_x) = cursor.panel(resp.response.rect, &resp.transform, true) {
            for (name, color, _, data) in &drawn {
                if hidden.contains(*name) {
                    continue;
                }
                if let Some(y) = interpolate_line(data, data_x) {
                    cursor.add(
                        &resp.transform,
                        data_x,
                        y,
                        format!("{name}: {y:.2} GiB"),
                        *color,
                    );
                }
            }
        }
        cursor.last_plot_rect = Some(resp.response.rect);
    }

    /// Swap in use (GiB), with the configured total in the title.
    pub(crate) fn swap_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        let Some(agg) = self.aggregate.as_ref() else {
            return;
        };
        let series = &self.mem_series;
        if !series.has_swap() {
            return;
        }
        let seed = self.agg_time_axis.seed(0.0, agg.time_max.max(1.0));
        self.agg_time_axis.apply(ui.ctx(), SWAP_PLOT_ID);
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.strong(RichText::new("Swap").size(14.0));
            ui.add_space(16.0);
            ui.label(
                RichText::new(format!("Total {:.1} GiB", series.swap_total_gib))
                    .size(12.0)
                    .color(Color32::from_gray(180)),
            );
        });
        let lines = [PlotLine {
            name: "used".to_string(),
            color: COLOR_MEM_SWAP,
            data: &series.swap_used,
        }];
        draw_series_plot(
            ui,
            SWAP_PLOT_ID,
            "GiB",
            cursor.plot_h(ui),
            &lines,
            cursor,
            seed,
        );
    }
}

pub(super) fn memory_help_popup_id() -> egui::Id {
    egui::Id::new("mem_states_help")
}

/// `?` next to the Memory title. Explains each stacked band.
fn show_memory_help_button(ui: &mut egui::Ui) {
    let help_id = memory_help_popup_id();
    let help_open = ui
        .memory(|m| m.data.get_temp::<bool>(help_id))
        .unwrap_or(false);
    let (help_rect, help_response) =
        super::state::tab_chrome_glyph_button(ui, "?", 13.0, help_open, "What the bands mean");
    if help_response.clicked() {
        let next = !help_open;
        ui.memory_mut(|m| m.data.insert_temp(help_id, next));
        if next {
            super::state::tab_chrome_close_others(ui, help_id);
        }
    }
    if !help_open {
        return;
    }

    const POPUP_W: f32 = 420.0;
    let area = egui::Area::new(help_id.with("area"))
        .order(egui::Order::Foreground)
        .fixed_pos(help_rect.left_bottom() + egui::vec2(0.0, 4.0))
        .pivot(egui::Align2::LEFT_TOP)
        .constrain_to(ui.ctx().screen_rect());
    let text_color = ui.visuals().text_color();
    let weak_color = ui.visuals().weak_text_color();
    let area_resp = area.show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(POPUP_W);
            ui.label(RichText::new("Memory").strong().size(12.0));
            ui.separator();
            mem_help_text(
                ui,
                "Page states, stacked bottom to top. The top edge is usable memory.",
                text_color,
                11.0,
                POPUP_W,
            );
            ui.add_space(6.0);
            let bands: [(&str, Color32, &str); 6] = [
                (
                    "Wired",
                    MEM_STATE_COLORS[4],
                    "Allocated and pinned, so they cannot be reclaimed.",
                ),
                (
                    "Active",
                    MEM_STATE_COLORS[3],
                    "Allocated and used recently.",
                ),
                (
                    "Inactive",
                    MEM_STATE_COLORS[1],
                    "Allocated, and not used recently. Reclaimed before Active.",
                ),
                (
                    "Laundry",
                    MEM_STATE_COLORS[2],
                    "Dirty pages queued for writeback, plus pages that cannot be swapped.",
                ),
                (
                    MEM_OTHER_LABEL,
                    COLOR_MEM_OTHER,
                    "Usable memory the five counters leave out. Most of it is pages that are neither wired nor on a page queue: held by the kernel, and not free. A smaller part is free pages in per-CPU caches, and pages held permanently. The capture does not split those apart.",
                ),
                (
                    "Free",
                    MEM_STATE_COLORS[0],
                    "On the free lists. Nothing owns these pages; the next allocation takes from here.",
                ),
            ];
            for (name, color, text) in bands {
                mem_help_text(ui, &format!("■ {name}"), color, 12.0, POPUP_W);
                mem_help_text(ui, text, text_color, 11.0, POPUP_W);
                ui.add_space(4.0);
            }
            mem_help_text(
                ui,
                "A wired page can also sit on a page queue, so the five states can add up past usable. Other then has no height. Free against the pagedaemon thresholds is the Memory Pressure plot.",
                weak_color,
                10.5,
                POPUP_W,
            );
        });
    });

    let popup_rect = area_resp.response.rect;
    if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.memory_mut(|m| m.data.insert_temp(help_id, false));
    }
    if ui.ctx().input(|i| i.pointer.any_click()) {
        if let Some(pos) = ui.ctx().input(|i| i.pointer.interact_pos()) {
            if !help_rect.contains(pos) && !popup_rect.contains(pos) {
                ui.memory_mut(|m| m.data.insert_temp(help_id, false));
            }
        }
    }
}

/// One wrapped line in the Memory help popup. Width is explicit
/// because an `Area` gives labels an unbounded wrap width.
fn mem_help_text(ui: &mut egui::Ui, text: &str, color: Color32, size: f32, width: f32) {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(size),
            color,
            ..Default::default()
        },
    );
    let galley = ui.fonts(|f| f.layout_job(job));
    let (rect, _) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    ui.painter().galley(rect.left_top(), galley, color);
}

fn mem_band_style(band: Option<usize>) -> (&'static str, Color32) {
    match band {
        Some(k) => (analysis::MEM_STATE_LABELS[k], MEM_STATE_COLORS[k]),
        None => (MEM_OTHER_LABEL, COLOR_MEM_OTHER),
    }
}

/// Legend in the plot's lower right, top entry matching the top band.
fn paint_mem_legend(ui: &mut egui::Ui, plot_rect: egui::Rect, show_other: bool) {
    let entries: Vec<(&str, Color32)> = analysis::MEM_STACK
        .iter()
        .rev()
        .filter_map(|band| {
            if band.is_none() && !show_other {
                None
            } else {
                Some(mem_band_style(*band))
            }
        })
        .collect();
    if entries.is_empty() {
        return;
    }
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let galleys: Vec<_> = entries
        .iter()
        .map(|(name, _)| {
            ui.fonts(|f| f.layout_no_wrap((*name).to_string(), font_id.clone(), Color32::WHITE))
        })
        .collect();
    let row_h = galleys.iter().map(|g| g.size().y).fold(0.0_f32, f32::max);
    let text_w = galleys.iter().map(|g| g.size().x).fold(0.0_f32, f32::max);
    let icon = row_h;
    let icon_gap = icon / 5.0;
    let margin = egui::vec2(8.0, 4.0);
    let box_w = margin.x * 2.0 + text_w + icon_gap + icon;
    let box_h = margin.y * 2.0 + row_h * entries.len() as f32;
    let inset = 4.0;
    let legend = egui::Rect::from_min_size(
        egui::pos2(
            plot_rect.right() - inset - box_w,
            plot_rect.bottom() - inset - box_h,
        ),
        egui::vec2(box_w, box_h),
    );
    let painter = ui.painter().with_clip_rect(plot_rect);
    painter.rect(
        legend,
        ui.visuals().window_rounding,
        ui.visuals().extreme_bg_color.gamma_multiply(0.75),
        ui.visuals().window_stroke(),
    );
    let text_color = ui.visuals().text_color();
    for (i, ((_, color), galley)) in entries.iter().zip(galleys).enumerate() {
        let y = legend.top() + margin.y + row_h * i as f32 + row_h * 0.5;
        let cx = legend.right() - margin.x - icon * 0.5;
        painter.circle_filled(egui::pos2(cx, y), icon * 0.4, *color);
        painter.galley(
            egui::pos2(legend.left() + margin.x, y - galley.size().y * 0.5),
            galley,
            text_color,
        );
    }
}

/// Opaque band between each stack slot's cumulative top and the band below.
fn paint_mem_stack(
    ui: &mut egui::Ui,
    transform: &egui_plot::PlotTransform,
    centers: &[f64],
    tops: &[Vec<f64>; 6],
) {
    if centers.len() < 2 {
        return;
    }
    let clip = transform.frame().intersect(ui.clip_rect());
    let painter = ui.painter().with_clip_rect(clip);
    let pt = |x: f64, y: f64| transform.position_from_point(&PlotPoint { x, y });
    for k in 0..6 {
        let (_, color) = mem_band_style(analysis::MEM_STACK[k]);
        let color = mem_band_fill(color);
        let mut mesh = egui::Mesh::default();
        for i in 0..centers.len() - 1 {
            let (x0, x1) = (centers[i], centers[i + 1]);
            let (top0, top1) = (tops[k][i], tops[k][i + 1]);
            let (bot0, bot1) = if k == 0 {
                (0.0, 0.0)
            } else {
                (tops[k - 1][i], tops[k - 1][i + 1])
            };
            if top0 <= bot0 && top1 <= bot1 {
                continue;
            }
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
}
