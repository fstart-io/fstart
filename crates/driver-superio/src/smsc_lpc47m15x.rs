//! SMSC LPC47M15x SuperIO driver descriptor.
//!
//! Intel D945GCLF exposes the LPC47M15x at LPC PnP config port `0x2e`.
//! Ported from coreboot `superio/smsc/lpc47m15x/early_serial.c`: config
//! mode is entered with a single `0x55` byte to the index port and exited
//! with a raw `0xAA` byte to the index port (no register/value pair,
//! hence [`crate::SuperIoChip::EXIT_RAW`]).
//!
//! Logical device numbers follow `lpc47m15x.h`: FDC=0, PP=3, SP1=4,
//! SP2=5, KBC=7, GAME=9, PME=10, MPU=11.

use crate::{SuperIo, SuperIoChip};

pub use crate::{
    CirConfig, ComPortConfig, EcConfig, GpioConfig, KbcConfig, LpcBaseProvider, MouseConfig,
    ParallelConfig, SuperIoConfig,
};

/// Zero-sized chip descriptor for the SMSC LPC47M15x.
pub struct SmscLpc47m15xChip;

impl SuperIoChip for SmscLpc47m15xChip {
    const ENTER_SEQ: &'static [u8] = &[0x55];
    const EXIT_REG: u8 = 0x00;
    const EXIT_VAL: u8 = 0x00;
    const EXIT_RAW: Option<u8> = Some(0xaa);
    // coreboot never validates the chip ID on this part; the ID registers
    // are not reliable across LPC47M15x/192/997 revisions, so skip the check.
    const CHIP_ID: u16 = 0;
    const COM1_LDN: Option<u8> = Some(0x04);
    const COM2_LDN: Option<u8> = Some(0x05);
    const KBC_LDN: Option<u8> = Some(0x07);
    const MOUSE_LDN: Option<u8> = None;
    const EC_LDN: Option<u8> = None;
    const GPIO_LDN: Option<u8> = None;
    const CIR_LDN: Option<u8> = None;
    const PARALLEL_LDN: Option<u8> = Some(0x03);
}

/// LPC47M15x SuperIO driver.
pub type SmscLpc47m15x = SuperIo<SmscLpc47m15xChip>;

impl SmscLpc47m15x {
    /// Enable the PME logical device at its board-selected runtime I/O base.
    /// The platform must already decode the PnP ports and runtime window.
    /// Call before `init()` when PME must precede serial/KBC bring-up.
    pub fn enable_pme(
        &mut self,
        io_base: u16,
    ) -> Result<(), fstart_core::services::device::DeviceError> {
        let registers = pme_registers(io_base).ok_or(
            fstart_core::services::device::DeviceError::MissingResource("PME I/O base"),
        )?;
        self.enter_config();
        for (reg, value) in registers {
            self.write_reg(reg, value);
        }
        self.exit_config();
        // SAFETY: decoded config data port of this exclusively owned device.
        let _ = unsafe { fstart_core::pio::inb(self.data_port()) };
        Ok(())
    }
}

fn pme_registers(io_base: u16) -> Option<[(u8, u8); 5]> {
    (io_base != 0).then_some([
        (0x07, 10), // PME LDN
        (0x30, 0),  // Disable before moving the runtime window
        (0x60, (io_base >> 8) as u8),
        (0x61, io_base as u8),
        (0x30, 1), // Activate at the new base
    ])
}

#[cfg(test)]
mod tests {
    use super::pme_registers;

    #[test]
    fn pme_disables_before_relocating_and_enabling() {
        assert_eq!(
            pme_registers(0x680),
            Some([(7, 10), (0x30, 0), (0x60, 6), (0x61, 0x80), (0x30, 1)])
        );
        assert_eq!(pme_registers(0), None);
    }
}

/// Board-facing LPC47M15x config.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmscLpc47m15xConfig(pub SuperIoConfig);

impl core::ops::Deref for SmscLpc47m15xConfig {
    type Target = SuperIoConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for SmscLpc47m15xConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<SuperIoConfig> for SmscLpc47m15xConfig {
    fn from(config: SuperIoConfig) -> Self {
        Self(config)
    }
}
