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

//! Folding several [`ParsedCapture`]s into one continuous capture.
//!
//! `elog --roll-*` splits a long capture into a numbered set of files.
//! Each one is a complete `.elog` with its own header and provider
//! table, but every event timestamp in the set comes from the same
//! kernel monotonic clock, so the events form one timeline that merely
//! happens to be stored in pieces. This module reassembles it:
//! per-file parse results in, one capture out, with the earliest file's
//! anchor as the shared origin and every stream re-ordered onto the
//! joint axis.
//!
//! Two invariants make the merge safe, and [`merge_captures`] refuses
//! the input when either fails rather than producing a plausible-looking
//! nonsense timeline:
//!
//! 1. **One clock domain.** `start_utc_us - capture_start` is the
//!    monotonic-to-wall-clock offset — effectively a boot fingerprint.
//!    Files from one `elog` run agree on it to the microsecond; files
//!    from either side of a reboot disagree by the whole uptime.
//! 2. **One provider numbering.** Provider ids are assigned per file, and
//!    `TcpStateSample`/`EllaEvent`/`GenericEvent` and the profile maps
//!    key on them. A rolled set carries the same table forward, so ids
//!    that appear in more than one file must name the same provider.
//!
//! Both the ordering and the clock check work off event timestamps and
//! tolerate either header-anchor convention `elog` has used for rolled
//! files: a per-file pair that advances with the roll (what current
//! captures carry), or the first file's pair repeated verbatim in every
//! later file — see "Drained ring backlog" in `docs/file-format.md`.
//!
//! Every `merge_*` helper below destructures its input struct field by
//! field, with no `..` rest pattern. Adding a field to any of the merged
//! types therefore fails to compile until it is given a merge rule,
//! which is what keeps this module from silently dropping data as the
//! parser grows.

#[allow(unused_imports)]
use super::*;

/// How far two files' monotonic-to-wall-clock offsets may disagree and
/// still count as the same boot. Sized to swallow any `gettimeofday()`
/// drift or NTP slew across a long capture while staying far below the
/// uptime-sized gap a reboot produces.
pub const CLOCK_DOMAIN_TOLERANCE_US: u64 = 60_000_000;

