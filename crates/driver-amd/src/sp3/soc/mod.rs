//! CPU identification and the per-generation SoC constants.
//!
//! AGESA's SoC PEIMs accept one family/model each and treat an unknown
//! stepping as the newest known one (SOC_DF §2.3, CCX §3). Here an unknown
//! stepping, model, family or package is an error: each generation's code is
//! only run on silicon it was recovered from.

use fstart_amdfw_format::efs::BiosDirectorySlot;
use fstart_amdfw_format::{Flash, HALF_SIZE, Profile};

/// CPUID Fn8000_0001_EBX\[31:28\] package type of SP3 (all three generations).
pub const PACKAGE_SP3: u8 = 4;

/// The SP3 silicon generations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Generation {
    /// EPYC 7001, ZP: Family 17h models 00h–0Fh, four dies per socket.
    Naples,
    /// EPYC 7002, SSP: Family 17h models 30h–3Fh, IOD + CCDs.
    Rome,
    /// EPYC 7003, GN: Family 19h models 00h–0Fh, IOD + CCDs.
    Milan,
}

impl From<Generation> for crate::psp::Variant {
    fn from(generation: Generation) -> Self {
        match generation {
            Generation::Naples => Self::Zp,
            Generation::Rome => Self::Ssp,
            Generation::Milan => Self::Gn,
        }
    }
}

impl Generation {
    /// The flash format of this generation's boot ROM and BIOS directory
    /// (EFS generation word, PSP ID, BIOS directory pointer).
    pub const fn profile(self) -> Profile {
        Profile::for_slot(match self {
            Self::Naples => BiosDirectorySlot::Family17hModel00,
            Self::Rome => BiosDirectorySlot::Family17hModel30,
            Self::Milan => BiosDirectorySlot::Family19h,
        })
    }

    /// The flash offsets of the 16 MiB halves the boot ROM searches for its
    /// EFS, in order: Naples only the lower one (FIRMWARE_IMAGE §1.2, AMD
    /// #55758 §4.1.7).
    pub const fn boot_halves(self) -> &'static [usize] {
        match self {
            Self::Naples => &[0],
            Self::Rome | Self::Milan => &[0, HALF_SIZE],
        }
    }

    /// `bytes` from the start of a 16 MiB half, as the boot ROM maps the
    /// half below 4 GiB.
    pub const fn flash(self, bytes: &[u8]) -> Flash<'_> {
        Flash::region(bytes, 0, fstart_amdfw_format::ROM2_BASE, self.profile())
    }

    /// Maximum sockets on SP3.
    pub const MAX_SOCKETS: usize = 2;

    /// Dies with their own DF node per socket: four ZP dies on Naples, one
    /// IOD on Rome and Milan.
    pub const fn dies_per_socket(self) -> usize {
        match self {
            Self::Naples => 4,
            Self::Rome | Self::Milan => 1,
        }
    }

    /// Root bridges (PCI domains) per socket: one per die on Naples, one per
    /// IOMS/NBIO quadrant on Rome and Milan (before Milan's fuse filter).
    pub const fn root_bridges_per_socket(self) -> usize {
        4
    }

    /// The PCIe ECAM window the vendor firmware uses: `(base, buses)`
    /// (SOC_DF §3.2).
    pub const fn ecam(self) -> (u64, u16) {
        match self {
            Self::Naples => (0xF000_0000, 128),
            Self::Rome | Self::Milan => (0xE000_0000, 256),
        }
    }

    /// Human readable name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Naples => "Naples",
            Self::Rome => "Rome",
            Self::Milan => "Milan",
        }
    }
}

use crate::cpu::{Revision, Unsupported};

/// The identified CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub revision: Revision,
    /// Raw CPUID Fn0000_0001_EAX.
    pub signature: u32,
}

impl Identity {
    pub const fn generation(&self) -> Generation {
        self.revision.generation()
    }
}

/// Identify the CPU from CPUID Fn0000_0001_EAX and Fn8000_0001_EBX.
pub const fn identify(signature: u32, ext_ebx: u32) -> Result<Identity, Unsupported> {
    let revision = match Revision::from_signature(signature) {
        Ok(revision) => revision,
        Err(error) => return Err(error),
    };
    let package = (ext_ebx >> 28) as u8;
    if package != PACKAGE_SP3 {
        return Err(Unsupported::Package(package));
    }
    Ok(Identity {
        revision,
        signature,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SP3: u32 = (PACKAGE_SP3 as u32) << 28;

    #[test]
    fn known_signatures() {
        let cases = [
            (0x0080_0F12, Revision::ZpB2),
            (0x0080_0F11, Revision::ZpB1),
            (0x0083_0F10, Revision::SspB0),
            (0x00A0_0F11, Revision::GnB1),
            (0x00A0_0F12, Revision::GnB2),
        ];
        for (sig, rev) in cases {
            assert_eq!(identify(sig, SP3).unwrap().revision, rev, "{sig:#x}");
        }
        assert_eq!(
            identify(0x00A0_0F12, SP3).unwrap().generation(),
            Generation::Milan
        );
    }

    #[test]
    fn fails_closed() {
        // Unknown Milan stepping, Vermeer (19h/21h), Matisse, Genoa.
        assert!(matches!(
            identify(0x00A0_0F13, SP3),
            Err(Unsupported::Stepping { .. })
        ));
        assert!(matches!(
            identify(0x00A2_0F10, SP3),
            Err(Unsupported::Model { .. })
        ));
        assert!(matches!(
            identify(0x0087_0F10, SP3),
            Err(Unsupported::Model { .. })
        ));
        assert!(matches!(
            identify(0x00A1_0F11, SP3),
            Err(Unsupported::Model { .. })
        ));
        // An SP3 model in another package (SP4/AM4) is rejected.
        assert_eq!(identify(0x0080_0F12, 1 << 28), Err(Unsupported::Package(1)));
    }
}
