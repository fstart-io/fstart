//! Adapter layer between fstart drivers and CrabEFI platform traits.
//!
//! Bridges fstart's service traits (`Console`, `Timer`, `PciRootBus`) to the
//! trait objects that [`crabefi::PlatformConfig`] expects (`DebugOutput`,
//! `Timer`, `ResetHandler`).
//!
//! The adapter types are safe wrappers — no `unsafe` at the call site.

#![no_std]

use core::fmt;

// Type aliases for generated code convenience.
pub type MemoryRegion = crabefi::MemoryRegion;
pub type MemoryType = crabefi::MemoryType;
pub type PlatformConfig<'a> = crabefi::PlatformConfig<'a>;
pub type RuntimeRegion = crabefi::RuntimeRegion;

/// Call `crabefi::init_platform()`. This is the entry point that never returns.
pub fn init_platform(config: crabefi::PlatformConfig) -> ! {
    crabefi::init_platform(config)
}

// ---------------------------------------------------------------------------
// Console → DebugOutput adapter
// ---------------------------------------------------------------------------

/// Wraps an fstart [`Console`](fstart_services::Console) as a CrabEFI
/// [`DebugOutput`](crabefi::DebugOutput).
///
/// fstart's `Console` uses `&self` (MMIO is inherently interior-mutable)
/// and returns `Result`. CrabEFI's `DebugOutput` uses `&mut self` and
/// ignores errors. The adapter bridges both differences.
pub struct ConsoleAdapter<'a, C: fstart_services::Console + ?Sized>(pub &'a C);

impl<C: fstart_services::Console + ?Sized> crabefi::DebugOutput for ConsoleAdapter<'_, C> {
    fn write_byte(&mut self, byte: u8) {
        let _ = self.0.write_byte(byte);
    }

    fn try_read_byte(&self) -> Option<u8> {
        self.0.read_byte().ok().flatten()
    }

    fn has_input(&self) -> bool {
        // fstart's Console trait has no `has_input()` method.
        // A future extension could add one; for now, return false.
        false
    }
}

impl<C: fstart_services::Console + ?Sized> fmt::Write for ConsoleAdapter<'_, C> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            let _ = self.0.write_byte(byte);
        }
        Ok(())
    }
}

// SAFETY: fstart's Console is Send + Sync (required by the trait bound).
// ConsoleAdapter holds an immutable reference to it, which is Send.
unsafe impl<C: fstart_services::Console + ?Sized> Send for ConsoleAdapter<'_, C> {}

// ---------------------------------------------------------------------------
// ARM Generic Timer → CrabEFI Timer adapter
// ---------------------------------------------------------------------------

/// CrabEFI [`Timer`](crabefi::Timer) backed by the ARM Generic Timer.
///
/// Reads `CNTPCT_EL0` for the tick count and `CNTFRQ_EL0` for the
/// frequency. Works on any AArch64 platform where the generic timer
/// is available (QEMU virt, SBSA, real hardware).
pub struct ArmGenericTimer {
    freq: u64,
}

impl ArmGenericTimer {
    /// Create a new timer by reading `CNTFRQ_EL0`.
    pub fn new() -> Self {
        let freq: u64;
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!(
                "mrs {}, CNTFRQ_EL0",
                out(reg) freq,
                options(nomem, nostack, preserves_flags)
            );
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            freq = 1_000_000; // fallback for non-aarch64 (compile-only)
        }
        Self { freq }
    }
}

impl crabefi::Timer for ArmGenericTimer {
    fn current_ticks(&self) -> u64 {
        #[cfg(target_arch = "aarch64")]
        {
            let ticks: u64;
            unsafe {
                core::arch::asm!(
                    "mrs {}, CNTPCT_EL0",
                    out(reg) ticks,
                    options(nomem, nostack, preserves_flags)
                );
            }
            ticks
        }
        #[cfg(not(target_arch = "aarch64"))]
        0
    }

    fn ticks_per_second(&self) -> u64 {
        self.freq
    }
}

// ---------------------------------------------------------------------------
// PSCI Reset Handler
// ---------------------------------------------------------------------------

/// CrabEFI [`ResetHandler`](crabefi::ResetHandler) using ARM PSCI calls.
///
/// Uses HVC #0 to call PSCI SYSTEM_RESET (warm/cold) or SYSTEM_OFF
/// (shutdown). Works on QEMU virt and any PSCI-capable platform.
pub struct PsciReset;

impl crabefi::ResetHandler for PsciReset {
    fn reset(&self, reset_type: crabefi::ResetType) -> ! {
        let function_id: u32 = match reset_type {
            crabefi::ResetType::Cold | crabefi::ResetType::Warm => 0x8400_0009, // SYSTEM_RESET
            crabefi::ResetType::Shutdown => 0x8400_0008,                        // SYSTEM_OFF
        };

        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!(
                "hvc #0",
                in("x0") function_id as u64,
                options(noreturn)
            );
        }

        #[cfg(not(target_arch = "aarch64"))]
        loop {
            core::hint::spin_loop();
        }
    }
}
