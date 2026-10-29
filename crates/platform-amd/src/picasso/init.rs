// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Picasso silicon init phases a board's `main` calls.
use super::{FchConfig, acpi};
use crate::noncar::{self, Cpus, Ctx, Mapped, Silicon, Stage, Stop, layout::Region, mp, psp, smm};
use fstart_arch::x86::cpu::msr::{MCG_CAP, McgCap, MsrAccess};
use fstart_driver_amd::{
    ccx::{
        Thread,
        picasso::{self as cpu, Identity},
    },
    df::picasso as fabric,
    dxio,
    fch::{
        Fch, NativeBus, emmc, espi,
        gpio::Gpio,
        sandstone::{self, legacy, preinit, usb},
        smi::FchSmi,
    },
    nbio::picasso as nbio,
    smu::v10::power,
    soc::{Soc as _, picasso::Picasso},
};
use fstart_log::Hex;
use tock_registers::interfaces::Readable;

// The cold silicon init in the native coreboot sequence, followed by the
// shared architectural MP and SMM installers. The board's `main` drives it
// and does its own work between the phases:
//
// ```ignore
// let stage = stage.init_fch();
// // board: FCH is up — its GPIO tables, ROM sharing, the EC
// let stage = stage.train_dxio()
//     // board: links are trained
//     .init_chipset(power, emmc_fitted)
//     .start_cpus();
// ```

/// What the early silicon init found, for the later phases.
#[derive(Clone, Copy)]
struct Found {
    identity: Identity,
    capabilities: nbio::Capabilities,
}

/// After the DF, NBIO early init, the BSP, DRAM routing and the FCH.
pub struct FchUp(Found);

/// After DXIO link training.
pub struct Trained {
    found: Found,
    topology: dxio::picasso::Topology,
}

/// After the SMU power policy, legacy/LPC, clocks, USB, the late DF and
/// eMMC.
pub struct Chipset {
    found: Found,
    emmc: Option<emmc::Config>,
}

impl Stage<Picasso, Mapped> {
    /// Data fabric, NBIO early init, the BSP, DRAM routing and the FCH:
    /// LPC/SPI, eSPI, SPI ROM, SMBus. The board's own pads come after.
    pub fn init_fch(self) -> Stage<Picasso, FchUp> {
        let found = try_init_fch(&self).unwrap_or_else(|why| self.stop(why));
        self.into_phase(FchUp(found))
    }
}

fn try_init_fch(stage: &Stage<Picasso, Mapped>) -> Result<Found, Stop> {
    let config = stage.config();
    let ctx = stage.ctx();
    let Ctx { dies, msr, .. } = ctx;
    let die = &dies[0];
    let banks = msr.reg(MCG_CAP).read(McgCap::COUNT) as u8;
    if !(17..=32).contains(&banks) {
        return Err("CPU identification".into());
    }
    let (smn, df) = (&die.smn(), &die.df());
    let mut delay = ctx.clock;
    let memory = stage.memory();
    let identity = stage.identity();
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
        Hex(u64::from(<Picasso as noncar::Soc>::TSEG_SIZE))
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
    fch_init(ctx.fch, &config.fch, &delay)?;
    Ok(Found {
        identity,
        capabilities,
    })
}

impl Stage<Picasso, FchUp> {
    /// `DISABLE_SPI_FLASH_ROM_SHARING`: stop sharing the SPI ROM with the
    /// EC. Boards that need it call it once their early pads are set.
    pub fn disable_rom_sharing(&self) {
        legacy::disable_rom_sharing(self.fch().bus());
    }

    /// IOAPIC decode, the IOMMU BAR and DXIO link training.
    pub fn train_dxio(self) -> Stage<Picasso, Trained> {
        let FchUp(found) = self.phase;
        let topology = try_train_dxio(&self, found).unwrap_or_else(|why| self.stop(why));
        self.into_phase(Trained { found, topology })
    }
}

