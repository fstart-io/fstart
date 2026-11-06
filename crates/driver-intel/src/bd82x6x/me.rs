//! Intel Management Engine host interface, early (pre-DRAM) protocol of the
//! 6/7-series PCH (ME firmware 7.x/8.x, MEI1 at PCI 00:16.0).
//!
//! Follows coreboot `southbridge/intel/bd82x6x/early_me.c`: wait for the ME
//! to size its UMA, tell it when DRAM is up (the DRAM-init-done message) and
//! perform the reset the ME then requests. The ramstage half
//! (`me.c`, `me_common.c`, `me_smm.c`) picks the BIOS path, drains the ME 8
//! boot parameters, applies the optional soft-disable policy, sends
//! END_OF_POST and hides the interface. This is ME firmware policy, not
//! firmware removal.

use fstart_core::mmio::{MmioReadOnly, MmioReadWrite};
use fstart_core::services::ServiceError;
use fstart_pci::ecam::EcamDevice;
use fstart_pci::pci_type0_config;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

pub const ME_DEV: u8 = 0x16;
pub const ME_FUNC: u8 = 0;

register_bitfields![u32,
    /// Host firmware status (HFS, config 0x40).
    pub HFS [
        WORKING_STATE OFFSET(0) NUMBITS(4) [],
        MFG_MODE OFFSET(4) NUMBITS(1) [],
        FPT_BAD OFFSET(5) NUMBITS(1) [],
        OPERATION_STATE OFFSET(6) NUMBITS(3) [],
        FW_INIT_COMPLETE OFFSET(9) NUMBITS(1) [],
        FT_BUP_LD_FLR OFFSET(10) NUMBITS(1) [],
        UPDATE_IN_PROGRESS OFFSET(11) NUMBITS(1) [],
        ERROR_CODE OFFSET(12) NUMBITS(4) [],
        OPERATION_MODE OFFSET(16) NUMBITS(4) [
            Normal = 0,
            Debug = 2,
            Disable = 3,
            OverrideJumper = 4,
            OverrideMei = 5
        ],
        BOOT_OPTIONS_PRESENT OFFSET(24) NUMBITS(1) [],
        ACK_DATA OFFSET(25) NUMBITS(3) [
            NoDid = 0,
            Reset = 1,
            PowerCycle = 2,
            S3 = 3,
            S4 = 4,
            S5 = 5,
            GlobalReset = 6,
            Continue = 7
        ],
        BIOS_MSG_ACK OFFSET(28) NUMBITS(4) [
            DramAck = 1
        ]
    ],
    /// ME UMA request (config 0x44).
    pub UMA [
        SIZE_MIB OFFSET(0) NUMBITS(6) [],
        VALID OFFSET(16) NUMBITS(1) []
    ],
    /// General ME status (GMES / FWSTS2, config 0x48).
    pub GMES [
        BIST_IN_PROGRESS OFFSET(0) NUMBITS(1) [],
        ICC_PROG_STS OFFSET(1) NUMBITS(2) [],
        INVOKE_MEBX OFFSET(3) NUMBITS(1) [],
        CPU_REPLACED_STS OFFSET(4) NUMBITS(1) [],
        MBP_READY OFFSET(5) NUMBITS(1) [],
        MFS_FAILURE OFFSET(6) NUMBITS(1) [],
        WARM_RESET_REQ_FOR_DF OFFSET(7) NUMBITS(1) [],
        CPU_REPLACED_VALID OFFSET(8) NUMBITS(1) [],
        FW_UPDATE_IPU OFFSET(11) NUMBITS(1) [],
        CURRENT_STATE OFFSET(16) NUMBITS(8) [],
        CURRENT_PMEVENT OFFSET(24) NUMBITS(4) [],
        PROGRESS_CODE OFFSET(28) NUMBITS(4) []
    ],
    /// Host general status (H_GS, config 0x4c): the DRAM-init-done message.
    pub H_GS [
        UMA_BASE OFFSET(0) NUMBITS(16) [],
        STATUS OFFSET(24) NUMBITS(4) [
            Success = 0,
            NoMemory = 1,
            Error = 2
        ],
        INIT_DONE OFFSET(28) NUMBITS(4) []
    ]
];

register_bitfields![u32,
    /// Extend register status (HERES, config 0xbc).
    pub HERES [
        ALGORITHM OFFSET(0) NUMBITS(4) [
            Sha1 = 0,
            Sha256 = 2
        ],
        FEATURE_PRESENT OFFSET(30) NUMBITS(1) [],
        VALID OFFSET(31) NUMBITS(1) []
    ],
    /// Host and ME circular-buffer control/status (`struct mei_csr`).
    pub MEI_CSR [
        INTERRUPT_ENABLE OFFSET(0) NUMBITS(1) [],
        INTERRUPT_STATUS OFFSET(1) NUMBITS(1) [],
        INTERRUPT_GENERATE OFFSET(2) NUMBITS(1) [],
        READY OFFSET(3) NUMBITS(1) [],
        RESET OFFSET(4) NUMBITS(1) [],
        READ_PTR OFFSET(8) NUMBITS(8) [],
        WRITE_PTR OFFSET(16) NUMBITS(8) [],
        DEPTH OFFSET(24) NUMBITS(8) []
    ],
    /// `struct mei_header`.
    pub MEI_HEADER [
        CLIENT OFFSET(0) NUMBITS(8) [],
        HOST OFFSET(8) NUMBITS(8) [],
        LENGTH OFFSET(16) NUMBITS(9) [],
        COMPLETE OFFSET(31) NUMBITS(1) []
    ],
    /// `struct mkhi_header`.
    pub MKHI_HEADER [
        GROUP OFFSET(0) NUMBITS(8) [],
        COMMAND OFFSET(8) NUMBITS(7) [],
        RESPONSE OFFSET(15) NUMBITS(1) [],
        RESULT OFFSET(24) NUMBITS(8) []
    ]
];

