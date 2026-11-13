//! Low power subsystem: DMA, I2C 1-7, PWM, HS-UART and SPI (00:18.x and
//! 00:1e.x) (coreboot `lpss.c`).

use super::{AcpiModeDevice, Bar, Function};
use crate::MmioBar;
use crate::baytrail::iosf::{Iosf, LPSS};
use fstart_pci::ConfigRegionAccess;

const CTL_PM_CAP_PRSNT: u32 = 1 << 1;
const CTL_SNOOP: u32 = 1 << 18;
const CTL_NOSNOOP: u32 = 1 << 19;
const CTL_PCI_CFG_DIS: u32 = 1 << 20;
const CTL_ACPI_INT_EN: u32 = 1 << 21;

/// The LPSS functions.
pub const FUNCTIONS: [Function; 14] = [
    Function::SioDma1,
    Function::I2c(1),
    Function::I2c(2),
    Function::I2c(3),
    Function::I2c(4),
    Function::I2c(5),
    Function::I2c(6),
    Function::I2c(7),
    Function::SioDma2,
    Function::Pwm1,
    Function::Pwm2,
    Function::Hsuart1,
    Function::Hsuart2,
    Function::Spi,
];

/// The function's control register on the LPSS sideband port.
const fn ctl_reg(function: Function) -> Option<u32> {
    Some(match function {
        Function::SioDma1 => 0x280,
        Function::I2c(n @ 1..=7) => 0x288 + 8 * (n as u32 - 1),
        Function::SioDma2 => 0x240,
        Function::Pwm1 => 0x248,
        Function::Pwm2 => 0x250,
        Function::Hsuart1 => 0x258,
        Function::Hsuart2 => 0x260,
        Function::Spi => 0x268,
        _ => return None,
    })
}

/// Snoop and PM capability for every present function, I2C out of reset,
/// optionally ACPI mode; `record` receives each function switched to it.
pub fn init<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    acpi_mode: bool,
    mut record: impl FnMut(AcpiModeDevice),
) {
    for function in FUNCTIONS {
        let device = function.device();
        let Some(ctl) = ctl_reg(function) else {
            continue;
        };
        if !device.is_present() {
            continue;
        }
        iosf.modify(
            LPSS,
            ctl,
            CTL_SNOOP | CTL_NOSNOOP,
            CTL_SNOOP | CTL_PM_CAP_PRSNT,
        );
        if matches!(function, Function::I2c(_))
            && let Some(bar) = Bar::of(&device)
        {
            fstart_log::debug!("baytrail: releasing I2C from reset");
            bar.write32(0x804, 0x3);
        }
        if acpi_mode {
            let entry = AcpiModeDevice::capture(function, &device);
            // PCI interrupt off, memory and bus master on, then hide it.
            device.or16(0x04, 1 << 1 | 1 << 2 | 1 << 10);
            iosf.modify(LPSS, ctl, 0, CTL_PCI_CFG_DIS | CTL_ACPI_INT_EN);
            record(entry);
        }
    }
}
