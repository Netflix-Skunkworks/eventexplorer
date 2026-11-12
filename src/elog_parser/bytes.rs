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
// Little-endian read helpers
// ---------------------------------------------------------------------------

pub fn read_u16(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(data[off..off + 2].try_into().unwrap())
}

pub fn read_u32(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

pub fn read_u64(data: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(data[off..off + 8].try_into().unwrap())
}

pub fn read_i32(data: &[u8], off: usize) -> i32 {
    i32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

// ---------------------------------------------------------------------------
// Byte source
// ---------------------------------------------------------------------------

/// Sequential view of a capture. `release_before` drops bytes behind the
/// cursor so a later allocation can reuse them.
pub trait ByteSrc {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn u16(&mut self, off: usize) -> u16;
    fn u32(&mut self, off: usize) -> u32;
    fn u64(&mut self, off: usize) -> u64;
    fn i32(&mut self, off: usize) -> i32;
    fn slice(&mut self, start: usize, end: usize) -> &[u8];
    /// `len` bytes starting at `off`, contiguous.
    fn event(&mut self, off: usize, len: usize) -> &[u8];
    fn release_before(&mut self, off: usize);
}

/// Borrowed capture. `release_before` is a no-op; the caller owns the
/// bytes for the whole parse (native mmap).
pub struct SliceSrc<'a>(pub &'a [u8]);

impl ByteSrc for SliceSrc<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn u16(&mut self, off: usize) -> u16 {
        read_u16(self.0, off)
    }
    fn u32(&mut self, off: usize) -> u32 {
        read_u32(self.0, off)
    }
    fn u64(&mut self, off: usize) -> u64 {
        read_u64(self.0, off)
    }
    fn i32(&mut self, off: usize) -> i32 {
        read_i32(self.0, off)
    }
    fn slice(&mut self, start: usize, end: usize) -> &[u8] {
        &self.0[start..end]
    }
    fn event(&mut self, off: usize, len: usize) -> &[u8] {
        &self.0[off..off + len]
    }
    fn release_before(&mut self, _off: usize) {}
}

/// Chunk size for [`ChunkedSrc`]. An event is at most `u16::MAX` bytes,
/// so a read spans at most two chunks.
pub const CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// Capture stored as fixed-size chunks. Dropping a chunk the cursor has
/// passed returns that allocation to the allocator. Wasm linear memory
/// stays at its high-water mark, so a multi-gigabyte capture fits when
/// decoded events reuse the space the raw bytes occupied.
pub struct ChunkedSrc {
    chunks: std::collections::VecDeque<Vec<u8>>,
    /// Logical offset of `chunks[0][0]`.
    origin: usize,
    total: usize,
    hint: usize,
    hint_origin: usize,
    scratch: Vec<u8>,
}

impl ChunkedSrc {
    pub fn new() -> Self {
        Self {
            chunks: std::collections::VecDeque::new(),
            origin: 0,
            total: 0,
            hint: 0,
            hint_origin: 0,
            scratch: Vec::new(),
        }
    }

    pub fn push(&mut self, chunk: Vec<u8>) {
        if chunk.is_empty() {
            return;
        }
        self.total += chunk.len();
        self.chunks.push_back(chunk);
    }

    fn seek(&mut self, off: usize) -> (usize, usize) {
        if off < self.hint_origin {
            self.hint = 0;
            self.hint_origin = self.origin;
        }
        while self.hint < self.chunks.len() {
            let len = self.chunks[self.hint].len();
            if off < self.hint_origin + len {
                return (self.hint, off - self.hint_origin);
            }
            self.hint_origin += len;
            self.hint += 1;
        }
        (self.chunks.len(), 0)
    }

    fn copy_at(&self, off: usize, dst: &mut [u8]) {
        let mut filled = 0;
        let mut pos = self.origin;
        for c in &self.chunks {
            let end = pos + c.len();
            if off + filled >= end {
                pos = end;
                continue;
            }
            let start = off + filled - pos;
            let n = (c.len() - start).min(dst.len() - filled);
            dst[filled..filled + n].copy_from_slice(&c[start..start + n]);
            filled += n;
            if filled == dst.len() {
                return;
            }
            pos = end;
        }
    }

    fn fill_scratch(&mut self, off: usize, len: usize) {
        let origin = self.origin;
        self.scratch.resize(len, 0);
        let mut filled = 0;
        let mut pos = origin;
        let scratch = &mut self.scratch;
        let chunks = &self.chunks;
        for c in chunks.iter() {
            let end = pos + c.len();
            if off + filled >= end {
                pos = end;
                continue;
            }
            let start = off + filled - pos;
            let n = (c.len() - start).min(len - filled);
            scratch[filled..filled + n].copy_from_slice(&c[start..start + n]);
            filled += n;
            if filled == len {
                break;
            }
            pos = end;
        }
    }
}

impl Default for ChunkedSrc {
    fn default() -> Self {
        Self::new()
    }
}

