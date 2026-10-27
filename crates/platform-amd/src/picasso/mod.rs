// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native AMD Picasso on the noncar flow.
//!
//! The flow reaches a checkpoint after TP1's early part: the data fabric,
//! NBIO and CPU early init, the early FCH setup and the
//! board's `fch_reset_done`. It halts there:
//! MP, SMM, PCI, ACPI and the payload aren't ported yet, so Picasso's
//! [`noncar::Soc::State`] is uninhabited.
use crate::noncar::{
    self, Board, Ctx, Silicon, Stop, Tp1,
    layout::{Layout, Region},
};
use core::convert::Infallible;
use fstart_arch::x86::cpu::msr::{MCG_CAP, McgCap, MsrAccess};
use fstart_driver_amd::{
    ccx::{
        Thread,
        picasso::{self as cpu, Identity},
        tsc::{Clock, Tsc},
    },
    df::picasso as fabric,
    fch::{
        Fch, NativeBus, espi,
        gpio::{Gpio, Pad, Pull},
        sandstone::{
            legacy,
            preinit::{self, SpiConfig},
        },
    },
    nbio::picasso as nbio,
    soc::{
        Soc as _,
        picasso::{self as soc, EARLY_AOAC, Picasso, UART_AOAC, UART_BASES},
    },
};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_log::Hex;
use tock_registers::interfaces::Readable;

/// A board's Picasso configuration.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub fch: FchConfig,
}

/// The Sandstone FCH setup before and right after the console.
#[derive(Debug, Clone, Copy)]
pub struct FchConfig {
    /// The FCH UART of the console (0-3).
    pub console_uart: u8,
    /// SPI ROM read mode and speeds.
    pub spi: SpiConfig,
    /// eSPI link to the EC and its pads.
    pub espi: Option<(espi::Config, &'static [Pad])>,
    /// GPIO tables programmed once the FCH is up, in order.
    pub early_gpios: &'static [&'static [Pad]],
    /// `DISABLE_SPI_FLASH_ROM_SHARING`: before the board's `fch_reset_done`.
    pub disable_rom_sharing: bool,
}

/// The console on FCH UART `uart`.
pub const fn console(uart: u8) -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Mmio {
            base: UART_BASES[uart as usize] as u64,
            reg_shift: 2,
            reg_width: 4,
        },
        clock_freq: 48_000_000,
        baud_rate: 115200,
    }
}

/// The reset image in DRAM. The APOB (`0x02001000`), the PSP shared memory
/// of a PSP verstage (`0x02011000..0x02019000`) and the image are fixed by
/// the PSP directories.
pub const LAYOUT: Layout = Layout {
    early: Region::new(0x0200_0000, 0x30_0000),
    page_tables: 0x0202_0000,
    data: Region::new(0x0203_0000, 0x6_0000),
    heap: Region::new(0x0209_0000, 0x4_0000),
    stack: Region::new(0x020d_0000, 0x3_0000),
    image: Region::new(0x0210_0000, 0x20_0000),
    flash: Region::new(0xff00_0000, 0x100_0000),
    ffs: None,
}
.check();

#[cfg(feature = "host")]
impl noncar::host::HostSoc for Picasso {
    const POLICY: noncar::host::Policy = noncar::host::Policy {
        feature: "bundle-picasso",
        payloads: false,
        smm: false,
        release_only: true,
        image_config: noncar::host::ImageConfig::Packed,
    };
    fn check(config: &Config) -> Result<(), &'static str> {
        (config.fch.console_uart < UART_BASES.len() as u8)
            .then_some(())
            .ok_or("Picasso console UART index exceeds hardware table")
    }
}

impl noncar::Soc for Picasso {
    const NAME: &'static str = "picasso";
    const CPU_NAME: &'static str = "native-picasso";
    const LAYOUT: Layout = LAYOUT;
    const ECAM: u64 = soc::ECAM;
    const ECAM_BUSES: u16 = soc::ECAM_BUSES as u16;
    /// BIOS directory entry 0x61.
    const APOB: Region = Region::new(0x0200_1000, 0x1_0000);
    const BIOS_DIRECTORY: fstart_amdfw_format::efs::BiosDirectorySlot =
        fstart_amdfw_format::efs::BiosDirectorySlot::Family17hModel10;
    /// 8 MiB of TSEG.
    const TSEG_SIZE: u32 = 8 << 20;
    /// The topology decoder's envelope: 2 CCXs, 4 cores, 2 threads.
    const MAX_CPUS: u16 = 16;
    type Config = Config;
    type Identity = Identity;
    type State = Infallible;

