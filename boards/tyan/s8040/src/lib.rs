// SPDX-License-Identifier: MIT OR Apache-2.0
//! Tyan S8040GM2NE: one SP6 socket, Siena (EPYC 8004), six DDR5 channels
//! of two slots, an AST2600 BMC on eSPI.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod bmc;
pub mod gpio;
pub mod mpio;
/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
mod smm;
pub mod usb;

use fstart_driver_amd::ccx::{pm::Wdt, rs as ccx};
use fstart_driver_amd::dxio::rs::{mailbox::Encoding, session};
use fstart_driver_amd::nbio::rs as nbio;
use fstart_platform_amd::board::SocId;
use fstart_platform_amd::rs::{Config, Mpio};

pub struct Board;

impl fstart_platform_amd::board::Board for Board {
    const NAME: &'static str = "Tyan S8040GM2NE";
    const SOCS: &'static [SocId] = &[SocId::Siena];
    /// 64 cores, SMT.
    const MAX_CPUS: u16 = 128;
}

/// Soldered endpoints that must train: the two I210s (lanes 132 and 133)
/// and the AST2600's display bridge (lane 134).
const REQUIRED: [(u8, u16); 3] = [(132, 0x8086), (133, 0x8086), (134, 0x1a03)];

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
            required: &REQUIRED,
        },
        usb: usb::POLICY,
        // The AST2600's display, behind its AST1150 bridge on the BMC's
        // MPIO port.
        display: Some(mpio::BMC_VGA.0),
        legacy_devices: true,
        // UMC 1, 3, 4, 7, 9 and 10 (vendor type 16: 12 devices).
        memory_channels: 1 << 1 | 1 << 3 | 1 << 4 | 1 << 7 | 1 << 9 | 1 << 10,
        // The vendor's type 16: 6 TB over twelve slots.
        max_dimm_mib: 512 * 1024,
    }
}

/// The MPIO ports of the default connector choices. The BMC's display
/// link is trained cold, as every other port.
pub fn mpio_ports() -> Result<heapless::Vec<session::Config<'static>, 32>, mpio::Error> {
    let engines = mpio::engines(
        &mpio::Options::DEFAULT,
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

/// The table identity.
pub fn smbios_identity() -> fstart_acpi::smbios::SmbiosIdentity<'static> {
    fstart_acpi::smbios::SmbiosIdentity::new("TYAN", "S8040GM2NE")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_cold_ports_keep_the_sata_ancillary_data() {
        let ports = super::mpio_ports().unwrap();
        assert_eq!(ports.len(), 16);
        let bmc = ports.iter().find(|p| p.port.start == 134).unwrap();
        assert!(bmc.port.present && !bmc.port.early_train);
        let sata = ports.iter().find(|p| p.port.start == 92).unwrap();
        assert_eq!(sata.speed, &[0x1c07, 0x2107, 0x0800_2807]);
    }
}
