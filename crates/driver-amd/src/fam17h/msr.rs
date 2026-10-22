// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h MSR access and the memory/configuration MSRs every Zen
//! generation programs the same way.
use tock_registers::register_bitfields;

pub const SYSCFG: u32 = 0xc0010010;
/// SYSCFG.MtrrFixDramModEn: fixed-MTRR RdDram/WrDram bits are accessible.
pub const SYSCFG_MTRR_FIX_DRAM_MOD_EN: u64 = 1 << 19;
pub const TOP_MEM: u32 = 0xc001001a;
pub const TOP_MEM2: u32 = 0xc001001d;
pub const MMCONFIG: u32 = 0xc0010058;
pub const PSP_ADDRESS: u32 = 0xc00110a2;
pub const MCG_CAP: u32 = 0x179;

register_bitfields![u64,
    pub Syscfg [
        MTRR_FIX_DRAM_MOD_EN OFFSET(19) NUMBITS(1) [],
        TOM2_EN OFFSET(21) NUMBITS(1) [],
        /// Memory between 4 GiB and TOM2 is write-back.
        TOM2_WB OFFSET(22) NUMBITS(1) [],
        /// Secure memory encryption enable.
        SMEE OFFSET(23) NUMBITS(1) []
    ],
    pub McaConfig [
        MCAX OFFSET(0) NUMBITS(1) [],
        TRANSPARENT_LOGGING_SUPPORTED OFFSET(1) NUMBITS(1) [],
        DEFERRED_LOGGING_SUPPORTED OFFSET(2) NUMBITS(1) [],
        MCAX_ENABLE OFFSET(32) NUMBITS(1) [],
        TRANSPARENT_LOGGING_ENABLE OFFSET(33) NUMBITS(1) [],
        LOG_DEFERRED_IN_MCA_STAT OFFSET(34) NUMBITS(1) []
    ]
];

/// MCAX register `offset` of MCA `bank` (MSRC000_2xxx).
pub const fn mcax(bank: u32, offset: u32) -> u32 {
    0xc0002000 + bank * 16 + offset
}
pub const MCA_MISC0: u32 = 3;
pub const MCA_CONFIG: u32 = 4;
pub const MCA_MISC1: u32 = 10;
/// MCA_CTL_MASK of `bank`: errors whose bit is set are not reported.
pub const fn mca_ctl_mask(bank: u32) -> u32 {
    0xc0010400 + bank
}

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