fn invalid(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

/// Merge per-file parse results into a single capture spanning all of
/// them. Input order doesn't matter — the captures are sorted onto the
/// time axis by their own origins first.
///
/// Errors when the captures don't belong to the same `elog` run: see the
/// two invariants in the module docs.
pub fn merge_captures(mut captures: Vec<ParsedCapture>) -> Result<ParsedCapture> {
    if captures.is_empty() {
        return Err(invalid("no captures to merge".to_string()));
    }
    // Order by first event rather than by header origin: a rolled file
    // may repeat the first file's header anchor, which would tie (or
    // invert, once the first file's backlog is trimmed off its origin)
    // while the event streams are plainly an hour apart.
    captures.sort_by_key(|c| capture_origin(&c.9, &c.8));
    check_one_clock_domain(&captures)?;

    let mut merged = captures.remove(0);
    for capture in captures {
        merge_into(&mut merged, capture)?;
    }
    resort_sessions(&mut merged.0);
    Ok(merged)
}

/// Where a capture's timeline really begins: the later of the parser's
/// origin (the header anchor, trimmed past any drained backlog) and its
/// first event. The two differ only when a rolled file repeats the first
/// file's header anchor, in which case the events are the honest answer.
pub fn capture_origin(providers: &[Provider], info: &FileInfo) -> u64 {
    let first_event = providers.iter().filter_map(|p| p.first_ts).min();
    info.capture_start.max(first_event.unwrap_or(0))
}

/// Reject a set spanning a reboot (or two different machines), where the
/// monotonic timestamps are numerically comparable but mean nothing
/// relative to each other.
///
/// The two candidate anchors are compared independently and the closer
/// agreement wins, because exactly one of them lines up under each of
/// `elog`'s rolled-header conventions: with a per-file anchor pair the
/// backlog-trimmed origins agree, and with the first file's pair
/// repeated the raw header values do. A reboot moves the monotonic base
/// by the whole uptime, so it fails both.
fn check_one_clock_domain(captures: &[ParsedCapture]) -> Result<()> {
    let header_offset = |i: &FileInfo| i.start_utc_us as i128 - i.header_capture_start as i128;
    let live_offset = |i: &FileInfo| i.start_utc_us as i128 - i.capture_start as i128;
    let first = &captures[0].8;
    let (base_header, base_live) = (header_offset(first), live_offset(first));
    for capture in &captures[1..] {
        let info = &capture.8;
        let skew = (header_offset(info) - base_header)
            .unsigned_abs()
            .min((live_offset(info) - base_live).unsigned_abs());
        if skew > CLOCK_DOMAIN_TOLERANCE_US as u128 {
            return Err(invalid(
                "These captures are from different boots of the same machine. \
                 Open each file individually instead."
                    .to_string(),
            ));
        }
    }
    Ok(())
}

fn merge_into(dst: &mut ParsedCapture, src: ParsedCapture) -> Result<()> {
    let (
        s_sessions,
        s_agg,
        s_pace,
        s_cpu,
        s_if,
        s_tcp,
        s_ella,
        s_nginx,
        s_info,
        s_providers,
        s_schema,
    ) = src;
    let (
        d_sessions,
        d_agg,
        d_pace,
        d_cpu,
        d_if,
        d_tcp,
        d_ella,
        d_nginx,
        d_info,
        d_providers,
        d_schema,
    ) = dst;

    merge_providers(d_providers, s_providers)?;
    merge_sessions(d_sessions, s_sessions);
    append_by_ts(d_agg, s_agg, |s| s.ts);
    append_by_ts(d_pace, s_pace, |s| s.ts);
    merge_cpu(d_cpu, s_cpu);
    merge_if(d_if, s_if);
    merge_tcp_samples(d_tcp, s_tcp);
    merge_ella(d_ella, s_ella);
    merge_nginx(d_nginx, s_nginx);
    merge_schema(d_schema, s_schema);
    merge_file_info(d_info, s_info);
    Ok(())
}

/// Append `src` onto `dst`, both already in timestamp order. Ordering
/// only needs restoring when the two files' spans interleave, which a
/// clean rolled set never does.
fn append_by_ts<T>(dst: &mut Vec<T>, src: Vec<T>, ts: impl Fn(&T) -> u64) {
    let interleaved = match (dst.last(), src.first()) {
        (Some(last), Some(first)) => ts(first) < ts(last),
        _ => false,
    };
    dst.extend(src);
    if interleaved {
        dst.sort_by_key(|e| ts(e));
    }
}

/// A connection that spans a roll boundary keeps its kernel session id,
/// so its events land in the same entry and just need concatenating.
fn merge_sessions(dst: &mut Sessions, src: Sessions) {
    dst.reserve(src.len());
    for (session_id, events) in src {
        match dst.get_mut(&session_id) {
            Some(existing) => existing.extend(events),
            None => {
                dst.insert(session_id, events);
            }
        }
    }
}

/// Restore per-session timestamp order after the concatenation above.
/// Sessions that only ever appeared in one file are already sorted, so
/// this is a scan for most of the map and a sort for the few connections
/// that actually crossed a boundary out of order.
fn resort_sessions(sessions: &mut Sessions) {
    let sort_one = |events: &mut Vec<Event>| {
        if !events.is_sorted_by_key(|e| e.ts) {
            events.sort_by_key(|e| e.ts);
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    sessions
        .par_iter_mut()
        .for_each(|(_, events)| sort_one(events));
    #[cfg(target_arch = "wasm32")]
    sessions.iter_mut().for_each(|(_, events)| sort_one(events));
}

fn merge_providers(dst: &mut Vec<Provider>, src: Vec<Provider>) -> Result<()> {
    for provider in src {
        let Provider {
            id,
            name,
            event_count,
            first_ts,
            last_ts,
            session_count,
            byte_size,
            schema_name,
            schema_event_count,
            schema_divergent_events,
            schema_new_fields,
            events,
        } = provider;

        let Some(existing) = dst.iter_mut().find(|p| p.id == id) else {
            dst.push(Provider {
                id,
                name,
                event_count,
                first_ts,
                last_ts,
                session_count,
                byte_size,
                schema_name,
                schema_event_count,
                schema_divergent_events,
                schema_new_fields,
                events,
            });
            continue;
        };

        if existing.name != name {
            return Err(invalid(format!(
                "captures are not from the same elog run: provider id {id} is {:?} in one file \
                 and {name:?} in another. Open these files separately.",
                existing.name,
            )));
        }
        existing.event_count += event_count;
        existing.byte_size += byte_size;
        // Sums the per-file counts, so a connection live across a roll
        // boundary is counted once per file it appears in.
        existing.session_count += session_count;
        existing.first_ts = min_opt(existing.first_ts, first_ts);
        existing.last_ts = max_opt(existing.last_ts, last_ts);
        // A provider's schema is announced once per file, so the first
        // file that resolved one describes the whole merged capture.
        if existing.schema_name.is_none() {
            existing.schema_name = schema_name;
            existing.schema_event_count = schema_event_count;
            existing.schema_divergent_events = schema_divergent_events;
            existing.schema_new_fields = schema_new_fields;
        }
        merge_event_stats(&mut existing.events, events);
    }
    Ok(())
}

fn merge_event_stats(dst: &mut Vec<EventStat>, src: Vec<EventStat>) {
    for ev in src {
        let Some(existing) = dst.iter_mut().find(|e| e.event_id == ev.event_id) else {
            dst.push(ev);
            continue;
        };
        let dst_seen = existing.count > 0;
        let src_seen = ev.count > 0;
        existing.count += ev.count;
        existing.bytes += ev.bytes;
        existing.payload_bytes += ev.payload_bytes;
        existing.defined_only = existing.count == 0;
        if src_seen && !dst_seen {
            existing.min_len = ev.min_len;
            existing.max_len = ev.max_len;
            existing.first_ts = ev.first_ts;
            existing.last_ts = ev.last_ts;
        } else if src_seen && dst_seen {
            existing.min_len = existing.min_len.min(ev.min_len);
            existing.max_len = existing.max_len.max(ev.max_len);
            existing.first_ts = min_opt(existing.first_ts, ev.first_ts);
            existing.last_ts = max_opt(existing.last_ts, ev.last_ts);
        }
        if existing.level.is_none() {
            existing.level = ev.level;
        }
        if existing.keywords.is_empty() {
            existing.keywords = ev.keywords;
        }
        if existing.fields.is_empty() {
            existing.fields = ev.fields;
        }
        if existing.format.is_empty() {
            existing.format = ev.format;
        }
        if existing.name.starts_with("id_") && !ev.name.starts_with("id_") {
            existing.name = ev.name;
        }
    }
}

fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (some, None) | (None, some) => some,
    }
}

fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (some, None) | (None, some) => some,
    }
}

