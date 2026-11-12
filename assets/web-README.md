# EventExplorer (browser build)

This is the WebAssembly build of the EventExplorer GUI. It runs the
same parser and visualisations as the desktop binary, but inside any
modern browser tab — no install, no server, no system dependencies.

## Quick start

You **cannot** open `index.html` directly by double-clicking it.
Browsers refuse to load WebAssembly modules over the `file://` scheme,
so the page will hang on the loading screen forever. You need a
static HTTP server pointing at this directory.

Pick whichever of these is convenient:

```bash
# Python (preinstalled on macOS and most Linux distros)
python3 -m http.server 8000

# Node.js
npx serve

# Caddy (single-binary, cross-platform)
caddy file-server --listen :8000

# Rust
cargo install simple-http-server && simple-http-server --port 8000
```

Then open <http://localhost:8000/> in your browser.

To share with a colleague on the same network, `python3 -m http.server`
binds to `0.0.0.0` by default, so anyone can hit
`http://<your-ip>:8000/`. For permanent hosting, drop this directory on
any static-hosting service (an S3-compatible bucket, an nginx host, a
CDN, GitHub Pages, etc.) — it's fully self-contained.

## What's inside

- `index.html` — entry page with a dark boot overlay.
- `favicon.svg` / `favicon-16.png` / `favicon-32.png` — tab icon (pixel-aligned navy **E** with two bars).
- `event_explorer-<hash>.js` — wasm-bindgen JS shim.
- `event_explorer-<hash>_bg.wasm` — the WebAssembly module (~3 MB
  uncompressed; gzip-compressed by most static hosts to ~1 MB).
- `LICENSE` — Apache-2.0.

The hashed filenames let browsers cache aggressively; new releases
bust the cache automatically.

## Browser support

Any modern browser with WebAssembly + WebGL2:

- Chrome / Edge / Brave (any current version)
- Firefox (any current version)
- Safari 15+ (macOS Monterey, iOS 15+)

Mobile browsers render correctly but the dense multi-panel UI is
designed for mouse + keyboard; touch UX is rough.

No special HTTP headers are required — we don't use `SharedArrayBuffer`
or wasm threads, so the host doesn't need `Cross-Origin-Opener-Policy`
or `Cross-Origin-Embedder-Policy` headers.

## Opening a file via the URL

The page accepts a `?file=<url>` query parameter and, if present,
fetches that URL via the browser's Fetch API on startup and feeds
the bytes through the same parse pipeline as the file picker.
Relative paths resolve against the page origin, so deployments can
deep-link to captures hosted alongside the wasm bundle:

```
http://localhost:8000/?file=samples/foo.elog.gz
http://localhost:8000/?file=https://other.host/foo.elog
```

Absolute URLs are subject to the target server's CORS policy.

On deployments that serve a sibling `../logs/` autoindex (a dev host's
`~/public_html` layout next to this bundle), the splash page lists those `.elog` /
`.elog.gz` files for one-click open. Opening one (or a URL from the
splash field) pushes `?file=` so the browser Back button returns to
the landing page.

`file://` URLs **do not work** — browsers refuse to fetch local
files from a web page (Chromium logs "Not allowed to load local
resource"). To open a file from your own machine, run a static
server in its directory and use that as the URL:

```bash
cd ~/Downloads
python3 -m http.server 8000
# then open http://<host>:<eventexplorer-port>/?file=http://localhost:8000/foo.elog.gz
```

Or just use the file picker (button in the top-left of the app).

## Limitations vs. the desktop GUI

- **No recent-files list, no SSH remote-fetch.** Files come in via
  the file picker or the `?file=<url>` URL parameter described above.
- **Parsing blocks the UI thread.** The page freezes until the parse
  completes (browsers don't expose threads to wasm without
  SharedArrayBuffer + COOP/COEP, which we don't require).
- **Sequential parse.** No `rayon` parallelism on web, so per-session
  sort and summary loops run sequentially.
- **Memory ceiling.** The wasm32 sandbox caps at ~4 GB; iOS Safari is
  closer to 1–2 GB. Captures of a few hundred MB load comfortably;
  multi-GB captures may OOM.
- **No window-state persistence.** Browser owns the tab geometry.

## Site features

Capture sharing by upload, symbol-pack resolution, and the page share
links open come from a site config baked in at build time
(`EE_SITE_CONFIG=site.toml trunk build`). A bundle built without one
shares only the view link for captures loaded from a URL, and leaves
symbol-pack frames as `kernel+0xOFF`. See `docs/site-config.md` in the
source repository.

For the full feature set, parallelism, and bigger captures, use the
desktop binary from the same release.
