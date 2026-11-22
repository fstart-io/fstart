//! The S8040 stage: from the PSP's release of the BSP to the payload, on
//! Siena.

#![no_std]
#![no_main]

use fstart_board_tyan_s8040::{self as s8040, Board, bmc};
use fstart_driver_aspeed::gfx;
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
    let stage = stage.init_fch().train_dxio(&ports).init_chipset(());
    // Read BMC FRU before `start_cpus()` installs SMM so SMM RAS SEL reports
    // cannot interleave with boot-time FRU queries on KCS2.
    let fru = bmc::read_fru(stage.clock());
    match &fru {
        Some(fru) => fstart_log::info!(
            "s8040: BMC FRU board {} product {}, UUID {}",
            fru.board_serial.as_str(),
            fru.product_serial.as_str(),
            if fru.system_uuid.is_some() {
                "yes"
            } else {
                "no"
            }
        ),
        None => fstart_log::warn!("s8040: BMC FRU unavailable; using static SMBIOS identity"),
    }
    let mut stage = stage.start_cpus().scan_bus();
    if pcie.vga {
        if let Some(address) = stage.configured_display() {
            // SAFETY: BSP only, after allocation of the onboard display's
            // 32-bit BARs in the identity map. The BMC retains its DRAM.
            match unsafe { gfx::initialize(stage.pci(), address, &stage.clock()) } {
                Ok(framebuffer) => stage.set_framebuffer(framebuffer),
                Err(error) => {
                    let reason = match error {
                        gfx::Error::Device => "unsupported device",
                        gfx::Error::Resources => "invalid BAR or VRAM",
                        gfx::Error::BmcDramNotReady => "BMC DRAM not ready",
                    };
                    fstart_log::warn!("s8040: native VGA unavailable ({}); keeping serial", reason);
                }
            }
        } else {
            fstart_log::warn!("s8040: onboard VGA was not enumerated; keeping serial");
        }
    }
    stage
        .ready_to_boot()
        .emit_tables(&s8040::smbios_identity(fru.as_ref()))
        .boot()
}
