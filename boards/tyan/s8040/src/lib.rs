// SPDX-License-Identifier: MIT OR Apache-2.0
//! Tyan S8040GM2NE: one SP6 socket, Siena (EPYC 8004), six DDR5 channels
//! of two slots, an AST2600 BMC on eSPI.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod bmc;
pub mod gpio;
pub mod mpio;
pub mod options;
/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
mod smm;
pub mod usb;

use fstart_driver_amd::ccx::{pm::Wdt, rs as ccx};
use fstart_driver_amd::dxio::rs::{mailbox::Encoding, session};
use fstart_driver_amd::fch::{gpio::Pin, songshan::Perst};
use fstart_driver_amd::nbio::rs as nbio;
use fstart_platform_amd::board::SocId;
use fstart_platform_amd::rs::links::{Behind, Soldered};
use fstart_platform_amd::rs::{Config, Mpio};

pub struct Board;

impl fstart_platform_amd::board::Board for Board {
    const NAME: &'static str = "Tyan S8040GM2NE";
    const SOCS: &'static [SocId] = &[SocId::Siena];
    /// 64 cores, SMT.
    const MAX_CPUS: u16 = 128;
    const OPTION_SETS: &'static [&'static dyn fstart_options::Describe] =
        &[&options::PCIE, &options::POWER];
    /// Corrected errors counted in SMM and reported to the BMC's SEL
    /// ([`bmc::sel`]), ten per thresholding SMI.
    const RAS: fstart_platform_amd::smm::ras::Policy =
        fstart_platform_amd::smm::ras::Policy::THRESHOLDS;
    const BMC_EVENT: Option<fstart_platform_amd::board::BmcEvent> = Some(bmc::sel);

    fn acpi_board() -> fstart_platform_amd::acpi::Board {
        bmc::acpi_board()
    }

    fn apcb_tokens(_soc: SocId) -> &'static [fstart_platform_amd::board::ApcbTokenOverride] {
        const TOKENS: &[fstart_platform_amd::board::ApcbTokenOverride] =
            &[fstart_platform_amd::board::ApcbTokenOverride::disable_psb_auto_fuse()];
        TOKENS
    }
}

/// Soldered devices the boot needs, by their lane and what is behind the
/// port: the two I210s (lanes 132 and 133), and the AST2600's display
/// behind its AST1150 bridge (lane 134).
const REQUIRED: [Soldered; 3] = [
    Soldered {
        lane: mpio::LAN[0],
        behind: Behind::Device,
    },
    Soldered {
        lane: mpio::LAN[1],
        behind: Behind::Device,
    },
    Soldered {
        lane: mpio::BMC_VGA.0,
        behind: Behind::BridgedDisplay,
    },
];

/// The PCIe slot resets: FCH pad 26 and pad 10 of the remote bank, at
/// drive strength 2 (the vendor's FchPerstDeassertGpio `ffa46748`).
const PERST: [Perst; 2] = [
    Perst {
        pin: Pin(26),
        drive_strength: 2,
    },
    Perst {
        pin: Pin(266),
        drive_strength: 2,
    },
];

/// The board's configuration of Siena, as the vendor firmware has it.
pub fn config() -> Config {
    Config {
        gpio: &gpio::GPIO,
        // All NBIO clock gating and TPH completion on.
        nbio: nbio::Policy {
            clock_gating: true,
            syshub_clock_gating: true,
            tph_completer: true,
        },
        // The CPU watchdog on (CpuWdtCfg time base 0, count 1) and
        // write-combining speculation on every thread.
        ccx: ccx::Policy {
            wdt: Some(Wdt {
                count_sel: 1,
                time_base: 0,
            }),
            wc_spec: Some(true),
            ..ccx::Policy::default()
        },
        mpio: Mpio {
            psp_vetting: true,
            encoding: Encoding::Retained,
            required: REQUIRED.into_iter().collect(),
            // Every port is MPIO's, the BMC's display link too.
            psp_trained: &[],
            perst: &PERST,
        },
        usb: usb::POLICY,
        // "Clock Spread Spectrum" off, the V2.00 Setup default (`Setup
        // 0xbf` = 0; FchSongshanDxe takes its policy from PCD 0x370,
        // also 0), as on the S8030's Milan image and the MJ11, whose
        // I210s log correctable errors on a spread clock.
        spread_spectrum: false,
        // The AST2600's display, behind its AST1150 bridge on the BMC's
        // MPIO port.
        display: Some(mpio::BMC_VGA.0),
        legacy_devices: true,
        // UMC 1, 3, 4, 7, 9 and 10 (vendor type 16: 12 devices).
        memory_channels: 1 << 1 | 1 << 3 | 1 << 4 | 1 << 7 | 1 << 9 | 1 << 10,
        // The vendor's type 16: 6 TB over twelve slots.
        max_dimm_mib: 512 * 1024,
        // The PSP's EINJ, for RAS validation on this development board.
        einj: true,
    }
}

