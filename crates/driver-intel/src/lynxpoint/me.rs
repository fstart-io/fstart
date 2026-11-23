//! Intel ME9 pre-memory UMA, CPU replacement status, DRAM Init Done (DID),
//! MBP consumption, ICC clock disable, and End of Post (EOP).
//!
//! All host-bridge information (such as `MESEG_BASE`) is passed in by the
//! caller so this southbridge module has zero coupling to the northbridge.

use super::{
    lpc_regs, rcba_regs,
    regs::{ETR3, FD2, MEI_DID, dev},
};
use crate::southbridge::mei::{
    CSR, GMES, HERES, HFS, MEI_ADDRESS_MKHI, MEI_H_CSR, MEI_ME_CB_RW, Mei, UMA,
};
use fstart_core::services::ServiceError;
use fstart_pci::ecam::EcamDevice;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable},
};

/// PCI `H_GS2` (`0x70`, 32 bits): notify ME that BIOS cannot accept the MBP.
const H_GS2: u16 = 0x70;
const MBP_GIVE_UP: u32 = 1;

fn mei() -> EcamDevice {
    EcamDevice::from_devfn(0, dev::MEI1)
}

fn encode_did(status: u32, meseg_base_mib: u16) -> u32 {
    (MEI_DID::COMMAND.val(1)
        + MEI_DID::BASE.val(u32::from(meseg_base_mib))
        + MEI_DID::STATUS.val(status))
    .value
}

/// ME UMA size in MiB; zero when the ME does not request UMA.
///
/// As in coreboot `intel_early_me_init`/`intel_early_me_uma_size`, an absent,
/// unready, or broken ME is logged and boot continues without UMA.
pub fn uma() -> Result<u32, ServiceError> {
    let me = mei();
    if me.vendor_id() != 0x8086 {
        fstart_log::warn!("ME: MEI hidden, no UMA");
        return Ok(0);
    }
    if !fstart_arch::x86::wait_us(1_000_000, || UMA::VALID.is_set(me.read32(0x44))) {
        fstart_log::error!("ME: not ready, no UMA");
        return Ok(0);
    }
    if HFS::FPT_BAD.is_set(me.read32(0x40)) {
        fstart_log::warn!("ME: bad firmware");
    }
    Ok(UMA::SIZE.read(me.read32(0x44)))
}

/// Report whether the ME detected a CPU replacement since the last boot.
pub fn replaced() -> bool {
    let me = mei();
    if !fstart_arch::x86::wait_us(50_000, || GMES::CPU_REPLACED_VALID.is_set(me.read32(0x48))) {
        return true;
    }
    let s = me.read32(0x48);
    if GMES::WARM_RESET_REQUEST.is_set(s) {
        reset(false, false);
    }
    GMES::CPU_REPLACED.is_set(s)
}

/// Issue a PCH warm, full, or global reset.
pub fn reset(global: bool, power_cycle: bool) -> ! {
    lpc_regs().etr3.modify(ETR3::CF9GR.val(u32::from(global)));
    if power_cycle {
        fstart_arch::x86::boot::full_reset()
    } else {
        fstart_arch::x86::boot::system_reset(true)
    }
}

/// Send the DRAM Init Done (DID) message to ME9.
///
/// `meseg_base_mib` is `MESEG_BASE[35:20]` (in 1 MiB units), supplied by the
/// caller from the northbridge memory map.
pub fn done(status: u32, meseg_base_mib: u16) -> Result<(), ServiceError> {
    let dev = mei();
    dev.write32(0x4c, encode_did(status, meseg_base_mib));
    if matches!(status, 1 | 2) {
        return Ok(());
    }
    // coreboot's perform_raminit ignores a missing or non-reset answer and
    // boots on; only the requested resets are acted upon.
    if !fstart_arch::x86::wait_us(1_000_000, || HFS::BIOS_MSG_ACK.read(dev.read32(0x40)) != 0) {
        fstart_log::error!("ME: no DRAM Init Done acknowledgement");
        return Ok(());
    }
    match HFS::ACK_DATA.read(dev.read32(0x40)) {
        1 => reset(false, false),
        2 => reset(false, true),
        6 => reset(true, true),
        7 => {}
        action => fstart_log::warn!("ME: ignoring requested BIOS action {}", action),
    }
    Ok(())
}

fn normal(s: u32) -> bool {
    HFS::WORKING_STATE.read(s) == 5 && HFS::OPERATION_MODE.read(s) == 0 && !HFS::FPT_BAD.is_set(s)
}

/// True when ME is hardware-disabled via AltMeDisable/HAP (`Debug` = 2) or
/// soft-temporarily disabled (`Disabled` = 3) and will not service MEI.
fn disabled_mode(s: u32) -> bool {
    matches!(HFS::OPERATION_MODE.read(s), 2 | 3)
}

pub(super) fn is_disabled_mode() -> bool {
    let dev = mei();
    dev.is_present() && disabled_mode(dev.read32(0x40))
}

