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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FunctionSort {
    SelfSamples,
    InclusiveSamples,
}

pub struct FunctionsViewState {
    profile: analysis::FunctionProfile,
    filter: FlameFilter,
    top_n: usize,
    sort: FunctionSort,
    selected: Option<ProfileFunctionId>,
    selected_name: String,
    stacks: Vec<analysis::FunctionStack>,
    wrapped_stack_layout: Option<WrappedStackLayout>,
    stack_build: Option<mpsc::Receiver<Vec<analysis::FunctionStack>>>,
}

impl FunctionsViewState {
    fn from_profile(profile: &CpuProfile, filter: FlameFilter) -> Self {
        Self {
            profile: analysis::build_function_profile(profile, &filter),
            filter,
            top_n: 25,
            sort: FunctionSort::SelfSamples,
            selected: None,
            selected_name: String::new(),
            stacks: Vec::new(),
            wrapped_stack_layout: None,
            stack_build: None,
        }
    }
}

fn sample_pct(count: u64, total: u64) -> String {
    if total == 0 {
        "0.00%".to_string()
    } else {
        format!("{:.2}%", count as f64 * 100.0 / total as f64)
    }
}

fn function_sort_value(function: &analysis::FunctionRow, sort: FunctionSort) -> u64 {
    match sort {
        FunctionSort::SelfSamples => function.self_samples,
        FunctionSort::InclusiveSamples => function.inclusive_samples,
    }
}

fn stack_frame_label(frame: &str, show_module: bool) -> &str {
    if show_module {
        frame
    } else {
        frame.split_once(':').map_or(frame, |(_, symbol)| symbol)
    }
}

#[derive(Clone, Copy)]
struct StackLayoutOptions<'a> {
    selected_name: &'a str,
    show_modules: bool,
    font_size: f32,
    wrap_width: f32,
    text_color: Color32,
}

struct WrappedStackLayout {
    width: f32,
    font_size: f32,
    show_modules: bool,
    text_color: Color32,
    jobs: Vec<(egui::text::LayoutJob, f32)>,
}

fn stack_layout_job(
    index: usize,
    stack: &analysis::FunctionStack,
    stack_total: u64,
    options: StackLayoutOptions<'_>,
) -> egui::text::LayoutJob {
    let mono = egui::FontId::monospace(options.font_size);
    let proportional = egui::FontId::proportional(options.font_size);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = options.wrap_width;
    job.first_row_min_height = (options.font_size + 3.0).max(9.0);

    job.append(
        &format!("{}.", index + 1),
        0.0,
        egui::TextFormat {
            font_id: mono.clone(),
            color: Color32::from_gray(130),
            ..Default::default()
        },
    );
    job.append(
        &format!(
            " {} ({}) ",
            format_count(stack.count),
            sample_pct(stack.count, stack_total)
        ),
        0.0,
        egui::TextFormat {
            font_id: proportional.clone(),
            color: options.text_color,
            ..Default::default()
        },
    );
    for (frame_idx, frame) in stack.frames.iter().enumerate() {
        if frame_idx > 0 {
            job.append(
                " > ",
                0.0,
                egui::TextFormat {
                    font_id: proportional.clone(),
                    color: Color32::from_gray(110),
                    ..Default::default()
                },
            );
        }
        job.append(
            stack_frame_label(frame, options.show_modules),
            0.0,
            egui::TextFormat {
                font_id: mono.clone(),
                color: if frame.contains(options.selected_name) {
                    Color32::from_rgb(255, 190, 80)
                } else {
                    options.text_color
                },
                ..Default::default()
            },
        );
    }
    job
}

fn show_function_table_header(ui: &mut egui::Ui, show_module: bool) {
    ui.strong("Function");
    if show_module {
        ui.strong("Module");
    }
    ui.strong("Self");
    ui.strong("Self %");
    ui.strong("Inclusive");
    ui.strong("Inclusive %");
    ui.end_row();
}

#[derive(Default)]
struct FunctionTableActions {
    selected: Option<(ProfileFunctionId, String)>,
    #[cfg(not(target_arch = "wasm32"))]
    open_source: Option<super::source::SourceJump>,
}

