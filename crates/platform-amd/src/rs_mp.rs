// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rs AP reset/rendezvous, shared by Genoa and Siena. The caller first
//! validates its BSP/platform identity and reserves the architectural MP
//! stacks/trampoline. This does not claim complete per-stepping CPU policy.
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use fstart_arch::x86::{
    cpu::{
        amd::msr::{SYSCFG, Syscfg},
        msr::{MsrAccess, NativeMsr, PATCH_LEVEL},
    },
    mp::{
        self, CpuDriver, CpuIdMatch, CpuIdentity, CpuVendor, MpConfig, MpError, MpHandle,
        SipiUnlock,
    },
    mtrr::SipiMsrs,
    psp_reset::{self, ApRoute},
};
use fstart_driver_amd::{
    apob::ccx::CoreMap,
    ccx::{
        cache,
        microcode::{Error as MicrocodeError, Load, Patch},
        zen4,
    },
    smn::Smn,
};
use fstart_pci::ecam::GlobalEcam;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// The compiler binding is a snapshot, never a runtime flash pointer. Its
/// immutable, aligned bytes live in the reserved reset image through reset.
fn selected_microcode(signature: u32) -> Result<Option<Patch<'static>>, MpError> {
    #[cfg(fstart_amd_has_rs_microcode)]
    {
        const BYTES: &[u8] = include_bytes!(env!("FSTART_RS_MICROCODE"));
        #[repr(C, align(16))]
        struct Aligned([u8; BYTES.len()]);
        static PATCH: Aligned = Aligned(*include_bytes!(env!("FSTART_RS_MICROCODE")));
        let patch = Patch::parse(&PATCH.0, signature).map_err(|_| MpError::UnsupportedCpu)?;
        patch
            .validate_address(patch.address())
            .map_err(|_| MpError::UnsupportedCpu)?;
        fstart_log::info!("rs: explicit microcode revision={:#x}", patch.revision());
        Ok(Some(patch))
    }
    #[cfg(not(fstart_amd_has_rs_microcode))]
    {
        let _ = signature;
        fstart_log::warn!("rs: no explicit microcode input; CPU policy remains incomplete");
        Ok(None)
    }
}

