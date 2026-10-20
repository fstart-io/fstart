// SPDX-License-Identifier: MIT OR Apache-2.0
//! FCH LPC bridge (D14F3) I/O decode. Common to every Family 17h FCH.
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::{LocalRegisterCopy, register_bitfields};

/// D14F3.
pub fn lpc_address() -> PciAddress {
    PciAddress::new(0, 0, 0x14, 3)
}

register_bitfields![u32,
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
        PORT80 OFFSET(21) NUMBITS(1) [],
        WIDEIO1 OFFSET(24) NUMBITS(1) [],
        WIDEIO2 OFFSET(25) NUMBITS(1) []
    ],
    /// D14F3x74: wide I/O ranges of 16 bytes instead of 512.
    pub AltWideIo [
        WIDEIO0 OFFSET(0) NUMBITS(1) [],
        WIDEIO1 OFFSET(2) NUMBITS(1) [],
        WIDEIO2 OFFSET(3) NUMBITS(1) []
    ]
];
const IO_PORT_DECODE: u16 = 0x44;
const IO_MEM_DECODE: u16 = 0x48;
const ALT_WIDEIO: u16 = 0x74;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// All three wide I/O ranges are already in use.
    NoWideIo,
}

/// The LPC bridge function, through any config-space transport.
pub struct Lpc<P> {
    pci: P,
}
impl<P: ConfigRegionAccess> Lpc<P> {
    /// # Safety
    /// Exclusive access to the LPC bridge decode registers.
    pub unsafe fn new(pci: P) -> Self {
        Self { pci }
    }
    fn read<R: tock_registers::RegisterLongName>(&self, reg: u16) -> LocalRegisterCopy<u32, R> {
        LocalRegisterCopy::new(unsafe { self.pci.read(lpc_address(), reg) })
    }
    fn write(&mut self, reg: u16, value: u32) {
        unsafe { self.pci.write(lpc_address(), reg, value) }
    }
    fn modify_decode(&mut self, field: tock_registers::fields::FieldValue<u32, IoMemDecode::Register>) {
        let mut value = self.read::<IoMemDecode::Register>(IO_MEM_DECODE);
        value.modify(field);
        self.write(IO_MEM_DECODE, value.get());
    }
    /// Decode the Super I/O config ports 0x2e/0x2f.
    pub fn enable_sio_2e(&mut self) {
        self.modify_decode(IoMemDecode::SIO_2E::SET);
    }
    pub fn enable_port80(&mut self) {
        self.modify_decode(IoMemDecode::PORT80::SET);
    }
    /// Add fixed legacy decodes (COM ports, KBC, ...).
    pub fn enable_io_decode(&mut self, field: tock_registers::fields::FieldValue<u32, IoPortDecode::Register>) {
        let mut value = self.read::<IoPortDecode::Register>(IO_PORT_DECODE);
        value.modify(field);
        self.write(IO_PORT_DECODE, value.get());
    }
    /// Decode `size` (16 or 512) bytes at `start` through a free wide I/O range.
    pub fn set_wideio_range(&mut self, start: u16, size: u16) -> Result<(), Error> {
        let decode = self.read::<IoMemDecode::Register>(IO_MEM_DECODE);
        let ranges = [
            (IoMemDecode::WIDEIO0, 0x64, 0, AltWideIo::WIDEIO0),
            (IoMemDecode::WIDEIO1, 0x64, 16, AltWideIo::WIDEIO1),
            (IoMemDecode::WIDEIO2, 0x90, 0, AltWideIo::WIDEIO2),
        ];
        let &(enable, reg, shift, alt) = ranges
            .iter()
            .find(|(enable, ..)| !decode.is_set(*enable))
            .ok_or(Error::NoWideIo)?;
        let base = unsafe { self.pci.read(lpc_address(), reg) };
        self.write(reg, (base & !(0xffff << shift)) | (u32::from(start) << shift));
        self.modify_decode(enable.val(1));
        let mut alternate = self.read::<AltWideIo::Register>(ALT_WIDEIO & !3);
        alternate.modify(if size <= 16 { alt.val(1) } else { alt.val(0) });
        self.write(ALT_WIDEIO & !3, alternate.get());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    struct Config(RefCell<[u32; 64]>);
    impl ConfigRegionAccess for &Config {
        unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
            assert_eq!(address, lpc_address());
            self.0.borrow()[usize::from(offset / 4)]
        }
        unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
            assert_eq!(address, lpc_address());
            self.0.borrow_mut()[usize::from(offset / 4)] = value;
        }
    }
    #[test]
    fn wide_io_ranges_fill_in_order() {
        let config = Config(RefCell::new([0; 64]));
        let mut lpc = unsafe { Lpc::new(&config) };
        lpc.set_wideio_range(0xa00, 512).unwrap();
        lpc.set_wideio_range(0xca0, 16).unwrap();
        lpc.set_wideio_range(0x600, 16).unwrap();
        assert_eq!(lpc.set_wideio_range(0x700, 16), Err(Error::NoWideIo));
        let regs = config.0.borrow();
        assert_eq!(regs[0x64 / 4], 0x0ca0_0a00);
        assert_eq!(regs[0x90 / 4], 0x600);
        assert_eq!(regs[0x48 / 4], (1 << 2) | (1 << 24) | (1 << 25));
        assert_eq!(regs[0x74 / 4], (1 << 2) | (1 << 3));
    }
}
