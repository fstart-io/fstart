// SPDX-License-Identifier: MIT OR Apache-2.0
//! The LPC bridge (D14F3, FCH::ITF::LPC) of every Family 17h FCH: legacy
//! decode, wide I/O windows, SPI BAR, and its reset/env tasks (FCH_SP3
//! §3.1 af42, §3.2 af52).
use super::FchHw;
use crate::cpu::Silicon;
use crate::window::{At, Window, update};
use fstart_pci::PciAddress;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

/// The LPC bridge function.
pub fn function() -> PciAddress {
    PciAddress::new(0, 0, 0x14, 3)
}

register_bitfields![u32,
    /// D14F3x00.
    pub Id [VENDOR OFFSET(0) NUMBITS(16) [Amd = 0x1022]],
    /// D14F3x44: fixed legacy I/O decodes.
    pub IoPortDecode [
        PARALLEL0 OFFSET(0) NUMBITS(2) [],
        SERIAL0 OFFSET(6) NUMBITS(1) [],
        SERIAL1 OFFSET(7) NUMBITS(1) [],
        SERIAL2 OFFSET(8) NUMBITS(1) [],
        SERIAL3 OFFSET(9) NUMBITS(1) [],
        SERIAL4 OFFSET(10) NUMBITS(1) [],
        SERIAL5 OFFSET(11) NUMBITS(1) [],
        SERIAL6 OFFSET(12) NUMBITS(1) [],
        SERIAL7 OFFSET(13) NUMBITS(1) [],
        KBC OFFSET(29) NUMBITS(1) []
    ],
    /// D14F3x48 FCH::ITF::LPC::IOMemPortDecodeEn.
    pub IoMemDecode [
        SIO_2E OFFSET(0) NUMBITS(1) [],
        SIO_4E OFFSET(1) NUMBITS(1) [],
        WIDEIO0 OFFSET(2) NUMBITS(1) [],
        RTC_IO OFFSET(6) NUMBITS(1) [],
        SYNC_TIMEOUT_EN OFFSET(7) NUMBITS(1) [],
        SYNC_TIMEOUT_COUNT OFFSET(8) NUMBITS(8) [],
        PORT80 OFFSET(21) NUMBITS(1) [],
        WIDEIO1 OFFSET(24) NUMBITS(1) [],
        WIDEIO2 OFFSET(25) NUMBITS(1) []
    ],
    /// D14F3x64 (ranges 0 and 1) and D14F3x90 (range 2).
    pub WideIoBase [
        LOW OFFSET(0) NUMBITS(16) [],
        HIGH OFFSET(16) NUMBITS(16) []
    ],
    /// D14F3x74: wide I/O ranges of 16 bytes instead of 512.
    pub AltWideIo [
        WIDEIO0 OFFSET(0) NUMBITS(1) [],
        WIDEIO1 OFFSET(2) NUMBITS(1) [],
        WIDEIO2 OFFSET(3) NUMBITS(1) []
    ],
    /// D14F3xA0 FCH::ITF::LPC::SPIBaseAddr: the SPI/eSPI controller BAR.
    pub SpiBase [
        ROM_ALT_ENABLE OFFSET(0) NUMBITS(1) [],
        ROM_ENABLE OFFSET(1) NUMBITS(1) [],
        ABORT_ENABLE OFFSET(2) NUMBITS(1) [],
        ROUTE_TPM_2_SPI OFFSET(3) NUMBITS(1) [],
        PSP_SPI_MMIO_SEL OFFSET(4) NUMBITS(1) [],
        RESERVED OFFSET(5) NUMBITS(3) [],
        BASE OFFSET(8) NUMBITS(24) []
    ],
    /// D14F3xA4 (the vendor code's bit 0).
    pub LpcA4 [BIT0 OFFSET(0) NUMBITS(1) []],
    /// D14F3xB8.
    pub RomDmaEcHostControl [
        SPI_FROM_USB_PREFETCH_EN OFFSET(23) NUMBITS(1) [],
        SPI_FROM_HOST_PREFETCH_EN OFFSET(24) NUMBITS(1) []
    ],
    /// D14F3xD0.
    pub ClockControl [BIT31 OFFSET(31) NUMBITS(1) []]
];
register_bitfields![u16,
    /// D14F3xBA: ROM DMA control, written as words by the vendor code.
    pub RomDmaControl [
        BIT7 OFFSET(7) NUMBITS(1) [],
        BIT8 OFFSET(8) NUMBITS(1) []
    ]
];
register_bitfields![u8,
    /// D14F3x40.
    pub PciControl [
        LEGACY_DMA_EN OFFSET(2) NUMBITS(1) [],
        VW_ROM_SHARING_EN OFFSET(3) NUMBITS(1) [],
        EXT_ROM_SHARING_EN OFFSET(4) NUMBITS(1) []
    ],
    /// The vendor's bits of single bytes: D14F3x78, 0x7C, 0xBA, 0xBB, 0xD1.
    pub Bits [
        B0 OFFSET(0) NUMBITS(1) [],
        B1 OFFSET(1) NUMBITS(1) [],
        B2 OFFSET(2) NUMBITS(1) [],
        B3 OFFSET(3) NUMBITS(1) [],
        B5 OFFSET(5) NUMBITS(1) [],
        B6 OFFSET(6) NUMBITS(1) []
    ]
];

