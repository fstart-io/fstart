//! Barrier-aware MMIO accessors and tock-register-compatible register types.
//!
//! Provides two things:
//!
//! 1. **Free functions** (`read8`, `write8`, `read32`, `write32`, etc.) for
//!    one-off MMIO accesses where a full register struct isn't warranted.
//!
//! 2. **Drop-in replacements** for tock-registers' `ReadWrite`, `ReadOnly`,
//!    and `WriteOnly` that bracket every access with memory barriers.
//!    Use these in `register_structs!` definitions for hardware peripherals.
//!
//! Every MMIO write is bracketed by barriers (before AND after) and every
//! MMIO read is followed by a barrier.  The write pattern follows coreboot's
//! belt-and-suspenders approach:
//!
//! - **Write**: `fence → write_volatile → fence`
//! - **Read**:  `read_volatile → fence`
//!
//! On ARM and AArch64, barriers use inline `dmb sy` (full-system data
//! memory barrier), matching U-Boot exactly.  Rust's
//! `core::sync::atomic::fence(SeqCst)` cannot be used here because LLVM
//! lowers it to `dmb ish` (inner-shareable), which does NOT order accesses
//! to device memory outside the inner-shareable domain.
//!
//! On RISC-V, targeted `fence iorw, iorw` instructions are used since
//! Rust's generic fence doesn't map to these precise orderings.
//!
//! # Usage — free functions
//!
//! ```ignore
//! use fstart_mmio::{read32, write32, read8, write8};
//!
//! let val = unsafe { read32(0x01C2_0000 as *const u32) };
//! unsafe { write32(0x01C2_0000 as *mut u32, val | 0x01) };
//! unsafe { write8(addr, 0x03) };
//! ```
//!
//! # Usage — register structs
//!
//! ```ignore
//! use fstart_mmio::{MmioReadWrite, MmioReadOnly};
//! use tock_registers::{register_structs, register_bitfields};
//!
//! register_bitfields![u32, CTRL [ EN OFFSET(0) NUMBITS(1) [] ]];
//!
//! register_structs! {
//!     MyRegs {
//!         (0x00 => ctrl: MmioReadWrite<u32, CTRL::Register>),
//!         (0x04 => status: MmioReadOnly<u32>),
//!         (0x08 => @END),
//!     }
//! }
//! ```

#![no_std]

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ptr;

use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{RegisterLongName, UIntLike};

// Re-export tock_registers for consumers that need register_structs!/register_bitfields!
pub use tock_registers;

// ---------------------------------------------------------------------------
// Barriers
// ---------------------------------------------------------------------------

/// Full-system data memory barrier for MMIO ordering.
///
/// - ARM/AArch64: `dmb sy` — must be full-system (`sy`), NOT inner-shareable
///   (`ish`).  Rust's `fence(SeqCst)` emits `dmb ish` which does NOT order
///   device-memory accesses.  U-Boot uses `dmb sy` everywhere for MMIO.
/// - RISC-V: targeted `fence iorw, iorw` (full device+memory barrier).
/// - Other targets (host tests): no-op.
#[inline(always)]
fn iomb() {
    #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
    // SAFETY: `dmb sy` is a full-system data memory barrier, no side effects.
    unsafe {
        core::arch::asm!("dmb sy", options(nostack, preserves_flags));
    }

    #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
    // SAFETY: `fence iorw, iorw` is a full device+memory barrier.
    unsafe {
        core::arch::asm!("fence iorw, iorw", options(nostack, preserves_flags));
    }
}

// ---------------------------------------------------------------------------
// Public MMIO accessors — free functions
// ---------------------------------------------------------------------------

/// Read a `u8` from an MMIO register with a trailing barrier.
///
/// # Safety
/// `addr` must point to a valid, mapped MMIO register.
#[inline(always)]
pub unsafe fn read8(addr: *const u8) -> u8 {
    let val = ptr::read_volatile(addr);
    iomb();
    val
}

