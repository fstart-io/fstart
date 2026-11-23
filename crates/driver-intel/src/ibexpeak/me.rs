//! Intel Management Engine on the 5 Series PCH: status, the raminit UMA
//! handshake over HECI and the ramstage MEI policy.
//!
//! coreboot `ibexpeak/{me.c,setup_heci_uma.c}` and
//! `bd82x6x/{early_me.c,me_status.c}`. The firmware sends the ME no
//! messages besides `MKHI_SET_UMA` during raminit.

use super::regs::{FD2, VCTL};
use super::{IntelIbexPeak, PchFunction};
pub use crate::southbridge::mei::{CSR, GMES, HERES, HFS, MEI_HEADER, Mei, UMA, log_status};
use crate::southbridge::mei::{ME_DELAY_US, ME_RETRY, MEI_ADDRESS_MKHI};
use fstart_core::services::ServiceError;
use fstart_pci::ecam::EcamDevice;
use tock_registers::LocalRegisterCopy;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// HECI/MEI #1.
const HECI: u8 = fstart_pci::devfn(0x16, 0);
/// MKHI UMA setup command.
const MKHI_SET_UMA: u8 = 0x08;

fn heci() -> EcamDevice {
    EcamDevice::from_devfn(0, HECI)
}

/// The register state the raminit UMA handshake touches on both DMI ends.
pub struct UmaChannels {
    pub dmibar: usize,
    pub rcba: usize,
    pub mchbar: usize,
}

/// coreboot `intel_early_me_status()`.
pub fn early_status() {
    let me = heci();
    match me.read32(0) {
        0xffff_ffff | 0 | 0x0000_ffff | 0xffff_0000 => {
            fstart_log::info!("ME: PCI device missing");
        }
        _ => log_status(me.read32(0x40), me.read32(0x48)),
    }
}

/// coreboot `intel_early_me_init()`: wait for the UMA request and check
/// the firmware partition table.
pub fn early_init() -> Result<(), ServiceError> {
    let me = heci();
    let valid = fstart_arch::wait_us(ME_RETRY * ME_DELAY_US, || {
        LocalRegisterCopy::<u32, UMA::Register>::new(me.read32(0x44)).is_set(UMA::VALID)
    });
    if !valid {
        fstart_log::error!("ME: not ready");
        return Err(ServiceError::Timeout);
    }
    if LocalRegisterCopy::<u32, HFS::Register>::new(me.read32(0x40)).is_set(HFS::FPT_BAD) {
        fstart_log::error!("ME: bad firmware");
        return Err(ServiceError::HardwareError);
    }
    fstart_log::info!("ME: firmware ready");
    Ok(())
}

/// coreboot `intel_early_me_uma_size()`: the MiB the ME asks for.
pub fn early_uma_size() -> u32 {
    let uma = LocalRegisterCopy::<u32, UMA::Register>::new(heci().read32(0x44));
    if uma.is_set(UMA::VALID) {
        let size = uma.read(UMA::SIZE);
        fstart_log::info!("ME: requested {} MiB UMA", size);
        size
    } else {
        fstart_log::info!("ME: invalid UMA size");
        0
    }
}

