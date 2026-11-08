// SPDX-License-Identifier: MIT OR Apache-2.0
//! AST2600 host-side early Super I/O sequence from S8040 V2.00,
//! Ast2600PeiInit `ffd8e0a9`, script at `ffd8eb04`.
//!
//! It leaves UART bases and pin routing to the BMC. In particular it does
//! NOT call AST2500's SCU84/90 routing helper or program registers60/61.
//! Host decodes and BMC readiness are the platform's responsibility.

use fstart_core::indexed::Indexed;
use tock_registers::{interfaces::Writeable, register_bitfields};

pub const CONFIG_PORT: u16 = 0x2e;
register_bitfields![u8,
    LogicalDevice [NUMBER OFFSET(0) NUMBITS(8) []],
    Activation [ENABLE OFFSET(0) NUMBITS(1) []],
    InterruptSelect [VALUE OFFSET(0) NUMBITS(8) []]
];

/// The exact 15 writes through any tock-compatible index/data pair.
/// The caller serializes and exclusively owns both ports throughout.
pub fn early_init<I, D>(index: I, data: D)
where
    I: Writeable<T = u8> + Copy,
    D: Writeable<T = u8> + Copy,
{
    index.set(0xa5);
    index.set(0xa5);
    let ldn = Indexed::<_, _, LogicalDevice::Register>::new(index, data, 0x07);
    let activation = Indexed::<_, _, Activation::Register>::new(index, data, 0x30);
    // This is the AST interrupt-select register, not standard PnP IRQ0x70.
    let interrupt = Indexed::<_, _, InterruptSelect::Register>::new(index, data, 0x71);
    ldn.write(LogicalDevice::NUMBER.val(2));
    activation.write(Activation::ENABLE::CLEAR);
    interrupt.write(InterruptSelect::VALUE.val(0));
    activation.write(Activation::ENABLE::SET);
    ldn.write(LogicalDevice::NUMBER.val(3));
    interrupt.write(InterruptSelect::VALUE.val(0));
    index.set(0xaa);
}

/// # Safety
/// AST2600 configuration ports are decoded and exclusively owned. BMC
/// firmware and board jumpers provide the expected UART routing/base.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub unsafe fn early_init_at(base: u16) {
    use fstart_core::pio::PioRegister;
    assert!(base < u16::MAX);
    early_init(
        PioRegister::<u8>::new(base),
        PioRegister::<u8>::new(base + 1),
    );
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{cell::RefCell, vec::Vec};

    #[derive(Clone, Copy)]
    struct Port<'a>(&'a RefCell<Vec<(u16, u8)>>, u16);
    impl Writeable for Port<'_> {
        type T = u8;
        type R = ();
        fn set(&self, value: u8) {
            self.0.borrow_mut().push((self.1, value));
        }
    }
    #[test]
    fn exact_donor_sequence_without_uart_base_or_scu_writes() {
        let log = RefCell::new(Vec::new());
        early_init(Port(&log, 0x2e), Port(&log, 0x2f));
        assert_eq!(
            *log.borrow(),
            [
                (0x2e, 0xa5),
                (0x2e, 0xa5),
                (0x2e, 7),
                (0x2f, 2),
                (0x2e, 0x30),
                (0x2f, 0),
                (0x2e, 0x71),
                (0x2f, 0),
                (0x2e, 0x30),
                (0x2f, 1),
                (0x2e, 7),
                (0x2f, 3),
                (0x2e, 0x71),
                (0x2f, 0),
                (0x2e, 0xaa),
            ]
        );
    }
}
