//! Minimal dependency-free ZIP reader: Stored + Deflate.
//!
//! Only what a decompiler needs from jars: list entries, extract `.class`
//! bytes. Supports methods 0 (stored) and 8 (deflated) via a small
//! RFC1951 inflate implementation below. No encryption, no zip64, no
//! multi-disk — those entries are skipped, never fatal.

use std::collections::HashMap;

/// A raw central-directory entry we care about.
#[derive(Debug, Clone)]
struct CentralEntry {
    name: String,
    method: u16,
    flags: u16,
    comp_size: u32,
    uncomp_size: u32,
    local_offset: u32,
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    bytes
        .get(offset..offset + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// IEEE CRC32, table generated on the fly (small + dependency-free).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xedb8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

fn is_safe_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') || name.starts_with('\\') {
        return false;
    }
    if name.contains(':') {
        // Windows drive (`C:...`) — keep behaviour close to `enclosed_name`.
        return false;
    }
    for part in name.replace('\\', "/").split('/') {
        if part == ".." {
            return false;
        }
    }
    true
}

fn find_eocd(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 22 {
        return None;
    }
    // EOCD may carry a comment (up to 64k); scan backwards.
    let start = bytes.len().saturating_sub(22 + 65535);
    let mut i = bytes.len().saturating_sub(22);
    loop {
        if bytes.get(i..i + 4) == Some(&[0x50, 0x4b, 0x05, 0x06][..]) {
            return Some(i);
        }
        if i == start {
            return None;
        }
        i -= 1;
    }
}

fn parse_central_directory(bytes: &[u8]) -> Vec<CentralEntry> {
    let mut out = Vec::new();
    let eocd = match find_eocd(bytes) {
        Some(e) => e,
        None => {
            // No EOCD: fall back to sequential local-header scan.
            return scan_local_headers(bytes);
        }
    };
    let total = read_u16_le(bytes, eocd + 10).unwrap_or(0) as usize;
    let cd_offset = read_u32_le(bytes, eocd + 16).unwrap_or(0) as usize;
    let mut off = cd_offset;
    for _ in 0..total {
        if bytes.get(off..off + 4) != Some(&[0x50, 0x4b, 0x01, 0x02][..]) {
            break;
        }
        let flags = read_u16_le(bytes, off + 8).unwrap_or(0);
        let method = read_u16_le(bytes, off + 10).unwrap_or(0);
        let comp_size = read_u32_le(bytes, off + 20).unwrap_or(0);
        let uncomp_size = read_u32_le(bytes, off + 24).unwrap_or(0);
        let name_len = read_u16_le(bytes, off + 28).unwrap_or(0) as usize;
        let extra_len = read_u16_le(bytes, off + 30).unwrap_or(0) as usize;
        let comment_len = read_u16_le(bytes, off + 32).unwrap_or(0) as usize;
        let local_offset = read_u32_le(bytes, off + 42).unwrap_or(0);
        let name_start = off + 46;
        let name_bytes = bytes.get(name_start..name_start + name_len).unwrap_or(&[]);
        // ZIP names are UTF-8 (or ASCII for jars in practice); lossy is fine.
        let name = String::from_utf8_lossy(name_bytes).replace('\\', "/");
        off = name_start + name_len + extra_len + comment_len;
        if is_safe_name(&name) {
            out.push(CentralEntry {
                name,
                method,
                flags,
                comp_size,
                uncomp_size,
                local_offset,
            });
        }
    }
    // Empty central directory but input looks like a zip: try local scan.
    if out.is_empty() {
        return scan_local_headers(bytes);
    }
    out
}

