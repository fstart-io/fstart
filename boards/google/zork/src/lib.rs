// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vilboz, Vilboz14 and Vilboz360 share Dalboz wiring and a single native flow.
//! SKU differences come from EC CBI FW_CONFIG, not independent Rust board copies.
//!
//! The flow is the board's: `src/bin/reset.rs`, from the PSP's reset vector to
//! the payload. This library holds the board's facts and the mainboard work
//! that `main` calls.
#![no_std]
pub mod dalboz;
pub mod ec;
pub mod policy;
/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
pub mod smm;
pub mod vilboz;
use fstart_driver_amd::soc::picasso::Picasso;
use fstart_driver_uart::ns16550::Ns16550Config;
use fstart_platform_amd::picasso::{self, Config, FchConfig};

/// How Vilboz wires the Picasso blocks.
pub static CONFIG: Config = Config {
    fch: FchConfig {
        console_uart: 0,
        spi: policy::SPI,
        espi: Some((policy::ESPI, dalboz::ESPI_GPIO_TABLE)),
    },
    pcie: policy::PCIE,
    usb: policy::USB,
    // Fitted or not per CBI.
    emmc: Some(policy::EMMC),
    i2c: &[
        picasso::I2cBus {
            controller: 2,
            // 150-MHz controller, 400-kHz bus; 18/57-ns board rise/fall.
            timing: fstart_driver_amd::fch::i2c::Timing {
                speed: fstart_driver_amd::fch::i2c::Speed::Fast,
                scl_high: 117,
                scl_low: 245,
                spike_length: Some(2),
                sda_hold: Some(45),
            },
            // Vilboz is fitted with one of the two touchpads.
            devices: &[
                picasso::I2cDevice {
                    ids: &["ELAN0000"],
                    address: 0x15,
                    interrupt: Some(picasso::GpioInterrupt::Level {
                        pin: 9,
                        active_low: true,
                    }),
                    hid_descriptor_register: None,
                },
                picasso::I2cDevice {
                    ids: &["SYNA0000", "PNP0C50"],
                    address: 0x2c,
                    interrupt: Some(picasso::GpioInterrupt::Level {
                        pin: 9,
                        active_low: true,
                    }),
                    hid_descriptor_register: Some(0x20),
                },
            ],
        },
        picasso::I2cBus {
            controller: 3,
            // 150-MHz controller, 400-kHz bus; 110/5-ns board rise/fall.
            timing: fstart_driver_amd::fch::i2c::Timing {
                speed: fstart_driver_amd::fch::i2c::Speed::Fast,
                scl_high: 125,
                scl_low: 223,
                spike_length: Some(2),
                sda_hold: Some(45),
            },
            // The Cr50 TPM, its ready interrupt latched on GPIO 3.
            devices: &[picasso::I2cDevice {
                ids: &["GOOG0005"],
                address: 0x50,
                interrupt: Some(picasso::GpioInterrupt::Edge {
                    pin: 3,
                    falling: true,
                }),
                hid_descriptor_register: None,
            }],
        },
    ],
    graphics: fstart_driver_amd::nbio::picasso::gfx::Config {
        displays: policy::DISPLAYS,
        hdmi2_disable: 2,
    },
};
pub struct Board;
impl fstart_platform_amd::noncar::Board for Board {
    type Soc = Picasso;
    const CONFIG: &'static Config = &CONFIG;
}

/// The console on FCH UART 0.
pub fn console() -> Ns16550Config {
    picasso::console(CONFIG.fch.console_uart)
}

/// The board's pads once the FCH is up. Without a PSP verstage the early
/// and TPM tables are programmed first, then the PCIe and memory init ones.
pub fn early_gpios(
    gpio: &fstart_driver_amd::fch::gpio::Gpio<impl fstart_driver_amd::fch::Bus>,
) -> Result<(), fstart_driver_amd::fch::gpio::Error> {
    for table in [
        dalboz::EARLY_GPIO_TABLE,
        dalboz::TPM_GPIO_TABLE,
        dalboz::PCIE_GPIO_TABLE,
        dalboz::ROMSTAGE_GPIO_TABLE,
    ] {
        gpio.configure(table, &[])?;
    }
    Ok(())
}

