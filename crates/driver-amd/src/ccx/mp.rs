// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Family 17h CPU driver of the architectural MP engine: on every CPU
//! the microcode patch from the FFS, then the SoC's CCX init, one CPU at a
//! time; and the MTRRs and PAT the APs replay from the BSP.
use core::{
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use fstart_arch::x86::{
    cpu::{
        amd::msr::{SYSCFG, Syscfg},
        msr::{MsrAccess, NativeMsr},
    },
    mp::{CpuDriver, CpuIdMatch, CpuIdentity, CpuVendor, MpError, SipiUnlock},
    mtrr::{self, SipiMsrs},
};
use tock_registers::interfaces::ReadWriteable;

use crate::ccx::{
    Thread, cache,
    microcode::{Bundle, Family, Load, Patch},
};

/// The CPU driver of SoC family `M`: the SoC's CCX init `init` with its
/// `policy` on every CPU.
pub struct Fam17hCpu<M, P> {
    name: &'static str,
    /// The staged patch and its revision.
    patch: Option<(&'static [u8], u32)>,
    policy: P,
    init: fn(&P, &NativeMsr, Thread) -> bool,
    /// The SMT siblings share MSRs, and so do the CPUs of a complex for some
    /// of the SoC tables: one CPU at a time.
    lock: AtomicBool,
    errors: AtomicU32,
    family: PhantomData<M>,
}

impl<M: Family, P> Fam17hCpu<M, P> {
    /// Select the patch for this CPU from `microcode`, the verified FFS
    /// copy of the concatenated patches (16-byte aligned, below 4 GiB, as
    /// the loader reads it in place). BSP only, before any AP runs.
    pub fn new(
        name: &'static str,
        microcode: Option<&'static [u8]>,
        policy: P,
        init: fn(&P, &NativeMsr, Thread) -> bool,
    ) -> Self {
        let signature = fstart_arch::x86::cpuid(1).0;
        let patch = microcode
            .and_then(|bytes| Bundle::<M>::parse(bytes).ok()?.select(signature).ok())
            .map(|patch| (patch.bytes(), patch.revision()));
        match patch {
            Some((_, revision)) => {
                fstart_log::info!(
                    "{}: microcode patch {}",
                    name,
                    fstart_log::Hex(revision.into())
                )
            }
            None => fstart_log::warn!("{}: no microcode patch for this CPU", name),
        }
        Self {
            name,
            patch,
            policy,
            init,
            lock: AtomicBool::new(false),
            errors: AtomicU32::new(0),
            family: PhantomData,
        }
    }

    /// CPUs whose microcode update or CCX init failed.
    pub fn errors(&self) -> u32 {
        self.errors.load(Ordering::Acquire)
    }

    fn locked(&self, work: impl FnOnce() -> bool) {
        while self
            .lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        if !work() {
            self.errors.fetch_add(1, Ordering::AcqRel);
        }
        self.lock.store(false, Ordering::Release);
    }
}

impl<M, P> CpuDriver for Fam17hCpu<M, P>
where
    M: Family + Send + Sync,
    P: Send + Sync,
{
    fn name(&self) -> &'static str {
        self.name
    }
    fn id_table(&self) -> &'static [CpuIdMatch] {
        &[]
    }
    /// The CPUs the SoC's microcode patches are for.
    fn matches(&self, identity: CpuIdentity) -> bool {
        identity.vendor == CpuVendor::Amd && M::SIGNATURES.contains(&(identity.signature & !0xf))
    }
    fn update_microcode(&self) {
        let Some((bytes, _)) = self.patch else {
            return;
        };
        self.locked(|| {
            let signature = fstart_arch::x86::cpuid(1).0;
            // SAFETY: serialized MSR access on a CPU the driver matched.
            let msr = unsafe { NativeMsr::new() };
            // SAFETY: the staged page is immutable and reserved; serialized.
            Patch::parse(bytes, signature)
                .and_then(|patch| unsafe {
                    patch.apply(&msr, bytes.as_ptr() as u64, signature, Load::default())
                })
                .is_ok()
        });
    }
    fn init_cpu(&self) {
        self.locked(|| {
            // SAFETY: serialized MSR access on a CPU the driver matched.
            let msr = unsafe { NativeMsr::new() };
            let ok = (self.init)(&self.policy, &msr, Thread::current());
            hide_fixed_dram_routing(&msr);
            ok
        });
    }
    fn sipi_msrs(&self) -> Result<SipiMsrs, MpError> {
        // SAFETY: BSP only, before any AP starts.
        sipi_msrs(&unsafe { NativeMsr::new() }).map_err(|_| MpError::CacheabilitySetupFailed)
    }
    fn sipi_unlock(&self) -> Option<SipiUnlock> {
        Some(SIPI_UNLOCK)
    }
}

