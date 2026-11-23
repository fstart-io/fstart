// SPDX-License-Identifier: MIT OR Apache-2.0
//! Package PRMRR placement consumer, not core SGX/SMM initialization.
//! Native memory_map_native.c:393–429; intelblocks/msr.h:51–53 and
//! cpulib.c:get_valid_prmrr_size. No PRMRR MSR is accessed until MTRRCAP permits it.
use fstart_arch::x86::cpu::msr::{Msr, MsrAccess};
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};

register_bitfields![u64,
    Cap [ PRMRR OFFSET(12) NUMBITS(1) [] ],
    Base [ ADDRESS OFFSET(12) NUMBITS(40) [] ],
    Mask [
        LOCK OFFSET(10) NUMBITS(1) [],
        VALID OFFSET(11) NUMBITS(1) [],
        ADDRESS OFFSET(12) NUMBITS(40) []
    ]
];
const CAP: Msr<Cap::Register> = Msr::new(0xfe);
const VALID_CONFIG: Msr = Msr::new(0x1fb);
const BASE: Msr<Base::Register> = Msr::new(0x2f4);
const MASK: Msr<Mask::Register> = Msr::new(0x2f5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    InvalidSize,
    InvalidAddress,
    Conflict,
    LockedMismatch,
    Readback,
}

/// Explicit board PRM-feature policy. `maximum_mib` has the native config's
/// units; the greatest hardware-supported size <= this value is selected.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Actual native PRM feature gate: configured AND CPUID-supported SGX
    /// (or the source helper's configured/supported Key Locker gate).
    pub features_enabled: bool,
    pub maximum_mib: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    capable: bool,
    size_mib: u32,
    physical_bits: u32,
}
impl Request {
    pub fn read(cpu: &impl MsrAccess, policy: Policy) -> Result<Self, Error> {
        let (maximum, _, _, _) = fstart_arch::x86::cpuid(0x8000_0000);
        if maximum < 0x8000_0008 {
            return Err(Error::InvalidAddress);
        }
        let (width, _, _, _) = fstart_arch::x86::cpuid(0x8000_0008);
        let physical_bits = fstart_arch::x86::physical_address_bits();
        if width & 0xff != physical_bits {
            return Err(Error::InvalidAddress);
        }
        Self::read_width(cpu, policy, physical_bits)
    }
    fn read_width(cpu: &impl MsrAccess, policy: Policy, physical_bits: u32) -> Result<Self, Error> {
        if !(32..=52).contains(&physical_bits) {
            return Err(Error::InvalidAddress);
        }
        if cpu.reg(CAP).get() == u64::MAX {
            return Err(Error::Unsupported);
        }
        let capable = cpu.reg(CAP).is_set(Cap::PRMRR);
        let mut size_mib = 0;
        if policy.features_enabled && policy.maximum_mib != 0 {
            if !capable {
                return Err(Error::Unsupported);
            }
            let supported = cpu.reg(VALID_CONFIG).get();
            if supported & !0x1ff != 0 {
                return Err(Error::InvalidSize);
            }
            for i in (0..=8).rev() {
                let size = 1 << i;
                if supported & u64::from(size) != 0 && size <= u32::from(policy.maximum_mib) {
                    size_mib = size;
                    break;
                }
            }
            // Native's 2–31 MiB ignore/fallback is not a valid reservation.
            if size_mib == 0 || size_mib != 1 && size_mib < 32 {
                return Err(Error::InvalidSize);
            }
        }
        Ok(Self {
            capable,
            size_mib,
            physical_bits,
        })
    }
    pub fn size_mib(self) -> u32 {
        self.size_mib
    }
    pub(crate) fn address_limit(self) -> u64 {
        1u64 << self.physical_bits
    }
    pub(crate) fn placement(self, base_mib: u32) -> Result<Placement, Error> {
        let base = u64::from(base_mib) << 20;
        let size = u64::from(self.size_mib) << 20;
        if size != 0
            && (base == 0
                || base & (size - 1) != 0
                || base
                    .checked_add(size)
                    .is_none_or(|e| e > 1u64 << self.physical_bits))
        {
            return Err(Error::InvalidAddress);
        }
        Ok(Placement {
            request: self,
            base: if size == 0 { 0 } else { base },
        })
    }
}

/// Geometry only; created by the checked controller map, not a RAM certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    request: Request,
    base: u64,
}
impl Placement {
    pub fn base(self) -> u64 {
        self.base
    }
    pub fn size_mib(self) -> u32 {
        self.request.size_mib
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockStatus {
    Unsupported,
    Locked,
}

/// Call in the native MC-done order, after MC locks and before normal memory
/// mode/PCODE acknowledgement. This routine does not perform either step.
/// The caller owns the package MSRs exclusively and supplies the actual CPU's
/// endpoint; errors after writes are fatal partial programming, NOT rollback.
pub fn lock(cpu: &impl MsrAccess, placement: Placement) -> Result<LockStatus, Error> {
    let Some(wanted_mask) = desired_mask(cpu, placement)? else {
        return Ok(LockStatus::Unsupported);
    };
    let size = u64::from(placement.request.size_mib) << 20;
    let live_mask = cpu.reg(MASK).get();
    let live_base = cpu.reg(BASE).get();
    if live_mask & Mask::LOCK::SET.value != 0 {
        if live_mask != wanted_mask || live_base != placement.base {
            return Err(Error::LockedMismatch);
        }
        return Ok(LockStatus::Locked);
    }
    // Only unconfigured or exact unlocked geometry is owned. No takeover of
    // an active different PRMRR, nor silent clobber of unknown control bits.
    if live_mask != 0 && live_mask != wanted_mask & !Mask::LOCK::SET.value
        || live_base != 0 && live_base != placement.base
    {
        return Err(Error::Conflict);
    }
    if size != 0 {
        cpu.reg(BASE).set(placement.base); // native UC type = zero
        if cpu.reg(BASE).get() != placement.base {
            return Err(Error::Readback);
        }
    }
    cpu.reg(MASK).set(wanted_mask); // even a zero-sized capable PRMRR is locked
    if cpu.reg(MASK).get() != wanted_mask || cpu.reg(BASE).get() != placement.base {
        return Err(Error::Readback);
    }
    Ok(LockStatus::Locked)
}

fn desired_mask(cpu: &impl MsrAccess, placement: Placement) -> Result<Option<u64>, Error> {
    let request = placement.request;
    if cpu.reg(CAP).get() == u64::MAX || cpu.reg(CAP).is_set(Cap::PRMRR) != request.capable {
        return Err(Error::Conflict);
    }
    if !request.capable {
        return Ok(None);
    }
    let valid_bits = ((1u64 << request.physical_bits) - 1) & !0xfff;
    let size = u64::from(request.size_mib) << 20;
    if size != 0 {
        let supported = cpu.reg(VALID_CONFIG).get();
        if supported & !0x1ff != 0 || supported & u64::from(request.size_mib) == 0 {
            return Err(Error::Conflict);
        }
    }
    Ok(Some(if size == 0 {
        Mask::LOCK::SET.value
    } else {
        (valid_bits & !(size - 1)) | Mask::LOCK::SET.value | Mask::VALID::SET.value
    }))
}

#[cfg(test)]
#[path = "uncore_prmrr_tests.rs"]
pub(crate) mod tests;
