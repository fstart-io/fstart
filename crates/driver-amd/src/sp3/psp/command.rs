//! Typed C2P command buffers (PSP_APOB §3.3, §3.4).
//!
//! Every buffer starts with an 8-byte header `{size, status}`; `size`
//! counts the header. AGESA builds them zeroed and 32-byte aligned
//! (DXE `and rbx, ~0x1f`), which [`C2pBuffer`] reproduces.

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::sp3::soc::Generation;

/// Buffer header.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout,
)]
#[repr(C)]
pub struct Header {
    /// Total buffer size including this header.
    pub size: u32,
    /// Command status written back by the PSP (Rome: by the sender).
    pub status: u32,
}

/// A C2P request body: the bytes after the header.
pub trait Command: Copy + FromBytes + IntoBytes + Immutable {
    /// C2P command id.
    const ID: u8;
    /// AGESA's name for it.
    const NAME: &'static str;

    /// Whether a donor of `generation` sends this command; the mailbox
    /// refuses the others.
    fn supported(generation: Generation) -> bool;
}

/// A command buffer as the PSP reads it.
#[derive(Debug, Clone, Copy)]
#[repr(C, align(32))]
pub struct C2pBuffer<C> {
    pub header: Header,
    pub request: C,
}

impl<C: Command> C2pBuffer<C> {
    pub fn new(request: C) -> Self {
        Self {
            header: Header {
                size: (size_of::<Header>() + size_of::<C>()) as u32,
                status: 0,
            },
            request,
        }
    }
}

macro_rules! header_only {
    ($(#[$doc:meta])* $name:ident = $id:literal, $agesa:literal, |$g:ident| $supported:expr) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
        #[repr(C)]
        pub struct $name;

        impl Command for $name {
            const ID: u8 = $id;
            const NAME: &'static str = $agesa;

            fn supported($g: Generation) -> bool {
                $supported
            }
        }
    };
}

header_only!(
    /// ReadyToBoot ("ExitBootServices"): afterwards the PSP accepts
    /// commands from SMM only. All gens; Rome skips it when PCD 0xbf (KVM
    /// PSP message) is set, default FALSE (PSP_APOB §3.3).
    BootDone = 0x06,
    "BootDone",
    |_g| true
);
header_only!(
    /// Lock the DF registers; AGESA sends it at PciEnumerationComplete,
    /// after the last DF write (PSP_APOB §3.3).
    LockDfReg = 0x1b,
    "LockDFReg",
    |_g| true
);
header_only!(
    /// Naples only, from the SMM capsule hook (PSP_APOB §3.3).
    ClearSmmLock = 0x1c,
    "ClearSmmLock",
    |g| g == Generation::Naples
);
header_only!(
    /// Milan FCH lock, sent from the SW SMI 0xD2 handler with the SMM flag
    /// set (FCH_SP3 §SMM; FchHuashanSmmInit, decompiler-only). The handler
    /// sends 0x30 and [`LockFchReg3a`] under separate gates, both logged as
    /// "LockFCHReg".
    LockFchReg = 0x30,
    "LockFCHReg",
    |g| g == Generation::Milan
);
header_only!(
    /// The second "LockFCHReg" command of the Milan D2 handler (see
    /// [`LockFchReg`]); its gate flag was not traced.
    LockFchReg3a = 0x3a,
    "LockFCHReg",
    |g| g == Generation::Milan
);
header_only!(
    /// Milan ReadyToBoot when DRTM is off: sent only if a TPM answers at
    /// `0xfed40000` (dword != 0xffffffff) (PSP_APOB §3.3).
    ResetTpmEstablishment = 0x61,
    "ResetTpmEstablishment",
    |g| g == Generation::Milan
);

/// A register the PSP touches to raise or check its SMI (SmmInfo).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout,
)]
#[repr(C)]
pub struct SmmRegister {
    pub address: u64,
    /// 1 = memory (the only value AGESA uses).
    pub address_type: u32,
    /// 2 = dword (the only value AGESA uses).
    pub width: u32,
    /// Trigger: AND mask; check: mask.
    pub mask: u32,
    /// Trigger: OR value; check: expected value under `mask`.
    pub value: u32,
}

impl SmmRegister {
    const MEMORY: u32 = 1;
    const DWORD: u32 = 2;

    const fn mem32(address: u64, mask: u32, value: u32) -> Self {
        Self {
            address,
            address_type: Self::MEMORY,
            width: Self::DWORD,
            mask,
            value,
        }
    }
}

