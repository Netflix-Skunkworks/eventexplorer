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
use std::io::{BufRead, Read};

/// Holds file data that was either memory-mapped or decompressed into memory.
#[cfg(not(target_arch = "wasm32"))]
pub enum FileData {
    Mmap(Mmap),
    Buf(Vec<u8>),
}

#[cfg(not(target_arch = "wasm32"))]
impl std::ops::Deref for FileData {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            FileData::Mmap(m) => m,
            FileData::Buf(v) => v,
        }
    }
}

/// Detects an SCP-style remote path of the form `[user@]host:path`.
///
/// The host portion must contain no `/` (so plain Unix paths are unambiguous),
/// and we reject single-letter "hosts" so Windows-style `C:\foo` is treated
/// as local. Both halves must be non-empty.
pub fn is_remote_path(s: &str) -> Option<(&str, &str)> {
    let colon = s.find(':')?;
    if s[..colon].contains('/') {
        return None;
    }
    let host = &s[..colon];
    let path = &s[colon + 1..];
    if host.is_empty() || path.is_empty() {
        return None;
    }
    if host.len() == 1 && host.chars().next().unwrap().is_ascii_alphabetic() {
        return None;
    }
    Some((host, path))
}

/// Single-quote a string for safe inclusion in a remote shell command.
#[cfg(not(target_arch = "wasm32"))]
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// One gzip member, already past its header. Read it to EOF before
/// [`GzipMember::into_inner`]: that returns the underlying reader
/// positioned at the next member.
pub(crate) struct GzipMember<R> {
    pub capture_index: u32,
    pub new_capture: bool,
    pub name: Option<String>,
    dec: flate2::bufread::GzDecoder<R>,
    prefix: [u8; 8],
    prefix_len: usize,
    prefix_pos: usize,
}

impl<R> Read for GzipMember<R>
where
    R: BufRead,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.prefix_pos < self.prefix_len {
            let n = (self.prefix_len - self.prefix_pos).min(buf.len());
            buf[..n].copy_from_slice(&self.prefix[self.prefix_pos..self.prefix_pos + n]);
            self.prefix_pos += n;
            return Ok(n);
        }
        self.dec.read(buf)
    }
}

impl<R> GzipMember<R> {
    pub(crate) fn into_inner(self) -> R {
        self.dec.into_inner()
    }
}

impl GzipMember<&[u8]> {
    pub(crate) fn unread_compressed(&self) -> usize {
        self.dec.get_ref().len()
    }
}

/// How far a multi-member gzip walk has got. A member whose inflated
/// bytes start with an ELOG v1–v3 header opens another capture; every
/// other member appends to the capture already open, which is how a
/// parallel gzip stores one file.
#[derive(Default)]
pub(crate) struct GzipSplit {
    captures: u32,
    open: bool,
}

fn looks_like_elog(prefix: &[u8]) -> bool {
    if prefix.len() < 8 || &prefix[..4] != b"ELOG" {
        return false;
    }
    let version = u32::from_le_bytes([prefix[4], prefix[5], prefix[6], prefix[7]]);
    (1..=3).contains(&version)
}