fn show_function_table_row(
    ui: &mut egui::Ui,
    binary: &analysis::BinaryFunctionProfile,
    function: &analysis::FunctionRow,
    total: u64,
    show_module: bool,
    is_selected: bool,
    actions: &mut FunctionTableActions,
) {
    let response = ui.selectable_label(is_selected, RichText::new(&function.name).monospace());
    if response.clicked() {
        actions.selected = Some((function.id, function.name.clone()));
    }
    if response.secondary_clicked() {
        actions.selected = Some((function.id, function.name.clone()));
        let _ = binary;
        #[cfg(not(target_arch = "wasm32"))]
        {
            actions.open_source =
                function
                    .sampled_offsets
                    .first()
                    .map(|&(offset, _)| super::source::SourceJump {
                        pc: offset,
                        pid: 0,
                        stack_path: Vec::new(),
                    });
        }
    }
    if show_module {
        let module = ui.label(RichText::new(&binary.name).monospace());
        if !binary.pathname.is_empty() {
            module.on_hover_text(&binary.pathname);
        }
    }
    ui.label(format_count(function.self_samples));
    ui.label(sample_pct(function.self_samples, total));
    ui.label(format_count(function.inclusive_samples));
    ui.label(sample_pct(function.inclusive_samples, total));
    ui.end_row();
}

impl EventExplorerApp {
    pub(crate) fn current_function_filter(&self) -> FlameFilter {
        self.flame
            .as_ref()
            .map(|f| f.filter.clone())
            .or_else(|| self.pending_flame_filter.clone())
            .unwrap_or_default()
    }

