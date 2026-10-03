// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Zen/Zen+ CPU policy.
//! AMD MCAX, SYSCFG and MMCONFIG must not use legacy Intel CPU programming.
pub const SYSCFG: u32 = 0xc0010010;
pub const TOP_MEM: u32 = 0xc001001a;
pub const TOP_MEM2: u32 = 0xc001001d;
pub const MMCONFIG: u32 = 0xc0010058;
pub const PSP_ADDRESS: u32 = 0xc00110a2;
const LS_CFG: u32 = 0xc0011020;
const DE_CFG: u32 = 0xc0011029;

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
    /// Ring 0 on a supported Picasso-family CPU, with local interrupt and
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
pub enum Silicon {
    Raven,
    Picasso,
    Raven2,
}
#[derive(Debug, Clone, Copy)]
pub struct Identity {
    pub silicon: Silicon,
    pub raven2_cpuid: bool,
    pub am4: bool,
    pub reduced_io: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    CpuIdentity,
    McaBanks(u8),
    MemoryBoundary,
}
impl Identity {
    /// Mirrors the original CPUID versus SMU-silicon distinction, including
    /// reduced-I/O PCO Dali parts that retain a Family 17h model-1x CPUID.
    pub fn from_cpuid(signature: u32, extended_ebx: u32, silicon_id: u32) -> Result<Self, Error> {
        let base_family = (signature >> 8) & 0xf;
        let family = base_family
            + if base_family == 0xf {
                (signature >> 20) & 0xff
            } else {
                0
            };
        let model = (signature >> 4) & 0xf
            | if matches!(base_family, 6 | 15) {
                (signature >> 12) & 0xf0
            } else {
                0
            };
        let package = extended_ebx >> 28;
        if family != 0x17 || !matches!(model, 0x11 | 0x18 | 0x20) || !matches!(package, 0 | 2 | 3) {
            return Err(Error::CpuIdentity);
        }
        let raven2_cpuid = model >> 4 == 2;
        let silicon = if model == 0x20 || silicon_id & (1 << 30) != 0 {
            Silicon::Raven2
        } else if model == 0x18 {
            Silicon::Picasso
        } else {
            Silicon::Raven
        };
        let dali = package == 0 && (raven2_cpuid || silicon_id & (1 << 31) != 0);
        Ok(Self {
            silicon,
            raven2_cpuid,
            am4: package == 2,
            reduced_io: silicon == Silicon::Raven2 || dali,
        })
    }
}

const fn mcax(bank: u32, offset: u32) -> u32 {
    0xc0002000 + bank * 16 + offset
}
const DEFERRED: u64 = (1 << 32) | (1 << 34) | (1 << 37);
const RESET: &[(u32, u64, u64)] = &[
    (0x0000001b, 0, 1 << 11),
    (0xc0010015, 0, 1 << 27),
    (0xc0011005, (1 << 48) | (1 << 42), 0),
    (0xc00110dc, 0x28000, 0),
    (0xc0010074, 0, 9),
    (mcax(0, 4), 1 << 33, DEFERRED),
    (mcax(1, 4), 1 << 33, DEFERRED),
    (mcax(2, 4), 1 << 33, DEFERRED),
    (mcax(3, 4), 1 << 33, DEFERRED),
    (mcax(5, 4), 1 << 33, DEFERRED),
    (mcax(6, 4), 1 << 33, DEFERRED),
    (0xc0010401, 0, 1 << 13),
    (0xc0010406, 0, 1 << 6),
    (0xc0010402, 0, 1 << 3),
    (0xc0010407, 0, 1 << 7),
    (LS_CFG, 0, 0x100),
    (0xc0011023, 0, 0x0000200000000000),
    (0xc0011028, 0, 0x14),
    (DE_CFG, 0, 0x26000),
    (0xc001102a, 0, 0x0280000000008080),
    (0xc001102c, 0, 0x0168000000000000),
    (0xc001102b, 0, 0x80000),
    (0xc001102d, 0, 0x1000000100000000),
    (0xc001102e, 0, 1),
    (0xc0011093, 0x58d8c, 0x480003),
    (0xc0011097, 8, 0xc54),
];

