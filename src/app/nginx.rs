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

//! nginx System-tab panels and the per-session HTTP request table.

#[allow(unused_imports)]
use super::*;

/// System-tab nginx view state. Built lazily on first render; cleared on
/// file load.
pub struct NginxSamplesViewState {
    pub(crate) agg: analysis::NginxAggData,
    /// First draw of HTTP responses/sec hides TCP.
    pub(crate) rps_legend_init: bool,
    pub(crate) settle_frames: u8,
}

/// Legend name and color for an nginx request lane. Class labels and
/// palette slots match System HTTP RPS (`2xx` blue, `4xx` orange, …).
pub(crate) fn nginx_status_legend(status: Option<u16>) -> (&'static str, Color32) {
    match status.and_then(analysis::NginxStatusClass::of) {
        Some(c) => {
            let i = match c {
                analysis::NginxStatusClass::Success => 0,
                analysis::NginxStatusClass::ClientError => 1,
                analysis::NginxStatusClass::ServerError => 2,
                analysis::NginxStatusClass::Redirect => 3,
                analysis::NginxStatusClass::Informational => 4,
            };
            (c.label(), series_color(i))
        }
        None => ("open", COLOR_NGINX_REQ_OPEN),
    }
}

impl EventExplorerApp {
    pub(crate) fn ensure_nginx_samples_view(&mut self, ui: &mut egui::Ui) {
        if self.nginx_samples_view.is_none() {
            let t0 = self
                .file_info
                .as_ref()
                .map(|f| f.capture_start)
                .unwrap_or(0);
            self.nginx_samples_view = Some(NginxSamplesViewState {
                agg: analysis::build_nginx_agg(&self.nginx_profile, t0),
                rps_legend_init: true,
                settle_frames: SETTLE_FRAMES,
            });
        }
        if let Some(state) = self.nginx_samples_view.as_mut() {
            if state.settle_frames > 0 {
                state.settle_frames -= 1;
                ui.ctx().request_repaint();
            }
        }
    }

    /// HTTP completion `bytes_sent` as Gb/s (catalog-hidden by default).
    /// Completed-request rates live on the HTTP Requests plot.
    pub(crate) fn nginx_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        if self.nginx_profile.is_empty() {
            return;
        }
        self.ensure_nginx_samples_view(ui);
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.agg_time_axis.apply(ui.ctx(), "nginx_bps_plot");

        let agg = &self.nginx_samples_view.as_ref().unwrap().agg;
        if agg.completions.is_empty() && agg.samples.is_empty() {
            return;
        }
        let first_ts = agg
            .samples
            .first()
            .map(|s| s.ts_s)
            .or_else(|| agg.completions.first().map(|c| c.ts_s));
        let last_ts = agg
            .samples
            .last()
            .map(|s| s.ts_s)
            .or_else(|| agg.completions.last().map(|c| c.ts_s));
        let (Some(data_lo), Some(mut data_hi)) = (first_ts, last_ts) else {
            return;
        };
        if data_hi <= data_lo {
            data_hi = data_lo + 1.0;
        }
        let [vx0, vx1] = seed_view_x(seed.0, seed.1, data_lo, data_hi);
        let gbps = analysis::nginx_bytes_gbps_bins(agg, vx0, vx1, cursor.plot_bins);
        if gbps.iter().all(|p| p[1] == 0.0) {
            return;
        }

