//! What the SoCs of the SP3 socket share on the flow ([`crate::flow`]):
//! [`crate::naples::Naples`], [`crate::rome::Rome`] and
//! [`crate::milan::Milan`] each implement [`Init`] and
//! [`crate::flow::Soc`] with the functions here: the socket's handles and
//! FCH reset tasks, its configuration, and the flow's services for it (the
//! flash windows and the APOB behind them, the options, the memory map, the
//! TPM, SMM with RAS and EINJ, and the tables).
//!
//! One reset image carries every SoC the board supports: [`start`]
//! returns the running one ([`Start`]). A CPU this build does not carry
//! halts at POST 13 before the console (its FCH bring-up is not compiled
//! in); one the board does not list halts after logging.
//!
//! Every step writes a POST code to port 80h and logs its name; a failing
//! step logs and halts (the last POST code shows where).
//!
//! | POST | Step |
//! |---|---|
//! | 10 | ECAM |
//! | 11 | FCH reset tasks, board LPC/SIO |
//! | 31 | console (the flow's) |
//! | 13 | CPU identity |
//! | 14 | APOB |
//! | 15 | memory map, MTRRs |
//! | 16 | TPM: measured boot starts (`Board::DISCRETE_TPM`) |
//! | 48 | boot media (the FFS, with the microcode MP loads) |
//! | 32 | the memory map is done (the flow's) |
//! | 20/21 | DF plan + maps / DF init |
//! | 30/31/32 | NBIO / SMU / FCH secondaries and USB; board pads and GPIO |
//! | 38 | DXIO |
//! | 40 | FCH env |
//! | 50 | MP + CCX |
//! | 58 | SMM |
//! | 60 | PCI enumeration |
//! | 70 | ACPI |
//! | 80 | ReadyToBoot |
//! | 43/44 | locked / the APs are parked (the flow's) |
//! | 90 | payload |
extern crate alloc;

use crate::soc::{Entered, Init, Roots};
use crate::sp3::silicon::hw::{self, PrimaryFch};
use crate::sp3::silicon::steps;
use crate::sp3::silicon::{self, Generation, Identity, NumaPlan};
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_arch::x86::mp::MpHandle;
use fstart_core::description::PciRootInfo;
use fstart_driver_amd::df::Version;
use fstart_driver_amd::dxio::lanes::Engine;
use fstart_driver_amd::dxio::wire::MAX_ENGINES;
use fstart_driver_amd::fch::gpio::Pad;
use fstart_driver_amd::fch::hw::FchHw;
use fstart_driver_amd::nbio::Nbio;
use fstart_driver_amd::nbio::iohc::DramTops;

use super::board::Gpio;
use super::flow::{self as parts, step};
use super::layout;
use crate::flow::{Ctx, Early, Mapped, Reserved, Silicon as Up, Stage, Tables};
use crate::layout::Region;

/// The board's DXIO engines, what the SP3 SoCs train.
pub type Engines = heapless::Vec<Engine<'static>, MAX_ENGINES>;

/// A SoC of the SP3 socket on the flow: what a board's flow is generic
/// over, `fn flow<S: Socket>(id: Identified<Board, S>) -> !`.
pub trait Socket:
    crate::flow::Soc<Config = Config, Hw = Hw, Runtime = (), Flow = Flow>
    + for<'a> Init<Lanes<'a> = &'a Engines>
{
}
impl<S> Socket for S where
    S: crate::flow::Soc<Config = Config, Hw = Hw, Runtime = (), Flow = Flow>
        + for<'a> Init<Lanes<'a> = &'a Engines>
{
}

pub use super::board::Config;

/// The handles of the socket's flow: the identified CPU, its ECAM segment,
/// the clock and socket 0's FCH.
pub struct Hw {
    pub identity: Identity,
    pub pci_root: PciRootInfo,
    pub timer: hw::Tsc,
    pub fch: PrimaryFch,
    /// The FCH woke from S3, which is not supported.
    resume: bool,
}

