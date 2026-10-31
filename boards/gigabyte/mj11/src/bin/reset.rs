// SPDX-License-Identifier: MIT OR Apache-2.0
//! MJ11 reset image: the PSP trained DRAM and released the BSP here; one
//! stage runs to the payload.

#![no_std]
#![no_main]

use fstart_board_gigabyte_mj11::{self as mj11, CONFIG};
use fstart_driver_amd::soc::zeppelin::Zeppelin;
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::noncar::Reset;
use fstart_platform_amd::zeppelin::Init;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    let reset = Reset::<Zeppelin>::enter(&CONFIG);
    mj11::early_init(&reset.lpc());
    let stage = reset.console(&CONSOLE, mj11::console());
    let init = Init::start(&stage);
    let ctx = init.ctx();
    let gpio = ctx.gpio();
    let riser = mj11::program_i2c_devices(&gpio, ctx.clock);
    mj11::clear_pm1(ctx.fch);
    let dxio = init.train_dxio(&mj11::dxio(riser));
    mj11::apply_phy_tuning(&ctx.dies[0]);
    mj11::wait_for_bmc(&gpio, ctx.clock);
    let mut silicon = dxio.finish();
    let pci = stage.enumerate(&mut silicon);
    mj11::program_vendor_gpios(&gpio);
    mj11::set_vga_ssid(&ctx.dies[0]);
    mj11::bmc::post_done(ctx.fch, ctx.clock);
    stage.ready_to_boot(&mut silicon, &pci);
    mj11::hand_smbus_to_os(&gpio, ctx.fch);
    stage.boot(silicon, pci, mj11::acpi(), &mj11::SMBIOS)
}
