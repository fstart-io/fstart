// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Picasso silicon init as the steps of [`Init`]: the cold init in the
//! native coreboot sequence. After [`Init::init_fch`] the FCH is up for a
//! board's GPIO tables, ROM sharing and the EC; after [`Init::train_dxio`]
//! the links are trained.
use super::{Config, EARLY_AOAC, FchConfig, Picasso, UART_AOAC};
use crate::{
    apob::Hole,
    ccx::{
        Thread,
        picasso::{self as cpu, Identity},
        tsc::Tsc,
    },
    df::picasso as fabric,
    dxio,
    fch::{
        Fch, NativeBus, emmc, espi,
        gpio::{Gpio, Pad, Pull},
        sandstone::{self, legacy, preinit, usb},
        smi::FchSmi,
    },
    nbio::picasso as nbio,
    psp,
    smu::v10::power,
    soc::{Ctx, Init, Soc as _},
};
use fstart_arch::x86::cpu::msr::{APIC_BASE, ApicBase, MCG_CAP, McgCap, MsrAccess, NativeMsr};
use fstart_log::Hex;
use fstart_pci::PciEcam;
use tock_registers::interfaces::Readable;

/// After the DF, NBIO early init, the BSP, DRAM routing and the FCH: what
/// the early silicon init found, for the later steps.
#[derive(Clone, Copy)]
pub struct FchUp {
    identity: Identity,
    capabilities: nbio::Capabilities,
}

/// After DXIO link training.
pub struct Trained {
    found: FchUp,
    topology: dxio::picasso::Topology,
}

/// After the SMU power policy, legacy/LPC, clocks, USB, the late DF and
/// eMMC.
pub struct Chipset {
    found: FchUp,
    emmc: Option<emmc::Config>,
}

/// What a board learned at run time once the FCH was up, e.g. from the EC.
#[derive(Debug, Clone, Copy)]
pub struct Runtime {
    pub power: power::Config,
    pub emmc_fitted: bool,
}

/// What the silicon init leaves for the steps after MP.
#[derive(Debug, Clone, Copy)]
pub struct State {
    /// The reserved UMA and the part, when it has a GPU.
    pub uma: Option<(Hole, cpu::Silicon)>,
    /// The eMMC controller, if it is fitted and set up.
    pub emmc: Option<emmc::Config>,
}

impl Init for Picasso {
    const NAME: &'static str = "picasso";
    const ECAM: u64 = super::ECAM;
    const ECAM_BUSES: u16 = super::ECAM_BUSES as u16;
    /// BIOS directory entry 0x61.
    const PSP: psp::Variant = psp::Variant::Rv;

    type Config = Config;
    type Identity = Identity;
    type Lanes<'a> = ();
    type Runtime = Runtime;
    type FchUp = FchUp;
    type Trained = Trained;
    type Chipset = Chipset;
    type CpuPolicy = cpu::thread::Policy;
    type State = State;

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

