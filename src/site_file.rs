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

// Site file parser, shared by `build.rs` (via `include!`) and `src/site.rs`.
//
// The format is the flat subset of TOML the site keys need: one
// `key = value` per line, where the value is a basic string or an
// array of basic strings. Blank lines and `#` comments are skipped.
// Strings become their text; arrays become their JSON encoding.

/// Parse a site file into `(key, value)` pairs, in file order.
pub fn parse_site_file(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let lineno = index + 1;
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!("line {lineno}: bad key `{key}`"));
        }
        let mut stream =
            serde_json::Deserializer::from_str(value.trim()).into_iter::<serde_json::Value>();
        let parsed = match stream.next() {
            Some(Ok(v)) => v,
            _ => return Err(format!("line {lineno}: `{key}` needs a quoted string or array")),
        };
        let rest = value.trim()[stream.byte_offset()..].trim();
        if !rest.is_empty() && !rest.starts_with('#') {
            return Err(format!("line {lineno}: unexpected text after `{key}`"));
        }
        let text = match parsed {
            serde_json::Value::String(s) => s,
            serde_json::Value::Array(items) if items.iter().all(|i| i.is_string()) => {
                serde_json::Value::Array(items).to_string()
            }
            _ => return Err(format!("line {lineno}: `{key}` must be a string or string array")),
        };
        out.push((key.to_string(), text));
    }
    Ok(out)
}
