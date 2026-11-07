//! PXR24 (OpenEXR's algorithm, BSD-3-Clause; see THIRD_PARTY_NOTICES.md):
//! FLOAT samples rounded to 24 bits, every sample replaced by its
//! difference from the one to its left, each scanline's channel split
//! into byte planes (most significant first), then zlib. HALF and UINT
//! samples are kept whole (2 and 4 planes), so only FLOAT is lossy.
use super::Shape;
use crate::exr::SampleType;
use kinogaki_deflate::Level;

/// Bytes a sample keeps.
fn planes(t: SampleType) -> usize {
    match t {
        SampleType::F16 => 2,
        SampleType::F32 => 3,
        SampleType::U32 => 4,
    }
}

/// A float's bits rounded to 24 (sign, exponent, 15 mantissa bits), as
/// OpenEXR rounds them: to nearest, never up into infinity, NaNs kept NaN.
fn float24(bits: u32) -> u32 {
    let sign = bits & 0x8000_0000;
    let exponent = bits & 0x7F80_0000;
    let mantissa = bits & 0x007F_FFFF;
    let rest = if exponent == 0x7F80_0000 {
        if mantissa != 0 {
            let m = mantissa >> 8;
            (exponent >> 8) | m | u32::from(m == 0)
        } else {
            exponent >> 8
        }
    } else {
        let rounded = ((exponent | mantissa) + (mantissa & 0x80)) >> 8;
        if rounded >= 0x7F_8000 {
            (exponent | mantissa) >> 8
        } else {
            rounded
        }
    };
    (sign >> 8) | rest
}

pub(crate) fn decode(shape: &Shape, data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let row_bytes: usize = shape.types.iter().map(|&t| planes(t) * shape.width).sum();
    let packed_len = row_bytes * shape.height;
    let packed = kinogaki_deflate::zlib_decompress(data, packed_len)
        .map_err(|_| "PXR24 zlib data does not decompress")?;
    if packed.len() != packed_len {
        return Err("PXR24 data has the wrong size");
    }
    let mut out = Vec::with_capacity(shape.raw_len());
    let width = shape.width;
    let mut source = 0;
    for _ in 0..shape.height {
        for &t in shape.types {
            let n = planes(t);
            let rows: Vec<&[u8]> = (0..n)
                .map(|b| &packed[source + b * width..source + (b + 1) * width])
                .collect();
            let mut previous = 0u32;
            for x in 0..width {
                let mut diff = 0u32;
                for row in &rows {
                    diff = diff << 8 | u32::from(row[x]);
                }
                if t == SampleType::F32 {
                    diff <<= 8;
                }
                previous = previous.wrapping_add(diff);
                match t {
                    SampleType::F16 => out.extend_from_slice(&(previous as u16).to_le_bytes()),
                    _ => out.extend_from_slice(&previous.to_le_bytes()),
                }
            }
            source += n * width;
        }
    }
    Ok(out)
}

pub(crate) fn encode(shape: &Shape, raw: &[u8], level: Level) -> Vec<u8> {
    let width = shape.width;
    let row_bytes: usize = shape.types.iter().map(|&t| planes(t) * width).sum();
    let mut packed = vec![0u8; row_bytes * shape.height];
    let mut source = 0;
    let mut target = 0;
    for _ in 0..shape.height {
        for &t in shape.types {
            let n = planes(t);
            let mut previous = 0u32;
            for x in 0..width {
                let value = match t {
                    SampleType::F16 => {
                        let v = u32::from(u16::from_le_bytes([raw[source], raw[source + 1]]));
                        source += 2;
                        v
                    }
                    _ => {
                        let v = u32::from_le_bytes(
                            raw[source..source + 4].try_into().expect("four bytes"),
                        );
                        source += 4;
                        if t == SampleType::F32 { float24(v) } else { v }
                    }
                };
                let diff = value.wrapping_sub(previous);
                previous = value;
                for b in 0..n {
                    packed[target + b * width + x] = (diff >> (8 * (n - 1 - b))) as u8;
                }
            }
            target += n * width;
        }
    }
    kinogaki_deflate::zlib_compress(&packed, level)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_round_to_24_bits() {
        assert_eq!(float24(1.0f32.to_bits()), 0x3F_8000);
        // Halfway up rounds away; the largest finite never becomes infinity.
        assert_eq!(float24(0x3F80_0080), 0x3F_8001);
        assert_eq!(float24(f32::MAX.to_bits()), 0x7F_7FFF);
        assert_eq!(float24(f32::INFINITY.to_bits()), 0x7F_8000);
        assert_ne!(float24(f32::NAN.to_bits()) & 0x7FFF, 0);
        assert_ne!(float24(0x7F80_0001) & 0x7FFF, 0);
    }

    #[test]
    fn halves_and_uints_are_lossless() {
        let types = [SampleType::F16, SampleType::F32, SampleType::U32];
        let shape = Shape {
            width: 5,
            height: 3,
            types: &types,
        };
        let mut raw = Vec::new();
        for y in 0..3u32 {
            for x in 0..5u32 {
                raw.extend_from_slice(&((y * 977 + x * 31) as u16).to_le_bytes());
            }
            for x in 0..5u32 {
                raw.extend_from_slice(&(1.5f32 + x as f32).to_le_bytes());
            }
            for x in 0..5u32 {
                raw.extend_from_slice(&(y.wrapping_mul(0x9E37_79B9) ^ x).to_le_bytes());
            }
        }
        let back = decode(&shape, &encode(&shape, &raw, Level::Balanced)).unwrap();
        // Floats of few mantissa bits survive 24-bit rounding exactly.
        assert_eq!(back, raw);
    }
}
