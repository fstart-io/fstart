// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stage phases: the PSP released the BSP at the reset vector of the
//! image it copied to DRAM. The arch crate's shared `amd_psp_reset.S` opens a
//! write-back window over the firmware RAM, builds the identity map, enters
//! long mode and continues at the architectural `_start_ram` (stack, BSS,
//! IDT), which calls the board's `main` (`fstart_platform_amd::entry!`).
//!
//! The board's `main` drives the phases and does its own work between them.
//! Each phase consumes the previous one ([`Stage`]'s second parameter), so
//! they run once and in order:
//!
//! ```ignore
//! let stage = noncar::start::<Board>();           // ECAM, FCH pre-init
//! // board: LPC decodes for its console
//! let stage = stage.console(&CONSOLE, console)    // memory map, FFS
//!     .init_fch();                                // DF, FCH
//! // board: devices on the FCH's buses, early pads
//! let stage = stage.train_dxio(/* lanes */);
//! // board: links are trained
//! let stage = stage.init_chipset(/* what the board found */)
//!     .start_cpus()                               // MP, SMM
//!     .scan_bus();
//! // board: after PCI enumeration
//! let stage = stage.ready_to_boot();
//! // board: right before the tables
//! stage.emit_tables(&SMBIOS, board_acpi).boot()
//! ```
//!
//! What the silicon needs in each phase is the SoC driver's
//! ([`fstart_driver_amd::soc::Init`]); the phases are the same for every
//! SoC.
use super::{
    Board, Ctx, Memory, Silicon, Soc, Stop, acpi, boot, layout::Region, memory, mp, pci, post, psp,
    smm,
};
use crate::acpi::BoardAcpi;
use fstart_arch::x86::{cpu::msr::NativeMsr, ioapic::IoApic};
use fstart_core::services::memory_detect::E820State;
use fstart_driver_amd::{
    apob::Apob,
    ccx::{cache, msr, tsc::Tsc},
    df::{self, Die, map},
    fch::{self, Acpimmio, Fch, NativeBus, gpio::Gpio, lpc::Lpc},
};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::{Hex, StaticConsole};
use fstart_pci::{PciEcam, ecam::GlobalEcam};
use heapless::Vec;

/// The shared AMD PSP reset entry with the selected SoC's geometry; a
/// reset image is for one SoC.
mod entry {
    #[cfg(all(feature = "picasso", feature = "zeppelin"))]
    compile_error!("a noncar reset image is for one SoC: enable picasso or zeppelin, not both");
    #[cfg(feature = "picasso")]
    use crate::picasso::LAYOUT;
    #[cfg(feature = "zeppelin")]
    use crate::zeppelin::layout::LAYOUT;
    core::arch::global_asm!(
        include_str!("../../../arch/asm/amd_psp_reset.S"),
        cache_base = const LAYOUT.cache_window().base,
        cache_mask = const !(LAYOUT.cache_window().size - 1) & 0xffff_ffff,
        pdpts = const crate::noncar::layout::PDPT_COUNT,
        options(att_syntax),
    );
}

fn halt() -> ! {
    fstart_arch::x86::boot::halt()
}

/// Stop the boot: log why and halt.
pub fn stop<S: Soc>(stop: Stop) -> ! {
    match stop {
        Stop::Failed(what) => fstart_log::error!("{}: {} failed, halting", S::NAME, what),
        Stop::Checkpoint(what) => {
            fstart_log::info!("{}: {} checkpoint reached, halting", S::NAME, what)
        }
    }
    halt()
}

fn fail<S: Soc>(what: &'static str) -> ! {
    stop::<S>(Stop::Failed(what))
}

fn apob_bytes<S: Soc>() -> &'static [u8] {
    // SAFETY: the PSP wrote the APOB here before releasing the BSP and the
    // memory map keeps it reserved.
    unsafe { core::slice::from_raw_parts(S::APOB.base as *const u8, S::APOB.size as usize) }
}

/// The board's first phase: ECAM and the SoC's FCH setup before the
/// console, from the board's configuration of the SoC.
pub fn start<B: Board>() -> Early<B::Soc> {
    Early::enter(B::CONFIG)
}

