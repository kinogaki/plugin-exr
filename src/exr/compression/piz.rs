//! PIZ (OpenEXR's algorithm, BSD-3-Clause; see LICENSE-OpenEXR):
//! the chunk's 16-bit words, each channel as its own plane (FLOAT and UINT
//! samples as two words), are renumbered densely by a bitmap of the
//! values that occur, transformed by a 2D Haar-like wavelet per plane, and
//! Huffman coded with runs of a repeated word as a special symbol.
//!
//! Huffman codes are canonical over lengths of up to 58 bits, assigned
//! from the longest length down (OpenEXR's order). Decoding resolves codes
//! of up to 14 bits with one table lookup and longer ones by length.
use super::Shape;
use crate::exr::SampleType;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

const BITMAP_SIZE: usize = 8192;
const MAX_LENGTH: usize = 58;
const LOOKAHEAD: u32 = 14;

/// Words a sample takes.
fn words(t: SampleType) -> usize {
    t.bytes() / 2
}

/// Each channel's first word in the planar layout, and the total.
fn planes(shape: &Shape) -> (Vec<usize>, usize) {
    let mut starts = Vec::with_capacity(shape.types.len());
    let mut at = 0;
    for &t in shape.types {
        starts.push(at);
        at += shape.width * shape.height * words(t);
    }
    (starts, at)
}

