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

//! Events tab: per-provider event id, name, count, and on-disk size.

#[allow(unused_imports)]
use super::*;

use crate::elog_parser::{EventFieldStat, EventStat};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventSortColumn {
    Provider,
    Id,
    Name,
    Level,
    Count,
    Bytes,
    Avg,
    Rate,
}

#[derive(Clone)]
pub(crate) struct EventsTabState {
    pub open: bool,
    pub sort: EventSortColumn,
    pub sort_ascending: bool,
    pub filter: String,
    /// Selected provider name. `None` shows every name.
    pub provider: Option<String>,
    pub show_unused: bool,
    pub selected: Option<(String, u32)>,
}

impl Default for EventsTabState {
    fn default() -> Self {
        Self {
            open: false,
            sort: EventSortColumn::Bytes,
            sort_ascending: false,
            filter: String::new(),
            provider: None,
            show_unused: false,
            selected: None,
        }
    }
}

struct EventRow {
    provider: String,
    color: Color32,
    event_id: u32,
    name: String,
    level: Option<u8>,
    count: u64,
    bytes: u64,
    payload_bytes: u64,
    min_len: u32,
    max_len: u32,
    first_ts: Option<u64>,
    last_ts: Option<u64>,
    keywords: String,
    defined_only: bool,
}

impl EventRow {
    fn from_stat(provider: String, color: Color32, ev: &EventStat) -> Self {
        Self {
            provider,
            color,
            event_id: ev.event_id,
            name: ev.name.clone(),
            level: ev.level,
            count: ev.count,
            bytes: ev.bytes,
            payload_bytes: ev.payload_bytes,
            min_len: ev.min_len,
            max_len: ev.max_len,
            first_ts: ev.first_ts,
            last_ts: ev.last_ts,
            keywords: ev.keywords.join(", "),
            defined_only: ev.defined_only,
        }
    }

    /// Records per second when this event spans at least one second.
    fn rate(&self) -> Option<f64> {
        let (Some(a), Some(b)) = (self.first_ts, self.last_ts) else {
            return None;
        };
        let span_us = b.saturating_sub(a);
        if span_us < 1_000_000 || self.count < 2 {
            return None;
        }
        Some(self.count as f64 / (span_us as f64 / 1_000_000.0))
    }

    fn avg_record(&self) -> u64 {
        self.bytes.checked_div(self.count).unwrap_or(0)
    }
}

struct ProviderGroup {
    name: String,
    color: Color32,
    ids: Vec<u16>,
    event_count: u64,
    byte_size: u64,
    events: Vec<EventStat>,
}

fn display_name(name: &str) -> String {
    if name.is_empty() {
        "(unnamed)".to_string()
    } else {
        name.to_string()
    }
}

/// One entry per provider name. Same-named providers share a color and
/// their event ids are summed.
fn provider_groups(providers: &[Provider]) -> Vec<ProviderGroup> {
    let mut groups: Vec<ProviderGroup> = Vec::new();
    for p in providers {
        let name = display_name(&p.name);
        if let Some(g) = groups.iter_mut().find(|g| g.name == name) {
            g.ids.push(p.id);
            g.event_count += p.event_count;
            g.byte_size += p.byte_size;
            for ev in &p.events {
                if let Some(existing) = g.events.iter_mut().find(|e| e.event_id == ev.event_id) {
                    merge_stat(existing, ev);
                } else {
                    g.events.push(ev.clone());
                }
            }
        } else {
            groups.push(ProviderGroup {
                name,
                color: Color32::PLACEHOLDER,
                ids: vec![p.id],
                event_count: p.event_count,
                byte_size: p.byte_size,
                events: p.events.clone(),
            });
        }
    }
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(groups[i].byte_size));
    for (rank, idx) in order.into_iter().enumerate() {
        groups[idx].color = series_color(rank);
    }
    groups
}

