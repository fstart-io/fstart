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

    /// The 8-bit ID field; the APIC bus only decodes its low 4 bits.
    pub fn set_id(&self, id: u8) {
        self.reg::<Id::Register>(ID).modify(Id::ID.val(id.into()));
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
