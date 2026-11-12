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

//! Left-side plot catalog: hide/show, reorder, Apply.

#[allow(unused_imports)]
use super::*;

fn paint_visibility_icon(ui: &egui::Ui, rect: egui::Rect, shown: bool, hovered: bool) {
    let color = if shown {
        if hovered {
            Color32::from_rgb(150, 210, 150)
        } else {
            Color32::from_rgb(120, 180, 120)
        }
    } else if hovered {
        Color32::from_gray(180)
    } else {
        Color32::from_gray(120)
    };
    let c = rect.center();
    let w = 7.0_f32;
    let h = 4.0_f32;
    let painter = ui.painter();
    painter.line_segment(
        [egui::pos2(c.x - w, c.y), egui::pos2(c.x, c.y - h)],
        Stroke::new(1.3_f32, color),
    );
    painter.line_segment(
        [egui::pos2(c.x, c.y - h), egui::pos2(c.x + w, c.y)],
        Stroke::new(1.3_f32, color),
    );
    painter.line_segment(
        [egui::pos2(c.x + w, c.y), egui::pos2(c.x, c.y + h)],
        Stroke::new(1.3_f32, color),
    );
    painter.line_segment(
        [egui::pos2(c.x, c.y + h), egui::pos2(c.x - w, c.y)],
        Stroke::new(1.3_f32, color),
    );
    painter.circle_filled(c, 1.6, color);
    if !shown {
        painter.line_segment(
            [egui::pos2(c.x - w, c.y + h), egui::pos2(c.x + w, c.y - h)],
            Stroke::new(1.4_f32, color),
        );
    }
}

fn paint_lock_icon(ui: &egui::Ui, rect: egui::Rect, locked: bool, enabled: bool, hovered: bool) {
    let color = if !enabled {
        Color32::from_gray(70)
    } else if hovered {
        Color32::from_gray(210)
    } else {
        Color32::from_gray(180)
    };
    let c = rect.center();
    let painter = ui.painter();
    let stroke = Stroke::new(1.7_f32, color);
    let body_w = 9.0_f32;
    let body_h = 7.5_f32;
    let cx = c.x + if locked { 0.0 } else { 2.0 };
    let body_top = c.y + 1.4;
    let body = egui::Rect::from_min_size(
        egui::pos2(cx - body_w * 0.5, body_top),
        egui::vec2(body_w, body_h),
    );
    let r = 3.8_f32;
    let hinge = egui::pos2(cx - r, body_top);
    let post = 1.5_f32;
    let theta = if locked {
        0.0
    } else {
        std::f32::consts::FRAC_PI_2
    };
    let (ct, st) = (theta.cos(), theta.sin());
    let map =
        |dx: f32, dy: f32| egui::pos2(hinge.x + dx * ct + dy * st, hinge.y - dx * st + dy * ct);
    let n = 14;
    let mut prev: Option<egui::Pos2> = None;
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let a = std::f32::consts::PI * (1.0 - t);
        let p = map(r + r * a.cos(), -r * a.sin());
        if let Some(q) = prev {
            painter.line_segment([q, p], stroke);
        }
        prev = Some(p);
    }
    painter.line_segment([hinge, egui::pos2(hinge.x, body_top + post)], stroke);
    let right = map(2.0 * r, 0.0);
    let right_end = map(2.0 * r, post);
    painter.line_segment([right, right_end], stroke);
    if !locked {
        painter.circle_filled(right_end, 1.15, color);
    }
    painter.rect_filled(body, 1.4, ui.visuals().panel_fill);
    painter.rect_stroke(body, 1.4, stroke);
    let hole = egui::pos2(cx, body.center().y - 0.6);
    painter.circle_filled(hole, 1.35, color);
    painter.line_segment(
        [
            egui::pos2(cx, hole.y + 1.1),
            egui::pos2(cx, body.bottom() - 1.4),
        ],
        Stroke::new(1.5_f32, color),
    );
}

