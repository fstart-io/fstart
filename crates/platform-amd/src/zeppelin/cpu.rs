// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin CPU driver for the architectural MP engine: microcode and
//! the CCX init on every CPU.
use core::{
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use fstart_arch::x86::{
    cpu::msr::NativeMsr,
    mp::{CpuDriver, CpuIdMatch, CpuIdentity, MpError, SipiUnlock},
};
use fstart_driver_amd::{
    fam17h::bios_directory,
    zeppelin::{
        ccx::{self, CpuPolicy, Thread},
        microcode::{PATCH_BYTES, Patch},
    },
};

/// BIOS directory type of the vendor microcode patches.
const MICROCODE: u8 = 0x66;

/// The selected patch, copied to DRAM the MP code keeps reserved. Written once
/// by the BSP before any AP starts.
#[repr(C, align(16))]
struct PatchPage([u8; PATCH_BYTES]);
static mut PATCH: MaybeUninit<PatchPage> = MaybeUninit::uninit();

/// The SMT siblings share MSRs, and so do the CPUs of a complex for some of
/// the table: initialize one CPU at a time.
static LOCK: AtomicBool = AtomicBool::new(false);
static ERRORS: AtomicU32 = AtomicU32::new(0);

pub struct ZeppelinCpu {
    policy: CpuPolicy,
    patch: Option<(u64, u32)>,
}

impl ZeppelinCpu {
    /// Select the patch for this CPU from the vendor directory in the mapped
    /// boot flash and stage it.
    ///
    /// # Safety
    /// BSP only, once, before any AP runs. `flash` is the whole boot flash
    /// mapped below 4 GiB.
    pub unsafe fn new(policy: CpuPolicy, flash: &[u8], signature: u32) -> Self {
        let patch = bios_directory::entries(flash)
            .filter(|entry| entry.kind == MICROCODE)
            .find_map(|entry| Patch::parse(entry.data.get(..PATCH_BYTES)?, signature).ok())
            .map(|patch| {
                // SAFETY: BSP-only before the APs start; nothing else uses it.
                let page = unsafe { &mut *core::ptr::addr_of_mut!(PATCH) };
                let page = page.write(PatchPage([0; PATCH_BYTES]));
                page.0.copy_from_slice(patch.bytes());
                (page.0.as_ptr() as u64, patch.revision())
            });
        match patch {
            Some((_, revision)) => {
                fstart_log::info!(
                    "zeppelin: microcode patch {}",
                    fstart_log::Hex(revision.into())
                )
            }
            None => fstart_log::warn!("zeppelin: no microcode patch for this CPU"),
        }
        Self { policy, patch }
    }

    pub fn errors() -> u32 {
        ERRORS.load(Ordering::Acquire)
    }
}

fn locked(f: impl FnOnce()) {
    while LOCK
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    f();
    LOCK.store(false, Ordering::Release);
}

fn signature() -> u32 {
    fstart_arch::x86::cpuid(1).0
}

impl CpuDriver for ZeppelinCpu {
    fn name(&self) -> &'static str {
        "AMD Zeppelin"
    }
    fn id_table(&self) -> &'static [CpuIdMatch] {
        &[]
    }
    fn matches(&self, _identity: CpuIdentity) -> bool {
        true
    }
    fn update_microcode(&self) {
        let Some((address, _)) = self.patch else {
            return;
        };
        locked(|| {
            // SAFETY: the staged page is immutable and reserved; serialized.
            let page = unsafe { &*core::ptr::addr_of!(PATCH) };
            let bytes = unsafe { &page.assume_init_ref().0 };
            let msr = unsafe { NativeMsr::new() };
            let applied = Patch::parse(bytes, signature())
                .and_then(|patch| unsafe { patch.apply(&msr, address, signature()) });
            if applied.is_err() {
                ERRORS.fetch_add(1, Ordering::AcqRel);
            }
        });
    }
    fn init_cpu(&self) {
        locked(|| {
            let thread = Thread::from_cpuid(
                fstart_arch::x86::cpuid(1).1,
                fstart_arch::x86::cpuid(0x8000_001e).1,
            );
            // SAFETY: serialized per-CPU MSR programming.
            let msr = unsafe { NativeMsr::new() };
            if ccx::cpu_init(&msr, &self.policy, thread).is_err() {
                ERRORS.fetch_add(1, Ordering::AcqRel);
            }
            crate::mp::hide_fixed_dram_routing(&msr);
        });
    }
    fn sipi_unlock(&self) -> Option<SipiUnlock> {
        Some(crate::mp::SIPI_UNLOCK)
    }
    fn sipi_msrs(&self) -> Result<fstart_arch::x86::mtrr::SipiMsrs, MpError> {
        // SAFETY: BSP only, before any AP starts.
        crate::mp::sipi_msrs(&unsafe { NativeMsr::new() })
            .map_err(|_| MpError::CacheabilitySetupFailed)
    }
}
