//! The single QEMU stage: console, boot media from the device tree, PCI,
//! then the payload.

#![no_std]
#![no_main]

use fstart_driver_uart::ns16550::Ns16550;
use fstart_board_qemu_riscv64::{QEMU_RISCV64_VIRT, console};
use fstart_log::StaticConsole;
use fstart_platform_qemu::Virt;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_qemu::entry!(main, sbi_resume: resume);

fn main() -> ! {
    Virt::enter(&QEMU_RISCV64_VIRT, &CONSOLE, console())
        .boot_media()
        .scan_pci()
        .boot()
}

/// OpenSBI returns here before CrabEFI.
fn resume(hart_id: u64, dtb_addr: u64) -> ! {
    Virt::resume_sbi(&QEMU_RISCV64_VIRT, &CONSOLE, console(), hart_id, dtb_addr)
}
