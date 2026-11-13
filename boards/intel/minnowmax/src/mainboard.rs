//! MinnowBoard MAX board functions: the console, the BOM straps that pick
//! the memory-down SPD, and identity.

use fstart_driver_intel::baytrail::BaytrailNorth;
use fstart_driver_intel::baytrail::gpio::Bank;
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};

/// The PCU's legacy COM1.
pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Pio {
            base: crate::UART0_PIO_BASE as u64,
        },
        clock_freq: crate::UART0_CLOCK_FREQ,
        baud_rate: crate::UART0_BAUD_RATE,
    }
}

/// Suspend-well pads of GPIO_S5[4..7], the BOM option straps.
const BOM_PADS: [usize; 4] = [32, 34, 36, 35];

/// The four BOM straps: S5_5, S5_6 and S5_7 are bits 0-2, S5_4 is bit 3.
pub fn bom_id() -> u8 {
    for pad in BOM_PADS {
        Bank::Ssus.strap_input(pad);
    }
    let [s5_4, s5_5, s5_6, s5_7] = BOM_PADS.map(|pad| u8::from(Bank::Ssus.level(pad)));
    s5_5 | s5_6 << 1 | s5_7 << 2 | s5_4 << 3
}

/// Hand raminit the SPD of the memory this BOM populates: bit 0 set means
/// 2 GiB (or 4 GiB), clear means 1 GiB.
pub fn select_spd(north: &mut BaytrailNorth) {
    let bom = bom_id();
    let two_gib = bom & 1 != 0;
    fstart_log::info!(
        "minnowmax: BOM ID {:#x}, {} GiB memory-down SPD",
        bom,
        if two_gib { 2 } else { 1 }
    );
    north.set_spd(if two_gib {
        &crate::spd::SPD_2GIB
    } else {
        &crate::spd::SPD_1GIB
    });
}

/// No board devices in the namespace beyond the SoC's.
#[cfg(feature = "acpi")]
pub struct Acpi;

#[cfg(feature = "acpi")]
impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = ();
    fn dsdt_aml(&self, _config: &Self::Config) -> alloc::vec::Vec<u8> {
        alloc::vec::Vec::new()
    }
}

#[cfg(feature = "acpi")]
pub static MINNOWMAX_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "Intel",
        sys_product: "MinnowBoard MAX",
        sys_version: "",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "Intel",
        bb_product: "MinnowBoard MAX",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x03,
        chassis_manufacturer: "Intel",
        processor_sockets: &["FCBGA1170"],
        oem_string: None,
    };