fn gzip_member_name(header: Option<&flate2::GzHeader>) -> Option<String> {
    let raw = header?.filename()?;
    let text = std::str::from_utf8(raw).ok()?;
    let name = text.rsplit(['/', '\\']).next().unwrap_or(text).trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Label for one capture inside a multi-member `.gz`. `index` is 1-based.
pub(crate) fn member_display_name(header: Option<&str>, outer: &str, index: u32) -> String {
    match header {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => format!("{outer} #{index}"),
    }
}

/// Return `name`, or `name #2`, `name #3`, … when it was already used.
pub(crate) fn unique_part_name(
    used: &mut std::collections::HashSet<String>,
    name: String,
) -> String {
    if used.insert(name.clone()) {
        return name;
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{name} #{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// Next gzip member of `reader`, or `None` at EOF (trailing non-gzip
/// bytes after a member count as EOF). Empty members are skipped.
pub(crate) fn open_gzip_member<R: BufRead>(
    mut reader: R,
    split: &mut GzipSplit,
) -> std::io::Result<Option<GzipMember<R>>> {
    loop {
        let peek = reader.fill_buf()?;
        if peek.is_empty() {
            return Ok(None);
        }
        if peek.len() < 2 || peek[0] != 0x1f || peek[1] != 0x8b {
            if split.captures == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "not a gzip stream",
                ));
            }
            return Ok(None);
        }
        let mut dec = flate2::bufread::GzDecoder::new(reader);
        let name = gzip_member_name(dec.header());
        let mut prefix = [0u8; 8];
        let mut prefix_len = 0;
        while prefix_len < prefix.len() {
            match dec.read(&mut prefix[prefix_len..]) {
                Ok(0) => break,
                Ok(n) => prefix_len += n,
                Err(e) => return Err(e),
            }
        }
        if prefix_len == 0 {
            reader = dec.into_inner();
            continue;
        }
        let new_capture = !split.open || looks_like_elog(&prefix[..prefix_len]);
        if new_capture {
            split.captures += 1;
            split.open = true;
        }
        return Ok(Some(GzipMember {
            capture_index: split.captures,
            new_capture,
            name,
            dec,
            prefix,
            prefix_len,
            prefix_pos: 0,
        }));
    }
}

/// Wraps a `Read` and updates the shared `LoadProgress` as bytes flow
/// through. Used to surface progress while either downloading from
/// `ssh cat` (unknown total, asymptotic fill) or feeding compressed bytes
/// into `GzDecoder` for a local `.gz` file (known total, linear fill).
///
/// `saturation_permille` caps the bar at the boundary between the
/// open/decompress phase and the subsequent parse phase, which fills the
/// rest. With `total_bytes` known the bar fills linearly to that cap;
/// without it we inch up asymptotically so the user still sees motion.
#[cfg(not(target_arch = "wasm32"))]
pub struct ProgressReader<'a, R> {
    inner: R,
    progress: Option<&'a LoadProgress>,
    file_label: String,
    read_so_far: u64,
    last_status_at: u64,
    /// Total expected bytes flowing through this reader, if known.
    total_bytes: Option<u64>,
    /// Permille value the bar saturates at while this reader is active.
    saturation_permille: u32,
    /// Verb used in the status string ("Downloading", "Decompressing").
    verb: &'static str,
}

#[cfg(not(target_arch = "wasm32"))]
impl<'a, R: std::io::Read> std::io::Read for ProgressReader<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read_so_far += n as u64;
        if let Some(p) = self.progress {
            // Throttle status updates to ~every 256 KiB so we don't burn
            // mutex traffic on every read.
            const STATUS_INTERVAL: u64 = 256 * 1024;
            if self.read_so_far - self.last_status_at >= STATUS_INTERVAL || n == 0 {
                self.last_status_at = self.read_so_far;
                let sat = self.saturation_permille;
                if let Some(total) = self.total_bytes {
                    let denom = total.max(1);
                    let done = self.read_so_far.min(total);
                    p.set_status(format!(
                        "{} {}: {} / {}",
                        self.verb,
                        self.file_label,
                        fmt_bytes(done),
                        fmt_bytes(total),
                    ));
                    let permille = (done as f64 / denom as f64 * sat as f64) as u32;
                    p.store_permille(permille.min(sat));
                } else {
                    p.set_status(format!(
                        "{} {}: {}",
                        self.verb,
                        self.file_label,
                        fmt_bytes(self.read_so_far),
                    ));
                    // Inch the bar forward asymptotically so the user sees
                    // motion even though we don't know the total.
                    let scale = sat as f64;
                    let inched = (scale
                        - scale * (-(self.read_so_far as f64) / (8.0 * 1024.0 * 1024.0)).exp())
                        as u32;
                    p.store_permille(inched.min(sat));
                }
            }
        }
        Ok(n)
    }
}