/// What the silicon steps hand on (`F`: the SoC's own handles), and the
/// SMM handler's configuration once it is installed.
pub struct State<F> {
    pub soc: silicon::Ctx,
    pub si: F,
    /// EINJ's targets, once their scratch pages are planned.
    einj: Option<super::smm::EinjConfig>,
    smm: Option<super::smm::SmmConfig>,
}

impl<F> State<F> {
    pub fn new(soc: silicon::Ctx, si: F) -> Self {
        Self {
            soc,
            si,
            einj: None,
            smm: None,
        }
    }
}

/// What the socket's services keep between the phases.
pub struct Flow {
    /// The FFS in the CPU view (ROM3), from the flash anchor.
    #[cfg(feature = "payload-uefi")]
    ffs: Region,
    /// The FFS's flash offset, for the payload's settings storage.
    #[cfg(feature = "payload-uefi")]
    ffs_offset: u32,
    /// The settings store ([`layout::NVRAM`]) in the CPU view, if a ROM
    /// window shows it.
    nvram: Option<&'static [u8]>,
    /// The outcome of every option knob resolved so far.
    options: fstart_options::Report,
    tseg: Region,
    /// The size of the flash part.
    rom_size: u32,
}

/// CPUID → revision: the generation picks the silicon steps. An unknown
/// CPU or a non-SP3 package halts at POST 13 (SOC_DF §2.3: no "unknown
/// stepping = first row"); there is no console yet, as bringing it up
/// already needs the generation's FCH code.
fn identify() -> Identity {
    let signature = fstart_arch::x86::cpuid(1).0;
    let ext_ebx = fstart_arch::x86::cpuid(0x8000_0001).1;
    silicon::identify(signature, ext_ebx).unwrap_or_else(|_| {
        hw::post(0x13);
        fstart_arch::x86::boot::halt()
    })
}

impl<B, S: Socket> crate::flow::Identified<B, S> {
    /// The running CPU's generation, for the board's choice of its silicon
    /// policy.
    pub fn generation(&self) -> Generation {
        Generation::of(identify().silicon())
    }
}

impl<S: Socket> Early<S> {
    /// Socket 0's FCH, for the board's LPC decodes.
    pub fn fch(&self) -> &PrimaryFch {
        &self.hw().fch
    }
}

impl<S: Socket, P> Stage<S, P> {
    /// The running CPU's generation.
    pub fn generation(&self) -> Generation {
        Generation::of(self.hw().identity.silicon())
    }

    /// Socket 0's FCH.
    pub fn fch(&self) -> &PrimaryFch {
        &self.hw().fch
    }

    /// The calibrated clock, to pace board device transactions.
    pub fn timer(&self) -> &dyn fstart_core::services::Timer {
        &self.hw().timer
    }

    /// Configure and read the board's detect pads (M.2 SATA/PCIe detect,
    /// straps), in order, before its GPIO table.
    pub fn read_pads(&self, pads: &[Pad]) -> heapless::Vec<bool, 8> {
        let acpi = self.fch().acpi();
        pads.iter()
            .map(|pad| {
                fstart_driver_amd::fch::gpio::configure(acpi, pad);
                fstart_driver_amd::fch::gpio::read(acpi, pad.pin)
            })
            .collect()
    }

    /// The board GPIO program: the OEM pads, then the CPM table. The vendor
    /// applies both in PEI before the PCIe topology reaches DXIO
    /// (BOARD_S8030 §2 steps 3–5), so board enables and resets are in place
    /// when the links train.
    pub fn program_gpio(&self, gpio: &Gpio) {
        let acpi = self.fch().acpi();
        fstart_driver_amd::fch::gpio::configure_all(acpi, gpio.early);
        fstart_driver_amd::fch::gpio::configure_all(acpi, gpio.table);
    }

