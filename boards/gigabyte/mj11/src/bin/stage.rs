// SPDX-License-Identifier: MIT OR Apache-2.0
//! MJ11 stage: the PSP trained DRAM, copied this image and released the
//! BSP here; it runs to the payload.

#![no_std]
#![no_main]

use fstart_board_gigabyte_mj11::{self as mj11, Board};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    let stage = fstart_platform_amd::noncar::start::<Board>();
    mj11::early_init(&stage.hw().lpc());
    let stage = stage.console(&CONSOLE, mj11::console()).init_fch();
    let riser = mj11::program_i2c_devices(&stage.hw().gpio(), stage.clock());
    mj11::clear_pm1(&stage.hw().fch);
    let stage = stage.train_dxio(&mj11::dxio(riser));
    mj11::apply_phy_tuning(&stage.hw().dies[0]);
    mj11::wait_for_bmc(&stage.hw().gpio(), stage.clock());
    let stage = stage.init_chipset(()).start_cpus().scan_bus();
    mj11::program_vendor_gpios(&stage.hw().gpio());
    mj11::set_vga_ssid(&stage.hw().dies[0]);
    mj11::bmc::post_done(&stage.hw().fch, stage.clock());
    let stage = stage.ready_to_boot();
    mj11::hand_dimm_smbus_to_bmc(&stage.hw().gpio(), &stage.hw().fch);
    stage.emit_tables(&mj11::SMBIOS, mj11::acpi()).boot()
}