/// Inflate every gzip member of `reader` into unlinked temp files, one
/// per capture. See [`GzipSplit`].
#[cfg(not(target_arch = "wasm32"))]
fn spool_gzip_members<R: BufRead>(reader: R, outer: &str) -> Result<Vec<(String, std::fs::File)>> {
    let mut split = GzipSplit::default();
    let mut incoming = Some(reader);
    let mut used = std::collections::HashSet::new();
    let mut parts: Vec<(String, std::fs::File)> = Vec::new();
    let mut current: Option<(String, std::fs::File)> = None;
    while let Some(r) = incoming.take() {
        let Some(mut member) = open_gzip_member(r, &mut split)? else {
            break;
        };
        if member.new_capture {
            if let Some(done) = current.take() {
                parts.push(done);
            }
            let name = unique_part_name(
                &mut used,
                member_display_name(member.name.as_deref(), outer, member.capture_index),
            );
            let mut file = tempfile::tempfile()?;
            std::io::copy(&mut member, &mut file)?;
            current = Some((name, file));
        } else {
            let (_, file) = current
                .as_mut()
                .expect("a continuing gzip member follows a capture");
            std::io::copy(&mut member, file)?;
        }
        incoming = Some(member.into_inner());
    }
    if let Some(done) = current.take() {
        parts.push(done);
    }
    if parts.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "empty gzip stream",
        ));
    }
    Ok(parts)
}

#[cfg(not(target_arch = "wasm32"))]
fn map_temp_file(file: &std::fs::File) -> Result<FileData> {
    // Spooling the decompressed capture to an (unlinked) temp file and
    // mmapping that, rather than holding it in a `Vec<u8>`, keeps a
    // highly compressed capture from sitting on the heap next to the
    // parser's own allocations. mmap lets the OS reclaim those pages
    // under pressure instead of only being able to swap them.
    let mmap = unsafe { Mmap::map(file)? };
    #[cfg(unix)]
    {
        let _ = mmap.advise(memmap2::Advice::Sequential);
    }
    Ok(FileData::Mmap(mmap))
}

#[cfg(not(target_arch = "wasm32"))]
fn map_temp_files(files: Vec<(String, std::fs::File)>) -> Result<Vec<(String, FileData)>> {
    files
        .into_iter()
        .map(|(name, file)| Ok((name, map_temp_file(&file)?)))
        .collect()
}

fn path_basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Stream a remote file via `ssh <host> cat <path>`. A `.gz` path is
/// inflated member by member into one temp file per capture. ssh's
/// stderr is captured and folded into any returned error so the GUI
/// loading screen shows useful diagnostics.
#[cfg(not(target_arch = "wasm32"))]
fn fetch_remote_parts(
    host: &str,
    path: &str,
    progress: Option<&LoadProgress>,
) -> Result<Vec<(String, FileData)>> {
    if let Some(p) = progress {
        p.set_status(format!("Connecting to {host}…"));
    }

    let remote_cmd = format!("cat -- {}", shell_quote(path));
    let mut child = Command::new("ssh")
        .args(["-n", "--", host, &remote_cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("failed to spawn ssh for {host}:{path}: {e}"),
            )
        })?;

    let stdout = child.stdout.take().expect("ssh stdout was piped");
    let mut stderr = child.stderr.take().expect("ssh stderr was piped");

    // Drain stderr on a side thread so its pipe never blocks the child
    // and we have its contents to report on failure.
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        buf
    });

    let basename = path_basename(path);

    let mut reader = ProgressReader {
        inner: stdout,
        progress,
        file_label: basename.clone(),
        read_so_far: 0,
        last_status_at: 0,
        total_bytes: None,
        saturation_permille: 40,
        verb: "Downloading",
    };

    let tmp_result: Result<Vec<(String, std::fs::File)>> = (|| {
        if path.ends_with(".gz") {
            let buffered = std::io::BufReader::new(&mut reader);
            spool_gzip_members(buffered, &basename)
        } else {
            let mut tmp = tempfile::tempfile()?;
            std::io::copy(&mut reader, &mut tmp)?;
            Ok(vec![(basename.clone(), tmp)])
        }
    })();

    let total_downloaded = reader.read_so_far;

    let status = child.wait()?;
    let stderr_text = stderr_handle.join().unwrap_or_default();
    let stderr_trimmed = stderr_text.trim();

    if !status.success() {
        let suffix = if stderr_trimmed.is_empty() {
            String::new()
        } else {
            format!(": {stderr_trimmed}")
        };
        return Err(std::io::Error::other(format!(
            "ssh {host} cat {path} exited with {status}{suffix}"
        )));
    }

    let files = tmp_result.map_err(|e| {
        let suffix = if stderr_trimmed.is_empty() {
            String::new()
        } else {
            format!(" ({stderr_trimmed})")
        };
        std::io::Error::new(e.kind(), format!("reading {host}:{path}: {e}{suffix}"))
    })?;

    if let Some(p) = progress {
        p.set_status(format!("Downloaded {}", fmt_bytes(total_downloaded)));
    }

    map_temp_files(files)
}

