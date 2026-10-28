//! Typed model-specific registers.
//!
//! An [`Msr`] names a register and its tock bitfields; an [`MsrAccess`]
//! reaches the MSRs of the executing CPU. Together they are a tock register,
//! so the bitfields work with `read`, `write`, `modify`, etc.:
//!
//! ```ignore
//! let cpu = unsafe { NativeMsr::new() };
//! cpu.reg(SYSCFG).modify(Syscfg::TOM2_WB::SET);
//! ```
//!
//! [`NativeMsr`] is the CPU itself. `rdmsr`/`wrmsr` are only valid on CPUs
//! implementing the register, so its constructor carries that precondition.
//! Tests use a model of the registers ([`testing`]).

use core::marker::PhantomData;
use tock_registers::RegisterLongName;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_bitfields;

/// MSR `index` with the bitfields `R`.
pub struct Msr<R: RegisterLongName = ()> {
    index: u32,
    _reg: PhantomData<R>,
}
impl<R: RegisterLongName> Clone for Msr<R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R: RegisterLongName> Copy for Msr<R> {}
impl<R: RegisterLongName> core::fmt::Debug for Msr<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Msr({:#x})", self.index)
    }
}
impl<R: RegisterLongName> Msr<R> {
    pub const fn new(index: u32) -> Self {
        Self {
            index,
            _reg: PhantomData,
        }
    }
    pub const fn index(self) -> u32 {
        self.index
    }
    /// The register `n` further on: an instance of a register array.
    pub const fn offset(self, n: u32) -> Self {
        Self::new(self.index + n)
    }
}

/// The MSRs of the executing CPU.
pub trait MsrAccess {
    fn read(&self, index: u32) -> u64;
    fn write(&self, index: u32, value: u64);

    /// `msr` as a tock register.
    fn reg<R: RegisterLongName>(&self, msr: Msr<R>) -> MsrRegister<'_, Self, R> {
        MsrRegister {
            cpu: self,
            index: msr.index,
            _reg: PhantomData,
        }
    }
}
impl<A: MsrAccess + ?Sized> MsrAccess for &A {
    fn read(&self, index: u32) -> u64 {
        (**self).read(index)
    }
    fn write(&self, index: u32, value: u64) {
        (**self).write(index, value)
    }
}

/// An MSR of a CPU as a tock register.
pub struct MsrRegister<'a, A: ?Sized, R: RegisterLongName> {
    cpu: &'a A,
    index: u32,
    _reg: PhantomData<R>,
}
impl<A: MsrAccess + ?Sized, R: RegisterLongName> Readable for MsrRegister<'_, A, R> {
    type T = u64;
    type R = R;
    fn get(&self) -> u64 {
        self.cpu.read(self.index)
    }
}
impl<A: MsrAccess + ?Sized, R: RegisterLongName> Writeable for MsrRegister<'_, A, R> {
    type T = u64;
    type R = R;
    fn set(&self, value: u64) {
        self.cpu.write(self.index, value)
    }
}

/// The executing CPU's MSRs. Not `Send`: MSRs belong to one CPU.
#[derive(Clone, Copy)]
pub struct NativeMsr {
    _cpu: PhantomData<*mut ()>,
}
impl NativeMsr {
    /// # Safety
    /// Ring 0, on a CPU implementing every MSR accessed through this, with
    /// values it accepts. Callers serialize MSRs shared with other CPUs
    /// (SMT siblings, core complexes).
    pub unsafe fn new() -> Self {
        Self { _cpu: PhantomData }
    }
}
impl MsrAccess for NativeMsr {
    fn read(&self, index: u32) -> u64 {
        // SAFETY: the constructor's contract.
        unsafe { crate::x86::msr::rdmsr(index) }
    }
    fn write(&self, index: u32, value: u64) {
        // SAFETY: the constructor's contract.
        unsafe { crate::x86::msr::wrmsr(index, value) }
    }
}

