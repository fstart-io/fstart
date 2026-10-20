//! Milan SMN fuse filter (NBIO_SMU §3.1, `gn.smn_fuse_filter`,
//! `gn.ioms_fuse_list`).
//!
//! AmdNbioBaseGnPei reads two fuse words and marks every 1 MiB SMN aperture
//! of a harvested unit (NBIF, IOMMU L1, PCIe core, SDPMUX, …) as absent;
//! each later SMN access in that aperture is skipped. Bits 11..14 of the
//! first word also drop fused-off IOMS from the SP3 component list.
//! Naples and Rome have no such filter ([`Fuses::NONE`]).

use fstart_pci::ConfigRegionAccess;

use crate::smn::{Smn, SmnAddr};

/// First fuse word (SMN).
pub const FUSE0: SmnAddr = 0x0005_D148;
/// Second fuse word (SMN).
pub const FUSE1: SmnAddr = 0x0005_D14C;

/// The two Milan fuse words; zero means "nothing fused off".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fuses {
    pub word: [u32; 2],
}

impl Fuses {
    /// No filtering (Naples, Rome, or a Milan part with nothing fused).
    pub const NONE: Self = Self { word: [0; 2] };

    /// Read the fuse words.
    ///
    /// AGESA reads them once through bus 0 (socket 0) and applies them to
    /// every NBIO; read them per socket through that socket's SMN.
    ///
    /// # Safety
    /// As [`Smn::read32`].
    pub unsafe fn read<P: ConfigRegionAccess>(smn: &Smn<P>) -> Self {
        // SAFETY: forwarded contract.
        unsafe {
            Self {
                word: [smn.read32(FUSE0), smn.read32(FUSE1)],
            }
        }
    }

    /// Whether IOMS / NBIO quadrant `quadrant` exists (word 0 bit 11 + n
    /// clear).
    pub const fn quadrant_present(&self, quadrant: u8) -> bool {
        quadrant < 4 && self.word[0] & (0x800 << quadrant) == 0
    }

    /// Whether an SMN access to `addr` is made.
    pub fn allows(&self, addr: SmnAddr) -> bool {
        if self.word == [0; 2] {
            return true;
        }
        #[cfg(feature = "milan")]
        {
            let aperture = addr & 0xFFF0_0000;
            !super::sp3::tables::milan::FUSE_APERTURES
                .iter()
                .any(|&(w, mask, a)| a == aperture && self.word[w as usize] & mask != 0)
        }
        #[cfg(not(feature = "milan"))]
        {
            let _ = addr;
            true
        }
    }
}

#[cfg(all(test, feature = "milan"))]
mod tests {
    use super::*;

    #[test]
    fn filter() {
        // NBIF2 of quadrant 1 (0x10A) and IOMS 3 fused off.
        let f = Fuses {
            word: [0x1000_0000 | 0x4000, 0],
        };
        assert!(!f.allows(0x10a3_101c));
        assert!(f.allows(0x1093_101c));
        assert!(f.quadrant_present(2) && !f.quadrant_present(3));
        assert!(Fuses::NONE.allows(0x10a3_101c));
        // Word 1: IOMMU L1 0x147 (PCIE0 of quadrant 0).
        assert!(!Fuses { word: [0, 0x80] }.allows(0x1470_00c8));
    }
}