fn merge_stat(dst: &mut EventStat, src: &EventStat) {
    let dst_seen = dst.count > 0;
    let src_seen = src.count > 0;
    dst.count += src.count;
    dst.bytes += src.bytes;
    dst.payload_bytes += src.payload_bytes;
    dst.defined_only = dst.count == 0;
    if src_seen && !dst_seen {
        dst.min_len = src.min_len;
        dst.max_len = src.max_len;
        dst.first_ts = src.first_ts;
        dst.last_ts = src.last_ts;
    } else if src_seen && dst_seen {
        dst.min_len = dst.min_len.min(src.min_len);
        dst.max_len = dst.max_len.max(src.max_len);
        dst.first_ts = match (dst.first_ts, src.first_ts) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        dst.last_ts = match (dst.last_ts, src.last_ts) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };
    }
    if dst.level.is_none() {
        dst.level = src.level;
    }
    if dst.keywords.is_empty() {
        dst.keywords.clone_from(&src.keywords);
    }
    if dst.fields.is_empty() {
        dst.fields.clone_from(&src.fields);
    }
    if dst.format.is_empty() {
        dst.format.clone_from(&src.format);
    }
    if dst.name.starts_with("id_") && !src.name.starts_with("id_") {
        dst.name.clone_from(&src.name);
    }
}

fn level_label(level: Option<u8>) -> String {
    match level {
        Some(1) => "ERROR".to_string(),
        Some(2) => "WARN".to_string(),
        Some(3) => "INFO".to_string(),
        Some(4) => "VERBOSE".to_string(),
        Some(5) => "TRACE".to_string(),
        Some(n) => n.to_string(),
        None => "—".to_string(),
    }
}

fn format_share(part: u64, whole: u64) -> String {
    if whole == 0 || part == 0 {
        return "0%".to_string();
    }
    let pct = part as f64 / whole as f64 * 100.0;
    if pct < 0.1 {
        "<0.1%".to_string()
    } else {
        format!("{pct:.1}%")
    }
}

fn format_rate(per_sec: Option<f64>) -> String {
    let Some(per_sec) = per_sec else {
        return "—".to_string();
    };
    if per_sec >= 100.0 {
        format!("{}/s", format_count(per_sec.round() as u64))
    } else if per_sec >= 10.0 {
        format!("{per_sec:.0}/s")
    } else {
        format!("{per_sec:.1}/s")
    }
}

fn format_len(n: u32, seen: bool) -> String {
    if !seen {
        "—".to_string()
    } else if n < 1024 {
        format!("{n} B")
    } else {
        format_bytes(n as u64)
    }
}

fn text_match(row: &EventRow, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    let q = q.to_ascii_lowercase();
    row.provider.to_ascii_lowercase().contains(&q)
        || row.name.to_ascii_lowercase().contains(&q)
        || row.event_id.to_string().contains(&q)
        || row.keywords.to_ascii_lowercase().contains(&q)
        || level_label(row.level).to_ascii_lowercase().contains(&q)
}

fn collect_rows(groups: &[ProviderGroup], state: &EventsTabState) -> Vec<EventRow> {
    let mut rows = Vec::new();
    for g in groups {
        if state.provider.as_ref().is_some_and(|name| name != &g.name) {
            continue;
        }
        for ev in &g.events {
            if ev.defined_only && !state.show_unused {
                continue;
            }
            rows.push(EventRow::from_stat(g.name.clone(), g.color, ev));
        }
    }
    rows
}

fn sort_rows(rows: &mut [EventRow], col: EventSortColumn, ascending: bool) {
    rows.sort_by(|a, b| {
        let ord = match col {
            EventSortColumn::Provider => a
                .provider
                .cmp(&b.provider)
                .then(a.event_id.cmp(&b.event_id)),
            EventSortColumn::Id => a
                .event_id
                .cmp(&b.event_id)
                .then(a.provider.cmp(&b.provider)),
            EventSortColumn::Name => a.name.cmp(&b.name).then(a.provider.cmp(&b.provider)),
            EventSortColumn::Level => a
                .level
                .unwrap_or(0)
                .cmp(&b.level.unwrap_or(0))
                .then(a.name.cmp(&b.name)),
            EventSortColumn::Count => a.count.cmp(&b.count),
            EventSortColumn::Bytes => a.bytes.cmp(&b.bytes),
            EventSortColumn::Avg => a.avg_record().cmp(&b.avg_record()),
            EventSortColumn::Rate => a.rate().unwrap_or(0.0).total_cmp(&b.rate().unwrap_or(0.0)),
        };
        if ascending {
            ord
        } else {
            ord.reverse()
        }
    });
}