/// Fallback: walk local file headers front-to-back (no EOCD needed).
fn scan_local_headers(bytes: &[u8]) -> Vec<CentralEntry> {
    let mut out = Vec::new();
    let mut off = 0usize;
    let mut guard = 0usize;
    while off + 30 <= bytes.len() && guard < 100_000 {
        guard += 1;
        if bytes.get(off..off + 4) != Some(&[0x50, 0x4b, 0x03, 0x04][..]) {
            off += 1;
            continue;
        }
        let flags = read_u16_le(bytes, off + 6).unwrap_or(0);
        let method = read_u16_le(bytes, off + 8).unwrap_or(0);
        let comp_size = read_u32_le(bytes, off + 18).unwrap_or(0) as usize;
        let name_len = read_u16_le(bytes, off + 26).unwrap_or(0) as usize;
        let extra_len = read_u16_le(bytes, off + 28).unwrap_or(0) as usize;
        let name_start = off + 30;
        let Some(name_bytes) = bytes.get(name_start..name_start + name_len) else {
            break;
        };
        let name = String::from_utf8_lossy(name_bytes).replace('\\', "/");
        let data_start = name_start + name_len + extra_len;
        // Bit 3 = data descriptor: sizes in local header are unreliable.
        // Without a central directory we cannot know the length — skip.
        if flags & 0x08 != 0 {
            break;
        }
        let Some(_) = bytes.get(data_start..data_start + comp_size) else {
            break;
        };
        if is_safe_name(&name) {
            out.push(CentralEntry {
                name,
                method,
                flags,
                comp_size: comp_size as u32,
                uncomp_size: 0,
                local_offset: off as u32,
            });
        }
        off = data_start + comp_size;
    }
    out
}

fn local_data_range(bytes: &[u8], entry: &CentralEntry) -> Option<(usize, usize)> {
    let off = entry.local_offset as usize;
    if bytes.get(off..off + 4) != Some(&[0x50, 0x4b, 0x03, 0x04][..]) {
        return None;
    }
    let name_len = read_u16_le(bytes, off + 26)? as usize;
    let extra_len = read_u16_le(bytes, off + 28)? as usize;
    let data_start = off + 30 + name_len + extra_len;
    let len = entry.comp_size as usize;
    bytes.get(data_start..data_start + len)?;
    Some((data_start, len))
}

/// Extract every `.class` entry as `(archive_path, bytes)`.
pub fn extract_class_files(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in parse_central_directory(bytes) {
        if !entry.name.ends_with(".class") {
            continue;
        }
        if let Some(decoded) = decode_entry(bytes, &entry) {
            out.push((entry.name, decoded));
        }
    }
    out
}

/// Find one `.class` entry by archive path (`com/foo/Bar.class`) and
/// extract it without decompressing the rest of the archive.
/// Used for on-demand classpath lookups.
pub fn find_class_bytes(archive: &[u8], internal_path: &str) -> Option<Vec<u8>> {
    let want = internal_path.replace('\\', "/");
    parse_central_directory(archive)
        .into_iter()
        .find(|entry| entry.name == want)
        .and_then(|entry| decode_entry(archive, &entry))
}

fn decode_entry(bytes: &[u8], entry: &CentralEntry) -> Option<Vec<u8>> {
    // Encrypted entries are skipped, never fatal.
    if entry.flags & 0x01 != 0 {
        return None;
    }
    let (start, len) = local_data_range(bytes, entry)?;
    let raw = bytes.get(start..start + len)?;
    match entry.method {
        0 => Some(raw.to_vec()),
        8 => inflate(raw, entry.uncomp_size as usize),
        _ => None,
    }
}

/// Entry-name de-duplication helper shared by the CLI writer.
pub fn sanitize_name(name: &str) -> Option<String> {
    let n = name.replace('\\', "/");
    if is_safe_name(&n) { Some(n) } else { None }
}

// ─── Minimal RFC1951 inflate (stored / fixed / dynamic) ───

