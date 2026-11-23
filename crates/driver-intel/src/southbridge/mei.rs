//! Shared Intel Management Engine Interface (HECI / MEI) transport and PCI
//! status bitfields, used by Ibex Peak (5 Series) and Lynx Point (8 Series).

use fstart_core::mmio::MmioWindow;
use fstart_core::services::ServiceError;
use tock_registers::{LocalRegisterCopy, register_bitfields};

/// coreboot `ME_RETRY` × `ME_DELAY`: one second.
pub(crate) const ME_RETRY: u32 = 100_000;
pub(crate) const ME_DELAY_US: u32 = 10;

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
        MBP_CLEARED OFFSET(13) NUMBITS(1) [],
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

/// The MEI MMIO window (HECI BAR0, 16 bytes).
#[derive(Clone, Copy)]
pub struct Mei(MmioWindow);

const MEI_H_CB_WW: u32 = 0x00;
pub(crate) const MEI_H_CSR: u32 = 0x04;
pub(crate) const MEI_ME_CB_RW: u32 = 0x08;
const MEI_ME_CSR_HA: u32 = 0x0c;

/// MKHI client address.
pub(crate) const MEI_ADDRESS_MKHI: u8 = 7;

fn free_space(depth: u32, write: u32, read: u32) -> u32 {
    depth.saturating_sub(write.wrapping_sub(read) & 0xff)
}

fn read_fragment(bytes: &mut [u8], mut read: impl FnMut() -> u32) {
    for chunk in bytes.chunks_mut(4) {
        chunk.copy_from_slice(&read().to_le_bytes()[..chunk.len()]);
    }
}

impl Mei {
    /// Wrap a mapped 16-byte HECI BAR0 MMIO window.
    ///
    /// # Safety
    /// `base` must be the mapped 16-byte HECI BAR0 MMIO window.
    pub(crate) unsafe fn new(base: usize) -> Self {
        // SAFETY: forwarded from caller.
        Self(unsafe { MmioWindow::new(base, 0x10) })
    }

    /// Read a 32-bit dword from the HECI MMIO window.
    pub fn read_dword(self, offset: u32) -> u32 {
        self.0.read32(offset)
    }

    /// Write a 32-bit dword to the HECI MMIO window.
    pub fn write_dword(self, offset: u32, value: u32) {
        self.0.write32(offset, value);
    }

    pub(crate) fn host_csr(self) -> LocalRegisterCopy<u32, CSR::Register> {
        LocalRegisterCopy::new(self.0.read32(MEI_H_CSR))
    }

    pub(crate) fn me_csr(self) -> LocalRegisterCopy<u32, CSR::Register> {
        LocalRegisterCopy::new(self.0.read32(MEI_ME_CSR_HA))
    }

    pub(crate) fn set_host_ready(self) {
        let mut host = self.host_csr();
        host.modify(CSR::RESET::CLEAR + CSR::INTERRUPT_GENERATE::SET + CSR::READY::SET);
        self.0.write32(MEI_H_CSR, host.get());
    }

    fn wait(condition: impl FnMut() -> bool) -> Result<(), ServiceError> {
        fstart_arch::wait_us(ME_RETRY * ME_DELAY_US, condition)
            .then_some(())
            .ok_or(ServiceError::Timeout)
    }

    /// Establish the host side first; ME9 can wait for host READY.
    pub(crate) fn wait_ready(self) -> Result<(), ServiceError> {
        self.set_host_ready();
        Self::wait(|| self.me_csr().is_set(CSR::READY))
    }

    /// coreboot `wait_heci_cb_avail()`.
    fn wait_space(self, dwords: u32) -> Result<(), ServiceError> {
        Self::wait(|| self.me_csr().is_set(CSR::READY))?;
        Self::wait(|| {
            let host = self.host_csr();
            dwords
                <= free_space(
                    host.read(CSR::DEPTH),
                    host.read(CSR::WRITE_PTR),
                    host.read(CSR::READ_PTR),
                )
        })
    }

    fn send_dwords(self, bytes: &[u8]) {
        for chunk in bytes.chunks(4) {
            let mut dword = [0u8; 4];
            dword[..chunk.len()].copy_from_slice(chunk);
            self.0.write32(MEI_H_CB_WW, u32::from_le_bytes(dword));
        }
    }

