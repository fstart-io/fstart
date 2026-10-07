//! Register subset shared by the ICH7/ICH8 LPC flash interfaces.

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
}

pub fn device() -> EcamDevice {
    EcamDevice::new(0, LPC_DEV, LPC_FUNC)
}

/// # Safety
/// The caller must have mapped and enabled ICH7/ICH8 ECAM.
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

#[cfg(test)]
mod tests {
    use super::*;
    use tock_registers::interfaces::Writeable;

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
