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

//! Per-tab plot catalogs: hide/show, order, and defaults for the sidebar.

use std::collections::{BTreeMap, BTreeSet};

/// Default stacked-plot height (logical px). Catalog entries without
/// a stored height use this, except a few compact defaults in
/// [`default_plot_height`].
pub(crate) const PLOT_H_DEFAULT: f32 = 260.0;
pub(crate) const PLOT_H_MIN: f32 = 64.0;
pub(crate) const PLOT_H_MAX: f32 = 800.0;
pub(crate) const PLOT_SIZER_H: f32 = 8.0;

/// One catalog entry. System, session, and Fanout-channel tabs each
/// use a disjoint subset.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(crate) enum PlotId {
    CpuUsage,
    Memory,
    MemoryPressure,
    Swap,
    SessionCount,
    SrttAvg,
    GoodputAvg,
    NetworkTx,
    NetworkRx,
    HttpRps,
    HttpLatency,
    TcpStates,
    ConnRate,
    TcpGoodput,
    TcpSrtt,
    TcpSend,
    TcpLro,
    FanoutServerTx,
    FanoutServerRx,
    FanoutServerFrameRate,
    FanoutServerFrameDuration,
    FanoutServerFrameRatio,
    FanoutServerErrors,
    FanoutServerTraffic,
    FanoutServerJoins,
    FanoutServerObjectTiming,
    FanoutRelaySubs,
    FanoutRelayRx,
    FanoutRelayTx,
    FanoutRelayFrameRate,
    FanoutRelayFrameDuration,
    FanoutRelayFrameRatio,
    FanoutRelayChannels,
    FanoutRelayErrors,
    FanoutRelayTraffic,
    FanoutRelayJoins,
    NginxBytes,
    NginxDiag,
    Stars,
    HptsLateness,
    HptsBatch,
    HptsSlotGateOutput,
    HptsSlotGateFires,
    HptsSlotGateChecks,
    HptsSlotGateRollover,
    Rtt,
    Rate,
    SendState,
    Sizes,
    SizeCdf,
    HttpReqLanes,
    HttpRespSize,
    HttpRespLatency,
    EllaSubscribers,
    EllaObjectRate,
    EllaObjectTiming,
    EllaObjectId,
    EllaFrameId,
    EllaObjectDuration,
    EllaObjectSize,
    EllaFrameRate,
    EllaFrameRateRelay,
    EllaFrameDuration,
    EllaFrameDurationRelay,
    EllaFrameRatio,
    EllaFrameRatioRelay,
    EllaErrors,
    OriginSegments,
}

