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

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
const PRIMARY_COPY_PENDING_ID: &str = "event_explorer_primary_copy_pending";

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
impl UnixTextSelectionState {
    fn update_drag(
        &mut self,
        pressed: bool,
        released: bool,
        pointer_pos: Option<egui::Pos2>,
        text_under_cursor: bool,
    ) -> bool {
        if pressed {
            self.drag_origin = pointer_pos;
            self.dragged = false;
            self.drag_started_on_text = text_under_cursor;
        }
        if let (Some(origin), Some(pointer)) = (self.drag_origin, pointer_pos) {
            self.dragged |= origin.distance_sq(pointer) >= 9.0;
        }
        let completed_drag = released && self.dragged && self.drag_started_on_text;
        if released {
            self.drag_origin = None;
            self.dragged = false;
            self.drag_started_on_text = false;
        }
        completed_drag
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
impl EventExplorerApp {
    fn prepare_unix_text_selection(&mut self, ctx: &egui::Context) {
        use arboard::{GetExtLinux as _, LinuxClipboardKind, SetExtLinux as _};

        if !self.unix_text_selection.hook_installed {
            let primary_selection = Arc::clone(&self.unix_text_selection.primary_selection);
            ctx.on_end_frame(
                "EventExplorer Unix PRIMARY selection",
                Arc::new(move |ctx| {
                    let pending = ctx.data_mut(|data| {
                        data.remove_temp::<bool>(egui::Id::new(PRIMARY_COPY_PENDING_ID))
                            .unwrap_or(false)
                    });
                    if !pending {
                        return;
                    }
                    let selected = ctx.output_mut(|output| std::mem::take(&mut output.copied_text));
                    if selected.is_empty() {
                        return;
                    }
                    if let Ok(mut primary_selection) = primary_selection.lock() {
                        *primary_selection = Some(selected);
                        ctx.request_repaint();
                    }
                }),
            );
            self.unix_text_selection.hook_installed = true;
        }

        let selected = self
            .unix_text_selection
            .primary_selection
            .lock()
            .ok()
            .and_then(|mut selection| selection.take());
        if let Some(selected) = selected {
            if self.unix_text_selection.clipboard.is_none() {
                self.unix_text_selection.clipboard = arboard::Clipboard::new().ok();
            }
            if let Some(clipboard) = self.unix_text_selection.clipboard.as_mut() {
                let _ = clipboard
                    .set()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text(selected);
            }
        }

        let (primary_pressed, primary_released, primary_multi_clicked, middle_pressed, pointer_pos) =
            ctx.input(|input| {
                (
                    input.pointer.button_pressed(egui::PointerButton::Primary),
                    input.pointer.button_released(egui::PointerButton::Primary),
                    input
                        .pointer
                        .button_double_clicked(egui::PointerButton::Primary)
                        || input
                            .pointer
                            .button_triple_clicked(egui::PointerButton::Primary),
                    input.pointer.button_pressed(egui::PointerButton::Middle),
                    input.pointer.interact_pos(),
                )
            });
        let completed_drag = self.unix_text_selection.update_drag(
            primary_pressed,
            primary_released,
            pointer_pos,
            self.unix_text_selection.selectable_text_under_cursor,
        );
        if completed_drag
            || (primary_multi_clicked && self.unix_text_selection.selectable_text_under_cursor)
        {
            ctx.input_mut(|input| input.events.push(egui::Event::Copy));
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::new(PRIMARY_COPY_PENDING_ID), true);
            });
        }

