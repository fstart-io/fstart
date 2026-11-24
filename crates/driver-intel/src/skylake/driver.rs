// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Skylake/Kaby Lake system agent: native DDR4 training in the
//! bootblock, the memory map, PCI root and SA devices in the ramstage.

use super::{
    additional_steps::OltmPolicy,
    capabilities::Capabilities,
    cold_controller,
    cold_handoff::{self, Reservation},
    cold_training::{ColdPolicy, ColdPolicyInput, ColdWorkspace},
    cpu::Cpu,
    cse::{self, DramAction, DramInitStatus},
    early_sa::{self, EcamConfig},
    memory_check::MemoryCheckWorkspace,
    memory_map::{self, ProgrammedMap},
    overrides::{self, PostSpdOverrides, PreSpdOutcome},
    phy::PhyPolicy,
    registers::{GdxcRegisters, MCHBAR_BASE, MchRegisters},
    sa_dmi, sa_graphics, sa_peg, sa_post_mem, sa_pre_mem,
    spd::{self, Dimm, SPD_BYTES},
    training_state::CalibrationState,
    uncore_prmrr,
};
use crate::{
    SpdTraining, heci,
    sunrise_point::{
        early, hsio, pcr,
        pmc::{self, MemoryBootMode},
    },
};
use fstart_arch::x86::{
    Udelay,
    boot::car_storage::{CarLease, CarSlot},
    cpu::msr::NativeMsr,
};
use fstart_core::services::{
    MemoryInit, ServiceError,
    memory_detect::{E820Entry, E820Kind, E820State, MemoryDetector},
};
use fstart_pci::{EcamDevice, PciRootInfo};
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{Readable, Writeable},
};

mod acpi;
mod runtime;

pub type SpdStorage = [[[u8; SPD_BYTES]; 2]; 2];

/// Training's CAR workspaces, taken once by [`MemoryInit::bring_up`]: only
/// the bootblock, which trains, links them.
struct ColdResources {
    training: CarLease<ColdWorkspace>,
    calibration: CarLease<CalibrationState>,
    checks: CarLease<MemoryCheckWorkspace>,
    spd: CarLease<SpdStorage>,
    scan: CarLease<sa_graphics::ScanWorkspace>,
    table: CarLease<E820State>,
}

struct ColdStorage {
    training: CarSlot<ColdWorkspace>,
    calibration: CarSlot<CalibrationState>,
    checks: CarSlot<MemoryCheckWorkspace>,
    spd: CarSlot<SpdStorage>,
    scan: CarSlot<sa_graphics::ScanWorkspace>,
    table: CarSlot<E820State>,
}

static COLD: ColdStorage = ColdStorage {
    training: CarSlot::new(),
    calibration: CarSlot::new(),
    checks: CarSlot::new(),
    spd: CarSlot::new(),
    scan: CarSlot::new(),
    table: CarSlot::new(),
};

unsafe fn initialize_spd(destination: *mut SpdStorage) {
    // Zero bytes are an unread SPD, never a valid DIMM.
    unsafe { core::ptr::write_bytes(destination, 0, 1) };
}

unsafe fn initialize_table(destination: *mut E820State) {
    unsafe { destination.write(E820State::new()) };
}

