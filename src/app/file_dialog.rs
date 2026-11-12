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

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy)]
pub(crate) enum NativeFileDialogPurpose {
    OpenCapture,
    /// Baseline capture for the flame-graph comparison (single file).
    CompareCapture,
    LoadSegmentsCsv,
    SourceBinary(u64),
    SourceFile(u64),
    SourcePrefix(u64),
}

#[cfg(not(target_arch = "wasm32"))]
struct FileDialogEntry {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct NativeFileDialog {
    purpose: NativeFileDialogPurpose,
    title: String,
    directory: PathBuf,
    location_input: String,
    extensions: Vec<String>,
    multiple: bool,
    pick_directory: bool,
    entries: Vec<FileDialogEntry>,
    selected: std::collections::BTreeSet<PathBuf>,
    error: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) enum NativeFileDialogOutcome {
    Pending,
    Cancelled,
    Selected {
        purpose: NativeFileDialogPurpose,
        paths: Vec<PathBuf>,
    },
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeFileDialog {
    pub(crate) fn captures() -> Self {
        Self::new(
            NativeFileDialogPurpose::OpenCapture,
            "Open EventLog capture",
            true,
            &["elog", "gz", "csv"],
            None,
        )
    }

    /// Origin segments ground-truth CSVs. Several files load together.
    pub(crate) fn segments_csv() -> Self {
        Self::new(
            NativeFileDialogPurpose::LoadSegmentsCsv,
            "Load Origin Segments CSV",
            true,
            &["csv"],
            None,
        )
    }

    /// Single-select picker for the flame-graph comparison baseline.
    pub(crate) fn compare_capture() -> Self {
        Self::new(
            NativeFileDialogPurpose::CompareCapture,
            "Compare with capture…",
            false,
            &["elog", "gz"],
            None,
        )
    }

    pub(crate) fn source(
        purpose: NativeFileDialogPurpose,
        title: &str,
        current_input: &str,
    ) -> Self {
        let suggested =
            (!current_input.trim().is_empty()).then(|| PathBuf::from(current_input.trim()));
        Self::new(purpose, title, false, &[], suggested)
    }

    pub(crate) fn source_directory(
        purpose: NativeFileDialogPurpose,
        title: &str,
        current_input: &str,
    ) -> Self {
        let mut dialog = Self::source(purpose, title, current_input);
        dialog.pick_directory = true;
        dialog.selected.clear();
        dialog.refresh();
        dialog
    }

    fn new(
        purpose: NativeFileDialogPurpose,
        title: &str,
        multiple: bool,
        extensions: &[&str],
        suggested: Option<PathBuf>,
    ) -> Self {
        let directory = suggested
            .as_deref()
            .and_then(|path| {
                if path.is_dir() {
                    Some(path.to_path_buf())
                } else {
                    path.parent()
                        .filter(|parent| parent.is_dir())
                        .map(PathBuf::from)
                }
            })
            .or_else(dirs::home_dir)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut dialog = Self {
            purpose,
            title: title.to_string(),
            location_input: directory.display().to_string(),
            directory,
            extensions: extensions.iter().map(|ext| ext.to_string()).collect(),
            multiple,
            pick_directory: false,
            entries: Vec::new(),
            selected: std::collections::BTreeSet::new(),
            error: None,
        };
        if let Some(path) = suggested.filter(|path| path.is_file()) {
            dialog.selected.insert(path);
        }
        dialog.refresh();
        dialog
    }

    fn accepts(&self, path: &std::path::Path) -> bool {
        self.extensions.is_empty()
            || path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| self.extensions.iter().any(|allowed| ext == allowed))
    }

    fn refresh(&mut self) {
        self.entries.clear();
        self.error = None;
        match std::fs::read_dir(&self.directory) {
            Ok(entries) => {
                self.entries = entries
                    .filter_map(Result::ok)
                    .filter_map(|entry| {
                        let path = entry.path();
                        let is_dir = path.is_dir();
                        (is_dir || (!self.pick_directory && path.is_file() && self.accepts(&path)))
                            .then(|| FileDialogEntry {
                                name: entry.file_name().to_string_lossy().into_owned(),
                                path,
                                is_dir,
                            })
                    })
                    .collect();
                self.entries.sort_by(|a, b| {
                    b.is_dir
                        .cmp(&a.is_dir)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
            }
            Err(error) => {
                self.error = Some(format!(
                    "Could not read {}: {error}",
                    self.directory.display()
                ));
            }
        }
    }

    fn navigate(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.directory = path;
            self.location_input = self.directory.display().to_string();
            self.selected.clear();
            self.refresh();
        } else {
            self.error = Some(format!("{} is not a directory", path.display()));
        }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> NativeFileDialogOutcome {
        let mut window_open = true;
        let mut cancel = false;
        let mut submit = false;
        let mut navigate_to = None;

        egui::Window::new(&self.title)
            .collapsible(false)
            .resizable(true)
            .default_size([760.0, 560.0])
            .open(&mut window_open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Home").clicked() {
                        navigate_to = dirs::home_dir();
                    }
                    if ui.button("Up").clicked() {
                        navigate_to = self.directory.parent().map(PathBuf::from);
                    }
                    let location = ui.add(
                        egui::TextEdit::singleline(&mut self.location_input)
                            .desired_width((ui.available_width() - 100.0).max(160.0)),
                    );
                    if ui.button("Go").clicked()
                        || (location.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                    {
                        let path = PathBuf::from(self.location_input.trim());
                        if path.is_file() && self.accepts(&path) {
                            self.selected.clear();
                            self.selected.insert(path);
                            submit = true;
                        } else {
                            navigate_to = Some(path);
                        }
                    }
                });

                if let Some(error) = &self.error {
                    ui.colored_label(Color32::from_rgb(230, 100, 90), error);
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_source(("native_file_dialog", self.title.as_str()))
                    .max_height((ui.available_height() - 48.0).max(160.0))
                    .show(ui, |ui| {
                        for entry in &self.entries {
                            if entry.is_dir {
                                let response = ui.selectable_label(
                                    false,
                                    RichText::new(format!("📁 {}", entry.name)).monospace(),
                                );
                                if response.double_clicked() {
                                    navigate_to = Some(entry.path.clone());
                                }
                            } else {
                                let is_selected = self.selected.contains(&entry.path);
                                let response = ui.selectable_label(
                                    is_selected,
                                    RichText::new(&entry.name).monospace(),
                                );
                                if response.clicked() {
                                    if !self.multiple {
                                        self.selected.clear();
                                    }
                                    if is_selected && self.multiple {
                                        self.selected.remove(&entry.path);
                                    } else {
                                        self.selected.insert(entry.path.clone());
                                    }
                                }
                                if response.double_clicked() {
                                    self.selected.clear();
                                    self.selected.insert(entry.path.clone());
                                    submit = true;
                                }
                            }
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    let can_submit = self.pick_directory || !self.selected.is_empty();
                    let submit_label = if self.pick_directory {
                        "Select directory"
                    } else {
                        "Open"
                    };
                    if ui
                        .add_enabled(can_submit, egui::Button::new(submit_label))
                        .clicked()
                    {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if self.multiple {
                        ui.label(format!("{} selected", self.selected.len()));
                    }
                });
            });

        if let Some(path) = navigate_to {
            self.navigate(path);
        }
        if !window_open || cancel {
            NativeFileDialogOutcome::Cancelled
        } else if submit {
            NativeFileDialogOutcome::Selected {
                purpose: self.purpose,
                paths: if self.pick_directory {
                    vec![self.directory.clone()]
                } else {
                    self.selected.iter().cloned().collect()
                },
            }
        } else {
            NativeFileDialogOutcome::Pending
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn capture_browser_lists_directories_and_supported_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("capture.elog"), []).unwrap();
        std::fs::write(root.path().join("capture.elog.gz"), []).unwrap();
        std::fs::write(root.path().join("notes.txt"), []).unwrap();

        let dialog = NativeFileDialog::new(
            NativeFileDialogPurpose::OpenCapture,
            "test",
            true,
            &["elog", "gz"],
            Some(root.path().to_path_buf()),
        );
        let names: Vec<_> = dialog
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert!(names.contains(&"nested"));
        assert!(names.contains(&"capture.elog"));
        assert!(names.contains(&"capture.elog.gz"));
        assert!(!names.contains(&"notes.txt"));
    }

    #[test]
    fn directory_browser_selects_its_current_directory() {
        let root = tempfile::tempdir().unwrap();
        let dialog = NativeFileDialog::source_directory(
            NativeFileDialogPurpose::SourcePrefix(1),
            "test",
            &root.path().display().to_string(),
        );
        assert!(dialog.pick_directory);
        assert_eq!(dialog.directory, root.path());
        assert!(dialog.entries.iter().all(|entry| entry.is_dir));
    }
}
