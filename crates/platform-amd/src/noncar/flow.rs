// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stage phases: the PSP released the BSP at the reset vector of the
//! image it copied to DRAM. `psp_reset.S` enters long mode and continues at
//! the architectural `_start_ram` (stack, BSS, data, IDT), which calls the
//! board's `main` (`fstart_platform_amd::entry!`).
//!
//! The board's `main` drives the phases and does its own work between them:
//!
//! ```ignore
//! let stage = Reset::<Picasso>::enter(&CONFIG).console(&CONSOLE, console);
//! let mut silicon = /* the SoC's silicon init phases, from &stage */;
//! let pci = stage.enumerate(&mut silicon);
//! // board: after PCI enumeration
//! stage.ready_to_boot(&mut silicon, &pci);
//! // board: right before the payload
//! stage.boot(silicon, pci, board_acpi)
//! ```
use super::{Ctx, Memory, Silicon, Soc, Stop, acpi, boot, layout::Region, memory, pci, post, psp};
use crate::acpi::BoardAcpi;
use fstart_arch::x86::{cpu::msr::NativeMsr, ioapic::IoApic};
use fstart_core::services::memory_detect::E820State;
use fstart_driver_amd::{
    apob::Apob,
    ccx::{cache, msr, tsc::Tsc},
    df::{self, Die, map},
    fch::{self, Acpimmio, Fch, NativeBus, lpc::Lpc},
};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::{Hex, StaticConsole};
use fstart_pci::{PciEcam, ecam::GlobalEcam};
use heapless::Vec;

core::arch::global_asm!(
    include_str!("../../asm/psp_reset.S"),
    ap_reset_segment = const {
        let address = fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS;
        assert!(address & 15 == 0 && address <= 0xffff0);
        address >> 4
    },
);
core::arch::global_asm!(
    ".code64",
    ".global _fstart_amd_long_mode_entry",
    "_fstart_amd_long_mode_entry:",
    "jmp _start_ram",
);

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

/// Right after the reset vector: ECAM and the FCH decode the console needs.
pub struct Reset<S: Soc> {
    config: &'static S::Config,
    fch: Fch<NativeBus>,
    msr: NativeMsr,
    mhz: u32,
    clock: Option<Tsc>,
    pre_init: Result<(), &'static str>,
}

impl<S: Soc> Reset<S> {
    /// ECAM first, everything else uses it; then the ACPI MMIO and the
    /// SoC's FCH setup for the console.
    pub fn enter(config: &'static S::Config) -> Self {
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
    ) -> Stage<S> {
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
            && boot::mount(&e820, ffs).is_err()
        {
            fail::<S>("boot media");
        }

        // SAFETY: BSP-only silicon init serializes all die register accesses.
        let dies = unsafe { df::dies(pci) }.collect();
        Stage {
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
        }
    }
}

/// The console is up, DRAM is mapped and cached, the FFS is mounted.
pub struct Stage<S: Soc> {
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

/// PCI after enumeration and resource allocation.
pub struct Enumerated {
    roots: Vec<pci::Root, { super::MAX_DIES }>,
    ecam: PciEcam,
}

impl Enumerated {
    pub fn ecam(&self) -> &PciEcam {
        &self.ecam
    }
}

impl<S: Soc> Stage<S> {
    /// What the silicon phases work on.
    pub fn ctx(&self) -> Ctx<'_, S> {
        Ctx {
            dies: &self.dies,
            fch: &self.fch,
            msr: &self.msr,
            clock: self.clock,
        }
    }