    /// The console: ACPI MMIO decode, legacy decode, AOAC power of the
    /// early devices and the console UART, its pads.
    fn fch_pre_init(
        fch: &Fch<NativeBus>,
        config: &Config,
        clock: Option<Tsc>,
    ) -> Result<(), &'static str> {
        let mut delay = clock.ok_or("TSC frequency")?;
        let uart = config.fch.console_uart;
        fch.enable_legacy_decode();
        // Power all common early devices before waiting, then the UART.
        fch.power_devices(EARLY_AOAC, 10000, &mut delay)
            .map_err(|_| "AOAC power")?;
        let aoac = *UART_AOAC.get(usize::from(uart)).ok_or("console UART")?;
        fch.power_devices(&[aoac], 10000, &mut delay)
            .map_err(|_| "AOAC power")?;
        // Range_0 comes out of reset enabled; clear legacy UART aliases.
        fch.clear_legacy_uart_decode();
        let pins = match uart {
            0 => [
                Pad::native(138, 0, Pull::None),
                Pad::native(136, 0, Pull::None),
            ],
            1 => [
                Pad::native(143, 0, Pull::None),
                Pad::native(141, 0, Pull::None),
            ],
            2 => [
                Pad::native(137, 1, Pull::None),
                Pad::native(135, 1, Pull::None),
            ],
            _ => [
                Pad::native(140, 1, Pull::None),
                Pad::native(142, 1, Pull::None),
            ],
        };
        // SAFETY: as the flow's FCH: BSP only, before the console.
        let fch = Fch::new(unsafe { NativeBus::new() }, fch.acpimmio());
        Gpio::new(fch, Picasso::GPIO_BANK)
            .configure(&pins, &[])
            .map_err(|_| "console pads")
    }

    fn identify(_: &Fch<NativeBus>, _: &Config, mhz: u32, _: u8) -> Result<Identity, &'static str> {
        fstart_log::info!("fstart: AMD Picasso cold boot, TSC {} MHz", mhz);
        let (signature, _, _, _) = fstart_arch::x86::cpuid(1);
        let (_, extended_ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
        Identity::from_cpuid(signature, extended_ebx, 0).map_err(|_| "CPU identification")
    }

    /// The early silicon init (data fabric, NBIO, PSP BAR, BSP), then the
    /// early FCH and the board's hook. Stops there.
    fn silicon_init<B: Board<Soc = Self>>(tp1: Tp1<'_, Self>) -> Result<Silicon<Self>, Stop> {
        let Tp1 {
            ctx,
            config,
            identity,
            memory,
            ..
        } = tp1;
        let Ctx { dies, msr, .. } = ctx;
        let die = &dies[0];
        let banks = msr.reg(MCG_CAP).read(McgCap::COUNT) as u8;
        if !(17..=32).contains(&banks) {
            return Err("CPU identification".into());
        }
        let (smn, df) = (&die.smn(), &die.df());
        let mut delay = ctx.clock;
        fabric::ensure_legacy_routes(die, memory.top_mem as u32).map_err(|_| "data fabric")?;
        fabric::early_init(df);
        let capabilities = nbio::early_init(smn, identity.raven2_cpuid, identity.am4, &mut delay)
            .map_err(|_| "NBIO early init")?;
        let (signature, _, _, _) = fstart_arch::x86::cpuid(1);
        let (_, extended_ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
        Identity::from_cpuid(signature, extended_ebx, capabilities.silicon_id)
            .map_err(|_| "CPU identification")?;
        nbio::psp_bar(smn).map_err(|_| "NBIO early init")?;
        cpu::bsp_init(msr);
        cpu::mca_init(msr, Thread::current()).map_err(|_| "CPU identification")?;
        nbio::dram_routes(die, memory.top_mem, memory.top_mem2).map_err(|_| "NBIO early init")?;
        fstart_log::info!(
            "picasso: silicon {}, TSEG {} + {}",
            Hex(u64::from(capabilities.silicon_id)),
            Hex(memory.tseg),
            Hex(u64::from(Self::TSEG_SIZE))
        );
        for entry in memory.e820.entries() {
            let (addr, size, kind) = (entry.addr, entry.size, entry.kind);
            fstart_log::info!(
                "picasso: e820 {}..{} type {}",
                Hex(addr),
                Hex(addr + size),
                kind
            );
        }
        fch_init(ctx.fch, &config.fch, &mut delay)?;
        B::fch_reset_done(&mut ctx.board());
        Err(Stop::Checkpoint("early init"))
    }

    fn after_resources<B: Board<Soc = Self>>(silicon: &mut Silicon<Self>, _: &Ctx<'_, Self>) {
        match silicon.state {}
    }

    fn ready_to_boot<B: Board<Soc = Self>>(
        silicon: &mut Silicon<Self>,
        _: &Ctx<'_, Self>,
        _: &fstart_pci::PciEcam,
    ) {
        match silicon.state {}
    }
}

/// The early FCH setup: LPC/SPI aperture, eSPI, SPI
/// ROM, SMBus, the board's GPIOs, ROM sharing.
fn fch_init(
    fch: &Fch<NativeBus>,
    config: &FchConfig,
    clock: &mut impl Clock,
) -> Result<(), &'static str> {
    preinit::prepare_lpc(fch).map_err(|_| "LPC setup")?;
    // SAFETY: as the flow's FCH: BSP only.
    let gpio = Gpio::new(
        Fch::new(unsafe { NativeBus::new() }, fch.acpimmio()),
        Picasso::GPIO_BANK,
    );
    if let Some((espi, pads)) = &config.espi {
        gpio.configure(pads, &[]).map_err(|_| "eSPI GPIOs")?;
        // SAFETY: the SPI/eSPI BAR prepare_lpc set up; BSP only.
        let mut controller = unsafe { espi::Native::new(preinit::SPI_BASE as usize) };
        espi::initialize(&mut controller, clock, espi).map_err(|_| "eSPI link")?;
        fstart_log::info!("picasso: eSPI link up");
    }
    preinit::configure_spi(fch, config.spi).map_err(|_| "SPI setup")?;
    preinit::initialize_smbus(fch).map_err(|_| "SMBus setup")?;
    for table in config.early_gpios {
        gpio.configure(table, &[]).map_err(|_| "board GPIOs")?;
    }
    if config.disable_rom_sharing {
        legacy::disable_rom_sharing(fch.bus());
    }
    Ok(())
}
