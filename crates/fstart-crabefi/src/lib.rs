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
pub type FramebufferConfig = crabefi::FramebufferConfig;

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
// EFI memory map construction
// ---------------------------------------------------------------------------

/// Read the FDT total size from a raw pointer to an FDT blob.
///
/// Reads the `totalsize` field (big-endian `u32` at offset 4) from the
/// FDT header and rounds up to the next 4 KiB page boundary.
///
/// Returns 0 if `fdt_addr` is null (no FDT).
///
/// # Safety
///
/// `fdt_addr` must point to a valid FDT blob with at least 8 readable
/// bytes, or be null.
pub unsafe fn fdt_page_aligned_size(fdt_addr: u64) -> u64 {
    if fdt_addr == 0 {
        return 0;
    }
    let ptr = fdt_addr as *const u8;
    // SAFETY: caller guarantees valid FDT at this address.
    let total = unsafe { u32::from_be(core::ptr::read_unaligned(ptr.add(4) as *const u32)) } as u64;
    (total + 0xFFF) & !0xFFF // page-align up
}

/// Build the EFI memory map with firmware regions carved out of RAM.
///
/// Takes static entries (ROM, Reserved from board config), the RAM
/// region, firmware data/stack locations, and an optional FDT
/// reservation. Splits the RAM region into:
///
/// ```text
/// [FDT reserved] [free RAM] [BSS/data reserved] [free RAM] [stack reserved]
/// ```
///
/// - ROM is `RuntimeServicesCode` (kernel maps it after ExitBootServices
///   for runtime service calls).
/// - BSS/data/heap is `RuntimeServicesData` (contains CrabEFI's statics,
///   heap backing store, RUNTIME_SERVICES table).
/// - Stack is `RuntimeServicesData` (contains FirmwareState on the stack
///   since `init_platform()` is `-> !`).
/// - FDT (if present) is `Reserved` (GRUB/kernel reads it as a
///   configuration table).
///
/// Returns the number of entries written to `buf`.
///
/// # Panics
///
/// Panics if `buf` is too small to hold all entries (12 should suffice).
#[allow(clippy::too_many_arguments)]
pub fn build_efi_memory_map(
    static_entries: &[MemoryRegion],
    ram_base: u64,
    ram_size: u64,
    fw_data_addr: u64,
    fw_bss_reserve: u64,
    fw_stack_size: u64,
    fdt_reservation: Option<(u64, u64)>,
    buf: &mut [MemoryRegion],
) -> usize {
    let mut idx = 0;

    // 1. Copy static entries (ROM, Reserved from board config).
    for entry in static_entries {
        buf[idx] = *entry;
        idx += 1;
    }

    let ram_end = ram_base + ram_size;
    let fw_bss_end = fw_data_addr + fw_bss_reserve;
    let fw_stack_bottom = ram_end - fw_stack_size;

    // 2. RAM below firmware BSS, with optional FDT carved out.
    if fw_data_addr > ram_base {
        match fdt_reservation {
            Some((fdt_addr, fdt_size)) if fdt_size > 0 => {
                // FDT region: Reserved so allocator won't hand it out.
                buf[idx] = MemoryRegion {
                    base: fdt_addr,
                    size: fdt_size,
                    region_type: MemoryType::Reserved,
                };
                idx += 1;

                // Free RAM between FDT end and firmware BSS start.
                let post_fdt = fdt_addr + fdt_size;
                if fw_data_addr > post_fdt {
                    buf[idx] = MemoryRegion {
                        base: post_fdt,
                        size: fw_data_addr - post_fdt,
                        region_type: MemoryType::Ram,
                    };
                    idx += 1;
                }
            }
            _ => {
                // No FDT reservation -- entire pre-BSS RAM is free.
                buf[idx] = MemoryRegion {
                    base: ram_base,
                    size: fw_data_addr - ram_base,
                    region_type: MemoryType::Ram,
                };
                idx += 1;
            }
        }
    }

    // 3. Firmware BSS/data/heap -- RuntimeServicesData.
    buf[idx] = MemoryRegion {
        base: fw_data_addr,
        size: fw_bss_reserve,
        region_type: MemoryType::RuntimeServicesData,
    };
    idx += 1;

    // 4. Free RAM between BSS end and stack bottom.
    if fw_stack_bottom > fw_bss_end {
        buf[idx] = MemoryRegion {
            base: fw_bss_end,
            size: fw_stack_bottom - fw_bss_end,
            region_type: MemoryType::Ram,
        };
        idx += 1;
    }

    // 5. Firmware stack -- RuntimeServicesData (top of RAM, grows down).
    buf[idx] = MemoryRegion {
        base: fw_stack_bottom,
        size: fw_stack_size,
        region_type: MemoryType::RuntimeServicesData,
    };
    idx += 1;

    idx
}

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

impl Default for ArmGenericTimer {
    fn default() -> Self {
        Self::new()
    }
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
        let _function_id: u32 = match reset_type {
            crabefi::ResetType::Cold | crabefi::ResetType::Warm => 0x8400_0009, // SYSTEM_RESET
            crabefi::ResetType::Shutdown => 0x8400_0008,                        // SYSTEM_OFF
            // ResetType is #[non_exhaustive]; default unknown variants to cold reset.
            _ => 0x8400_0009,
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