/// Right after the reset vector: ECAM and the FCH decode the console needs.
pub struct Early<S: Soc> {
    config: &'static S::Config,
    fch: Fch<NativeBus>,
    msr: NativeMsr,
    mhz: u32,
    clock: Option<Tsc>,
    pre_init: Result<(), &'static str>,
}

impl<S: Soc> Early<S> {
    /// ECAM first, everything else uses it; then the ACPI MMIO and the
    /// SoC's FCH setup for the console.
    fn enter(config: &'static S::Config) -> Self {
        fstart_stage::anchor::initial_stage();
        // SAFETY: BSP only, single-threaded cold boot; the ECAM comes up
        // before the first config space access.
        let fch = Fch::new(unsafe { NativeBus::new() }, Acpimmio::DIE0);
        post(&fch, 0x30);
        // SAFETY: BSP only, single-threaded cold boot.
        let msr = unsafe { NativeMsr::new() };
        msr::enable_ecam(&msr, S::ECAM, S::ECAM_BUSES);
        fstart_pci::ecam::init(S::ECAM as usize);
        // SAFETY: exclusive FCH programming before the console.
        unsafe { fch::enable_acpimmio() };
        let mhz = fstart_arch::x86::cpu::amd::msr::tsc_mhz(&msr).unwrap_or(0);
        let clock = core::num::NonZeroU32::new(mhz).map(Tsc::new);
        // A failure is reported once there is a console.
        let pre_init = S::fch_pre_init(&fch, config, clock);
        Self {
            config,
            fch,
            msr,
            mhz,
            clock,
            pre_init,
        }
    }

    /// The LPC bridge, for decodes and Super I/O routing of a console
    /// behind it.
    pub fn lpc(&self) -> Lpc<&NativeBus> {
        // SAFETY: exclusive LPC decode programming before the console.
        unsafe { Lpc::new(self.fch.bus()) }
    }

    /// Bring up the console, identify the CPU, build the memory map from the
    /// APOB, cache it and mount the verified FFS.
    pub fn console(
        self,
        slot: &'static StaticConsole<Ns16550>,
        config: fstart_driver_uart::ns16550::Ns16550Config,
    ) -> Stage<S, Mapped> {
        let Self {
            config: soc_config,
            fch,
            msr,
            mhz,
            clock,
            pre_init,
        } = self;
        post(&fch, 0x31);
        let Ok(mut console) = Ns16550::new(config) else {
            halt()
        };
        if console.init().is_err() {
            halt()
        }
        let console = slot.install(console);
        if let Err(what) = pre_init {
            fail::<S>(what);
        }

        let pci = GlobalEcam;
        let dies = df::die_count(&pci);
        let identity = match S::identify(&fch, soc_config, mhz, dies) {
            Ok(identity) => identity,
            Err(what) => fail::<S>(what),
        };
        let Some(clock) = clock else {
            fail::<S>("CPU identification")
        };
        let Ok((top_mem, top_mem2)) = msr::memory_limits(&msr) else {
            fail::<S>("TOP_MEM from the PSP")
        };
        let apob = Apob::new(apob_bytes::<S>(), S::PSP).ok();
        let map = apob.as_ref().and_then(|apob| {
            let map = fstart_driver_amd::apob::sysmap::first_system_memory_map(apob).ok()?;
            map.sorted().ok()
        });
        let Some(map) = map else { fail::<S>("APOB") };
        fstart_log::info!(
            "{}: {} die(s), TOM {}, TOM2 {}, APOB top {}",
            S::NAME,
            dies,
            Hex(top_mem),
            Hex(top_mem2.unwrap_or(0)),
            Hex(map.top)
        );
        let tseg_size = u64::from(S::TSEG_SIZE);
        let tseg = memory::tseg_base(&map, top_mem, tseg_size);
        let e820 = match memory::e820(&map, top_mem, tseg_size, &[S::LAYOUT.early, S::APOB]) {
            Ok(e820) => e820,
            Err(_) => fail::<S>("memory map"),
        };

        // The early cache plan: WB DRAM, WP flash.
        let address_bits = fstart_arch::x86::physical_address_bits() as u8;
        // SAFETY: CPUID reported MTRRs; the cache plan rejects CPUs without.
        let mtrrs = unsafe { fstart_arch::x86::mtrr::variable_count() } as usize;
        let installed = cache::Plan::new(memory::low_top(&map, top_mem), address_bits, mtrrs)
            .and_then(|plan| cache::install(&msr, &plan));
        if installed.is_err() {
            fail::<S>("MTRR setup");
        }
        post(&fch, 0x32);

        // The verified FFS, mounted before the silicon init: MP init loads the CPUs'
        // microcode from it.
        if let Some(ffs) = S::LAYOUT.ffs
            && boot::mount(&e820, S::LAYOUT.early, ffs).is_err()
        {
            fail::<S>("boot media");
        }

        // SAFETY: BSP-only silicon init serializes all die register accesses.
        let dies = unsafe { df::dies(pci) }.collect();
        Stage {
            machine: Machine {
                config: soc_config,
                console,
                fch,
                msr,
                clock,
                dies,
                identity,
                top_mem,
                top_mem2,
                apob,
                tseg,
                e820,
            },
            phase: Mapped,
        }
    }
}

