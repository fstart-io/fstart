// SPDX-License-Identifier: MIT OR Apache-2.0
//! Qilian (`Ql`), the FCH of the WH dual-IOD platform (up to 4 FCH instances:
//! 2 sockets × 2 IODs).
//!
//! Primary FCH takeover, ACPI handoff, and GPIO PERST release reuse
//! [`super::songshan`] (`Pin(26)` at drive strength 2 writes `0x00C4_0000` to
//! `acpi_mmio + 0x1568`, matching `FchIp2IpDeassertPciePerstQl`). This module
//! adds Qilian Multi-FCH secondary ACPI MMIO relocation (`SMN 0x02D0_13D4`),
//! secondary PERST release on relocated ACPI MMIO windows, and what differs
//! from Songshan (FCH.md; Dell PR9XJ and GIGABYTE MD25-CG0 SilVenicePei):
//!
//! - [`espi`]: the server eSPI controllers, whose link the ABL owns: decode
//!   windows only, never a reset;
//! - [`tpm`]: the discrete TPM on the SPI TPM chip select;
//! - [`hwacpi`]: the HW-ACPI tables, interrupt routes, A-Link decode and
//!   sync flood;
//! - [`isa`]: the LPC bridge's and SPI controller's reset;
//! - [`secondary`]: each secondary FCH's HwAcpi, A-Link and SD steps;
//! - [`sx`]: S5 through SMU v14's posted sleep entry;
//! - [`usb`]: Songshan's xHCI messages with Qilian's PHY register and
//!   debug-pipe messages;
//! - [`uart`]: the UARTs' pads, AOAC devices and legacy decodes;
//! - [`pm`]: HW-ACPI's register steps beyond the tables (watchdog decode
//!   and cold-reset acknowledge, RTC extension, HPET, pre-OS);
//! - [`i2c`]: the two DesignWare I2C masters and their timing;
//! - [`reset`]: the vendor's warm, cold and full reset recipes, the last
//!   reset's kind and the guard that bounds firmware warm resets;
//! - [`scratch`]: the bytes that survive a warm reset (CMOS bank 1, BIOS
//!   RAM) and who owns them.
//!
//! The flash's SMN window is [`super::spi::smn`], ROM3's hardware base
//! [`super::rom3::hardware_base`].

pub mod espi;
pub mod hwacpi;
pub mod i2c;
pub mod isa;
pub mod pm;
pub mod reset;
pub mod scratch;
pub mod secondary;
pub mod sx;
pub mod tpm;
pub mod uart;
pub mod usb;

use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;

use super::gpio::{Control as GpioControl, Pin};
pub use super::songshan::{Perst, acpi_mode, release_perst, route_legacy, take_over};
use crate::smn::{SmnAccess, SmnReg};
use crate::window::Window;

/// Maximum FCH instances in a 2P dual-IOD system.
pub const MAX_FCHS: u8 = 4;

/// Common PCIe PERST# output (`acpi_mmio + 0x1568 = 0x00C4_0000`).
pub const PERST_COMMON: Perst = Perst {
    pin: Pin(26),
    drive_strength: 2,
};

register_bitfields![u32,
    /// SMN 0x02D0_13D4 (32-bit): Qilian secondary FCH ACPI MMIO base/enable
    /// (`(base & 0xFFFF_0000) | 0x100`).
    pub AcpiMmioDecode [
        ENABLE OFFSET(8) NUMBITS(1) [],
        BASE OFFSET(16) NUMBITS(16) []
    ]
];

/// SMN 0x02D0_13D4 (32-bit): secondary FCH ACPI MMIO decode register.
pub const SECONDARY_ACPI_MMIO: SmnReg<AcpiMmioDecode::Register> = SmnReg::new(0x02d0_13d4);

/// Relocate a secondary Qilian FCH's ACPI MMIO window to 64 KiB-aligned `base`
/// and verify via SMN readback that the aperture is live and decoded at `base`.
/// Returns `false` if `base` is zero, misaligned, or readback fails / returns
/// all-ones.
pub fn set_secondary_acpi_mmio(smn: &impl SmnAccess, base: u32) -> bool {
    if base == 0 || (base & 0xffff) != 0 {
        return false;
    }
    let reg = smn.reg(SECONDARY_ACPI_MMIO);
    if reg.get() == u32::MAX {
        return false;
    }
    let want = AcpiMmioDecode::BASE.val(base >> 16) + AcpiMmioDecode::ENABLE::SET;
    reg.write(want);
    let got = reg.get();
    got != u32::MAX && reg.matches_all(want)
}