fn try_train_dxio(
    stage: &Stage<Picasso, FchUp>,
    found: Found,
) -> Result<dxio::picasso::Topology, Stop> {
    let ctx = stage.ctx();
    let die = &ctx.dies[0];
    let smn = &die.smn();
    let mut delay = ctx.clock;
    nbio::ioapic_decode(smn);
    fstart_driver_amd::nbio::iommu::init_bar(die, nbio::IOMMU_MMIO_BASE);
    static mut DXIO: dxio::picasso::Buffers = dxio::picasso::Buffers::new();
    // SAFETY: BSP-only, once per boot. The buffers remain in reserved
    // firmware BSS and are never reclaimed, including on failure.
    let topology = unsafe {
        dxio::picasso::train(
            die,
            &mut *core::ptr::addr_of_mut!(DXIO),
            stage.config().pcie,
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
    Ok(topology)
}

impl Stage<Picasso, Trained> {
    /// The SMU power policy, legacy/LPC, clocks, USB, the late DF and eMMC.
    /// `power` and `emmc_fitted` are what the board learned at runtime once
    /// the FCH was up, e.g. from the EC.
    pub fn init_chipset(self, power: power::Config, emmc_fitted: bool) -> Stage<Picasso, Chipset> {
        let Trained { found, topology } = self.phase;
        let emmc = try_init_chipset(&self, found, topology, power, emmc_fitted)
            .unwrap_or_else(|why| self.stop(why));
        self.into_phase(Chipset { found, emmc })
    }
}

impl Stage<Picasso, Chipset> {
    /// MP init, graphics, the final NBIO init and SMM with the PSP's SMM
    /// info.
    pub fn start_cpus(self) -> Stage<Picasso, Cpus<Picasso>> {
        let Chipset { found, emmc } = self.phase;
        let silicon = try_start_cpus(&self, found, emmc).unwrap_or_else(|why| self.stop(why));
        self.into_phase(Cpus(silicon))
    }
}

fn try_init_chipset(
    stage: &Stage<Picasso, Trained>,
    Found {
        identity,
        capabilities,
    }: Found,
    topology: dxio::picasso::Topology,
    power: power::Config,
    emmc_fitted: bool,
) -> Result<Option<emmc::Config>, Stop> {
    let config = stage.config();
    let ctx = stage.ctx();
    let Ctx { dies, .. } = ctx;
    let die = &dies[0];
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
    Ok(emmc)
}

fn try_start_cpus(
    stage: &Stage<Picasso, Chipset>,
    Found {
        identity,
        capabilities,
    }: Found,
    emmc: Option<emmc::Config>,
) -> Result<Silicon<Picasso>, Stop> {
    let config = stage.config();
    let ctx = stage.ctx();
    let memory = stage.memory();
    let Ctx { dies, msr, .. } = ctx;
    let die = &dies[0];
    let smn = &die.smn();
    let mut delay = ctx.clock;
    let policy = cpu::thread::Policy::capture(msr, nbio::PSP_MMIO_BASE.into())
        .map_err(|_| "CPU cache policy")?;
    let cpus = mp::start::<Picasso, _>(
        dies,
        stage.microcode(),
        move |msr, thread| {
            use fstart_arch::x86::cpu::msr::{APIC_BASE, ApicBase};
            let bsp = msr.reg(APIC_BASE).is_set(ApicBase::BSP);
            policy.initialize(msr, bsp, thread)
        },
        delay,
    )?;
    nbio::late::ccx_done(smn, capabilities.graphics, &mut delay, &mut smu_notice)
        .map_err(|_| "SMU CCX done")?;
    let uma = if capabilities.graphics {
        // SAFETY: reserved, identity-mapped UMA; no display driver is active.
        Some(unsafe {
            nbio::gfx::initialize(
                smn,
                memory.apob.ok_or("GFX APOB")?,
                config.graphics,
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
    // SAFETY: BSP exclusively owns the decoded ACPI and SMI blocks.
    let smi = unsafe { FchSmi::new(sandstone::ACPI_PORTS, 0) };
    let tseg = smm::install(
        <Picasso as noncar::Soc>::NAME,
        &cpus.mp,
        &smi,
        memory.tseg,
        <Picasso as noncar::Soc>::TSEG_SIZE,
    )?;
    let psp = psp::notify_smm(
        nbio::PSP_MMIO_BASE,
        <Picasso as noncar::Soc>::PSP,
        &tseg,
        &delay,
    );
    let devices = emmc
        .into_iter()
        .map(|_| acpi::emmc())
        .collect::<Result<_, _>>()
        .map_err(|_| "eMMC ACPI")?;
    let aml = acpi::devices(config.i2c, emmc.is_some()).map_err(|_| "ACPI devices")?;
    Ok(Silicon {
        cpus,
        psp,
        devices,
        aml,
        tables: alloc::vec::Vec::new(),
        framebuffer: None,
        ioapics: core::array::from_fn(|i| if i == 0 { 0xfec0_1000 } else { 0 }),
        reserved_mmio: core::array::from_fn(|i| {
            if i == 0 {
                Region::new(nbio::SMU_MMIO_BASE.into(), 0x28_0000)
            } else {
                Region::new(0, 0)
            }
        }),
        acpi: &acpi::ACPI,
        state: uma.map(|region| (region, identity.silicon)),
    })
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
