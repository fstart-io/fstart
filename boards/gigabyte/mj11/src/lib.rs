// SPDX-License-Identifier: MIT OR Apache-2.0
//! Gigabyte MJ11-EC1 (G431-MM0) and MJ11-EC0: EPYC Embedded 3000 (SP4r2) with
//! an AST2500 BMC.
//!
//! The flow is the board's: `src/bin/reset.rs`, from the PSP's reset vector
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

use fstart_driver_amd::{ccx::tsc::Tsc, fch::gpio::Gpio, soc::zeppelin::Die};
use fstart_driver_amd::{
    fch::{
        self, ACPI_PM1_ENABLE, ACPI_PM1_STATUS, Bus, Fch, Pm1Enable, Pm1Status,
        gpio::Control,
        iomux,
        lpc::{IoPortDecode, Lpc},
        taishan::irq::{self, Route},
    },
    smn::{apply, write},
    soc::zeppelin::{self as soc, Zeppelin},
};
use fstart_driver_superio::aspeed_ast2400::{self as ast, Ast2400, SuperIoConfig};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_pci::ConfigRegionAccess;
use fstart_pci::ecam::GlobalEcam;
use fstart_platform_amd::zeppelin::{CcxConfig, Config, DxioConfig, FchConfig};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

pub struct Board;

/// The BMC's diagnostic interrupt button.
const NMI_BUTTON_GEVENT: u8 = 11;
/// SW SMIs 0xC0 + Sx report the sleep state to the BMC (AMI BmcAcpi).
const BMC_SLEEP_NOTIFY: fstart_driver_ipmi::SleepNotify = fstart_driver_ipmi::SleepNotify {
    kcs_port: fstart_driver_ipmi::kcs::DEFAULT_PORT,
    kcs_spacing: fstart_driver_ipmi::kcs::DEFAULT_SPACING as u8,
    sw_smi_base: 0xc0,
};

const COM1: u16 = 0x3f8;
const COM1_IRQ: u8 = 4;

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
pub fn clear_pm1(fch: &Fch<impl Bus>) {
    fch.reg(ACPI_PM1_STATUS)
        .modify(Pm1Status::WAK::SET + Pm1Status::PCIEXP_WAKE::SET);
    fch.reg(ACPI_PM1_ENABLE)
        .modify(Pm1Enable::PCIEXP_WAKE_DIS::CLEAR);
}

register_bitfields![u32,
    /// The IOMUX bytes of pins 136-139 as one dword.
    Iomux136 [PIN137_BIT1 OFFSET(9) NUMBITS(1) [], PIN139_HIGH OFFSET(28) NUMBITS(4) []]
];
/// SMBusSwitch's GPIO137 and IOMUX settings.
const GPIO137: u8 = 137;

/// GbtPreBootSvid: the AST2500 VGA on the last bus behind 00:01.1 gets the
/// Gigabyte subsystem ID; config register 0x80 unlocks the write.
fn ast_vga_set_ssid(pci: &impl ConfigRegionAccess) {
    use fstart_pci::PciAddress;
    // SAFETY: the enumerated root port and the device behind it.
    unsafe {
        let bus = (pci.read(PciAddress::new(0, 0, 1, 1), 0x18) >> 16) as u8;
        let vga = PciAddress::new(0, bus, 0, 0);
        if bus == 0 || pci.read(vga, 0) != 0x2000_1a03 {
            return;
        }
        pci.write(vga, 0x80, 0xaa1a_03aa);
        pci.write(vga, 0x2c, CONFIG.subsystem_id);
        pci.write(vga, 0x80, 0);
    }
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

impl fstart_platform_amd::noncar::Board for Board {
    type Soc = Zeppelin;
    const CONFIG: &'static Config = &CONFIG;
}

/// LPC decodes and the AST2500 SuperIO routing for COM1, before the console.
pub fn early_init(lpc: &Lpc<impl Bus>) {
    lpc.enable_sio_2e();
    lpc.enable_io_decode(IoPortDecode::SERIAL0::SET);
    // AST2500 SIO runtime registers (0xa00-0xbff) and the IPMI KCS interface.
    let _ = lpc.set_wideio_range(0xa00, 512);
    let _ = lpc.set_wideio_range(0xca0, 16);
    let Ok(sio) = Ast2400::new_at_base(SuperIoConfig::default(), ast::CONFIG_PORT) else {
        return;
    };
    sio.route_uart1_pins();
    // SUART4 carries the BMC's IPMI SOL; the host must leave it alone.
    sio.disable(&[
        ast::SUART2,
        ast::SWC,
        ast::KBC,
        ast::GPIO,
        ast::SUART3,
        ast::SUART4,
        ast::MAILBOX,
    ]);
    sio.enable_uart(ast::SUART1, COM1, COM1_IRQ, 0);
}

/// COM1, routed to the AST2500's SUART1.
pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Pio { base: COM1 as u64 },
        clock_freq: 1_843_200,
        baud_rate: 115_200,
    }
}

/// The VRs and PCIe redrivers on I2C3 (the vendor board PEIMs, once the
/// FCH's SMBus is up): route the I2C3 pads, then program both. Returns the
/// PCIE_1 riser's ID: from its EEPROM on the same bus, else from the straps.
pub fn program_i2c_devices(gpio: &Gpio<impl Bus>, clock: Tsc) -> Option<u8> {
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

/// Wait for the BMC before its VGA is used.
pub fn wait_for_bmc(gpio: &Gpio<impl Bus>, clock: Tsc) {
    gpio::wait_for_bmc(gpio, &mut { clock });
}

/// The vendor GPIO table, after PCI enumeration.
pub fn program_vendor_gpios(gpio: &Gpio<impl Bus>) {
    if gpio.configure(gpio::VENDOR, &[]).is_err() {
        fstart_log::error!("Vendor GPIO table rejected");
    }
}

/// The AST2500 VGA's subsystem ID.
pub fn set_vga_ssid(die0: &Die<GlobalEcam>) {
    ast_vga_set_ssid(&die0.pci());
}

/// The vendor's SMBusSwitch at ReadyToBoot. Inferred purpose: hand the
/// SMBus to the OS. The vendor writes the IOMUX of pins 136-139 through SMN;
/// it is the same register as the ACPI MMIO one.
pub fn hand_smbus_to_os(gpio: &Gpio<impl Bus>, fch: &Fch<impl Bus>) {
    // Bit 1 of the GPIO137 IOMUX byte set, bits 7:4 of pin 139 cleared.
    fch.reg(iomux::at::<u32, Iomux136::Register>(136))
        .modify(Iomux136::PIN137_BIT1::SET + Iomux136::PIN139_HIGH.val(0));
    // GPIO137 driven low, keeping the drive strength and STS_EN bits.
    if let Ok(control) = gpio.control(GPIO137) {
        let old = control.extract();
        control.write(
            Control::DRIVE_STRENGTH.val(old.read(Control::DRIVE_STRENGTH))
                + Control::STATUS_ENABLE.val(old.read(Control::STATUS_ENABLE))
                + Control::OUTPUT_ENABLE::SET,
        );
    }
    clear_pm1(fch);
    // Pins 113-117 back to function 0 (SMBus0): two unaligned dword
    // writes, as in the vendor code.
    fch.reg(iomux::at::<u32, ()>(113)).set(0);
    fch.reg(iomux::at::<u32, ()>(114)).set(0);
}

/// What the board adds to the ACPI tables.
pub fn acpi() -> fstart_platform_amd::acpi::BoardAcpi {
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
