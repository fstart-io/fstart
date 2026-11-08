//! I/O APIC register interface: ID, masking
//! and the virtual-wire route of the GSI0 IOAPIC. The registers sit behind
//! the select/window pair.

use fstart_core::{indexed::Indexed, mmio::MmioRegister};
use tock_registers::{
    RegisterLongName,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    Id [ID OFFSET(24) NUMBITS(8) []],
    Version [MAX_REDIRECTION OFFSET(16) NUMBITS(8) []],
    RedirectionLow [
        DELIVERY OFFSET(8) NUMBITS(3) [ExtInt = 7],
        MASKED OFFSET(16) NUMBITS(1) []
    ],
    RedirectionHigh [DESTINATION OFFSET(24) NUMBITS(8) []]
];

const SELECT: usize = 0x00;
const WINDOW: usize = 0x10;
/// The IRQ pin assertion register of chipset IOAPICs.
const PIN_ASSERTION: usize = 0x20;

const ID: u32 = 0x00;
const VERSION: u32 = 0x01;
const fn redirection(pin: u32) -> u32 {
    0x10 + 2 * pin
}

type Register<R> = Indexed<MmioRegister<u32>, MmioRegister<u32>, R>;

pub struct IoApic {
    base: usize,
}

impl IoApic {
    /// # Safety
    /// `base` is a decoded, UC-mapped IOAPIC; the caller owns it exclusively.
    pub unsafe fn new(base: usize) -> Self {
        Self { base }
    }

    fn mmio(&self, offset: usize) -> MmioRegister<u32> {
        // SAFETY: the constructor's contract.
        unsafe { MmioRegister::new(self.base + offset) }
    }

    fn reg<R: RegisterLongName>(&self, register: u32) -> Register<R> {
        Indexed::new(self.mmio(SELECT), self.mmio(WINDOW), register)
    }

    /// Read the programmed ID without changing routing/redirection state.
    pub fn id(&self) -> u8 {
        self.reg::<Id::Register>(ID).read(Id::ID) as u8
    }

    /// The 8-bit ID field; the APIC bus only decodes its low 4 bits.
    pub fn set_id(&self, id: u8) {
        self.reg::<Id::Register>(ID).modify(Id::ID.val(id.into()));
    }

    /// Rewrite the write-once maximum-redirection field with its current
    /// value, locking the full table size (coreboot
    /// `ioapic_lock_max_vectors()`).
    pub fn lock_redirection_entries(&self) {
        let version = self.reg::<Version::Register>(VERSION);
        version.set(version.get());
    }

    pub fn redirection_entries(&self) -> u32 {
        self.reg::<Version::Register>(VERSION)
            .read(Version::MAX_REDIRECTION)
            + 1
    }

    /// Mask every redirection entry.
    pub fn mask_all(&self) {
        for pin in 0..self.redirection_entries() {
            self.reg::<RedirectionLow::Register>(redirection(pin))
                .write(RedirectionLow::MASKED::SET);
            self.reg::<RedirectionHigh::Register>(redirection(pin) + 1)
                .set(0);
        }
    }

    /// Read back that every redirection entry is masked.
    pub fn all_masked(&self) -> bool {
        (0..self.redirection_entries()).all(|pin| {
            self.reg::<RedirectionLow::Register>(redirection(pin))
                .is_set(RedirectionLow::MASKED)
        })
    }

    /// Assert `pin` through the IRQ pin assertion register of chipset
    /// IOAPICs.
    pub fn assert_pin(&self, pin: u8) {
        self.mmio(PIN_ASSERTION).set(pin.into());
    }

    /// Route the legacy 8259 output (pin 0) to `bsp_apic_id` as ExtINT, the
    /// virtual-wire mode of the GSI0 IOAPIC.
    pub fn route_i8259(&self, bsp_apic_id: u8) {
        self.reg::<RedirectionLow::Register>(redirection(0))
            .write(RedirectionLow::DELIVERY::ExtInt);
        self.reg::<RedirectionHigh::Register>(redirection(0) + 1)
            .write(RedirectionHigh::DESTINATION.val(bsp_apic_id.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_probe_only_writes_the_index_not_routing_data() {
        let mut words = [0u32; 5];
        words[4] = 0x1234_0000;
        // SAFETY: modeled select/window in aligned, exclusively owned RAM.
        let apic = unsafe { IoApic::new(words.as_mut_ptr() as usize) };
        assert_eq!(apic.id(), 0x12);
        assert_eq!(words[0], 0);
        assert_eq!(apic.redirection_entries(), 0x35);
        assert_eq!(words[0], 1);
        assert_eq!(words[4], 0x1234_0000);
        assert!(!apic.all_masked());
        // SAFETY: the modeled window register, written as the IOAPIC would.
        unsafe { words.as_mut_ptr().add(4).write_volatile(0x0001_0000) };
        assert!(apic.all_masked());
    }
}
