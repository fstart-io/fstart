// SPDX-License-Identifier: MIT OR Apache-2.0
//! The x86 side of the PSP's boot: the SMM info right after SMM is set up,
//! LockDFReg after the last DF write and
//! BootDone right before the payload.
use super::{
    C2pBuffer, C2pTransport, Command, Mailbox, MmioTransport, PspError, Session, Storage, Variant,
    command::{
        BootDone, Default32, DisableDrtm, LockDfReg, ResetTpmEstablishment, SmmInfo, SmmLock,
    },
};
use fstart_arch::x86::cpu::amd::smm::InstalledTseg;
use fstart_core::services::Timer;

/// Die 0's PSP mailbox, the one owner of the boot, after it took the SMM
/// info. BootDone ends its use.
pub type Psp = Session<MmioTransport>;

/// PSP-to-core buffer, then the core-to-PSP page with the SMM flag at its
/// +0xc00, as the vendor firmware lays them out in TSEG.
const P2C_SIZE: u64 = 0x1000;
const PSP_REGION: u64 = P2C_SIZE + 0x1000;

/// The SMM info, its buffers in unused TSEG.
fn smm_info(tseg: &InstalledTseg) -> Option<SmmInfo> {
    let p2c = tseg.free.start;
    (tseg.free.end - p2c >= PSP_REGION).then(|| SmmInfo {
        p2c_size: P2C_SIZE,
        // The native flows send the reserved descriptors zeroed.
        reserved: Default::default(),
        // Without TValid and the memory types.
        ..SmmInfo::new(tseg.base, tseg.mask & !0xffff, p2c, p2c + P2C_SIZE)
    })
}

/// Die 0's PSP mailbox at `bar`, after it got the SMM info. `None` if that
/// failed: the mailbox is unusable then, and LockDFReg without the SMM info
/// would break device DMA anyway.
pub fn notify_smm(
    bar: u32,
    variant: Variant,
    tseg: &InstalledTseg,
    timer: &impl Timer,
) -> Option<Psp> {
    let Some(info) = smm_info(tseg) else {
        fstart_log::error!("PSP: no room for the SMM buffers in TSEG");
        return None;
    };
    if bar == 0 {
        fstart_log::error!("PSP: BAR of die 0 isn't enabled");
        return None;
    }
    static mut STORAGE: Storage = Storage::new();
    // SAFETY: the BAR was mapped and locked by the silicon init; BSP only.
    let mailbox = Mailbox::new(unsafe { MmioTransport::new(bar.into()) }, variant);
    // SAFETY: firmware BSS in the early region, identity mapped and
    // reserved until reset; this is its only reference.
    let session = Session::new(mailbox, unsafe { &mut *core::ptr::addr_of_mut!(STORAGE) });
    let mut psp = match session {
        Ok(psp) => psp,
        Err(error) => {
            let (why, value) = error.parts();
            fstart_log::error!("PSP: mailbox: {} {:#x}", why, value);
            return None;
        }
    };
    match psp.send(timer, info) {
        Ok(_) => {
            fstart_log::info!(
                "PSP: SMM info sent, P2C {:#x}, C2P {:#x}",
                info.p2c_buffer,
                info.c2p_smm_buffer
            );
            Some(psp)
        }
        Err(error) => {
            {
                let (why, value) = error.parts();
                fstart_log::error!("PSP: SMM info failed: {} {:#x}", why, value)
            };
            None
        }
    }
}

/// LockDFReg, after the last DF write; it needs the SMM info. The DF must
/// not reach the OS unlocked, so a missing mailbox or a failed lock is an
/// error the flow halts on, as SP3 does.
pub fn lock_df_registers(psp: Option<&mut Psp>, timer: &impl Timer) -> Result<(), &'static str> {
    let psp = psp.ok_or("PSP SMM info (needed to lock the DF)")?;
    match psp.send(timer, Default32::<LockDfReg>::new()) {
        Ok(_) => {
            fstart_log::info!("PSP: DF registers locked");
            Ok(())
        }
        Err(error) => {
            {
                let (why, value) = error.parts();
                fstart_log::error!("PSP: LockDFReg failed: {} {:#x}", why, value)
            };
            Err("PSP LockDFReg")
        }
    }
}

/// A command's name with the error it ended in.
fn sent<C: Command>(
    result: Result<C2pBuffer<C>, PspError>,
) -> Result<(), (&'static str, PspError)> {
    result.map(drop).map_err(|error| (C::NAME, error))
}

/// LockDFReg through `psp`, header only, as Milan's and the RS SoCs' PSP
/// DXE drivers send it once PCI is enumerated: no DF write follows. It
/// needs the SMM info: without it the DF lock breaks device DMA.
pub fn lock_df<T: C2pTransport>(
    psp: &mut Session<T>,
    timer: &impl Timer,
) -> Result<(), (&'static str, PspError)> {
    sent(psp.send(timer, LockDfReg))
}

/// The PSP's locks through `psp` once PCI is enumerated and SMM is
/// locked, as the RS SoCs' PSP DXE driver sends them (`AmdPspDxeV2Rs`,
/// sp5 `closure-runtime/lifecycle/REPORT.md`): LockDFReg from its
/// PciEnumerationComplete notification (`2588`/`2754`, [`lock_df`]), then
/// SmmLock from its DxeSmmReadyToLock notification (`26C8`/`2894`). The
/// vendor ignores a refusal; the first one ends this, with the command's
/// name, and the caller halts on it.
pub fn lock<T: C2pTransport>(
    psp: &mut Session<T>,
    timer: &impl Timer,
) -> Result<(), (&'static str, PspError)> {
    lock_df(psp, timer)?;
    sent(psp.send(timer, SmmLock))
}