/// Stream a remote file via `ssh <host> cat <path>` into memory. If the
/// path ends in `.gz`, decompresses on the fly so we never hold the
/// compressed buffer. ssh's stderr is captured and folded into any
/// returned error so the GUI loading screen shows useful diagnostics.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_remote(host: &str, path: &str, progress: Option<&LoadProgress>) -> Result<FileData> {
    single_capture(
        fetch_remote_parts(host, path, progress)?,
        &format!("{host}:{path}"),
    )
}

/// Open one path as the captures it contains. A plain `.elog` is one
/// entry. A `.gz` is one entry per compressed capture inside it, in
/// member order; parallel-gzip chunks of a single capture stay one entry.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_captures(
    filepath: &str,
    progress: Option<&LoadProgress>,
) -> Result<Vec<(String, FileData)>> {
    if let Some((host, path)) = is_remote_path(filepath) {
        return fetch_remote_parts(host, path, progress);
    }
    if filepath.ends_with(".gz") {
        let file = File::open(filepath)?;
        let total = file.metadata().ok().map(|m| m.len());
        let basename = path_basename(filepath);
        let mut reader = ProgressReader {
            inner: file,
            progress,
            file_label: basename.clone(),
            read_so_far: 0,
            last_status_at: 0,
            total_bytes: total,
            saturation_permille: 50,
            verb: "Decompressing",
        };
        let buffered = std::io::BufReader::new(&mut reader);
        let files =
            spool_gzip_members(buffered, &basename).map_err(|e| gzip_failed(filepath, e))?;
        return map_temp_files(files);
    }
    let file = File::open(filepath)?;
    // SAFETY: We treat the mapped region as read-only and do not hold references
    // across the function boundary.  The file is not concurrently modified.
    let mmap = unsafe { Mmap::map(&file)? };
    #[cfg(unix)]
    {
        let _ = mmap.advise(memmap2::Advice::Sequential);
    }
    Ok(vec![(path_basename(filepath), FileData::Mmap(mmap))])
}