        if middle_pressed && self.unix_text_selection.mutable_text_under_cursor {
            if self.unix_text_selection.clipboard.is_none() {
                self.unix_text_selection.clipboard = arboard::Clipboard::new().ok();
            }
            let primary = self
                .unix_text_selection
                .clipboard
                .as_mut()
                .and_then(|clipboard| {
                    clipboard
                        .get()
                        .clipboard(LinuxClipboardKind::Primary)
                        .text()
                        .ok()
                });
            if let Some(primary) = primary {
                if !primary.is_empty() {
                    ctx.input_mut(|input| input.events.push(egui::Event::Paste(primary)));
                }
            }
        }
    }

    fn finish_unix_text_selection(&mut self, ctx: &egui::Context) {
        self.unix_text_selection.selectable_text_under_cursor =
            ctx.output(|output| output.cursor_icon == egui::CursorIcon::Text);
        self.unix_text_selection.mutable_text_under_cursor =
            ctx.output(|output| output.mutable_text_under_cursor);
    }
}

impl eframe::App for EventExplorerApp {
    #[cfg(target_os = "macos")]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // winit 0.29 forwards Control-left-click as a primary button event.
        // Translate before egui sees it so every secondary-click action
        // works with the standard macOS gesture, including secondary drags.
        for event in &mut raw_input.events {
            if let egui::Event::PointerButton {
                button,
                pressed,
                modifiers,
                ..
            } = event
            {
                if *button == egui::PointerButton::Primary {
                    if *pressed {
                        self.macos_control_click = modifiers.ctrl;
                    }
                    if self.macos_control_click {
                        *button = egui::PointerButton::Secondary;
                    }
                    if !*pressed {
                        self.macos_control_click = false;
                    }
                }
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Fast shutdown. When the user hits the window's close button, the
        // default path lets eframe drop the app, which frees the (potentially
        // multi-gigabyte) parsed capture — millions of per-sample allocations
        // — synchronously on the UI thread. Until that finishes the window
        // stays on screen, frozen, for a second or more, so it looks like the
        // click didn't register. Persist the small window-state file, then
        // exit the process and let the OS reclaim the heap in bulk so the
        // window disappears immediately.
        #[cfg(not(target_arch = "wasm32"))]
        if ctx.input(|i| i.viewport().close_requested()) {
            if let Some(state) = &self.current_window_state {
                save_window_state(state);
            }
            save_processes_preferences(&self.processes_preferences);
            std::process::exit(0);
        }

        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
        ))]
        self.prepare_unix_text_selection(ctx);

        #[cfg(not(target_arch = "wasm32"))]
        self.show_remote_input(ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.show_native_file_dialog(ctx);

        #[cfg(not(target_arch = "wasm32"))]
        {
            let dropped = ctx.input(|i| i.raw.dropped_files.clone());
            if !dropped.is_empty() {
                let mut csv_paths = Vec::new();
                let mut capture_paths = Vec::new();
                for d in dropped {
                    if let Some(path) = d.path {
                        if path
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
                        {
                            csv_paths.push(path);
                        } else {
                            capture_paths.push(path);
                        }
                    }
                }
                if !csv_paths.is_empty() {
                    if !capture_paths.is_empty() {
                        self.pending_origin_csv.extend(csv_paths);
                    } else {
                        for csv in csv_paths {
                            self.load_origin_segments_path(csv);
                        }
                    }
                }
                if !capture_paths.is_empty() {
                    self.start_loading_paths(capture_paths);
                }
            }
        }

        // Drain a completed file pick from the async picker (web only).
        #[cfg(target_arch = "wasm32")]
        {
            let drained = if let Some(rx) = &self.pending_file_pick {
                match rx.try_recv() {
                    Ok(picked) => Some(picked),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => Some(None),
                }
            } else {
                None
            };
            if let Some(picked) = drained {
                self.pending_file_pick = None;
                if let Some(files) = picked {
                    // Keep the browser File handles: a Share-overlay
                    // upload re-reads them without holding the bytes.
                    let handles: Vec<(String, web_sys::File)> = files
                        .iter()
                        .map(|(name, _, file)| (name.clone(), file.clone()))
                        .collect();
                    self.start_loading_web(
                        files
                            .into_iter()
                            .map(|(name, bytes, _)| super::state::WebSource::Bytes(name, bytes))
                            .collect(),
                    );
                    self.loaded_local_files = handles;
                }
            }
            // Drain a completed baseline pick (flame compare).
            let drained_baseline = if let Some(rx) = &self.pending_baseline_pick {
                match rx.try_recv() {
                    Ok(picked) => Some(picked),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => Some(None),
                }
            } else {
                None
            };
            if let Some(picked) = drained_baseline {
                self.pending_baseline_pick = None;
                if let Some((name, bytes)) = picked {
                    self.start_baseline_load_web(name, bytes);
                }
            }
            self.drain_hosted_logs();
            self.drain_history_nav();
            // The browser doesn't deliver per-frame ticks while idle; ask
            // for a repaint so progress / async-pick polling makes
            // forward progress.
            ctx.request_repaint_after(web_time::Duration::from_millis(100));
        }
        self.poll_pack_fetch(ctx);
        apply_visuals(ctx, self.dark_mode);
        Self::snapshot_tab_chrome_escape(ctx);

        let hostname = self
            .file_info
            .as_ref()
            .and_then(|fi| fi.machine.as_ref())
            .map(|mi| mi.hostname.as_str())
            .filter(|h| !h.is_empty());
        let mut desired_title = match (&self.loaded_file_name, hostname) {
            (Some(name), Some(host)) => format!("{name} ({host})"),
            (Some(name), None) => name.clone(),
            (None, _) => format!("EventExplorer v{}", env!("CARGO_PKG_VERSION")),
        };
        if let Some(job) = &self.pack_fetch {
            desired_title.push_str(&format!(
                " — fetching symbol packs ({}/{})",
                job.done, job.total
            ));
        }
        if self.current_title.as_deref() != Some(desired_title.as_str()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(desired_title.clone()));
            self.current_title = Some(desired_title);
        }

        // Window-position persistence is desktop-only. macOS reports logical
        // coordinates through winit; X11 keeps its existing xwininfo fallback.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let maximized = ctx.input(|i| i.viewport().maximized);
            let screen = ctx.screen_rect();
            if let Some((mon_w, mon_h)) = self.startup_monitor {
                if screen.width() > 0.0 {
                    self.current_window_state = Some(WindowState {
                        pos_x: self.current_window_state.as_ref().map_or(0.0, |s| s.pos_x),
                        pos_y: self.current_window_state.as_ref().map_or(0.0, |s| s.pos_y),
                        has_position: self
                            .current_window_state
                            .as_ref()
                            .is_some_and(|s| s.has_position),
                        width: screen.width(),
                        height: screen.height(),
                        maximized: maximized.unwrap_or(false),
                        monitor_width: mon_w,
                        monitor_height: mon_h,
                    });
                    let should_save = match self.last_window_save {
                        None => true,
                        Some(t) => t.elapsed().as_secs() >= 2,
                    };
                    if should_save {
                        if self.last_window_save.is_none() {
                            ctx.request_repaint_after(std::time::Duration::from_secs(2));
                        }
                        self.last_window_save = Some(Instant::now());
                        #[cfg(target_os = "macos")]
                        let position =
                            ctx.input(|i| i.viewport().outer_rect.map(|r| (r.min.x, r.min.y)));
                        #[cfg(not(target_os = "macos"))]
                        let position = query_x11_window_position().map(|(x, y)| {
                            let scale = ctx.pixels_per_point();
                            (x / scale, y / scale)
                        });
                        if let Some((x, y)) = position {
                            if let Some(state) = &mut self.current_window_state {
                                state.pos_x = x;
                                state.pos_y = y;
                                state.has_position = true;
                            }
                        }
                        if let Some(state) = &self.current_window_state {
                            save_window_state(state);
                        }
                    }
                }
            }
        }

        let show_tabs = matches!(self.view, View::Loaded);

        let multi_session = self.summaries.len() > 1;
        let has_ella_channels = !self.ella_profile.channels.is_empty();
        let agg_available = self.system_tab_available();
        if !agg_available && self.active_tab == Tab::Aggregate {
            if let Some(pv) = self.open_sessions.first() {
                self.active_tab = Tab::Session(pv.summary.session_id);
            }
        }

        // Snapshotted so the block below can tell whether the tab bar
        // (clicks, close-tab fallback, etc.) actually changed tabs this
        // frame — see the System/Session <-> Flame sync after it.
        let tab_before_bar = self.active_tab;

        if show_tabs {
            egui::TopBottomPanel::top("tab_bar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let icon_font = egui::FontId::proportional(16.0);
                    let icon_galley = ui.fonts(|f| {
                        f.layout_no_wrap("📂".to_owned(), icon_font.clone(), Color32::WHITE)
                    });
                    let sb_size = icon_galley.size() + ui.spacing().button_padding * 2.0;
                    let (sb_rect, sb_resp) = ui.allocate_exact_size(sb_size, egui::Sense::click());
                    let sb_vis = if self.plot_sidebar_open {
                        ui.visuals().widgets.open
                    } else {
                        *ui.style().interact(&sb_resp)
                    };
                    ui.painter().rect(
                        sb_rect.expand(sb_vis.expansion),
                        sb_vis.rounding,
                        sb_vis.weak_bg_fill,
                        sb_vis.bg_stroke,
                    );
                    // Left-sidebar glyph: outer frame plus a filled strip
                    // on the left, matching the gear's painted-icon style.
                    let icon = sb_rect.shrink2(ui.spacing().button_padding + egui::vec2(2.0, 2.0));
                    let stroke = Stroke::new(1.2_f32, sb_vis.text_color());
                    ui.painter().rect_stroke(icon, 1.5, stroke);
                    let strip_w = icon.width() * 0.32;
                    let strip =
                        egui::Rect::from_min_size(icon.min, egui::vec2(strip_w, icon.height()));
                    ui.painter().rect_filled(
                        strip,
                        1.0,
                        if self.plot_sidebar_open {
                            sb_vis.text_color()
                        } else {
                            Color32::from_rgba_unmultiplied(
                                sb_vis.text_color().r(),
                                sb_vis.text_color().g(),
                                sb_vis.text_color().b(),
                                80,
                            )
                        },
                    );
                    if sb_resp
                        .on_hover_text(if self.plot_sidebar_open {
                            "Hide plot catalog"
                        } else {
                            "Show plot catalog"
                        })
                        .clicked()
                    {
                        self.plot_sidebar_open = !self.plot_sidebar_open;
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("📂").size(16.0)))
                        .on_hover_text("Open File...")
                        .clicked()
                    {
                        self.open_file_dialog();
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui
                        .add(egui::Button::new(RichText::new("🌐").size(16.0)))
                        .on_hover_text("Open Remote... (ssh)")
                        .clicked()
                    {
                        self.open_remote_dialog();
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui
                        .add(egui::Button::new(RichText::new("🎬").size(16.0)))
                        .on_hover_text(if self.origin_segments.is_some() {
                            "Change Origin Segments CSV..."
                        } else {
                            "Load Origin Segments CSV... (ground truth)"
                        })
                        .clicked()
                    {
                        self.open_origin_segments_dialog();
                    }
                    // Share: on web a permalink to this view (plus an
                    // upload when the capture came from the file picker
                    // and the site has a share server); on desktop an
                    // upload dialog that lands the same kind of
                    // permalink, so it needs the share server.
                    let share_tip = if cfg!(target_arch = "wasm32") {
                        "Share link to this view"
                    } else {
                        "Share capture (upload to the share server)"
                    };
                    let can_share =
                        cfg!(target_arch = "wasm32") || elog_parser::uploads_configured();
                    if can_share
                        && ui
                            .add(egui::Button::new(RichText::new("🔗").size(16.0)))
                            .on_hover_text(share_tip)
                            .clicked()
                    {
                        self.share_popup_open = !self.share_popup_open;
                    }
                    ui.separator();

                    if agg_available {
                        ui.selectable_value(
                            &mut self.active_tab,
                            Tab::Aggregate,
                            RichText::new("System").size(15.0),
                        );
                    }

                    let mut close_events = false;
                    if self.events_tab.open {
                        let selected = self.active_tab == Tab::Events;
                        let resp = ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let tab_resp =
                                ui.selectable_label(selected, RichText::new("Events").size(15.0));
                            if tab_resp.clicked() {
                                self.active_tab = Tab::Events;
                            }
                            let x_resp = ui.selectable_label(false, RichText::new("x").size(11.0));
                            if x_resp.clicked() || tab_resp.middle_clicked() {
                                close_events = true;
                            }
                        });
                        if resp.response.middle_clicked() {
                            close_events = true;
                        }
                    }
                    if close_events {
                        self.close_events_tab();
                    }

                    if !self.cpu_profile.is_empty() {
                        ui.selectable_value(
                            &mut self.active_tab,
                            Tab::Processes,
                            RichText::new("Processes").size(15.0),
                        );
                        ui.selectable_value(
                            &mut self.active_tab,
                            Tab::CpuFlame,
                            RichText::new("🔥 Flame").size(15.0),
                        );
                    }

                    // Session and Fanout-channel tabs open from the `+`
                    // picker, so they follow it. Also shown for a single
                    // TCP session when System is on the bar — otherwise
                    // that session has no click path.
                    if self.show_session_list_tab() {
                        ui.selectable_value(
                            &mut self.active_tab,
                            Tab::SessionList,
                            RichText::new("+").size(15.0),
                        );
                    }

                    let tab_info: Vec<(u64, String)> = self
                        .open_sessions
                        .iter()
                        .map(|pv| {
                            let sid = pv.summary.session_id;
                            let label = pv
                                .custom_name
                                .clone()
                                .unwrap_or_else(|| format!("Session {sid}"));
                            (sid, label)
                        })
                        .collect();

                    let mut close_idx: Option<usize> = None;
                    let mut start_rename: Option<(u64, String)> = None;
                    for (i, (sid, label)) in tab_info.iter().enumerate() {
                        let sid = *sid;
                        let tab = Tab::Session(sid);
                        let selected = self.active_tab == tab;
                        let is_renaming =
                            self.renaming_tab.as_ref().is_some_and(|(id, _)| *id == sid);

                        let resp = ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if is_renaming {
                                if let Some((_, ref mut buf)) = self.renaming_tab {
                                    let te = egui::TextEdit::singleline(buf)
                                        .desired_width(100.0)
                                        .font(egui::TextStyle::Body);
                                    let r = ui.add(te);
                                    if r.lost_focus()
                                        || ui.input(|i| i.key_pressed(egui::Key::Enter))
                                    {
                                        let name = buf.trim().to_string();
                                        if let Some(pv) = self
                                            .open_sessions
                                            .iter_mut()
                                            .find(|pv| pv.summary.session_id == sid)
                                        {
                                            pv.custom_name =
                                                if name.is_empty() { None } else { Some(name) };
                                        }
                                        self.renaming_tab = None;
                                    } else {
                                        r.request_focus();
                                    }
                                }
                            } else {
                                let tab_resp =
                                    ui.selectable_label(selected, RichText::new(label).size(15.0));
                                if tab_resp.clicked() {
                                    self.active_tab = tab;
                                }
                                if tab_resp.middle_clicked() && multi_session {
                                    close_idx = Some(i);
                                }
                                if tab_resp.secondary_clicked() {
                                    start_rename = Some((sid, label.clone()));
                                }
                                if multi_session {
                                    let x_resp =
                                        ui.selectable_label(false, RichText::new("x").size(11.0));
                                    if x_resp.clicked() {
                                        close_idx = Some(i);
                                    }
                                }
                            }
                        });
                        if resp.response.middle_clicked() && multi_session {
                            close_idx = Some(i);
                        }
                    }

                    if let Some((sid, label)) = start_rename {
                        self.renaming_tab = Some((sid, label));
                    }

                    let ella_tabs: Vec<(u16, u64, String)> = self
                        .open_ella_channels
                        .iter()
                        .map(|c| (c.provider_id, c.session_id, c.label.clone()))
                        .collect();
                    let mut ella_close: Option<usize> = None;
                    let can_close_ella = ella_tabs.len() > 1 || multi_session || agg_available;
                    for (i, (pid, sid, label)) in ella_tabs.iter().enumerate() {
                        let tab = Tab::EllaChannel {
                            provider_id: *pid,
                            session_id: *sid,
                        };
                        let selected = self.active_tab == tab;
                        let resp = ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let tab_resp =
                                ui.selectable_label(selected, RichText::new(label).size(15.0));
                            if tab_resp.clicked() {
                                self.active_tab = tab;
                            }
                            if can_close_ella {
                                let x_resp =
                                    ui.selectable_label(false, RichText::new("x").size(11.0));
                                if x_resp.clicked() || tab_resp.middle_clicked() {
                                    ella_close = Some(i);
                                }
                            }
                        });
                        if can_close_ella && resp.response.middle_clicked() {
                            ella_close = Some(i);
                        }
                    }

                    // Global gear + help + trace-info: right-aligned so they
                    // stay put regardless of how many session tabs are open.
                    // RTL: (i) is rightmost, then (?), then gear.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.show_tab_info_button(ui);
                        self.show_plot_help_button(ui);
                        ui.add_space(4.0);
                        self.show_global_gear(ui);
                    });

                    if let Some(idx) = close_idx {
                        let closed_sid = self.open_sessions[idx].summary.session_id;
                        self.open_sessions.remove(idx);
                        if self.active_tab == Tab::Session(closed_sid) {
                            if !self.open_sessions.is_empty() {
                                let new_idx = idx.min(self.open_sessions.len() - 1);
                                self.active_tab =
                                    Tab::Session(self.open_sessions[new_idx].summary.session_id);
                            } else if !self.summaries.is_empty() || agg_available {
                                self.active_tab = Tab::Aggregate;
                            } else if !self.open_ella_channels.is_empty() {
                                let c = &self.open_ella_channels[0];
                                self.active_tab = Tab::EllaChannel {
                                    provider_id: c.provider_id,
                                    session_id: c.session_id,
                                };
                            } else if !self.cpu_profile.is_empty() {
                                self.active_tab = Tab::CpuFlame;
                            } else {
                                self.active_tab = Tab::Aggregate;
                            }
                        }
                    }
                    if let Some(idx) = ella_close {
                        let closed = (
                            self.open_ella_channels[idx].provider_id,
                            self.open_ella_channels[idx].session_id,
                        );
                        self.open_ella_channels.remove(idx);
                        if self.active_tab
                            == (Tab::EllaChannel {
                                provider_id: closed.0,
                                session_id: closed.1,
                            })
                        {
                            if !self.open_ella_channels.is_empty() {
                                let new_idx = idx.min(self.open_ella_channels.len() - 1);
                                let c = &self.open_ella_channels[new_idx];
                                self.active_tab = Tab::EllaChannel {
                                    provider_id: c.provider_id,
                                    session_id: c.session_id,
                                };
                            } else if agg_available {
                                self.active_tab = Tab::Aggregate;
                            } else if !self.open_sessions.is_empty() {
                                self.active_tab =
                                    Tab::Session(self.open_sessions[0].summary.session_id);
                            } else if has_ella_channels {
                                self.active_tab = Tab::SessionList;
                            } else if !self.cpu_profile.is_empty() {
                                self.active_tab = Tab::CpuFlame;
                            } else {
                                self.active_tab = Tab::Aggregate;
                            }
                        }
                    }
                });
            });
        }

        // Unfinished box-zoom / Δt drags belong to the tab that started
        // them. System, Flame, and Processes share one time window;
        // session and channel tabs keep their own.
        if self.active_tab != tab_before_bar {
            self.agg_time_axis.cancel_gesture();
            for pv in &mut self.open_sessions {
                pv.time_axis.cancel_gesture();
            }
            for ch in &mut self.open_ella_channels {
                ch.time_axis.cancel_gesture();
            }
            let shares_flame_time = |tab: Tab| matches!(tab, Tab::CpuFlame | Tab::Processes);
            match (tab_before_bar, self.active_tab) {
                (from, to) if shares_flame_time(to) && !shares_flame_time(from) => {
                    self.sync_flame_time_from_system();
                }
                (from, to) if shares_flame_time(from) && !shares_flame_time(to) => {
                    self.sync_system_time_from_flame();
                }
                _ => {}
            }
        }

        if matches!(self.view, View::Loaded) {
            self.show_plot_sidebar(ctx);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            match &self.view {
                View::NoFile => self.show_no_file(ui),
                View::Loading { .. } => self.show_loading(ctx, ui),
                View::Loaded => {
                    match self.active_tab {
                        Tab::Aggregate => {
                            // Cpu-only captures fall through to Aggregate
                            // when no other tab is active. Surface the
                            // flame view in that case so the user lands
                            // on something useful instead of the empty
                            // multi-session aggregate. The aggregate page
                            // still has content without sessions when the
                            // capture carries interface, TCP, or STARS
                            // aggregate samples, so only redirect when it
                            // would truly be empty.
                            let agg_has_content = self.system_tab_available();
                            if !agg_has_content && !self.cpu_profile.is_empty() {
                                self.active_tab = Tab::CpuFlame;
                                self.show_cpu_flame_view(ui);
                            } else {
                                self.show_aggregate_view(ctx, ui);
                            }
                        }
                        Tab::SessionList => self.show_session_list(ui),
                        Tab::Session(id) => {
                            if let Some(idx) = self
                                .open_sessions
                                .iter()
                                .position(|pv| pv.summary.session_id == id)
                            {
                                self.show_plot_view(ctx, ui, idx);
                            }
                        }
                        Tab::EllaChannel {
                            provider_id,
                            session_id,
                        } => {
                            if let Some(idx) = self.open_ella_channels.iter().position(|c| {
                                c.provider_id == provider_id && c.session_id == session_id
                            }) {
                                self.show_ella_channel_view(ui, idx);
                            }
                        }
                        Tab::CpuFlame => self.show_cpu_flame_view(ui),
                        Tab::Processes => self.show_processes_view(ui),
                        Tab::Events => self.show_events_view(ui),
                    }
                }
            }
        });

        #[cfg(target_arch = "wasm32")]
        self.show_share_popup(ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.show_share_dialog(ctx);

        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
        ))]
        self.finish_unix_text_selection(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(state) = &self.current_window_state {
                save_window_state(state);
            }
            save_processes_preferences(&self.processes_preferences);
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_pointer_tests {
    use super::*;
    use eframe::App as _;

    fn button_event(button: egui::PointerButton, pressed: bool, ctrl: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::pos2(80.0, 80.0),
            button,
            pressed,
            modifiers: egui::Modifiers {
                ctrl,
                ..Default::default()
            },
        }
    }

    fn canvas_frame(
        app: &mut EventExplorerApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::Response {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            events,
            ..Default::default()
        };
        app.raw_input_hook(ctx, &mut input);
        let mut response = None;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                response = Some(
                    ui.allocate_response(egui::vec2(300.0, 200.0), egui::Sense::click_and_drag()),
                );
            });
        });
        response.unwrap()
    }

    #[test]
    fn control_click_triggers_secondary_canvas_action_even_if_control_released_first() {
        use egui::PointerButton::{Primary, Secondary};

        for release_ctrl in [true, false] {
            let mut app = EventExplorerApp::new_empty(None);
            let ctx = egui::Context::default();
            canvas_frame(&mut app, &ctx, vec![]);
            canvas_frame(&mut app, &ctx, vec![button_event(Primary, true, true)]);
            assert!(ctx.input(|i| i.pointer.button_down(Secondary)));
            assert!(!ctx.input(|i| i.pointer.button_down(Primary)));
            let response = canvas_frame(
                &mut app,
                &ctx,
                vec![button_event(Primary, false, release_ctrl)],
            );
            assert!(response.secondary_clicked());
            assert!(!response.clicked());
            assert!(!ctx.input(|i| i.pointer.any_down()));

            // A following ordinary click must not inherit the mapping.
            canvas_frame(&mut app, &ctx, vec![button_event(Primary, true, false)]);
            let response = canvas_frame(&mut app, &ctx, vec![button_event(Primary, false, false)]);
            assert!(response.clicked());
            assert!(!response.secondary_clicked());
        }
    }

    #[test]
    fn command_click_and_physical_secondary_click_keep_their_buttons_and_modifiers() {
        use egui::PointerButton::{Extra1, Extra2, Middle, Primary, Secondary};

        let mut app = EventExplorerApp::new_empty(None);
        let ctx = egui::Context::default();
        for button in [Primary, Secondary, Middle, Extra1, Extra2] {
            for ctrl in [false, true] {
                if button == Primary && ctrl {
                    continue;
                }
                let mut input = egui::RawInput {
                    events: vec![
                        button_event(button, true, ctrl),
                        button_event(button, false, ctrl),
                    ],
                    ..Default::default()
                };
                // Command is the flame stack-selection modifier on macOS.
                for event in &mut input.events {
                    if let egui::Event::PointerButton { modifiers, .. } = event {
                        modifiers.command = true;
                        modifiers.mac_cmd = true;
                    }
                }
                let original = input.events.clone();
                app.raw_input_hook(&ctx, &mut input);
                assert_eq!(input.events, original);
            }
        }
    }

    #[test]
    fn control_pressed_after_primary_press_does_not_change_release_button() {
        use egui::PointerButton::Primary;

        let mut app = EventExplorerApp::new_empty(None);
        let ctx = egui::Context::default();
        canvas_frame(&mut app, &ctx, vec![]);
        canvas_frame(&mut app, &ctx, vec![button_event(Primary, true, false)]);
        let response = canvas_frame(&mut app, &ctx, vec![button_event(Primary, false, true)]);
        assert!(response.clicked());
        assert!(!response.secondary_clicked());
        assert!(!ctx.input(|i| i.pointer.any_down()));
    }
}

