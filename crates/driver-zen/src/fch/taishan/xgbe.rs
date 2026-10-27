//! ZP xGbE (four 10GbE ports per die), off on the SP3 donors (FCH_SP3 §3.2
//! af49, §4.1). Only the disable path is recovered: the NBIF strap of each
//! port's function is cleared and `1631F030 + 4n` bit 0 set.

use super::super::{FchHw, hide_nbif_function, update};
use crate::nbio::nbif::func_strap0;

pub const PORTS: u32 = 4;

fn each(ports: u8) -> impl Iterator<Item = u32> {
    (0..PORTS).filter(move |n| ports & (1 << n) != 0)
}

/// Hide xGbE ports `ports` (bit n = port n): clear their NBIF strap. This
/// is all the secondary-die env does (FchMultiFchDxe).
pub fn hide<H: FchHw>(hw: &H, ports: u8) {
    let smn = hw.smn_window();
    // NBIF1 device 0 functions 4..7 (`1023_4800h` + 200h·n).
    each(ports).for_each(|n| hide_nbif_function(&smn, 0x1020_0000 + func_strap0(0, 4 + n as u8)));
}

/// The af49 disable path: hide, then set `1631F030 + 4n` bit 0.
pub fn disable<H: FchHw>(hw: &H, ports: u8) {
    hide(hw, ports);
    let smn = hw.smn_window();
    each(ports).for_each(|n| update::<u32, _>(&smn, 0x1631_F030 + 4 * n, 0, 1));
}
