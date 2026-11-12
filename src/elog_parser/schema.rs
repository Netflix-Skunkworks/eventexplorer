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
// EventLog v2 "decode blob" (`EDB1`) parser + format-string interpreter.
//
// A decode blob is a flat, self-describing binary image of one provider's
// schema, embedded in a v2 `.elog` file's blob table (keyed by
// `schema_id = SHA256(blob)[..16]`). This is a byte-for-byte Rust port of
// `usr.bin/elog/decode.c`'s `decode_provider_parse` /
// `decode_format_payload` from the FreeBSD `eventlog` kernel tree — see
// `sys/sys/eventlog_decode.h` for the authoritative wire-format comment
// this mirrors. Ported (not reimplemented from scratch) so the hardening
// details there — the `%n` exclusion, the field-width cap, the
// placeholder-count cap — carry over exactly, since a blob can come from
// an untrusted capture file.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DecodeValue {
    pub value: u32,
    pub name: String,
}

/// An enum or flag value table.
#[derive(Debug, Clone)]
pub struct DecodeTable {
    pub name: String,
    pub values: Vec<DecodeValue>,
}

#[derive(Debug, Clone)]
pub struct DecodeField {
    pub name: String,
    /// `EVENTLOG_DECODE_TYPE_*` (see `constants.rs`'s `DECODE_TYPE_*`).
    pub type_: u8,
    /// `EVENTLOG_DECODE_ANNOT_*` (see `constants.rs`'s `DECODE_ANNOT_*`).
    pub annotation: u8,
    /// 1-based index into the schema's `enums`/`flags` table, else 0.
    pub annot_index: u16,
    /// Array length for `DECODE_TYPE_CHAR_ARRAY`, else 0.
    pub char_len: u16,
    /// Byte offset of this field within the event payload.
    pub offset: u16,
    /// `EVENTLOG_DECODE_FIELD_*` bits (see `constants.rs`'s `DECODE_FIELD_*`).
    pub flags: u8,
    pub varlen_count_index: u16,
    pub varlen_max: u16,
}

#[derive(Debug, Clone)]
pub struct DecodeStruct {
    pub name: String,
    pub fields: Vec<DecodeField>,
}

#[derive(Debug, Clone)]
pub struct DecodeEvent {
    pub id: u32,
    pub name: String,
    pub level: u8,
    pub keywords: u32,
    /// 1-based index into the schema's `structs`, `0` == no payload.
    pub struct_index: u16,
    /// Original positional (`%1`, `%2`, ...) or direct (`%u`, `%s`, ...)
    /// format string.
    pub format: String,
}

#[derive(Debug, Clone)]
pub struct DecodeKeyword {
    pub name: String,
    pub value: u32,
}

/// A fully parsed provider schema (one `EDB1` blob). See the module doc
/// comment; `ProviderSchema::parse` is the entry point.
#[derive(Debug, Clone)]
pub struct ProviderSchema {
    pub name: String,
    pub word_size: u8,
    pub endianness: u8,
    pub keywords: Vec<DecodeKeyword>,
    pub enums: Vec<DecodeTable>,
    pub flags: Vec<DecodeTable>,
    pub structs: Vec<DecodeStruct>,
    pub events: Vec<DecodeEvent>,
    /// `event_id → field name → field`, built once after parse so typed
    /// ingest can look up offsets without scanning the struct each event.
    event_fields: FxHashMap<u32, FxHashMap<String, DecodeField>>,
}

/// Forward-only blob reader with bounds checking (`struct rdr` in
/// `decode.c`). Once `err` is set, every subsequent read is a no-op
/// returning a zero value, so callers can read a whole record eagerly and
/// check `err` once afterward instead of threading `Result` through every
/// field.
struct Rdr<'a> {
    data: &'a [u8],
    pos: usize,
    err: bool,
}

impl<'a> Rdr<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            err: false,
        }
    }

    fn u8(&mut self) -> u8 {
        if self.err || self.pos + 1 > self.data.len() {
            self.err = true;
            return 0;
        }
        let v = self.data[self.pos];
        self.pos += 1;
        v
    }

    fn u16(&mut self) -> u16 {
        if self.err || self.pos + 2 > self.data.len() {
            self.err = true;
            return 0;
        }
        let v = u16::from_le_bytes(self.data[self.pos..self.pos + 2].try_into().unwrap());
        self.pos += 2;
        v
    }

    fn u32(&mut self) -> u32 {
        if self.err || self.pos + 4 > self.data.len() {
            self.err = true;
            return 0;
        }
        let v = u32::from_le_bytes(self.data[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        v
    }

    /// A `uint16_t`-length-prefixed string with no NUL terminator.
    fn str(&mut self) -> String {
        let len = self.u16() as usize;
        if self.err || self.pos + len > self.data.len() {
            self.err = true;
            return String::new();
        }
        let s = String::from_utf8_lossy(&self.data[self.pos..self.pos + len]).into_owned();
        self.pos += len;
        s
    }

    fn table(&mut self, n: u16) -> Vec<DecodeTable> {
        let mut out = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let name = self.str();
            let count = self.u16();
            if self.err {
                break;
            }
            let mut values = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let value = self.u32();
                let vname = self.str();
                values.push(DecodeValue { value, name: vname });
            }
            out.push(DecodeTable { name, values });
        }
        out
    }
}

