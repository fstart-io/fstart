//! ITE IT8721F SuperIO driver descriptor.
//!
//! Foxconn D41S exposes the IT8721F at LPC PnP config port `0x2e`.

use crate::{SuperIo, SuperIoChip};

pub use crate::{
    CirConfig, ComPortConfig, EcConfig, GpioConfig, KbcConfig, LpcBaseProvider, MouseConfig,
    ParallelConfig, SuperIoConfig,
};

/// Zero-sized chip descriptor for the ITE IT8721F.
pub struct Ite8721fChip;

impl SuperIoChip for Ite8721fChip {
    const ENTER_SEQ: &'static [u8] = &[0x87, 0x01, 0x55];
    const EXIT_REG: u8 = 0x02;
    const EXIT_VAL: u8 = 0x02;
    const CHIP_ID: u16 = 0x8721;
    const COM1_LDN: Option<u8> = Some(0x01);
    const COM2_LDN: Option<u8> = Some(0x02);
    const KBC_LDN: Option<u8> = Some(0x05);
    const MOUSE_LDN: Option<u8> = Some(0x06);
    const EC_LDN: Option<u8> = Some(0x04);
    const GPIO_LDN: Option<u8> = Some(0x07);
    const CIR_LDN: Option<u8> = Some(0x0a);
    const PARALLEL_LDN: Option<u8> = Some(0x03);

    fn enter_last_byte(base_port: u16) -> Option<u8> {
        Some(if base_port == 0x4e { 0xaa } else { 0x55 })
    }
}

/// IT8721F SuperIO driver.
pub type Ite8721f = SuperIo<Ite8721fChip>;

/// Board-facing IT8721F config.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ite8721fConfig(pub SuperIoConfig);

impl core::ops::Deref for Ite8721fConfig {
    type Target = SuperIoConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for Ite8721fConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<SuperIoConfig> for Ite8721fConfig {
    fn from(config: SuperIoConfig) -> Self {
        Self(config)
    }
}
