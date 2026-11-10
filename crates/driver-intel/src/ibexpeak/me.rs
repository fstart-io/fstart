//! Intel Management Engine on the 5 Series PCH: status, the raminit UMA
//! handshake over HECI and the ramstage MEI policy.
//!
//! coreboot `ibexpeak/{me.c,setup_heci_uma.c}` and
//! `bd82x6x/{early_me.c,me_status.c}`. The firmware sends the ME no
//! messages besides `MKHI_SET_UMA` during raminit.

use super::regs::{FD2, VCTL};
use super::{IntelIbexPeak, PchFunction};
use crate::MmioBar;
use fstart_core::services::ServiceError;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

/// HECI/MEI #1.
const HECI: (u8, u8) = (0x16, 0);
/// coreboot `ME_RETRY` × `ME_DELAY`: one second.
const ME_RETRY: u32 = 100_000;
const ME_DELAY_US: u32 = 10;

register_bitfields![u32,
    /// Host Firmware Status (`PCI_ME_HFS`, 0x40).
    pub HFS [
        WORKING_STATE OFFSET(0) NUMBITS(4) [
            Reset = 0,
            Init = 1,
            Recovery = 2,
            Normal = 5,
            Wait = 6,
            Transition = 7,
            Invalid = 8
        ],
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
            Disabled = 3,
            OverrideJumper = 4,
            OverrideMei = 5
        ],
        BOOT_OPTIONS_PRESENT OFFSET(24) NUMBITS(1) [],
        ACK_DATA OFFSET(25) NUMBITS(3) [],
        BIOS_MSG_ACK OFFSET(28) NUMBITS(4) []
    ],
    /// UMA size request (`PCI_ME_UMA`, 0x44).
    pub UMA [
        SIZE OFFSET(0) NUMBITS(6) [],
        VALID OFFSET(16) NUMBITS(1) []
    ],
    /// General ME status (`PCI_ME_GMES`, 0x48).
    pub GMES [
        BIST_IN_PROGRESS OFFSET(0) NUMBITS(1) [],
        ICC_PROGRESS OFFSET(1) NUMBITS(2) [],
        INVOKE_MEBX OFFSET(3) NUMBITS(1) [],
        CPU_REPLACED OFFSET(4) NUMBITS(1) [],
        MBP_READY OFFSET(5) NUMBITS(1) [],
        MFS_FAILURE OFFSET(6) NUMBITS(1) [],
        WARM_RESET_REQUEST OFFSET(7) NUMBITS(1) [],
        CPU_REPLACED_VALID OFFSET(8) NUMBITS(1) [],
        FW_UPDATE_IPU OFFSET(11) NUMBITS(1) [],
        CURRENT_STATE OFFSET(16) NUMBITS(8) [],
        CURRENT_PMEVENT OFFSET(24) NUMBITS(4) [],
        PROGRESS_CODE OFFSET(28) NUMBITS(4) []
    ],
    /// Extend register status (`PCI_ME_HERES`, 0xbc).
    pub HERES [
        ALGORITHM OFFSET(0) NUMBITS(4) [
            Sha1 = 0,
            Sha256 = 2
        ],
        FEATURE_PRESENT OFFSET(30) NUMBITS(1) [],
        VALID OFFSET(31) NUMBITS(1) []
    ],
    /// MEI host/ME control and status (`MEI_H_CSR`, `MEI_ME_CSR_HA`).
    pub CSR [
        INTERRUPT_ENABLE OFFSET(0) NUMBITS(1) [],
        INTERRUPT_STATUS OFFSET(1) NUMBITS(1) [],
        INTERRUPT_GENERATE OFFSET(2) NUMBITS(1) [],
        READY OFFSET(3) NUMBITS(1) [],
        RESET OFFSET(4) NUMBITS(1) [],
        READ_PTR OFFSET(8) NUMBITS(8) [],
        WRITE_PTR OFFSET(16) NUMBITS(8) [],
        DEPTH OFFSET(24) NUMBITS(8) []
    ],
    /// A MEI message header.
    pub MEI_HEADER [
        CLIENT_ADDRESS OFFSET(0) NUMBITS(8) [],
        HOST_ADDRESS OFFSET(8) NUMBITS(8) [],
        LENGTH OFFSET(16) NUMBITS(9) [],
        COMPLETE OFFSET(31) NUMBITS(1) []
    ]
];

/// The MEI MMIO window (HECI BAR0).
#[derive(Clone, Copy)]
pub struct Mei(usize);

impl MmioBar for Mei {
    fn base(self) -> usize {
        self.0
    }
    fn mapped_size(self) -> Option<u32> {
        Some(0x10)
    }
}

