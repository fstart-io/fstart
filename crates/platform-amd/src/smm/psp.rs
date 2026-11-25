// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PSP's side of SMM, for every SoC whose handler registers with it
//! (SP3: [`crate::sp3::smm`], RS: [`crate::rs::smm`]): the P2C buffer and
//! the C2P page in the handler's SMRAM, SmmInfo with them, the SMM-flagged
//! C2P commands and the answer to the PSP's SMI.
//!
//! AGESA sends SmmInfo from its SMM driver's entry (AmdPspSmmV2Ssp,
//! AmdPspSmmV2Rs); sending it from the handler, on
//! [`super::sw::PSP_SMM_INFO`], puts the buffers in the handler's own SMRAM
//! statics, whose addresses normal mode cannot know before the image is
//! placed. SmmInfo routes the
//! PSP's SMI (SmiStat1\[1\], raised by the PSP setting SmiTrig0\[25\];
//! `fch::smi::route_psp` before it) to SMI: the handler must answer every
//! P2C request and drop that trigger ([`answer`]), or the source stays
//! asserted and the FCH raises SMIs forever.
//!
//! Only the platform's handlers use these: the statics stay in this crate
//! (a static exported to the board crate's SMM image would need a GOT
//! relocation, which the SMM image builder rejects).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use fstart_arch::x86::cpu::amd::msr::{SMM_ADDR, SMM_MASK};
use fstart_arch::x86::cpu::msr::{MsrAccess, NativeMsr};
use fstart_core::services::timer::Timer;
use fstart_driver_amd::fch::smi::{self, source};
use fstart_driver_amd::psp::Variant;
use fstart_driver_amd::psp::command::{
    C2pBuffer, Command, P2C_BUFFER_SIZE, SMM_FLAG_OFFSET, SmmInfo,
};
use fstart_driver_amd::psp::mailbox::{C2pTransport, Mailbox, PspError};
use fstart_driver_amd::psp::p2c;
use fstart_driver_amd::window::Window;
use tock_registers::interfaces::Readable;

#[repr(C, align(4096))]
struct Page<const N: usize>(UnsafeCell<[u8; N]>);
// SAFETY: only the SMM rendezvous owner touches the pages.
unsafe impl<const N: usize> Sync for Page<N> {}

/// The PSP-to-CPU buffer and the C2P page (command buffer at +0, SMM flag
/// at +0xC00), in the handler's SMRAM.
static P2C: Page<{ P2C_BUFFER_SIZE as usize }> =
    Page(UnsafeCell::new([0; P2C_BUFFER_SIZE as usize]));
static C2P: Page<0x1000> = Page(UnsafeCell::new([0; 0x1000]));

/// The C2P page SmmInfo registers: a command buffer at +0, the SMM flag
/// at +0xC00 (`psp::command::SMM_FLAG_OFFSET`).
#[inline(always)]
fn c2p() -> *mut u8 {
    C2P.0.get().cast()
}

/// Whose the C2P page is: nobody's until SmmInfo registered it, then the
/// handler's between commands. A command whose completion is uncertain
/// ([`PspError::uncertain`]) leaves it to the PSP for the rest of the boot:
/// the PSP may still write its status into it, so no later command, of
/// any SMI, may put its request there.
static PAGE: AtomicU8 = AtomicU8::new(UNREGISTERED);
const UNREGISTERED: u8 = 0;
const FREE: u8 = 1;
const PSPS: u8 = 2;

/// The page after `result` of a command in it.
#[inline(always)]
fn returned(result: &Result<(), PspError>) {
    PAGE.store(
        match result {
            Err(error) if error.uncertain() => PSPS,
            _ => FREE,
        },
        Ordering::Release,
    );
}

/// SmmInfo (PSP_APOB §3.3) with this handler's buffers, the P2C mailbox
/// idle and ready first as on every donor ([`p2c::READY`]); `info` sees
/// the SoC's SmmInfo before it is sent. Once per boot, as AGESA's driver
/// entry: `None` from the second call on.
///
/// The request travels in the C2P page it registers, not on the SMM
/// stack: after a timeout the PSP may still write its status into it,
/// and nothing uses the page after a failed registration.
#[inline(always)]
pub(crate) fn register<T: C2pTransport>(
    mailbox: &Mailbox<T>,
    timer: &impl Timer,
    info: impl FnOnce(SmmInfo) -> SmmInfo,
) -> Option<Result<(), PspError>> {
    static SENT: AtomicBool = AtomicBool::new(false);
    if SENT.swap(true, Ordering::AcqRel) {
        return None;
    }
    // SAFETY: SMM, ring 0; the SMM range MSRs.
    let cpu = unsafe { NativeMsr::new() };
    let p2c = P2C.0.get();
    let page = c2p();
    // SAFETY: SMRAM static, owned by the rendezvous owner.
    unsafe { p2c::ready(p2c.cast()) };
    let smm_info = SmmInfo::new(
        cpu.reg(SMM_ADDR).get(),
        cpu.reg(SMM_MASK).get(),
        p2c as u64,
        page as u64,
    );
    let buffer = page.cast::<C2pBuffer<SmmInfo>>();
    // SAFETY: the C2P page is SMRAM owned by the rendezvous owner, page
    // aligned and larger than the request.
    let result = unsafe {
        buffer.write(C2pBuffer::new(info(smm_info)));
        mailbox.send(timer, &mut *buffer)
    };
    // A refused registration leaves the page unregistered.
    if result.is_ok() || result.is_err_and(|error| error.uncertain()) {
        returned(&result);
    }
    Some(result)
}