#[cfg(not(target_arch = "wasm32"))]
fn single_capture(mut parts: Vec<(String, FileData)>, filepath: &str) -> Result<FileData> {
    if parts.len() == 1 {
        Ok(parts.pop().unwrap().1)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{filepath}: gzip contains {} captures", parts.len()),
        ))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn open_file(filepath: &str, progress: Option<&LoadProgress>) -> Result<FileData> {
    single_capture(open_captures(filepath, progress)?, filepath)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn parse_elog(filepath: &str) -> Result<ParsedCapture> {
    let progress = LoadProgress::new();
    parse_elog_with_progress(filepath, &progress)
}

/// Like `parse_elog`, but reports progress (0–1000 permille + status text)
/// via a shared [`LoadProgress`]. A `.gz` with several compressed captures
/// is merged as one rolled set.
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_elog_with_progress(filepath: &str, progress: &LoadProgress) -> Result<ParsedCapture> {
    progress.store_permille(0);
    let parts = open_captures(filepath, Some(progress))?;
    parse_opened_parts(filepath, parts, progress)
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_opened_parts(
    filepath: &str,
    parts: Vec<(String, FileData)>,
    progress: &LoadProgress,
) -> Result<ParsedCapture> {
    if parts.len() <= 1 {
        let data = parts
            .into_iter()
            .next()
            .map(|(_, data)| data)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "no capture files given")
            })?;
        progress.store_permille(50);
        let mut capture = parse_elog_core(&data, Some(progress))?;
        attach_source(&mut capture.8, &capture.9, filepath);
        return Ok(capture);
    }

    let (parent_lo, parent_hi) = progress.window();
    let span = parent_hi.saturating_sub(parent_lo);
    let n = parts.len() as u32;
    let parse_lo = parent_lo + span * 50 / 100;
    let merge_lo = parent_lo + span * 90 / 100;
    let mut captures = Vec::with_capacity(parts.len());
    for (i, (name, data)) in parts.into_iter().enumerate() {
        let i = i as u32;
        let lo = parse_lo + (merge_lo - parse_lo) * i / n;
        let hi = parse_lo + (merge_lo - parse_lo) * (i + 1) / n;
        progress.set_window(lo, hi);
        progress.store_permille(0);
        progress.set_status(format!("Parsing {name}…"));
        let mut capture = parse_elog_core(&data, Some(progress))?;
        attach_source_named(&mut capture.8, &capture.9, name);
        captures.push(capture);
    }
    progress.set_window(merge_lo, parent_hi);
    progress.store_permille(0);
    progress.set_status(format!("Merging {n} captures…"));
    let merged = merge_captures(captures)?;
    progress.set_window(parent_lo, parent_hi);
    Ok(merged)
}

/// Record which file a parse came from, so a merged capture can report
/// its constituent files (see [`CaptureSource`]).
#[cfg(not(target_arch = "wasm32"))]
fn attach_source(info: &mut FileInfo, providers: &[Provider], filepath: &str) {
    let name = match is_remote_path(filepath) {
        Some(_) => filepath.to_string(),
        None => std::path::Path::new(filepath)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| filepath.to_string()),
    };
    attach_source_named(info, providers, name);
}

/// Label a single-file parse result with its display `name`, as the path
/// loaders do; the web loader calls this directly with a URL or picked
/// file name before [`merge_captures`].
pub fn attach_source_named(info: &mut FileInfo, providers: &[Provider], name: String) {
    info.sources = vec![CaptureSource {
        // Where this file's events actually start, which a rolled file
        // repeating the first file's header anchor doesn't say.
        capture_start: capture_origin(providers, info),
        capture_end: info.capture_end,
        event_count: info.event_count,
        name,
    }];
}

/// Load a rolled capture set as one continuous capture: parse each file,
/// then fold the results with [`merge_captures`] (which orders them on
/// the time axis and rejects files from different `elog` runs). Repeated
/// paths are loaded once. A single path behaves exactly like
/// [`parse_elog`].
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_elogs(filepaths: &[String]) -> Result<ParsedCapture> {
    let progress = LoadProgress::new();
    parse_elogs_with_progress(filepaths, &progress)
}

/// Progress-reporting counterpart to [`parse_elogs`]. Each file gets an
/// equal slice of the bar and a `[i/n]` status prefix naming it.
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_elogs_with_progress(
    filepaths: &[String],
    progress: &LoadProgress,
) -> Result<ParsedCapture> {
    let mut unique: Vec<&String> = Vec::with_capacity(filepaths.len());
    for path in filepaths {
        if !unique.contains(&path) {
            unique.push(path);
        }
    }
    let Some((&first, rest)) = unique.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no capture files given",
        ));
    };
    if rest.is_empty() {
        progress.set_window(0, 1000);
        return parse_elog_with_progress(first, progress);
    }

    // Merging keeps every file's parse result live at once, so the peak
    // footprint is the whole set rather than one file. Reserve the last
    // tenth of the bar for the fold itself, which on a large set is
    // minutes of sorting and rehashing, not an instant.
    let n = unique.len() as u32;
    let mut captures = Vec::with_capacity(unique.len());
    for (i, path) in unique.iter().enumerate() {
        let idx = i as u32;
        progress.set_prefix(format!("[{}/{n}] ", idx + 1));
        progress.set_window(idx * 900 / n, (idx + 1) * 900 / n);
        captures.push(parse_elog_with_progress(path, progress)?);
    }
    progress.set_prefix("");
    progress.set_window(900, 1000);
    progress.store_permille(0);
    progress.set_status(format!("Merging {n} captures…"));
    let merged = merge_captures(captures)?;
    progress.set_window(0, 1000);
    Ok(merged)
}

