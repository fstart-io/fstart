//! National Semiconductor PC87392 SuperIO driver descriptor.
//!
//! The ThinkPad X61 dock exposes a PC87392 at LPC PnP config port `0x2e`.
//! This descriptor maps the logical devices used by coreboot's X61
//! devicetree, including the dock WDT LDN so board code can explicitly keep it
//! disabled.

#![no_std]

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

/// Board-facing config alias.
pub type Pc87392Config = SuperIoConfig;

/// PC87392 watchdog logical-device number from coreboot's `pc87392.h`.
pub const PC87392_WDT_LDN: u8 = 0x0a;
