// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin parts of the FCH SMM handler, and SMN access
//! from SMM.
use super::{Acpimmio, Bus, Reg, Space};
use crate::fam17h::registers::SmnReg;
use crate::fch::smi::{FchSmmHandler, FchSmmHooks};
use core::marker::PhantomData;
use fstart_core::{
    indexed::Indexed,
    pio::{self, PioRegister},
};
use tock_registers::{
    RegisterLongName,
    interfaces::{Readable, Writeable},
};

/// Board parts of the Zeppelin SMM handler.
pub trait SmmBoard {
    /// The board's GEVENT SMIs, bit n =
    /// GEVENTn.
    /// # Safety
    /// Called only by the SMM handler while it owns the SMM rendezvous.
    unsafe fn gevent(_status: u32) {}
}

/// The SMM handler of a Zeppelin board `B`.
pub type Handler<B> = FchSmmHandler<Hooks<B>>;

pub struct Hooks<B>(PhantomData<B>);
impl<B: SmmBoard> FchSmmHooks for Hooks<B> {
    #[inline(always)]
    unsafe fn sleep_entry(slp_typ: u8) {
        // SAFETY: SMM owns the FCH while it puts the system to sleep.
        unsafe { sleep_entry(&mut SmmBus, slp_typ) }
    }
    #[inline(always)]
    unsafe fn gevent(status: u32) {
        unsafe { B::gevent(status) }
    }
}

/// The FCH's MMIO and I/O spaces from SMM. Config spaces aren't decoded
/// there: reads return all ones, writes are dropped.
struct SmmBus;
impl Bus for SmmBus {
    #[inline(always)]
    fn read(&mut self, space: Space, address: u32, bytes: u8) -> u32 {
        use fstart_core::mmio;
        // SAFETY: SMM owns ACPI MMIO and the legacy I/O ports.
        unsafe {
            match (space, bytes) {
                (Space::Mmio, 1) => mmio::read8(address as usize as *const u8).into(),
                (Space::Mmio, 2) => mmio::read16(address as usize as *const u16).into(),
                (Space::Mmio, _) => mmio::read32(address as usize as *const u32),
                (Space::Io, 1) => pio::inb(address as u16).into(),
                (Space::Io, 2) => pio::inw(address as u16).into(),
                (Space::Io, _) => pio::inl(address as u16),
                (Space::Lpc | Space::Smbus, _) => u32::MAX,
            }
        }
    }
    #[inline(always)]
    fn write(&mut self, space: Space, address: u32, bytes: u8, value: u32) {
        use fstart_core::mmio;
        // SAFETY: as for read.
        unsafe {
            match (space, bytes) {
                (Space::Mmio, 1) => mmio::write8(address as usize as *mut u8, value as u8),
                (Space::Mmio, 2) => mmio::write16(address as usize as *mut u16, value as u16),
                (Space::Mmio, _) => mmio::write32(address as usize as *mut u32, value),
                (Space::Io, 1) => pio::outb(address as u16, value as u8),
                (Space::Io, 2) => pio::outw(address as u16, value as u16),
                (Space::Io, _) => pio::outl(address as u16, value),
                (Space::Lpc | Space::Smbus, _) => {}
            }
        }
    }
}

const FCH_IOAPIC: usize = 0xfec0_0000;

/// The Sx entry callbacks of the vendor FchTaishanSmmInit: PMxEE[1:0] = 3 at
/// S3/S4 entry; at S3, S4 and S5 entry AOAC 0x94[15:14] (cleared by the reset
/// tasks) get set again, pin 0x17 is asserted at the FCH IOAPIC and all its
/// redirection entries get masked. The 0xed reads are the vendor delays.
/// What AOAC 0x94 and the pin 0x17 assertion do isn't known. The S3 IRQ table
/// save and USB handshake aren't needed without S3.
#[inline(always)]
unsafe fn sleep_entry(bus: &mut impl Bus, slp_typ: u8) {
    const S3: u8 = 3;
    const S4: u8 = 4;
    let delay = Reg::<u8>::io(0xed);
    if matches!(slp_typ, S3 | S4) {
        bus.update(Acpimmio::DIE0.pm::<u8, ()>(0xee), 3, 3);
    }
    bus.update(Acpimmio::DIE0.aoac::<u32, ()>(0x94), 0, 0xc000);
    // SAFETY: the FCH IOAPIC is decoded; the OS that owned it is going away.
    let ioapic = unsafe { fstart_arch::x86::ioapic::IoApic::new(FCH_IOAPIC) };
    bus.get(delay);
    ioapic.assert_pin(0x17);
    bus.get(delay);
    ioapic.mask_all();
}

/// A dword of D0F0 behind the legacy CF8/CFC pair.
type D0f0Register = Indexed<PioRegister<u32>, PioRegister<u32>>;
const fn d0f0(offset: u8) -> D0f0Register {
    Indexed::new(
        PioRegister::new(pio::PCI_CF8),
        PioRegister::new(pio::PCI_CFC),
        pio::cf8_addr(0, 0, 0, offset),
    )
}
const SMN_INDEX: D0f0Register = d0f0(0xb8);
const SMN_DATA: D0f0Register = d0f0(0xbc);

/// SMN of die 0 from SMM: its index/data pair in D0F0 config space, itself
/// behind the CF8/CFC pair. The CF8 address and SMN index of the
/// interrupted code are restored on drop, as the vendor SMI dispatcher does.
pub struct SmmSmn {
    cf8: u32,
    index: u32,
}
impl SmmSmn {
    /// # Safety
    /// In SMM, owning the SMM rendezvous.
    #[inline(always)]
    pub unsafe fn new() -> Self {
        // SAFETY: SMM owns CF8/CFC and D0F0 for the SMI's duration.
        let cf8 = unsafe { pio::inl(pio::PCI_CF8) };
        Self {
            cf8,
            index: SMN_INDEX.get(),
        }
    }
    #[inline(always)]
    pub fn reg<R: RegisterLongName>(
        &self,
        reg: SmnReg<R>,
    ) -> Indexed<D0f0Register, D0f0Register, R> {
        Indexed::new(SMN_INDEX, SMN_DATA, reg.address())
    }
}
impl Drop for SmmSmn {
    #[inline(always)]
    fn drop(&mut self) {
        SMN_INDEX.set(self.index);
        // SAFETY: as in new.
        unsafe { pio::outl(pio::PCI_CF8, self.cf8) };
    }
}
