//! HiFive Unmatched DRAM mainstage: take the bootblock's handoff, then
//! boot the payload.

#![no_std]
#![no_main]

use fstart_board_sifive_unmatched::{HIFIVE_UNMATCHED, HIFIVE_UNMATCHED_UART};
use fstart_driver_uart::sifive::SifiveUart;
use fstart_log::StaticConsole;

static CONSOLE: StaticConsole<SifiveUart> = StaticConsole::new();

fstart_platform_sifive::entry!(main, handoff, sbi_resume: resume);

fn main(handoff: usize) -> ! {
    fstart_platform_sifive::main_stage(&HIFIVE_UNMATCHED, &CONSOLE, &HIFIVE_UNMATCHED_UART, handoff)
}

/// OpenSBI returns here before CrabEFI.
fn resume(hart_id: u64, dtb_addr: u64) -> ! {
    fstart_platform_sifive::resume_sbi(
        &HIFIVE_UNMATCHED,
        &CONSOLE,
        &HIFIVE_UNMATCHED_UART,
        hart_id,
        dtb_addr,
    )
}