impl EventExplorerApp {
    pub(crate) fn active_plot_catalog_kind(&self) -> Option<PlotCatalogKind> {
        match self.active_tab {
            Tab::Aggregate => Some(PlotCatalogKind::System),
            Tab::Session(_) => Some(PlotCatalogKind::Session),
            Tab::EllaChannel { .. } => Some(PlotCatalogKind::EllaChannel),
            Tab::CpuFlame => Some(PlotCatalogKind::Flame),
            Tab::Processes => Some(PlotCatalogKind::Processes),
            _ => None,
        }
    }

    fn ella_catalog_has_data(&self, role: EllaNodeRole) -> bool {
        if !self.ella_profile.has_channel_counts()
            && !self.ella_has_frame_timing
            && self.ella_profile.channels.is_empty()
        {
            return false;
        }
        self.ella_fanout_ids
            .iter()
            .any(|&pid| ella_catalog_role_matches(role, self.ella_profile.node_role(pid)))
    }

    /// Exact per-row check once the System view exists; before that,
    /// role-level data plus frame timing for the frame plots.
    fn ella_fanout_row_has_data(&self, id: PlotId) -> bool {
        let Some((server, kind)) = id.fanout_split() else {
            return false;
        };
        let role = if server {
            EllaNodeRole::Server
        } else {
            EllaNodeRole::Relay
        };
        if let Some(view) = &self.ella_samples_view {
            return view
                .nodes
                .iter()
                .any(|n| ella_catalog_role_matches(role, n.role) && kind.node_has_data(n));
        }
        if !self.ella_catalog_has_data(role) {
            return false;
        }
        match kind {
            FanoutPlot::FrameRate | FanoutPlot::FrameDuration | FanoutPlot::FrameRatio => {
                self.ella_has_frame_timing
            }
            _ => true,
        }
    }

    /// A TCP_RPS_SAMPLE rate series, or nginx events.
    fn http_rps_has_plot_data(&self) -> bool {
        let t0 = self
            .file_info
            .as_ref()
            .map(|f| f.capture_start)
            .unwrap_or(0);
        !analysis::build_tcp_rps_series(&self.tcp_samples, t0).is_empty()
            || !self.nginx_profile.is_empty()
    }