    /// Apply the stored overrides of `knobs` to the board's `config`
    /// (`fstart_options`), before the board uses it. `check` is the whole-set
    /// validation of the consumer: if the overridden configuration fails
    /// it, `config` keeps the board's values and the overrides are reported
    /// as rejected. The knobs are shown in the payload's settings menu.
    pub fn resolve_options<C: Clone>(
        &mut self,
        knobs: &'static [fstart_options::Knob<C>],
        config: &mut C,
        check: impl FnOnce(&C) -> Result<(), &'static str>,
    ) {
        let flow = self.flow_mut();
        parts::options::resolve(flow.nvram, knobs, config, &mut flow.options, check);
    }
}

/// ECAM, then the reset tasks of socket 0's FCH ([`Init::enter`] of every
/// SoC of the socket).
pub(crate) fn enter<S: Init<Hw = Hw>>(config: &Config) -> Entered<S> {
    let identity = identify();
    let generation = Generation::of(identity.silicon());
    step(0x10, "ECAM");
    let pci_root = hw::enable_ecam(generation);
    let timer = hw::tsc();
    let fch = hw::primary_fch(timer);
    step(0x11, "FCH reset tasks");
    // There is no console yet; the POST code shows where.
    let resume = steps::fch_reset(&fch, generation.silicon(), &config.policy)
        .unwrap_or_else(|what| parts::fail(what));
    Entered {
        hw: Hw {
            identity,
            pci_root,
            timer,
            fch,
            resume,
        },
        clock: Some(timer),
        pre_init: Ok(()),
    }
}

/// The identified CPU, logged with the console up: [`Init::identify`].
pub(crate) fn check_identity(hw: &Hw) -> Result<Identity, &'static str> {
    let generation = Generation::of(hw.identity.silicon());
    fstart_log::info!("sp3: {} console ready", generation.name());
    if hw.resume {
        // S3 needs the resume path (boot scripts, wake vector); not built.
        fstart_log::error!("sp3: S3 resume is not supported, cold booting");
    }
    step(0x13, "CPU identity");
    fstart_log::info!(
        "sp3: CPU {} ({:#x}), TSC {} MHz",
        hw.identity.revision.name(),
        hw.identity.signature,
        hw.timer.mhz()
    );
    Ok(hw.identity)
}

/// The socket's silicon context with the fabric discovered, its buses,
/// MMIO and I/O planned and routed by the rules of DF version `V`
/// ([`steps::data_fabric`]): the first part of every SoC's
/// [`Init::init_fch`].
pub(crate) fn fabric<V: Version, S: Init<Hw = Hw, Config = Config>>(
    ctx: &Ctx<'_, S>,
) -> Result<silicon::Ctx, &'static str> {
    let apob = *ctx.memory.apob.ok_or("APOB")?;
    let mut soc = silicon::Ctx::new(
        ctx.hw.identity,
        ctx.hw.timer,
        ctx.hw.pci_root,
        hw::primary_fch(ctx.hw.timer),
        ctx.config.policy,
        apob,
        DramTops {
            tom: ctx.memory.top_mem,
            tom2: ctx.memory.top_mem2,
        },
    );
    step(0x20, "DF topology and maps");
    steps::data_fabric::<V>(&mut soc)?;
    Ok(soc)
}

/// Every root bridge the DF routes, each within its own buses and windows,
/// from the plan the fabric step programmed.
pub(crate) fn roots<F>(state: &State<F>) -> Option<Roots> {
    (state.soc.plan.root_bridges.iter())
        .map(|rb| fstart_driver_amd::df::root::planned(rb, &state.soc.pci_root))
        .collect()
}

impl<F> State<F> {
    /// What the tables and the handoff read.
    #[cfg_attr(not(feature = "payload-uefi"), allow(unused_variables))]
    fn view<'a>(&'a self, flow: &'a Flow) -> parts::Ctx<'a> {
        parts::Ctx {
            soc: &self.soc,
            #[cfg(feature = "payload-uefi")]
            ffs: flow.ffs,
            #[cfg(feature = "payload-uefi")]
            ffs_offset: flow.ffs_offset,
            #[cfg(feature = "payload-uefi")]
            options: &flow.options,
            smm: self.smm,
        }
    }
}

