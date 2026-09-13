//! Intel ACPI OpRegion layout helpers.

use zerocopy::byteorder::little_endian::U32;
use zerocopy::{Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::error::GmaError;

/// Intel OpRegion signature.
pub const OPREGION_SIGNATURE: &[u8; 16] = b"IntelGraphicsMem";
/// OpRegion size used by legacy Intel graphics generations.
pub const OPREGION_SIZE: usize = 8 * 1024;
/// VBT mailbox offset inside OpRegion.
pub const OPREGION_VBT_OFFSET: usize = 0x400;
/// Maximum VBT payload bytes in mailbox 4.
pub const OPREGION_VBT_SIZE: usize = OPREGION_SIZE - OPREGION_VBT_OFFSET;

/// Fixed OpRegion header written at offset 0.
#[repr(C)]
#[derive(Debug, Clone, Copy, Immutable, IntoBytes, KnownLayout, Unaligned)]
struct OpRegionHeader {
    signature: [u8; 16],
    size_kb: U32,
    _reserved: [u8; 2],
    version_major: u8,
    version_minor: u8,
}

impl OpRegionHeader {
    const fn new() -> Self {
        Self {
            signature: *OPREGION_SIGNATURE,
            size_kb: U32::new(OPREGION_SIZE as u32 / 1024),
            _reserved: [0; 2],
            version_major: 2,
            version_minor: 0,
        }
    }
}

/// Initialize an OpRegion buffer and copy optional VBT bytes into mailbox 4.
pub fn build_opregion(buffer: &mut [u8], vbt: Option<&[u8]>) -> Result<(), GmaError> {
    if buffer.len() < OPREGION_SIZE {
        return Err(GmaError::HardwareError);
    }
    buffer[..OPREGION_SIZE].fill(0);
    let header = OpRegionHeader::new();
    let header_bytes = header.as_bytes();
    buffer[..header_bytes.len()].copy_from_slice(header_bytes);
    if let Some(vbt) = vbt {
        if vbt.len() > OPREGION_VBT_SIZE {
            return Err(GmaError::VbtInvalid);
        }
        let end = OPREGION_VBT_OFFSET + vbt.len();
        buffer[OPREGION_VBT_OFFSET..end].copy_from_slice(vbt);
    }
    Ok(())
}
