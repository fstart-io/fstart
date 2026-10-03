// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD DRAM-first cache setup.
//! Fixed MTRRs carry AMD RdDram/WrDram attributes; high RAM uses TOM2WB.
use super::cpu::{Msr, SYSCFG};

const MAX_RANGES: usize = 10;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Range {
    pub base: u64,
    pub size: u64,
    pub kind: u8,
}
#[derive(Debug, Clone)]
pub struct Plan {
    ranges: [Range; MAX_RANGES],
    count: usize,
    physical_mask: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    MemoryBoundary,
    AddressBits,
    NoMtrrs,
    FixedUnsupported,
    EncryptionEnabled,
}

impl Plan {
    /// Preflight the complete variable-MTRR plan before changing cache state.
    /// Decompose low DRAM exactly: never round WB caching into peripheral MMIO.
    pub fn new(low_top: u64, address_bits: u8, available: usize) -> Result<Self, Error> {
        if !(0x02050000..0x1_0000_0000).contains(&low_top) || low_top & 0x7fffff != 0 {
            return Err(Error::MemoryBoundary);
        }
        if !(32..=52).contains(&address_bits) {
            return Err(Error::AddressBits);
        }
        let mut plan = Self {
            ranges: [Range::default(); MAX_RANGES],
            count: 0,
            physical_mask: ((1u64 << address_bits) - 1) & !0xfff,
        };
        let mut base = 0;
        let available = available.min(MAX_RANGES);
        while base < low_top {
            let remaining = low_top - base;
            let size = 1u64 << (63 - remaining.leading_zeros());
            plan.push(
                Range {
                    base,
                    size,
                    kind: 6,
                },
                available,
            )?;
            base += size;
        }
        plan.push(
            Range {
                base: 0xff000000,
                size: 0x1000000,
                kind: 5,
            },
            available,
        )?;
        Ok(plan)
    }
    fn push(&mut self, range: Range, available: usize) -> Result<(), Error> {
        if self.count == available {
            return Err(Error::NoMtrrs);
        }
        self.ranges[self.count] = range;
        self.count += 1;
        Ok(())
    }
    pub fn ranges(&self) -> &[Range] {
        &self.ranges[..self.count]
    }
    pub fn encoded(&self, index: usize) -> Option<(u64, u64)> {
        self.ranges().get(index).map(|range| {
            (
                (range.base & self.physical_mask) | u64::from(range.kind),
                (!(range.size - 1) & self.physical_mask) | (1 << 11),
            )
        })
    }
}

/// Current-CPU cache controls. All APs must install the same plan while their
/// cache is disabled, before entering coherent firmware or the OS.
pub trait CacheControl {
    fn disable(&mut self);
    fn flush(&mut self);
    fn enable(&mut self);
}
#[cfg(target_arch = "x86_64")]
impl CacheControl for super::cpu::NativeMsr {
    fn disable(&mut self) {
        let cr0: u64;
        unsafe {
            core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nostack));
            core::arch::asm!("mov cr0, {}", in(reg) ((cr0 | (1 << 30)) & !(1 << 29)), options(nostack));
        }
    }
    fn flush(&mut self) {
        unsafe {
            core::arch::asm!("wbinvd", options(nostack));
        }
    }
    fn enable(&mut self) {
        let cr0: u64;
        unsafe {
            core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nostack));
            core::arch::asm!("mov cr0, {}", in(reg) (cr0 & !((1 << 30) | (1 << 29))), options(nostack));
        }
    }
}

/// Interrupts must be disabled; called by the BSP before starting APs or by
/// each AP before joining the rendezvous. No Intel CAR teardown is involved.
pub fn install(cpu: &mut (impl Msr + CacheControl), plan: &Plan) -> Result<(), Error> {
    let cap = cpu.read(0xfe);
    let available = (cap & 0xff) as usize;
    if cap & (1 << 8) == 0 {
        return Err(Error::FixedUnsupported);
    }
    if plan.count > available {
        return Err(Error::NoMtrrs);
    }
    let syscfg = cpu.read(SYSCFG);
    if syscfg & (1 << 23) != 0 {
        return Err(Error::EncryptionEnabled);
    }
    let default = cpu.read(0x2ff);
    cpu.disable();
    cpu.flush();
    cpu.write(0x2ff, 0);
    cpu.write(SYSCFG, syscfg | (1 << 19));
    for index in 0..available {
        // Disable each mask before its base changes, including stale pairs.
        cpu.write(0x201 + index as u32 * 2, 0);
        let (base, mask) = plan.encoded(index).unwrap_or((0, 0));
        cpu.write(0x200 + index as u32 * 2, base);
        cpu.write(0x201 + index as u32 * 2, mask);
    }
    let ram = 0x1e1e1e1e1e1e1e1e; // WB + AMD RdDram + WrDram in each fixed slot.
    cpu.write(0x250, ram);
    cpu.write(0x258, ram);
    cpu.write(0x259, 0); // A0000..C0000 is VGA MMIO, not DRAM.
    for index in 0x268..=0x26f {
        cpu.write(index, ram);
    }
    cpu.write(0x2ff, (default & !0xff) | (1 << 10) | (1 << 11));
    // Fixed/variable DRAM routing, high-memory TOM2 write-back, and the fixed
    // modification gate left set.
    cpu.write(SYSCFG, syscfg | (0x1f << 18));
    cpu.flush();
    cpu.enable();
    Ok(())
}
