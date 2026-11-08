//! HPET register access and delays. The platform owns address/decode policy.

use tock_registers::{
    interfaces::{ReadWriteable, Readable},
    register_bitfields, register_structs,
    registers::{ReadOnly, ReadWrite},
};

register_bitfields![u32,
    Configuration [ENABLE OFFSET(0) NUMBITS(1) []]
];
register_structs! {
    Registers {
        (0x00 => capabilities: ReadOnly<u32>),
        (0x04 => period_fs: ReadOnly<u32>),
        (0x08 => _reserved0),
        (0x10 => configuration: ReadWrite<u32, Configuration::Register>),
        (0x14 => _reserved1),
        (0xf0 => counter: ReadOnly<u32>),
        (0xf4 => @END),
    }
}

/// A platform-selected HPET. Only the low counter word is needed for delays.
#[derive(Clone, Copy)]
pub struct Hpet {
    base: usize,
}

impl Hpet {
    /// # Safety
    /// `base` is an aligned, UC-mapped HPET register block whenever accessed.
    /// The caller owns configuration writes and enables it before delays.
    pub const unsafe fn new(base: usize) -> Self {
        Self { base }
    }

    fn registers(&self) -> &Registers {
        // SAFETY: the constructor's MMIO mapping contract.
        unsafe { &*(self.base as *const Registers) }
    }

    /// Start the counter without changing legacy routing or timer enables.
    pub fn enable(&self) {
        self.registers()
            .configuration
            .modify(Configuration::ENABLE::SET);
    }

    /// Delay at least `us` microseconds at the device's advertised frequency.
    pub fn delay_us(&self, us: u32) {
        if us == 0 {
            return;
        }
        let registers = self.registers();
        let ticks = ticks_for_us(us, registers.period_fs.get());
        wait_ticks(ticks, || registers.counter.get());
    }
}

fn ticks_for_us(us: u32, period_fs: u32) -> u64 {
    assert!(period_fs != 0, "HPET clock period is zero");
    (u64::from(us) * 1_000_000_000).div_ceil(u64::from(period_fs))
}

fn wait_ticks(mut ticks: u64, mut counter: impl FnMut() -> u32) {
    // Bound each interval to half a counter wrap; long delays cannot truncate
    // to u32 or mistake a wrapped deadline for an already-expired one.
    while ticks != 0 {
        let interval = ticks.min(u64::from(u32::MAX / 2)) as u32;
        let start = counter();
        // The first sample may be just before a tick. Wait one extra edge
        // per interval so quantization cannot shorten the requested delay.
        while counter().wrapping_sub(start) <= interval {
            core::hint::spin_loop();
        }
        ticks -= u64::from(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_advertised_period_and_rounds_up() {
        assert_eq!(ticks_for_us(1, 100_000_000), 10);
        assert_eq!(ticks_for_us(1, 41_666_666), 25);
        assert_eq!(ticks_for_us(2, 69_841_279), 29);
        assert_eq!(
            ticks_for_us(u32::MAX, 1),
            u64::from(u32::MAX) * 1_000_000_000
        );
    }

    #[test]
    #[should_panic(expected = "HPET clock period is zero")]
    fn rejects_invalid_period() {
        ticks_for_us(1, 0);
    }

    #[test]
    fn waits_across_counter_wrap_and_long_intervals() {
        let mut counter = u32::MAX - 2;
        let mut reads = 0;
        wait_ticks(7, || {
            let value = counter;
            counter = counter.wrapping_add(1);
            reads += 1;
            value
        });
        assert_eq!(reads, 9);
        let mut counter = 0u32;
        let mut reads = 0;
        wait_ticks(u64::from(u32::MAX) + 1, || {
            counter = counter.wrapping_add(u32::MAX / 2);
            reads += 1;
            counter
        });
        assert_eq!(reads, 8);
        wait_ticks(0, || panic!("zero delay must not read a counter"));
    }
}
