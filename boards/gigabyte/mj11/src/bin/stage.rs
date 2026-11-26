// SPDX-License-Identifier: MIT OR Apache-2.0
//! MJ11 stage: the PSP trained DRAM, copied this image and released the
//! BSP here; it runs to the payload.

#![no_std]
#![no_main]

use fstart_board_gigabyte_mj11::{self as mj11, Board};
use fstart_driver_aspeed::gfx;
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::flow::Cpu;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    fstart_core::task::run(|spawner| async move {
        let stage = match fstart_platform_amd::flow::start::<Board>() {
            Cpu::Zeppelin(id) => id.enter(mj11::CONFIG),
            cpu => cpu.unsupported(),
        };
        mj11::early_init(&stage.hw().fch);
        let stage = stage.console(&CONSOLE, mj11::console()).init_fch();
        fstart_timestamp::add(fstart_timestamp::id::BOARD_EARLY_BUS);
        let riser = mj11::program_i2c_devices(&stage.hw().gpio(), stage.clock());
        mj11::clear_pm1(&stage.hw().fch);
        // If the BMC is already ready, spawn its IPMI KCS POST-done sequence
        // so it overlaps with SMU DXIO training (~1040 ms of SMU waits). If
        // the BMC is still booting (simultaneous AC power-on), defer until
        // after `wait_for_bmc()`.
        let clock = stage.clock();
        let bmc_post = mj11::bmc_ready(&stage.hw().gpio()).then(|| {
            // SAFETY: KCS ports are decoded in early_init; BSP-only executor.
            spawner.spawn(unsafe { mj11::bmc::run_post_done(clock) })
        });
        let stage = stage.train_dxio_async(&mj11::dxio(riser)).await;
        fstart_timestamp::add(fstart_timestamp::id::BOARD_POST_DXIO);
        mj11::apply_phy_tuning(&stage.hw().dies[0]);
        mj11::wait_for_bmc(&stage.hw().gpio(), stage.clock());
        let mut stage = stage.init_chipset(()).start_cpus().scan_bus();
        fstart_timestamp::add(fstart_timestamp::id::BOARD_POST_SCAN);
        mj11::program_vendor_gpios(&stage.hw().gpio());
        mj11::set_vga_ssid(&stage.hw().dies[0]);
        // The board waited for its BMC above; use its initialized shared DRAM.
        // Native modesetting must not execute a VGA ROM or reset the BMC.
        fstart_timestamp::add(fstart_timestamp::id::DISPLAY_INIT);
        let pci = stage.pci();
        let address = pci
            .devices()
            .find(|&a| {
                pci.device(a).is_some_and(|d| {
                    d.vendor_id() == gfx::ASPEED_VENDOR_ID
                        && d.device_id() == gfx::AST_VGA_DEVICE_ID
                })
            })
            .unwrap_or_else(|| stage.fail("ASPEED VGA device"));
        // SAFETY: BSP exclusively owns the enumerated GPU and its allocated,
        // identity-mapped BARs; no payload display driver is running yet.
        let framebuffer =
            unsafe { gfx::initialize(pci, address, &stage.clock()) }.unwrap_or_else(|error| {
                fstart_log::error!(
                    "mj11: ASPEED display error {}",
                    match error {
                        gfx::Error::Device => "device",
                        gfx::Error::Resources => "resources",
                        gfx::Error::BmcDramNotReady => "BMC DRAM not ready",
                    }
                );
                stage.fail("ASPEED display")
            });
        stage.set_framebuffer(framebuffer);
        fstart_timestamp::add(fstart_timestamp::id::BOARD_BMC_POST_DONE);
        let bmc_done = match bmc_post {
            Some(handle) => handle.await,
            // SAFETY: KCS ports are decoded in early_init; BMC is ready now.
            None => unsafe { mj11::bmc::run_post_done(stage.clock()).await },
        };
        mj11::bmc::apply_post_done(&stage.hw().fch, bmc_done);
        let stage = stage.ready_to_boot();
        mj11::hand_dimm_smbus_to_bmc(&stage.hw().gpio(), &stage.hw().fch);
        stage.emit_tables(&mj11::SMBIOS).boot()
    })
}
