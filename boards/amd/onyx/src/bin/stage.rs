//! The Onyx stage: from the PSP's release of the BSP to the payload, on
//! Genoa.

#![no_std]
#![no_main]

use fstart_board_amd_onyx::{self as onyx, Board};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::flow::Cpu;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    let id = match fstart_platform_amd::flow::start::<Board>() {
        Cpu::Genoa(id) => id,
        cpu => cpu.unsupported(),
    };
    let stage = id.enter(onyx::config());
    if onyx::early_init(&stage.hw().fch, stage.hw().clock).is_err() {
        stage.fail("board early init");
    }
    stage
        .console(&CONSOLE, onyx::CONSOLE)
        .init_fch()
        .train_dxio(&onyx::wiring::PCIE)
        .init_chipset(())
        .start_cpus()
        .scan_bus()
        .ready_to_boot()
        .emit_tables(&onyx::smbios_identity(), onyx::acpi_board())
        .boot()
}