register_bitfields![u64,
    pub ApicBase [
        BSP OFFSET(8) NUMBITS(1) [],
        X2APIC_ENABLE OFFSET(10) NUMBITS(1) [],
        ENABLE OFFSET(11) NUMBITS(1) []
    ],
    pub MtrrCap [
        VARIABLE_COUNT OFFSET(0) NUMBITS(8) [],
        FIXED OFFSET(8) NUMBITS(1) [],
        WRITE_COMBINING OFFSET(10) NUMBITS(1) [],
        SMRR OFFSET(11) NUMBITS(1) []
    ],
    pub MtrrDefType [
        TYPE OFFSET(0) NUMBITS(8) [],
        FIXED_ENABLE OFFSET(10) NUMBITS(1) [],
        ENABLE OFFSET(11) NUMBITS(1) []
    ],
    /// IA32_SPEC_CTRL.
    pub SpecCtrl [
        IBRS OFFSET(0) NUMBITS(1) [],
        STIBP OFFSET(1) NUMBITS(1) [],
        SSBD OFFSET(2) NUMBITS(1) []
    ],
    pub McgCap [
        COUNT OFFSET(0) NUMBITS(8) []
    ]
];

pub const APIC_BASE: Msr<ApicBase::Register> = Msr::new(0x1b);
pub const SPEC_CTRL: Msr<SpecCtrl::Register> = Msr::new(0x48);
/// IA32_BIOS_SIGN_ID: the installed microcode patch level.
pub const PATCH_LEVEL: Msr = Msr::new(0x8b);
pub const MTRR_CAP: Msr<MtrrCap::Register> = Msr::new(crate::x86::mtrr::IA32_MTRR_CAP);
pub const PAT: Msr = Msr::new(0x277);
pub const MTRR_DEF_TYPE: Msr<MtrrDefType::Register> =
    Msr::new(crate::x86::mtrr::IA32_MTRR_DEF_TYPE);
pub const MCG_CAP: Msr<McgCap::Register> = Msr::new(0x179);
pub const MCG_STATUS: Msr = Msr::new(0x17a);

#[cfg(any(test, feature = "testing"))]
pub mod testing {
    //! MSR models for tests.
    extern crate std;
    use super::MsrAccess;
    use core::cell::RefCell;
    use std::{collections::BTreeMap, vec::Vec};

    /// A test model of a CPU's MSRs with side effects, made a [`MsrAccess`]
    /// by a [`RefCell`].
    pub trait MsrModel {
        fn read(&mut self, index: u32) -> u64;
        fn write(&mut self, index: u32, value: u64);
    }
    impl<M: MsrModel> MsrAccess for RefCell<M> {
        fn read(&self, index: u32) -> u64 {
            self.borrow_mut().read(index)
        }
        fn write(&self, index: u32, value: u64) {
            self.borrow_mut().write(index, value)
        }
    }

    /// Plain registers: unset ones read as zero, writes are logged.
    #[derive(Debug, Default)]
    pub struct MsrSpace {
        pub values: BTreeMap<u32, u64>,
        pub writes: Vec<(u32, u64)>,
    }
    impl MsrSpace {
        pub fn new(values: impl IntoIterator<Item = (u32, u64)>) -> RefCell<Self> {
            RefCell::new(Self {
                values: values.into_iter().collect(),
                writes: Vec::new(),
            })
        }
        pub fn get(&self, index: u32) -> u64 {
            self.values.get(&index).copied().unwrap_or(0)
        }
        pub fn written(&self, index: u32) -> bool {
            self.writes.iter().any(|&(i, _)| i == index)
        }
    }
    /// An order-sensitive digest of a write log, for tests that pin a
    /// whole register sequence.
    pub fn fingerprint(writes: &[(u32, u64)]) -> u64 {
        writes
            .iter()
            .fold(0xcbf2_9ce4_8422_2325, |hash, &(index, value)| {
                (u64::from(index) ^ value.rotate_left(17) ^ hash)
                    .wrapping_mul(0x0000_0100_0000_01b3)
            })
    }
    impl MsrModel for MsrSpace {
        fn read(&mut self, index: u32) -> u64 {
            self.get(index)
        }
        fn write(&mut self, index: u32, value: u64) {
            self.values.insert(index, value);
            self.writes.push((index, value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::MsrSpace;
    use super::*;
    use tock_registers::interfaces::ReadWriteable;

    #[test]
    fn bitfields_through_the_access() {
        let cpu = MsrSpace::new([(0x1b, 0xfee0_0100)]);
        cpu.reg(APIC_BASE).modify(ApicBase::ENABLE::SET);
        assert!(cpu.reg(APIC_BASE).is_set(ApicBase::BSP));
        let cpu = cpu.into_inner();
        assert_eq!(cpu.writes, [(0x1b, 0xfee0_0900)]);
    }
}
