//! The single QEMU stage: console, boot media from the device tree, PCI,
//! then the payload.

#![no_std]
#![no_main]

use fstart_driver_uart::pl011::Pl011;
use fstart_board_qemu_armv7::{QEMU_ARMV7_VIRT, console};
use fstart_log::StaticConsole;
use fstart_platform_qemu::Virt;

static CONSOLE: StaticConsole<Pl011> = StaticConsole::new();

fstart_platform_qemu::entry!(main);

fn main() -> ! {
    Virt::enter(&QEMU_ARMV7_VIRT, &CONSOLE, console())
        .boot_media()
        .scan_pci()
        .boot()
}
