//! HiFive Unmatched binding for the handwritten FU740 flow.

use fstart_driver_uart::sifive::SifiveUartConfig;
use crate::config::Fu740Config;
use crate::fu740::{Fu740, Fu740Board, Fu740BuildSelectedPayload, Fu740Hooks};
use fstart_stage::{StageBoard, StageEnvironment};

use crate::Board;

impl StageBoard for Board {
    const NAME: &'static str = crate::BOARD_NAME;
    const PLATFORM: fstart_core::Platform = crate::PLATFORM;

    fn run_stage(env: StageEnvironment, handoff: usize) -> ! {
        Fu740::run_stage::<Self>(env, handoff)
    }

    #[cfg(feature = "crabefi")]
    fn resume_sbi(hart_id: u64, dtb_addr: u64) -> ! {
        Fu740::resume_sbi::<Self>(hart_id, dtb_addr)
    }
}

/// Board-specific seams for the fixed FU740 flow.
#[derive(Default)]
pub struct HiFiveUnmatchedHooks;

impl Fu740Hooks for HiFiveUnmatchedHooks {}

impl Fu740Board for Board {
    type Hooks = HiFiveUnmatchedHooks;
    type Payload = Fu740BuildSelectedPayload;

    const CONFIG: &'static Fu740Config = &crate::HIFIVE_UNMATCHED;
    const CONSOLE_CONFIG: &'static SifiveUartConfig = &crate::HIFIVE_UNMATCHED_UART;
}
