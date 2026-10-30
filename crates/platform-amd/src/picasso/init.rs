// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Picasso silicon init phases a board's `main` calls.
use super::{Config, FchConfig, acpi};
use crate::noncar::{self, Ctx, Silicon, Stage, Stop, layout::Region, mp, psp, smm};
use fstart_arch::x86::cpu::msr::{MCG_CAP, McgCap, MsrAccess};
use fstart_driver_amd::{
    ccx::{
        Thread,
        picasso::{self as cpu, Identity},
        tsc::{Clock, Tsc},
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

/// The cold silicon init in the native coreboot sequence, followed by the
/// shared architectural MP and SMM installers. The board's `main` drives it
/// and does its own work between the phases:
///
/// ```ignore
/// let init = picasso::Init::start(&stage)?;
/// // board: FCH is up — its GPIO tables, ROM sharing, the EC
/// let silicon = init.train_dxio()?
///     // board: links are trained
///     .finish(power, emmc_fitted)?;
/// ```
pub struct Init<'a> {
    stage: &'a Stage<Picasso>,
    config: &'static Config,
    identity: Identity,
    capabilities: nbio::Capabilities,
    delay: Tsc,
}

/// The silicon init after DXIO link training, before the SMU policy and the rest.
pub struct Dxio<'a> {
    init: Init<'a>,
    topology: dxio::picasso::Topology,
}

impl<'a> Init<'a> {
    /// Data fabric, NBIO early init, the BSP, DRAM routing and the FCH:
    /// LPC/SPI, eSPI, SPI ROM, SMBus. The board's own pads come after.
    pub fn start(stage: &'a Stage<Picasso>) -> Result<Self, Stop> {
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
        fch_init(ctx.fch, &config.fch, &mut delay)?;
        Ok(Self {
            stage,
            config,
            identity,
            capabilities,
            delay,
        })
    }

    /// `DISABLE_SPI_FLASH_ROM_SHARING`: stop sharing the SPI ROM with the
    /// EC. Boards that need it call it once their early pads are set.
    pub fn disable_rom_sharing(&self) {
        legacy::disable_rom_sharing(self.stage.ctx().fch.bus());
    }

    /// The GPIO pads of die 0.
    pub fn gpio(&self) -> Gpio<NativeBus> {
        self.stage.ctx().gpio()
    }

    /// The calibrated clock.
    pub fn clock(&self) -> Tsc {
        self.delay
    }

    /// IOAPIC decode, the IOMMU BAR and DXIO link training.
    pub fn train_dxio(self) -> Result<Dxio<'a>, Stop> {
        let ctx = self.stage.ctx();
        let die = &ctx.dies[0];
        let smn = &die.smn();
        let mut delay = self.delay;
        nbio::ioapic_decode(smn);
        fstart_driver_amd::nbio::iommu::init_bar(die, nbio::IOMMU_MMIO_BASE);
        static mut DXIO: dxio::picasso::Buffers = dxio::picasso::Buffers::new();
        // SAFETY: BSP-only, once per boot. The buffers remain in reserved
        // firmware BSS and are never reclaimed, including on failure.
        let topology = unsafe {
            dxio::picasso::train(
                die,
                &mut *core::ptr::addr_of_mut!(DXIO),
                self.config.pcie,
                dxio::picasso::Options {
                    power: dxio::picasso::PowerPolicy::Balanced,
                    am4: self.identity.am4,
                    silicon_id: self.capabilities.silicon_id,
                    graphics: self.capabilities.graphics,
                    hda_enabled: true,
                    acp_enabled: true,
                },
                &mut delay,
            )
        }
        .map_err(|_| "DXIO training")?;
        fstart_log::info!("picasso: DXIO training complete");
        Ok(Dxio {
            init: self,
            topology,
        })
    }
}

impl Dxio<'_> {
    /// SMU power policy, legacy/LPC, clocks, USB, the late DF, eMMC, MP init,
    /// graphics, the final NBIO init and SMM with the PSP's SMM info.
    /// `power` and `emmc_fitted` are what the board learned at runtime once
    /// the FCH was up, e.g. from the EC.
    pub fn finish(self, power: power::Config, emmc_fitted: bool) -> Result<Silicon<Picasso>, Stop> {
        let Dxio {
            init:
                Init {
                    stage,
                    config,
                    identity,
                    capabilities,
                    mut delay,
                },
            topology,
        } = self;
        let ctx = stage.ctx();
        let memory = stage.memory();
        let Ctx { dies, msr, .. } = ctx;
        let die = &dies[0];
        let (smn, df) = (&die.smn(), &die.df());
        static mut POWER: power::Pages = power::Pages::new();
        let mut notice = |message, status| {
            fstart_log::warn!(
                "SMU message {} returned {}",
                Hex(u64::from(message)),
                Hex(u64::from(status))
            )
        };
        // SAFETY: permanent coherent BSS pages, BSP-only, once per boot.
        unsafe {
            nbio::late::pre_enumeration(
                smn,
                &topology,
                &mut *core::ptr::addr_of_mut!(POWER),
                power,
                &mut delay,
                &mut notice,
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
        nbio::late::ccx_done(smn, capabilities.graphics, &mut delay, &mut notice)
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
}

/// The early FCH setup: LPC/SPI aperture, eSPI, SPI ROM, SMBus.
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
    preinit::initialize_smbus(fch).map_err(|_| "SMBus setup")
}