pci_type0_config! {
    /// MEI1 (HECI) configuration space.
    pub struct MeiConfig {
        (0x40 => pub hfs: MmioReadOnly<u32, HFS::Register>),
        (0x44 => pub uma: MmioReadOnly<u32, UMA::Register>),
        (0x48 => pub gmes: MmioReadOnly<u32, GMES::Register>),
        (0x4c => pub h_gs: MmioReadWrite<u32, H_GS::Register>),
        (0x50 => _rsvd0),
        (0xbc => pub heres: MmioReadOnly<u32, HERES::Register>),
        (0xc0 => pub her: [MmioReadOnly<u32>; 8]),
        (0xe0 => _rsvd1),
        (0x100 => @END),
    }
}

tock_registers::register_structs! {
    /// MEI MMIO (BAR0).
    pub MeiMmio {
        (0x00 => pub h_cb_ww: MmioReadWrite<u32>),
        (0x04 => pub h_csr: MmioReadWrite<u32, MEI_CSR::Register>),
        (0x08 => pub me_cb_rw: MmioReadOnly<u32>),
        (0x0c => pub me_csr_ha: MmioReadOnly<u32, MEI_CSR::Register>),
        (0x10 => @END),
    }
}

/// Device IDs of the 6- and 7-series MEI (ME firmware 7.x and 8.x).
pub const DEVID_ME_7: u16 = 0x1c3a;
pub const DEVID_ME_8: u16 = 0x1e3a;
const MEI_ADDRESS_MKHI: u32 = 0x07;
const MKHI_GROUP_ID_GEN: u32 = 0xff;
const MKHI_END_OF_POST: u32 = 0x0c;
const MKHI_GROUP_ID_FWCAPS: u32 = 0x03;
const MKHI_FWCAPS_SET_RULE: u32 = 0x03;
const MKHI_DISABLE_RULE_ID: u32 = 0x06;
// coreboot `struct me_disable`: packed u32 rule_id followed by u16 data.
const SOFT_DISABLE_REQUEST: [u8; 6] = [6, 0, 0, 0, 1, 0];
const MEI_RETRY: u32 = 100_000;
const MEI_DELAY_US: u32 = 10;

/// coreboot `me_bios_path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeBiosPath {
    Normal,
    S3Wake,
    Error,
    Recovery,
    Disable,
    FirmwareUpdate,
}

pub use crate::southbridge::options::MeState;

register_bitfields![u8,
    /// coreboot `me_state_prev`: the state a transition was attempted to.
    ME_CONTROL_STATE [
        DISABLED OFFSET(0) NUMBITS(1) [],
        ATTEMPTED OFFSET(1) NUMBITS(1) []
    ]
];

/// The CMOS option record of [`ME_CONTROL_STATE`], beside the `me_state`
/// option it guards.
const ME_STATE_PREV: &str = "me_state_prev";

/// The reset-loop guard of an ME state change (coreboot `me_state_prev`),
/// a record in the platform's CMOS option store: a requested state is
/// attempted once, until the request changes or the record is cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeStateGuard {
    window: fstart_options::cmos::Window,
}

impl MeStateGuard {
    #[must_use]
    pub const fn new(window: fstart_options::cmos::Window) -> Self {
        Self { window }
    }

    fn store(self) -> fstart_options::cmos::Cmos<fstart_driver_rtc::Cmos> {
        fstart_options::cmos::Cmos::new(self.window, fstart_driver_rtc::Cmos)
    }

    fn load(self) -> Option<u8> {
        use fstart_options::Source;
        let value = self.store().read(ME_STATE_PREV).ok().flatten()?;
        u8::try_from(value).ok()
    }

    fn save(self, state: MeState) -> Result<(), ServiceError> {
        if self
            .store()
            .write(ME_STATE_PREV, u32::from(state.marker()))
            .is_err()
        {
            fstart_log::error!("intel-me: cannot persist transition guard; refusing reset");
            return Err(ServiceError::HardwareError);
        }
        Ok(())
    }
}

/// The requested ME state and the storage for its reset-loop guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeControl {
    pub state: MeState,
    pub guard: MeStateGuard,
}

trait Marker {
    fn marker(self) -> u8;
}

impl Marker for MeState {
    fn marker(self) -> u8 {
        (ME_CONTROL_STATE::DISABLED.val(u8::from(self == Self::Disabled))
            + ME_CONTROL_STATE::ATTEMPTED::SET)
            .value
    }
}

impl MeBiosPath {
    fn from_status(device_id: u16, hfs: u32, gmes: u32) -> Self {
        let hfs = tock_registers::LocalRegisterCopy::<u32, HFS::Register>::new(hfs);
        let gmes = tock_registers::LocalRegisterCopy::<u32, GMES::Register>::new(gmes);
        let mut path = match hfs.read(HFS::WORKING_STATE) {
            5 => Self::Normal,
            2 => Self::Recovery,
            _ => Self::Disable,
        };
        if !hfs.matches_all(HFS::OPERATION_MODE::Normal) {
            path = Self::Disable;
        }
        if hfs.is_set(HFS::UPDATE_IN_PROGRESS) {
            path = Self::FirmwareUpdate;
        }
        if hfs.read(HFS::ERROR_CODE) != 0 || hfs.is_set(HFS::FPT_BAD) {
            path = Self::Error;
        }
        // MBP belongs to normal ME 8 startup, not soft-disable mode. In
        // particular its absence must not prevent the H_GS re-enable path.
        if path == Self::Normal && device_id == DEVID_ME_8 && !gmes.is_set(GMES::MBP_READY) {
            path = Self::Error;
        }
        path
    }
}

