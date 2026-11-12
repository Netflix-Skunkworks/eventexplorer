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

/// Per-(provider, event) on-disk totals for one record shape.
#[derive(Default)]
struct SizeRow {
    count: u64,
    /// Sum of whole event records (32-byte header + payload).
    bytes: u64,
    payload_bytes: u64,
    min_len: usize,
    max_len: usize,
    name: String,
    level: Option<u8>,
    keywords: Option<u32>,
}

type SizeKey = (String, u32);

fn level_name(l: u8) -> &'static str {
    match l {
        1 => "ERROR",
        2 => "WARN",
        3 => "INFO",
        4 => "VERBOSE",
        5 => "TRACE",
        _ => "?",
    }
}

fn le_u16(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d[off], d[off + 1]])
}
fn le_u32(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]])
}

/// One file's contribution to the size table. Returns the byte offset
/// where event records start (i.e. the header's total size).
fn scan_file(
    data: &[u8],
    rows: &mut FxHashMap<SizeKey, SizeRow>,
    structs: &mut FxHashMap<SizeKey, (DecodeEvent, DecodeStruct, Vec<String>)>,
) -> Result<usize> {
    if data.len() < 32 || &data[0..4] != b"ELOG" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not an ELOG file",
        ));
    }
    let version = le_u32(data, 4);
    let mut off = if version >= 3 {
        let counters = le_u32(data, 32) as usize;
        if counters > HEADER_COUNTER_SANE_MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid header counter count {counters}"),
            ));
        }
        FILE_HDR_SIZE_V3 + counters * 8
    } else {
        FILE_HDR_SIZE
    };
    if off + 4 > data.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "truncated in provider count",
        ));
    }
    let prov_count = le_u32(data, off) as usize;
    off += 4;
    let row_size = if version >= 2 {
        PROVIDER_SIZE_V2
    } else {
        PROVIDER_SIZE
    };
    let mut prov_name: FxHashMap<u16, String> = FxHashMap::default();
    let mut prov_kind: FxHashMap<u16, ProviderKind> = FxHashMap::default();
    let mut prov_sid: FxHashMap<u16, [u8; SCHEMA_ID_LEN]> = FxHashMap::default();
    for _ in 0..prov_count {
        if off + row_size > data.len() {
            break;
        }
        let pid = le_u16(data, off);
        let name_bytes = &data[off + 2..off + 34];
        let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(32);
        let name = String::from_utf8_lossy(&name_bytes[..end]).to_string();
        prov_kind.insert(pid, provider_kind_for_name(&name));
        prov_name.insert(pid, name);
        if version >= 2 {
            let mut sid = [0u8; SCHEMA_ID_LEN];
            sid.copy_from_slice(&data[off + 34..off + 34 + SCHEMA_ID_LEN]);
            prov_sid.insert(pid, sid);
        }
        off += row_size;
    }
    let mut blob_by_sid: FxHashMap<[u8; SCHEMA_ID_LEN], ProviderSchema> = FxHashMap::default();
    if version >= 2 && off + 4 <= data.len() {
        let blob_count = le_u32(data, off) as usize;
        off += 4;
        for _ in 0..blob_count {
            if off + SCHEMA_ID_LEN + 4 > data.len() {
                break;
            }
            let mut sid = [0u8; SCHEMA_ID_LEN];
            sid.copy_from_slice(&data[off..off + SCHEMA_ID_LEN]);
            off += SCHEMA_ID_LEN;
            let blob_size = le_u32(data, off) as usize;
            off += 4;
            if blob_size > EVENTLOG_DECODE_BLOB_MAX || off + blob_size > data.len() {
                break;
            }
            let blob = &data[off..off + blob_size];
            off += blob_size;
            if compute_schema_id(blob) != sid {
                continue;
            }
            if let Some(schema) = ProviderSchema::parse(blob) {
                blob_by_sid.insert(sid, schema);
            }
        }
    }
    let header_bytes = off;
    let mut schema_by_pid: FxHashMap<u16, ProviderSchema> = FxHashMap::default();
    let resolve_schema =
        |pid: u16,
         prov_sid: &FxHashMap<u16, [u8; SCHEMA_ID_LEN]>,
         blob_by_sid: &FxHashMap<[u8; SCHEMA_ID_LEN], ProviderSchema>,
         schema_by_pid: &mut FxHashMap<u16, ProviderSchema>| {
            if schema_by_pid.contains_key(&pid) {
                return;
            }
            if let Some(sid) = prov_sid.get(&pid) {
                if let Some(schema) = blob_by_sid.get(sid) {
                    schema_by_pid.insert(pid, schema.clone());
                }
            }
        };
    let pids: Vec<u16> = prov_name.keys().copied().collect();
    for pid in pids {
        resolve_schema(pid, &prov_sid, &blob_by_sid, &mut schema_by_pid);
    }

    // v1 cpu-provider disambiguation, mirroring the main parser's
    // id1-vs-id3 pre-pass (split-schema captures put SAMPLE on id 1).
    let mut cpu_id1: u64 = 0;
    let mut cpu_id3: u64 = 0;
    let cpu_needs_scan = prov_kind
        .iter()
        .any(|(pid, k)| *k == ProviderKind::Cpu && !schema_by_pid.contains_key(pid));
    if cpu_needs_scan {
        let mut scan = header_bytes;
        while scan + EVENT_HDR_SIZE <= data.len() {
            let ev_len = le_u16(data, scan) as usize;
            if ev_len < EVENT_HDR_SIZE || scan + ev_len > data.len() {
                break;
            }
            if prov_kind.get(&le_u16(data, scan + 4)) == Some(&ProviderKind::Cpu) {
                match le_u32(data, scan + 24) {
                    1 => cpu_id1 += 1,
                    3 => cpu_id3 += 1,
                    _ => {}
                }
            }
            scan += ev_len;
        }
    }
    let cpu_split = cpu_id1 > cpu_id3;

    let mut walk = header_bytes;
    while walk + EVENT_HDR_SIZE <= data.len() {
        let ev_len = le_u16(data, walk) as usize;
        if ev_len < EVENT_HDR_SIZE || walk + ev_len > data.len() {
            break;
        }
        let pid = le_u16(data, walk + 4);
        let eid = le_u32(data, walk + 24);
        // In-band provider-table / decode-blob announcements (elog.5):
        // register them so late providers still resolve to names, and
        // account their own bytes under a pseudo-provider.
        if eid == EVT_PROVIDER_INFO || eid == EVT_PROVIDER_BLOB {
            let payload = &data[walk + EVENT_HDR_SIZE..walk + ev_len];
            if eid == EVT_PROVIDER_INFO && payload.len() >= PROVIDER_SIZE_V2 {
                let npid = le_u16(payload, 0);
                let end = payload[2..34].iter().position(|&b| b == 0).unwrap_or(32) + 2;
                let name = String::from_utf8_lossy(&payload[2..end]).to_string();
                prov_kind.insert(npid, provider_kind_for_name(&name));
                prov_name.insert(npid, name);
                let mut sid = [0u8; SCHEMA_ID_LEN];
                sid.copy_from_slice(&payload[34..34 + SCHEMA_ID_LEN]);
                prov_sid.insert(npid, sid);
                resolve_schema(npid, &prov_sid, &blob_by_sid, &mut schema_by_pid);
            } else if eid == EVT_PROVIDER_BLOB && payload.len() >= SCHEMA_ID_LEN + 4 {
                let mut sid = [0u8; SCHEMA_ID_LEN];
                sid.copy_from_slice(&payload[..SCHEMA_ID_LEN]);
                let blob_size = le_u32(payload, SCHEMA_ID_LEN) as usize;
                if blob_size <= EVENTLOG_DECODE_BLOB_MAX
                    && payload.len() == SCHEMA_ID_LEN + 4 + blob_size
                {
                    let blob = &payload[SCHEMA_ID_LEN + 4..];
                    if compute_schema_id(blob) == sid {
                        if let Some(schema) = ProviderSchema::parse(blob) {
                            for (&p, &psid) in &prov_sid {
                                if psid == sid {
                                    schema_by_pid.insert(p, schema.clone());
                                }
                            }
                            blob_by_sid.insert(sid, schema);
                        }
                    }
                }
            }
            let key = ("(elog)".to_string(), eid);
            let row = rows.entry(key).or_default();
            row.count += 1;
            row.bytes += ev_len as u64;
            row.payload_bytes += (ev_len - EVENT_HDR_SIZE) as u64;
            row.name = if eid == EVT_PROVIDER_INFO {
                "PROVIDER_INFO".into()
            } else {
                "PROVIDER_BLOB".into()
            };
            walk += ev_len;
            continue;
        }
        let pname = prov_name
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| format!("provider_{pid}"));
        let key = (pname, eid);
        let row = rows.entry(key.clone()).or_default();
        row.count += 1;
        row.bytes += ev_len as u64;
        row.payload_bytes += (ev_len - EVENT_HDR_SIZE) as u64;
        row.min_len = if row.min_len == 0 {
            ev_len
        } else {
            row.min_len.min(ev_len)
        };
        row.max_len = row.max_len.max(ev_len);
        if row.name.is_empty() {
            let schema = schema_by_pid.get(&pid);
            let kind = prov_kind.get(&pid).copied().unwrap_or(ProviderKind::Other);
            row.name = schema
                .and_then(|s| s.find_event(eid).map(|e| e.name.clone()))
                .unwrap_or_else(|| match kind {
                    ProviderKind::Tcp => event_name(eid).to_string(),
                    ProviderKind::If => if_event_name(eid).to_string(),
                    ProviderKind::Hpts => {
                        if eid == EVT_HPTS_STARS_AGG_SAMPLE {
                            "STARS_AGG_SAMPLE".to_string()
                        } else {
                            hpts_wheel_event_name(eid).to_string()
                        }
                    }
                    ProviderKind::Cpu => {
                        if cpu_split && eid == 1 {
                            "CPU_SAMPLE".to_string()
                        } else {
                            cpu_event_name(eid).to_string()
                        }
                    }
                    ProviderKind::EllaRelay => ella_relay_event_name(eid).to_string(),
                    _ => format!("id_{eid}"),
                });
            if let Some(s) = schema {
                if let Some(ev) = s.find_event(eid) {
                    row.level = Some(ev.level);
                    row.keywords = Some(ev.keywords);
                    if let Some(st) = s.struct_for_event(ev) {
                        let kw_names = s
                            .keywords
                            .iter()
                            .filter(|k| ev.keywords & k.value != 0)
                            .map(|k| k.name.clone())
                            .collect();
                        structs.insert(key, (ev.clone(), st.clone(), kw_names));
                    }
                }
            }
        }
        walk += ev_len;
    }
    Ok(header_bytes)
}

