//! Local APIC (xAPIC mode) register interface.
//!
//! The Local APIC is the per-CPU interrupt controller on x86. This crate
//! provides an MMIO-based interface for xAPIC mode, covering the operations
//! needed by firmware:
//!
//! - LAPIC enable and virtual-wire setup
//! - APIC ID readback
//! - BSP detection
//! - IPI delivery (INIT, SIPI, SMI, self-IPI)
//!
//! The LAPIC sits at a well-known physical address (default `0xFEE0_0000`),
//! configurable via the `IA32_APIC_BASE` MSR.  All register access is
//! 32-bit aligned MMIO with volatile semantics.
//!
//! This crate is intentionally minimal — it exposes the LAPIC as a
//! thin register interface, not a full interrupt framework.  Higher-level
//! MP orchestration lives in `fstart-mp`.

#![no_std]

use core::ptr;
use core::sync::atomic::{fence, Ordering};

// ---------------------------------------------------------------------------
// MSR constants
// ---------------------------------------------------------------------------

/// IA32_APIC_BASE MSR index.
const IA32_APIC_BASE: u32 = 0x1B;

/// LAPIC is enabled (bit 11).
const APIC_BASE_ENABLE: u64 = 1 << 11;
/// This is the bootstrap processor (bit 8).
const APIC_BASE_BSP: u64 = 1 << 8;
/// Address mask for the LAPIC base (bits 12..35).
const APIC_BASE_ADDR_MASK: u64 = 0xFFFF_F000;

// ---------------------------------------------------------------------------
// Register offsets
// ---------------------------------------------------------------------------

/// LAPIC ID register (read-only).
const REG_ID: u32 = 0x020;
/// LAPIC Version register.
const REG_VERSION: u32 = 0x030;
/// Task Priority Register.
const REG_TPR: u32 = 0x080;
/// End-Of-Interrupt register (write-only).
const REG_EOI: u32 = 0x0B0;
/// Spurious Interrupt Vector Register.
const REG_SVR: u32 = 0x0F0;
/// Error Status Register.
const REG_ESR: u32 = 0x280;
/// Interrupt Command Register (low 32 bits).
const REG_ICR_LO: u32 = 0x300;
/// Interrupt Command Register (high 32 bits — destination field).
const REG_ICR_HI: u32 = 0x310;
/// LVT Local Interrupt 0 (LINT0).
const REG_LVT0: u32 = 0x350;
/// LVT Local Interrupt 1 (LINT1).
const REG_LVT1: u32 = 0x360;
/// LVT Error.
const REG_LVTERR: u32 = 0x370;
/// Timer Initial Count Register.
const REG_TMICT: u32 = 0x380;
/// Timer Current Count Register.
const REG_TMCCT: u32 = 0x390;
/// Timer Divide Configuration Register.
const REG_TDCR: u32 = 0x3E0;

// ---------------------------------------------------------------------------
// ICR flags
// ---------------------------------------------------------------------------

/// Destination shorthand: self only.
pub const DEST_SELF: u32 = 0x0004_0000;
/// Destination shorthand: all including self.
pub const DEST_ALL_INCL: u32 = 0x0008_0000;
/// Destination shorthand: all excluding self.
pub const DEST_ALL_EXCL: u32 = 0x000C_0000;
/// ICR delivery status: busy.
const ICR_BUSY: u32 = 0x0000_1000;
/// Assert level (for level-triggered IPIs).
pub const INT_ASSERT: u32 = 0x0000_4000;
/// Level-triggered (vs edge-triggered).
pub const INT_LEVEL: u32 = 0x0000_8000;

// ---------------------------------------------------------------------------
// Delivery mode (message type) — ICR bits [10:8]
// ---------------------------------------------------------------------------

/// Fixed delivery.
pub const MT_FIXED: u32 = 0x000;
/// SMI delivery.
pub const MT_SMI: u32 = 0x200;
/// NMI delivery.
pub const MT_NMI: u32 = 0x400;
/// INIT delivery.
pub const MT_INIT: u32 = 0x500;
/// Startup IPI (SIPI) delivery.
pub const MT_STARTUP: u32 = 0x600;
/// ExtINT delivery.
pub const MT_EXTINT: u32 = 0x700;

// ---------------------------------------------------------------------------
// LVT flags
// ---------------------------------------------------------------------------

