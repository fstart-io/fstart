//! CoM260 kit SRAM bootblock: the console on UART0, then the boot report.

#![no_std]
#![no_main]

use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_spacemit::k3;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_spacemit::entry!(main);

fn main() -> ! {
    k3::bootblock(&CONSOLE, k3::uart0_console())
}
