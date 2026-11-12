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

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

pub const COLOR_RAW_RTT: Color32 = Color32::from_rgb(0xFF, 0x98, 0x00);
pub const COLOR_SRTT: Color32 = Color32::from_rgb(0x00, 0xBC, 0xD4);
pub const COLOR_PATH_RTT: Color32 = Color32::from_rgb(0xE9, 0x1E, 0x63);
pub const COLOR_MIN_RTT: Color32 = Color32::from_rgb(0x79, 0x55, 0x48);
pub const COLOR_GP_FILTER_MIN: Color32 = Color32::from_rgb(0x8B, 0xC3, 0x4A);
pub const COLOR_GP_SRTT: Color32 = Color32::from_rgb(0x9C, 0x27, 0xB0);
pub const COLOR_KERNEL_GOODPUT: Color32 = Color32::from_rgb(0xFF, 0x6F, 0x00);
pub const COLOR_PACING_RATE: Color32 = Color32::from_rgb(0x7B, 0x1F, 0xA2);
pub const COLOR_CWND: Color32 = Color32::from_rgb(0x21, 0x96, 0xF3);
pub const COLOR_SSTHRESH: Color32 = Color32::from_rgb(0xF4, 0x43, 0x36);
pub const COLOR_INFLIGHT: Color32 = Color32::from_rgb(0x4C, 0xAF, 0x50);
pub const COLOR_RECOVERY_FILL: Color32 = Color32::from_rgba_premultiplied(70, 0, 0, 70);
pub const COLOR_RECOVERY_STROKE: Color32 = Color32::from_rgba_premultiplied(110, 0, 0, 110);
// Brighter, fully opaque variant for the tooltip indicator dot; the alpha-blended
// shading colors above are tuned for the plot background and read as muted pink
// there but would be nearly invisible against the dark tooltip box.
pub const COLOR_RECOVERY_LEGEND: Color32 = Color32::from_rgb(0xC0, 0x55, 0x55);
pub const COLOR_GP_SPAN_FILL: Color32 = Color32::from_rgba_premultiplied(0, 60, 180, 30);
pub const COLOR_GP_SPAN_FILL_HOVER: Color32 = Color32::from_rgba_premultiplied(30, 100, 220, 55);
pub const COLOR_GP_SPAN_STROKE: Color32 = Color32::from_rgba_premultiplied(0, 60, 180, 50);
pub const COLOR_GP_SPAN_STROKE_HOVER: Color32 = Color32::from_rgba_premultiplied(30, 100, 220, 80);

// Per-cap shading for the Rate panel.  `Color32::from_rgba_premultiplied`
// requires the RGB channels to be *already* multiplied by the alpha fraction
// — otherwise egui effectively adds the full RGB to the framebuffer and the
// shading reads as near-opaque regardless of the alpha byte.  We therefore
// pre-compute `rgb * alpha / 255` manually so a "fill alpha" of 18 really
// does render as ~7% opacity, not a heavy wash.
//
// Helper kept as a const fn so the colours can remain `const` values.
pub const fn premul(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    let pr = ((r as u16) * (a as u16) / 255) as u8;
    let pg = ((g as u16) * (a as u16) / 255) as u8;
    let pb = ((b as u16) * (a as u16) / 255) as u8;
    Color32::from_rgba_premultiplied(pr, pg, pb, a)
}

pub const CAP_FILL_ALPHA: u8 = 32;
pub const CAP_STROKE_ALPHA: u8 = 80;

pub const COLOR_CAP_STARS_FILL: Color32 = premul(0xFF, 0x8F, 0x00, CAP_FILL_ALPHA);
pub const COLOR_CAP_STARS_STROKE: Color32 = premul(0xFF, 0x8F, 0x00, CAP_STROKE_ALPHA);
pub const COLOR_CAP_STARS_LEGEND: Color32 = Color32::from_rgb(0xFF, 0x8F, 0x00);

