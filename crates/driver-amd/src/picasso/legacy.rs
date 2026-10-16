// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native FCH legacy/RTC/internal IRQ/LPC policy; no PCI enumeration or SMM.
// tock generates byte-register alignment assertions using remainder by one.
#![allow(clippy::modulo_one)]
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_structs,
    registers::{ReadOnly, ReadWrite},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Register {
    EspiInterruptMask,
    SdControl,
    SdEnable,
    SdPower,
    LpcControl,
    LpcA4,
    LpcClock,
}
#[derive(Debug, Clone, Copy)]
pub enum Status {
    SmiEvent,
    SmiEnable,
}
pub trait Registers {
    fn read(&mut self, register: Register) -> u32;
    fn write(&mut self, register: Register, value: u32);
    fn status(&mut self, register: Status) -> u32;
}
pub trait Io {
    fn read8(&mut self, port: u16) -> u8;
    fn write8(&mut self, port: u16, value: u8);
}
const INTERNAL_IRQS: [(u8, u8); 18] = [
    (0, 0x10),
    (1, 0x11),
    (2, 0x12),
    (3, 0x13),
    (0x17, 0x10),
    (0x1a, 0x10),
    (0x43, 5),
    (0x62, 7),
    (0x76, 3),
    (0x77, 4),
    (0x78, 15),
    (0x79, 5),
    (0x70, 10),
    (0x71, 11),
    (0x72, 14),
    (0x73, 6),
    (0x74, 14),
    (0x75, 15),
];
fn indexed_read(io: &mut impl Io, port: u16, index: u8) -> u8 {
    io.write8(port, index);
    io.read8(port + 1)
}
fn indexed_write(io: &mut impl Io, port: u16, index: u8, value: u8) {
    io.write8(port, index);
    io.write8(port + 1, value);
}
fn indexed_update(io: &mut impl Io, port: u16, index: u8, clear: u8, set: u8) {
    let old = indexed_read(io, port, index);
    indexed_write(io, port, index, (old & !clear) | set);
}
fn update(registers: &mut impl Registers, register: Register, clear: u32, set: u32) {
    let old = registers.read(register);
    registers.write(register, (old & !clear) | set);
}
/// Disable ROM sharing BEFORE the board changes GPIO67 to the DMIC mux.
/// BYTE PCI access is intentional: adjacent control bytes are not touched.
pub fn disable_rom_sharing(registers: &mut impl Registers) {
    update(registers, Register::LpcControl, (1 << 3) | (1 << 4), 0);
}
pub fn lpc_init(registers: &mut impl Registers) {
    update(registers, Register::LpcA4, 0, 1);
    update(registers, Register::LpcClock, 0, 1 << 31);
}
/// ACPI ports/SMBus must already be decoded. These are temporary native routing
/// defaults; platform/board IRQ tables replace them after enumeration.
/// Bank0 RTC accesses follow native C and enable NMI; install exception/NMI
/// handling before this step. No calendar/checksum update is performed.
pub fn initialize(registers: &mut impl Registers, io: &mut impl Io, espi: bool, legacy_sd: bool) {
    if espi {
        update(
            registers,
            Register::EspiInterruptMask,
            (1 << 1) | (1 << 12),
            0,
        );
    }
    indexed_update(io, 0xcd6, 0x0a, 0, 1 << 3);
    indexed_update(io, 0xcd6, 0x0a, 0, 1 << 4);
    indexed_update(io, 0xcd6, 0xc0, 0, 1 << 1);
    io.write8(0xb14, 0); // SMBus autopoll off
    indexed_update(io, 0x70, 0x0b, 1, 0); // RTC daylight-saving off
    indexed_update(io, 0x70, 0x0a, 1 << 4, 0);
    indexed_update(io, 0xc00, 8, 0, 0x0a);
    indexed_update(io, 0xc00, 9, !0x99, 0x10);
    io.write8(0xb2a, 0x2f); // ASF W1C status
    if registers.status(Status::SmiEnable) & registers.status(Status::SmiEvent) != 0 {
        io.write8(0x401, 0x40);
    }
    if io.read8(0x401) & 5 != 0 {
        io.write8(0x401, 0x40);
    }
    for (index, irq) in INTERNAL_IRQS {
        indexed_write(io, 0xc00, index | 0x80, irq);
    }
    if !legacy_sd {
        update(registers, Register::SdControl, 1 << 6, 0);
        update(registers, Register::SdEnable, 1, 0);
        update(registers, Register::SdPower, 1 << 3, 0);
    }
}
register_structs! {
    Pm {
        (0 => _reserved0), (0x40 => espi: ReadWrite<u32>), (0x44 => _reserved1),
        (0xd3 => sd_control: ReadWrite<u8>), (0xd4 => _reserved2),
        (0xe8 => sd_enable: ReadWrite<u8>), (0xe9 => @END),
    },
    Smi { (0 => status: ReadOnly<u32>), (4 => enable: ReadOnly<u32>), (8 => @END), }
}
/// LPC bridge (D14F3) config registers. These go through ECAM, so they use
/// the accumulator-only `EcamDevice` accessors, never a tock overlay.
const LPC_CONTROL: u16 = 0x40;
const LPC_A4: u16 = 0xa4;
const LPC_CLOCK: u16 = 0xd0;
pub struct NativeRegisters {
    lpc: fstart_pci::EcamDevice,
}
impl NativeRegisters {
    /// # Safety
    /// ECAM is decoded and published through `fstart_pci::ecam::init`,
    /// ACPIMMIO decoded UC, exclusive BSP ownership.
    pub unsafe fn new() -> Self {
        Self {
            lpc: fstart_pci::EcamDevice::new(0, 0x14, 3),
        }
    }
    fn pm(&self) -> &Pm {
        unsafe { &*((super::ACPI_MMIO + 0x300) as *const Pm) }
    }
    fn smi(&self) -> &Smi {
        unsafe { &*((super::ACPI_MMIO + 0x200) as *const Smi) }
    }
    fn sd_power(&self) -> &ReadWrite<u8> {
        unsafe { &*((super::ACPI_MMIO + 0x1e40 + 25 * 2) as *const ReadWrite<u8>) }
    }
}
impl Registers for NativeRegisters {
    fn read(&mut self, register: Register) -> u32 {
        match register {
            Register::EspiInterruptMask => self.pm().espi.get(),
            Register::SdControl => u32::from(self.pm().sd_control.get()),
            Register::SdEnable => u32::from(self.pm().sd_enable.get()),
            Register::SdPower => u32::from(self.sd_power().get()),
            Register::LpcControl => u32::from(self.lpc.read8(LPC_CONTROL)),
            Register::LpcA4 => self.lpc.read32(LPC_A4),
            Register::LpcClock => self.lpc.read32(LPC_CLOCK),
        }
    }
    fn write(&mut self, register: Register, value: u32) {
        match register {
            Register::EspiInterruptMask => self.pm().espi.set(value),
            Register::SdControl => self.pm().sd_control.set(value as u8),
            Register::SdEnable => self.pm().sd_enable.set(value as u8),
            Register::SdPower => self.sd_power().set(value as u8),
            Register::LpcControl => self.lpc.write8(LPC_CONTROL, value as u8),
            Register::LpcA4 => self.lpc.write32(LPC_A4, value),
            Register::LpcClock => self.lpc.write32(LPC_CLOCK, value),
        }
    }
    fn status(&mut self, register: Status) -> u32 {
        match register {
            Status::SmiEvent => self.smi().status.get(),
            Status::SmiEnable => self.smi().enable.get(),
        }
    }
}
pub struct NativeIo {
    _exclusive: core::marker::PhantomData<*mut ()>,
}
impl NativeIo {
    /// # Safety
    /// Ring0 and exclusive indexed-I/O ownership; RTC bank0 may enable NMI.
    /// Exception/NMI handlers installed, SMBus/ACPI/PM routing already decoded.
    pub unsafe fn new() -> Self {
        Self {
            _exclusive: core::marker::PhantomData,
        }
    }
}
#[cfg(target_arch = "x86_64")]
impl Io for NativeIo {
    fn read8(&mut self, port: u16) -> u8 {
        let value: u8;
        unsafe {
            core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nostack));
        }
        value
    }
    fn write8(&mut self, port: u16, value: u8) {
        unsafe {
            core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nostack));
        }
    }
}
