//! National Semiconductor PC87392 SuperIO driver descriptor.
//!
//! The ThinkPad X61 dock exposes a PC87392 at LPC PnP config port `0x2e`.
//! This descriptor maps the logical devices used by coreboot's X61
//! devicetree, including the dock WDT LDN so board code can explicitly keep it
//! disabled.

#![no_std]

extern crate alloc;

use fstart_superio::{SuperIo, SuperIoChip};

pub use fstart_superio::{
    CirConfig, ComPortConfig, EcConfig, GpioConfig, KbcConfig, LpcBaseProvider, MouseConfig,
    ParallelConfig, SuperIoConfig,
};

/// Zero-sized chip descriptor for the NSC PC87392.
pub struct Pc87392Chip;

impl SuperIoChip for Pc87392Chip {
    const ENTER_SEQ: &'static [u8] = &[];
    const EXIT_REG: u8 = 0;
    const EXIT_VAL: u8 = 0;
    // The dock-side chip is behind a board-controlled LPC switch and coreboot
    // configures it as a fixed PnP resource.  Skip the generic ID check.
    const CHIP_ID: u16 = 0;
    const COM1_LDN: Option<u8> = Some(0x03);
    const COM2_LDN: Option<u8> = Some(0x02);
    const KBC_LDN: Option<u8> = None;
    const MOUSE_LDN: Option<u8> = None;
    const EC_LDN: Option<u8> = None;
    const GPIO_LDN: Option<u8> = Some(0x07);
    const CIR_LDN: Option<u8> = None;
    const PARALLEL_LDN: Option<u8> = Some(0x01);
}

/// Dock-side PC87392 SuperIO driver.
pub type Pc87392 = SuperIo<Pc87392Chip>;

/// Board-facing PC87392 config.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pc87392Config(pub SuperIoConfig);

impl core::ops::Deref for Pc87392Config {
    type Target = SuperIoConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for Pc87392Config {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<SuperIoConfig> for Pc87392Config {
    fn from(config: SuperIoConfig) -> Self {
        Self(config)
    }
}

/// PC87392 floppy-controller logical-device number from coreboot's X61 dock devicetree.
pub const PC87392_FDC_LDN: u8 = 0x00;

/// PC87392 watchdog logical-device number from coreboot's `pc87392.h`.
pub const PC87392_WDT_LDN: u8 = 0x0a;

impl fstart_board_meta::BoardDriver for Pc87392Config {
    fn feature(&self) -> &'static str {
        "nsc-pc87392"
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
