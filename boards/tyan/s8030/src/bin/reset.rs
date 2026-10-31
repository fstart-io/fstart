//! The S8030 reset image: from the PSP's release of the BSP to the payload.
//! Rome and Milan run the same flow with the SP3 silicon steps.

#![no_std]
#![no_main]

use fstart_board_tyan_s8030::{Board, bmc, platform};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_sp3::{Identified, Silicon, Start, Tables};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_sp3::entry!(main);

fn main() -> ! {
    let Start::RomeMilan(id) = fstart_platform_sp3::start::<Board>();
    flow(id)
}

fn flow<S: Silicon>(id: Identified<Board, S>) -> ! {
    let generation = id.generation();
    let reset = id.reset(platform::policy(generation));
    if platform::early_init(reset.fch()).is_err() {
        reset.fail("board early init");
    }
    let stage = reset.console(&CONSOLE, platform::CONSOLE);
    let levels = stage.read_pads(platform::detect_pads(generation));
    let stage = stage.program_gpio(&platform::gpio(generation));
    let engines = platform::dxio_engines(generation, &levels, stage.fch(), stage.timer())
        .unwrap_or_else(|_| stage.fail("board DXIO devices"));
    let lpc_aml = bmc::lpc_aml();
    stage.finish(
        &engines,
        Tables {
            smbios: platform::smbios_identity(),
            acpi: bmc::acpi_board(&lpc_aml),
        },
    )
}
