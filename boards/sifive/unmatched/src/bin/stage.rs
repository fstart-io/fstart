//! HiFive Unmatched: the single stage, from the FU740 reset vector to the
//! payload.

#![no_std]
#![no_main]

use fstart_board_sifive_unmatched::{self as board, HIFIVE_UNMATCHED, HIFIVE_UNMATCHED_UART};
use fstart_driver_uart::sifive::SifiveUart;
use fstart_log::StaticConsole;
use fstart_platform_sifive::Fu740;

static CONSOLE: StaticConsole<SifiveUart> = StaticConsole::new();

fstart_platform_sifive::entry!(main, sbi_resume: resume);

fn main() -> ! {
    let fu740 = Fu740::enter(&HIFIVE_UNMATCHED);
    board::reset_ethernet_phy();
    fu740
        .enable_ethernet()
        .console(&CONSOLE, &HIFIVE_UNMATCHED_UART)
        .train_memory()
        .boot_media()
        .boot()
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