pub const COLOR_CAP_BW_RATE_FILL: Color32 = premul(0xC8, 0x28, 0x50, CAP_FILL_ALPHA);
pub const COLOR_CAP_BW_RATE_STROKE: Color32 = premul(0xC8, 0x28, 0x50, CAP_STROKE_ALPHA);
pub const COLOR_CAP_BW_RATE_LEGEND: Color32 = Color32::from_rgb(0xC8, 0x28, 0x50);

pub const COLOR_CAP_FIXED_FILL: Color32 = premul(0x5A, 0x8C, 0xE6, CAP_FILL_ALPHA);
pub const COLOR_CAP_FIXED_STROKE: Color32 = premul(0x5A, 0x8C, 0xE6, CAP_STROKE_ALPHA);
pub const COLOR_CAP_FIXED_LEGEND: Color32 = Color32::from_rgb(0x5A, 0x8C, 0xE6);

pub const COLOR_CAP_UNKNOWN_FILL: Color32 = premul(0x82, 0x82, 0x82, CAP_FILL_ALPHA);
pub const COLOR_CAP_UNKNOWN_STROKE: Color32 = premul(0x82, 0x82, 0x82, CAP_STROKE_ALPHA);
pub const COLOR_CAP_UNKNOWN_LEGEND: Color32 = Color32::from_rgb(0x82, 0x82, 0x82);

/// Fill / stroke / legend colours for a given PACING_CAP_* value.
pub fn pacing_cap_colors(cap: u8) -> (Color32, Color32, Color32) {
    use elog_parser::{PACING_CAP_BW_RATE_CAP, PACING_CAP_FIXED, PACING_CAP_STARS};
    match cap {
        PACING_CAP_STARS => (
            COLOR_CAP_STARS_FILL,
            COLOR_CAP_STARS_STROKE,
            COLOR_CAP_STARS_LEGEND,
        ),
        PACING_CAP_BW_RATE_CAP => (
            COLOR_CAP_BW_RATE_FILL,
            COLOR_CAP_BW_RATE_STROKE,
            COLOR_CAP_BW_RATE_LEGEND,
        ),
        PACING_CAP_FIXED => (
            COLOR_CAP_FIXED_FILL,
            COLOR_CAP_FIXED_STROKE,
            COLOR_CAP_FIXED_LEGEND,
        ),
        _ => (
            COLOR_CAP_UNKNOWN_FILL,
            COLOR_CAP_UNKNOWN_STROKE,
            COLOR_CAP_UNKNOWN_LEGEND,
        ),
    }
}
pub const COLOR_SRTT_UNPACED: Color32 = Color32::from_rgb(0xAB, 0x47, 0xBC);
pub const COLOR_ACK_RATE: Color32 = Color32::from_rgb(0x00, 0x89, 0x7B);
pub const COLOR_GOODPUT_UNPACED: Color32 = Color32::from_rgb(0x66, 0xBB, 0x6A);
pub const COLOR_COUNT_UNPACED: Color32 = Color32::from_rgb(0x90, 0xCA, 0xF9);
pub const COLOR_SEND_SIZE: Color32 = Color32::from_rgb(0x29, 0xB6, 0xF6);
pub const COLOR_ACK_SIZE: Color32 = Color32::from_rgb(0xEF, 0x6C, 0x00);
/// Retransmitted send on the Sizes panel — a distinct, saturated green so
/// retransmits stand out against the blue Send Size dots. Distinct enough
/// from `COLOR_INFLIGHT` (Material Green 500) on the Send-state panel
/// since they never appear in the same plot.
pub const COLOR_RTX_SIZE: Color32 = Color32::from_rgb(0x00, 0xC8, 0x53);
// --- New series colors ---
/// Peer's advertised receive window (CWND panel) — warm tan, distinct from
/// the cool blues/greens already used for cwnd/ssthresh/inflight.
pub const COLOR_PEER_RWND: Color32 = Color32::from_rgb(0xC6, 0x94, 0x1E);
/// Estimated send-socket-buffer occupancy (Send-state panel) — Material
/// cyan. Distinct from the warm tan (peer rwnd), the blues/greens
/// already used for cwnd/inflight, and the red (ssthresh).
pub const COLOR_SNDBUF: Color32 = Color32::from_rgb(0x00, 0xBC, 0xD4);
/// Outstanding SACKed bytes (Sizes panel) — orange-pink, visually
/// paired with loss/recovery. Rendered as a step line rather than
/// points since it's a continuous state, not a discrete event.
pub const COLOR_SACKED: Color32 = Color32::from_rgb(0xE5, 0x73, 0x73);

