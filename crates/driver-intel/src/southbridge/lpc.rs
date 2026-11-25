//! Shared ICH/PCH LPC decode policy and flash register interface.

use fstart_core::mmio::MmioReadWrite;
use fstart_pci::{ecam::EcamDevice, pci_type0_config};
use tock_registers::{
    interfaces::{ReadWriteable, Readable},
    register_bitfields,
};

pub const LPC_DEV: u8 = 0x1f;
pub const LPC_FUNC: u8 = 0;

register_bitfields![u8,
    pub BIOS_CONTROL [
        WRITE_ENABLE OFFSET(0) NUMBITS(1) [],
        LOCK_ENABLE OFFSET(1) NUMBITS(1) [],
        READ_CONFIGURATION OFFSET(2) NUMBITS(2) [
            CacheNoPrefetch = 0,
            NoPrefetchNoCache = 1,
            PrefetchAndCache = 2
        ],
        // Reserved on earlier revisions; enforce protection when implemented.
        SMM_WRITE_PROTECT OFFSET(5) NUMBITS(1) []
    ]
];
register_bitfields![u32,
    pub ROOT_COMPLEX_BASE [
        ENABLE OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(14) NUMBITS(18) []
    ]
];

pci_type0_config! {
    pub struct LpcFlashConfig {
        (0x40 => _reserved_flash0),
        (0xdc => pub bios_control: MmioReadWrite<u8, BIOS_CONTROL::Register>),
        (0xdd => _reserved_flash1),
        (0xf0 => pub rcba: MmioReadWrite<u32, ROOT_COMPLEX_BASE::Register>),
        (0xf4 => @END),
    }
}

impl LpcFlashConfig {
    pub fn enable_prefetching_and_caching(&self) {
        self.bios_control
            .modify(BIOS_CONTROL::READ_CONFIGURATION::PrefetchAndCache);
    }

    /// Never open BIOS writes at handoff. With no SMM-only policy, leave any
    /// stronger existing hardware protection in place.
    pub fn finalize_bios_protection(&self, smm_only: bool) -> bool {
        let reg = &self.bios_control;
        if smm_only {
            reg.modify(
                BIOS_CONTROL::WRITE_ENABLE::CLEAR
                    + BIOS_CONTROL::LOCK_ENABLE::SET
                    + BIOS_CONTROL::SMM_WRITE_PROTECT::SET,
            );
        } else {
            reg.modify(BIOS_CONTROL::WRITE_ENABLE::CLEAR);
        }
        let actual = reg.extract();
        !actual.is_set(BIOS_CONTROL::WRITE_ENABLE)
            && (!smm_only
                || actual.is_set(BIOS_CONTROL::LOCK_ENABLE)
                    && actual.is_set(BIOS_CONTROL::SMM_WRITE_PROTECT))
    }

    /// Platform-owned development write policy, matching coreboot's
    /// `spi_init()` / `spi_set_smm_only_flashing(false)`. Ibex Peak deliberately
    /// keeps this policy through payload handoff; its SPI opcode lockdown
    /// does not restore BLE/SMM_BWP. Production relocking needs an authorized
    /// SMM write service for subsequent flash logging/payload writes.
    /// Regional protection and each writer's bounded extent remain enforced.
    pub fn prepare_firmware_writes(&self) {
        self.bios_control.modify(
            BIOS_CONTROL::LOCK_ENABLE::CLEAR
                + BIOS_CONTROL::SMM_WRITE_PROTECT::CLEAR
                + BIOS_CONTROL::WRITE_ENABLE::SET,
        );
    }
}

pub fn device() -> EcamDevice {
    EcamDevice::new(0, LPC_DEV, LPC_FUNC)
}

/// # Safety
/// The caller must have mapped and enabled ICH7/ICH8/ICH9 ECAM.
pub unsafe fn flash_config() -> &'static LpcFlashConfig {
    unsafe { device().regs() }
}

/// Device/revision bytes used by platform cache policy, not mutable boot state.
pub fn training_identity(header: &fstart_pci::overlay::PciType0Config) -> [u8; 5] {
    let vendor = header.vendor_id.get().to_le_bytes();
    let device = header.device_id.get().to_le_bytes();
    [
        vendor[0],
        vendor[1],
        device[0],
        device[1],
        header.revision_id.get(),
    ]
}

