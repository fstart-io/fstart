//! The S8040 stage: from the PSP's release of the BSP to the payload, on
//! Siena.

#![no_std]
#![no_main]

use fstart_board_tyan_s8040::{self as s8040, Board, bmc};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::flow::Cpu;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    let id = match fstart_platform_amd::flow::start::<Board>() {
        Cpu::Siena(id) => id,
        cpu => cpu.unsupported(),
    };
    let stage = id.enter(s8040::config());
    bmc::early_init(&stage.hw().fch);
    let mut stage = stage.console(&CONSOLE, bmc::CONSOLE);
    let pcie = stage.resolve_set(&s8040::options::PCIE, s8040::check_options);
    let power = stage.resolve_set(&s8040::options::POWER, |_| Ok(()));
    s8040::configure_pcie(stage.config_mut(), &pcie);
    fstart_driver_amd::fch::tasks::env::restore_ac_power(&stage.hw().fch, power);
    let ports = s8040::mpio_ports(&pcie).unwrap_or_else(|_| stage.fail("board MPIO ports"));
    stage
        .init_fch()
        .train_dxio(&ports)
        .init_chipset(())
        .start_cpus()
        .scan_bus()
        .ready_to_boot()
        .emit_tables(&s8040::smbios_identity(), bmc::acpi_board())
        .boot()
}