/// Size of a secondary Qilian FCH ACPI MMIO allocation (`0x2000`, 64 KiB aligned;
/// verified from openSIL `MultiFch.c` `FchMultiFchAllocateAcpiMmio`).
pub const SECONDARY_ACPI_MMIO_SIZE: u64 = 0x2000;
/// Alignment of a secondary Qilian FCH ACPI MMIO allocation (64 KiB).
pub const SECONDARY_ACPI_MMIO_ALIGN: u64 = 0x1_0000;

/// Relocate and verify a secondary Qilian FCH instance's ACPI MMIO decode
/// (`fch_index` in `1..MAX_FCHS`, `SMN 0x02D0_13D4`).
///
/// The FCH's own init follows ([`secondary::init`]).
pub fn init_secondary_fch(smn: &impl SmnAccess, fch_index: u8, mmio_base: u32) -> bool {
    if fch_index == 0 || fch_index >= MAX_FCHS {
        return false;
    }
    set_secondary_acpi_mmio(smn, mmio_base)
}

/// Release PCIe slot resets `outputs` on any Qilian FCH ACPI MMIO [`Window`]
/// (primary or relocated secondary FCH).
///
/// Matches `FchIp2IpDeassertPciePerstQl` (`decomp/00f67c80.c`), which writes
/// `0x00C4_0000` to `acpi_mmio + 0x1568` on every live FCH instance. Returns
/// `false` without writing if any pin is invalid, drive strength is out of
/// range, or the pad reads back all-ones; also returns `false` if any write
/// fails readback verification.
pub fn release_perst_window(acpi: &impl Window, outputs: &[Perst]) -> bool {
    let control = |pin: Pin| acpi.reg::<u32, GpioControl::Register>(pin.control());
    if outputs.iter().any(|output| {
        !output.pin.is_valid() || output.drive_strength > 3 || control(output.pin).get() == u32::MAX
    }) {
        return false;
    }
    outputs.iter().all(|output| {
        let value = GpioControl::OUTPUT::SET
            + GpioControl::OUTPUT_ENABLE::SET
            + GpioControl::DRIVE_STRENGTH.val(output.drive_strength.into());
        control(output.pin).write(value);
        control(output.pin).matches_all(value)
    })
}

/// Secondary FCH AB routing FabricID offset for FCH instance `fch_index` (`0..4`).
/// Primary FCH (`0`) has offset `0`; secondaries `1..=3` use `0x40`, `0x80`, `0xC0`.
pub const fn ab_fabric_id_offset(fch_index: u8) -> Option<u16> {
    if fch_index >= MAX_FCHS {
        return None;
    }
    Some((fch_index as u16) * 0x40)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::fch;
    use crate::smn::testing::{SmnModel, SmnSpace, SmnTest};

    #[test]
    fn secondary_acpi_mmio_encodes_and_verifies_readback() {
        let space = SmnSpace::default();
        assert!(!set_secondary_acpi_mmio(&space.smn(), 0));
        assert!(!set_secondary_acpi_mmio(&space.smn(), 0xfed8_1000));
        assert!(set_secondary_acpi_mmio(&space.smn(), 0xfed9_0000));
        assert_eq!(space.get(SECONDARY_ACPI_MMIO.address()), 0xfed9_0100);

        // Unavailable aperture (all-ones) is rejected.
        struct Dead;
        impl SmnModel for Dead {
            fn read(&mut self, _: u32) -> u32 {
                u32::MAX
            }
            fn write(&mut self, _: u32, _: u32) {}
        }
        let dead = SmnTest::new(Dead);
        assert!(!init_secondary_fch(&dead.smn(), 1, 0xfed9_0000));
    }

    #[test]
    fn ab_fabric_offsets_and_common_perst_match_ql() {
        assert_eq!(ab_fabric_id_offset(0), Some(0x00));
        assert_eq!(ab_fabric_id_offset(1), Some(0x40));
        assert_eq!(ab_fabric_id_offset(2), Some(0x80));
        assert_eq!(ab_fabric_id_offset(3), Some(0xc0));
        assert_eq!(ab_fabric_id_offset(4), None);

        let hw = fch();
        assert!(release_perst(&hw, &[PERST_COMMON]));
        assert_eq!(hw.acpi.get(0x1568, 4), 0x00c4_0000);

        let sec = fch();
        assert!(release_perst_window(&sec.acpi, &[PERST_COMMON]));
        assert_eq!(sec.acpi.get(0x1568, 4), 0x00c4_0000);
    }
}