/// `elog --roll-time` re-triggers a dump-state replay at every boundary,
/// so each file re-declares the identity records (threads, linker files,
/// symbol tables, process maps) that the parser already treats as
/// upserts within one file — see the identity-maps note in `core.rs`.
/// The later file wins for the same reason it does mid-file: a repeat
/// sighting of a reusable key is the newer truth.
fn merge_cpu(dst: &mut CpuProfile, src: CpuProfile) {
    let CpuProfile {
        samples,
        threads,
        proc_execs,
        proc_titles,
        kernel,
        sample_rates: _,
        linker_files,
        symbols,
        user_files,
        proc_maps,
        user_symbols,
        pack_symbols,
        pack_user_symbols,
        pack_refs,
        source_files,
        source_lines,
        overflow_dropped,
    } = src;

    append_by_ts(&mut dst.samples, samples, |s| s.ts);
    dst.threads.extend(threads);
    if kernel.is_some() {
        dst.kernel = kernel;
    }
    dst.linker_files.extend(linker_files);
    dst.symbols.extend(symbols);
    dst.user_files.extend(user_files);
    dst.user_symbols.extend(user_symbols);
    dst.pack_symbols.append(&pack_symbols);
    dst.pack_user_symbols.append(&pack_user_symbols);
    dst.pack_refs.extend(pack_refs);
    dst.source_files.extend(source_files);
    for (key, lines) in source_lines {
        let entries = dst.source_lines.entry(key).or_default();
        entries.extend(lines);
        entries.sort_unstable_by_key(|line| line.file_offset);
        entries.dedup_by(|a, b| a.file_offset == b.file_offset);
    }
    // A pid's mappings are legitimately multi-valued, so these union
    // rather than replace, deduplicating re-dumped entries the same way
    // the parser does on push.
    for (pid, maps) in proc_maps {
        let entries = dst.proc_maps.entry(pid).or_default();
        for map in maps {
            if !entries.contains(&map) {
                entries.push(map);
            }
        }
        entries.sort_unstable_by_key(|pm| pm.base);
    }
    // PROC_EXEC history unions across rolls; dump_state may replay the
    // same (ts, comm) at a roll boundary.
    for (pid, execs) in proc_execs {
        let entries = dst.proc_execs.entry(pid).or_default();
        for e in execs {
            if !entries.contains(&e) {
                entries.push(e);
            }
        }
        entries.sort_unstable_by_key(|e| e.ts);
    }
    for (pid, titles) in proc_titles {
        let entries = dst.proc_titles.entry(pid).or_default();
        for t in titles {
            if !entries.contains(&t) {
                entries.push(t);
            }
        }
        entries.sort_unstable_by_key(|t| t.ts);
    }
    dst.overflow_dropped += overflow_dropped;
    dst.sample_rates =
        detect_sample_rates(&dst.samples, dst.kernel.as_ref().map(|k| k.sample_hz()));
}

