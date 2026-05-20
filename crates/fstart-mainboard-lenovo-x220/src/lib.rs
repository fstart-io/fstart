//! Lenovo ThinkPad X220 mainboard glue.
//!
//! Reusable Sandy Bridge host-bridge and bd82x6x/Cougar Point PCH logic belongs
//! in the chipset drivers.  This crate keeps the X220-specific policy from
//! coreboot `mainboard/lenovo/x220`: Lenovo AT24RF08C RFID EEPROM locking,
//! PMH7/H8 EC defaults, dock/backlight/ThinkLight notes, and the SMM/ACPI EC
//! routing contract.  The first port records the values and sequencing hooks;
//! full EC/PMH7 drivers are still TODO.

#![no_std]

pub mod smm;

use fstart_services::device::{Device, DeviceError};
use fstart_services::{FinalizeInit, Mainboard, PostDramInit, PreConsoleInit, ServiceError};
use serde::{Deserialize, Serialize};

/// Lenovo H8 EC event-enable bytes used by X220.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct H8EventMaskConfig {
    /// EC event enable bytes for events 0x0..0xf.
    #[serde(default = "default_h8_event_enable")]
    pub event_enable: [u8; 16],
}

impl Default for H8EventMaskConfig {
    fn default() -> Self {
        Self {
            event_enable: default_h8_event_enable(),
        }
    }
}

/// Lenovo ThinkPad X220 mainboard configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LenovoX220MainboardConfig {
    /// Lock the Lenovo AT24RF08C RFID/serial EEPROM in the early board hook.
    /// Coreboot writes 0x0f to offsets 0..7 at SMBus address 0x5c, retrying
    /// because the part may stop responding after register writes.
    #[serde(default = "default_true")]
    pub eeprom_early_lock: bool,
    /// Enable PMH7 backlight control (PMH7 reg 0x50 bit 5 in coreboot).
    #[serde(default = "default_true")]
    pub backlight_enable: bool,
    /// Enable PMH7 dock events (PMH7 reg 0x60 bit 3 in coreboot).
    #[serde(default = "default_true")]
    pub dock_event_enable: bool,
    /// X220 has a ThinkLight through the Lenovo H8 EC override tree.
    #[serde(default = "default_true")]
    pub has_thinklight: bool,
    /// WWAN detect GPIO from coreboot: GPIO70 active-low.
    #[serde(default = "default_wwan_gpio")]
    pub wwan_gpio_num: u8,
    /// Active level for WWAN detect GPIO.
    #[serde(default)]
    pub wwan_gpio_lvl: bool,
    /// H8 EC event masks from coreboot devicetree/overridetree.
    #[serde(default)]
    pub h8_events: H8EventMaskConfig,
    /// ACPI contributor name marker for codegen.
    #[serde(default)]
    pub acpi_name: Option<heapless::String<8>>,
}

impl Default for LenovoX220MainboardConfig {
    fn default() -> Self {
        Self {
            eeprom_early_lock: true,
            backlight_enable: true,
            dock_event_enable: true,
            has_thinklight: true,
            wwan_gpio_num: default_wwan_gpio(),
            wwan_gpio_lvl: false,
            h8_events: H8EventMaskConfig::default(),
            acpi_name: None,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_wwan_gpio() -> u8 {
    70
}
fn default_h8_event_enable() -> [u8; 16] {
    // X220 coreboot: event2 ff, event3 ff, event4 d0, event5 fc, event6 00,
    // event7 81, event8 7b, event9 ff, eventa 01, eventb f0, eventc ff,
    // eventd ff, evente 0d. Unspecified event0/1/f default to 0.
    [
        0x00, 0x00, 0xff, 0xff, 0xd0, 0xfc, 0x00, 0x81, 0x7b, 0xff, 0x01, 0xf0, 0xff, 0xff, 0x0d,
        0x00,
    ]
}

/// Lenovo ThinkPad X220 mainboard hook driver.
pub struct LenovoX220Mainboard {
    config: &'static LenovoX220MainboardConfig,
}

impl Device for LenovoX220Mainboard {
    const NAME: &'static str = "lenovo-x220-mainboard";
    const COMPATIBLE: &'static [&'static str] = &["lenovo,thinkpad-x220"];
    type Config = LenovoX220MainboardConfig;

    fn new(config: &'static Self::Config) -> Result<Self, DeviceError> {
        Ok(Self { config })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }
}

impl PreConsoleInit for LenovoX220Mainboard {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl PostDramInit for LenovoX220Mainboard {
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl FinalizeInit for LenovoX220Mainboard {
    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl Mainboard for LenovoX220Mainboard {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        PreConsoleInit::pre_console_init(self)
    }

    fn pre_console_init_with_southbridge(
        &mut self,
        _southbridge: &mut dyn fstart_services::Southbridge,
    ) -> Result<(), ServiceError> {
        if self.config.eeprom_early_lock {
            // The AT24RF08C EEPROM/RFID lock runs from the bd82x6x early init
            // after SMBus is enabled and before DRAM init. Keep this hook
            // log-free: it intentionally runs before ConsoleInit.
        }
        Ok(())
    }

    fn ramstage_init(&mut self) -> Result<(), ServiceError> {
        PostDramInit::post_dram_init(self)
    }

    fn ramstage_init_with_southbridge(
        &mut self,
        southbridge: &mut dyn fstart_services::Southbridge,
    ) -> Result<(), ServiceError> {
        // BDC detection is broken on this board: BDC shorts pin14 and pin1;
        // BDC connector pin14 floats; pin1 is routed to SB GPIO54.  Coreboot
        // only gives the H8 driver WWAN GPIO70 active-low for presence.
        let _wwan_present = southbridge
            .gpio_get(self.config.wwan_gpio_num as u32)
            .is_ok_and(|level| level == self.config.wwan_gpio_lvl);
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), ServiceError> {
        FinalizeInit::finalize_init(self)
    }
}

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi::aml::Path;
    use fstart_acpi::device::AcpiDevice;
    use fstart_acpi_macros::acpi_dsl;

    use super::*;

    impl AcpiDevice for LenovoX220Mainboard {
        type Config = LenovoX220MainboardConfig;

        fn dsdt_aml(&self, _config: &Self::Config) -> Vec<u8> {
            let mute = Path::new("\\_SB_.PCI0.LPCB.EC__.MUTE");
            let usbp = Path::new("\\_SB_.PCI0.LPCB.EC__.USBP");
            let radi = Path::new("\\_SB_.PCI0.LPCB.EC__.RADI");
            let wake = Path::new("\\_SB_.PCI0.LPCB.EC__.HKEY.WAKE");

            acpi_dsl! {
                Scope("\\") {
                    Name("TECG", 17u32);
                    Method("_PTS", 1, NotSerialized) {
                        #{mute}(1u32);
                        #{usbp}(0u32);
                        #{radi}(0u32);
                    }
                    Method("_WAK", 1, NotSerialized) {
                        #{wake}(Arg0);
                        Return(Package(0u32, 0u32));
                    }
                }
            }
        }
    }
}
