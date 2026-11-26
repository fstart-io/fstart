// SPDX-License-Identifier: MIT OR Apache-2.0
//! The WH die and package by CPUID: one decoder ([`Part::from_cpuid`]) that
//! every WH block's choice derives from: the PCIe complex layout of MPIO and
//! NBIO ([`Part::layout`]), the DF device table ([`Part::devices`]), the
//! NBIO table gates ([`Part::a0`], [`Part::sp8`], [`Part::base_b0_sp8`]) and
//! the core revision of the CCX tables ([`Part::revision`]).
//!
//! The vendor tests CPUID afresh in each block, with slightly different
//! rules: the DF device table (`0xf2be40`) by family/model/stepping and, for
//! model ≥ 8, the package; the PCIe complex (`0xf37dc0`) by exact signature
//! and package, every other part taking the SP7 B0 template; the NBIO tables
//! (`CpuidSignatureMatch` `0xf1c9f0`) by exact signature and package. Those
//! rules disagree only on parts no donor runs on (an A0 stepping outside
//! package 2, a `0x00B9`/`0x00BA` stepping other than 0 or `C0`), where they
//! would give one die two different root-bridge counts. Here the die is
//! decided once, by the DF's rule (what the IOD has: [`Die`]), and the PCIe
//! layout follows it; the NBIO tables keep their exact vendor gates.

use crate::ccx::wh::Revision;
use crate::df::wh::devices::{self, Devices};
use crate::dxio::wh::{Layout, Wrapper};

/// What a WH IOD is, as its DF device table says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Die {
    /// SP7 A0 (`0x00B5_0F00`): two root bridges per IOD.
    A0,
    /// SP7 B0 and later (`0x00B5_0Fxx`, `0x00BC_0Fxx`): three.
    B0,
    /// SP7 model ≥ 8 in package type `0xD`: three, other PIE runs.
    M8,
    /// `0x00B9_0Fxx`/`0x00BA_0Fxx` (SP8): four.
    Sp8,
}

/// A WH part: CPUID 8000_0001 EAX (without its reserved bits) and the
/// package type of EBX\[31:28\], and the die they make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    /// CPUID 8000_0001 EAX `& 0x0FFF_0FFF`.
    pub signature: u32,
    /// CPUID 8000_0001 EBX\[31:28\].
    pub package: u8,
    pub die: Die,
}

impl Part {
    /// Package type of SP7.
    pub const SP7: u8 = 2;
    /// Package type of SP8 (`0x00B9_0F00`/`0x00BA_0F00`).
    pub const SP8: u8 = 4;

    /// Decode CPUID 8000_0001 EAX and EBX.
    pub const fn from_cpuid(eax: u32, ebx: u32) -> Self {
        let signature = eax & 0x0fff_0fff;
        let package = (ebx >> 28) as u8;
        let family_model = (signature >> 16) & 0xfff;
        let model = (signature >> 4) & 0xf;
        let stepping = signature & 0xf;
        let die = match family_model {
            0x0b9 | 0x0ba => Die::Sp8,
            0x0b5 | 0x0bc if model >= 8 && package == 0xd => Die::M8,
            0x0b5 if model == 0 && stepping == 0 => Die::A0,
            _ => Die::B0,
        };
        Self {
            signature,
            package,
            die,
        }
    }

    /// The PCIe complex of each IOD (MPIO wrappers, IOHC numbering).
    pub const fn layout(self) -> Layout {
        match self.die {
            Die::A0 => Layout::Sp7A0,
            Die::B0 | Die::M8 => Layout::Sp7B0,
            Die::Sp8 => Layout::Sp8,
        }
    }