/// FCH SmiTrig0 (`SMI_REG + 0x98`).
pub const FCH_SMI_TRIGGER0: u64 = 0xfed8_0298;
/// FCH SmiControl for the PSP SMI (`SMI_REG + 0xa8`).
pub const FCH_SMI_CONTROL_PSP: u64 = 0xfed8_02a8;
/// The SMM flag byte inside the C2P SMM buffer page.
pub const SMM_FLAG_OFFSET: u64 = 0xc00;
/// P2C (PSP to CPU) buffer size AGESA allocates in SMRAM.
pub const P2C_BUFFER_SIZE: u64 = 0x2000;
/// Milan sets bit 31 of the P2C buffer's second header dword before SmmInfo
/// (P2CmboxV2Ssp, decompiler-only); Naples and Rome leave it 0.
pub const P2C_HEADER1_MILAN: u32 = 0x8000_0000;

/// SmmInfo (0x02): where SMRAM is, how the PSP raises its SMI and where the
/// SMM-side buffers are. Identical on all three generations (PSP_APOB §3.4,
/// AmdPspP2CmboxV2Ssp `0x2068`, decompiler-only); header size 0x110.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct SmmInfo {
    /// `SMM_ADDR` (C001_0112) & `0xff_fffe_0000`.
    pub smm_base: u64,
    /// `SMM_MASK` (C001_0113), raw.
    pub smm_mask: u64,
    /// P2C buffer in SMRAM.
    pub p2c_buffer: u64,
    pub p2c_size: u64,
    /// How the PSP raises its SMI: SmiTrig0 AND ~BIT25, OR BIT25.
    pub trigger: SmmRegister,
    /// SmiEnb: BIT31 must read 0.
    pub smi_enable: SmmRegister,
    /// EOS: BIT28 must read 1.
    pub eos: SmmRegister,
    /// PSP SMI control: bits 3:2 must read 1 (SMI).
    pub psp_smi_control: SmmRegister,
    pub reserved: [SmmRegister; 5],
    /// The SMRAM page used for C2P commands from SMM.
    pub c2p_smm_buffer: u64,
    /// The SMM flag byte (`c2p_smm_buffer + 0xc00`).
    pub smm_flag: u64,
}

impl SmmInfo {
    /// The AGESA values for the SP3 FCH. `smm_addr`/`smm_mask` are the raw
    /// MSR values; `p2c_buffer` is [`P2C_BUFFER_SIZE`] bytes and
    /// `c2p_smm_buffer` one 4 KiB page, both in SMRAM.
    ///
    /// Before sending, AGESA clears SmiStatus1 bit 1 (`0xfed80284 |= 2`),
    /// SmiTrig0 BIT25 and sets SmiControl[0xa8] bits 3:2 to 1 (FCH).
    pub const fn new(smm_addr: u64, smm_mask: u64, p2c_buffer: u64, c2p_smm_buffer: u64) -> Self {
        Self {
            smm_base: smm_addr & 0xff_fffe_0000,
            smm_mask,
            p2c_buffer,
            p2c_size: P2C_BUFFER_SIZE,
            trigger: SmmRegister::mem32(FCH_SMI_TRIGGER0, !(1 << 25), 1 << 25),
            smi_enable: SmmRegister::mem32(FCH_SMI_TRIGGER0, 1 << 31, 0),
            eos: SmmRegister::mem32(FCH_SMI_TRIGGER0, 1 << 28, 1 << 28),
            psp_smi_control: SmmRegister::mem32(FCH_SMI_CONTROL_PSP, 0xc, 0x4),
            reserved: [SmmRegister::mem32(0, 0, 0); 5],
            c2p_smm_buffer,
            smm_flag: c2p_smm_buffer + SMM_FLAG_OFFSET,
        }
    }
}

impl Command for SmmInfo {
    const ID: u8 = 0x02;
    const NAME: &'static str = "SmmInfo";

    fn supported(_: Generation) -> bool {
        true
    }
}

/// SxInfo (0x03): sent from the S3-entry SMI with the SMM flag set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct SxInfo {
    pub sleep_type: u8,
}

impl Command for SxInfo {
    const ID: u8 = 0x03;
    const NAME: &'static str = "SxInfo";

    fn supported(_: Generation) -> bool {
        true
    }
}

/// QueryCaps (0x05): capability dword returned at `+8`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout,
)]
#[repr(C)]
pub struct QueryCaps {
    pub capabilities: u32,
}

impl Command for QueryCaps {
    const ID: u8 = 0x05;
    const NAME: &'static str = "QueryCaps";

