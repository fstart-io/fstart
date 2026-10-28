//! MP bring-up: the SMU releases each thread once into the reset vector,
//! which the reset image routes to the arch crate's SIPI trampoline
//! (`entry::ApRoute`); every thread then runs the CCX per-thread steps
//! (CCX §3.7, CCX_SP3 §2 steps 5–7).
//!
//! The arch MP core owns AP stacks, the flight plan and the mailboxes; this
//! module supplies the release ([`release_all`]), the MSRs every AP copies
//! from the BSP before it enables caching ([`Sp3Cpu::sipi_msrs`]) and the
//! CCX work per thread ([`Sp3Cpu::init_cpu`]).

use core::sync::atomic::{AtomicU32, Ordering};

use fstart_arch::x86::cpu::amd::msr::{MMIO_CFG_BASE, SYSCFG, Syscfg, TOP_MEM, TOP_MEM2};
use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_arch::x86::mp::{CpuDriver, CpuIdMatch, CpuIdentity, MpError, SipiUnlock};
use fstart_arch::x86::mtrr::{MsrEntry, SipiMsrs};
use fstart_driver_amd::sp3::ccx::power::Wdt;
use fstart_driver_amd::sp3::ccx::release::{Launch, ReleaseError};
use fstart_driver_amd::sp3::ccx::topology::Thread;
use fstart_driver_amd::sp3::ccx::{self, ApInputs, Ccx};
use fstart_driver_amd::sp3::psp::bar::MSR_PSP_ADDR;
use fstart_driver_amd::sp3::soc::Generation;
use tock_registers::interfaces::{ReadWriteable, Readable};

use crate::hw::msr;

/// `APIC_BAR` (MSR 1Bh) bit 8: this thread is the BSP.
const APIC_BAR: u32 = 0x1b;
const APIC_BAR_BSP: u64 = 1 << 8;

/// The executing thread as the CCX code sees it.
pub fn this_thread(generation: Generation) -> Thread {
    let (eax, ebx, _, _) = fstart_arch::x86::cpuid(0x8000_001e);
    let bsp = msr().read(APIC_BAR) & APIC_BAR_BSP != 0;
    Thread::from_cpuid(generation, eax, ebx, bsp)
}

/// The executing thread's (extended) APIC ID, valid in xAPIC and x2APIC.
pub fn apic_id() -> u32 {
    fstart_arch::x86::cpuid(0x8000_001e).0
}

/// The SP3 CPU driver handed to `mp_init_with_launcher`.
pub struct Sp3Cpu<'a> {
    pub ccx: Ccx,
    pub policy: &'a ccx::Policy,
    pub inputs: ApInputs<'a>,
    pub wdt: Option<Wdt>,
    /// `PSP_ADDR` per socket: every thread loads its socket's PSP BAR
    /// (PSP_APOB §3.2; Naples: die 0's, the BAR AGESA uses).
    pub psp_addr: [Option<u64>; 2],
    failures: AtomicU32,
}

impl<'a> Sp3Cpu<'a> {
    pub fn new(
        ccx: Ccx,
        policy: &'a ccx::Policy,
        inputs: ApInputs<'a>,
        wdt: Option<Wdt>,
        psp_addr: [Option<u64>; 2],
    ) -> Self {
        Self {
            ccx,
            policy,
            inputs,
            wdt,
            psp_addr,
            failures: AtomicU32::new(0),
        }
    }

    /// Threads whose microcode load failed.
    pub fn failures(&self) -> u32 {
        self.failures.load(Ordering::Acquire)
    }
}