    pub(crate) fn plot_has_capture_data(&self, id: PlotId) -> bool {
        match id {
            PlotId::CpuUsage => {
                if let Tab::Session(sid) = self.active_tab {
                    return self
                        .open_sessions
                        .iter()
                        .find(|pv| pv.summary.session_id == sid)
                        .is_some_and(|pv| pv.cpu_panel.is_some());
                }
                self.aggregate
                    .as_ref()
                    .map(|a| a.cpu_panel.is_some())
                    .unwrap_or(false)
            }
            PlotId::SessionCount => self.aggregate.as_ref().is_some_and(|a| {
                !a.session_count_paced.is_empty() || !a.session_count_unpaced.is_empty()
            }),
            PlotId::SrttAvg => self
                .aggregate
                .as_ref()
                .is_some_and(|a| !a.srtt_paced_cdf.is_empty() || !a.srtt_unpaced_cdf.is_empty()),
            PlotId::GoodputAvg => self.aggregate.as_ref().is_some_and(|a| {
                !a.goodput_paced_cdf.is_empty() || !a.goodput_unpaced_cdf.is_empty()
            }),
            PlotId::NetworkTx | PlotId::NetworkRx => !self.if_profile.is_empty(),
            PlotId::Memory | PlotId::MemoryPressure => !self.mem_series.is_empty(),
            PlotId::Swap => self.mem_series.has_swap(),
            PlotId::HttpRps => self.http_rps_has_plot_data(),
            PlotId::HttpLatency => !self.nginx_profile.is_empty(),
            PlotId::TcpStates | PlotId::ConnRate => !self.tcp_samples.state_samples.is_empty(),
            PlotId::TcpGoodput => {
                !self.tcp_samples.gp_samples.is_empty()
                    || !self.tcp_samples.pacing_rate_samples.is_empty()
                    || !self.tcp_samples.realized_rate_samples.is_empty()
            }
            PlotId::TcpSrtt => !self.tcp_samples.srtt_samples.is_empty(),
            PlotId::TcpSend => !self.tcp_samples.send_samples.is_empty(),
            PlotId::TcpLro => !self.tcp_samples.lro_flush_samples.is_empty(),
            PlotId::FanoutServerTx
            | PlotId::FanoutServerRx
            | PlotId::FanoutServerTraffic
            | PlotId::FanoutServerJoins
            | PlotId::FanoutServerFrameRate
            | PlotId::FanoutServerFrameDuration
            | PlotId::FanoutServerFrameRatio
            | PlotId::FanoutServerObjectTiming
            | PlotId::FanoutServerErrors
            | PlotId::FanoutRelaySubs
            | PlotId::FanoutRelayRx
            | PlotId::FanoutRelayTx
            | PlotId::FanoutRelayTraffic
            | PlotId::FanoutRelayJoins
            | PlotId::FanoutRelayFrameRate
            | PlotId::FanoutRelayFrameDuration
            | PlotId::FanoutRelayFrameRatio
            | PlotId::FanoutRelayChannels
            | PlotId::FanoutRelayErrors => self.ella_fanout_row_has_data(id),
            PlotId::NginxBytes => !self.nginx_profile.is_empty(),
            PlotId::NginxDiag => self.nginx_has_diag,
            PlotId::OriginSegments => self.origin_segments.is_some(),
            PlotId::Stars => !self.stars_agg_samples.is_empty(),
            PlotId::HptsLateness | PlotId::HptsBatch => !self.hpts_wheel_events.is_empty(),
            PlotId::HptsSlotGateOutput
            | PlotId::HptsSlotGateFires
            | PlotId::HptsSlotGateChecks
            | PlotId::HptsSlotGateRollover => self.hpts_has_slot_gate,
            PlotId::Rtt | PlotId::Rate | PlotId::SendState | PlotId::Sizes | PlotId::SizeCdf => {
                let Tab::Session(sid) = self.active_tab else {
                    return true;
                };
                self.open_sessions
                    .iter()
                    .find(|pv| pv.summary.session_id == sid)
                    .and_then(|pv| id.session_index().map(|i| pv.plot_has_data()[i]))
                    .unwrap_or(false)
            }
            PlotId::HttpReqLanes => {
                let Tab::Session(sid) = self.active_tab else {
                    return false;
                };
                self.open_sessions
                    .iter()
                    .find(|pv| pv.summary.session_id == sid)
                    .is_some_and(|pv| pv.has_http_req_data())
            }
            PlotId::HttpRespSize => {
                let Tab::Session(sid) = self.active_tab else {
                    return false;
                };
                self.open_sessions
                    .iter()
                    .find(|pv| pv.summary.session_id == sid)
                    .is_some_and(|pv| pv.has_http_resp_size_data())
            }
            PlotId::HttpRespLatency => {
                let Tab::Session(sid) = self.active_tab else {
                    return false;
                };
                self.open_sessions
                    .iter()
                    .find(|pv| pv.summary.session_id == sid)
                    .is_some_and(|pv| pv.has_http_latency_data())
            }
            PlotId::EllaSubscribers
            | PlotId::EllaObjectRate
            | PlotId::EllaObjectTiming
            | PlotId::EllaObjectId
            | PlotId::EllaFrameId
            | PlotId::EllaObjectDuration
            | PlotId::EllaObjectSize
            | PlotId::EllaFrameRate
            | PlotId::EllaFrameRateRelay
            | PlotId::EllaFrameDuration
            | PlotId::EllaFrameDurationRelay
            | PlotId::EllaFrameRatio
            | PlotId::EllaFrameRatioRelay
            | PlotId::EllaErrors => {
                let Tab::EllaChannel {
                    provider_id,
                    session_id,
                } = self.active_tab
                else {
                    return true;
                };
                let Some(ch) = self
                    .open_ella_channels
                    .iter()
                    .find(|c| c.provider_id == provider_id && c.session_id == session_id)
                else {
                    return false;
                };
                if ch.pending.is_some() {
                    return true;
                }
                match id {
                    PlotId::EllaSubscribers => !ch.data.subscribers.is_empty(),
                    PlotId::EllaObjectRate => !ch.data.object_rate.is_empty(),
                    PlotId::EllaObjectTiming => !ch.data.object_timing.is_empty(),
                    PlotId::EllaObjectId => !ch.data.object_spans.is_empty(),
                    PlotId::EllaFrameId => !ch.data.frame_spans.is_empty(),
                    PlotId::EllaObjectDuration => !ch.data.object_duration.is_empty(),
                    PlotId::EllaObjectSize => !ch.data.object_size.is_empty(),
                    PlotId::EllaFrameRate => ch
                        .data
                        .frame_rate
                        .iter()
                        .any(|s| !ella_frame_label_is_relay(&s.label)),
                    PlotId::EllaFrameRateRelay => ch
                        .data
                        .frame_rate
                        .iter()
                        .any(|s| ella_frame_label_is_relay(&s.label)),
                    PlotId::EllaFrameDuration => ch
                        .data
                        .frame_duration
                        .iter()
                        .any(|s| !ella_frame_label_is_relay(&s.label)),
                    PlotId::EllaFrameDurationRelay => ch
                        .data
                        .frame_duration
                        .iter()
                        .any(|s| ella_frame_label_is_relay(&s.label)),
                    PlotId::EllaFrameRatio => ch
                        .data
                        .frame_ratio
                        .iter()
                        .any(|s| !ella_frame_label_is_relay(&s.label)),
                    PlotId::EllaFrameRatioRelay => ch
                        .data
                        .frame_ratio
                        .iter()
                        .any(|s| ella_frame_label_is_relay(&s.label)),
                    PlotId::EllaErrors => !ch.data.errors.is_empty(),
                    _ => false,
                }
            }
        }
    }