fn click_sort(state: &mut EventsTabState, col: EventSortColumn) {
    if state.sort == col {
        state.sort_ascending = !state.sort_ascending;
    } else {
        state.sort = col;
        state.sort_ascending = matches!(
            col,
            EventSortColumn::Provider | EventSortColumn::Name | EventSortColumn::Level
        );
    }
}

fn rel_time(ts: Option<u64>, t0: u64) -> String {
    let Some(ts) = ts else {
        return "—".to_string();
    };
    let sec = ts.saturating_sub(t0) as f64 / 1_000_000.0;
    format_time_span(sec)
}

impl EventExplorerApp {
    pub(crate) fn close_events_tab(&mut self) {
        self.events_tab.open = false;
        if self.active_tab != Tab::Events {
            return;
        }
        if self.system_tab_available() {
            self.active_tab = Tab::Aggregate;
        } else if let Some(pv) = self.open_sessions.first() {
            self.active_tab = Tab::Session(pv.summary.session_id);
        } else if !self.cpu_profile.is_empty() {
            self.active_tab = Tab::CpuFlame;
        } else if let Some(c) = self.open_ella_channels.first() {
            self.active_tab = Tab::EllaChannel {
                provider_id: c.provider_id,
                session_id: c.session_id,
            };
        } else {
            self.active_tab = Tab::Aggregate;
        }
    }