impl PlotId {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CpuUsage => "CPU Usage",
            Self::Memory => "Memory",
            Self::MemoryPressure => "Memory Pressure",
            Self::Swap => "Swap",
            Self::SessionCount => "TCP Session Count",
            Self::SrttAvg => "TCP Average RTT",
            Self::GoodputAvg => "TCP Average Goodput",
            Self::NetworkTx => "Network TX",
            Self::NetworkRx => "Network RX",
            Self::HttpRps => "HTTP Responses Per Second",
            Self::HttpLatency => "HTTP Duration",
            Self::TcpStates => "TCP Connections by State",
            Self::ConnRate => "TCP Connection Rate",
            Self::TcpGoodput => "TCP Goodput",
            Self::TcpSrtt => "TCP Smoothed RTT",
            Self::TcpSend => "TCP Send Size",
            Self::TcpLro => "TCP LRO Flush",
            Self::FanoutServerTx => "Fanout Server TX",
            Self::FanoutServerRx => "Fanout Server RX",
            Self::FanoutServerFrameRate => "Fanout Server Frame Send",
            Self::FanoutServerFrameDuration => "Fanout Server Frame Duration",
            Self::FanoutServerFrameRatio => "Fanout Server Frame Ratio",
            Self::FanoutServerErrors => "Fanout Server Errors",
            Self::FanoutServerTraffic => "Fanout Server Traffic",
            Self::FanoutServerJoins => "Fanout Server Joins",
            Self::FanoutServerObjectTiming => "Fanout Server Object Timing",
            Self::FanoutRelaySubs => "Fanout Relay Subscribers",
            Self::FanoutRelayRx => "Fanout Relay RX",
            Self::FanoutRelayTx => "Fanout Relay TX",
            Self::FanoutRelayFrameRate => "Fanout Relay Frame Send",
            Self::FanoutRelayFrameDuration => "Fanout Relay Frame Duration",
            Self::FanoutRelayFrameRatio => "Fanout Relay Frame Ratio",
            Self::FanoutRelayChannels => "Fanout Relay Channels",
            Self::FanoutRelayErrors => "Fanout Relay Errors",
            Self::FanoutRelayTraffic => "Fanout Relay Traffic",
            Self::FanoutRelayJoins => "Fanout Relay Joins",
            Self::NginxBytes => "HTTP Completion Bytes",
            Self::NginxDiag => "nginx Problem Events",
            Self::Stars => "STARS",
            Self::HptsLateness => "HPTS Callout Lateness",
            Self::HptsBatch => "HPTS Batch Size",
            Self::HptsSlotGateOutput => "Slot Gate Output",
            Self::HptsSlotGateFires => "Slot Gate Fires",
            Self::HptsSlotGateChecks => "Slot Gate Checks",
            Self::HptsSlotGateRollover => "Slot Gate Rollover",
            Self::Rtt => "RTT",
            Self::Rate => "Rate",
            Self::SendState => "Send State",
            Self::Sizes => "Sizes",
            Self::SizeCdf => "Size CDF",
            Self::HttpReqLanes => "HTTP Requests",
            Self::HttpRespSize => "HTTP Response Size",
            Self::HttpRespLatency => "HTTP Duration",
            Self::EllaSubscribers => "Subscribers",
            Self::EllaObjectRate => "Object Rate",
            Self::EllaObjectTiming => "Object Timing",
            Self::EllaObjectId => "Object Timeline",
            Self::EllaFrameId => "Frame Timeline",
            Self::EllaObjectDuration => "Object Duration",
            Self::EllaObjectSize => "Object Size",
            Self::EllaFrameRate => "Frame Rate",
            Self::EllaFrameRateRelay => "Relay Frame Rate",
            Self::EllaFrameDuration => "Frame Duration",
            Self::EllaFrameDurationRelay => "Relay Frame Duration",
            Self::EllaFrameRatio => "Duration Ratio",
            Self::EllaFrameRatioRelay => "Relay Duration Ratio",
            Self::EllaErrors => "Errors",
            Self::OriginSegments => "Origin Segment Rate",
        }
    }

    /// Stable sizer / maximize id when this catalog row draws one plot.
    pub(crate) fn chrome_slot(self) -> &'static str {
        match self {
            Self::CpuUsage => "cpu_usage",
            Self::Memory => "mem_states_plot",
            Self::MemoryPressure => "mem_pressure_plot",
            Self::Swap => "mem_swap_plot",
            Self::SessionCount => "agg_session_count_plot",
            Self::SrttAvg => "agg_srtt_plot",
            Self::GoodputAvg => "agg_goodput_plot",
            Self::NetworkTx => "network_tx",
            Self::NetworkRx => "network_rx",
            Self::HttpRps => "http_rps_plot",
            Self::HttpLatency => "http_latency_plot",
            Self::TcpStates => "agg_tcp_states_plot",
            Self::ConnRate => "agg_conn_rate_plot",
            Self::TcpGoodput => "tcp_gp_stack",
            Self::TcpSrtt => "tcp_srtt_stack",
            Self::TcpSend => "tcp_send_stack",
            Self::TcpLro => "tcp_lro_flush_stack",
            Self::FanoutServerTx => "fanout_server_tx",
            Self::FanoutServerRx => "fanout_server_rx",
            Self::FanoutServerFrameRate => "fanout_server_frame_rate",
            Self::FanoutServerFrameDuration => "fanout_server_frame_duration",
            Self::FanoutServerFrameRatio => "fanout_server_frame_ratio",
            Self::FanoutServerErrors => "fanout_server_errors",
            Self::FanoutServerTraffic => "fanout_server_traffic",
            Self::FanoutServerJoins => "fanout_server_joins",
            Self::FanoutServerObjectTiming => "fanout_server_object_timing",
            Self::FanoutRelaySubs => "fanout_relay_subs",
            Self::FanoutRelayRx => "fanout_relay_rx",
            Self::FanoutRelayTx => "fanout_relay_tx",
            Self::FanoutRelayFrameRate => "fanout_relay_frame_rate",
            Self::FanoutRelayFrameDuration => "fanout_relay_frame_duration",
            Self::FanoutRelayFrameRatio => "fanout_relay_frame_ratio",
            Self::FanoutRelayChannels => "fanout_relay_channels",
            Self::FanoutRelayErrors => "fanout_relay_errors",
            Self::FanoutRelayTraffic => "fanout_relay_traffic",
            Self::FanoutRelayJoins => "fanout_relay_joins",
            Self::NginxBytes => "nginx_bps_plot",
            Self::NginxDiag => "nginx_diag_plot",
            Self::Stars => "stars_pressure_plot",
            Self::HptsLateness => "hpts_lateness_stack",
            Self::HptsBatch => "hpts_batch_stack",
            Self::HptsSlotGateOutput => "hpts_slot_gate_bytes",
            Self::HptsSlotGateFires => "hpts_slot_gate_fires",
            Self::HptsSlotGateChecks => "hpts_slot_gate_walks",
            Self::HptsSlotGateRollover => "hpts_slot_gate_rollover",
            Self::Rtt => "session_rtt",
            Self::Rate => "session_rate",
            Self::SendState => "session_send_state",
            Self::Sizes => "session_sizes",
            Self::SizeCdf => "session_size_cdf",
            Self::HttpReqLanes => "session_http_req",
            Self::HttpRespSize => "session_http_resp_size",
            Self::HttpRespLatency => "session_http_latency",
            Self::EllaSubscribers => "ella_ch_subscribers",
            Self::EllaObjectRate => "ella_ch_object_rate",
            Self::EllaObjectTiming => "ella_ch_object_timing",
            Self::EllaObjectId => "ella_ch_object_timeline",
            Self::EllaFrameId => "ella_ch_frame_timeline",
            Self::EllaObjectDuration => "ella_ch_object_duration",
            Self::EllaObjectSize => "ella_ch_object_size",
            Self::EllaFrameRate => "ella_ch_frame_rate",
            Self::EllaFrameRateRelay => "ella_ch_frame_rate_relay",
            Self::EllaFrameDuration => "ella_ch_frame_duration",
            Self::EllaFrameDurationRelay => "ella_ch_frame_duration_relay",
            Self::EllaFrameRatio => "ella_ch_frame_ratio",
            Self::EllaFrameRatioRelay => "ella_ch_frame_ratio_relay",
            Self::EllaErrors => "ella_ch_errors",
            Self::OriginSegments => "origin_segments_plot",
        }
    }

    /// Catalog id for a `chrome_slot` name (share-URL `plot=` param).
    pub(crate) fn from_chrome_slot(s: &str) -> Option<Self> {
        Some(match s {
            "origin_segments_plot" => Self::OriginSegments,
            "cpu_usage" => Self::CpuUsage,
            "mem_states_plot" => Self::Memory,
            "mem_pressure_plot" => Self::MemoryPressure,
            "mem_swap_plot" => Self::Swap,
            "agg_session_count_plot" => Self::SessionCount,
            "agg_srtt_plot" => Self::SrttAvg,
            "agg_goodput_plot" => Self::GoodputAvg,
            "network_tx" => Self::NetworkTx,
            "network_rx" => Self::NetworkRx,
            "http_rps_plot" => Self::HttpRps,
            "http_latency_plot" => Self::HttpLatency,
            "agg_tcp_states_plot" => Self::TcpStates,
            "agg_conn_rate_plot" => Self::ConnRate,
            "tcp_gp_stack" => Self::TcpGoodput,
            "tcp_srtt_stack" => Self::TcpSrtt,
            "tcp_send_stack" => Self::TcpSend,
            "tcp_lro_flush_stack" => Self::TcpLro,
            "fanout_server_tx" => Self::FanoutServerTx,
            "fanout_server_rx" => Self::FanoutServerRx,
            "fanout_server_frame_rate" => Self::FanoutServerFrameRate,
            "fanout_server_frame_duration" => Self::FanoutServerFrameDuration,
            "fanout_server_frame_ratio" => Self::FanoutServerFrameRatio,
            "fanout_server_errors" => Self::FanoutServerErrors,
            "fanout_server_traffic" => Self::FanoutServerTraffic,
            "fanout_server_joins" => Self::FanoutServerJoins,
            "fanout_server_object_timing" => Self::FanoutServerObjectTiming,
            "fanout_relay_subs" => Self::FanoutRelaySubs,
            "fanout_relay_rx" => Self::FanoutRelayRx,
            "fanout_relay_tx" => Self::FanoutRelayTx,
            "fanout_relay_frame_rate" => Self::FanoutRelayFrameRate,
            "fanout_relay_frame_duration" => Self::FanoutRelayFrameDuration,
            "fanout_relay_frame_ratio" => Self::FanoutRelayFrameRatio,
            "fanout_relay_channels" => Self::FanoutRelayChannels,
            "fanout_relay_errors" => Self::FanoutRelayErrors,
            "fanout_relay_traffic" => Self::FanoutRelayTraffic,
            "fanout_relay_joins" => Self::FanoutRelayJoins,
            "nginx_bps_plot" => Self::NginxBytes,
            "nginx_diag_plot" => Self::NginxDiag,
            "stars_pressure_plot" => Self::Stars,
            "hpts_lateness_stack" => Self::HptsLateness,
            "hpts_batch_stack" => Self::HptsBatch,
            "hpts_slot_gate_bytes" => Self::HptsSlotGateOutput,
            "hpts_slot_gate_fires" => Self::HptsSlotGateFires,
            "hpts_slot_gate_walks" => Self::HptsSlotGateChecks,
            "hpts_slot_gate_rollover" => Self::HptsSlotGateRollover,
            "session_rtt" => Self::Rtt,
            "session_rate" => Self::Rate,
            "session_send_state" => Self::SendState,
            "session_sizes" => Self::Sizes,
            "session_size_cdf" => Self::SizeCdf,
            "session_http_req" => Self::HttpReqLanes,
            "session_http_resp_size" => Self::HttpRespSize,
            "session_http_latency" => Self::HttpRespLatency,
            "ella_ch_subscribers" => Self::EllaSubscribers,
            "ella_ch_object_rate" => Self::EllaObjectRate,
            "ella_ch_object_timing" => Self::EllaObjectTiming,
            "ella_ch_object_timeline" => Self::EllaObjectId,
            "ella_ch_frame_timeline" => Self::EllaFrameId,
            "ella_ch_object_duration" => Self::EllaObjectDuration,
            "ella_ch_object_size" => Self::EllaObjectSize,
            "ella_ch_frame_rate" => Self::EllaFrameRate,
            "ella_ch_frame_rate_relay" => Self::EllaFrameRateRelay,
            "ella_ch_frame_duration" => Self::EllaFrameDuration,
            "ella_ch_frame_duration_relay" => Self::EllaFrameDurationRelay,
            "ella_ch_frame_ratio" => Self::EllaFrameRatio,
            "ella_ch_frame_ratio_relay" => Self::EllaFrameRatioRelay,
            "ella_ch_errors" => Self::EllaErrors,
            _ => return None,
        })
    }

    /// This catalog row draws several `Plot` widgets and chromes each
    /// one itself (`finish_plot_slot` per widget).
    pub(crate) fn self_chromes_plots(self) -> bool {
        matches!(
            self,
            Self::FanoutServerTx
                | Self::FanoutServerRx
                | Self::FanoutServerFrameRate
                | Self::FanoutServerFrameDuration
                | Self::FanoutServerFrameRatio
                | Self::FanoutServerErrors
                | Self::FanoutServerTraffic
                | Self::FanoutServerJoins
                | Self::FanoutServerObjectTiming
                | Self::FanoutRelaySubs
                | Self::FanoutRelayRx
                | Self::FanoutRelayTx
                | Self::FanoutRelayFrameRate
                | Self::FanoutRelayFrameDuration
                | Self::FanoutRelayFrameRatio
                | Self::FanoutRelayChannels
                | Self::FanoutRelayErrors
                | Self::FanoutRelayTraffic
                | Self::FanoutRelayJoins
                | Self::Stars
        )
    }

    /// `(is_server, plot)` for the split System-tab Fanout catalog rows.
    pub(crate) fn fanout_split(self) -> Option<(bool, FanoutPlot)> {
        use FanoutPlot::*;
        Some(match self {
            Self::FanoutServerTx => (true, Tx),
            Self::FanoutServerRx => (true, Rx),
            Self::FanoutServerFrameRate => (true, FrameRate),
            Self::FanoutServerFrameDuration => (true, FrameDuration),
            Self::FanoutServerFrameRatio => (true, FrameRatio),
            Self::FanoutServerErrors => (true, Errors),
            Self::FanoutServerTraffic => (true, Traffic),
            Self::FanoutServerJoins => (true, Joins),
            Self::FanoutServerObjectTiming => (true, ObjectTiming),
            Self::FanoutRelaySubs => (false, Subscribers),
            Self::FanoutRelayRx => (false, Rx),
            Self::FanoutRelayTx => (false, Tx),
            Self::FanoutRelayFrameRate => (false, FrameRate),
            Self::FanoutRelayFrameDuration => (false, FrameDuration),
            Self::FanoutRelayFrameRatio => (false, FrameRatio),
            Self::FanoutRelayChannels => (false, Channels),
            Self::FanoutRelayErrors => (false, Errors),
            Self::FanoutRelayTraffic => (false, Traffic),
            Self::FanoutRelayJoins => (false, Joins),
            _ => return None,
        })
    }

    /// Session stacked-panel index 0..=4, when this id is a session plot.
    pub(crate) fn session_index(self) -> Option<usize> {
        match self {
            Self::Rtt => Some(0),
            Self::Rate => Some(1),
            Self::SendState => Some(2),
            Self::Sizes => Some(3),
            Self::SizeCdf => Some(4),
            _ => None,
        }
    }

    pub(crate) fn from_session_index(i: usize) -> Option<Self> {
        match i {
            0 => Some(Self::Rtt),
            1 => Some(Self::Rate),
            2 => Some(Self::SendState),
            3 => Some(Self::Sizes),
            4 => Some(Self::SizeCdf),
            _ => None,
        }
    }

    pub(crate) fn is_session_time_series(self) -> bool {
        matches!(
            self,
            Self::Rtt
                | Self::Rate
                | Self::SendState
                | Self::Sizes
                | Self::HttpReqLanes
                | Self::HttpRespSize
                | Self::HttpRespLatency
                | Self::CpuUsage
                | Self::NetworkTx
                | Self::NetworkRx
        )
    }
}

