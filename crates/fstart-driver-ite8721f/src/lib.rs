//! ITE IT8721F SuperIO driver.
//!
//! The IT8721F is a commonly-used SuperIO on Atom-class boards
//! (Pineview/Cedarview + ICH7/NM10): COM1 + COM2 + PS/2 KBC + mouse +
//! parallel port + environment controller + GPIO + consumer IR.
//!
//! This crate is a thin chip descriptor that plugs into the generic
//! [`fstart_superio::SuperIo`] driver. All logic lives in that crate.

#![no_std]

extern crate alloc;

use fstart_superio::{SuperIo, SuperIoChip};

// Re-export all SuperIO config types so selected stage code can refer
// to them via `use fstart_driver_ite8721f::*;` without a second glob
// import from `fstart_superio`.
pub use fstart_superio::{
    CirConfig, ComPortConfig, EcConfig, GpioConfig, KbcConfig, LpcBaseProvider, MouseConfig,
    ParallelConfig, SuperIoConfig,
};

/// Zero-sized chip descriptor for the IT8721F.
pub struct Ite8721fChip;

impl SuperIoChip for Ite8721fChip {
    // First 3 bytes are constant; the 4th depends on port (0x55 for
    // 0x2E, 0xAA for 0x4E) — handled by enter_last_byte().
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
        match base_port {
            0x2E => Some(0x55),
            0x4E => Some(0xAA),
            _ => None,
        }
    }
}

/// IT8721F SuperIO driver — alias for `SuperIo<Ite8721fChip>`.
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

impl fstart_board_meta::BoardDriver for Ite8721fConfig {
    fn feature(&self) -> &'static str {
        "ite8721f"
    }

    fn services(&self) -> fstart_board_meta::ServiceSet {
        use fstart_board_meta::ServiceKind;
        let mut services = fstart_board_meta::ServiceSet::from_static(&[ServiceKind::SuperIoHost]);
        if self.console_port.is_some() {
            services.insert(ServiceKind::Console);
        }
        services
    }

    fn clone_box(&self) -> alloc::boxed::Box<dyn fstart_board_meta::BoardDriver> {
        alloc::boxed::Box::new(self.clone())
    }
}