/// What every phase after the console works on.
pub(crate) struct Machine<S: Soc> {
    config: &'static S::Config,
    console: &'static Ns16550,
    fch: Fch<NativeBus>,
    msr: NativeMsr,
    clock: Tsc,
    dies: Vec<Die<S, GlobalEcam>, { super::MAX_DIES }>,
    identity: S::Identity,
    top_mem: u64,
    top_mem2: Option<u64>,
    apob: Option<Apob<'static>>,
    tseg: u64,
    e820: E820State,
}

/// The stage after the console, in phase `P`.
pub struct Stage<S: Soc, P> {
    machine: Machine<S>,
    phase: P,
}

/// The console is up, DRAM is mapped and cached, the FFS is mounted.
pub struct Mapped;

/// The FCH's buses are up ([`fstart_driver_amd::soc::Init::init_fch`]).
pub struct FchUp<S: Soc>(S::FchUp);

/// The links are trained.
pub struct Trained<S: Soc>(S::Trained);

/// The silicon is ready for MP.
pub struct Chipset<S: Soc>(S::Chipset);

/// The silicon init is done: MP, SMM and what the SoC reports for the
/// tables.
pub struct Cpus<S: Soc>(pub(crate) Silicon<S>);

/// PCI is enumerated and its resources are allocated.
pub struct Enumerated<S: Soc> {
    silicon: Silicon<S>,
    pci: Pci,
}

/// Everything the tables describe is ready.
pub struct Ready<S: Soc> {
    silicon: Silicon<S>,
    pci: Pci,
}

/// The ACPI and SMBIOS tables are installed.
pub struct Published<S: Soc> {
    silicon: Silicon<S>,
    pci: Pci,
    rsdp: Option<u64>,
    smbios: Option<u64>,
    #[cfg(feature = "payload-uefi-basic")]
    peripherals: super::Peripherals,
}

/// PCI after enumeration and resource allocation. The allocator keeps the
/// root bridges it was built from; the tables describe those.
struct Pci {
    ecam: PciEcam,
}

impl<S: Soc> Machine<S> {
    /// What the SoC's steps work on.
    fn ctx(&self) -> Ctx<'_, S> {
        Ctx {
            config: self.config,
            identity: self.identity,
            dies: &self.dies,
            fch: &self.fch,
            msr: &self.msr,
            clock: self.clock,
            memory: Memory {
                top_mem: self.top_mem,
                top_mem2: self.top_mem2,
                apob: self.apob.as_ref(),
                tseg: self.tseg,
                tseg_size: S::TSEG_SIZE,
                e820: &self.e820,
            },
        }
    }
}

impl<S: Soc, P> Stage<S, P> {
    /// The board's configuration of the SoC.
    pub fn config(&self) -> &'static S::Config {
        self.machine.config
    }

    /// What the SoC's CPU identification found.
    pub fn identity(&self) -> S::Identity {
        self.machine.identity
    }

    /// The memory map from the APOB and the CPU's TOP_MEM/TOM2.
    pub fn memory(&self) -> Memory<'_> {
        self.machine.ctx().memory
    }

    /// The dies, die 0 first.
    pub fn dies(&self) -> &[Die<S, GlobalEcam>] {
        &self.machine.dies
    }

    /// Die 0's FCH.
    pub fn fch(&self) -> &Fch<NativeBus> {
        &self.machine.fch
    }

    /// The GPIO pads of die 0.
    pub fn gpio(&self) -> Gpio<NativeBus> {
        self.machine.ctx().gpio()
    }

    /// The calibrated clock, to pace board device transactions.
    pub fn clock(&self) -> Tsc {
        self.machine.clock
    }

    /// Stop the boot with `why`.
    pub fn stop(&self, why: Stop) -> ! {
        stop::<S>(why)
    }

    /// Stop the boot after logging that the board's `what` failed.
    pub fn fail(&self, what: &'static str) -> ! {
        fail::<S>(what)
    }
}