/// On-disk size breakdown by (provider, event): which record shapes
/// dominate the capture. `--fields N` adds a per-field byte breakdown
/// (from the embedded decode schema) for the top N shapes.
pub fn cmd_sizes(files: &[String], args: &[String]) {
    let fields_top: usize = args
        .iter()
        .position(|a| a == "--fields")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut rows: FxHashMap<SizeKey, SizeRow> = FxHashMap::default();
    let mut structs: FxHashMap<SizeKey, (DecodeEvent, DecodeStruct, Vec<String>)> =
        FxHashMap::default();
    let mut total_file_bytes: u64 = 0;
    let mut total_header_bytes: u64 = 0;
    for path in files {
        let parts = match open_captures(path, None) {
            Ok(parts) => parts,
            Err(e) => {
                eprintln!("Error reading {path}: {e}");
                process::exit(1);
            }
        };
        for (_, data) in parts {
            total_file_bytes += data.len() as u64;
            match scan_file(&data, &mut rows, &mut structs) {
                Ok(hb) => total_header_bytes += hb as u64,
                Err(e) => {
                    eprintln!("Error scanning {path}: {e}");
                    process::exit(1);
                }
            }
        }
    }

    let mut sorted: Vec<(&SizeKey, &SizeRow)> = rows.iter().collect();
    sorted.sort_by_key(|(_, r)| std::cmp::Reverse(r.bytes));
    let event_bytes: u64 = rows.values().map(|r| r.bytes).sum();
    let event_count: u64 = rows.values().map(|r| r.count).sum();

    println!("=== Size breakdown ===");
    println!(
        "  file bytes:      {} ({} uncompressed)",
        fmt_bytes(total_file_bytes),
        total_file_bytes
    );
    println!(
        "  header+tables:   {} ({:.1}%)",
        fmt_bytes(total_header_bytes),
        total_header_bytes as f64 / total_file_bytes.max(1) as f64 * 100.0
    );
    println!(
        "  event records:   {} ({:.1}%) in {} events",
        fmt_bytes(event_bytes),
        event_bytes as f64 / total_file_bytes.max(1) as f64 * 100.0,
        event_count
    );
    println!(
        "  record headers:  {} of that ({:.1}%) ({}B each)",
        fmt_bytes(event_count * EVENT_HDR_SIZE as u64),
        event_count * EVENT_HDR_SIZE as u64 * 100 / event_bytes.max(1),
        EVENT_HDR_SIZE
    );
    println!();
    println!(
        "{:<12} {:<26} {:<8} {:<22} {:>12} {:>12} {:>7} {:>6} {:>6}",
        "provider", "event", "level", "keywords", "count", "bytes", "%", "avg", "pay"
    );
    for (key, row) in &sorted {
        let (prov, _) = key;
        let kws = structs
            .get(key)
            .map(|(_, _, k)| k.join("|"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<12} {:<26} {:<8} {:<22} {:>12} {:>12} {:>6.1} {:>6} {:>6}",
            prov,
            row.name,
            row.level.map(level_name).unwrap_or("-"),
            kws,
            row.count,
            fmt_bytes(row.bytes),
            row.bytes as f64 / event_bytes.max(1) as f64 * 100.0,
            row.bytes / row.count.max(1),
            row.payload_bytes / row.count.max(1),
        );
    }

    // Provider subtotals.
    let mut by_prov: FxHashMap<&String, (u64, u64)> = FxHashMap::default();
    for ((prov, _), row) in &sorted {
        let e = by_prov.entry(prov).or_default();
        e.0 += row.count;
        e.1 += row.bytes;
    }
    let mut provs: Vec<_> = by_prov.into_iter().collect();
    provs.sort_by_key(|(_, b)| std::cmp::Reverse(b.1));
    println!();
    println!(
        "{:<12} {:>12} {:>12} {:>7}",
        "provider", "count", "bytes", "%"
    );
    for (prov, (count, bytes)) in provs {
        println!(
            "{:<12} {:>12} {:>12} {:>6.1}",
            prov,
            count,
            fmt_bytes(bytes),
            bytes as f64 / event_bytes.max(1) as f64 * 100.0
        );
    }

    if fields_top == 0 {
        return;
    }
    println!();
    for ((prov, eid), row) in sorted.iter().take(fields_top) {
        let Some((_, st, _)) = structs.get(&((*prov).clone(), *eid)) else {
            continue;
        };
        println!(
            "--- {} {} (id {}, {} events, payload avg {} min {} max {}) ---",
            prov,
            row.name,
            eid,
            row.count,
            fmt_bytes(row.payload_bytes / row.count.max(1)),
            row.min_len.saturating_sub(EVENT_HDR_SIZE),
            row.max_len.saturating_sub(EVENT_HDR_SIZE),
        );
        let mut fields: Vec<&DecodeField> = st.fields.iter().collect();
        fields.sort_by_key(|f| f.offset);
        let mut cursor = 0u16;
        for (i, f) in fields.iter().enumerate() {
            let sz = field_byte_size(f);
            if f.offset > cursor {
                println!(
                    "    {:<24} {:>6} {:>14}",
                    format!("[pad @{}]", cursor),
                    f.offset - cursor,
                    fmt_bytes((f.offset - cursor) as u64 * row.count)
                );
            }
            let varlen = f.flags & DECODE_FIELD_VARLEN != 0;
            let is_last = i == fields.len() - 1;
            if varlen {
                // A trailing varlen array's actual bytes are whatever the
                // payload has left after the fixed prefix.
                let label = format!("{} (varlen ≤{})", f.name, f.varlen_max);
                if is_last {
                    let actual = row
                        .payload_bytes
                        .saturating_sub(f.offset as u64 * row.count);
                    println!(
                        "    {:<24} {:>6} {:>14}",
                        label,
                        "var",
                        format!("{} (avg {})", fmt_bytes(actual), actual / row.count.max(1))
                    );
                } else {
                    println!("    {:<24} {:>6} {:>14}", label, "var", "?");
                }
            } else {
                println!(
                    "    {:<24} {:>6} {:>14}",
                    f.name,
                    sz,
                    fmt_bytes(sz as u64 * row.count)
                );
            }
            cursor = cursor.max(f.offset + sz);
        }
    }
}
