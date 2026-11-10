//! PC/AT interrupt, ISA DMA and legacy ACPI sleep mechanisms.
//!
//! Platforms choose vector offsets and interrupt masks. Call only during
//! serialized firmware initialization with interrupts disabled and the legacy
//! controllers decoded.

use fstart_core::description::AcpiSleepControl;
use fstart_core::pio::{PioRegister, inb, outb};
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u16,
    Pm1Control [
        SLEEP_TYPE OFFSET(10) NUMBITS(3) [],
        SLEEP_ENABLE OFFSET(13) NUMBITS(1) []
    ]
];

fn sleep_control(current: u16, sleep_type: u8, enable: bool) -> u16 {
    let mut value = LocalRegisterCopy::<u16, Pm1Control::Register>::new(current);
    value.modify(
        Pm1Control::SLEEP_TYPE.val(u16::from(sleep_type))
            + Pm1Control::SLEEP_ENABLE.val(u16::from(enable)),
    );
    value.get()
}

/// Enter the platform's S5 state, then halt if it returns.
/// An unsupported shutdown never guesses a port or falls back to rebooting.
/// The platform owns and has decoded the supplied PM1 I/O blocks.
pub fn enter_acpi_s5(controls: &[AcpiSleepControl]) -> ! {
    // SAFETY: a firmware reset/shutdown request will not return to its caller.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
    // ACPI PM1a/PM1b: program both sleep types before asserting either SLP_EN.
    for control in controls {
        let reg = PioRegister::<u16, Pm1Control::Register>::new(control.port());
        reg.set(sleep_control(reg.get(), control.sleep_type(), false));
    }
    for control in controls {
        let reg = PioRegister::<u16, Pm1Control::Register>::new(control.port());
        reg.modify(Pm1Control::SLEEP_ENABLE::SET);
    }
    loop {
        // SAFETY: shutdown has been requested; an SMI may return, but not reset us.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

#[cfg(test)]
mod sleep_tests {
    use super::*;

    #[test]
    fn platform_sleep_types_preserve_unrelated_control_bits() {
        for (port, sleep_type, expected) in [(0x604, 0, 0), (0x504, 7, 0x1c00), (0x804, 5, 0x1400)]
        {
            let control = AcpiSleepControl::new(port, sleep_type);
            assert_eq!(control.port(), port);
            let current = 0xc3ff | 0x3c00;
            let prepared = sleep_control(current, control.sleep_type(), false);
            assert_eq!(prepared, 0xc3ff | expected);
            assert_eq!(sleep_control(prepared, sleep_type, true), prepared | 0x2000);
        }
    }
}

pub const PIC_MASTER_MASK: u16 = 0x21;
pub const PIC_SLAVE_MASK: u16 = 0xa1;
const PIC_MASTER_COMMAND: u16 = 0x20;
const PIC_SLAVE_COMMAND: u16 = 0xa0;
const PIC_INIT_WITH_ICW4: u8 = 0x11;
const PIC_8086_MODE: u8 = 1;
const CASCADE_IRQ: u8 = 2;

/// Initialize dual 8259 PICs without changing the platform's mask policy.
pub fn initialize_pic(master_vector: u8, slave_vector: u8) {
    // SAFETY: caller is in the serialized legacy-controller init phase.
    unsafe {
        outb(PIC_MASTER_COMMAND, PIC_INIT_WITH_ICW4);
        outb(PIC_SLAVE_COMMAND, PIC_INIT_WITH_ICW4);
        outb(PIC_MASTER_MASK, master_vector);
        outb(PIC_SLAVE_MASK, slave_vector);
        outb(PIC_MASTER_MASK, 1 << CASCADE_IRQ);
        outb(PIC_SLAVE_MASK, CASCADE_IRQ);
        outb(PIC_MASTER_MASK, PIC_8086_MODE);
        outb(PIC_SLAVE_MASK, PIC_8086_MODE);
    }
}

/// Reset both 8237 controllers, retaining channel 4 as their cascade.
pub fn initialize_isa_dma() {
    const DMA1_MASTER_CLEAR: u16 = 0x0d;
    const DMA1_MODE: u16 = 0x0b;
    const DMA1_ALL_MASK: u16 = 0x0f;
    const DMA2_MASTER_CLEAR: u16 = 0xda;
    const DMA2_MODE: u16 = 0xd6;
    const DMA2_CHANNEL_MASK: u16 = 0xd4;
    const SINGLE_TRANSFER: u8 = 1 << 6;
    const CASCADE: u8 = 3 << 6;
    // SAFETY: caller is in the serialized legacy-controller init phase.
    unsafe {
        outb(DMA1_MASTER_CLEAR, 0);
        for channel in 0..4 {
            outb(DMA1_MODE, SINGLE_TRANSFER | channel);
        }
        outb(DMA2_MASTER_CLEAR, 0);
        outb(DMA2_MODE, CASCADE);
        for channel in 1..4 {
            outb(DMA2_MODE, SINGLE_TRANSFER | channel);
        }
        outb(DMA2_CHANNEL_MASK, 0); // Unmask channel 4 (cascade).
        outb(DMA1_ALL_MASK, 0x0f);
        // Retain the existing POST-port read delay, not a dummy write.
        let _ = inb(0x80);
    }
}