impl MeControl {
    fn transition(
        self,
        path: MeBiosPath,
        mode: u32,
        previous: Option<u8>,
    ) -> Result<Option<MeState>, ServiceError> {
        let current = match path {
            MeBiosPath::Normal => MeState::Normal,
            MeBiosPath::Disable if mode == HFS::OPERATION_MODE::Value::Disable as u32 => {
                MeState::Disabled
            }
            // No MKHI or H_GS writes on S3, errors, recovery, debug/override.
            _ => return Ok(None),
        };
        if current == self.state {
            return Ok(None);
        }
        if previous == Some(self.state.marker()) {
            fstart_log::error!(
                "intel-me: state transition already attempted; refusing another reset"
            );
            return Err(ServiceError::HardwareError);
        }
        Ok(Some(self.state))
    }
}

/// Polling budget for the UMA-valid bit (coreboot `ME_RETRY` * `ME_DELAY`).
const UMA_RETRY: u32 = 100_000;
const UMA_DELAY_US: u32 = 10;

/// What the ME asked the BIOS to do after DRAM init done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeAction {
    Continue,
    /// Host reset without a power cycle.
    Reset,
    /// Power-cycle reset.
    PowerCycle,
    /// Global (ME and host) reset.
    GlobalReset,
    /// The ME asked for a sleep state; coreboot continues.
    Sleep,
}

/// The MEI1 function on bus 0.
pub struct Me {
    dev: EcamDevice,
}

impl Default for Me {
    fn default() -> Self {
        Self::new()
    }
}

impl Me {
    #[must_use]
    pub fn new() -> Self {
        Self {
            dev: EcamDevice::new(0, ME_DEV, ME_FUNC),
        }
    }

    fn regs(&self) -> Option<&'static MeiConfig> {
        let id = self.dev.read32(0);
        if matches!(id, 0xffff_ffff | 0 | 0x0000_ffff | 0xffff_0000) {
            return None;
        }
        // SAFETY: the MEI function answered; ECAM is enabled.
        Some(unsafe { self.dev.regs::<MeiConfig>() })
    }

    /// coreboot `intel_early_me_init()`: wait for the ME's UMA size and check
    /// its firmware. Returns whether the ME is usable.
    pub fn early_init(&self) -> bool {
        fstart_log::info!("intel-me: early init");
        let Some(regs) = self.regs() else {
            fstart_log::info!("intel-me: MEI device absent");
            return false;
        };
        let ready = (0..UMA_RETRY).any(|attempt| {
            if attempt != 0 {
                fstart_arch::udelay(UMA_DELAY_US);
            }
            regs.uma.is_set(UMA::VALID)
        });
        if !ready {
            fstart_log::error!("intel-me: ME is not ready");
            return false;
        }
        if regs.hfs.is_set(HFS::FPT_BAD) {
            fstart_log::warn!("intel-me: ME has bad firmware");
            return false;
        }
        fstart_log::info!("intel-me: ME firmware is ready");
        true
    }

    /// coreboot `intel_early_me_uma_size()`: requested UMA in MiB.
    #[must_use]
    pub fn uma_size_mib(&self) -> u32 {
        match self.regs() {
            Some(regs) if regs.uma.is_set(UMA::VALID) => {
                let size = regs.uma.read(UMA::SIZE_MIB);
                fstart_log::info!("intel-me: requested {} MiB UMA", size);
                size
            }
            _ => {
                fstart_log::info!("intel-me: invalid UMA size");
                0
            }
        }
    }

    /// coreboot `intel_early_me_cpu_replaced()`: whether the ME reports a
    /// replaced CPU, which forces full DRAM training. `since_boot_ms` is the
    /// time already elapsed; the ME gets until 50 ms after boot to answer.
    /// `Err` means the ME demands a reset for dynamic fusing.
    pub fn cpu_replaced(&self, since_boot_ms: u32) -> Result<bool, MeAction> {
        let Some(regs) = self.regs() else {
            return Ok(false);
        };
        let mut gmes = regs.gmes.extract();
        let mut elapsed = since_boot_ms;
        while !gmes.is_set(GMES::CPU_REPLACED_VALID) && elapsed < 50 {
            fstart_arch::udelay(1000);
            gmes = regs.gmes.extract();
            elapsed += 1;
        }
        if !gmes.is_set(GMES::CPU_REPLACED_VALID) {
            fstart_log::info!("intel-me: CPU replaced is not valid, assuming not replaced");
            return Ok(false);
        }
        if gmes.is_set(GMES::WARM_RESET_REQ_FOR_DF) {
            fstart_log::info!("intel-me: CPU was replaced and warm reset required");
            return Err(MeAction::Reset);
        }
        if gmes.is_set(GMES::CPU_REPLACED_STS) {
            fstart_log::info!("intel-me: full memory training required");
            return Ok(true);
        }
        Ok(false)
    }

    /// coreboot `intel_early_me_init_done()`: report DRAM ready with the ME
    /// UMA base (MESEG_BASE bits 35:20) and wait for the ME's answer.
    pub fn init_done(&self, uma_base_mib: u32) -> MeAction {
        let Some(regs) = self.regs() else {
            return MeAction::Continue;
        };
        let hfs = regs.hfs.extract();
        let debug = hfs.matches_all(HFS::OPERATION_MODE::Debug);
        if !hfs.matches_all(HFS::OPERATION_MODE::Normal) {
            fstart_log::info!(
                "intel-me: wrong mode {}",
                hfs.read(HFS::OPERATION_MODE) as u8
            );
        }
        if hfs.read(HFS::ERROR_CODE) != 0 {
            fstart_log::info!("intel-me: HFS error {}", hfs.read(HFS::ERROR_CODE) as u8);
        }
        log_gmes(regs.gmes.get());
        fstart_log::info!("intel-me: telling the ME that DRAM is ready");
        regs.h_gs.write(
            H_GS::UMA_BASE.val(uma_base_mib & 0xffff)
                + H_GS::STATUS::Success
                + H_GS::INIT_DONE.val(1),
        );
        let mut hfs = regs.hfs.extract();
        if debug {
            fstart_log::info!("intel-me: ME reports disabled, not waiting for a response");
        } else {
            fstart_arch::udelay(100);
            for _ in 0..=5000 {
                fstart_arch::udelay(1000);
                hfs = regs.hfs.extract();
                if hfs.matches_all(HFS::BIOS_MSG_ACK::DramAck) {
                    break;
                }
            }
        }
        log_gmes(regs.gmes.get());
        let action = match hfs.read_as_enum(HFS::ACK_DATA) {
            Some(HFS::ACK_DATA::Value::Reset) => MeAction::Reset,
            Some(HFS::ACK_DATA::Value::PowerCycle) => MeAction::PowerCycle,
            Some(HFS::ACK_DATA::Value::GlobalReset) => MeAction::GlobalReset,
            Some(
                HFS::ACK_DATA::Value::S3 | HFS::ACK_DATA::Value::S4 | HFS::ACK_DATA::Value::S5,
            ) => MeAction::Sleep,
            _ => MeAction::Continue,
        };
        fstart_log::info!(
            "intel-me: requested BIOS action {}",
            hfs.read(HFS::ACK_DATA) as u8
        );
        action
    }

    /// coreboot `intel_early_me_status()`.
    pub fn log_status(&self) {
        match self.regs() {
            Some(regs) => {
                let hfs = regs.hfs.extract();
                fstart_log::info!(
                    "intel-me: HFS {:#x}: mode {} state {} error {} init {}",
                    hfs.get(),
                    hfs.read(HFS::OPERATION_MODE) as u8,
                    hfs.read(HFS::WORKING_STATE) as u8,
                    hfs.read(HFS::ERROR_CODE) as u8,
                    hfs.read(HFS::FW_INIT_COMPLETE) as u8,
                );
                log_gmes(regs.gmes.get());
            }
            None => fstart_log::info!("intel-me: MEI device absent"),
        }
    }
}

