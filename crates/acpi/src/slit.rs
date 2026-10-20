//! SLIT (System Locality Distance Information Table), ACPI 6.5 §5.2.17.

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;

/// Distance of a locality to itself.
pub const LOCAL: u8 = 10;
/// An unreachable locality.
pub const UNREACHABLE: u8 = 0xff;

/// Build the SLIT from `n` localities and `distance(i, j)`. The diagonal is
/// always [`LOCAL`]; other values below it are invalid per the spec and are
/// raised to `LOCAL + 1`.
pub fn build_slit(n: usize, distance: impl Fn(usize, usize) -> u8) -> Vec<u8> {
    let matrix: Vec<u8> = (0..n * n)
        .map(|k| (k / n, k % n))
        .map(|(i, j)| match i == j {
            true => LOCAL,
            false => distance(i, j).max(LOCAL + 1),
        })
        .collect();
    let mut sdt = Sdt::new(
        *b"SLIT",
        44,
        1,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    sdt.write_u64(36, n as u64);
    sdt.append_slice(&matrix);
    crate::serialize(&sdt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix() {
        let slit = build_slit(2, |_, _| 5);
        assert_eq!(&slit[..4], b"SLIT");
        assert_eq!(slit.len(), 48);
        assert_eq!(&slit[36..44], &2u64.to_le_bytes());
        assert_eq!(&slit[44..], &[10, 11, 11, 10]);
        assert_eq!(slit.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
    }
}