struct CheckpointCpu {
    failures: AtomicU32,
    patch: Option<Patch<'static>>,
    patch_lock: AtomicBool,
    signature: u32,
    package: u8,
}
impl CpuDriver for CheckpointCpu {
    fn name(&self) -> &'static str {
        "amd-rs-rendezvous-checkpoint"
    }
    fn id_table(&self) -> &'static [CpuIdMatch] {
        &[]
    }
    fn matches(&self, identity: CpuIdentity) -> bool {
        identity.vendor == CpuVendor::Amd
            && identity.signature == self.signature
            && (fstart_arch::x86::cpuid(0x8000_0001).1 >> 28) as u8 == self.package
    }
    fn update_microcode(&self) {
        let Some(patch) = self.patch else { return };
        // SMT siblings share patch state. Serialize all loads, including the
        // BSP; Patch::apply retains a newer patch and verifies each readback.
        while self
            .patch_lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        if self.failures.load(Ordering::Acquire) != 0 {
            self.patch_lock.store(false, Ordering::Release);
            fstart_arch::halt();
        }
        // The BSP leaves the patch address in C001_100C like AGESA and openSIL
        // LoadMicrocodePatch (vendor S8040: BSP only, APs keep 0). Our patch
        // stays reserved through reset, unlike the vendor's DRAM copy.
        let load = Load {
            record_for_s3: fstart_arch::x86::lapic::Lapic::is_bsp(),
            ..Load::default()
        };
        // SAFETY: matched CPU at CPL0, WB identity-mapped immutable patch in
        // the reserved reset image. Interrupts are off, loads serialized.
        let result = unsafe {
            patch.apply(
                &NativeMsr::new(),
                patch.address(),
                fstart_arch::x86::cpuid(1).0,
                load,
            )
        };
        if result.is_err() {
            // Poison before unlocking: no other CPU may submit a load after
            // this failure. The MP flight-plan barrier cannot complete.
            self.failures.fetch_add(1, Ordering::Release);
        }
        self.patch_lock.store(false, Ordering::Release);
        if let Err(error) = result {
            fstart_log::error!("rs: microcode failure; fail-stop, no retry");
            if let MicrocodeError::Rejected { expected, actual } = error {
                fstart_log::error!(
                    "rs: microcode expected={:#x} actual={:#x}",
                    expected,
                    actual
                );
            }
            fstart_arch::halt();
        }
    }
    fn init_cpu(&self) {
        // SAFETY: on a CPU matching the platform-checked BSP, at CPL0.
        let cpu = unsafe { NativeMsr::new() };
        let syscfg = cpu.reg(SYSCFG).extract();
        if !syscfg.matches_all(Syscfg::MTRR_FIX_DRAM_EN::SET + Syscfg::MTRR_VAR_DRAM_EN::SET)
            || syscfg.is_set(Syscfg::MTRR_FIX_DRAM_MOD_EN)
            || syscfg.is_set(Syscfg::SMEE)
            || (syscfg.is_set(Syscfg::TOM2_EN) && !syscfg.is_set(Syscfg::TOM2_WB))
        {
            self.failures.fetch_add(1, Ordering::AcqRel);
        }
    }
    fn sipi_msrs(&self) -> Result<SipiMsrs, MpError> {
        // SAFETY: checked BSP, before any AP release.
        let cpu = unsafe { NativeMsr::new() };
        let snapshot = zen4::sipi_msrs(&cpu).map_err(|_| MpError::CacheabilitySetupFailed)?;
        cache::enable_tom2_writeback(&cpu);
        Ok(snapshot)
    }
    fn sipi_unlock(&self) -> Option<SipiUnlock> {
        Some(zen4::sipi_unlock())
    }
}
fn prepare_cache() -> Result<(), MpError> {
    use fstart_arch::x86::cpu::{
        amd::msr::TOP_MEM,
        msr::{MTRR_CAP, MtrrCap},
    };
    // SAFETY: checked BSP, before AP release, interrupts disabled.
    let cpu = unsafe { NativeMsr::new() };
    let low = cpu.reg(TOP_MEM).get();
    let physical_bits = (fstart_arch::x86::cpuid(0x8000_0008).0 & 0xff) as u8;
    let available = cpu.reg(MTRR_CAP).read(MtrrCap::VARIABLE_COUNT) as usize;
    let plan = cache::Plan::new(low, physical_bits, available)
        .map_err(|_| MpError::CacheabilitySetupFailed)?;
    cache::install(&cpu, &plan).map_err(|_| MpError::CacheabilitySetupFailed)?;
    cpu.reg(SYSCFG).modify(Syscfg::MTRR_FIX_DRAM_MOD_EN::CLEAR);
    fstart_log::info!(
        "rs: BSP cache low-DRAM={:#x} PA-bits={} MTRRs={} SYS_CFG={:#x}",
        low,
        physical_bits,
        available,
        cpu.reg(SYSCFG).get()
    );
    Ok(())
}

/// A verified CPU: its APIC ID and the physical CCD it was released on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpu {
    pub apic_id: u32,
    pub ccd: u8,
}
/// The verified CPUs, in MP index order.
pub type Cpus<const MAX: usize> = heapless::Vec<Cpu, MAX>;

/// The ACPI C-state I/O base: openSIL's Onyx configuration and the S8040's
/// NFX firmware (C001_0073 = 813h); C2 (CC6) is a read of base + 1.
pub const CSTATE_IO_BASE: u16 = 0x813;

/// Per-thread CPU policy of the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// CpuWdtCfg (time base, count select) to enable, or leave it off.
    pub watchdog: Option<(u8, u8)>,
    /// Write-combining speculation (AGESA AmdWcSpecConfiguration), or leave
    /// the reset value.
    pub wc_spec: Option<bool>,
}