fn merge_if(dst: &mut IfProfile, src: IfProfile) {
    let IfProfile {
        interfaces,
        samples,
        events,
        period_us,
        nifs,
    } = src;

    dst.interfaces.extend(interfaces);
    append_by_ts(&mut dst.samples, samples, |s| s.ts);
    append_by_ts(&mut dst.events, events, |e| e.ts);
    // Both come from IF_INFO, which a file only carries if its capture
    // window included one; keep the last real reading.
    if period_us != 0 {
        dst.period_us = period_us;
    }
    if nifs != 0 {
        dst.nifs = nifs;
    }
}

fn merge_tcp_samples(dst: &mut TcpSampleProfile, src: TcpSampleProfile) {
    let TcpSampleProfile {
        stacks,
        state_samples,
        rps_samples,
        gp_samples,
        pacing_rate_samples,
        realized_rate_samples,
        send_samples,
        lro_flush_samples,
        srtt_samples,
    } = src;

    dst.stacks.extend(stacks);
    append_by_ts(&mut dst.state_samples, state_samples, |s| s.ts);
    append_by_ts(&mut dst.rps_samples, rps_samples, |s| s.ts);
    append_by_ts(&mut dst.gp_samples, gp_samples, |s| s.ts);
    append_by_ts(&mut dst.pacing_rate_samples, pacing_rate_samples, |s| s.ts);
    append_by_ts(&mut dst.realized_rate_samples, realized_rate_samples, |s| {
        s.ts
    });
    append_by_ts(&mut dst.send_samples, send_samples, |s| s.ts);
    append_by_ts(&mut dst.lro_flush_samples, lro_flush_samples, |s| s.ts);
    append_by_ts(&mut dst.srtt_samples, srtt_samples, |s| s.ts);
}

