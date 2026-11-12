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

//! ELLA System-tab panels and per-channel plot view.

#[allow(unused_imports)]
use super::*;
use crate::elog_parser::{ELLA_SIDE_DOWNSTREAM, ELLA_SIDE_UPSTREAM};
use std::collections::{HashMap, HashSet};

pub(crate) struct EllaFrameRatioView {
    pub(crate) side_label: Option<&'static str>,
    pub(crate) plot_base: &'static str,
    pub(crate) series: Vec<analysis::EllaNamedSeries>,
    pub(crate) is_fallback: bool,
}

pub struct EllaNodeViewState {
    pub(crate) provider_id: u16,
    pub(crate) label: String,
    pub(crate) color_off: usize,
    pub(crate) role: EllaNodeRole,
    pub(crate) relay: analysis::EllaRelayPlotData,
    pub(crate) stack: analysis::EllaSubStackData,
    pub(crate) rx_stack: analysis::EllaSubStackData,
    pub(crate) tx_stack: analysis::EllaSubStackData,
    pub(crate) traffic: Vec<analysis::EllaNamedSeries>,
    pub(crate) joins: Vec<analysis::EllaNamedSeries>,
    pub(crate) object_timing: Vec<analysis::EllaNamedSeries>,
    pub(crate) frame_rate: Vec<analysis::EllaNamedSeries>,
    pub(crate) frame_duration_views: Vec<EllaFrameRatioView>,
    pub(crate) frame_ratio_views: Vec<EllaFrameRatioView>,
    /// Short-name → color, shared by this node's per-title stacks.
    pub(crate) title_colors: HashMap<String, Color32>,
}

pub struct EllaSamplesViewState {
    pub(crate) nodes: Vec<EllaNodeViewState>,
    pub(crate) multi: bool,
    pub(crate) subs_view: EllaSubsView,
    pub(crate) dn_agg: EllaDownstreamAgg,
    pub(crate) settle_frames: u8,
    /// Widget slots whose default-hidden legend entries are applied.
    /// Per slot because split catalog rows draw on separate frames.
    pub(crate) legend_inited: HashSet<String>,
}

/// Relay catalog entry includes unlabeled nodes.
pub(crate) fn ella_catalog_role_matches(want: EllaNodeRole, got: EllaNodeRole) -> bool {
    match want {
        EllaNodeRole::Relay => matches!(got, EllaNodeRole::Relay | EllaNodeRole::Other),
        _ => want == got,
    }
}

/// System-tab Fanout plots, one of each per node.
pub(crate) const ELLA_NODE_PLOT_IDS: &[&str] = &[
    "ella_subs_plot",
    "ella_title_stack_plot",
    "ella_rx_stack_plot",
    "ella_tx_stack_plot",
    "ella_traffic_plot",
    "ella_joins_plot",
    "ella_object_timing_plot",
    "ella_frame_rate_plot",
    "ella_frame_duration_plot",
    "ella_frame_duration_internal_plot",
    "ella_frame_duration_external_plot",
    "ella_frame_ratio_plot",
    "ella_frame_ratio_internal_plot",
    "ella_frame_ratio_external_plot",
    "ella_up_chans_plot",
    "ella_dn_chans_plot",
    "ella_errors_plot",
];

/// Fanout Subscribers: sampled totals or per-title occupancy.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EllaSubsView {
    Aggregate,
    PerChannel,
}

/// How aggregated Downstream channels / pending counts combine instances.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EllaDownstreamAgg {
    Max,
    Sum,
}

/// Sortable Fanout channel-list columns (Session List `+` tab).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EllaSortColumn {
    Node,
    Ids,
    Channel,
    Name,
    Duration,
    Clients,
    Rate,
    Frame,
    Stretch,
    Instances,
    Errors,
    Events,
}

fn sort_ella_channel_list(
    rows: &mut [analysis::EllaChannelListRow],
    col: EllaSortColumn,
    ascending: bool,
) {
    rows.sort_by(|a, b| {
        let ord = match col {
            EllaSortColumn::Node => a.node.cmp(&b.node),
            EllaSortColumn::Ids => a.ids_label.cmp(&b.ids_label),
            EllaSortColumn::Channel => a.short_label.cmp(&b.short_label),
            EllaSortColumn::Name => a.channel_name.cmp(&b.channel_name),
            EllaSortColumn::Duration => a
                .duration_s
                .partial_cmp(&b.duration_s)
                .unwrap_or(std::cmp::Ordering::Equal),
            EllaSortColumn::Clients => a.peak_subscribers.cmp(&b.peak_subscribers),
            EllaSortColumn::Rate => a
                .rx_rate_mbps
                .unwrap_or(-1.0)
                .partial_cmp(&b.rx_rate_mbps.unwrap_or(-1.0))
                .unwrap_or(std::cmp::Ordering::Equal),
            EllaSortColumn::Frame => a
                .frame_rate_mbps
                .unwrap_or(-1.0)
                .partial_cmp(&b.frame_rate_mbps.unwrap_or(-1.0))
                .unwrap_or(std::cmp::Ordering::Equal),
            EllaSortColumn::Stretch => a
                .stretch_frac
                .unwrap_or(-1.0)
                .partial_cmp(&b.stretch_frac.unwrap_or(-1.0))
                .unwrap_or(std::cmp::Ordering::Equal),
            EllaSortColumn::Instances => a.instance_count.cmp(&b.instance_count),
            EllaSortColumn::Errors => a.error_count.cmp(&b.error_count),
            EllaSortColumn::Events => a.event_count.cmp(&b.event_count),
        };
        if ascending {
            ord
        } else {
            ord.reverse()
        }
    });
}

impl EllaSamplesViewState {
    pub(crate) fn new(prof: &EllaProfile, t0_us: u64) -> Self {
        let mut ids = prof.fanout_provider_ids();
        ids.sort_by_key(|pid| prof.node_role(*pid).rank());
        let multi = ids.len() > 1;
        let mut nodes = Vec::with_capacity(ids.len());
        for &pid in &ids {
            let (stack, rx_stack, tx_stack) =
                analysis::build_ella_node_title_stacks(prof, t0_us, pid);
            let frame_rate = analysis::build_ella_frame_rate_aggregate(prof, pid, t0_us);
            let frame_duration_views = if prof.node_role(pid) == EllaNodeRole::Server {
                vec![ella_frame_duration_view(
                    prof,
                    pid,
                    t0_us,
                    None,
                    None,
                    "ella_frame_duration_plot",
                )]
            } else {
                vec![
                    ella_frame_duration_view(
                        prof,
                        pid,
                        t0_us,
                        Some(ELLA_SIDE_UPSTREAM),
                        Some("Internal"),
                        "ella_frame_duration_internal_plot",
                    ),
                    ella_frame_duration_view(
                        prof,
                        pid,
                        t0_us,
                        Some(ELLA_SIDE_DOWNSTREAM),
                        Some("External"),
                        "ella_frame_duration_external_plot",
                    ),
                ]
            };
            let frame_ratio_views = if prof.node_role(pid) == EllaNodeRole::Server {
                vec![ella_frame_ratio_view(
                    prof,
                    pid,
                    t0_us,
                    None,
                    None,
                    "ella_frame_ratio_plot",
                )]
            } else {
                vec![
                    ella_frame_ratio_view(
                        prof,
                        pid,
                        t0_us,
                        Some(ELLA_SIDE_UPSTREAM),
                        Some("Internal"),
                        "ella_frame_ratio_internal_plot",
                    ),
                    ella_frame_ratio_view(
                        prof,
                        pid,
                        t0_us,
                        Some(ELLA_SIDE_DOWNSTREAM),
                        Some("External"),
                        "ella_frame_ratio_external_plot",
                    ),
                ]
            };
            let color_off = if multi {
                match prof.node_role(pid) {
                    EllaNodeRole::Server => 0,
                    EllaNodeRole::Relay => 4,
                    EllaNodeRole::Other => 8,
                }
            } else {
                0
            };
            let title_colors =
                ella_title_colors(prof, &[&stack, &rx_stack, &tx_stack], pid, color_off);
            let cdn = analysis::build_ella_cdn_metrics_plot_for(prof, pid, t0_us);
            let object_timing = analysis::build_ella_object_timing_plot(prof, pid, t0_us);
            nodes.push(EllaNodeViewState {
                provider_id: pid,
                label: prof.node_label(pid),
                color_off,
                role: prof.node_role(pid),
                relay: analysis::build_ella_relay_plot_for(prof, pid, t0_us),
                stack,
                rx_stack,
                tx_stack,
                traffic: cdn.traffic,
                joins: cdn.joins,
                object_timing,
                frame_rate,
                frame_duration_views,
                frame_ratio_views,
                title_colors,
            });
        }
        Self {
            nodes,
            multi,
            subs_view: EllaSubsView::PerChannel,
            dn_agg: EllaDownstreamAgg::Sum,
            settle_frames: SETTLE_FRAMES,
            legend_inited: HashSet::new(),
        }
    }
}

fn ella_frame_ratio_view(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
    instance_side: Option<u8>,
    side_label: Option<&'static str>,
    plot_base: &'static str,
) -> EllaFrameRatioView {
    let mut series = analysis::build_ella_frame_ratio_percentiles_for_side(
        prof,
        provider_id,
        t0_us,
        instance_side,
    );
    let is_fallback = series.is_empty();
    if is_fallback {
        series = analysis::build_ella_frame_sample_ratio_percentiles_for_side(
            prof,
            provider_id,
            t0_us,
            instance_side,
        );
    }
    EllaFrameRatioView {
        side_label,
        plot_base,
        series,
        is_fallback,
    }
}

fn ella_frame_duration_view(
    prof: &EllaProfile,
    provider_id: u16,
    t0_us: u64,
    instance_side: Option<u8>,
    side_label: Option<&'static str>,
    plot_base: &'static str,
) -> EllaFrameRatioView {
    EllaFrameRatioView {
        side_label,
        plot_base,
        series: analysis::build_ella_frame_duration_aggregate(
            prof,
            provider_id,
            t0_us,
            instance_side,
        ),
        is_fallback: false,
    }
}

pub struct EllaChannelViewState {
    pub(crate) provider_id: u16,
    pub(crate) session_id: u64,
    pub(crate) label: String,
    pub(crate) node: String,
    pub(crate) show_node: bool,
    pub(crate) full_name: String,
    pub(crate) version: u8,
    pub(crate) duration_s: f64,
    pub(crate) peak_subscribers: u32,
    pub(crate) state: &'static str,
    pub(crate) ids_label: String,
    pub(crate) caption: String,
    pub(crate) data: analysis::EllaChannelPlotData,
    pub(crate) time_axis: TimeAxisController,
    pub(crate) settle_frames: u8,
    pub(crate) t_min: f64,
    pub(crate) t_max: f64,
    pub(crate) plot_layout: PlotLayout,
    pub(crate) plot_draft: PlotLayout,
    pub(crate) needs_legend_init: bool,
    pub(crate) object_pct: analysis::EllaObjectPercentile,
    /// Background plot-data + caption compute; `Some` while it runs.
    pub(crate) pending: Option<mpsc::Receiver<(analysis::EllaChannelPlotData, String)>>,
}

/// Palette slots for per-title System stacks. Titles take colours in
/// peak-subscriber order (Subscribers, then leftover RX/TX names) so
/// the three stacks agree. `color_off` shifts the palette for a second node.
fn ella_title_colors(
    prof: &EllaProfile,
    stacks: &[&analysis::EllaSubStackData],
    provider_id: u16,
    color_off: usize,
) -> HashMap<String, Color32> {
    let mut names: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for stack in stacks {
        for ser in &stack.series {
            if ser.label != "Other" && seen.insert(ser.label.clone()) {
                names.push(ser.label.clone());
            }
        }
    }
    for c in prof.channels.values() {
        if c.provider_id != provider_id {
            continue;
        }
        let label = if c.channel_name.is_empty() {
            format!("#{}", c.session_id)
        } else {
            analysis::ella_channel_short_label(&c.channel_name)
        };
        if seen.insert(label.clone()) {
            names.push(label);
        }
    }
    names
        .into_iter()
        .enumerate()
        .map(|(i, n)| (n, series_color(i + color_off)))
        .collect()
}

fn title_hover_stats(
    rates: &HashMap<String, Option<f64>>,
    stack: &[analysis::EllaNamedSeries],
) -> HashMap<String, analysis::EllaStackLegendStats> {
    let mut out: HashMap<String, analysis::EllaStackLegendStats> = rates
        .iter()
        .map(|(k, rate)| {
            (
                k.clone(),
                analysis::EllaStackLegendStats {
                    rate_mbps: *rate,
                    ..Default::default()
                },
            )
        })
        .collect();
    for s in stack {
        if is_stack_other(&s.label) {
            continue;
        }
        let (peak, avg) = analysis::occupancy_peak_avg(&s.data);
        let e = out.entry(s.label.clone()).or_default();
        e.peak_external = Some(peak.round() as u32);
        e.avg_external = avg;
    }
    out
}

fn inst_phrase(n: usize) -> String {
    if n == 1 {
        "1 instance".into()
    } else {
        format!("{n} instances")
    }
}