pub const ID: At<u32, Id::Register> = At::new(0x00);
pub const PCI_CONTROL: At<u8, PciControl::Register> = At::new(0x40);
pub const IO_PORT_DECODE: At<u32, IoPortDecode::Register> = At::new(0x44);
pub const IO_MEM_DECODE: At<u32, IoMemDecode::Register> = At::new(0x48);
pub const MEM_PORT1: At<u32> = At::new(0x4c);
pub const MEM_PORT0: At<u32> = At::new(0x60);
pub const WIDEIO_GENERIC_PORT: At<u32, WideIoBase::Register> = At::new(0x64);
pub const R6C: At<u8> = At::new(0x6c);
pub const ALT_WIDEIO: At<u32, AltWideIo::Register> = At::new(0x74);
pub const MISC_CONTROL: At<u8, Bits::Register> = At::new(0x78);
pub const R7C: At<u8, Bits::Register> = At::new(0x7c);
pub const WIDEIO2_GENERIC_PORT: At<u32, WideIoBase::Register> = At::new(0x90);
pub const SPI_BASE_ADDRESS: At<u32, SpiBase::Register> = At::new(0xa0);
pub const RA4: At<u32, LpcA4::Register> = At::new(0xa4);
pub const RA4_BYTE: At<u8, Bits::Register> = At::new(0xa4);
pub const ROM_DMA_EC_HOST_CONTROL: At<u32, RomDmaEcHostControl::Register> = At::new(0xb8);
pub const ROM_DMA_CONTROL: At<u16, RomDmaControl::Register> = At::new(0xba);
pub const ROM_DMA_CONTROL_BYTE: At<u8, Bits::Register> = At::new(0xba);
pub const ROM_DMA_CONTROL_HIGH: At<u8, Bits::Register> = At::new(0xbb);
pub const CLOCK_CONTROL: At<u32, ClockControl::Register> = At::new(0xd0);
pub const CLOCK_CONTROL_BYTE1: At<u8, Bits::Register> = At::new(0xd1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// All three wide I/O ranges are already in use.
    NoWideIo,
}

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
pub fn set_wide_io(fch: &impl FchHw, which: WideIo, base: u16, small: bool) {
    let lpc = fch.lpc();
    let (reg, enable, alt) = match which {
        WideIo::W0 => (
            WIDEIO_GENERIC_PORT.offset,
            IoMemDecode::WIDEIO0::SET,
            AltWideIo::WIDEIO0,
        ),
        WideIo::W1 => (
            WIDEIO_GENERIC_PORT.offset + 2,
            IoMemDecode::WIDEIO1::SET,
            AltWideIo::WIDEIO1,
        ),
        WideIo::W2 => (
            WIDEIO2_GENERIC_PORT.offset,
            IoMemDecode::WIDEIO2::SET,
            AltWideIo::WIDEIO2,
        ),
    };
    lpc.at(At::<u16>::new(reg)).set(base);
    lpc.at(ALT_WIDEIO).modify(alt.val(small.into()));
    lpc.at(IO_MEM_DECODE).modify(enable);
}