/// Parse from an in-memory byte slice. This is the path the wasm/web
/// build uses (the user uploads a `.elog` or `.elog.gz` via the
/// browser file picker; the bytes never live on a filesystem). Native
/// callers can also use this if they already have the bytes.
///
/// Handles a leading gzip magic transparently — on web we decompress
/// in-memory using `flate2`'s pure-rust backend. Several compressed
/// captures in one gzip are merged as one rolled set.
pub fn parse_elog_bytes_with_progress(
    data: &[u8],
    progress: &LoadProgress,
) -> Result<ParsedCapture> {
    progress.store_permille(0);
    // gzip magic is 0x1f 0x8b. Detect by sniffing rather than relying
    // on a filename suffix, since the web file picker may surface
    // ".elog" with gzipped bytes.
    if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        return parse_gzip_bytes(data, progress);
    }
    progress.store_permille(50);
    parse_elog_core(data, Some(progress))
}

fn parse_gzip_bytes(data: &[u8], progress: &LoadProgress) -> Result<ParsedCapture> {
    progress.set_status("Decompressing…");
    let mut split = GzipSplit::default();
    let mut rest = data;
    let mut used = std::collections::HashSet::new();
    let mut chunks: Vec<(String, Vec<u8>)> = Vec::new();
    let mut current: Option<(String, Vec<u8>)> = None;
    while let Some(mut member) =
        open_gzip_member(rest, &mut split).map_err(|e| gzip_failed("capture", e))?
    {
        if member.new_capture {
            if let Some(done) = current.take() {
                chunks.push(done);
            }
            let name = unique_part_name(
                &mut used,
                member_display_name(member.name.as_deref(), "capture", member.capture_index),
            );
            let mut buf = Vec::new();
            std::io::copy(&mut member, &mut buf).map_err(|e| gzip_failed("capture", e))?;
            current = Some((name, buf));
        } else {
            let buf = &mut current
                .as_mut()
                .expect("a continuing gzip member follows a capture")
                .1;
            std::io::copy(&mut member, buf).map_err(|e| gzip_failed("capture", e))?;
        }
        rest = member.into_inner();
    }
    if let Some(done) = current.take() {
        chunks.push(done);
    }
    if chunks.is_empty() {
        return Err(gzip_failed(
            "capture",
            std::io::Error::new(std::io::ErrorKind::InvalidData, "empty gzip stream"),
        ));
    }
    progress.store_permille(50);
    if chunks.len() == 1 {
        return parse_elog_core(&chunks.into_iter().next().unwrap().1, Some(progress));
    }
    let n = chunks.len() as u32;
    let mut captures = Vec::with_capacity(chunks.len());
    for (name, buf) in chunks {
        progress.set_status(format!("Parsing {name}…"));
        let mut capture = parse_elog_core(&buf, Some(progress))?;
        attach_source_named(&mut capture.8, &capture.9, name);
        captures.push(capture);
    }
    progress.set_status(format!("Merging {n} captures…"));
    merge_captures(captures)
}