pub(crate) fn decode(shape: &Shape, data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let short = "PIZ data ends early";
    let u16_at = |i: usize| {
        data.get(i..i + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .ok_or(short)
    };
    let (min, max) = (usize::from(u16_at(0)?), usize::from(u16_at(2)?));
    if max >= BITMAP_SIZE {
        return Err("PIZ bitmap range past its end");
    }
    let mut bitmap = [0u8; BITMAP_SIZE];
    let mut at = 4;
    if min <= max {
        bitmap[min..=max].copy_from_slice(data.get(at..at + max - min + 1).ok_or(short)?);
        at += max - min + 1;
    }
    // Dense numbers back to values: zero always, then each value present
    // (looked for in the bitmap's range alone).
    let mut lut = vec![0u16];
    if min <= max {
        for value in (min * 8).max(1)..(max + 1) * 8 {
            if bitmap[value >> 3] & (1 << (value & 7)) != 0 {
                lut.push(value as u16);
            }
        }
    }
    let length = data
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes(b.try_into().expect("four")))
        .ok_or(short)? as usize;
    at += 4;
    let huffman = data.get(at..at.saturating_add(length)).ok_or(short)?;
    let (starts, total) = planes(shape);
    let mut words_out = vec![0u16; total];
    huffman_decode(huffman, &mut words_out)?;
    let small = lut.len() <= 1 << 14;
    for (c, &t) in shape.types.iter().enumerate() {
        let n = words(t);
        for j in 0..n {
            wavelet_decode(
                &mut words_out[starts[c] + j..],
                shape.width,
                shape.height,
                n,
                small,
            );
        }
    }
    let mut out = Vec::with_capacity(total * 2);
    for y in 0..shape.height {
        for (c, &t) in shape.types.iter().enumerate() {
            let row = shape.width * words(t);
            for &code in &words_out[starts[c] + y * row..starts[c] + (y + 1) * row] {
                let value = *lut
                    .get(usize::from(code))
                    .ok_or("PIZ word outside its bitmap")?;
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    Ok(out)
}

pub(crate) fn encode(shape: &Shape, raw: &[u8]) -> Vec<u8> {
    let (starts, total) = planes(shape);
    let mut data = vec![0u16; total];
    let mut bitmap = [0u8; BITMAP_SIZE];
    let mut source = raw.as_chunks::<2>().0.iter();
    for y in 0..shape.height {
        for (c, &t) in shape.types.iter().enumerate() {
            let row = shape.width * words(t);
            for word in &mut data[starts[c] + y * row..starts[c] + (y + 1) * row] {
                *word = u16::from_le_bytes(*source.next().expect("raw words"));
                bitmap[usize::from(*word >> 3)] |= 1 << (*word & 7);
            }
        }
    }
    // Zero is implied; the bitmap stores the others.
    bitmap[0] &= !1;
    let (min, max) = match (
        bitmap.iter().position(|&b| b != 0),
        bitmap.iter().rposition(|&b| b != 0),
    ) {
        (Some(min), Some(max)) => (min, max),
        _ => (BITMAP_SIZE - 1, 0),
    };
    let mut lut = vec![0u16; 1 << 16];
    let mut count = 0usize;
    for value in 0..(1usize << 16) {
        if value == 0 || bitmap[value >> 3] & (1 << (value & 7)) != 0 {
            lut[value] = count as u16;
            count += 1;
        }
    }
    for word in &mut data {
        *word = lut[usize::from(*word)];
    }
    let small = count <= 1 << 14;
    for (c, &t) in shape.types.iter().enumerate() {
        let n = words(t);
        for j in 0..n {
            wavelet_encode(
                &mut data[starts[c] + j..],
                shape.width,
                shape.height,
                n,
                small,
            );
        }
    }
    let huffman = huffman_encode(&data);
    let mut out = Vec::with_capacity(huffman.len() + 16 + max.saturating_sub(min));
    out.extend_from_slice(&(min as u16).to_le_bytes());
    out.extend_from_slice(&(max as u16).to_le_bytes());
    if min <= max {
        out.extend_from_slice(&bitmap[min..=max]);
    }
    out.extend_from_slice(&(huffman.len() as u32).to_le_bytes());
    out.extend_from_slice(&huffman);
    out
}

// The wavelet ==========================================================

/// The 14-bit pair transforms (values fit in 15 signed bits) and the
/// 16-bit ones (modular, for wide value ranges).
#[inline(always)]
fn encode_pair(a: u16, b: u16, small: bool) -> (u16, u16) {
    if small {
        let (a, b) = (i32::from(a as i16), i32::from(b as i16));
        return (((a + b) >> 1) as u16, (a - b) as u16);
    }
    let a = (i32::from(a) + 32768) & 0xFFFF;
    let b = i32::from(b);
    let d = a - b;
    let m = ((a + b) >> 1) + if d < 0 { 32768 } else { 0 };
    (m as u16, d as u16)
}

#[inline(always)]
fn decode_pair(l: u16, h: u16, small: bool) -> (u16, u16) {
    if small {
        let h = i32::from(h as i16);
        let a = i32::from(l as i16) + (h & 1) + (h >> 1);
        return (a as u16, (a - h) as u16);
    }
    let b = (i32::from(l) - (i32::from(h) >> 1)) & 0xFFFF;
    let a = (i32::from(h) + b - 32768) & 0xFFFF;
    (a as u16, b as u16)
}

/// The forward wavelet of a `width` × `height` plane of `data` whose
/// samples are `step` words apart (rows `width * step` apart).
fn wavelet_encode(data: &mut [u16], width: usize, height: usize, step: usize, small: bool) {
    let n = width.min(height);
    let at = |x: usize, y: usize| (y * width + x) * step;
    let mut p = 1;
    while p * 2 <= n {
        let p2 = p * 2;
        let mut y = 0;
        while y + p2 <= height {
            let mut x = 0;
            while x + p2 <= width {
                let (a, b, c, d) = (at(x, y), at(x + p, y), at(x, y + p), at(x + p, y + p));
                let (i00, i01) = encode_pair(data[a], data[b], small);
                let (i10, i11) = encode_pair(data[c], data[d], small);
                (data[a], data[c]) = encode_pair(i00, i10, small);
                (data[b], data[d]) = encode_pair(i01, i11, small);
                x += p2;
            }
            if width & p != 0 {
                let (a, c) = (at(x, y), at(x, y + p));
                (data[a], data[c]) = encode_pair(data[a], data[c], small);
            }
            y += p2;
        }
        if height & p != 0 {
            let mut x = 0;
            while x + p2 <= width {
                let (a, b) = (at(x, y), at(x + p, y));
                (data[a], data[b]) = encode_pair(data[a], data[b], small);
                x += p2;
            }
        }
        p = p2;
    }
}

/// The inverse of [`wavelet_encode`].
fn wavelet_decode(data: &mut [u16], width: usize, height: usize, step: usize, small: bool) {
    let n = width.min(height);
    let at = |x: usize, y: usize| (y * width + x) * step;
    let mut p = 1;
    while p <= n {
        p <<= 1;
    }
    p >>= 1;
    let mut p2 = p;
    p >>= 1;
    while p > 0 {
        let mut y = 0;
        while y + p2 <= height {
            let mut x = 0;
            while x + p2 <= width {
                let (a, b, c, d) = (at(x, y), at(x + p, y), at(x, y + p), at(x + p, y + p));
                let (i00, i10) = decode_pair(data[a], data[c], small);
                let (i01, i11) = decode_pair(data[b], data[d], small);
                (data[a], data[b]) = decode_pair(i00, i01, small);
                (data[c], data[d]) = decode_pair(i10, i11, small);
                x += p2;
            }
            if width & p != 0 {
                let (a, c) = (at(x, y), at(x, y + p));
                (data[a], data[c]) = decode_pair(data[a], data[c], small);
            }
            y += p2;
        }
        if height & p != 0 {
            let mut x = 0;
            while x + p2 <= width {
                let (a, b) = (at(x, y), at(x + p, y));
                (data[a], data[b]) = decode_pair(data[a], data[b], small);
                x += p2;
            }
        }
        p2 = p;
        p >>= 1;
    }
}

// Huffman ==============================================================

/// Codes of each length, from the longest down: the first code of a
/// length is (that of the next longer + its count) / 2.
fn canonical_bases(counts: &[u64; MAX_LENGTH + 1]) -> Option<[u64; MAX_LENGTH + 1]> {
    let mut base = [0u64; MAX_LENGTH + 1];
    let mut code = 0u64;
    for length in (1..=MAX_LENGTH).rev() {
        base[length] = code;
        if code + counts[length] > 1u64 << length {
            return None;
        }
        code = (code + counts[length]) >> 1;
    }
    Some(base)
}

/// Bits read most significant first, zero past the end.
struct BitStream<'a> {
    data: &'a [u8],
}

impl BitStream<'_> {
    /// At least 57 bits from bit `p`, at the top of the word.
    #[inline(always)]
    fn peek(&self, p: usize) -> u64 {
        let byte = p >> 3;
        let word = match self.data.get(byte..byte + 8) {
            Some(bytes) => u64::from_be_bytes(bytes.try_into().expect("eight")),
            None => {
                let mut padded = [0u8; 8];
                let rest = self.data.get(byte..).unwrap_or(&[]);
                padded[..rest.len()].copy_from_slice(rest);
                u64::from_be_bytes(padded)
            }
        };
        word << (p & 7)
    }

    /// 64 bits from bit `p` (for codes past 57 bits).
    fn peek_wide(&self, p: usize) -> u64 {
        let high = self.peek(p);
        let low = self.peek(p + 32);
        high & !0xFFFF_FFFF | low >> 32
    }
}

/// Reads the code lengths of symbols `first..=last` (the first at index 0):
/// six bits each, 59..62 a run of 2..5 zeros, 63 then eight bits a run of
/// 6..261 zeros.
fn read_lengths(table: &[u8], first: usize, last: usize) -> Result<Vec<u8>, &'static str> {
    let stream = BitStream { data: table };
    let end = table.len() * 8;
    let mut p = 0;
    let mut lengths = vec![0u8; last + 1 - first];
    let mut symbol = first;
    while symbol <= last {
        if p + 6 > end {
            return Err("PIZ code table ends early");
        }
        let length = (stream.peek(p) >> 58) as usize;
        p += 6;
        if length >= 59 {
            let run = if length == 63 {
                if p + 8 > end {
                    return Err("PIZ code table ends early");
                }
                let n = (stream.peek(p) >> 56) as usize + 6;
                p += 8;
                n
            } else {
                length - 59 + 2
            };
            if run > last + 1 - symbol {
                return Err("PIZ zero run past the code table");
            }
            symbol += run;
        } else {
            lengths[symbol - first] = length as u8;
            symbol += 1;
        }
    }
    Ok(lengths)
}