// --- STARS aggregate-sample colors (Aggregate tab) ---
/// Controller throttle gauge — warm red, since higher pressure means more
/// throttling.
pub const COLOR_STARS_PRESSURE: Color32 = Color32::from_rgb(0xFF, 0x52, 0x52);
/// Slot-gate deny rate — purple, distinct from the red pressure line on
/// the same panel.
pub const COLOR_STARS_DENY: Color32 = Color32::from_rgb(0xE0, 0x40, 0xFB);
/// Since-last-sample peak slot burst size — teal.
pub const COLOR_STARS_PEAK: Color32 = Color32::from_rgb(0x26, 0xA6, 0x9A);
/// Configured per-slot budget — muted blue-gray, since it's mostly a flat
/// reference line rather than an active signal.
pub const COLOR_STARS_BUDGET: Color32 = Color32::from_rgb(0x90, 0xA4, 0xAE);

// --- HPTS slot-gate sample colors (Aggregate tab) ---
/// Bytes the gate counted, drawn as Gbps.
pub const COLOR_SLOT_BYTES: Color32 = Color32::from_rgb(0x66, 0xBB, 0x6A);
/// Completed wheel fires per sample.
pub const COLOR_SLOT_FIRES: Color32 = Color32::from_rgb(0x42, 0xA5, 0xF5);
/// Flows diverted onto the rollover list.
pub const COLOR_SLOT_DIVERTS: Color32 = Color32::from_rgb(0xFF, 0xA7, 0x26);
/// CPUs that walked the interface counters to refresh the sample.
pub const COLOR_SLOT_WALKS: Color32 = Color32::from_rgb(0x26, 0xC6, 0xDA);
/// Samples published while the gate was closed.
pub const COLOR_SLOT_CLOSED: Color32 = Color32::from_rgb(0xEF, 0x53, 0x50);
/// Deepest rollover list on any CPU in the second.
pub const COLOR_SLOT_ROLLOVER: Color32 = Color32::from_rgb(0xAB, 0x47, 0xBC);

// --- Limiter overlay colors (very faint background shading) ---
pub const LIMITER_FILL_ALPHA: u8 = 40;
pub const LIMITER_STROKE_ALPHA: u8 = 0; // no edge stroke; overlays are blocky
pub const COLOR_LIMITER_CWND_FILL: Color32 = premul(0xF4, 0x43, 0x36, LIMITER_FILL_ALPHA);
pub const COLOR_LIMITER_RWND_FILL: Color32 = premul(0xFB, 0xC0, 0x2D, LIMITER_FILL_ALPHA);
pub const COLOR_LIMITER_PACING_FILL: Color32 = premul(0x7B, 0x1F, 0xA2, LIMITER_FILL_ALPHA);
pub const COLOR_LIMITER_APP_FILL: Color32 = premul(0x21, 0x96, 0xF3, LIMITER_FILL_ALPHA);
// Unknown (mystery gap: data queued, no sends) — subtle warm gray
pub const COLOR_LIMITER_UNKNOWN_FILL: Color32 = premul(0x9E, 0x9E, 0x9E, LIMITER_FILL_ALPHA);
// Idle (truly parked) — invisible; the idle_spans overlay handles it
pub const COLOR_LIMITER_IDLE_FILL: Color32 = Color32::TRANSPARENT;
pub const COLOR_LIMITER_STROKE: Color32 = premul(0x00, 0x00, 0x00, LIMITER_STROKE_ALPHA);
pub const COLOR_LIMITER_CWND_LEGEND: Color32 = Color32::from_rgb(0xF4, 0x43, 0x36);
pub const COLOR_LIMITER_RWND_LEGEND: Color32 = Color32::from_rgb(0xFB, 0xC0, 0x2D);
pub const COLOR_LIMITER_PACING_LEGEND: Color32 = Color32::from_rgb(0x7B, 0x1F, 0xA2);
pub const COLOR_LIMITER_APP_LEGEND: Color32 = Color32::from_rgb(0x21, 0x96, 0xF3);
pub const COLOR_LIMITER_UNKNOWN_LEGEND: Color32 = Color32::from_rgb(0x9E, 0x9E, 0x9E);
pub const COLOR_LIMITER_IDLE_LEGEND: Color32 = Color32::from_rgb(0x61, 0x61, 0x61);

