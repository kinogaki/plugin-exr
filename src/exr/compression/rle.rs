//! Run-length coding (RLE compression, after the shared predictor): a
//! signed count byte, negative for that many literal bytes, else
//! count + 1 copies of the next byte.

/// Decodes into exactly `len` bytes.
pub(crate) fn decode(data: &[u8], len: usize) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::with_capacity(len);
    let mut i = 0;
    while i < data.len() {
        let code = data[i] as i8;
        i += 1;
        if code < 0 {
            let count = usize::from(code.unsigned_abs());
            let literal = data
                .get(i..i + count)
                .ok_or("RLE literal runs past the chunk")?;
            if out.len() + count > len {
                return Err("RLE data longer than the chunk");
            }
            out.extend_from_slice(literal);
            i += count;
        } else {
            let count = code as usize + 1;
            let &value = data.get(i).ok_or("RLE run has no value")?;
            if out.len() + count > len {
                return Err("RLE data longer than the chunk");
            }
            out.resize(out.len() + count, value);
            i += 1;
        }
    }
    if out.len() != len {
        return Err("RLE data shorter than the chunk");
    }
    Ok(out)
}

/// Encodes: runs of three or more repeat (up to 128), the rest as
/// literal runs (up to 127).
pub(crate) fn encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let run_at = |i: usize| -> usize {
        let mut n = 1;
        while n < 128 && i + n < data.len() && data[i + n] == data[i] {
            n += 1;
        }
        n
    };
    let mut i = 0;
    while i < data.len() {
        let run = run_at(i);
        if run >= 3 {
            out.push((run - 1) as u8);
            out.push(data[i]);
            i += run;
            continue;
        }
        let start = i;
        while i < data.len() && i - start < 127 && run_at(i) < 3 {
            i += 1;
        }
        out.push((-((i - start) as i32)) as u8);
        out.extend_from_slice(&data[start..i]);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trips() {
        let mut data = vec![7u8; 300];
        data.extend((0..500).map(|i| (i * 13 % 7) as u8));
        data.extend([1, 1, 2, 2, 2, 3]);
        let packed = super::encode(&data);
        assert!(packed.len() < data.len());
        assert_eq!(super::decode(&packed, data.len()).unwrap(), data);
        assert!(super::decode(&packed, data.len() - 1).is_err());
    }
}
