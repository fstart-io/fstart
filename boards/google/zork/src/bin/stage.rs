// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vilboz stage: the PSP trained DRAM, copied this image and released the
//! BSP here; it runs to the payload.

#![no_std]
#![no_main]

use fstart_board_google_vilboz::{self as vilboz, Board};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::flow::Cpu;
use fstart_platform_amd::picasso::Runtime;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    let stage = match fstart_platform_amd::flow::start::<Board>() {
        Cpu::Picasso(id) => id.enter(vilboz::CONFIG),
        cpu => cpu.unsupported(),
    };
    let stage = stage.console(&CONSOLE, vilboz::console()).init_fch();
    let gpio = stage.hw().gpio();
    if vilboz::early_gpios(&gpio).is_err() {
        stage.fail("board GPIOs");
    }
    // GPIO67 (DMIC_SEL) in the RAM-stage pads needs ROM sharing off.
    stage.disable_rom_sharing();
    let cbi = vilboz::after_fch(&gpio, &stage.clock());
    stage
        .train_dxio(())
        .init_chipset(Runtime {
            power: cbi.native_power(),
            emmc_fitted: cbi.has_emmc(),
        })
        .start_cpus()
        .scan_bus()
        .ready_to_boot()
        .emit_tables(&vilboz::SMBIOS, vilboz::acpi())
        .boot()
}
