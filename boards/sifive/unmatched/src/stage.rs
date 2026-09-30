//! HiFive Unmatched binding for the handwritten FU740 flow.

use crate::config::Fu740Config;
use crate::fu740::{Fu740Board, Fu740Hooks};
use fstart_driver_uart::sifive::SifiveUartConfig;

use crate::Board;

/// Board-specific seams for the fixed FU740 flow.
#[derive(Default)]
pub struct HiFiveUnmatchedHooks;

impl Fu740Hooks for HiFiveUnmatchedHooks {}

impl Fu740Board for Board {
    type Hooks = HiFiveUnmatchedHooks;

    const CONFIG: &'static Fu740Config = &crate::HIFIVE_UNMATCHED;
    const CONSOLE_CONFIG: &'static SifiveUartConfig = &crate::HIFIVE_UNMATCHED_UART;
}
