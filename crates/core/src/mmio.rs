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
//! On ARMv7, ARMv6-M and AArch64, barriers use inline `dmb sy` (full-system data
//! memory barrier), matching U-Boot exactly.  Rust's
//! `core::sync::atomic::fence(SeqCst)` cannot be used here because LLVM
//! lowers it to `dmb ish` (inner-shareable), which does NOT order accesses
//! to device memory outside the inner-shareable domain.
//!
//! ARMv5TE and the ARMv6 A-profile have no `dmb`; there the CP15
//! drain-write-buffer operation (the ARMv6 DSB) takes its place. Earlier
//! ARM architectures may lack CP15 altogether and are rejected at build
//! time.
//!
//! On RISC-V, targeted `fence iorw, iorw` instructions are used since
//! Rust's generic fence doesn't map to these precise orderings.
//!
//! # Usage — free functions
//!
//! ```ignore
//! use fstart_core::mmio::{read32, write32, read8, write8};
//!
//! let val = unsafe { read32(0x01C2_0000 as *const u32) };
//! unsafe { write32(0x01C2_0000 as *mut u32, val | 0x01) };
//! unsafe { write8(addr, 0x03) };
//! ```
//!
//! # Usage — register structs
//!
//! ```ignore
//! use fstart_core::mmio::{MmioReadWrite, MmioReadOnly};
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

#![expect(
    clippy::disallowed_methods,
    reason = "the barrier-aware accessors everything else uses"
)]

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
/// - ARMv7/ARMv6-M/AArch64: `dmb sy` — must be full-system (`sy`), NOT inner-shareable
///   (`ish`).  Rust's `fence(SeqCst)` emits `dmb ish` which does NOT order
///   device-memory accesses.  U-Boot uses `dmb sy` everywhere for MMIO.
/// - ARMv5TE/ARMv6 A-profile: `mcr p15, 0, rN, c7, c10, 4` (drain write
///   buffer; DSB on ARMv6), the only barrier these cores have.
/// - RISC-V: targeted `fence iorw, iorw` (full device+memory barrier).
/// - Other targets (host tests): no-op.
#[cfg(all(target_arch = "arm", not(target_feature = "v5te")))]
compile_error!("MMIO barriers need ARMv5TE or later: earlier cores may have no CP15");

#[inline(always)]
fn iomb() {
    #[cfg(any(miri, kani))]
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

    #[cfg(all(
        not(any(miri, kani)),
        any(
            all(
                target_arch = "arm",
                any(target_feature = "v7", target_feature = "v6m")
            ),
            target_arch = "aarch64"
        )
    ))]
    // SAFETY: `dmb sy` is a full-system data memory barrier, no side effects.
    unsafe {
        core::arch::asm!("dmb sy", options(nostack, preserves_flags));
    }

    #[cfg(all(
        not(any(miri, kani)),
        target_arch = "arm",
        target_feature = "v5te",
        not(any(target_feature = "v7", target_feature = "v6m"))
    ))]
    // SAFETY: CP15 c7/c10/4 drains the write buffer; the source register
    // should be zero and nothing else changes.
    unsafe {
        core::arch::asm!(
            "mcr p15, 0, {zero}, c7, c10, 4",
            zero = in(reg) 0u32,
            options(nostack, preserves_flags)
        );
    }

    #[cfg(all(
        not(any(miri, kani)),
        any(target_arch = "riscv32", target_arch = "riscv64")
    ))]
    // SAFETY: `fence iorw, iorw` is a full device+memory barrier.
    unsafe {
        core::arch::asm!("fence iorw, iorw", options(nostack, preserves_flags));
    }

    #[cfg(all(not(any(miri, kani)), any(target_arch = "x86", target_arch = "x86_64")))]
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
    // SAFETY: caller guarantees a valid, mapped MMIO register.
    unsafe {
        let val = ptr::read_volatile(addr);
        iomb();
        val
    }
}