    /// Data fabric, NBIO early init, the BSP, DRAM routing and the FCH:
    /// LPC/SPI, eSPI, SPI ROM, SMBus. The board's own pads come after.
    fn init_fch(ctx: &Ctx<'_, Self>) -> Result<FchUp, &'static str> {
        let Ctx {
            dies, msr, memory, ..
        } = *ctx;
        let die = &dies[0];
        let banks = msr.reg(MCG_CAP).read(McgCap::COUNT) as u8;
        if !(17..=32).contains(&banks) {
            return Err("CPU identification");
        }
        let (smn, df) = (&die.smn(), &die.df());
        let mut delay = ctx.clock;
        let identity = ctx.identity;
        fabric::ensure_legacy_routes(die, memory.top_mem as u32).map_err(|_| "data fabric")?;
        fabric::early_init(df);
        let capabilities = nbio::early_init(smn, identity.raven2_cpuid, identity.am4, &mut delay)
            .map_err(|_| "NBIO early init")?;
        let (signature, _, _, _) = fstart_arch::x86::cpuid(1);
        let (_, extended_ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
        let identity = Identity::from_cpuid(signature, extended_ebx, capabilities.silicon_id)
            .map_err(|_| "CPU identification")?;
        nbio::psp_bar(smn).map_err(|_| "NBIO early init")?;
        cpu::bsp_init(msr);
        cpu::mca_init(msr, Thread::current()).map_err(|_| "CPU identification")?;
        nbio::dram_routes(die, memory.top_mem, memory.top_mem2).map_err(|_| "NBIO early init")?;
        fstart_log::info!(
            "picasso: silicon {}, TSEG {} + {}",
            Hex(u64::from(capabilities.silicon_id)),
            Hex(memory.tseg),
            Hex(u64::from(memory.tseg_size))
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
        fch_init(ctx.fch, &ctx.config.fch, &delay)?;
        Ok(FchUp {
            identity,
            capabilities,
        })
    }

    /// IOAPIC decode, the IOMMU BAR and DXIO link training.
    fn train_dxio(ctx: &Ctx<'_, Self>, found: FchUp, (): ()) -> Result<Trained, &'static str> {
        let die = &ctx.dies[0];
        let smn = &die.smn();
        let mut delay = ctx.clock;
        nbio::ioapic_decode(smn);
        crate::nbio::iommu::init_bar(die, nbio::IOMMU_MMIO_BASE);
        static mut DXIO: dxio::DmaArea = dxio::DmaArea::new();
        // SAFETY: BSP-only, once per boot. The buffers remain in reserved
        // firmware BSS and are never reclaimed, including on failure.
        let topology = unsafe {
            dxio::picasso::train(
                die,
                &mut *core::ptr::addr_of_mut!(DXIO),
                ctx.config.pcie,
                dxio::picasso::Options {
                    power: dxio::picasso::PowerPolicy::Balanced,
                    am4: found.identity.am4,
                    silicon_id: found.capabilities.silicon_id,
                    graphics: found.capabilities.graphics,
                    hda_enabled: true,
                    acp_enabled: true,
                },
                &mut delay,
            )
        }
        .map_err(|_| "DXIO training")?;
        fstart_log::info!("picasso: DXIO training complete");
        Ok(Trained { found, topology })
    }

    /// The SMU power policy, legacy/LPC, clocks, USB, the late DF and eMMC.
    fn init_chipset(
        ctx: &Ctx<'_, Self>,
        Trained { found, topology }: Trained,
        Runtime { power, emmc_fitted }: Runtime,
    ) -> Result<Chipset, &'static str> {
        let FchUp {
            identity,
            capabilities,
        } = found;
        let config = ctx.config;
        let die = &ctx.dies[0];
        let mut delay = ctx.clock;
        let (smn, df) = (&die.smn(), &die.df());
        static mut POWER: power::Pages = power::Pages::new();
        // SAFETY: permanent coherent BSS pages, BSP-only, once per boot.
        unsafe {
            nbio::late::pre_enumeration(
                smn,
                &topology,
                &mut *core::ptr::addr_of_mut!(POWER),
                power,
                &mut delay,
                &mut smu_notice,
            )
        }
        .map_err(|_| "SMU power policy")?;
        sandstone::alink_init(ctx.fch);
        ctx.fch.acpi_decode(sandstone::ACPI_PORTS);
        legacy::initialize(ctx.fch, config.fch.espi.is_some(), false);
        legacy::lpc_init(ctx.fch.bus());
        sandstone::clock_init(smn);
        // SAFETY: BSP owns the decoded fixed USB PHY registers.
        usb::initialize(
            smn,
            &mut unsafe { usb::NativePhy::new() },
            &config.usb,
            identity,
            capabilities.silicon_id,
            false,
            &mut delay,
        )
        .map_err(|_| "USB init")?;
        fabric::late_init(df, identity.raven2_cpuid);
        let emmc = config.emmc.filter(|_| emmc_fitted);
        if let Some(emmc) = emmc {
            // SAFETY: BSP owns the decoded eMMC configuration registers.
            emmc::initialize(&mut unsafe { emmc::Native::new() }, emmc).map_err(|_| "eMMC init")?;
        }
        Ok(Chipset { found, emmc })
    }

    fn cpu_policy(ctx: &Ctx<'_, Self>, _: &Chipset) -> Result<Self::CpuPolicy, &'static str> {
        cpu::thread::Policy::capture(ctx.msr, nbio::PSP_MMIO_BASE.into())
            .map_err(|_| "CPU cache policy")
    }

    fn cpu_init(policy: &Self::CpuPolicy, msr: &NativeMsr, thread: Thread) -> bool {
        let bsp = msr.reg(APIC_BASE).is_set(ApicBase::BSP);
        policy.initialize(msr, bsp, thread).is_ok()
    }

    /// The SMU's CCX-done, graphics and the final NBIO init.
    fn cpus_up(
        ctx: &Ctx<'_, Self>,
        Chipset { found, emmc }: Chipset,
    ) -> Result<State, &'static str> {
        let FchUp {
            identity,
            capabilities,
        } = found;
        let memory = ctx.memory;
        let smn = &ctx.dies[0].smn();
        let mut delay = ctx.clock;
        nbio::late::ccx_done(smn, capabilities.graphics, &mut delay, &mut smu_notice)
            .map_err(|_| "SMU CCX done")?;
        let uma = if capabilities.graphics {
            // SAFETY: reserved, identity-mapped UMA; no display driver is active.
            Some(unsafe {
                nbio::gfx::initialize(
                    smn,
                    memory.apob.ok_or("GFX APOB")?,
                    ctx.config.graphics,
                    memory.top_mem,
                    memory.top_mem2,
                    &mut delay,
                )
            }?)
        } else {
            None
        };
        nbio::late::post_silicon(smn, identity, &mut delay).map_err(|_| "NBIO final init")?;
        ctx.fch.acpi_ports(sandstone::ACPI_PORTS);
        Ok(State {
            uma: uma.map(|region| (region, identity.silicon)),
            emmc,
        })
    }

    fn smi(_: &Ctx<'_, Self>) -> FchSmi {
        // SAFETY: BSP exclusively owns the decoded ACPI and SMI blocks.
        unsafe { FchSmi::new(sandstone::ACPI_PORTS, 0) }
    }

    fn after_resources(ctx: &Ctx<'_, Self>, _: &mut State) {
        ctx.fch.acpi_ports(sandstone::ACPI_PORTS);
    }

    fn ready_to_boot(
        ctx: &Ctx<'_, Self>,
        _: &mut State,
        pci: &PciEcam,
    ) -> Result<(), &'static str> {
        let devices: heapless::Vec<_, 256> = pci.devices().collect();
        nbio::late::after_enumeration(&ctx.dies[0].smn(), &ctx.dies[0].pci(), &devices);
        Ok(())
    }

    fn psp_bar(_: &State) -> u32 {
        nbio::PSP_MMIO_BASE
    }

    fn ioapic(_: &State, die: u8) -> u32 {
        if die == 0 { 0xfec0_1000 } else { 0 }
    }

    fn nonpci_mmio(_: &State, die: u8) -> (u64, u64) {
        if die == 0 {
            (nbio::SMU_MMIO_BASE.into(), 0x28_0000)
        } else {
            (0, 0)
        }
    }
}

/// Log an SMU message that did not return OK.
fn smu_notice(message: u32, status: u32) {
    fstart_log::warn!(
        "SMU message {} returned {}",
        Hex(u64::from(message)),
        Hex(u64::from(status))
    )
}

/// The early FCH setup: LPC/SPI aperture, eSPI, SPI ROM, SMBus.
fn fch_init(
    fch: &Fch<NativeBus>,
    config: &FchConfig,
    timer: &impl fstart_core::services::Timer,
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
        espi::initialize(&mut controller, timer, espi).map_err(|_| "eSPI link")?;
        fstart_log::info!("picasso: eSPI link up");
    }
    preinit::configure_spi(fch, config.spi).map_err(|_| "SPI setup")?;
    preinit::initialize_smbus(fch).map_err(|_| "SMBus setup")
}
