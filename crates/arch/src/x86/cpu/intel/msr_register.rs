//! Typed access to Intel model-specific registers.
//!
//! Unlike MMIO, `rdmsr`/`wrmsr` are only valid on CPUs implementing a given
//! register. Keep that precondition explicit instead of implementing the safe
//! `tock_registers::interfaces` traits over an unsafe MSR operation.

use core::marker::PhantomData;
use tock_registers::{LocalRegisterCopy, RegisterLongName};

/// MSR index paired with the register's tock bitfield definition.
pub(super) struct Msr<R: RegisterLongName> {
    index: u32,
    register: PhantomData<R>,
}

impl<R: RegisterLongName> Msr<R> {
    pub(super) const fn new(index: u32) -> Self {
        Self {
            index,
            register: PhantomData,
        }
    }

    /// # Safety
    ///
    /// The current CPU must implement this MSR.
    pub(super) unsafe fn read(&self) -> LocalRegisterCopy<u64, R> {
        LocalRegisterCopy::new(unsafe { crate::x86::msr::rdmsr(self.index) })
    }

    /// # Safety
    ///
    /// The current CPU must implement this MSR and accept `value`.
    pub(super) unsafe fn write(&self, value: u64) {
        unsafe { crate::x86::msr::wrmsr(self.index, value) };
    }
}