/// Write a `u8` to an MMIO register with leading and trailing barriers.
///
/// # Safety
/// `addr` must point to a valid, mapped MMIO register.
#[inline(always)]
pub unsafe fn write8(addr: *mut u8, val: u8) {
    // SAFETY: caller guarantees a valid, mapped MMIO register.
    unsafe {
        iomb();
        ptr::write_volatile(addr, val);
        iomb();
    }
}

/// Read a `u16` from an MMIO register with a trailing barrier.
///
/// # Safety
/// `addr` must point to a valid, mapped, 2-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn read16(addr: *const u16) -> u16 {
    // SAFETY: caller guarantees a valid, mapped, aligned MMIO register.
    unsafe {
        let val = ptr::read_volatile(addr);
        iomb();
        val
    }
}

/// Write a `u16` to an MMIO register with leading and trailing barriers.
///
/// # Safety
/// `addr` must point to a valid, mapped, 2-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn write16(addr: *mut u16, val: u16) {
    // SAFETY: caller guarantees a valid, mapped, aligned MMIO register.
    unsafe {
        iomb();
        ptr::write_volatile(addr, val);
        iomb();
    }
}

/// Read a `u32` from an MMIO register with a trailing barrier.
///
/// # Safety
/// `addr` must point to a valid, mapped, 4-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn read32(addr: *const u32) -> u32 {
    // SAFETY: caller guarantees a valid, mapped, aligned MMIO register.
    unsafe {
        let val = ptr::read_volatile(addr);
        iomb();
        val
    }
}

/// Write a `u32` to an MMIO register with leading and trailing barriers.
///
/// # Safety
/// `addr` must point to a valid, mapped, 4-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn write32(addr: *mut u32, val: u32) {
    // SAFETY: caller guarantees a valid, mapped, aligned MMIO register.
    unsafe {
        iomb();
        ptr::write_volatile(addr, val);
        iomb();
    }
}

/// Load a possibly unaligned MMIO word with one volatile x86 instruction.
/// Unlike `read_unaligned`, this access has compiler-visible memory effects.
/// The CPU/chipset may split the transaction; atomicity is not guaranteed.
///
/// # Safety
/// `addr` must name two mapped bytes of a device register supporting this access.
/// Under `cfg(any(miri, kani))`, the fallback models normal allocated RAM
/// rather than volatile device MMIO, so `addr` must point to two readable bytes
/// of a live Rust allocation.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[inline(always)]
pub unsafe fn read16_unaligned(addr: *const u8) -> u16 {
    #[cfg(any(miri, kani))]
    unsafe {
        let value = ptr::read_unaligned(addr.cast::<u16>());
        iomb();
        value
    }
    #[cfg(not(any(miri, kani)))]
    {
        let value: u16;
        unsafe {
            core::arch::asm!("mov {value:x}, word ptr [{addr}]", addr = in(reg) addr,
                value = out(reg) value, options(nostack, preserves_flags));
            iomb();
        }
        value
    }
}

/// Store a possibly unaligned MMIO word with one volatile x86 instruction
/// and the same barriers as aligned stores. The CPU/chipset may split the
/// transaction; atomicity is not guaranteed.
///
/// # Safety
/// `addr` must name two mapped bytes of a device register supporting this access.
/// Under `cfg(any(miri, kani))`, the fallback models normal allocated RAM
/// rather than volatile device MMIO, so `addr` must point to two readable bytes
/// of a live Rust allocation.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[inline(always)]
pub unsafe fn write16_unaligned(addr: *mut u8, value: u16) {
    unsafe {
        iomb();
        #[cfg(any(miri, kani))]
        ptr::write_unaligned(addr.cast::<u16>(), value);
        #[cfg(not(any(miri, kani)))]
        core::arch::asm!("mov word ptr [{addr}], {value:x}", addr = in(reg) addr,
            value = in(reg) value, options(nostack, preserves_flags));
        iomb();
    }
}

