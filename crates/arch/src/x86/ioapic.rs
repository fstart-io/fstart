//! I/O APIC register interface: ID, masking
//! and the virtual-wire route of the GSI0 IOAPIC.

use tock_registers::{
    LocalRegisterCopy,
    interfaces::{Readable, Writeable},
    register_bitfields, register_structs,
    registers::ReadWrite,
};

register_structs! {
    Regs {
        (0x00 => select: ReadWrite<u32>),
        (0x04 => _reserved),
        (0x10 => window: ReadWrite<u32>),
        (0x14 => @END),
    }
}

register_bitfields![u32,
    Id [ID OFFSET(24) NUMBITS(8) []],
    Version [MAX_REDIRECTION OFFSET(16) NUMBITS(8) []],
    RedirectionLow [
        DELIVERY OFFSET(8) NUMBITS(3) [ExtInt = 7],
        MASKED OFFSET(16) NUMBITS(1) []
    ]
];

const ID: u32 = 0x00;
const VERSION: u32 = 0x01;
const fn redirection(pin: u32) -> u32 {
    0x10 + 2 * pin
}

pub struct IoApic {
    regs: &'static Regs,
}

impl IoApic {
    /// # Safety
    /// `base` is a decoded, UC-mapped IOAPIC; the caller owns it exclusively.
    pub unsafe fn new(base: usize) -> Self {
        Self {
            regs: unsafe { &*(base as *const Regs) },
        }
    }

    fn read(&self, register: u32) -> u32 {
        self.regs.select.set(register);
        self.regs.window.get()
    }

    fn write(&self, register: u32, value: u32) {
        self.regs.select.set(register);
        self.regs.window.set(value);
    }

    /// The 8-bit ID field; the APIC bus only decodes its low 4 bits.
    pub fn set_id(&self, id: u8) {
        let mut value = LocalRegisterCopy::<u32, Id::Register>::new(self.read(ID));
        value.modify(Id::ID.val(id.into()));
        self.write(ID, value.get());
    }

    pub fn redirection_entries(&self) -> u32 {
        LocalRegisterCopy::<u32, Version::Register>::new(self.read(VERSION))
            .read(Version::MAX_REDIRECTION)
            + 1
    }

    /// Mask every redirection entry.
    pub fn mask_all(&self) {
        for pin in 0..self.redirection_entries() {
            self.write(redirection(pin), RedirectionLow::MASKED::SET.value);
            self.write(redirection(pin) + 1, 0);
        }
    }

    /// Route the legacy 8259 output (pin 0) to `bsp_apic_id` as ExtINT, the
    /// virtual-wire mode of the GSI0 IOAPIC.
    pub fn route_i8259(&self, bsp_apic_id: u8) {
        self.write(redirection(0), RedirectionLow::DELIVERY::ExtInt.value);
        self.write(redirection(0) + 1, u32::from(bsp_apic_id) << 24);
    }
}
