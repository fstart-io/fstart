//! Inter-stage handoff data.
//!
//! When a multi-stage firmware boots (e.g., bootblock → main), the
//! outgoing stage writes a [`StageHandoff`] to a reserved DRAM buffer and
//! passes its address to the next stage, which reads it back for the
//! runtime-discovered parameters (DRAM size, the image's location).
//!
//! Both stages come from one build, so the record is a fixed little-endian
//! layout; the magic and version reject a buffer from anything else.

use zerocopy::byteorder::{LE, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::ffs::locator::LocatorBlock;

/// Magic number for StageHandoff validation.
///
/// ASCII "FSTH" (fstart handoff). If the incoming stage reads a buffer
/// whose first 4 bytes don't match this, there is no valid handoff
/// (e.g., first stage loaded by BROM, or jump from non-fstart code).
pub const HANDOFF_MAGIC: u32 = 0x4653_5448;

/// Layout version. Increment when the record changes; the incoming stage
/// rejects any other.
pub const HANDOFF_VERSION: u32 = 3;

/// Size of the reserved handoff buffer: the record plus headroom for
/// future fields without moving the reservation.
pub const HANDOFF_MAX_SIZE: usize = 256;

/// Inter-stage handoff data.
///
/// Carries runtime-discovered parameters from one stage to the next.
/// Device init state is determined by the fixed platform flow and stage
/// build metadata, so it is NOT included here — only truly dynamic data
/// belongs in the handoff.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct StageHandoff {
    /// [`HANDOFF_MAGIC`].
    magic: U32<LE>,
    /// [`HANDOFF_VERSION`].
    version: U32<LE>,
    /// DRAM size in bytes, discovered by DRAM training; 0 when the previous
    /// stage did not determine it.
    pub dram_size: U64<LE>,
    /// Location-only image metadata from the predecessor. Consumers must check
    /// it against platform/device bounds before any read; it carries no trust
    /// policy and cannot expand writable RAM or media authority.
    pub locator: LocatorBlock,
}

const _: () = assert!(core::mem::size_of::<StageHandoff>() <= HANDOFF_MAX_SIZE);

impl StageHandoff {
    pub fn new(dram_size: u64, locator: LocatorBlock) -> Self {
        Self {
            magic: U32::new(HANDOFF_MAGIC),
            version: U32::new(HANDOFF_VERSION),
            dram_size: U64::new(dram_size),
            locator,
        }
    }

    /// Write the record to the start of `buf`; `None` if it does not fit.
    pub fn write_to(&self, buf: &mut [u8]) -> Option<usize> {
        self.write_to_prefix(buf).ok()?;
        Some(core::mem::size_of::<Self>())
    }

    /// Read a record from the start of `buf` (any alignment). Rejects a
    /// wrong magic or version and a malformed locator. A valid record is not
    /// proof of trusted provenance.
    pub fn read_from(buf: &[u8]) -> Option<Self> {
        let (handoff, _) = Self::read_from_prefix(buf).ok()?;
        (handoff.magic.get() == HANDOFF_MAGIC
            && handoff.version.get() == HANDOFF_VERSION
            && LocatorBlock::parse(handoff.locator.as_bytes()).is_some())
        .then_some(handoff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerocopy::IntoBytes;

    fn locator() -> LocatorBlock {
        LocatorBlock {
            total_image_size: U32::new(0x10000),
            manifest_offset: U32::new(0xfe00),
            manifest_size: U32::new(512),
            ..LocatorBlock::placeholder()
        }
    }

    #[test]
    fn round_trip_and_rejections() {
        let mut bytes = [0; HANDOFF_MAX_SIZE];
        let sent = StageHandoff::new(0x1000_0000, locator());
        let length = sent.write_to(&mut bytes).unwrap();
        assert_eq!(length, 56);
        let received = StageHandoff::read_from(&bytes).unwrap();
        assert_eq!(received.dram_size.get(), 0x1000_0000);
        assert_eq!(received.locator.as_bytes(), sent.locator.as_bytes());
        // Unaligned buffers read too.
        let mut shifted = [0; HANDOFF_MAX_SIZE + 1];
        shifted[1..].copy_from_slice(&bytes);
        assert!(StageHandoff::read_from(&shifted[1..]).is_some());
        assert!(StageHandoff::read_from(&bytes[..length - 1]).is_none());
        for corrupt in [0, 4, 16] {
            let mut bad = bytes;
            bad[corrupt] ^= 1;
            assert!(StageHandoff::read_from(&bad).is_none(), "byte {corrupt}");
        }
        assert!(sent.write_to(&mut [0; 55]).is_none());
    }
}
