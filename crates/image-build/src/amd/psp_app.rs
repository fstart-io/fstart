// SPDX-License-Identifier: MIT OR Apache-2.0
//! fstart's PSP user application: the smallest one, which only exits.
//!
//! Some PSP boot loaders release the x86 cores only after the user
//! application of PSP directory entry 0x52 (the verstage slot) ran; the
//! Chromebook parts of Picasso do. This one gives the boot back to the PSP
//! at once (`SVC_EXIT`). It is unsigned: such a PSP does not check it.
use zerocopy::little_endian::U32;
use zerocopy::{Immutable, IntoBytes, KnownLayout};

/// The PSP directory entry the boot loader starts the application from.
pub const PSP_ENTRY: u8 = 0x52;

/// The 256-byte header of a PSP firmware image; the code follows it.
#[derive(IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
struct Header {
    nonce: [u8; 16],
    header_version: U32,
    /// Bytes after the header.
    image_size: U32,
    reserved_18: [u8; 0x48],
    image_version: U32,
    family_id: U32,
    /// The code's offset in the image.
    load_address: U32,
    reserved_6c: [u8; 0x10],
    fw_type: u8,
    reserved_7d: [u8; 0x83],
}
const _: () = assert!(size_of::<Header>() == 0x100);

/// ARM: `mov r0, #0`, `svc #0` (SVC_EXIT), `b .`.
const CODE: [u32; 4] = [0xe3a0_0000, 0xef00_0000, 0xeaff_fffe, 0];

/// The application image.
pub fn image() -> Vec<u8> {
    let code: Vec<u8> = CODE.iter().flat_map(|word| word.to_le_bytes()).collect();
    let header = Header {
        nonce: [0; 16],
        header_version: U32::new(0),
        image_size: U32::new(code.len() as u32),
        reserved_18: [0; 0x48],
        image_version: U32::new(1),
        family_id: U32::new(0),
        load_address: U32::new(size_of::<Header>() as u32),
        reserved_6c: [0; 0x10],
        fw_type: PSP_ENTRY,
        reserved_7d: [0; 0x83],
    };
    [header.as_bytes(), &code].concat()
}

#[cfg(test)]
mod tests {
    #[test]
    fn header_fields_and_code() {
        let image = super::image();
        assert_eq!(image.len(), 0x110);
        assert_eq!(image[0x14..0x18], 0x10u32.to_le_bytes());
        assert_eq!(image[0x68..0x6c], 0x100u32.to_le_bytes());
        assert_eq!(image[0x7c], 0x52);
        assert_eq!(image[0x100..0x108], [0, 0, 0xa0, 0xe3, 0, 0, 0, 0xef]);
    }
}
