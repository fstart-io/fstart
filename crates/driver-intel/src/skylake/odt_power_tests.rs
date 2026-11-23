// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[test]
fn odt_power_checked_slices_preserve_reserved_bits_and_read_duration_neighbors() {
    // Source masks six bits, even RDWR SG. Its seventh bit is NOT cleared.
    let [rdrd, rdwr] = stretch(0xfefe_ffff, 0xfefe_fefe, 1).unwrap();
    assert_eq!(rdrd, 0xffff_ffff); // SG and DG remain untouched.
    assert_eq!(rdwr, 0xffff_ffff); // SG bit6 stays set.
    assert_eq!(read_duration(0xffff_ffff, 6), Ok(0xffff_fff8));
    assert_eq!(read_duration(0xffff_ffff, 7), Ok(0xffff_fff9));
    assert_eq!(read_duration(0xffff_ffff, 8), Ok(0xffff_fffa));
    assert_eq!(read_duration(0xffff_ffff, 13), Ok(0xffff_ffff));
    assert_eq!(read_duration(0, 5), Err(Error::InvalidTiming));
    assert_eq!(read_duration(0, 14), Err(Error::InvalidTiming));
    assert_eq!(stretch(0, 0, u32::MAX), Err(Error::InvalidTiming));
}
