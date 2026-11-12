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

#[derive(Clone)]
pub(crate) struct SourceJump {
    pub pc: u64,
    pub pid: u32,
    pub stack_path: Vec<(u64, u32)>,
}

#[derive(Debug, Clone)]
pub struct SourceOpenRequest {
    pub function: ProfileFunctionId,
    pub function_name: String,
    pub binary_name: String,
    pub captured_path: String,
    pub build_id: String,
    pub sampled_offsets: Vec<(u64, u64)>,
    /// BySymbol flame keys from the root through the clicked function.
    /// Empty when the function was opened from the process table.
    pub stack_path: Vec<(u64, u32)>,
    pub split_user_by_pid: bool,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourcePathRewrite {
    pub(crate) dwarf_prefix: PathBuf,
    pub(crate) local_prefix: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl SourcePathRewrite {
    fn new(dwarf_prefix: &str, local_prefix: &str) -> Option<Self> {
        let dwarf_prefix = dwarf_prefix.trim();
        let local_prefix = local_prefix.trim();
        if dwarf_prefix.is_empty() || local_prefix.is_empty() {
            return None;
        }
        Some(Self {
            dwarf_prefix: PathBuf::from(dwarf_prefix),
            local_prefix: PathBuf::from(local_prefix),
        })
    }

    fn apply(&self, dwarf_path: &std::path::Path) -> Option<PathBuf> {
        let normalized_path = lexical_normalize(dwarf_path);
        let normalized_prefix = lexical_normalize(&self.dwarf_prefix);
        let relative = normalized_path.strip_prefix(normalized_prefix).ok()?;
        Some(self.local_prefix.join(relative))
    }
}

#[cfg(not(target_arch = "wasm32"))]
const MAX_SOURCE_LOOKUP_PREFERENCES: usize = 128;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceLookupPreference {
    binary_name: String,
    captured_path: String,
    build_id: String,
    binary_path: Option<PathBuf>,
    rewrites: Vec<SourcePathRewrite>,
}

#[cfg(not(target_arch = "wasm32"))]
impl SourceLookupPreference {
    fn new(request: &SourceOpenRequest) -> Self {
        Self {
            binary_name: request.binary_name.clone(),
            captured_path: request.captured_path.clone(),
            build_id: request.build_id.clone(),
            binary_path: None,
            rewrites: Vec::new(),
        }
    }

    fn matches_build(&self, request: &SourceOpenRequest) -> bool {
        !self.build_id.is_empty()
            && !request.build_id.is_empty()
            && self.build_id.eq_ignore_ascii_case(&request.build_id)
    }

    fn matches_captured_executable(&self, request: &SourceOpenRequest) -> bool {
        !self.captured_path.is_empty()
            && self.binary_name == request.binary_name
            && self.captured_path == request.captured_path
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn source_lookup_preferences_path() -> Option<PathBuf> {
    dirs::config_dir().map(|directory| directory.join("event_explorer").join("source_lookups.txt"))
}

#[cfg(not(target_arch = "wasm32"))]
fn escape_source_lookup_field(field: &str) -> String {
    let mut escaped = String::with_capacity(field.len());
    for character in field.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(not(target_arch = "wasm32"))]
fn unescape_source_lookup_field(field: &str) -> Option<String> {
    let mut unescaped = String::with_capacity(field.len());
    let mut characters = field.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            unescaped.push(character);
            continue;
        }
        match characters.next()? {
            '\\' => unescaped.push('\\'),
            't' => unescaped.push('\t'),
            'n' => unescaped.push('\n'),
            'r' => unescaped.push('\r'),
            _ => return None,
        }
    }
    Some(unescaped)
}

#[cfg(not(target_arch = "wasm32"))]
fn encode_source_lookup_preference(preference: &SourceLookupPreference) -> String {
    let mut fields = vec![
        "v1".to_string(),
        escape_source_lookup_field(&preference.binary_name),
        escape_source_lookup_field(&preference.captured_path),
        escape_source_lookup_field(&preference.build_id),
        escape_source_lookup_field(
            &preference
                .binary_path
                .as_deref()
                .unwrap_or_else(|| std::path::Path::new(""))
                .to_string_lossy(),
        ),
    ];
    for rewrite in &preference.rewrites {
        fields.push(escape_source_lookup_field(
            &rewrite.dwarf_prefix.to_string_lossy(),
        ));
        fields.push(escape_source_lookup_field(
            &rewrite.local_prefix.to_string_lossy(),
        ));
    }
    fields.join("\t")
}

#[cfg(not(target_arch = "wasm32"))]
fn decode_source_lookup_preference(line: &str) -> Option<SourceLookupPreference> {
    let fields = line
        .split('\t')
        .map(unescape_source_lookup_field)
        .collect::<Option<Vec<_>>>()?;
    if fields.len() < 5 || fields[0] != "v1" || !(fields.len() - 5).is_multiple_of(2) {
        return None;
    }
    let mut rewrites = Vec::new();
    let (pairs, remainder) = fields[5..].as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    for pair in pairs {
        if let Some(rewrite) = SourcePathRewrite::new(&pair[0], &pair[1]) {
            rewrites.push(rewrite);
        }
    }
    Some(SourceLookupPreference {
        binary_name: fields[1].clone(),
        captured_path: fields[2].clone(),
        build_id: fields[3].clone(),
        binary_path: (!fields[4].is_empty()).then(|| PathBuf::from(&fields[4])),
        rewrites,
    })
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn load_source_lookup_preferences() -> Vec<SourceLookupPreference> {
    let Some(path) = source_lookup_preferences_path() else {
        return Vec::new();
    };
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(decode_source_lookup_preference)
        .take(MAX_SOURCE_LOOKUP_PREFERENCES)
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn save_source_lookup_preferences(preferences: &[SourceLookupPreference]) {
    let Some(path) = source_lookup_preferences_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let contents = preferences
        .iter()
        .take(MAX_SOURCE_LOOKUP_PREFERENCES)
        .map(encode_source_lookup_preference)
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(path, contents);
}

#[cfg(not(target_arch = "wasm32"))]
fn lexical_normalize(path: &std::path::Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(std::path::Component::Normal(_))
                ) {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push(component.as_os_str());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[cfg(not(target_arch = "wasm32"))]
fn rewrite_source_path(
    dwarf_path: &std::path::Path,
    rewrites: &[SourcePathRewrite],
) -> Option<PathBuf> {
    rewrites
        .iter()
        .filter_map(|rewrite| {
            rewrite
                .apply(dwarf_path)
                .map(|path| (rewrite.dwarf_prefix.components().count(), path))
        })
        .max_by_key(|(components, _)| *components)
        .map(|(_, path)| path)
}

#[cfg(not(target_arch = "wasm32"))]
fn default_source_rewrite<'a>(
    dwarf_path: &std::path::Path,
    accepted: &'a [SourcePathRewrite],
    saved: &'a [SourcePathRewrite],
) -> Option<&'a SourcePathRewrite> {
    accepted
        .iter()
        .chain(saved)
        .find(|rewrite| rewrite.apply(dwarf_path).is_some())
}

#[cfg(not(target_arch = "wasm32"))]
fn infer_source_rewrite(
    dwarf_path: &std::path::Path,
    local_binary: &std::path::Path,
) -> Option<SourcePathRewrite> {
    let ignored = [
        "bin", "build", "data", "home", "mnt", "obj", "src", "tmp", "usr", "work",
    ];
    let dwarf_parts: Vec<_> = dwarf_path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    let local_parts: Vec<_> = local_binary
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    let (dwarf_index, local_index) = dwarf_parts.iter().enumerate().find_map(|(di, part)| {
        let name = part.to_string_lossy();
        (!ignored.contains(&name.as_ref())).then(|| {
            local_parts
                .iter()
                .position(|candidate| candidate == part)
                .map(|li| (di, li))
        })?
    })?;

    let prefix_before = |path: &std::path::Path, normal_count: usize| {
        let mut prefix = PathBuf::new();
        let mut seen = 0;
        for component in path.components() {
            if matches!(component, std::path::Component::Normal(_)) {
                if seen == normal_count {
                    break;
                }
                seen += 1;
            }
            prefix.push(component.as_os_str());
        }
        prefix
    };
    let dwarf_prefix = prefix_before(dwarf_path, dwarf_index);
    let local_prefix = prefix_before(local_binary, local_index);
    (!dwarf_prefix.as_os_str().is_empty() && !local_prefix.as_os_str().is_empty()).then_some(
        SourcePathRewrite {
            dwarf_prefix,
            local_prefix,
        },
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn prefix_before_dot_obj(path: &std::path::Path) -> Option<PathBuf> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        if component.as_os_str().to_string_lossy().starts_with(".obj") {
            return (!prefix.as_os_str().is_empty()).then_some(prefix);
        }
        prefix.push(component.as_os_str());
    }
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn dwarf_prefix_for_paths(
    dwarf_path: &std::path::Path,
    local_binary: &std::path::Path,
) -> Option<PathBuf> {
    prefix_before_dot_obj(local_binary)
        .filter(|prefix| dwarf_path.starts_with(prefix))
        .or_else(|| prefix_before_dot_obj(dwarf_path))
}

#[cfg(not(target_arch = "wasm32"))]
const NGINX_PORTS_LOCAL_SUFFIX: &str = "ports/netflix/nginx/files/nginx";

#[cfg(not(target_arch = "wasm32"))]
fn nginx_ports_dwarf_prefix(path: &std::path::Path) -> Option<PathBuf> {
    let parts: Vec<_> = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    if !path.is_absolute()
        || parts.len() < 7
        || parts[0] != "wrkdirs"
        || parts[1] != "usr"
        || parts[2] != "ports"
        || parts[3] != "netflix"
        || parts[4] != "nginx"
        || !parts[5].to_string_lossy().starts_with("work")
        || !parts[6].to_string_lossy().starts_with("nginx-")
    {
        return None;
    }

    Some(std::path::Path::new("/").join(parts[..7].iter().collect::<PathBuf>()))
}

#[cfg(not(target_arch = "wasm32"))]
fn checkout_root_from_nginx_local_prefix(path: &std::path::Path) -> Option<PathBuf> {
    let suffix = std::path::Path::new(NGINX_PORTS_LOCAL_SUFFIX);
    if !path.ends_with(suffix) {
        return None;
    }
    let keep = path.components().count() - suffix.components().count();
    Some(path.components().take(keep).collect())
}

#[cfg(not(target_arch = "wasm32"))]
fn source_rewrite_from_inputs(
    dwarf_path: &std::path::Path,
    dwarf_prefix: &str,
    local_input: &str,
) -> Option<SourcePathRewrite> {
    let mut rewrite = SourcePathRewrite::new(dwarf_prefix, local_input)?;
    if nginx_ports_dwarf_prefix(dwarf_path).is_some() {
        rewrite.local_prefix = rewrite.local_prefix.join(NGINX_PORTS_LOCAL_SUFFIX);
    }
    Some(rewrite)
}

#[cfg(not(target_arch = "wasm32"))]
fn guess_checkout_root_from_kernel_rewrites(
    rewrites: &rustc_hash::FxHashMap<ProfileBinaryId, Vec<SourcePathRewrite>>,
) -> Option<PathBuf> {
    rewrites
        .iter()
        .filter(|(binary, _)| matches!(binary, ProfileBinaryId::Kernel(_)))
        .flat_map(|(_, rewrites)| rewrites.iter().rev())
        .map(|rewrite| rewrite.local_prefix.clone())
        .find(|root| root.join(NGINX_PORTS_LOCAL_SUFFIX).is_dir())
}

#[cfg(not(target_arch = "wasm32"))]
fn guess_checkout_root_from_saved_preferences(
    preferences: &[SourceLookupPreference],
) -> Option<PathBuf> {
    preferences
        .iter()
        .flat_map(|preference| preference.rewrites.iter().rev())
        .map(|rewrite| rewrite.local_prefix.clone())
        .find(|root| root.join(NGINX_PORTS_LOCAL_SUFFIX).is_dir())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn infer_source_rewrite_from_files(
    dwarf_path: &std::path::Path,
    local_path: &std::path::Path,
) -> Option<SourcePathRewrite> {
    if dwarf_path == local_path {
        return None;
    }
    let dwarf_parts: Vec<_> = dwarf_path.components().collect();
    let local_parts: Vec<_> = local_path.components().collect();
    let common_suffix = dwarf_parts
        .iter()
        .rev()
        .zip(local_parts.iter().rev())
        .take_while(|(dwarf, local)| dwarf == local)
        .count();
    if common_suffix == 0 {
        return None;
    }
    let dwarf_prefix: PathBuf = dwarf_parts[..dwarf_parts.len() - common_suffix]
        .iter()
        .map(|component| component.as_os_str())
        .collect();
    let local_prefix: PathBuf = local_parts[..local_parts.len() - common_suffix]
        .iter()
        .map(|component| component.as_os_str())
        .collect();
    (!dwarf_prefix.as_os_str().is_empty() && !local_prefix.as_os_str().is_empty()).then_some(
        SourcePathRewrite {
            dwarf_prefix,
            local_prefix,
        },
    )
}

fn selectable_message(ui: &mut egui::Ui, message: &str, color: Option<Color32>) {
    let text = color.map_or_else(
        || RichText::new(message),
        |color| RichText::new(message).color(color),
    );
    ui.add(egui::Label::new(text).selectable(true).wrap(true));
}

/// What the docked source panel does with a user gesture.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) enum SourcePanelAction {
    /// No gesture this frame.
    None,
    /// Open annotated source for this frame (caller/callee click).
    Open(SourceJump),
    /// A sampled source line was clicked. The callstack view reads it.
    FocusLine,
}

#[cfg(not(target_arch = "wasm32"))]
struct StackFrame {
    addr: u64,
    pid: u32,
    label: String,
    function: Option<ProfileFunctionId>,
}

#[cfg(not(target_arch = "wasm32"))]
struct CalleeRow {
    addr: u64,
    pid: u32,
    label: String,
    count: u64,
    function: Option<ProfileFunctionId>,
}

#[cfg(not(target_arch = "wasm32"))]
struct LineDetailCache {
    path: Vec<(u64, u32)>,
    frames: Vec<StackFrame>,
    function_index: usize,
    callees: Vec<CalleeRow>,
    self_count: u64,
    sample_count: u64,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct AnnotatedSource {
    source_path: PathBuf,
    /// `DWARF: <binary>` for a local image, or an `https://` blob URL.
    origin: String,
    /// Set when the firmware blob is `master` or a dirty build's commit.
    ref_note: Option<String>,
    lines: Vec<String>,
    line_samples: rustc_hash::FxHashMap<u32, u64>,
    function_line_range: Option<(u32, u32)>,
    hottest_line: u32,
    total_mapped: u64,
    function_total: u64,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
enum SourceLoadResult {
    Ready(AnnotatedSource),
    NeedSource {
        message: String,
        binary_path: PathBuf,
        expected_path: PathBuf,
        detected_prefix: Option<PathBuf>,
        line_samples: rustc_hash::FxHashMap<u32, u64>,
        line_offsets: rustc_hash::FxHashMap<u32, Vec<u64>>,
        function_name: String,
    },
    NeedBinary(String),
    Error(String),
}

enum SourceLoadState {
    #[cfg(not(target_arch = "wasm32"))]
    Loading(mpsc::Receiver<SourceLoadResult>),
    #[cfg(not(target_arch = "wasm32"))]
    NeedBinary(String),
    #[cfg(not(target_arch = "wasm32"))]
    NeedSource {
        message: String,
        binary_path: PathBuf,
        expected_path: PathBuf,
        detected_prefix: Option<PathBuf>,
        line_samples: rustc_hash::FxHashMap<u32, u64>,
        line_offsets: rustc_hash::FxHashMap<u32, Vec<u64>>,
        function_name: String,
    },
    #[cfg(not(target_arch = "wasm32"))]
    Ready(AnnotatedSource),
    Error(String),
}

pub struct SourceViewState {
    pub(crate) id: u64,
    pub(crate) request: SourceOpenRequest,
    state: SourceLoadState,
    #[cfg(not(target_arch = "wasm32"))]
    focused_source_line: Option<u32>,
    #[cfg(not(target_arch = "wasm32"))]
    pending_source_scroll: Option<u32>,
    #[cfg(not(target_arch = "wasm32"))]
    profile: Arc<CpuProfile>,
    #[cfg(not(target_arch = "wasm32"))]
    filter: FlameFilter,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) binary_path_input: String,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) source_path_input: String,
    #[cfg(not(target_arch = "wasm32"))]
    rewrite_from_input: String,
    #[cfg(not(target_arch = "wasm32"))]
    rewrite_to_input: String,
    #[cfg(not(target_arch = "wasm32"))]
    cached_line_detail: Option<LineDetailCache>,
}

#[cfg(not(target_arch = "wasm32"))]
enum LocalPathAction {
    Load(PathBuf),
    Browse,
}

#[cfg(not(target_arch = "wasm32"))]
fn local_path_picker(
    ui: &mut egui::Ui,
    input: &mut String,
    browse_label: &str,
) -> Option<LocalPathAction> {
    let mut selected = None;
    ui.horizontal(|ui| {
        let response = ui.add(
            egui::TextEdit::singleline(input)
                .hint_text("Enter a local filesystem path")
                .desired_width(420.0),
        );
        let has_path = !input.trim().is_empty();
        let enter = has_path
            && response.lost_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if ui
            .add_enabled(has_path, egui::Button::new("Load path"))
            .clicked()
            || enter
        {
            selected = Some(LocalPathAction::Load(PathBuf::from(input.trim())));
        }
        if ui.button(browse_label).clicked() {
            selected = Some(LocalPathAction::Browse);
        }
    });
    selected
}

#[cfg(not(target_arch = "wasm32"))]
fn sampled_lines_hottest_first(line_samples: &rustc_hash::FxHashMap<u32, u64>) -> Vec<u32> {
    let mut lines: Vec<u32> = line_samples
        .iter()
        .filter(|(_, samples)| **samples > 0)
        .map(|(&line, _)| line)
        .collect();
    lines.sort_by(|&left, &right| {
        line_samples[&right]
            .cmp(&line_samples[&left])
            .then(left.cmp(&right))
    });
    lines
}

#[cfg(not(target_arch = "wasm32"))]
fn hotter_sampled_line(order: &[u32], current: u32) -> Option<u32> {
    let index = order.iter().position(|&line| line == current)?;
    (index > 0).then(|| order[index - 1])
}

#[cfg(not(target_arch = "wasm32"))]
fn colder_sampled_line(order: &[u32], current: u32) -> Option<u32> {
    match order.iter().position(|&line| line == current) {
        Some(index) if index + 1 < order.len() => Some(order[index + 1]),
        Some(_) => None,
        None => order.first().copied(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn centered_source_scroll_offset(
    line: u32,
    row_height_with_spacing: f32,
    viewport_height: f32,
) -> f32 {
    let row_center =
        line.saturating_sub(1) as f32 * row_height_with_spacing + row_height_with_spacing * 0.5;
    (row_center - viewport_height * 0.5).max(0.0)
}

#[cfg(not(target_arch = "wasm32"))]
fn source_line_navigation_button(
    ui: &mut egui::Ui,
    enabled: bool,
    points_left: bool,
) -> egui::Response {
    let size = egui::vec2(ui.spacing().interact_size.y, ui.spacing().interact_size.y);
    let response = ui.add_enabled(enabled, egui::Button::new("").min_size(size));
    let color = ui.style().interact(&response).fg_stroke.color;
    let stroke = Stroke::new(1.5_f32, color);
    let center = response.rect.center();
    let half_width = 5.0;
    let head = 3.5;
    let tip_x = if points_left {
        center.x - half_width
    } else {
        center.x + half_width
    };
    let tail_x = if points_left {
        center.x + half_width
    } else {
        center.x - half_width
    };
    let head_x = if points_left {
        tip_x + head
    } else {
        tip_x - head
    };
    ui.painter().line_segment(
        [egui::pos2(tail_x, center.y), egui::pos2(tip_x, center.y)],
        stroke,
    );
    ui.painter().line_segment(
        [
            egui::pos2(head_x, center.y - head),
            egui::pos2(tip_x, center.y),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            egui::pos2(head_x, center.y + head),
            egui::pos2(tip_x, center.y),
        ],
        stroke,
    );
    response
}

#[cfg(not(target_arch = "wasm32"))]
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn line_highlight_alpha(count: u64, max_line_samples: u64, function_total: u64) -> u8 {
    if count == 0 {
        return 0;
    }
    let line_frac = count as f32 / max_line_samples.max(1) as f32;
    let weight = (function_total as f32 / 100.0).clamp(0.12, 1.0);
    let base_alpha = 14.0 * weight;
    let range_alpha = 136.0 * weight;
    (base_alpha + range_alpha * line_frac).clamp(8.0, 150.0) as u8
}

fn find_ident_call(haystack: &str, name: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(rel) = haystack[search_from..].find(name) {
        let pos = search_from + rel;
        let before_ok = if pos == 0 {
            true
        } else {
            let prev = haystack[..pos].chars().last().unwrap();
            !prev.is_alphanumeric() && prev != '_'
        };
        let after_idx = pos + name.len();
        let after = &haystack[after_idx..];
        let trimmed_after = after.trim_start();
        let after_ok = trimmed_after.starts_with('(');
        if before_ok && after_ok {
            return Some(pos);
        }
        search_from = pos + name.len();
    }
    None
}

fn count_braces(s: &str) -> i32 {
    let mut count = 0i32;
    let mut in_str = false;
    let mut in_char = false;
    let mut prev_esc = false;
    for c in s.chars() {
        if in_str {
            if c == '"' && !prev_esc {
                in_str = false;
            }
            prev_esc = c == '\\' && !prev_esc;
        } else if in_char {
            if c == '\'' && !prev_esc {
                in_char = false;
            }
            prev_esc = c == '\\' && !prev_esc;
        } else {
            if c == '"' {
                in_str = true;
            } else if c == '\'' {
                in_char = true;
            } else if c == '{' {
                count += 1;
            } else if c == '}' {
                count -= 1;
            }
            prev_esc = false;
        }
    }
    count
}

fn find_function_in_lines(lines: &[String], name: &str) -> Option<(u32, u32)> {
    let mut in_body = false;
    let mut depth: i32 = 0;
    let mut start_line: u32 = 0;
    let mut in_block_comment = false;

    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        let mut clean = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if in_block_comment {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    in_block_comment = false;
                }
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                in_block_comment = true;
            } else if c == '/' && chars.peek() == Some(&'/') {
                break;
            } else {
                clean.push(c);
            }
        }

        if !in_body {
            let trimmed = clean.trim();
            if !trimmed.starts_with('#') && find_ident_call(&clean, name).is_some() {
                let mut j = i;
                let mut is_proto = false;
                let mut lookahead_comment = in_block_comment;

                while j < lines.len() {
                    let l = &lines[j];
                    let mut look_clean = String::new();
                    let mut l_chars = l.chars().peekable();
                    while let Some(lc) = l_chars.next() {
                        if lookahead_comment {
                            if lc == '*' && l_chars.peek() == Some(&'/') {
                                l_chars.next();
                                lookahead_comment = false;
                            }
                        } else if lc == '/' && l_chars.peek() == Some(&'*') {
                            l_chars.next();
                            lookahead_comment = true;
                        } else if lc == '/' && l_chars.peek() == Some(&'/') {
                            break;
                        } else {
                            look_clean.push(lc);
                        }
                    }

                    if look_clean.contains('{') {
                        start_line = (i + 1) as u32;
                        in_body = true;
                        depth = count_braces(&look_clean);
                        i = j;
                        break;
                    }
                    if look_clean.contains(';') {
                        is_proto = true;
                        break;
                    }
                    j += 1;
                }
                if is_proto {
                    i = j + 1;
                    continue;
                }
            }
        } else {
            depth += count_braces(&clean);
            if depth <= 0 {
                let end_line = (i + 1) as u32;
                return Some((start_line, end_line));
            }
        }
        i += 1;
    }
    None
}

fn function_name_candidates(function_name: &str) -> Vec<&str> {
    let mut candidates = Vec::new();
    let trimmed = function_name.trim();
    if trimmed.is_empty() {
        return candidates;
    }
    candidates.push(trimmed);
    let ununderscored = trimmed.trim_start_matches('_');
    if !ununderscored.is_empty() && ununderscored != trimmed {
        candidates.push(ununderscored);
    }
    if let Some(rest) = trimmed.strip_prefix("tcp_ngen_") {
        candidates.push(rest);
        if let Some((_, after_ver)) = rest.split_once('_') {
            if !after_ver.is_empty() {
                candidates.push(after_ver);
            }
        }
    }
    for prefix in ["rack_", "ctf_", "bbr_", "hpts_", "lro_"] {
        if let Some(idx) = trimmed.find(prefix) {
            let suffix = &trimmed[idx..];
            if !candidates.contains(&suffix) {
                candidates.push(suffix);
            }
        }
    }
    candidates
}

#[cfg(test)]
fn find_function_line_range(lines: &[String], function_name: &str) -> Option<(u32, u32)> {
    let candidates = function_name_candidates(function_name);
    function_line_range_for_samples(lines, &candidates, &rustc_hash::FxHashMap::default())
}

fn function_line_range_for_samples(
    lines: &[String],
    candidates: &[&str],
    line_samples: &rustc_hash::FxHashMap<u32, u64>,
) -> Option<(u32, u32)> {
    let mut first = None;
    let mut best: Option<(u32, u32, u64)> = None;
    for candidate in candidates {
        let Some((start, end)) = find_function_in_lines(lines, candidate) else {
            continue;
        };
        if first.is_none() {
            first = Some((start, end));
        }
        let inside: u64 = line_samples
            .iter()
            .filter(|(&line, _)| line >= start && line <= end)
            .map(|(_, count)| *count)
            .sum();
        if inside > best.map(|(_, _, samples)| samples).unwrap_or(0) {
            best = Some((start, end, inside));
        }
    }
    if let Some((start, end, inside)) = best {
        if inside > 0 {
            return Some((start, end));
        }
    }
    first
}

#[cfg(not(target_arch = "wasm32"))]
fn build_annotated_source(
    source_path: PathBuf,
    origin: String,
    ref_note: Option<String>,
    lines: Vec<String>,
    line_samples: rustc_hash::FxHashMap<u32, u64>,
    _line_offsets: rustc_hash::FxHashMap<u32, Vec<u64>>,
    function_name: &str,
) -> AnnotatedSource {
    let total_mapped: u64 = line_samples.values().sum();
    let candidates = function_name_candidates(function_name);
    let mut function_line_range =
        function_line_range_for_samples(&lines, &candidates, &line_samples);
    let function_total: u64 = if let Some((start, end)) = function_line_range {
        let inside: u64 = line_samples
            .iter()
            .filter(|(&line, _)| line >= start && line <= end)
            .map(|(_, count)| *count)
            .sum();
        if inside == 0 && total_mapped > 0 {
            function_line_range = None;
            total_mapped
        } else {
            inside
        }
    } else {
        total_mapped
    };
    let hottest_line = if !line_samples.is_empty() {
        line_samples
            .iter()
            .max_by_key(|&(line, count)| (*count, std::cmp::Reverse(*line)))
            .map(|(&line, _)| line)
            .unwrap_or(1)
    } else if let Some((start, _)) = function_line_range {
        start
    } else {
        1
    };
    AnnotatedSource {
        source_path,
        origin,
        ref_note,
        lines,
        line_samples,
        function_line_range,
        hottest_line,
        total_mapped,
        function_total,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn read_source(
    path: PathBuf,
    origin: String,
    line_samples: rustc_hash::FxHashMap<u32, u64>,
    line_offsets: rustc_hash::FxHashMap<u32, Vec<u64>>,
    function_name: &str,
) -> Result<AnnotatedSource, String> {
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("Could not read source {}: {e}", path.display()))?;
    let lines = text.lines().map(str::to_owned).collect();
    Ok(build_annotated_source(
        path,
        origin,
        None,
        lines,
        line_samples,
        line_offsets,
        function_name,
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn source_binary(binary: ProfileBinaryId) -> (u8, u32) {
    match binary {
        ProfileBinaryId::Kernel(id) => (crate::elog_parser::SOURCE_BINARY_KIND_LINKER, id),
        ProfileBinaryId::User(id) => (crate::elog_parser::SOURCE_BINARY_KIND_USER, id),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn embedded_source_for_binary(profile: &CpuProfile, binary: ProfileBinaryId) -> bool {
    let key = source_binary(binary);
    profile.source_lines.contains_key(&key)
}

#[cfg(not(target_arch = "wasm32"))]
fn dwarf_origin(binary_path: &std::path::Path) -> String {
    format!("DWARF: {}", binary_path.display())
}

#[cfg(not(target_arch = "wasm32"))]
fn need_binary_message(profile: &CpuProfile, request: &SourceOpenRequest) -> String {
    let (kind, id) = source_binary(request.function.binary);
    let this_has = profile.source_lines.contains_key(&(kind, id));
    let mut message = if !profile.source_files.is_empty() && !this_has {
        format!(
            "No embedded source lines for {}. {} was recorded as a symbol only, so there is no GitHub file to open.",
            request.binary_name, request.function_name
        )
    } else if this_has {
        format!(
            "No embedded source line matched any sampled instruction in {}.",
            request.function_name
        )
    } else {
        "Select the local executable or debug file for this function.".to_string()
    };
    if !request.captured_path.is_empty() {
        message.push_str(&format!(
            " The captured target path {} is informational and is not searched on this host.",
            request.captured_path
        ));
    }
    message
}

#[cfg(not(target_arch = "wasm32"))]
fn load_github_source(
    request: &SourceOpenRequest,
    profile: &CpuProfile,
    firmware_commit: &str,
    ref_note: Option<String>,
) -> SourceLoadResult {
    let (kind, binary_id) = source_binary(request.function.binary);
    let mut by_file: rustc_hash::FxHashMap<u32, rustc_hash::FxHashMap<u32, u64>> =
        rustc_hash::FxHashMap::default();
    let mut file_line_offsets: rustc_hash::FxHashMap<u32, rustc_hash::FxHashMap<u32, Vec<u64>>> =
        rustc_hash::FxHashMap::default();
    for &(offset, count) in &request.sampled_offsets {
        let Some((file, line)) = profile.embedded_source_line(kind, binary_id, offset) else {
            continue;
        };
        if line == 0 {
            continue;
        }
        *by_file
            .entry(file.source_file_id)
            .or_default()
            .entry(line)
            .or_default() += count;
        file_line_offsets
            .entry(file.source_file_id)
            .or_default()
            .entry(line)
            .or_default()
            .push(offset);
    }
    let mut ranked: Vec<_> = by_file.into_iter().collect();
    ranked.sort_by(|left, right| {
        let total = |lines: &rustc_hash::FxHashMap<u32, u64>| lines.values().copied().sum::<u64>();
        total(&right.1)
            .cmp(&total(&left.1))
            .then(left.0.cmp(&right.0))
    });
    if ranked.is_empty() {
        return SourceLoadResult::NeedBinary(need_binary_message(profile, request));
    }
    let hint = source_repo_hint(profile, kind, binary_id, firmware_commit);
    let mut best_unscoped = None;
    let mut last_error = None;
    for (file_id, line_samples) in ranked.into_iter().take(6) {
        let Some(file) = profile.source_files.get(&file_id) else {
            last_error = Some(format!(
                "SOURCE_LINE rows for {} reference source file {file_id}, which has no SOURCE_FILE",
                request.function_name
            ));
            continue;
        };
        let line_offsets = file_line_offsets.remove(&file_id).unwrap_or_default();
        let (text, blob) = match fetch_source_text(file, firmware_commit, hint.as_ref()) {
            Ok(loaded) => loaded,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        let note = super::github_source::is_firmware_blob(&blob)
            .then(|| ref_note.clone())
            .flatten();
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let source = build_annotated_source(
            PathBuf::from(&file.pathname),
            blob.url(),
            note,
            lines,
            line_samples,
            line_offsets,
            &request.function_name,
        );
        if source.function_line_range.is_some() && source.function_total > 0 {
            return SourceLoadResult::Ready(source);
        }
        let mapped = source.total_mapped;
        if best_unscoped
            .as_ref()
            .is_none_or(|existing: &AnnotatedSource| existing.total_mapped < mapped)
        {
            best_unscoped = Some(source);
        }
    }
    if let Some(source) = best_unscoped {
        return SourceLoadResult::Ready(source);
    }
    SourceLoadResult::Error(last_error.unwrap_or_else(|| {
        format!(
            "Embedded source for {} has no GitHub mapping.",
            request.function_name
        )
    }))
}

#[cfg(not(target_arch = "wasm32"))]
fn source_repo_hint(
    profile: &CpuProfile,
    kind: u8,
    binary_id: u32,
    firmware_commit: &str,
) -> Option<super::github_source::GithubBlob> {
    profile.source_files.values().find_map(|file| {
        if file.binary_kind != kind || file.binary_id != binary_id {
            return None;
        }
        super::github_source::resolve_vendored_port(&file.pathname, "", firmware_commit).or_else(
            || super::github_source::resolve_github_blob(&file.pathname, "", firmware_commit),
        )
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_source_text(
    file: &crate::elog_parser::EmbeddedSourceFile,
    firmware_commit: &str,
    hint: Option<&super::github_source::GithubBlob>,
) -> Result<(String, super::github_source::GithubBlob), String> {
    let blob = match super::github_source::resolve_vendored_port(
        &file.pathname,
        &file.commit_id,
        firmware_commit,
    )
    .or_else(|| {
        super::github_source::resolve_github_blob(&file.pathname, &file.commit_id, firmware_commit)
    }) {
        Some(blob) => blob,
        None => blob_from_repo_hint(file, hint)?,
    };
    match super::github_source::fetch_github_blob(&blob) {
        Ok(text) => Ok((text, blob)),
        Err(error) => {
            if file.commit_id.trim().is_empty() {
                return Err(error);
            }
            let Some(fallback) =
                super::github_source::resolve_vendored_port(&file.pathname, "", firmware_commit)
                    .or_else(|| {
                        super::github_source::resolve_github_blob(
                            &file.pathname,
                            "",
                            firmware_commit,
                        )
                    })
            else {
                return Err(error);
            };
            if fallback.git_ref == blob.git_ref {
                return Err(error);
            }
            super::github_source::fetch_github_blob(&fallback).map(|text| (text, fallback))
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn blob_from_repo_hint(
    file: &crate::elog_parser::EmbeddedSourceFile,
    hint: Option<&super::github_source::GithubBlob>,
) -> Result<super::github_source::GithubBlob, String> {
    let unmapped = format!("Embedded source {} has no GitHub mapping.", file.pathname);
    let Some(hint) = hint else {
        return Err(unmapped);
    };
    if file.pathname.contains('/') && !file.pathname.starts_with('/') {
        let path = file.pathname.trim_start_matches("./");
        if !path.is_empty() {
            return Ok(super::github_source::GithubBlob {
                host: hint.host,
                owner: hint.owner,
                repo: hint.repo,
                git_ref: hint.git_ref.clone(),
                path: path.to_string(),
            });
        }
    }
    let filename = file
        .pathname
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(file.pathname.as_str());
    super::github_source::search_repo_filename(hint, filename).ok_or(unmapped)
}

#[cfg(not(target_arch = "wasm32"))]
fn load_source(
    request: &SourceOpenRequest,
    binary_path: PathBuf,
    source_rewrites: &[SourcePathRewrite],
) -> SourceLoadResult {
    use gimli::{DwarfSections, EndianSlice, RunTimeEndian, SectionId};
    use object::{Object, ObjectSection, ObjectSymbol, SymbolKind};
    use std::borrow::Cow;

    let data = match std::fs::read(&binary_path) {
        Ok(data) => data,
        Err(e) => {
            return SourceLoadResult::Error(format!(
                "Could not read {}: {e}",
                binary_path.display()
            ))
        }
    };
    let object = match object::File::parse(data.as_slice()) {
        Ok(object) => object,
        Err(e) => return SourceLoadResult::Error(format!("Invalid object file: {e}")),
    };
    if !request.build_id.is_empty() {
        match object.build_id() {
            Ok(Some(local)) if hex(local).eq_ignore_ascii_case(&request.build_id) => {}
            Ok(Some(local)) => {
                return SourceLoadResult::Error(format!(
                    "Build-id mismatch for {}: capture {}, local {}",
                    binary_path.display(),
                    request.build_id,
                    hex(local)
                ))
            }
            Ok(None) => {
                return SourceLoadResult::Error(format!(
                    "{} has no build id; expected {}",
                    binary_path.display(),
                    request.build_id
                ))
            }
            Err(e) => return SourceLoadResult::Error(format!("Could not read build id: {e}")),
        }
    }

    let symbol_address = object
        .symbols()
        .chain(object.dynamic_symbols())
        .filter(|s| s.kind() == SymbolKind::Text)
        .find_map(|s| (s.name().ok()? == request.function_name).then_some(s.address()));
    let Some(function_address) = symbol_address else {
        return SourceLoadResult::Error(format!(
            "{} has no text symbol named {}",
            binary_path.display(),
            request.function_name
        ));
    };

    let endian = if object.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };
    let dwarf_cow =
        match DwarfSections::load(|id: SectionId| -> Result<Cow<'_, [u8]>, gimli::Error> {
            Ok(match object.section_by_name(id.name()) {
                Some(section) => section.uncompressed_data().unwrap_or(Cow::Borrowed(&[])),
                None => Cow::Borrowed(&[]),
            })
        }) {
            Ok(dwarf) => dwarf,
            Err(e) => return SourceLoadResult::Error(format!("Could not load DWARF: {e}")),
        };
    let dwarf = dwarf_cow.borrow(|section| EndianSlice::new(section, endian));
    let context = match addr2line::Context::from_dwarf(dwarf) {
        Ok(context) => context,
        Err(e) => return SourceLoadResult::Error(format!("Could not index DWARF: {e}")),
    };

    let mut by_file: rustc_hash::FxHashMap<PathBuf, rustc_hash::FxHashMap<u32, u64>> =
        rustc_hash::FxHashMap::default();
    let mut file_line_offsets: rustc_hash::FxHashMap<
        PathBuf,
        rustc_hash::FxHashMap<u32, Vec<u64>>,
    > = rustc_hash::FxHashMap::default();
    let fallback = [(request.function.offset, 0)];
    let sampled_offsets = if request.sampled_offsets.is_empty() {
        fallback.as_slice()
    } else {
        request.sampled_offsets.as_slice()
    };
    for &(sampled_offset, count) in sampled_offsets {
        let address = function_address.wrapping_add(sampled_offset - request.function.offset);
        let Ok(Some(location)) = context.find_location(address) else {
            continue;
        };
        let (Some(file), Some(line)) = (location.file, location.line) else {
            continue;
        };
        let file_path = PathBuf::from(file);
        *by_file
            .entry(file_path.clone())
            .or_default()
            .entry(line)
            .or_default() += count;
        file_line_offsets
            .entry(file_path)
            .or_default()
            .entry(line)
            .or_default()
            .push(sampled_offset);
    }
    let Some((source_path, line_samples)) = by_file
        .into_iter()
        .max_by_key(|(_, lines)| lines.values().copied().max().unwrap_or(0))
    else {
        return SourceLoadResult::Error(format!(
            "No sampled instruction in {} mapped to a DWARF source line",
            request.function_name
        ));
    };
    let line_offsets = file_line_offsets.remove(&source_path).unwrap_or_default();
    let local_source_path =
        rewrite_source_path(&source_path, source_rewrites).unwrap_or_else(|| source_path.clone());
    if !local_source_path.is_file() {
        let detected_prefix = dwarf_prefix_for_paths(&source_path, &binary_path);
        let rewrite_note = (local_source_path != source_path).then(|| {
            format!(
                " The configured rewrite resolves it to {}, which is also unavailable.",
                local_source_path.display()
            )
        });
        return SourceLoadResult::NeedSource {
            message: format!(
                "DWARF names {}, but that source path is not available locally.{}",
                source_path.display(),
                rewrite_note.as_deref().unwrap_or_default()
            ),
            binary_path,
            expected_path: source_path,
            detected_prefix,
            line_samples,
            line_offsets,
            function_name: request.function_name.clone(),
        };
    }
    let origin = dwarf_origin(&binary_path);
    match read_source(
        local_source_path,
        origin,
        line_samples,
        line_offsets,
        &request.function_name,
    ) {
        Ok(source) => SourceLoadResult::Ready(source),
        Err(e) => SourceLoadResult::Error(e),
    }
}

impl SourceViewState {
    pub(crate) fn new(
        id: u64,
        request: SourceOpenRequest,
        profile: Arc<CpuProfile>,
        filter: FlameFilter,
        firmware_commit: String,
        ref_note: Option<String>,
    ) -> Self {
        #[cfg(target_arch = "wasm32")]
        let _ = (profile, filter, firmware_commit, ref_note);
        #[cfg(not(target_arch = "wasm32"))]
        let state = if embedded_source_for_binary(&profile, request.function.binary) {
            let (tx, rx) = mpsc::channel();
            let mut request_bg = request.clone();
            let profile_bg = profile.clone();
            let filter_bg = filter.clone();
            std::thread::spawn(move || {
                if request_bg.sampled_offsets.is_empty() {
                    request_bg.sampled_offsets = analysis::function_sampled_offsets(
                        &profile_bg,
                        &filter_bg,
                        request_bg.function,
                    );
                }
                let _ = tx.send(load_github_source(
                    &request_bg,
                    &profile_bg,
                    &firmware_commit,
                    ref_note,
                ));
            });
            SourceLoadState::Loading(rx)
        } else {
            SourceLoadState::NeedBinary(need_binary_message(&profile, &request))
        };
        #[cfg(target_arch = "wasm32")]
        let state = SourceLoadState::Error(
            "Annotated source requires local ELF/DWARF and source files; use the desktop build."
                .to_string(),
        );
        Self {
            id,
            request,
            state,
            #[cfg(not(target_arch = "wasm32"))]
            focused_source_line: None,
            #[cfg(not(target_arch = "wasm32"))]
            pending_source_scroll: None,
            #[cfg(not(target_arch = "wasm32"))]
            profile,
            #[cfg(not(target_arch = "wasm32"))]
            filter,
            #[cfg(not(target_arch = "wasm32"))]
            binary_path_input: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            source_path_input: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            rewrite_from_input: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            rewrite_to_input: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            cached_line_detail: None,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn restart(&mut self, binary: PathBuf, source_rewrites: Vec<SourcePathRewrite>) {
        let (tx, rx) = mpsc::channel();
        let mut request = self.request.clone();
        let profile = self.profile.clone();
        let filter = self.filter.clone();
        std::thread::spawn(move || {
            if request.sampled_offsets.is_empty() {
                request.sampled_offsets =
                    analysis::function_sampled_offsets(&profile, &filter, request.function);
            }
            let _ = tx.send(load_source(&request, binary, &source_rewrites));
        });
        self.state = SourceLoadState::Loading(rx);
        self.focused_source_line = None;
        self.pending_source_scroll = None;
        self.cached_line_detail = None;
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn set_ready_source(&mut self, source: AnnotatedSource) {
        let target = source
            .hottest_line
            .clamp(1, source.lines.len().max(1) as u32);
        self.focused_source_line = Some(target);
        self.pending_source_scroll = Some(target);
        self.state = SourceLoadState::Ready(source);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn load_source_file(&mut self, path: PathBuf) -> bool {
        let SourceLoadState::NeedSource {
            message,
            binary_path,
            line_samples,
            line_offsets,
            function_name,
            ..
        } = &mut self.state
        else {
            return false;
        };
        match read_source(
            path,
            dwarf_origin(binary_path),
            line_samples.clone(),
            line_offsets.clone(),
            function_name,
        ) {
            Ok(source) => {
                self.set_ready_source(source);
                true
            }
            Err(error) => {
                *message = error;
                false
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn expected_source_path(&self) -> Option<&std::path::Path> {
        match &self.state {
            SourceLoadState::NeedSource { expected_path, .. } => Some(expected_path),
            _ => None,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn set_source_rewrite_inputs(&mut self, rewrite: &SourcePathRewrite) {
        self.rewrite_from_input = rewrite.dwarf_prefix.display().to_string();
        self.rewrite_to_input = rewrite.local_prefix.display().to_string();
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn set_local_source_prefix(&mut self, path: &std::path::Path) {
        self.rewrite_to_input = path.display().to_string();
    }

    pub(crate) fn poll(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        let result = match &self.state {
            SourceLoadState::Loading(rx) => rx.try_recv().ok(),
            _ => None,
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(result) = result {
            match result {
                SourceLoadResult::Ready(source) => self.set_ready_source(source),
                SourceLoadResult::NeedSource {
                    message,
                    binary_path,
                    expected_path,
                    detected_prefix,
                    line_samples,
                    line_offsets,
                    function_name,
                } => {
                    if self.rewrite_from_input.trim().is_empty() {
                        if let Some(prefix) = &detected_prefix {
                            self.rewrite_from_input = prefix.display().to_string();
                        }
                    }
                    self.state = SourceLoadState::NeedSource {
                        message,
                        binary_path,
                        expected_path,
                        detected_prefix,
                        line_samples,
                        line_offsets,
                        function_name,
                    };
                }
                SourceLoadResult::NeedBinary(message) => {
                    self.state = SourceLoadState::NeedBinary(message);
                }
                SourceLoadResult::Error(message) => {
                    self.state = SourceLoadState::Error(message);
                }
            }
        }
    }
}

impl EventExplorerApp {
    #[cfg(not(target_arch = "wasm32"))]
    fn source_lookup_preference(
        &self,
        request: &SourceOpenRequest,
    ) -> Option<&SourceLookupPreference> {
        self.source_lookup_preferences
            .iter()
            .find(|preference| preference.matches_build(request))
            .or_else(|| {
                self.source_lookup_preferences
                    .iter()
                    .find(|preference| preference.matches_captured_executable(request))
            })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn take_source_lookup_preference(
        &mut self,
        request: &SourceOpenRequest,
    ) -> Option<SourceLookupPreference> {
        let index = self
            .source_lookup_preferences
            .iter()
            .position(|preference| preference.matches_build(request))
            .or_else(|| {
                self.source_lookup_preferences
                    .iter()
                    .position(|preference| preference.matches_captured_executable(request))
            })?;
        Some(self.source_lookup_preferences.remove(index))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn remember_source_binary(
        &mut self,
        request: &SourceOpenRequest,
        binary_path: PathBuf,
    ) {
        if request.build_id.is_empty() && request.captured_path.is_empty() {
            return;
        }
        let mut preference = self
            .take_source_lookup_preference(request)
            .unwrap_or_else(|| SourceLookupPreference::new(request));
        preference.binary_name.clone_from(&request.binary_name);
        preference.captured_path.clone_from(&request.captured_path);
        preference.build_id.clone_from(&request.build_id);
        preference.binary_path = Some(binary_path);
        self.source_lookup_preferences.insert(0, preference);
        self.source_lookup_preferences
            .truncate(MAX_SOURCE_LOOKUP_PREFERENCES);
        save_source_lookup_preferences(&self.source_lookup_preferences);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn remember_source_rewrite(
        &mut self,
        request: &SourceOpenRequest,
        rewrite: SourcePathRewrite,
    ) {
        if request.build_id.is_empty() && request.captured_path.is_empty() {
            return;
        }
        let mut preference = self
            .take_source_lookup_preference(request)
            .unwrap_or_else(|| SourceLookupPreference::new(request));
        preference.binary_name.clone_from(&request.binary_name);
        preference.captured_path.clone_from(&request.captured_path);
        preference.build_id.clone_from(&request.build_id);
        preference
            .rewrites
            .retain(|existing| existing.dwarf_prefix != rewrite.dwarf_prefix);
        preference.rewrites.push(rewrite);
        self.source_lookup_preferences.insert(0, preference);
        self.source_lookup_preferences
            .truncate(MAX_SOURCE_LOOKUP_PREFERENCES);
        save_source_lookup_preferences(&self.source_lookup_preferences);
    }

    /// Build the docked source view for a request, reusing the flame's
    /// current panel when it already serves this function.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn source_view_state(&mut self, request: SourceOpenRequest) -> SourceViewState {
        let id = self.next_source_id;
        self.next_source_id += 1;
        let existing = self
            .flame
            .as_mut()
            .and_then(|flame| flame.source_view.take());
        if let Some(mut existing) = existing {
            if existing.request.function == request.function
                && matches!(
                    existing.state,
                    SourceLoadState::Loading(_) | SourceLoadState::Ready(_)
                )
            {
                let hottest = match &existing.state {
                    SourceLoadState::Ready(source) => Some(
                        source
                            .hottest_line
                            .clamp(1, source.lines.len().max(1) as u32),
                    ),
                    _ => None,
                };
                existing.id = id;
                existing.request.stack_path.clone_from(&request.stack_path);
                existing.request.split_user_by_pid = request.split_user_by_pid;
                existing.cached_line_detail = None;
                if let Some(line) = hottest {
                    existing.focused_source_line = Some(line);
                    existing.pending_source_scroll = Some(line);
                }
                return *existing;
            }
        }
        let binary_id = request.function.binary;
        let saved_preference = self.source_lookup_preference(&request).cloned();
        let filter = self.current_function_filter();
        let kernel_version = self
            .file_info
            .as_ref()
            .and_then(|info| info.machine.as_ref())
            .map(|machine| machine.kernel_version.clone())
            .unwrap_or_default();
        let firmware_commit =
            super::github_source::firmware_commit_from_kernel_version(&kernel_version)
                .unwrap_or_default();
        let ref_note = super::github_source::firmware_source_note(&kernel_version);
        let mut view = SourceViewState::new(
            id,
            request,
            self.cpu_profile.clone(),
            filter,
            firmware_commit,
            ref_note,
        );
        let embedded = embedded_source_for_binary(&self.cpu_profile, binary_id);
        if let Some(binary_path) = self.source_binary_paths.get(&binary_id).cloned() {
            let rewrites = self
                .source_path_rewrites
                .get(&binary_id)
                .cloned()
                .unwrap_or_default();
            view.binary_path_input = binary_path.display().to_string();
            if !embedded {
                view.restart(binary_path, rewrites);
            }
        } else if let Some(binary_path) =
            saved_preference.and_then(|preference| preference.binary_path)
        {
            view.binary_path_input = binary_path.display().to_string();
        }
        view
    }

    /// Dock annotated source for `jump` beside the flame. Zoom and
    /// history stay as they are; while the panel is open the canvas
    /// draws the frames under `jump`.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_flame_source(&mut self, jump: SourceJump) {
        let Some(mut request) = self.source_request_for_pc(jump.pc, jump.pid) else {
            return;
        };
        if self.flame.is_none() {
            self.flame = Some(FlameViewState::new_empty());
        }
        request.stack_path = jump.stack_path.clone();
        let view = self.source_view_state(request);
        let flame = self.flame.as_mut().unwrap();
        flame.show_source = true;
        flame.source_focus_suspended = false;
        flame.source_frame = Some(jump);
        flame.source_view = Some(Box::new(view));
    }

    pub(crate) fn source_request_for_pc(&self, pc: u64, pid: u32) -> Option<SourceOpenRequest> {
        let resolved = analysis::resolve_profile_function(&self.cpu_profile, pc, pid)?;
        let (binary_name, captured_path, build_id) = match resolved.id.binary {
            ProfileBinaryId::Kernel(file_id) => {
                let file = self.cpu_profile.linker_files.get(&file_id)?;
                (
                    file.filename.clone(),
                    file.pathname.clone(),
                    file.build_id.clone(),
                )
            }
            ProfileBinaryId::User(file_id) => {
                let file = self.cpu_profile.user_files.get(&file_id)?;
                (
                    file.short_name().to_string(),
                    file.pathname.clone(),
                    file.build_id.clone(),
                )
            }
        };
        Some(SourceOpenRequest {
            function: resolved.id,
            function_name: resolved.name,
            binary_name,
            captured_path,
            build_id,
            sampled_offsets: Vec::new(),
            stack_path: Vec::new(),
            split_user_by_pid: self
                .flame
                .as_ref()
                .map(|flame| flame.split_user_by_pid)
                .unwrap_or(false),
        })
    }

    /// Render the docked source panel beside the flame. The heading
    /// names the function; the Source button on the flame row hides it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn show_flame_source_panel(
        &mut self,
        ui: &mut egui::Ui,
        view: &mut SourceViewState,
    ) -> SourcePanelAction {
        let mut action = SourcePanelAction::None;
        let title = format!(
            "{} — {}",
            view.request.function_name, view.request.binary_name
        );
        let path = match &view.state {
            SourceLoadState::Ready(source) => Some((
                source.source_path.display().to_string(),
                source.origin.clone(),
            )),
            _ => None,
        };
        show_source_heading(
            ui,
            &title,
            path.as_ref()
                .map(|(full, origin)| (full.as_str(), origin.as_str())),
        );
        let mut open_source_pc: Option<SourceJump> = None;
        let mut line_focused = None;
        self.show_source_state_body(ui, view, &mut open_source_pc, &mut line_focused);
        if let Some(jump) = open_source_pc {
            action = SourcePanelAction::Open(jump);
        } else if line_focused.is_some() {
            action = SourcePanelAction::FocusLine;
        }
        action
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn show_source_state_body(
        &mut self,
        ui: &mut egui::Ui,
        view: &mut SourceViewState,
        open_source_pc: &mut Option<SourceJump>,
        line_focused: &mut Option<u32>,
    ) {
        let binary_id = view.request.function.binary;
        let source_lookup_request = view.request.clone();
        let source_rewrites = self
            .source_path_rewrites
            .get(&binary_id)
            .cloned()
            .unwrap_or_default();
        let saved_source_rewrites = self
            .source_lookup_preference(&source_lookup_request)
            .map(|preference| preference.rewrites.clone())
            .unwrap_or_default();
        let checkout_root_guess = match &view.state {
            SourceLoadState::NeedSource { expected_path, .. }
                if nginx_ports_dwarf_prefix(expected_path).is_some() =>
            {
                guess_checkout_root_from_kernel_rewrites(&self.source_path_rewrites).or_else(|| {
                    guess_checkout_root_from_saved_preferences(&self.source_lookup_preferences)
                })
            }
            _ => None,
        };
        let mut accepted_rewrite: Option<SourcePathRewrite> = None;
        let mut loaded_source = None;
        let mut binary_to_remember = None;
        let mut browse_purpose = None;
        let id = view.id;

        match &mut view.state {
            SourceLoadState::Loading(_) => {
                ui.spinner();
                let fetching =
                    embedded_source_for_binary(&view.profile, view.request.function.binary);
                ui.label(if fetching {
                    "Fetching source from GitHub\u{2026}"
                } else {
                    "Resolving DWARF source lines\u{2026}"
                });
                ui.ctx().request_repaint();
            }
            SourceLoadState::NeedBinary(message) => {
                selectable_message(ui, message, None);
                ui.label("Local executable or debug file:");
                match local_path_picker(ui, &mut view.binary_path_input, "Browse\u{2026}") {
                    Some(LocalPathAction::Load(path)) => {
                        binary_to_remember = Some(path.clone());
                        view.restart(path, source_rewrites.clone());
                    }
                    Some(LocalPathAction::Browse) => {
                        browse_purpose = Some((
                            NativeFileDialogPurpose::SourceBinary(id),
                            "Choose executable or debug file",
                            view.binary_path_input.clone(),
                        ));
                    }
                    None => {}
                }
            }
            SourceLoadState::NeedSource {
                message,
                binary_path,
                expected_path,
                detected_prefix,
                line_samples,
                line_offsets,
                function_name,
            } => {
                let nginx_dwarf_prefix = nginx_ports_dwarf_prefix(expected_path);
                let suggested_dwarf_prefix = nginx_dwarf_prefix
                    .clone()
                    .or_else(|| detected_prefix.clone());
                selectable_message(ui, message, None);
                ui.horizontal(|ui| {
                    ui.label("DWARF source path:");
                    if ui.button("Copy path").clicked() {
                        ui.output_mut(|output| {
                            output.copied_text = expected_path.display().to_string();
                        });
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(expected_path.display().to_string()).monospace(),
                        )
                        .selectable(true)
                        .wrap(true),
                    );
                });
                if view.rewrite_to_input.is_empty() {
                    if let Some(rewrite) = default_source_rewrite(
                        expected_path,
                        &source_rewrites,
                        &saved_source_rewrites,
                    ) {
                        view.rewrite_from_input = rewrite.dwarf_prefix.display().to_string();
                        view.rewrite_to_input = if nginx_dwarf_prefix.is_some() {
                            checkout_root_from_nginx_local_prefix(&rewrite.local_prefix)
                                .unwrap_or_else(|| rewrite.local_prefix.clone())
                                .display()
                                .to_string()
                        } else {
                            rewrite.local_prefix.display().to_string()
                        };
                    }
                }
                if view.rewrite_from_input.is_empty() {
                    if let Some(prefix) = suggested_dwarf_prefix.as_ref() {
                        view.rewrite_from_input = prefix.display().to_string();
                    } else if let Some(rewrite) = infer_source_rewrite(expected_path, binary_path) {
                        view.rewrite_from_input = rewrite.dwarf_prefix.display().to_string();
                        if view.rewrite_to_input.is_empty() {
                            view.rewrite_to_input = rewrite.local_prefix.display().to_string();
                        }
                    }
                }
                if nginx_dwarf_prefix.is_some() && view.rewrite_to_input.is_empty() {
                    if let Some(root) = checkout_root_guess.as_ref() {
                        view.rewrite_to_input = root.display().to_string();
                    }
                }
                ui.group(|ui| {
                    ui.strong(format!(
                        "Source path rewrite for {}",
                        view.request.binary_name
                    ));
                    ui.label(
                        "Replace a DWARF path prefix for every source path in this executable.",
                    );
                    if nginx_dwarf_prefix.is_some() {
                        ui.label(format!(
                            "nginx ports source is read from <checkout>/{}.",
                            NGINX_PORTS_LOCAL_SUFFIX
                        ));
                    }
                    egui::Grid::new(("source_rewrite", id))
                        .num_columns(2)
                        .spacing(egui::vec2(10.0, 4.0))
                        .show(ui, |ui| {
                            ui.label("DWARF prefix");
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut view.rewrite_from_input)
                                        .hint_text("DWARF build root")
                                        .desired_width(280.0),
                                );
                                if ui
                                    .add_enabled(
                                        suggested_dwarf_prefix.is_some(),
                                        egui::Button::new("Use detected"),
                                    )
                                    .clicked()
                                {
                                    if let Some(prefix) = suggested_dwarf_prefix.as_ref() {
                                        view.rewrite_from_input = prefix.display().to_string();
                                    }
                                }
                            });
                            ui.end_row();
                            ui.label(if nginx_dwarf_prefix.is_some() {
                                "Checkout root"
                            } else {
                                "Local prefix"
                            });
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut view.rewrite_to_input)
                                        .hint_text(if nginx_dwarf_prefix.is_some() {
                                            "Local checkout root"
                                        } else {
                                            "Local source root"
                                        })
                                        .desired_width(340.0),
                                );
                                if ui.button("Browse\u{2026}").clicked() {
                                    browse_purpose = Some((
                                        NativeFileDialogPurpose::SourcePrefix(id),
                                        if nginx_dwarf_prefix.is_some() {
                                            "Choose local checkout root"
                                        } else {
                                            "Choose local source root"
                                        },
                                        view.rewrite_to_input.clone(),
                                    ));
                                }
                            });
                            ui.end_row();
                        });
                    if let Some(prefix) = suggested_dwarf_prefix.as_ref() {
                        selectable_message(
                            ui,
                            &format!("Detected DWARF prefix: {}", prefix.display()),
                            None,
                        );
                    }
                    if let Some(rewrite) = source_rewrite_from_inputs(
                        expected_path,
                        &view.rewrite_from_input,
                        &view.rewrite_to_input,
                    ) {
                        if let Some(path) = rewrite.apply(expected_path) {
                            let text = if path.is_file() {
                                format!("Resolved source: {}", path.display())
                            } else {
                                format!("Resolved source not found: {}", path.display())
                            };
                            selectable_message(
                                ui,
                                &text,
                                (!path.is_file()).then_some(Color32::from_rgb(230, 100, 90)),
                            );
                        }
                    }
                    if ui.button("Apply rewrite").clicked() {
                        let Some(rewrite) = source_rewrite_from_inputs(
                            expected_path,
                            &view.rewrite_from_input,
                            &view.rewrite_to_input,
                        ) else {
                            *message = "Enter both the DWARF and local prefixes.".to_string();
                            return;
                        };
                        match rewrite.apply(expected_path) {
                            Some(path) => match read_source(
                                path,
                                dwarf_origin(binary_path),
                                line_samples.clone(),
                                line_offsets.clone(),
                                function_name,
                            ) {
                                Ok(source) => {
                                    loaded_source = Some(source);
                                    accepted_rewrite = Some(rewrite);
                                    ui.ctx().request_repaint();
                                }
                                Err(error) => *message = error,
                            },
                            None => {
                                *message = format!(
                                    "DWARF path {} does not start with {}.",
                                    expected_path.display(),
                                    rewrite.dwarf_prefix.display()
                                );
                            }
                        }
                    }
                });
                ui.add_space(8.0);
                ui.label("Or select just this local source file:");
                match local_path_picker(ui, &mut view.source_path_input, "Browse\u{2026}") {
                    Some(LocalPathAction::Load(path)) => {
                        let inferred = infer_source_rewrite_from_files(expected_path, &path);
                        match read_source(
                            path,
                            dwarf_origin(binary_path),
                            line_samples.clone(),
                            line_offsets.clone(),
                            function_name,
                        ) {
                            Ok(source) => {
                                if let Some(rewrite) = inferred {
                                    view.rewrite_from_input =
                                        rewrite.dwarf_prefix.display().to_string();
                                    view.rewrite_to_input =
                                        rewrite.local_prefix.display().to_string();
                                    accepted_rewrite = Some(rewrite);
                                }
                                loaded_source = Some(source);
                            }
                            Err(error) => *message = error,
                        }
                    }
                    Some(LocalPathAction::Browse) => {
                        browse_purpose = Some((
                            NativeFileDialogPurpose::SourceFile(id),
                            "Choose local source file",
                            view.source_path_input.clone(),
                        ));
                    }
                    None => {}
                }
            }
            SourceLoadState::Error(message) => {
                selectable_message(ui, message, Some(Color32::from_rgb(230, 100, 90)));
                ui.label("Choose a different local executable or debug file:");
                match local_path_picker(ui, &mut view.binary_path_input, "Browse\u{2026}") {
                    Some(LocalPathAction::Load(path)) => {
                        binary_to_remember = Some(path.clone());
                        view.restart(path, source_rewrites.clone());
                    }
                    Some(LocalPathAction::Browse) => {
                        browse_purpose = Some((
                            NativeFileDialogPurpose::SourceBinary(id),
                            "Choose executable or debug file",
                            view.binary_path_input.clone(),
                        ));
                    }
                    None => {}
                }
            }
            SourceLoadState::Ready(_) => {
                show_source_ready_body(ui, view, open_source_pc, line_focused);
            }
        }
        if let Some(source) = loaded_source {
            view.set_ready_source(source);
        }
        if let Some(rewrite) = accepted_rewrite {
            let rewrites = self.source_path_rewrites.entry(binary_id).or_default();
            rewrites.retain(|existing| existing.dwarf_prefix != rewrite.dwarf_prefix);
            rewrites.push(rewrite.clone());
            self.remember_source_rewrite(&source_lookup_request, rewrite);
        }
        if let Some(path) = binary_to_remember {
            self.source_binary_paths.insert(binary_id, path.clone());
            self.remember_source_binary(&source_lookup_request, path);
        }
        if let Some((purpose, title, current_input)) = browse_purpose {
            self.native_file_dialog = Some(match purpose {
                NativeFileDialogPurpose::SourcePrefix(_) => {
                    NativeFileDialog::source_directory(purpose, title, &current_input)
                }
                _ => NativeFileDialog::source(purpose, title, &current_input),
            });
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_line_detail_cache(
    cached: &mut Option<LineDetailCache>,
    profile: &CpuProfile,
    filter: &FlameFilter,
    target: ProfileFunctionId,
    function_name: &str,
    stack_path: &[(u64, u32)],
    split_user_by_pid: bool,
) {
    if cached
        .as_ref()
        .is_some_and(|cache| cache.path == stack_path)
    {
        return;
    }
    let (hits, anchor_key) = analysis::samples_on_symbol_path(
        profile,
        filter,
        stack_path,
        split_user_by_pid,
        Some(target),
    );
    let mut frames = Vec::new();
    if stack_path.is_empty() {
        let (addr, pid) = anchor_key.unwrap_or((0, 0));
        let label = if addr == 0 {
            function_name.to_string()
        } else {
            super::flame::flame_label(addr, pid, profile, true, false)
        };
        frames.push(StackFrame {
            addr,
            pid,
            label,
            function: Some(target),
        });
    } else {
        for &(addr, pid) in stack_path {
            let resolved = analysis::resolve_profile_function(profile, addr, pid);
            frames.push(StackFrame {
                addr,
                pid,
                label: if addr == analysis::FLAME_GROUP_ADDR {
                    analysis::flame_group_bar_label(profile, pid)
                } else {
                    super::flame::flame_label(addr, pid, profile, true, false)
                },
                function: resolved.map(|hit| hit.id),
            });
        }
    }
    let function_index = frames
        .iter()
        .rposition(|frame| frame.function == Some(target))
        .unwrap_or(frames.len().saturating_sub(1));

    #[derive(Hash, PartialEq, Eq)]
    enum CalleeKey {
        Func(ProfileFunctionId),
        Pc(u64, u32),
    }
    let mut callee_counts: rustc_hash::FxHashMap<CalleeKey, CalleeRow> =
        rustc_hash::FxHashMap::default();
    let mut self_count = 0u64;
    for hit in &hits {
        let Some((addr, pid)) = hit.callee else {
            self_count += 1;
            continue;
        };
        let resolved = analysis::resolve_profile_function(profile, addr, pid);
        let key = match resolved.as_ref() {
            Some(function) => CalleeKey::Func(function.id),
            None => CalleeKey::Pc(addr, pid),
        };
        let row = callee_counts.entry(key).or_insert_with(|| CalleeRow {
            addr,
            pid,
            label: resolved.as_ref().map_or_else(
                || super::flame::flame_label(addr, pid, profile, true, false),
                |function| function.name.clone(),
            ),
            count: 0,
            function: resolved.as_ref().map(|function| function.id),
        });
        row.count += 1;
    }
    let mut callees: Vec<CalleeRow> = callee_counts.into_values().collect();
    callees.sort_unstable_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then(left.label.cmp(&right.label))
    });

    *cached = Some(LineDetailCache {
        path: stack_path.to_vec(),
        frames,
        function_index,
        callees,
        self_count,
        sample_count: hits.len() as u64,
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn stack_path_through(frames: &[StackFrame], end: usize) -> Vec<(u64, u32)> {
    frames[..=end.min(frames.len().saturating_sub(1))]
        .iter()
        .filter(|frame| frame.addr != 0)
        .map(|frame| (frame.addr, frame.pid))
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn show_clicked_stack(
    ui: &mut egui::Ui,
    view_id: u64,
    cache: &LineDetailCache,
    open_source_pc: &mut Option<SourceJump>,
    zoom_to: &mut Option<Vec<(u64, u32)>>,
) {
    if cache.sample_count == 0 {
        ui.label(RichText::new("No samples on this callstack.").color(Color32::from_gray(140)));
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_source(("side_panel_stacks", view_id))
        .show(ui, |ui| {
            let num_width = if cache.frames.len() >= 10 { 2 } else { 1 };
            for (index, frame) in cache.frames.iter().enumerate() {
                let is_current = index == cache.function_index;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("#{index:>width$}", width = num_width))
                            .monospace()
                            .size(11.0)
                            .color(if is_current {
                                Color32::from_gray(210)
                            } else {
                                Color32::from_gray(130)
                            }),
                    );
                    let text = RichText::new(&frame.label).monospace().size(11.0);
                    let text = if is_current { text.strong() } else { text };
                    let resp = ui.add(
                        egui::Label::new(text)
                            .selectable(false)
                            .sense(egui::Sense::click()),
                    );
                    if frame.addr != 0 && !is_current {
                        let path = stack_path_through(&cache.frames, index);
                        if frame.function.is_some() {
                            resp.clone().on_hover_text("Open this function");
                            if resp.clicked() {
                                *open_source_pc = Some(SourceJump {
                                    pc: frame.addr,
                                    pid: frame.pid,
                                    stack_path: path,
                                });
                            }
                        } else {
                            resp.clone().on_hover_text("Show this frame");
                            if resp.clicked() {
                                *zoom_to = Some(path);
                            }
                        }
                    }
                });
                if !is_current {
                    continue;
                }
                let indent = 16.0;
                for callee in &cache.callees {
                    let pct = if cache.sample_count > 0 {
                        callee.count as f64 * 100.0 / cache.sample_count as f64
                    } else {
                        0.0
                    };
                    let pct_label = if pct < 0.1 {
                        "<0.1%".to_string()
                    } else {
                        format!("{pct:.1}%")
                    };
                    ui.horizontal(|ui| {
                        ui.add_space(indent);
                        ui.monospace(format!("{pct_label:>6}"));
                        let resp = ui.add(
                            egui::Label::new(RichText::new(&callee.label).monospace().size(11.0))
                                .selectable(false)
                                .sense(egui::Sense::click()),
                        );
                        if callee.function.is_some() {
                            resp.clone().on_hover_text("Open this function");
                            if resp.clicked() {
                                let mut path = stack_path_through(&cache.frames, index);
                                path.push((callee.addr, callee.pid));
                                *open_source_pc = Some(SourceJump {
                                    pc: callee.addr,
                                    pid: callee.pid,
                                    stack_path: path,
                                });
                            }
                        } else {
                            resp.clone().on_hover_text("Show this frame");
                            if resp.clicked() {
                                let mut path = stack_path_through(&cache.frames, index);
                                path.push((callee.addr, callee.pid));
                                *zoom_to = Some(path);
                            }
                        }
                    });
                }
                if cache.self_count > 0 {
                    let pct = cache.self_count as f64 * 100.0 / cache.sample_count.max(1) as f64;
                    ui.horizontal(|ui| {
                        ui.add_space(indent);
                        ui.monospace(format!("{pct:>5.1}%"));
                        ui.label(
                            RichText::new("(self)")
                                .monospace()
                                .size(11.0)
                                .color(Color32::from_gray(150)),
                        );
                    });
                }
            }
        });
}

#[cfg(not(target_arch = "wasm32"))]
fn github_blob_path(origin: &str) -> Option<&str> {
    let after_blob = origin.split_once("/blob/")?.1;
    let path = after_blob.split_once('/')?.1;
    (!path.is_empty()).then_some(path)
}

#[cfg(not(target_arch = "wasm32"))]
fn source_link_label(path: &str, origin: &str) -> String {
    if let Some(blob_path) = github_blob_path(origin) {
        return blob_path.to_string();
    }
    for text in [path, origin] {
        if let Some(index) = text.find("FreeBSD/sys") {
            return text[index..].to_string();
        }
    }
    path.to_string()
}

/// Function title, with the source path continuing on that same line.
#[cfg(not(target_arch = "wasm32"))]
fn show_source_heading(ui: &mut egui::Ui, title: &str, path: Option<(&str, &str)>) {
    let heading = egui::TextStyle::Heading.resolve(ui.style());
    let text_color = ui.visuals().text_color();
    let title_galley =
        ui.fonts(|fonts| fonts.layout_no_wrap(title.to_owned(), heading, text_color));
    let title_width = title_galley.size().x;
    let title_height = title_galley.size().y;

    let Some((full_path, origin)) = path else {
        let (rect, _) = ui.allocate_exact_size(title_galley.size(), egui::Sense::hover());
        ui.painter()
            .galley(rect.left_top(), title_galley, text_color);
        return;
    };

    let label = source_link_label(full_path, origin);
    let linked = web_origin(origin);
    let path_color = if linked {
        ui.visuals().hyperlink_color
    } else {
        text_color
    };
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    job.wrap.break_anywhere = true;
    job.first_row_min_height = title_height;
    job.append(
        &label,
        title_width + 8.0,
        egui::TextFormat {
            font_id: egui::FontId::monospace(10.0),
            color: path_color,
            ..Default::default()
        },
    );
    let path_galley = ui.fonts(|fonts| fonts.layout_job(job));
    let sense = if linked {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) =
        ui.allocate_exact_size(path_galley.size().max(title_galley.size()), sense);
    let row_height = path_galley
        .rows
        .first()
        .map(|row| row.rect.height())
        .unwrap_or(title_height);
    let title_pos = egui::pos2(rect.left(), rect.top() + (row_height - title_height) * 0.5);
    ui.painter().galley(title_pos, title_galley, text_color);
    ui.painter().galley(
        rect.left_top(),
        path_galley.clone(),
        egui::Color32::PLACEHOLDER,
    );

    let on_path =
        |pos: egui::Pos2| pointer_on_source_path(pos, rect.left_top(), &path_galley, title_width);
    if let Some(pos) = response.hover_pos() {
        if on_path(pos) {
            if linked {
                response.clone().on_hover_text(origin);
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                let stroke = egui::Stroke::new(1.0_f32, path_color);
                for (index, row) in path_galley.rows.iter().enumerate() {
                    let y = rect.top() + row.rect.bottom();
                    let x0 = if index == 0 {
                        rect.left() + title_width + 8.0
                    } else {
                        rect.left() + row.rect.left()
                    };
                    let x1 = rect.left() + row.rect.right();
                    if x1 > x0 {
                        ui.painter()
                            .line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], stroke);
                    }
                }
            } else if label != full_path {
                response.clone().on_hover_text(full_path);
            }
        }
    }
    if linked && (response.clicked() || response.middle_clicked()) {
        if let Some(pos) = response.interact_pointer_pos() {
            if on_path(pos) {
                ui.ctx().open_url(egui::OpenUrl {
                    url: origin.to_owned(),
                    new_tab: response.middle_clicked() || ui.input(|input| input.modifiers.any()),
                });
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn pointer_on_source_path(
    pos: egui::Pos2,
    galley_pos: egui::Pos2,
    galley: &egui::Galley,
    title_width: f32,
) -> bool {
    let local = pos - galley_pos;
    for (index, row) in galley.rows.iter().enumerate() {
        if row.rect.contains(local.to_pos2()) {
            return index > 0 || local.x >= title_width;
        }
    }
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn web_origin(origin: &str) -> bool {
    origin.starts_with("https://") || origin.starts_with("http://")
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn show_source_ready_body(
    ui: &mut egui::Ui,
    view: &mut SourceViewState,
    open_source_pc: &mut Option<SourceJump>,
    line_focused: &mut Option<u32>,
) {
    let _ = open_source_pc;
    let SourceLoadState::Ready(source) = &view.state else {
        return;
    };
    let mut focused_line = view
        .focused_source_line
        .unwrap_or(source.hottest_line)
        .clamp(1, source.lines.len().max(1) as u32);
    let heat_order = sampled_lines_hottest_first(&source.line_samples);
    let hotter_line = hotter_sampled_line(&heat_order, focused_line);
    let colder_line = colder_sampled_line(&heat_order, focused_line);
    let linked = web_origin(&source.origin);
    if let Some((start, end)) = source.function_line_range {
        let outside = source.total_mapped.saturating_sub(source.function_total);
        let hottest_in_function = source.hottest_line >= start && source.hottest_line <= end;
        if outside > 0 && !hottest_in_function {
            ui.label(format!(
                "lines {start}-{end} • {} samples in function • {} in inlined helpers (hottest line {})",
                format_count(source.function_total),
                format_count(outside),
                source.hottest_line
            ));
        } else if outside > 0 {
            ui.label(format!(
                "lines {start}-{end} • {} samples in function (hottest line {}) • {} in inlined helpers",
                format_count(source.function_total),
                source.hottest_line,
                format_count(outside)
            ));
        } else {
            ui.label(format!(
                "lines {start}-{end} • {} samples in function (hottest line {})",
                format_count(source.function_total),
                source.hottest_line
            ));
        }
    } else {
        ui.label(format!(
            "{} mapped self samples; hottest line {}",
            format_count(source.total_mapped),
            source.hottest_line
        ));
    }
    ui.horizontal(|ui| {
        if source_line_navigation_button(ui, hotter_line.is_some(), true)
            .on_hover_text("Hotter line")
            .clicked()
        {
            focused_line = hotter_line.unwrap_or(focused_line);
            view.pending_source_scroll = hotter_line;
        }
        if source_line_navigation_button(ui, colder_line.is_some(), false)
            .on_hover_text("Colder line")
            .clicked()
        {
            focused_line = colder_line.unwrap_or(focused_line);
            view.pending_source_scroll = colder_line;
        }
        let count = source.line_samples.get(&focused_line).copied().unwrap_or(0);
        let (sample_total, sample_scope) = if source.total_mapped > source.function_total {
            (source.total_mapped.max(1), "samples")
        } else {
            (source.function_total.max(1), "function")
        };
        let pct = count as f64 * 100.0 / sample_total as f64;
        ui.label(format!(
            "sampled line {focused_line} — {} samples ({pct:.1}% of {sample_scope})",
            format_count(count),
        ));
    });
    view.focused_source_line = Some(focused_line);
    if !linked {
        ui.label(
            RichText::new(&source.origin)
                .monospace()
                .size(10.0)
                .color(Color32::from_gray(130)),
        );
    }
    if let Some(note) = &source.ref_note {
        selectable_message(ui, note, Some(Color32::from_rgb(212, 168, 72)));
    }

    ensure_line_detail_cache(
        &mut view.cached_line_detail,
        &view.profile,
        &view.filter,
        view.request.function,
        &view.request.function_name,
        &view.request.stack_path,
        view.request.split_user_by_pid,
    );

    let max_samples = source.line_samples.values().copied().max().unwrap_or(1);
    let (sample_total, sample_scope) = if source.total_mapped > source.function_total {
        (source.total_mapped.max(1), "samples")
    } else {
        (source.function_total.max(1), "function")
    };
    let row_height = 19.0;
    let total_h = (ui.max_rect().bottom() - ui.cursor().min.y).max(0.0);
    let mut line_to_focus = None;
    let mut pending_scroll = view.pending_source_scroll.take();

    let mut scroll_area = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .min_scrolled_height(total_h)
        .id_source(("source_lines", view.id));
    if let Some(line) = pending_scroll.take() {
        let row_height_with_spacing = row_height + ui.spacing().item_spacing.y;
        let offset = centered_source_scroll_offset(
            line.min(source.lines.len().max(1) as u32),
            row_height_with_spacing,
            total_h,
        );
        scroll_area = scroll_area.vertical_scroll_offset(offset);
    }
    scroll_area.show_rows(ui, row_height, source.lines.len(), |ui, rows| {
        for row in rows {
            let line_no = row as u32 + 1;
            let count = source.line_samples.get(&line_no).copied().unwrap_or(0);
            let alpha = line_highlight_alpha(count, max_samples, sample_total);
            let is_focused = line_no == focused_line;
            let (row_rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), row_height),
                egui::Sense::hover(),
            );
            ui.painter().rect(
                row_rect,
                0.0,
                Color32::from_rgba_unmultiplied(220, 80, 45, alpha),
                if is_focused {
                    Stroke::new(1.5_f32, Color32::from_rgb(245, 190, 75))
                } else {
                    Stroke::NONE
                },
            );
            let mut row_ui =
                ui.child_ui(row_rect, egui::Layout::left_to_right(egui::Align::Center));
            row_ui.add_sized(
                [80.0, 16.0],
                egui::Label::new(
                    RichText::new(if count == 0 {
                        String::new()
                    } else {
                        format!(
                            "{} {:>5.1}%",
                            format_count(count),
                            count as f64 * 100.0 / sample_total as f64
                        )
                    })
                    .monospace()
                    .size(10.0),
                ),
            );
            row_ui.add_sized(
                [42.0, 16.0],
                egui::Label::new(
                    RichText::new(format!("{line_no:>5}"))
                        .monospace()
                        .size(10.0)
                        .color(Color32::from_gray(130)),
                ),
            );
            row_ui.label(RichText::new(&source.lines[row]).monospace().size(11.0));
            let interact_resp = ui.interact(
                row_rect,
                ui.id().with(("row_click", line_no)),
                egui::Sense::click(),
            );
            if count > 0 {
                interact_resp.clone().on_hover_text(format!(
                    "Line {line_no}: {} samples ({:.1}% of {sample_scope}) — click to inspect callstack",
                    format_count(count),
                    count as f64 * 100.0 / sample_total as f64
                ));
            }
            let is_clicked = interact_resp.clicked()
                || (interact_resp.hovered()
                    && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary)));
            if is_clicked {
                line_to_focus = Some(line_no);
                ui.ctx().request_repaint();
            }
        }
    });

    if let Some(l) = line_to_focus {
        view.focused_source_line = Some(l);
        *line_focused = Some(l);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn show_callstack_panel(
    ui: &mut egui::Ui,
    view: &mut SourceViewState,
    open_source_pc: &mut Option<SourceJump>,
    zoom_to: &mut Option<Vec<(u64, u32)>>,
) {
    let function = view.request.function;
    let name = view.request.function_name.clone();
    let path = view.request.stack_path.clone();
    let split = view.request.split_user_by_pid;
    ensure_line_detail_cache(
        &mut view.cached_line_detail,
        &view.profile,
        &view.filter,
        function,
        &name,
        &path,
        split,
    );
    if let Some(line) = view.focused_source_line {
        ui.label(format!("Line {line}"));
    }
    let Some(cache) = view.cached_line_detail.as_ref() else {
        ui.label("Loading callstack…");
        return;
    };
    show_clicked_stack(ui, view.id, cache, open_source_pc, zoom_to);
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use object::{Object, ObjectSymbol};

    #[test]
    fn source_link_label_starts_at_freebsd_sys() {
        let dwarf = "/build/workspace/firmware-2/FreeBSD/sys/netinet/tcp_lro.c";
        let url = "https://git.example.com/acme/firmware/blob/abc/FreeBSD/sys/netinet/tcp_lro.c";
        assert_eq!(
            source_link_label(dwarf, url),
            "FreeBSD/sys/netinet/tcp_lro.c"
        );
        let objdir =
            "/obj/FreeBSD/amd64.amd64/sys/GENERIC/../../../../FreeBSD/sys/netinet/tcp_lro.c";
        assert_eq!(
            source_link_label(objdir, url),
            "FreeBSD/sys/netinet/tcp_lro.c"
        );
        let nginx = "ports/netflix/nginx/work-default/nginx/src/os/unix/ngx_process.c";
        assert_eq!(
            source_link_label(nginx, "https://example/ngx_process.c"),
            nginx
        );
        let wrkdirs = "/wrkdirs/usr/ports/netflix/nginx/work-default/nginx-1.31.4/src/event/modules/ngx_kqueue_module.c";
        let nginx_url = "https://git.example.com/acme/firmware/blob/36d625ca270c/ports/netflix/nginx/files/nginx/src/event/modules/ngx_kqueue_module.c";
        assert_eq!(
            source_link_label(wrkdirs, nginx_url),
            "ports/netflix/nginx/files/nginx/src/event/modules/ngx_kqueue_module.c"
        );
    }

    #[test]
    fn local_dwarf_maps_a_sample_to_source() {
        // Always analyze a FreeBSD ELF. The host test executable is Mach-O
        // on macOS, with prefixed symbols and debug info in a separate dSYM.
        let checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let executable = checkout.join("tests/fixtures/source/source_fixture.elf");
        let data = std::fs::read(&executable).unwrap();
        let object = object::File::parse(data.as_slice()).unwrap();
        let symbol = object
            .symbols()
            .find(|symbol| symbol.name().ok() == Some("event_explorer_source_fixture"))
            .expect("fixture symbol");
        assert!(symbol.address() > 0);

        let request = SourceOpenRequest {
            function: ProfileFunctionId {
                binary: ProfileBinaryId::User(1),
                offset: 0,
            },
            function_name: "event_explorer_source_fixture".to_string(),
            binary_name: "test".to_string(),
            captured_path: executable.to_string_lossy().into_owned(),
            build_id: String::new(),
            sampled_offsets: vec![(0, 7)],
            stack_path: Vec::new(),
            split_user_by_pid: false,
        };
        let rewrite =
            SourcePathRewrite::new("/eventexplorer-test", env!("CARGO_MANIFEST_DIR")).unwrap();
        match load_source(&request, executable, &[rewrite]) {
            SourceLoadResult::Ready(source) => {
                assert!(source
                    .source_path
                    .file_name()
                    .is_some_and(|name| name == "source_fixture.c"));
                assert_eq!(source.total_mapped, 7);
                assert!(source.hottest_line > 0);
            }
            other => panic!("unexpected source result: {other:?}"),
        }
    }

    #[test]
    fn source_rewrite_replaces_only_a_component_prefix() {
        let rewrite = SourcePathRewrite::new("/remote/build", "/local/checkout").unwrap();
        assert_eq!(
            rewrite.apply(std::path::Path::new("/remote/build/project/sys/net/if.c")),
            Some(PathBuf::from("/local/checkout/project/sys/net/if.c"))
        );
        assert_eq!(
            rewrite.apply(std::path::Path::new(
                "/remote/build-other/project/sys/net/if.c"
            )),
            None
        );
    }

    #[test]
    fn most_specific_source_rewrite_wins() {
        let rewrites = vec![
            SourcePathRewrite::new("/remote", "/wrong").unwrap(),
            SourcePathRewrite::new("/remote/build", "/local/checkout").unwrap(),
        ];
        assert_eq!(
            rewrite_source_path(
                std::path::Path::new("/remote/build/project/sys/net/if.c"),
                &rewrites
            ),
            Some(PathBuf::from("/local/checkout/project/sys/net/if.c"))
        );
    }

    #[test]
    fn source_rewrite_is_inferred_from_a_shared_project_component() {
        let rewrite = infer_source_rewrite(
            std::path::Path::new("/remote/build/project/sys/net/if.c"),
            std::path::Path::new("/local/checkout/project/obj/amd64/kernel"),
        )
        .unwrap();
        assert_eq!(rewrite.dwarf_prefix, PathBuf::from("/remote/build"));
        assert_eq!(rewrite.local_prefix, PathBuf::from("/local/checkout"));
    }

    #[test]
    fn dwarf_prefix_is_everything_before_dot_obj() {
        assert_eq!(
            prefix_before_dot_obj(std::path::Path::new(
                "/mounted/build/tree/.obj/target/source.c"
            )),
            Some(PathBuf::from("/mounted/build/tree"))
        );
        assert_eq!(
            prefix_before_dot_obj(std::path::Path::new(
                "/mounted/build/tree/.obj-amd64/target/source.c"
            )),
            Some(PathBuf::from("/mounted/build/tree"))
        );
    }

    #[test]
    fn dwarf_prefix_comes_from_object_binary_build_root() {
        assert_eq!(
            dwarf_prefix_for_paths(
                std::path::Path::new("/mounted/build/tree/project/sys/source.c"),
                std::path::Path::new("/mounted/build/tree/.obj-amd64/target/kernel")
            ),
            Some(PathBuf::from("/mounted/build/tree"))
        );
    }

    #[test]
    fn rewrite_collapses_object_tree_parent_chain_after_capturing_prefix() {
        let dwarf_path = std::path::Path::new(
            "/remote/tree/.obj/target/remote/tree/project/arch/sys/CONF/../../../../../../../../project/sys/header.h",
        );
        assert_eq!(
            prefix_before_dot_obj(dwarf_path),
            Some(PathBuf::from("/remote/tree"))
        );
        let rewrite = SourcePathRewrite::new("/remote/tree", "/local/checkout").unwrap();
        assert_eq!(
            rewrite.apply(dwarf_path),
            Some(PathBuf::from("/local/checkout/project/sys/header.h"))
        );
    }

    #[test]
    fn nginx_ports_rewrite_uses_checkout_root() {
        let dwarf_path = std::path::Path::new(
            "/wrkdirs/usr/ports/netflix/nginx/work-default/nginx-1.31.4/src/http/modules/ngx_http_headers_filter_module.c",
        );
        assert_eq!(
            nginx_ports_dwarf_prefix(dwarf_path),
            Some(PathBuf::from(
                "/wrkdirs/usr/ports/netflix/nginx/work-default/nginx-1.31.4"
            ))
        );
        let rewrite = source_rewrite_from_inputs(
            dwarf_path,
            "/wrkdirs/usr/ports/netflix/nginx/work-default/nginx-1.31.4",
            "/local/checkout",
        )
        .unwrap();
        assert_eq!(
            rewrite.apply(dwarf_path),
            Some(PathBuf::from(
                "/local/checkout/ports/netflix/nginx/files/nginx/src/http/modules/ngx_http_headers_filter_module.c"
            ))
        );
        assert_eq!(
            checkout_root_from_nginx_local_prefix(&rewrite.local_prefix),
            Some(PathBuf::from("/local/checkout"))
        );

        let kernel_rewrite = source_rewrite_from_inputs(
            std::path::Path::new("/remote/build/FreeBSD/sys/net/if.c"),
            "/remote/build",
            "/local/checkout",
        )
        .unwrap();
        assert_eq!(
            kernel_rewrite.local_prefix,
            PathBuf::from("/local/checkout")
        );
    }

    #[test]
    fn nginx_checkout_root_is_reused_from_kernel_rewrite() {
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(checkout.path().join(NGINX_PORTS_LOCAL_SUFFIX)).unwrap();
        let mut rewrites = rustc_hash::FxHashMap::default();
        rewrites.insert(
            ProfileBinaryId::Kernel(1),
            vec![SourcePathRewrite {
                dwarf_prefix: PathBuf::from("/remote/build"),
                local_prefix: checkout.path().to_path_buf(),
            }],
        );
        assert_eq!(
            guess_checkout_root_from_kernel_rewrites(&rewrites),
            Some(checkout.path().to_path_buf())
        );
        let preferences = vec![SourceLookupPreference {
            binary_name: "kernel".to_string(),
            captured_path: "/boot/kernel/kernel".to_string(),
            build_id: "aabbcc".to_string(),
            binary_path: None,
            rewrites: rewrites.remove(&ProfileBinaryId::Kernel(1)).unwrap(),
        }];
        assert_eq!(
            guess_checkout_root_from_saved_preferences(&preferences),
            Some(checkout.path().to_path_buf())
        );
    }

    #[test]
    fn source_lookup_preference_round_trips_paths_and_rewrites() {
        let preference = SourceLookupPreference {
            binary_name: "kernel\tdebug".to_string(),
            captured_path: "/boot/kernel/kernel\nold".to_string(),
            build_id: "aabbcc".to_string(),
            binary_path: Some(PathBuf::from("/local/debug\\kernel")),
            rewrites: vec![
                SourcePathRewrite::new("/remote/build", "/local/checkout").unwrap(),
                SourcePathRewrite::new("/wrkdirs/usr/ports", "/local/ports").unwrap(),
            ],
        };
        let encoded = encode_source_lookup_preference(&preference);
        assert_eq!(decode_source_lookup_preference(&encoded), Some(preference));
    }

    #[test]
    fn saved_source_lookup_prefills_without_becoming_accepted() {
        let binary = ProfileBinaryId::Kernel(19);
        let binary_path = PathBuf::from("/local/debug/kernel");
        let rewrite = SourcePathRewrite::new("/remote/build", "/local/checkout").unwrap();
        let mut app = EventExplorerApp::new_empty(None);
        app.source_lookup_preferences = vec![SourceLookupPreference {
            binary_name: "old-kernel-name".to_string(),
            captured_path: "/old/target/kernel".to_string(),
            build_id: "AABBCC".to_string(),
            binary_path: Some(binary_path.clone()),
            rewrites: vec![rewrite.clone()],
        }];
        app.flame = Some(FlameViewState::new_empty());
        app.flame.as_mut().unwrap().source_view =
            Some(Box::new(app.source_view_state(SourceOpenRequest {
                function: ProfileFunctionId { binary, offset: 42 },
                function_name: "function".to_string(),
                binary_name: "kernel".to_string(),
                captured_path: "/new/target/kernel".to_string(),
                build_id: "aabbcc".to_string(),
                sampled_offsets: Vec::new(),
                stack_path: Vec::new(),
                split_user_by_pid: false,
            })));
        let view = app.flame.as_ref().unwrap().source_view.as_ref().unwrap();
        assert_eq!(app.source_binary_paths.get(&binary), None);
        assert_eq!(app.source_path_rewrites.get(&binary), None);
        assert_eq!(view.binary_path_input, binary_path.display().to_string());
        assert!(matches!(view.state, SourceLoadState::NeedBinary(_)));
        let expected_path = std::path::Path::new("/remote/build/project/sys/net/if.c");
        assert_eq!(
            default_source_rewrite(expected_path, &[], std::slice::from_ref(&rewrite)),
            Some(&rewrite)
        );
        assert!(!app.source_path_rewrites.contains_key(&binary));
    }

    #[test]
    fn saved_source_lookup_falls_back_to_captured_executable_for_a_new_build() {
        let preference = SourceLookupPreference {
            binary_name: "kernel".to_string(),
            captured_path: "/boot/kernel/kernel".to_string(),
            build_id: "old-build".to_string(),
            binary_path: Some(PathBuf::from("/local/debug/kernel")),
            rewrites: Vec::new(),
        };
        let request = SourceOpenRequest {
            function: ProfileFunctionId {
                binary: ProfileBinaryId::Kernel(1),
                offset: 0,
            },
            function_name: "function".to_string(),
            binary_name: "kernel".to_string(),
            captured_path: "/boot/kernel/kernel".to_string(),
            build_id: "new-build".to_string(),
            sampled_offsets: Vec::new(),
            stack_path: Vec::new(),
            split_user_by_pid: false,
        };
        assert!(!preference.matches_build(&request));
        assert!(preference.matches_captured_executable(&request));
    }

    #[test]
    fn ready_source_focuses_hottest_and_navigates_sampled_lines() {
        let request = SourceOpenRequest {
            function: ProfileFunctionId {
                binary: ProfileBinaryId::Kernel(1),
                offset: 0,
            },
            function_name: "function".to_string(),
            binary_name: "kernel".to_string(),
            captured_path: String::new(),
            build_id: String::new(),
            sampled_offsets: Vec::new(),
            stack_path: Vec::new(),
            split_user_by_pid: false,
        };
        let mut view = SourceViewState::new(
            1,
            request,
            Arc::new(CpuProfile::default()),
            FlameFilter::default(),
            String::new(),
            None,
        );
        let line_samples: rustc_hash::FxHashMap<u32, u64> =
            [(12, 3), (40, 20), (91, 7)].into_iter().collect();
        view.set_ready_source(AnnotatedSource {
            source_path: PathBuf::from("/local/source.c"),
            origin: "DWARF: /local/kernel".to_string(),
            ref_note: None,
            lines: vec![String::new(); 100],
            line_samples,
            function_line_range: None,
            hottest_line: 40,
            total_mapped: 30,
            function_total: 30,
        });
        assert_eq!(view.focused_source_line, Some(40));
        assert_eq!(view.pending_source_scroll, Some(40));
        let SourceLoadState::Ready(source) = &view.state else {
            panic!("source did not become ready");
        };
        let order = sampled_lines_hottest_first(&source.line_samples);
        assert_eq!(order, vec![40, 91, 12]);
        assert_eq!(hotter_sampled_line(&order, 40), None);
        assert_eq!(colder_sampled_line(&order, 40), Some(91));
        assert_eq!(hotter_sampled_line(&order, 12), Some(91));
        assert_eq!(colder_sampled_line(&order, 12), None);
        assert_eq!(centered_source_scroll_offset(1, 20.0, 200.0), 0.0);
        assert_eq!(centered_source_scroll_offset(40, 20.0, 200.0), 690.0);
    }

    #[test]
    fn need_source_transition_prefills_detected_dwarf_prefix() {
        let request = SourceOpenRequest {
            function: ProfileFunctionId {
                binary: ProfileBinaryId::Kernel(1),
                offset: 0,
            },
            function_name: "function".to_string(),
            binary_name: "kernel".to_string(),
            captured_path: String::new(),
            build_id: String::new(),
            sampled_offsets: Vec::new(),
            stack_path: Vec::new(),
            split_user_by_pid: false,
        };
        let mut view = SourceViewState::new(
            1,
            request,
            Arc::new(CpuProfile::default()),
            FlameFilter::default(),
            String::new(),
            None,
        );
        let (tx, rx) = mpsc::channel();
        let binary_path = PathBuf::from("/remote/tree/.obj/target/kernel");
        let expected_path = PathBuf::from(
            "/remote/tree/.obj/target/remote/tree/project/arch/sys/CONF/../../../../../../../../project/sys/header.h",
        );
        let detected_prefix = dwarf_prefix_for_paths(&expected_path, &binary_path);
        tx.send(SourceLoadResult::NeedSource {
            message: String::new(),
            binary_path,
            expected_path,
            detected_prefix,
            line_samples: rustc_hash::FxHashMap::default(),
            line_offsets: rustc_hash::FxHashMap::default(),
            function_name: "function".to_string(),
        })
        .unwrap();
        view.state = SourceLoadState::Loading(rx);
        view.poll();
        assert_eq!(view.rewrite_from_input, "/remote/tree");
    }

    #[test]
    fn opening_another_function_reuses_binary_for_its_image() {
        let binary = ProfileBinaryId::Kernel(7);
        let binary_path = PathBuf::from("/local/debug/kernel");
        let mut app = EventExplorerApp::new_empty(None);
        app.source_binary_paths.insert(binary, binary_path.clone());
        app.flame = Some(FlameViewState::new_empty());
        app.flame.as_mut().unwrap().source_view =
            Some(Box::new(app.source_view_state(SourceOpenRequest {
                function: ProfileFunctionId { binary, offset: 42 },
                function_name: "another_function".to_string(),
                binary_name: "kernel".to_string(),
                captured_path: String::new(),
                build_id: String::new(),
                sampled_offsets: Vec::new(),
                stack_path: Vec::new(),
                split_user_by_pid: false,
            })));
        let view = app.flame.as_ref().unwrap().source_view.as_ref().unwrap();
        assert_eq!(view.binary_path_input, binary_path.display().to_string());
        assert!(matches!(view.state, SourceLoadState::Loading(_)));
    }

    #[test]
    fn source_rewrite_is_inferred_from_matching_source_files() {
        let rewrite = infer_source_rewrite_from_files(
            std::path::Path::new("/remote/build/project/sys/net/if.c"),
            std::path::Path::new("/local/checkout/project/sys/net/if.c"),
        )
        .unwrap();
        assert_eq!(rewrite.dwarf_prefix, PathBuf::from("/remote/build"));
        assert_eq!(rewrite.local_prefix, PathBuf::from("/local/checkout"));
    }

    #[test]
    fn find_function_line_range_detects_c_definitions() {
        let code = r#"
#include <stdio.h>

/* Prototype declaration should be skipped */
static int helper(int x);

static int helper(int x)
{
    return x * 2;
}

int do_work(void *p)
{
    if (p == NULL) {
        return -1;
    }
    return 0;
}
"#;
        let lines: Vec<String> = code.lines().map(str::to_owned).collect();
        let helper_range = find_function_line_range(&lines, "helper");
        assert_eq!(helper_range, Some((7, 10)));

        // Prefixed symbol name "tcp_ngen_do_work" matches "do_work" in C source
        let work_range = find_function_line_range(&lines, "tcp_ngen_do_work");
        assert_eq!(work_range, Some((12, 18)));

        let work_direct = find_function_line_range(&lines, "do_work");
        assert_eq!(work_direct, Some((12, 18)));
    }

    #[test]
    fn build_annotated_source_keeps_inlined_helper_lines() {
        let code = r#"
static void other_func(void)
{
    int a = 1;
}

static void target_func(void)
{
    int b = 2;
    int c = 3;
}
"#;
        let lines: Vec<String> = code.lines().map(str::to_owned).collect();
        let line_samples = [(4, 100), (9, 50), (10, 10)].into_iter().collect();
        let source = build_annotated_source(
            PathBuf::from("/test.c"),
            "test".to_string(),
            None,
            lines,
            line_samples,
            rustc_hash::FxHashMap::default(),
            "target_func",
        );
        assert_eq!(source.function_line_range, Some((7, 11)));
        assert_eq!(source.line_samples.get(&4), Some(&100));
        assert_eq!(source.line_samples.get(&9), Some(&50));
        assert_eq!(source.line_samples.get(&10), Some(&10));
        assert_eq!(source.function_total, 60);
        assert_eq!(source.total_mapped, 160);
        assert_eq!(source.hottest_line, 4);
    }

    #[test]
    fn line_highlight_alpha_is_transparent_for_low_sample_functions() {
        let single_sample_alpha = line_highlight_alpha(1, 1, 1);
        assert!(
            single_sample_alpha <= 25,
            "expected <= 25, got {single_sample_alpha}"
        );

        let hot_line_alpha = line_highlight_alpha(10_000, 10_000, 10_000);
        assert!(
            hot_line_alpha >= 120,
            "expected >= 120, got {hot_line_alpha}"
        );

        assert_eq!(line_highlight_alpha(0, 10_000, 10_000), 0);
    }

    #[test]
    fn open_flame_source_reuses_panel_at_hottest_or_requested() {
        let binary = ProfileBinaryId::Kernel(1);
        let mut app = EventExplorerApp::new_empty(None);
        let request = SourceOpenRequest {
            function: ProfileFunctionId {
                binary,
                offset: 100,
            },
            function_name: "target_func".to_string(),
            binary_name: "kernel".to_string(),
            captured_path: String::new(),
            build_id: String::new(),
            sampled_offsets: Vec::new(),
            stack_path: Vec::new(),
            split_user_by_pid: false,
        };
        app.flame = Some(FlameViewState::new_empty());
        app.flame.as_mut().unwrap().source_view =
            Some(Box::new(app.source_view_state(request.clone())));

        {
            let view = app
                .flame
                .as_mut()
                .unwrap()
                .source_view
                .as_deref_mut()
                .unwrap();
            view.set_ready_source(AnnotatedSource {
                source_path: PathBuf::from("/test.c"),
                origin: "test".to_string(),
                ref_note: None,
                lines: vec![String::new(); 100],
                line_samples: [(55, 10)].into_iter().collect(),
                function_line_range: Some((50, 70)),
                hottest_line: 55,
                total_mapped: 10,
                function_total: 10,
            });
            view.focused_source_line = Some(90);
            view.pending_source_scroll = None;
        }

        let view = app.source_view_state(request);
        assert_eq!(view.focused_source_line, Some(55));
        assert_eq!(view.pending_source_scroll, Some(55));

        let req_with_line = SourceOpenRequest {
            function: ProfileFunctionId {
                binary,
                offset: 100,
            },
            function_name: "target_func".to_string(),
            binary_name: "kernel".to_string(),
            captured_path: String::new(),
            build_id: String::new(),
            sampled_offsets: Vec::new(),
            stack_path: vec![(9, 0), (10, 1)],
            split_user_by_pid: false,
        };
        app.flame.as_mut().unwrap().source_view = Some(Box::new(view));
        app.flame
            .as_mut()
            .unwrap()
            .source_view
            .as_deref_mut()
            .unwrap()
            .focused_source_line = Some(90);
        let view = app.source_view_state(req_with_line);
        assert_eq!(view.focused_source_line, Some(55));
        assert_eq!(view.request.stack_path, vec![(9, 0), (10, 1)]);
    }

    #[test]
    fn sampled_lines_cycle_from_hottest_to_coldest() {
        let samples = [(10, 1u64), (20, 50), (30, 5)]
            .into_iter()
            .collect::<rustc_hash::FxHashMap<_, _>>();
        let order = sampled_lines_hottest_first(&samples);
        assert_eq!(order, vec![20, 30, 10]);
        assert_eq!(hotter_sampled_line(&order, 20), None);
        assert_eq!(colder_sampled_line(&order, 20), Some(30));
        assert_eq!(hotter_sampled_line(&order, 10), Some(30));
        assert_eq!(colder_sampled_line(&order, 10), None);
        assert_eq!(colder_sampled_line(&order, 99), Some(20));
    }
}