/// The flash as this boot sees it, the APOB its BIOS directory names,
/// the stored options over the board's policy, the memory map with
/// TSEG, the TPM, then the FFS the flash anchor names.
pub(crate) fn map<S: crate::flow::Soc<Flow = Flow>>(
    hw: &mut Hw,
    config: &mut Config,
    board: &crate::board::Facts,
    msr: &NativeMsr,
) -> Result<Mapped<S>, &'static str> {
    let generation = Generation::of(hw.identity.silicon());
    step(0x14, "APOB");
    let parts::Flash {
        efs: flash,
        ffs,
        #[cfg(feature = "payload-uefi")]
        ffs_offset,
        size: rom_size,
        nvram,
    } = parts::flash(&hw.fch, generation);
    let (apob, apob_span) = parts::read_apob(flash, generation);
    // The console level first, then the policy overrides, before the
    // first step that reads the policy after the FCH reset tasks (the
    // DF plan).
    let mut options = fstart_options::Report::new();
    let mut level = fstart_log::max_level();
    parts::options::resolve(
        nvram,
        &fstart_log::option::KNOBS,
        &mut level,
        &mut options,
        |_| Ok(()),
    );
    // SAFETY: BSP only, before MP.
    unsafe { fstart_log::set_max_level(level) };
    parts::options::resolve(
        nvram,
        config.options,
        &mut config.policy,
        &mut options,
        |_| Ok(()),
    );

    step(0x15, "memory map");
    let (tom, tom2) = fstart_driver_amd::ccx::msr::memory_limits(msr).map_err(|_| "TOP_MEM")?;
    let tops = crate::memory::TopOfMemory { tom, tom2 };
    let regions = parts::memory_regions(&apob, generation);
    let tseg = crate::memory::place_tseg(
        &regions,
        tops.tom,
        super::smm::TSEG_SIZE,
        &[layout::LAYOUT.early, apob_span],
    )
    .ok_or("TSEG placement")?;
    let reservations = super::memory::reservations(generation, apob_span, Some(tseg));
    let e820 = crate::memory::build("sp3", &regions, tops, &reservations).map_err(|_| "e820")?;
    parts::cache_ram(&regions);

    if board.discrete_tpm {
        step(0x16, "TPM");
        parts::tpm::start(flash, generation);
    }

    step(0x48, "boot media");
    crate::media::mount(e820.entries(), &layout::LAYOUT, ffs).map_err(|error| error.step())?;

    Ok(Mapped {
        top_mem: tops.tom,
        top_mem2: tops.tom2,
        apob: Some(apob),
        tseg: tseg.base,
        e820,
        ffs: ffs.base..ffs.end(),
        flow: Flow {
            #[cfg(feature = "payload-uefi")]
            ffs,
            #[cfg(feature = "payload-uefi")]
            ffs_offset,
            nvram,
            options,
            tseg,
            rom_size,
        },
    })
}

/// EINJ's scratch pages, selected with the live routing map.
pub(crate) fn smm_memory<F>(
    e820: &fstart_core::services::memory_detect::E820State,
    state: &mut State<F>,
) -> Result<Reserved, &'static str> {
    step(0x58, "SMM");
    let mut reserved = Reserved::new();
    state.einj = parts::plan_einj(&state.soc, e820, &mut reserved);
    Ok(reserved)
}

