//! PC/AT interrupt and ISA DMA mechanisms shared by chipset flows.
//!
//! Platforms choose vector offsets and interrupt masks. Call only during
//! serialized firmware initialization with interrupts disabled and the legacy
//! controllers decoded.

use fstart_core::pio::{inb, outb};

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