/// One LPC generic I/O decode window (GENx_DEC), shared by ICH7/8/9 and
/// the 5 Series PCH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpcGenericIoDecode {
    /// I/O base address. Must be 4-byte aligned.
    pub base: u16,
    /// Window size in bytes. Must be a power of two from 4 through 256.
    pub size: u16,
}

impl LpcGenericIoDecode {
    /// Validate all active windows before any decoder is programmed.
    /// Const evaluation rejects malformed and overlapping board policy.
    pub const fn validate_all<const N: usize>(decodes: &fstart_core::ConstVec<Self, N>) {
        let mut index = 0;
        while index < decodes.len() {
            let decode = decodes.get(index);
            assert!(
                decode.encode().is_some(),
                "LPC generic I/O decode is invalid"
            );
            let mut next = index + 1;
            while next < decodes.len() {
                assert!(
                    !decode.overlaps(decodes.get(next)),
                    "LPC generic I/O decodes overlap"
                );
                next += 1;
            }
            index += 1;
        }
    }

    /// The GENx_DEC value, or `None` for a window the decoder cannot express.
    #[must_use]
    pub const fn encode(self) -> Option<u32> {
        encode_generic_io_window(self.base, self.size)
    }

    /// Whether two windows share an I/O port.
    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        let end = self.base as u32 + self.size as u32;
        let other_end = other.base as u32 + other.size as u32;
        (self.base as u32) < other_end && (other.base as u32) < end
    }
}

/// Check an ICH7/8/9 GENx_DEC window before programming it.
#[must_use]
pub const fn valid_generic_io_window(base: u16, size: u16) -> bool {
    size >= 4 && size <= 0x100 && size.is_power_of_two() && base & (size - 1) == 0
}

/// Encode a naturally aligned power-of-two LPC I/O window.
#[must_use]
pub const fn encode_generic_io_window(base: u16, size: u16) -> Option<u32> {
    if !valid_generic_io_window(base, size) {
        return None;
    }
    Some(((size as u32 - 4) << 16) | (base & 0xfffc) as u32 | 1)
}

register_bitfields![u32,
    /// I/O trap register low dword (RCBA 0x1e80 + 8*n, 32-bit, n = 0..3).
    pub IOTR_LO [
        TRAP_SMI_ENABLE OFFSET(0) NUMBITS(1) [],
        IO_ADDRESS_DWORD OFFSET(2) NUMBITS(14) [],
        ADDRESS_MASK OFFSET(18) NUMBITS(6) []
    ],
    /// I/O trap register high dword (RCBA 0x1e84 + 8*n, 32-bit, n = 0..3).
    pub IOTR_HI [
        BYTE_ENABLES OFFSET(0) NUMBITS(4) [],
        BYTE_ENABLE_MASK OFFSET(4) NUMBITS(4) [],
        READ_WRITE OFFSET(16) NUMBITS(1) [],
        READ_WRITE_MASK OFFSET(17) NUMBITS(1) []
    ]
];

/// I/O access types matched by an ICH8/ICH9 trap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoTrapAccess {
    /// Trap read cycles only.
    Read,
    /// Trap write cycles only.
    Write,
    /// Trap both read and write cycles.
    Any,
}

/// Semantic ICH8/ICH9 I/O trap configuration.
#[derive(Debug, Clone, Copy)]
pub struct IoTrapConfig {
    /// I/O trap register index, 0..3.
    pub index: u8,
    /// Base I/O port. Must be dword-aligned and aligned to `size`.
    pub base: u16,
    /// Trapped range size in bytes. Must be a power-of-two dword multiple, up to 256.
    pub size: u16,
    /// Cycle type to trap.
    pub access: IoTrapAccess,
}

