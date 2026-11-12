# Site configuration

EventExplorer reads and plots captures with no server. Four features
need servers or repos that belong to a deployment: sharing captures
by upload, resolving symbol packs, linking frames to firmware source,
and the hosted page that share links open. A *site config* points
EventExplorer at them. Every key is optional; a feature whose key is
unset stays off (see [Unconfigured behaviour](#unconfigured-behaviour)).

The implementation is [`src/site.rs`](../src/site.rs). Every build is
the same binary; a deployment configures it by installing a file.

## Keys

| Key | Env override | Meaning |
|-----|--------------|---------|
| `uploads_url` | `ELOG_UPLOADS_URL` | Base of the [upload API](#upload-server-api). Turns on capture sharing. |
| `share_url` | `ELOG_SHARE_URL` | Base that share links embed as `?file=<base>/<id>/download`. Defaults to `uploads_url`; set it when the browser reaches the server at a different URL than the native API calls do. |
| `eventexplorer_page` | `ELOG_EVENTEXPLORER_PAGE` | Hosted EventExplorer page that share links open. Unset leaves a bare `?file=…` query, which the hosting page resolves against itself. |
| `symbols_url` | `ELOG_SYMBOLS_URL` | Base of the [symbol catalog](#symbol-catalog). Turns on symbol-pack resolution. |
| `http_command` | `ELOG_HTTP_COMMAND` | Command that performs native site API requests in place of `curl -sS`, so a site can add its own authentication. See [`http_command`](#http_command). |
| `firmware_repo` | `ELOG_FIRMWARE_REPO` | Firmware source repo, `host/owner/repo[@ref]` (ref defaults to `HEAD`). Turns on firmware source links. |
| `upload_note` | `ELOG_UPLOAD_NOTE` | Extra text shown in the share dialog (retention, network requirements, …). |

An empty value counts as unset.

## Lookup order

A native build (desktop GUI and `elog_cli`) takes each key from the
first of:

1. the `ELOG_<KEY>` environment variable;
2. the user file: `$ELOG_SITE_CONFIG`, else
   `<config dir>/event_explorer/site.toml` (`~/.config/event_explorer/site.toml`
   on Linux/FreeBSD, `~/Library/Application Support/event_explorer/site.toml`
   on macOS, `%APPDATA%\event_explorer\site.toml` on Windows);
3. `site.toml` next to the executable, which lets a repackaged release
   carry its site;
4. `/usr/local/etc/event_explorer/site.toml` (Unix), for a machine-wide
   install.

Files are merged per key, so a user file can override one key of a
packaged site. The browser build fetches `site.toml` from its own
directory (next to `index.html`) at startup; a missing file (404)
leaves the site empty.

## Installing a site

```bash
elog_cli site install https://example.com/eventexplorer/site.toml   # or a local path
elog_cli site show       # effective value and source of every key
```

`site install` checks the file and writes it as the user file, so it
survives upgrades. `site show` lists the site files in lookup order and
where each key's value came from.

## File format

A flat subset of TOML: one `key = value` per line, where the value is a
double-quoted string or an array of strings. Blank lines and `#`
comments (whole-line or trailing) are skipped. An unknown key is
reported on stderr and ignored; a malformed file is reported and
ignored as a whole.

```toml
# ~/.config/event_explorer/site.toml
uploads_url        = "https://share.example.com/api/uploads"
eventexplorer_page = "https://share.example.com/eventexplorer/"
symbols_url        = "https://symbols.example.com/api/packs"
http_command       = ["auth-curl", "--profile", "eventexplorer"]
firmware_repo      = "git.example.com/acme/firmware@main"
upload_note        = "Uploads are kept for 90 days."
```

## `http_command`

The command runs in place of `curl -sS` for upload API calls and
symbol-catalog GETs. It is an argv array in the site file, or a JSON
array or whitespace-separated words in the env var. EventExplorer
appends curl-style arguments — `-X <method>`, `-H <header>`,
`-d <body>`, `-v` — and then the URL, and reads the response body from
stdout. A wrapper that adds credentials and then calls curl fits.

For symbol fetches through a custom command, EventExplorer passes `-v`
and reads the status line and `Location:` header from its output: the
catalog must answer with a redirect, and the redirect target (a
presigned URL that carries its own signature) is fetched with plain
`curl -L`. A 404 status means the pack does not exist.

Object-store PUTs for uploads always use plain curl.

## Upload server API

Capture bytes never pass through the upload server: it signs a
presigned object-store PUT and indexes the result. Error responses
carry a FastAPI-style `{"detail": "..."}` (a string, or an array of
`{"msg": ...}`), which the client shows as the error text.

1. **Who am I** — `GET <base>/whoami` →
   `{"user": "alice@example.com", "max_bytes": 2147483648}`. An empty
   `user` means "not signed in". `elog_cli upload` calls this first.
2. **Start** — `POST <base>` with
   `{"cache_name", "file_name", "content_length", "note"}` →
   `{"upload_id", "url", "method", "headers", "expires_in"}`: a
   presigned PUT target. `method` defaults to `PUT`; `headers` are
   replayed verbatim on the PUT.
3. **PUT** — the client sends the bytes to `url` with those headers
   (native: `curl -T`, so `Content-Length` matches the signature;
   browser: an XHR blob). The URL carries its own signature.
4. **Complete** — `POST <base>/<id>/complete` with `{}` →
   `{"upload": {"upload_id", "user", "cache_name", "file_name", "size",
   "note", "etag"}}`. The server checks the stored object and indexes it.
5. **Download** — `<share base>/<id>/download` answers 307 to a
   freshly-signed GET, so a share link lives as long as the upload,
   not as long as one presign. Share links embed this URL as `?file=`,
   with the file name in a `#fragment` that the browser never sends.

One upload is capped at 2 GiB client-side; a server-side cap wins (the
start call rejects it). The browser build calls the API with the
hosting page's session/cookies (`withCredentials`), so the server needs
CORS for that origin unless it is same-origin.

The cache name defaults to the capture's `MACHINE_INFO` hostname, else
an appliance-style file-name prefix (`c101.lab001.dev-eventlog-…` →
`c101.lab001.dev`).

## Symbol catalog

A reference-mode capture stores `SYMBOL_PACK_REF` rows naming a pack by
GNU build-id instead of carrying function ranges.
`GET <symbols_url>/<build_id>` returns that pack: a gzip'd `.elog`
holding the binary's function ranges, directly or via a redirect. A 404
means no pack. Native builds cache hits under
`~/.cache/event_explorer/symbol-packs/` and remember 404s for six hours
(`ELOG_SYMBOLS_CACHE=off` disables the cache,
`ELOG_SYMBOLS_CACHE_DIR` moves it). The browser fetches with the page's
credentials, so the catalog and any redirect target need CORS for the
hosting origin.

## Firmware source links

With `firmware_repo` set, annotated source for files in the firmware
tree is fetched from that repo at the commit `elog -L` recorded, else
the firmware commit in the kernel ident, else the configured ref.
Lookups use the `gh` CLI against the repo's host
(`gh api --hostname <host>`), so `gh auth login --hostname <host>` must
have been run for a private host. Paths that map to public GitHub
repos (FreeBSD ports, Go's GOROOT, …) link without any site config.

## Unconfigured behaviour

| Key unset | Effect |
|-----------|--------|
| `uploads_url` | Desktop: the 🔗 Share button is hidden. Web: the Share dialog shows only the view link (for URL-loaded captures). `elog_cli upload` fails with "no upload server is configured". |
| `symbols_url` | Pack refs stay unresolved; their frames show `kernel+0xOFF`. |
| `firmware_repo` | Firmware source links are off; public GitHub mappings still work. |
| `eventexplorer_page` | Share links are a bare `?file=…` query. |
| `http_command` | Site requests use `curl -sS`. |