// --- HTTP request lane colors (per-slot Gantt bars) ---
/// `HTTP_REQ_FREED` — normal completion.
pub const COLOR_HTTP_REQ_FREED: Color32 = Color32::from_rgb(0x43, 0xA0, 0x47);
/// `HTTP_REQ_STALE` — reaped while still open (client likely stalled).
pub const COLOR_HTTP_REQ_STALE: Color32 = Color32::from_rgb(0xFF, 0xB3, 0x00);
/// `HTTP_REQ_FORCEFREE` — evicted while still in-flight.
pub const COLOR_HTTP_REQ_FORCEFREE: Color32 = Color32::from_rgb(0xE5, 0x39, 0x35);
/// `HTTP_REQ_ALLOCFAIL` marker — no free tracking slot.
pub const COLOR_HTTP_REQ_ALLOCFAIL: Color32 = Color32::from_rgb(0xB7, 0x1C, 0x1C);
/// nginx request still open at capture end.
pub const COLOR_NGINX_REQ_OPEN: Color32 = Color32::from_rgb(0x9E, 0x9E, 0x9E);

// --- Discrete-event vertical marker colors ---
// Each marker is a thin (≤1 px) vertical line, so the alpha needs to
// be fairly high to stay visible against busy plot content. Each event
// type gets its own hue and its own alpha — more opaque for the rare,
// more serious events, lighter for DSACK which can fire hundreds of
// times per session.
/// RTO — deep purple. Most visually prominent: highest-severity event
/// and typically rarest.
pub const COLOR_MARKER_RTO: Color32 = premul(0x6A, 0x1B, 0x9A, 220);
/// Window Collapse — muted amber. Also rare; slightly lighter than RTO
/// so side-by-side events are distinguishable.
pub const COLOR_MARKER_WINDOW_COLLAPSE: Color32 = premul(0xE6, 0x51, 0x00, 200);
/// DSACK — deep teal. Can fire hundreds of times per session on loss-
/// heavy paths, so the alpha is the lightest of the three to keep a
/// dense cluster from saturating into a solid band.
pub const COLOR_MARKER_DSACK: Color32 = premul(0x00, 0x69, 0x8F, 160);

// --- Session list timeline colors ---
/// Faint track under each row's lifetime bar — the full trace span.
pub const COLOR_SESSION_TRACK: Color32 = premul(0x9E, 0x9E, 0x9E, 36);
/// Session lifetime on the trace span, muted so the active window pops.
pub const COLOR_SESSION_LIFETIME: Color32 = premul(0x42, 0xA5, 0xF5, 110);
/// Byte-weighted active window inside the lifetime bar.
pub const COLOR_SESSION_ACTIVE: Color32 = Color32::from_rgb(0x90, 0xCA, 0xF9);
/// One session on the overview minimap; overlaps accumulate into a
/// concurrency density read.
pub const COLOR_MINIMAP_SESSION: Color32 = premul(0x42, 0xA5, 0xF5, 28);
/// Dim over the minimap regions the time-window filter excludes.
pub const COLOR_MINIMAP_DIM: Color32 = premul(0x00, 0x00, 0x00, 90);

// ---------------------------------------------------------------------------
// Interface / TCP-sample view colours
// ---------------------------------------------------------------------------

