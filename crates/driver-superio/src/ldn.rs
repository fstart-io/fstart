//! Common indexed PnP logical-device resources.
//!
//! Callers own chip-specific configuration entry/exit and serialize access to
//! the index/data ports. Do not select another LDN through a separate accessor
//! while programming a selected device.

/// Standard PnP I/O address pairs (0x60 through 0x69), carrying their bases.
#[derive(Clone, Copy)]
pub enum IoResource {
    Primary(u16),
    Secondary(u16),
    Third(u16),
    Fourth(u16),
    Fifth(u16),
}

/// Standard PnP interrupt slots, carrying their IRQ numbers.
#[derive(Clone, Copy)]
pub enum IrqResource {
    Primary(u8),
    Secondary(u8),
}

/// Standard PnP DMA-request slots, carrying their channel numbers.
#[derive(Clone, Copy)]
pub enum DmaResource {
    Primary(u8),
    Secondary(u8),
}

/// A selected logical device using the caller's indexed-register transport.
pub struct LogicalDevice<R: FnMut(u8) -> u8, W: FnMut(u8, u8)> {
    read: R,
    write: W,
}

impl<R: FnMut(u8) -> u8, W: FnMut(u8, u8)> LogicalDevice<R, W> {
    pub fn select(ldn: u8, read: R, mut write: W) -> Self {
        write(0x07, ldn);
        Self { read, write }
    }

    /// Write a chip-specific register without changing the selected LDN.
    pub fn write(&mut self, reg: u8, value: u8) {
        (self.write)(reg, value);
    }

    pub fn set_io_base(&mut self, resource: IoResource) {
        let (high, address) = match resource {
            IoResource::Primary(base) => (0x60, base),
            IoResource::Secondary(base) => (0x62, base),
            IoResource::Third(base) => (0x64, base),
            IoResource::Fourth(base) => (0x66, base),
            IoResource::Fifth(base) => (0x68, base),
        };
        let [hi, lo] = address.to_be_bytes();
        self.write(high, hi);
        self.write(high + 1, lo);
    }

    pub fn set_irq(&mut self, resource: IrqResource) {
        let (register, irq) = match resource {
            IrqResource::Primary(irq) => (0x70, irq),
            IrqResource::Secondary(irq) => (0x72, irq),
        };
        self.write(register, irq);
    }

    pub fn set_dma(&mut self, resource: DmaResource) {
        let (register, channel) = match resource {
            DmaResource::Primary(channel) => (0x74, channel),
            DmaResource::Secondary(channel) => (0x75, channel),
        };
        self.write(register, channel);
    }

    /// Toggle the normal activation bit, preserving sibling functions.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.set_activation_bit(0, enabled);
    }

    /// Toggle a virtual function's activation bit in the shared 0x30 register.
    /// `bit` must be in 0..8; other activation bits are preserved.
    pub fn set_activation_bit(&mut self, bit: u8, enabled: bool) {
        assert!(bit < 8);
        let mask = 1 << bit;
        let old = (self.read)(0x30);
        self.write(0x30, (old & !mask) | if enabled { mask } else { 0 });
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{cell::RefCell, vec::Vec};

    #[test]
    fn relocation_keeps_disable_before_address_and_enable_after() {
        let mut writes = Vec::new();
        let mut device = LogicalDevice::select(10, |_| 0, |reg, val| writes.push((reg, val)));
        device.set_enabled(false);
        device.set_io_base(IoResource::Primary(0x680));
        device.set_enabled(true);
        assert_eq!(
            writes,
            [(7, 10), (0x30, 0), (0x60, 6), (0x61, 0x80), (0x30, 1)]
        );
    }

    #[test]
    fn additional_resources_do_not_overwrite_primary_resources() {
        let registers = RefCell::new([0xa5u8; 256]);
        let mut device = LogicalDevice::select(
            7,
            |reg| registers.borrow()[reg as usize],
            |reg, val| registers.borrow_mut()[reg as usize] = val,
        );
        for resource in [
            IoResource::Secondary(0xabcd),
            IoResource::Third(0xabcd),
            IoResource::Fourth(0xabcd),
            IoResource::Fifth(0xabcd),
        ] {
            device.set_io_base(resource);
        }
        device.set_irq(IrqResource::Secondary(9));
        device.set_dma(DmaResource::Secondary(3));
        let registers = registers.borrow();
        assert_eq!(&registers[0x60..0x62], &[0xa5, 0xa5]);
        assert_eq!(
            &registers[0x62..0x6a],
            &[0xab, 0xcd, 0xab, 0xcd, 0xab, 0xcd, 0xab, 0xcd]
        );
        assert_eq!(registers[0x70], 0xa5);
        assert_eq!(registers[0x72], 9);
        assert_eq!(registers[0x74], 0xa5);
        assert_eq!(registers[0x75], 3);
    }

    #[test]
    fn activation_preserves_sibling_functions() {
        let activation = RefCell::new(0b1010_0100u8);
        let mut device = LogicalDevice::select(
            7,
            |_| *activation.borrow(),
            |reg, val| {
                if reg == 0x30 {
                    *activation.borrow_mut() = val
                }
            },
        );
        device.set_enabled(true);
        assert_eq!(*activation.borrow(), 0b1010_0101);
        device.set_activation_bit(3, true);
        assert_eq!(*activation.borrow(), 0b1010_1101);
        device.set_enabled(false);
        assert_eq!(*activation.borrow(), 0b1010_1100);
        device.set_activation_bit(3, false);
        assert_eq!(*activation.borrow(), 0b1010_0100);
    }
}