fn merge_ella(dst: &mut EllaProfile, src: EllaProfile) {
    let EllaProfile {
        channels,
        events,
        counts,
        counts_total,
        channel_samples,
        frame_samples,
        offload_samples,
        instances,
        roles,
        live_subs,
    } = src;

    for (key, channel) in channels {
        match dst.channels.get_mut(&key) {
            Some(existing) => merge_ella_channel(existing, channel),
            None => {
                dst.channels.insert(key, channel);
            }
        }
    }
    append_by_ts(&mut dst.events, events, |e| e.ts);
    append_by_ts(&mut dst.counts, counts, |s| s.ts);
    append_by_ts(&mut dst.counts_total, counts_total, |s| s.ts);
    append_by_ts(&mut dst.channel_samples, channel_samples, |s| s.ts);
    append_by_ts(&mut dst.frame_samples, frame_samples, |s| s.ts);
    append_by_ts(&mut dst.offload_samples, offload_samples, |s| s.ts);
    dst.instances.extend(instances);
    dst.roles.extend(roles);
    dst.live_subs.extend(live_subs);
}

/// Fold a later file's view of a channel into the earlier one. A channel
/// live across a roll boundary is re-observed mid-life, so the later
/// file's SESSION_CREATE-derived identity fields are only there when it
/// caught a fresh create — take them when present and extend the
/// lifetime either way.
fn merge_ella_channel(dst: &mut EllaChannelInfo, src: EllaChannelInfo) {
    let EllaChannelInfo {
        provider_id: _,
        session_id: _,
        channel_name,
        version,
        first_ts,
        last_ts,
        event_count,
        channel_ids,
        last_subscribers,
        peak_subscribers,
        occ_by_inst,
        last_state,
        destroy_reason,
    } = src;

    if !channel_name.is_empty() {
        dst.channel_name = channel_name;
    }
    if version != 0 {
        dst.version = version;
    }
    dst.first_ts = dst.first_ts.min(first_ts);
    dst.last_ts = dst.last_ts.max(last_ts);
    dst.event_count += event_count;
    dst.channel_ids.extend(channel_ids);
    dst.last_subscribers = last_subscribers;
    dst.peak_subscribers = dst.peak_subscribers.max(peak_subscribers);
    dst.occ_by_inst.extend(occ_by_inst);
    if last_state.is_some() {
        dst.last_state = last_state;
    }
    if destroy_reason.is_some() {
        dst.destroy_reason = destroy_reason;
    }
}

/// Fold a later file's view of a request into the earlier one. A request
/// live across a roll boundary is re-observed via dump replay, so the later
/// file re-states its START (and HEADERS, once sent) — take the earliest
/// start, the latest completion, and the last-known status/length either way.
fn merge_nginx(dst: &mut NginxProfile, src: NginxProfile) {
    let NginxProfile {
        requests,
        conns,
        events,
    } = src;

    for (key, req) in requests {
        match dst.requests.get_mut(&key) {
            Some(existing) => merge_nginx_request(existing, req),
            None => {
                dst.requests.insert(key, req);
            }
        }
    }
    for (key, conn) in conns {
        match dst.conns.get_mut(&key) {
            Some(existing) => merge_nginx_conn(existing, conn),
            None => {
                dst.conns.insert(key, conn);
            }
        }
    }
    append_by_ts(&mut dst.events, events, |e| e.ts);
}