struct BitReader<'a> {
    data: &'a [u8],
    byte: usize,
    bit: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte: 0,
            bit: 0,
        }
    }
    fn read_bit(&mut self) -> Option<u32> {
        let b = *self.data.get(self.byte)?;
        let v = ((b >> self.bit) & 1) as u32;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.byte += 1;
        }
        Some(v)
    }
    fn read_bits(&mut self, n: u8) -> Option<u32> {
        let mut v = 0u32;
        for i in 0..n {
            v |= self.read_bit()? << i;
        }
        Some(v)
    }
    fn align_to_byte(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.byte += 1;
        }
    }
}

fn reverse_bits(mut code: u16, len: u8) -> u16 {
    let mut rev = 0u16;
    for _ in 0..len {
        rev = (rev << 1) | (code & 1);
        code >>= 1;
    }
    rev
}

struct Huffman {
    /// lengths indexed decode: map (len -> Vec<(revcode, symbol)>)
    table: HashMap<(u8, u16), u16>,
    max_len: u8,
}

impl Huffman {
    fn from_lengths(lengths: &[u8]) -> Self {
        let mut table = HashMap::new();
        let max_len = *lengths.iter().max().unwrap_or(&0);
        // Canonical code assignment.
        let mut code: u16 = 0;
        let mut next_code: HashMap<u8, u16> = HashMap::new();
        for len in 1..=max_len {
            code <<= 1;
            next_code.insert(len, code);
            let count = lengths.iter().filter(|&&l| l == len).count() as u16;
            code += count;
        }
        let mut assigned: HashMap<u8, u16> = HashMap::new();
        for (sym, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let base = next_code[&len];
            let off = assigned.entry(len).or_insert(0);
            let c = base + *off;
            *off += 1;
            table.insert((len, reverse_bits(c, len)), sym as u16);
        }
        Self { table, max_len }
    }

