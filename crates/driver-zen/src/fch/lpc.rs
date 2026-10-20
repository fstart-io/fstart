//! The LPC bridge (D14F3, FCH::ITF::LPC): legacy decode, wide I/O windows,
//! SPI BAR, and its reset/env tasks (FCH_SP3 §3.1 af42, §3.2 af52).

use fstart_pci::PciAddress;
use tock_registers::interfaces::{ReadWriteable, Writeable};
use tock_registers::register_bitfields;

use super::{FchHw, Window, update};
use crate::soc::Generation;

/// The LPC bridge function.
pub fn lpc() -> PciAddress {
    PciAddress::new(0, 0, 0x14, 3)
}

pub const PCI_CONTROL: u32 = 0x40;
pub const IO_PORT_DECODE: u32 = 0x44;
pub const IO_MEM_DECODE: u32 = 0x48;
pub const WIDE_IO_0_1: u32 = 0x64;
pub const ROM_RANGE2: u32 = 0x6C;
pub const ALT_WIDE_IO: u32 = 0x74;
pub const MISC_CONTROL: u32 = 0x78;
pub const TPM: u32 = 0x7C;
pub const WIDE_IO_2: u32 = 0x90;
pub const SPI_BASE: u32 = 0xA0;
pub const LPC_A4: u32 = 0xA4;
pub const LPC_BA: u32 = 0xBA;
pub const LPC_BB: u32 = 0xBB;
pub const CLK_CONTROL: u32 = 0xD0;

register_bitfields![u32,
    /// D14F3x48 FCH::ITF::LPC::IOMemPortDecodeEn.
    pub IO_MEM_DECODE [
        SUPER_IO OFFSET(0) NUMBITS(1) [],
        ALT_SUPER_IO OFFSET(1) NUMBITS(1) [],
        WIDE_IO0 OFFSET(2) NUMBITS(1) [],
        RTC_IO OFFSET(6) NUMBITS(1) [],
        WIDE_IO1 OFFSET(24) NUMBITS(1) [],
        WIDE_IO2 OFFSET(25) NUMBITS(1) []
    ],
    /// D14F3x74 FCH::ITF::LPC::AlternativeWideIORngEn (16-byte windows).
    pub ALT_WIDE_IO [
        WIDE_IO0 OFFSET(0) NUMBITS(1) [],
        WIDE_IO1 OFFSET(2) NUMBITS(1) [],
        WIDE_IO2 OFFSET(3) NUMBITS(1) []
    ],
    /// D14F3xA0 FCH::ITF::LPC::SPIBaseAddr.
    pub SPI_BASE [
        ALT_SPI_CS OFFSET(0) NUMBITS(1) [],
        SPI_ROM_ENABLE OFFSET(1) NUMBITS(1) [],
        ABORT_ENABLE OFFSET(2) NUMBITS(1) [],
        ROUTE_TPM_TO_SPI OFFSET(3) NUMBITS(1) [],
        PSP_SPI_MMIO_SEL OFFSET(4) NUMBITS(1) [],
        BASE OFFSET(8) NUMBITS(24) []
    ]
];

/// Legacy decode enables of D14F3x44 after the reset task (FCH_SP3 §3.1
/// af42): serial, parallel, KBC, ACPI micro-controller and the rest.
pub const RESET_PORT_DECODE: u32 = 0xFF03_FFD5;

/// One of the three wide I/O windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WideIo {
    W0,
    W1,
    W2,
}

/// Open wide I/O window `which` at `base`: 512 bytes, or 16 bytes with
/// `small` (the alternative range). Boards use this for BMC/SIO ranges
/// (S8030: `A00h`, `3E0h`, `2E0h`, KCS `CA0h`; BOARD_S8030 §4).
pub fn set_wide_io<H: FchHw>(hw: &H, which: WideIo, base: u16, small: bool) {
    let lpc = hw.lpc();
    let (reg, enable, alt) = match which {
        WideIo::W0 => (
            WIDE_IO_0_1,
            IO_MEM_DECODE::WIDE_IO0::SET,
            ALT_WIDE_IO::WIDE_IO0,
        ),
        WideIo::W1 => (
            WIDE_IO_0_1 + 2,
            IO_MEM_DECODE::WIDE_IO1::SET,
            ALT_WIDE_IO::WIDE_IO1,
        ),
        WideIo::W2 => (
            WIDE_IO_2,
            IO_MEM_DECODE::WIDE_IO2::SET,
            ALT_WIDE_IO::WIDE_IO2,
        ),
    };
    lpc.reg::<u16, ()>(reg).set(base);
    lpc.reg::<u32, ALT_WIDE_IO::Register>(ALT_WIDE_IO)
        .modify(alt.val(small.into()));
    lpc.reg::<u32, IO_MEM_DECODE::Register>(IO_MEM_DECODE)
        .modify(enable);
}