    pub(crate) fn show_events_view(&mut self, ui: &mut egui::Ui) {
        let t0 = self
            .file_info
            .as_ref()
            .map(|f| f.capture_start)
            .unwrap_or(0);
        let span_us = self
            .file_info
            .as_ref()
            .map(|f| f.capture_end.saturating_sub(f.capture_start))
            .unwrap_or(0);
        let groups = provider_groups(&self.providers);
        let mut rows = collect_rows(&groups, &self.events_tab);
        let recorded: Vec<&EventRow> = rows.iter().filter(|r| r.count > 0).collect();
        let total_count: u64 = recorded.iter().map(|r| r.count).sum();
        let total_bytes: u64 = recorded.iter().map(|r| r.bytes).sum();
        let total_payload: u64 = recorded.iter().map(|r| r.payload_bytes).sum();
        let header_bytes = total_count.saturating_mul(32);
        let types = recorded.len();
        let providers_shown = groups
            .iter()
            .filter(|g| {
                self.events_tab
                    .provider
                    .as_ref()
                    .is_none_or(|name| name == &g.name)
                    && g.events
                        .iter()
                        .any(|e| e.count > 0 || self.events_tab.show_unused)
            })
            .count();

        ui.horizontal(|ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new("Events").strong().size(16.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.events_tab.filter)
                        .desired_width(200.0)
                        .hint_text("name, id, keyword"),
                );
            });
        });

        let summary = if total_count == 0 {
            "No event records in this view.".to_string()
        } else {
            let rate = total_count
                .saturating_mul(1_000_000)
                .checked_div(span_us)
                .map(|per_sec| {
                    format!(
                        " · {}/s over {}",
                        format_count(per_sec),
                        format_time_span(span_us as f64 / 1_000_000.0),
                    )
                })
                .unwrap_or_default();
            format!(
                "{} providers · {} event types · {} events · {} · headers {} ({}) · payload {}{rate}",
                providers_shown,
                types,
                format_count(total_count),
                format_bytes(total_bytes),
                format_bytes(header_bytes),
                format_share(header_bytes, total_bytes),
                format_bytes(total_payload),
            )
        };
        ui.label(RichText::new(summary).size(12.0).weak());
        ui.add_space(6.0);

        if total_bytes > 0 {
            composition_bar(ui, total_payload, header_bytes);
            ui.add_space(8.0);
        }

        let mut picked_provider: Option<Option<String>> = None;
        if !groups.is_empty() {
            ui.label(RichText::new("By provider").strong().size(13.0));
            provider_bar(
                ui,
                &groups,
                self.events_tab.provider.as_deref(),
                &mut picked_provider,
            );
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let all_on = self.events_tab.provider.is_none();
                if ui
                    .selectable_label(all_on, RichText::new("All").size(11.0))
                    .clicked()
                {
                    picked_provider = Some(None);
                }
                for g in &groups {
                    let on = self.events_tab.provider.as_deref() == Some(g.name.as_str());
                    let text = RichText::new(&g.name).size(11.0).color(g.color);
                    let n = if g.ids.len() > 1 {
                        format!("{} providers · ", g.ids.len())
                    } else {
                        String::new()
                    };
                    let resp = ui.selectable_label(on, text).on_hover_text(format!(
                        "{n}{} events · {}",
                        format_count(g.event_count),
                        format_bytes(g.byte_size),
                    ));
                    if resp.clicked() {
                        picked_provider = Some(Some(g.name.clone()));
                    }
                }
            });
            ui.add_space(8.0);
        }

        let query = self.events_tab.filter.clone();
        rows.retain(|r| text_match(r, &query));

        ui.checkbox(
            &mut self.events_tab.show_unused,
            RichText::new("Show schema events with no records").size(12.0),
        )
        .on_hover_text("Event ids the decode schema defines that this capture never wrote.");

        sort_rows(
            &mut rows,
            self.events_tab.sort,
            self.events_tab.sort_ascending,
        );
        let selected = self.events_tab.selected.clone();
        let detail_w = 540.0;
        let side_by_side = selected.is_some() && ui.available_width() > detail_w + 720.0;
        let avail_h = ui.available_height();
        let detail_h = if selected.is_some() && !side_by_side {
            440.0
        } else {
            0.0
        };
        let table_h = if side_by_side {
            avail_h
        } else {
            (avail_h - detail_h).max(140.0)
        };
        let mut sort_click: Option<EventSortColumn> = None;
        let mut row_click: Option<(String, u32)> = None;
        let sort = self.events_tab.sort;
        let ascending = self.events_tab.sort_ascending;
        let mut show_table = |ui: &mut egui::Ui| {
            egui::ScrollArea::horizontal()
                .id_source("events_table_hscroll")
                .show(ui, |ui| {
                    ui.set_min_width(980.0);
                    events_table(
                        ui,
                        &rows,
                        &mut EventTableOpts {
                            total_bytes,
                            sort,
                            ascending,
                            selected: selected.clone(),
                            table_h,
                            sort_click: &mut sort_click,
                            row_click: &mut row_click,
                        },
                    );
                });
        };

        if side_by_side {
            if let Some((name, eid)) = selected.clone() {
                ui.horizontal_top(|ui| {
                    let table_w = (ui.available_width() - detail_w - 16.0).max(480.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(table_w);
                        show_table(ui);
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_max_width(detail_w);
                        egui::ScrollArea::vertical()
                            .id_source("events_detail_vscroll")
                            .max_height(table_h)
                            .show(ui, |ui| {
                                show_event_detail(ui, &groups, &name, eid, t0);
                            });
                    });
                });
            }
        } else {
            show_table(ui);
            if let Some((name, eid)) = selected {
                ui.add_space(4.0);
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_source("events_detail_vscroll")
                    .max_height(detail_h)
                    .show(ui, |ui| {
                        show_event_detail(ui, &groups, &name, eid, t0);
                    });
            }
        }

        if let Some(name) = picked_provider {
            self.events_tab.provider = match (&self.events_tab.provider, &name) {
                (Some(cur), Some(next)) if cur == next => None,
                _ => name,
            };
        }
        if let Some(col) = sort_click {
            click_sort(&mut self.events_tab, col);
        }
        if let Some(key) = row_click {
            self.events_tab.selected = if self.events_tab.selected.as_ref() == Some(&key) {
                None
            } else {
                Some(key)
            };
        }
    }
}