/// Send `request` from SMM through the C2P page, the SMM flag set around
/// it ([`Mailbox::send_in_smm`]): the one way into the page after SmmInfo,
/// for every SoC's handler. [`PspError::Poisoned`], with the page
/// untouched, before SmmInfo registered it and after a command in it
/// whose completion is uncertain ([`PAGE`]).
///
/// # Safety
/// The caller owns the rendezvous.
pub(crate) unsafe fn send<C: Command, T: C2pTransport>(
    mailbox: &Mailbox<T>,
    timer: &impl Timer,
    request: C,
) -> Result<(), PspError> {
    if PAGE
        .compare_exchange(FREE, PSPS, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(PspError::Poisoned);
    }
    let page = c2p();
    let buffer = page.cast::<C2pBuffer<C>>();
    // SAFETY: the registered SMRAM page, owned by the rendezvous owner and
    // not the PSP's; the flag byte is inside it.
    let result = unsafe {
        buffer.write(C2pBuffer::new(request));
        mailbox.send_in_smm(timer, &mut *buffer, page.add(SMM_FLAG_OFFSET as usize))
    };
    returned(&result);
    result
}

/// The PSP's SMI: answer its P2C request (AmdPspP2CmboxV2Ssp `0x720`) and
/// acknowledge the SMI, SmiTrig0\[25\] dropped (its `0x944`) and the
/// source cleared ([`p2c::answer`]). The acknowledgement is this
/// function's alone: the dispatcher leaves the source to it
/// ([`smi::Pending::acknowledged`]), so the status of a request the PSP
/// sends later stays for the next SMI. The buffer says READY only when
/// the SMI has ended ([`release`]). Returns the PSP's command.
#[inline(always)]
pub(crate) fn answer<W: Window>(acpi: &W, pending: &smi::Pending) -> u32 {
    answer_of(acpi, pending, Variant::Gn)
}

/// [`answer`] by the rules of the `generation` PSP's P2C handler
/// ([`p2c::answer_of`]: the WH PSP's differ).
#[inline(always)]
pub(crate) fn answer_of<W: Window>(acpi: &W, pending: &smi::Pending, generation: Variant) -> u32 {
    pending.acknowledged(source::PSP);
    // SAFETY: the P2C static is SMRAM owned by the rendezvous owner; ring 0.
    let command = unsafe {
        p2c::answer_of(
            generation,
            P2C.0.get().cast(),
            fstart_arch::x86::writeback_invalidate_caches,
            || smi::acknowledge_psp(acpi),
        )
    };
    ANSWERED.store(true, Ordering::Release);
    command
}

/// An [`answer`] waits for [`release`].
static ANSWERED: AtomicBool = AtomicBool::new(false);

/// After the SMI that [`answer`]ed ended (the dispatcher's EOS): command 0
/// and READY, and the PSP may send its next request. Nothing may write
/// SmiTrig0 after this before RSM ([`p2c::answer`] says why).
#[inline(always)]
pub(crate) fn release() {
    if ANSWERED.swap(false, Ordering::AcqRel) {
        // SAFETY: as in `answer`.
        unsafe {
            p2c::release(
                P2C.0.get().cast(),
                fstart_arch::x86::writeback_invalidate_caches,
            )
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_core::services::ManualTimer;
    use fstart_driver_amd::psp::Variant;
    use fstart_driver_amd::psp::command::LockFchReg;
    use fstart_driver_amd::psp::mailbox::{C2pReg, Wait};
    use std::cell::RefCell;

    /// A PSP whose C2P completes every command unless it hangs.
    struct Psp {
        cmd: RefCell<u32>,
        hang: bool,
        sent: RefCell<u32>,
    }
    impl C2pTransport for &Psp {
        fn read(&self, reg: C2pReg) -> u32 {
            match reg {
                C2pReg::Cmd => *self.cmd.borrow(),
                _ => 0,
            }
        }
        fn write(&self, reg: C2pReg, value: u32) {
            if reg == C2pReg::Cmd {
                *self.sent.borrow_mut() += 1;
                *self.cmd.borrow_mut() = if self.hang { value } else { 1 << 31 };
            }
        }
    }

    /// The handler's page states, in one test: they are one static.
    #[test]
    fn the_page_is_not_reused_after_an_uncertain_completion() {
        let timer = ManualTimer::new();
        let ok = Psp {
            cmd: RefCell::new(1 << 31),
            hang: false,
            sent: RefCell::new(0),
        };
        let mailbox = Mailbox::new(&ok, Variant::Gn).with_timeout(100);
        // Not before SmmInfo registered it.
        assert_eq!(
            unsafe { send(&mailbox, &timer, LockFchReg) },
            Err(PspError::Poisoned)
        );
        assert_eq!(*ok.sent.borrow(), 0);
        PAGE.store(FREE, Ordering::Release);
        assert_eq!(unsafe { send(&mailbox, &timer, LockFchReg) }, Ok(()));
        // A timeout: the PSP may complete it late, into the page.
        let hung = Psp {
            cmd: RefCell::new(1 << 31),
            hang: true,
            sent: RefCell::new(0),
        };
        let late = Mailbox::new(&hung, Variant::Gn).with_timeout(100);
        assert_eq!(
            unsafe { send(&late, &timer, LockFchReg) },
            Err(PspError::Timeout(Wait::Done))
        );
        // The PSP completes it late; no later command of any SMI gets
        // the page.
        *hung.cmd.borrow_mut() = 1 << 31;
        let before = *ok.sent.borrow();
        assert_eq!(
            unsafe { send(&mailbox, &timer, LockFchReg) },
            Err(PspError::Poisoned)
        );
        assert_eq!(*ok.sent.borrow(), before);
    }
}