impl<S: Soc> Stage<S, Mapped> {
    /// The silicon up to the FCH's buses; the board's devices on them and
    /// its early pads come next.
    pub fn init_fch(self) -> Stage<S, FchUp<S>> {
        let up = S::init_fch(&self.machine.ctx()).unwrap_or_else(|what| fail::<S>(what));
        Stage {
            machine: self.machine,
            phase: FchUp(up),
        }
    }
}

impl<S: Soc> Stage<S, FchUp<S>> {
    /// Link training of the board's `lanes`; board work that needs trained
    /// links comes next.
    pub fn train_dxio(self, lanes: S::Lanes<'_>) -> Stage<S, Trained<S>> {
        let Stage {
            machine,
            phase: FchUp(up),
        } = self;
        let trained =
            S::train_dxio(&machine.ctx(), up, lanes).unwrap_or_else(|what| fail::<S>(what));
        Stage {
            machine,
            phase: Trained(trained),
        }
    }
}

impl<S: Soc> Stage<S, Trained<S>> {
    /// The rest of the silicon init before MP, with what the board learned
    /// at run time since the FCH came up (`runtime`).
    pub fn init_chipset(self, runtime: S::Runtime) -> Stage<S, Chipset<S>> {
        let Stage {
            machine,
            phase: Trained(trained),
        } = self;
        let chipset = S::init_chipset(&machine.ctx(), trained, runtime)
            .unwrap_or_else(|what| fail::<S>(what));
        Stage {
            machine,
            phase: Chipset(chipset),
        }
    }
}

impl<S: Soc> Stage<S, Chipset<S>> {
    /// MP init with the SoC's CCX init on every CPU, SMM with the PSP's SMM
    /// info, then what the SoC does before PCI enumeration.
    pub fn start_cpus(self) -> Stage<S, Cpus<S>> {
        let Stage {
            machine,
            phase: Chipset(chipset),
        } = self;
        let silicon = start_cpus(&machine, chipset).unwrap_or_else(|what| fail::<S>(what));
        Stage {
            machine,
            phase: Cpus(silicon),
        }
    }
}

fn start_cpus<S: Soc>(
    machine: &Machine<S>,
    chipset: S::Chipset,
) -> Result<Silicon<S>, &'static str> {
    let ctx = machine.ctx();
    let policy = S::cpu_policy(&ctx, &chipset)?;
    let cpus = mp::start::<S>(
        ctx.dies,
        fstart_stage::microcode_blob(),
        move |msr, thread| S::cpu_init(&policy, msr, thread),
        ctx.clock,
    )?;
    let mut state = S::cpus_up(&ctx, chipset)?;
    let tseg = smm::install(
        S::NAME,
        &cpus.mp,
        &S::smi(&ctx),
        ctx.memory.tseg,
        S::TSEG_SIZE,
    )?;
    // The PSP gets the SMM info now, LockDFReg at ready-to-boot and
    // BootDone right before the payload.
    let psp = psp::notify_smm(S::psp_bar(&state), S::PSP, &tseg, &ctx.clock);
    S::before_enumeration(&ctx, &mut state)?;
    let (devices, aml) = S::acpi_devices(ctx.config, &state)?;
    Ok(Silicon {
        cpus,
        psp,
        devices,
        aml,
        tables: alloc::vec::Vec::new(),
        framebuffer: None,
        state,
    })
}

