//! The Ethanol-X stage: from the PSP's release of the BSP to the
//! payload, on Milan with the SP3 silicon steps.

#![no_std]
#![no_main]

use fstart_board_amd_ethanolx::{self as ethanolx, Board};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::sp3::{Identified, Silicon, Start};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    let Start::RomeMilan(id) = fstart_platform_amd::sp3::start::<Board>();
    flow(id)
}

fn flow<F: Silicon>(id: Identified<Board, F>) -> ! {
    let generation = id.generation();
    let stage = id.init_fch(ethanolx::policy(generation));
    if ethanolx::early_init(stage.fch()).is_err() {
        stage.fail("board early init");
    }
    let mut stage = stage.console(&CONSOLE, ethanolx::CONSOLE).init_fch();
    stage.program_gpio(&ethanolx::GPIO_PROGRAM);
    let mut layout = ethanolx::dxio_layout(stage.fch(), stage.timer())
        .unwrap_or_else(|_| stage.fail("board DXIO devices"));
    stage.resolve_options(&ethanolx::dxio::KNOBS, &mut layout, ethanolx::dxio::check);
    let engines = ethanolx::dxio_engines(&layout, stage.timer())
        .unwrap_or_else(|_| stage.fail("board DXIO devices"));
    stage
        .train_dxio(&engines)
        .init_chipset(())
        .start_cpus()
        .scan_bus()
        .ready_to_boot()
        .emit_tables(&ethanolx::smbios_identity(), ethanolx::acpi_board())
        .boot()
}
