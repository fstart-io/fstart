// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vilboz reset image: the PSP trained DRAM and released the BSP here; one
//! stage runs to the payload.

#![no_std]
#![no_main]

use fstart_board_google_vilboz::{self as vilboz, CONFIG};
use fstart_driver_amd::soc::picasso::Picasso;
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::noncar::Reset;
use fstart_platform_amd::picasso::Init;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    let stage = Reset::<Picasso>::enter(&CONFIG).console(&CONSOLE, vilboz::console());
    let init = Init::start(&stage);
    let gpio = init.gpio();
    if vilboz::early_gpios(&gpio).is_err() {
        init.fail("board GPIOs");
    }
    // GPIO67 (DMIC_SEL) in the RAM-stage pads needs ROM sharing off.
    init.disable_rom_sharing();
    let cbi = vilboz::after_fch(&gpio, &mut init.clock());
    let mut silicon = init
        .train_dxio()
        .finish(cbi.native_power(), cbi.has_emmc());
    let pci = stage.enumerate(&mut silicon);
    stage.ready_to_boot(&mut silicon, &pci);
    stage.boot(silicon, pci, vilboz::acpi(), &vilboz::SMBIOS)
}