impl ProviderSchema {
    /// Parse one `EDB1` decode blob. Returns `None` on any malformed
    /// input (bad magic/version/endianness, truncated data, or an
    /// out-of-range cross-reference caught by [`Self::validate`]) rather
    /// than panicking — the blob is untrusted capture-file content.
    pub fn parse(blob: &[u8]) -> Option<Self> {
        if blob.len() < 4 + 2 || blob.len() > EVENTLOG_DECODE_BLOB_MAX {
            return None;
        }
        if &blob[0..4] != EVENTLOG_DECODE_MAGIC {
            return None;
        }

        let mut r = Rdr::new(&blob[4..]);
        let version = r.u16();
        if version != EVENTLOG_DECODE_VERSION {
            return None;
        }
        let word_size = r.u8();
        let endianness = r.u8();
        let _reserved = r.u16();
        if r.err || endianness != EVENTLOG_DECODE_ENDIAN_LE {
            return None;
        }

        let name = r.str();
        let nkeywords = r.u16();
        let nenums = r.u16();
        let nflags = r.u16();
        let nstructs = r.u16();
        let nevents = r.u16();
        if r.err {
            return None;
        }

        let mut keywords = Vec::with_capacity(nkeywords as usize);
        for _ in 0..nkeywords {
            let kname = r.str();
            let value = r.u32();
            keywords.push(DecodeKeyword { name: kname, value });
        }

        let enums = r.table(nenums);
        let flags = r.table(nflags);
        if r.err {
            return None;
        }

        let mut structs = Vec::with_capacity(nstructs as usize);
        for _ in 0..nstructs {
            let sname = r.str();
            let field_count = r.u16();
            if r.err {
                return None;
            }
            let mut sfields = Vec::with_capacity(field_count as usize);
            for _ in 0..field_count {
                sfields.push(DecodeField {
                    name: r.str(),
                    type_: r.u8(),
                    annotation: r.u8(),
                    annot_index: r.u16(),
                    char_len: r.u16(),
                    offset: r.u16(),
                    flags: r.u8(),
                    varlen_count_index: r.u16(),
                    varlen_max: r.u16(),
                });
            }
            structs.push(DecodeStruct {
                name: sname,
                fields: sfields,
            });
        }

        let mut events = Vec::with_capacity(nevents as usize);
        for _ in 0..nevents {
            events.push(DecodeEvent {
                id: r.u32(),
                name: r.str(),
                level: r.u8(),
                keywords: r.u32(),
                struct_index: r.u16(),
                format: r.str(),
            });
        }

        if r.err {
            return None;
        }

        let mut schema = ProviderSchema {
            name,
            word_size,
            endianness,
            keywords,
            enums,
            flags,
            structs,
            events,
            event_fields: FxHashMap::default(),
        };
        if !schema.validate() {
            return None;
        }
        schema.index_fields();
        Some(schema)
    }

    fn index_fields(&mut self) {
        let mut event_fields = FxHashMap::default();
        for ev in &self.events {
            if let Some(st) = self.struct_for_event(ev) {
                let mut fields = FxHashMap::default();
                for f in &st.fields {
                    fields.insert(f.name.clone(), f.clone());
                }
                event_fields.insert(ev.id, fields);
            }
        }
        self.event_fields = event_fields;
    }

    /// Cross-reference validation (`validate_provider` in `decode.c`):
    /// every enum/flag/varlen/struct index in the blob must be in range
    /// so the formatting paths never need a bounds check. Indices are
    /// 1-based with `0` meaning "none".
    fn validate(&self) -> bool {
        for st in &self.structs {
            for f in &st.fields {
                if f.annotation == DECODE_ANNOT_ENUM
                    && (f.annot_index == 0 || f.annot_index as usize > self.enums.len())
                {
                    return false;
                }
                if f.annotation == DECODE_ANNOT_FLAG
                    && (f.annot_index == 0 || f.annot_index as usize > self.flags.len())
                {
                    return false;
                }
                if f.flags & DECODE_FIELD_VARLEN != 0
                    && (f.varlen_count_index == 0
                        || f.varlen_count_index as usize > st.fields.len())
                {
                    return false;
                }
            }
        }
        for ev in &self.events {
            if ev.struct_index as usize > self.structs.len() {
                return false;
            }
        }
        true
    }

    pub fn find_event(&self, event_id: u32) -> Option<&DecodeEvent> {
        self.events.iter().find(|e| e.id == event_id)
    }

    pub fn struct_for_event(&self, ev: &DecodeEvent) -> Option<&DecodeStruct> {
        if ev.struct_index == 0 {
            return None;
        }
        self.structs.get(ev.struct_index as usize - 1)
    }

    /// Find a field by name in the struct backing `event_id`. Returns
    /// `None` if the event is unknown, has no payload struct, or the
    /// struct has no field with that name (the last case is exactly
    /// "this event doesn't/no-longer carries this field").
    pub fn find_field(&self, event_id: u32, field_name: &str) -> Option<&DecodeField> {
        self.event_fields.get(&event_id)?.get(field_name)
    }

    /// Name this schema's enum table gives `value` for an enum-annotated
    /// field. Typed ingest keys off the name so a producer that renumbers
    /// an enum stays decodable (ELLA once inserted a value mid-table).
    pub fn enum_name(&self, event_id: u32, field_name: &str, value: u32) -> Option<&str> {
        let f = self.find_field(event_id, field_name)?;
        if f.annotation != DECODE_ANNOT_ENUM {
            return None;
        }
        table_at(&self.enums, f.annot_index)?
            .values
            .iter()
            .find(|dv| dv.value == value)
            .map(|dv| dv.name.as_str())
    }

    /// Read a named scalar field's raw value out of `payload`, at the
    /// field's *actual* schema offset rather than any hardcoded
    /// assumption. Used to rebuild a typed `EventPayload` from a schema
    /// whose layout has drifted from a hardcoded decoder (see the
    /// divergent-event routing in `core.rs`), so a struct reorder alone
    /// doesn't cost the event its per-session plot data. Returns `None`
    /// if the field isn't present in this schema (caller should default
    /// the corresponding struct field, e.g. to 0) or doesn't fit `payload`.
    pub fn read_field_u64(&self, event_id: u32, field_name: &str, payload: &[u8]) -> Option<u64> {
        let f = self.find_field(event_id, field_name)?;
        let v = load_uint(f, payload)?;
        if f.annotation == DECODE_ANNOT_NTOHS {
            Some((v as u16).swap_bytes() as u64)
        } else {
            Some(v)
        }
    }

