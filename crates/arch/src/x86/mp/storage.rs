//! Permanently retained CPU bootstrap storage, independent of stage BSS.
use super::{AP_STACK_SIZE, MpError, exception};
use core::mem::{MaybeUninit, size_of};

/// Private AP stacks and both fatal-exception tables. Declare in a platform's
/// reserved NOLOAD RAM section, not in immutable reset-image data. N counts APs,
/// excluding the BSP. Mailbox/callback capacity remains the build-time limit.
///
/// This contains only uninitialized bytes until MP preparation. Use the const
/// constructor in a static declaration, not on the stage stack. Range inspection
/// touches no hardware; MP preparation never constructs/copies a whole arena.
#[repr(C, align(16))]
pub struct BootstrapStorage<const N: usize> {
    stacks: [MaybeUninit<[u8; AP_STACK_SIZE]>; N],
    tables: MaybeUninit<exception::Tables>,
}
impl<const N: usize> BootstrapStorage<N> {
    pub const fn new() -> Self {
        Self {
            stacks: [const { MaybeUninit::uninit() }; N],
            tables: MaybeUninit::uninit(),
        }
    }
    /// Full allocation, including unused stack slots, tables and padding. The
    /// platform must reserve/exclude this entire range through reset, including
    /// errors and payload handoff; stack addresses are not allocator free space.
    pub fn allocation_range(&self) -> core::ops::Range<u64> {
        let start = self as *const Self as usize as u64;
        start
            ..start
                .checked_add(size_of::<Self>() as u64)
                .expect("valid allocation extent")
    }
}
impl<const N: usize> Default for BootstrapStorage<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
pub(super) struct Resident {
    start: u64,
    bytes: usize,
    pub stack_base: u64,
    table_offset: usize,
    slots: usize,
}
/// Derive addresses only: no references, field reads, initialization or writes.
/// A raw default-storage address can be formed even on a rejected second MP
/// attempt without aliasing stacks/tables still owned by APs.
pub(super) fn parts<const N: usize>(storage: *mut BootstrapStorage<N>) -> Resident {
    Resident {
        start: storage as usize as u64,
        bytes: size_of::<BootstrapStorage<N>>(),
        // First repr(C) field; integer geometry also works for rejected fake
        // addresses without an in-bounds raw field projection.
        stack_base: storage as usize as u64,
        table_offset: core::mem::offset_of!(BootstrapStorage<N>, tables),
        slots: N,
    }
}
impl Resident {
    /// Only after successful non-singleton validate: full extent covers offset.
    pub fn tables(&self) -> *mut exception::Tables {
        (self.start + self.table_offset as u64) as usize as *mut exception::Tables
    }
    /// Cached numerical preflight only, before ownership or hardware. Actual
    /// UC/WB policy, identity mapping, residency and exclusion are NOT proven.
    pub fn validate(&self, max_cpus: u16, aps: u16) -> Result<(), MpError> {
        let required = usize::from(max_cpus - 1);
        if self.slots < required {
            return Err(MpError::ResidentStorageTooSmall {
                required,
                available: self.slots,
            });
        }
        // Singleton does not publish/use these resources, like the launch path.
        if aps == 0 {
            return Ok(());
        }
        let end = self
            .start
            .checked_add(self.bytes as u64)
            .ok_or(MpError::TrampolinePlacementFailed)?;
        if self.start == 0
            || self.start & 15 != 0
            || end > (1u64 << 32)
            || (self.start < super::AP_BOOTSTRAP_ADDRESS + 4096
                && end > super::AP_BOOTSTRAP_ADDRESS)
            || (self.start < super::SMM_DEFAULT_ASEG.1 && end > super::SMM_DEFAULT_ASEG.0)
            || exception::pointers(
                self.tables() as usize as u64,
                super::AP_BOOTSTRAP_ADDRESS + super::sipi_blob::FAULT32_OFFSET as u64,
                super::AP_BOOTSTRAP_ADDRESS + super::sipi_blob::FAULT64_OFFSET as u64,
            )
            .is_none()
        {
            return Err(MpError::TrampolinePlacementFailed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whole_allocation_and_capacity_are_preflighted_without_memory_access() {
        assert_eq!(size_of::<BootstrapStorage<7>>(), 7 * AP_STACK_SIZE + 6144);
        let plan = parts(0x0205_0000 as *mut BootstrapStorage<7>);
        assert_eq!(plan.stack_base, 0x0205_0000);
        assert_eq!(plan.tables() as usize, 0x0205_0000 + 7 * AP_STACK_SIZE);
        assert!(plan.validate(8, 7).is_ok());
        assert!(matches!(
            plan.validate(9, 7),
            Err(MpError::ResidentStorageTooSmall {
                required: 8,
                available: 7
            })
        ));
        assert!(matches!(
            parts(0x8000 as *mut BootstrapStorage<7>).validate(8, 7),
            Err(MpError::TrampolinePlacementFailed)
        ));
        assert!(matches!(
            parts(0x30000 as *mut BootstrapStorage<7>).validate(8, 7),
            Err(MpError::TrampolinePlacementFailed)
        ));
        assert!(matches!(
            parts(0xffff_f000 as *mut BootstrapStorage<7>).validate(8, 7),
            Err(MpError::TrampolinePlacementFailed)
        ));
        assert!(parts(0 as *mut BootstrapStorage<0>).validate(1, 0).is_ok());
    }
}
