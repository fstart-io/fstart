//! IOSF sideband: Bay Trail's message network to the SoC units.
//!
//! Three registers in the config space of 00:00.0 reach every unit: the
//! control register MCR (opcode, port, register bits 7:0, byte enables),
//! the data register MDR and the extension MCRX (register bits 31:8). A
//! read writes MCRX then MCR and reads MDR; a write loads MDR first, then
//! MCRX and MCR, which starts the transaction.
//!
//! Every unit has its own port and read opcode (the write opcode is the
//! read opcode with bit 0 set). [`IosfReg`] names a register of a port with
//! its tock bitfields, and [`Iosf::reg`] turns it into a tock register:
//!
//! ```ignore
//! iosf.reg(bunit::BMISC).modify(bunit::BMISC::DUAL_CHAN.val(0));
//! ```

use core::marker::PhantomData;
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::RegisterLongName;
use tock_registers::interfaces::{Readable, Writeable};

/// MCR: opcode, port, register and byte enables.
pub const MCR: u16 = 0xd0;
/// MDR: data.
pub const MDR: u16 = 0xd4;
/// MCRX: register bits 31:8.
pub const MCRX: u16 = 0xd8;

/// All four byte enables.
pub const BYTE_EN_ALL: u8 = 0xf;

/// An IOSF unit: its port and read opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IosfPort {
    pub port: u8,
    pub read_opcode: u8,
}

impl IosfPort {
    pub const fn new(port: u8, read_opcode: u8) -> Self {
        Self { port, read_opcode }
    }

    pub const fn write_opcode(self) -> u8 {
        self.read_opcode | 1
    }

    /// Register `reg` of this unit with the bitfields `R`.
    pub const fn reg<R: RegisterLongName>(self, reg: u32) -> IosfReg<R> {
        IosfReg {
            port: self,
            reg,
            _reg: PhantomData,
        }
    }
}

/// IO arbiter.
pub const AUNIT: IosfPort = IosfPort::new(0x00, 0x10);
/// Memory controller, channel 0.
pub const DUNIT_CH0: IosfPort = IosfPort::new(0x01, 0x10);
/// CPU bus interface controller.
pub const CPU_BUS: IosfPort = IosfPort::new(0x02, 0x10);
/// System memory arbiter.
pub const BUNIT: IosfPort = IosfPort::new(0x03, 0x10);
/// Power management unit (P-unit).
pub const PUNIT: IosfPort = IosfPort::new(0x04, 0x06);
/// Graphics.
pub const GFX: IosfPort = IosfPort::new(0x06, 0x00);
/// Memory controller, channel 1.
pub const DUNIT_CH1: IosfPort = IosfPort::new(0x07, 0x10);
pub const CUNIT: IosfPort = IosfPort::new(0x08, 0x10);
/// DDR PHY (system memory I/O).
pub const DDRIO: IosfPort = IosfPort::new(0x0c, 0x06);
/// Converged pattern generator and checker.
pub const CPGC: IosfPort = IosfPort::new(0x0d, 0x10);
/// Fuses, read by the vendor MRC for the memory-rate limit.
pub const FUSE: IosfPort = IosfPort::new(0x14, 0x10);
/// USB 2 PHY.
pub const USBPHY: IosfPort = IosfPort::new(0x43, 0x06);
/// Security engine (TXE).
pub const SEC: IosfPort = IosfPort::new(0x44, 0x04);
pub const PORT_45: IosfPort = IosfPort::new(0x45, 0x06);
pub const PORT_46: IosfPort = IosfPort::new(0x46, 0x06);
pub const PORT_47: IosfPort = IosfPort::new(0x47, 0x06);
/// South core GPIO community.
pub const SCORE: IosfPort = IosfPort::new(0x48, 0x06);
pub const PORT_55: IosfPort = IosfPort::new(0x55, 0x04);
/// LPE audio.
pub const LPE: IosfPort = IosfPort::new(0x58, 0x06);
pub const PORT_59: IosfPort = IosfPort::new(0x59, 0x06);
pub const PORT_5A: IosfPort = IosfPort::new(0x5a, 0x04);
/// USB 3 PHY.
pub const USHPHY: IosfPort = IosfPort::new(0x61, 0x06);
/// Storage control cluster.
pub const SCC: IosfPort = IosfPort::new(0x63, 0x06);
/// Low power subsystem.
pub const LPSS: IosfPort = IosfPort::new(0xa0, 0x06);
pub const PORT_A2: IosfPort = IosfPort::new(0xa2, 0x06);
/// SATA PHY.
pub const SATAPHY: IosfPort = IosfPort::new(0xa3, 0x00);
/// PCIe PHY.
pub const PCIEPHY: IosfPort = IosfPort::new(0xa6, 0x00);
/// Suspend well GPIO community.
pub const SSUS: IosfPort = IosfPort::new(0xa8, 0x10);
/// Clock control unit.
pub const CCU: IosfPort = IosfPort::new(0xa9, 0x06);