/// Qualitative palette for per-interface lines and per-stack lines
/// (TCP requests/sec) on the Aggregate tab. Indexed modulo length so an
/// arbitrary number of interfaces each get a stable, distinct hue.
pub const SERIES_PALETTE: &[Color32] = &[
    Color32::from_rgb(0x42, 0xA5, 0xF5), // blue
    Color32::from_rgb(0xFF, 0x70, 0x43), // deep orange
    Color32::from_rgb(0x66, 0xBB, 0x6A), // green
    Color32::from_rgb(0xAB, 0x47, 0xBC), // purple
    Color32::from_rgb(0xFF, 0xCA, 0x28), // amber
    Color32::from_rgb(0x26, 0xC6, 0xDA), // cyan
    Color32::from_rgb(0xEC, 0x40, 0x7A), // pink
    Color32::from_rgb(0x8D, 0x6E, 0x63), // brown
    Color32::from_rgb(0x9C, 0xCC, 0x65), // light green
    Color32::from_rgb(0x7E, 0x57, 0xC2), // indigo
];

pub fn series_color(i: usize) -> Color32 {
    SERIES_PALETTE[i % SERIES_PALETTE.len()]
}

pub const COLOR_OBJ_TIMELINE_A: Color32 = Color32::from_rgb(0x5C, 0xAD, 0xCF);
pub const COLOR_OBJ_TIMELINE_B: Color32 = Color32::from_rgb(0xB4, 0x9A, 0xDC);
pub const COLOR_OBJ_TIMELINE_SKIP: Color32 = Color32::from_rgb(0xE5, 0x39, 0x35);
pub const COLOR_FRAME_TL_TEAL_LIGHT: Color32 = Color32::from_rgb(0x7A, 0xC2, 0xDC);
pub const COLOR_FRAME_TL_TEAL_DARK: Color32 = Color32::from_rgb(0x3E, 0x8E, 0xB8);
pub const COLOR_FRAME_TL_PURPLE_LIGHT: Color32 = Color32::from_rgb(0xCC, 0xB6, 0xEA);
pub const COLOR_FRAME_TL_PURPLE_DARK: Color32 = Color32::from_rgb(0x8E, 0x72, 0xC4);

pub fn frame_timeline_fill(gop_odd: bool, frame_odd: bool) -> Color32 {
    match (gop_odd, frame_odd) {
        (false, false) => COLOR_FRAME_TL_TEAL_LIGHT,
        (false, true) => COLOR_FRAME_TL_TEAL_DARK,
        (true, false) => COLOR_FRAME_TL_PURPLE_LIGHT,
        (true, true) => COLOR_FRAME_TL_PURPLE_DARK,
    }
}

/// Gray band for leftover titles folded into Other on Fanout stacks.
pub const COLOR_STACK_OTHER: Color32 = Color32::from_gray(160);

/// True for the leftover-titles band of a Fanout per-title stack, which
/// has no channel tab to open.
pub fn is_stack_other(label: &str) -> bool {
    label == "Other"
}

/// Color for a Fanout per-title stack band. Looked up by short name so
/// Subscribers / RX / TX use the same palette slot for the same channel.
pub fn title_stack_color(map: &std::collections::HashMap<String, Color32>, label: &str) -> Color32 {
    if is_stack_other(label) {
        COLOR_STACK_OTHER
    } else {
        map.get(label).copied().unwrap_or_else(|| series_color(0))
    }
}

// TCP PCB-state gauge lines (Aggregate tab). `established` keeps a fixed
// green; the individual per-state lines draw from SERIES_PALETTE.
pub const COLOR_TCP_ESTAB: Color32 = Color32::from_rgb(0x4C, 0xAF, 0x50);

/// Requests/sec line and goodput-histogram bars.
pub const COLOR_TCP_RPS: Color32 = Color32::from_rgb(0xFF, 0x70, 0x43);
pub const COLOR_GOODPUT_BAR: Color32 = Color32::from_rgb(0x29, 0xB6, 0xF6);

