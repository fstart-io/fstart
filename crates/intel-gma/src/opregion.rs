//! Intel ACPI OpRegion layout helpers.

use crate::error::GmaError;

/// Intel OpRegion signature.
pub const OPREGION_SIGNATURE: &[u8; 16] = b"IntelGraphicsMem";
/// OpRegion size used by legacy Intel graphics generations.
pub const OPREGION_SIZE: usize = 8 * 1024;
/// VBT mailbox offset inside OpRegion.
pub const OPREGION_VBT_OFFSET: usize = 0x400;
/// Maximum VBT payload bytes in mailbox 4.
pub const OPREGION_VBT_SIZE: usize = OPREGION_SIZE - OPREGION_VBT_OFFSET;

/// Initialize an OpRegion buffer and copy optional VBT bytes into mailbox 4.
pub fn build_opregion(buffer: &mut [u8], vbt: Option<&[u8]>) -> Result<(), GmaError> {
    if buffer.len() < OPREGION_SIZE {
        return Err(GmaError::HardwareError);
    }
    buffer[..OPREGION_SIZE].fill(0);
    buffer[..OPREGION_SIGNATURE.len()].copy_from_slice(OPREGION_SIGNATURE);
    // Header size in KB at offset 0x10, version 2.0 at 0x16/0x17.
    buffer[0x10..0x14].copy_from_slice(&(OPREGION_SIZE as u32 / 1024).to_le_bytes());
    buffer[0x16] = 2;
    buffer[0x17] = 0;
    if let Some(vbt) = vbt {
        if vbt.len() > OPREGION_VBT_SIZE {
            return Err(GmaError::VbtInvalid);
        }
        let end = OPREGION_VBT_OFFSET + vbt.len();
        buffer[OPREGION_VBT_OFFSET..end].copy_from_slice(vbt);
    }
    Ok(())
}
