//! TXE UMA handshake (00:1a.0).
//!
//! The TXE ROM advertises the UMA it needs in config space. Raminit reads
//! it before training, carves that much DRAM at the top of low memory,
//! then bridges the carved base to the TXE and sends DRAM Init Done.

use super::{dev, did, pmc};
use fstart_core::services::ServiceError;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::ReadWriteable;
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u32,
    /// HFS0.
    FW_STS0 [ BIOS_ACTION OFFSET(25) NUMBITS(3) [
        NoDid = 0,
        Reset = 1,
        PowerCycle = 2,
        S3 = 3,
        S4 = 4,
        S5 = 5,
        GlobalReset = 6,
        Continue = 7
    ], DID_ACK OFFSET(28) NUMBITS(1) [] ],
    MEM_REQ [
        SIZE_KB OFFSET(0) NUMBITS(16) [],
        INVALID OFFSET(30) NUMBITS(1) [],
        VALID OFFSET(31) NUMBITS(1) []
    ],
    DID_MSG [ DID OFFSET(28) NUMBITS(1) [] ],
    SATT_CTRL [
        ENTRY_VLD OFFSET(0) NUMBITS(1) [],
        BASE_MSB OFFSET(8) NUMBITS(4) []
    ]
];

const FW_STS0_REG: u16 = 0x40;
const MEM_REQ_REG: u16 = 0x44;
const FW_STS1_REG: u16 = 0x48;
const DID_MSG_REG: u16 = 0x60;
/// Written before polling the UMA request, as the vendor firmware does.
const UMA_HANDSHAKE_REG: u16 = 0x80;
const SATT1_CTRL: u16 = 0xc0;
const SATT1_BRG_BA_LSB: u16 = 0xc4;
const SATT1_SAP_SIZE: u16 = 0xc8;
/// Five seconds.
const POLL_US: u32 = 5_000_000;

fn txe() -> EcamDevice {
    EcamDevice::new(0, dev::TXE, 0)
}

/// The UMA size the TXE ROM requests in MiB, or 0 without a TXE.
pub fn uma_size_mb(enabled: bool) -> u32 {
    // Hiding the TXE after a DRAM Init Done locks it up: skip the
    // handshake entirely when the board disables it.
    if !enabled {
        fstart_log::info!("baytrail: TXE disabled, no UMA");
        return 0;
    }
    let id = txe().device_id();
    if id & 0xfff8 != did::TXE {
        fstart_log::info!("baytrail: no TXE (device {:#06x}), no UMA", id);
        return 0;
    }
    fstart_log::debug!(
        "baytrail: TXE FWSTS0 {:#010x} FWSTS1 {:#010x}",
        txe().read32(FW_STS0_REG),
        txe().read32(FW_STS1_REG)
    );
    txe().write32(UMA_HANDSHAKE_REG, 0x4803_a001);
    let mut request = LocalRegisterCopy::<u32, MEM_REQ::Register>::new(0);
    for _ in 0..POLL_US / 10 {
        request.set(txe().read32(MEM_REQ_REG));
        if request.is_set(MEM_REQ::VALID) || request.is_set(MEM_REQ::INVALID) {
            break;
        }
        fstart_arch::x86::udelay(10);
    }
    if !request.is_set(MEM_REQ::VALID) {
        fstart_log::error!(
            "baytrail: TXE UMA request not valid ({:#010x})",
            request.get()
        );
        return 0;
    }
    // The request is in KiB; the vendor firmware clamps it below 32 MiB.
    let mb = (request.read(MEM_REQ::SIZE_KB) / 1024).min(31);
    fstart_log::info!("baytrail: TXE requests {} MiB UMA", mb);
    mb
}

/// Arm (or disarm) a global reset, which also resets the TXE, for the
/// next CF9 reset.
fn set_global_reset(enable: bool) {
    use pmc::ETR;
    let etr = &pmc::regs().etr;
    etr.modify(ETR::CWORWRE::CLEAR);
    etr.modify(if enable {
        ETR::CF9GR::SET
    } else {
        ETR::CF9GR::CLEAR
    });
}

/// Bridge the UMA at `base_mb` to the TXE and send DRAM Init Done, then
/// follow the reset the TXE asks for.
pub fn dram_init_done(base_mb: u32, size_mb: u32) -> Result<(), ServiceError> {
    if size_mb == 0 {
        return Ok(());
    }
    let txe = txe();
    // SATT1: linear bridge from the TXE's view to host physical UMA.
    txe.write32(SATT1_BRG_BA_LSB, base_mb << 20);
    txe.or32(
        SATT1_CTRL,
        SATT_CTRL::BASE_MSB.val((base_mb >> 12) & 0xf).value,
    );
    txe.write32(SATT1_SAP_SIZE, size_mb << 20);
    txe.or32(SATT1_CTRL, SATT_CTRL::ENTRY_VLD::SET.value);
    // DRAM Init Done, status 0 (success).
    txe.or32(DID_MSG_REG, DID_MSG::DID::SET.value);
    fstart_log::info!(
        "baytrail: TXE DID sent (UMA {} MiB at {} MiB)",
        size_mb,
        base_mb
    );

    let mut status = LocalRegisterCopy::<u32, FW_STS0::Register>::new(0);
    for _ in 0..POLL_US / 1000 {
        status.set(txe.read32(FW_STS0_REG));
        if status.is_set(FW_STS0::DID_ACK) {
            break;
        }
        fstart_arch::x86::udelay(1000);
    }
    if !status.is_set(FW_STS0::DID_ACK) {
        fstart_log::error!("baytrail: TXE DID ACK timeout ({:#010x})", status.get());
        // As the reference: the boot continues without the ACK.
        return Ok(());
    }
    let action = status.read_as_enum(FW_STS0::BIOS_ACTION);
    fstart_log::info!(
        "baytrail: TXE DID ACK, BIOS action {}",
        status.read(FW_STS0::BIOS_ACTION)
    );
    match action {
        Some(FW_STS0::BIOS_ACTION::Value::Reset) => {
            fstart_log::info!("baytrail: TXE requests a warm reset");
            set_global_reset(false);
            fstart_arch::x86::boot::system_reset(true)
        }
        Some(FW_STS0::BIOS_ACTION::Value::PowerCycle) => {
            fstart_log::info!("baytrail: TXE requests a power cycle");
            set_global_reset(false);
            fstart_arch::x86::boot::full_reset()
        }
        Some(FW_STS0::BIOS_ACTION::Value::GlobalReset) => {
            fstart_log::info!("baytrail: TXE requests a global reset");
            set_global_reset(true);
            fstart_arch::x86::boot::full_reset()
        }
        // No DID, continue, and the sleep-state codes: boot on.
        _ => Ok(()),
    }
}