    fn decode(&self, br: &mut BitReader) -> Option<u16> {
        let mut code: u16 = 0;
        for len in 1..=self.max_len {
            let bit = br.read_bit()? as u16;
            code |= bit << (len - 1);
            if let Some(&sym) = self.table.get(&(len, code)) {
                return Some(sym);
            }
        }
        None
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

fn fixed_litlen() -> Huffman {
    let mut lens = vec![0u8; 288];
    for b in lens.iter_mut().take(144) {
        *b = 8;
    }
    for b in lens.iter_mut().take(256).skip(144) {
        *b = 9;
    }
    for b in lens.iter_mut().take(280).skip(256) {
        *b = 7;
    }
    for b in lens.iter_mut().take(288).skip(280) {
        *b = 8;
    }
    Huffman::from_lengths(&lens)
}

fn fixed_dist() -> Huffman {
    Huffman::from_lengths(&[5u8; 32])
}

fn decode_dynamic(br: &mut BitReader) -> Option<(Huffman, Huffman)> {
    let hlit = br.read_bits(5)? as usize + 257;
    let hdist = br.read_bits(5)? as usize + 1;
    let hclen = br.read_bits(4)? as usize + 4;
    if hlit > 286 || hdist > 32 {
        return None;
    }
    let mut cl_lens = vec![0u8; 19];
    for i in 0..hclen {
        cl_lens[CL_ORDER[i]] = br.read_bits(3)? as u8;
    }
    let cl_huff = Huffman::from_lengths(&cl_lens);
    let total = hlit + hdist;
    let mut lens: Vec<u8> = Vec::with_capacity(total);
    while lens.len() < total {
        let sym = cl_huff.decode(br)? as usize;
        match sym {
            0..=15 => lens.push(sym as u8),
            16 => {
                let rep = br.read_bits(2)? as usize + 3;
                let last = *lens.last()?;
                for _ in 0..rep {
                    lens.push(last);
                }
            }
            17 => {
                let rep = br.read_bits(3)? as usize + 3;
                lens.extend(std::iter::repeat_n(0, rep));
            }
            18 => {
                let rep = br.read_bits(7)? as usize + 11;
                lens.extend(std::iter::repeat_n(0, rep));
            }
            _ => return None,
        }
        if lens.len() > total {
            return None;
        }
    }
    let litlen = Huffman::from_lengths(&lens[..hlit]);
    let dist = Huffman::from_lengths(&lens[hlit..]);
    Some((litlen, dist))
}

fn inflate_block(
    br: &mut BitReader,
    litlen: &Huffman,
    dist_huff: &Huffman,
    out: &mut Vec<u8>,
) -> Option<bool> {
    loop {
        let sym = litlen.decode(br)? as usize;
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Some(false);
        } else if sym <= 285 {
            let idx = sym - 257;
            let mut len = LEN_BASE[idx] as u32;
            let eb = LEN_EXTRA[idx];
            if eb > 0 {
                len += br.read_bits(eb)?;
            }
            let dsym = dist_huff.decode(br)? as usize;
            if dsym >= 30 {
                return None;
            }
            let mut dist = DIST_BASE[dsym] as u32;
            let deb = DIST_EXTRA[dsym];
            if deb > 0 {
                dist += br.read_bits(deb)?;
            }
            if dist == 0 || dist as usize > out.len() {
                return None;
            }
            for _ in 0..len {
                let b = out[out.len() - dist as usize];
                out.push(b);
            }
        } else {
            return None;
        }
        // Safety cap: jars are small, but never let corrupt streams OOM us.
        if out.len() > 256 * 1024 * 1024 {
            return None;
        }
    }
}

/// Decompress raw DEFLATE data. `hint` is the uncompressed size from the
/// central directory (used only for capacity reservation).
pub fn inflate(data: &[u8], hint: usize) -> Option<Vec<u8>> {
    let mut br = BitReader::new(data);
    let mut out = Vec::with_capacity(hint.min(16 * 1024 * 1024));
    loop {
        let final_block = br.read_bit()? != 0;
        let btype = br.read_bits(2)?;
        match btype {
            0 => {
                br.align_to_byte();
                if br.byte + 4 > data.len() {
                    return None;
                }
                let len = u16::from_le_bytes([data[br.byte], data[br.byte + 1]]) as usize;
                let nlen = u16::from_le_bytes([data[br.byte + 2], data[br.byte + 3]]) as usize;
                if len ^ 0xffff != nlen {
                    return None;
                }
                br.byte += 4;
                if br.byte + len > data.len() {
                    return None;
                }
                out.extend_from_slice(&data[br.byte..br.byte + len]);
                br.byte += len;
            }
            1 => {
                let lit = fixed_litlen();
                let dist = fixed_dist();
                inflate_block(&mut br, &lit, &dist, &mut out)?;
            }
            2 => {
                let (lit, dist) = decode_dynamic(&mut br)?;
                inflate_block(&mut br, &lit, &dist, &mut out)?;
            }
            _ => return None,
        }
        if final_block {
            break;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn inflate_stored_block() {
        // BFINAL=1, BTYPE=00, LEN=3, "hey"
        let raw = [0x01, 0x03, 0x00, 0xfc, 0xff, b'h', b'e', b'y'];
        assert_eq!(inflate(&raw, 3), Some(b"hey".to_vec()));
    }

    #[test]
    fn inflate_dynamic_huffman_zlib_vector() {
        // Raw DEFLATE (wbits=-15) of `"The quick brown fox jumps over the lazy dog. " * 20`,
        // produced by zlib level 9 — exercises dynamic Huffman + LZ77 matches.
        let raw: [u8; 55] = [
            11, 201, 72, 85, 40, 44, 205, 76, 206, 86, 72, 42, 202, 47, 207, 83, 72, 203, 175, 80,
            200, 42, 205, 45, 40, 86, 200, 47, 75, 45, 82, 40, 1, 74, 231, 36, 86, 85, 42, 164,
            228, 167, 235, 41, 132, 140, 42, 30, 85, 60, 170, 152, 218, 138, 1,
        ];
        let expected = "The quick brown fox jumps over the lazy dog. "
            .repeat(20)
            .into_bytes();
        assert_eq!(inflate(&raw, expected.len()), Some(expected));
    }
}