/// Decodes a Huffman block (20-byte header, code table, bits) into
/// exactly `out.len()` words.
fn huffman_decode(data: &[u8], out: &mut [u16]) -> Result<(), &'static str> {
    let field = |i: usize| {
        data.get(i * 4..i * 4 + 4)
            .map(|b| u32::from_le_bytes(b.try_into().expect("four")) as usize)
            .ok_or("PIZ Huffman header ends early")
    };
    let (first, last, table_len, bits) = (field(0)?, field(1)?, field(2)?, field(3)?);
    if first > last || last > 65_536 {
        return Err("invalid PIZ Huffman symbol range");
    }
    let table = data
        .get(20..20usize.saturating_add(table_len))
        .ok_or("PIZ code table ends early")?;
    let lengths = read_lengths(table, first, last)?;
    let coded = &data[20 + table_len..];
    if bits > coded.len() * 8 {
        return Err("PIZ Huffman bits past the data");
    }
    let mut counts = [0u64; MAX_LENGTH + 1];
    for &l in &lengths {
        counts[usize::from(l)] += 1;
    }
    counts[0] = 0;
    let base =
        canonical_bases(&counts).ok_or("PIZ Huffman lengths oversubscribe the code space")?;
    // Symbols by length, each length in symbol order; and the lookup table.
    let mut offset = [0usize; MAX_LENGTH + 2];
    for length in 1..=MAX_LENGTH {
        offset[length + 1] = offset[length] + counts[length] as usize;
    }
    let mut next = offset;
    let mut symbols = vec![0u32; offset[MAX_LENGTH + 1]];
    // The lookup table spans the longest code's bits, at most LOOKAHEAD's:
    // a chunk of short codes builds a small one.
    let longest = (1..=MAX_LENGTH).rev().find(|&l| counts[l] > 0).unwrap_or(1);
    let look = (longest as u32).min(LOOKAHEAD);
    let mut fast = vec![0u32; 1 << look];
    for (at, &l) in lengths.iter().enumerate() {
        let (symbol, l) = (first + at, usize::from(l));
        if l == 0 {
            continue;
        }
        let rank = next[l] - offset[l];
        symbols[next[l]] = symbol as u32;
        next[l] += 1;
        if l <= look as usize {
            let code = (base[l] + rank as u64) as usize;
            let spare = look as usize - l;
            let entry = (symbol as u32) << 8 | l as u32;
            fast[code << spare..(code + 1) << spare].fill(entry);
        }
    }
    let rlc = last as u32;
    let stream = BitStream { data: coded };
    let mut p = 0usize;
    let mut i = 0usize;
    while p < bits {
        let word = stream.peek(p);
        let entry = fast[(word >> (64 - look)) as usize];
        let (symbol, length) = if entry != 0 {
            (entry >> 8, (entry & 0xFF) as usize)
        } else {
            let wide = stream.peek_wide(p);
            let mut found = None;
            for length in look as usize + 1..=MAX_LENGTH {
                let value = wide >> (64 - length);
                if value >= base[length] && value - base[length] < counts[length] {
                    found = Some((
                        symbols[offset[length] + (value - base[length]) as usize],
                        length,
                    ));
                    break;
                }
            }
            found.ok_or("invalid PIZ Huffman code")?
        };
        p += length;
        if symbol == rlc {
            let run = (stream.peek(p) >> 56) as usize;
            p += 8;
            if i == 0 || run > out.len() - i {
                return Err("invalid PIZ run");
            }
            let previous = out[i - 1];
            out[i..i + run].fill(previous);
            i += run;
        } else {
            if i == out.len() {
                return Err("PIZ Huffman data longer than the chunk");
            }
            out[i] = symbol as u16;
            i += 1;
        }
    }
    if p > bits || i != out.len() {
        return Err("PIZ Huffman data does not fill the chunk");
    }
    Ok(())
}