/// The vendor reset-time thread policy: the Zen 4 CCX reset table for the
/// CPU's logical ID (AGESA CcxProgramTablesAtReset) and TW_CFG.CombineCr0Cd
/// (AmdCcxZen4Pei after AP launch). SMT siblings share CR0.CD: without it,
/// Linux's one-thread-at-a-time cache_disable() (MTRR rendezvous) runs a
/// thread uncached next to a cached sibling, losing stores.
/// Then CC6 and the C-state I/O base (openSIL CcxInitializeC6: CSTATE_CONFIG
/// on the compute unit's primary thread), the CPU watchdog and WcSpec
/// (AmdCcxZen4Pei misc MSRs, openSIL Turin CcxMiscInit).
fn reset_policy(logical: Option<(u32, u32)>, policy: Policy) {
    use fstart_driver_amd::ccx::{
        CSTATE_CONFIG_CC6,
        msr::{
            CPU_WDT_CFG, CSTATE_BASE_ADDR, CSTATE_CONFIG, CpuWdtCfg, CstateBaseAddr, LS_CFG,
            LS_CFG3, LsCfg, LsCfg3, TW_CFG, TwCfg,
        },
        zen4_tables as reset,
    };
    // SAFETY: the current CPU matches the checked BSP identity, at CPL0.
    let cpu = unsafe { NativeMsr::new() };
    if let Some(id) = logical {
        let apic = fstart_arch::x86::cpuid(0x8000_001e).0;
        reset::apply_reset(
            &cpu,
            &reset::Thread {
                cpu: id,
                // Unknown core logical IDs and platform flags skip their records.
                core: None,
                platform: None,
                first_of_core: apic & 1 == 0,
                first_of_complex: apic & 0xf == 0,
            },
        );
    }
    cpu.reg(TW_CFG).modify(TwCfg::COMBINE_CR0_CD::SET);
    if let Some(on) = policy.wc_spec {
        cpu.reg(LS_CFG).modify(LsCfg::WC_SPEC_RSVD::CLEAR);
        let bits = if on {
            LsCfg3::WC_SPEC0::SET + LsCfg3::WC_SPEC1::SET
        } else {
            LsCfg3::WC_SPEC0::CLEAR + LsCfg3::WC_SPEC1::CLEAR
        };
        cpu.reg(LS_CFG3).modify(bits);
    }
    if fstart_arch::x86::cpuid(0x8000_001e).0 & 1 == 0 {
        cpu.reg(CSTATE_CONFIG).set(CSTATE_CONFIG_CC6);
    }
    cpu.reg(CSTATE_BASE_ADDR)
        .write(CstateBaseAddr::CSTATE_ADDR.val(CSTATE_IO_BASE.into()));
    if let Some((time_base, count)) = policy.watchdog {
        // Keep the reset severity.
        cpu.reg(CPU_WDT_CFG).modify(
            CpuWdtCfg::EN::SET
                + CpuWdtCfg::TIME_BASE.val(time_base.into())
                + CpuWdtCfg::COUNT_SEL.val(count.into()),
        );
    }
}