fn composition_bar(ui: &mut egui::Ui, payload: u64, headers: u64) {
    let total = payload + headers;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 16.0), egui::Sense::hover());
    let payload_w = rect.width() * (payload as f32 / total.max(1) as f32);
    let payload_rect = egui::Rect::from_min_size(rect.min, egui::vec2(payload_w, rect.height()));
    let header_rect =
        egui::Rect::from_min_max(egui::pos2(rect.left() + payload_w, rect.top()), rect.max);
    let payload_color = series_color(0);
    let header_color = series_color(4);
    ui.painter().rect_filled(payload_rect, 2.0, payload_color);
    ui.painter().rect_filled(header_rect, 2.0, header_color);
    ui.horizontal(|ui| {
        swatch(ui, payload_color);
        ui.label(RichText::new(format!("Payload {}", format_bytes(payload))).size(11.0));
        ui.add_space(8.0);
        swatch(ui, header_color);
        ui.label(RichText::new(format!("Record headers {}", format_bytes(headers))).size(11.0));
    });
}

fn swatch(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, color);
}

fn provider_bar(
    ui: &mut egui::Ui,
    groups: &[ProviderGroup],
    selected: Option<&str>,
    picked: &mut Option<Option<String>>,
) {
    let total: u64 = groups.iter().map(|g| g.byte_size).sum();
    if total == 0 {
        ui.label(RichText::new("No recorded bytes.").weak().size(12.0));
        return;
    }
    let width = ui.available_width();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 1.0;
        let mut used = 0.0;
        let segs: Vec<usize> = {
            let mut idx: Vec<usize> = (0..groups.len())
                .filter(|&i| groups[i].byte_size > 0)
                .collect();
            idx.sort_by_key(|&i| std::cmp::Reverse(groups[i].byte_size));
            idx
        };
        for (n, i) in segs.iter().copied().enumerate() {
            let share = groups[i].byte_size as f32 / total as f32;
            let w = if n + 1 == segs.len() {
                (width - used).max(2.0)
            } else {
                (width * share).max(2.0)
            };
            used += w;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 18.0), egui::Sense::click());
            let mut color = groups[i].color;
            if selected.is_some_and(|name| name != groups[i].name) {
                color = color.gamma_multiply(0.35);
            }
            ui.painter().rect_filled(rect, 1.0, color);
            let nprov = if groups[i].ids.len() > 1 {
                format!("{} providers · ", groups[i].ids.len())
            } else {
                String::new()
            };
            let resp = resp.on_hover_text(format!(
                "{nprov}{} · {} events · {} ({})",
                groups[i].name,
                format_count(groups[i].event_count),
                format_bytes(groups[i].byte_size),
                format_share(groups[i].byte_size, total),
            ));
            if resp.clicked() {
                *picked = Some(Some(groups[i].name.clone()));
            }
        }
    });
}

struct EventTableOpts<'a> {
    total_bytes: u64,
    sort: EventSortColumn,
    ascending: bool,
    selected: Option<(String, u32)>,
    table_h: f32,
    sort_click: &'a mut Option<EventSortColumn>,
    row_click: &'a mut Option<(String, u32)>,
}