#[cfg(all(
    test,
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
mod unix_text_selection_tests {
    use super::*;

    #[test]
    fn primary_drag_completes_only_after_selection_motion() {
        let mut state = UnixTextSelectionState::default();
        assert!(!state.update_drag(false, true, Some(egui::pos2(10.0, 10.0)), true));
        assert!(!state.update_drag(true, false, Some(egui::pos2(10.0, 10.0)), true));
        assert!(!state.update_drag(false, false, Some(egui::pos2(11.0, 11.0)), true));
        assert!(!state.update_drag(false, false, Some(egui::pos2(20.0, 10.0)), true));
        assert!(state.update_drag(false, true, Some(egui::pos2(20.0, 10.0)), true));
        assert!(!state.update_drag(false, true, Some(egui::pos2(20.0, 10.0)), true));

        assert!(!state.update_drag(true, false, Some(egui::pos2(10.0, 10.0)), false));
        assert!(!state.update_drag(false, false, Some(egui::pos2(20.0, 10.0)), false));
        assert!(!state.update_drag(false, true, Some(egui::pos2(20.0, 10.0)), false));
    }

    #[test]
    fn middle_press_focuses_text_edit_before_paste_event() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0));
        let mut text = String::new();
        let mut edit_rect = egui::Rect::NOTHING;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    edit_rect = ui.add(egui::TextEdit::singleline(&mut text)).rect;
                });
            },
        );

        let pointer = edit_rect.center();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Middle,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                    egui::Event::Paste("selected path".to_string()),
                ],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut text));
                });
            },
        );
        assert_eq!(text, "selected path");
    }
}