    fn supported(_: Generation) -> bool {
        true
    }
}

/// PspCapsQuery (0x27): Milan's capability query, same layout as
/// [`QueryCaps`] (PeiV2Ssp `ffcf5164`).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout,
)]
#[repr(C)]
pub struct PspCapsQuery {
    pub capabilities: u32,
}

impl Command for PspCapsQuery {
    const ID: u8 = 0x27;
    const NAME: &'static str = "PspCapsQuery";

    fn supported(generation: Generation) -> bool {
        generation == Generation::Milan
    }
}

/// BiosDisablePsb (0x4a): `'DBSP'` at buffer `+0xc` (AmdPspPsbDisablePei,
/// instruction-checked on Milan). Irreversible PSB state change: only on an
/// explicit policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct BiosDisablePsb {
    pub reserved: u32,
    pub magic: u32,
}

impl BiosDisablePsb {
    pub const MAGIC: u32 = u32::from_le_bytes(*b"DBSP");

    pub const fn new() -> Self {
        Self {
            reserved: 0,
            magic: Self::MAGIC,
        }
    }
}

impl Default for BiosDisablePsb {
    fn default() -> Self {
        Self::new()
    }
}

impl Command for BiosDisablePsb {
    const ID: u8 = 0x4a;
    const NAME: &'static str = "BiosDisablePsb";

    fn supported(generation: Generation) -> bool {
        matches!(generation, Generation::Rome | Generation::Milan)
    }
}

/// DisableDrtmCapability (0x62), data 1: Milan ReadyToBoot with the DRTM
/// virtual device off (PCD 0x4b FALSE, the default; PSP_APOB §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct DisableDrtm {
    pub value: u32,
}

impl DisableDrtm {
    pub const fn new() -> Self {
        Self { value: 1 }
    }
}

impl Default for DisableDrtm {
    fn default() -> Self {
        Self::new()
    }
}

impl Command for DisableDrtm {
    const ID: u8 = 0x62;
    const NAME: &'static str = "DisableDrtmCapability";

    fn supported(generation: Generation) -> bool {
        generation == Generation::Milan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::offset_of;

    #[test]
    fn header_sizes_match_agesa() {
        assert_eq!(C2pBuffer::new(BootDone).header.size, 8);
        assert_eq!(C2pBuffer::new(SxInfo { sleep_type: 3 }).header.size, 9);
        assert_eq!(C2pBuffer::new(QueryCaps::default()).header.size, 0xc);
        assert_eq!(C2pBuffer::new(DisableDrtm::new()).header.size, 0xc);
        assert_eq!(C2pBuffer::new(BiosDisablePsb::new()).header.size, 0x10);
        let smm = C2pBuffer::new(SmmInfo::new(0, 0, 0, 0));
        assert_eq!(smm.header.size, 0x110);
    }

    #[test]
    fn smm_info_layout() {
        // Request-relative offsets of P2CmboxV2Ssp `0x2068`.
        assert_eq!(offset_of!(SmmInfo, trigger), 0x20);
        assert_eq!(offset_of!(SmmInfo, smi_enable), 0x38);
        assert_eq!(offset_of!(SmmInfo, eos), 0x50);
        assert_eq!(offset_of!(SmmInfo, psp_smi_control), 0x68);
        assert_eq!(offset_of!(SmmInfo, c2p_smm_buffer), 0xf8);
        assert_eq!(offset_of!(SmmInfo, smm_flag), 0x100);
        let info = SmmInfo::new(0x7700_0000_0abc, 0xffff_fffe_0003, 0x7700_1000, 0x7700_3000);
        assert_eq!(info.smm_base, 0x7700_0000_0000 & 0xff_fffe_0000);
        assert_eq!(info.smm_mask, 0xffff_fffe_0003, "raw, as AGESA");
        assert_eq!(info.smm_flag, 0x7700_3c00);
        let b = info.as_bytes();
        assert_eq!(
            &b[0x28..0x38],
            &[1, 0, 0, 0, 2, 0, 0, 0, 0xff, 0xff, 0xff, 0xfd, 0, 0, 0, 2]
        );
        assert_eq!(&b[0x78..0x80], &[0xc, 0, 0, 0, 4, 0, 0, 0]);
    }

    #[test]
    fn psb_magic_at_0xc() {
        let buf = C2pBuffer::new(BiosDisablePsb::new());
        let bytes = buf.request.as_bytes();
        assert_eq!(&bytes[4..8], b"DBSP");
    }
}