impl CpuDriver for Sp3Cpu<'_> {
    fn name(&self) -> &'static str {
        "amd-sp3"
    }

    fn id_table(&self) -> &'static [CpuIdMatch] {
        &[]
    }

    /// The flow identified the BSP (fail closed) before MP; the APs are the
    /// same OPN by construction of the socket.
    fn matches(&self, _identity: CpuIdentity) -> bool {
        true
    }

    fn init_cpu(&self) {
        let cpu = msr();
        let thread = this_thread(self.ccx.generation());
        let bsp = thread.in_scope(ccx::topology::Scope::Bsp);
        // APs only (PSP_APOB §3.2, threads ≠ 0): the BSP wrote its PSP_ADDR
        // with the BAR, and a second write raises #GP.
        if let (false, Some(bar)) = (
            bsp,
            self.psp_addr[usize::from(thread.location().socket & 1)],
        ) {
            cpu.write(MSR_PSP_ADDR, bar);
        }
        if !bsp {
            self.ccx.early(&cpu, &thread);
            // SAFETY: ring 0 with caching enabled; the BSP's patch is
            // identity mapped and stays reserved.
            if unsafe { self.ccx.ap_init(&cpu, &thread, &self.inputs) }.is_err() {
                self.failures.fetch_add(1, Ordering::AcqRel);
            }
        }
        self.ccx.all_threads(&cpu, &thread, self.policy, self.wdt);
    }

    /// The BSP's memory routing (TOP_MEM/TOM2, SYSCFG, MMIO_CFG_BASE: the
    /// PSP/ABL leave APs' copies to the BIOS, CCX §3.7) and its MTRRs, the
    /// fixed ones with their RdDram/WrDram bits (read with SYSCFG
    /// MtrrFixDramModEn set).
    fn sipi_msrs(&self) -> Result<SipiMsrs, MpError> {
        let cpu = msr();
        // SAFETY: BSP, before any AP starts; caching state is final.
        unsafe { fstart_arch::x86::mtrr::install_final_solution(&[]) }
            .map_err(|_| MpError::CacheabilitySetupFailed)?;
        let syscfg = cpu.reg(SYSCFG);
        syscfg.modify(Syscfg::MTRR_FIX_DRAM_MOD_EN::SET);
        // Replayed with MtrrFixDramModEn still set: the fixed MTRRs that
        // follow carry RdDram/WrDram bits, and writing those with the bit
        // clear raises #GP (a triple fault this early). The trampoline
        // clears it after the replay (`sipi_unlock`).
        let syscfg_unlocked = syscfg.get();
        let mtrrs = fstart_arch::x86::mtrr::snapshot_with(|i| cpu.read(i));
        syscfg.modify(Syscfg::MTRR_FIX_DRAM_MOD_EN::CLEAR);
        let mtrrs = mtrrs.map_err(|_| MpError::CacheabilitySetupFailed)?;
        // Order matters on the AP: SYSCFG (MtrrFixDramEn) only after the
        // fixed MTRRs hold their RdDram/WrDram bits, else the trampoline's
        // own fetches below 1 MiB go to MMIO; IA32_MTRR_DEF_TYPE stays last.
        let (def_type, mtrrs) = mtrrs.split_last().ok_or(MpError::CacheabilitySetupFailed)?;
        let routing = [
            MsrEntry::new(TOP_MEM.index(), cpu.reg(TOP_MEM).get()),
            MsrEntry::new(TOP_MEM2.index(), cpu.reg(TOP_MEM2).get()),
            MsrEntry::new(MMIO_CFG_BASE.index(), cpu.reg(MMIO_CFG_BASE).get()),
        ];
        routing
            .into_iter()
            .chain(mtrrs.iter().copied())
            .chain([MsrEntry::new(SYSCFG.index(), syscfg_unlocked), *def_type])
            .try_fold(SipiMsrs::new(), |mut v, e| v.push(e).map(|_| v))
            .map_err(|_| MpError::CacheabilitySetupFailed)
    }

    fn sipi_unlock(&self) -> Option<SipiUnlock> {
        Some(SipiUnlock::new(SYSCFG, Syscfg::MTRR_FIX_DRAM_MOD_EN::SET))
    }
}

/// Release every thread of `order` (one SMU THREAD_ENABLE bit + apply
/// message each). `launch` sends one; the arch MP core waits for the
/// check-ins (bounded: 50 ms per AP; AGESA waits forever, CCX §3.7).
/// The error is the failing launch's index in `order` + 1.
pub fn release_all(
    order: &[Launch],
    mut launch: impl FnMut(&Launch) -> Result<(), ReleaseError>,
) -> Result<(), u32> {
    order
        .iter()
        .enumerate()
        .try_for_each(|(i, l)| launch(l).map_err(|_| i as u32 + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_stops_at_the_first_failure() {
        let l = |bit| Launch {
            socket: 0,
            die: 0,
            thread_enable: 0x3008_1018,
            bit,
        };
        let order = [l(1), l(2), l(3)];
        let mut sent = std::vec::Vec::new();
        let r = release_all(&order, |l| {
            sent.push(l.bit);
            if l.bit == 2 {
                Err(ReleaseError::AlreadyRunning)
            } else {
                Ok(())
            }
        });
        assert_eq!(r, Err(2));
        assert_eq!(sent, [1, 2]);
        assert_eq!(release_all(&order[..1], |_| Ok(())), Ok(()));
    }
}