impl MeAction {
    /// Perform a requested reset (coreboot `intel_early_me_init_done` tail):
    /// CF9 with the global-reset bit (`ETR3.CF9GR`) as the ME asks. Returns
    /// for `Continue` and the sleep requests coreboot also ignores.
    pub fn perform(self) {
        use super::regs::{ETR3, LpcConfig};
        let global = match self {
            Self::Continue | Self::Sleep => return,
            Self::Reset | Self::PowerCycle => false,
            Self::GlobalReset => true,
        };
        // SAFETY: the PCH LPC bridge is the fixed 00:1f.0 and ECAM is up.
        let lpc = unsafe { EcamDevice::from_devfn(0, super::dev::LPC).regs::<LpcConfig>() };
        lpc.etr3.modify(ETR3::CF9GR.val(u32::from(global)));
        fstart_log::flush();
        match self {
            Self::Reset => fstart_arch::x86::boot::system_reset(true),
            _ => fstart_arch::x86::boot::full_reset(),
        }
    }
}

impl Me {
    /// The MEI device ID, when present.
    #[must_use]
    pub fn device_id(&self) -> Option<u16> {
        self.regs().map(|regs| regs.device_id.get())
    }

    /// coreboot `intel_me_path`.
    #[must_use]
    pub fn bios_path(&self, s3_wake: bool) -> MeBiosPath {
        if s3_wake {
            return MeBiosPath::S3Wake;
        }
        let Some(regs) = self.regs() else {
            return MeBiosPath::Disable;
        };
        let hfs = regs.hfs.extract();
        let gmes = regs.gmes.extract();
        self.log_status();
        MeBiosPath::from_status(regs.device_id.get(), hfs.get(), gmes.get())
    }

    /// Apply an explicit board policy after normal MEI/MBP initialization,
    /// before END_OF_POST. The caller performs a returned global reset before
    /// PCH lockdown. Failed commands never trigger a reset.
    pub fn apply_control(
        &self,
        control: MeControl,
        path: MeBiosPath,
        mei: Option<&Mei>,
    ) -> Result<MeAction, ServiceError> {
        if !matches!(path, MeBiosPath::Normal | MeBiosPath::Disable) {
            return Ok(MeAction::Continue);
        }
        let regs = self.regs().ok_or(ServiceError::NotInitialized)?;
        if !matches!(regs.device_id.get(), DEVID_ME_7 | DEVID_ME_8) {
            return Err(ServiceError::HardwareError);
        }
        let hfs = regs.hfs.extract();
        let mode = hfs.read(HFS::OPERATION_MODE);
        // Status can change while the MBP/version exchange is in progress.
        if hfs.is_set(HFS::FPT_BAD)
            || hfs.is_set(HFS::UPDATE_IN_PROGRESS)
            || hfs.read(HFS::ERROR_CODE) != 0
            || (path == MeBiosPath::Normal
                && (hfs.read(HFS::WORKING_STATE) != 5
                    || !hfs.matches_all(HFS::OPERATION_MODE::Normal)))
        {
            return Err(ServiceError::HardwareError);
        }
        if path == MeBiosPath::Disable && mode != HFS::OPERATION_MODE::Value::Disable as u32 {
            // Debug/jumper/override modes are not soft-disable mode.
            fstart_log::error!("intel-me: requested policy cannot change this operation mode");
            return Err(ServiceError::HardwareError);
        }
        let previous = control.guard.load();
        let transition = control.transition(path, mode, previous)?;
        if transition == Some(MeState::Disabled) && mei.is_none() {
            return Err(ServiceError::NotInitialized);
        }
        if previous != Some(control.state.marker()) {
            // Persist before the command: even an ambiguous failure must not
            // cause an automatic retry/reset loop on the next boot.
            control.guard.save(control.state)?;
        }
        match transition {
            None => return Ok(MeAction::Continue),
            Some(MeState::Disabled) => {
                let mei = mei.ok_or(ServiceError::NotInitialized)?;
                fstart_log::info!("intel-me: entering soft temporary disable mode");
                mei.enter_soft_temp_disable()?;
                // coreboot/vendor requirement: an ACK alone is too early to
                // reset; ME needs one second to commit the requested state.
                fstart_arch::udelay(1_000_000);
            }
            Some(MeState::Normal) => {
                fstart_log::info!("intel-me: leaving soft temporary disable mode");
                // H_GS = 0x20000000, not an MKHI enable request.
                regs.h_gs.write(H_GS::INIT_DONE.val(2));
                let ready = (0..400).any(|_| {
                    fstart_arch::udelay(50_000);
                    regs.hfs.is_set(HFS::FW_INIT_COMPLETE)
                });
                if !ready {
                    fstart_log::error!("intel-me: enable timed out after 20 seconds");
                    return Err(ServiceError::Timeout);
                }
            }
        }
        Ok(MeAction::GlobalReset)
    }

