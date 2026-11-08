// SPDX-License-Identifier: MIT OR Apache-2.0
//! Tyan S8040GM2NE: one SP6 socket, Siena / EPYC 8004, six DDR5 channels.
//!
//! Hardware mechanisms live in drivers and fstart-platform-rs; this crate
//! owns the board's connector, GPIO, USB, console and BMC policy.
#![cfg_attr(not(test), no_std)]

pub mod bmc;
pub mod gpio;
pub mod mpio;
#[cfg(feature = "smm")]
pub mod smm;
pub mod usb;
// Host fixtures belong to the library: an integration test would also
// build the firmware-only binary without fbuild's selected stage runtime.
#[cfg(test)]
mod v200_test;

pub struct Board;
impl fstart_platform_rs::Board for Board {
    type Soc = fstart_platform_rs::Siena;
    const NAME: &'static str = "Tyan S8040GM2NE-NFX";
    const SMBIOS_MANUFACTURER: &'static str = "TYAN";
    const SMBIOS_PRODUCT: &'static str = "S8040GM2NE-NFX";
    /// The vendor's type 16: 6 TB over 12 positions.
    const SMBIOS_MAX_DIMM_MIB: u32 = 512 * 1024;
    /// Six channels, two slots each (vendor type 16: 12 devices): UMC 1, 3,
    /// 4, 7, 9 and 10.
    const MEMORY_CHANNELS: u16 = 1 << 1 | 1 << 3 | 1 << 4 | 1 << 7 | 1 << 9 | 1 << 10;
    /// 64 MiB host NOR.
    const FLASH_SIZE: u32 = 0x400_0000;
    /// The AST2600's COM1, the BMC's SOL.
    const CONSOLE: fstart_platform_rs::Console =
        fstart_platform_rs::Console::SuperIoCom1 { irq: bmc::COM1_IRQ };
    /// As the vendor firmware sets them: all NBIO clock gating and TPH
    /// completion on, the CPU watchdog on (CpuWdtCfg time base 0, count 1)
    /// and write-combining speculation on every thread (LS_CFG3 bits 30/33).
    const NBIO: fstart_driver_amd::nbio::rs::Policy = fstart_driver_amd::nbio::rs::Policy {
        clock_gating: true,
        syshub_clock_gating: true,
        tph_completer: true,
    };
    const WATCHDOG: Option<(u8, u8)> = Some((0, 1));
    const WC_SPEC: Option<bool> = Some(true);
    const LINUX_BOOTARGS: &'static str =
        "console=ttyS0,115200 earlycon=uart8250,io,0x3f8,115200n8 panic=0";
    const REQUIRED_PCIE: &'static [(u8, u16)] = &[(132, 0x8086), (133, 0x8086), (134, 0x1a03)];
    /// The BMC's AST2600 display behind its AST1150 bridge (MPIO lane 134).
    const PRIMARY_VGA: Option<(u16, u16)> = Some((0x1a03, 0x2000));
    fn cpm_gpio() -> heapless::Vec<(u16, u16), 64> {
        let mut pads = heapless::Vec::new();
        let mode = gpio::connector_mode(&mpio::Options::DEFAULT);
        for &pad in gpio::CPM_GPIO.iter().chain(mode.iter()) {
            pads.push(pad).expect("CPM table fits");
        }
        pads
    }
    unsafe fn early_init() {
        // SAFETY: the platform owns and has decoded the SIO ports.
        unsafe { bmc::early_init() };
    }
    fn mpio_ports() -> Result<
        heapless::Vec<fstart_driver_amd::mpio::session::Config<'static>, 32>,
        fstart_driver_amd::mpio::session::Error,
    > {
        use fstart_driver_amd::mpio::session::{Config, Error};
        // Do not fabricate the vendor's optional early-link PPI. Normal cold
        // training below explicitly probes this populated board's soldered
        // BMC-VGA link; it is not reported up until actual config readback.
        let engines = mpio::engines(
            &mpio::Options::DEFAULT,
            &mpio::Detected {
                bmc_link_present: false,
            },
        )
        .map_err(|_| Error::Capacity)?;
        let mut ports = heapless::Vec::new();
        for mut engine in engines {
            if engine.destination == mpio::Destination::BmcVga {
                // Native cold receiver detection, not the legacy early path.
                engine.port.present = true;
                engine.port.early_train = false;
            }
            if engine.port.present {
                ports
                    .push(Config {
                        port: engine.port,
                        phy: &[],
                        speed: engine.speed_overrides,
                    })
                    .map_err(|_| Error::Capacity)?;
            }
        }
        Ok(ports)
    }
    fn usb_policy() -> Option<fstart_driver_amd::fch::songshan::usb::Policy> {
        Some(usb::POLICY)
    }
    unsafe fn release_perst() -> Result<(), &'static str> {
        unsafe { gpio::release_perst() }
    }
}

/// Only GM2NE V2.00 board policy has been decoded. GM4NE-2T and the
/// live GM2NE-NFX firmware must not be assumed equivalent.
pub const MODEL: &str = "S8040GM2NE";

#[cfg(test)]
mod tests {
    #[test]
    fn native_cold_policy_keeps_ancillary_and_requires_real_onboard_identities() {
        use fstart_platform_rs::Board as _;
        let ports = super::Board::mpio_ports().unwrap();
        assert_eq!(ports.len(), 16);
        assert_eq!(
            super::Board::REQUIRED_PCIE,
            &[(132, 0x8086), (133, 0x8086), (134, 0x1a03)]
        );
        let bmc = ports.iter().find(|p| p.port.start == 134).unwrap();
        assert!(bmc.port.present);
        assert!(!bmc.port.early_train);
        let sata = ports.iter().find(|p| p.port.start == 92).unwrap();
        assert_eq!(sata.speed, &[0x1c07, 0x2107, 0x08002807]);
    }
}
