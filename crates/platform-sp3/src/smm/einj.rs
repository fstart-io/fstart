//! Native EINJ dispatcher. Called only by the shared SMM dispatch owner.
//! The shared request is copied once and validated before any PSP command.

use crate::einj::*;
use core::cell::UnsafeCell;
use fstart_core::services::timer::Timer;
use fstart_driver_zen::psp::{
    command::{C2pBuffer, Command, SMM_FLAG_OFFSET},
    mailbox::{C2pTransport, Mailbox, PspError},
    ras::{Control, CorrectableMemory, MemoryTarget},
};
use fstart_driver_zen::smn::Smn;
use fstart_pci::ConfigRegionAccess;

struct State {
    enabled: bool,
    active: bool,
    failed: bool,
    attempts: u8,
    saved_nbio: Option<(u8, [u32; 3])>,
}
struct Shared(UnsafeCell<State>);
// SAFETY: only the shared SMM dispatcher owner accesses STATE, never APs.
unsafe impl Sync for Shared {}
static STATE: Shared = Shared(UnsafeCell::new(State {
    enabled: false,
    active: false,
    failed: false,
    attempts: 0,
    saved_nbio: None,
}));

// Keep this crate-local: exporting a static address across crate boundaries
// introduces a forbidden GOT relocation into the standalone SMM image.
#[inline(never)]
fn state() -> *mut State {
    STATE.0.get()
}

fn write(offset: u64, value: u64) {
    // SAFETY: fixed, reserved communication page; no OS-supplied pointer.
    unsafe { ((BASE + offset) as *mut u64).write_volatile(value) }
}

pub(super) fn status(error: PspError) -> u64 {
    match error {
        PspError::Status { status, .. } => u64::from(status),
        PspError::Timeout(_) => 0x1_0000,
        PspError::Recovery => 0x2_0000,
        PspError::Unsupported { .. } => 0x3_0000,
    }
}

/// Use only the C2P page registered with SmmInfo, never the SMM stack or
/// shared ACPI buffer. A timed-out request leaves this page untouched on
/// subsequent calls: the PSP may still own it. No retry after any failure.
unsafe fn send<C: Command>(
    mailbox: &Mailbox<impl C2pTransport>,
    timer: &impl Timer,
    page: *mut u8,
    request: C,
) -> Result<(), PspError> {
    let buffer = page.cast::<C2pBuffer<C>>();
    // SAFETY: registered SMRAM page, caller owns shared dispatch.
    unsafe {
        buffer.write(C2pBuffer::new(request));
        mailbox.send_in_smm(timer, &mut *buffer, page.add(SMM_FLAG_OFFSET as usize))
    }
}

/// Once, after successful SmmInfo and Rome endpoint validation.
/// `supported` includes board opt-in, generation and topology. Match the
/// donor ordering: enable the PSP first, then validate live map/ECC gates.
pub unsafe fn init(
    supported: bool,
    mailbox: &Mailbox<impl C2pTransport>,
    timer: &impl Timer,
    page: *mut u8,
    sockets: u8,
    targets: &[u64; 2],
    validate_target: impl FnOnce() -> bool,
) {
    write(SUPPORTED, 0);
    write(SOCKET_COUNT, u64::from(sockets));
    for (s, &address) in targets.iter().enumerate() {
        write(TARGETS + s as u64 * 8, address);
    }
    write(TRIGGER_POINTER, BASE + TRIGGER);
    write(BUSY, 0);
    write(STATUS, 1);
    write(LEGACY_TYPE, 0);
    for offset in (PARAMETERS..PARAMETERS + 40).step_by(8) {
        write(offset, 0);
    }
    for offset in (BACKEND_STATUS..BACKEND_STATUS + 32).step_by(8) {
        write(offset, u64::MAX);
    }
    if !supported {
        return;
    }
    // SAFETY: shared dispatch owner, registered page, first use.
    let result = unsafe { send(mailbox, timer, page, Control::ENABLE) };
    write(BACKEND_STATUS, result.map_or_else(status, |_| 0));
    // SAFETY: only this dispatcher accesses the state.
    let state = unsafe { &mut *state() };
    state.enabled = result.is_ok() && validate_target();
    state.failed = result.is_err();
    if state.enabled {
        write(SUPPORTED, MEMORY_CORRECTABLE.into());
        write(STATUS, 0);
    }
}

