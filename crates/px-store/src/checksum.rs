//! Checksums, hand-written so the crate stays std-only.
//!
//! Both are integrity checks against accident (torn writes, bit rot, a copy
//! that dropped bytes). Neither is a MAC: somebody who can write the profile
//! directory can write a valid checksum too, and that is outside this crate's
//! threat model (the OS permissions on the directory are the boundary).

/// CRC-32/ISO-HDLC (the zlib/PNG polynomial, reflected `0xEDB88320`).
///
/// Bitwise rather than table-driven: no table to index (the crate denies
/// indexing), and the inputs are small records. Check value for
/// `"123456789"` is `0xCBF43926`.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

/// Continue a CRC-32 over more data. `crc32_update(crc32(a), b) == crc32(a ++ b)`.
pub fn crc32_update(previous: u32, data: &[u8]) -> u32 {
    let mut c = !previous;
    for &byte in data {
        c ^= u32::from(byte);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// FNV-1a 64-bit offset basis.
pub const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a, 64 bit. Used beside CRC-32 by the migration so that a file is
/// compared by two independent functions plus its length.
pub fn fnv1a64(data: &[u8]) -> u64 {
    fnv1a64_update(FNV_OFFSET, data)
}

/// Continue an FNV-1a hash. Start from [`FNV_OFFSET`].
pub fn fnv1a64_update(mut hash: u64, data: &[u8]) -> u64 {
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn crc32_is_incremental() {
        let whole = crc32(b"hello, world");
        assert_eq!(crc32_update(crc32(b"hello, "), b"world"), whole);
    }

    #[test]
    fn fnv_known_vectors() {
        // Published FNV-1a 64 vectors.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }
}