impl IoTrapConfig {
    #[must_use]
    pub(crate) fn encode(self) -> Option<(u32, u32)> {
        if self.index > 3 || self.base & 0x3 != 0 || self.size == 0 || self.size > 0x100 {
            return None;
        }
        if self.size & 0x3 != 0 || !self.size.is_power_of_two() || self.base & (self.size - 1) != 0
        {
            return None;
        }

        let address_mask = u32::from((self.size / 4) - 1);
        let io_address = u32::from(self.base) >> 2;
        let low = IOTR_LO::TRAP_SMI_ENABLE::SET.value
            | IOTR_LO::IO_ADDRESS_DWORD.val(io_address).value
            | IOTR_LO::ADDRESS_MASK.val(address_mask).value;
        let high = match self.access {
            IoTrapAccess::Write => IOTR_HI::READ_WRITE::CLEAR.value,
            IoTrapAccess::Read => IOTR_HI::READ_WRITE::SET.value,
            IoTrapAccess::Any => {
                IOTR_HI::READ_WRITE::CLEAR.value | IOTR_HI::READ_WRITE_MASK::SET.value
            }
        } | IOTR_HI::BYTE_ENABLE_MASK.val(0xf).value;

        Some((low, high))
    }
}

/// Legacy serial-port decode selector in the LPC I/O decode register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LpcSerialDecode {
    /// COM1 at 0x3f8.
    Com1,
    /// COM2 at 0x2f8.
    Com2,
    /// Serial decode at 0x220.
    Io220,
    /// Serial decode at 0x228.
    Io228,
    /// Serial decode at 0x238.
    Io238,
    /// Serial decode at 0x2e8.
    Io2e8,
    /// Serial decode at 0x338.
    Io338,
    /// Serial decode at 0x3e8.
    Io3e8,
}

impl LpcSerialDecode {
    const fn bits(self) -> u16 {
        match self {
            Self::Com1 => 0,
            Self::Com2 => 1,
            Self::Io220 => 2,
            Self::Io228 => 3,
            Self::Io238 => 4,
            Self::Io2e8 => 5,
            Self::Io338 => 6,
            Self::Io3e8 => 7,
        }
    }
}

/// Parallel-port decode selector in the LPC I/O decode register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LpcParallelDecode {
    /// LPT at 0x378.
    Lpt378,
    /// LPT at 0x278.
    Lpt278,
    /// LPT at 0x3bc.
    Lpt3bc,
}

impl LpcParallelDecode {
    const fn bits(self) -> u16 {
        match self {
            Self::Lpt378 => 0,
            Self::Lpt278 => 1,
            Self::Lpt3bc => 2,
        }
    }
}

/// Floppy-controller decode selector in the LPC I/O decode register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LpcFloppyDecode {
    /// FDC at 0x3f0.
    Fdd3f0,
    /// FDC at 0x370.
    Fdd370,
}

impl LpcFloppyDecode {
    const fn bits(self) -> u16 {
        match self {
            Self::Fdd3f0 => 0,
            Self::Fdd370 => 1,
        }
    }
}

/// Fixed legacy I/O decode selections for COM/LPT/FDC ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpcFixedIoDecode {
    /// COMA selector. COMA is enabled through `LPC_EN_ALL`.
    pub com_a: LpcSerialDecode,
    /// COMB selector. COMB is enabled through `LPC_EN_ALL`.
    pub com_b: LpcSerialDecode,
    /// Optional LPT selector.
    pub lpt: Option<LpcParallelDecode>,
    /// Optional FDC selector.
    pub fdd: Option<LpcFloppyDecode>,
}

impl Default for LpcFixedIoDecode {
    fn default() -> Self {
        Self {
            com_a: LpcSerialDecode::Com1,
            com_b: LpcSerialDecode::Com2,
            lpt: None,
            fdd: None,
        }
    }
}

impl LpcFixedIoDecode {
    #[must_use]
    pub(crate) const fn encode(self) -> u16 {
        let mut value = self.com_a.bits() | (self.com_b.bits() << 4);
        if let Some(lpt) = self.lpt {
            value |= lpt.bits() << 8;
        }
        if let Some(fdd) = self.fdd {
            value |= fdd.bits() << 12;
        }
        value
    }
}

