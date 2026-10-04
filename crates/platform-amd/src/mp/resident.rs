// SPDX-License-Identifier: MIT OR Apache-2.0
//! One-shot ownership of the platform's permanent cold-boot MP region.
use crate::{layout, memory::Memory};
use core::{
    ptr::addr_of_mut,
    sync::atomic::{AtomicBool, Ordering},
};
use fstart_arch::x86::mp::{
    self, ApLauncher, BootstrapStorage, MpConfig, MpError, MpFailStop, MpHandle,
};

const AP_SLOTS: usize = layout::MP_CPU_CAPACITY as usize - 1;
#[used]
#[unsafe(link_section = ".picasso_mp")]
static mut STORAGE: BootstrapStorage<AP_SLOTS> = BootstrapStorage::new();
static CLAIMED: AtomicBool = AtomicBool::new(false);
const _: () = assert!(core::mem::size_of::<BootstrapStorage<AP_SLOTS>>() == 0x10800);

#[derive(Debug)]
pub enum Error {
    Mp(MpError),
    MemoryNotReserved,
    Placement,
    AlreadyClaimed,
}

/// Use the linker-owned allocation, retaining it and its map exclusion through
/// reset on every result. This entry is not called by the console checkpoint.
/// # Safety
/// In addition to the architectural MP/launcher contracts, the caller establishes
/// identity mapping, coherence, event exclusion, cache/reset semantics and
/// complete native CPU initialization. All code, globals, page tables and other
/// AP resources remain mapped/resident through reset. Every allocator and payload
/// uses the supplied memory map WITHOUT removing its permanent reservations.
/// Numerical linker placement and E820 bookkeeping do not establish these facts.
pub unsafe fn initialize(
    config: &MpConfig<'_>,
    failure: &impl MpFailStop,
    launcher: &mut dyn ApLauncher,
    memory: &Memory,
) -> Result<MpHandle, Error> {
    let mut selected = Some(launcher);
    // Including zero APs: cached complete-count validation before ownership,
    // hardware, publication, or even constructing a mutable storage reference.
    mp::validate_mp_configuration(config, &mut selected).map_err(Error::Mp)?;
    if config.max_cpus > layout::MP_CPU_CAPACITY {
        return Err(Error::Mp(MpError::ResidentStorageTooSmall {
            required: usize::from(config.max_cpus - 1),
            available: AP_SLOTS,
        }));
    }
    if !memory.mp_reserved() {
        return Err(Error::MemoryNotReserved);
    }
    let pointer = addr_of_mut!(STORAGE);
    if pointer as usize as u64 != layout::MP_START
        || (pointer as usize as u64)
            .checked_add(core::mem::size_of::<BootstrapStorage<AP_SLOTS>>() as u64)
            .is_none_or(|end| end > layout::MP_END)
    {
        return Err(Error::Placement);
    }
    CLAIMED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| Error::AlreadyClaimed)?;
    // The claim is never cleared. Rejected/repeated calls cannot alias storage
    // used by parked APs; no destructor/reclaim token is returned on any path.
    unsafe { mp::mp_init_with_resident_storage(config, failure, selected, &mut *pointer) }
        .map_err(Error::Mp)
}