/// Load a possibly unaligned MMIO dword with one volatile x86 instruction.
/// The assembly has compiler-visible memory effects and is not pure. The
/// CPU/chipset may split the transaction; atomicity is not guaranteed.
///
/// # Safety
/// `addr` must name four mapped bytes of a device register supporting this access.
/// Under `cfg(any(miri, kani))`, the fallback models normal allocated RAM
/// rather than volatile device MMIO, so `addr` must point to four readable or
/// writable bytes of a live Rust allocation.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[inline(always)]
pub unsafe fn read32_unaligned(addr: *const u8) -> u32 {
    #[cfg(any(miri, kani))]
    unsafe {
        let value = ptr::read_unaligned(addr.cast::<u32>());
        iomb();
        value
    }
    #[cfg(not(any(miri, kani)))]
    {
        let value: u32;
        unsafe {
            core::arch::asm!("mov {value:e}, dword ptr [{addr}]", addr = in(reg) addr,
                value = out(reg) value, options(nostack, preserves_flags));
            iomb();
        }
        value
    }
}

/// Store a possibly unaligned MMIO dword with one volatile x86 instruction
/// and the same barriers as aligned stores. The CPU/chipset may split the
/// transaction; atomicity is not guaranteed.
///
/// # Safety
/// `addr` must name four mapped bytes of a device register supporting this access.
/// Under `cfg(any(miri, kani))`, the fallback models normal allocated RAM
/// rather than volatile device MMIO, so `addr` must point to four readable or
/// writable bytes of a live Rust allocation.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[inline(always)]
pub unsafe fn write32_unaligned(addr: *mut u8, value: u32) {
    unsafe {
        iomb();
        #[cfg(any(miri, kani))]
        ptr::write_unaligned(addr.cast::<u32>(), value);
        #[cfg(not(any(miri, kani)))]
        core::arch::asm!("mov dword ptr [{addr}], {value:e}", addr = in(reg) addr,
            value = in(reg) value, options(nostack, preserves_flags));
        iomb();
    }
}

/// Read a `u64` from an MMIO register with a trailing barrier.
///
/// # Safety
/// `addr` must point to a valid, mapped, 8-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn read64(addr: *const u64) -> u64 {
    // SAFETY: caller guarantees a valid, mapped, aligned MMIO register.
    unsafe {
        let val = ptr::read_volatile(addr);
        iomb();
        val
    }
}

/// Write a `u64` to an MMIO register with leading and trailing barriers.
///
/// # Safety
/// `addr` must point to a valid, mapped, 8-byte-aligned MMIO register.
#[inline(always)]
pub unsafe fn write64(addr: *mut u64, val: u64) {
    // SAFETY: caller guarantees a valid, mapped, aligned MMIO register.
    unsafe {
        iomb();
        ptr::write_volatile(addr, val);
        iomb();
    }
}

#[cfg(all(test, any(target_arch = "x86", target_arch = "x86_64")))]
mod unaligned_tests {
    use super::*;