/// Write a `u8` to an MMIO register with leading and trailing barriers.
///
/// # Safety
/// `addr` must point to a valid, mapped MMIO register.
#[inline(always)]
pub unsafe fn write8(addr: *mut u8, val: u8) {
    iomb();
    ptr::write_volatile(addr, val);
    iomb();
}

/// Read a `u16` from an MMIO register with a trailing barrier.
///
/// # Safety
/// `addr` must point to a valid, mapped, 2-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn read16(addr: *const u16) -> u16 {
    let val = ptr::read_volatile(addr);
    iomb();
    val
}

/// Write a `u16` to an MMIO register with leading and trailing barriers.
///
/// # Safety
/// `addr` must point to a valid, mapped, 2-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn write16(addr: *mut u16, val: u16) {
    iomb();
    ptr::write_volatile(addr, val);
    iomb();
}

/// Read a `u32` from an MMIO register with a trailing barrier.
///
/// # Safety
/// `addr` must point to a valid, mapped, 4-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn read32(addr: *const u32) -> u32 {
    let val = ptr::read_volatile(addr);
    iomb();
    val
}

/// Write a `u32` to an MMIO register with leading and trailing barriers.
///
/// # Safety
/// `addr` must point to a valid, mapped, 4-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn write32(addr: *mut u32, val: u32) {
    iomb();
    ptr::write_volatile(addr, val);
    iomb();
}

// ---------------------------------------------------------------------------
// tock-registers-compatible MMIO register types
// ---------------------------------------------------------------------------

/// MMIO read-write register with memory barriers.
///
/// Drop-in replacement for `tock_registers::registers::ReadWrite<T, R>`.
/// Same layout — a single `UnsafeCell<T>` — so it works in
/// `register_structs!` offset calculations.
///
/// Every `get()` is followed by a barrier; every `set()` is bracketed
/// by barriers.
#[repr(transparent)]
pub struct MmioReadWrite<T: UIntLike, R: RegisterLongName = ()> {
    value: UnsafeCell<T>,
    _reg: PhantomData<R>,
}

impl<T: UIntLike, R: RegisterLongName> Readable for MmioReadWrite<T, R> {
    type T = T;
    type R = R;

    #[inline(always)]
    fn get(&self) -> T {
        let val = unsafe { ptr::read_volatile(self.value.get()) };
        iomb();
        val
    }
}

impl<T: UIntLike, R: RegisterLongName> Writeable for MmioReadWrite<T, R> {
    type T = T;
    type R = R;

    #[inline(always)]
    fn set(&self, value: T) {
        iomb();
        unsafe { ptr::write_volatile(self.value.get(), value) };
        iomb();
    }
}

/// MMIO read-only register with memory barriers.
///
/// Drop-in replacement for `tock_registers::registers::ReadOnly<T, R>`.
/// Every `get()` is followed by a barrier.
#[repr(transparent)]
pub struct MmioReadOnly<T: UIntLike, R: RegisterLongName = ()> {
    value: UnsafeCell<T>,
    _reg: PhantomData<R>,
}

impl<T: UIntLike, R: RegisterLongName> Readable for MmioReadOnly<T, R> {
    type T = T;
    type R = R;

    #[inline(always)]
    fn get(&self) -> T {
        let val = unsafe { ptr::read_volatile(self.value.get()) };
        iomb();
        val
    }
}

/// MMIO write-only register with memory barriers.
///
/// Drop-in replacement for `tock_registers::registers::WriteOnly<T, R>`.
/// Every `set()` is bracketed by barriers.
#[repr(transparent)]
pub struct MmioWriteOnly<T: UIntLike, R: RegisterLongName = ()> {
    value: UnsafeCell<T>,
    _reg: PhantomData<R>,
}

impl<T: UIntLike, R: RegisterLongName> Writeable for MmioWriteOnly<T, R> {
    type T = T;
    type R = R;

    #[inline(always)]
    fn set(&self, value: T) {
        iomb();
        unsafe { ptr::write_volatile(self.value.get(), value) };
        iomb();
    }
}