fn gzip_failed(src: &str, e: std::io::Error) -> std::io::Error {
    let hint = if e.kind() == std::io::ErrorKind::UnexpectedEof {
        "; capture is truncated or still being written"
    } else {
        ""
    };
    std::io::Error::new(
        e.kind(),
        format!("{src}: gzip decompress failed: {e}{hint}"),
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    #[test]
    fn truncated_gzip_names_the_file() {
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&[0u8; 64 * 1024]).unwrap();
        let mut gz = enc.finish().unwrap();
        gz.truncate(gz.len() / 2);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.elog.gz");
        std::fs::write(&path, &gz).unwrap();
        let err = match open_file(path.to_str().unwrap(), None) {
            Ok(_) => panic!("truncated gzip opened"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(msg.contains("partial.elog.gz"), "{msg}");
        assert!(msg.contains("gzip decompress failed"), "{msg}");
        assert!(
            msg.contains("truncated") || msg.contains("unexpected end"),
            "{msg}"
        );
    }

    fn tiny_elog(capture_start: u64, event_count: u64) -> Vec<u8> {
        let start_utc = 1_700_000_000_000_000u64 + capture_start;
        let mut out = Vec::new();
        out.extend_from_slice(b"ELOG");
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&capture_start.to_le_bytes());
        out.extend_from_slice(&start_utc.to_le_bytes());
        out.extend_from_slice(&event_count.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out
    }

    fn gz_named(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut enc = flate2::GzBuilder::new()
            .filename(name)
            .write(Vec::new(), Compression::fast());
        enc.write_all(bytes).unwrap();
        enc.finish().unwrap()
    }

    fn gz_raw(bytes: &[u8]) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(bytes).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn concatenated_gzip_members_merge() {
        let mut gz = gz_named("a.elog", &tiny_elog(1_000_000, 10));
        gz.extend(gz_named("b.elog", &tiny_elog(2_000_000, 20)));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bundle.elog.gz");
        std::fs::write(&path, &gz).unwrap();
        let cap = parse_elog(path.to_str().unwrap()).unwrap();
        assert_eq!(cap.8.event_count, 30);
        let names: Vec<&str> = cap.8.sources.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a.elog", "b.elog"]);
    }

    #[test]
    fn concatenated_gzip_without_names_uses_outer_name() {
        let mut gz = gz_raw(&tiny_elog(1_000_000, 1));
        gz.extend(gz_raw(&tiny_elog(2_000_000, 2)));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bundle.elog.gz");
        std::fs::write(&path, &gz).unwrap();
        let cap = parse_elog(path.to_str().unwrap()).unwrap();
        assert_eq!(cap.8.event_count, 3);
        let names: Vec<&str> = cap.8.sources.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["bundle.elog.gz #1", "bundle.elog.gz #2"]);
    }

    #[test]
    fn split_gzip_members_are_one_capture() {
        let raw = tiny_elog(1_000_000, 7);
        let mut gz = gz_raw(&raw[..20]);
        gz.extend(gz_raw(&raw[20..]));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.elog.gz");
        std::fs::write(&path, &gz).unwrap();
        let cap = parse_elog(path.to_str().unwrap()).unwrap();
        assert_eq!(cap.8.event_count, 7);
        assert_eq!(cap.8.sources.len(), 1);
        assert_eq!(cap.8.sources[0].name, "one.elog.gz");
    }

    #[test]
    fn concatenated_gzip_bytes_merge() {
        let mut gz = gz_named("a.elog", &tiny_elog(1_000_000, 4));
        gz.extend(gz_named("b.elog", &tiny_elog(2_000_000, 6)));
        let progress = LoadProgress::new();
        let cap = parse_elog_bytes_with_progress(&gz, &progress).unwrap();
        assert_eq!(cap.8.event_count, 10);
        let names: Vec<&str> = cap.8.sources.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a.elog", "b.elog"]);
    }

    #[test]
    fn single_gzip_bytes_leave_sources_empty() {
        let gz = gz_raw(&tiny_elog(1_000_000, 4));
        let progress = LoadProgress::new();
        let cap = parse_elog_bytes_with_progress(&gz, &progress).unwrap();
        assert_eq!(cap.8.event_count, 4);
        assert!(cap.8.sources.is_empty());
    }
}
