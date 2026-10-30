// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD's VFCT video BIOS table, used by Linux amdgpu/radeon.
//!
//! Wire ABI: AMD's published `UEFI_ACPI_VFCT`, `VFCT_IMAGE_HEADER` and
//! `GOP_VBIOS_CONTENT` definitions in `drivers/gpu/drm/amd/include/atombios.h`
//! (also in `atomfirmware.h`). This is a vendor table, not the standard ACPI
//! Appendix B `_ROM` interface. The builder performs no PCI or memory access.

use crate::AmlError;
use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    little_endian::{U16, U32},
};

/// One posted VBIOS and the actual PCI function it belongs to.
///
/// Use hardware identity, not a compatible ROM's PCIR alias. VFCT has no PCI
/// segment field: nonzero segments are rejected rather than misidentified.
pub struct VbiosImage<'a> {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub subsystem_vendor_id: u16,
    pub subsystem_id: u16,
    /// Producer-supplied image revision, or zero when unspecified. This is
    /// not inferred from the PCI configuration-space revision ID.
    pub revision: u32,
    pub rom: &'a [u8],
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct ImageHeader {
    bus: U32,
    device: U32,
    function: U32,
    vendor: U16,
    device_id: U16,
    subsystem_vendor: U16,
    subsystem: U16,
    revision: U32,
    length: U32,
}
const _: () = assert!(size_of::<ImageHeader>() == 28);
const HEADER_SIZE: u32 = 76;

/// Build a revision-1 VFCT with one or more images, in input order.
///
/// All data is copied into the returned ACPI table; no source pointer survives.
/// UUID/reserved/LIB1 policy is not guessed: the caller supplies its table UUID
/// (coreboot uses zero), reserved fields are zero and no LIB1 images are emitted.
/// Lengths, SBDFs and duplicate functions are checked before serialization.
pub fn build_vfct(uuid: [u8; 16], images: &[VbiosImage<'_>]) -> Result<Vec<u8>, AmlError> {
    if images.is_empty() {
        return Err(AmlError::InvalidRange);
    }
    let mut length = HEADER_SIZE;
    for (index, image) in images.iter().enumerate() {
        if image.segment != 0
            || image.device > 31
            || image.function > 7
            || image.rom.is_empty()
            || images.iter().take(index).any(|prior| {
                (prior.bus, prior.device, prior.function)
                    == (image.bus, image.device, image.function)
            })
        {
            return Err(AmlError::InvalidRange);
        }
        let bytes = u32::try_from(image.rom.len()).map_err(|_| AmlError::LengthOverflow)?;
        length = length
            .checked_add(size_of::<ImageHeader>() as u32)
            .and_then(|length| length.checked_add(bytes))
            .ok_or(AmlError::LengthOverflow)?;
    }
    let mut table = Sdt::new(
        *b"VFCT",
        HEADER_SIZE,
        1,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    table.write_bytes(36, &uuid);
    table.write_u32(52, HEADER_SIZE); // Table-relative first VBIOS image offset.
    for image in images {
        table.append(ImageHeader {
            bus: U32::new(u32::from(image.bus)),
            device: U32::new(u32::from(image.device)),
            function: U32::new(u32::from(image.function)),
            vendor: U16::new(image.vendor_id),
            device_id: U16::new(image.device_id),
            subsystem_vendor: U16::new(image.subsystem_vendor_id),
            subsystem: U16::new(image.subsystem_id),
            revision: U32::new(image.revision),
            length: U32::new(image.rom.len() as u32),
        });
        table.append_slice(image.rom);
    }
    debug_assert_eq!(table.len(), length as usize);
    Ok(crate::serialize(&table))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image(rom: &[u8]) -> VbiosImage<'_> {
        VbiosImage {
            segment: 0,
            bus: 4,
            device: 0,
            function: 1,
            vendor_id: 0x1002,
            device_id: 0x15d8,
            subsystem_vendor_id: 0x1ae0,
            subsystem_id: 0x15dd,
            revision: 0xc4,
            rom,
        }
    }

    #[test]
    fn layout_checksum_identity_and_multiple_images_match_the_amd_abi() {
        let rom = [0x55, 0xaa, 1, 0xcb];
        let mut second = image(&[0x11, 0x22, 0x33]);
        second.bus = 9;
        let table = build_vfct([0; 16], &[image(&rom), second]).unwrap();
        assert_eq!(&table[..4], b"VFCT");
        assert_eq!(table.len(), 76 + 28 + 4 + 28 + 3);
        assert_eq!(&table[4..8], &(table.len() as u32).to_le_bytes());
        assert_eq!(table[8], 1);
        assert_eq!(
            table.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
            0
        );
        assert_eq!(&table[36..52], &[0; 16]);
        assert_eq!(&table[52..56], &76u32.to_le_bytes());
        assert_eq!(&table[56..76], &[0; 20]);
        // Independent offsets from AMD's public header; in particular the
        // per-image header is 28 bytes, not a native-aligned Rust struct.
        assert_eq!(&table[76..88], &[4, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(
            &table[88..96],
            &[2, 0x10, 0xd8, 0x15, 0xe0, 0x1a, 0xdd, 0x15]
        );
        assert_eq!(&table[96..104], &[0xc4, 0, 0, 0, 4, 0, 0, 0]);
        assert_eq!(&table[104..108], &rom);
        assert_eq!(&table[108..112], &9u32.to_le_bytes());
        assert_eq!(&table[136..], &[0x11, 0x22, 0x33]);
    }

    #[test]
    fn invalid_or_ambiguous_images_are_not_published() {
        assert_eq!(build_vfct([0; 16], &[]), Err(AmlError::InvalidRange));
        for case in 0..4 {
            let mut entry = image(&[0x55, 0xaa]);
            match case {
                0 => entry.segment = 1,
                1 => entry.device = 32,
                2 => entry.function = 8,
                _ => entry.rom = &[],
            }
            assert_eq!(build_vfct([0; 16], &[entry]), Err(AmlError::InvalidRange));
        }
        assert_eq!(
            build_vfct([0; 16], &[image(&[1]), image(&[2])]),
            Err(AmlError::InvalidRange)
        );
    }
}