    /// Read a `CHAR_ARRAY` (or any field whose payload bytes are a
    /// NUL-terminated string) at the schema offset. Missing/truncated
    /// fields return an empty string.
    pub fn read_field_cstr(&self, event_id: u32, field_name: &str, payload: &[u8]) -> String {
        let Some(f) = self.find_field(event_id, field_name) else {
            return String::new();
        };
        let off = f.offset as usize;
        let len = if f.type_ == DECODE_TYPE_CHAR_ARRAY {
            f.char_len as usize
        } else {
            field_byte_size(f) as usize
        };
        if len == 0 || off >= payload.len() {
            return String::new();
        }
        let end = (off + len).min(payload.len());
        let raw = &payload[off..end];
        let n = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..n]).to_string()
    }

    pub fn read_field_bytes(
        &self,
        event_id: u32,
        field_name: &str,
        payload: &[u8],
    ) -> Option<Vec<u8>> {
        let f = self.find_field(event_id, field_name)?;
        if f.flags & DECODE_FIELD_VARLEN != 0 {
            return Some(self.read_varlen_bytes(event_id, f, payload));
        }
        let off = f.offset as usize;
        let len = field_byte_size(f) as usize;
        if len == 0 || off + len > payload.len() {
            return None;
        }
        Some(payload[off..off + len].to_vec())
    }

    pub fn read_field_in_addr(
        &self,
        event_id: u32,
        field_name: &str,
        payload: &[u8],
    ) -> Option<Ipv4Addr> {
        let f = self.find_field(event_id, field_name)?;
        let off = f.offset as usize;
        if off + 4 > payload.len() {
            return None;
        }
        Some(Ipv4Addr::new(
            payload[off],
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
        ))
    }

    pub fn read_field_in6_addr(
        &self,
        event_id: u32,
        field_name: &str,
        payload: &[u8],
    ) -> Option<Ipv6Addr> {
        let b = self.read_field_bytes(event_id, field_name, payload)?;
        if b.len() < 16 {
            return None;
        }
        let mut o = [0u8; 16];
        o.copy_from_slice(&b[..16]);
        Some(Ipv6Addr::from(o))
    }

    /// Trailing `uint64_t[]` (histograms, CPU frames). Uses the named
    /// varlen field when present; otherwise the struct's first varlen
    /// field. `max_n` caps the count (schema `varlen_max` also applies).
    pub fn read_varlen_u64s(
        &self,
        event_id: u32,
        field_name: Option<&str>,
        payload: &[u8],
        max_n: usize,
    ) -> Vec<u64> {
        let Some(f) = field_name
            .and_then(|n| self.find_field(event_id, n))
            .or_else(|| self.first_varlen_field(event_id))
        else {
            return Vec::new();
        };
        let n = self.varlen_count(event_id, f, payload).min(max_n);
        let off = f.offset as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let at = off + i * 8;
            if at + 8 > payload.len() {
                break;
            }
            out.push(u64::from_le_bytes(payload[at..at + 8].try_into().unwrap()));
        }
        out
    }

    fn first_varlen_field(&self, event_id: u32) -> Option<&DecodeField> {
        let ev = self.find_event(event_id)?;
        let st = self.struct_for_event(ev)?;
        st.fields
            .iter()
            .find(|f| f.flags & DECODE_FIELD_VARLEN != 0)
    }

    fn varlen_count(&self, event_id: u32, f: &DecodeField, payload: &[u8]) -> usize {
        if f.flags & DECODE_FIELD_VARLEN == 0 {
            return 0;
        }
        let Some(ev) = self.find_event(event_id) else {
            return 0;
        };
        let Some(st) = self.struct_for_event(ev) else {
            return 0;
        };
        let idx = f.varlen_count_index as usize;
        if idx == 0 || idx > st.fields.len() {
            return 0;
        }
        let n = load_uint(&st.fields[idx - 1], payload).unwrap_or(0) as usize;
        if f.varlen_max == 0 {
            n
        } else {
            n.min(f.varlen_max as usize)
        }
    }

    fn read_varlen_bytes(&self, event_id: u32, f: &DecodeField, payload: &[u8]) -> Vec<u8> {
        let n = self.varlen_count(event_id, f, payload);
        let elem = field_byte_size(f) as usize;
        if elem == 0 {
            return Vec::new();
        }
        let off = f.offset as usize;
        let bytes = n.saturating_mul(elem);
        if off >= payload.len() {
            return Vec::new();
        }
        let end = (off + bytes).min(payload.len());
        payload[off..end].to_vec()
    }

    /// Render `payload` for `event_id` using the event's schema format
    /// string (`decode_format_payload` in `decode.c`). Returns `None`
    /// only if `event_id` isn't in this schema at all; an event with a
    /// struct/format but out-of-range field data still renders (with
    /// `<?>`/`<invalid>` placeholders for the fields that don't fit).
    pub fn format_payload(&self, event_id: u32, payload: &[u8]) -> Option<String> {
        let ev = self.find_event(event_id)?;

        // SESSION_CREATE with no payload is rendered without the tp pointer
        // (mirrors decode.c's special case for the same reason: older
        // captures didn't carry a payload on this event at all).
        if payload.is_empty() && ev.name == "SESSION_CREATE" {
            return Some("Session created".to_string());
        }

        let st = self.struct_for_event(ev);
        let chars: Vec<char> = ev.format.chars().collect();
        let mut out = String::new();
        let mut i = 0usize;
        let mut seq = 0usize;
        let mut nplaceholders = 0usize;

        while i < chars.len() {
            if chars[i] != '%' {
                out.push(chars[i]);
                i += 1;
                continue;
            }
            if i + 1 < chars.len() && chars[i + 1] == '%' {
                out.push('%');
                i += 2;
                continue;
            }

            let mut q = i + 1;
            let mut flag_chars = String::new();
            while q < chars.len() && matches!(chars[q], '-' | '+' | ' ' | '#' | '0') {
                flag_chars.push(chars[q]);
                q += 1;
            }
            let mut width: Option<u64> = None;
            let mut digit_run = String::new();
            digit_run.push_str(&flag_chars);
            if q < chars.len() && chars[q].is_ascii_digit() {
                let mut w: u64 = 0;
                while q < chars.len() && chars[q].is_ascii_digit() {
                    digit_run.push(chars[q]);
                    w = w
                        .saturating_mul(10)
                        .saturating_add(chars[q] as u64 - '0' as u64);
                    q += 1;
                }
                width = Some(w.min(DECODE_MAX_FIELD_WIDTH as u64));
            }
            // Skip length modifiers; integer fields are reformatted with
            // an explicit width based on the field's real type.
            while q < chars.len() && matches!(chars[q], 'l' | 'h' | 'j' | 'z' | 't' | 'q' | 'L') {
                q += 1;
            }

            if q < chars.len() && is_conv_char(chars[q]) {
                // Direct printf-style placeholder: next scalar field.
                let conv = chars[q];
                let fld = if let Some(s) = st {
                    nplaceholders += 1;
                    if nplaceholders <= DECODE_MAX_FORMAT_FIELDS {
                        seq += 1;
                        nth_scalar_field(s, seq)
                    } else {
                        None
                    }
                } else {
                    None
                };
                match fld {
                    Some(f) => {
                        self.append_field_spec(&mut out, f, payload, &flag_chars, width, conv)
                    }
                    None => out.push_str("<?>"),
                }
                i = q + 1;
                continue;
            }

            // Positional placeholder %N (digits only, no length modifier
            // / conversion letter, no non-digit flags): `digit_run` is a
            // 1-based field index. `'0'` doubles as both a flag char and
            // a digit, so a run like "%01" still qualifies (matches
            // decode.c's `flagswidth[0] >= '0' && <= '9'` check exactly).
            if !digit_run.is_empty() && digit_run.as_bytes()[0].is_ascii_digit() {
                let field_idx: usize = digit_run.parse().unwrap_or(0);
                let fld = if let Some(s) = st {
                    nplaceholders += 1;
                    if nplaceholders <= DECODE_MAX_FORMAT_FIELDS
                        && field_idx >= 1
                        && field_idx <= s.fields.len()
                    {
                        s.fields.get(field_idx - 1)
                    } else {
                        None
                    }
                } else {
                    None
                };
                match fld {
                    Some(f) => self.append_field_positional(&mut out, f, payload),
                    None => out.push_str("<?>"),
                }
                i = q;
                continue;
            }

            // Not a recognized conversion; emit the '%' literally (matches
            // decode.c advancing only past the '%', not the trailing junk).
            out.push('%');
            i += 1;
        }
        Some(out)
    }

    /// Append the string form of an enum/flag/address/char field
    /// (`append_field_string` in `decode.c`).
    fn append_field_string(&self, out: &mut String, f: &DecodeField, payload: &[u8]) {
        match f.type_ {
            DECODE_TYPE_IN_ADDR => {
                let off = f.offset as usize;
                if off + 4 <= payload.len() {
                    let a = Ipv4Addr::new(
                        payload[off],
                        payload[off + 1],
                        payload[off + 2],
                        payload[off + 3],
                    );
                    out.push_str(&a.to_string());
                } else {
                    out.push_str("<invalid>");
                }
                return;
            }
            DECODE_TYPE_IN6_ADDR => {
                let off = f.offset as usize;
                if off + 16 <= payload.len() {
                    let mut o = [0u8; 16];
                    o.copy_from_slice(&payload[off..off + 16]);
                    out.push_str(&std::net::Ipv6Addr::from(o).to_string());
                } else {
                    out.push_str("<invalid>");
                }
                return;
            }
            DECODE_TYPE_CHAR_ARRAY => {
                let off = f.offset as usize;
                let len = f.char_len as usize;
                if off + len <= payload.len() {
                    let raw = &payload[off..off + len];
                    let end = raw.iter().position(|&b| b == 0).unwrap_or(len);
                    out.push_str(&String::from_utf8_lossy(&raw[..end]));
                } else {
                    out.push_str("<invalid>");
                }
                return;
            }
            _ => {}
        }

        let v = match load_uint(f, payload) {
            Some(v) => v,
            None => {
                out.push_str("<?>");
                return;
            }
        };

        if f.annotation == DECODE_ANNOT_ENUM {
            if let Some(t) = table_at(&self.enums, f.annot_index) {
                if let Some(dv) = t.values.iter().find(|dv| dv.value as u64 == v) {
                    out.push_str(&dv.name);
                    return;
                }
            }
            out.push_str(&v.to_string());
            return;
        }

        if f.annotation == DECODE_ANNOT_FLAG {
            let mut first = true;
            if let Some(t) = table_at(&self.flags, f.annot_index) {
                if v != 0 {
                    for dv in &t.values {
                        if dv.value != 0 && (v & dv.value as u64) == dv.value as u64 {
                            if !first {
                                out.push('|');
                            }
                            out.push_str(&dv.name);
                            first = false;
                        }
                    }
                }
            }
            if first {
                out.push_str(&v.to_string());
            }
            return;
        }

        out.push_str(&v.to_string());
    }

    /// Append a field formatted by its own type/annotation, positional
    /// `%N` style (`append_field_positional` in `decode.c`).
    fn append_field_positional(&self, out: &mut String, f: &DecodeField, payload: &[u8]) {
        if matches!(
            f.type_,
            DECODE_TYPE_IN_ADDR | DECODE_TYPE_IN6_ADDR | DECODE_TYPE_CHAR_ARRAY
        ) {
            self.append_field_string(out, f, payload);
            return;
        }
        if f.annotation == DECODE_ANNOT_ENUM || f.annotation == DECODE_ANNOT_FLAG {
            self.append_field_string(out, f, payload);
            return;
        }
        let v = match load_uint(f, payload) {
            Some(v) => v,
            None => {
                out.push_str("<?>");
                return;
            }
        };
        match f.annotation {
            DECODE_ANNOT_HEX => {
                out.push_str(&format!("{v:#x}"));
                return;
            }
            DECODE_ANNOT_NTOHS => {
                out.push_str(&(v as u16).swap_bytes().to_string());
                return;
            }
            _ => {}
        }
        if f.type_ == DECODE_TYPE_PTR {
            out.push_str(&format!("{v:#x}"));
            return;
        }
        if field_is_signed(f) {
            out.push_str(&(v as i64).to_string());
        } else {
            out.push_str(&v.to_string());
        }
    }

    /// Append a field formatted to a printf conversion taken directly
    /// from the schema's format string (`append_field_spec` in
    /// `decode.c`). Address/enum/flag/char fields always render as
    /// their string form; numeric fields honor `flags`/`width`.
    fn append_field_spec(
        &self,
        out: &mut String,
        f: &DecodeField,
        payload: &[u8],
        flags: &str,
        width: Option<u64>,
        conv: char,
    ) {
        if matches!(
            f.type_,
            DECODE_TYPE_IN_ADDR | DECODE_TYPE_IN6_ADDR | DECODE_TYPE_CHAR_ARRAY
        ) {
            self.append_field_string(out, f, payload);
            return;
        }
        if conv == 's' || f.annotation == DECODE_ANNOT_ENUM || f.annotation == DECODE_ANNOT_FLAG {
            self.append_field_string(out, f, payload);
            return;
        }
        let v = match load_uint(f, payload) {
            Some(v) => v,
            None => {
                out.push_str("<?>");
                return;
            }
        };
        if conv == 'p' {
            out.push_str(&format!("{v:#x}"));
            return;
        }
        if conv == 'c' {
            out.push(char::from_u32(v as u32 & 0xff).unwrap_or('?'));
            return;
        }
        let body = match conv {
            'd' | 'i' => (v as i64).to_string(),
            'o' => format!("{v:o}"),
            'x' => format!("{v:x}"),
            'X' => format!("{v:X}"),
            _ => v.to_string(), // 'u'
        };
        out.push_str(&pad_numeric(&body, flags, width));
    }
}