/// Bits written most significant first.
struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    count: u32,
    total: usize,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter {
            bytes: Vec::new(),
            acc: 0,
            count: 0,
            total: 0,
        }
    }

    /// The low `n` bits (at most 32) of `value`.
    #[inline(always)]
    fn put(&mut self, value: u64, n: u32) {
        self.acc = self.acc << n | value & ((1u64 << n) - 1);
        self.count += n;
        self.total += n as usize;
        while self.count >= 8 {
            self.count -= 8;
            self.bytes.push((self.acc >> self.count) as u8);
        }
    }

    /// A code of up to 58 bits.
    #[inline(always)]
    fn code(&mut self, code: u64, length: u32) {
        if length > 32 {
            self.put(code >> 32, length - 32);
            self.put(code, 32);
        } else {
            self.put(code, length);
        }
    }

    fn finish(mut self) -> (Vec<u8>, usize) {
        if self.count > 0 {
            self.bytes.push((self.acc << (8 - self.count)) as u8);
        }
        (self.bytes, self.total)
    }
}

/// Huffman code lengths for `freq` (zero for unused symbols).
fn code_lengths(freq: &[u64]) -> Vec<u8> {
    let used: Vec<usize> = (0..freq.len()).filter(|&s| freq[s] > 0).collect();
    let mut lengths = vec![0u8; freq.len()];
    if used.len() == 1 {
        lengths[used[0]] = 1;
        return lengths;
    }
    let mut parent = vec![usize::MAX; used.len() * 2];
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> = used
        .iter()
        .enumerate()
        .map(|(i, &s)| Reverse((freq[s], i)))
        .collect();
    let mut nodes = used.len();
    while heap.len() > 1 {
        let Reverse((fa, a)) = heap.pop().expect("two");
        let Reverse((fb, b)) = heap.pop().expect("two");
        parent[a] = nodes;
        parent[b] = nodes;
        heap.push(Reverse((fa + fb, nodes)));
        nodes += 1;
    }
    let mut depth = vec![0u32; nodes];
    for i in (0..nodes - 1).rev() {
        depth[i] = depth[parent[i]] + 1;
    }
    if depth[..used.len()].iter().any(|&d| d as usize > MAX_LENGTH) {
        // Unreachable for chunk-sized counts; equal lengths are still valid.
        let flat = (usize::BITS - (used.len() - 1).leading_zeros()) as u8;
        for &s in &used {
            lengths[s] = flat;
        }
        return lengths;
    }
    for (i, &s) in used.iter().enumerate() {
        lengths[s] = depth[i] as u8;
    }
    lengths
}