    /// coreboot `send_heci_message()`: split into packets of the buffer size.
    pub(crate) fn send(self, message: &[u8], host: u8, client: u8) -> Result<(), ServiceError> {
        self.wait_ready()?;
        self.send_part(message, host, client, true)
    }

    /// Send one part of a multi-part MEI message (used for segmented ME9 payloads).
    pub(crate) fn send_part(
        self,
        mut message: &[u8],
        host: u8,
        client: u8,
        final_part: bool,
    ) -> Result<(), ServiceError> {
        // Protocol length is a 9-bit byte count. Keep non-final fragments
        // dword aligned, and never spin on a zero-depth controller.
        let max = ((self.host_csr().read(CSR::DEPTH) * 4).saturating_sub(4)).min(508) as usize;
        if max == 0 {
            return Err(ServiceError::IoError);
        }
        while !message.is_empty() {
            let length = message.len().min(max);
            let header = MEI_HEADER::CLIENT_ADDRESS.val(u32::from(client))
                + MEI_HEADER::HOST_ADDRESS.val(u32::from(host))
                + MEI_HEADER::LENGTH.val(length as u32)
                + MEI_HEADER::COMPLETE.val(u32::from(final_part && length == message.len()));
            self.wait_space((4 + length as u32).div_ceil(4))?;
            self.0.write32(MEI_H_CB_WW, header.value);
            self.send_dwords(&message[..length]);
            let mut csr = self.host_csr();
            csr.modify(CSR::INTERRUPT_GENERATE::SET);
            self.0.write32(MEI_H_CSR, csr.get());
            Self::wait(|| self.me_csr().is_set(CSR::READY))?;
            message = &message[length..];
        }
        Ok(())
    }

    /// coreboot `recv_heci_message()`: up to `reply.len()` dwords; returns
    /// the bytes received.
    pub(crate) fn receive(self, reply: &mut [u32]) -> Result<usize, ServiceError> {
        let mut position = 0usize;
        loop {
            Self::wait(|| self.me_csr().is_set(CSR::READY))?;
            Self::wait(|| {
                let me = self.me_csr();
                me.read(CSR::WRITE_PTR) != me.read(CSR::READ_PTR)
            })?;
            let header =
                LocalRegisterCopy::<u32, MEI_HEADER::Register>::new(self.0.read32(MEI_ME_CB_RW));
            let length = header.read(MEI_HEADER::LENGTH) as usize;
            if length == 0 {
                let mut csr = self.host_csr();
                csr.modify(CSR::INTERRUPT_STATUS::SET);
                self.0.write32(MEI_H_CSR, csr.get());
                return Err(ServiceError::IoError);
            }
            let dwords = length.div_ceil(4);
            if length + 4 > 4 * self.me_csr().read(CSR::DEPTH) as usize
                || position + length > core::mem::size_of_val(reply)
            {
                return Err(ServiceError::IoError);
            }
            Self::wait(|| {
                let me = self.me_csr();
                me.read(CSR::WRITE_PTR).wrapping_sub(me.read(CSR::READ_PTR)) & 0xff >= dwords as u32
            })?;
            use zerocopy::IntoBytes;
            read_fragment(
                &mut reply.as_mut_bytes()[position..position + length],
                || self.0.read32(MEI_ME_CB_RW),
            );
            let mut csr = self.host_csr();
            csr.modify(CSR::INTERRUPT_STATUS::SET + CSR::INTERRUPT_GENERATE::SET);
            self.0.write32(MEI_H_CSR, csr.get());
            Self::wait(|| self.me_csr().is_set(CSR::READY))?;
            position += length;
            if header.is_set(MEI_HEADER::COMPLETE) {
                return Ok(position);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_wrap_full_and_unaligned_fragments() {
        assert_eq!(free_space(8, 2, 254), 4);
        assert_eq!(free_space(8, 2, 250), 0);
        let mut result = [0; 7];
        read_fragment(&mut result[..3], || 0xff03_0201);
        read_fragment(&mut result[3..], || 0x0706_0504);
        assert_eq!(result, [1, 2, 3, 4, 5, 6, 7]);
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