        ui.add_space(6.0);
        ui.strong(RichText::new("HTTP Completion Bytes").size(14.0));
        let lines = [PlotLine {
            name: "completed".to_string(),
            color: series_color(1),
            data: &gbps,
        }];
        draw_series_plot(
            ui,
            "nginx_bps_plot",
            "Gb/s",
            cursor.panel_h,
            &lines,
            cursor,
            seed,
        );
    }

    /// Mean START→DONE duration for completed nginx responses, equal-time
    /// buckets over the visible window (same count as HTTP responses / Completion Bytes).
    pub(crate) fn http_latency_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        if self.nginx_profile.is_empty() {
            return;
        }
        self.ensure_nginx_samples_view(ui);
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.agg_time_axis.apply(ui.ctx(), "http_latency_plot");

        let agg = &self.nginx_samples_view.as_ref().unwrap().agg;
        if agg.latencies.is_empty() {
            ui.add_space(6.0);
            ui.strong(RichText::new("HTTP Duration").size(14.0));
            ui.label(
                RichText::new("No completed nginx requests in this capture.")
                    .size(12.0)
                    .color(Color32::from_gray(180)),
            );
            return;
        }
        let data_lo = agg.latencies.first().map(|s| s.ts_s).unwrap_or(0.0);
        let mut data_hi = agg.latencies.last().map(|s| s.ts_s).unwrap_or(1.0);
        if data_hi <= data_lo {
            data_hi = data_lo + 1.0;
        }
        let [vx0, vx1] = seed_view_x(seed.0, seed.1, data_lo, data_hi);
        let series = analysis::nginx_latency_mean_bins(agg, vx0, vx1, cursor.plot_bins);
        let mut n_small = 0u64;
        let mut sum_small = 0.0;
        let mut n_rest = 0u64;
        let mut sum_rest = 0.0;
        for s in &agg.latencies {
            if s.ts_s < vx0 || s.ts_s > vx1 {
                continue;
            }
            let ms = s.latency_s * 1000.0;
            if s.bytes_sent
                .is_some_and(|b| b < analysis::NGINX_SMALL_RESP_BYTES)
            {
                n_small += 1;
                sum_small += ms;
            } else {
                n_rest += 1;
                sum_rest += ms;
            }
        }
        let fmt_mean = |n: u64, sum: f64| {
            if n == 0 {
                None
            } else {
                Some((sum / n as f64, n))
            }
        };

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.strong(RichText::new("HTTP Duration").size(14.0));
            let mut parts = Vec::new();
            if let Some((m, n)) = fmt_mean(n_small, sum_small) {
                parts.push(format!("Small (< 5K) {m:.1} ms (n={n})"));
            }
            if let Some((m, n)) = fmt_mean(n_rest, sum_rest) {
                parts.push(format!("Large (≥ 5K) {m:.1} ms (n={n})"));
            }
            if !parts.is_empty() {
                ui.add_space(12.0);
                ui.label(
                    RichText::new(parts.join("  ·  "))
                        .size(12.0)
                        .color(Color32::from_gray(180)),
                );
            }
        });
        if series.iter().all(|s| s.data.iter().all(|p| p[1] == 0.0)) {
            return;
        }
        let lines: Vec<PlotLine> = series
            .iter()
            .enumerate()
            .map(|(i, s)| PlotLine {
                name: s.label.clone(),
                color: series_color(i),
                data: &s.data,
            })
            .collect();
        draw_series_plot(
            ui,
            "http_latency_plot",
            "ms",
            cursor.panel_h,
            &lines,
            cursor,
            seed,
        );
    }
}

impl EventExplorerApp {
    /// nginx problem events/sec by kind (worker stalls, blocking aio
    /// reads, sendfile EBUSY/errors, open failures, TLS and upstream
    /// failures). Shown only when the capture has any.
    pub(crate) fn nginx_diag_section(&mut self, ui: &mut egui::Ui, cursor: &mut SysCursor) {
        if !self.nginx_has_diag {
            return;
        }
        self.ensure_nginx_samples_view(ui);
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.agg_time_axis.apply(ui.ctx(), "nginx_diag_plot");

        let agg = &self.nginx_samples_view.as_ref().unwrap().agg;
        let (Some(first), Some(last)) = (agg.diag.first(), agg.diag.last()) else {
            return;
        };
        let data_lo = first.ts_s;
        let data_hi = last.ts_s.max(data_lo + 1.0);
        let [vx0, vx1] = seed_view_x(seed.0, seed.1, data_lo, data_hi);
        let series = analysis::nginx_diag_bins(agg, vx0, vx1, cursor.plot_bins);
        let counts: Vec<String> = analysis::NginxDiagKind::ALL
            .iter()
            .filter_map(|&k| {
                let n = agg
                    .diag
                    .iter()
                    .filter(|e| e.kind == k && e.ts_s >= vx0 && e.ts_s <= vx1)
                    .count();
                (n > 0).then(|| format!("{} {n}", k.label()))
            })
            .collect();

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.strong(RichText::new("nginx Problem Events").size(14.0));
            if !counts.is_empty() {
                ui.add_space(12.0);
                ui.label(
                    RichText::new(counts.join("  ·  "))
                        .size(12.0)
                        .color(Color32::from_gray(180)),
                );
            }
        });
        // Colors follow the fixed kind order so a kind keeps its color
        // as others enter and leave the window.
        let lines: Vec<PlotLine> = series
            .iter()
            .map(|(kind, data)| PlotLine {
                name: kind.label().to_string(),
                color: series_color(
                    analysis::NginxDiagKind::ALL
                        .iter()
                        .position(|k| k == kind)
                        .unwrap_or(0),
                ),
                data,
            })
            .collect();
        draw_series_plot(
            ui,
            "nginx_diag_plot",
            "events/s",
            cursor.panel_h,
            &lines,
            cursor,
            seed,
        );
    }
}