fn merge_nginx_request(dst: &mut NginxRequestInfo, src: NginxRequestInfo) {
    let NginxRequestInfo {
        provider_id: _,
        session_id: _,
        request: _,
        method,
        uri,
        range_start,
        range_end,
        start_ts,
        headers_ts,
        done_ts,
        status,
        content_length,
        bytes_sent,
        event_count,
        dev,
        detail,
        messages,
        cache,
        upstream,
        open_fail,
    } = src;

    if !method.is_empty() {
        dst.method = method;
    }
    if !uri.is_empty() {
        dst.uri = uri;
    }
    if range_start >= 0 {
        dst.range_start = range_start;
    }
    if range_end >= 0 {
        dst.range_end = range_end;
    }
    dst.start_ts = dst.start_ts.min(start_ts);
    if headers_ts.is_some() {
        dst.headers_ts = headers_ts;
    }
    if done_ts.is_some() {
        dst.done_ts = done_ts;
    }
    if status.is_some() {
        dst.status = status;
    }
    if content_length.is_some() {
        dst.content_length = content_length;
    }
    if bytes_sent.is_some() {
        dst.bytes_sent = bytes_sent;
    }
    dst.event_count += event_count;
    if dev.is_some() {
        dst.dev = dev;
    }
    if detail.is_some() {
        dst.detail = detail;
    }
    dst.messages.extend(messages);
    if cache.is_some() {
        dst.cache = cache;
    }
    for a in upstream {
        // An attempt open at the roll finishes in the later file.
        match dst
            .upstream
            .iter_mut()
            .find(|d| d.attempt == a.attempt && d.subrequest == a.subrequest && d.end_ts.is_none())
        {
            Some(d) if a.end_ts.is_some() => {
                d.end_ts = a.end_ts;
                d.ft = a.ft;
                d.status = a.status;
                d.connect_ms = a.connect_ms;
                d.header_ms = a.header_ms;
                d.bytes_received = a.bytes_received;
            }
            _ => dst.upstream.push(a),
        }
    }
    if open_fail.is_some() {
        dst.open_fail = open_fail;
    }
}

/// A connection live across a roll gets CONN_START (and TLS) replayed in
/// the later file; keep the earliest start and the later file's end.
fn merge_nginx_conn(dst: &mut NginxConnInfo, src: NginxConnInfo) {
    let NginxConnInfo {
        provider_id: _,
        session_id: _,
        start_ts,
        end_ts,
        server_port,
        flags,
        close_reason,
        requests,
        bytes_sent,
        duration_ms,
        tls,
        tls_fail,
        busy_defers,
        busy_defer_ms,
        sendfile_errors,
    } = src;

    dst.start_ts = match (dst.start_ts, start_ts) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    if server_port != 0 {
        dst.server_port = server_port;
        dst.flags = flags;
    }
    if end_ts.is_some() {
        dst.end_ts = end_ts;
        dst.close_reason = close_reason;
        dst.requests = requests;
        dst.bytes_sent = bytes_sent;
        dst.duration_ms = duration_ms;
    }
    if dst.tls.is_none() {
        dst.tls = tls;
    }
    if tls_fail.is_some() {
        dst.tls_fail = tls_fail;
    }
    dst.busy_defers += busy_defers;
    dst.busy_defer_ms += busy_defer_ms;
    dst.sendfile_errors += sendfile_errors;
}

fn merge_schema(dst: &mut SchemaCapture, src: SchemaCapture) {
    let SchemaCapture {
        hpts_wheel_events,
        generic_events,
        mem,
    } = src;

    append_by_ts(&mut dst.hpts_wheel_events, hpts_wheel_events, |e| e.ts);
    append_by_ts(&mut dst.generic_events, generic_events, |e| e.ts);
    append_by_ts(&mut dst.mem.infos, mem.infos, |e| e.ts);
    append_by_ts(&mut dst.mem.samples, mem.samples, |e| e.ts);
}

/// `dst` is the earlier capture, so it already holds the merged
/// timeline's origin (and the backlog trim that produced it); the later
/// file extends the span and contributes its totals.
fn merge_file_info(dst: &mut FileInfo, src: FileInfo) {
    let FileInfo {
        version,
        capture_start: _,
        header_capture_start: _,
        backlog_events: _,
        start_utc_us: _,
        capture_end,
        event_count,
        dropped,
        user_dropped_full,
        user_dropped_prejoin,
        user_dropped_late,
        parsed,
        skipped_other_providers,
        sources,
        machine,
    } = src;

    dst.version = dst.version.max(version);
    if dst.machine.is_none() {
        dst.machine = machine;
    }
    dst.capture_end = dst.capture_end.max(capture_end);
    dst.event_count += event_count;
    dst.dropped += dropped;
    dst.user_dropped_full += user_dropped_full;
    dst.user_dropped_prejoin += user_dropped_prejoin;
    dst.user_dropped_late += user_dropped_late;
    dst.parsed += parsed;
    dst.skipped_other_providers += skipped_other_providers;
    dst.sources.extend(sources);
}