#[derive(Debug)]
pub enum Error {
    AmdCache(cache::Error),
    Capacity,
}

/// SYSCFG.MtrrFixDramModEn while an AP replays [`sipi_msrs`]: the fixed
/// MTRRs' RdDram/WrDram bits are only writable with it.
pub const SIPI_UNLOCK: SipiUnlock = SipiUnlock::new(SYSCFG, Syscfg::MTRR_FIX_DRAM_MOD_EN::SET);

/// BSP only, after the AMD cache plan is installed and before AP release:
/// the MTRRs and PAT the APs replay, the fixed ones read with their
/// RdDram/WrDram routing exposed.
///
/// The APs keep the SYSCFG, TOP_MEM and
/// TOM2 the ABL programmed on every core: a fresh core faults on the BSP's
/// SYSCFG. They replay with [`SIPI_UNLOCK`].
pub fn sipi_msrs(cpu: &impl MsrAccess) -> Result<SipiMsrs, Error> {
    cache::fixed_dram_view(cpu, |cpu| mtrr::snapshot_with(|index| cpu.read(index)))
        .map_err(Error::AmdCache)?
        .map_err(|_| Error::Capacity)
}

/// Hide the fixed MTRRs' RdDram/WrDram bits again, on every CPU after its
/// init: the ABL and the early cache setup leave them exposed, the OS
/// expects them hidden.
pub fn hide_fixed_dram_routing(cpu: &impl MsrAccess) {
    cpu.reg(SYSCFG).modify(Syscfg::MTRR_FIX_DRAM_MOD_EN::CLEAR);
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::RefCell;
    use fstart_arch::x86::cpu::msr::testing::MsrModel;
    const SYSCFG: u32 = super::SYSCFG.index();
    struct Cpu {
        syscfg: u64,
        writes: std::vec::Vec<(u32, u64)>,
        cap: u64,
    }
    impl MsrModel for Cpu {
        fn read(&mut self, index: u32) -> u64 {
            match index {
                SYSCFG => self.syscfg,
                0xfe => self.cap,
                0x2ff => 0xc00,
                0x277 => 0x0007040600070406,
                0x250 | 0x258 | 0x259 | 0x268..=0x26f => {
                    assert_ne!(self.syscfg & (1 << 19), 0, "RdDram/WrDram must be exposed");
                    if index == 0x259 {
                        0
                    } else {
                        0x1e1e1e1e1e1e1e1e
                    }
                }
                _ => 0,
            }
        }
        fn write(&mut self, index: u32, value: u64) {
            assert_eq!(index, SYSCFG);
            self.syscfg = value;
            self.writes.push((index, value));
        }
    }
    #[test]
    fn replays_the_mtrrs_and_pat_read_with_the_amd_routing() {
        let original = (1 << 18) | (1 << 20) | (1 << 21) | (1 << 22);
        let cpu = RefCell::new(Cpu {
            syscfg: original,
            writes: std::vec::Vec::new(),
            cap: 0x108,
        });
        let msrs = sipi_msrs(&cpu).unwrap();
        assert_eq!(cpu.borrow().syscfg, original);
        let writes: std::vec::Vec<_> = msrs.iter().map(|w| (w.index, w.value())).collect();
        assert_eq!(writes[0], (0x250, 0x1e1e1e1e1e1e1e1e));
        assert!(writes.contains(&(0x259, 0)));
        assert!(!writes.iter().any(|&(index, _)| index == SYSCFG));
        assert_eq!(
            &writes[writes.len() - 2..],
            &[(0x277, 0x0007040600070406), (0x2ff, 0xc00)]
        );
        assert_eq!(
            SIPI_UNLOCK,
            SipiUnlock::new(super::SYSCFG, Syscfg::MTRR_FIX_DRAM_MOD_EN::SET)
        );
    }
    #[test]
    fn encrypted_or_missing_fixed_state_has_no_bsp_writes() {
        for (syscfg, cap) in [((1 << 23) | (1 << 21), 0x108), (1 << 21, 8)] {
            let cpu = RefCell::new(Cpu {
                syscfg,
                cap,
                writes: std::vec::Vec::new(),
            });
            assert!(matches!(sipi_msrs(&cpu), Err(Error::AmdCache(_))));
            assert!(cpu.borrow().writes.is_empty());
        }
    }
}