    /// coreboot `intel_me_extend_valid`: log the firmware hash.
    pub fn extend_valid(&self) -> bool {
        let Some(regs) = self.regs() else {
            return false;
        };
        let status = regs.heres.extract();
        if !status.is_set(HERES::FEATURE_PRESENT) || !status.is_set(HERES::VALID) {
            fstart_log::error!("intel-me: extend register not present or invalid");
            return false;
        }
        let count = match status.read_as_enum(HERES::ALGORITHM) {
            Some(HERES::ALGORITHM::Value::Sha1) => 5,
            Some(HERES::ALGORITHM::Value::Sha256) => 8,
            None => {
                fstart_log::error!("intel-me: unknown extend algorithm");
                return false;
            }
        };
        for her in &regs.her[..count] {
            fstart_log::debug!("intel-me: extend {:#010x}", her.get());
        }
        true
    }

    /// The MEI MMIO window as PCI enumeration assigned it, with memory
    /// decode and bus mastering enabled (coreboot `intel_mei_setup`).
    #[must_use]
    pub fn mei(&self) -> Option<Mei> {
        self.regs()?;
        let base = self.dev.memory_bar(0x10)?;
        if base == 0 {
            return None;
        }
        self.dev.or16(0x04, 1 << 1 | 1 << 2);
        // SAFETY: BAR0 is the 16-byte MEI MMIO window assigned by enumeration.
        let mmio = unsafe { &*(base as usize as *const MeiMmio) };
        mmio.h_csr
            .modify(MEI_CSR::INTERRUPT_GENERATE::SET + MEI_CSR::READY::SET + MEI_CSR::RESET::CLEAR);
        Some(Mei { mmio })
    }

    /// Hide MEI1 (coreboot `intel_me_hide` / the `me_smm.c` tail): clear its
    /// decode and set the function-disable bit through `hide`.
    pub fn hide(&self, hide: impl FnOnce()) {
        if self.regs().is_some() {
            self.dev.and16(0x04, !(1 << 0 | 1 << 1 | 1 << 2));
        }
        hide();
    }

    /// coreboot `intel_me_finalize_smm`: END_OF_POST in the normal working
    /// state, then disable decode; the caller hides the function.
    pub fn finalize(&self) {
        let Some(regs) = self.regs() else {
            return;
        };
        let hfs = regs.hfs.extract();
        if hfs.is_set(HFS::FPT_BAD)
            || hfs.read(HFS::WORKING_STATE) != 5
            || !hfs.matches_all(HFS::OPERATION_MODE::Normal)
        {
            return;
        }
        match (regs.device_id.get(), self.mei()) {
            (DEVID_ME_7 | DEVID_ME_8, Some(mei)) => {
                // ME 8 answers END_OF_POST with a status dword.
                let mut ack = [0u32; 1];
                let response: &mut [u32] = if regs.device_id.get() == DEVID_ME_8 {
                    &mut ack
                } else {
                    &mut []
                };
                match mei.sendrecv(MKHI_GROUP_ID_GEN, MKHI_END_OF_POST, &[], response) {
                    Ok(()) => fstart_log::info!("intel-me: END OF POST successful ({})", ack[0]),
                    Err(_) => fstart_log::error!("intel-me: END OF POST failed"),
                }
            }
            (did, _) => fstart_log::error!("intel-me: no finalize handler for ME {:#x}", did),
        }
        self.dev.and16(0x04, !(1 << 0 | 1 << 1 | 1 << 2));
    }
}

/// The MEI circular-buffer transport (coreboot `me_common.c`).
pub struct Mei {
    mmio: &'static MeiMmio,
}

impl Mei {
    fn wait_me_ready(&self) -> Result<(), ServiceError> {
        for _ in 0..MEI_RETRY {
            if self.mmio.me_csr_ha.is_set(MEI_CSR::READY) {
                return Ok(());
            }
            fstart_arch::udelay(MEI_DELAY_US);
        }
        fstart_log::error!("intel-me: failed to become ready");
        Err(ServiceError::Timeout)
    }

    fn reset(&self) {
        if self.wait_me_ready().is_err() {
            return;
        }
        self.mmio
            .h_csr
            .modify(MEI_CSR::RESET::SET + MEI_CSR::INTERRUPT_GENERATE::SET);
        if self.wait_me_ready().is_err() {
            return;
        }
        self.mmio
            .h_csr
            .modify(MEI_CSR::INTERRUPT_GENERATE::SET + MEI_CSR::READY::SET + MEI_CSR::RESET::CLEAR);
    }