fn ella_channel_caption(events: &analysis::EllaChannelEventList, t_max: f64) -> String {
    match (
        events.first_created_s,
        events.n_created,
        events.last_destroyed_s,
        events.n_destroyed,
    ) {
        (Some(c), n, None, _) => {
            format!(
                "Created at {c:.1}s ({}). Still live at {t_max:.1}s.",
                inst_phrase(n)
            )
        }
        (Some(c), n, Some(d), nd) => {
            format!(
                "Created at {c:.1}s ({}). Destroyed at {d:.1}s ({}).",
                inst_phrase(n),
                inst_phrase(nd)
            )
        }
        (None, _, Some(d), nd) => {
            format!("Destroyed at {d:.1}s ({}).", inst_phrase(nd))
        }
        _ => String::new(),
    }
}

fn named_lines_off<'a>(series: &'a [analysis::EllaNamedSeries], off: usize) -> Vec<PlotLine<'a>> {
    series
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.data.is_empty())
        .map(|(i, s)| PlotLine {
            name: s.label.clone(),
            color: series_color(i + off),
            data: &s.data,
        })
        .collect()
}

fn hide_plot_legend_defaults(
    ctx: &egui::Context,
    plot_id: &str,
    labels: &[String],
    hide: impl Fn(&str) -> bool,
) {
    let id = egui::Id::new(plot_id);
    let Some(mut mem) = PlotMemory::load(ctx, id) else {
        return;
    };
    let mut changed = false;
    for label in labels {
        if hide(label) && mem.hidden_items.insert(label.clone()) {
            changed = true;
        }
    }
    if changed {
        mem.store(ctx, id);
    }
}

fn hide_ella_frame_legend(ctx: &egui::Context, plot_id: &str, labels: &[String]) {
    hide_plot_legend_defaults(
        ctx,
        plot_id,
        labels,
        analysis::ella_frame_legend_hidden_by_default,
    );
}

fn hide_ella_object_legend(ctx: &egui::Context, plot_id: &str, labels: &[String]) {
    hide_plot_legend_defaults(
        ctx,
        plot_id,
        labels,
        analysis::ella_object_legend_hidden_by_default,
    );
}

fn hide_ella_ratio_legend(ctx: &egui::Context, plot_id: &str, labels: &[String]) {
    hide_plot_legend_defaults(
        ctx,
        plot_id,
        labels,
        analysis::ella_ratio_legend_hidden_by_default,
    );
}

pub(crate) fn ella_frame_label_is_relay(label: &str) -> bool {
    label.contains("Relay")
}

fn ella_frame_rate_subset(
    series: &[analysis::EllaChannelSubSeries],
    relay: bool,
) -> Vec<analysis::EllaChannelSubSeries> {
    series
        .iter()
        .filter(|s| ella_frame_label_is_relay(&s.label) == relay)
        .cloned()
        .collect()
}

fn ella_plot_id(multi: bool, pid: u16, base: &str) -> String {
    if multi {
        format!("{base}_{pid}")
    } else {
        base.to_string()
    }
}

fn bin_channel_plot_lines(
    lines: &[PlotLine<'_>],
    seed: (f64, f64, bool),
    nbins: usize,
) -> Vec<(String, Color32, Vec<[f64; 2]>)> {
    let mut data_lo = f64::INFINITY;
    let mut data_hi = f64::NEG_INFINITY;
    for line in lines {
        if let (Some(first), Some(last)) = (line.data.first(), line.data.last()) {
            data_lo = data_lo.min(first[0]);
            data_hi = data_hi.max(last[0]);
        }
    }
    if !data_lo.is_finite() {
        data_lo = 0.0;
    }
    if !data_hi.is_finite() || data_hi <= data_lo {
        data_hi = data_lo + 1.0;
    }
    let view = seed_view_x(seed.0, seed.1, data_lo, data_hi);
    lines
        .iter()
        .filter(|line| !line.data.is_empty())
        .map(|line| {
            (
                line.name.clone(),
                line.color,
                analysis::step_series_bins(line.data, view[0], view[1], nbins),
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn draw_channel_binned_lines(
    ui: &mut egui::Ui,
    cursor: &mut SysCursor,
    seed: (f64, f64, bool),
    lines: Vec<PlotLine<'_>>,
    y_label: &str,
    plot_id: &str,
    pin_y0: bool,
    hold_last: bool,
) {
    let binned = bin_channel_plot_lines(&lines, seed, cursor.plot_bins);
    let drawn: Vec<PlotLine<'_>> = binned
        .iter()
        .map(|(name, color, data)| PlotLine {
            name: name.clone(),
            color: *color,
            data,
        })
        .collect();
    draw_series_plot_marked(
        ui,
        plot_id,
        y_label,
        cursor.panel_h,
        &drawn,
        &[],
        cursor,
        seed,
        hold_last,
        pin_y0,
        &[],
    );
}

#[allow(clippy::too_many_arguments)]
/// One channel-tab plot holding every side's lines, RX ahead of TX in
/// the color key.
fn draw_channel_series_block(
    ui: &mut egui::Ui,
    cursor: &mut SysCursor,
    seed: (f64, f64, bool),
    series: &[analysis::EllaChannelSubSeries],
    heading: Option<&str>,
    y_label: &str,
    plot_id: &str,
    pin_y0: bool,
    keep_order: bool,
) {
    if series.iter().all(|s| s.data.is_empty()) {
        return;
    }
    if let Some(heading) = heading {
        ui.add_space(6.0);
        let resp = ui.strong(RichText::new(heading).size(14.0));
        if heading == "Object Rate" {
            resp.on_hover_text(
                "Payload Mbps. OBJECT_STATE bytes over STARTED→COMPLETED for the Timeline's selected worker, or CHANNEL_SAMPLE object_bytes per stats tick when those events are absent.",
            );
        } else if heading == "Object Timing" {
            resp.on_hover_text(
                "OBJECT_TIMING medians in 1 s bins: start lateness and encode/enqueue durations (ms).",
            );
        } else if heading == "Frame Rate" || heading.ends_with("Frame Rate") {
            resp.on_hover_text(
                "Byte-weighted FDACE rate from FRAME_SENT / FRAME_RELAY, or FRAME_SAMPLE. Actual / Wire start on; Planned / Recv / Fwd start hidden.",
            );
        } else if heading == "Frame Duration" || heading.ends_with("Frame Duration") {
            resp.on_hover_text(
                "Mean per-frame duration from FRAME_SENT / FRAME_RELAY, or FRAME_SAMPLE. Intercept is Recv/Fwd holding above a shorter Wire.",
            );
        } else if heading == "Object Timeline" {
            resp.on_hover_text(
                "Lanes are Server, Relay Internal, Relay External. Color flips each GOP. Red means the previous object id was skipped. Hover for the FRAME_SENT id.",
            );
        } else if heading == "Object Duration" {
            resp.on_hover_text(
                "STARTED until the next object's first packet. Server and Internal are that face's copies. External is Median / P75 / P95 / Max of workers (P75+ start hidden).",
            );
        } else if heading == "Object Size" {
            resp.on_hover_text(
                "Payload of that GOP in MB. Server and Internal are that face's copies. External is Median / P75 / P95 / Max of workers (P75+ start hidden).",
            );
        }
        if heading == "Relay Frame Rate" {
            ui.label(
                RichText::new(ella_frame_plot_caption(true))
                    .size(11.0)
                    .color(Color32::from_gray(160)),
            );
        } else if heading == "Server Frame Rate" || heading == "Frame Rate" {
            ui.label(
                RichText::new(ella_frame_plot_caption(false))
                    .size(11.0)
                    .color(Color32::from_gray(160)),
            );
        } else if heading == "Relay Frame Duration" {
            ui.label(
                RichText::new(ella_frame_duration_caption(true))
                    .size(11.0)
                    .color(Color32::from_gray(160)),
            );
        } else if heading == "Server Frame Duration" || heading == "Frame Duration" {
            ui.label(
                RichText::new(ella_frame_duration_caption(false))
                    .size(11.0)
                    .color(Color32::from_gray(160)),
            );
        } else if heading == "Object Duration" {
            ui.label(
                RichText::new(ella_object_duration_caption())
                    .size(11.0)
                    .color(Color32::from_gray(160)),
            );
        }
    }
    let lines = if keep_order {
        series
            .iter()
            .filter(|s| !s.data.is_empty())
            .enumerate()
            .map(|(i, s)| PlotLine {
                name: s.label.clone(),
                color: series_color(i),
                data: &s.data,
            })
            .collect()
    } else {
        let (internal, external) = channel_occupancy_lines(series);
        let mut lines = internal;
        lines.extend(external);
        lines
    };
    draw_channel_binned_lines(ui, cursor, seed, lines, y_label, plot_id, pin_y0, false);
}

/// Channel-tab duration-ratio plot: Median/P75/P95/Max lines, y scaled
/// to the visible series both ways (ratios hover near 1.0, so pinning
/// 0 would flatten them).
fn draw_channel_ratio_block(
    ui: &mut egui::Ui,
    cursor: &mut SysCursor,
    seed: (f64, f64, bool),
    series: &[analysis::EllaChannelSubSeries],
    heading: &str,
    fallback: bool,
    plot_id: &str,
) {
    let (internal, external) = channel_occupancy_lines(series);
    if internal.is_empty() && external.is_empty() {
        return;
    }
    ui.add_space(6.0);
    let heading_s = if fallback {
        format!("{heading} — FRAME_SAMPLE Fallback")
    } else {
        heading.to_string()
    };
    ui.strong(RichText::new(heading_s).size(14.0))
        .on_hover_text(if fallback {
            "Fallback from FRAME_SAMPLE: each value first sums one instance's timing totals over 2 s, then percentiles are calculated across those aggregates. It is not a per-frame distribution."
        } else {
            "Per-frame duration-ratio percentiles, equal-time buckets over the visible window: server FRAME_SENT actual/planned send, relay FRAME_RELAY receive/wire. Max starts hidden."
        });
    ui.label(
        RichText::new(if fallback {
            "FRAME_SAMPLE fallback — 2 s per-instance aggregate ratios, not individual frames."
        } else {
            "Detailed per-frame ratios — Median, P75, P95, and Max in equal-time buckets over the visible window."
        })
        .size(11.0)
        .color(Color32::from_gray(160)),
    );
    let mut lines = internal;
    lines.extend(external);
    draw_channel_binned_lines(ui, cursor, seed, lines, "Ratio", plot_id, false, false);
}

fn is_internal_occupancy(label: &str) -> bool {
    // A merged tab prefixes the side with `<node> - `; the side is the
    // part after it.
    let side = label.rsplit(" - ").next().unwrap_or(label);
    side == "Internal" || side == "RX" || side.starts_with("Internal ") || side.starts_with("RX ")
}

/// Split a channel tab's series into the internal (RX) and external
/// (TX) plots. Colours run across both so a merged tab's Server and
/// Relay lines never share one.
fn channel_occupancy_lines(
    series: &[analysis::EllaChannelSubSeries],
) -> (Vec<PlotLine<'_>>, Vec<PlotLine<'_>>) {
    let mut internal = Vec::new();
    let mut external = Vec::new();
    for (i, s) in series.iter().filter(|s| !s.data.is_empty()).enumerate() {
        let line = PlotLine {
            name: s.label.clone(),
            color: series_color(i),
            data: &s.data,
        };
        if is_internal_occupancy(&s.label) {
            internal.push(line);
        } else {
            external.push(line);
        }
    }
    (internal, external)
}

fn ella_frame_plot_caption(relay: bool) -> &'static str {
    if relay {
        "Wire = origin send duration (header timestamps). Recv = first to last packet arrival at this instance. Fwd = first to last forward to clients or workers. Mbps is frame bytes / that duration, so stretch reads as a drop."
    } else {
        "Planned is FDACE's scheduled send duration; Actual is how long the pacer took. Mbps is frame bytes / that duration, so stretch reads as Actual falling below Planned."
    }
}

fn ella_object_timeline_caption() -> &'static str {
    "Lanes are Server, Relay Internal, and Relay External. Each bar is STARTED until the next object's first packet. External Median/P75/P95/Max picks one worker; Internal is the ingest instance."
}

fn ella_object_duration_caption() -> &'static str {
    "STARTED until the next object's first packet (the GOP clock). Internal is ingest; External percentiles are the workers. A skipped object reads as ~4s or ~6s."
}

fn ella_frame_timeline_caption() -> &'static str {
    "Same GOP colors as Object Timeline. Server Actual is [event − actual_send_us, event] when FRAME_SENT is per-frame; older GOP-end bursts still start at object STARTED. Relay Recv is first-to-last packet at that instance. Gaps are often dropped events."
}

fn ella_frame_duration_caption(relay: bool) -> &'static str {
    if relay {
        "Mean per-frame milliseconds. Wire is origin send span in the headers; Recv is first-to-last arrival; Fwd is first-to-last forward. Intercept is Recv/Fwd holding a few milliseconds above Wire."
    } else {
        "Mean per-frame milliseconds. Planned is the FDACE send window; Actual is how long the pacer took."
    }
}

