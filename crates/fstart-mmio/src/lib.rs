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

    #[cfg(target_arch = "x86_64")]
    // SAFETY: `mfence` is a full serializing fence for loads and stores.
    // On x86 MMIO (mapped as UC/WC), this ensures all prior writes are
    // visible to the device before subsequent reads/writes.
    unsafe {
        core::arch::asm!("mfence", options(nostack, preserves_flags));
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

/// Read a `u64` from an MMIO register with a trailing barrier.
///
/// # Safety
/// `addr` must point to a valid, mapped, 8-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn read64(addr: *const u64) -> u64 {
    let val = ptr::read_volatile(addr);
    iomb();
    val
}

/// Write a `u64` to an MMIO register with leading and trailing barriers.
///
/// # Safety
/// `addr` must point to a valid, mapped, 8-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn write64(addr: *mut u64, val: u64) {
    iomb();
    ptr::write_volatile(addr, val);
    iomb();
}

// ---------------------------------------------------------------------------
// Bounded raw BAR helpers
// ---------------------------------------------------------------------------

/// Common bounded raw-MMIO helpers for BAR-like register windows.
///
/// Prefer named `register_structs!` fields for fixed offsets. This trait is
/// intended for computed register offsets (for example channel/rank/lane tables)
/// and temporary access to not-yet-documented registers. Implementors provide a
/// base address and window size; every helper checks that the requested access
/// fits inside the window before touching MMIO.
pub trait RawMmioBar {
    /// Size of the mapped MMIO window in bytes.
    const SIZE: usize;

    /// Base virtual/physical address of the mapped MMIO window.
    fn base(&self) -> usize;

    #[inline]
    fn checked_addr<T>(&self, off: u32) -> *mut T {
        let off = off as usize;
        let width = core::mem::size_of::<T>();
        let end = off
            .checked_add(width)
            .expect("MMIO offset arithmetic overflow");
        assert!(
            end <= Self::SIZE,
            "MMIO access out of bounds: offset={:#x} width={} size={:#x}",
            off,
            width,
            Self::SIZE
        );
        let addr = self
            .base()
            .checked_add(off)
            .expect("MMIO address arithmetic overflow");
        assert!(
            addr.is_multiple_of(core::mem::align_of::<T>()),
            "unaligned MMIO access: addr={:#x} align={}",
            addr,
            core::mem::align_of::<T>()
        );
        addr as *mut T
    }

    /// Read an 8-bit MMIO register at `off`.
    #[inline]
    fn read8(&self, off: u32) -> u8 {
        // SAFETY: `checked_addr` verifies that the access is in bounds.
        unsafe { read8(self.checked_addr::<u8>(off)) }
    }

    /// Write an 8-bit MMIO register at `off`.
    #[inline]
    fn write8(&self, off: u32, val: u8) {
        // SAFETY: `checked_addr` verifies that the access is in bounds.
        unsafe { write8(self.checked_addr::<u8>(off), val) }
    }

    /// Read a 16-bit MMIO register at `off`.
    #[inline]
    fn read16(&self, off: u32) -> u16 {
        // SAFETY: `checked_addr` verifies that the access is in bounds/aligned.
        unsafe { read16(self.checked_addr::<u16>(off)) }
    }

    /// Write a 16-bit MMIO register at `off`.
    #[inline]
    fn write16(&self, off: u32, val: u16) {
        // SAFETY: `checked_addr` verifies that the access is in bounds/aligned.
        unsafe { write16(self.checked_addr::<u16>(off), val) }
    }

    /// Read a 32-bit MMIO register at `off`.
    #[inline]
    fn read32(&self, off: u32) -> u32 {
        // SAFETY: `checked_addr` verifies that the access is in bounds/aligned.
        unsafe { read32(self.checked_addr::<u32>(off)) }
    }

    /// Write a 32-bit MMIO register at `off`.
    #[inline]
    fn write32(&self, off: u32, val: u32) {
        // SAFETY: `checked_addr` verifies that the access is in bounds/aligned.
        unsafe { write32(self.checked_addr::<u32>(off), val) }
    }

    /// Set bits in an 8-bit register.
    #[inline]
    fn setbits8(&self, off: u32, bits: u8) {
        self.write8(off, self.read8(off) | bits);
    }

    /// Clear bits in an 8-bit register.
    #[inline]
    fn clrbits8(&self, off: u32, bits: u8) {
        self.write8(off, self.read8(off) & !bits);
    }

    /// Clear and set bits in an 8-bit register.
    #[inline]
    fn clrsetbits8(&self, off: u32, clear: u8, set: u8) {
        self.write8(off, (self.read8(off) & !clear) | set);
    }

    /// Set bits in a 16-bit register.
    #[inline]
    fn setbits16(&self, off: u32, bits: u16) {
        self.write16(off, self.read16(off) | bits);
    }

    /// Clear bits in a 16-bit register.
    #[inline]
    fn clrbits16(&self, off: u32, bits: u16) {
        self.write16(off, self.read16(off) & !bits);
    }

    /// Clear and set bits in a 16-bit register.
    #[inline]
    fn clrsetbits16(&self, off: u32, clear: u16, set: u16) {
        self.write16(off, (self.read16(off) & !clear) | set);
    }

    /// Set bits in a 32-bit register.
    #[inline]
    fn setbits32(&self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) | bits);
    }

    /// Clear bits in a 32-bit register.
    #[inline]
    fn clrbits32(&self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) & !bits);
    }

    /// Clear and set bits in a 32-bit register.
    #[inline]
    fn clrsetbits32(&self, off: u32, clear: u32, set: u32) {
        self.write32(off, (self.read32(off) & !clear) | set);
    }
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
