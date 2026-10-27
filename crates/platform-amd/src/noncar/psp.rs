// SPDX-License-Identifier: MIT OR Apache-2.0
//! The x86 side of the PSP's boot: the SMM info right after SMM is set up,
//! LockDFReg after the last DF write and
//! BootDone right before the payload.
use core::ptr::addr_of_mut;
use fstart_arch::x86::cpu::amd::smm::InstalledTseg;
use fstart_driver_amd::{
    ccx::tsc::DelayNs,
    fch::smi,
    psp::{Buffer, Mailbox, Native, SmmInfo},
};

/// The PSP mailbox of die 0, the one owner of the boot.
pub type Psp = Mailbox<Native>;

/// PSP-to-core buffer, then the core-to-PSP buffer with the SMM flag at its
/// +0xc00, as the vendor firmware lays them out in TSEG.
const P2C_SIZE: u64 = 0x1000;
const C2P_SIZE: u64 = 0xc00;
const PSP_REGION: u64 = 0x2000;

/// The SMM info, its buffers in unused TSEG.
fn smm_info(tseg: &InstalledTseg) -> Option<SmmInfo> {
    let p2c = tseg.free.start;
    (tseg.free.end - p2c >= PSP_REGION).then(|| SmmInfo {
        tseg_base: tseg.base,
        // Without TValid and the memory types.
        tseg_mask: tseg.mask & !0xffff,
        p2c_buffer: p2c,
        p2c_size: P2C_SIZE,
        registers: smi::psp_registers(),
        c2p_buffer: p2c + P2C_SIZE,
        smm_flag: p2c + P2C_SIZE + C2P_SIZE,
    })
}

/// Die 0's PSP mailbox at `bar`, after it got the SMM info. `None` if that
/// failed: the mailbox is unusable then, and LockDFReg without the SMM info
/// would break device DMA anyway.
pub fn notify_smm(bar: u32, tseg: &InstalledTseg, delay: &mut impl DelayNs) -> Option<Psp> {
    static mut BUFFER: Buffer = Buffer::new();
    let Some(info) = smm_info(tseg) else {
        fstart_log::error!("PSP: no room for the SMM buffers in TSEG");
        return None;
    };
    if bar == 0 {
        fstart_log::error!("PSP: BAR of die 0 isn't enabled");
        return None;
    }
    // SAFETY: the BAR was mapped and locked by the silicon init; BSP only.
    // The buffer is firmware BSS in the early region, reserved until reset.
    let mailbox = unsafe { Mailbox::new(Native::at(bar as usize), &mut *addr_of_mut!(BUFFER)) };
    let mut mailbox = match mailbox {
        Ok(mailbox) => mailbox,
        Err(error) => {
            fstart_log::error!("PSP: mailbox: {}", error.name());
            return None;
        }
    };
    match mailbox.smm_info(&info, delay) {
        Ok(()) => {
            fstart_log::info!(
                "PSP: SMM info sent, P2C {:#x}, C2P {:#x}",
                info.p2c_buffer,
                info.c2p_buffer
            );
            Some(mailbox)
        }
        Err(error) => {
            fstart_log::error!("PSP: SMM info failed: {}", error.name());
            None
        }
    }
}

/// LockDFReg, after the last DF write; it needs the SMM info. The DF must
/// not reach the OS unlocked, so a missing mailbox or a failed lock is an
/// error the flow halts on, as SP3 does.
pub fn lock_df_registers(
    psp: Option<&mut Psp>,
    delay: &mut impl DelayNs,
) -> Result<(), &'static str> {
    let psp = psp.ok_or("PSP SMM info (needed to lock the DF)")?;
    match psp.lock_df_registers(delay) {
        Ok(()) => {
            fstart_log::info!("PSP: DF registers locked");
            Ok(())
        }
        Err(error) => {
            fstart_log::error!("PSP: LockDFReg failed: {}", error.name());
            Err("PSP LockDFReg")
        }
    }
}

/// BootDone: afterwards the PSP only takes commands with SMM buffers.
///
/// # Safety
/// The firmware is done with the CPUs and SMM is locked.
pub unsafe fn boot_done(psp: Option<&mut Psp>, delay: &mut impl DelayNs) {
    if let Some(psp) = psp {
        // SAFETY: as the caller guarantees.
        match unsafe { psp.boot_done(delay) } {
            Ok(()) => fstart_log::info!("PSP: boot done"),
            Err(error) => fstart_log::error!("PSP: BootDone failed: {}", error.name()),
        }
    }
}