impl ColdResources {
    /// # Safety
    /// The bootblock on the BSP, in CAR; the leases end with the stage.
    unsafe fn take() -> Option<Self> {
        unsafe {
            Some(Self {
                training: COLD
                    .training
                    .take_initialized(ColdWorkspace::initialize_at)?,
                calibration: COLD
                    .calibration
                    .take_initialized(CalibrationState::initialize_at)?,
                checks: COLD
                    .checks
                    .take_initialized(MemoryCheckWorkspace::initialize_at)?,
                spd: COLD.spd.take_initialized(initialize_spd)?,
                scan: COLD
                    .scan
                    .take_initialized(sa_graphics::ScanWorkspace::initialize_at)?,
                table: COLD.table.take_initialized(initialize_table)?,
            })
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub ecam: EcamConfig,
    /// Physical channel, then physical DIMM slot; zero means not wired.
    pub spd_addresses: [[u8; 2]; 2],
    pub phy: PhyPolicy,
    pub overrides: PostSpdOverrides,
    pub platform_tck_min_fs: u32,
    pub dram_odt: bool,
    pub ca_vref_config: u8,
    pub dummy_reads: bool,
    pub thermal: OltmPolicy,
    pub memory_map: memory_map::Policy,
    pub prmrr: uncore_prmrr::Policy,
    pub controller: cold_controller::Policy,
    /// Gen1-only is replaced by the actual HSIO producer's decision at runtime.
    pub dmi: sa_dmi::Policy,
    pub peg: sa_peg::Policy,
    pub graphics: sa_graphics::Policy,
    pub post_memory: sa_post_mem::Policy,
    pub cio2_enabled: bool,
    pub pavp_enabled: bool,
    /// Native package policy, applied after actual BIOS reset completion.
    pub package_power: super::package_power::Policy,
    /// Native display-audio frequency/2T policy; None when HDA is disabled.
    pub display_audio: Option<(bool, bool)>,
    /// Board's native scanout policy and verified VBT asset.
    pub display: Option<crate::igd::IgdDisplayPolicy>,
    pub vbt: crate::igd::VbtSource,
    /// The CPU cores' policy.
    pub cpu: fstart_arch::x86::cpu::intel::skylake::Policy,
    /// Board memory the E820 map marks besides the chipset's own.
    pub reservations: &'static [Reservation],
    /// Platform-approved free PCI I/O allocation interval, exclusive end.
    pub pci_io: (u16, u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    AlreadyAttempted,
    Service(ServiceError),
    Early(early_sa::Error),
    Pcr(pcr::Error),
    Hsio(hsio::Error),
    Cpu(super::Error),
    Pmc(pmc::DecodeError),
    Txt(crate::txt::Error),
    Cse(cse::Error),
    PreMem(sa_pre_mem::Error),
    Overrides(super::Error),
    Spd(super::Error),
    Map(memory_map::Error),
    Prmrr(uncore_prmrr::Error),
    Handoff(cold_handoff::Error),
    WarmResetRequired,
    MeAction(DramAction),
    PackagePower(super::package_power::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Before training (bootblock) or memory detection (later stages).
    New,
    Running,
    /// Trained in this stage.
    Ready,
    /// The memory map read back from the programmed controller.
    Observed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaPhase {
    Fresh,
    Devices,
    Running,
    PostMemory,
    Locked,
    Failed,
}

pub struct IntelSkylake {
    config: &'static Config,
    sa: Option<EcamDevice>,
    sa_phase: SaPhase,
    resources: Option<ColdResources>,
    pre_memory: Option<early::PreMemory>,
    scanout: Option<crate::igd::IgdAddresses>,
    display: crate::igd::IgdDisplay,
    memory_info: Option<fstart_core::memory_info::MemoryInfo>,
    state: State,
    map: Option<ProgrammedMap>,
    failure: Option<Error>,
    #[cfg(not(target_os = "none"))]
    host_baseline: bool,
}
impl IntelSkylake {
    /// Logical CPU count for the host ACPI baseline check.
    #[cfg(not(target_os = "none"))]
    pub const HOST_CPUS: usize = 4;

    pub fn new(config: &'static Config) -> Result<Self, Error> {
        Self::validate_config(config)?;
        Ok(Self {
            config,
            sa: None,
            sa_phase: SaPhase::Fresh,
            resources: None,
            pre_memory: None,
            scanout: None,
            display: crate::igd::IgdDisplay::new(),
            memory_info: None,
            state: State::New,
            map: None,
            failure: None,
            #[cfg(not(target_os = "none"))]
            host_baseline: false,
        })
    }

    /// Populate synthetic host-side state so `check-acpi` can assemble the
    /// DSDT, MCFG, and DMAR without touching live PCI/MMIO/MSR hardware.
    #[cfg(not(target_os = "none"))]
    pub fn set_host_acpi_baseline(&mut self) {
        self.host_baseline = true;
    }

    /// The boot mode and HSIO state the PCH captured before memory init,
    /// before it cleared the wake status.
    pub fn set_pre_memory(&mut self, context: early::PreMemory) {
        self.pre_memory = Some(context);
    }

    /// Pure shape/resource validation; does not approve RNG or touch hardware.
    pub fn validate_config(config: &Config) -> Result<(), Error> {
        config
            .package_power
            .validate()
            .map_err(Error::PackagePower)?;
        if config.controller.txt_active
            || config.prmrr.features_enabled
            || config.ecam != config.dmi.ecam
            || config.post_memory.dmi.ecam != config.ecam
            || config.post_memory.dmi.pch != config.dmi.pch
            || config
                .post_memory
                .peg_max_payload
                .iter()
                .any(|value| *value > 2)
            || config.platform_tck_min_fs == 0
            || config.pci_io.1 > 0x1_0000
            || u32::from(config.pci_io.0) >= config.pci_io.1
        {
            return Err(Error::Policy);
        }
        config.ecam.encoded_pciexbar().map_err(Error::Early)?;
        config.peg.validate().map_err(|_| Error::Policy)?;
        config.graphics.validate().map_err(|_| Error::Policy)?;
        if config.peg.devices != config.post_memory.devices
            || (config.pavp_enabled || config.display_audio.is_some())
                && !config.graphics.igd_enabled
            || config.peg.devices.igd != config.graphics.igd_enabled
        {
            return Err(Error::Policy);
        }
        let mut used = 0u8;
        for address in config.spd_addresses.iter().flatten().copied() {
            if address == 0 {
                continue;
            }
            if !(0x50..=0x57).contains(&address) {
                return Err(Error::Policy);
            }
            let bit = 1 << (address - 0x50);
            if used & bit != 0 {
                return Err(Error::Policy);
            }
            used |= bit;
        }
        if used == 0 {
            return Err(Error::Policy);
        }
        Ok(())
    }

    pub fn failure(&self) -> Option<&Error> {
        self.failure.as_ref()
    }

    pub fn programmed_map(&self) -> Option<&ProgrammedMap> {
        if matches!(self.state, State::Ready | State::Observed) {
            self.map.as_ref()
        } else {
            None
        }
    }

    /// The E820 map training built, in the stage that trained.
    pub fn memory_table(&self) -> Result<&E820State, ServiceError> {
        let map = self.programmed_map().ok_or(ServiceError::NotInitialized)?;
        map.validate_live(&EcamDevice::new(0, 0, 0))
            .map_err(|_| ServiceError::HardwareError)?;
        let table = &self
            .resources
            .as_ref()
            .ok_or(ServiceError::NotSupported)?
            .table;
        if table.count() == 0 || table.total_ram() == 0 {
            return Err(ServiceError::NotInitialized);
        }
        Ok(table)
    }

    /// The memory map as the controller was programmed, with the CSE's UMA
    /// from its HECI shadow: what stages after training detect.
    fn observe(&self) -> Result<(ProgrammedMap, cse::UmaRequest), ServiceError> {
        let sa = self.sa.as_ref().ok_or(ServiceError::NotInitialized)?;
        let heci = EcamDevice::new(0, 0x16, 0);
        let status = cse::FirmwareStatus::read(&heci).map_err(|_| ServiceError::HardwareError)?;
        let uma = if status.is_debug_mode() {
            cse::UmaRequest::zero()
        } else {
            cse::UmaRequest::from_shadow(heci.read32(0x44))
                .map_err(|_| ServiceError::HardwareError)?
                .ok_or(ServiceError::NotInitialized)?
        };
        let msr = unsafe { NativeMsr::new() };
        let prmrr = uncore_prmrr::Request::read(&msr, self.config.prmrr)
            .map_err(|_| ServiceError::HardwareError)?;
        let map =
            ProgrammedMap::observe_live(sa, self.config.ecam, uma, self.config.memory_map, prmrr)
                .map_err(|_| ServiceError::HardwareError)?;
        Ok((map, uma))
    }

    fn train(&mut self, input: SpdTraining<'_>) -> Result<Option<usize>, Error> {
        use fstart_log::Hex;
        fstart_log::debug!("DRAM: validate cold boot context");
        if self.state != State::New {
            return Err(Error::AlreadyAttempted);
        }
        let context = self.pre_memory.ok_or(Error::Policy)?;
        let mode = context.power().mode();
        let config = self.config;
        if mode != MemoryBootMode::Cold || config.dmi.pch != context.hsio().kind() {
            return Err(Error::Policy);
        }
        context
            .hsio()
            .validate_current(&EcamDevice::new(0, 0x1f, 0))
            .map_err(Error::Hsio)?;
        self.state = State::Running;
        // SAFETY: MemoryInit runs once, in the bootblock on the BSP.
        let cold = self
            .resources
            .insert(unsafe { ColdResources::take() }.ok_or(Error::AlreadyAttempted)?);
        fstart_log::debug!("DRAM: bind system agent ECAM");
        let sa = unsafe { early_sa::bootstrap_ecam(config.ecam) }.map_err(Error::Early)?;
        self.sa = Some(sa);
        let signature = fstart_arch::x86::cpuid(1).0;
        fstart_log::debug!("DRAM: CPU signature {}", Hex(signature.into()));
        let cpu = Cpu::from_signature(signature).map_err(Error::Cpu)?;
        let msrs = unsafe { NativeMsr::new() };
        let mut delay = Udelay;
        let pmc = unsafe { pmc::PmcRegisters::at(pmc::PWRM_BASE) };
        let reset = pmc
            .capture_reset_state(&EcamDevice::new(0, 0x1f, 2))
            .map_err(Error::Pmc)?;
        fstart_log::debug!(
            "DRAM: GEN_PMCON_A={} GEN_PMCON_B={}",
            Hex(reset.gen_pmcon_a.into()),
            Hex(reset.gen_pmcon_b.into())
        );
        if MemoryBootMode::from_power_state(reset.gen_pmcon_a, false) != MemoryBootMode::Cold {
            return Err(Error::Policy);
        }
        // DRAM still in self-refresh means a warm reset the PCH turned into a
        // cold boot: tell the CSE its memory was not kept (native
        // romstage_native.c DID_INIT_STAT_MEM_NOT_PRESERVED).
        let did_status =
            if LocalRegisterCopy::<u32, pmc::GenPmconA::Register>::new(reset.gen_pmcon_a)
                .is_set(pmc::GenPmconA::MEM_SR)
            {
                DramInitStatus::MemoryNotPreserved
            } else {
                DramInitStatus::Success
            };
        fstart_log::debug!("DRAM: check TXT/PTT state and unlock memory");
        unsafe { crate::txt::unlock_inactive_memory(&mut delay) }.map_err(Error::Txt)?;
        fstart_log::debug!("DRAM: enable system agent, PAM/TSEG and VT-d decodes");
        early_sa::enable_pci_decodes_cold(&sa, config.ecam, mode).map_err(Error::Early)?;
        let mch = unsafe { MchRegisters::at(MCHBAR_BASE) };
        early_sa::prepare_auxiliary_pam_tseg_cold(&sa, mch, config.ecam, mode)
            .map_err(Error::Early)?;
        let p2sb = EcamDevice::new(0, 0x1f, 1);
        early_sa::prepare_vtd_decodes_cold(
            &sa,
            mch,
            &p2sb,
            config.ecam,
            mode,
            config.graphics.igd_enabled,
        )
        .map_err(Error::Early)?;
        let pcr = unsafe { pcr::Pcr::at(pcr::DEFAULT_BASE as usize) }.map_err(Error::Pcr)?;
        let resources = unsafe {
            sa_peg::Resources::from_ecam(
                PciRootInfo {
                    segment: 0,
                    ecam_base: config.ecam.base,
                    bus_start: 0,
                    bus_end: (config.ecam.buses - 1) as u8,
                },
                usize::from(config.ecam.buses) << 20,
                mch,
            )
        }
        .map_err(|_| Error::Policy)?;
        let dmi = unsafe { sa_dmi::Dmi::at(early_sa::DMI_BASE as usize, early_sa::DMI_BASE) }
            .map_err(|_| Error::Policy)?;
        fstart_log::debug!("DRAM: initialize HECI and wait for CSE CPU replacement status");
        let heci = EcamDevice::new(0, 0x16, 0);
        // SAFETY: the cold owner reserves the native UC HECI1 page and engine.
        unsafe { cse::assign_bar(&heci, cse::HECI1_BASE) }.map_err(Error::Cse)?;
        let mut cse =
            unsafe { cse::MmioCse::bind(heci, heci::Budgets::SOURCE) }.map_err(Error::Cse)?;
        let me_status = cse::FirmwareStatus::read(&heci).map_err(Error::Cse)?;
        let replacement = if me_status.is_debug_mode() {
            fstart_log::warn!(
                "DRAM: ME in Debug mode; skipping HECI init and CPU replacement wait"
            );
            cse::CpuReplacement {
                replaced: false,
                dynamic_fusing_warm_reset: false,
            }
        } else {
            cse.initialize(&mut delay).map_err(Error::Cse)?;
            cse::wait_cpu_replacement(&heci, 50, &mut delay).map_err(Error::Cse)?
        };
        if replacement.dynamic_fusing_warm_reset {
            return Err(Error::WarmResetRequired);
        }
        let premem = sa_pre_mem::Policy {
            dmi: sa_dmi::Policy {
                hsio_gen1_only: context.hsio().dmi_gen1_only(),
                ..config.dmi
            },
            peg: config.peg,
            graphics: config.graphics,
            oc_watchdog_control: unsafe { fstart_core::pio::inl(pmc::ACPI_BASE + 0x54) },
        };
        fstart_log::debug!("DRAM: system agent pre-memory setup (DMI, PEG and IGD)");
        match sa_pre_mem::init(
            &resources,
            &dmi,
            &pcr,
            cpu,
            mode,
            premem,
            &msrs,
            &mut cse,
            &mut cold.scan,
            &mut delay,
        )
        .map_err(Error::PreMem)?
        {
            sa_pre_mem::Outcome::Complete(_) => {}
            sa_pre_mem::Outcome::WarmResetRequested(_) => return Err(Error::WarmResetRequired),
        }
        let caps = Capabilities::read().map_err(Error::Cpu)?;
        fstart_log::debug!(
            "DRAM: apply pre-SPD overrides; CAPID0_A={}",
            Hex(caps.capid_a.into())
        );
        if overrides::pre_spd(mch, &mut delay, caps.capid_a, [0, 1, 2, 2])
            .map_err(Error::Overrides)?
            == PreSpdOutcome::WarmResetRequired
        {
            return Err(Error::WarmResetRequired);
        }
        // Keep physical channel/slot numbering; only a real SMBus NAK is absent.
        let mut present = [[false; 2]; 2];
        for ch in 0..2 {
            for slot in 0..2 {
                let address = config.spd_addresses[ch][slot];
                if address == 0 {
                    continue;
                }
                fstart_log::debug!("DRAM: read SPD CH{} DIMM{} at {:#04x}", ch, slot, address);
                match spd::read(input.smbus, address, &mut cold.spd[ch][slot]) {
                    Ok(()) => present[ch][slot] = true,
                    Err(ServiceError::NoDevice) => {
                        fstart_log::debug!("DRAM: no DIMM at CH{} DIMM{}", ch, slot);
                    }
                    Err(error) => return Err(Error::Service(error)),
                }
            }
        }
        // Do not set SPD_WD before the last DDR4 page-select/offset write.
        let smbus = EcamDevice::new(0, 0x1f, 4);
        if smbus.vendor_id() != 0x8086 {
            return Err(Error::Policy);
        }
        let host_config = unsafe { smbus.register::<u8, ()>(0x40) };
        host_config.set(host_config.get() | 0x10);
        if host_config.get() & 0x10 == 0 {
            return Err(Error::Policy);
        }
        let mut dimms = [[None; 2]; 2];
        for ch in 0..2 {
            for slot in 0..2 {
                if present[ch][slot] {
                    let dimm = Dimm::decode(&cold.spd[ch][slot]).map_err(|error| {
                        fstart_log::error!("DRAM: SPD decode failed for CH{} DIMM{}", ch, slot);
                        Error::Spd(error)
                    })?;
                    fstart_log::debug!(
                        "DRAM: CH{} DIMM{}: {} MiB, {} ranks, x{}, rank 1 mirrored={}",
                        ch,
                        slot,
                        dimm.size_mib,
                        dimm.ranks,
                        dimm.device_width,
                        dimm.rank_one_mirrored
                    );
                    dimms[ch][slot] = Some(dimm);
                }
            }
        }
        let gdxc_overrides = unsafe { overrides::GdxcRegisters::at(early_sa::GDXC_BASE as usize) };
        let policy = ColdPolicy::resolve(
            &dimms,
            gdxc_overrides,
            ColdPolicyInput {
                cpu,
                capabilities: caps,
                overrides: config.overrides,
                platform_tck_min_fs: config.platform_tck_min_fs,
                phy: config.phy,
                dram_odt: config.dram_odt,
                ca_vref_config: config.ca_vref_config,
                dummy_reads: config.dummy_reads,
                thermal: config.thermal,
            },
        )
        .map_err(Error::Cpu)?;
        let identity = super::training_cache::identity(
            signature,
            [caps.capid_a, caps.capid_b, caps.capid_c],
            core::array::from_fn(|ch| {
                core::array::from_fn(|slot| present[ch][slot].then_some(&cold.spd[ch][slot][..]))
            }),
        );
        // native_cpu_replacement_check() forces full training.
        let record = input
            .cached
            .filter(|_| !replacement.replaced)
            .and_then(|bytes| super::training_cache::Record::parse(bytes, &identity));
        match (input.cached.is_some(), record.is_some()) {
            (_, true) => fstart_log::info!("DRAM: cached training matches this CPU and DIMMs"),
            (true, false) => fstart_log::info!("DRAM: cached training is stale; full training"),
            (false, false) => fstart_log::info!("DRAM: no cached training; full training"),
        }
        let t = policy.timings();
        fstart_log::debug!(
            "DRAM: selected DDR4 {} MT/s (tCK {} fs), CL{}-tRCD{}-tRP{}-tRAS{}, tCWL{}",
            t.clock.data_rate_mts(),
            t.clock.tck_fs,
            t.cas,
            t.trcd,
            t.trp,
            t.tras,
            t.cwl
        );
        fstart_log::debug!("DRAM: wait for CSE UMA request");
        let uma = cse::wait_uma(&heci, 5000, &mut delay).map_err(Error::Cse)?;
        fstart_log::debug!(
            "DRAM: program address map with {} MiB of CSE UMA",
            uma.mib()
        );
        let prmrr = uncore_prmrr::Request::read(&msrs, config.prmrr).map_err(Error::Prmrr)?;
        let gdxc = unsafe { GdxcRegisters::at(early_sa::GDXC_BASE as usize) };
        let map = unsafe {
            memory_map::program_cold(
                &sa,
                mch,
                gdxc,
                config.ecam,
                mode,
                &policy.memory().dimms,
                caps,
                uma,
                config.memory_map,
                prmrr,
            )
        }
        .map_err(Error::Map)?;
        pmc::clear_dram_initialized(&EcamDevice::new(0, 0x1f, 2)).map_err(Error::Pmc)?;
        fstart_log::debug!(
            "DRAM: TOM={} TOLUD={} TOUUD={}; begin cold training",
            Hex(map.plan().tom),
            Hex(map.plan().tolud),
            Hex(map.plan().touud)
        );
        let outcome = unsafe {
            cold_handoff::train_and_handoff_cold(
                mch,
                pmc,
                &sa,
                &map,
                &msrs,
                &policy,
                config.controller,
                &mut cold.calibration,
                &mut cold.training,
                &mut cold.checks,
                0,
                4096,
                &mut cse,
                uma,
                did_status,
                config.reservations,
                &mut cold.table,
                super::cold_controller::Cache {
                    identity,
                    record,
                    capture: input.capture,
                },
                &mut delay,
            )
        }
        .map_err(Error::Handoff)?;
        let captured = match outcome {
            cold_handoff::Outcome::Complete(report) => {
                match report.controller.training {
                    Some(training) => fstart_log::debug!(
                        "DRAM: controller and CSE handoff completed after {} attempt(s)",
                        training.attempts
                    ),
                    None => fstart_log::info!("DRAM: cached training replayed"),
                }
                report.controller.captured
            }
            cold_handoff::Outcome::ResetRequired(report) => {
                // Native romstage_native.c: do the reset the CSE asked for.
                fstart_log::info!(
                    "DRAM: CSE requested {:?}",
                    fstart_log::DebugValue(&report.action)
                );
                let global = report.action == DramAction::GlobalReset;
                pmc::select_cf9_reset(&EcamDevice::new(0, 0x1f, 2), global).map_err(Error::Pmc)?;
                if report.action == DramAction::NonPowerCycleReset {
                    fstart_arch::x86::boot::system_reset(true);
                }
                fstart_arch::x86::boot::full_reset();
            }
            cold_handoff::Outcome::Stopped(report) => return Err(Error::MeAction(report.action)),
        };
        // CSE continuation cannot authorize an unreserved PTT DMA handoff.
        // On failure bring_up clears the still-private table and poisons this
        // owner; neither DISB nor Ready is published.
        unsafe { crate::txt::require_no_ptt() }.map_err(Error::Txt)?;
        pmc::mark_dram_initialized(&EcamDevice::new(0, 0x1f, 2)).map_err(Error::Pmc)?;
        let active = core::array::from_fn(|ch| {
            core::array::from_fn(|slot| policy.memory().dimms[ch][slot].is_some())
        });
        let configured_mts = u16::try_from(t.clock.data_rate_mts()).unwrap_or(0);
        self.memory_info = Some(build_memory_info(
            config.spd_addresses,
            &cold.spd,
            active,
            configured_mts,
            caps,
            policy.memory().ecc,
        ));
        self.map = Some(map);
        self.state = State::Ready;
        Ok(captured)
    }
}

fn build_memory_info(
    spd_addresses: [[u8; 2]; 2],
    spd: &SpdStorage,
    active: [[bool; 2]; 2],
    configured_mts: u16,
    caps: Capabilities,
    ecc: bool,
) -> fstart_core::memory_info::MemoryInfo {
    use super::capabilities::CapidA;
    use fstart_core::memory_info::{MemoryDevice, MemoryInfo};

    let a = LocalRegisterCopy::<u32, CapidA::Register>::new(caps.capid_a);
    let max_channel_mib = match a.read(CapidA::MAX_CHANNEL_SIZE) {
        0 => 32768u32,
        1 => 8192,
        2 => 4096,
        _ => 2048,
    };
    let channels = if a.is_set(CapidA::SINGLE_CHANNEL) {
        1
    } else {
        2
    };
    let mut info = MemoryInfo::new(max_channel_mib * channels);
    if ecc {
        info.ecc = 0x05;
    }
    for ch in 0..2 {
        for slot in 0..2 {
            if spd_addresses[ch][slot] == 0 {
                continue;
            }
            let device = if active[ch][slot] {
                fstart_driver_spd::ddr4::inventory(
                    &spd[ch][slot],
                    configured_mts,
                    ch as u8,
                    slot as u8,
                )
                .unwrap_or_else(|_| MemoryDevice::unknown(ch as u8, slot as u8))
            } else {
                MemoryDevice::empty(ch as u8, slot as u8)
            };
            info.push(device);
        }
    }
    info
}

impl MemoryInit for IntelSkylake {
    type Input<'a> = SpdTraining<'a>;
    type Output = Option<usize>;

    fn bring_up(&mut self, input: SpdTraining<'_>) -> Result<Option<usize>, ServiceError> {
        match self.train(input) {
            Ok(captured) => Ok(captured),
            Err(error) => {
                fstart_log::error!("DRAM: {:?}", fstart_log::DebugValue(&error));
                self.state = State::Failed;
                self.failure = Some(error);
                if let Some(cold) = &mut self.resources {
                    let _ = cold.table.set_detected(0, 0);
                }
                Err(ServiceError::HardwareError)
            }
        }
    }
}

impl IntelSkylake {
    /// The E820 map: the one training built, or else the programmed
    /// controller's read back.
    fn with_table<T>(&self, f: impl FnOnce(&E820State) -> T) -> Result<T, ServiceError> {
        if self.resources.is_some() {
            return self.memory_table().map(f);
        }
        let (map, uma) = self.observe()?;
        let mut table = E820State::new();
        cold_handoff::build_table(&mut table, map.plan(), uma, self.config.reservations)
            .map_err(|_| ServiceError::HardwareError)?;
        Ok(f(&table))
    }
}

impl MemoryDetector for IntelSkylake {
    fn detect_memory(&self, entries: &mut [E820Entry]) -> Result<usize, ServiceError> {
        self.with_table(|table| {
            let destination = entries
                .get_mut(..table.count())
                .ok_or(ServiceError::InvalidParam)?;
            destination.copy_from_slice(table.entries());
            // The RAM the ramstage caches write-back (`mtrr::setup_ram_wb`) and
            // the final MTRR solution covers, as the other northbridges publish.
            fstart_arch::x86::mtrr::set_ram_wb_ranges_from(
                table
                    .entries()
                    .iter()
                    .filter(|entry| entry.kind == E820Kind::Ram as u32)
                    .map(|entry| (entry.addr, entry.size)),
            )?;
            Ok(table.count())
        })?
    }
    fn total_ram_bytes(&self) -> Result<u64, ServiceError> {
        self.with_table(E820State::total_ram)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ddr4_spd() -> [u8; SPD_BYTES] {
        let mut spd = [0u8; SPD_BYTES];
        spd[0] = 0x23; // 384 bytes used, 512 bytes total
        spd[1] = 0x11; // rev 1.1
        spd[2] = 0x0c; // DDR4
        spd[3] = 0x03; // SO-DIMM
        spd[4] = 0x85; // 8 Gb die (density code 5 -> 1024 MiB)
        spd[12] = 0x01; // 1 rank, x8
        spd[13] = 0x03; // 64-bit primary bus, no ECC
        spd[17] = 0x00; // MTB 125ps, FTB 1ps
        spd[18] = 0x07; // tCKmin = 875ps - 42ps = 833ps -> 2400 MT/s
        spd[125] = (-42i8) as u8;
        let crc = fstart_driver_spd::crc16(&spd[..126]).to_le_bytes();
        spd[126] = crc[0];
        spd[127] = crc[1];
        spd[320] = 0x80; // bank 0 (Samsung)
        spd[321] = 0xce;
        spd[329..339].copy_from_slice(b"M471A1K43B");
        spd
    }

    #[test]
    fn build_memory_info_records_populated_and_empty_wired_slots() {
        let mut storage: SpdStorage = [[[0; SPD_BYTES]; 2]; 2];
        storage[0][0] = sample_ddr4_spd();
        let caps = Capabilities {
            capid_a: 0, // dual channel, 32 GiB/channel
            capid_b: 1 << 3,
            capid_c: 0,
        };
        let info = build_memory_info(
            [[0x50, 0], [0x52, 0]],
            &storage,
            [[true, false], [false, false]],
            2400,
            caps,
            false,
        );
        assert_eq!(info.max_capacity_mib.get(), 65536);
        assert_eq!(info.ecc, fstart_core::memory_info::ECC_NONE);
        let devices = info.devices();
        assert_eq!(devices.len(), 2);
        assert!(devices[0].is_populated());
        assert_eq!(devices[0].size_mib.get(), 8192);
        assert_eq!(devices[0].configured_mts.get(), 2400);
        assert_eq!(devices[0].manufacturer(), Some("Samsung"));
        assert_eq!(devices[0].part_number(), Some("M471A1K43B"));
        assert_eq!((devices[0].channel, devices[0].slot), (0, 0));
        assert!(!devices[1].is_populated());
        assert_eq!((devices[1].channel, devices[1].slot), (1, 0));
    }
}