/// Decode `size` (16 or 512) bytes at `start` through the first free wide
/// I/O window.
pub fn set_wideio_range(fch: &impl FchHw, start: u16, size: u16) -> Result<(), Error> {
    let decode = fch.lpc().at(IO_MEM_DECODE).extract();
    let which = [
        (IoMemDecode::WIDEIO0, WideIo::W0),
        (IoMemDecode::WIDEIO1, WideIo::W1),
        (IoMemDecode::WIDEIO2, WideIo::W2),
    ]
    .into_iter()
    .find(|(enable, _)| !decode.is_set(*enable))
    .ok_or(Error::NoWideIo)?
    .1;
    set_wide_io(fch, which, start, size <= 16);
    Ok(())
}

/// Decode the Super I/O config ports `2Eh` and/or `4Eh`.
pub fn enable_super_io(fch: &impl FchHw, port_2e: bool, port_4e: bool) {
    fch.lpc()
        .at(IO_MEM_DECODE)
        .modify(IoMemDecode::SIO_2E.val(port_2e.into()) + IoMemDecode::SIO_4E.val(port_4e.into()));
}
/// Decode POST codes on port 0x80 to LPC.
pub fn enable_port80(fch: &impl FchHw) {
    fch.lpc().at(IO_MEM_DECODE).modify(IoMemDecode::PORT80::SET);
}
/// Add fixed legacy decodes (COM ports, KBC, ...).
pub fn enable_io_decode(
    fch: &impl FchHw,
    field: tock_registers::fields::FieldValue<u32, IoPortDecode::Register>,
) {
    fch.lpc().at(IO_PORT_DECODE).modify(field);
}

/// PEI reset task af42.
pub fn reset<H: FchHw>(fch: &H, generation: Silicon) {
    let lpc = fch.lpc();
    update::<u32, _>(&lpc, R6C.offset, 0xff, 0);
    lpc.write8(IO_MEM_DECODE.offset, 0x07);
    if generation == Silicon::Zp {
        lpc.write8(R7C.offset, 0x05);
        update::<u8, _>(&lpc, ROM_DMA_CONTROL_BYTE.offset, 0, 0xe0);
    }
    update::<u8, _>(&lpc, RA4.offset, 0, 0x01);
    // D14F3xD1: bit 5 then bit 6 (FCH::ITF::LPC::ClkCntrl[14:13] =
    // LPCCLK0/1 enable), two writes as in the donor.
    update::<u8, _>(&lpc, CLOCK_CONTROL_BYTE1.offset, 0, 0x20);
    update::<u8, _>(&lpc, CLOCK_CONTROL_BYTE1.offset, 0, 0x40);
    lpc.at(IO_PORT_DECODE).set(RESET_PORT_DECODE);
}

/// DXE env task af52 (LPC part). The ZP JEDEC-ID read and EFS rewrite of
/// af53 (FCH_SP3 §3.2) are not ported: they serve AGESA's flash-part
/// override, not the FCH.
pub fn env<H: FchHw>(fch: &H, generation: Silicon) {
    let lpc = fch.lpc();
    // D14F3x40[LegacyDmaEnable].
    update::<u8, _>(&lpc, PCI_CONTROL.offset, 0, 0x04);
    if generation == Silicon::Zp {
        lpc.write8(IO_MEM_DECODE.offset, 0x07);
    }
    // D14F3x78[1:0]: NoHog off.
    update::<u8, _>(&lpc, MISC_CONTROL.offset, 0x03, 0);
    update::<u8, _>(&lpc, ROM_DMA_CONTROL_HIGH.offset, 0, 0x0c);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::testing::{Space, World};

    #[test]
    fn wide_io_ranges_fill_in_order() {
        let world = World::default();
        let fch = world.fch();
        set_wideio_range(&fch, 0xa00, 512).unwrap();
        set_wideio_range(&fch, 0xca0, 16).unwrap();
        set_wideio_range(&fch, 0x600, 16).unwrap();
        assert_eq!(set_wideio_range(&fch, 0x700, 16), Err(Error::NoWideIo));
        let reg = |offset| world.get(Space::Lpc, offset, 4);
        assert_eq!(reg(0x64), 0x0ca0_0a00);
        assert_eq!(reg(0x90), 0x600);
        assert_eq!(reg(0x48), (1 << 2) | (1 << 24) | (1 << 25));
        assert_eq!(reg(0x74), (1 << 2) | (1 << 3));
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use crate::fch::model::fch;

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
        reset(&hw, Silicon::Ssp);
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