    pub(crate) fn show_process_functions(&mut self, ui: &mut egui::Ui, owner: &ProcessSelection) {
        self.show_sample_cpu_filter(ui);
        if self.process_functions_owner.as_ref() != Some(owner) {
            self.process_functions_owner = Some(owner.clone());
            self.process_functions_view = None;
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.process_functions_build = None;
            }
        }
        let pids = match owner {
            ProcessSelection::Pid(pid) => vec![*pid],
            ProcessSelection::Comm(comm) => self
                .processes_view
                .as_ref()
                .and_then(|view| view.groups().iter().find(|group| group.comm == *comm))
                .map(|group| group.pids.clone())
                .unwrap_or_default(),
        };
        if pids.is_empty() {
            ui.label("No sampled pids belong to this process row.");
            return;
        }
        let (ts_start, ts_end) = self
            .processes_view
            .as_ref()
            .map(|view| (view.ts_start_us, view.ts_end_us))
            .unwrap_or((0, u64::MAX));
        let mut filter = self.current_function_filter();
        filter.pids = pids;
        filter.ts_start_us = ts_start;
        filter.ts_end_us = ts_end;
        if self
            .process_functions_view
            .as_ref()
            .is_none_or(|view| view.filter != filter)
        {
            if self.process_functions_build.is_none() {
                let profile = self.cpu_profile.clone();
                let filter = filter.clone();
                let (tx, rx) = mpsc::channel();
                #[cfg(not(target_arch = "wasm32"))]
                std::thread::spawn(move || {
                    let _ = tx.send(FunctionsViewState::from_profile(&profile, filter));
                });
                #[cfg(target_arch = "wasm32")]
                wasm_bindgen_futures::spawn_local(async move {
                    crate::app::recent::yield_to_browser().await;
                    let _ = tx.send(FunctionsViewState::from_profile(&profile, filter));
                });
                self.process_functions_build = Some(rx);
            }
            if let Some(mut built) = self
                .process_functions_build
                .as_ref()
                .and_then(|rx| rx.try_recv().ok())
            {
                self.process_functions_build = None;
                if built.filter == filter {
                    if let Some(previous) = &self.process_functions_view {
                        built.top_n = previous.top_n;
                        built.sort = previous.sort;
                    }
                    self.process_functions_view = Some(built);
                }
            }
            if self
                .process_functions_view
                .as_ref()
                .is_none_or(|view| view.filter != filter)
            {
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.label("Building function rankings for this process…");
                });
                ui.ctx().request_repaint();
                return;
            }
        }

        let saved_list_height = self.processes_preferences.function_list_height;
        let mut merge_modules = self.processes_preferences.merge_modules;
        let mut show_stack_modules = self.processes_preferences.show_stack_modules;
        let mut wrap_stacks = self.processes_preferences.wrap_stacks;
        let mut stack_font_size = self.processes_preferences.stack_font_size;
        let mut preferences_changed = false;
        let view = self.process_functions_view.as_mut().unwrap();
        let total = view.profile.selected_samples;
        let mut actions = FunctionTableActions::default();

        ui.horizontal(|ui| {
            ui.heading("Top functions");
            ui.separator();
            ui.label("Top");
            egui::ComboBox::from_id_source("functions_top_n")
                .selected_text(view.top_n.to_string())
                .show_ui(ui, |ui| {
                    for n in [10, 25, 50, 100] {
                        ui.selectable_value(&mut view.top_n, n, n.to_string());
                    }
                });
            ui.separator();
            ui.selectable_value(&mut view.sort, FunctionSort::SelfSamples, "Self");
            ui.selectable_value(&mut view.sort, FunctionSort::InclusiveSamples, "Inclusive");
            ui.separator();
            if ui
                .checkbox(&mut merge_modules, "Merge modules")
                .on_hover_text(
                    "Rank functions from every module in one Top N table. Function identities remain module-specific.",
                )
                .changed()
            {
                preferences_changed = true;
            }
            ui.separator();
            ui.label(format!("{} selected samples", format_count(total)));
            if view.profile.unresolved_leaf_samples > 0 {
                ui.label(
                    RichText::new(format!(
                        "{} unresolved leaves",
                        format_count(view.profile.unresolved_leaf_samples)
                    ))
                    .color(Color32::from_rgb(220, 170, 80)),
                )
                .on_hover_text(
                    "These leaf PCs had no captured function symbol and are not ranked.",
                );
            }
        });
        ui.add_space(4.0);

        let available = ui.available_size();
        let min_list_height = 80.0;
        let min_stacks_height = 100.0;
        let max_list_height =
            (available.y - PROCESS_SPLITTER_SIZE - min_stacks_height).max(min_list_height);
        let mut list_height = saved_list_height
            .unwrap_or(available.y * 0.45)
            .clamp(min_list_height, max_list_height);
        egui::ScrollArea::vertical()
            .id_source("function_rankings")
            .max_height(list_height)
            .min_scrolled_height(list_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if view.profile.binaries.is_empty() {
                    ui.label(
                        "No resolved functions. Capture with `elog --embed-symbols -U` to add kernel and user symbols.",
                    );
                    return;
                }
                if merge_modules {
                    let mut rows: Vec<_> = view
                        .profile
                        .binaries
                        .iter()
                        .flat_map(|binary| {
                            binary
                                .functions
                                .iter()
                                .map(move |function| (binary, function))
                        })
                        .collect();
                    rows.sort_unstable_by(|(a_binary, a), (b_binary, b)| {
                        function_sort_value(b, view.sort)
                            .cmp(&function_sort_value(a, view.sort))
                            .then_with(|| a.name.cmp(&b.name))
                            .then_with(|| a_binary.name.cmp(&b_binary.name))
                    });
                    egui::Grid::new("merged_function_grid")
                        .num_columns(6)
                        .striped(true)
                        .spacing(egui::vec2(16.0, 3.0))
                        .show(ui, |ui| {
                            show_function_table_header(ui, true);
                            for (binary, function) in rows.into_iter().take(view.top_n) {
                                show_function_table_row(
                                    ui,
                                    binary,
                                    function,
                                    total,
                                    true,
                                    view.selected == Some(function.id),
                                    &mut actions,
                                );
                            }
                        });
                } else {
                    for binary in &view.profile.binaries {
                        let binary_self: u64 =
                            binary.functions.iter().map(|f| f.self_samples).sum();
                        let title = format!(
                            "{} — {} self samples ({})",
                            binary.name,
                            format_count(binary_self),
                            sample_pct(binary_self, total)
                        );
                        egui::CollapsingHeader::new(title)
                            .id_source(("function_binary", binary.id))
                            .default_open(true)
                            .show(ui, |ui| {
                                if !binary.pathname.is_empty() {
                                    ui.label(
                                        RichText::new(&binary.pathname)
                                            .monospace()
                                            .size(11.0)
                                            .color(Color32::from_gray(140)),
                                    );
                                }
                                let mut rows: Vec<_> = binary.functions.iter().collect();
                                rows.sort_unstable_by(|a, b| {
                                    function_sort_value(b, view.sort)
                                        .cmp(&function_sort_value(a, view.sort))
                                        .then_with(|| a.name.cmp(&b.name))
                                });
                                egui::Grid::new(("function_grid", binary.id))
                                    .num_columns(5)
                                    .striped(true)
                                    .spacing(egui::vec2(16.0, 3.0))
                                    .show(ui, |ui| {
                                        show_function_table_header(ui, false);
                                        for function in rows.into_iter().take(view.top_n) {
                                            show_function_table_row(
                                                ui,
                                                binary,
                                                function,
                                                total,
                                                false,
                                                view.selected == Some(function.id),
                                                &mut actions,
                                            );
                                        }
                                    });
                            });
                    }
                }
            });

        if let Some((id, name)) = actions.selected {
            view.selected = Some(id);
            view.selected_name = name;
            view.stacks.clear();
            view.wrapped_stack_layout = None;
            let profile = self.cpu_profile.clone();
            let filter = view.filter.clone();
            let (tx, rx) = mpsc::channel();
            #[cfg(not(target_arch = "wasm32"))]
            std::thread::spawn(move || {
                let _ = tx.send(analysis::function_stacks(&profile, &filter, id));
            });
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(async move {
                crate::app::recent::yield_to_browser().await;
                let _ = tx.send(analysis::function_stacks(&profile, &filter, id));
            });
            view.stack_build = Some(rx);
        }
        if let Some(stacks) = view.stack_build.as_ref().and_then(|rx| rx.try_recv().ok()) {
            view.stacks = stacks;
            view.wrapped_stack_layout = None;
            view.stack_build = None;
        }

        let (split_delta, split_drag_stopped) = draw_process_horizontal_splitter(ui);
        if split_delta != 0.0 {
            list_height = (list_height + split_delta).clamp(min_list_height, max_list_height);
            self.processes_preferences.function_list_height = Some(list_height);
        }
        if split_drag_stopped {
            save_processes_preferences(&self.processes_preferences);
        }
        if view.selected.is_none() {
            if preferences_changed {
                self.processes_preferences.merge_modules = merge_modules;
                save_processes_preferences(&self.processes_preferences);
            }
            ui.label(
                RichText::new("Select a function to list every sampled stack containing it.")
                    .color(Color32::from_gray(150)),
            );
            return;
        }
        let stack_total: u64 = view.stacks.iter().map(|s| s.count).sum();
        ui.horizontal(|ui| {
            ui.heading(format!("Stacks containing {}", view.selected_name));
            if view.stack_build.is_some() {
                ui.spinner();
                ui.ctx().request_repaint();
            }
            ui.label(format!(
                "{} unique, {} samples",
                format_count(view.stacks.len() as u64),
                format_count(stack_total)
            ));
            ui.separator();
            if ui
                .checkbox(&mut show_stack_modules, "Show modules")
                .on_hover_text("Prefix resolved stack frames with their originating module.")
                .changed()
            {
                preferences_changed = true;
            }
            ui.separator();
            if ui
                .checkbox(&mut wrap_stacks, "Wrap stacks")
                .on_hover_text("Wrap long stack prints to the available pane width.")
                .changed()
            {
                preferences_changed = true;
            }
            ui.separator();
            ui.label("Stack font");
            egui::ComboBox::from_id_source("process_stack_font_size")
                .selected_text(format!("{stack_font_size:.0} px"))
                .show_ui(ui, |ui| {
                    for size in [
                        6.0,
                        7.0,
                        8.0,
                        9.0,
                        10.0,
                        11.0,
                        12.0,
                        13.0,
                        DEFAULT_STACK_FONT_SIZE,
                    ] {
                        if ui
                            .selectable_value(&mut stack_font_size, size, format!("{size:.0} px"))
                            .changed()
                        {
                            preferences_changed = true;
                        }
                    }
                })
                .response
                .on_hover_text("Reduce the stack-trace text size to fit more frames and rows.");
        });
        let stacks_h = ui.available_height().max(80.0);
        let row_h = (stack_font_size + 3.0).max(9.0);
        let stack_item_spacing = (stack_font_size - 4.0).clamp(2.0, 8.0);
        if wrap_stacks {
            let text_color = ui.visuals().text_color();
            ui.push_id("wrapped_function_stacks", |ui| {
                TableBuilder::new(ui)
                    .column(Column::remainder())
                    .max_scroll_height(stacks_h)
                    .min_scrolled_height(stacks_h)
                    .auto_shrink([false, false])
                    .body(|mut body| {
                        let wrap_width = body.widths()[0].max(1.0);
                        let layout_matches =
                            view.wrapped_stack_layout.as_ref().is_some_and(|layout| {
                                (layout.width - wrap_width).abs() < 0.5
                                    && layout.font_size == stack_font_size
                                    && layout.show_modules == show_stack_modules
                                    && layout.text_color == text_color
                            });
                        if !layout_matches {
                            let jobs = body.ui_mut().fonts(|fonts| {
                                view.stacks
                                    .iter()
                                    .enumerate()
                                    .map(|(index, stack)| {
                                        let job = stack_layout_job(
                                            index,
                                            stack,
                                            stack_total,
                                            StackLayoutOptions {
                                                selected_name: &view.selected_name,
                                                show_modules: show_stack_modules,
                                                font_size: stack_font_size,
                                                wrap_width,
                                                text_color,
                                            },
                                        );
                                        let height =
                                            fonts.layout_job(job.clone()).size().y.max(row_h);
                                        (job, height)
                                    })
                                    .collect()
                            });
                            view.wrapped_stack_layout = Some(WrappedStackLayout {
                                width: wrap_width,
                                font_size: stack_font_size,
                                show_modules: show_stack_modules,
                                text_color,
                                jobs,
                            });
                        }
                        let jobs = &view.wrapped_stack_layout.as_ref().unwrap().jobs;
                        body.heterogeneous_rows(
                            jobs.iter().map(|(_, height)| *height),
                            |mut row| {
                                let index = row.index();
                                row.col(|ui| {
                                    ui.label(jobs[index].0.clone());
                                });
                            },
                        );
                    });
            });
        } else {
            egui::ScrollArea::both()
                .id_source("function_stacks")
                .auto_shrink([false, false])
                .max_height(stacks_h)
                .show_rows(ui, row_h, view.stacks.len(), |ui, range| {
                    ui.style_mut().wrap = Some(false);
                    for index in range {
                        let stack = &view.stacks[index];
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = stack_item_spacing;
                            ui.set_min_height(row_h);
                            ui.label(
                                RichText::new(format!("{}.", index + 1))
                                    .monospace()
                                    .size(stack_font_size)
                                    .color(Color32::from_gray(130)),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{} ({})",
                                    format_count(stack.count),
                                    sample_pct(stack.count, stack_total)
                                ))
                                .size(stack_font_size)
                                .strong(),
                            );
                            for (frame_idx, frame) in stack.frames.iter().enumerate() {
                                if frame_idx > 0 {
                                    ui.label(
                                        RichText::new(">")
                                            .size(stack_font_size)
                                            .color(Color32::from_gray(110)),
                                    );
                                }
                                let text =
                                    RichText::new(stack_frame_label(frame, show_stack_modules))
                                        .monospace()
                                        .size(stack_font_size);
                                if frame.contains(&view.selected_name) {
                                    ui.label(text.color(Color32::from_rgb(255, 190, 80)).strong());
                                } else {
                                    ui.label(text);
                                }
                            }
                        });
                    }
                });
        }

        if preferences_changed {
            self.processes_preferences.merge_modules = merge_modules;
            self.processes_preferences.show_stack_modules = show_stack_modules;
            self.processes_preferences.wrap_stacks = wrap_stacks;
            self.processes_preferences.stack_font_size = stack_font_size;
            save_processes_preferences(&self.processes_preferences);
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(jump) = actions.open_source {
            self.open_flame_source(jump);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stack_frame_label;

    #[test]
    fn stack_frame_label_optionally_omits_module() {
        assert_eq!(
            stack_frame_label("nginx:nginx_add_socket", false),
            "nginx_add_socket"
        );
        assert_eq!(
            stack_frame_label("libc:std::terminate", false),
            "std::terminate"
        );
        assert_eq!(stack_frame_label("0xffff", false), "0xffff");
        assert_eq!(
            stack_frame_label("nginx:nginx_add_socket", true),
            "nginx:nginx_add_socket"
        );
    }
}
