// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin parts of the FCH SMM handler, and SMN access
//! from SMM.
use super::init::{AOAC_94, Aoac94};
use crate::fch::smi::{FchSmmHandler, FchSmmHooks};
use crate::fch::{AcpiReg, Acpimmio, Bus, Fch, IOAPIC, Reg, SmmBus, pm};
use crate::smn::SmnReg;
use core::marker::PhantomData;
use fstart_core::{
    indexed::Indexed,
    pio::{self, PioRegister},
};
use tock_registers::{
    RegisterLongName,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
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
        unsafe { sleep_entry(&Fch::new(SmmBus::without_config(), Acpimmio::DIE0), slp_typ) }
    }
    #[inline(always)]
    unsafe fn gevent(status: u32) {
        unsafe { B::gevent(status) }
    }
}

register_bitfields![u8,
    /// PMxEE.
    PmEe [SLEEP OFFSET(0) NUMBITS(2) []]
];
const PM_EE: AcpiReg<u8, PmEe::Register> = pm::at(0xee);

/// The Sx entry callbacks of the vendor FchTaishanSmmInit: PMxEE[1:0] = 3 at
/// S3/S4 entry; at S3, S4 and S5 entry AOAC 0x94[15:14] (cleared by the reset
/// tasks) get set again, pin 0x17 is asserted at the FCH IOAPIC and all its
/// redirection entries get masked. The 0xed reads are the vendor delays.
/// What AOAC 0x94 and the pin 0x17 assertion do isn't known. The S3 IRQ table
/// save and USB handshake aren't needed without S3.
#[inline(always)]
unsafe fn sleep_entry(fch: &Fch<impl Bus>, slp_typ: u8) {
    const S3: u8 = 3;
    const S4: u8 = 4;
    let delay = fch.bus().reg(Reg::<u8>::io(0xed));
    if matches!(slp_typ, S3 | S4) {
        fch.reg(PM_EE).modify(PmEe::SLEEP.val(3));
    }
    fch.reg(AOAC_94).modify(Aoac94::BITS.val(3));
    // SAFETY: the FCH IOAPIC is decoded; the OS that owned it is going away.
    let ioapic = unsafe { fstart_arch::x86::ioapic::IoApic::new(IOAPIC as usize) };
    delay.get();
    ioapic.assert_pin(0x17);
    delay.get();
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
