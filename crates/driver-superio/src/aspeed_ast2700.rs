// SPDX-License-Identifier: MIT OR Apache-2.0
//! ASPEED AST2700 host-side early Super I/O initialization (verified against
//! GIGABYTE MD25-CG0 `SioAst2xxxPei` `0xeaa`, `0xA5`/`0xA5` unlock, `0xAA`
//! exit, SUART1 LDN `0x02` / SUART2 LDN `0x03`).
//!
//! Host-side LPC/eSPI I/O decodes (`0x2E`/`0x2F`, `0x3F8`, `0xCA0..0xCAF`)
//! are the platform/board's responsibility before calling [`early_init`].

use fstart_core::indexed::Indexed;
use tock_registers::{interfaces::Writeable, register_bitfields};

pub const CONFIG_PORT: u16 = 0x2e;

/// AST2700 Super I/O logical device numbers.
pub mod ldn {
    pub const SUART1: u8 = 0x02;
    pub const SUART2: u8 = 0x03;
    pub const SWC: u8 = 0x04;
    pub const KBC: u8 = 0x05;
    pub const GPIO: u8 = 0x07;
    pub const SUART3: u8 = 0x0b;
    pub const SUART4: u8 = 0x0c;
    pub const ILPC2AHB: u8 = 0x0d;
    pub const MAILBOX: u8 = 0x0e;
}

register_bitfields![u8,
    LogicalDevice [NUMBER OFFSET(0) NUMBITS(8) []],
    Activation [ENABLE OFFSET(0) NUMBITS(1) []],
    InterruptSelect [VALUE OFFSET(0) NUMBITS(8) []]
];

/// AST2700 early Super I/O sequence: unlock (`0xA5`, `0xA5`), cycle SUART1
/// (LDN 2) with AST interrupt-select `0x71 = 0` and activate it, clear SUART2
/// (LDN 3) interrupt-select `0x71 = 0`, and exit configuration mode (`0xAA`).
pub fn early_init<I, D>(index: I, data: D)
where
    I: Writeable<T = u8> + Copy,
    D: Writeable<T = u8> + Copy,
{
    index.set(0xa5);
    index.set(0xa5);
    let sel = Indexed::<_, _, LogicalDevice::Register>::new(index, data, 0x07);
    let act = Indexed::<_, _, Activation::Register>::new(index, data, 0x30);
    let irq = Indexed::<_, _, InterruptSelect::Register>::new(index, data, 0x71);

    sel.write(LogicalDevice::NUMBER.val(ldn::SUART1));
    act.write(Activation::ENABLE::CLEAR);
    irq.write(InterruptSelect::VALUE.val(0));
    act.write(Activation::ENABLE::SET);

    sel.write(LogicalDevice::NUMBER.val(ldn::SUART2));
    irq.write(InterruptSelect::VALUE.val(0));

    index.set(0xaa);
}

/// # Safety
/// AST2700 configuration ports (`base`/`base + 1`) are decoded and exclusively
/// owned by the BSP.
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
    fn unlocks_activates_suart1_and_exits() {
        let log = RefCell::new(Vec::new());
        early_init(Port(&log, CONFIG_PORT), Port(&log, CONFIG_PORT + 1));
        assert_eq!(
            *log.borrow(),
            [
                (0x2e, 0xa5),
                (0x2e, 0xa5),
                (0x2e, 0x07),
                (0x2f, 0x02),
                (0x2e, 0x30),
                (0x2f, 0x00),
                (0x2e, 0x71),
                (0x2f, 0x00),
                (0x2e, 0x30),
                (0x2f, 0x01),
                (0x2e, 0x07),
                (0x2f, 0x03),
                (0x2e, 0x71),
                (0x2f, 0x00),
                (0x2e, 0xaa),
            ]
        );
    }
}