/// coreboot `setup_heci_uma()`: retire the DMI virtual channels for the
/// ME's private channel when it gets memory, then send `MKHI_SET_UMA`.
pub fn setup_uma(
    channels: &UmaChannels,
    hecibar: usize,
    address: u64,
    size_mib: u32,
) -> Result<(), ServiceError> {
    let me = heci();
    if size_mib == 0 && me.read32(0x40) & 0x20 == 0 {
        return Ok(());
    }
    if size_mib != 0 {
        // VC0, VC1, VCp and VCm lose TC7 on both ends; VCm then takes it.
        const DMIVC0RCTL: u32 = 0x014;
        const DMIVC1RCTL: u32 = 0x020;
        const DMIVCPRCTL: u32 = 0x02c;
        const DMIVCMRCTL: u32 = 0x038;
        const DMIVCMRSTS: u32 = 0x03e;
        // SAFETY: DMIBAR is a 4 KiB mapped chipset MMIO register block.
        let dmi = unsafe { fstart_core::mmio::MmioWindow::new(channels.dmibar, 0x1000) };
        // SAFETY: RCBA is enabled at this base by the southbridge.
        let rcba = unsafe { &*(channels.rcba as *const super::regs::RcbaRegs) };
        dmi.clrbits32(DMIVC0RCTL, 1 << 7);
        rcba.v0ctl
            .modify(VCTL::TC_MAP.val(rcba.v0ctl.read(VCTL::TC_MAP) & !0x80));
        dmi.clrbits32(DMIVC1RCTL, 1 << 7);
        rcba.v1ctl
            .modify(VCTL::TC_MAP.val(rcba.v1ctl.read(VCTL::TC_MAP) & !0x80));
        dmi.clrbits32(DMIVCPRCTL, 1 << 7);
        rcba.vcpctl
            .modify(VCTL::TC_MAP.val(rcba.vcpctl.read(VCTL::TC_MAP) & !0x80));
        dmi.clrbits32(DMIVCMRCTL, 1 << 7);
        rcba.vcmctl
            .modify(VCTL::TC_MAP.val(rcba.vcmctl.read(VCTL::TC_MAP) & !0x80));
        rcba.vcmctl.set(0x8700_0080);
        dmi.write32(DMIVCMRCTL, 0x8700_0080);
        let negotiated = fstart_arch::wait_us(ME_RETRY * ME_DELAY_US, || {
            !(rcba.vcmsts.is_set(super::regs::VSTS::NEGOTIATION_PENDING)
                && dmi.read16(DMIVCMRSTS) & (1 << 1) != 0)
        });
        if !negotiated {
            fstart_log::error!("ME: VCm negotiation timeout");
            return Err(ServiceError::Timeout);
        }
    }
    // MCHBAR+0x24: the ME memory size.
    // SAFETY: MCHBAR is mapped for at least 4 KiB during raminit.
    let mchbar = unsafe { fstart_core::mmio::MmioWindow::new(channels.mchbar, 0x1000) };
    mchbar.write32(0x24, 0x10000 + size_mib);
    // SAFETY: HECI BAR0 was just programmed and memory-enabled above.
    send_uma_message(unsafe { Mei::new(hecibar) }, address, size_mib)?;
    me.write32(0x10, 0);
    me.write8(0x04, 0);
    Ok(())
}

/// The `MKHI_SET_UMA` request and its acknowledgement.
fn send_uma_message(mei: Mei, address: u64, size_mib: u32) -> Result<(), ServiceError> {
    let mut message = [0u8; 22];
    message[0] = 0; // group
    message[1] = MKHI_SET_UMA;
    message[4..8].copy_from_slice(&0x82u32.to_le_bytes());
    message[8..16].copy_from_slice(&address.to_le_bytes());
    message[16..20].copy_from_slice(&size_mib.to_le_bytes());
    mei.send(&message, 0, MEI_ADDRESS_MKHI)?;
    let mut reply = [0u32; 0x48 / 4];
    // coreboot ignores a failed receive and only checks a received reply.
    if mei.receive(&mut reply).is_err() {
        fstart_log::error!("ME: no UMA reply");
        return Ok(());
    }
    let command = ((reply[0] >> 8) & 0xff) as u8;
    if command != MKHI_SET_UMA | (1 << 7) {
        fstart_log::error!("ME: HECI UMA setup failed ({:#x})", u32::from(command));
        return Err(ServiceError::HardwareError);
    }
    Ok(())
}

/// The BIOS path the ME state selects (coreboot `me_bios_path`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BiosPath {
    Normal,
    S3Wake,
    Error,
    Recovery,
    Disable,
}

