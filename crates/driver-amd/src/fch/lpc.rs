// SPDX-License-Identifier: MIT OR Apache-2.0
//! FCH LPC bridge (D14F3) registers and I/O decode. Common to every Family 17h FCH.
use crate::fch::{Bus, BusRegister, Reg};
use tock_registers::{
    RegisterLongName,
    interfaces::{ReadWriteable, Readable},
    register_bitfields,
};

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
    /// D14F3x48: I/O and memory range decodes.
    pub IoMemDecode [
        SIO_2E OFFSET(0) NUMBITS(1) [],
        SIO_4E OFFSET(1) NUMBITS(1) [],
        WIDEIO0 OFFSET(2) NUMBITS(1) [],
        SYNC_TIMEOUT_EN OFFSET(7) NUMBITS(1) [],
        SYNC_TIMEOUT_COUNT OFFSET(8) NUMBITS(8) [],
        PORT80 OFFSET(21) NUMBITS(1) [],
        WIDEIO1 OFFSET(24) NUMBITS(1) [],
        WIDEIO2 OFFSET(25) NUMBITS(1) []
    ],
    /// D14F3x64 (ranges 0 and 1) and D14F3x90 (range 2).
    pub WideIo [LOW OFFSET(0) NUMBITS(16) [], HIGH OFFSET(16) NUMBITS(16) []],
    /// D14F3x74: wide I/O ranges of 16 bytes instead of 512.
    pub AltWideIo [
        WIDEIO0 OFFSET(0) NUMBITS(1) [],
        WIDEIO1 OFFSET(2) NUMBITS(1) [],
        WIDEIO2 OFFSET(3) NUMBITS(1) []
    ],
    /// D14F3xA0: the SPI/eSPI controller BAR.
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
    pub RomDmaControl [BIT7 OFFSET(7) NUMBITS(1) [], BIT8 OFFSET(8) NUMBITS(1) []]
];
register_bitfields![u8,
    /// D14F3x40.
    pub PciControl [
        LEGACY_DMA_EN OFFSET(2) NUMBITS(1) [],
        VW_ROM_SHARING_EN OFFSET(3) NUMBITS(1) [],
        EXT_ROM_SHARING_EN OFFSET(4) NUMBITS(1) []
    ],
    /// The vendor's bits of single bytes: D14F3x78, 0x7C, 0xBA, 0xBB, 0xD1.
    pub Bits [B0 OFFSET(0) NUMBITS(1) [], B1 OFFSET(1) NUMBITS(1) [], B2 OFFSET(2) NUMBITS(1) [],
        B3 OFFSET(3) NUMBITS(1) [], B5 OFFSET(5) NUMBITS(1) [], B6 OFFSET(6) NUMBITS(1) []]
];

pub const ID: Reg<u32, Id::Register> = Reg::lpc(0x00);
pub const PCI_CONTROL: Reg<u8, PciControl::Register> = Reg::lpc(0x40);
pub const IO_PORT_DECODE: Reg<u32, IoPortDecode::Register> = Reg::lpc(0x44);
pub const IO_MEM_DECODE: Reg<u32, IoMemDecode::Register> = Reg::lpc(0x48);
pub const MEM_PORT1: Reg<u32> = Reg::lpc(0x4c);
pub const MEM_PORT0: Reg<u32> = Reg::lpc(0x60);
pub const WIDEIO_GENERIC_PORT: Reg<u32, WideIo::Register> = Reg::lpc(0x64);
pub const R6C: Reg<u8> = Reg::lpc(0x6c);
pub const ALT_WIDEIO: Reg<u32, AltWideIo::Register> = Reg::lpc(0x74);
pub const MISC_CONTROL: Reg<u8, Bits::Register> = Reg::lpc(0x78);
pub const R7C: Reg<u8, Bits::Register> = Reg::lpc(0x7c);
pub const WIDEIO2_GENERIC_PORT: Reg<u32, WideIo::Register> = Reg::lpc(0x90);
pub const SPI_BASE_ADDRESS: Reg<u32, SpiBase::Register> = Reg::lpc(0xa0);
pub const RA4: Reg<u32, LpcA4::Register> = Reg::lpc(0xa4);
pub const RA4_BYTE: Reg<u8, Bits::Register> = Reg::lpc(0xa4);
pub const ROM_DMA_EC_HOST_CONTROL: Reg<u32, RomDmaEcHostControl::Register> = Reg::lpc(0xb8);
pub const ROM_DMA_CONTROL: Reg<u16, RomDmaControl::Register> = Reg::lpc(0xba);
pub const ROM_DMA_CONTROL_BYTE: Reg<u8, Bits::Register> = Reg::lpc(0xba);
pub const ROM_DMA_CONTROL_HIGH: Reg<u8, Bits::Register> = Reg::lpc(0xbb);
pub const CLOCK_CONTROL: Reg<u32, ClockControl::Register> = Reg::lpc(0xd0);
pub const CLOCK_CONTROL_BYTE1: Reg<u8, Bits::Register> = Reg::lpc(0xd1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// All three wide I/O ranges are already in use.
    NoWideIo,
}