impl<S: Soc> Stage<S, Cpus<S>> {
    /// PCI enumeration and resource allocation, write-combining for the
    /// framebuffer apertures, then what the SoC does after the allocation.
    pub fn scan_bus(self) -> Stage<S, Enumerated<S>> {
        let Stage {
            machine,
            phase: Cpus(mut silicon),
        } = self;
        let ctx = machine.ctx();
        let ecam = enumerate::<S>(&ctx, &silicon);
        // PCI resources become known after AMD's required MP and SMU steps.
        // Extend cacheability coherently on all CPUs without replacing AMD's
        // fixed DRAM routing, high-memory TOM2WB or the existing RAM/ROM MTRRs.
        let wc = ecam
            .prefetchable_vga_windows()
            .map(|window| (window.base, window.size));
        // SAFETY: BSP owns MP dispatch; interrupts are disabled, APs are online,
        // and only allocated prefetchable VGA BARs are eligible.
        if let Err(error) =
            unsafe { fstart_arch::x86::mtrr::install_write_combining(wc, &silicon.cpus.mp) }
        {
            if error == fstart_core::services::ServiceError::NotSupported {
                fstart_log::warn!(
                    "mtrr: framebuffer WC unavailable; preserving existing cache policy"
                );
            } else {
                fail::<S>("framebuffer cache policy");
            }
        }
        S::after_resources(&ctx, &mut silicon.state);
        let pci = Pci { ecam };
        Stage {
            machine,
            phase: Enumerated { silicon, pci },
        }
    }
}

impl<S: Soc> Stage<S, Enumerated<S>> {
    /// The IOAPICs, the SoC's ready-to-boot, then the display
    /// ([`Soc::init_display`]). Graphics POST may patch the ROM and
    /// contribute its ACPI; everything the tables describe is ready
    /// afterwards.
    pub fn ready_to_boot(self) -> Stage<S, Ready<S>> {
        let Stage {
            machine,
            phase: Enumerated { mut silicon, pci },
        } = self;
        let ctx = machine.ctx();
        ioapics(&ctx, &silicon);
        if let Err(what) = S::ready_to_boot(&ctx, &mut silicon.state, &pci.ecam) {
            fail::<S>(what);
        }
        if let Err(why) = S::init_display(&mut silicon, &ctx, &pci.ecam) {
            stop::<S>(why);
        }
        Stage {
            machine,
            phase: Ready { silicon, pci },
        }
    }
}

impl<S: Soc> Stage<S, Ready<S>> {
    /// Finish the SoC's ready-to-boot, then install ACPI with the board's
    /// contribution `board` and SMBIOS with its `identity`.
    pub fn emit_tables(
        self,
        identity: &crate::acpi::SmbiosIdentity<'_>,
        mut board: BoardAcpi,
    ) -> Stage<S, Published<S>> {
        let Stage {
            machine,
            phase: Ready { mut silicon, pci },
        } = self;
        let ctx = machine.ctx();
        S::end_of_post(&ctx, &mut silicon.state);
        let hosts: Vec<acpi::HostBridge, { super::MAX_DIES }> = pci
            .ecam
            .roots()
            .iter()
            .map(|root| acpi::HostBridge::new(root, &pci.ecam))
            .collect();
        board.tables.append(&mut silicon.tables);
        board.devices.extend(silicon.devices.iter().cloned());
        board.sb_aml.extend_from_slice(&silicon.aml);
        let ioapics: [u32; super::MAX_DIES] =
            core::array::from_fn(|die| S::ioapic(&silicon.state, die as u8));
        let rsdp = acpi::install(
            S::ACPI,
            ctx.dies,
            &hosts,
            &ioapics,
            S::pci_root(),
            &silicon.cpus.apic_ids,
            &board,
        );
        let smbios =
            super::smbios::install::<S>(identity, machine.apob.as_ref(), &silicon.cpus.apic_ids);
        #[cfg(feature = "payload-uefi-basic")]
        let peripherals = S::peripherals(&ctx, machine.config);
        Stage {
            machine,
            phase: Published {
                silicon,
                pci,
                rsdp,
                smbios,
                #[cfg(feature = "payload-uefi-basic")]
                peripherals,
            },
        }
    }
}