    #[test]
    fn preserves_width_and_adjacent_bytes() {
        let mut bytes = [0xa5u8; 16];
        for offset in 1..4 {
            bytes.fill(0xa5);
            // SAFETY: ordinary RAM provides a mapped test backing for the access.
            unsafe {
                write32_unaligned(bytes.as_mut_ptr().add(offset), 0x1234_5678);
                assert_eq!(read32_unaligned(bytes.as_ptr().add(offset)), 0x1234_5678);
            }
            assert_eq!(&bytes[offset..offset + 4], &[0x78, 0x56, 0x34, 0x12]);
            assert!(bytes[..offset].iter().all(|&byte| byte == 0xa5));
            assert!(bytes[offset + 4..].iter().all(|&byte| byte == 0xa5));
        }
        unsafe {
            write16_unaligned(bytes.as_mut_ptr().add(1), 0xcdef);
            assert_eq!(read16_unaligned(bytes.as_ptr().add(1)), 0xcdef);
        }
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

/// A typed MMIO register at an address, the MMIO counterpart of
/// `pio::PioRegister`: `Copy`, so it can be passed around and used inside
/// composite registers such as [`crate::indexed::Indexed`]. Accesses have
/// the barriers of [`MmioReadWrite`].
pub struct MmioRegister<T: UIntLike, R: RegisterLongName = ()> {
    address: usize,
    _reg: PhantomData<(T, R)>,
}

impl<T: UIntLike, R: RegisterLongName> Clone for MmioRegister<T, R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: UIntLike, R: RegisterLongName> Copy for MmioRegister<T, R> {}

impl<T: UIntLike, R: RegisterLongName> MmioRegister<T, R> {
    /// # Safety
    /// `address` is a mapped, naturally aligned MMIO register of width `T`
    /// for as long as this value or a copy of it is used.
    pub const unsafe fn new(address: usize) -> Self {
        Self {
            address,
            _reg: PhantomData,
        }
    }

    pub const fn address(&self) -> usize {
        self.address
    }
}

impl<T: UIntLike, R: RegisterLongName> Readable for MmioRegister<T, R> {
    type T = T;
    type R = R;

    #[inline(always)]
    fn get(&self) -> T {
        // SAFETY: the constructor's contract.
        let val = unsafe { ptr::read_volatile(self.address as *const T) };
        iomb();
        val
    }
}

impl<T: UIntLike, R: RegisterLongName> Writeable for MmioRegister<T, R> {
    type T = T;
    type R = R;

    #[inline(always)]
    fn set(&self, value: T) {
        iomb();
        // SAFETY: the constructor's contract.
        unsafe { ptr::write_volatile(self.address as *mut T, value) };
        iomb();
    }
}

// ---------------------------------------------------------------------------
// Bounded MMIO windows and typed BAR register descriptors
// ---------------------------------------------------------------------------

mod sealed {
    pub trait MmioPrimitive: tock_registers::UIntLike {
        const WIDTH: u32;
    }

    impl MmioPrimitive for u8 {
        const WIDTH: u32 = 1;
    }
    impl MmioPrimitive for u16 {
        const WIDTH: u32 = 2;
    }
    impl MmioPrimitive for u32 {
        const WIDTH: u32 = 4;
    }
    impl MmioPrimitive for u64 {
        const WIDTH: u32 = 8;
    }
}

/// Sealed unsigned integer widths valid for typed MMIO registers (`u8`, `u16`,
/// `u32`, `u64`). Every bit pattern of an `MmioPrimitive` is valid.
pub trait MmioPrimitive: sealed::MmioPrimitive {}
impl MmioPrimitive for u8 {}
impl MmioPrimitive for u16 {}
impl MmioPrimitive for u32 {}
impl MmioPrimitive for u64 {}

/// A typed register offset inside an [`MmioWindow`].
///
/// Encodes the register's byte offset, integer width `T`, and optional
/// `tock-registers` bitfield type `R` so sparse or computed-offset BARs
/// (such as DRAM training register spaces) can still use typed
/// [`MmioRegister`] accessors without an all-encompassing `register_structs!`.
#[derive(Debug, PartialEq, Eq)]
pub struct BarReg<T: MmioPrimitive, R: RegisterLongName = ()> {
    offset: u32,
    _reg: PhantomData<(T, R)>,
}

impl<T: MmioPrimitive, R: RegisterLongName> Clone for BarReg<T, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: MmioPrimitive, R: RegisterLongName> Copy for BarReg<T, R> {}

impl<T: MmioPrimitive, R: RegisterLongName> BarReg<T, R> {
    /// Declare a register at `offset` bytes from the start of a BAR.
    #[must_use]
    pub const fn new(offset: u32) -> Self {
        Self {
            offset,
            _reg: PhantomData,
        }
    }

    /// Shift this register descriptor by `delta` bytes (for per-channel or
    /// per-lane register arrays).
    ///
    /// # Panics
    ///
    /// Panics on `u32` offset overflow.
    #[must_use]
    pub const fn offset_by(self, delta: u32) -> Self {
        Self {
            offset: self
                .offset
                .checked_add(delta)
                .expect("BarReg offset overflow"),
            _reg: PhantomData,
        }
    }

    /// Byte offset from the BAR base.
    #[must_use]
    pub const fn offset(self) -> u32 {
        self.offset
    }
}

/// A bounded MMIO register window (`[base, base + size)`).
///
/// The single `unsafe` boundary is [`MmioWindow::new`], which establishes
/// that the entire `[base, base + size)` range is a valid mapped MMIO
/// region. Every access and [`subwindow`](Self::subwindow) is bounds-checked
/// against `size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioWindow {
    base: usize,
    size: u32,
}

impl MmioWindow {
    /// Construct a bounded MMIO window over `[base, base + size)`.
    ///
    /// # Safety
    ///
    /// - `[base, base + size)` must be a valid, mapped MMIO register region
    ///   (or test buffer) for as long as this window or any copy/derived
    ///   [`MmioRegister`] is used.
    /// - The underlying device/buffer must support the access widths (and on
    ///   x86, any unaligned accesses) issued through this window without
    ///   violating hardware or memory-model invariants.
    /// - Callers must synchronize concurrent accesses (e.g. single-threaded
    ///   boot or external locking) so RAM-backed test buffers do not race.
    ///
    /// # Panics
    ///
    /// Panics if `base == 0`, `size == 0`, or the last byte `base + size - 1`
    /// overflows `usize`.
    #[must_use]
    pub const unsafe fn new(base: usize, size: u32) -> Self {
        assert!(base != 0, "MMIO window base must be non-null");
        assert!(size != 0, "MMIO window size must be non-zero");
        assert!(
            base.checked_add(size as usize - 1).is_some(),
            "MMIO window overflows address space"
        );
        Self { base, size }
    }