    fn free(&self) -> u32 {
        let host = self.mmio.h_csr.extract();
        let used = host
            .read(MEI_CSR::WRITE_PTR)
            .wrapping_sub(host.read(MEI_CSR::READ_PTR))
            & 0xff;
        host.read(MEI_CSR::DEPTH).saturating_sub(used)
    }

    fn request_header(payload_len: usize) -> Result<u32, ServiceError> {
        let length = 4usize
            .checked_add(payload_len)
            .ok_or(ServiceError::InvalidParam)?;
        if length > 0x1ff {
            return Err(ServiceError::InvalidParam);
        }
        Ok((MEI_HEADER::CLIENT.val(MEI_ADDRESS_MKHI)
            + MEI_HEADER::LENGTH.val(length as u32)
            + MEI_HEADER::COMPLETE::SET)
            .value)
    }

    fn payload_words(data: &[u8]) -> impl Iterator<Item = u32> + '_ {
        data.chunks(4).map(|chunk| {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            u32::from_le_bytes(word)
        })
    }

    fn response_header_valid(raw: u32, words: usize) -> bool {
        let header = tock_registers::LocalRegisterCopy::<u32, MEI_HEADER::Register>::new(raw);
        header.is_set(MEI_HEADER::COMPLETE)
            && header.read(MEI_HEADER::CLIENT) == MEI_ADDRESS_MKHI
            && header.read(MEI_HEADER::HOST) == 0
            && header.read(MEI_HEADER::LENGTH) as usize == 4 + 4 * words
    }

    fn reply_valid(raw: u32, group: u32, command: u32) -> bool {
        let reply = tock_registers::LocalRegisterCopy::<u32, MKHI_HEADER::Register>::new(raw);
        reply.is_set(MKHI_HEADER::RESPONSE)
            && reply.read(MKHI_HEADER::GROUP) == group
            && reply.read(MKHI_HEADER::COMMAND) == command
            && reply.read(MKHI_HEADER::RESULT) == 0
    }

    /// One MKHI request with a byte-exact payload and a response of
    /// `response.len()` dwords (coreboot `mei_sendrecv`).
    pub fn sendrecv(
        &self,
        group: u32,
        command: u32,
        data: &[u8],
        response: &mut [u32],
    ) -> Result<(), ServiceError> {
        // MEI header, MKHI header, payload.
        if response.len() > 126 {
            return Err(ServiceError::InvalidParam);
        }
        let header = Self::request_header(data.len())?;
        let length = MEI_HEADER::LENGTH.read(header);
        let needed = 1 + length.div_ceil(4);
        if self.free() < needed {
            fstart_log::error!("intel-me: circular buffer full, resetting");
            self.reset();
        }
        if self.free() < needed {
            fstart_log::error!("intel-me: message too large for buffer");
            return Err(ServiceError::HardwareError);
        }
        let mkhi = (MKHI_HEADER::GROUP.val(group) + MKHI_HEADER::COMMAND.val(command)).value;
        self.mmio.h_cb_ww.set(header);
        self.mmio.h_cb_ww.set(mkhi);
        for word in Self::payload_words(data) {
            self.mmio.h_cb_ww.set(word);
        }
        self.mmio.h_csr.modify(MEI_CSR::INTERRUPT_GENERATE::SET);
        self.wait_me_ready()?;

        // Wait for the whole response in the ME's circular buffer.
        let expected = 2 + response.len() as u32;
        let ready = (0..MEI_RETRY).any(|attempt| {
            if attempt != 0 {
                fstart_arch::udelay(MEI_DELAY_US);
            }
            let me = self.mmio.me_csr_ha.extract();
            me.read(MEI_CSR::WRITE_PTR)
                .wrapping_sub(me.read(MEI_CSR::READ_PTR))
                & 0xff
                >= expected
        });
        if !ready {
            fstart_log::error!("intel-me: timeout waiting for data");
            return Err(ServiceError::HardwareError);
        }
        if !Self::response_header_valid(self.mmio.me_cb_rw.get(), response.len()) {
            fstart_log::error!("intel-me: invalid response envelope");
            return Err(ServiceError::HardwareError);
        }
        if !Self::reply_valid(self.mmio.me_cb_rw.get(), group, command) {
            fstart_log::error!("intel-me: invalid response");
            return Err(ServiceError::HardwareError);
        }
        for word in response.iter_mut() {
            *word = self.mmio.me_cb_rw.get();
        }
        self.mmio
            .h_csr
            .modify(MEI_CSR::INTERRUPT_STATUS::SET + MEI_CSR::INTERRUPT_GENERATE::SET);
        self.wait_me_ready()
    }

    /// coreboot `enter_soft_temp_disable`: SET_RULE 6 with u16 value 1.
    pub fn enter_soft_temp_disable(&self) -> Result<(), ServiceError> {
        let mut response = [0u32; 1];
        self.sendrecv(
            MKHI_GROUP_ID_FWCAPS,
            MKHI_FWCAPS_SET_RULE,
            &SOFT_DISABLE_REQUEST,
            &mut response,
        )?;
        if response[0] != MKHI_DISABLE_RULE_ID {
            fstart_log::error!("intel-me: disable response has wrong rule ID");
            return Err(ServiceError::HardwareError);
        }
        Ok(())
    }

    /// coreboot `mkhi_get_fw_version` (ME 7): log the firmware version.
    pub fn log_firmware_version(&self) -> Result<(), ServiceError> {
        const MKHI_GET_FW_VERSION: u32 = 0x02;
        // code minor/major, build/hotfix, recovery minor/major, build/hotfix.
        let mut version = [0u32; 4];
        self.sendrecv(MKHI_GROUP_ID_GEN, MKHI_GET_FW_VERSION, &[], &mut version)?;
        fstart_log::info!(
            "intel-me: firmware {}.{}.{}.{}, recovery {}.{}.{}.{}",
            version[0] >> 16,
            version[0] & 0xffff,
            version[1] & 0xffff,
            version[1] >> 16,
            version[2] >> 16,
            version[2] & 0xffff,
            version[3] & 0xffff,
            version[3] >> 16
        );
        Ok(())
    }

    /// coreboot `intel_me_read_mbp`: drain the ME 8 boot parameter block the
    /// ME queued for the BIOS, logging the firmware version.
    pub fn read_mbp(&self) -> Result<(), ServiceError> {
        let pending = |me: &tock_registers::LocalRegisterCopy<u32, MEI_CSR::Register>| {
            me.read(MEI_CSR::WRITE_PTR)
                .wrapping_sub(me.read(MEI_CSR::READ_PTR))
                & 0xff
        };
        let mut remaining = pending(&self.mmio.me_csr_ha.extract());
        if remaining == 0 {
            fstart_log::error!("intel-me: no MBP data");
            return Err(ServiceError::HardwareError);
        }
        // mbp_header: [7:0] size in dwords, [15:8] entries.
        let header = self.mmio.me_cb_rw.get();
        let size = header & 0xff;
        let mut entries = (header >> 8) & 0xff;
        if entries > size / 2 || remaining < size {
            fstart_log::error!(
                "intel-me: MBP of {} entries, {} words, {} pending",
                entries,
                size,
                remaining
            );
            return Err(ServiceError::HardwareError);
        }
        remaining -= 1;
        while entries > 0 {
            entries -= 1;
            if remaining == 0 {
                return Err(ServiceError::HardwareError);
            }
            // mbp_item_header: [7:0] app id, [15:8] item id, [23:16] length.
            let item = self.mmio.me_cb_rw.get();
            let length = (item >> 16) & 0xff;
            if length == 0 || length > remaining {
                return Err(ServiceError::HardwareError);
            }
            remaining -= length;
            let id = ((item >> 8) & 0xff) << 8 | (item & 0xff);
            let mut words = [0u32; 4];
            for n in 0..length - 1 {
                let word = self.mmio.me_cb_rw.get();
                if let Some(slot) = words.get_mut(n as usize) {
                    *slot = word;
                }
            }
            if id == 0x101 {
                // mbp_fw_version_name: major/minor, hotfix/build.
                fstart_log::info!(
                    "intel-me: firmware {}.{}.{}.{}",
                    words[0] & 0xffff,
                    words[0] >> 16,
                    words[1] & 0xffff,
                    words[1] >> 16
                );
            }
        }
        self.mmio.h_csr.modify(MEI_CSR::INTERRUPT_GENERATE::SET);
        let done = (0..MEI_RETRY).any(|attempt| {
            if attempt != 0 {
                fstart_arch::udelay(MEI_DELAY_US);
            }
            !self.mmio.h_csr.is_set(MEI_CSR::INTERRUPT_GENERATE)
        });
        if done {
            Ok(())
        } else {
            Err(ServiceError::Timeout)
        }
    }
}

