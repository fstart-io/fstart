// SPDX-License-Identifier: MIT OR Apache-2.0
//! Qilian's ISA step (`InitializeFchIsaQlTp1`, Dell SilVenicePei
//! `0xf1d930`; GIGABYTE `0x7099c3a3`, same steps; FCH.md §2.3, RESIDUAL.md
//! §4): the LPC bridge's reset (`FchInitResetLpcV80 0xf2f3a0`), its control
//! (`FchInitLpcCtrl 0xf2f2d0`) and the SPI controller's (`FchIsaInitSpi
//! 0xf2efe0`), in that order, after the HW-ACPI step.
//!
//! Both donors build with `PcdLpcEnable` 0 (Dell token 0x81, GIGABYTE ex
//! `0x3F400`): their LPC bridge loses every fixed and wide I/O decode
//! (`D14F3x44` = 0, `D14F3x48 &= 0xFF00`) once the ABL has put the BMC's
//! ports on eSPI, which has its own decodes ([`super::espi`]).
//!
//! The SPI writes leave the flash readable at every point: the normal-read
//! clock only (`SPIx22[15:12]`; Dell 16.66 MHz, GIGABYTE 33 MHz) changes,
//! the fast-read clock and read mode the PSP chose and the command engine's
//! alternate clock (`PcdResetWriteSpeed` 0 in both: untouched) stay; the SMN
//! window ([`crate::fch::spi::smn`]) and the command engine
//! ([`crate::fch::spi::flash`]) are synchronous, so no read is in flight
//! across them; host prefetch and the 4-dword host read only change how
//! x86 ROM2/ROM3 reads are fetched.

use tock_registers::interfaces::{ReadWriteable, Writeable};

use crate::fch::FchHw;
use crate::fch::lpc::{self, Bits};
use crate::fch::spi::{self, Cntrl1, Speed, Spi100HostPrefetch, Spi100Speed};
use crate::window::{Window, update};

/// The inputs of the ISA step (the FCH ISA input block, Dell host glue
/// `0xf0d360`, and FCH class `+0x30`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// `PcdLpcEnable` (0 in both donors): keep the LPC bridge's decodes.
    pub lpc: bool,
    /// `PcdLpcClk0`/`PcdLpcClk1` (1/1): `D14F3xD1[5]`/`[6]`, LPC only.
    pub lpc_clocks: (bool, bool),
    /// `PcdLegacyFree` (FCH class `+0x30`, 1/1): with LPC, the fixed
    /// decodes `D14F3x44` = `0x0003_C000` instead of `0xFF03_FFD5`.
    pub legacy_free: bool,
    /// The normal-read clock (`PcdResetSpiSpeed` − 1; `None` keeps it):
    /// Dell 4 → 16.66 MHz, GIGABYTE 2 → 33 MHz.
    pub normal_speed: Option<Speed>,
    /// The alternate (command engine) clock (`PcdResetWriteSpeed` − 1;
    /// 0 in both donors: `None`).
    pub alternate_speed: Option<Speed>,
    /// The TPM clock (`PcdResetSpiTpmSpeed` − 1; 0 in both: `None`).
    pub tpm_speed: Option<Speed>,
}

impl Policy {
    /// The Dell PR9XJ 1.0.8 build's values [P].
    pub const DELL: Self = Self {
        lpc: false,
        lpc_clocks: (true, true),
        legacy_free: true,
        normal_speed: Some(Speed::Mhz16),
        alternate_speed: None,
        tpm_speed: None,
    };
    /// The GIGABYTE MD25-CG0 F01 build's (`PcdResetSpiSpeed` ex `0x3F011`
    /// = 2): Dell's but a 33 MHz normal read.
    pub const GIGABYTE: Self = Self {
        normal_speed: Some(Speed::Mhz33),
        ..Self::DELL
    };
}