unsafe fn end(
    state: &mut State,
    mailbox: &Mailbox<impl C2pTransport>,
    timer: &impl Timer,
    page: *mut u8,
    smns: &[Smn<impl ConfigRegionAccess>; 2],
) -> Result<(), PspError> {
    state.active = false;
    let result = unsafe { send(mailbox, timer, page, Control::END) };
    write(BACKEND_STATUS + 24, result.map_or_else(status, |_| 0));
    if result.is_ok() {
        if let Some((socket, [control, low, high])) = state.saved_nbio.take() {
            let Some(smn) = smns.get(usize::from(socket)) else {
                return Err(PspError::Status {
                    command: 0x22,
                    status: 2,
                });
            };
            // Restore the target socket, not the socket executing SMM.
            // Exact donor ordering; preserve this endpoint's OS index.
            unsafe {
                smn.with_saved_index(|s| {
                    s.write32(0x1d840, 0);
                    s.write32(0x1d848, low);
                    s.write32(0x1d84c, high);
                    s.write32(0x1d840, control);
                });
            }
        }
    }
    result
}

pub unsafe fn dispatch(
    command: u8,
    mailbox: &Mailbox<impl C2pTransport>,
    timer: &impl Timer,
    page: *mut u8,
    target: impl Fn(u64) -> Option<MemoryTarget>,
    owned: &[u64; 2],
    smns: &[Smn<impl ConfigRegionAccess>; 2],
) {
    // SAFETY: only the shared SMM dispatcher owner accesses the state.
    let s = unsafe { &mut *state() };
    if !s.enabled || s.failed {
        write(STATUS, 1);
        return;
    }
    write(BUSY, 1);
    let result = match command {
        SW_BEGIN if !s.active && s.attempts < 4 => {
            // Clear stale parameters; every operation needs fresh type
            // and address input, including after an invalid request.
            for offset in (PARAMETERS..PARAMETERS + 40).step_by(8) {
                write(offset, 0);
            }
            // SAFETY: dispatch ownership and registered C2P page.
            let result = unsafe { send(mailbox, timer, page, Control::BEGIN) };
            write(BACKEND_STATUS + 8, result.map_or_else(status, |_| 0));
            s.active = result.is_ok();
            result.map(|_| 0)
        }
        SW_EXECUTE if s.active && s.attempts < 4 => {
            s.attempts += 1;
            // SAFETY: fixed, bounded 36-byte request. Copy once so later
            // OS changes cannot change the validated injection target.
            let p = unsafe { ((BASE + PARAMETERS) as *const Parameters).read_volatile() };
            let request = s
                .saved_nbio
                .is_none()
                .then(|| p.target(owned))
                .flatten()
                .and_then(|addr| target(addr).map(|t| (addr, t)))
                .and_then(|(addr, t)| smns.get(usize::from(t.socket)).map(|smn| (addr, t, smn)));
            match request {
                None => {
                    // Linux returns immediately on INVALID and does not
                    // execute END_OPERATION. Close the PSP operation here.
                    unsafe { end(s, mailbox, timer, page, smns) }.map(|_| 2)
                }
                Some((address, target, smn)) => {
                    // Patch only the trigger's fixed GAS address, never
                    // a pointer obtained from untrusted shared memory.
                    write(TRIGGER + 24, address);
                    // The donor saves these before action 4 and restores
                    // them after action 3 (1d54 / 1f38). Do not leave the
                    // NBIO address-match state changed by an experiment.
                    if s.saved_nbio.is_none() {
                        s.saved_nbio = Some((target.socket, unsafe {
                            smn.with_saved_index(|s| {
                                [s.read32(0x1d840), s.read32(0x1d848), s.read32(0x1d84c)]
                            })
                        }));
                    }
                    // SAFETY: as above; no arbitrary address/type reaches PSP.
                    let result = unsafe {
                        send(
                            mailbox,
                            timer,
                            page,
                            CorrectableMemory::new(address, target),
                        )
                    };
                    write(BACKEND_STATUS + 16, result.map_or_else(status, |_| 0));
                    result.map(|_| 0)
                }
            }
        }
        SW_END if s.active => unsafe { end(s, mailbox, timer, page, smns) }.map(|_| 0),
        _ => Ok(2),
    };
    let acpi_status = match result {
        Ok(status) => status,
        Err(error) => {
            // A completed PSP rejection can be closed and restored. A
            // timeout cannot: the PSP may still be using the C2P page.
            if matches!(error, PspError::Status { .. }) && s.active {
                let _ = unsafe { end(s, mailbox, timer, page, smns) };
            }
            s.failed = true;
            1
        }
    };
    write(STATUS, acpi_status);
    write(BUSY, 0);
}
