//! Huashan-only SMM pieces (FCH_SP3 §5): Milan's FCH register lock through
//! the PSP (SW SMI D2) and Rome's CF9 I/O trap.
//!
//! The Sx entry with the V11 USB/sleep messages is in
//! [`super::super::common::sx`].

use fstart_core::services::Timer;

use super::super::FchHw;
use super::super::smi::{self, Route, source};
use crate::psp::command::{C2pBuffer, LockFchReg};
use crate::psp::mailbox::{C2pTransport, Mailbox, PspError};

/// Milan late task af78: raise SW SMI D2 at ReadyToBoot; the SMM handler
/// then calls [`lock_fch_registers`].
pub fn request_fch_lock<H: FchHw>(hw: &H) {
    smi::software_smi(hw.io(), smi::sw::FCH_LOCK);
}

/// The SW SMI D2 handler body (Milan): send LockFCHReg (C2P 0x30) with the
/// SMM flag set. The donor's second LockFCHReg (0x3A) has an untraced gate
/// and is not sent.
///
/// # Safety
/// `flag` is the SMM flag byte registered with SmmInfo; `buffer` is in SMM
/// memory the PSP may access.
pub unsafe fn lock_fch_registers<T: C2pTransport>(
    psp: &Mailbox<T>,
    timer: &impl Timer,
    buffer: &mut C2pBuffer<LockFchReg>,
    flag: *mut u8,
) -> Result<(), PspError> {
    // SAFETY: the caller's contract.
    unsafe { psp.send_in_smm(timer, buffer, flag) }
}

/// The I/O trap slot Rome's Cf9ResetSmm uses here.
pub const CF9_TRAP: u8 = 0;

/// Rome: trap writes to `CF9h` into SMM (the donor registers an I/O trap on
/// CF9h through the FCH dispatcher; its handler body was not traced, the
/// integrator's handler performs the reset).
pub fn arm_cf9_trap<H: FchHw>(hw: &H) {
    smi::io_trap(hw.acpi(), CF9_TRAP, 0xCF9, true);
}

/// Whether the pending SMI is the CF9 trap; acknowledges it.
pub fn take_cf9_trap<H: FchHw>(hw: &H) -> bool {
    let trap = source::io_trap(CF9_TRAP);
    let pending = smi::is_pending(hw.acpi(), trap);
    if pending {
        smi::clear(hw.acpi(), trap);
    }
    pending
}

/// Turn the trap off again.
pub fn disarm_cf9_trap<H: FchHw>(hw: &H) {
    smi::set_route(hw.acpi(), source::io_trap(CF9_TRAP), Route::Disabled);
}

#[cfg(test)]
mod tests {
    use super::super::super::model::fch;
    use super::*;

    #[test]
    fn cf9_trap_round_trip() {
        let hw = fch();
        arm_cf9_trap(&hw);
        assert_eq!(hw.acpi.get(0xec0, 2), 0xcf9);
        assert!(!take_cf9_trap(&hw));
        hw.acpi.put(0x290, 4, 1 << 20);
        assert!(take_cf9_trap(&hw));
        assert_eq!(hw.acpi.writes_to(0x290), vec![1 << 20]);
    }

    #[test]
    fn d2_request() {
        let hw = fch();
        request_fch_lock(&hw);
        assert_eq!(hw.io.writes(), vec![(0xb2, 1, 0xd2)]);
    }
}