// --- Interface protocol-stack colors (Interfaces panel, Single Interface
// view) ---
/// TCP band -- reuses `COLOR_TCP_ESTAB`'s green so "TCP" reads
/// consistently across tabs.
pub const COLOR_IF_TCP: Color32 = COLOR_TCP_ESTAB;
/// UDP band -- deep orange, distinct from TCP's green and Other's gray.
pub const COLOR_IF_UDP: Color32 = Color32::from_rgb(0xFF, 0x70, 0x43);
/// Non-TCP/UDP band -- neutral gray, matching the "not a classified
/// protocol" gray used elsewhere (e.g. goodput bucket 0).
pub const COLOR_IF_OTHER: Color32 = Color32::from_rgb(0x9E, 0x9E, 0x9E);

/// Band color for goodput log2 bucket `b` in the stacked distribution-over-
/// time plot. Bucket 0 (`flsll(0)` — a goodput estimate that truncated to
/// <1 Mbps, i.e. effectively idle) is a muted gray so it reads as "not a
/// real rate". Buckets `1..nbuckets` trend cold→hot (magenta/blue for low
/// goodput → red for the saturated top bucket) so faster flows read warmer.
///
/// A smooth hue ramp alone leaves neighbouring stacked bands nearly
/// indistinguishable, so we (a) sweep a wide hue range and (b) alternate
/// brightness/saturation between adjacent buckets — every band therefore
/// differs from its neighbours in both hue *and* lightness.
pub fn goodput_bucket_color(b: usize, nbuckets: usize) -> Color32 {
    if b == 0 {
        return Color32::from_gray(96);
    }
    let slots = nbuckets.saturating_sub(1).max(1) as f32;
    let frac = ((b - 1) as f32 / (slots - 1.0).max(1.0)).clamp(0.0, 1.0);
    // Wide ramp: 0.82 (magenta) → 0.0 (red), passing through blue, cyan,
    // green, yellow, orange.
    let hue = 0.82 * (1.0 - frac);
    // Zig-zag the value/saturation so consecutive bands never look alike.
    let (sat, val) = if b.is_multiple_of(2) {
        (0.90, 0.98)
    } else {
        (0.55, 0.72)
    };
    Color32::from(egui::ecolor::Hsva::new(hue, sat, val, 1.0))
}

/// Goodput bucket color. Low Mbps is blue and the ramp reaches green at
/// 256 Mbps, so the rates a connection actually takes use the whole span;
/// faster buckets stay green. A shift toward green is speeding up and
/// toward blue is slowing down. Bucket 0 (idle, under 1 Mbps) stays gray.
/// Adjacent buckets alternate lightness so neighbouring bands stay
/// separable.
pub fn goodput_speed_color(b: usize, nbuckets: usize) -> Color32 {
    if b == 0 {
        return Color32::from_gray(96);
    }
    // Bucket b covers [2^(b-1), 2^b) Mbps. Span 8 finishes at bucket 9
    // (256 Mbps). The saturated tail is far above that.
    let span = 8.0_f32.min((nbuckets.saturating_sub(1).max(1)) as f32);
    let t = ((b - 1) as f32 / span).clamp(0.0, 1.0);
    // 0.66 = blue, 0.33 = green (same green as a low smoothed RTT).
    let hue = 0.66 + (0.33 - 0.66) * t;
    let (sat, val) = if b.is_multiple_of(2) {
        (0.80, 0.97)
    } else {
        (0.88, 0.70)
    };
    Color32::from(egui::ecolor::Hsva::new(hue, sat, val, 1.0))
}

