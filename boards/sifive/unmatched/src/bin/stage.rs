//! HiFive Unmatched: the single stage, from the FU740 reset vector to the
//! payload.

#![no_std]
#![no_main]

use fstart_board_sifive_unmatched::{HIFIVE_UNMATCHED, HIFIVE_UNMATCHED_UART, fu740};

fstart_platform_qemu::entry!(main, sbi_resume: resume);

fn main() -> ! {
    fu740::run(&HIFIVE_UNMATCHED, &HIFIVE_UNMATCHED_UART)
}

/// OpenSBI returns here before CrabEFI.
fn resume(hart_id: u64, dtb_addr: u64) -> ! {
    fu740::resume_sbi(&HIFIVE_UNMATCHED, &HIFIVE_UNMATCHED_UART, hart_id, dtb_addr)
}
