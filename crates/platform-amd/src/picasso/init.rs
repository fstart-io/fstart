// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Picasso silicon init as the steps of [`Init`]: the cold init in the
//! native coreboot sequence. After [`Init::init_fch`] the FCH is up for a
//! board's GPIO tables, ROM sharing and the EC; after [`Init::train_dxio`]
//! the links are trained.
use super::{Config, EARLY_AOAC, FchConfig, Picasso, UART_AOAC};
use crate::soc::{Ctx, DieInit, Entered, Hw, Init, Roots, Soc as _};
use fstart_arch::x86::{
    cpu::{
        amd::smm::InstalledTseg,
        msr::{APIC_BASE, ApicBase, MCG_CAP, McgCap, MsrAccess, NativeMsr},
    },
    mp::MpHandle,
};
use fstart_core::description::PciRootInfo;
use fstart_driver_amd::{
    apob::Hole,
    ccx::{
        Thread,
        mp::Fam17hCpu,
        rv::{self as cpu, Identity},
        tsc::Tsc,
    },
    df::rv as fabric,
    dxio,
    fch::{
        emmc, espi,
        gpio::{Gpio, Pad, Pull},
        hw::{FchHw, Native},
        sandstone::{self, legacy, preinit, usb},
        smi::FchSmi,
    },
    nbio::rv as nbio,
    psp::{self, boot::Psp},
    smu::v10::power,
};
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
    topology: dxio::rv::Topology,
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
pub struct State {
    /// The reserved UMA and the part, when it has a GPU.
    pub uma: Option<(Hole, cpu::Silicon)>,
    /// The eMMC controller, if it is fitted and set up.
    pub emmc: Option<emmc::Config>,
    bsp_apic: u8,
    /// The PSP mailbox, once it took the SMM info.
    psp: Option<Psp>,
}

impl Picasso {
    /// The NBIO IOAPIC.
    pub const IOAPIC: u32 = 0xfec0_1000;
}

impl Init for Picasso {
    const NAME: &'static str = "picasso";

    type Config = Config;
    type Identity = Identity;
    type Hw = Hw<Self>;
    type Lanes<'a> = ();
    type Runtime = Runtime;
    type FchUp = FchUp;
    type Trained = Trained;
    type Chipset = Chipset;
    type Cpu = Fam17hCpu<Self, cpu::thread::Policy>;
    type State = State;

    /// The console: ACPI MMIO decode, legacy decode, AOAC power of the
    /// early devices and the console UART, its pads.
    fn enter(config: &Config, msr: &NativeMsr) -> Entered<Self> {
        Hw::enter(msr, super::ECAM, super::ECAM_BUSES.into(), |fch, clock| {
            let mut delay = clock.ok_or("TSC frequency")?;
            let uart = config.fch.console_uart;
            fstart_driver_amd::fch::enable_legacy_decode(fch.acpi());
            // Power all common early devices before waiting, then the UART.
            fstart_driver_amd::fch::power_devices(fch.acpi(), EARLY_AOAC, 10000, &mut delay)
                .map_err(|_| "AOAC power")?;
            let aoac = *UART_AOAC.get(usize::from(uart)).ok_or("console UART")?;
            fstart_driver_amd::fch::power_devices(fch.acpi(), &[aoac], 10000, &mut delay)
                .map_err(|_| "AOAC power")?;
            // Range_0 comes out of reset enabled; clear legacy UART aliases.
            fstart_driver_amd::fch::clear_legacy_uart_decode(fch.mmio());
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
            Gpio::new(fch.acpi(), Picasso::GPIO_BANK)
                .configure(&pins, &[])
                .map_err(|_| "console pads")
        })
    }

    fn identify(_: &Hw<Self>, _: &Config, clock: Option<Tsc>) -> Result<Identity, &'static str> {
        fstart_log::info!(
            "fstart: AMD Picasso cold boot, TSC {} MHz",
            clock.map_or(0, |clock| clock.mhz())
        );
        let (signature, _, _, _) = fstart_arch::x86::cpuid(1);
        let (_, extended_ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
        Identity::from_cpuid(signature, extended_ebx, 0).map_err(|_| "CPU identification")
    }