/// Huffman-codes `data` with a run symbol, as a block `huffman_decode`
/// reads.
fn huffman_encode(data: &[u16]) -> Vec<u8> {
    let mut freq = vec![0u64; 65_537];
    for &w in data {
        freq[usize::from(w)] += 1;
    }
    let first = freq.iter().position(|&f| f > 0).unwrap_or(0);
    let rlc = freq.iter().rposition(|&f| f > 0).unwrap_or(0) + 1;
    freq[rlc] = 1;
    let last = rlc;
    let lengths = code_lengths(&freq[..=last]);
    let mut counts = [0u64; MAX_LENGTH + 1];
    for &l in &lengths {
        counts[usize::from(l)] += 1;
    }
    counts[0] = 0;
    let base = canonical_bases(&counts).expect("Huffman lengths fit");
    let mut next = base;
    let mut codes = vec![0u64; last + 1];
    for (s, &l) in lengths.iter().enumerate() {
        if l > 0 {
            codes[s] = next[usize::from(l)];
            next[usize::from(l)] += 1;
        }
    }
    // The table: lengths with zero runs.
    let mut table = BitWriter::new();
    let mut s = first;
    while s <= last {
        if lengths[s] == 0 {
            let mut run = 1;
            while s + run <= last && run < 261 && lengths[s + run] == 0 {
                run += 1;
            }
            if run >= 6 {
                table.put(63, 6);
                table.put((run - 6) as u64, 8);
                s += run;
                continue;
            }
            if run >= 2 {
                table.put((59 + run - 2) as u64, 6);
                s += run;
                continue;
            }
        }
        table.put(u64::from(lengths[s]), 6);
        s += 1;
    }
    let (table, _) = table.finish();
    // The data, runs of up to 255 repeats coded as the run symbol and a
    // count where that is shorter.
    let mut bits = BitWriter::new();
    let send = |bits: &mut BitWriter, symbol: usize, repeats: usize| {
        let (code, length) = (codes[symbol], u32::from(lengths[symbol]));
        let run_length = u32::from(lengths[rlc]);
        if (length + run_length + 8) as usize > length as usize * repeats {
            for _ in 0..=repeats {
                bits.code(code, length);
            }
        } else {
            bits.code(code, length);
            bits.code(codes[rlc], run_length);
            bits.put(repeats as u64, 8);
        }
    };
    let mut current = usize::from(data[0]);
    let mut repeats = 0;
    for &w in &data[1..] {
        let w = usize::from(w);
        if w == current && repeats < 255 {
            repeats += 1;
        } else {
            send(&mut bits, current, repeats);
            repeats = 0;
            current = w;
        }
    }
    send(&mut bits, current, repeats);
    let (coded, total) = bits.finish();
    let mut out = Vec::with_capacity(20 + table.len() + coded.len());
    for field in [first, last, table.len(), total, 0] {
        out.extend_from_slice(&(field as u32).to_le_bytes());
    }
    out.extend_from_slice(&table);
    out.extend_from_slice(&coded);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wavelet_round_trips_in_both_ranges() {
        for (width, height) in [(1, 1), (7, 5), (16, 16), (33, 9)] {
            for small in [true, false] {
                let original: Vec<u16> = (0..width * height * 2)
                    .map(|i| {
                        if small {
                            (i * 37 % 9000) as u16
                        } else {
                            (i * 7919 % 65536) as u16
                        }
                    })
                    .collect();
                let mut data = original.clone();
                for j in 0..2 {
                    wavelet_encode(&mut data[j..], width, height, 2, small);
                }
                for j in 0..2 {
                    wavelet_decode(&mut data[j..], width, height, 2, small);
                }
                assert_eq!(data, original, "{width}x{height} small {small}");
            }
        }
    }

    #[test]
    fn huffman_round_trips_with_runs_and_long_codes() {
        let mut data: Vec<u16> = vec![5; 1000];
        data.extend((0..3000).map(|i| (i % 77) as u16));
        // Skewed frequencies give long codes.
        let (mut a, mut b) = (1usize, 1usize);
        for s in 0..30u16 {
            data.extend(std::iter::repeat_n(1000 + s, a.min(5000)));
            (a, b) = (b, a + b);
        }
        let packed = huffman_encode(&data);
        let mut out = vec![0u16; data.len()];
        huffman_decode(&packed, &mut out).unwrap();
        assert_eq!(out, data);
        let mut short = vec![0u16; data.len() - 1];
        assert!(huffman_decode(&packed, &mut short).is_err());
    }
}