fn collect_timeline_lanes(
    prof: &EllaProfile,
    ids: impl IntoIterator<Item = (u16, u32)>,
) -> (Vec<(u16, u8)>, Vec<String>) {
    let mut lanes: Vec<(u16, u8)> = Vec::new();
    for (pid, iid) in ids {
        let k = analysis::object_timeline_lane(prof, pid, iid);
        if !lanes.contains(&k) {
            lanes.push(k);
        }
    }
    lanes.sort_by_key(|&(pid, side)| analysis::object_timeline_lane_sort_key(pid, side));
    let names = lanes
        .iter()
        .map(|&(pid, side)| analysis::object_timeline_lane_label(prof, pid, side))
        .collect();
    (lanes, names)
}

fn draw_object_id_strip(
    ui: &mut egui::Ui,
    cursor: &mut SysCursor,
    seed: (f64, f64, bool),
    spans: &[analysis::EllaObjectSpan],
    prof: &EllaProfile,
    object_pct: Option<&mut analysis::EllaObjectPercentile>,
    plot_id: &str,
) {
    if spans.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.strong(RichText::new("Object Timeline").size(14.0))
            .on_hover_text(
                "Lanes are Server, Relay Internal, Relay External. Color flips each GOP. External Median / P75 / P95 / Max picks one worker. Red means the previous object id was skipped. Hover for the FRAME_SENT id.",
            );
        if let Some(pct) = object_pct {
            ui.add_space(12.0);
            ui.label(RichText::new("External:").size(13.0).color(Color32::from_gray(180)));
            for kind in analysis::EllaObjectPercentile::ALL {
                ui.selectable_value(pct, kind, RichText::new(kind.label()).size(13.0));
            }
        }
    });
    ui.label(
        RichText::new(ella_object_timeline_caption())
            .size(11.0)
            .color(Color32::from_gray(160)),
    );
    let (lanes, names) =
        collect_timeline_lanes(prof, spans.iter().map(|s| (s.provider_id, s.instance_id)));
    let n_lanes = lanes.len().max(1) as f64;
    let n_lane_ticks = names.len();
    let height = if cursor.fill_remaining {
        ui.available_height().max(PLOT_H_MIN)
    } else {
        cursor.panel_h
    };
    let (x_lo, x_hi, _) = seed;
    let names_fmt = names.clone();
    let y_axis = AxisHints::new_y()
        .label("")
        .max_digits(8)
        .formatter(move |mark, _, _| {
            let i = mark.value.floor() as usize;
            names_fmt.get(i).cloned().unwrap_or_default()
        });
    let mut plot = Plot::new(plot_id)
        .id(egui::Id::new(plot_id))
        .height(height)
        .include_x(x_lo)
        .include_x(x_hi)
        .include_y(0.0)
        .include_y(n_lanes)
        .auto_bounds(egui::Vec2b::new(false, false))
        .set_margin_fraction(egui::vec2(0.0, 0.0))
        .y_axis_width(12)
        .custom_y_axes(vec![y_axis])
        .custom_x_axes(vec![agg_x_time_axis(cursor.utc_origin)])
        .y_grid_spacer(move |_| {
            (0..n_lane_ticks)
                .map(|i| GridMark {
                    value: i as f64 + 0.5,
                    step_size: 1.0,
                })
                .collect()
        })
        .show_x(false)
        .show_y(false)
        .allow_scroll(false)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false);
    plot = maybe_legend(plot, false);
    if cursor.utc_origin.is_some() {
        plot = plot.x_grid_spacer(utc_x_grid_spacer);
    }
    let color_a = COLOR_OBJ_TIMELINE_A;
    let color_b = COLOR_OBJ_TIMELINE_B;
    let color_skip = COLOR_OBJ_TIMELINE_SKIP;
    let skip_at = analysis::object_span_id_skipped(spans);
    let stripe = analysis::object_span_stripe(spans);
    let sub = analysis::object_timeline_sublanes(prof, spans);
    let mut depth = vec![1u32; lanes.len()];
    for (si, s) in spans.iter().enumerate() {
        if let Some(lane) = lanes
            .iter()
            .position(|&k| k == analysis::object_timeline_lane(prof, s.provider_id, s.instance_id))
        {
            depth[lane] = depth[lane].max(sub.get(si).copied().unwrap_or(0) + 1);
        }
    }
    let resp = plot.show(ui, |plot_ui| {
        plot_ui.set_plot_bounds(PlotBounds::from_min_max([x_lo, 0.0], [x_hi, n_lanes]));
        plot_ui.set_auto_bounds(egui::Vec2b::new(false, false));
        for (si, s) in spans.iter().enumerate() {
            let Some(lane) = lanes.iter().position(|&k| {
                k == analysis::object_timeline_lane(prof, s.provider_id, s.instance_id)
            }) else {
                continue;
            };
            let skipped = skip_at.get(si).copied().unwrap_or(false);
            let fill = if skipped {
                color_skip
            } else if stripe.get(si).copied().unwrap_or(0) % 2 == 0 {
                color_a
            } else {
                color_b
            };
            let (y0, y1) = object_bar_y(lane, sub.get(si).copied().unwrap_or(0), depth[lane]);
            let x0 = s.t0.max(x_lo);
            let x1 = s.t1.min(x_hi);
            if x1 <= x0 {
                continue;
            }
            let pts = PlotPoints::new(vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]);
            plot_ui.polygon(Polygon::new(pts).fill_color(fill).stroke(Stroke::NONE));
        }
    });
    if let Some(data_x) = cursor.panel(resp.response.rect, &resp.transform, true) {
        let data_y = resp
            .response
            .hover_pos()
            .map(|p| resp.transform.value_from_position(p).y);
        let mut best: Vec<Option<usize>> = vec![None; lanes.len()];
        let mut fallback: Vec<Option<usize>> = vec![None; lanes.len()];
        for (si, s) in spans.iter().enumerate() {
            if data_x < s.t0 || data_x > s.t1 {
                continue;
            }
            let Some(lane) = lanes.iter().position(|&k| {
                k == analysis::object_timeline_lane(prof, s.provider_id, s.instance_id)
            }) else {
                continue;
            };
            let sub_i = sub.get(si).copied().unwrap_or(0);
            let (y0, y1) = object_bar_y(lane, sub_i, depth[lane]);
            match fallback[lane] {
                Some(j) if spans[j].t0 >= s.t0 => {}
                _ => fallback[lane] = Some(si),
            }
            if let Some(y) = data_y {
                if y >= y0 && y <= y1 {
                    best[lane] = Some(si);
                }
            }
        }
        for lane in 0..lanes.len() {
            if best[lane].is_none() {
                best[lane] = fallback[lane];
            }
        }
        for (lane, si) in best.into_iter().enumerate().rev() {
            let Some(si) = si else {
                continue;
            };
            let s = &spans[si];
            let name = names.get(lane).map(|n| n.as_str()).unwrap_or("node");
            let skipped = skip_at.get(si).copied().unwrap_or(false);
            let id = s
                .object_id
                .map(|id| format!("{id}"))
                .unwrap_or_else(|| format!("pn {}", s.first_pn));
            let size = elog_parser::fmt_bytes(s.bytes as u64);
            let dur = format_time_span(s.t1 - s.t0);
            let (y0, y1) = object_bar_y(lane, sub.get(si).copied().unwrap_or(0), depth[lane]);
            let text = if skipped {
                format!(
                    "{name} i{}: object {id} · {size} · {dur} (id skipped)",
                    s.instance_id
                )
            } else {
                format!("{name} i{}: object {id} · {size} · {dur}", s.instance_id)
            };
            cursor.add(
                &resp.transform,
                data_x,
                (y0 + y1) * 0.5,
                text,
                if skipped {
                    color_skip
                } else if stripe.get(si).copied().unwrap_or(0) % 2 == 0 {
                    color_a
                } else {
                    color_b
                },
            );
        }
    }
    cursor.last_plot_rect = Some(resp.response.rect);
}

fn object_bar_y(lane: usize, sub: u32, depth: u32) -> (f64, f64) {
    let top = lane as f64 + 0.08;
    let bot = lane as f64 + 0.92;
    let d = depth.max(1);
    if d == 1 {
        return (top, bot);
    }
    let h = (bot - top) / d as f64;
    let pad = 0.02;
    let y0 = top + sub as f64 * h + pad;
    let y1 = top + (sub as f64 + 1.0) * h - pad;
    (y0, y1.max(y0 + 0.02))
}

fn draw_frame_id_strip(
    ui: &mut egui::Ui,
    cursor: &mut SysCursor,
    seed: (f64, f64, bool),
    spans: &[analysis::EllaFrameSpan],
    prof: &EllaProfile,
    object_pct: Option<&mut analysis::EllaObjectPercentile>,
    plot_id: &str,
) {
    if spans.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.strong(RichText::new("Frame Timeline").size(14.0))
            .on_hover_text(
                "Same lanes as Object Timeline. Each bar is one FDACE frame. Hover for object id and frame index.",
            );
        if let Some(pct) = object_pct {
            ui.add_space(12.0);
            ui.label(
                RichText::new("External:")
                    .size(13.0)
                    .color(Color32::from_gray(180)),
            );
            for kind in analysis::EllaObjectPercentile::ALL {
                ui.selectable_value(pct, kind, RichText::new(kind.label()).size(13.0));
            }
        }
    });
    ui.label(
        RichText::new(ella_frame_timeline_caption())
            .size(11.0)
            .color(Color32::from_gray(160)),
    );
    let (lanes, names) =
        collect_timeline_lanes(prof, spans.iter().map(|s| (s.provider_id, s.instance_id)));
    let n_lanes = lanes.len().max(1) as f64;
    let n_lane_ticks = names.len();
    let height = if cursor.fill_remaining {
        ui.available_height().max(PLOT_H_MIN)
    } else {
        cursor.panel_h
    };
    let (x_lo, x_hi, _) = seed;
    let names_fmt = names.clone();
    let y_axis = AxisHints::new_y()
        .label("")
        .max_digits(8)
        .formatter(move |mark, _, _| {
            let i = mark.value.floor() as usize;
            names_fmt.get(i).cloned().unwrap_or_default()
        });
    let mut plot = Plot::new(plot_id)
        .id(egui::Id::new(plot_id))
        .height(height)
        .include_x(x_lo)
        .include_x(x_hi)
        .include_y(0.0)
        .include_y(n_lanes)
        .auto_bounds(egui::Vec2b::new(false, false))
        .set_margin_fraction(egui::vec2(0.0, 0.0))
        .y_axis_width(12)
        .custom_y_axes(vec![y_axis])
        .custom_x_axes(vec![agg_x_time_axis(cursor.utc_origin)])
        .y_grid_spacer(move |_| {
            (0..n_lane_ticks)
                .map(|i| GridMark {
                    value: i as f64 + 0.5,
                    step_size: 1.0,
                })
                .collect()
        })
        .show_x(false)
        .show_y(false)
        .allow_scroll(false)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false);
    plot = maybe_legend(plot, false);
    if cursor.utc_origin.is_some() {
        plot = plot.x_grid_spacer(utc_x_grid_spacer);
    }
    let sub = analysis::frame_span_sublanes(prof, spans);
    let stripe = analysis::frame_span_stripe(spans);
    let shade = analysis::frame_span_shade(spans);
    let mut depth = vec![1u32; lanes.len()];
    for (si, s) in spans.iter().enumerate() {
        if let Some(lane) = lanes
            .iter()
            .position(|&k| k == analysis::object_timeline_lane(prof, s.provider_id, s.instance_id))
        {
            depth[lane] = depth[lane].max(sub.get(si).copied().unwrap_or(0) + 1);
        }
    }
    let resp = plot.show(ui, |plot_ui| {
        plot_ui.set_plot_bounds(PlotBounds::from_min_max([x_lo, 0.0], [x_hi, n_lanes]));
        plot_ui.set_auto_bounds(egui::Vec2b::new(false, false));
        for (si, s) in spans.iter().enumerate() {
            let Some(lane) = lanes.iter().position(|&k| {
                k == analysis::object_timeline_lane(prof, s.provider_id, s.instance_id)
            }) else {
                continue;
            };
            let fill = frame_timeline_fill(
                stripe.get(si).copied().unwrap_or(0) % 2 == 1,
                shade.get(si).copied().unwrap_or(false),
            );
            let (y0, y1) = object_bar_y(lane, sub.get(si).copied().unwrap_or(0), depth[lane]);
            let x0 = s.t0.max(x_lo);
            let x1 = s.t1.min(x_hi);
            if x1 <= x0 {
                continue;
            }
            let pts = PlotPoints::new(vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]);
            plot_ui.polygon(Polygon::new(pts).fill_color(fill).stroke(Stroke::NONE));
        }
    });
    if let Some(data_x) = cursor.panel(resp.response.rect, &resp.transform, true) {
        let data_y = resp
            .response
            .hover_pos()
            .map(|p| resp.transform.value_from_position(p).y);
        let mut best: Vec<Option<usize>> = vec![None; lanes.len()];
        let mut fallback: Vec<Option<usize>> = vec![None; lanes.len()];
        for (si, s) in spans.iter().enumerate() {
            if data_x < s.t0 || data_x > s.t1 {
                continue;
            }
            let Some(lane) = lanes.iter().position(|&k| {
                k == analysis::object_timeline_lane(prof, s.provider_id, s.instance_id)
            }) else {
                continue;
            };
            let sub_i = sub.get(si).copied().unwrap_or(0);
            let (y0, y1) = object_bar_y(lane, sub_i, depth[lane]);
            match fallback[lane] {
                Some(j) if spans[j].t0 >= s.t0 => {}
                _ => fallback[lane] = Some(si),
            }
            if let Some(y) = data_y {
                if y >= y0 && y <= y1 {
                    best[lane] = Some(si);
                }
            }
        }
        for lane in 0..lanes.len() {
            if best[lane].is_none() {
                best[lane] = fallback[lane];
            }
        }
        for (lane, si) in best.into_iter().enumerate().rev() {
            let Some(si) = si else {
                continue;
            };
            let s = &spans[si];
            let name = names.get(lane).map(|n| n.as_str()).unwrap_or("node");
            let (y0, y1) = object_bar_y(lane, sub.get(si).copied().unwrap_or(0), depth[lane]);
            let dt_ms = (s.t1 - s.t0) * 1000.0;
            let size = elog_parser::fmt_bytes(s.bytes as u64);
            let fill = frame_timeline_fill(
                stripe.get(si).copied().unwrap_or(0) % 2 == 1,
                shade.get(si).copied().unwrap_or(false),
            );
            cursor.add(
                &resp.transform,
                data_x,
                (y0 + y1) * 0.5,
                format!(
                    "{name} i{}: object {} frame {} · {size} · {dt_ms:.1} ms",
                    s.instance_id, s.object_id, s.frame_index
                ),
                fill,
            );
        }
    }
    cursor.last_plot_rect = Some(resp.response.rect);
}