    fn with_current_layouts<R>(
        &mut self,
        f: impl FnOnce(&mut PlotLayout, &mut PlotLayout) -> R,
    ) -> Option<R> {
        match self.active_tab {
            Tab::Aggregate => Some(f(&mut self.system_layout, &mut self.system_draft)),
            Tab::Session(sid) => self
                .open_sessions
                .iter_mut()
                .find(|pv| pv.summary.session_id == sid)
                .map(|pv| f(&mut pv.plot_layout, &mut pv.plot_draft)),
            Tab::EllaChannel {
                provider_id,
                session_id,
            } => self
                .open_ella_channels
                .iter_mut()
                .find(|c| c.provider_id == provider_id && c.session_id == session_id)
                .map(|c| f(&mut c.plot_layout, &mut c.plot_draft)),
            Tab::CpuFlame => Some(f(&mut self.flame_layout, &mut self.flame_draft)),
            Tab::Processes => Some(f(&mut self.processes_layout, &mut self.processes_draft)),
            _ => None,
        }
    }

    pub(crate) fn show_plot_sidebar(&mut self, ctx: &egui::Context) {
        if !self.plot_sidebar_open {
            return;
        }
        egui::SidePanel::left("plot_catalog")
            .resizable(true)
            .default_width(240.0)
            .min_width(180.0)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                let kind = self.active_plot_catalog_kind();
                let heading = match kind {
                    Some(PlotCatalogKind::System) => "System plots",
                    Some(PlotCatalogKind::Session) => "Session plots",
                    Some(PlotCatalogKind::EllaChannel) => "Fanout plots",
                    Some(PlotCatalogKind::Flame) => "Flame plots",
                    Some(PlotCatalogKind::Processes) => "Process plots",
                    None => "Plots",
                };
                ui.strong(RichText::new(heading).size(14.0));
                ui.add_space(4.0);
                let Some(kind) = kind else {
                    ui.label(
                        RichText::new("This tab has no configurable plots.")
                            .size(12.0)
                            .color(Color32::from_gray(160)),
                    );
                    return;
                };

                let dirty = self
                    .with_current_layouts(|applied, draft| applied != draft)
                    .unwrap_or(false);

                ui.label(
                    RichText::new(
                        "Hide, show, lock to the top, or drag to reorder. Apply updates this tab.",
                    )
                    .size(11.0)
                    .color(Color32::from_gray(150)),
                );
                ui.add_space(6.0);

                let draft_order = self
                    .with_current_layouts(|_, draft| draft.order.clone())
                    .unwrap_or_default();
                let draft_hidden = self
                    .with_current_layouts(|_, draft| draft.hidden.clone())
                    .unwrap_or_default();
                let draft_locked = self
                    .with_current_layouts(|_, draft| draft.locked.clone())
                    .unwrap_or_default();
                let mut toggle: Option<PlotId> = None;
                let mut lock_toggle: Option<PlotId> = None;
                let mut drop_at: Option<(PlotId, usize)> = None;
                let pointer_released = ui.input(|i| i.pointer.primary_released());
                let pointer_pos = ui.input(|i| i.pointer.interact_pos());

                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let apply = ui.add_enabled(dirty, egui::Button::new("Apply"));
                        if apply.clicked() {
                            self.with_current_layouts(|applied, draft| {
                                *applied = draft.clone();
                            });
                        }
                        if ui.button("Reset").clicked() {
                            let def = default_layout(kind);
                            self.with_current_layouts(|applied, draft| {
                                *applied = def.clone();
                                *draft = def;
                            });
                        }
                    });
                    ui.add_space(2.0);
                    ui.checkbox(
                        &mut self.plot_catalog_show_empty,
                        RichText::new("Show plots with no data").size(12.0),
                    )
                    .on_hover_text(
                        "Plots that have nothing to draw in this capture stay hidden unless this is checked.",
                    );
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.with_layout(
                        egui::Layout::top_down(egui::Align::Min).with_cross_justify(true),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_source("plot_catalog_rows")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    let row_h = ui.spacing().interact_size.y.max(22.0);
                                    for (i, &id) in draft_order.iter().enumerate() {
                                        let has_data = self.plot_has_capture_data(id);
                                        if !has_data && !self.plot_catalog_show_empty {
                                            continue;
                                        }
                                        let shown = !draft_hidden.contains(&id);
                                        let dragging = self.plot_sidebar_drag == Some(id);
                                        let (row_rect, _) = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h),
                                            egui::Sense::hover(),
                                        );
                                        let handle_w = 22.0;
                                        let eye_w = 22.0;
                                        let lock_w = 26.0;
                                        let handle_rect = egui::Rect::from_min_size(
                                            row_rect.min,
                                            egui::vec2(handle_w, row_h),
                                        );
                                        let eye_rect = egui::Rect::from_min_size(
                                            egui::pos2(row_rect.min.x + handle_w, row_rect.min.y),
                                            egui::vec2(eye_w, row_h),
                                        );
                                        let lock_rect = egui::Rect::from_min_size(
                                            egui::pos2(row_rect.max.x - lock_w, row_rect.min.y),
                                            egui::vec2(lock_w, row_h),
                                        );
                                        let name_rect = egui::Rect::from_min_max(
                                            egui::pos2(eye_rect.max.x + 4.0, row_rect.min.y),
                                            egui::pos2(lock_rect.min.x, row_rect.max.y),
                                        );
                                        let row_id = ui.id().with(("plot_cat_row", id));

                                        if dragging {
                                            ui.painter().rect_filled(
                                                row_rect,
                                                2.0,
                                                Color32::from_rgba_unmultiplied(70, 110, 160, 50),
                                            );
                                        } else if self.plot_sidebar_drag.is_some()
                                            && pointer_pos.is_some_and(|p| row_rect.contains(p))
                                        {
                                            ui.painter().rect_filled(
                                                row_rect,
                                                2.0,
                                                Color32::from_rgba_unmultiplied(70, 110, 160, 40),
                                            );
                                        }

                                        let handle = ui
                                            .interact(
                                                handle_rect,
                                                row_id.with("handle"),
                                                egui::Sense::drag(),
                                            )
                                            .on_hover_cursor(egui::CursorIcon::Grab);
                                        let name_hit = ui
                                            .interact(
                                                name_rect,
                                                row_id.with("name"),
                                                egui::Sense::drag(),
                                            )
                                            .on_hover_cursor(egui::CursorIcon::Grab);
                                        if handle.dragged() || name_hit.dragged() || dragging {
                                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                                        }
                                        if handle.drag_started() || name_hit.drag_started() {
                                            self.plot_sidebar_drag = Some(id);
                                        }

                                        ui.painter().text(
                                            handle_rect.center(),
                                            Align2::CENTER_CENTER,
                                            "\u{2630}",
                                            egui::FontId::proportional(13.0),
                                            Color32::from_gray(if dragging { 220 } else { 140 }),
                                        );

                                        let eye_resp = ui.interact(
                                            eye_rect,
                                            row_id.with("eye"),
                                            egui::Sense::click(),
                                        );
                                        paint_visibility_icon(
                                            ui,
                                            eye_rect,
                                            shown,
                                            eye_resp.hovered(),
                                        );
                                        if eye_resp
                                            .on_hover_text(if shown { "Hide" } else { "Show" })
                                            .clicked()
                                        {
                                            toggle = Some(id);
                                        }

                                        let locked = draft_locked.contains(&id);
                                        let lock_resp = ui.interact(
                                            lock_rect,
                                            row_id.with("lock"),
                                            egui::Sense::click(),
                                        );
                                        paint_lock_icon(
                                            ui,
                                            lock_rect,
                                            locked,
                                            true,
                                            lock_resp.hovered(),
                                        );
                                        let lock_tip = if locked {
                                            "Unlock (also unlocks plots below)"
                                        } else {
                                            "Lock this plot and everything above it"
                                        };
                                        if lock_resp.on_hover_text(lock_tip).clicked() {
                                            lock_toggle = Some(id);
                                        }

                                        let name_color = if !has_data {
                                            Color32::from_gray(90)
                                        } else if shown {
                                            Color32::from_gray(210)
                                        } else {
                                            Color32::from_gray(140)
                                        };
                                        ui.painter().text(
                                            name_rect.left_center(),
                                            Align2::LEFT_CENTER,
                                            id.label(),
                                            egui::FontId::proportional(13.0),
                                            name_color,
                                        );

                                        if pointer_released {
                                            if let Some(pos) = pointer_pos {
                                                if row_rect.contains(pos) {
                                                    if let Some(drag) = self.plot_sidebar_drag {
                                                        drop_at = Some((drag, i));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                });
                        },
                    );
                });

                if pointer_released {
                    if let Some((id, to)) = drop_at {
                        self.with_current_layouts(|_, draft| draft.move_id(id, to));
                    }
                    self.plot_sidebar_drag = None;
                }

                if let Some(id) = toggle {
                    self.with_current_layouts(|_, draft| draft.toggle_hidden(id));
                }
                if let Some(id) = lock_toggle {
                    self.with_current_layouts(|_, draft| draft.toggle_lock(id));
                }
            });
    }
}