    fn pci_segment(_: &Hw<Self>) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: super::ECAM,
            bus_start: 0,
            bus_end: super::ECAM_BUSES - 1,
        }
    }

    /// BIOS directory entry 0x61.
    fn psp(_: &Hw<Self>) -> psp::Variant {
        psp::Variant::Rv
    }

    /// Data fabric, NBIO early init, the BSP, DRAM routing and the FCH:
    /// LPC/SPI, eSPI, SPI ROM, SMBus. The board's own pads come after.
    fn init_fch(ctx: &Ctx<'_, Self>) -> Result<FchUp, &'static str> {
        let Ctx { msr, memory, .. } = *ctx;
        let dies = &ctx.hw.dies;
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
        fch_init(&ctx.hw.fch, &ctx.config.fch, &delay)?;
        Ok(FchUp {
            identity,
            capabilities,
        })
    }

    /// IOAPIC decode, the IOMMU BAR and DXIO link training.
    fn train_dxio(ctx: &Ctx<'_, Self>, found: FchUp, (): ()) -> Result<Trained, &'static str> {
        let die = &ctx.hw.dies[0];
        let smn = &die.smn();
        let delay = ctx.clock;
        nbio::ioapic_decode(smn);
        fstart_driver_amd::nbio::iommu::init_bar(die, nbio::IOMMU_MMIO_BASE);
        static mut DXIO: dxio::DmaArea = dxio::DmaArea::new();
        // SAFETY: BSP-only, once per boot. The buffers remain in reserved
        // firmware BSS and are never reclaimed, including on failure.
        let topology = unsafe {
            dxio::rv::train(
                die,
                &mut *core::ptr::addr_of_mut!(DXIO),
                ctx.config.pcie,
                dxio::rv::Options {
                    power: dxio::rv::PowerPolicy::Balanced,
                    am4: found.identity.am4,
                    silicon_id: found.capabilities.silicon_id,
                    graphics: found.capabilities.graphics,
                    hda_enabled: true,
                    acp_enabled: true,
                },
                &delay,
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
        let die = &ctx.hw.dies[0];
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
        sandstone::alink_init(&ctx.hw.fch);
        fstart_driver_amd::fch::acpi_decode(ctx.hw.fch.acpi(), sandstone::ACPI_PORTS);
        legacy::initialize(&ctx.hw.fch, config.fch.espi.is_some(), false);
        legacy::lpc_init(&ctx.hw.fch);
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

    fn cpu_driver(
        ctx: &Ctx<'_, Self>,
        _: &mut Chipset,
        microcode: Option<&'static [u8]>,
    ) -> Result<Self::Cpu, &'static str> {
        let policy = cpu::thread::Policy::capture(
            ctx.msr,
            nbio::PSP_MMIO_BASE.into(),
            (super::ECAM, super::ECAM_BUSES.into()),
        )
        .map_err(|_| "CPU cache policy")?;
        Ok(Fam17hCpu::new(
            "native-picasso",
            microcode,
            policy,
            |policy, msr| {
                let bsp = msr.reg(APIC_BASE).is_set(ApicBase::BSP);
                policy
                    .initialize(msr, bsp, fstart_driver_amd::ccx::Thread::current())
                    .is_ok()
            },
        ))
    }

    fn threads(ctx: &Ctx<'_, Self>, _: &Chipset) -> Result<u16, &'static str> {
        ctx.hw.threads()
    }

    fn release_aps(ctx: &Ctx<'_, Self>, _: &Chipset, aps: u16) -> Result<(), u32> {
        ctx.hw.release_aps(aps, ctx.clock)
    }

    fn cpu_errors(cpu: &Self::Cpu) -> u32 {
        cpu.errors()
    }

    /// The SMU's CCX-done, graphics and the final NBIO init.
    fn cpus_up(
        ctx: &Ctx<'_, Self>,
        Chipset { found, emmc }: Chipset,
        _: &MpHandle,
        apic_ids: &[u32],
    ) -> Result<State, &'static str> {
        let FchUp {
            identity,
            capabilities,
        } = found;
        let memory = ctx.memory;
        let smn = &ctx.hw.dies[0].smn();
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
        fstart_driver_amd::fch::acpi_ports(ctx.hw.fch.acpi(), sandstone::ACPI_PORTS);
        Ok(State {
            uma: uma.map(|region| (region, identity.silicon)),
            emmc,
            bsp_apic: apic_ids.first().copied().unwrap_or(0) as u8,
            psp: None,
        })
    }

    fn roots(ctx: &Ctx<'_, Self>, _: &State) -> Option<Roots> {
        ctx.hw.roots(&Self::pci_segment(ctx.hw), |die| {
            if die == 0 {
                (nbio::SMU_MMIO_BASE.into(), 0x28_0000)
            } else {
                (0, 0)
            }
        })
    }

    fn after_resources(ctx: &Ctx<'_, Self>, _: &mut State) -> Result<(), &'static str> {
        fstart_driver_amd::fch::acpi_ports(ctx.hw.fch.acpi(), sandstone::ACPI_PORTS);
        Ok(())
    }

    fn ready_to_boot(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        pci: &PciEcam,
    ) -> Result<(), &'static str> {
        ctx.hw.init_ioapics(state.bsp_apic, |_| Self::IOAPIC);
        let devices: heapless::Vec<_, 256> = pci.devices().collect();
        nbio::late::after_enumeration(&ctx.hw.dies[0].smn(), &ctx.hw.dies[0].pci(), &devices);
        Ok(())
    }

    fn finalize(ctx: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        psp::boot::lock_df_registers(state.psp.as_mut(), &ctx.clock)
    }

    /// A failed BootDone is logged: the session refuses a retry, and the
    /// transition may have happened.
    fn boot_done(ctx: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        // SAFETY: the flow is done with the CPUs and SMM is locked.
        unsafe { psp::boot::boot_done(state.psp.as_mut(), &ctx.clock) };
        Ok(())
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
    fch: &Native<Tsc>,
    config: &FchConfig,
    timer: &impl fstart_core::services::Timer,
) -> Result<(), &'static str> {
    preinit::prepare_lpc(fch).map_err(|_| "LPC setup")?;
    let gpio = Gpio::new(fch.acpi(), Picasso::GPIO_BANK);
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

impl DieInit for Picasso {
    fn smi(_: &Ctx<'_, Self>, _: &State) -> FchSmi {
        // SAFETY: BSP exclusively owns the decoded ACPI and SMI blocks.
        unsafe { FchSmi::new(sandstone::ACPI_PORTS, 0) }
    }

    fn smm_installed(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        tseg: &InstalledTseg,
    ) -> Result<(), &'static str> {
        state.psp = psp::boot::notify_smm(nbio::PSP_MMIO_BASE, psp::Variant::Rv, tseg, &ctx.clock);
        Ok(())
    }
}
