//! Logical CPU and core IDs: the family/revision bit masks the MSR tables
//! and the revision-specific steps are guarded by (CCX_SP3 §3.2).
//!
//! AGESA maps CPUID to these with "unknown stepping = first row of its
//! family" (Naples → B2, Rome → B0, Milan → B2). Here the mapping starts
//! from a [`Revision`], which [`crate::soc::identify`] only yields for known
//! steppings, so an unknown CPU never gets a logical ID.
//!
//! Naples has no core logical ID (its 11-opcode engine has no core guard);
//! Rome and Milan add one per core microarchitecture (Zen 2, Zen 3).

use crate::soc::{Revision, Unsupported, identify};

/// One logical ID: a family bit and a revision bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Id {
    pub family: u32,
    pub revision: u32,
}

/// The CPU and core logical IDs of a revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalId {
    pub cpu: Id,
    /// `None` on Naples.
    pub core: Option<Id>,
}

/// Naples (ZP) CPU family and revision bits (AmdSocSp3ZpPei map).
pub mod zp {
    pub const CPU: u32 = 0x2;
    pub const A0: u32 = 0x1;
    pub const B0: u32 = 0x2;
    pub const B1: u32 = 0x4;
    pub const B2: u32 = 0x8;
}

/// Rome (SSP) CPU family and Zen 2 core family; core revisions use the same
/// bits as the CPU revisions (AmdSocSp3RmPei map).
pub mod ssp {
    pub const CPU: u32 = 0x10;
    pub const CORE: u32 = 0x1;
    pub const A0: u32 = 0x1;
    pub const B0: u32 = 0x10;
}

/// Milan (GN) CPU family and Zen 3 core family; core revisions use the same
/// bits as the CPU revisions (AmdSocSp3GnPei map).
pub mod gn {
    pub const CPU: u32 = 0x80;
    pub const CORE: u32 = 0x2;
    pub const A0: u32 = 0x1;
    pub const B0: u32 = 0x10;
    pub const B1: u32 = 0x20;
    pub const B2: u32 = 0x40;
}

impl LogicalId {
    /// The logical IDs of `revision`.
    pub const fn of(revision: Revision) -> Self {
        const fn ids(cpu: u32, core: u32, revision: u32) -> LogicalId {
            LogicalId {
                cpu: Id {
                    family: cpu,
                    revision,
                },
                core: Some(Id {
                    family: core,
                    revision,
                }),
            }
        }
        const fn zp(revision: u32) -> LogicalId {
            LogicalId {
                cpu: Id {
                    family: zp::CPU,
                    revision,
                },
                core: None,
            }
        }
        match revision {
            Revision::ZpA0 => zp(zp::A0),
            Revision::ZpB0 => zp(zp::B0),
            Revision::ZpB1 => zp(zp::B1),
            Revision::ZpB2 => zp(zp::B2),
            Revision::SspA0 => ids(ssp::CPU, ssp::CORE, ssp::A0),
            Revision::SspB0 => ids(ssp::CPU, ssp::CORE, ssp::B0),
            Revision::GnA0 => ids(gn::CPU, gn::CORE, gn::A0),
            Revision::GnB0 => ids(gn::CPU, gn::CORE, gn::B0),
            Revision::GnB1 => ids(gn::CPU, gn::CORE, gn::B1),
            Revision::GnB2 => ids(gn::CPU, gn::CORE, gn::B2),
        }
    }

    /// From CPUID Fn0000_0001_EAX and Fn8000_0001_EBX; fails closed on any
    /// CPU [`identify`] rejects.
    pub const fn from_cpuid(signature: u32, ext_ebx: u32) -> Result<Self, Unsupported> {
        match identify(signature, ext_ebx) {
            Ok(id) => Ok(Self::of(id.revision)),
            Err(e) => Err(e),
        }
    }

    /// The CPU ID is in `family` with a revision in `revisions`.
    pub const fn cpu_is(&self, family: u32, revisions: u32) -> bool {
        self.cpu.family & family != 0 && self.cpu.revision & revisions != 0
    }

    /// The core ID is in `family` with a revision in `revisions` (never on
    /// Naples).
    pub const fn core_is(&self, family: u32, revisions: u32) -> bool {
        match self.core {
            Some(c) => c.family & family != 0 && c.revision & revisions != 0,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soc::PACKAGE_SP3;

    #[test]
    fn rows_match_the_maps() {
        let sp3 = (PACKAGE_SP3 as u32) << 28;
        let rome_b0 = LogicalId::from_cpuid(0x0083_0F10, sp3).unwrap();
        assert_eq!(
            rome_b0.cpu,
            Id {
                family: 0x10,
                revision: 0x10
            }
        );
        assert_eq!(
            rome_b0.core,
            Some(Id {
                family: 0x1,
                revision: 0x10
            })
        );
        let milan_b1 = LogicalId::from_cpuid(0x00A0_0F11, sp3).unwrap();
        assert!(milan_b1.cpu_is(gn::CPU, gn::B0 | gn::B1));
        assert!(milan_b1.core_is(gn::CORE, gn::B1));
        assert!(!milan_b1.core_is(ssp::CORE, gn::B1), "Zen 2 core family");
        let naples = LogicalId::of(Revision::ZpB2);
        assert!(naples.cpu_is(zp::CPU, zp::B2) && !naples.core_is(u32::MAX, u32::MAX));
    }

    #[test]
    fn unknown_steppings_have_no_id() {
        let sp3 = (PACKAGE_SP3 as u32) << 28;
        // AGESA would run these as B2 / B0 / B2.
        for sig in [0x0080_0F13, 0x0083_0F11, 0x00A0_0F13] {
            assert!(LogicalId::from_cpuid(sig, sp3).is_err(), "{sig:#x}");
        }
    }
}