/// Reads named fields from a payload, using the embedded schema's offsets
/// when one is present and hardcoded natural-alignment offsets otherwise.
pub struct PayloadReader<'a> {
    schema: Option<&'a ProviderSchema>,
    event_id: u32,
    payload: &'a [u8],
}

impl<'a> PayloadReader<'a> {
    pub fn new(
        schema: Option<&'a ProviderSchema>,
        event_id: u32,
        data: &'a [u8],
        off: usize,
        size: usize,
    ) -> Self {
        let end = off.saturating_add(size).min(data.len());
        let payload = if off < end { &data[off..end] } else { &[] };
        Self {
            schema,
            event_id,
            payload,
        }
    }

    /// The event is in the resolved schema — typed ingest should use
    /// schema offsets even when the payload is shorter than the
    /// natural-alignment size guard.
    pub fn typed(&self) -> bool {
        self.schema
            .is_some_and(|s| s.find_event(self.event_id).is_some())
    }

    pub fn has(&self, name: &str) -> bool {
        self.schema
            .is_some_and(|s| s.find_field(self.event_id, name).is_some())
    }

    /// Name this event's schema gives `value` for an enum field, so a
    /// decoder can match on the name instead of pinning the number.
    pub fn enum_name(&self, name: &str, value: u32) -> Option<&str> {
        self.schema?.enum_name(self.event_id, name, value)
    }

