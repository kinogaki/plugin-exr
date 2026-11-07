//! Cryptomatte's hash: MurmurHash3_x86_32 with seed zero over a name's
//! UTF-8 bytes, and the float IDs made from it.

/// MurmurHash3_x86_32 of `bytes`, seed zero.
pub fn murmur3_32(bytes: &[u8]) -> u32 {
    const C1: u32 = 0xCC9E_2D51;
    const C2: u32 = 0x1B87_3593;
    let mix = |k: u32| k.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
    let mut h = 0u32;
    let (blocks, tail) = bytes.as_chunks::<4>();
    for block in blocks {
        h = (h ^ mix(u32::from_le_bytes(*block)))
            .rotate_left(13)
            .wrapping_mul(5)
            .wrapping_add(0xE654_6B64);
    }
    if !tail.is_empty() {
        let k = tail
            .iter()
            .enumerate()
            .fold(0u32, |k, (i, &b)| k | u32::from(b) << (8 * i));
        h ^= mix(k);
    }
    h ^= bytes.len() as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

/// A hash as an ID's float bits: exponents 0 and 255 (zero, subnormal,
/// infinity, NaN) have their low exponent bit flipped. Idempotent, so IDs
/// already converted stay as they are.
pub fn id_bits(hash: u32) -> u32 {
    let exponent = (hash >> 23) & 0xFF;
    if exponent == 0 || exponent == 255 {
        hash ^ 1 << 23
    } else {
        hash
    }
}

/// The float ID of a hash.
pub fn hash_to_id(hash: u32) -> f32 {
    f32::from_bits(id_bits(hash))
}

/// The float ID of `name` by the standard hash.
pub fn name_id(name: &str) -> f32 {
    hash_to_id(murmur3_32(name.as_bytes()))
}

/// The seven hex digits a layer's metadata keys use
/// (`cryptomatte/<key>/name`).
pub fn metadata_key(layer: &str) -> String {
    format!("{:08x}", murmur3_32(layer.as_bytes()))[..7].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specification_vectors() {
        assert_eq!(murmur3_32(b"torus"), 4_053_562_365);
        assert_eq!(murmur3_32("равнина".as_bytes()), 2_518_896_926);
        assert_eq!(murmur3_32(b""), 0);
        assert_eq!(id_bits(0), 8_388_608);
        assert_eq!(id_bits(2_139_095_040), 2_130_706_432);
        assert_eq!(id_bits(4_294_967_295), 4_286_578_687);
        assert_eq!(id_bits(id_bits(0)), id_bits(0));
        assert!(name_id("torus") < -1.5494e30);
        assert!(hash_to_id(0) > 0.0);
        assert_eq!(metadata_key("CryptoObject").len(), 7);
        // The Psyop fixtures' keys.
        assert_eq!(metadata_key("crypto_asset"), "28322e9");
        assert_eq!(metadata_key("crypto_object"), "f834d0a");
    }
}