const MEI_H_CB_WW: u32 = 0x00;
const MEI_H_CSR: u32 = 0x04;
const MEI_ME_CB_RW: u32 = 0x08;
const MEI_ME_CSR_HA: u32 = 0x0c;
/// MKHI client address and the UMA command.
const MEI_ADDRESS_MKHI: u8 = 7;
const MKHI_SET_UMA: u8 = 0x08;

impl Mei {
    fn host_csr(self) -> LocalRegisterCopy<u32, CSR::Register> {
        LocalRegisterCopy::new(self.read32(MEI_H_CSR))
    }
    fn me_csr(self) -> LocalRegisterCopy<u32, CSR::Register> {
        LocalRegisterCopy::new(self.read32(MEI_ME_CSR_HA))
    }

    fn wait(condition: impl FnMut() -> bool) -> Result<(), ServiceError> {
        fstart_arch::wait_us(ME_RETRY * ME_DELAY_US, condition)
            .then_some(())
            .ok_or(ServiceError::Timeout)
    }

    /// coreboot `wait_heci_ready()`: ME ready, then host ready with
    /// interrupt-generate and reset clear.
    fn wait_ready(self) -> Result<(), ServiceError> {
        Self::wait(|| self.me_csr().is_set(CSR::READY))?;
        let mut host = self.host_csr();
        host.modify(CSR::RESET::CLEAR + CSR::INTERRUPT_GENERATE::SET + CSR::READY::SET);
        self.write32(MEI_H_CSR, host.get());
        Ok(())
    }

    /// coreboot `wait_heci_cb_avail()`.
    fn wait_space(self, dwords: u32) -> Result<(), ServiceError> {
        Self::wait(|| self.me_csr().is_set(CSR::READY))?;
        Self::wait(|| {
            let host = self.host_csr();
            let used = host
                .read(CSR::WRITE_PTR)
                .wrapping_sub(host.read(CSR::READ_PTR))
                & 0xff;
            dwords <= host.read(CSR::DEPTH).saturating_sub(used)
        })
    }

    fn send_dwords(self, bytes: &[u8]) {
        for chunk in bytes.chunks(4) {
            let mut dword = [0u8; 4];
            dword[..chunk.len()].copy_from_slice(chunk);
            self.write32(MEI_H_CB_WW, u32::from_le_bytes(dword));
        }
    }

    /// coreboot `send_heci_message()`: split into packets of the buffer size.
    fn send(self, mut message: &[u8], host: u8, client: u8) -> Result<(), ServiceError> {
        self.wait_ready()?;
        let max = ((self.host_csr().read(CSR::DEPTH) * 4).saturating_sub(4)) as usize;
        while !message.is_empty() {
            let length = message.len().min(max);
            let header = MEI_HEADER::CLIENT_ADDRESS.val(u32::from(client))
                + MEI_HEADER::HOST_ADDRESS.val(u32::from(host))
                + MEI_HEADER::LENGTH.val(length as u32)
                + MEI_HEADER::COMPLETE.val(u32::from(length == message.len()));
            self.wait_space((4 + length as u32).div_ceil(4))?;
            self.write32(MEI_H_CB_WW, header.value);
            self.send_dwords(&message[..length]);
            let mut csr = self.host_csr();
            csr.modify(CSR::INTERRUPT_GENERATE::SET);
            self.write32(MEI_H_CSR, csr.get());
            message = &message[length..];
        }
        Ok(())
    }

    /// coreboot `recv_heci_message()`: up to `reply.len()` dwords; returns
    /// the bytes received.
    fn receive(self, reply: &mut [u32]) -> Result<usize, ServiceError> {
        let mut position = 0usize;
        loop {
            // Request the next packet.
            let mut csr = self.host_csr();
            csr.modify(CSR::INTERRUPT_STATUS::SET);
            self.write32(MEI_H_CSR, csr.get());
            Self::wait(|| {
                let me = self.me_csr();
                me.read(CSR::WRITE_PTR) != me.read(CSR::READ_PTR)
            })?;
            let header =
                LocalRegisterCopy::<u32, MEI_HEADER::Register>::new(self.read32(MEI_ME_CB_RW));
            let length = header.read(MEI_HEADER::LENGTH) as usize;
            if length == 0 {
                let mut csr = self.host_csr();
                csr.modify(CSR::INTERRUPT_STATUS::SET);
                self.write32(MEI_H_CSR, csr.get());
                return Err(ServiceError::IoError);
            }
            let dwords = length.div_ceil(4);
            if length + 4 > 4 * self.me_csr().read(CSR::DEPTH) as usize
                || position / 4 + dwords > reply.len()
            {
                return Err(ServiceError::IoError);
            }
            Self::wait(|| {
                let me = self.me_csr();
                me.read(CSR::WRITE_PTR).wrapping_sub(me.read(CSR::READ_PTR)) & 0xff >= dwords as u32
            })?;
            for slot in &mut reply[position / 4..position / 4 + dwords] {
                *slot = self.read32(MEI_ME_CB_RW);
            }
            let ready = self.me_csr().is_set(CSR::READY);
            let mut csr = self.host_csr();
            csr.modify(CSR::INTERRUPT_GENERATE::SET);
            self.write32(MEI_H_CSR, csr.get());
            if !ready {
                return Err(ServiceError::IoError);
            }
            position += length;
            if header.is_set(MEI_HEADER::COMPLETE) {
                return Ok(position);
            }
        }
    }
}