fn events_table(ui: &mut egui::Ui, rows: &[EventRow], opts: &mut EventTableOpts<'_>) {
    // Hover-only labels, so a press on the text selects the row.
    ui.style_mut().interaction.selectable_labels = false;
    let table = TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::initial(110.0).at_least(72.0))
        .column(Column::initial(52.0).at_least(40.0))
        .column(Column::initial(200.0).at_least(100.0).clip(true))
        .column(Column::initial(68.0).at_least(52.0))
        .column(Column::initial(80.0).at_least(56.0))
        .column(Column::initial(64.0).at_least(48.0))
        .column(Column::initial(78.0).at_least(56.0))
        .column(Column::initial(64.0).at_least(48.0))
        .column(Column::initial(64.0).at_least(48.0))
        .column(Column::initial(56.0).at_least(44.0))
        .column(Column::initial(56.0).at_least(44.0))
        .column(Column::initial(72.0).at_least(52.0))
        .min_scrolled_height(0.0)
        .max_scroll_height(opts.table_h)
        .sense(egui::Sense::click());
    let cols = [
        (Some(EventSortColumn::Provider), "Provider", "Provider name"),
        (Some(EventSortColumn::Id), "ID", "Event id"),
        (Some(EventSortColumn::Name), "Event", "Event name"),
        (Some(EventSortColumn::Level), "Level", "Schema level"),
        (
            Some(EventSortColumn::Count),
            "Count",
            "Records in the capture",
        ),
        (None, "Share", "Share of recorded bytes"),
        (
            Some(EventSortColumn::Bytes),
            "Bytes",
            "Header plus payload, summed",
        ),
        (Some(EventSortColumn::Avg), "Avg", "Average record size"),
        (None, "Payload", "Average payload size"),
        (None, "Min", "Smallest record"),
        (None, "Max", "Largest record"),
        (
            Some(EventSortColumn::Rate),
            "/s",
            "Records per second over this event's own span",
        ),
    ];
    table
        .header(22.0, |mut row| {
            for (col, label, tip) in cols {
                row.col(|ui| {
                    let arrow = match col {
                        Some(c) if c == opts.sort => {
                            if opts.ascending {
                                " ^"
                            } else {
                                " v"
                            }
                        }
                        _ => "",
                    };
                    let resp = ui
                        .add(
                            egui::Label::new(
                                RichText::new(format!("{label}{arrow}")).strong().size(12.0),
                            )
                            .sense(egui::Sense::click()),
                        )
                        .on_hover_text(tip);
                    if let Some(col) = col {
                        if resp.clicked() {
                            *opts.sort_click = Some(col);
                        }
                    }
                });
            }
        })
        .body(|body| {
            body.rows(20.0, rows.len(), |mut row| {
                let ev = &rows[row.index()];
                let on = opts
                    .selected
                    .as_ref()
                    .is_some_and(|(name, id)| name == &ev.provider && *id == ev.event_id);
                let style = |s: String| {
                    let mut t = RichText::new(s).size(12.0);
                    if on {
                        t = t.strong();
                    } else if ev.defined_only {
                        t = t.weak();
                    }
                    t
                };
                row.set_selected(on);
                row.col(|ui| {
                    ui.label(style(ev.provider.clone()));
                });
                row.col(|ui| {
                    ui.label(style(ev.event_id.to_string()));
                });
                row.col(|ui| {
                    ui.label(style(ev.name.clone()));
                });
                row.col(|ui| {
                    ui.label(style(level_label(ev.level)));
                });
                row.col(|ui| {
                    ui.label(style(format_count(ev.count)));
                });
                row.col(|ui| {
                    let frac = if opts.total_bytes == 0 {
                        0.0
                    } else {
                        ev.bytes as f32 / opts.total_bytes as f32
                    };
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width().max(8.0), 10.0),
                        egui::Sense::hover(),
                    );
                    ui.painter()
                        .rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
                    let fill = rect.with_max_x(rect.left() + rect.width() * frac.clamp(0.0, 1.0));
                    ui.painter().rect_filled(fill, 2.0, ev.color);
                });
                row.col(|ui| {
                    ui.label(style(format_bytes(ev.bytes)));
                });
                row.col(|ui| {
                    ui.label(style(format_len(ev.avg_record() as u32, ev.count > 0)));
                });
                row.col(|ui| {
                    let avg_payload = ev.payload_bytes.checked_div(ev.count).unwrap_or(0) as u32;
                    ui.label(style(format_len(avg_payload, ev.count > 0)));
                });
                row.col(|ui| {
                    ui.label(style(format_len(ev.min_len, ev.count > 0)));
                });
                row.col(|ui| {
                    ui.label(style(format_len(ev.max_len, ev.count > 0)));
                });
                row.col(|ui| {
                    ui.label(style(format_rate(ev.rate())));
                });
                if row.response().clicked() {
                    *opts.row_click = Some((ev.provider.clone(), ev.event_id));
                }
            });
        });
}

