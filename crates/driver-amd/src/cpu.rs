// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Family 17h/19h CPUs the server and embedded flows run on, by their
//! CPUID: the silicon revision (ZP: Zeppelin and Naples, SSP: Rome, GN:
//! Milan) and the package. An unknown family, model or stepping is an
//! error: each generation's code only runs on silicon it was recovered
//! from (AGESA treats an unknown stepping as the newest known one).

/// CPUID Fn8000_0001_EBX[31:28].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Package {
    /// EPYC Embedded 3000, two dies.
    Sp4,
    Am4,
    /// EPYC Embedded 3000, one die.
    Sp4r2,
    /// EPYC 7001/7002/7003.
    Sp3,
    Unknown(u8),
}
impl Package {
    pub const fn from_cpuid(extended_ebx: u32) -> Self {
        match extended_ebx >> 28 {
            1 => Self::Sp4,
            2 => Self::Am4,
            3 => Self::Sp4r2,
            4 => Self::Sp3,
            other => Self::Unknown(other as u8),
        }
    }
    /// ZP dies an embedded package has.
    pub const fn dies(self) -> Option<u8> {
        match self {
            Self::Sp4 => Some(2),
            Self::Sp4r2 => Some(1),
            _ => None,
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sp4 => "SP4",
            Self::Am4 => "AM4",
            Self::Sp4r2 => "SP4r2",
            Self::Sp3 => "SP3",
            Self::Unknown(_) => "unknown",
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
    /// The SP3 generation of this revision.
    #[cfg(feature = "sp3")]
    pub const fn generation(self) -> crate::sp3::soc::Generation {
        use crate::sp3::soc::Generation;
        match self {
            Self::ZpA0 | Self::ZpB0 | Self::ZpB1 | Self::ZpB2 => Generation::Naples,
            Self::SspA0 | Self::SspB0 => Generation::Rome,
            Self::GnA0 | Self::GnB0 | Self::GnB1 | Self::GnB2 => Generation::Milan,
        }
    }
    /// A ZP die: Zeppelin or Naples.
    pub const fn zp(self) -> bool {
        matches!(self, Self::ZpA0 | Self::ZpB0 | Self::ZpB1 | Self::ZpB2)
    }
    /// The revision bit of the ZP vendor register-table predicates.
    pub const fn zp_mask(self) -> u8 {
        match self {
            Self::ZpA0 => 1 << 0,
            Self::ZpB0 => 1 << 1,
            Self::ZpB1 => 1 << 2,
            Self::ZpB2 => 1 << 3,
            _ => 0,
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
    /// Family/model of none of these generations.
    Model { family: u32, model: u32 },
    /// Known model, but a stepping no donor was recovered for.
    Stepping {
        family: u32,
        model: u32,
        stepping: u32,
    },
    /// Not the package the flow is for.
    Package(u8),
}

impl Revision {
    /// From CPUID Fn0000_0001_EAX.
    pub const fn from_signature(signature: u32) -> Result<Self, Unsupported> {
        let base_family = (signature >> 8) & 0xf;
        let family = if base_family == 0xf {
            base_family + ((signature >> 20) & 0xff)
        } else {
            base_family
        };
        let model = ((signature >> 12) & 0xf0) | ((signature >> 4) & 0xf);
        let stepping = signature & 0xf;
        Ok(match (family, model, stepping) {
            (0x17, 0x00, 0) => Self::ZpA0,
            (0x17, 0x01, 0) => Self::ZpB0,
            (0x17, 0x01, 1) => Self::ZpB1,
            (0x17, 0x01, 2) => Self::ZpB2,
            (0x17, 0x30, 0) => Self::SspA0,
            (0x17, 0x31, 0) => Self::SspB0,
            (0x19, 0x00, 0) => Self::GnA0,
            (0x19, 0x01, 0) => Self::GnB0,
            (0x19, 0x01, 1) => Self::GnB1,
            (0x19, 0x01, 2) => Self::GnB2,
            (0x17, 0x00..=0x0f | 0x30..=0x3f, _) | (0x19, 0x00..=0x0f, _) => {
                return Err(Unsupported::Stepping {
                    family,
                    model,
                    stepping,
                });
            }
            _ => return Err(Unsupported::Model { family, model }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifies_epyc_3151() {
        // EPYC 3151: family 17h model 1 stepping 2, SP4r2.
        assert_eq!(Revision::from_signature(0x00800f12), Ok(Revision::ZpB2));
        assert!(Revision::from_signature(0x00810f10).is_err());
        assert_eq!(Package::from_cpuid(0x3000_0000).dies(), Some(1));
        assert_eq!(Revision::ZpB1.zp_mask(), 4);
    }
}