fn append_line<'a>(lines: &mut Vec<PlotLine<'a>>, name: &str, data: &'a [[f64; 2]]) {
    if data.is_empty() {
        return;
    }
    let i = lines.len();
    lines.push(PlotLine {
        name: name.to_string(),
        color: series_color(i),
        data,
    });
}

fn named_data<'a>(series: &'a [analysis::EllaNamedSeries], label: &str) -> &'a [[f64; 2]] {
    series
        .iter()
        .find(|s| s.label == label)
        .map(|s| s.data.as_slice())
        .unwrap_or(&[])
}

fn side_prefix(upstream: bool) -> &'static str {
    if upstream {
        "upstream "
    } else {
        "downstream "
    }
}

fn per_instance_side_lines<'a>(
    relay: &'a analysis::EllaRelayPlotData,
    upstream: bool,
) -> Vec<PlotLine<'a>> {
    let prefix = side_prefix(upstream);
    let mut lines = Vec::new();
    for s in relay.channels.iter().chain(relay.pending.iter()) {
        if !s.label.starts_with(prefix) || s.data.is_empty() {
            continue;
        }
        let name = s.label.strip_prefix(prefix).unwrap_or(&s.label);
        append_line(&mut lines, name, &s.data);
    }
    lines
}

fn upstream_channel_lines(relay: &analysis::EllaRelayPlotData) -> Vec<PlotLine<'_>> {
    if !relay.aggregated {
        return per_instance_side_lines(relay, true);
    }
    let mut lines = Vec::new();
    append_line(
        &mut lines,
        "Channels",
        named_data(&relay.channels, "Upstream"),
    );
    append_line(
        &mut lines,
        "Leave",
        named_data(&relay.pending, "Upstream Leave"),
    );
    append_line(
        &mut lines,
        "Retire",
        named_data(&relay.pending, "Upstream Retire"),
    );
    lines
}

fn downstream_channel_lines<'a>(
    relay: &'a analysis::EllaRelayPlotData,
    dn_agg: EllaDownstreamAgg,
) -> Vec<PlotLine<'a>> {
    if !relay.aggregated {
        return per_instance_side_lines(relay, false);
    }
    let (ch, leave, retire) = match dn_agg {
        EllaDownstreamAgg::Max => (
            relay.dn_channels_max.as_slice(),
            relay.dn_leave_max.as_slice(),
            relay.dn_retire_max.as_slice(),
        ),
        EllaDownstreamAgg::Sum => (
            relay.dn_channels_sum.as_slice(),
            relay.dn_leave_sum.as_slice(),
            relay.dn_retire_sum.as_slice(),
        ),
    };
    let mut lines = Vec::new();
    append_line(&mut lines, "Channels", ch);
    append_line(&mut lines, "Leave", leave);
    append_line(&mut lines, "Retire", retire);
    lines
}

fn last_occupancy_s(relay: &analysis::EllaRelayPlotData) -> Option<f64> {
    let mut last = f64::NEG_INFINITY;
    let mut consider = |pts: &[[f64; 2]]| {
        if let Some(p) = pts.last() {
            last = last.max(p[0]);
        }
    };
    for s in relay
        .subscribers
        .iter()
        .chain(&relay.channels)
        .chain(&relay.pending)
    {
        consider(&s.data);
    }
    for pts in [
        relay.dn_channels_max.as_slice(),
        relay.dn_channels_sum.as_slice(),
        relay.dn_leave_max.as_slice(),
        relay.dn_leave_sum.as_slice(),
        relay.dn_retire_max.as_slice(),
        relay.dn_retire_sum.as_slice(),
    ] {
        consider(pts);
    }
    last.is_finite().then_some(last)
}

impl FanoutPlot {
    /// Widget slot bases this plot kind draws (suffixed per node when
    /// the capture has more than one `fanout_node` provider).
    fn slot_bases(self) -> &'static [&'static str] {
        match self {
            Self::Subscribers => &["ella_subs_plot", "ella_title_stack_plot"],
            Self::Rx => &["ella_rx_stack_plot"],
            Self::Tx => &["ella_tx_stack_plot"],
            Self::Traffic => &["ella_traffic_plot"],
            Self::Joins => &["ella_joins_plot"],
            Self::ObjectTiming => &["ella_object_timing_plot"],
            Self::FrameRate => &["ella_frame_rate_plot"],
            Self::FrameDuration => &[
                "ella_frame_duration_plot",
                "ella_frame_duration_internal_plot",
                "ella_frame_duration_external_plot",
            ],
            Self::FrameRatio => &[
                "ella_frame_ratio_plot",
                "ella_frame_ratio_internal_plot",
                "ella_frame_ratio_external_plot",
            ],
            Self::Channels => &["ella_up_chans_plot", "ella_dn_chans_plot"],
            Self::Errors => &["ella_errors_plot"],
        }
    }

    /// Whether this node has any series for the plot kind.
    pub(crate) fn node_has_data(self, n: &EllaNodeViewState) -> bool {
        match self {
            Self::Subscribers => {
                n.relay.subscribers.iter().any(|s| !s.data.is_empty())
                    || n.stack.series.iter().any(|s| !s.data.is_empty())
            }
            Self::Rx => n.rx_stack.series.iter().any(|s| !s.data.is_empty()),
            Self::Tx => n.tx_stack.series.iter().any(|s| !s.data.is_empty()),
            Self::Traffic => n.traffic.iter().any(|s| !s.data.is_empty()),
            Self::Joins => n.joins.iter().any(|s| !s.data.is_empty()),
            Self::ObjectTiming => n.object_timing.iter().any(|s| !s.data.is_empty()),
            Self::FrameRate => n.frame_rate.iter().any(|s| !s.data.is_empty()),
            Self::FrameDuration => n
                .frame_duration_views
                .iter()
                .any(|v| v.series.iter().any(|s| !s.data.is_empty())),
            Self::FrameRatio => n
                .frame_ratio_views
                .iter()
                .any(|v| v.series.iter().any(|s| !s.data.is_empty())),
            Self::Channels => {
                !upstream_channel_lines(&n.relay).is_empty()
                    || !downstream_channel_lines(&n.relay, EllaDownstreamAgg::Sum).is_empty()
                    || !downstream_channel_lines(&n.relay, EllaDownstreamAgg::Max).is_empty()
            }
            Self::Errors => n.relay.errors.iter().any(|s| !s.data.is_empty()),
        }
    }
}

/// Caption noting when occupancy samples end before the capture does.
fn note_stale_occupancy(ui: &mut egui::Ui, relay: &analysis::EllaRelayPlotData, x_max: f64) {
    let has_occ = !relay.subscribers.is_empty()
        || !relay.channels.is_empty()
        || !relay.dn_channels_sum.is_empty();
    if !has_occ {
        return;
    }
    if let Some(last) = last_occupancy_s(relay) {
        if last + SERIES_STALE_AFTER_S < x_max {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("Last occupancy sample at {last:.1}s."))
                    .size(11.0)
                    .color(Color32::from_gray(160)),
            );
        }
    }
}

impl EllaChannelViewState {
    pub(crate) fn new(
        prof: &Arc<EllaProfile>,
        provider_id: u16,
        session_id: u64,
        t0_us: u64,
        capture_t_max: f64,
    ) -> Self {
        let info = prof.channels.get(&(provider_id, session_id));
        let label = info
            .map(|c| {
                if c.channel_name.is_empty() {
                    format!("ch#{session_id}")
                } else {
                    analysis::ella_channel_short_label(&c.channel_name)
                }
            })
            .unwrap_or_else(|| format!("ch#{session_id}"));
        let full_name = info.map(|c| c.channel_name.clone()).unwrap_or_default();
        let full_name = if full_name.is_empty() || full_name == label {
            String::new()
        } else {
            full_name
        };
        // Every reincarnation of the title, on the capture-wide axis.
        let members = analysis::ella_channel_tab_members(prof, provider_id, session_id);
        let infos: Vec<_> = members
            .iter()
            .filter_map(|k| prof.channels.get(k))
            .collect();
        let version = info.map(|c| c.version).unwrap_or(0);
        let state = info.map(|c| c.end_state_name()).unwrap_or("-");
        let peak_subscribers = infos.iter().map(|c| c.peak_subscribers).max().unwrap_or(0);
        let first_ts = infos.iter().map(|c| c.first_ts).min().unwrap_or(0);
        let last_ts = infos.iter().map(|c| c.last_ts).max().unwrap_or(0);
        let duration_s = last_ts.saturating_sub(first_ts) as f64 / 1_000_000.0;
        let mut ids: HashMap<(u16, u32), u32> = HashMap::new();
        for c in &infos {
            for (inst, id) in &c.channel_ids {
                ids.insert((c.provider_id, *inst), *id);
            }
        }
        let ids_label = analysis::ella_compact_channel_ids(&ids);
        let last_s = last_ts.saturating_sub(t0_us) as f64 / 1_000_000.0;
        let t_min = 0.0;
        let t_max = capture_t_max.max(last_s).max(1.0);
        let node = analysis::ella_nodes_label(
            prof,
            &members.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(),
        );
        let show_node = prof.fanout_provider_ids().len() > 1;

        // Plot data + caption iterate the channel's events. Native
        // offloads that; wasm runs it in `new`.
        #[cfg(not(target_arch = "wasm32"))]
        let (data, caption, pending) = {
            let (tx, rx) = mpsc::channel();
            let prof = Arc::clone(prof);
            std::thread::spawn(move || {
                let data = analysis::build_ella_channel_plot_for(&prof, &members, t0_us);
                let events = analysis::build_ella_channel_events_for(&prof, &members, t0_us);
                let caption = ella_channel_caption(&events, t_max);
                let _ = tx.send((data, caption));
            });
            (
                analysis::EllaChannelPlotData::default(),
                String::new(),
                Some(rx),
            )
        };
        #[cfg(target_arch = "wasm32")]
        let (data, caption, pending) = {
            let data = analysis::build_ella_channel_plot_for(prof, &members, t0_us);
            let events = analysis::build_ella_channel_events_for(prof, &members, t0_us);
            (data, ella_channel_caption(&events, t_max), None)
        };

        Self {
            provider_id,
            session_id,
            label,
            node,
            show_node,
            full_name,
            version,
            duration_s,
            peak_subscribers,
            state,
            ids_label,
            caption,
            data,
            time_axis: TimeAxisController::default(),
            settle_frames: SETTLE_FRAMES,
            t_min,
            t_max,
            plot_layout: default_layout(PlotCatalogKind::EllaChannel),
            plot_draft: default_layout(PlotCatalogKind::EllaChannel),
            needs_legend_init: true,
            object_pct: analysis::EllaObjectPercentile::Median,
            pending,
        }
    }
}

impl EventExplorerApp {
    fn ensure_ella_samples_view(&mut self, ui: &mut egui::Ui) -> &mut EllaSamplesViewState {
        if self.ella_samples_view.is_none() {
            let t0 = self
                .file_info
                .as_ref()
                .map(|f| f.capture_start)
                .unwrap_or(0);
            self.ella_samples_view = Some(EllaSamplesViewState::new(&self.ella_profile, t0));
        }
        let state = self.ella_samples_view.as_mut().unwrap();
        if state.settle_frames > 0 {
            state.settle_frames -= 1;
            ui.ctx().request_repaint();
        }
        state
    }