/// Smoothed-RTT bucket color. Low usec is green, the middle is yellow,
/// and high usec is red, so a stack shifting toward red is slowing down.
/// Bucket 0 (unmeasured) stays gray. Adjacent buckets step slightly in
/// lightness so neighbouring bands stay separable.
pub fn srtt_speed_color(b: usize, nbuckets: usize) -> Color32 {
    if b == 0 {
        return Color32::from_gray(96);
    }
    let slots = nbuckets.saturating_sub(1).max(1) as f32;
    let t = ((b - 1) as f32 / (slots - 1.0).max(1.0)).clamp(0.0, 1.0);
    // Green → yellow over the first half, yellow → red over the second.
    let (c0, c1, u) = if t <= 0.5 {
        ((0x2E, 0xC7, 0x4B), (0xF5, 0xD0, 0x22), t / 0.5)
    } else {
        ((0xF5, 0xD0, 0x22), (0xE5, 0x3E, 0x3E), (t - 0.5) / 0.5)
    };
    let mix = |a: u8, c: u8| -> u8 { (a as f32 + (c as f32 - a as f32) * u) as u8 };
    let scale = if b.is_multiple_of(2) { 1.0 } else { 0.88 };
    Color32::from_rgb(
        (mix(c0.0, c1.0) as f32 * scale) as u8,
        (mix(c0.1, c1.1) as f32 * scale) as u8,
        (mix(c0.2, c1.2) as f32 * scale) as u8,
    )
}

pub fn limiter_colors(class: Limiter) -> (Color32, Color32, Color32) {
    match class {
        Limiter::Cwnd => (
            COLOR_LIMITER_CWND_FILL,
            COLOR_LIMITER_STROKE,
            COLOR_LIMITER_CWND_LEGEND,
        ),
        Limiter::Rwnd => (
            COLOR_LIMITER_RWND_FILL,
            COLOR_LIMITER_STROKE,
            COLOR_LIMITER_RWND_LEGEND,
        ),
        Limiter::Pacing => (
            COLOR_LIMITER_PACING_FILL,
            COLOR_LIMITER_STROKE,
            COLOR_LIMITER_PACING_LEGEND,
        ),
        Limiter::App => (
            COLOR_LIMITER_APP_FILL,
            COLOR_LIMITER_STROKE,
            COLOR_LIMITER_APP_LEGEND,
        ),
        Limiter::Unknown => (
            COLOR_LIMITER_UNKNOWN_FILL,
            COLOR_LIMITER_STROKE,
            COLOR_LIMITER_UNKNOWN_LEGEND,
        ),
        Limiter::Idle => (
            COLOR_LIMITER_IDLE_FILL,
            COLOR_LIMITER_STROKE,
            COLOR_LIMITER_IDLE_LEGEND,
        ),
    }
}

/// Memory-panel bands, indexed like `analysis::MEM_STATE_LABELS`
/// (Free, Inactive, Laundry, Active, Wired). Legend and cursor use
/// these opaque; the stack fill uses [`mem_band_fill`].
pub const MEM_STATE_COLORS: [Color32; 5] = [
    Color32::from_rgb(0x8C, 0xE0, 0xA8), // free
    Color32::from_rgb(120, 190, 235),    // inactive
    Color32::from_rgb(0xE0, 0xA8, 0xC8), // laundry
    Color32::from_rgb(0xE8, 0xBE, 0x70), // active
    Color32::from_rgb(0xE0, 0x90, 0x80), // wired
];
/// Gap between the five sampled states and usable memory.
pub const COLOR_MEM_OTHER: Color32 = Color32::from_rgb(0xC8, 0xD2, 0xDC);

/// Same opacity as the CPU Usage stack fill.
const MEM_BAND_ALPHA: u8 = 190;

/// Stack fill: same hue as `color`, open enough for the plot grid.
pub const fn mem_band_fill(color: Color32) -> Color32 {
    premul(color.r(), color.g(), color.b(), MEM_BAND_ALPHA)
}
pub const COLOR_MEM_FREE_TARGET: Color32 = Color32::from_rgb(0xE0, 0xE0, 0xE0);
pub const COLOR_MEM_FREE_MIN: Color32 = Color32::from_rgb(0xFF, 0xEE, 0x58);
pub const COLOR_MEM_FREE_SEVERE: Color32 = Color32::from_rgb(0xFF, 0x52, 0x52);
pub const COLOR_MEM_SWAP: Color32 = Color32::from_rgb(0xAB, 0x47, 0xBC);