/// One System-tab Fanout plot kind. Server rows omit Subscribers and
/// Channels (a co-located relay is the server's only subscriber, so
/// those read as noise there).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FanoutPlot {
    Subscribers,
    Rx,
    Tx,
    Traffic,
    Joins,
    ObjectTiming,
    FrameRate,
    FrameDuration,
    FrameRatio,
    Channels,
    Errors,
}

/// Session panel indices to draw: catalog order, minus hidden and empty.
pub(crate) fn session_visible_order(layout: &PlotLayout, has_data: [bool; 5]) -> Vec<usize> {
    layout
        .order
        .iter()
        .filter_map(|id| {
            let i = id.session_index()?;
            if !layout.is_visible(*id) || !has_data[i] {
                return None;
            }
            if layout.maximized.is_some_and(|m| m != *id) {
                return None;
            }
            Some(i)
        })
        .collect()
}

/// Data-presence flags gating the non-stacked session catalog rows.
#[derive(Clone, Copy, Default)]
pub(crate) struct SessionCatalogData {
    pub has_http: bool,
    pub has_http_size: bool,
    pub has_http_latency: bool,
    pub has_cpu: bool,
    pub has_if: bool,
}

/// Session catalog ids that should draw this frame.
pub(crate) fn session_catalog_shown(
    layout: &PlotLayout,
    has_data: [bool; 5],
    flags: SessionCatalogData,
) -> Vec<PlotId> {
    layout
        .shown_ids()
        .into_iter()
        .filter(|&id| {
            if layout.maximized.is_some_and(|m| m != id) {
                return false;
            }
            if let Some(i) = id.session_index() {
                return has_data[i];
            }
            match id {
                PlotId::HttpReqLanes => flags.has_http,
                PlotId::HttpRespSize => flags.has_http_size,
                PlotId::HttpRespLatency => flags.has_http_latency,
                PlotId::CpuUsage => flags.has_cpu,
                PlotId::NetworkTx | PlotId::NetworkRx => flags.has_if,
                _ => false,
            }
        })
        .collect()
}