    /// System-tab Fanout occupancy panels and per-title subscriber stack
    /// for one catalog role (`Fanout Server` / `Fanout Relay`).
    pub(crate) fn ella_section(
        &mut self,
        ui: &mut egui::Ui,
        cursor: &mut SysCursor,
        plot_id: PlotId,
    ) {
        let Some((server, kind)) = plot_id.fanout_split() else {
            return;
        };
        let role = if server {
            EllaNodeRole::Server
        } else {
            EllaNodeRole::Relay
        };
        if !self.ella_profile.has_channel_counts()
            && !self.ella_has_frame_timing
            && self.ella_profile.channels.is_empty()
        {
            return;
        }
        let x_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        let seed = self.agg_time_axis.seed(0.0, x_max);
        self.ensure_ella_samples_view(ui);
        let plot_ids: Vec<String> = {
            let state = self.ella_samples_view.as_ref().unwrap();
            state
                .nodes
                .iter()
                .filter(|n| ella_catalog_role_matches(role, n.role))
                .flat_map(|n| {
                    kind.slot_bases()
                        .iter()
                        .map(|base| ella_plot_id(state.multi, n.provider_id, base))
                })
                .collect()
        };
        for id in &plot_ids {
            self.agg_time_axis.apply(ui.ctx(), id);
        }
        let node_match: Vec<bool> = {
            let state = self.ella_samples_view.as_ref().unwrap();
            state
                .nodes
                .iter()
                .map(|n| ella_catalog_role_matches(role, n.role))
                .collect()
        };
        let title_rates: HashMap<String, Option<f64>> = self
            .ella_channel_list
            .iter()
            .map(|r| (r.short_label.clone(), r.rx_rate_mbps))
            .collect();
        let clicked = {
            let state = self.ella_samples_view.as_mut().unwrap();
            let multi = state.multi;
            let any = state
                .nodes
                .iter()
                .enumerate()
                .any(|(i, n)| node_match[i] && kind.node_has_data(n));
            if !any {
                return;
            }

            let mut clicked: Option<String> = None;
            let catalog = plot_id;

            for (i, &ok) in node_match.iter().enumerate() {
                if !ok || !kind.node_has_data(&state.nodes[i]) {
                    continue;
                }
                let hover = match kind {
                    FanoutPlot::Subscribers | FanoutPlot::Rx | FanoutPlot::Tx => {
                        title_hover_stats(&title_rates, &state.nodes[i].stack.series)
                    }
                    _ => HashMap::new(),
                };
                let pid = state.nodes[i].provider_id;
                let id = |base: &str| ella_plot_id(multi, pid, base);
                let heading = |name: &str| {
                    if multi {
                        name.to_string()
                    } else {
                        format!("Fanout {name}")
                    }
                };

                if multi {
                    ui.add_space(6.0);
                    ui.strong(
                        RichText::new(format!("Fanout — {}", state.nodes[i].label)).size(14.0),
                    );
                }

                match kind {
                    FanoutPlot::Subscribers => {
                        note_stale_occupancy(ui, &state.nodes[i].relay, x_max);
                        // Subscribers holds either this node's per-title stack or
                        // its sampled Internal / External totals; both exist when
                        // the node has occupancy samples and channel events.
                        let has_subs = !state.nodes[i].relay.subscribers.is_empty();
                        let has_stack = !state.nodes[i].stack.series.is_empty();
                        let show_subs_selector = has_subs && has_stack;
                        if has_subs || has_stack {
                            let show_per_channel = has_stack
                                && (!has_subs || state.subs_view == EllaSubsView::PerChannel);
                            let slot = if show_per_channel {
                                id("ella_title_stack_plot")
                            } else {
                                id("ella_subs_plot")
                            };
                            if self.system_layout.slot_is_shown(&slot) {
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    ui.strong(RichText::new(heading("Subscribers")).size(14.0));
                                    if show_subs_selector {
                                        ui.add_space(12.0);
                                        ui.selectable_value(
                                            &mut state.subs_view,
                                            EllaSubsView::Aggregate,
                                            RichText::new("Aggregate").size(13.0),
                                        )
                                        .on_hover_text("Sampled Internal / External occupancy.");
                                        ui.selectable_value(
                                            &mut state.subs_view,
                                            EllaSubsView::PerChannel,
                                            RichText::new("Per channel").size(13.0),
                                        )
                                        .on_hover_text("CHANNEL_SAMPLE occupancy per title.");
                                    }
                                });
                                prepare_plot_slot(
                                    &self.system_layout,
                                    catalog,
                                    &slot,
                                    cursor,
                                    ui.available_height(),
                                );
                                if show_per_channel {
                                    if !show_subs_selector {
                                        ui.label(
                                    RichText::new(
                                        "Live downstream clients per title from CHANNEL_SAMPLE.",
                                    )
                                    .size(11.0)
                                    .color(Color32::from_gray(160)),
                                );
                                    }
                                    if let Some(label) = draw_named_stack(
                                        ui,
                                        &slot,
                                        "Subscribers",
                                        cursor.panel_h,
                                        &state.nodes[i].stack.series,
                                        cursor,
                                        seed,
                                        &state.nodes[i].title_colors,
                                        &hover,
                                    ) {
                                        clicked = Some(label);
                                    }
                                } else {
                                    let subs = named_lines_off(
                                        &state.nodes[i].relay.subscribers,
                                        state.nodes[i].color_off,
                                    );
                                    draw_channel_binned_lines(
                                        ui,
                                        cursor,
                                        seed,
                                        subs,
                                        "Subscribers",
                                        &slot,
                                        true,
                                        false,
                                    );
                                }
                                finish_plot_slot(
                                    ui,
                                    &mut self.system_layout,
                                    &mut self.system_draft,
                                    catalog,
                                    &slot,
                                    cursor.last_plot_rect,
                                );
                            }
                        }
                    }
                    FanoutPlot::Rx => {
                        if !state.nodes[i].rx_stack.series.is_empty() {
                            let slot = id("ella_rx_stack_plot");
                            if self.system_layout.slot_is_shown(&slot) {
                                ui.add_space(6.0);
                                ui.strong(RichText::new(heading("RX")).size(14.0));
                                ui.label(
                            RichText::new(
                                "Upstream object Mbps per title (same RX as the channel tab).",
                            )
                            .size(11.0)
                            .color(Color32::from_gray(160)),
                        );
                                prepare_plot_slot(
                                    &self.system_layout,
                                    catalog,
                                    &slot,
                                    cursor,
                                    ui.available_height(),
                                );
                                if let Some(label) = draw_named_stack(
                                    ui,
                                    &slot,
                                    "Mbps",
                                    cursor.panel_h,
                                    &state.nodes[i].rx_stack.series,
                                    cursor,
                                    seed,
                                    &state.nodes[i].title_colors,
                                    &hover,
                                ) {
                                    clicked = Some(label);
                                }
                                finish_plot_slot(
                                    ui,
                                    &mut self.system_layout,
                                    &mut self.system_draft,
                                    catalog,
                                    &slot,
                                    cursor.last_plot_rect,
                                );
                            }
                        }
                    }
                    FanoutPlot::Tx => {
                        if !state.nodes[i].tx_stack.series.is_empty() {
                            let slot = id("ella_tx_stack_plot");
                            if self.system_layout.slot_is_shown(&slot) {
                                ui.add_space(6.0);
                                ui.strong(RichText::new(heading("TX")).size(14.0));
                                ui.label(
                            RichText::new(
                                "Downstream object Mbps per title (same TX as the channel tab).",
                            )
                            .size(11.0)
                            .color(Color32::from_gray(160)),
                        );
                                prepare_plot_slot(
                                    &self.system_layout,
                                    catalog,
                                    &slot,
                                    cursor,
                                    ui.available_height(),
                                );
                                if let Some(label) = draw_named_stack(
                                    ui,
                                    &slot,
                                    "Mbps",
                                    cursor.panel_h,
                                    &state.nodes[i].tx_stack.series,
                                    cursor,
                                    seed,
                                    &state.nodes[i].title_colors,
                                    &hover,
                                ) {
                                    clicked = Some(label);
                                }
                                finish_plot_slot(
                                    ui,
                                    &mut self.system_layout,
                                    &mut self.system_draft,
                                    catalog,
                                    &slot,
                                    cursor.last_plot_rect,
                                );
                            }
                        }
                    }
                    FanoutPlot::Traffic => {
                        if !state.nodes[i].traffic.is_empty() {
                            let slot = id("ella_traffic_plot");
                            if self.system_layout.slot_is_shown(&slot) {
                                ui.add_space(6.0);
                                ui.strong(RichText::new(heading("Traffic")).size(14.0))
                                    .on_hover_text(
                                        "CHANNEL_STATS DATA bytes over the sample interval (syslog bytesSentCount / relay upstream+aggregate).",
                                    );
                                ui.label(
                                    RichText::new(
                                        "Process-wide DATA Mbps from CHANNEL_STATS. Upstream is origin-facing; Downstream is local send; Delivered is subscriber KEEPALIVE reports.",
                                    )
                                    .size(11.0)
                                    .color(Color32::from_gray(160)),
                                );
                                prepare_plot_slot(
                                    &self.system_layout,
                                    catalog,
                                    &slot,
                                    cursor,
                                    ui.available_height(),
                                );
                                let lines = named_lines_off(
                                    &state.nodes[i].traffic,
                                    state.nodes[i].color_off,
                                );
                                draw_channel_binned_lines(
                                    ui, cursor, seed, lines, "Mbps", &slot, true, false,
                                );
                                finish_plot_slot(
                                    ui,
                                    &mut self.system_layout,
                                    &mut self.system_draft,
                                    catalog,
                                    &slot,
                                    cursor.last_plot_rect,
                                );
                            }
                        }
                    }
                    FanoutPlot::Joins => {
                        if !state.nodes[i].joins.is_empty() {
                            let slot = id("ella_joins_plot");
                            if self.system_layout.slot_is_shown(&slot) {
                                ui.add_space(6.0);
                                ui.strong(RichText::new(heading("Joins")).size(14.0))
                                    .on_hover_text(
                                        "CHANNEL_STATS / TRAFFIC_STATS JOIN/SWITCH/LEAVE/new-client deltas per second.",
                                    );
                                ui.label(
                                    RichText::new(
                                        "Accepted JOIN/SWITCH, ended (LEAVE, KEEPALIVE expiry) and rejected subscriptions, and new clients per second. Delivered joins are subscriber KEEPALIVE reports.",
                                    )
                                    .size(11.0)
                                    .color(Color32::from_gray(160)),
                                );
                                prepare_plot_slot(
                                    &self.system_layout,
                                    catalog,
                                    &slot,
                                    cursor,
                                    ui.available_height(),
                                );
                                let lines = named_lines_off(
                                    &state.nodes[i].joins,
                                    state.nodes[i].color_off,
                                );
                                draw_channel_binned_lines(
                                    ui, cursor, seed, lines, "/s", &slot, true, false,
                                );
                                finish_plot_slot(
                                    ui,
                                    &mut self.system_layout,
                                    &mut self.system_draft,
                                    catalog,
                                    &slot,
                                    cursor.last_plot_rect,
                                );
                            }
                        }
                    }
                    FanoutPlot::ObjectTiming => {
                        if !state.nodes[i].object_timing.is_empty() {
                            let slot = id("ella_object_timing_plot");
                            if self.system_layout.slot_is_shown(&slot) {
                                ui.add_space(6.0);
                                ui.strong(RichText::new(heading("Object Timing")).size(14.0))
                                    .on_hover_text(
                                        "OBJECT_TIMING medians in 1 s bins (ELLA_TRACE object spans).",
                                    );
                                ui.label(
                                    RichText::new(
                                        "Origin object start lateness and signature/encode/enqueue durations (ms). Requires keyword OBJECT in the capture.",
                                    )
                                    .size(11.0)
                                    .color(Color32::from_gray(160)),
                                );
                                prepare_plot_slot(
                                    &self.system_layout,
                                    catalog,
                                    &slot,
                                    cursor,
                                    ui.available_height(),
                                );
                                let lines = named_lines_off(
                                    &state.nodes[i].object_timing,
                                    state.nodes[i].color_off,
                                );
                                draw_channel_binned_lines(
                                    ui, cursor, seed, lines, "ms", &slot, true, false,
                                );
                                finish_plot_slot(
                                    ui,
                                    &mut self.system_layout,
                                    &mut self.system_draft,
                                    catalog,
                                    &slot,
                                    cursor.last_plot_rect,
                                );
                            }
                        }
                    }
                    FanoutPlot::FrameRate => {
                        if !state.nodes[i].frame_rate.is_empty() {
                            let slot = id("ella_frame_rate_plot");
                            if self.system_layout.slot_is_shown(&slot) {
                                ui.add_space(6.0);
                                ui.strong(RichText::new(heading("Frame Send")).size(14.0))
                        .on_hover_text(
                            "Byte-weighted FDACE rate from FRAME_SENT / FRAME_RELAY, or FRAME_SAMPLE, summed across titles. Actual / Wire start on; Planned / Recv / Fwd start hidden.",
                        );
                                ui.label(
                                    RichText::new(if state.nodes[i].role == EllaNodeRole::Server {
                                        ella_frame_plot_caption(false)
                                    } else {
                                        ella_frame_plot_caption(true)
                                    })
                                    .size(11.0)
                                    .color(Color32::from_gray(160)),
                                );
                                prepare_plot_slot(
                                    &self.system_layout,
                                    catalog,
                                    &slot,
                                    cursor,
                                    ui.available_height(),
                                );
                                let lines = named_lines_off(
                                    &state.nodes[i].frame_rate,
                                    state.nodes[i].color_off,
                                );
                                draw_channel_binned_lines(
                                    ui, cursor, seed, lines, "Mbps", &slot, true, false,
                                );
                                if state.legend_inited.insert(slot.clone()) {
                                    let labels: Vec<String> = state.nodes[i]
                                        .frame_rate
                                        .iter()
                                        .map(|s| s.label.clone())
                                        .collect();
                                    hide_ella_frame_legend(ui.ctx(), &slot, &labels);
                                    ui.ctx().request_repaint();
                                }
                                finish_plot_slot(
                                    ui,
                                    &mut self.system_layout,
                                    &mut self.system_draft,
                                    catalog,
                                    &slot,
                                    cursor.last_plot_rect,
                                );
                            }
                        }
                    }
                    FanoutPlot::FrameDuration => {
                        let duration_views = state.nodes[i].frame_duration_views.len();
                        for didx in 0..duration_views {
                            if state.nodes[i].frame_duration_views[didx].series.is_empty() {
                                continue;
                            }
                            let slot = id(state.nodes[i].frame_duration_views[didx].plot_base);
                            if !self.system_layout.slot_is_shown(&slot) {
                                continue;
                            }
                            ui.add_space(6.0);
                            let server = state.nodes[i].role == EllaNodeRole::Server;
                            let duration_view = &state.nodes[i].frame_duration_views[didx];
                            let name = if server {
                                "Server Frame Duration".to_string()
                            } else {
                                format!(
                                    "Relay Frame Duration — {}",
                                    duration_view.side_label.unwrap_or("External")
                                )
                            };
                            ui.strong(RichText::new(heading(&name)).size(14.0))
                        .on_hover_text(
                            "Mean per-frame duration. Intercept is Recv/Fwd holding above a shorter Wire.",
                        );
                            ui.label(
                                RichText::new(ella_frame_duration_caption(!server))
                                    .size(11.0)
                                    .color(Color32::from_gray(160)),
                            );
                            prepare_plot_slot(
                                &self.system_layout,
                                catalog,
                                &slot,
                                cursor,
                                ui.available_height(),
                            );
                            let lines = named_lines_off(
                                &state.nodes[i].frame_duration_views[didx].series,
                                state.nodes[i].color_off,
                            );
                            draw_channel_binned_lines(
                                ui, cursor, seed, lines, "ms", &slot, false, false,
                            );
                            finish_plot_slot(
                                ui,
                                &mut self.system_layout,
                                &mut self.system_draft,
                                catalog,
                                &slot,
                                cursor.last_plot_rect,
                            );
                        }
                    }
                    FanoutPlot::FrameRatio => {
                        let ratio_views = state.nodes[i].frame_ratio_views.len();
                        for ridx in 0..ratio_views {
                            if state.nodes[i].frame_ratio_views[ridx].series.is_empty() {
                                continue;
                            }
                            let slot = id(state.nodes[i].frame_ratio_views[ridx].plot_base);
                            if !self.system_layout.slot_is_shown(&slot) {
                                continue;
                            }
                            ui.add_space(6.0);
                            let server = state.nodes[i].role == EllaNodeRole::Server;
                            let ratio_view = &state.nodes[i].frame_ratio_views[ridx];
                            let name = if server {
                                "Server Actual Send Duration / Planned Send Duration".to_string()
                            } else {
                                format!(
                                    "Relay Receive Duration / Server Send Duration — {}",
                                    ratio_view.side_label.unwrap_or("External")
                                )
                            };
                            let detail = if ratio_view.is_fallback {
                                "Fallback from FRAME_SAMPLE: each value first sums one channel/instance's timing totals over 2 s, then percentiles are calculated across those aggregates. It is not a per-frame distribution."
                            } else if server {
                                "Detailed FRAME_SENT actual / planned send-duration percentiles in 100 ms bins."
                            } else {
                                "Detailed FRAME_RELAY receive / server-send-duration percentiles in 100 ms bins."
                            };
                            let heading_s = if ratio_view.is_fallback {
                                format!("{} — FRAME_SAMPLE Fallback", heading(&name))
                            } else {
                                heading(&name)
                            };
                            ui.strong(RichText::new(heading_s).size(14.0))
                                .on_hover_text(detail);
                            let caption = if ratio_view.is_fallback {
                                "FRAME_SAMPLE fallback — 2 s per-channel/instance aggregate ratios, not individual frames."
                            } else {
                                "Detailed per-frame ratios — Median, P75, P95, and Max in 100 ms bins."
                            };
                            ui.label(
                                RichText::new(caption)
                                    .size(11.0)
                                    .color(Color32::from_gray(160)),
                            );
                            prepare_plot_slot(
                                &self.system_layout,
                                catalog,
                                &slot,
                                cursor,
                                ui.available_height(),
                            );
                            let lines = named_lines_off(
                                &state.nodes[i].frame_ratio_views[ridx].series,
                                state.nodes[i].color_off,
                            );
                            draw_channel_binned_lines(
                                ui, cursor, seed, lines, "Ratio", &slot, false, false,
                            );
                            if state.legend_inited.insert(slot.clone()) {
                                let labels: Vec<String> = state.nodes[i].frame_ratio_views[ridx]
                                    .series
                                    .iter()
                                    .map(|s| s.label.clone())
                                    .collect();
                                hide_ella_ratio_legend(ui.ctx(), &slot, &labels);
                                ui.ctx().request_repaint();
                            }
                            finish_plot_slot(
                                ui,
                                &mut self.system_layout,
                                &mut self.system_draft,
                                catalog,
                                &slot,
                                cursor.last_plot_rect,
                            );
                        }
                    }
                    FanoutPlot::Channels => {
                        note_stale_occupancy(ui, &state.nodes[i].relay, x_max);
                        let has_up = !upstream_channel_lines(&state.nodes[i].relay).is_empty();
                        let has_dn = !downstream_channel_lines(&state.nodes[i].relay, state.dn_agg)
                            .is_empty();
                        let up_slot = id("ella_up_chans_plot");
                        let dn_slot = id("ella_dn_chans_plot");
                        let show_up = has_up && self.system_layout.slot_is_shown(&up_slot);
                        let show_dn = has_dn && self.system_layout.slot_is_shown(&dn_slot);
                        if show_up || show_dn {
                            ui.add_space(6.0);
                            ui.strong(RichText::new(heading("Channels")).size(14.0));
                            ui.label(
                    RichText::new(
                        "Leave = origin LEAVE held until the delay expires. Retire = waiting on a join boundary.",
                    )
                    .size(11.0)
                    .color(Color32::from_gray(160)),
                );
                            if show_up {
                                let up = upstream_channel_lines(&state.nodes[i].relay);
                                ui.strong(RichText::new("Upstream").size(14.0));
                                prepare_plot_slot(
                                    &self.system_layout,
                                    catalog,
                                    &up_slot,
                                    cursor,
                                    ui.available_height(),
                                );
                                draw_channel_binned_lines(
                                    ui, cursor, seed, up, "Channels", &up_slot, true, false,
                                );
                                finish_plot_slot(
                                    ui,
                                    &mut self.system_layout,
                                    &mut self.system_draft,
                                    catalog,
                                    &up_slot,
                                    cursor.last_plot_rect,
                                );
                            }
                            if show_dn {
                                ui.horizontal(|ui| {
                                    ui.strong(RichText::new("Downstream").size(14.0));
                                    if state.nodes[i].relay.aggregated
                                        && !state.nodes[i].relay.dn_channels_max.is_empty()
                                    {
                                        ui.add_space(12.0);
                                        ui.selectable_value(
                                            &mut state.dn_agg,
                                            EllaDownstreamAgg::Max,
                                            RichText::new("Max").size(13.0),
                                        );
                                        ui.selectable_value(
                                            &mut state.dn_agg,
                                            EllaDownstreamAgg::Sum,
                                            RichText::new("Sum").size(13.0),
                                        );
                                    }
                                });
                                prepare_plot_slot(
                                    &self.system_layout,
                                    catalog,
                                    &dn_slot,
                                    cursor,
                                    ui.available_height(),
                                );
                                let dn =
                                    downstream_channel_lines(&state.nodes[i].relay, state.dn_agg);
                                draw_channel_binned_lines(
                                    ui, cursor, seed, dn, "Channels", &dn_slot, true, false,
                                );
                                finish_plot_slot(
                                    ui,
                                    &mut self.system_layout,
                                    &mut self.system_draft,
                                    catalog,
                                    &dn_slot,
                                    cursor.last_plot_rect,
                                );
                            }
                        }
                    }
                    FanoutPlot::Errors => {
                        let errs =
                            named_lines_off(&state.nodes[i].relay.errors, state.nodes[i].color_off);
                        if !errs.is_empty() {
                            let slot = id("ella_errors_plot");
                            if self.system_layout.slot_is_shown(&slot) {
                                ui.add_space(6.0);
                                ui.strong(RichText::new(heading("Errors")).size(14.0));
                                prepare_plot_slot(
                                    &self.system_layout,
                                    catalog,
                                    &slot,
                                    cursor,
                                    ui.available_height(),
                                );
                                draw_channel_binned_lines(
                                    ui, cursor, seed, errs, "Errors", &slot, true, false,
                                );
                                finish_plot_slot(
                                    ui,
                                    &mut self.system_layout,
                                    &mut self.system_draft,
                                    catalog,
                                    &slot,
                                    cursor.last_plot_rect,
                                );
                            }
                        }
                    }
                }
            }
            clicked
        };

        if let Some(title) = clicked {
            if let Some((pid, sid)) =
                analysis::ella_channel_for_stack_label(&self.ella_profile, &title)
            {
                self.select_ella_channel(pid, sid);
            }
        }
    }

    pub(crate) fn select_ella_channel(&mut self, provider_id: u16, session_id: u64) {
        if self
            .open_ella_channels
            .iter()
            .any(|c| c.provider_id == provider_id && c.session_id == session_id)
        {
            self.active_tab = Tab::EllaChannel {
                provider_id,
                session_id,
            };
            return;
        }
        let t0 = self
            .file_info
            .as_ref()
            .map(|f| f.capture_start)
            .unwrap_or(0);
        let capture_t_max = self
            .aggregate
            .as_ref()
            .map(|a| a.time_max.max(1.0))
            .unwrap_or(1.0);
        self.open_ella_channels.push(EllaChannelViewState::new(
            &self.ella_profile,
            provider_id,
            session_id,
            t0,
            capture_t_max,
        ));
        self.active_tab = Tab::EllaChannel {
            provider_id,
            session_id,
        };
    }

    pub(crate) fn show_ella_channel_view(&mut self, ui: &mut egui::Ui, idx: usize) {
        let utc_origin = utc_axis_origin(
            self.utc_time_labels,
            self.file_info.as_ref(),
            self.file_info
                .as_ref()
                .map(|f| f.capture_start)
                .unwrap_or(0),
        );
        {
            let state = &mut self.open_ella_channels[idx];
            if state.settle_frames > 0 {
                state.settle_frames -= 1;
                ui.ctx().request_repaint();
            }
        }
        // Land the background plot-data compute when it finishes.
        let mut landed: Option<(analysis::EllaChannelPlotData, String)> = None;
        let mut disconnected = false;
        if let Some(rx) = self.open_ella_channels[idx].pending.as_ref() {
            match rx.try_recv() {
                Ok(v) => landed = Some(v),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => disconnected = true,
            }
        }
        if let Some((data, caption)) = landed {
            let ch = &mut self.open_ella_channels[idx];
            ch.data = data;
            ch.caption = caption;
            ch.pending = None;
            ui.ctx().request_repaint();
        } else if disconnected {
            self.open_ella_channels[idx].pending = None;
        }
        let loading = self.open_ella_channels[idx].pending.is_some();
        let label = self.open_ella_channels[idx].label.clone();
        let full_name = self.open_ella_channels[idx].full_name.clone();
        let version = self.open_ella_channels[idx].version;
        let duration_s = self.open_ella_channels[idx].duration_s;
        let peak = self.open_ella_channels[idx].peak_subscribers;
        let peak_objects = self.open_ella_channels[idx]
            .data
            .objects
            .iter()
            .flat_map(|s| s.data.iter().map(|p| p[1]))
            .fold(0.0_f64, f64::max);
        let state_name = self.open_ella_channels[idx].state;
        let ids_label = self.open_ella_channels[idx].ids_label.clone();
        let node = self.open_ella_channels[idx].node.clone();
        let show_node = self.open_ella_channels[idx].show_node;
        let caption = self.open_ella_channels[idx].caption.clone();
        ui.heading(if show_node {
            format!("Fanout  {node}  {label}")
        } else {
            format!("Fanout  {label}")
        });
        if !full_name.is_empty() {
            ui.label(
                RichText::new(full_name)
                    .size(12.0)
                    .color(Color32::from_gray(160)),
            );
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let chip = |ui: &mut egui::Ui, key: &str, value: String| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(key).size(11.0).color(Color32::from_gray(140)));
                    ui.label(RichText::new(value).size(13.0));
                });
            };
            chip(ui, "Peak", format_count(peak as u64));
            if show_node {
                chip(ui, "Node", node);
            }
            if peak_objects > 0.0 {
                chip(ui, "Objects", format_count(peak_objects as u64));
            }
            chip(ui, "Duration", format!("{duration_s:.1}s"));
            chip(ui, "State", state_name.to_string());
            chip(ui, "IDs", ids_label);
            if version > 0 {
                chip(ui, "Ver", format!("{version}"));
            }
        });
        ui.separator();

        if loading {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(format!("Computing {label}…"))
                        .size(14.0)
                        .color(Color32::from_gray(160)),
                );
                ui.add_space(8.0);
                ui.add(egui::Spinner::new());
            });
            ui.ctx().request_repaint();
            return;
        }

        let t_min = self.open_ella_channels[idx].t_min;
        let t_max = self.open_ella_channels[idx].t_max;
        let seed = self.open_ella_channels[idx].time_axis.seed(t_min, t_max);
        let ch_key = {
            let c = &self.open_ella_channels[idx];
            format!("{}_{}", c.provider_id, c.session_id)
        };
        let one_id = format!("ella_ch_subs_{ch_key}");
        let tx_one_id = format!("ella_ch_tx_{ch_key}");
        let object_timing_id = format!("ella_ch_object_timing_{ch_key}");
        let obj_id = format!("ella_ch_objid_{ch_key}");
        let frame_tl_id = format!("ella_ch_frametl_{ch_key}");
        let obj_dur_id = format!("ella_ch_objdur_{ch_key}");
        let obj_size_id = format!("ella_ch_objsize_{ch_key}");
        let frame_id = format!("ella_ch_frame_{ch_key}");
        let frame_relay_id = format!("ella_ch_frame_relay_{ch_key}");
        let duration_id = format!("ella_ch_duration_{ch_key}");
        let duration_relay_id = format!("ella_ch_duration_relay_{ch_key}");
        let ratio_id = format!("ella_ch_ratio_{ch_key}");
        let ratio_relay_id = format!("ella_ch_ratio_relay_{ch_key}");
        let errors_id = format!("ella_ch_errors_{ch_key}");
        for id in [
            one_id.as_str(),
            tx_one_id.as_str(),
            obj_id.as_str(),
            frame_tl_id.as_str(),
            obj_dur_id.as_str(),
            obj_size_id.as_str(),
            frame_id.as_str(),
            frame_relay_id.as_str(),
            duration_id.as_str(),
            duration_relay_id.as_str(),
            ratio_id.as_str(),
            ratio_relay_id.as_str(),
            errors_id.as_str(),
        ] {
            self.open_ella_channels[idx].time_axis.apply(ui.ctx(), id);
        }

        let mut cursor = SysCursor::new(ui, utc_origin, self.plot_bins);
        cursor.show_legends = self.show_legends;
        let plot_budget = ui.available_height().max(AGG_PANEL_H);
        let mut plot_clip: Option<egui::Rect> = None;
        egui::ScrollArea::vertical()
            .id_source(format!("ella_ch_scroll_{ch_key}"))
            .max_height(plot_budget)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                plot_clip = Some(ui.clip_rect());
                let order = self.open_ella_channels[idx].plot_layout.order.clone();
                for &id in &order {
                    let remaining = ui.available_height();
                    {
                        let ch = &self.open_ella_channels[idx];
                        if !ch.plot_layout.is_visible(id) {
                            continue;
                        }
                        if ch.plot_layout.maximized.is_some_and(|m| m != id) {
                            continue;
                        }
                        cursor.fill_remaining = ch.plot_layout.fills_catalog_row(id);
                        cursor.panel_h = ch.plot_layout.draw_height(id, remaining);
                    }
                    if self.pending_scroll_plot == Some(id) {
                        ui.scroll_to_cursor(Some(egui::Align::Center));
                        self.pending_scroll_plot = None;
                    }
                    cursor.last_plot_rect = None;
                    match id {
                        PlotId::EllaSubscribers => {
                            draw_channel_series_block(
                                ui,
                                &mut cursor,
                                seed,
                                &self.open_ella_channels[idx].data.subscribers,
                                Some("Subscribers"),
                                "Subscribers",
                                &one_id,
                                true,
                                false,
                            );
                        }
                        PlotId::EllaObjectRate => {
                            let sample_rate = self.open_ella_channels[idx]
                                .data
                                .object_span_copies
                                .is_empty();
                            let rate = if sample_rate {
                                self.open_ella_channels[idx].data.object_rate.clone()
                            } else {
                                let pct = self.open_ella_channels[idx].object_pct;
                                let copies = &self.open_ella_channels[idx].data.object_span_copies;
                                let qualify = {
                                    let mut pids: Vec<u16> =
                                        copies.iter().map(|s| s.provider_id).collect();
                                    pids.sort_unstable();
                                    pids.dedup();
                                    pids.len() > 1
                                };
                                analysis::object_rate_at_percentile(
                                    &self.ella_profile,
                                    copies,
                                    pct,
                                    qualify,
                                )
                            };
                            draw_channel_series_block(
                                ui,
                                &mut cursor,
                                seed,
                                &rate,
                                Some("Object Rate"),
                                "Mbps",
                                &tx_one_id,
                                true,
                                true,
                            );
                        }
                        PlotId::EllaObjectTiming => {
                            draw_channel_series_block(
                                ui,
                                &mut cursor,
                                seed,
                                &self.open_ella_channels[idx].data.object_timing,
                                Some("Object Timing"),
                                "ms",
                                &object_timing_id,
                                true,
                                false,
                            );
                        }
                        PlotId::EllaObjectId => {
                            let pct = self.open_ella_channels[idx].object_pct;
                            let spans = analysis::object_spans_at_percentile(
                                &self.ella_profile,
                                &self.open_ella_channels[idx].data.object_span_copies,
                                pct,
                            );
                            let has_external = analysis::object_has_external_relay(
                                &self.ella_profile,
                                &self.open_ella_channels[idx].data.object_span_copies,
                            );
                            draw_object_id_strip(
                                ui,
                                &mut cursor,
                                seed,
                                &spans,
                                &self.ella_profile,
                                has_external
                                    .then_some(&mut self.open_ella_channels[idx].object_pct),
                                &obj_id,
                            );
                        }
                        PlotId::EllaFrameId => {
                            let pct = self.open_ella_channels[idx].object_pct;
                            let spans = analysis::frame_spans_at_percentile(
                                &self.ella_profile,
                                &self.open_ella_channels[idx].data.frame_spans,
                                &self.open_ella_channels[idx].data.object_span_copies,
                                pct,
                            );
                            let has_external = analysis::object_has_external_relay(
                                &self.ella_profile,
                                &self.open_ella_channels[idx].data.object_span_copies,
                            );
                            draw_frame_id_strip(
                                ui,
                                &mut cursor,
                                seed,
                                &spans,
                                &self.ella_profile,
                                has_external
                                    .then_some(&mut self.open_ella_channels[idx].object_pct),
                                &frame_tl_id,
                            );
                        }
                        PlotId::EllaObjectDuration => {
                            draw_channel_series_block(
                                ui,
                                &mut cursor,
                                seed,
                                &self.open_ella_channels[idx].data.object_duration,
                                Some("Object Duration"),
                                "seconds",
                                &obj_dur_id,
                                false,
                                true,
                            );
                        }
                        PlotId::EllaObjectSize => {
                            draw_channel_series_block(
                                ui,
                                &mut cursor,
                                seed,
                                &self.open_ella_channels[idx].data.object_size,
                                Some("Object Size"),
                                "MB",
                                &obj_size_id,
                                false,
                                true,
                            );
                        }
                        PlotId::EllaFrameRate => {
                            let series = ella_frame_rate_subset(
                                &self.open_ella_channels[idx].data.frame_rate,
                                false,
                            );
                            let heading = if self.open_ella_channels[idx]
                                .data
                                .frame_rate
                                .iter()
                                .any(|s| ella_frame_label_is_relay(&s.label))
                            {
                                "Server Frame Rate"
                            } else {
                                "Frame Rate"
                            };
                            draw_channel_series_block(
                                ui,
                                &mut cursor,
                                seed,
                                &series,
                                Some(heading),
                                "Mbps",
                                &frame_id,
                                false,
                                false,
                            );
                        }
                        PlotId::EllaFrameRateRelay => {
                            let series = ella_frame_rate_subset(
                                &self.open_ella_channels[idx].data.frame_rate,
                                true,
                            );
                            draw_channel_series_block(
                                ui,
                                &mut cursor,
                                seed,
                                &series,
                                Some("Relay Frame Rate"),
                                "Mbps",
                                &frame_relay_id,
                                false,
                                false,
                            );
                        }
                        PlotId::EllaFrameDuration => {
                            let series = ella_frame_rate_subset(
                                &self.open_ella_channels[idx].data.frame_duration,
                                false,
                            );
                            let heading = if self.open_ella_channels[idx]
                                .data
                                .frame_duration
                                .iter()
                                .any(|s| ella_frame_label_is_relay(&s.label))
                            {
                                "Server Frame Duration"
                            } else {
                                "Frame Duration"
                            };
                            draw_channel_series_block(
                                ui,
                                &mut cursor,
                                seed,
                                &series,
                                Some(heading),
                                "ms",
                                &duration_id,
                                false,
                                false,
                            );
                        }
                        PlotId::EllaFrameDurationRelay => {
                            let series = ella_frame_rate_subset(
                                &self.open_ella_channels[idx].data.frame_duration,
                                true,
                            );
                            draw_channel_series_block(
                                ui,
                                &mut cursor,
                                seed,
                                &series,
                                Some("Relay Frame Duration"),
                                "ms",
                                &duration_relay_id,
                                false,
                                false,
                            );
                        }
                        PlotId::EllaFrameRatio => {
                            let data = &self.open_ella_channels[idx].data;
                            let series = ella_frame_rate_subset(&data.frame_ratio, false);
                            let heading = if data
                                .frame_ratio
                                .iter()
                                .any(|s| ella_frame_label_is_relay(&s.label))
                            {
                                "Server Duration Ratio"
                            } else {
                                "Duration Ratio"
                            };
                            draw_channel_ratio_block(
                                ui,
                                &mut cursor,
                                seed,
                                &series,
                                heading,
                                data.frame_ratio_fallback,
                                &ratio_id,
                            );
                        }
                        PlotId::EllaFrameRatioRelay => {
                            let data = &self.open_ella_channels[idx].data;
                            let series = ella_frame_rate_subset(&data.frame_ratio, true);
                            draw_channel_ratio_block(
                                ui,
                                &mut cursor,
                                seed,
                                &series,
                                "Relay Duration Ratio",
                                data.frame_ratio_fallback,
                                &ratio_relay_id,
                            );
                        }
                        PlotId::EllaErrors => {
                            let errors =
                                named_lines_off(&self.open_ella_channels[idx].data.errors, 0);
                            if !errors.is_empty() {
                                ui.add_space(6.0);
                                ui.strong(RichText::new("Errors").size(14.0));
                                draw_channel_binned_lines(
                                    ui,
                                    &mut cursor,
                                    seed,
                                    errors,
                                    "Errors",
                                    &errors_id,
                                    true,
                                    false,
                                );
                            }
                        }
                        _ => {}
                    }
                    let ch = &mut self.open_ella_channels[idx];
                    finish_catalog_plot(
                        ui,
                        &mut ch.plot_layout,
                        &mut ch.plot_draft,
                        id,
                        cursor.last_plot_rect,
                    );
                }
                if !caption.is_empty() {
                    ui.label(
                        RichText::new(caption)
                            .size(11.0)
                            .color(Color32::from_gray(160)),
                    );
                }
            });

        if self.open_ella_channels[idx].needs_legend_init {
            self.open_ella_channels[idx].needs_legend_init = false;
            let labels: Vec<String> = self.open_ella_channels[idx]
                .data
                .frame_rate
                .iter()
                .map(|s| s.label.clone())
                .collect();
            hide_ella_frame_legend(ui.ctx(), &frame_id, &labels);
            hide_ella_frame_legend(ui.ctx(), &frame_relay_id, &labels);
            let ratio_labels: Vec<String> = self.open_ella_channels[idx]
                .data
                .frame_ratio
                .iter()
                .map(|s| s.label.clone())
                .collect();
            hide_ella_ratio_legend(ui.ctx(), &ratio_id, &ratio_labels);
            hide_ella_ratio_legend(ui.ctx(), &ratio_relay_id, &ratio_labels);
            let obj_labels: Vec<String> = self.open_ella_channels[idx]
                .data
                .object_duration
                .iter()
                .chain(self.open_ella_channels[idx].data.object_size.iter())
                .map(|s| s.label.clone())
                .collect();
            hide_ella_object_legend(ui.ctx(), &obj_dur_id, &obj_labels);
            hide_ella_object_legend(ui.ctx(), &obj_size_id, &obj_labels);
            ui.ctx().request_repaint();
        }

        let layout = &self.open_ella_channels[idx].plot_layout;
        let mut plot_ids: Vec<&str> = Vec::new();
        if layout.is_visible(PlotId::EllaSubscribers)
            && !self.open_ella_channels[idx].data.subscribers.is_empty()
        {
            plot_ids.push(one_id.as_str());
        }
        if layout.is_visible(PlotId::EllaObjectRate)
            && !self.open_ella_channels[idx].data.object_rate.is_empty()
        {
            plot_ids.push(tx_one_id.as_str());
        }
        if layout.is_visible(PlotId::EllaObjectTiming)
            && !self.open_ella_channels[idx].data.object_timing.is_empty()
        {
            plot_ids.push(object_timing_id.as_str());
        }
        if layout.is_visible(PlotId::EllaObjectId)
            && !self.open_ella_channels[idx].data.object_spans.is_empty()
        {
            plot_ids.push(obj_id.as_str());
        }
        if layout.is_visible(PlotId::EllaFrameId)
            && !self.open_ella_channels[idx].data.frame_spans.is_empty()
        {
            plot_ids.push(frame_tl_id.as_str());
        }
        if layout.is_visible(PlotId::EllaObjectDuration)
            && !self.open_ella_channels[idx].data.object_duration.is_empty()
        {
            plot_ids.push(obj_dur_id.as_str());
        }
        if layout.is_visible(PlotId::EllaObjectSize)
            && !self.open_ella_channels[idx].data.object_size.is_empty()
        {
            plot_ids.push(obj_size_id.as_str());
        }
        if layout.is_visible(PlotId::EllaFrameRate)
            && self.open_ella_channels[idx]
                .data
                .frame_rate
                .iter()
                .any(|s| !ella_frame_label_is_relay(&s.label))
        {
            plot_ids.push(frame_id.as_str());
        }
        if layout.is_visible(PlotId::EllaFrameRateRelay)
            && self.open_ella_channels[idx]
                .data
                .frame_rate
                .iter()
                .any(|s| ella_frame_label_is_relay(&s.label))
        {
            plot_ids.push(frame_relay_id.as_str());
        }
        if layout.is_visible(PlotId::EllaFrameDuration)
            && self.open_ella_channels[idx]
                .data
                .frame_duration
                .iter()
                .any(|s| !ella_frame_label_is_relay(&s.label))
        {
            plot_ids.push(duration_id.as_str());
        }
        if layout.is_visible(PlotId::EllaFrameDurationRelay)
            && self.open_ella_channels[idx]
                .data
                .frame_duration
                .iter()
                .any(|s| ella_frame_label_is_relay(&s.label))
        {
            plot_ids.push(duration_relay_id.as_str());
        }
        if layout.is_visible(PlotId::EllaFrameRatio)
            && self.open_ella_channels[idx]
                .data
                .frame_ratio
                .iter()
                .any(|s| !ella_frame_label_is_relay(&s.label))
        {
            plot_ids.push(ratio_id.as_str());
        }
        if layout.is_visible(PlotId::EllaFrameRatioRelay)
            && self.open_ella_channels[idx]
                .data
                .frame_ratio
                .iter()
                .any(|s| ella_frame_label_is_relay(&s.label))
        {
            plot_ids.push(ratio_relay_id.as_str());
        }
        if layout.is_visible(PlotId::EllaErrors)
            && !self.open_ella_channels[idx].data.errors.is_empty()
        {
            plot_ids.push(errors_id.as_str());
        }
        let t_min = self.open_ella_channels[idx].t_min;
        let t_max = self.open_ella_channels[idx].t_max;
        // Clip the gesture/band geometries to the scroll region so the
        // measurement band and Δt label stop at the plots, below the header.
        let geoms: Vec<_> = match plot_clip {
            Some(clip) => cursor
                .geoms()
                .iter()
                .map(|(r, t)| (r.intersect(clip), *t))
                .filter(|(r, _)| r.is_positive())
                .collect(),
            None => cursor.geoms().to_vec(),
        };
        if let TimeAxisOutcome::Reset { .. } = self.open_ella_channels[idx]
            .time_axis
            .handle(ui, &geoms, t_min, t_max)
        {
            self.open_ella_channels[idx]
                .time_axis
                .reset_plots(ui.ctx(), &plot_ids);
        }
        if self.open_ella_channels[idx].plot_layout.maximized.is_some()
            && Self::escape_restores_plot_maximize(ui)
        {
            self.open_ella_channels[idx].plot_layout.clear_maximize();
            self.open_ella_channels[idx].plot_draft.clear_maximize();
        }
        cursor.draw(ui);
    }

    pub(crate) fn show_ella_channel_list(&mut self, ui: &mut egui::Ui) {
        if self.ella_channel_list.is_empty() {
            return;
        }
        sort_ella_channel_list(
            &mut self.ella_channel_list,
            self.ella_sort_column,
            self.ella_sort_ascending,
        );
        let n_sessions = self.ella_profile.channels.len();
        let n = self.ella_channel_list.len();

        ui.add_space(12.0);
        ui.heading(format!(
            "Fanout channels ({n} titles, {n_sessions} sessions)"
        ));
        ui.separator();

        let header_label =
            |col: EllaSortColumn, text: &str, current: EllaSortColumn, asc: bool| -> String {
                if col == current {
                    let arrow = if asc { " ^" } else { " v" };
                    format!("{text}{arrow}")
                } else {
                    text.to_string()
                }
            };

        let mut selected: Option<(u16, u64)> = None;
        let text_height = ui.text_style_height(&egui::TextStyle::Body);
        let body_h = ui.available_height();
        let open: Vec<(u16, u64)> = self
            .open_ella_channels
            .iter()
            .map(|c| (c.provider_id, c.session_id))
            .collect();

        ui.push_id("ella_channel_list_cols6", |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                // Node holds a merged label like `Server + Relay`.
                .column(Column::exact(104.0))
                .column(Column::auto().at_least(100.0))
                .column(Column::exact(78.0))
                .column(Column::remainder().at_least(280.0))
                .column(Column::exact(70.0))
                .column(Column::auto().at_least(70.0))
                .column(Column::exact(62.0))
                .column(Column::exact(62.0))
                .column(Column::exact(70.0))
                .column(Column::auto().at_least(70.0))
                .column(Column::auto().at_least(80.0))
                .column(Column::auto().at_least(70.0))
                .min_scrolled_height(0.0)
                .max_scroll_height(body_h)
                .drag_to_scroll(false)
                .sense(egui::Sense::click())
                .header(text_height * 1.5, |mut header| {
                    let cols = [
                        (EllaSortColumn::Node, "Node"),
                        (EllaSortColumn::Ids, "IDs"),
                        (EllaSortColumn::Channel, "Channel"),
                        (EllaSortColumn::Name, "Name"),
                        (EllaSortColumn::Duration, "Duration"),
                        (EllaSortColumn::Clients, "Clients"),
                        (EllaSortColumn::Rate, "Rate"),
                        (EllaSortColumn::Frame, "Frame"),
                        (EllaSortColumn::Stretch, "Stretch"),
                        (EllaSortColumn::Instances, "Instances"),
                        (EllaSortColumn::Errors, "Errors"),
                        (EllaSortColumn::Events, "Events"),
                    ];
                    for (col, text) in cols {
                        header.col(|ui| {
                            let label = header_label(
                                col,
                                text,
                                self.ella_sort_column,
                                self.ella_sort_ascending,
                            );
                            let mut resp = ui.button(label);
                            if col == EllaSortColumn::Clients {
                                resp = resp.on_hover_text("Peak live subscribers");
                            } else if col == EllaSortColumn::Rate {
                                resp = resp.on_hover_text(
                                    "Typical object Mbps (time-weighted mode): TX on Server, RX otherwise.",
                                );
                            } else if col == EllaSortColumn::Frame {
                                resp = resp.on_hover_text(
                                    "Byte-weighted frame Mbps: actual send on Server, recv on Relay.",
                                );
                            } else if col == EllaSortColumn::Stretch {
                                resp = resp.on_hover_text(
                                    "Fraction of frames slower than 1.25× the reference: actual vs planned on Server, recv vs wire on Relay.",
                                );
                            } else if col == EllaSortColumn::Instances {
                                resp =
                                    resp.on_hover_text("Fanout instances that touched this title.");
                            } else if col == EllaSortColumn::Errors {
                                resp = resp.on_hover_text(
                                    "CHANNEL_ERROR events for this title (all reincarnations).",
                                );
                            }
                            if resp.clicked() {
                                if self.ella_sort_column == col {
                                    self.ella_sort_ascending = !self.ella_sort_ascending;
                                } else {
                                    self.ella_sort_column = col;
                                    self.ella_sort_ascending = false;
                                }
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(text_height * 1.4, n, |mut row| {
                        let r = &self.ella_channel_list[row.index()];
                        row.set_selected(open.contains(&(r.provider_id, r.session_id)));
                        row.col(|ui| {
                            ui.label(&r.node);
                        });
                        row.col(|ui| {
                            ui.label(&r.ids_label);
                        });
                        row.col(|ui| {
                            ui.label(&r.short_label);
                        });
                        row.col(|ui| {
                            ui.label(if r.channel_name.is_empty() {
                                "-"
                            } else {
                                &r.channel_name
                            });
                        });
                        row.col(|ui| {
                            ui.label(format!("{:.1}s", r.duration_s));
                        });
                        row.col(|ui| {
                            ui.label(format!("{}", r.peak_subscribers));
                        });
                        row.col(|ui| {
                            ui.label(analysis::format_ella_rx_rate(r.rx_rate_mbps));
                        });
                        row.col(|ui| {
                            ui.label(analysis::format_ella_rx_rate(r.frame_rate_mbps));
                        });
                        row.col(|ui| {
                            ui.label(analysis::format_ella_stretch(r.stretch_frac));
                        });
                        row.col(|ui| {
                            ui.label(format!("{}", r.instance_count));
                        });
                        row.col(|ui| {
                            ui.label(format!("{}", r.error_count));
                        });
                        row.col(|ui| {
                            ui.label(format!("{}", r.event_count));
                        });
                        if row.response().clicked() {
                            selected = Some((r.provider_id, r.session_id));
                        }
                    });
                });
        });
        if let Some((pid, sid)) = selected {
            self.select_ella_channel(pid, sid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(label: &str) -> analysis::EllaNamedSeries {
        analysis::EllaNamedSeries {
            label: label.into(),
            data: vec![[0.0, 1.0]],
        }
    }

    #[test]
    fn title_colors_follow_first_stack_order() {
        let subs = analysis::EllaSubStackData {
            series: vec![named("zeta"), named("alpha")],
        };
        let rx = analysis::EllaSubStackData {
            series: vec![named("alpha"), named("zeta")],
        };
        let colors = ella_title_colors(&EllaProfile::default(), &[&subs, &rx], 1, 0);
        assert_eq!(colors.get("zeta"), Some(&series_color(0)));
        assert_eq!(colors.get("alpha"), Some(&series_color(1)));
        assert_ne!(
            title_stack_color(&colors, "alpha"),
            title_stack_color(&colors, "zeta")
        );
        assert_eq!(title_stack_color(&colors, "Other"), COLOR_STACK_OTHER);
    }

    #[test]
    fn each_node_gets_its_own_palette_range_for_the_same_title() {
        // Server and Relay draw the same title on separate stacks, so the
        // band colours have to differ to read as separate nodes.
        let subs = analysis::EllaSubStackData {
            series: vec![named("title")],
        };
        let server = ella_title_colors(&EllaProfile::default(), &[&subs], 1, 0);
        let relay = ella_title_colors(&EllaProfile::default(), &[&subs], 1, 4);
        assert_eq!(server.get("title"), Some(&series_color(0)));
        assert_eq!(relay.get("title"), Some(&series_color(4)));
    }

    #[test]
    fn merged_channel_tab_series_split_by_side_not_by_node() {
        // `Relay - RX` has to land on the internal plot even though the
        // node prefix pushed it off the bare "RX" label.
        let series = vec![
            analysis::EllaChannelSubSeries {
                label: "Relay - RX".into(),
                data: vec![[0.0, 1.0]],
            },
            analysis::EllaChannelSubSeries {
                label: "Relay - TX".into(),
                data: vec![[0.0, 2.0]],
            },
            analysis::EllaChannelSubSeries {
                label: "Server - TX".into(),
                data: vec![[0.0, 3.0]],
            },
        ];
        let (internal, external) = channel_occupancy_lines(&series);
        assert_eq!(
            internal.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
            vec!["Relay - RX"]
        );
        assert_eq!(
            external.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
            vec!["Relay - TX", "Server - TX"]
        );
        // Every line gets its own colour, across both plots.
        assert_ne!(external[0].color, external[1].color);
        assert_ne!(internal[0].color, external[0].color);
    }
}
