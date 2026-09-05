//! Mainstage-only ledger of live file destinations. Loader scratch is checked
//! against existing destinations but is not retained after successful loading.
use crate::boot::{LoadError, MemoryWindow};
use crate::heap::boxed::Box;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

const MAX_LOADED_WINDOWS: usize = 32;
// Current consumers load one FFS segment or a FIT kernel+ramdisk pair.
const MAX_PLAN_WINDOWS: usize = 2;

struct State {
    windows: [MemoryWindow; MAX_LOADED_WINDOWS],
    count: usize,
}
struct Ledger {
    busy: AtomicBool,
    state: UnsafeCell<State>,
}
// SAFETY: state is accessed only by the exclusive busy-lock owner.
unsafe impl Sync for Ledger {}
impl Ledger {
    const fn new() -> Self {
        Self {
            busy: AtomicBool::new(false),
            state: UnsafeCell::new(State {
                windows: [MemoryWindow { start: 0, size: 0 }; MAX_LOADED_WINDOWS],
                count: 0,
            }),
        }
    }

    fn begin<'a>(
        &'a self,
        footprints: &[MemoryWindow],
        live: &[MemoryWindow],
    ) -> Result<PendingLoad<'a>, LoadError> {
        if footprints.len() != live.len() || live.is_empty() || live.len() > MAX_PLAN_WINDOWS {
            return Err(LoadError::MemoryPolicy);
        }
        if self
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(LoadError::MemoryPolicy);
        }
        // Construct guard immediately, so every failure unlocks the ledger.
        let mut pending = PendingLoad {
            ledger: self,
            live: heapless::Vec::new(),
        };
        // SAFETY: pending owns the ledger lock until completion or drop.
        let state = unsafe { &*self.state.get() };
        if state.count + live.len() > MAX_LOADED_WINDOWS {
            return Err(LoadError::MemoryPolicy);
        }
        for (index, footprint) in footprints.iter().enumerate() {
            if footprint.size == 0
                || footprint.start.checked_add(footprint.size).is_none()
                || live[index].size == 0
                || live[index].start != footprint.start
                || live[index].size > footprint.size
                || state.windows[..state.count]
                    .iter()
                    .any(|old| old.overlaps(*footprint))
                || footprints[..index]
                    .iter()
                    .any(|other| other.overlaps(*footprint))
            {
                return Err(LoadError::MemoryPolicy);
            }
            pending
                .live
                .push(live[index])
                .map_err(|_| LoadError::MemoryPolicy)?;
        }
        Ok(pending)
    }
}

static LEDGER: AtomicPtr<Ledger> = AtomicPtr::new(core::ptr::null_mut());

/// Allocate once after DRAM initialization. Policy replacement never clears it.
pub(crate) fn initialize() {
    if !LEDGER.load(Ordering::Acquire).is_null() {
        return;
    }
    let pointer = Box::into_raw(Box::new(Ledger::new()));
    if LEDGER
        .compare_exchange(
            core::ptr::null_mut(),
            pointer,
            Ordering::Release,
            Ordering::Acquire,
        )
        .is_err()
    {
        // SAFETY: unpublished allocation remains exclusively owned here.
        unsafe {
            drop(Box::from_raw(pointer));
        }
    }
}

pub(crate) fn begin(
    footprints: &[MemoryWindow],
    live: &[MemoryWindow],
) -> Result<PendingLoad<'static>, LoadError> {
    if footprints
        .iter()
        .any(|&range| crate::fdt_workspace::conflicts(range))
    {
        return Err(LoadError::MemoryPolicy);
    }
    // SAFETY: published ledger allocations are retained for stage lifetime.
    let ledger =
        unsafe { LEDGER.load(Ordering::Acquire).as_ref() }.ok_or(LoadError::MemoryPolicy)?;
    ledger.begin(footprints, live)
}

/// Borrow the dedicated FDT workspace for mutation. Existing executable/data
/// reservations still apply; callers drop (never commit) this guard afterwards.
pub(crate) fn begin_fdt(range: MemoryWindow) -> Result<PendingLoad<'static>, LoadError> {
    // SAFETY: retained immutable ledger pointer, mutable state protected by lock.
    let ledger =
        unsafe { LEDGER.load(Ordering::Acquire).as_ref() }.ok_or(LoadError::MemoryPolicy)?;
    ledger.begin(&[range], &[range])
}

/// Holds the exclusive load lock from complete preflight through final hashes.
/// A failed load unlocks without publishing an executable/live reservation.
pub(crate) struct PendingLoad<'a> {
    ledger: &'a Ledger,
    live: heapless::Vec<MemoryWindow, MAX_PLAN_WINDOWS>,
}
impl PendingLoad<'_> {
    pub(crate) fn commit(self) {
        // SAFETY: this guard owns the exclusive lock, and begin checked capacity
        // before any writes. Commit cannot fail after executable bytes are ready.
        let state = unsafe { &mut *self.ledger.state.get() };
        for window in &self.live {
            state.windows[state.count] = *window;
            state.count += 1;
        }
    }
}
impl Drop for PendingLoad<'_> {
    fn drop(&mut self) {
        self.ledger.busy.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(start: u64, size: u64) -> MemoryWindow {
        MemoryWindow { start, size }
    }

    #[test]
    fn firmware_blocks_kernel_and_fdt_overlap_but_scratch_can_be_reused() {
        let ledger = Ledger::new();
        ledger
            .begin(&[window(0x1000, 0x200)], &[window(0x1000, 0x100)])
            .unwrap()
            .commit();
        // Kernel initialized output is disjoint, but its compressed scratch
        // would overwrite firmware. FDT overlapping firmware also fails.
        assert!(
            ledger
                .begin(&[window(0xf00, 0x180)], &[window(0xf00, 0x80)])
                .is_err()
        );
        assert!(
            ledger
                .begin(&[window(0x1080, 0x20)], &[window(0x1080, 0x20)])
                .is_err()
        );
        ledger
            .begin(&[window(0x1100, 0x100)], &[window(0x1100, 0x100)])
            .unwrap()
            .commit();
        assert!(
            ledger
                .begin(&[window(0x1100, 1)], &[window(0x1100, 1)])
                .is_err()
        );
    }

    #[test]
    fn ledger_capacity_fails_before_admitting_a_load() {
        let ledger = Ledger::new();
        for index in 0..MAX_LOADED_WINDOWS {
            let range = window(0x1000 + index as u64, 1);
            ledger.begin(&[range], &[range]).unwrap().commit();
        }
        assert!(
            ledger
                .begin(&[window(0x2000, 1)], &[window(0x2000, 1)])
                .is_err()
        );
    }

    #[test]
    fn policy_refresh_preserves_successful_destinations() {
        let buffer = Box::into_raw(Box::new([0u8; 128]));
        let writable = [window(buffer as u64, 128)];
        let policy = crate::boot::MemoryPolicy {
            writable: &writable,
            reserved: &[],
            entry_alignment: 1,
        };
        // SAFETY: permanently retained exclusive allocation; no physical loads
        // are performed by this test, only scalar policy and ledger operations.
        unsafe { crate::directory::set_load_policy(&policy) }.unwrap();
        let live = [window(buffer as u64, 32)];
        begin(&live, &live).unwrap().commit();
        unsafe { crate::directory::set_load_policy(&policy) }.unwrap();
        assert!(begin(&live, &live).is_err());
    }
}
