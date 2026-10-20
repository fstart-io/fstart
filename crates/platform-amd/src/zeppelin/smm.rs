// SPDX-License-Identifier: MIT OR Apache-2.0
//! SMM on Zeppelin: the board's FCH SMM
//! handler in an 8 MiB TSEG at the top of the DRAM below 4 GiB, every CPU's
//! SMBASE in it and SMM locked, then the FCH's SMI sources enabled. Then the
//! PSP learns about it.
use fstart_driver_amd::fam17h::tsc::DelayNs;
use super::{Board, memory::TSEG_SIZE};
use core::ptr::addr_of_mut;
use fstart_arch::x86::{
    cpu::amd::smm::{AmdSmm, InstalledTseg},
    mp::MpHandle,
};
use fstart_driver_amd::{
    fch::smi::{self, FchSmi},
    psp::{Buffer, Mailbox, Native, SmmInfo},
    zeppelin::fch,
};

/// The board's SMM image that fbuild built from its `smm.rs`.
#[cfg(fstart_amd_has_smm_image)]
const IMAGE: Option<&[u8]> = Some(include_bytes!(env!("FSTART_SMM_IMAGE")));
#[cfg(not(fstart_amd_has_smm_image))]
const IMAGE: Option<&[u8]> = None;

pub fn install<B: Board>(mp: &MpHandle, tseg_base: u64) -> Result<InstalledTseg, &'static str> {
    let image = IMAGE.ok_or("no SMM image")?;
    // SAFETY: BSP only after MP init; ACPI MMIO and the ACPI blocks are
    // decoded since the FCH init.
    let smi = unsafe { FchSmi::new(fch::ACPI_PORTS, B::SMI_GEVENTS) };
    AmdSmm::new("zeppelin", &smi)
        .install(mp, image, tseg_base, TSEG_SIZE)
        .map_err(|error| error.name())
}

/// PSP-to-core buffer, then the core-to-PSP buffer with the SMM flag at its
/// +0xc00, as the vendor firmware lays them out in TSEG.
const P2C_SIZE: u64 = 0x1000;
const C2P_SIZE: u64 = 0xc00;
const PSP_REGION: u64 = 0x2000;

/// The SMM info, its buffers in unused TSEG.
fn psp_smm_info(tseg: &InstalledTseg) -> Option<SmmInfo> {
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

/// The one PSP mailbox owner of the boot, die 0's, after it got the SMM
/// info. `None` if that failed: the mailbox is unusable then, and LockDFReg
/// without the SMM info would break device DMA anyway.
pub fn psp_mailbox(
    bar: u32,
    tseg: &InstalledTseg,
    delay: &mut impl DelayNs,
) -> Option<Mailbox<Native>> {
    static mut BUFFER: Buffer = Buffer::new();
    let Some(info) = psp_smm_info(tseg) else {
        fstart_log::error!("PSP: no room for the SMM buffers in TSEG");
        return None;
    };
    if bar == 0 {
        fstart_log::error!("PSP: BAR of die 0 isn't enabled");
        return None;
    }
    // SAFETY: the BAR was mapped and locked by `psp::init_bar`; BSP only.
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
