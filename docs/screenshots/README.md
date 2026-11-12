# Screenshots

This directory holds the screenshot assets referenced from
[`../../README.md`](../../README.md) and [`../gui.md`](../gui.md).

## Capture list

The docs reference these filenames. Drop PNGs with the exact names into this
directory and the embedded `![...]` references will resolve.

The README showcases three of these (`flame-graph.png`, `processes.png`,
and `system-overview.png`); the rest are embedded in `docs/gui.md`
alongside the panels they show.

### Core set

1. **`hero-session.png`** — the per-session screenshot in `docs/gui.md`.
   - Multi-session capture loaded, one session tab active.
   - All four time-linked panels visible (RTT, Rate, CWND, Sizes); the
     Size CDF can be hidden / minimized to give the time panels room.
   - Legend readable on each panel — must include **RTX Size** in the
     Sizes legend so readers see retransmits are split out.
   - **Limiter overlay** turned on so the background tint (cwnd / pacing /
     app shading) is clearly visible across the time panels.
   - **Title-bar limiter summary** ("Pacing N% · CWND N% · Idle N%") legible
     to the right of the session label.
   - At least one recovery episode (pink shading) and visibly cwnd-bound
     stretches (red shading) so the overlays read at a glance.
   - Gear menu open in the corner is fine — it advertises the available
     toggles (DSACK / RTO / Window-collapse markers, Goodput spans, etc.).
   - Wide window (the 1400×900 default is fine; 1600×1000 looks great).

2. **`system-overview.png`** — the README's third showcase image, and the
   System tab section of `docs/gui.md`.
   - Top of the **System** tab on a capture with `cpu` and `if` provider
     data: **CPU Usage** strip, **Network TX**, and **Network RX** panels
     all visible.
   - A capture with real, bursty multi-Gb/s throughput across several
     interfaces (a `lagg` aggregate plus its members) makes the point best.
   - A hover tooltip on the Network TX/RX plot showing per-interface rates
     is a nice touch but optional.

3. **`plot-catalog.png`** — `docs/gui.md`'s Plot catalog section. The left
   sidebar open on the System tab, showing plot reordering, visibility
   toggles, lock controls, and the Apply/Reset buttons.

4. **`tcp-stack-telemetry.png`** — `docs/gui.md`'s TCP stack section.
   Per-stack TCP telemetry
   that doesn't require any individual TCP *session* to be present in the
   capture (good for `hpts`/`if`/`cpu`-only traces).
   - **TCP Requests per Second**, **TCP Connections by State** (PCB-state
     gauges), and **TCP Goodput** distribution-over-time panels, scrolled
     together on the System tab.
   - A hover tooltip on the Connections-by-State panel listing every PCB
     state's count is a nice touch but optional.

5. **`stars-panels.png`** — `docs/gui.md`'s System tab (shows off the
   `hpts` provider's STARS telemetry). **STARS Pressure & Slot-Gate
   Denies** and **STARS Slot-Gate Budget / Peak Burst** panels on the
   System tab. Only possible on captures with `STARS_AGG_SAMPLE` events.

6. **`hpts-histograms.png`** — `docs/gui.md`'s HPTS callout histograms
   section. **HPTS Callout
   Lateness** and **HPTS Batch Size** stacked histograms on the System
   tab. A hover tooltip on the lateness histogram, listing every usec-late
   bucket's share, is a great demonstration of the panel's density — keep
   it if present.

7. **`flame-graph.png`** — the README's first showcase image, and
   `docs/gui.md`'s CPU flame graph section. The **🔥 Flame** tab on a
   capture with `cpu` provider data, zoomed out to the whole capture.
   Prefer a real-world capture with a recognizable, varied stack (multiple
   processes / kernel + user frames) over a synthetic one — it sells the
   symbolization quality.

8. **`processes.png`** — the README's second showcase image, and the
   Processes bullet in `docs/gui.md`'s tab bar. The **Processes** tab
   table, ideally with a row selected so the detail panel (with its
   "Open flame" shortcut) is visible too.

9. **`trace-info-popup.png`** — `docs/gui.md`'s tab bar section. The global
   trace-info popup (the ⓘ button in the tab bar), showing the **File**
   section (name, duration, event counts) and **Providers** table on a
   capture with several providers loaded.

10. **`one-second-zoom.png`** — the walked example in `docs/gui.md`.
   - The four time-linked panels zoomed into ~1 s of wall time.
   - **Limiter overlay** on, with at least one cwnd-bound (red) stretch
     visible.
   - **DSACK markers** on (and ideally a marker visible inside the zoom).
   - Hover anywhere in the zoom so the **shared cursor + tooltip** is
     captured, listing per-series values, the active limiter class, and
     the marker reason.
   - Title-bar limiter summary still legible.

11. **`rtt-tooltip.png`** — `docs/gui.md`'s tooltip contents section.
    - Zoomed into a short time window on the RTT panel (or the full
      session view with the RTT panel maximized).
    - Vertical cursor line and per-series value dots visible.
    - Bonus: hover near a goodput measurement span so its orange band is
      also visible in the Rate panel behind the RTT zoom.

### Optional

12. **`session-list.png`** — the session list / `+` tab.
    - Multi-session capture, sorted by any column.
    - "Hide short sessions" either on or off — whichever looks less empty.

13. **`pacing-caps.png`** — Rate panel with PACING_CALC_V2 cap shading.
    - A session that was pacing-capped for at least a portion of its life
      (STARS, BW_RATE_CAP, or FIXED all work; UNKNOWN is less informative).
    - Optional but sells the pacing-cap feature well.

## Tips

- Use a light or dark theme consistently across shots — readers notice when
  two are mismatched.
- PNG at native resolution is fine; the README renders them at whatever
  width GitHub allots, so don't worry about downscaling.
- If a capture is sensitive, blur or crop the `Session <hex>` / remote
  `IP:port` labels. The `(unknown)` placeholder works too — just pick a
  capture without `CONN_SET_IP_V4` events or rename the session inline
  before grabbing the shot.
- The per-session shots (1, 9, 10, 11, 12) need a capture with real TCP
  *sessions*; the System-tab / Flame / Processes / trace-info shots (2–8)
  work great on `hpts`/`if`/`cpu`-only captures (e.g. a STARS-focused OCA
  trace) that never registered an individual TCP session.

## A note for maintainers

If you change screenshot filenames here, update the references in
[`../../README.md`](../../README.md) and [`../gui.md`](../gui.md)
accordingly.