fn show_event_detail(
    ui: &mut egui::Ui,
    groups: &[ProviderGroup],
    provider: &str,
    event_id: u32,
    t0: u64,
) {
    let Some(group) = groups.iter().find(|g| g.name == provider) else {
        return;
    };
    let Some(ev) = group.events.iter().find(|e| e.event_id == event_id) else {
        return;
    };
    ui.label(
        RichText::new(format!("{provider}  {}  (id {event_id})", ev.name))
            .strong()
            .size(14.0),
    );
    ui.add_space(2.0);
    let span = match (ev.first_ts, ev.last_ts) {
        (Some(a), Some(b)) if b > a => format_time_span((b - a) as f64 / 1_000_000.0),
        (Some(_), Some(_)) => "0 s".to_string(),
        _ => "—".to_string(),
    };
    egui::Grid::new("event_detail_stats")
        .num_columns(4)
        .spacing([16.0, 2.0])
        .show(ui, |ui| {
            let pair = |ui: &mut egui::Ui, k: &str, v: String| {
                ui.label(RichText::new(k).weak().size(11.0));
                ui.label(RichText::new(v).size(11.0));
            };
            if group.ids.len() > 1 {
                pair(ui, "Providers", format_count(group.ids.len() as u64));
                ui.end_row();
            }
            pair(ui, "Level", level_label(ev.level));
            pair(
                ui,
                "Keywords",
                if ev.keywords.is_empty() {
                    "—".to_string()
                } else {
                    ev.keywords.join(", ")
                },
            );
            ui.end_row();
            pair(ui, "First", rel_time(ev.first_ts, t0));
            pair(ui, "Last", rel_time(ev.last_ts, t0));
            ui.end_row();
            pair(ui, "Span", span);
            pair(
                ui,
                "Rate",
                format_rate(EventRow::from_stat(String::new(), Color32::WHITE, ev).rate()),
            );
            ui.end_row();
            pair(ui, "Records", format_bytes(ev.bytes));
            pair(ui, "Payload", format_bytes(ev.payload_bytes));
            ui.end_row();
        });
    if !ev.format.is_empty() {
        ui.add_space(2.0);
        ui.label(
            RichText::new(format!("Format  {}", ev.format))
                .size(11.0)
                .weak(),
        );
    }
    ui.add_space(4.0);
    if ev.fields.is_empty() {
        let msg = if ev.level.is_some() {
            "No payload fields in the schema."
        } else {
            "No decode schema for this event."
        };
        ui.label(RichText::new(msg).weak().size(12.0));
        return;
    }
    ui.label(RichText::new("Payload fields").strong().size(12.0));
    egui::Grid::new("event_detail_fields")
        .num_columns(5)
        .spacing([14.0, 2.0])
        .striped(true)
        .show(ui, |ui| {
            for h in ["Offset", "Size", "Field", "Bytes", "Share"] {
                ui.label(RichText::new(h).weak().size(11.0));
            }
            ui.end_row();
            let mut cursor = 0u32;
            for (n, f) in ev.fields.iter().enumerate() {
                if f.offset as u32 > cursor && ev.count > 0 {
                    let pad = f.offset as u32 - cursor;
                    field_line(
                        ui,
                        cursor,
                        format!("{pad} B"),
                        format!("[pad @{cursor}]"),
                        format_bytes(pad as u64 * ev.count),
                        format_share(pad as u64 * ev.count, ev.payload_bytes),
                    );
                }
                let size = if f.varlen {
                    if f.varlen_max > 0 {
                        format!("≤{}", f.varlen_max)
                    } else {
                        "var".to_string()
                    }
                } else {
                    format!("{} B", f.size)
                };
                let (bytes, share) = field_bytes(ev, f, n == ev.fields.len() - 1);
                field_line(ui, f.offset as u32, size, f.name.clone(), bytes, share);
                if !f.varlen {
                    cursor = cursor.max(f.offset as u32 + f.size as u32);
                }
            }
        });
}

