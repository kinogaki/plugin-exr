//! The byte predictor ZIP, ZIPS and RLE share: the chunk's bytes split
//! into even and odd halves (low and high bytes of 16-bit words side by
//! side), then each byte replaced by its difference from the one before,
//! plus 128.

/// Splits and delta-codes `raw` for compression.
pub(crate) fn predict(raw: &[u8]) -> Vec<u8> {
    let half = raw.len().div_ceil(2);
    let mut out = vec![0u8; raw.len()];
    let (even, odd) = out.split_at_mut(half);
    for (i, pair) in raw.chunks(2).enumerate() {
        even[i] = pair[0];
        if let Some(&b) = pair.get(1) {
            odd[i] = b;
        }
    }
    let mut previous = out.first().copied().unwrap_or(0);
    for b in out.iter_mut().skip(1) {
        let value = *b;
        *b = value.wrapping_sub(previous).wrapping_add(128);
        previous = value;
    }
    out
}

/// Byte-wise wrapping addition of eight lanes.
#[inline(always)]
fn add_bytes(a: u64, b: u64) -> u64 {
    const LOW: u64 = 0x7F7F_7F7F_7F7F_7F7F;
    ((a & LOW) + (b & LOW)) ^ ((a ^ b) & !LOW)
}

/// Undoes [`predict`]. The running sum goes eight bytes at a time: the
/// prefix sums within a word in three shifted lane-wise additions, then
/// the previous word's last byte added to every lane.
pub(crate) fn unpredict(mut predicted: Vec<u8>) -> Vec<u8> {
    if let Some(first) = predicted.first_mut() {
        // Every byte but the first is a difference plus 128.
        *first ^= 0x80;
    }
    let mut carry = 0u64;
    let (words, rest) = predicted.as_chunks_mut::<8>();
    for word in words {
        let mut w = u64::from_le_bytes(*word) ^ 0x8080_8080_8080_8080;
        w = add_bytes(w, w << 8);
        w = add_bytes(w, w << 16);
        w = add_bytes(w, w << 32);
        w = add_bytes(w, carry * 0x0101_0101_0101_0101);
        carry = w >> 56;
        *word = w.to_le_bytes();
    }
    let mut previous = carry as u8;
    for b in rest {
        previous = previous.wrapping_add(*b ^ 0x80);
        *b = previous;
    }
    let half = predicted.len().div_ceil(2);
    let (even, odd) = predicted.split_at(half);
    let mut raw = vec![0u8; predicted.len()];
    for (i, pair) in raw.chunks_mut(2).enumerate() {
        pair[0] = even[i];
        if pair.len() == 2 {
            pair[1] = odd[i];
        }
    }
    raw
}

#[cfg(test)]
mod tests {
    #[test]
    fn predict_round_trips() {
        for n in [0usize, 1, 2, 3, 17, 1000] {
            let raw: Vec<u8> = (0..n).map(|i| (i * 37 % 256) as u8).collect();
            assert_eq!(super::unpredict(super::predict(&raw)), raw);
        }
    }
}