/// Apply the same enable policy to endpoint checks and display selection
/// as to link training. Disabled devices are not boot requirements.
pub fn configure_pcie(config: &mut Config, options: &mpio::Options) {
    config.mpio.required = REQUIRED
        .into_iter()
        .zip([options.lan[0], options.lan[1], options.vga])
        .filter_map(|(device, enabled)| enabled.then_some(device))
        .collect();
    config.display = options.vga.then_some(mpio::BMC_VGA.0);
}

/// The MPIO ports of the selected connector choices. The BMC's display
/// link, if enabled, is trained cold, as every other port.
pub fn mpio_ports(
    options: &mpio::Options,
) -> Result<heapless::Vec<session::Config<'static>, 32>, mpio::Error> {
    let engines = mpio::engines(
        options,
        &mpio::Detected {
            bmc_link_present: true,
        },
    )?;
    Ok(engines
        .iter()
        .filter(|engine| engine.port.present)
        .map(|engine| session::Config {
            port: engine.port,
            phy: &[],
            speed: engine.speed_overrides,
        })
        .collect())
}

/// Check the generated requests before accepting a settings override.
pub fn check_options(options: &mpio::Options) -> Result<(), &'static str> {
    let ports = mpio_ports(options).map_err(|_| "too many MPIO ports")?;
    // A supported Siena can have IOS1 fused off. MPIO also checks against
    // the actual detected roots immediately before sending the requests.
    session::Prepared::new(0, &[0, 2, 3], &ports)
        .map(|_| ())
        .map_err(|_| "invalid S8040 MPIO ports")
}

/// The static board SMBIOS identity. Type 38 is the KCS3 of
/// [`bmc::acpi_board`]; the BMC's FRU names the board `S8040GM2NE` too.
pub const BASE_SMBIOS: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity::new("TYAN", "S8040GM2NE");

/// The table identity, enriched with BMC FRU serials and system UUID when
/// available.
pub fn smbios_identity<'a>(
    fru: Option<&'a fstart_driver_ipmi::fru::FruIdentity>,
) -> fstart_acpi::smbios::SmbiosIdentity<'a> {
    match fru {
        Some(fru) => fru.apply_to(BASE_SMBIOS),
        None => BASE_SMBIOS,
    }
}

#[cfg(test)]
mod tests {
    use fstart_pci::PciAddress;
    use fstart_platform_amd::rs::links::{Behind, missing};

    /// The board's three requirements are its lanes and what is behind
    /// them; the display the legacy VGA goes to is the BMC's, behind its
    /// bridge. A link that does not come up is a missing device.
    #[test]
    fn the_soldered_devices_are_lanes_and_topology() {
        let config = super::config();
        let required = config.mpio.required;
        let lanes: Vec<_> = required.iter().map(|s| (s.lane, s.behind)).collect();
        assert_eq!(
            lanes,
            [
                (132, Behind::Device),
                (133, Behind::Device),
                (134, Behind::BridgedDisplay)
            ]
        );
        assert!(
            required
                .iter()
                .any(|s| Some(s.lane) == config.display && s.behind == Behind::BridgedDisplay)
        );
        // Each of them is a port MPIO trains.
        let ports = super::mpio_ports(&super::mpio::Options::DEFAULT).unwrap();
        for soldered in &required {
            assert!(ports.iter().any(|p| p.port.start == soldered.lane));
        }
        let port = PciAddress::new(0, 0, 1, 1);
        let up = [(132, port, true), (133, port, true), (134, port, true)];
        assert_eq!(missing(&required, &up).count(), 0);
        let display_down = [(132, port, true), (133, port, true), (134, port, false)];
        let missing: Vec<_> = missing(&required, &display_down).map(|s| s.lane).collect();
        assert_eq!(missing, [134]);
    }

    #[test]
    fn disabled_links_are_not_required_or_selected_for_display() {
        for mask in 0..8 {
            let options = super::mpio::Options {
                lan: [mask & 1 != 0, mask & 2 != 0],
                vga: mask & 4 != 0,
                ..super::mpio::Options::DEFAULT
            };
            let mut config = super::config();
            super::configure_pcie(&mut config, &options);
            let ports = super::mpio_ports(&options).unwrap();
            for (i, device) in super::REQUIRED.iter().enumerate() {
                let enabled = mask & (1 << i) != 0;
                assert_eq!(ports.iter().any(|p| p.port.start == device.lane), enabled);
                assert_eq!(config.mpio.required.contains(device), enabled);
            }
            assert_eq!(
                config.display,
                options.vga.then_some(super::mpio::BMC_VGA.0)
            );
        }
    }

    #[test]
    fn the_cold_ports_keep_the_sata_ancillary_data() {
        let ports = super::mpio_ports(&super::mpio::Options::DEFAULT).unwrap();
        assert_eq!(ports.len(), 16);
        let bmc = ports.iter().find(|p| p.port.start == 134).unwrap();
        assert!(bmc.port.present && !bmc.port.early_train);
        let sata = ports.iter().find(|p| p.port.start == 92).unwrap();
        assert_eq!(sata.speed, &[0x1c07, 0x2107, 0x0800_2807]);
    }
}
