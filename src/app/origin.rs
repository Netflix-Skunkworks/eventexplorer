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

fn origin_file_summary(prof: &analysis::OriginSegmentsProfile) -> String {
    if prof.files.is_empty() {
        return format!(
            "{} ({} in window)",
            prof.file_name,
            format_count(prof.overlapping_segments as u64)
        );
    }
    prof.files
        .iter()
        .map(|file| {
            if file.overlapping_segments == 0 {
                format!("{} (no overlap)", file.file_name)
            } else {
                format!(
                    "{} ({} in window)",
                    file.file_name,
                    format_count(file.overlapping_segments as u64)
                )
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum OriginSegmentViewMode {
    #[default]
    RateMbps,
    SizeMb,
}

impl EventExplorerApp {
    pub(crate) fn origin_segments_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        let Some(prof) = self.origin_segments.as_ref() else {
            return;
        };
        if prof.is_empty() {
            ui.add_space(6.0);
            ui.strong(RichText::new("Origin Segment Rate").size(14.0));
            let weak = ui.visuals().weak_text_color();
            ui.label(
                RichText::new(format!(
                    "Capture {} – {}.",
                    format_utc_us(prof.capture_start_utc_us),
                    format_utc_us(prof.capture_end_utc_us),
                ))
                .size(12.0)
                .color(weak),
            );
            for file in &prof.files {
                ui.label(
                    RichText::new(format!(
                        "{} has no segments in this capture. Segments {} – {}.",
                        file.file_name,
                        format_utc_us(file.first_utc_us),
                        format_utc_us(file.last_utc_us),
                    ))
                    .size(12.0)
                    .color(weak),
                );
            }
            return;
        }

        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.agg_time_axis.apply(ui.ctx(), "origin_segments_plot");

        ui.add_space(6.0);
        let mut unload = false;
        ui.horizontal(|ui| {
            ui.strong(RichText::new("Origin Segment Rate").size(14.0));
            ui.label(
                RichText::new(format!(
                    "{} (avg: enc1 {:.2} Mbps, enc2 {:.2} Mbps)",
                    origin_file_summary(prof),
                    prof.enc1_avg_mbps,
                    prof.enc2_avg_mbps,
                ))
                .size(12.0)
                .color(ui.visuals().weak_text_color()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("✕")
                    .on_hover_text("Unload origin segments")
                    .clicked()
                {
                    unload = true;
                }
                ui.selectable_value(
                    &mut self.origin_segments_view,
                    OriginSegmentViewMode::SizeMb,
                    "Size (MB)",
                );
                ui.selectable_value(
                    &mut self.origin_segments_view,
                    OriginSegmentViewMode::RateMbps,
                    "Rate (Mbps)",
                );
            });
        });

        if unload {
            self.origin_segments = None;
            self.raw_origin_segments.clear();
            return;
        }

        let (lines, unit) = match self.origin_segments_view {
            OriginSegmentViewMode::RateMbps => {
                let mut lines = Vec::new();
                if !prof.enc1_rate_mbps.is_empty() {
                    lines.push(PlotLine {
                        name: "enc1".to_string(),
                        color: series_color(0),
                        data: &prof.enc1_rate_mbps,
                    });
                }
                if !prof.enc2_rate_mbps.is_empty() {
                    lines.push(PlotLine {
                        name: "enc2".to_string(),
                        color: series_color(1),
                        data: &prof.enc2_rate_mbps,
                    });
                }
                (lines, "Mbps")
            }
            OriginSegmentViewMode::SizeMb => {
                let mut lines = Vec::new();
                if !prof.enc1_size_mb.is_empty() {
                    lines.push(PlotLine {
                        name: "enc1".to_string(),
                        color: series_color(0),
                        data: &prof.enc1_size_mb,
                    });
                }
                if !prof.enc2_size_mb.is_empty() {
                    lines.push(PlotLine {
                        name: "enc2".to_string(),
                        color: series_color(1),
                        data: &prof.enc2_size_mb,
                    });
                }
                (lines, "MB")
            }
        };

        draw_series_plot(
            ui,
            "origin_segments_plot",
            unit,
            cursor.panel_h,
            &lines,
            cursor,
            seed,
        );
    }
}
