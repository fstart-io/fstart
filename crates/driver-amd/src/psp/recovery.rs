// SPDX-License-Identifier: MIT OR Apache-2.0
//! The recovery reason the x86 leaves the WH PSP in FCH BIOS RAM 78h–7Fh
//! before a warm reset that switches the PSP's boot partition
//! (reports/research/WARM_RESET.md §2.3, §2.5; `SetRecoveryReason`, Dell
//! SilVenicePei `0xf17c80`, GIGABYTE `0x7092f9fa`; the PSP boot loader
//! writes the same eight bytes at `0x18a6`).
//!
//! Two layouts, chosen by CPUID from the vendor's SoC profile table
//! ([`PROFILES`]): v2 is one dword at 78h, v3 two (78h, 7Ch). The vendor
//! sets one only before a partition switch (`{66h, instance, 0, 3, 0}`
//! after a microcode `ValidateBinary` failure), which fstart does not do:
//! it validates its microcode itself (WARM_RESET G11).

use tock_registers::interfaces::Writeable;
use tock_registers::register_bitfields;

use crate::fch::qilian::scratch::{bios_ram, enable_bios_ram};
use crate::window::{At, Window};

register_bitfields![u32,
    /// BIOS RAM 78h, v2.
    pub ReasonV2 [
        ID OFFSET(0) NUMBITS(8) [],
        A OFFSET(8) NUMBITS(4) [],
        B OFFSET(12) NUMBITS(4) [],
        C OFFSET(16) NUMBITS(4) [],
        D OFFSET(22) NUMBITS(3) []
    ],
    /// BIOS RAM 78h, v3.
    pub ReasonV3 [
        ID OFFSET(0) NUMBITS(16) [],
        A OFFSET(16) NUMBITS(4) [],
        B OFFSET(20) NUMBITS(4) []
    ],
    /// BIOS RAM 7Ch, v3.
    pub ReasonV3High [
        C OFFSET(0) NUMBITS(4) [],
        D OFFSET(4) NUMBITS(3) []
    ]
];

/// BIOS RAM 78h (both layouts).
pub const REASON_V2: At<u32, ReasonV2::Register> = bios_ram(0x78);
pub const REASON_V3: At<u32, ReasonV3::Register> = bios_ram(0x78);
/// BIOS RAM 7Ch (v3).
pub const REASON_V3_HIGH: At<u32, ReasonV3High::Register> = bios_ram(0x7c);

/// The layout of the recovery reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V2,
    V3,
}

/// The vendor's SoC profile table, its recovery-reason column: CPUID(1)
/// with the stepping masked off (SilVenicePei, Dell file offset `0xba180`,
/// GIGABYTE `0x2eb00`, byte-identical; `WARM_RESET/soc_profiles.json`).
pub const PROFILES: [(u32, Version); 14] = [
    (0x00b8_0f00, Version::V3),
    (0x00b8_0f80, Version::V3),
    (0x00be_0f00, Version::V3),
    (0x00be_0f40, Version::V3),
    (0x00b5_0f00, Version::V2),
    (0x00b5_0f80, Version::V3),
    (0x00b9_0f00, Version::V3),
    (0x00ba_0f00, Version::V3),
    (0x00bc_0f00, Version::V2),
    (0x00bc_0f80, Version::V3),
    (0x00c0_0f00, Version::V3),
    (0x00c0_0f80, Version::V3),
    (0x00c1_0f00, Version::V3),
    (0x00c1_0f80, Version::V3),
];

/// The layout for CPUID(1) `signature`: the vendor's match order, exact,
/// then `& 0FFF_0FF0h`, then `& 0FFF_0F00h`.
pub fn version(signature: u32) -> Option<Version> {
    [u32::MAX, 0x0fff_0ff0, 0x0fff_0f00]
        .into_iter()
        .find_map(|mask| {
            PROFILES
                .iter()
                .find(|(cpuid, _)| *cpuid == signature & mask)
                .map(|&(_, version)| version)
        })
}

/// A recovery reason: an ID and four small fields whose meaning the
/// vendor code does not name (`a` the instance in the microcode case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reason {
    pub id: u16,
    pub a: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
}

impl Reason {
    /// The microcode `ValidateBinary` failure of `instance`.
    pub const fn microcode(instance: u8) -> Self {
        Self {
            id: 0x66,
            a: instance & 0xf,
            b: 0,
            c: 3,
            d: 0,
        }
    }

    /// Write it to BIOS RAM in `version`'s layout: `acpi` is the FCH's
    /// ACPI MMIO (PMx04\[0\] decodes BIOS RAM), `mmio` the host's physical
    /// window.
    pub fn write<A: Window, M: Window>(&self, acpi: &A, mmio: &M, version: Version) {
        enable_bios_ram(acpi);
        let nibble = |v: u8| u32::from(v & 0xf);
        match version {
            Version::V2 => mmio.at(REASON_V2).write(
                ReasonV2::ID.val(u32::from(self.id & 0xff))
                    + ReasonV2::A.val(nibble(self.a))
                    + ReasonV2::B.val(nibble(self.b))
                    + ReasonV2::C.val(nibble(self.c))
                    + ReasonV2::D.val(u32::from(self.d & 7)),
            ),
            Version::V3 => {
                mmio.at(REASON_V3).write(
                    ReasonV3::ID.val(self.id.into())
                        + ReasonV3::A.val(nibble(self.a))
                        + ReasonV3::B.val(nibble(self.b)),
                );
                mmio.at(REASON_V3_HIGH).write(
                    ReasonV3High::C.val(nibble(self.c))
                        + ReasonV3High::D.val(u32::from(self.d & 7)),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::acpimmio::PM;
    use crate::fch::model::Mem;

    #[test]
    fn version_follows_the_vendor_match_order() {
        assert_eq!(version(0x00b5_0f00), Some(Version::V2));
        assert_eq!(version(0x00b5_0f01), Some(Version::V2));
        assert_eq!(version(0x00b5_0f81), Some(Version::V3));
        assert_eq!(version(0x00bc_0f12), Some(Version::V2));
        assert_eq!(version(0x00bc_0f8f), Some(Version::V3));
        // Only the third mask finds a row for an unlisted model nibble.
        assert_eq!(version(0x00b8_0f31), Some(Version::V3));
        assert_eq!(version(0x00a7_0f00), None);
    }

    #[test]
    fn layouts_v2_and_v3() {
        let reason = Reason {
            id: 0x1266,
            a: 0x15,
            b: 2,
            c: 3,
            d: 0xf,
        };
        let (acpi, mmio) = (Mem::default(), Mem::default());
        reason.write(&acpi, &mmio, Version::V2);
        assert_eq!(acpi.get(PM + 4, 1) & 1, 1);
        assert_eq!(
            mmio.writes(),
            [(0xfed1_0078, 4, 0x66 | 5 << 8 | 2 << 12 | 3 << 16 | 7 << 22)]
        );
        let mmio = Mem::default();
        reason.write(&acpi, &mmio, Version::V3);
        assert_eq!(
            mmio.writes(),
            [
                (0xfed1_0078, 4, 0x1266 | 5 << 16 | 2 << 20),
                (0xfed1_007c, 4, 3 | 7 << 4)
            ]
        );
        assert_eq!(
            Reason::microcode(0x12),
            Reason {
                id: 0x66,
                a: 2,
                b: 0,
                c: 3,
                d: 0
            }
        );
    }
}
