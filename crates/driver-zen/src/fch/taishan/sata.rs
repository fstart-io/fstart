//! The per-die ZP SATA controller (SMN `0310_0000h` on the die's bus).

use super::super::{FchHw, Window, hide_nbif_function, update};
use crate::nbio::nbif::func_strap0;

const CONTROL: u32 = 0x0310_0000;
const PORT_CONTROL: u32 = 0x0310_0004;
const ABAR: u32 = 0x0310_1000;
/// NBIF1 device 0 function 2 strap: the die's SATA function (FCH_SP3 §3.2
/// af57, `1023_4400h`).
const NBIF_STRAP: u32 = 0x1020_0000 + func_strap0(0, 2);

/// PEI af47 for one die. Enabled: out of reset, all eight ports offline
/// (PxSCTL.DET = 4) until the OS/driver brings them up. Disabled: reset
/// held. The Naples donor defaults: die 0 off, die 2 on (S8026 lanes,
/// FCH_SP3 §8).
pub fn reset<H: FchHw>(hw: &H, enabled: bool) {
    let smn = hw.smn_window();
    (0..8).for_each(|i| update::<u32, _>(&smn, PORT_CONTROL, (2 << i) - 1, 0));
    if enabled {
        update::<u32, _>(&smn, CONTROL, 1, 0);
        (0..8).for_each(|n| update::<u32, _>(&smn, ABAR + 0x12C + 0x80 * n, 0xB, 0x4));
    } else {
        update::<u32, _>(&smn, CONTROL, 0, 0x1);
        update::<u32, _>(&smn, CONTROL, 0, 0x801);
    }
}

/// DXE af57 for a disabled die: hide the SATA function (NBIF1 strap
/// bit 28). The enabled-die env path was not reached at the donor default
/// (FCH_SP3 §4.1: "full SATA env + xGbE (sp4r2 §4)"); an enabled die keeps
/// its strap and reset state here.
pub fn env<H: FchHw>(hw: &H, enabled: bool) {
    if !enabled {
        hide_nbif_function(&hw.smn_window(), NBIF_STRAP);
    }
}

/// Whether a die's controller is out of reset.
pub fn is_enabled<H: FchHw>(hw: &H) -> bool {
    hw.smn_window().read32(CONTROL) & 1 == 0
}

#[cfg(test)]
mod tests {
    use super::super::super::model::{fch, smn};
    use super::*;

    #[test]
    fn die_enable_and_disable() {
        let mut hw = fch();
        hw.bus = 0x40;
        reset(&hw, true);
        assert_eq!(smn(&hw.pci, 0x40, 0x0310_14ac), 4);
        assert!(is_enabled(&hw));
        hw.bus = 0x60;
        reset(&hw, false);
        env(&hw, false);
        assert_eq!(smn(&hw.pci, 0x60, CONTROL), 0x801);
    }
}
