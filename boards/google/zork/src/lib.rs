// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vilboz, Vilboz14 and Vilboz360 share Dalboz wiring and a single native flow.
//! SKU differences come from EC CBI FW_CONFIG, not independent Rust board copies.
#![no_std]
pub mod dalboz;
pub mod ec;
pub mod policy;
pub mod vilboz;
use fstart_driver_amd::{
    gpio::{self, Pad},
    picasso::{espi, preinit::SpiConfig},
};
use fstart_platform_amd::picasso::{BoardFacts, Config, Timer};
static CONFIG: Config = Config::new().uart(0).build();
pub struct Board;
impl BoardFacts for Board {
    const CONFIG: &'static Config = &CONFIG;
    const SPI: SpiConfig = policy::SPI;
    const ESPI: Option<(espi::Config, &'static [Pad])> =
        Some((policy::ESPI, dalboz::ESPI_GPIO_TABLE));
    // Without a PSP verstage the early and TPM tables are programmed
    // first, then the PCIe and memory init tables.
    const EARLY_GPIOS: &'static [&'static [Pad]] = &[
        dalboz::EARLY_GPIO_TABLE,
        dalboz::TPM_GPIO_TABLE,
        dalboz::PCIE_GPIO_TABLE,
        dalboz::ROMSTAGE_GPIO_TABLE,
    ];
    const DISABLE_ROM_SHARING: bool = true;
    fn mainboard_init(gpio: &gpio::Gpio<impl fstart_driver_amd::fch::Bus>, timer: &mut impl Timer) {
        mainboard::init(gpio, timer)
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
mod mainboard {
    use super::{ec, vilboz};
    use crate::policy::Cbi;
    use fstart_driver_amd::gpio;
    use fstart_driver_chromeec::{Clock, Ec, NativeIo};
    use fstart_log::Hex;
    use fstart_platform_amd::picasso::Timer;

    struct EcClock<'a, T>(&'a mut T);
    impl<T: Timer> Clock for EcClock<'_, T> {
        fn now_us(&mut self) -> u64 {
            self.0.now_us()
        }
        fn delay_us(&mut self, us: u32) {
            self.0.delay_us(us)
        }
    }

    pub(super) fn init(gpio: &gpio::Gpio<impl fstart_driver_amd::fch::Bus>, timer: &mut impl Timer) {
        // SAFETY: the eSPI windows for the EC ports are up; the BSP owns them.
        let cbi = match Ec::discover(unsafe { NativeIo::new() }) {
            Ok(mut ec) => ec::read_cbi(&mut ec, &mut EcClock(timer), &mut |_, _| {
                fstart_log::warn!("vilboz: CBI read failed")
            }),
            Err(_) => {
                fstart_log::error!("vilboz: EC not found");
                Cbi::new(None, None)
            }
        };
        fstart_log::info!(
            "vilboz: board version {}, FW_CONFIG {}",
            cbi.board_version.unwrap_or(u32::MAX),
            Hex(cbi.firmware_config.unwrap_or(u64::MAX))
        );
        // Not ChromeOS: GPIO40 stays a plain input on newer boards.
        if vilboz::configure_ram(gpio, cbi, false).is_err() {
            fstart_log::error!("vilboz: RAM-stage GPIOs failed");
        }
    }
}
#[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
mod mainboard {
    use fstart_driver_amd::gpio;
    use fstart_platform_amd::picasso::Timer;
    pub(super) fn init(_: &gpio::Gpio<impl fstart_driver_amd::fch::Bus>, _: &mut impl Timer) {}
}

pub const FLASH_SIZE: u32 = 0x1000000;
pub const EFS_OFFSET: u32 = 0xe20000;
/// RAM-ID GPIOs must be sampled before GPIO86 is driven high.
pub const RAM_ID_PINS: [u8; 4] = [132, 90, 86, 69];
/// APCB triples are NOT the same encoding as the x86 IOMUX pad selectors.
/// Dalboz only populates channel 0 DIMM 0.
pub const APCB_RAM_ID: [[u8; 3]; 4] = [[132, 1, 0], [90, 1, 0], [86, 3, 0], [69, 1, 0]];
/// Set-0 DDR4 SPD index selected by the *four-bit* RAM strap. IDs 0/1 are empty.
pub const SPD_SET0_INDEX: [Option<u8>; 16] = [
    None,
    None,
    Some(1),
    Some(1),
    Some(1),
    Some(7),
    Some(7),
    Some(2),
    Some(9),
    Some(9),
    Some(1),
    Some(1),
    Some(9),
    None,
    None,
    None,
];