/// LVT entry: masked (bit 16).
const LVT_MASKED: u32 = 1 << 16;
/// LVT delivery mode mask (bits [10:8]).
const LVT_DM_MASK: u32 = 7 << 8;
/// LVT delivery mode: NMI.
const LVT_DM_NMI: u32 = 4 << 8;
/// LVT delivery mode: ExtINT.
const LVT_DM_EXTINT: u32 = 7 << 8;

// ---------------------------------------------------------------------------
// SVR flags
// ---------------------------------------------------------------------------

/// Software enable bit in the Spurious Interrupt Vector Register.
const SVR_ENABLE: u32 = 0x100;

/// Default LAPIC MMIO base address.
pub const DEFAULT_BASE: usize = 0xFEE0_0000;

// ---------------------------------------------------------------------------
// MSR helpers (inline asm)
// ---------------------------------------------------------------------------

/// Read a 64-bit Model-Specific Register.
///
/// # Safety
///
/// Caller must ensure `msr` is a valid MSR index for this CPU.
#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: caller guarantees valid MSR index.
    core::arch::asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") lo,
        out("edx") hi,
        options(nomem, nostack),
    );
    ((hi as u64) << 32) | (lo as u64)
}

/// Write a 64-bit Model-Specific Register.
///
/// # Safety
///
/// Caller must ensure `msr` is a valid MSR index and `val` is a
/// legal value for that MSR.
#[inline]
unsafe fn wrmsr(msr: u32, val: u64) {
    let lo = val as u32;
    let hi = (val >> 32) as u32;
    // SAFETY: caller guarantees valid MSR index and value.
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") lo,
        in("edx") hi,
        options(nomem, nostack),
    );
}

// ---------------------------------------------------------------------------
// Lapic struct
// ---------------------------------------------------------------------------

/// Local APIC register interface (xAPIC mode).
///
/// Constructed from the `IA32_APIC_BASE` MSR or a known base address.
/// All register access is 32-bit aligned MMIO via volatile reads/writes.
///
/// # Example
///
/// ```ignore
/// let lapic = Lapic::from_msr();
/// lapic.enable();
/// lapic.setup_virtual_wire(Lapic::is_bsp());
/// ```
pub struct Lapic {
    base: usize,
}

// SAFETY: LAPIC registers are per-CPU (each CPU has its own LAPIC at
// the same physical address).  Firmware is single-threaded per core
// during init — no concurrent access to the same LAPIC instance.
unsafe impl Send for Lapic {}
unsafe impl Sync for Lapic {}

impl Lapic {
    // ---- Construction ----

    /// Read the LAPIC base from `IA32_APIC_BASE` and construct.
    pub fn from_msr() -> Self {
        // SAFETY: IA32_APIC_BASE (0x1B) is architecturally defined on
        // all x86 CPUs with a local APIC.
        let msr = unsafe { rdmsr(IA32_APIC_BASE) };
        Self {
            base: (msr & APIC_BASE_ADDR_MASK) as usize,
        }
    }

    /// Construct from a known MMIO base address.
    pub const fn at(base: usize) -> Self {
        Self { base }
    }

    // ---- Register access ----

    /// Read a 32-bit LAPIC register at `offset`.
    #[inline]
    fn read(&self, offset: u32) -> u32 {
        let addr = (self.base + offset as usize) as *const u32;
        // SAFETY: LAPIC registers are 32-bit aligned MMIO at known offsets.
        unsafe { ptr::read_volatile(addr) }
    }

    /// Write a 32-bit LAPIC register at `offset`.
    #[inline]
    fn write(&self, offset: u32, val: u32) {
        let addr = (self.base + offset as usize) as *mut u32;
        // SAFETY: LAPIC registers are 32-bit aligned MMIO at known offsets.
        unsafe { ptr::write_volatile(addr, val) }
    }

    /// Read-modify-write: `reg = (reg & !clear) | set`.
    #[inline]
    fn update(&self, offset: u32, clear: u32, set: u32) {
        let val = self.read(offset);
        self.write(offset, (val & !clear) | set);
    }

    // ---- Queries ----

    /// Read this CPU's APIC ID (bits [31:24] of the ID register).
    pub fn id(&self) -> u32 {
        self.read(REG_ID) >> 24
    }

    /// Read the LAPIC version register.
    pub fn version(&self) -> u32 {
        self.read(REG_VERSION)
    }

    /// Return the MMIO base address.
    pub fn base(&self) -> usize {
        self.base
    }