    pub fn u8(&self, name: &str, off: usize) -> u8 {
        if self.typed() {
            return self
                .schema
                .unwrap()
                .read_field_u64(self.event_id, name, self.payload)
                .unwrap_or(0) as u8;
        }
        self.payload.get(off).copied().unwrap_or(0)
    }

    pub fn u16(&self, name: &str, off: usize) -> u16 {
        if self.typed() {
            return self
                .schema
                .unwrap()
                .read_field_u64(self.event_id, name, self.payload)
                .unwrap_or(0) as u16;
        }
        if off + 2 > self.payload.len() {
            return 0;
        }
        u16::from_le_bytes(self.payload[off..off + 2].try_into().unwrap())
    }

    pub fn u32(&self, name: &str, off: usize) -> u32 {
        if self.typed() {
            return self
                .schema
                .unwrap()
                .read_field_u64(self.event_id, name, self.payload)
                .unwrap_or(0) as u32;
        }
        if off + 4 > self.payload.len() {
            return 0;
        }
        u32::from_le_bytes(self.payload[off..off + 4].try_into().unwrap())
    }

    pub fn u64(&self, name: &str, off: usize) -> u64 {
        if self.typed() {
            return self
                .schema
                .unwrap()
                .read_field_u64(self.event_id, name, self.payload)
                .unwrap_or(0);
        }
        if off + 8 > self.payload.len() {
            return 0;
        }
        u64::from_le_bytes(self.payload[off..off + 8].try_into().unwrap())
    }

    pub fn i32(&self, name: &str, off: usize) -> i32 {
        self.u32(name, off) as i32
    }

    pub fn i64(&self, name: &str, off: usize) -> i64 {
        self.u64(name, off) as i64
    }

    pub fn cstr(&self, name: &str, off: usize, max: usize) -> String {
        if self.typed() {
            return self
                .schema
                .unwrap()
                .read_field_cstr(self.event_id, name, self.payload);
        }
        if off >= self.payload.len() {
            return String::new();
        }
        let end = (off + max).min(self.payload.len());
        let raw = &self.payload[off..end];
        let n = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..n]).to_string()
    }

    /// Text in either a `CHAR_ARRAY` or a varlen `uint8_t[len]` field
    /// (e.g. nginx `uri`); untyped reads fall back to [`Self::cstr`].
    pub fn text(&self, name: &str, off: usize, max: usize) -> String {
        if let Some(s) = self.schema.filter(|_| self.typed()) {
            if s.find_field(self.event_id, name)
                .is_some_and(|f| f.flags & DECODE_FIELD_VARLEN != 0)
            {
                let raw = s
                    .read_field_bytes(self.event_id, name, self.payload)
                    .unwrap_or_default();
                let n = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                return String::from_utf8_lossy(&raw[..n]).to_string();
            }
        }
        self.cstr(name, off, max)
    }

    pub fn in_addr(&self, name: &str, off: usize) -> Ipv4Addr {
        if self.typed() {
            if let Some(a) =
                self.schema
                    .unwrap()
                    .read_field_in_addr(self.event_id, name, self.payload)
            {
                return a;
            }
        }
        if off + 4 <= self.payload.len() {
            Ipv4Addr::from(<[u8; 4]>::try_from(&self.payload[off..off + 4]).unwrap())
        } else {
            Ipv4Addr::UNSPECIFIED
        }
    }

    pub fn in6_addr(&self, name: &str, off: usize) -> Ipv6Addr {
        if self.typed() {
            if let Some(a) =
                self.schema
                    .unwrap()
                    .read_field_in6_addr(self.event_id, name, self.payload)
            {
                return a;
            }
        }
        if off + 16 <= self.payload.len() {
            let mut o = [0u8; 16];
            o.copy_from_slice(&self.payload[off..off + 16]);
            Ipv6Addr::from(o)
        } else {
            Ipv6Addr::UNSPECIFIED
        }
    }

    /// Ports stored in network byte order. Schema path honors `NTOHS`
    /// (and still swaps when the annotation is missing, matching the
    /// hardcoded `from_be_bytes` readers).
    pub fn port(&self, name: &str, off: usize) -> u16 {
        if self.typed() {
            let s = self.schema.unwrap();
            if let Some(f) = s.find_field(self.event_id, name) {
                if f.annotation == DECODE_ANNOT_NTOHS {
                    return s
                        .read_field_u64(self.event_id, name, self.payload)
                        .unwrap_or(0) as u16;
                }
                let v = load_uint(f, self.payload).unwrap_or(0) as u16;
                return v.swap_bytes();
            }
            return 0;
        }
        if off + 2 > self.payload.len() {
            return 0;
        }
        u16::from_be_bytes(self.payload[off..off + 2].try_into().unwrap())
    }

    /// IP bytes (varlen) or a `CHAR_ARRAY` / `IN_ADDR` / `IN6_ADDR`.
    pub fn ip_or_cstr(&self, name: &str, off: usize, max: usize) -> String {
        if self.typed() {
            let s = self.schema.unwrap();
            if let Some(f) = s.find_field(self.event_id, name) {
                match f.type_ {
                    DECODE_TYPE_CHAR_ARRAY => {
                        return s.read_field_cstr(self.event_id, name, self.payload);
                    }
                    DECODE_TYPE_IN_ADDR => {
                        return s
                            .read_field_in_addr(self.event_id, name, self.payload)
                            .map(|a| a.to_string())
                            .unwrap_or_default();
                    }
                    DECODE_TYPE_IN6_ADDR => {
                        if let Some(b) = s.read_field_bytes(self.event_id, name, self.payload) {
                            if b.len() >= 16 {
                                let mut o = [0u8; 16];
                                o.copy_from_slice(&b[..16]);
                                return std::net::Ipv6Addr::from(o).to_string();
                            }
                        }
                        return String::new();
                    }
                    _ => {
                        let b = s
                            .read_field_bytes(self.event_id, name, self.payload)
                            .unwrap_or_default();
                        return format_ip_addr(&b);
                    }
                }
            }
            return String::new();
        }
        self.cstr(name, off, max)
    }

    pub fn varlen_bytes(&self, name: &str, off: usize, count: usize) -> Vec<u8> {
        if self.typed() {
            return self
                .schema
                .unwrap()
                .read_field_bytes(self.event_id, name, self.payload)
                .unwrap_or_default();
        }
        if off >= self.payload.len() {
            return Vec::new();
        }
        let end = (off + count).min(self.payload.len());
        self.payload[off..end].to_vec()
    }

    pub fn hist_u64s(&self, max_n: usize, count_off: usize, data_off: usize) -> Vec<u64> {
        if self.typed() {
            return self
                .schema
                .unwrap()
                .read_varlen_u64s(self.event_id, None, self.payload, max_n);
        }
        let n = (self.payload.get(count_off).copied().unwrap_or(0) as usize).min(max_n);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let at = data_off + i * 8;
            if at + 8 > self.payload.len() {
                break;
            }
            out.push(u64::from_le_bytes(
                self.payload[at..at + 8].try_into().unwrap(),
            ));
        }
        out
    }
}