pub(crate) fn default_plot_height(id: PlotId) -> f32 {
    match id {
        PlotId::HttpReqLanes => 90.0,
        PlotId::CpuUsage => 140.0,
        PlotId::Memory => PLOT_H_DEFAULT * 0.8,
        PlotId::SizeCdf => 200.0,
        PlotId::EllaObjectId
        | PlotId::EllaFrameId
        | PlotId::EllaObjectDuration
        | PlotId::EllaObjectSize => PLOT_H_DEFAULT * 0.5,
        _ => PLOT_H_DEFAULT,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlotCatalogKind {
    System,
    Session,
    EllaChannel,
    Flame,
    Processes,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct PlotLayout {
    pub order: Vec<PlotId>,
    pub hidden: BTreeSet<PlotId>,
    /// Catalog-order prefix that stays pinned above the tab's scroll.
    /// Locking a plot also locks every plot above it; unlocking clears
    /// this plot and every plot after it.
    pub locked: BTreeSet<PlotId>,
    /// Per-plot height in logical px. Missing keys use
    /// [`default_plot_height`]. Independent: a sizer under a plot
    /// changes only that plot.
    pub heights: BTreeMap<PlotId, u16>,
    /// When set, only this catalog id is drawn (full remaining height).
    pub maximized: Option<PlotId>,
    /// Unique widget slot for the maximized plot. Required when a
    /// catalog row draws more than one `Plot` (Fanout, STARS, HPTS).
    pub maximized_slot: Option<String>,
    /// Per-widget heights for nested plots. Missing keys use the
    /// catalog row's [`Self::plot_height`].
    pub slot_heights: BTreeMap<String, u16>,
}

impl PlotLayout {
    pub(crate) fn is_visible(&self, id: PlotId) -> bool {
        !self.hidden.contains(&id)
    }

    pub(crate) fn toggle_hidden(&mut self, id: PlotId) {
        if !self.hidden.remove(&id) {
            self.hidden.insert(id);
        }
    }

    /// Move `id` so it sits at `to` in `order` (clamped).
    pub(crate) fn move_id(&mut self, id: PlotId, to: usize) {
        let Some(from) = self.order.iter().position(|&p| p == id) else {
            return;
        };
        let to = to.min(self.order.len().saturating_sub(1));
        if from == to {
            return;
        }
        let item = self.order.remove(from);
        self.order.insert(to, item);
        self.clamp_locked_prefix();
    }

    pub(crate) fn is_locked(&self, id: PlotId) -> bool {
        self.locked.contains(&id)
    }

    pub(crate) fn plot_height(&self, id: PlotId) -> f32 {
        self.heights
            .get(&id)
            .copied()
            .map(|h| h as f32)
            .unwrap_or_else(|| default_plot_height(id))
    }

    /// Height used this frame: stored px, or remaining space when
    /// this id is maximized. Titles and other chrome above a plot
    /// should already have been drawn so `remaining` is the leftover.
    pub(crate) fn draw_height(&self, id: PlotId, remaining: f32) -> f32 {
        if self.fills_catalog_row(id) {
            remaining.max(PLOT_H_MIN)
        } else {
            self.plot_height(id)
        }
    }

    /// Catalog-loop fill: a single-plot row whose slot is maximized.
    pub(crate) fn fills_catalog_row(&self, id: PlotId) -> bool {
        if self.maximized != Some(id) {
            return false;
        }
        match &self.maximized_slot {
            None => true,
            Some(slot) => !id.self_chromes_plots() && slot == id.chrome_slot(),
        }
    }

    pub(crate) fn slot_is_shown(&self, slot: &str) -> bool {
        self.maximized_slot.as_deref().is_none_or(|m| m == slot)
    }

    pub(crate) fn slot_height(&self, slot: &str, id: PlotId) -> f32 {
        self.slot_heights
            .get(slot)
            .copied()
            .map(|h| h as f32)
            .unwrap_or_else(|| self.plot_height(id))
    }

    pub(crate) fn set_slot_height(&mut self, slot: &str, h: f32) {
        let h = h.round().clamp(PLOT_H_MIN, PLOT_H_MAX) as u16;
        self.slot_heights.insert(slot.to_string(), h);
    }

    pub(crate) fn toggle_slot_maximize(&mut self, id: PlotId, slot: &str) {
        if self.maximized_slot.as_deref() == Some(slot) {
            self.clear_maximize();
        } else {
            self.maximized = Some(id);
            self.maximized_slot = Some(slot.to_string());
        }
    }

    pub(crate) fn clear_maximize(&mut self) {
        self.maximized = None;
        self.maximized_slot = None;
    }

    pub(crate) fn set_plot_height(&mut self, id: PlotId, h: f32) {
        let h = h.round().clamp(PLOT_H_MIN, PLOT_H_MAX) as u16;
        self.heights.insert(id, h);
    }

    /// Lock `id` and every plot above it, or unlock it and every plot after it.
    pub(crate) fn toggle_lock(&mut self, id: PlotId) {
        let Some(i) = self.order.iter().position(|&p| p == id) else {
            return;
        };
        if self.locked.contains(&id) {
            for p in &self.order[i..] {
                self.locked.remove(p);
            }
            return;
        }
        for p in &self.order[..=i] {
            self.locked.insert(*p);
        }
    }

    /// Drop locks that are no longer a leading run of `order`.
    pub(crate) fn clamp_locked_prefix(&mut self) {
        let mut in_prefix = true;
        for &id in &self.order {
            if !in_prefix {
                self.locked.remove(&id);
            } else if !self.locked.contains(&id) {
                in_prefix = false;
            }
        }
    }

    /// Catalog ids that are shown (not hidden). Empty series still skip
    /// drawing at the call site.
    pub(crate) fn shown_ids(&self) -> Vec<PlotId> {
        self.order
            .iter()
            .copied()
            .filter(|id| self.is_visible(*id))
            .collect()
    }

    /// Split `shown` into the locked prefix and the scrolling remainder.
    /// `shown` must be in catalog order. A locked plot with no data is
    /// omitted by the caller before this split, so the first remaining
    /// locked id still pins.
    pub(crate) fn split_pinned(&self, shown: &[PlotId]) -> (Vec<PlotId>, Vec<PlotId>) {
        let mut pinned = Vec::new();
        for &id in shown {
            if self.is_locked(id) {
                pinned.push(id);
            } else {
                break;
            }
        }
        let rest = shown[pinned.len()..].to_vec();
        (pinned, rest)
    }
}

pub(crate) fn catalog(kind: PlotCatalogKind) -> &'static [PlotId] {
    match kind {
        PlotCatalogKind::System => SYSTEM_ORDER,
        PlotCatalogKind::Session => SESSION_ORDER,
        PlotCatalogKind::EllaChannel => ELLA_ORDER,
        PlotCatalogKind::Flame => FLAME_ORDER,
        PlotCatalogKind::Processes => FLAME_ORDER,
    }
}

pub(crate) fn default_layout(kind: PlotCatalogKind) -> PlotLayout {
    PlotLayout {
        order: catalog(kind).to_vec(),
        hidden: match kind {
            PlotCatalogKind::System => SYSTEM_HIDDEN.iter().copied().collect(),
            PlotCatalogKind::Session => SESSION_HIDDEN.iter().copied().collect(),
            PlotCatalogKind::Flame | PlotCatalogKind::Processes => {
                FLAME_HIDDEN.iter().copied().collect()
            }
            _ => BTreeSet::new(),
        },
        locked: BTreeSet::new(),
        heights: BTreeMap::new(),
        maximized: None,
        maximized_slot: None,
        slot_heights: BTreeMap::new(),
    }
}

const FLAME_ORDER: &[PlotId] = &[PlotId::CpuUsage, PlotId::NetworkTx, PlotId::NetworkRx];

const FLAME_HIDDEN: &[PlotId] = &[PlotId::NetworkTx, PlotId::NetworkRx];

const SYSTEM_ORDER: &[PlotId] = &[
    PlotId::CpuUsage,
    PlotId::Memory,
    PlotId::MemoryPressure,
    PlotId::Swap,
    PlotId::OriginSegments,
    PlotId::NetworkTx,
    PlotId::NetworkRx,
    PlotId::HttpRps,
    PlotId::TcpStates,
    PlotId::ConnRate,
    PlotId::HttpLatency,
    PlotId::TcpGoodput,
    PlotId::TcpSrtt,
    PlotId::SrttAvg,
    PlotId::GoodputAvg,
    PlotId::TcpSend,
    PlotId::TcpLro,
    PlotId::SessionCount,
    PlotId::FanoutServerTx,
    PlotId::FanoutServerRx,
    PlotId::FanoutServerTraffic,
    PlotId::FanoutServerJoins,
    PlotId::FanoutServerFrameRate,
    PlotId::FanoutServerFrameDuration,
    PlotId::FanoutServerFrameRatio,
    PlotId::FanoutServerObjectTiming,
    PlotId::FanoutServerErrors,
    PlotId::FanoutRelaySubs,
    PlotId::FanoutRelayRx,
    PlotId::FanoutRelayTx,
    PlotId::FanoutRelayTraffic,
    PlotId::FanoutRelayJoins,
    PlotId::FanoutRelayFrameRate,
    PlotId::FanoutRelayFrameDuration,
    PlotId::FanoutRelayFrameRatio,
    PlotId::FanoutRelayChannels,
    PlotId::FanoutRelayErrors,
    PlotId::NginxBytes,
    PlotId::NginxDiag,
    PlotId::Stars,
    PlotId::HptsLateness,
    PlotId::HptsBatch,
    PlotId::HptsSlotGateOutput,
    PlotId::HptsSlotGateFires,
    PlotId::HptsSlotGateChecks,
    PlotId::HptsSlotGateRollover,
];

const SYSTEM_HIDDEN: &[PlotId] = &[
    PlotId::Memory,
    PlotId::MemoryPressure,
    PlotId::Swap,
    PlotId::NetworkRx,
    PlotId::ConnRate,
    PlotId::HttpRps,
    PlotId::HttpLatency,
    PlotId::TcpGoodput,
    PlotId::TcpSrtt,
    PlotId::SrttAvg,
    PlotId::GoodputAvg,
    PlotId::TcpSend,
    PlotId::TcpLro,
    PlotId::SessionCount,
    PlotId::FanoutServerRx,
    PlotId::FanoutServerFrameRate,
    PlotId::FanoutServerFrameDuration,
    PlotId::FanoutServerFrameRatio,
    PlotId::FanoutServerErrors,
    PlotId::FanoutRelayTx,
    PlotId::FanoutRelayFrameRate,
    PlotId::FanoutRelayFrameDuration,
    PlotId::FanoutRelayFrameRatio,
    PlotId::FanoutRelayChannels,
    PlotId::FanoutRelayErrors,
    PlotId::NginxBytes,
    PlotId::Stars,
    PlotId::HptsLateness,
    PlotId::HptsBatch,
    PlotId::HptsSlotGateOutput,
    PlotId::HptsSlotGateFires,
    PlotId::HptsSlotGateChecks,
    PlotId::HptsSlotGateRollover,
];

const SESSION_ORDER: &[PlotId] = &[
    PlotId::CpuUsage,
    PlotId::NetworkTx,
    PlotId::NetworkRx,
    PlotId::HttpReqLanes,
    PlotId::HttpRespSize,
    PlotId::HttpRespLatency,
    PlotId::Rtt,
    PlotId::Rate,
    PlotId::SendState,
    PlotId::Sizes,
    PlotId::SizeCdf,
];

const SESSION_HIDDEN: &[PlotId] = &[
    PlotId::CpuUsage,
    PlotId::NetworkTx,
    PlotId::NetworkRx,
    PlotId::HttpReqLanes,
    PlotId::HttpRespSize,
    PlotId::HttpRespLatency,
];

const ELLA_ORDER: &[PlotId] = &[
    PlotId::EllaSubscribers,
    PlotId::EllaObjectRate,
    PlotId::EllaObjectTiming,
    PlotId::EllaObjectId,
    PlotId::EllaFrameId,
    PlotId::EllaObjectDuration,
    PlotId::EllaObjectSize,
    PlotId::EllaFrameRate,
    PlotId::EllaFrameRateRelay,
    PlotId::EllaFrameDuration,
    PlotId::EllaFrameDurationRelay,
    PlotId::EllaFrameRatio,
    PlotId::EllaFrameRatioRelay,
    PlotId::EllaErrors,
];

/// Plot widget ids that share the System tab time axis.
pub(crate) const SYSTEM_TIME_PLOT_IDS: &[&str] = &[
    "agg_session_count_plot",
    "agg_srtt_avg_plot",
    "agg_goodput_avg_plot",
    "agg_tcp_states_plot",
    "agg_conn_rate_plot",
    "if_tx_plot",
    "if_rx_plot",
    "if_single_tx_plot",
    "if_single_rx_plot",
    "http_rps_plot",
    "http_latency_plot",
    "tcp_gp_stack",
    "tcp_srtt_stack",
    "tcp_send_stack",
    "tcp_lro_flush_stack",
    "ella_subs_plot",
    "ella_up_chans_plot",
    "ella_dn_chans_plot",
    "ella_errors_plot",
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
    "nginx_bps_plot",
    "nginx_diag_plot",
    "stars_pressure_plot",
    "stars_budget_plot",
    "hpts_lateness_stack",
    "hpts_batch_stack",
    "hpts_slot_gate_bytes",
    "hpts_slot_gate_fires",
    "hpts_slot_gate_walks",
    "hpts_slot_gate_rollover",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> PlotLayout {
        default_layout(PlotCatalogKind::Session)
    }

    #[test]
    fn locking_a_plot_locks_everything_above() {
        let mut l = session();
        l.toggle_lock(PlotId::Rate);
        assert!(l.is_locked(PlotId::CpuUsage));
        assert!(l.is_locked(PlotId::NetworkTx));
        assert!(l.is_locked(PlotId::NetworkRx));
        assert!(l.is_locked(PlotId::HttpReqLanes));
        assert!(l.is_locked(PlotId::Rtt));
        assert!(l.is_locked(PlotId::Rate));
        assert!(!l.is_locked(PlotId::SendState));
    }

    #[test]
    fn unlock_clears_everything_after() {
        let mut l = session();
        l.toggle_lock(PlotId::Rtt);
        l.toggle_lock(PlotId::Rate);
        l.toggle_lock(PlotId::SendState);
        l.toggle_lock(PlotId::Rate);
        assert!(l.is_locked(PlotId::Rtt));
        assert!(!l.is_locked(PlotId::Rate));
        assert!(!l.is_locked(PlotId::SendState));
    }

    #[test]
    fn moving_a_locked_plot_off_the_prefix_drops_the_lock() {
        let mut l = session();
        l.toggle_lock(PlotId::Rtt);
        l.move_id(PlotId::SizeCdf, 0);
        assert!(!l.is_locked(PlotId::Rtt));
        assert!(!l.is_locked(PlotId::SizeCdf));
    }

    #[test]
    fn split_pinned_stops_at_the_first_unlocked() {
        let mut l = session();
        l.toggle_lock(PlotId::Rate);
        let shown = l.shown_ids();
        let (pin, rest) = l.split_pinned(&shown);
        assert_eq!(pin, vec![PlotId::Rtt, PlotId::Rate]);
        assert_eq!(rest[0], PlotId::SendState);
    }

    #[test]
    fn sizer_changes_only_that_plot() {
        let mut l = session();
        let rtt0 = l.plot_height(PlotId::Rtt);
        l.set_plot_height(PlotId::HttpReqLanes, 200.0);
        assert!((l.plot_height(PlotId::HttpReqLanes) - 200.0).abs() < 0.5);
        assert!((l.plot_height(PlotId::Rtt) - rtt0).abs() < 0.5);
    }

    #[test]
    fn session_defaults_system_plots_first_and_hidden() {
        let l = session();
        assert_eq!(
            &l.order[..3],
            &[PlotId::CpuUsage, PlotId::NetworkTx, PlotId::NetworkRx,]
        );
        assert_eq!(l.order[3], PlotId::HttpReqLanes);
        assert_eq!(l.order[10], PlotId::SizeCdf);
        for id in [PlotId::CpuUsage, PlotId::NetworkTx, PlotId::NetworkRx] {
            assert!(!l.is_visible(id));
            assert!(!l.is_locked(id));
        }
        assert!(!l.is_visible(PlotId::HttpReqLanes));
        assert!(!l.is_visible(PlotId::HttpRespSize));
        assert!(!l.is_visible(PlotId::HttpRespLatency));
        assert!(!l.is_locked(PlotId::HttpReqLanes));
    }

    #[test]
    fn slot_maximize_hides_sibling_slots() {
        let mut l = default_layout(PlotCatalogKind::System);
        l.toggle_slot_maximize(PlotId::Stars, "stars_pressure_plot");
        assert_eq!(l.maximized, Some(PlotId::Stars));
        assert!(l.slot_is_shown("stars_pressure_plot"));
        assert!(!l.slot_is_shown("stars_budget_plot"));
        assert!(PlotId::Stars.self_chromes_plots());
        assert!(!l.fills_catalog_row(PlotId::Stars));
        l.clear_maximize();
        assert!(l.slot_is_shown("stars_budget_plot"));
    }

    #[test]
    fn system_defaults_hide_network_rx_and_conn_rate() {
        let l = default_layout(PlotCatalogKind::System);
        assert!(!l.is_visible(PlotId::Memory));
        assert!(!l.is_visible(PlotId::MemoryPressure));
        assert!(!l.is_visible(PlotId::Swap));
        assert!(l.is_visible(PlotId::NetworkTx));
        assert!(!l.is_visible(PlotId::NetworkRx));
        let rps = l
            .order
            .iter()
            .position(|id| *id == PlotId::HttpRps)
            .unwrap();
        let states = l
            .order
            .iter()
            .position(|id| *id == PlotId::TcpStates)
            .unwrap();
        assert!(rps < states);
        assert!(l.is_visible(PlotId::TcpStates));
        assert!(!l.is_visible(PlotId::ConnRate));
        assert!(l.is_visible(PlotId::FanoutServerTx));
        assert!(!l.is_visible(PlotId::FanoutServerRx));
        assert!(l.is_visible(PlotId::FanoutRelaySubs));
        assert!(l.is_visible(PlotId::FanoutRelayRx));
        assert!(!l.is_visible(PlotId::FanoutRelayTx));
        assert!(!l.is_visible(PlotId::FanoutRelayChannels));
        for id in [
            PlotId::HptsSlotGateOutput,
            PlotId::HptsSlotGateFires,
            PlotId::HptsSlotGateChecks,
            PlotId::HptsSlotGateRollover,
        ] {
            assert!(!l.is_visible(id));
        }
    }
}