    /// The board's configuration of the SoC.
    pub fn config(&self) -> &'static S::Config {
        self.config
    }

    /// What [`Soc::identify`] found.
    pub fn identity(&self) -> S::Identity {
        self.identity
    }

    /// The memory map from the APOB and the CPU's TOP_MEM/TOM2.
    pub fn memory(&self) -> Memory<'_> {
        Memory {
            top_mem: self.top_mem,
            top_mem2: self.top_mem2,
            apob: self.apob.as_ref(),
            tseg: self.tseg,
            e820: &self.e820,
        }
    }

    /// The verified microcode patches of the FFS, for MP init.
    pub fn microcode(&self) -> Option<&'static [u8]> {
        fstart_stage::microcode_blob()
    }

    /// Stop the boot with `why`.
    pub fn stop(&self, why: Stop) -> ! {
        stop::<S>(why)
    }

    /// Stop the boot after logging that the board's `what` failed.
    pub fn fail(&self, what: &'static str) -> ! {
        fail::<S>(what)
    }

    /// PCI enumeration and resource allocation, write-combining for the
    /// framebuffer apertures, then [`Soc::after_resources`].
    pub fn enumerate(&self, silicon: &mut Silicon<S>) -> Enumerated {
        let ctx = self.ctx();
        let (roots, ecam) = enumerate::<S>(&ctx, silicon);
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
        S::after_resources(silicon, &ctx);
        Enumerated { roots, ecam }
    }

    /// The IOAPICs, then [`Soc::ready_to_boot`]. Graphics POST may patch the ROM and contribute
    /// its ACPI; everything the tables describe is ready afterwards.
    pub fn ready_to_boot(&self, silicon: &mut Silicon<S>, pci: &Enumerated) {
        let ctx = self.ctx();
        ioapics(&ctx, silicon);
        if let Err(why) = S::ready_to_boot(silicon, &ctx, &pci.ecam) {
            stop::<S>(why);
        }
    }

    /// Finish the SoC's ready-to-boot ([`Soc::end_of_post`]), publish ACPI
    /// with the board's contribution, lock the DF, map RAM for
    /// the payload, park the APs, tell the PSP and boot the payload.
    pub fn boot(
        self,
        mut silicon: Silicon<S>,
        pci: Enumerated,
        mut board: BoardAcpi,
        identity: &crate::acpi::SmbiosIdentity<'_>,
    ) -> ! {
        let ctx = self.ctx();
        S::end_of_post(&mut silicon, &ctx);
        let hosts: Vec<acpi::HostBridge, { super::MAX_DIES }> = pci
            .roots
            .iter()
            .map(|root| acpi::HostBridge::new(root, &pci.ecam))
            .collect();
        board.tables.append(&mut silicon.tables);
        board.devices.extend(silicon.devices.iter().cloned());
        board.sb_aml.extend_from_slice(&silicon.aml);
        let rsdp = acpi::install(
            silicon.acpi,
            &self.dies,
            &hosts,
            &silicon.ioapics,
            acpi::Ecam {
                base: S::ECAM,
                buses: S::ECAM_BUSES,
            },
            &silicon.cpus.apic_ids,
            &board,
        );
        let smbios =
            super::smbios::install::<S>(identity, self.apob.as_ref(), &silicon.cpus.apic_ids);
        #[cfg(feature = "payload-uefi-basic")]
        let peripherals = S::peripherals(&ctx, self.config);
        let delay = self.clock;
        if let Err(what) = psp::lock_df_registers(silicon.psp.as_mut(), &delay) {
            fail::<S>(what);
        }
        post(ctx.fch, 0x43);

        let Some(ffs) = S::LAYOUT.ffs else {
            fail::<S>("boot media")
        };
        if boot::map_ram(&self.e820, &pci.ecam).is_err() {
            fail::<S>("page tables");
        }
        if !fstart_arch::x86::mp::park_aps_for_payload() {
            fail::<S>("parking the APs");
        }
        // SAFETY: the firmware is done with the CPUs and SMM is locked.
        unsafe { psp::boot_done(silicon.psp.as_mut(), &delay) };
        fstart_log::info!("{}: starting the payload", S::NAME);
        post(ctx.fch, 0x44);
        let handoff = boot::Handoff {
            console: self.console,
            e820: self.e820,
            rsdp,
            smbios,
            ffs,
            devices: silicon.devices,
            framebuffer: silicon.framebuffer,
            #[cfg(feature = "payload-uefi-basic")]
            peripherals,
            pci_root: fstart_pci::PciRootInfo {
                segment: 0,
                ecam_base: S::ECAM,
                bus_start: 0,
                bus_end: (S::ECAM_BUSES - 1) as u8,
            },
        };
        use fstart_stage::payload::MainstagePayload;
        fstart_stage::payload::BuildSelectedPayload::boot(handoff)
    }
}

/// PCI enumeration and resource allocation below the root complex of every
/// die, each from what the DF routes to it.
fn enumerate<S: Soc>(
    ctx: &Ctx<'_, S>,
    silicon: &Silicon<S>,
) -> (Vec<pci::Root, { super::MAX_DIES }>, PciEcam) {
    let ecam_region = Region::new(S::ECAM, u64::from(S::ECAM_BUSES) << 20);
    let roots: Option<Vec<pci::Root, { super::MAX_DIES }>> = ctx
        .dies
        .iter()
        .map(|die| {
            let reserved = silicon.reserved_mmio[usize::from(die.index())];
            pci::Root::new(&map::root_complex(die)?, ecam_region, reserved)
        })
        .collect();
    let Some(roots) = roots else {
        fail::<S>("PCI root")
    };
    let mut ecam = match PciEcam::from_providers(&roots) {
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
    (roots, ecam)
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
        let ioapic = unsafe { IoApic::new(silicon.ioapics[usize::from(die.index())] as usize) };
        ioapic.set_id(1 + die.index());
        ioapic.mask_all();
    }
}
