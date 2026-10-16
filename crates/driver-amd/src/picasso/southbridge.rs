// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Picasso FCH A-Link and clock sequencing. The index-zero writes are hardware requirements.
use super::registers::Smn;

pub trait AlinkIo {
    fn decode(&mut self, base: u32);
    fn clock_control(&mut self) -> u32;
    fn set_clock_control(&mut self, value: u32);
    fn index(&mut self, value: u32);
    fn read_data(&mut self) -> u32;
    fn write_data(&mut self, value: u32);
}
fn abcfg(io: &mut impl AlinkIo, register: u32, clear: u32, set: u32) {
    let index = (6 << 29) | register; // ABCFG register space, not AXCFG.
    io.index(index);
    let value = io.read_data();
    io.index(0);
    io.index(index);
    io.write_data((value & !clear) | set);
    io.index(0);
}
const ABCFG: &[(u32, u32, u32)] = &[
    (0x00080, 0x00060000, 0x00060001),
    (0x00090, 0x00200000, 0x00200000),
    (0x00094, 0x00100000, 0x00100fee),
    (0x10054, 0x00ffffff, 0x000007ff),
    (0x10060, 0xfbffffff, 0x02000000),
    (0x10090, 0x00010000, 0x00010000),
    (0x10050, 0x00000004, 0x00000000),
    (0x00054, 0x00ff0000, 0x00100000),
    (0x10054, 0x00ff0000, 0x00100000),
    (0x00054, 0x00000010, 0x00000010),
    (0x00054, 0x01000000, 0x01000000),
    (0x10054, 0x03000000, 0x03000000),
    (0x00054, 0x00000001, 0x00000000),
    (0x00054, 0x00000004, 0x00000004),
    (0x00054, 0x00000001, 0x00000001),
    (0x00054, 0x00000004, 0x00000000),
    (0x000bc, 0x00000010, 0x00000010),
    (0x00058, 0x20000000, 0x20000000),
    (0x00058, 0x80000000, 0x80000000),
    (0x00208, 0x00000280, 0x00000280),
    (0x00208, 0x00408010, 0x00408010),
    (0x00180, 0x00000001, 0x00000000),
    (0x00184, 0x00000004, 0x00000000),
    (0x00180, 0x00001f00, 0x00000000),
    (0x00180, 0x00000020, 0x00000000),
    (0x0022c, 0x00000800, 0x00000000),
    (0x0022c, 0x00004000, 0x00000000),
    (0x00228, 0x0000000a, 0x0000000a),
];
pub fn alink_init(io: &mut impl AlinkIo) {
    io.decode(0xcd8);
    let clock = io.clock_control();
    io.set_clock_control(clock & !(1 << 1));
    for &(register, clear, set) in ABCFG {
        abcfg(io, register, clear, set);
    }
    io.decode(0);
}
pub fn clock_init(smn: &mut impl Smn) {
    for (offset, clear, set) in [
        (0x10, 0x00000003, 0),
        (0x10, 0x00001ff0, 0),
        (0x34, 0xff800000, 0),
        (0x14, 0x00ffff00, 0),
        (0x18, 0xffff0000, 0),
        (0x1c, 0x0001e000, 0x00008000),
        (0x1c, 0x001e0000, 0x00060000),
        (0x1c, 0x0c000000, 0x0c000000),
        (0x1c, 0x00001e00, 0x00000200),
        (0x40, 1 << 25, 1 << 25),
        (0x40, 1 << 30, 1 << 30),
        (0x10, 1 << 29, 1 << 29),
        (0x08, 1, 1),
        (0x40, 1 << 30, 1 << 30),
    ] {
        smn.update(0x02d01e00 + offset, clear, set);
    }
}

#[cfg(target_arch = "x86_64")]
pub struct NativeAlink {
    _exclusive: core::marker::PhantomData<*mut ()>,
}
#[cfg(target_arch = "x86_64")]
impl NativeAlink {
    /// # Safety
    /// Ring0, interrupts disabled, ACPIMMIO decoded UC, exclusive CD8/CDC and
    /// PM/MISC access. Not usable alongside another indirect-port owner.
    pub unsafe fn new() -> Self {
        Self {
            _exclusive: core::marker::PhantomData,
        }
    }
    fn read_port() -> u32 {
        // SAFETY: the constructor owns the CD8/CDC index/data pair.
        unsafe { fstart_core::pio::inl(0xcdc) }
    }
    fn write_port(port: u16, value: u32) {
        // SAFETY: as for read_port.
        unsafe { fstart_core::pio::outl(port, value) }
    }
    fn mmio(address: usize) -> &'static tock_registers::registers::ReadWrite<u32> {
        // SAFETY: only the two aligned fixed addresses below are passed;
        // constructor requires their UC decode and exclusive access.
        unsafe { &*(address as *const tock_registers::registers::ReadWrite<u32>) }
    }
}
#[cfg(target_arch = "x86_64")]
impl AlinkIo for NativeAlink {
    fn decode(&mut self, base: u32) {
        use tock_registers::interfaces::Writeable;
        Self::mmio(0xfed803e0).set(base);
    }
    fn clock_control(&mut self) -> u32 {
        use tock_registers::interfaces::Readable;
        Self::mmio(0xfed80e40).get()
    }
    fn set_clock_control(&mut self, value: u32) {
        use tock_registers::interfaces::Writeable;
        Self::mmio(0xfed80e40).set(value);
    }
    fn index(&mut self, value: u32) {
        Self::write_port(0xcd8, value);
    }
    fn read_data(&mut self) -> u32 {
        Self::read_port()
    }
    fn write_data(&mut self, value: u32) {
        Self::write_port(0xcdc, value);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;
    #[derive(Default)]
    struct Io {
        events: Vec<(u8, u32)>,
    }
    impl AlinkIo for Io {
        fn decode(&mut self, value: u32) {
            self.events.push((0, value));
        }
        fn clock_control(&mut self) -> u32 {
            u32::MAX
        }
        fn set_clock_control(&mut self, value: u32) {
            self.events.push((1, value));
        }
        fn index(&mut self, value: u32) {
            self.events.push((2, value));
        }
        fn read_data(&mut self) -> u32 {
            self.events.push((3, 0));
            0x12345678
        }
        fn write_data(&mut self, value: u32) {
            self.events.push((4, value));
        }
    }
    #[test]
    fn abcfg_transactions_reset_index_after_read_and_write_and_remove_decode() {
        let mut io = Io::default();
        alink_init(&mut io);
        assert_eq!(io.events[0..2], [(0, 0xcd8), (1, u32::MAX & !2)]);
        for (events, &(register, clear, set)) in
            io.events[2..io.events.len() - 1].chunks_exact(6).zip(ABCFG)
        {
            let index = 0xc0000000 | register;
            assert_eq!(
                events,
                [
                    (2, index),
                    (3, 0),
                    (2, 0),
                    (2, index),
                    (4, (0x12345678 & !clear) | set),
                    (2, 0)
                ]
            );
        }
        assert_eq!(io.events.last(), Some(&(0, 0)));
    }
}