/// Once the early pads are set and ROM sharing is off: read the board
/// facts from the EC's CBI, power and release WLAN before DXIO probes it,
/// and program the remaining GPIOs. The CBI decides the SMU power policy
/// and whether eMMC is fitted.
pub fn after_fch(
    gpio: &fstart_driver_amd::fch::gpio::Gpio<impl fstart_driver_amd::fch::Bus>,
    clock: &mut impl fstart_driver_amd::ccx::tsc::Clock,
) -> policy::Cbi {
    mainboard::init(gpio, clock)
}

/// The keyboard behind the EC.
pub fn acpi() -> fstart_platform_amd::acpi::BoardAcpi {
    fstart_platform_amd::acpi::BoardAcpi {
        lpc_aml: fstart_driver_chromeec::acpi::keyboard_aml(),
        ..Default::default()
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
mod mainboard {
    use super::{ec, vilboz};
    use crate::policy::Cbi;
    use fstart_driver_amd::{ccx::tsc, fch::gpio};
    use fstart_driver_chromeec::{Clock, Ec, NativeIo};
    use fstart_log::Hex;

    struct EcClock<'a, T>(&'a mut T);
    impl<T: tsc::Clock> Clock for EcClock<'_, T> {
        fn now_us(&mut self) -> u64 {
            self.0.now_us()
        }
        fn delay_us(&mut self, us: u32) {
            self.0.delay_us(us)
        }
    }

    pub(super) fn init(
        gpio: &gpio::Gpio<impl fstart_driver_amd::fch::Bus>,
        clock: &mut impl tsc::Clock,
    ) -> Cbi {
        // SAFETY: the eSPI windows for the EC ports are up; the BSP owns them.
        let cbi = match Ec::discover(unsafe { NativeIo::new() }) {
            Ok(mut ec) => ec::read_cbi(&mut ec, &mut EcClock(clock), &mut |tag, error| {
                if matches!(
                    (tag, error),
                    (
                        fstart_driver_chromeec::CbiTag::Ssfc,
                        fstart_driver_chromeec::Error::Rejected(3)
                    )
                ) {
                    fstart_log::debug!("vilboz: optional SSFC absent");
                } else {
                    fstart_log::warn!("vilboz: CBI read failed");
                }
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
        // Power and release WLAN before DXIO probes the endpoint. The PCIe
        // GPIO table deliberately leaves GPIO29/42 to this revision-dependent
        // sequence; merely configuring the table leaves WLAN held in reset.
        let wifi = gpio.configure(vilboz::WLAN_PADS, &[]).and_then(|()| {
            let mut reset = gpio.output(vilboz::WLAN_RESET_GPIO)?;
            let mut power = gpio.output(vilboz::WLAN_POWER_GPIO)?;
            vilboz::wifi_power_reset(&mut reset, &mut power, clock, cbi)
        });
        if wifi.is_err() {
            fstart_log::error!("vilboz: WiFi power/reset failed");
        }
        // Not ChromeOS: GPIO40 stays a plain input on newer boards.
        if vilboz::configure_ram(gpio, cbi, false).is_err() {
            fstart_log::error!("vilboz: RAM-stage GPIOs failed");
        }
        cbi
    }
}
#[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
mod mainboard {
    use fstart_driver_amd::{ccx::tsc::Clock, fch::gpio};
    pub(super) fn init(
        _: &gpio::Gpio<impl fstart_driver_amd::fch::Bus>,
        _: &mut impl Clock,
    ) -> crate::policy::Cbi {
        crate::policy::Cbi::new(None, None)
    }
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