impl ByteSrc for ChunkedSrc {
    fn len(&self) -> usize {
        self.total
    }

    fn u16(&mut self, off: usize) -> u16 {
        let inline = {
            let (ci, start) = self.seek(off);
            if ci < self.chunks.len() && start + 2 <= self.chunks[ci].len() {
                Some(read_u16(&self.chunks[ci], start))
            } else {
                None
            }
        };
        if let Some(v) = inline {
            return v;
        }
        let mut buf = [0u8; 2];
        self.copy_at(off, &mut buf);
        read_u16(&buf, 0)
    }

    fn u32(&mut self, off: usize) -> u32 {
        let inline = {
            let (ci, start) = self.seek(off);
            if ci < self.chunks.len() && start + 4 <= self.chunks[ci].len() {
                Some(read_u32(&self.chunks[ci], start))
            } else {
                None
            }
        };
        if let Some(v) = inline {
            return v;
        }
        let mut buf = [0u8; 4];
        self.copy_at(off, &mut buf);
        read_u32(&buf, 0)
    }

    fn u64(&mut self, off: usize) -> u64 {
        let inline = {
            let (ci, start) = self.seek(off);
            if ci < self.chunks.len() && start + 8 <= self.chunks[ci].len() {
                Some(read_u64(&self.chunks[ci], start))
            } else {
                None
            }
        };
        if let Some(v) = inline {
            return v;
        }
        let mut buf = [0u8; 8];
        self.copy_at(off, &mut buf);
        read_u64(&buf, 0)
    }

    fn i32(&mut self, off: usize) -> i32 {
        let inline = {
            let (ci, start) = self.seek(off);
            if ci < self.chunks.len() && start + 4 <= self.chunks[ci].len() {
                Some(read_i32(&self.chunks[ci], start))
            } else {
                None
            }
        };
        if let Some(v) = inline {
            return v;
        }
        let mut buf = [0u8; 4];
        self.copy_at(off, &mut buf);
        read_i32(&buf, 0)
    }

    fn slice(&mut self, start: usize, end: usize) -> &[u8] {
        self.event(start, end - start)
    }

    fn event(&mut self, off: usize, len: usize) -> &[u8] {
        let in_one = {
            let (ci, start) = self.seek(off);
            if ci < self.chunks.len() && start + len <= self.chunks[ci].len() {
                Some((ci, start))
            } else {
                None
            }
        };
        if let Some((ci, start)) = in_one {
            &self.chunks[ci][start..start + len]
        } else {
            self.fill_scratch(off, len);
            &self.scratch[..len]
        }
    }

    fn release_before(&mut self, off: usize) {
        let mut popped = false;
        while let Some(front) = self.chunks.front() {
            if self.origin + front.len() > off {
                break;
            }
            let n = front.len();
            self.chunks.pop_front();
            self.origin += n;
            popped = true;
        }
        if popped {
            self.hint = 0;
            self.hint_origin = self.origin;
        }
    }
}

/// Read a fixed-width, NUL-padded C string field of `max` bytes starting
/// at `off`, trimming at the first NUL. Returns a lossy-UTF8 `String`.
pub fn read_cstr(data: &[u8], off: usize, max: usize) -> String {
    let raw = &data[off..off + max];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(max);
    String::from_utf8_lossy(&raw[..end]).to_string()
}

/// Format an `if`-provider address payload (`IF_ADDR`) given its
/// address family (`ENUM af`: 2 INET / 28 INET6) and raw address bytes.
pub fn format_if_addr(family: u16, addr: &[u8]) -> String {
    match family {
        2 if addr.len() >= 4 => Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]).to_string(),
        28 if addr.len() >= 16 => {
            let mut o = [0u8; 16];
            o.copy_from_slice(&addr[..16]);
            std::net::Ipv6Addr::from(o).to_string()
        }
        _ => format!("af{family}/{}B", addr.len()),
    }
}

pub fn format_ip_addr(addr: &[u8]) -> String {
    match addr.len() {
        4 => Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]).to_string(),
        16 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(addr);
            std::net::Ipv6Addr::from(octets).to_string()
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod chunked_src_tests {
    use super::*;

    #[test]
    fn reads_across_a_chunk_boundary_and_releases_the_prefix() {
        let mut src = ChunkedSrc::new();
        src.push(vec![0x11, 0x22, 0x33, 0x44]);
        src.push(vec![0x55, 0x66, 0x77, 0x88, 0x99]);
        assert_eq!(src.u32(2), u32::from_le_bytes([0x33, 0x44, 0x55, 0x66]));
        assert_eq!(src.u16(7), u16::from_le_bytes([0x88, 0x99]));
        let ev = src.event(3, 3).to_vec();
        assert_eq!(ev, vec![0x44, 0x55, 0x66]);
        src.release_before(4);
        assert_eq!(src.u32(4), u32::from_le_bytes([0x55, 0x66, 0x77, 0x88]));
        assert_eq!(src.len(), 9);
    }
}
