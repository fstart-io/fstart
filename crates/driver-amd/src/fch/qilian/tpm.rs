// SPDX-License-Identifier: MIT OR Apache-2.0
//! The discrete TPM on Qilian's SPI TPM chip select.
//!
//! Both Venice donors put the TPM on SPI, not on LPC/eSPI, and route it
//! from board code (FCH.md §1.2, §2.3): Dell PlatformInit
//! `DoSbInitialization 0xc1bd00` (IOMUX `0xD4C` = 1, `D14F3x7C |= 0x3085`,
//! `D14F3xA0 |= 8`) and GIGABYTE ChipsetSvcPei `ConfigureTpmDecoder
//! 0x70b5d0a2` (`D14F3x7C |= 0x83`, `D14F3xA0 |= 8`, IOMUX pad 76 = 1).
//! openSIL's own ISA TPM hook is a stub on Qilian. The TPM then answers at
//! the TIS/FIFO window `FED4_0000h`.
//!
//! The decode is GIGABYTE's (the MD25-CG0 donor): bits 0 and 7 are the
//! ones both donors set; bit 1 is GIGABYTE's, Dell's dword also sets 2, 12
//! and 13 (their meaning is open, FCH.md §4/5). Dell's `SPIx2E |= 0x200`
//! (TPM_DI_TO_ENABLE) is board policy and left to the board.

use crate::fch::FchHw;
use crate::fch::gpio::{self, Pad, Pin};
use crate::fch::lpc::{self, TpmControl};

/// The SPI TPM chip select pad and its IOMUX function (`SPI_TPM_CS_L`).
pub const TPM_CS: Pad = Pad::mux(Pin(76), 1);

/// Route the TPM to the SPI controller's TPM chip select, decode
/// `FED4_xxxx`, and give the pad its CS function, before the TPM is probed.
/// `false` if the routing bit does not read back.
pub fn route_to_spi(fch: &impl FchHw) -> bool {
    lpc::decode_tpm(
        fch,
        TpmControl::TPM12_EN::SET + TpmControl::BIT1::SET + TpmControl::BIT7::SET,
    );
    let routed = lpc::route_tpm_to_spi(fch);
    gpio::configure(fch.acpi(), &TPM_CS);
    routed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::acpimmio::IOMUX;
    use crate::fch::model::fch;

    #[test]
    fn routes_like_the_gigabyte_board_code() {
        let hw = fch();
        assert!(route_to_spi(&hw));
        let lpc = |off| hw.pci.0.borrow().config[&(0, 0x14, 3, off)];
        assert_eq!(lpc(0x7c) & 0xff, 0x83);
        assert_eq!(lpc(0xa0) & 8, 8);
        assert_eq!(hw.acpi.get(IOMUX + 76, 1), 1);
        // The pad's control register is not written.
        assert_eq!(hw.acpi.writes(), [(IOMUX + 76, 1, 1)]);
    }
}