    /// Physical/virtual base address of this window.
    #[must_use]
    pub const fn base(self) -> usize {
        self.base
    }

    /// Size of this window in bytes.
    #[must_use]
    pub const fn size(self) -> u32 {
        self.size
    }

    /// Carve a bounded sub-window `[offset, offset + size)` out of `self`.
    ///
    /// # Panics
    ///
    /// Panics if `size == 0` or `offset + size > self.size()`.
    #[must_use]
    pub const fn subwindow(self, offset: u32, size: u32) -> Self {
        assert!(size != 0, "MMIO subwindow size must be non-zero");
        let end = offset
            .checked_add(size)
            .expect("MMIO subwindow offset overflow");
        assert!(end <= self.size, "MMIO subwindow exceeds parent bounds");
        Self {
            base: self.base + offset as usize,
            size,
        }
    }

    /// Compute and bounds-check the address for an access of `width` bytes at `off`.
    ///
    /// # Panics
    ///
    /// Panics if `width == 0`, `off + width` overflows, or `off + width > self.size()`.
    #[inline]
    pub const fn register_address(self, off: u32, width: u32) -> usize {
        assert!(width != 0, "MMIO access width must be non-zero");
        let end = off.checked_add(width).expect("MMIO offset overflow");
        assert!(end <= self.size, "MMIO access exceeds window bounds");
        self.base + off as usize
    }

    /// Resolve a typed [`BarReg`] inside this window into a barrier-aware
    /// [`MmioRegister`] for `tock-registers` `.read()`/`.modify()`/`.write()`.
    ///
    /// # Panics
    ///
    /// Panics if the register exceeds `self.size()` or its resolved address
    /// is not naturally aligned for `T` (even on x86; deliberately unaligned
    /// registers must use [`read16`](Self::read16)/[`write16`](Self::write16)
    /// or [`read32`](Self::read32)/[`write32`](Self::write32)).
    #[inline]
    #[must_use]
    pub fn reg<T: MmioPrimitive, R: RegisterLongName>(
        self,
        reg: BarReg<T, R>,
    ) -> MmioRegister<T, R> {
        let addr = self.register_address(reg.offset(), T::WIDTH);
        assert!(
            addr.is_multiple_of(core::mem::align_of::<T>()),
            "MMIO register address is not aligned"
        );
        // SAFETY: `MmioWindow::new` guarantees `[base, base + size)` is mapped,
        // `MmioPrimitive` guarantees all bit patterns are valid, and we checked
        // both bounds and natural alignment above.
        unsafe { MmioRegister::new(addr) }
    }

