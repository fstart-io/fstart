//! The byte-sum checksum used by ACPI/SMBIOS tables, EDID, the Chrome
//! EC protocol and DDR2 SPD: the wrapping sum of all bytes. A valid
//! structure sums to zero; the checksum byte to store is
//! `0u8.wrapping_sub(sum8(bytes))`.

/// Wrapping sum of all bytes.
#[inline]
pub fn sum8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

#[cfg(test)]
mod tests {
    use super::sum8;

    #[test]
    fn byte_sum() {
        assert_eq!(sum8(&[]), 0);
        assert_eq!(sum8(&[255, 2]), 1);
        assert_eq!(sum8(&[1, 2, 3]), 6);
    }
}