impl<S: Soc> Stage<S, Published<S>> {
    /// PSP LockDFReg, map RAM for the payload, park the APs, PSP BootDone,
    /// then the payload.
    pub fn boot(self) -> ! {
        let Stage {
            machine,
            phase:
                Published {
                    mut silicon,
                    pci,
                    rsdp,
                    smbios,
                    #[cfg(feature = "payload-uefi-basic")]
                    peripherals,
                },
        } = self;
        let delay = machine.clock;
        if let Err(what) = psp::lock_df_registers(silicon.psp.as_mut(), &delay) {
            fail::<S>(what);
        }
        post(&machine.fch, 0x43);

        let Some(ffs) = S::LAYOUT.ffs else {
            fail::<S>("boot media")
        };
        if boot::map_ram(&machine.e820, &pci.ecam).is_err() {
            fail::<S>("page tables");
        }
        if !fstart_arch::x86::mp::park_aps_for_payload() {
            fail::<S>("parking the APs");
        }
        // SAFETY: the firmware is done with the CPUs and SMM is locked.
        unsafe { psp::boot_done(silicon.psp.as_mut(), &delay) };
        fstart_log::info!("{}: starting the payload", S::NAME);
        post(&machine.fch, 0x44);
        use fstart_payload::{AcpiTables, Description, Handoff, Placement, Range};
        let mut description =
            Description::new(machine.e820.entries(), Range::new(ffs.base, ffs.size));
        description.console = Some(machine.console);
        description.acpi = rsdp.map(AcpiTables::at);
        let acpi_s5 = [fstart_core::description::AcpiSleepControl::new(
            S::ACPI.ports.control,
            crate::acpi::S5_SLEEP_TYPE,
        )];
        description.acpi_s5 = &acpi_s5;
        description.smbios = smbios;
        description.acpi_devices = &silicon.devices;
        description.framebuffer = silicon.framebuffer;
        description.pci_root = Some(S::pci_root());
        #[allow(unused_mut)]
        let mut handoff = Handoff::new(description, Placement::default());
        #[cfg(feature = "payload-uefi-basic")]
        {
            handoff.peripherals = peripherals;
        }
        fstart_payload::boot(handoff, ())
    }
}

/// PCI enumeration and resource allocation below the root complex of every
/// die, each from what the DF routes to it.
fn enumerate<S: Soc>(ctx: &Ctx<'_, S>, silicon: &Silicon<S>) -> PciEcam {
    let ecam_region = Region::new(S::ECAM, u64::from(S::ECAM_BUSES) << 20);
    let roots: Option<Vec<pci::Root, { super::MAX_DIES }>> = ctx
        .dies
        .iter()
        .map(|die| {
            let (base, size) = S::nonpci_mmio(&silicon.state, die.index());
            pci::root(
                &map::root_complex(die)?,
                ecam_region,
                Region::new(base, size),
            )
        })
        .collect();
    let Some(roots) = roots else {
        fail::<S>("PCI root")
    };
    let mut ecam = match PciEcam::new(roots) {
        Ok(ecam) => ecam,
        Err(_) => fail::<S>("PCI root"),
    };
    if ecam.enumerate_and_allocate().is_err() {
        fail::<S>("PCI enumeration");
    }
    for address in ecam.devices() {
        if let Some(device) = ecam.device(address) {
            let id = device.read32(0);
            let class = device.read32(8) >> 8;
            fstart_log::info!(
                "PCI {}:{}.{} {} class {}",
                Hex(address.bus().into()),
                Hex(address.device().into()),
                address.function(),
                Hex(id.into()),
                Hex(class.into())
            );
        }
    }
    ecam
}

/// The FCH's IOAPIC (GSI 0-23, ID 0) in virtual-wire mode, then one per
/// die's NBIO (ID 1 + die), all masked.
fn ioapics<S: Soc>(ctx: &Ctx<'_, S>, silicon: &Silicon<S>) {
    ctx.fch.enable_hpet_ioapic();
    // SAFETY: decoded IOAPIC MMIO, BSP only.
    let fch_ioapic = unsafe { IoApic::new(fch::IOAPIC as usize) };
    fch_ioapic.set_id(0);
    fch_ioapic.mask_all();
    fch_ioapic.route_i8259(silicon.cpus.apic_ids.first().copied().unwrap_or(0) as u8);
    for die in ctx.dies {
        // SAFETY: as above, mapped by the silicon init.
        let ioapic = unsafe { IoApic::new(S::ioapic(&silicon.state, die.index()) as usize) };
        ioapic.set_id(1 + die.index());
        ioapic.mask_all();
    }
}