/// TSEG, SMBASE and lock on every thread with the FCH handler image, then
/// the PSP SmmInfo registration from SMM (PSP_APOB §3.3).
///
/// Order (AMD PPR 55803 §2.1.12.1, MSRC001_0111..0113, C001_0015): FCH
/// SMIs off; TValid/AValid cleared so TSEG is plain DRAM while the BSP
/// copies the handler and flushes it; then every thread (APs first, BSP
/// last) writes SMM_ADDR, SMM_MASK (TValid, WB), its own SMM_BASE and
/// sets HWCR.SmmLock, which makes all of them read-only. SMM_ADDR and
/// SMM_MASK are per core (both SMT threads write the same values), SMM_BASE
/// and HWCR per thread. Then the FCH sources, one self-SMI per thread
/// through the new entry, the PSP SMI route and SW SMI D0 (SmmInfo).
pub(crate) fn install_smm<F>(
    board: &crate::board::Facts,
    state: &mut State<F>,
    mp: &MpHandle,
    flow: &Flow,
    (usb_buses, usb_bus_count): ([u8; 8], u8),
) -> Result<(), &'static str> {
    let einj = state.einj.unwrap_or_default();
    parts::clear_einj_scratch(&einj);
    let soc = &state.soc;
    let config = super::smm::SmmConfig {
        generation: Generation::of(soc.generation),
        ecam_base: soc.pci_root.ecam_base,
        psp_bar: soc.psp_bar as u32,
        einj,
        usb_buses,
        usb_bus_count,
        bmc: board.bmc_sx_notify,
    };
    if let Some(image) = crate::tseg::IMAGE {
        parts::tpm::measure_smm(image);
    }
    // The handler's page tables map the low 4 GiB only; a PSP BAR
    // above would fault in SMM at SmmInfo, after the lock.
    if soc.psp_bar == 0 || soc.psp_bar >= crate::memory::FOUR_GIB {
        return Err("PSP BAR for SMM");
    }
    let smi = super::smm::Sp3Smi {
        fch: &soc.fch,
        config,
    };
    crate::tseg::install("sp3", mp, &smi, flow.tseg.base, flow.tseg.size as u32)?;
    super::smm::prepare_psp_smi(&soc.fch);
    fstart_driver_amd::fch::hw::smi::software_smi(soc.fch.io(), super::smm::sw::PSP_SMM_INFO);
    state.smm = Some(config);
    state.soc.smm_installed = true;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn tables<S: crate::flow::Soc<State = State<F>>, F>(
    board: &crate::board::Facts,
    silicon: &mut Up<S>,
    (numa, nbios): (Option<NumaPlan>, &[Nbio<hw::Pci>]),
    pci: &fstart_pci::PciEcam,
    flow: &Flow,
    smbios: &fstart_acpi::smbios::SmbiosIdentity<'_>,
    acpi: crate::acpi::Board,
) -> Tables {
    step(0x70, "ACPI");
    let state = &silicon.state;
    let view = state.view(flow);
    let rsdp = parts::handoff::acpi(&view, numa, nbios, pci, &acpi, board.bmc_sx_notify);
    let smbios = super::smbios::install(&view, &silicon.cpus, flow.rom_size, smbios, pci);
    Tables {
        rsdp: Some(rsdp),
        smbios: Some(smbios),
        s5: fstart_core::description::AcpiSleepControl::new(
            super::acpi::of(Generation::of(state.soc.generation))
                .ports
                .control,
            crate::acpi::S5_SLEEP_TYPE,
        ),
    }
}

/// CrabEFI keeps its variables, the option overrides among them, in the
/// settings store, and shows the options in its menu.
#[cfg_attr(not(feature = "payload-uefi"), allow(unused_mut, unused_variables))]
pub(crate) fn launch<F>(
    mut handoff: fstart_payload::Handoff<'_>,
    state: &State<F>,
    flow: &Flow,
) -> ! {
    step(0x90, "payload");
    fstart_log::flush();
    #[cfg(feature = "payload-uefi")]
    let view = state.view(flow);
    #[cfg(feature = "payload-uefi")]
    {
        type Storage = fstart_payload::crabefi::settings::FlashStorage<
            fstart_driver_amd::fch::spi::flash::Flash<hw::Tsc>,
        >;
        static mut STORAGE: Option<Storage> = None;
        // SAFETY: firmware BSS, written once here on the BSP; the
        // payload keeps the store for as long as it runs.
        let storage = unsafe { &mut *core::ptr::addr_of_mut!(STORAGE) };
        *storage = parts::options::storage(&view);
        handoff.settings = Some(parts::options::settings(&view));
        handoff.variable_storage = storage
            .as_mut()
            .map(|s| s as &mut dyn fstart_payload::crabefi::StorageBackend);
    }
    fstart_payload::boot(handoff, ())
}