    #[inline]
    #[must_use]
    pub fn read8(self, off: u32) -> u8 {
        // SAFETY: bounds checked by `register_address`; window mapping guaranteed by `new`.
        unsafe { read8(self.register_address(off, 1) as *const u8) }
    }

    #[inline]
    pub fn write8(self, off: u32, val: u8) {
        // SAFETY: bounds checked by `register_address`; window mapping guaranteed by `new`.
        unsafe { write8(self.register_address(off, 1) as *mut u8, val) }
    }

    #[inline]
    #[must_use]
    pub fn read16(self, off: u32) -> u16 {
        let addr = self.register_address(off, 2) as *const u16;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if !addr.is_aligned() {
            // Some x86 chipset register windows deliberately expose unaligned words.
            // SAFETY: bounds checked by `register_address`; window mapping guaranteed by `new`.
            return unsafe { read16_unaligned(addr.cast()) };
        }
        assert!(addr.is_aligned(), "unaligned 16-bit MMIO read");
        // SAFETY: bounds and alignment checked; window mapping guaranteed by `new`.
        unsafe { read16(addr) }
    }

    #[inline]
    pub fn write16(self, off: u32, val: u16) {
        let addr = self.register_address(off, 2) as *mut u16;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if !addr.is_aligned() {
            // SAFETY: bounds checked by `register_address`; window mapping guaranteed by `new`.
            unsafe { write16_unaligned(addr.cast(), val) };
            return;
        }
        assert!(addr.is_aligned(), "unaligned 16-bit MMIO write");
        // SAFETY: bounds and alignment checked; window mapping guaranteed by `new`.
        unsafe { write16(addr, val) }
    }

    #[inline]
    #[must_use]
    pub fn read32(self, off: u32) -> u32 {
        let addr = self.register_address(off, 4) as *const u32;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if !addr.is_aligned() {
            // SAFETY: bounds checked by `register_address`; window mapping guaranteed by `new`.
            return unsafe { read32_unaligned(addr.cast()) };
        }
        assert!(addr.is_aligned(), "unaligned 32-bit MMIO read");
        // SAFETY: bounds and alignment checked; window mapping guaranteed by `new`.
        unsafe { read32(addr) }
    }

    #[inline]
    pub fn write32(self, off: u32, val: u32) {
        let addr = self.register_address(off, 4) as *mut u32;
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if !addr.is_aligned() {
            // SAFETY: bounds checked by `register_address`; window mapping guaranteed by `new`.
            unsafe { write32_unaligned(addr.cast(), val) };
            return;
        }
        assert!(addr.is_aligned(), "unaligned 32-bit MMIO write");
        // SAFETY: bounds and alignment checked; window mapping guaranteed by `new`.
        unsafe { write32(addr, val) }
    }

    #[inline]
    pub fn setbits8(self, off: u32, bits: u8) {
        self.write8(off, self.read8(off) | bits);
    }

    #[inline]
    pub fn clrbits8(self, off: u32, bits: u8) {
        self.write8(off, self.read8(off) & !bits);
    }

    #[inline]
    pub fn clrsetbits8(self, off: u32, clear: u8, set: u8) {
        self.write8(off, (self.read8(off) & !clear) | set);
    }

    #[inline]
    pub fn setbits16(self, off: u32, bits: u16) {
        self.write16(off, self.read16(off) | bits);
    }

    #[inline]
    pub fn clrbits16(self, off: u32, bits: u16) {
        self.write16(off, self.read16(off) & !bits);
    }

    #[inline]
    pub fn clrsetbits16(self, off: u32, clear: u16, set: u16) {
        self.write16(off, (self.read16(off) & !clear) | set);
    }