    /// Check if the current CPU is the bootstrap processor.
    ///
    /// Reads `IA32_APIC_BASE` directly — can be called before
    /// constructing a `Lapic` instance.
    pub fn is_bsp() -> bool {
        // SAFETY: IA32_APIC_BASE is architecturally defined.
        let msr = unsafe { rdmsr(IA32_APIC_BASE) };
        (msr & APIC_BASE_BSP) != 0
    }

    // ---- Enable / setup ----

    /// Enable the LAPIC (set MSR enable bit + SVR software enable).
    ///
    /// After this call the LAPIC accepts interrupts.  Call
    /// [`setup_virtual_wire`](Self::setup_virtual_wire) to configure
    /// the LVT entries for firmware use.
    pub fn enable(&self) {
        // Set the enable bit in IA32_APIC_BASE if not already set.
        // SAFETY: IA32_APIC_BASE is architecturally defined; setting
        // the enable bit is the documented way to activate the LAPIC.
        unsafe {
            let msr = rdmsr(IA32_APIC_BASE);
            if (msr & APIC_BASE_ENABLE) == 0 {
                wrmsr(IA32_APIC_BASE, msr | APIC_BASE_ENABLE);
            }
        }
        // Set SVR: software enable + spurious vector 0x0F.
        self.update(REG_SVR, 0xFF | SVR_ENABLE, SVR_ENABLE | 0x0F);
    }

    /// Configure the LAPIC for virtual-wire mode.
    ///
    /// Sets up:
    /// - Task priority to 0 (accept all)
    /// - Spurious vector to 0x0F with software enable
    /// - LINT0: ExtINT (BSP) or masked ExtINT (AP)
    /// - LINT1: NMI
    ///
    /// This matches coreboot's `setup_lapic_interrupts()`.
    pub fn setup_virtual_wire(&self, is_bsp: bool) {
        // Accept all priorities.
        self.write(REG_TPR, 0);

        // SVR: enable + vector 0x0F.
        self.update(REG_SVR, 0xFF | SVR_ENABLE, SVR_ENABLE | 0x0F);

        // LINT0: ExtINT delivery; masked on APs.
        let lvt0_mask = LVT_MASKED | INT_LEVEL | LVT_DM_MASK;
        if is_bsp {
            self.update(REG_LVT0, lvt0_mask, LVT_DM_EXTINT);
        } else {
            self.update(REG_LVT0, lvt0_mask, LVT_MASKED | LVT_DM_EXTINT);
        }

        // LINT1: NMI delivery.
        self.update(REG_LVT1, lvt0_mask, LVT_DM_NMI);
    }

    // ---- EOI ----

    /// Signal end-of-interrupt.
    pub fn eoi(&self) {
        self.write(REG_EOI, 0);
    }

    // ---- IPI delivery ----

    /// Check if the LAPIC is busy delivering an IPI.
    #[inline]
    pub fn busy(&self) -> bool {
        (self.read(REG_ICR_LO) & ICR_BUSY) != 0
    }

    /// Spin-wait until the LAPIC is ready to accept an IPI.
    pub fn wait_ready(&self) {
        while self.busy() {
            core::hint::spin_loop();
        }
    }

    /// Send an INIT IPI to all processors except self.
    pub fn send_init_all_but_self(&self) {
        self.wait_ready();
        self.write(REG_ICR_LO, DEST_ALL_EXCL | INT_ASSERT | MT_INIT);
    }

    /// Send a Startup IPI (SIPI) to all processors except self.
    ///
    /// `vector_page` is the 4K-aligned physical page number where the
    /// SIPI trampoline has been placed (e.g., `0x01` for address `0x1000`).
    pub fn send_sipi_all_but_self(&self, vector_page: u8) {
        self.wait_ready();
        self.write(
            REG_ICR_LO,
            DEST_ALL_EXCL | INT_ASSERT | MT_STARTUP | (vector_page as u32),
        );
    }

    /// Send an IPI to self with the given flags.
    ///
    /// Used for SMM relocation (`MT_SMI`) or self-NMI.
    pub fn send_ipi_self(&self, flags: u32) {
        self.wait_ready();
        self.write(REG_ICR_LO, DEST_SELF | flags);
    }

    // ---- Timer (for future use) ----

    /// Set the LAPIC timer initial count.
    pub fn set_timer_count(&self, count: u32) {
        self.write(REG_TMICT, count);
    }

    /// Read the LAPIC timer current count.
    pub fn timer_count(&self) -> u32 {
        self.read(REG_TMCCT)
    }

    /// Set the timer divide configuration.
    pub fn set_timer_divide(&self, div: u32) {
        self.write(REG_TDCR, div);
    }
}