fn field_bytes(ev: &EventStat, f: &EventFieldStat, is_last: bool) -> (String, String) {
    if ev.count == 0 {
        return ("—".to_string(), "—".to_string());
    }
    if f.varlen && is_last {
        let actual = ev.payload_bytes.saturating_sub(f.offset as u64 * ev.count);
        return (
            format!(
                "{} (avg {})",
                format_bytes(actual),
                format_len((actual / ev.count) as u32, true),
            ),
            format_share(actual, ev.payload_bytes),
        );
    }
    if f.varlen {
        return ("—".to_string(), "—".to_string());
    }
    let total = f.size as u64 * ev.count;
    (format_bytes(total), format_share(total, ev.payload_bytes))
}

fn field_line(
    ui: &mut egui::Ui,
    offset: u32,
    size: String,
    name: String,
    bytes: String,
    share: String,
) {
    ui.label(RichText::new(format!("{offset}")).size(11.0));
    ui.label(RichText::new(size).size(11.0));
    ui.label(RichText::new(name).size(11.0));
    ui.label(RichText::new(bytes).size(11.0));
    ui.label(RichText::new(share).size(11.0));
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::{display_name, provider_groups, EventStat, Provider};

    fn provider(id: u16, name: &str, events: Vec<EventStat>) -> Provider {
        let event_count = events.iter().map(|e| e.count).sum();
        let byte_size = events.iter().map(|e| e.bytes).sum();
        Provider {
            id,
            name: name.into(),
            event_count,
            first_ts: None,
            last_ts: None,
            session_count: 0,
            byte_size,
            schema_name: None,
            schema_event_count: 0,
            schema_divergent_events: Vec::new(),
            schema_new_fields: Vec::new(),
            events,
        }
    }

    fn stat(id: u32, name: &str, count: u64, bytes: u64) -> EventStat {
        EventStat {
            event_id: id,
            name: name.into(),
            count,
            bytes,
            payload_bytes: bytes.saturating_sub(count.saturating_mul(32)),
            min_len: if count == 0 { 0 } else { 32 },
            max_len: if count == 0 { 0 } else { 48 },
            first_ts: if count == 0 { None } else { Some(10) },
            last_ts: if count == 0 { None } else { Some(2_000_000) },
            level: Some(3),
            keywords: Vec::new(),
            fields: Vec::new(),
            format: String::new(),
            defined_only: count == 0,
        }
    }

    #[test]
    fn same_name_providers_merge_event_counts() {
        let groups = provider_groups(&[
            provider(1, "tcp", vec![stat(5, "RTT", 10, 400)]),
            provider(2, "tcp", vec![stat(5, "RTT", 4, 160), stat(6, "IN", 1, 32)]),
            provider(3, "hpts", vec![stat(1, "HPTS", 2, 80)]),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "tcp");
        assert_eq!(groups[0].ids, vec![1, 2]);
        assert_eq!(groups[0].event_count, 15);
        let rtt = groups[0].events.iter().find(|e| e.event_id == 5).unwrap();
        assert_eq!(rtt.count, 14);
        assert_eq!(rtt.bytes, 560);
        assert_eq!(rtt.min_len, 32);
        assert!(groups[0].events.iter().any(|e| e.event_id == 6));
        assert_eq!(groups[1].name, "hpts");
        assert_eq!(display_name(""), "(unnamed)");
    }
}