/// D-unit messages: a JEDEC command for the DRAM, and the wake message.
pub const OP_DRAM_INIT: u8 = 0x68;
pub const OP_DRAM_WAKE: u8 = 0xca;

/// SAI agent bits of the B-unit access-control registers.
pub const SAI_IA_UNTRUSTED: u32 = 1 << 0;
pub const SAI_IA_SMM: u32 = 1 << 2;
pub const SAI_IA_BOOT: u32 = 1 << 4;

/// Register `reg` of an IOSF unit, with the bitfields `R`.
pub struct IosfReg<R: RegisterLongName = ()> {
    port: IosfPort,
    reg: u32,
    _reg: PhantomData<R>,
}
impl<R: RegisterLongName> Clone for IosfReg<R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R: RegisterLongName> Copy for IosfReg<R> {}
impl<R: RegisterLongName> IosfReg<R> {
    pub const fn port(self) -> IosfPort {
        self.port
    }
    pub const fn address(self) -> u32 {
        self.reg
    }
    /// The same register `bytes` further on: an instance of a replicated
    /// block (a DDRIO module, a rank copy).
    pub const fn offset(self, bytes: u32) -> Self {
        Self {
            port: self.port,
            reg: self.reg + bytes,
            _reg: PhantomData,
        }
    }
}

/// The IOSF sideband behind 00:00.0, through a config space accessor.
///
/// A transaction is three config writes and a read (MCRX, MDR, MCR, MDR),
/// so an accessor must not be copied or shared across CPUs: neither `Copy`
/// nor `Sync`.
#[derive(Debug)]
pub struct Iosf<P> {
    pci: P,
    _not_sync: PhantomData<*const ()>,
}

/// 00:00.0, the transaction router.
fn soc() -> PciAddress {
    PciAddress::new(0, 0, 0, 0)
}

const fn mcr(opcode: u8, port: u8, reg: u32, byte_enable: u8) -> u32 {
    (opcode as u32) << 24 | (port as u32) << 16 | (reg & 0xff) << 8 | (byte_enable as u32) << 4
}

impl<P: ConfigRegionAccess> Iosf<P> {
    /// # Safety
    /// 00:00.0 is the Bay Trail transaction router and its MCR/MDR/MCRX
    /// triple is exclusively owned while the value is used (BSP during
    /// boot). Every access reaches a SoC unit with its side effects.
    pub const unsafe fn new(pci: P) -> Self {
        Self {
            pci,
            _not_sync: PhantomData,
        }
    }

    /// `reg` as a tock register.
    pub fn reg<R: RegisterLongName>(&self, reg: IosfReg<R>) -> IosfRegister<'_, P, R> {
        IosfRegister {
            iosf: self,
            reg: reg.reg,
            port: reg.port,
            _reg: PhantomData,
        }
    }

    /// Read the dword `reg` of `port`, for vendor tables and
    /// address-computed registers.
    pub fn read(&self, port: IosfPort, reg: u32) -> u32 {
        // SAFETY: the constructor's contract.
        unsafe {
            self.pci.write(soc(), MCRX, reg & !0xff);
            self.pci.write(
                soc(),
                MCR,
                mcr(port.read_opcode, port.port, reg, BYTE_EN_ALL),
            );
            self.pci.read(soc(), MDR)
        }
    }

    pub fn write(&self, port: IosfPort, reg: u32, value: u32) {
        self.write_bytes(port, reg, value, BYTE_EN_ALL);
    }

    /// Write with explicit byte enables.
    pub fn write_bytes(&self, port: IosfPort, reg: u32, value: u32, byte_enable: u8) {
        self.send(port.write_opcode(), port.port, reg, value, byte_enable);
    }

    /// `reg = (reg & !clear) | set`.
    pub fn modify(&self, port: IosfPort, reg: u32, clear: u32, set: u32) {
        self.write(port, reg, (self.read(port, reg) & !clear) | set);
    }

    /// Send a message with an opcode of its own (the D-unit JEDEC command
    /// and wake messages).
    pub fn message(&self, port: IosfPort, opcode: u8, data: u32) {
        self.send(opcode, port.port, 0, data, BYTE_EN_ALL);
    }

    fn send(&self, opcode: u8, port: u8, reg: u32, value: u32, byte_enable: u8) {
        // SAFETY: the constructor's contract.
        unsafe {
            self.pci.write(soc(), MDR, value);
            self.pci.write(soc(), MCRX, reg & !0xff);
            self.pci
                .write(soc(), MCR, mcr(opcode, port, reg, byte_enable));
        }
    }
}

/// An IOSF register as a tock register.
pub struct IosfRegister<'a, P, R: RegisterLongName> {
    iosf: &'a Iosf<P>,
    port: IosfPort,
    reg: u32,
    _reg: PhantomData<R>,
}