fn heci() -> EcamDevice {
    EcamDevice::new(0, HECI.0, HECI.1)
}

/// The register state the raminit UMA handshake touches on both DMI ends.
pub struct UmaChannels {
    pub dmibar: usize,
    pub rcba: usize,
    pub mchbar: usize,
}

/// Uncore DMIBAR virtual-channel registers (Ironlake `registers/dmibar.h`).
#[derive(Clone, Copy)]
struct DmiBar(usize);
impl MmioBar for DmiBar {
    fn base(self) -> usize {
        self.0
    }
    fn mapped_size(self) -> Option<u32> {
        Some(0x1000)
    }
}

/// coreboot `intel_me_status()` (status names only; per-phase state codes
/// are printed as numbers).
pub fn log_status(hfs: u32, gmes: u32) {
    let hfs = LocalRegisterCopy::<u32, HFS::Register>::new(hfs);
    let gmes = LocalRegisterCopy::<u32, GMES::Register>::new(gmes);
    const CWS: [&str; 9] = [
        "Reset",
        "Initializing",
        "Recovery",
        "Test",
        "Disabled",
        "Normal",
        "Platform Disable Wait",
        "OP State Transition",
        "Invalid CPU Plugged In",
    ];
    const OPSTATE: [&str; 8] = [
        "Preboot",
        "M0 with UMA",
        "unknown",
        "unknown",
        "M3 without UMA",
        "M0 without UMA",
        "Bring up",
        "M0 without UMA but with error",
    ];
    const OPMODE: [&str; 6] = [
        "Normal",
        "unknown",
        "Debug or Disabled by AltDisableBit",
        "Soft Temporary Disable",
        "Security Override via Jumper",
        "Security Override via MEI Message",
    ];
    const PROGRESS: [&str; 7] = [
        "ROM Phase",
        "BUP Phase",
        "uKernel Phase",
        "Policy Module",
        "Module Loading",
        "Unknown",
        "Host Communication",
    ];
    let name = |table: &[&'static str], index: u32| -> &'static str {
        table.get(index as usize).copied().unwrap_or("unknown")
    };
    let yes = |set: bool| if set { "YES" } else { "NO" };
    fstart_log::info!(
        "ME: FPT {}, bringup loader failure {}, init complete {}, mfg mode {}",
        if hfs.is_set(HFS::FPT_BAD) {
            "BAD"
        } else {
            "OK"
        },
        yes(hfs.is_set(HFS::FT_BUP_LD_FLR)),
        yes(hfs.is_set(HFS::FW_INIT_COMPLETE)),
        yes(hfs.is_set(HFS::MFG_MODE))
    );
    fstart_log::info!(
        "ME: working state {}, operation state {}, mode {}, error {}",
        name(&CWS, hfs.read(HFS::WORKING_STATE)),
        name(&OPSTATE, hfs.read(HFS::OPERATION_STATE)),
        name(&OPMODE, hfs.read(HFS::OPERATION_MODE)),
        hfs.read(HFS::ERROR_CODE)
    );
    fstart_log::info!(
        "ME: progress {} state {:#x}, PM event {:#x}",
        name(&PROGRESS, gmes.read(GMES::PROGRESS_CODE)),
        gmes.read(GMES::CURRENT_STATE),
        gmes.read(GMES::CURRENT_PMEVENT)
    );
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
        let dmi = DmiBar(channels.dmibar);
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
    let mchbar = DmiBar(channels.mchbar);
    mchbar.write32(0x24, 0x10000 + size_mib);
    send_uma_message(Mei(hecibar), address, size_mib)?;
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
        let mei = Mei(bar as usize);
        let mut host = mei.host_csr();
        host.modify(CSR::INTERRUPT_GENERATE::SET + CSR::READY::SET + CSR::RESET::CLEAR);
        mei.write32(MEI_H_CSR, host.get());
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
