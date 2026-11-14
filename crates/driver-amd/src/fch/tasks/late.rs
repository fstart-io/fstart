//! The late (ReadyToBoot) HW ACPI task af70 (FCH_SP3 §3.3); also rerun by
//! SW SMI b4 after S3.

use crate::cpu::Silicon;
use crate::fch::acpimmio::{PM, PM_0B, PM_RESET_CONTROL1};
use crate::fch::irq;
use crate::fch::smi::{self, Route, source};
use crate::fch::{FchHw, uart};
use crate::window::update;

/// af70. `extra_routes` are board PCI_INTR overrides written after the
/// generation's table (FCH_SP3 §9: the F0–F9 I2C/UART routes are board
/// policy).
pub fn hw_acpi<H: FchHw>(hw: &H, generation: Silicon, extra_routes: &[(u8, u8)]) {
    let acpi = hw.acpi();
    // PMxA0 MsgCState: C-state message bits off.
    update::<u32, _>(acpi, PM + 0xA0, 0x3F80_FF5F, 0);
    // SLP_TYP write raises an SMI (the sleep handler, [`super::sx`]).
    smi::set_route(acpi, source::SLP_TYP, Route::Disabled);
    smi::set_route(acpi, source::SLP_TYP, Route::Smi);
    // ResetCtl1[SlpTypeControl] off until Sx entry; PMx0B[1:0] = 2.
    update::<u8, _>(acpi, PM_RESET_CONTROL1.offset, 0x20, 0);
    update::<u8, _>(acpi, PM_0B.offset, 0x03, 0);
    update::<u8, _>(acpi, PM_0B.offset, 0x01, 0x02);
    irq::program(hw.io(), irq::routes(generation));
    irq::program(hw.io(), extra_routes);
    // SCI level for GEVENTs 11 (ZP, SSP) and 19.
    let level = match generation {
        Silicon::Gn => 0x0008_0000,
        Silicon::Zp | Silicon::Ssp => 0x0008_0800,
    };
    update::<u32, _>(acpi, smi::SCI_LEVEL.offset, 0, level);
    if generation != Silicon::Zp {
        uart::set_legacy_decode(hw, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::fch;

    #[test]
    fn rome_late() {
        let hw = fch();
        hw_acpi(&hw, Silicon::Ssp, &[(0xf0, 0x0b)]);
        assert_eq!(hw.acpi.get(0x2b0, 4), 0x4);
        assert_eq!(hw.acpi.get(PM + 0x0b, 1), 0x2);
        assert_eq!(hw.io.at(0xc01, 0xf9), 0x0c);
        assert_eq!(hw.io.at(0xc01, 0xf0), 0x0b, "board override last");
        assert_eq!(hw.acpi.get(0x20c, 4), 0x80800);
        assert_eq!(hw.mmio.writes(), vec![(0xfedc_0020, 2, 0)]);
    }
}