impl<P: ConfigRegionAccess, R: RegisterLongName> Readable for IosfRegister<'_, P, R> {
    type T = u32;
    type R = R;

    #[inline]
    fn get(&self) -> u32 {
        self.iosf.read(self.port, self.reg)
    }
}

impl<P: ConfigRegionAccess, R: RegisterLongName> Writeable for IosfRegister<'_, P, R> {
    type T = u32;
    type R = R;

    #[inline]
    fn set(&self, value: u32) {
        self.iosf.write(self.port, self.reg, value)
    }
}

/// The SoC's IOSF through ECAM, once the B-unit decodes it.
///
/// # Safety
/// As [`Iosf::new`]: only the BSP's single-threaded boot flow uses the
/// sideband, and no other accessor is mid-transaction.
pub(crate) unsafe fn ecam() -> Iosf<fstart_pci::ecam::GlobalEcam> {
    // SAFETY: the caller's.
    unsafe { Iosf::new(fstart_pci::ecam::GlobalEcam) }
}

#[cfg(test)]
pub(crate) mod model {
    //! A model of the sideband behind MCR/MDR/MCRX, logging every
    //! completed transaction.
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use std::collections::BTreeMap;
    use std::vec::Vec;

    #[derive(Default)]
    pub struct Sideband {
        state: RefCell<State>,
    }

    #[derive(Default)]
    struct State {
        mdr: u32,
        mcrx: u32,
        regs: BTreeMap<(u8, u32), u32>,
        /// Bits the hardware clears once the operation they start is done.
        self_clearing: BTreeMap<(u8, u32), u32>,
        log: Vec<(u8, u8, u32, u32)>,
    }

    impl Sideband {
        pub fn get(&self, port: IosfPort, reg: u32) -> u32 {
            *self
                .state
                .borrow()
                .regs
                .get(&(port.port, reg))
                .unwrap_or(&0)
        }
        pub fn set(&self, port: IosfPort, reg: u32, value: u32) {
            self.state.borrow_mut().regs.insert((port.port, reg), value);
        }
        /// Make `mask` in `reg` read back clear after every write, like a
        /// start bit the hardware drops when done.
        pub fn self_clearing(&self, port: IosfPort, reg: u32, mask: u32) {
            self.state
                .borrow_mut()
                .self_clearing
                .insert((port.port, reg), mask);
        }
        /// Completed `(opcode, port, register, data)` transactions.
        pub fn log(&self) -> Vec<(u8, u8, u32, u32)> {
            self.state.borrow().log.clone()
        }
        pub fn iosf(&self) -> Iosf<&Self> {
            unsafe { Iosf::new(self) }
        }
    }

    impl ConfigRegionAccess for Sideband {
        unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
            assert_eq!((address, offset), (soc(), MDR));
            self.state.borrow().mdr
        }
        unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
            assert_eq!(address, soc());
            let mut s = self.state.borrow_mut();
            match offset {
                MDR => s.mdr = value,
                MCRX => s.mcrx = value,
                MCR => {
                    let opcode = (value >> 24) as u8;
                    let port = (value >> 16) as u8;
                    let reg = s.mcrx | ((value >> 8) & 0xff);
                    if opcode & 1 == 0 && opcode >= 0x04 && opcode <= 0x10 || opcode == 0 {
                        s.mdr = *s.regs.get(&(port, reg)).unwrap_or(&0);
                    } else {
                        let data = s.mdr;
                        if opcode & 1 == 1 {
                            let clear = *s.self_clearing.get(&(port, reg)).unwrap_or(&0);
                            s.regs.insert((port, reg), data & !clear);
                        }
                        s.log.push((opcode, port, reg, data));
                    }
                }
                _ => panic!("not an IOSF register: {offset:#x}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::model::Sideband;
    use super::*;
    use tock_registers::interfaces::ReadWriteable;
    use tock_registers::register_bitfields;

    register_bitfields![u32, T [LOW OFFSET(0) NUMBITS(4) [], HIGH OFFSET(28) NUMBITS(4) []]];

    #[test]
    fn control_word_packs_opcode_port_register_and_byte_enables() {
        assert_eq!(mcr(0x11, 0x03, 0x2b, BYTE_EN_ALL), 0x1103_2bf0);
        assert_eq!(mcr(0x07, 0x04, 0x105, 1), 0x0704_0510);
    }

    #[test]
    fn typed_register_round_trips_through_the_extension_register() {
        let sideband = Sideband::default();
        let iosf = sideband.iosf();
        let reg = iosf.reg(DDRIO.reg::<T::Register>(0x7880));
        reg.write(T::LOW.val(5));
        reg.modify(T::HIGH.val(0xa));
        assert_eq!(sideband.get(DDRIO, 0x7880), 0xa000_0005);
        assert_eq!(
            sideband.log().last(),
            Some(&(0x07, 0x0c, 0x7880, 0xa000_0005))
        );
    }
}
