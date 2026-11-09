//! Register subset shared by the ICH7/ICH8/ICH9 LPC flash interfaces.

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

/// One LPC generic I/O decode window (GENx_DEC), shared by ICH9 and the
/// 5 Series PCH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpcGenericIoDecode {
    /// I/O base address. Must be 4-byte aligned.
    pub base: u16,
    /// Window size in bytes. Must be a power of two from 4 through 256.
    pub size: u16,
}

impl LpcGenericIoDecode {
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
    fn firmware_write_policy_preserves_read_configuration_and_neighboring_bytes() {
        let mut bytes = [0xaaaa_aaaau32; core::mem::size_of::<LpcFlashConfig>() / 4];
        let config = unsafe { &*(bytes.as_mut_ptr() as *const LpcFlashConfig) };
        config.bios_control.set(0xfb);
        config.prepare_firmware_writes();
        assert_eq!(bytes[0xdc / 4], 0xaaaa_aad9);
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