    /// The DF device table of each IOD.
    pub const fn devices(self) -> &'static Devices {
        match self.die {
            Die::A0 => &devices::A0,
            Die::B0 => &devices::B0,
            Die::M8 => &devices::M8,
            Die::Sp8 => &devices::B9,
        }
    }

    /// Root bridges (IOS) of each IOD.
    pub const fn root_bridges_per_iod(self) -> u8 {
        self.devices().ios.count
    }

    /// SP7 A0: the A0 tables, routes and strap indices.
    pub const fn a0(self) -> bool {
        matches!(self.die, Die::A0)
    }

    /// The SP8 parts the NBIF and base tables name: `0x00B9_0F00` or
    /// `0x00BA_0F00`, package 4 (`NbioNbifInitSilWh`, `0xf69730`).
    pub const fn sp8(self) -> bool {
        matches!(self.signature, 0x00b9_0f00 | 0x00ba_0f00) && self.package == Self::SP8
    }

    /// `BASE_B0_SP8` applies: `0x00B5_0F10` package 2 or [`Self::sp8`].
    pub const fn base_b0_sp8(self) -> bool {
        (self.signature == 0x00b5_0f10 && self.package == Self::SP7) || self.sp8()
    }

    /// The core revision the CCX tables key on; `None` for the families
    /// they do not cover (SP8).
    pub const fn revision(self) -> Option<Revision> {
        Revision::from_signature(self.signature)
    }

    /// Whether IOHC `iohc` carries a PCIe wrapper: every IOHC of the
    /// layout, which on SP7 B0 are 0, 1 and 3 ([`Layout::iohc`]).
    pub const fn has_pcie(self, iohc: u8) -> bool {
        self.layout().has_pcie(iohc)
    }

    /// The PCIe wrappers of IOHC `iohc`.
    pub fn wrappers(self, iohc: u8) -> impl Iterator<Item = &'static Wrapper> {
        self.layout().wrappers_of(iohc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(signature: u32, package: u8) -> Part {
        Part::from_cpuid(signature, u32::from(package) << 28)
    }

    #[test]
    fn one_decoder_for_every_block() {
        // The donors' parts.
        let a0 = part(0x00b5_0f00, 2);
        assert_eq!((a0.die, a0.layout()), (Die::A0, Layout::Sp7A0));
        assert_eq!(a0.devices().table, 0xfc49c8);
        let b0 = part(0x00b5_0f01, 2);
        assert_eq!((b0.die, b0.layout()), (Die::B0, Layout::Sp7B0));
        assert_eq!(b0.devices().table, 0xfc4ac8);
        assert_eq!(part(0x00b5_0f0b, 2).devices().table, 0xfc4ac8);
        assert_eq!(part(0x00bc_0f01, 2).die, Die::B0);
        let m8 = part(0x00b5_0f80, 0xd);
        assert_eq!((m8.devices().table, m8.layout()), (0xfc47c8, Layout::Sp7B0));
        assert_eq!(part(0x00b5_0f80, 2).die, Die::B0);
        for sp8 in [part(0x00ba_0fc0, 8), part(0x00b9_0f00, 4)] {
            assert_eq!((sp8.devices().table, sp8.layout()), (0xfc48c8, Layout::Sp8));
        }
        // Where the vendor's per-block rules disagreed, the die decides.
        let a0_other_package = part(0x00b5_0f00, 1);
        assert_eq!(a0_other_package.layout(), Layout::Sp7A0);
        assert_eq!(a0_other_package.root_bridges_per_iod(), 2);
        assert_eq!(part(0x00b9_0f01, 4).layout(), Layout::Sp8);
        // NBIO's exact gates.
        assert!(part(0x00b9_0f00, 4).sp8() && !part(0x00ba_0fc0, 8).sp8());
        assert!(part(0x00b5_0f10, 2).base_b0_sp8() && !b0.base_b0_sp8());
        assert!(a0.revision().is_some() && part(0x00b9_0f00, 4).revision().is_none());
    }

    /// The root bridges each IOD has by its DF table are the ones its PCIe
    /// layout puts wrappers on, and their IOHCs are distinct.
    #[test]
    fn layout_and_device_table_agree_on_the_root_bridges() {
        for die in [Die::A0, Die::B0, Die::M8, Die::Sp8] {
            let p = Part {
                signature: 0,
                package: 0,
                die,
            };
            let rbs = p.root_bridges_per_iod();
            let layout = p.layout();
            assert_eq!(layout.iohcs().len(), usize::from(rbs), "{die:?}");
            assert!(layout.wrappers().iter().all(|w| w.rb < rbs), "{die:?}");
            let iohcs = layout.iohcs();
            assert!(
                iohcs
                    .iter()
                    .enumerate()
                    .all(|(i, a)| iohcs[i + 1..].iter().all(|b| a != b))
            );
        }
    }
}