/// Recognized printf conversion specifiers (`is_conv_char` in
/// `decode.c`). `%n` is deliberately excluded: a decode blob's format
/// strings can come from an untrusted source, and honoring `%n` in a
/// C `vsnprintf` would be an arbitrary memory write — Rust has no such
/// hazard, but we keep the same recognized set for parity with the
/// reference implementation. An unrecognized conversion is emitted
/// literally instead.
fn is_conv_char(c: char) -> bool {
    matches!(c, 'd' | 'i' | 'o' | 'u' | 'x' | 'X' | 'p' | 's' | 'c')
}

/// The k-th (1-based) field that is not a trailing variable-length array
/// (`nth_scalar_field` in `decode.c`).
fn nth_scalar_field(st: &DecodeStruct, k: usize) -> Option<&DecodeField> {
    let mut seen = 0;
    for f in &st.fields {
        if f.flags & DECODE_FIELD_VARLEN != 0 {
            continue;
        }
        seen += 1;
        if seen == k {
            return Some(f);
        }
    }
    None
}

fn table_at(tables: &[DecodeTable], idx1: u16) -> Option<&DecodeTable> {
    if idx1 == 0 {
        return None;
    }
    tables.get(idx1 as usize - 1)
}

/// Byte width of a field's element in the payload (`field_size` in
/// `decode.c`). Public so `schema_validate.rs` can compare a schema
/// field's size against a hardcoded decoder's assumed size.
pub fn field_byte_size(f: &DecodeField) -> u16 {
    match f.type_ {
        DECODE_TYPE_U8 | DECODE_TYPE_I8 => 1,
        DECODE_TYPE_U16 | DECODE_TYPE_I16 => 2,
        DECODE_TYPE_U32 | DECODE_TYPE_I32 | DECODE_TYPE_IN_ADDR => 4,
        DECODE_TYPE_U64 | DECODE_TYPE_I64 | DECODE_TYPE_PTR => 8,
        DECODE_TYPE_IN6_ADDR => 16,
        DECODE_TYPE_CHAR_ARRAY => f.char_len,
        _ => 0,
    }
}

fn field_is_signed(f: &DecodeField) -> bool {
    matches!(
        f.type_,
        DECODE_TYPE_I8 | DECODE_TYPE_I16 | DECODE_TYPE_I32 | DECODE_TYPE_I64
    )
}

/// Read a field's little-endian integer value, sign-extended if signed
/// (`load_uint` in `decode.c`). Returns `None` if the field doesn't fit
/// in `payload` (truncated/short event) or has a non-integer type.
fn load_uint(f: &DecodeField, payload: &[u8]) -> Option<u64> {
    let sz = field_byte_size(f);
    if sz == 0 || sz > 8 {
        return None;
    }
    let off = f.offset as usize;
    let sz = sz as usize;
    if off + sz > payload.len() {
        return None;
    }
    let mut v: u64 = 0;
    for (i, &byte) in payload[off..off + sz].iter().enumerate() {
        v |= (byte as u64) << (8 * i);
    }
    if field_is_signed(f) && sz < 8 {
        let signbit = 1u64 << (sz * 8 - 1);
        if v & signbit != 0 {
            v |= !((signbit << 1) - 1);
        }
    }
    Some(v)
}

/// Minimal printf-style width/flag padding for a pre-rendered numeric
/// body (`-` left-justify, `0` zero-pad after a leading `-` sign,
/// default right-justify with spaces). `+`/` `/`#` are accepted but
/// have no effect — no schema seen in practice uses them, and getting
/// padding/justification right matters far more than sign decoration.
fn pad_numeric(body: &str, flags: &str, width: Option<u64>) -> String {
    let width = match width {
        Some(w) => w as usize,
        None => return body.to_string(),
    };
    if body.len() >= width {
        return body.to_string();
    }
    let pad = width - body.len();
    if flags.contains('-') {
        format!("{body}{}", " ".repeat(pad))
    } else if flags.contains('0') {
        match body.strip_prefix('-') {
            Some(rest) => format!("-{}{rest}", "0".repeat(pad)),
            None => format!("{}{body}", "0".repeat(pad)),
        }
    } else {
        format!("{}{body}", " ".repeat(pad))
    }
}

/// `schema_id = SHA256(blob)[..SCHEMA_ID_LEN]` (`kern_eventlog.c`'s
/// registrar). Used to verify a v2 provider row's advertised
/// `schema_id` against the blob actually embedded in the file before
/// trusting it.
pub fn compute_schema_id(blob: &[u8]) -> [u8; SCHEMA_ID_LEN] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(blob);
    let mut out = [0u8; SCHEMA_ID_LEN];
    out.copy_from_slice(&digest[..SCHEMA_ID_LEN]);
    out
}

