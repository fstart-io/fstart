//! Global ECAM PCI config space access.
//!
//! Call [`init`] once after programming PCIEXBAR. After that,
//! use the free functions directly — no struct to pass around:
//!
//! ```ignore
//! fstart_ecam::init(0xE000_0000);
//! let rev = fstart_ecam::read8(0, 0, 0, 0x08);
//! fstart_ecam::write16(0, 0, 0, 0x52, (1 << 8) | (3 << 4));
//! ```

#![no_std]

use core::sync::atomic::{AtomicUsize, Ordering};

static BASE: AtomicUsize = AtomicUsize::new(0);

/// Set the ECAM base address. Call exactly once after the CF8/CFC
/// write that programs PCIEXBAR.
pub fn init(base: usize) {
    // Mask off low 20 bits — callers may pass the raw PCIEXBAR value
    // which includes enable/size bits.  ECAM addresses are 1 MiB-aligned.
    BASE.store(base & !0xF_FFFF, Ordering::Release);
}

/// Return the current ECAM base (0 if uninitialised).
#[inline]
pub fn base() -> usize {
    BASE.load(Ordering::Acquire)
}

#[inline]
fn addr(bus: u8, dev: u8, func: u8, reg: u16) -> usize {
    BASE.load(Ordering::Acquire)
        | ((bus as usize) << 20)
        | ((dev as usize) << 15)
        | ((func as usize) << 12)
        | ((reg as usize) & 0xFFF)
}

/// Read a 32-bit PCI config register.
#[inline]
pub fn read32(bus: u8, dev: u8, func: u8, reg: u16) -> u32 {
    // SAFETY: ECAM region is memory-mapped PCI config space.
    unsafe { fstart_mmio::read32(addr(bus, dev, func, reg) as *const u32) }
}

/// Write a 32-bit PCI config register.
#[inline]
pub fn write32(bus: u8, dev: u8, func: u8, reg: u16, val: u32) {
    unsafe { fstart_mmio::write32(addr(bus, dev, func, reg) as *mut u32, val) }
}

/// Read a 16-bit PCI config register.
#[inline]
pub fn read16(bus: u8, dev: u8, func: u8, reg: u16) -> u16 {
    unsafe { fstart_mmio::read16(addr(bus, dev, func, reg) as *const u16) }
}

/// Write a 16-bit PCI config register.
#[inline]
pub fn write16(bus: u8, dev: u8, func: u8, reg: u16, val: u16) {
    unsafe { fstart_mmio::write16(addr(bus, dev, func, reg) as *mut u16, val) }
}

/// Read an 8-bit PCI config register.
#[inline]
pub fn read8(bus: u8, dev: u8, func: u8, reg: u16) -> u8 {
    unsafe { fstart_mmio::read8(addr(bus, dev, func, reg) as *const u8) }
}

/// Write an 8-bit PCI config register.
#[inline]
pub fn write8(bus: u8, dev: u8, func: u8, reg: u16, val: u8) {
    unsafe { fstart_mmio::write8(addr(bus, dev, func, reg) as *mut u8, val) }
}

/// Read-modify-write: `reg = (reg & mask) | set`.
#[inline]
pub fn modify32(bus: u8, dev: u8, func: u8, reg: u16, mask: u32, set: u32) {
    let v = read32(bus, dev, func, reg);
    write32(bus, dev, func, reg, (v & mask) | set);
}

/// OR bits into a 32-bit register.
#[inline]
pub fn or32(bus: u8, dev: u8, func: u8, reg: u16, bits: u32) {
    modify32(bus, dev, func, reg, !0, bits);
}

/// AND bits out of an 8-bit register.
#[inline]
pub fn and8(bus: u8, dev: u8, func: u8, reg: u16, mask: u8) {
    let v = read8(bus, dev, func, reg);
    write8(bus, dev, func, reg, v & mask);
}

/// OR bits into an 8-bit register.
#[inline]
pub fn or8(bus: u8, dev: u8, func: u8, reg: u16, bits: u8) {
    let v = read8(bus, dev, func, reg);
    write8(bus, dev, func, reg, v | bits);
}

/// OR bits into a 16-bit register.
#[inline]
pub fn or16(bus: u8, dev: u8, func: u8, reg: u16, bits: u16) {
    let v = read16(bus, dev, func, reg);
    write16(bus, dev, func, reg, v | bits);
}

/// AND mask a 16-bit register.
#[inline]
pub fn and16(bus: u8, dev: u8, func: u8, reg: u16, mask: u16) {
    let v = read16(bus, dev, func, reg);
    write16(bus, dev, func, reg, v & mask);
}

/// AND mask a 32-bit register.
#[inline]
pub fn and32(bus: u8, dev: u8, func: u8, reg: u16, mask: u32) {
    let v = read32(bus, dev, func, reg);
    write32(bus, dev, func, reg, v & mask);
}

/// AND-then-OR an 8-bit register.
#[inline]
pub fn and8_or8(bus: u8, dev: u8, func: u8, reg: u16, mask: u8, bits: u8) {
    let v = read8(bus, dev, func, reg);
    write8(bus, dev, func, reg, (v & mask) | bits);
}
