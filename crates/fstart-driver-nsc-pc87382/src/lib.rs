//! National Semiconductor PC87382 SuperIO driver descriptor.
//!
//! The ThinkPad X61 uses this laptop-side SuperIO/DLPC block at LPC PnP
//! config port `0x164e`.  The generic [`fstart_superio::SuperIo`] driver
//! performs the common PnP resource programming; board-specific DLPC GPIO and
//! dock-switch sequencing remains in `fstart-mainboard-lenovo-x61`.

#![no_std]

extern crate alloc;

use fstart_superio::{SuperIo, SuperIoChip};

pub use fstart_superio::{
    CirConfig, ComPortConfig, EcConfig, GpioConfig, KbcConfig, LpcBaseProvider, MouseConfig,
    ParallelConfig, SuperIoConfig,
};

/// Zero-sized chip descriptor for the NSC PC87382.
pub struct Pc87382Chip;

impl SuperIoChip for Pc87382Chip {
    const ENTER_SEQ: &'static [u8] = &[];
    const EXIT_REG: u8 = 0;
    const EXIT_VAL: u8 = 0;
    // The X61 DLPC path is often accessed through already-decoded extended
    // PnP ports.  Skip the generic ID check; coreboot also treats this as a
    // fixed mainboard resource rather than probing by ID here.
    const CHIP_ID: u16 = 0;
    const COM1_LDN: Option<u8> = None;
    const COM2_LDN: Option<u8> = Some(0x02);
    const KBC_LDN: Option<u8> = None;
    const MOUSE_LDN: Option<u8> = None;
    const EC_LDN: Option<u8> = None;
    const GPIO_LDN: Option<u8> = Some(0x07);
    const CIR_LDN: Option<u8> = Some(0x03);
    const PARALLEL_LDN: Option<u8> = None;
}

/// PC87382 SuperIO driver.
pub type Pc87382 = SuperIo<Pc87382Chip>;

/// Board-facing PC87382 config.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pc87382Config(pub SuperIoConfig);

impl core::ops::Deref for Pc87382Config {
    type Target = SuperIoConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for Pc87382Config {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<SuperIoConfig> for Pc87382Config {
    fn from(config: SuperIoConfig) -> Self {
        Self(config)
    }
}

/// PC87382 DLPC logical-device number used by the X61 mainboard DLPC block.
pub const PC87382_DLPC_LDN: u8 = 0x19;