/// Board-level LPC decode policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpcDecodeConfig {
    /// Fixed COM/LPT/FDC decode selector register.
    pub fixed_io: LpcFixedIoDecode,
    /// Up to four generic I/O decode windows, programmed to GEN1..GEN4.
    pub generic_io: fstart_core::ConstVec<LpcGenericIoDecode, 4>,
}

impl LpcDecodeConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fixed_io: LpcFixedIoDecode {
                com_a: LpcSerialDecode::Com1,
                com_b: LpcSerialDecode::Com2,
                lpt: None,
                fdd: None,
            },
            generic_io: fstart_core::ConstVec::new(LpcGenericIoDecode { base: 0, size: 0 }),
        }
    }
}

impl Default for LpcDecodeConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tock_registers::interfaces::Writeable;

    #[test]
    fn generic_decode_requires_a_naturally_aligned_power_of_two() {
        assert_eq!(encode_generic_io_window(0x1600, 0x80), Some(0x007c_1601));
        assert_eq!(encode_generic_io_window(0x15e0, 0x10), Some(0x000c_15e1));
        assert_eq!(encode_generic_io_window(0x1600, 12), None);
        assert_eq!(encode_generic_io_window(0x1610, 0x20), None);
    }

    #[test]
    fn decode_validation_accepts_empty_and_adjacent_windows() {
        use fstart_core::ConstVec;
        let first = LpcGenericIoDecode {
            base: 0x1600,
            size: 0x20,
        };
        let adjacent = LpcGenericIoDecode {
            base: 0x1620,
            size: 0x20,
        };
        LpcGenericIoDecode::validate_all(&ConstVec::<_, 4>::new(first));
        let valid = ConstVec::<_, 4>::new(first).push(first).push(adjacent);
        LpcGenericIoDecode::validate_all(&valid);
    }

    #[test]
    #[should_panic(expected = "LPC generic I/O decodes overlap")]
    fn decode_validation_rejects_a_window_contained_in_another() {
        let first = LpcGenericIoDecode {
            base: 0x1600,
            size: 0x20,
        };
        let contained = LpcGenericIoDecode {
            base: 0x1610,
            size: 0x10,
        };
        let windows = fstart_core::ConstVec::<_, 4>::new(first)
            .push(first)
            .push(contained);
        LpcGenericIoDecode::validate_all(&windows);
    }

    #[test]
    #[should_panic(expected = "LPC generic I/O decode is invalid")]
    fn decode_validation_rejects_an_unaligned_window() {
        let window = LpcGenericIoDecode {
            base: 0x1604,
            size: 0x20,
        };
        let windows = fstart_core::ConstVec::<_, 4>::new(window).push(window);
        LpcGenericIoDecode::validate_all(&windows);
    }

    #[test]
    fn firmware_write_policy_preserves_read_configuration_and_neighboring_bytes() {
        let mut bytes = [0xaaaa_aaaau32; core::mem::size_of::<LpcFlashConfig>() / 4];
        let config = unsafe { &*(bytes.as_mut_ptr() as *const LpcFlashConfig) };
        config.bios_control.set(0xfb);
        config.prepare_firmware_writes();
        assert_eq!(bytes[0xdc / 4], 0xaaaa_aad9);
        assert!(config.finalize_bios_protection(false));
        assert_eq!(bytes[0xdc / 4], 0xaaaa_aad8);
        assert!(config.finalize_bios_protection(true));
        assert_eq!(bytes[0xdc / 4], 0xaaaa_aafa);
        assert!(config.finalize_bios_protection(false));
        assert_eq!(bytes[0xdc / 4], 0xaaaa_aafa);
        assert_eq!(bytes[0xe0 / 4], 0xaaaa_aaaa);
    }

    #[test]
    fn flash_read_policy_preserves_protection_and_neighboring_bytes() {
        let mut bytes = [0xaaaa_aaaau32; core::mem::size_of::<LpcFlashConfig>() / 4];
        let config = unsafe { &*(bytes.as_mut_ptr() as *const LpcFlashConfig) };
        config.bios_control.set(0xf7);
        config.enable_prefetching_and_caching();
        assert_eq!(bytes[0xdc / 4], 0xaaaa_aafb);
        assert_eq!(bytes[0xe0 / 4], 0xaaaa_aaaa);
    }
}
