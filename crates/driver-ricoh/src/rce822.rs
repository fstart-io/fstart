//! Ricoh RCE822/RCE823 PCIe card reader (coreboot `drivers/ricoh/rce822`):
//! function disable, SD write-protect polarity and the subsystem ID, all
//! behind their own unlock keys.

use fstart_core::services::ServiceError;
use fstart_pci::{EcamDevice, PCI_DEVICE_ID, PCI_VENDOR_ID};

const RICOH_VENDOR: u16 = 0x1180;
const DEVICES: [u16; 2] = [0xe822, 0xe823];
/// Function-disable key and mask.
const FUNCTION_KEY: u16 = 0xca;
const FUNCTION_DISABLE: u16 = 0xcb;
const FUNCTION_UNLOCK: u8 = 0x57;
/// SD write-protect key and control.
const SD_KEY: u16 = 0xf9;
const SD_CONTROL: u16 = 0xfb;
const SD_UNLOCK: u8 = 0xfc;
const KEY_LOCK: u8 = 0;
/// Writable subsystem ID mirror.
const SUBSYSTEM: u16 = 0xac;

/// A verified RCE822 function 0.
#[derive(Clone, Copy)]
pub struct Rce822 {
    dev: EcamDevice,
}

impl Rce822 {
    /// Bind `dev` after checking its identity.
    pub fn probe(dev: EcamDevice) -> Result<Self, ServiceError> {
        let vendor = dev
            .try_read16(PCI_VENDOR_ID)
            .ok_or(ServiceError::HardwareError)?;
        let device = dev
            .try_read16(PCI_DEVICE_ID)
            .ok_or(ServiceError::HardwareError)?;
        if vendor != RICOH_VENDOR || !DEVICES.contains(&device) {
            return Err(ServiceError::HardwareError);
        }
        Ok(Self { dev })
    }

    /// coreboot `rce822_enable`: disable the functions in `mask`.
    pub fn disable_functions(&self, mask: u8) {
        self.dev.write8(FUNCTION_KEY, FUNCTION_UNLOCK);
        self.dev.write8(FUNCTION_DISABLE, mask);
        self.dev.write8(FUNCTION_KEY, KEY_LOCK);
    }

    /// coreboot `rce822_init`: SD write-protect polarity.
    pub fn set_sd_write_protect_polarity(&self, inverted: bool) {
        self.dev.write8(SD_KEY, SD_UNLOCK);
        self.dev.write8(SD_CONTROL, u8::from(inverted) << 1);
        self.dev.write8(SD_KEY, KEY_LOCK);
    }

    /// coreboot `rce822_set_subsystem`: vendor in the low half.
    pub fn set_subsystem(&self, vendor: u16, device: u16) {
        self.dev
            .write32(SUBSYSTEM, u32::from(device) << 16 | u32::from(vendor));
    }
}
