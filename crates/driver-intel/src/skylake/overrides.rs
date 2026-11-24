// SPDX-License-Identifier: MIT OR Apache-2.0
//! native/overrides.c PCODE DLL bandwidth and post-SPD DDR4 overrides.
//! BARs and saved power state must be established by the platform first.

use super::{Error, phy_registers::*, registers::MchRegisters};
use embedded_hal::delay::DelayNs;
use fstart_core::mmio::MmioReadOnly;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields, register_structs};

fn mailbox_ready(r: &MchRegisters, delay: &mut impl DelayNs) -> Result<(), Error> {
    for _ in 0..10_000 {
        if !r.mailbox_interface.is_set(MailboxInterface::RUN_BUSY) {
            return Ok(());
        }
        delay.delay_us(10);
    }
    Err(Error::MailboxTimeout)
}
fn mailbox_status(r: &MchRegisters) -> Result<(), Error> {
    let status = r.mailbox_interface.read(MailboxInterface::COMMAND_STATUS) as u8;
    if status == 0 {
        Ok(())
    } else {
        Err(Error::MailboxCompletion(status))
    }
}
fn read_dll_bandwidth(r: &MchRegisters, delay: &mut impl DelayNs) -> Result<u32, Error> {
    mailbox_ready(r, delay)?;
    r.mailbox_interface
        .write(MailboxInterface::COMMAND_STATUS.val(0x29) + MailboxInterface::RUN_BUSY::SET);
    mailbox_ready(r, delay)?;
    let interface = r.mailbox_interface.get();
    let data = r.mailbox_data.get();
    delay.delay_us(10);
    if interface != r.mailbox_interface.get() {
        return Err(Error::MailboxInconsistent);
    }
    mailbox_status(r)?;
    Ok(data)
}
/// Shared native PCODE write protocol (overrides.c and additional_steps.c).
/// Only command[7:0] and EPG overhead index[8] are modeled. Reject other bits
/// before accessing the mailbox, rather than truncating an unsupported command.
/// Each ready phase is bounded to 10,000 samples separated by 10 us.
/// Caller must serialize all PCODE users; completion errors are fatal.
pub(crate) fn write_mailbox(
    r: &MchRegisters,
    delay: &mut impl DelayNs,
    command: u16,
    data: u32,
) -> Result<(), Error> {
    if command & !0x1ff != 0 {
        return Err(Error::InvalidPolicy);
    }
    mailbox_ready(r, delay)?;
    r.mailbox_data.set(data);
    r.mailbox_interface.write(
        MailboxInterface::COMMAND_STATUS.val(u32::from(command & 0xff))
            + MailboxInterface::EPG_OVERHEAD_INDEX.val(u32::from(command >> 8))
            + MailboxInterface::RUN_BUSY::SET,
    );
    mailbox_ready(r, delay)?;
    mailbox_status(r)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreSpdOutcome {
    Unchanged,
    /// The caller must perform the platform warm reset, NOT proceed to PHY
    /// initialization. This library never issues resets on its own.
    WarmResetRequired,
}

/// meminit_set_overrides_pre_spd. CAPID0_A is obtained through the existing
/// ECAM capability reader. Unlike the source, mailbox errors are propagated.
/// A successful write/read-back requests reset even if PCODE returns stale
/// data, as in the source; the next boot must re-evaluate the settings.
pub fn pre_spd(
    r: &MchRegisters,
    delay: &mut impl DelayNs,
    capid_a: u32,
    dll_bw_en: [u8; 4],
) -> Result<PreSpdOutcome, Error> {
    if capid_a == u32::MAX || dll_bw_en.iter().any(|v| *v > 7) {
        return Err(Error::InvalidPolicy);
    }
    if capid_a & (1 << 3) == 0 {
        return Ok(PreSpdOutcome::Unchanged);
    }
    let desired = (DllBandwidth::MT1067.val(dll_bw_en[0].into())
        + DllBandwidth::MT1333.val(dll_bw_en[1].into())
        + DllBandwidth::MT1600.val(dll_bw_en[2].into())
        + DllBandwidth::MT1867.val(dll_bw_en[3].into()))
    .value;
    let current = read_dll_bandwidth(r, delay)?;
    if current & 0xfff == desired {
        return Ok(PreSpdOutcome::Unchanged);
    }
    write_mailbox(r, delay, 0x2a, desired)?;
    read_dll_bandwidth(r, delay)?;
    Ok(PreSpdOutcome::WarmResetRequired)
}

register_bitfields![u32,
    GdxcRegion [
        START OFFSET(0) NUMBITS(16) [],
        END OFFSET(16) NUMBITS(16) []
    ]
];
register_structs! {
    pub GdxcRegisters {
        (0x00 => _reserved0),
        (0x18 => mot: MmioReadOnly<u32,GdxcRegion::Register>),
        (0x1c => _reserved1),
        (0x28 => ocla: MmioReadOnly<u32,GdxcRegion::Register>),
        (0x2c => @END),
    }
}
impl GdxcRegisters {
    /// # Safety
    /// GDXCBAR must be enabled, mapped for this layout and exclusively owned
    /// for the borrow. The native default address is 0xfed84000.
    pub unsafe fn at<'a>(base: usize) -> &'a Self {
        assert_eq!(base & 3, 0);
        unsafe { &*(base as *const Self) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostSpdOverrides {
    pub ecc_support: bool,
    pub rank_interleave: bool,
    pub enhanced_interleave: bool,
    pub vdd_voltage_done: bool,
    pub gdxc_enable: bool,
    /// Native GDXC size unit is 8 MiB.
    pub gdxc_mot_size: u8,
    pub gdxc_iot_size: u8,
    pub lpddr_dram_odt: bool,
}
impl PostSpdOverrides {
    /// meminit_set_overrides, DDR4 branch only. ECC/GDXC enables are policy
    /// inputs, not silently enabled here. Does not reserve a memory map.
    pub fn apply_ddr4(self, r: &GdxcRegisters) -> Result<Self, Error> {
        fn size(raw: u32, policy: u8) -> Result<u8, Error> {
            let reg = LocalRegisterCopy::<u32, GdxcRegion::Register>::new(raw);
            let end = reg.read(GdxcRegion::END);
            if reg.read(GdxcRegion::START) == 0 && end > 1 {
                // Native truncates to u8. Reject unrepresentable reservations.
                u8::try_from(end).map_err(|_| Error::InvalidPolicy)
            } else {
                Ok(policy)
            }
        }
        Ok(Self {
            rank_interleave: true,
            enhanced_interleave: true,
            vdd_voltage_done: false,
            lpddr_dram_odt: false,
            gdxc_mot_size: size(r.mot.get(), self.gdxc_mot_size)?,
            gdxc_iot_size: size(r.ocla.get(), self.gdxc_iot_size)?,
            ..self
        })
    }
}