    #[inline]
    pub fn setbits32(self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) | bits);
    }

    #[inline]
    pub fn clrbits32(self, off: u32, bits: u32) {
        self.write32(off, self.read32(off) & !bits);
    }

    #[inline]
    pub fn clrsetbits32(self, off: u32, clear: u32, set: u32) {
        self.write32(off, (self.read32(off) & !clear) | set);
    }

    /// Read-modify-write a 32-bit register with keep-mask semantics:
    /// `write32(off, (read32(off) & mask) | set)`.
    #[inline]
    pub fn modify32(self, off: u32, mask: u32, set: u32) {
        self.write32(off, (self.read32(off) & mask) | set);
    }
}

#[cfg(test)]
mod window_tests {
    use tock_registers::interfaces::{ReadWriteable, Readable};
    use tock_registers::register_bitfields;

    use super::{BarReg, MmioWindow};

    register_bitfields![u32,
        /// Test control register (offset 0x04, 32-bit).
        TEST_CTRL [
            ENABLE OFFSET(0) NUMBITS(1) [],
            MODE OFFSET(4) NUMBITS(3) []
        ]
    ];

    #[test]
    fn window_typed_reg_subwindow_and_rmw_helpers() {
        let mut backing = [0u32; 4];
        // SAFETY: `backing` is 16 bytes of valid aligned stack memory.
        let win = unsafe { MmioWindow::new(backing.as_mut_ptr() as usize, 16) };
        const CTRL: BarReg<u32, TEST_CTRL::Register> = BarReg::new(0).offset_by(4);

        win.reg(CTRL)
            .modify(TEST_CTRL::ENABLE::SET + TEST_CTRL::MODE.val(5));
        assert_eq!(win.reg(CTRL).read(TEST_CTRL::ENABLE), 1);
        assert_eq!(win.reg(CTRL).read(TEST_CTRL::MODE), 5);
        assert_eq!(backing[1], 0x51);

        let sub = win.subwindow(8, 8);
        sub.write32(4, 0xdead_beef);
        sub.modify32(4, 0xffff_0000, 0x0000_cafe);
        assert_eq!(backing[3], 0xdead_cafe);
        sub.setbits8(0, 0x0f);
        sub.clrbits8(0, 0x05);
        assert_eq!(sub.read8(0), 0x0a);
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn window_preserves_unaligned_widths_and_neighboring_bytes() {
        let mut backing = [0xa5a5_a5a5u32; 3];
        // SAFETY: `backing` is 12 bytes of valid stack memory.
        let win = unsafe { MmioWindow::new(backing.as_mut_ptr() as usize, 12) };
        win.write16(1, 0x1234);
        win.write32(5, 0x5678_9abc);
        assert_eq!(win.read8(0), 0xa5);
        assert_eq!(win.read16(1), 0x1234);
        assert_eq!(win.read8(3), 0xa5);
        assert_eq!(win.read8(4), 0xa5);
        assert_eq!(win.read32(5), 0x5678_9abc);
        assert_eq!(win.read8(9), 0xa5);
    }

    #[test]
    #[should_panic]
    fn window_rejects_zero_size() {
        let _ = unsafe { MmioWindow::new(0x1000, 0) };
    }

    #[test]
    #[should_panic]
    fn window_rejects_subwindow_exceeding_parent() {
        let win = unsafe { MmioWindow::new(0x1000, 8) };
        let _ = win.subwindow(4, 8);
    }

    #[test]
    #[should_panic]
    fn window_rejects_access_crossing_end() {
        // SAFETY: `register_address` does not dereference memory.
        let win = unsafe { MmioWindow::new(0x1000, 8) };
        let _ = win.register_address(5, 4);
    }

    #[test]
    #[should_panic]
    fn window_rejects_offset_overflow() {
        // SAFETY: `register_address` does not dereference memory.
        let win = unsafe { MmioWindow::new(0x1000, 8) };
        let _ = win.register_address(u32::MAX, 4);
    }

    #[test]
    #[should_panic]
    fn window_rejects_unaligned_typed_reg() {
        let mut backing = [0u32; 2];
        // SAFETY: `backing` is 8 bytes of valid stack memory.
        let win = unsafe { MmioWindow::new(backing.as_mut_ptr() as usize, 8) };
        let _ = win.reg(BarReg::<u32>::new(2));
    }
}
