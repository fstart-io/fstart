// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Family 17h CPU driver of the architectural MP engine: on every
//! CPU the vendor microcode patch from the
//! BIOS directory, then the SoC's CCX init, one CPU at a time.
use core::{
    marker::PhantomData,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use fstart_arch::x86::{
    cpu::msr::NativeMsr,
    mp::{CpuDriver, CpuIdMatch, CpuIdentity, CpuVendor, MpError, SipiUnlock},
    mtrr::SipiMsrs,
};
use fstart_driver_amd::{
    ccx::{
        Thread,
        microcode::{Family, PATCH_BYTES, Patch},
    },
    psp::bios_directory,
};

/// BIOS directory type of the vendor microcode patches.
const MICROCODE: u8 = 0x66;

/// DRAM for the selected patch, which the microcode loader reads.
#[repr(C, align(16))]
pub struct PatchPage([u8; PATCH_BYTES]);

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
    /// Select the patch for this CPU from the BIOS directory in the mapped
    /// boot flash and copy it to `page`.
    ///
    /// # Safety
    /// BSP only, before any AP runs. `flash` is the whole boot flash mapped
    /// below 4 GiB, `page` DRAM below 4 GiB the memory map reserves.
    pub unsafe fn new(
        name: &'static str,
        flash: &[u8],
        page: &'static mut MaybeUninit<PatchPage>,
        init: F,
    ) -> Self {
        let signature = fstart_arch::x86::cpuid(1).0;
        let patch = bios_directory::entries(flash)
            .filter(|entry| entry.kind == MICROCODE)
            .find_map(|entry| Patch::<M>::parse(entry.data.get(..PATCH_BYTES)?, signature).ok())
            .map(|patch| {
                let page = page.write(PatchPage([0; PATCH_BYTES]));
                page.0.copy_from_slice(patch.bytes());
                (&page.0[..], patch.revision())
            });
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
            Patch::<M>::parse(bytes, signature)
                .and_then(|patch| unsafe { patch.apply(&msr, bytes.as_ptr() as u64, signature) })
                .is_ok()
        });
    }
    fn init_cpu(&self) {
        self.locked(|| {
            // SAFETY: serialized MSR access on a CPU the driver matched.
            let msr = unsafe { NativeMsr::new() };
            let ok = (self.init)(&msr, Thread::current()).is_ok();
            crate::mp::hide_fixed_dram_routing(&msr);
            ok
        });
    }
    fn sipi_msrs(&self) -> Result<SipiMsrs, MpError> {
        // SAFETY: BSP only, before any AP starts.
        crate::mp::sipi_msrs(&unsafe { NativeMsr::new() })
            .map_err(|_| MpError::CacheabilitySetupFailed)
    }
    fn sipi_unlock(&self) -> Option<SipiUnlock> {
        Some(crate::mp::SIPI_UNLOCK)
    }
}