/// Hand-rolled `EDB1` blob builder for tests — independent of
/// `ProviderSchema::parse` (writes raw bytes per the wire format
/// documented at the top of this file) so a test failure here can't be
/// masked by a matching bug on both the write and read side. Used by
/// this module's own unit tests and by `schema_validate`'s.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) struct FieldSpec {
        pub name: &'static str,
        pub type_: u8,
        pub annotation: u8,
        pub annot_index: u16,
        pub char_len: u16,
        pub offset: u16,
        pub flags: u8,
        pub varlen_count_index: u16,
        pub varlen_max: u16,
    }

    impl FieldSpec {
        /// A plain (non-annotated, non-varlen) scalar field.
        pub(crate) fn scalar(name: &'static str, type_: u8, offset: u16) -> Self {
            FieldSpec {
                name,
                type_,
                annotation: DECODE_ANNOT_NONE,
                annot_index: 0,
                char_len: 0,
                offset,
                flags: 0,
                varlen_count_index: 0,
                varlen_max: 0,
            }
        }

        pub(crate) fn varlen(
            name: &'static str,
            type_: u8,
            offset: u16,
            count_index: u16,
            max: u16,
        ) -> Self {
            FieldSpec {
                flags: DECODE_FIELD_VARLEN,
                varlen_count_index: count_index,
                varlen_max: max,
                ..Self::scalar(name, type_, offset)
            }
        }

        /// A scalar annotated with `enums[annot_index - 1]`.
        pub(crate) fn enumerated(
            name: &'static str,
            type_: u8,
            offset: u16,
            annot_index: u16,
        ) -> Self {
            FieldSpec {
                annotation: DECODE_ANNOT_ENUM,
                annot_index,
                ..Self::scalar(name, type_, offset)
            }
        }
    }

    pub(crate) struct EnumSpec {
        pub name: &'static str,
        pub values: Vec<(u32, &'static str)>,
    }

    pub(crate) struct StructSpec {
        pub name: &'static str,
        pub fields: Vec<FieldSpec>,
    }

    pub(crate) struct EventSpec {
        pub id: u32,
        pub name: &'static str,
        /// 1-based index into the blob's struct list, `0` == no payload.
        pub struct_index: u16,
        pub format: &'static str,
    }

    fn write_str(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u16).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }

    /// Build a full `EDB1` blob: no keywords/enums/flags, `structs` and
    /// `events` written in order (so an `EventSpec::struct_index` of `1`
    /// refers to `structs[0]`).
    pub(crate) fn build_blob_multi(
        provider_name: &str,
        structs: &[StructSpec],
        events: &[EventSpec],
    ) -> Vec<u8> {
        build_blob_enums(provider_name, &[], structs, events)
    }

    /// `build_blob_multi` with an enum table, written in order so a
    /// `FieldSpec::enumerated` `annot_index` of `1` refers to `enums[0]`.
    pub(crate) fn build_blob_enums(
        provider_name: &str,
        enums: &[EnumSpec],
        structs: &[StructSpec],
        events: &[EventSpec],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(EVENTLOG_DECODE_MAGIC);
        out.extend_from_slice(&EVENTLOG_DECODE_VERSION.to_le_bytes());
        out.push(8); // word_size
        out.push(EVENTLOG_DECODE_ENDIAN_LE);
        out.extend_from_slice(&0u16.to_le_bytes()); // reserved
        write_str(&mut out, provider_name);
        out.extend_from_slice(&0u16.to_le_bytes()); // nkeywords
        out.extend_from_slice(&(enums.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // nflags
        out.extend_from_slice(&(structs.len() as u16).to_le_bytes());
        out.extend_from_slice(&(events.len() as u16).to_le_bytes());

        for e in enums {
            write_str(&mut out, e.name);
            out.extend_from_slice(&(e.values.len() as u16).to_le_bytes());
            for (value, name) in &e.values {
                out.extend_from_slice(&value.to_le_bytes());
                write_str(&mut out, name);
            }
        }

        for s in structs {
            write_str(&mut out, s.name);
            out.extend_from_slice(&(s.fields.len() as u16).to_le_bytes());
            for f in &s.fields {
                write_str(&mut out, f.name);
                out.push(f.type_);
                out.push(f.annotation);
                out.extend_from_slice(&f.annot_index.to_le_bytes());
                out.extend_from_slice(&f.char_len.to_le_bytes());
                out.extend_from_slice(&f.offset.to_le_bytes());
                out.push(f.flags);
                out.extend_from_slice(&f.varlen_count_index.to_le_bytes());
                out.extend_from_slice(&f.varlen_max.to_le_bytes());
            }
        }

        for e in events {
            out.extend_from_slice(&e.id.to_le_bytes());
            write_str(&mut out, e.name);
            out.push(0); // level
            out.extend_from_slice(&0u32.to_le_bytes()); // keywords
            out.extend_from_slice(&e.struct_index.to_le_bytes());
            write_str(&mut out, e.format);
        }

        out
    }

    /// Convenience wrapper for the common single-struct, single-event case.
    pub(crate) fn build_blob(
        provider_name: &str,
        struct_name: &'static str,
        fields: Vec<FieldSpec>,
        event_id: u32,
        event_name: &'static str,
        format: &'static str,
    ) -> Vec<u8> {
        build_blob_multi(
            provider_name,
            &[StructSpec {
                name: struct_name,
                fields,
            }],
            &[EventSpec {
                id: event_id,
                name: event_name,
                struct_index: 1,
                format,
            }],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn parses_minimal_blob_and_resolves_event_and_fields() {
        let blob = build_blob(
            "testprov",
            "IN",
            vec![
                FieldSpec::scalar("seq", DECODE_TYPE_U32, 0),
                FieldSpec::scalar("len", DECODE_TYPE_U32, 4),
            ],
            100,
            "IN",
            "seq=%1 len=%2",
        );
        let schema = ProviderSchema::parse(&blob).expect("blob should parse");
        assert_eq!(schema.name, "testprov");
        let ev = schema.find_event(100).expect("event 100 should resolve");
        assert_eq!(ev.name, "IN");
        let st = schema.struct_for_event(ev).expect("event has a struct");
        assert_eq!(st.fields.len(), 2);

        let seq = schema.find_field(100, "seq").expect("seq field");
        assert_eq!(seq.offset, 0);
        assert_eq!(field_byte_size(seq), 4);
        let len = schema.find_field(100, "len").expect("len field");
        assert_eq!(len.offset, 4);
        assert_eq!(field_byte_size(len), 4);

        assert!(schema.find_event(101).is_none());
        assert!(schema.find_field(100, "nope").is_none());
    }

    #[test]
    fn format_payload_renders_positional_specifiers() {
        let blob = build_blob(
            "testprov",
            "IN",
            vec![
                FieldSpec::scalar("seq", DECODE_TYPE_U32, 0),
                FieldSpec::scalar("len", DECODE_TYPE_U32, 4),
            ],
            100,
            "IN",
            "seq=%1 len=%2",
        );
        let schema = ProviderSchema::parse(&blob).unwrap();
        let mut payload = vec![0u8; 8];
        payload[0..4].copy_from_slice(&42u32.to_le_bytes());
        payload[4..8].copy_from_slice(&7u32.to_le_bytes());
        let out = schema.format_payload(100, &payload).unwrap();
        assert_eq!(out, "seq=42 len=7");
    }

    /// Direct printf-style placeholders (`%u`, not `%N`) consume fields in
    /// struct order via their own counter, independent of any positional
    /// `%N` placeholders that might appear elsewhere in the same format
    /// string (mirrors `decode.c`'s `append_field_spec` sequencing).
    #[test]
    fn format_payload_renders_direct_specifiers_in_struct_order() {
        let blob = build_blob(
            "testprov",
            "IN",
            vec![
                FieldSpec::scalar("seq", DECODE_TYPE_U32, 0),
                FieldSpec::scalar("len", DECODE_TYPE_U32, 4),
            ],
            100,
            "IN",
            "seq=%u len=%u",
        );
        let schema = ProviderSchema::parse(&blob).unwrap();
        let mut payload = vec![0u8; 8];
        payload[0..4].copy_from_slice(&42u32.to_le_bytes());
        payload[4..8].copy_from_slice(&7u32.to_le_bytes());
        let out = schema.format_payload(100, &payload).unwrap();
        assert_eq!(out, "seq=42 len=7");
    }

    #[test]
    fn format_payload_renders_hex_and_char_array_annotations() {
        let blob = build_blob(
            "testprov",
            "S",
            vec![
                FieldSpec {
                    annotation: DECODE_ANNOT_HEX,
                    ..FieldSpec::scalar("flags", DECODE_TYPE_U32, 0)
                },
                FieldSpec {
                    char_len: 4,
                    ..FieldSpec::scalar("name", DECODE_TYPE_CHAR_ARRAY, 4)
                },
            ],
            200,
            "S",
            "flags=%1 name=%2",
        );
        let schema = ProviderSchema::parse(&blob).unwrap();
        let mut payload = vec![0u8; 8];
        payload[0..4].copy_from_slice(&0xABu32.to_le_bytes());
        payload[4..8].copy_from_slice(b"abc\0");
        let out = schema.format_payload(200, &payload).unwrap();
        assert_eq!(out, "flags=0xab name=abc");
    }

    #[test]
    fn rejects_bad_magic() {
        let mut blob = build_blob("p", "S", vec![], 1, "E", "");
        blob[0] = b'X';
        assert!(ProviderSchema::parse(&blob).is_none());
    }

    #[test]
    fn rejects_bad_version() {
        let mut blob = build_blob("p", "S", vec![], 1, "E", "");
        // version is the u16 right after the 4-byte magic.
        blob[4..6].copy_from_slice(&(EVENTLOG_DECODE_VERSION + 1).to_le_bytes());
        assert!(ProviderSchema::parse(&blob).is_none());
    }

    #[test]
    fn rejects_truncated_blob() {
        let blob = build_blob(
            "p",
            "S",
            vec![FieldSpec::scalar("a", DECODE_TYPE_U32, 0)],
            1,
            "E",
            "%1",
        );
        let truncated = &blob[..blob.len() - 3];
        assert!(ProviderSchema::parse(truncated).is_none());
    }

    #[test]
    fn compute_schema_id_is_deterministic_and_content_sensitive() {
        let blob_a = build_blob("p", "S", vec![], 1, "E", "x");
        let blob_b = build_blob("p", "S", vec![], 2, "E", "x");
        assert_eq!(compute_schema_id(&blob_a), compute_schema_id(&blob_a));
        assert_ne!(compute_schema_id(&blob_a), compute_schema_id(&blob_b));
    }

    #[test]
    fn reads_packed_odd_offset_scalars_and_cstr() {
        let blob = build_blob(
            "p",
            "S",
            vec![
                FieldSpec::scalar("tag", DECODE_TYPE_U8, 0),
                FieldSpec::scalar("value", DECODE_TYPE_U64, 1),
                FieldSpec {
                    char_len: 4,
                    ..FieldSpec::scalar("name", DECODE_TYPE_CHAR_ARRAY, 9)
                },
            ],
            7,
            "S",
            "",
        );
        let schema = ProviderSchema::parse(&blob).unwrap();
        let mut payload = vec![0u8; 13];
        payload[0] = 9;
        payload[1..9].copy_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
        payload[9..13].copy_from_slice(b"ab\0x");
        assert_eq!(schema.read_field_u64(7, "tag", &payload), Some(9));
        assert_eq!(
            schema.read_field_u64(7, "value", &payload),
            Some(0x0102_0304_0506_0708)
        );
        assert_eq!(schema.read_field_cstr(7, "name", &payload), "ab");
        let r = PayloadReader::new(Some(&schema), 7, &payload, 0, payload.len());
        assert!(r.typed());
        assert_eq!(r.u8("tag", 0), 9);
        assert_eq!(r.u64("value", 8), 0x0102_0304_0506_0708);
        assert_eq!(r.cstr("name", 16, 4), "ab");
    }

    #[test]
    fn reads_packed_hist_tray_at_offset_1() {
        let blob = build_blob(
            "p",
            "H",
            vec![
                FieldSpec::scalar("nbuckets", DECODE_TYPE_U8, 0),
                FieldSpec::varlen("buckets", DECODE_TYPE_U64, 1, 1, 16),
            ],
            13,
            "H",
            "",
        );
        let schema = ProviderSchema::parse(&blob).unwrap();
        let mut payload = vec![0u8; 1 + 16];
        payload[0] = 2;
        payload[1..9].copy_from_slice(&11u64.to_le_bytes());
        payload[9..17].copy_from_slice(&22u64.to_le_bytes());
        assert_eq!(
            schema.read_varlen_u64s(13, Some("buckets"), &payload, 16),
            vec![11, 22]
        );
        let r = PayloadReader::new(Some(&schema), 13, &payload, 0, payload.len());
        assert_eq!(r.hist_u64s(16, 0, 8), vec![11, 22]);
    }

    #[test]
    fn truncated_field_reads_as_zero() {
        let blob = build_blob(
            "p",
            "S",
            vec![FieldSpec::scalar("v", DECODE_TYPE_U64, 0)],
            1,
            "S",
            "",
        );
        let schema = ProviderSchema::parse(&blob).unwrap();
        let payload = vec![1u8, 2, 3];
        assert_eq!(schema.read_field_u64(1, "v", &payload), None);
        let r = PayloadReader::new(Some(&schema), 1, &payload, 0, payload.len());
        assert_eq!(r.u64("v", 0), 0);
    }

    #[test]
    fn hardcoded_path_uses_natural_offsets() {
        let mut payload = vec![0u8; 16];
        payload[0] = 1;
        payload[8..16].copy_from_slice(&99u64.to_le_bytes());
        let r = PayloadReader::new(None, 13, &payload, 0, payload.len());
        assert!(!r.typed());
        assert_eq!(r.hist_u64s(16, 0, 8), vec![99]);
    }
}