/// The LPC bridge function.
pub struct Lpc<B> {
    bus: B,
}
impl<B: Bus> Lpc<B> {
    /// # Safety
    /// Exclusive access to the LPC bridge decode registers.
    pub unsafe fn new(bus: B) -> Self {
        Self { bus }
    }
    pub fn reg<T: crate::fch::Value, R: RegisterLongName>(
        &self,
        reg: Reg<T, R>,
    ) -> BusRegister<'_, B, T, R> {
        self.bus.reg(reg)
    }
    /// Decode the Super I/O config ports 0x2e/0x2f.
    pub fn enable_sio_2e(&self) {
        self.reg(IO_MEM_DECODE).modify(IoMemDecode::SIO_2E::SET);
    }
    /// Decode POST codes on port 0x80 to LPC.
    pub fn enable_port80(&self) {
        self.reg(IO_MEM_DECODE).modify(IoMemDecode::PORT80::SET);
    }
    /// Add fixed legacy decodes (COM ports, KBC, ...).
    pub fn enable_io_decode(
        &self,
        field: tock_registers::fields::FieldValue<u32, IoPortDecode::Register>,
    ) {
        self.reg(IO_PORT_DECODE).modify(field);
    }
    /// Decode `size` (16 or 512) bytes at `start` through a free wide I/O range.
    pub fn set_wideio_range(&self, start: u16, size: u16) -> Result<(), Error> {
        let decode = self.reg(IO_MEM_DECODE).extract();
        let ranges = [
            (
                IoMemDecode::WIDEIO0,
                WIDEIO_GENERIC_PORT,
                WideIo::LOW,
                AltWideIo::WIDEIO0,
            ),
            (
                IoMemDecode::WIDEIO1,
                WIDEIO_GENERIC_PORT,
                WideIo::HIGH,
                AltWideIo::WIDEIO1,
            ),
            (
                IoMemDecode::WIDEIO2,
                WIDEIO2_GENERIC_PORT,
                WideIo::LOW,
                AltWideIo::WIDEIO2,
            ),
        ];
        let &(enable, base, half, alt) = ranges
            .iter()
            .find(|(enable, ..)| !decode.is_set(*enable))
            .ok_or(Error::NoWideIo)?;
        self.reg(base).modify(half.val(start.into()));
        self.reg(IO_MEM_DECODE).modify(enable.val(1));
        self.reg(ALT_WIDEIO).modify(alt.val((size <= 16).into()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::{Space, testing::World};

    #[test]
    fn wide_io_ranges_fill_in_order() {
        let world = World::default();
        let lpc = unsafe { Lpc::new(&world) };
        lpc.set_wideio_range(0xa00, 512).unwrap();
        lpc.set_wideio_range(0xca0, 16).unwrap();
        lpc.set_wideio_range(0x600, 16).unwrap();
        assert_eq!(lpc.set_wideio_range(0x700, 16), Err(Error::NoWideIo));
        let reg = |offset| world.get(Space::Lpc, offset, 4);
        assert_eq!(reg(0x64), 0x0ca0_0a00);
        assert_eq!(reg(0x90), 0x600);
        assert_eq!(reg(0x48), (1 << 2) | (1 << 24) | (1 << 25));
        assert_eq!(reg(0x74), (1 << 2) | (1 << 3));
    }
}