pub fn reset_registers(msr: &mut impl Msr) {
    for &(index, clear, set) in RESET {
        msr.update(index, clear, set);
    }
    msr.update(DE_CFG, 1 << 14, 0);
    msr.update(LS_CFG, 1 << 28, 0);
}
pub fn bsp_init(msr: &mut impl Msr) {
    reset_registers(msr);
    // No encrypted memory is configured; otherwise every DMA/MTRR consumer
    // would have to account for the C-bit.
    msr.update(SYSCFG, 1 << 23, 0);
}

pub fn mca_init(msr: &mut impl Msr) -> Result<(), Error> {
    let cap = msr.read(0x179);
    let banks = (cap & 0xff) as u8;
    // This native flow accesses MISC1 for banks 15/16. Reject a mismatched CPU
    // before touching any MCAX register, rather than faulting midway through.
    if !(17..=32).contains(&banks) {
        return Err(Error::McaBanks(banks));
    }
    for bank in 0..u32::from(banks) {
        let config = msr.read(mcax(bank, 4));
        let mut value = config & !(1 << 32);
        if config & 2 != 0 {
            value &= !((1 << 32) | (1 << 33));
        }
        value |= 1 << 32;
        if config & 4 != 0 {
            value |= 1 << 34;
        }
        msr.write(mcax(bank, 4), value);
        if bank != 15 {
            msr.update(mcax(bank, 3), 0xff000000, 0);
        }
    }
    msr.update(mcax(15, 10), 0xff000000, 0);
    msr.update(mcax(16, 10), 0xff000000, 0);
    msr.update(0xc0000410, 0, 1 << 3);
    msr.update(0xc0010400, 0, (1 << 9) | (1 << 10));
    msr.update(0xc0010406, 0, 1 << 6);
    if (cap >> 4) & 0xf == 0 {
        for bank in 8..=15 {
            msr.update(0xc0010400 + bank, 0, 1 << 7);
        }
    }
    Ok(())
}

/// Native per-core tail. The platform serializes this across SMT siblings;
/// it installs identical AMD cache routing before invoking this function.
pub fn per_cpu_tail(msr: &mut impl Msr, primary_thread: bool) {
    if primary_thread {
        let value = msr.read(0xc0011074);
        msr.write(0xc0011074, 0);
        for index in 0..21 {
            msr.write(0xc0011076, index);
            msr.write(0xc0011077, 0);
        }
        msr.write(0xc0011074, value | 0x9000000000000000);
    }
    msr.update(DE_CFG, 0, 1 << 3);
    msr.update(LS_CFG, 0, (1 << 13) | (1 << 15));
    msr.update(0xc0011023, 0, 1 << 49);
    msr.write(0xc0010296, 0x484848);
    let pstate0 = msr.read(0xc0010064);
    msr.write(0xc0010064, pstate0);
    msr.update(0xc001102b, 1 << 2, 1 << 3);
    msr.update(0xc0011022, 0, (1 << 13) | (1 << 15));
}

/// Decode the PSP-established memory ceiling without narrowing high RAM to u32.
pub fn memory_limits(msr: &mut impl Msr) -> Result<(u64, Option<u64>), Error> {
    let low = msr.read(TOP_MEM) & 0x000000ffff800000;
    if low == 0 || low >= 1 << 32 {
        return Err(Error::MemoryBoundary);
    }
    let high = if msr.read(SYSCFG) & (1 << 21) != 0 {
        Some(msr.read(TOP_MEM2) & 0x000000ffffc00000)
    } else {
        None
    };
    Ok((low, high))
}

pub fn enable_ecam(msr: &mut impl Msr) {
    // AMD MMIO_CONF_BASE: 64 buses (BusRange=6), enabled. Not PCIEXBAR.
    msr.write(MMCONFIG, super::ECAM | (6 << 2) | 1);
}
