// SPDX-License-Identifier: MIT OR Apache-2.0
//! Gigabyte MJ11-EC1 (G431-MM0) and MJ11-EC0: EPYC Embedded 3000 (SP4r2) with
//! an AST2500 BMC.
//!
//! The flow is the board's: `src/bin/stage.rs`, from the PSP's reset vector
//! to the payload. This library holds the board's facts and the mainboard
//! work that `main` calls.
#![no_std]
extern crate alloc;

mod acpi;
pub mod bmc;
mod dxio;
mod gpio;
mod i2c;
mod riser;
/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
pub mod smm;
mod variant;

use fstart_driver_amd::{ccx::tsc::Tsc, fch::gpio::Gpio, window::Window};
use fstart_driver_amd::{
    fch::{
        self, FchHw,
        acpimmio::{ACPI_PM1_ENABLE, ACPI_PM1_STATUS, Pm1Enable, Pm1Status},
        irq::{self, Route},
    },
    smn::{apply, write},
};
use fstart_pci::ecam::GlobalEcam;
use fstart_platform_amd::zeppelin::{self as soc, Die, Zeppelin};
use fstart_platform_amd::zeppelin::{CcxConfig, Config, DxioConfig, FchConfig};
use tock_registers::interfaces::ReadWriteable;

pub struct Board;

/// The BMC's diagnostic interrupt button.
const NMI_BUTTON_GEVENT: u8 = 11;

/// The IOAPIC side matches the vendor firmware; the vendor PIC values aren't
/// known, so INTA-D are left for the OS to route through ACPI. MISC..MISC2
/// keep their native values (the platform enforces that).
const IRQ_ROUTES: &[Route] = &[
    Route {
        source: irq::A,
        pic: irq::NC,
        apic: 16,
    },
    Route {
        source: irq::B,
        pic: irq::NC,
        apic: 17,
    },
    Route {
        source: irq::C,
        pic: irq::NC,
        apic: 18,
    },
    Route {
        source: irq::D,
        pic: irq::NC,
        apic: 19,
    },
    Route {
        source: irq::SCI,
        pic: 9,
        apic: 9,
    },
    Route {
        source: irq::GPIO,
        pic: 7,
        apic: 7,
    },
];

/// The PM1 write of the vendor AmdCpmOemInitPeim and SMBusSwitch: write back
/// the pending PM1 status plus bits 15:14 (all W1C) and clear
/// PCIEXP_WAKE_DIS.
pub fn clear_pm1(fch: &impl FchHw) {
    fch.acpi()
        .at(ACPI_PM1_STATUS)
        .modify(Pm1Status::WAK::SET + Pm1Status::PCIEXP_WAKE::SET);
    fch.acpi()
        .at(ACPI_PM1_ENABLE)
        .modify(Pm1Enable::PCIEXP_WAKE_DIS::CLEAR);
}

/// How the MJ11 wires the Zeppelin blocks.
pub static CONFIG: Config = Config {
    // Gigabyte
    subsystem_id: 0x1000_1458,
    ccx: CcxConfig::DEFAULT,
    fch: FchConfig {
        irq_routes: IRQ_ROUTES,
        smi_gevents: 1 << NMI_BUTTON_GEVENT,
        // SSC causes I210 BadDLLP/BadTLP errors; F09 Setup also disables
        // "SB Clock Spread Spectrum" (0xfc) on this board.
        spread_spectrum: false,
        ..FchConfig::DEFAULT
    },
    dxio: DxioConfig {
        descriptors: [dxio::DEFAULT, &[]],
    },
};

impl fstart_platform_amd::board::Board for Board {
    const NAME: &'static str = "Gigabyte MJ11-EC1";
    const SOCS: &'static [fstart_platform_amd::board::SocId] =
        &[fstart_platform_amd::board::SocId::Zeppelin];
    /// The package's ceiling: two dies of eight SMT cores.
    const MAX_CPUS: u16 = 32;
    /// SW SMIs 0xC0 + Sx report the sleep state to the BMC (AMI BmcAcpi).
    const BMC_SX_NOTIFY: Option<fstart_driver_ipmi::SleepNotify> =
        Some(fstart_driver_ipmi::SleepNotify {
            kcs_port: fstart_driver_ipmi::kcs::DEFAULT_PORT,
            kcs_spacing: fstart_driver_ipmi::kcs::DEFAULT_SPACING as u8,
            sw_smi_base: 0xc0,
        });

