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

#[derive(Default)]
pub(super) struct CpuFilterUi {
    sampled_cpus: Vec<u16>,
    input: String,
    error: Option<String>,
}

/// Empty / "all" means no CPU constraint, just as in FlameFilter.
fn parse_cpu_selection(input: &str) -> Result<Vec<u16>, String> {
    let input = input.trim();
    if input.is_empty() || input.eq_ignore_ascii_case("all") {
        return Ok(Vec::new());
    }
    let mut selected = vec![false; u16::MAX as usize + 1];
    for term in input.split(',') {
        let term = term.trim();
        let parse_id = |id: &str| {
            id.trim()
                .parse::<u16>()
                .map_err(|_| format!("Invalid CPU ID: {id:?}. Use IDs from 0 to 65535."))
        };
        let (start, end) = match term.split_once('-') {
            Some((start, end)) => (parse_id(start)?, parse_id(end)?),
            None => {
                let id = parse_id(term)?;
                (id, id)
            }
        };
        if start > end {
            return Err(format!("CPU range {term:?} must start with the lower ID."));
        }
        selected[start as usize..=end as usize].fill(true);
    }
    Ok(selected
        .iter()
        .enumerate()
        .filter_map(|(cpu, &selected)| selected.then_some(cpu as u16))
        .collect())
}

/// Compact, editable ranges for the sorted CPU allowlist.
fn format_cpu_selection(cpus: &[u16]) -> String {
    let mut ranges = Vec::new();
    let mut ids = cpus.iter().copied().peekable();
    while let Some(start) = ids.next() {
        let mut end = start;
        while ids
            .peek()
            .is_some_and(|&next| end.checked_add(1) == Some(next))
        {
            end = ids.next().unwrap();
        }
        ranges.push(if start == end {
            start.to_string()
        } else {
            format!("{start}-{end}")
        });
    }
    ranges.join(",")
}

impl CpuFilterUi {
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        profile: &CpuProfile,
        cpus: &mut Vec<u16>,
    ) -> bool {
        let previous = cpus.clone();
        let ranges = format_cpu_selection(cpus);
        let label = match cpus.len() {
            0 => "All CPUs".to_string(),
            1 => format!("CPU {}", cpus[0]),
            n if ranges.len() > 24 => format!("{n} CPUs"),
            _ => format!("CPUs {ranges}"),
        };
        ui.menu_button(label, |ui| {
            ui.set_width(290.0);
            ui.label("Shared by CPU Usage, Top functions and Flame.");
            if ui.selectable_label(cpus.is_empty(), "All CPUs").clicked() {
                cpus.clear();
                self.input.clear();
                self.error = None;
                ui.close_menu();
            }
            ui.separator();
            ui.label("CPU group (IDs and inclusive ranges)");
            ui.horizontal(|ui| {
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.input)
                        .desired_width(205.0)
                        .hint_text("e.g. 0-7,16-23"),
                );
                if input.changed() {
                    self.error = None;
                }
                let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Apply").clicked() || enter {
                    match parse_cpu_selection(&self.input) {
                        Ok(selection) => {
                            *cpus = selection;
                            self.input = format_cpu_selection(cpus);
                            self.error = None;
                            ui.close_menu();
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
            });
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.separator();
            ui.label("Single CPU (sampled in this capture)");
            // Discover once per capture, only when the menu is opened.
            // Do not scan a multi-million-sample profile every UI frame.
            if self.sampled_cpus.is_empty() {
                let mut seen = vec![false; u16::MAX as usize + 1];
                for sample in &profile.samples {
                    seen[sample.cpu as usize] = true;
                }
                self.sampled_cpus = seen
                    .iter()
                    .enumerate()
                    .filter_map(|(cpu, &seen)| seen.then_some(cpu as u16))
                    .collect();
            }
            egui::ScrollArea::vertical()
                .id_source("sample_cpu_choices")
                .max_height(220.0)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for &cpu in &self.sampled_cpus {
                            if ui
                                .selectable_label(cpus.as_slice() == [cpu], format!("CPU {cpu}"))
                                .clicked()
                            {
                                *cpus = vec![cpu];
                                self.input = cpu.to_string();
                                self.error = None;
                                ui.close_menu();
                            }
                        }
                    });
                });
        })
        .response
        .on_hover_text(if cpus.is_empty() {
            "Use samples from every CPU.".to_string()
        } else {
            format!("Use samples from CPUs {ranges}.")
        });
        *cpus != previous
    }
}

impl EventExplorerApp {
    pub(crate) fn show_sample_cpu_filter(&mut self, ui: &mut egui::Ui) {
        let mut cpus = self.current_function_filter().cpus;
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Sample CPUs");
            changed = self.cpu_filter_ui.show(ui, &self.cpu_profile, &mut cpus);
        });
        if !changed {
            return;
        }
        if let Some(flame) = self.flame.as_mut() {
            flame.filter.cpus = cpus;
            Self::start_flame_rebuild(flame, &self.cpu_profile, &mut self.flame_build);
        } else {
            self.pending_flame_filter
                .get_or_insert_with(FlameFilter::default)
                .cpus = cpus;
        }
        // Keep the old view's Top N / sort choices, but cancel any worker
        // using the old CPU set. The full filter comparison in the function
        // view starts a new build and hides stale rankings and drill-downs.
        self.process_functions_build = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{format_cpu_selection, parse_cpu_selection};

    #[test]
    fn cpu_groups_accept_ids_ranges_and_all() {
        assert_eq!(parse_cpu_selection(" 3 ").unwrap(), [3]);
        assert_eq!(
            parse_cpu_selection("0-3, 8, 2-4").unwrap(),
            [0, 1, 2, 3, 4, 8]
        );
        assert!(parse_cpu_selection("").unwrap().is_empty());
        assert!(parse_cpu_selection(" ALL ").unwrap().is_empty());
        assert_eq!(parse_cpu_selection("65534-65535").unwrap(), [65534, 65535]);
    }

    #[test]
    fn invalid_cpu_groups_are_rejected_as_a_whole() {
        for input in ["3-1", "-1", "65536", "1,,2", "1,", "cpu2", "1-", "1-2-3"] {
            assert!(parse_cpu_selection(input).is_err(), "{input}");
        }
    }

    #[test]
    fn compact_cpu_ranges_round_trip() {
        for cpus in [vec![], vec![3], vec![0, 1, 2, 7, 9, 10, 65534, 65535]] {
            assert_eq!(
                parse_cpu_selection(&format_cpu_selection(&cpus)).unwrap(),
                cpus
            );
        }
        assert_eq!(format_cpu_selection(&[0, 1, 2, 8, 9]), "0-2,8-9");
    }
}
