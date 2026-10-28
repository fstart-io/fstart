// SPDX-License-Identifier: MIT OR Apache-2.0
//! The x86 side of the PSP's boot: the SMM info right after SMM is set up,
//! LockDFReg after the last DF write and
//! BootDone right before the payload.
use fstart_arch::x86::cpu::amd::smm::InstalledTseg;
use fstart_core::services::Timer;
use fstart_driver_amd::psp::{
    Mailbox, MmioTransport, Session, Storage, Variant,
    command::{BootDone, Default32, LockDfReg, SmmInfo},
};

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