fn log_gmes(raw: u32) {
    let gmes = tock_registers::LocalRegisterCopy::<u32, GMES::Register>::new(raw);
    fstart_log::info!(
        "intel-me: FWS2 {:#x}: state {:#x} progress {:#x} pm event {:#x} cpu replaced {}/{}",
        raw,
        gmes.read(GMES::CURRENT_STATE),
        gmes.read(GMES::PROGRESS_CODE),
        gmes.read(GMES::CURRENT_PMEVENT),
        gmes.read(GMES::CPU_REPLACED_STS),
        gmes.read(GMES::CPU_REPLACED_VALID),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(state: MeState) -> MeControl {
        MeControl {
            state,
            guard: MeStateGuard::new(fstart_options::cmos::Window {
                first: 0x40,
                last: 0x7f,
            }),
        }
    }

    #[test]
    fn disable_request_is_the_packed_coreboot_rule_not_two_dwords() {
        let header = Mei::request_header(SOFT_DISABLE_REQUEST.len()).unwrap();
        assert_eq!(header, 0x800a_0007); // 4 MKHI + 6 data bytes
        assert_eq!(1 + MEI_HEADER::LENGTH.read(header).div_ceil(4), 4);
        let words: alloc::vec::Vec<_> = Mei::payload_words(&SOFT_DISABLE_REQUEST).collect();
        assert_eq!(words, [MKHI_DISABLE_RULE_ID, 1]);
        assert_eq!(Mei::request_header(0), Ok(0x8004_0007));
        assert!(Mei::request_header(508).is_err());
        assert!(Mei::request_header(usize::MAX).is_err());
        // Re-enable uses H_GS, not an invented MKHI command.
        assert_eq!(H_GS::INIT_DONE.val(2).value, 0x2000_0000);
    }

    #[test]
    fn disable_ack_requires_correct_envelope_command_and_success() {
        let header =
            (MEI_HEADER::CLIENT.val(7) + MEI_HEADER::LENGTH.val(8) + MEI_HEADER::COMPLETE::SET)
                .value;
        let reply =
            (MKHI_HEADER::GROUP.val(3) + MKHI_HEADER::COMMAND.val(3) + MKHI_HEADER::RESPONSE::SET)
                .value;
        assert!(Mei::response_header_valid(header, 1));
        assert!(Mei::reply_valid(reply, 3, 3));
        assert!(!Mei::response_header_valid(header & !(1 << 31), 1));
        assert!(!Mei::response_header_valid(header | 1 << 8, 1));
        assert!(!Mei::response_header_valid(header ^ 1, 1));
        for length in [4, 7, 9, 12] {
            let bad = (MEI_HEADER::CLIENT.val(7)
                + MEI_HEADER::LENGTH.val(length)
                + MEI_HEADER::COMPLETE::SET)
                .value;
            assert!(!Mei::response_header_valid(bad, 1));
        }
        assert!(!Mei::reply_valid(reply & !(1 << 15), 3, 3));
        assert!(!Mei::reply_valid(reply ^ 1, 3, 3));
        assert!(!Mei::reply_valid(reply ^ 1 << 8, 3, 3));
        assert!(!Mei::reply_valid(reply | 1 << 24, 3, 3));
    }

    #[test]
    fn circular_buffer_capacity_tracks_both_wrapping_pointers() {
        for (read, write, expected) in [
            (250, 254, 12),
            (254, 3, 11),
            (4, 4, 16),
            (4, 20, 0),
            (4, 21, 0),
        ] {
            let csr = (MEI_CSR::DEPTH.val(16)
                + MEI_CSR::READ_PTR.val(read)
                + MEI_CSR::WRITE_PTR.val(write))
            .value;
            let words = alloc::boxed::Box::leak(alloc::boxed::Box::new([0, csr, 0, 0]));
            // SAFETY: aligned, live backing storage for the complete register block.
            let mmio = unsafe { &*(words.as_ptr() as *const MeiMmio) };
            assert_eq!(Mei { mmio }.free(), expected);
        }
    }

    #[test]
    fn policy_changes_are_guarded_and_idempotent() {
        let disabled = HFS::OPERATION_MODE::Value::Disable as u32;
        let off = control(MeState::Disabled);
        let on = control(MeState::Normal);
        assert_eq!(
            off.transition(MeBiosPath::Normal, 0, None),
            Ok(Some(off.state))
        );
        assert_eq!(
            off.transition(MeBiosPath::Normal, 0, Some(on.state.marker())),
            Ok(Some(off.state))
        );
        assert!(
            off.transition(MeBiosPath::Normal, 0, Some(off.state.marker()))
                .is_err()
        );
        assert_eq!(
            off.transition(MeBiosPath::Disable, disabled, Some(off.state.marker())),
            Ok(None)
        );
        assert_eq!(
            on.transition(MeBiosPath::Disable, disabled, Some(off.state.marker())),
            Ok(Some(on.state))
        );
        assert!(
            on.transition(MeBiosPath::Disable, disabled, Some(on.state.marker()))
                .is_err()
        );
        assert_eq!(
            on.transition(MeBiosPath::Normal, 0, Some(on.state.marker())),
            Ok(None)
        );
    }

    #[test]
    fn policy_never_transitions_on_s3_recovery_errors_updates_or_override() {
        for state in [MeState::Normal, MeState::Disabled] {
            for path in [
                MeBiosPath::S3Wake,
                MeBiosPath::Recovery,
                MeBiosPath::Error,
                MeBiosPath::FirmwareUpdate,
            ] {
                assert_eq!(control(state).transition(path, 3, None), Ok(None));
            }
            for mode in [0, 2, 4, 5] {
                assert_eq!(
                    control(state).transition(MeBiosPath::Disable, mode, None),
                    Ok(None)
                );
            }
        }
    }

    #[test]
    fn guard_record_does_not_collide_with_an_option() {
        use crate::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
        let guard = fstart_options::cmos::hash(ME_STATE_PREV);
        let keys = fstart_log::option::KNOBS
            .iter()
            .map(|k| k.meta.key)
            .chain(
                crate::sandybridge::IntelSandybridge::KNOBS
                    .iter()
                    .map(|k| k.meta.key),
            )
            .chain(super::super::IntelBd82x6x::KNOBS.iter().map(|k| k.meta.key));
        for key in keys {
            assert_ne!(fstart_options::cmos::hash(key), guard, "{key}");
        }
    }

    #[test]
    fn guard_markers_are_coreboot_me_state_prev_values() {
        assert_eq!(MeState::Normal.marker(), 2);
        assert_eq!(MeState::Disabled.marker(), 3);
    }

    #[test]
    fn bios_path_uses_recovery_state_two_and_allows_me8_reenable_without_mbp() {
        let normal = (HFS::WORKING_STATE.val(5) + HFS::OPERATION_MODE::Normal).value;
        assert_eq!(
            MeBiosPath::from_status(DEVID_ME_7, normal, 0),
            MeBiosPath::Normal
        );
        assert_eq!(
            MeBiosPath::from_status(DEVID_ME_8, normal, 0),
            MeBiosPath::Error
        );
        assert_eq!(
            MeBiosPath::from_status(DEVID_ME_8, normal, GMES::MBP_READY::SET.value),
            MeBiosPath::Normal
        );
        let disabled = (HFS::WORKING_STATE.val(4) + HFS::OPERATION_MODE::Disable).value;
        assert_eq!(
            MeBiosPath::from_status(DEVID_ME_8, disabled, 0),
            MeBiosPath::Disable
        );
        assert_eq!(
            MeBiosPath::from_status(DEVID_ME_7, 2, 0),
            MeBiosPath::Recovery
        );
        assert_eq!(
            MeBiosPath::from_status(DEVID_ME_7, normal | HFS::FPT_BAD::SET.value, 0),
            MeBiosPath::Error
        );
        assert_eq!(
            MeBiosPath::from_status(DEVID_ME_7, normal | HFS::UPDATE_IN_PROGRESS::SET.value, 0),
            MeBiosPath::FirmwareUpdate
        );
    }

    #[test]
    fn did_message_matches_coreboot_layout() {
        // coreboot writes uma_base | (1 << 28): init_done=1, status=success.
        let did =
            (H_GS::UMA_BASE.val(0x1f80) + H_GS::STATUS::Success + H_GS::INIT_DONE.val(1)).value;
        assert_eq!(did, 0x1f80 | 1 << 28);
        let ack = (HFS::BIOS_MSG_ACK::DramAck + HFS::ACK_DATA::Continue).value;
        assert_eq!(ack >> 28, 1);
        assert_eq!((ack >> 25) & 7, 7);
    }
}