/// Check the ME firmware measurement extend registers (coreboot `intel_me_extend_valid`).
fn extend_valid(me: EcamDevice) -> bool {
    let status = LocalRegisterCopy::<u32, HERES::Register>::new(me.read32(0xbc));
    if !status.is_set(HERES::FEATURE_PRESENT) {
        fstart_log::error!("ME: extend feature not present");
        return false;
    }
    if !status.is_set(HERES::VALID) {
        fstart_log::error!("ME: extend register not valid");
        return false;
    }
    let count = match status.read_as_enum(HERES::ALGORITHM) {
        Some(HERES::ALGORITHM::Value::Sha1) => 5,
        Some(HERES::ALGORITHM::Value::Sha256) => 8,
        None => {
            fstart_log::error!("ME: unknown extend algorithm");
            return false;
        }
    };
    for index in 0..count {
        fstart_log::info!(
            "ME: extend[{}] = {:#010x}",
            index,
            me.read32(0xc0 + 4 * index)
        );
    }
    true
}

fn reset_host_csr(transport: Mei) {
    let val =
        transport.read_dword(MEI_H_CSR) | (CSR::RESET::SET + CSR::INTERRUPT_GENERATE::SET).value;
    transport.write_dword(MEI_H_CSR, val);
}

/// Consume BIOS payload words directly from the circular buffer rather than
/// allocating a firmware-sized buffer. Signal the ME only after the complete MBP.
pub(super) fn mbp(icc_clock_disable: u32) {
    let dev = mei();
    let Some(bar) = dev.memory_bar(0x10) else {
        if dev.is_present() {
            dev.write32(H_GS2, MBP_GIVE_UP);
        }
        fstart_log::warn!("ME9 MBP: no MEI BAR");
        return;
    };
    let status = dev.read32(0x40);
    crate::southbridge::mei::log_status(status, dev.read32(0x48));
    if disabled_mode(status) {
        fstart_log::info!("ME9 disabled (HFS={:#x}), skipping MBP", status);
        return;
    }
    if normal(status) && HFS::ERROR_CODE.read(status) == 0 {
        extend_valid(dev);
    }
    dev.or16(4, 6);
    // SAFETY: MEI BAR0 was assigned by the PCI allocator and memory-enabled above.
    let transport = unsafe { Mei::new(bar as usize) };
    if let Err(e) = consume(dev, transport) {
        fstart_log::warn!("ME9: giving up MBP: {:?}", e);
        dev.write32(H_GS2, MBP_GIVE_UP);
        reset_host_csr(transport);
        return;
    }
    if icc_clock_disable != 0 {
        use zerocopy::IntoBytes;
        let msg = [0x0003_0000u32, 3, 0, 12, 0, 0, icc_clock_disable, 1];
        if let Err(e) = transport
            .send_part(msg[..5].as_bytes(), 0, 8, false)
            .and_then(|()| transport.send_part(msg[5..].as_bytes(), 0, 8, true))
        {
            fstart_log::warn!(
                "ME9 ICC SET CLOCK ENABLES mask {:#x}: {:?}",
                icc_clock_disable,
                e
            );
        }
    }
}

fn consume(dev: EcamDevice, transport: Mei) -> Result<(), ServiceError> {
    // Check MBP_READY before waiting on MEI CSR ready (coreboot intel_me_read_mbp)
    // so disabled/AltDisableBit ME doesn't stall boot for the full MEI timeout.
    if !GMES::MBP_READY.is_set(dev.read32(0x48)) {
        return Err(ServiceError::NoDevice);
    }
    transport.wait_ready()?;
    let csr = transport.me_csr();
    let count = csr
        .read(CSR::WRITE_PTR)
        .wrapping_sub(csr.read(CSR::READ_PTR))
        & 255;
    if count == 0 || count > csr.read(CSR::DEPTH) {
        return Err(ServiceError::HardwareError);
    }
    drain_mbp(count, || transport.read_dword(MEI_ME_CB_RW))?;
    let host_csr = transport.read_dword(MEI_H_CSR) | CSR::INTERRUPT_GENERATE::SET.value;
    transport.write_dword(MEI_H_CSR, host_csr);
    Ok(())
}

fn drain_mbp(count: u32, mut read: impl FnMut() -> u32) -> Result<(), ServiceError> {
    let header = read();
    let size = header & 255;
    if size != count || ((header >> 8) & 255) > size / 2 {
        return Err(ServiceError::HardwareError);
    }
    let mut remaining = size.checked_sub(1).ok_or(ServiceError::HardwareError)?;
    for _ in 0..((header >> 8) & 255) {
        if remaining == 0 {
            return Err(ServiceError::HardwareError);
        }
        let len = (read() >> 16) & 255;
        if len == 0 || len > remaining {
            return Err(ServiceError::HardwareError);
        }
        for _ in 1..len {
            read();
        }
        remaining -= len;
    }
    (remaining == 0)
        .then_some(())
        .ok_or(ServiceError::HardwareError)
}

