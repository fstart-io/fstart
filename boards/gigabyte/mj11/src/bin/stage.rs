// SPDX-License-Identifier: MIT OR Apache-2.0
//! MJ11 stage: the PSP trained DRAM, copied this image and released the
//! BSP here; it runs to the payload.

#![no_std]
#![no_main]

use fstart_board_gigabyte_mj11::{self as mj11, Board, bmc};
use fstart_driver_aspeed::gfx;
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::flow::Cpu;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    let stage = match fstart_platform_amd::flow::start::<Board>() {
        Cpu::Zeppelin(id) => id.enter(mj11::CONFIG),
        cpu => cpu.unsupported(),
    };
    bmc::early_init(&stage.hw().fch);
    let stage = stage.console(&CONSOLE, bmc::CONSOLE).init_fch();
    let riser = mj11::program_i2c_devices(&stage.hw().gpio(), stage.clock());
    mj11::clear_pm1(&stage.hw().fch);
    let stage = stage.train_dxio(&mj11::dxio(riser));
    mj11::apply_phy_tuning(&stage.hw().dies[0]);
    bmc::wait_ready(&stage.hw().gpio(), stage.clock());
    let mut stage = stage.init_chipset(()).start_cpus().scan_bus();
    mj11::program_vendor_gpios(&stage.hw().gpio());
    bmc::set_vga_ssid(&stage.hw().dies[0]);
    // The board waited for its BMC above; use its initialized shared DRAM.
    // Native modesetting must not execute a VGA ROM or reset the BMC.
    let address = stage
        .configured_display()
        .unwrap_or_else(|| stage.fail("ASPEED VGA device"));
    // SAFETY: BSP exclusively owns the enumerated GPU and its allocated,
    // identity-mapped BARs; no payload display driver is running yet.
    let framebuffer = unsafe { gfx::initialize(stage.pci(), address, &stage.clock()) }
        .unwrap_or_else(|error| {
            fstart_log::error!("mj11: ASPEED display error ({})", error.name());
            stage.fail("ASPEED display")
        });
    stage.set_framebuffer(framebuffer);
    bmc::post_done(&stage.hw().fch, stage.clock());
    let stage = stage.ready_to_boot();
    bmc::hand_dimm_smbus_to_bmc(&stage.hw().gpio(), &stage.hw().fch);
    stage.emit_tables(&mj11::SMBIOS).boot()
}