/// The step, in the vendor's order.
pub fn init(fch: &impl FchHw, policy: &Policy) {
    let lpc = fch.lpc();
    update::<u8, _>(&lpc, lpc::R6C.offset, 0xff, 0);
    if policy.lpc {
        // PCI table `0xfc4da4`: x48 byte `(x & 4) | 3`, xA4 byte `| 1`.
        update::<u8, _>(&lpc, lpc::IO_MEM_DECODE.offset, 0xfb, 0x03);
        update::<u8, _>(&lpc, lpc::RA4.offset, 0, 0x01);
        let (clk0, clk1) = policy.lpc_clocks;
        lpc.at(lpc::CLOCK_CONTROL_BYTE1)
            .modify(Bits::B5.val(clk0.into()));
        lpc.at(lpc::CLOCK_CONTROL_BYTE1)
            .modify(Bits::B6.val(clk1.into()));
        let decode = lpc.at(lpc::IO_PORT_DECODE);
        if policy.legacy_free {
            // The vendor's legacy-free `0x0003_C000`: the audio ports only.
            decode.write(lpc::IoPortDecode::AUDIO.val(0xf));
        } else {
            decode.set(lpc::RESET_PORT_DECODE);
        }
        // `FchInitLpcCtrl`, table `0xfc4d88`: x78[1:0] off.
        lpc.at(lpc::MISC_CONTROL)
            .modify(Bits::B0::CLEAR + Bits::B1::CLEAR);
    } else {
        lpc.at(lpc::IO_PORT_DECODE).set(0);
        update::<u32, _>(&lpc, lpc::IO_MEM_DECODE.offset, !0xff00, 0);
    }
    // `FchIsaInitSpi`.
    let mmio = fch.mmio();
    mmio.at(spi::CNTRL1).modify(Cntrl1::WAIT_COUNT.val(0));
    let speed = mmio.at(spi::SPI100_SPEED);
    for (field, value) in [
        (Spi100Speed::NORMAL, policy.normal_speed),
        (Spi100Speed::ALTERNATE, policy.alternate_speed),
        (Spi100Speed::TPM, policy.tpm_speed),
    ] {
        if let Some(value) = value {
            speed.modify(field.val(value as u16));
        }
    }
    let prefetch = mmio.at(spi::SPI100_HOST_PREFETCH);
    prefetch.modify(Spi100HostPrefetch::HOST_BURST_EN::SET);
    prefetch.modify(Spi100HostPrefetch::RD4DW_EN_HOST::CLEAR);
    lpc.at(lpc::ROM_DMA_CONTROL_HIGH).modify(Bits::B0::SET);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::SPI_BASE;
    use crate::fch::testing::{Space, World};

    #[test]
    fn lpc_off_drops_the_decodes_and_sets_the_spi_clock() {
        let world = World::default();
        world.set(Space::Lpc, 0x44, 4, 0x0000_0040);
        world.set(Space::Lpc, 0x48, 4, 0x0020_ff13);
        world.set(Space::Lpc, 0x6c, 4, 0x1234_56ff);
        world.set(Space::Mmio, SPI_BASE + 0x0c, 4, 0x003f_0000);
        world.set(Space::Mmio, SPI_BASE + 0x20, 4, 0x4444_0001);
        world.set(Space::Mmio, SPI_BASE + 0x2c, 2, 0x8000);
        let fch = world.fch();
        init(&fch, &Policy::GIGABYTE);
        assert_eq!(world.get(Space::Lpc, 0x44, 4), 0);
        assert_eq!(world.get(Space::Lpc, 0x48, 4), 0x0000_ff00);
        assert_eq!(world.get(Space::Lpc, 0x6c, 4), 0x1234_5600);
        assert_eq!(world.get(Space::Mmio, SPI_BASE + 0x0c, 4), 0);
        // Normal 33 MHz; fast, alternate and TPM clocks kept; SPI100 on.
        assert_eq!(world.get(Space::Mmio, SPI_BASE + 0x20, 4), 0x1444_0001);
        assert_eq!(world.get(Space::Mmio, SPI_BASE + 0x2c, 2), 0x4000);
        assert_eq!(world.get(Space::Lpc, 0xbb, 1), 1);
        init(&fch, &Policy::DELL);
        assert_eq!(world.get(Space::Mmio, SPI_BASE + 0x23, 1) >> 4, 0x3);
    }

    #[test]
    fn lpc_on_takes_the_vendor_tables() {
        let world = World::default();
        world.set(Space::Lpc, 0x48, 4, 0x0020_ff3f);
        world.set(Space::Lpc, 0x78, 1, 0xff);
        let fch = world.fch();
        let policy = Policy {
            lpc: true,
            ..Policy::DELL
        };
        init(&fch, &policy);
        assert_eq!(world.get(Space::Lpc, 0x48, 4), 0x0020_ff07);
        assert_eq!(world.get(Space::Lpc, 0xa4, 1), 1);
        assert_eq!(world.get(Space::Lpc, 0xd1, 1), 0x60);
        assert_eq!(world.get(Space::Lpc, 0x44, 4), 0x0003_c000);
        assert_eq!(world.get(Space::Lpc, 0x78, 1), 0xfc);
    }
}