/// The PSP's ReadyToBoot through `psp`, as the PSP DXE driver sends
/// it from Milan on (Milan's, and on the RS SoCs `AmdPspDxeV2Rs`
/// ReadyToBoot `22BC`/`22E0`, sp5 `closure-runtime/lifecycle/REPORT.md`):
/// the DRTM capability off (the vendors' default policy), the TPM's
/// establishment flag reset if there is a `tpm`, then BootDone; on the
/// older generations BootDone alone. The vendors send the TPM's when one
/// answers at `0xFED4_0000`, after their TPM driver started it; the
/// caller decides. Afterwards the PSP takes C2P commands only from SMM
/// with the SMM flag set; the RAS mailbox of EINJ ([`super::einj`]) and
/// the P2C requests to SMM stay. The first failure ends it, with the
/// command's name; the session takes no command after it.
///
/// # Safety
/// The firmware is done with the CPUs and SMM is locked.
pub unsafe fn ready_to_boot<T: C2pTransport>(
    psp: &mut Session<T>,
    timer: &impl Timer,
    tpm: bool,
) -> Result<(), (&'static str, PspError)> {
    if DisableDrtm::supported(psp.silicon()) {
        sent(psp.send(timer, DisableDrtm::new()))?;
        if tpm {
            sent(psp.send(timer, ResetTpmEstablishment))?;
        }
    }
    sent(psp.send(timer, BootDone))
}

/// BootDone: afterwards the PSP only takes commands with SMM buffers.
///
/// # Safety
/// The firmware is done with the CPUs and SMM is locked.
pub unsafe fn boot_done(psp: Option<&mut Psp>, timer: &impl Timer) {
    let Some(psp) = psp else {
        return;
    };
    // On error the transition may already have happened: the session
    // refuses a retry.
    match psp.send(timer, Default32::<BootDone>::new()) {
        Ok(_) => fstart_log::info!("PSP: boot done"),
        Err(error) => {
            let (why, value) = error.parts();
            fstart_log::error!("PSP: BootDone failed: {} {:#x}", why, value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::mailbox::{Wait, model::Model};
    use super::*;
    use fstart_core::services::ManualTimer;

    const READY: u32 = 0x8000_0000;

    fn commands(model: &Model) -> Vec<u8> {
        model.0.borrow().log.iter().map(|&(id, _)| id).collect()
    }

    fn session(model: &Model) -> Session<&Model> {
        let storage = std::boxed::Box::leak(std::boxed::Box::new(Storage::new()));
        Session::new(Mailbox::new(model, Variant::Gn).with_timeout(100), storage).unwrap()
    }

    #[test]
    fn locks_the_df_then_smm() {
        let model = Model::new(READY, |_, _| 0);
        assert_eq!(lock(&mut session(&model), &ManualTimer::new()), Ok(()));
        assert_eq!(commands(&model), [0x1b, 0x6c]);
    }

    #[test]
    fn a_refused_df_lock_ends_the_locks() {
        let model = Model::new(READY, |id, _| if id == 0x1b { 5 } else { 0 });
        let mut psp = session(&model);
        let (what, _) = lock(&mut psp, &ManualTimer::new()).unwrap_err();
        assert_eq!(what, "LockDFReg");
        assert_eq!(commands(&model), [0x1b]);
        // No BootDone after it either.
        let (what, error) =
            unsafe { ready_to_boot(&mut psp, &ManualTimer::new(), false) }.unwrap_err();
        assert_eq!((what, error), ("DisableDrtmCapability", PspError::Poisoned));
        assert_eq!(commands(&model), [0x1b]);
    }

    /// LockDFReg times out; the PSP completes it later, writing its status
    /// into the buffer it was given. That buffer is the session's retained
    /// storage, and nothing reuses it: no further command is sent, so the
    /// late write lands in a buffer no request owns.
    #[test]
    fn a_late_completion_after_a_timeout_writes_a_buffer_nobody_reuses() {
        let model = Model::new(READY, |_, _| 0);
        model.0.borrow_mut().hang = true;
        let mut psp = session(&model);
        assert_eq!(
            lock(&mut psp, &ManualTimer::new()),
            Err(("LockDFReg", PspError::Timeout(Wait::Done)))
        );
        let (_, buffer) = model.0.borrow().log[0];
        // The late completion: the status into the header, the command
        // register done.
        // SAFETY: the session's leaked storage, which the model was given.
        unsafe { ((buffer + 4) as *mut u32).write_volatile(0x5a) };
        {
            let mut p = model.0.borrow_mut();
            p.hang = false;
            p.cmd = READY;
        }
        assert_eq!(
            unsafe { ready_to_boot(&mut psp, &ManualTimer::new(), true) },
            Err(("DisableDrtmCapability", PspError::Poisoned))
        );
        assert_eq!(
            lock(&mut psp, &ManualTimer::new()).unwrap_err().1,
            PspError::Poisoned
        );
        assert_eq!(commands(&model), [0x1b]);
        // SAFETY: as above.
        assert_eq!(
            unsafe { ((buffer + 4) as *const u32).read_volatile() },
            0x5a
        );
    }

    #[test]
    fn ready_to_boot_ends_with_boot_done() {
        let model = Model::new(READY, |_, _| 0);
        let mut psp = session(&model);
        unsafe { ready_to_boot(&mut psp, &ManualTimer::new(), true) }.unwrap();
        assert_eq!(commands(&model), [0x62, 0x61, 0x06]);
        assert!(psp.finished());
    }
}