/// Rendezvous using the existing architectural MP owner, distinct stacks,
/// validated SIPI readback and per-AP SYS_CFG replay, then the vendor reset
/// thread policy on every thread. MAX must match the platform's
/// FSTART_MP_MAX_CPUS build contract. No final CPU policy implied.
pub fn initialize<const MAX: usize>(
    topology: &CoreMap<12>,
    policy: Policy,
) -> Result<(MpHandle, Cpus<MAX>), MpError> {
    let total = u16::try_from(topology.threads()).map_err(|_| MpError::TooManyCpus {
        requested: u16::MAX,
        supported: MAX as u16,
    })?;
    if total == 0 {
        return Err(MpError::NoCpus);
    }
    if usize::from(total) > MAX || MAX > 384 {
        return Err(MpError::TooManyCpus {
            requested: total,
            supported: MAX as u16,
        });
    }
    let identity = CpuIdentity::current();
    if identity.vendor != CpuVendor::Amd || fstart_arch::x86::cpuid(0x8000_001e).0 != 0 {
        return Err(MpError::UnsupportedCpu);
    }
    let order = zen4::launch_order(topology).map_err(|_| MpError::UnsupportedCpu)?;
    if order.len() + 1 != usize::from(total) {
        return Err(MpError::UnsupportedCpu);
    }
    // SAFETY: BSP owns primary IOD SMN; AP callbacks never touch this pair.
    let smn = unsafe { Smn::new(GlobalEcam) };
    let mut announced = 0u16;
    for (i, thread) in order.iter().enumerate() {
        let (register, mask) = thread
            .register_and_mask()
            .map_err(|_| MpError::PlatformLaunchFailed(i as u32 + 1))?;
        let current = smn.reg(register).get();
        if announced & (1 << thread.physical_ccd) == 0 {
            announced |= 1 << thread.physical_ccd;
            fstart_log::info!(
                "rs: AP CCD={} thread-enable[{:#x}]={:#x}",
                thread.physical_ccd,
                register.address(),
                current
            );
        }
        if current == u32::MAX || current & mask != 0 {
            return Err(MpError::PlatformLaunchFailed(i as u32 + 1));
        }
    }
    prepare_cache()?;
    let cpu = CheckpointCpu {
        failures: AtomicU32::new(0),
        patch: selected_microcode(identity.signature)?,
        patch_lock: AtomicBool::new(false),
        signature: identity.signature,
        package: (fstart_arch::x86::cpuid(0x8000_0001).1 >> 28) as u8,
    };
    // Verify the BSP load before releasing any AP. The architectural engine
    // calls this again on the BSP (readback only when already current), then
    // on every AP under the CPU driver's shared serialization lock.
    cpu.update_microcode();
    let drivers: [&dyn CpuDriver; 1] = [&cpu];
    let config = MpConfig {
        cpu_drivers: &drivers,
        num_cpus: total,
        max_cpus: MAX as u16,
    };
    let route =
        ApRoute::to(mp::AP_BOOTSTRAP_ADDRESS).map_err(|_| MpError::TrampolinePlacementFailed)?;
    let result = mp::mp_init_with_launcher(&config, &mut |expected| {
        if usize::from(expected) != order.len() {
            return Err(0);
        }
        psp_reset::set_ap_route(route);
        for (i, &thread) in order.iter().enumerate() {
            // SAFETY: validated tuple, not BSP, live trampoline/distinct stacks,
            // exclusive SMN and bounded barriers; never re-release an enabled AP.
            unsafe { zen4::enable_thread(&smn, thread) }.map_err(|_| i as u32 + 1)?;
            if !fstart_arch::x86::wait_us(50_000, || usize::from(mp::ap_checkins()) > i) {
                fstart_log::error!(
                    "rs: AP check-in timeout index={} CCD={} complex={} core={} thread={}",
                    i + 1,
                    thread.physical_ccd,
                    thread.logical_ccx,
                    thread.logical_core,
                    thread.thread
                );
                return Err(i as u32 + 1);
            }
        }
        Ok(())
    });
    psp_reset::set_ap_route(ApRoute::PARK);
    let mp = result?;
    let logical = fstart_driver_amd::ccx::zen4_tables::logical_id(identity.signature);
    if logical.is_none() {
        fstart_log::warn!(
            "rs: CPUID={:#x} has no known CCX logical ID; reset table skipped",
            identity.signature
        );
    }
    let ids = [const { AtomicU32::new(u32::MAX) }; MAX];
    let patches = [const { AtomicU32::new(0) }; MAX];
    let routing = [const { AtomicU32::new(0) }; MAX];
    mp.scope(|scope| {
        scope.scatter(&|index| {
            reset_policy(logical, policy);
            // SAFETY: current selected CPU at CPL0, per-thread reads.
            let cpu = unsafe { NativeMsr::new() };
            ids[index as usize].store(fstart_arch::x86::cpuid(0x8000_001e).0, Ordering::Release);
            patches[index as usize].store(cpu.reg(PATCH_LEVEL).get() as u32, Ordering::Release);
            routing[index as usize].store(cpu.reg(SYSCFG).get() as u32, Ordering::Release);
        })
    });
    let expected_patch = patches[0].load(Ordering::Acquire);
    let mask = (Syscfg::MTRR_FIX_DRAM_EN::SET
        + Syscfg::MTRR_FIX_DRAM_MOD_EN::SET
        + Syscfg::MTRR_VAR_DRAM_EN::SET
        + Syscfg::TOM2_EN::SET
        + Syscfg::TOM2_WB::SET)
        .value as u32;
    let expected_routing = routing[0].load(Ordering::Acquire) & mask;
    let mut invalid = cpu.failures.load(Ordering::Acquire) != 0
        || cpu
            .patch
            .is_some_and(|patch| expected_patch < patch.revision());
    let mut cpus = Cpus::<MAX>::new();
    for i in 0..usize::from(total) {
        let id = ids[i].load(Ordering::Acquire);
        // Index 0 is the BSP (the first core of logical CCD 0); AP i+1 was
        // released, and checked in, as order[i].
        let ccd = match i {
            0 => topology.ccds.first().map(|c| c.id),
            _ => order.get(i - 1).map(|t| t.physical_ccd),
        };
        invalid |= ccd.is_none_or(|ccd| cpus.push(Cpu { apic_id: id, ccd }).is_err());
        invalid |= id == u32::MAX || ids[..i].iter().any(|old| old.load(Ordering::Acquire) == id);
        let patch = patches[i].load(Ordering::Acquire);
        let syscfg = routing[i].load(Ordering::Acquire);
        invalid |= patch != expected_patch || syscfg & mask != expected_routing;
        fstart_log::info!(
            "rs: CPU={} APIC={} patch={:#x} SYS_CFG={:#x}",
            i,
            id,
            patch,
            syscfg
        );
    }
    if invalid {
        fstart_log::error!(
            "rs: AP identity/microcode/cache-state mismatch; not an OS-ready CPU set"
        );
        mp.park_aps();
        return Err(MpError::UnsupportedCpu);
    }
    fstart_log::info!(
        "rs: native rendezvous verified CPUs={} unique APIC IDs; patch={:#x}",
        total,
        expected_patch
    );
    Ok((mp, cpus))
}