/// Enable the Super I/O config ports (`2Eh` and/or `4Eh`).
pub fn enable_super_io<H: FchHw>(hw: &H, port_2e: bool, port_4e: bool) {
    hw.lpc()
        .reg::<u32, IO_MEM_DECODE::Register>(IO_MEM_DECODE)
        .modify(
            IO_MEM_DECODE::SUPER_IO.val(port_2e.into())
                + IO_MEM_DECODE::ALT_SUPER_IO.val(port_4e.into()),
        );
}

/// PEI reset task af42.
pub fn reset<H: FchHw>(hw: &H, generation: Generation) {
    let lpc = hw.lpc();
    update::<u32, _>(&lpc, ROM_RANGE2, 0xff, 0);
    lpc.write8(IO_MEM_DECODE, 0x07);
    if generation == Generation::Naples {
        lpc.write8(TPM, 0x05);
        update::<u8, _>(&lpc, LPC_BA, 0, 0xe0);
    }
    update::<u8, _>(&lpc, LPC_A4, 0, 0x01);
    // D14F3xD1: bit 5 then bit 6 (FCH::ITF::LPC::ClkCntrl[14:13] =
    // LPCCLK0/1 enable), two writes as in the donor.
    update::<u8, _>(&lpc, CLK_CONTROL + 1, 0, 0x20);
    update::<u8, _>(&lpc, CLK_CONTROL + 1, 0, 0x40);
    lpc.write32(IO_PORT_DECODE, RESET_PORT_DECODE);
}

/// DXE env task af52 (LPC part). The ZP JEDEC-ID read and EFS rewrite of
/// af53 (FCH_SP3 §3.2) are not ported: they serve AGESA's flash-part
/// override, not the FCH.
pub fn env<H: FchHw>(hw: &H, generation: Generation) {
    let lpc = hw.lpc();
    // D14F3x40[LegacyDmaEnable].
    update::<u8, _>(&lpc, PCI_CONTROL, 0, 0x04);
    if generation == Generation::Naples {
        lpc.write8(IO_MEM_DECODE, 0x07);
    }
    // D14F3x78[1:0]: NoHog off.
    update::<u8, _>(&lpc, MISC_CONTROL, 0x03, 0);
    update::<u8, _>(&lpc, LPC_BB, 0, 0x0c);
}

#[cfg(test)]
mod tests {
    use super::super::model::fch;
    use super::*;

    fn cfg(m: &crate::smn::model::Model, off: u16) -> u32 {
        m.0.borrow()
            .config
            .get(&(0, 0x14, 3, off))
            .copied()
            .unwrap_or(0)
    }

    #[test]
    fn reset_task_rome() {
        let hw = fch();
        hw.pci
            .0
            .borrow_mut()
            .config
            .insert((0, 0x14, 3, 0x6c), 0xffff_ff00);
        hw.pci
            .0
            .borrow_mut()
            .config
            .insert((0, 0x14, 3, 0x48), 0x0000_ff04);
        reset(&hw, Generation::Rome);
        assert_eq!(cfg(&hw.pci, 0x6c), 0xffff_ff00);
        assert_eq!(cfg(&hw.pci, 0x48), 0x0000_ff07);
        assert_eq!(cfg(&hw.pci, 0x44), RESET_PORT_DECODE);
        assert_eq!(cfg(&hw.pci, 0x7c), 0, "TPM register is ZP only");
        assert_eq!(cfg(&hw.pci, 0xd0) >> 8 & 0xff, 0x60);
    }

    #[test]
    fn wide_io_window() {
        let hw = fch();
        set_wide_io(&hw, WideIo::W1, 0x0ca0, true);
        assert_eq!(cfg(&hw.pci, 0x64) >> 16, 0x0ca0);
        assert_eq!(cfg(&hw.pci, 0x74), 0x4);
        assert_eq!(cfg(&hw.pci, 0x48), 1 << 24);
    }
}