    fn acpi_board() -> fstart_platform_amd::acpi::Board {
        crate::acpi()
    }
}

/// The VRs and PCIe redrivers on I2C3 (the vendor board PEIMs, once the
/// FCH's SMBus is up): route the I2C3 pads, then program both. Returns the
/// PCIE_1 riser's ID: from its EEPROM on the same bus, else from the straps.
pub fn program_i2c_devices(gpio: &Gpio<impl Window>, clock: Tsc) -> Option<u8> {
    if i2c::PADS
        .iter()
        .any(|&pad| gpio.set_function(pad, i2c::FUNCTION).is_err())
    {
        fstart_log::error!("I2C3 pads not routed");
    }
    // SAFETY: nothing else uses I2C3.
    let eeprom_id =
        match unsafe { fch::i2c::controller::<Zeppelin, _>(i2c::BUS, &soc::I2C_TIMING, clock) } {
            Ok(mut bus) => {
                i2c::program_vrs(&mut bus, clock, variant::VR_IMAGES);
                i2c::program_redrivers(&mut bus, variant::REDRIVERS);
                riser::eeprom_id(&mut bus)
            }
            Err(_) => {
                fstart_log::error!("I2C3 setup failed, VRs not programmed");
                None
            }
        };
    // The straps without the EEPROM, as without I2C.
    eeprom_id.or_else(|| {
        let levels = riser::STRAPS.map(|pin| gpio.get(pin) == Ok(true));
        riser::strap_id(levels)
    })
}

/// The DXIO engines for the PCIE_1 riser `id` (the vendor Setup default
/// "Auto"). The modes are the F02 (EC0) decoding; the EC1's F09 template
/// splits its slot on 3.1 and 3.3 instead and its mode table isn't decoded,
/// so the EC1 only reports the riser.
pub fn dxio(id: Option<u8>) -> DxioConfig {
    let mode = id.and_then(riser::bifurcation);
    fstart_log::info!(
        "PCIE_1 riser {}: {}",
        id.map_or(0xff, u32::from),
        match mode {
            Some(riser::Bifurcation::X4x4x4x4) => "x4x4x4x4",
            Some(riser::Bifurcation::X4x4) => "x4x4",
            Some(riser::Bifurcation::X8x4x4) => "x8x4x4",
            Some(riser::Bifurcation::X8x8) => "x8x8",
            None => "the default",
        }
    );
    let mut dxio = CONFIG.dxio;
    if let Some(mode) = mode.filter(|_| cfg!(feature = "ec0")) {
        dxio.descriptors[0] = dxio::with_riser(mode);
    }
    dxio
}

/// The vendor's PCIe TX presets and USB2 PHY values, after link training.
pub fn apply_phy_tuning(die0: &Die<GlobalEcam>) {
    let records = variant::POST_TRAINING_SMN
        .iter()
        .map(|&(register, value)| write(register, value));
    apply(&die0.smn(), records);
}

/// The vendor GPIO table, after PCI enumeration.
pub fn program_vendor_gpios(gpio: &Gpio<impl Window>) {
    if gpio.configure(gpio::VENDOR, &[]).is_err() {
        fstart_log::error!("Vendor GPIO table rejected");
    }
}

/// What the board adds to the ACPI tables.
pub fn acpi() -> fstart_platform_amd::acpi::Board {
    acpi::tables()
}

/// The board's SMBIOS identity.
pub const SMBIOS: fstart_platform_amd::acpi::SmbiosIdentity<'static> =
    fstart_platform_amd::acpi::SmbiosIdentity {
        sys_version: variant::NAME,
        bb_version: variant::NAME,
        // A rack server (G431-MM0).
        chassis_type: 0x17,
        chassis_manufacturer: "Gigabyte",
        processor_sockets: &["SP4r2"],
        ..fstart_platform_amd::acpi::SmbiosIdentity::new("Gigabyte", "MJ11")
    };
