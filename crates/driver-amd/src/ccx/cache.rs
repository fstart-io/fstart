// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD DRAM-first cache setup.
//! Fixed MTRRs carry AMD RdDram/WrDram attributes; high RAM uses TOM2WB.
use fstart_arch::x86::{
    cpu::{
        amd::msr::{SYSCFG, Syscfg},
        msr::{MTRR_CAP, MTRR_DEF_TYPE, MsrAccess, MtrrCap, MtrrDefType},
    },
    mtrr::{
        IA32_MTRR_FIX4K_C0000, IA32_MTRR_FIX16K_80000, IA32_MTRR_FIX16K_A0000,
        IA32_MTRR_FIX64K_00000, IA32_MTRR_PHYSBASE0, IA32_MTRR_PHYSMASK0,
    },
};
use tock_registers::interfaces::{Readable, Writeable};

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

/// Read fixed-MTRR RdDram/WrDram bits through AMD's modification gate.
/// BSP only before AP release, or with exclusive SMT-sibling access; local
/// interrupts/SMI must be excluded. The callback must only read registers.
/// Restore the exact original SYSCFG even when the callback returns an error.
pub fn fixed_dram_view<M: MsrAccess, T>(cpu: &M, read: impl FnOnce(&M) -> T) -> Result<T, Error> {
    let syscfg = cpu.reg(SYSCFG).extract();
    if syscfg.is_set(Syscfg::SMEE) {
        return Err(Error::EncryptionEnabled);
    }
    if !cpu.reg(MTRR_CAP).is_set(MtrrCap::FIXED) {
        return Err(Error::FixedUnsupported);
    }
    cpu.reg(SYSCFG)
        .set(syscfg.get() | Syscfg::MTRR_FIX_DRAM_MOD_EN::SET.value);
    let result = read(cpu);
    cpu.reg(SYSCFG).set(syscfg.get());
    Ok(result)
}

/// Current-CPU cache controls. All APs must install the same plan while their
/// cache is disabled, before entering coherent firmware or the OS.
pub trait CacheControl {
    fn disable(&self);
    fn flush(&self);
    fn enable(&self);
}
/// The executing CPU, under the contract of its MSR access: ring 0 in DRAM.
impl CacheControl for fstart_arch::x86::cpu::msr::NativeMsr {
    fn disable(&self) {
        // SAFETY: running from DRAM, never CAR.
        unsafe { fstart_arch::x86::mtrr::disable_cache() }
    }
    fn flush(&self) {
        // SAFETY: ring 0. Unlike the i945/Atom parts that hang on it, AMD
        // CPUs write back and invalidate with CR0.CD set.
        unsafe { fstart_arch::x86::writeback_invalidate_caches() }
    }
    fn enable(&self) {
        // SAFETY: the callers enable caching after a coherent MTRR setup.
        unsafe { fstart_arch::x86::mtrr::enable_cache() }
    }
}

/// Interrupts must be disabled; called by the BSP before starting APs or by
/// each AP before joining the rendezvous. No Intel CAR teardown is involved.
pub fn install(cpu: &(impl MsrAccess + CacheControl), plan: &Plan) -> Result<(), Error> {
    let cap = cpu.reg(MTRR_CAP).extract();
    let available = cap.read(MtrrCap::VARIABLE_COUNT) as usize;
    if !cap.is_set(MtrrCap::FIXED) {
        return Err(Error::FixedUnsupported);
    }
    if plan.count > available {
        return Err(Error::NoMtrrs);
    }
    let syscfg = cpu.reg(SYSCFG).extract();
    if syscfg.is_set(Syscfg::SMEE) {
        return Err(Error::EncryptionEnabled);
    }
    let default = cpu.reg(MTRR_DEF_TYPE).extract();
    cpu.disable();
    cpu.flush();
    cpu.reg(MTRR_DEF_TYPE).set(0);
    cpu.reg(SYSCFG)
        .set(syscfg.get() | Syscfg::MTRR_FIX_DRAM_MOD_EN::SET.value);
    for index in 0..available as u32 {
        // Disable each mask before its base changes, including stale pairs.
        cpu.write(IA32_MTRR_PHYSMASK0 + index * 2, 0);
        let (base, mask) = plan.encoded(index as usize).unwrap_or((0, 0));
        cpu.write(IA32_MTRR_PHYSBASE0 + index * 2, base);
        cpu.write(IA32_MTRR_PHYSMASK0 + index * 2, mask);
    }
    let ram = 0x1e1e1e1e1e1e1e1e; // WB + AMD RdDram + WrDram in each fixed slot.
    cpu.write(IA32_MTRR_FIX64K_00000, ram);
    cpu.write(IA32_MTRR_FIX16K_80000, ram);
    cpu.write(IA32_MTRR_FIX16K_A0000, 0); // A0000..C0000 is VGA MMIO, not DRAM.
    for index in IA32_MTRR_FIX4K_C0000..IA32_MTRR_FIX4K_C0000 + 8 {
        cpu.write(index, ram);
    }
    let mut enabled = default;
    enabled.modify(
        MtrrDefType::TYPE.val(0) + MtrrDefType::FIXED_ENABLE::SET + MtrrDefType::ENABLE::SET,
    );
    cpu.reg(MTRR_DEF_TYPE).set(enabled.get());
    // Fixed/variable DRAM routing, high-memory TOM2 write-back, and the fixed
    // modification gate left set.
    cpu.reg(SYSCFG).set(
        syscfg.get()
            | (Syscfg::MTRR_FIX_DRAM_EN::SET
                + Syscfg::MTRR_FIX_DRAM_MOD_EN::SET
                + Syscfg::MTRR_VAR_DRAM_EN::SET
                + Syscfg::TOM2_EN::SET
                + Syscfg::TOM2_WB::SET)
                .value,
    );
    cpu.flush();
    cpu.enable();
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    //! The cache controls of an MSR model: logged among its MSR writes.
    use super::CacheControl;
    use core::cell::RefCell;
    use fstart_arch::x86::cpu::msr::testing::MsrSpace;

    /// The "MSR index" of logged cache operations.
    pub const CACHE: u32 = u32::MAX;
    pub const DISABLE: (u32, u64) = (CACHE, 0);
    pub const FLUSH: (u32, u64) = (CACHE, 1);
    pub const ENABLE: (u32, u64) = (CACHE, 2);

    impl CacheControl for RefCell<MsrSpace> {
        fn disable(&self) {
            self.borrow_mut().writes.push(DISABLE);
        }
        fn flush(&self) {
            self.borrow_mut().writes.push(FLUSH);
        }
        fn enable(&self) {
            self.borrow_mut().writes.push(ENABLE);
        }
    }
}
