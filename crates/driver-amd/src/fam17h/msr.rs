// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h MSR access and the memory/configuration MSRs every Zen
//! generation programs the same way.
pub const SYSCFG: u32 = 0xc0010010;
/// SYSCFG.MtrrFixDramModEn: fixed-MTRR RdDram/WrDram bits are accessible.
pub const SYSCFG_MTRR_FIX_DRAM_MOD_EN: u64 = 1 << 19;
pub const TOP_MEM: u32 = 0xc001001a;
pub const TOP_MEM2: u32 = 0xc001001d;
pub const MMCONFIG: u32 = 0xc0010058;
pub const PSP_ADDRESS: u32 = 0xc00110a2;

pub trait Msr {
    fn read(&mut self, index: u32) -> u64;
    fn write(&mut self, index: u32, value: u64);
    fn update(&mut self, index: u32, clear: u64, set: u64) {
        let value = self.read(index);
        self.write(index, (value & !clear) | set);
    }
}
#[cfg(target_arch = "x86_64")]
pub struct NativeMsr {
    _exclusive: core::marker::PhantomData<*mut ()>,
}
#[cfg(target_arch = "x86_64")]
impl NativeMsr {
    /// # Safety
    /// Ring 0 on a supported Family 17h CPU, with local interrupt and
    /// SMT-sibling serialization appropriate to each MSR operation.
    pub unsafe fn new() -> Self {
        Self {
            _exclusive: core::marker::PhantomData,
        }
    }
}
#[cfg(target_arch = "x86_64")]
impl Msr for NativeMsr {
    fn read(&mut self, index: u32) -> u64 {
        let low: u32;
        let high: u32;
        unsafe {
            core::arch::asm!("rdmsr", in("ecx") index, out("eax") low, out("edx") high, options(nomem, nostack));
        }
        (u64::from(high) << 32) | u64::from(low)
    }
    fn write(&mut self, index: u32, value: u64) {
        unsafe {
            core::arch::asm!("wrmsr", in("ecx") index, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nostack));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBoundary;

/// Decode the PSP-established memory ceiling without narrowing high RAM to u32.
pub fn memory_limits(msr: &mut impl Msr) -> Result<(u64, Option<u64>), MemoryBoundary> {
    let low = msr.read(TOP_MEM) & 0x000000ffff800000;
    if low == 0 || low >= 1 << 32 {
        return Err(MemoryBoundary);
    }
    let high = if msr.read(SYSCFG) & (1 << 21) != 0 {
        Some(msr.read(TOP_MEM2) & 0x000000ffffc00000)
    } else {
        None
    };
    Ok((low, high))
}

/// AMD MMIO_CONF_BASE (not an Intel PCIEXBAR): `buses` must be a power of two
/// between 1 and 256 and `base` aligned to the window size.
pub fn enable_ecam(msr: &mut impl Msr, base: u64, buses: u16) {
    assert!(buses.is_power_of_two() && buses <= 256);
    assert!(base & ((u64::from(buses) << 20) - 1) == 0);
    msr.write(MMCONFIG, base | (u64::from(buses.trailing_zeros()) << 2) | 1);
}