impl BiosPath {
    const fn name(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::S3Wake => "S3 Wake",
            Self::Error => "Error",
            Self::Recovery => "Recovery",
            Self::Disable => "Disable",
        }
    }

    const fn should_hide_mei(self, hide_on_error: bool) -> bool {
        match self {
            Self::Disable => true,
            Self::Error => hide_on_error,
            Self::Normal | Self::Recovery | Self::S3Wake => false,
        }
    }
}

impl IntelIbexPeak {
    /// coreboot `intel_me_path()`.
    fn me_path(&self, me: EcamDevice) -> BiosPath {
        if self.resume {
            return BiosPath::S3Wake;
        }
        let hfs_raw = me.read32(0x40);
        log_status(hfs_raw, me.read32(0x48));
        let hfs = LocalRegisterCopy::<u32, HFS::Register>::new(hfs_raw);
        let mut path = match hfs.read_as_enum(HFS::WORKING_STATE) {
            Some(HFS::WORKING_STATE::Value::Normal) => BiosPath::Normal,
            Some(HFS::WORKING_STATE::Value::Recovery) => BiosPath::Recovery,
            _ => BiosPath::Disable,
        };
        if hfs.read(HFS::OPERATION_MODE) != 0 {
            path = BiosPath::Disable;
        }
        if hfs.read(HFS::ERROR_CODE) != 0 || hfs.is_set(HFS::FPT_BAD) {
            path = BiosPath::Error;
        }
        path
    }

    /// Hide a disabled ME, else check the extend register and prepare MEI.
    /// Unlike coreboot's old S3 policy, leave MEI visible on resume: Linux
    /// still owns the enumerated device and restores it from D3hot. S3
    /// skips the cold-boot MEI handshake without disabling the function.
    pub(super) fn me_init(&self) {
        if !self.config.enabled(PchFunction::Mei1) {
            return;
        }
        let me = heci();
        if !me.is_present() {
            return;
        }
        let path = self.me_path(me);
        fstart_log::info!("ME: BIOS path {}", path.name());
        if path.should_hide_mei(self.config.hide_mei_on_error) {
            // pch_enable() of a disabled device.
            me.write16(0x04, me.read16(0x04) & !0x7);
            self.rcba().fd2.modify(FD2::MEI1::SET);
            return;
        }
        if path != BiosPath::Normal {
            return;
        }
        if !extend_valid(me) {
            return;
        }
        let Some(bar) = me.memory_bar(0x10) else {
            fstart_log::info!("ME: MEI resource not present");
            return;
        };
        me.or16(0x04, 0x6);
        // SAFETY: HECI BAR0 is assigned and memory-enabled above.
        let mei = unsafe { Mei::new(bar as usize) };
        mei.set_host_ready();
        // The ME stays unlocked; there is no END_OF_POST on this PCH.
    }
}

/// coreboot `intel_me_extend_valid()`: the firmware measurement exists.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_keeps_mei_visible_without_changing_disabled_or_error_policy() {
        for hide_on_error in [false, true] {
            assert!(!BiosPath::S3Wake.should_hide_mei(hide_on_error));
            assert!(!BiosPath::Normal.should_hide_mei(hide_on_error));
            assert!(!BiosPath::Recovery.should_hide_mei(hide_on_error));
            assert!(BiosPath::Disable.should_hide_mei(hide_on_error));
            assert_eq!(
                BiosPath::Error.should_hide_mei(hide_on_error),
                hide_on_error
            );
        }
    }

    #[test]
    fn mei_header_layout_matches_coreboot() {
        let header = MEI_HEADER::CLIENT_ADDRESS.val(7)
            + MEI_HEADER::HOST_ADDRESS.val(0)
            + MEI_HEADER::LENGTH.val(22)
            + MEI_HEADER::COMPLETE::SET;
        assert_eq!(header.value, 0x8016_0007);
    }
}
