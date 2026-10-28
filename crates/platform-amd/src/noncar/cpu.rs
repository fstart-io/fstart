// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Family 17h CPU driver of the architectural MP engine: on every
//! CPU the microcode patch from the FFS,
//! then the SoC's CCX init, one CPU at a time.
use core::{
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use fstart_arch::x86::{
    cpu::msr::NativeMsr,
    mp::{CpuDriver, CpuIdMatch, CpuIdentity, CpuVendor, MpError, SipiUnlock},
    mtrr::SipiMsrs,
};
use fstart_driver_amd::ccx::{
    Thread,
    microcode::{Bundle, Family, Load, Patch},
};

/// The CPU driver of SoC family `M`, with the SoC's CCX init `init`.
pub struct Fam17hCpu<M, F> {
    name: &'static str,
    /// The staged patch and its revision.
    patch: Option<(&'static [u8], u32)>,
    init: F,
    /// The SMT siblings share MSRs, and so do the CPUs of a complex for some
    /// of the SoC tables: one CPU at a time.
    lock: AtomicBool,
    errors: AtomicU32,
    family: PhantomData<M>,
}

impl<M: Family, F> Fam17hCpu<M, F> {
    /// Select the patch for this CPU from `microcode`, the verified FFS
    /// copy of the concatenated patches (16-byte aligned, below 4 GiB, as
    /// the loader reads it in place). BSP only, before any AP runs.
    pub fn new(name: &'static str, microcode: Option<&'static [u8]>, init: F) -> Self {
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

impl<M, F, E> CpuDriver for Fam17hCpu<M, F>
where
    M: Family + Send + Sync,
    F: Fn(&NativeMsr, Thread) -> Result<(), E> + Send + Sync,
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
            let ok = (self.init)(&msr, Thread::current()).is_ok();
            super::mp::hide_fixed_dram_routing(&msr);
            ok
        });
    }
    fn sipi_msrs(&self) -> Result<SipiMsrs, MpError> {
        // SAFETY: BSP only, before any AP starts.
        super::mp::sipi_msrs(&unsafe { NativeMsr::new() })
            .map_err(|_| MpError::CacheabilitySetupFailed)
    }
    fn sipi_unlock(&self) -> Option<SipiUnlock> {
        Some(super::mp::SIPI_UNLOCK)
    }
}
