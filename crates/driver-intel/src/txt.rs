// SPDX-License-Identifier: MIT OR Apache-2.0
//! Inactive-TXT memory unlock from Intel txtlib.c/native_unlock_memory.
//! Never clear TPM establishment, enter TXT, reset the platform or assume that
//! an unresponsive TPM authorizes unlocking memory.

use embedded_hal::delay::DelayNs;
use fstart_arch::x86::{
    cpu::msr::{Msr, MsrAccess, NativeMsr},
    cpuid,
    mp::{CpuIdentity, CpuVendor},
};
use fstart_driver_tpm::tis::{Access, BASE, Locality};
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields,
};

register_bitfields![u64,
    MemoryLock [
        LOCKED1 OFFSET(1) NUMBITS(1) [],
        LOCKED2 OFFSET(2) NUMBITS(1) []
    ]
];
register_bitfields![u32,
    FirmwareTpmStatus [ FTPM OFFSET(16) NUMBITS(3) [] ]
];
const LOCK: Msr<MemoryLock::Register> = Msr::new(0x2e7);
const UNLOCK: Msr = Msr::new(0x2e6);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Cpu,
    Established,
    TpmTimeout,
    Locked,
    /// No 4-KiB PTT DMA backing/handoff or corresponding reservation exists.
    UnsupportedPtt,
    StatusUnavailable,
}

fn require_no_unreserved_ptt(status: u32) -> Result<(), Error> {
    if status == u32::MAX {
        return Err(Error::StatusUnavailable);
    }
    let status = tock_registers::LocalRegisterCopy::<u32, FirmwareTpmStatus::Register>::new(status);
    if status.read(FirmwareTpmStatus::FTPM) == 7 {
        return Err(Error::UnsupportedPtt);
    }
    Ok(())
}

/// Reject an active PTT handoff until its real DMA buffer is implemented.
/// Recheck at the cold producer and checked consumer boundaries, not just
/// before destructive memory work.
/// # Safety
/// Own the actual decoded, UC TXT public status page at FED30000. No competing
/// firmware agent may change TPM policy during this cold-only lifecycle.
pub(crate) unsafe fn require_no_ptt() -> Result<(), Error> {
    let ftif = unsafe {
        fstart_core::mmio::MmioRegister::<u32, FirmwareTpmStatus::Register>::new(0xfed30800)
    };
    require_no_unreserved_ptt(ftif.get())
}

/// # Safety
/// Firmware CPL0, TXT deliberately disabled and no retained-memory use. Own the
/// actual TPM locality-0 page and TXT public status page (FED30000), decoded
/// and mapped UC, with AP/SMI/TPM users
/// excluded. Call only after saved warm/S3 indications have been rejected.
pub unsafe fn unlock_inactive_memory(delay: &mut impl DelayNs) -> Result<(), Error> {
    if CpuIdentity::current().vendor != CpuVendor::Intel {
        return Err(Error::Cpu);
    }
    // romstage_native.c's FTIF_FTPM_PRESENT predicate. The reference merely
    // warns about its missing PTT handoff. Until real DMA backing and its
    // reservation exist, reject BEFORE training/DID/E820, even without SMX.
    unsafe { require_no_ptt() }?;
    if cpuid(1).2 & ((1 << 5) | (1 << 6)) != (1 << 5) | (1 << 6) {
        return Ok(());
    }
    let cpu = unsafe { NativeMsr::new() };
    let locked =
        || cpu.reg(LOCK).is_set(MemoryLock::LOCKED1) || cpu.reg(LOCK).is_set(MemoryLock::LOCKED2);
    if !locked() {
        return Ok(());
    }
    // Reuse the TIS/PTP register definitions; do not request/seize a locality
    // or send any TPM command. Establishment has inverted polarity.
    let locality = unsafe { &*(BASE as *const Locality) };
    let mut may_unlock = false;
    for sample in 0..=50 {
        let access = locality.access.extract();
        if access.get() == 0xff {
            may_unlock = true;
            break;
        }
        if access.is_set(Access::VALID) {
            if !access.is_set(Access::ESTABLISHMENT) {
                return Err(Error::Established);
            }
            may_unlock = true;
            break;
        }
        if sample != 50 {
            delay.delay_ms(1);
        }
    }
    if !may_unlock {
        return Err(Error::TpmTimeout);
    }
    cpu.reg(UNLOCK).set(0);
    if locked() {
        return Err(Error::Locked);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unreserved_ptt_or_inaccessible_status_never_authorizes_memory() {
        assert_eq!(
            require_no_unreserved_ptt(0x00070000),
            Err(Error::UnsupportedPtt)
        );
        assert_eq!(
            require_no_unreserved_ptt(0x80070001),
            Err(Error::UnsupportedPtt)
        );
        assert_eq!(
            require_no_unreserved_ptt(u32::MAX),
            Err(Error::StatusUnavailable)
        );
        assert_eq!(require_no_unreserved_ptt(0), Ok(()));
        // Firmware TPM support without the full source presence predicate is
        // not an active PTT DMA handoff.
        assert_eq!(require_no_unreserved_ptt(0x00030000), Ok(()));
    }
}
