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
        Flash::region(bytes, 0, crate::fch::rom3::ROM2_BASE, self.profile())
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

/// Silicon revision (the AGESA logical revision rows, SOC_DF §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Revision {
    /// ZP-A0 (0x00800F00).
    ZpA0,
    /// ZP-B0 (0x00800F10).
    ZpB0,
    /// ZP-B1 (0x00800F11).
    ZpB1,
    /// ZP-B2 (0x00800F12).
    ZpB2,
    /// SSP-A0 (0x00830F00).
    SspA0,
    /// SSP-B0 (0x00830F10).
    SspB0,
    /// GN-A0 (0x00A00F00).
    GnA0,
    /// GN-B0 (0x00A00F10).
    GnB0,
    /// GN-B1 (0x00A00F11).
    GnB1,
    /// GN-B2 (0x00A00F12).
    GnB2,
}

impl Revision {
    /// The generation of this revision.
    pub const fn generation(self) -> Generation {
        match self {
            Self::ZpA0 | Self::ZpB0 | Self::ZpB1 | Self::ZpB2 => Generation::Naples,
            Self::SspA0 | Self::SspB0 => Generation::Rome,
            Self::GnA0 | Self::GnB0 | Self::GnB1 | Self::GnB2 => Generation::Milan,
        }
    }

    /// Name as in the revision guides.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ZpA0 => "ZP-A0",
            Self::ZpB0 => "ZP-B0",
            Self::ZpB1 => "ZP-B1",
            Self::ZpB2 => "ZP-B2",
            Self::SspA0 => "SSP-A0",
            Self::SspB0 => "SSP-B0",
            Self::GnA0 => "GN-A0",
            Self::GnB0 => "GN-B0",
            Self::GnB1 => "GN-B1",
            Self::GnB2 => "GN-B2",
        }
    }
}

/// Why a CPU is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// Family/model is not an SP3 generation.
    Model { family: u32, model: u32 },
    /// Known model, but a stepping no donor was recovered for.
    Stepping {
        family: u32,
        model: u32,
        stepping: u32,
    },
    /// Not the SP3 package.
    Package(u8),
}

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
    let base_family = (signature >> 8) & 0xf;
    let family = if base_family == 0xf {
        base_family + ((signature >> 20) & 0xff)
    } else {
        base_family
    };
    let model = ((signature >> 12) & 0xf0) | ((signature >> 4) & 0xf);
    let stepping = signature & 0xf;
    let revision = match (family, model, stepping) {
        (0x17, 0x00, 0) => Revision::ZpA0,
        (0x17, 0x01, 0) => Revision::ZpB0,
        (0x17, 0x01, 1) => Revision::ZpB1,
        (0x17, 0x01, 2) => Revision::ZpB2,
        (0x17, 0x30, 0) => Revision::SspA0,
        (0x17, 0x31, 0) => Revision::SspB0,
        (0x19, 0x00, 0) => Revision::GnA0,
        (0x19, 0x01, 0) => Revision::GnB0,
        (0x19, 0x01, 1) => Revision::GnB1,
        (0x19, 0x01, 2) => Revision::GnB2,
        (0x17, 0x00..=0x0f | 0x30..=0x3f, _) | (0x19, 0x00..=0x0f, _) => {
            return Err(Unsupported::Stepping {
                family,
                model,
                stepping,
            });
        }
        _ => return Err(Unsupported::Model { family, model }),
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