pub(super) fn hide() {
    mei().and16(4, !7);
    let rcba = rcba_regs();
    rcba.fd2.modify(FD2::MEI1::SET);
    if !rcba.fd2.is_set(FD2::MEI1) {
        fstart_log::warn!("ME9: FD2.MEI1 did not latch");
    }
}

pub(super) fn finalize(disable: bool) {
    let dev = mei();
    if !dev.is_present() {
        return;
    }
    let status = dev.read32(0x40);
    if disabled_mode(status) {
        finalize_mode(status, disable, || Ok(()), hide);
        return;
    }
    let Some(base) = dev.memory_bar(0x10) else {
        fstart_log::warn!("ME9 EOP: no MEI BAR");
        return;
    };
    // SAFETY: MEI BAR0 was assigned by the PCI allocator.
    let t = unsafe { Mei::new(base as usize) };
    if !fstart_arch::x86::wait_us(1_000_000, || GMES::MBP_CLEARED.is_set(dev.read32(0x48))) {
        fstart_log::warn!("ME9: MBP clear timeout, HFS2={:#x}", dev.read32(0x48));
        dev.write32(H_GS2, MBP_GIVE_UP);
        reset_host_csr(t);
    }
    finalize_mode(dev.read32(0x40), disable, || end_of_post(t), hide);
}

fn finalize_mode(
    status: u32,
    disable: bool,
    eop: impl FnOnce() -> Result<(), ServiceError>,
    hide: impl FnOnce(),
) {
    if disabled_mode(status) {
        fstart_log::info!("ME9 disabled (HFS={:#x}), hiding MEI", status);
        hide();
        return;
    }
    if !normal(status) {
        fstart_log::info!("ME9 EOP omitted: HFS={:#x}", status);
        return;
    }
    if let Err(e) = eop() {
        fstart_log::warn!("ME9 EOP incomplete: {:?}", e);
    }
    if disable {
        hide();
    }
}

fn end_of_post(t: Mei) -> Result<(), ServiceError> {
    t.send(&[0xff, 0x0c, 0, 0], 0, MEI_ADDRESS_MKHI)
        .inspect_err(|e| fstart_log::warn!("ME9 EOP send: {:?}", e))?;
    let mut reply = [0; 2];
    let bytes = t
        .receive(&mut reply)
        .inspect_err(|e| fstart_log::warn!("ME9 EOP receive: {:?}", e))?;
    fstart_log::info!(
        "ME9 EOP: {} bytes, header {:#x}, action {}",
        bytes,
        reply[0],
        reply[1]
    );
    eop_action(bytes, reply)
}

fn eop_action(bytes: usize, reply: [u32; 2]) -> Result<(), ServiceError> {
    let [mkhi, action] = reply;
    if bytes != 8 || mkhi & 0x00ff_ffff != 0x8cff {
        return Err(ServiceError::IoError); // Not an EOP response.
    }
    if mkhi >> 24 != 0 {
        return Err(ServiceError::HardwareError); // MKHI result.
    }
    match action {
        0 | 1 => Ok(()), // LPT reports the action; unlike DID, this is not a reset request.
        _ => Err(ServiceError::HardwareError),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn did_message_encodes_command_status_and_meseg_base() {
        // Command = 1 (bits 31:28), Status = 0 (bits 27:24), MESEG_BASE[35:20] = 0x0fee.
        assert_eq!(encode_did(0, 0x0fee), 0x1000_0fee);
        // Status = 1 (no memory), MESEG_BASE = 0.
        assert_eq!(encode_did(1, 0), 0x1100_0000);
        // Status = 3 (memory init error), MESEG_BASE = 0x1234.
        assert_eq!(encode_did(3, 0x1234), 0x1300_1234);
    }

    #[test]
    fn recovery_drains_payload_but_does_not_send_eop() {
        assert!(!normal(2));
        let mut words = [0x103, 2 << 16, 0x1234].into_iter();
        assert!(drain_mbp(3, || words.next().unwrap()).is_ok());
        assert_eq!(words.next(), None);
        assert!(drain_mbp(3, || 0x103).is_err());
        assert!(normal(5 | (1 << 12)));
        let trace = core::cell::RefCell::new(alloc::vec::Vec::new());
        finalize_mode(
            5,
            true,
            || {
                trace.borrow_mut().push("EOP");
                Err(ServiceError::Timeout)
            },
            || trace.borrow_mut().push("FD2"),
        );
        finalize_mode(2, true, || unreachable!(), || unreachable!());
        assert_eq!(*trace.borrow(), ["EOP", "FD2"]);
        // AltMeDisable/HAP (OPERATION_MODE=2) skips EOP and hides MEI even
        // when `me_disable` is false.
        let hidden = core::cell::Cell::new(false);
        finalize_mode(0x1e02_0191, false, || unreachable!(), || hidden.set(true));
        assert!(hidden.get());
        assert_eq!(eop_action(8, [0x8cff, 1]), Ok(()));
        assert!(eop_action(8, [0x0100_8cff, 1]).is_err());
    }
}
