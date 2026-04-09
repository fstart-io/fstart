//! Adapter layer between fstart drivers and CrabEFI platform traits.
//!
//! Bridges fstart's service traits (`Console`, `Timer`, `PciRootBus`) to the
//! trait objects that [`crabefi::PlatformConfig`] expects (`DebugOutput`,
//! `Timer`, `ResetHandler`).
//!
//! Architecture-specific adapters:
//! - **AArch64**: [`ArmGenericTimer`] (CNTPCT_EL0), [`PsciReset`] (HVC)
//! - **RISC-V 64**: [`RiscvSbiTimer`] (rdtime CSR), [`SbiReset`] (SBI SRST)
//!
//! The adapter types are safe wrappers — no `unsafe` at the call site.

#![no_std]

use core::fmt;

// Type aliases and re-exports for generated code convenience.
pub type MemoryRegion = crabefi::MemoryRegion;
pub type MemoryType = crabefi::MemoryType;
pub type PlatformConfig<'a> = crabefi::PlatformConfig<'a>;
pub type FramebufferConfig = crabefi::FramebufferConfig;

/// Re-export CrabEFI's [`BlockDevice`](crabefi::BlockDevice) trait for use
/// in generated code that casts platform block device adapters.
pub use crabefi::BlockDevice as CrabEfiBlockDevice;

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

// `crabefi::DebugOutput` has `core::fmt::Write` as a supertrait,
// so this impl is required — not optional.
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
// BlockDevice → CrabEFI BlockDevice adapter
// ---------------------------------------------------------------------------

/// Wraps an fstart [`BlockDevice`](fstart_services::BlockDevice) as a CrabEFI
/// [`BlockDevice`](crabefi::BlockDevice).
///
/// fstart's `BlockDevice` uses byte-offset addressing with `&self` (MMIO
/// interior mutability) and returns `Result<usize, ServiceError>`.
/// CrabEFI's `BlockDevice` uses LBA-based addressing with `&mut self` and
/// returns `Result<(), BlockError>`.
///
/// The adapter translates LBA → byte offset (`lba * block_size`) and
/// loops reads until the full request is satisfied, mapping errors to
/// [`crabefi::BlockError::DeviceError`].
pub struct BlockDeviceAdapter<'a> {
    inner: &'a dyn fstart_services::BlockDevice,
    name: &'a str,
}

impl<'a> BlockDeviceAdapter<'a> {
    /// Create a new adapter wrapping an fstart block device.
    ///
    /// `name` is displayed in CrabEFI's boot menu (e.g. `"SD/MMC"`).
    pub fn new(inner: &'a dyn fstart_services::BlockDevice, name: &'a str) -> Self {
        Self { inner, name }
    }
}

impl crabefi::BlockDevice for BlockDeviceAdapter<'_> {
    fn info(&self) -> crabefi::BlockDeviceInfo {
        let block_size = self.inner.block_size();
        let num_blocks = if block_size > 0 {
            self.inner.size() / block_size as u64
        } else {
            0
        };
        crabefi::BlockDeviceInfo {
            num_blocks,
            block_size,
            media_id: 0,
            removable: true,
            read_only: false,
        }
    }

    fn read_blocks(
        &mut self,
        lba: u64,
        count: u32,
        buffer: &mut [u8],
    ) -> Result<(), crabefi::BlockError> {
        self.validate_read(lba, count, buffer)?;
        if count == 0 {
            return Ok(());
        }

        let block_size = self.inner.block_size() as u64;
        let byte_offset = lba * block_size;
        let total = count as u64 * block_size;

        let mut done = 0u64;
        while done < total {
            let start = done as usize;
            let end = total as usize;
            match self.inner.read(byte_offset + done, &mut buffer[start..end]) {
                Ok(0) => return Err(crabefi::BlockError::DeviceError),
                Ok(n) => done += n as u64,
                Err(_) => return Err(crabefi::BlockError::DeviceError),
            }
        }
        Ok(())
    }

    fn name(&self) -> &str {
        self.name
    }
}

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

/// Read the RISC-V timer frequency from an FDT blob.
///
/// Searches the FDT structure block for the `/cpus` node's
/// `timebase-frequency` property and returns the u32 value.
///
/// Returns `10_000_000` (10 MHz, QEMU virt default) if the property is
/// not found or the FDT is malformed.
///
/// # Safety
///
/// `fdt_addr` must point to a valid FDT blob, or be 0.
pub unsafe fn fdt_read_timebase_frequency(fdt_addr: u64) -> u64 {
    const DEFAULT_FREQ: u64 = 10_000_000; // QEMU virt default

    if fdt_addr == 0 {
        return DEFAULT_FREQ;
    }

    let ptr = fdt_addr as *const u8;

    // FDT header: magic (offset 0), totalsize (4), off_dt_struct (8),
    // off_dt_strings (12), ...
    // SAFETY: caller guarantees valid FDT.
    let magic = unsafe { u32::from_be(core::ptr::read_unaligned(ptr as *const u32)) };
    if magic != 0xd00dfeed {
        return DEFAULT_FREQ;
    }

    let totalsize =
        unsafe { u32::from_be(core::ptr::read_unaligned(ptr.add(4) as *const u32)) } as usize;
    let off_struct =
        unsafe { u32::from_be(core::ptr::read_unaligned(ptr.add(8) as *const u32)) } as usize;
    let off_strings =
        unsafe { u32::from_be(core::ptr::read_unaligned(ptr.add(12) as *const u32)) } as usize;

    // Walk the structure block looking for "cpus" node's timebase-frequency.
    const FDT_BEGIN_NODE: u32 = 0x00000001;
    const FDT_END_NODE: u32 = 0x00000002;
    const FDT_PROP: u32 = 0x00000003;
    const FDT_NOP: u32 = 0x00000004;
    const FDT_END: u32 = 0x00000009;

    let struct_base = unsafe { ptr.add(off_struct) };
    let strings_base = unsafe { ptr.add(off_strings) };
    let struct_end = off_struct + (totalsize - off_struct);

    let mut offset = 0usize;
    let mut in_cpus = false;
    let mut depth: u32 = 0;
    let mut cpus_depth: u32 = 0;

    while offset + 4 <= struct_end {
        let token = unsafe {
            u32::from_be(core::ptr::read_unaligned(
                struct_base.add(offset) as *const u32
            ))
        };
        offset += 4;

        match token {
            FDT_BEGIN_NODE => {
                // Node name follows (null-terminated, 4-byte aligned).
                let name_start = offset;
                while offset < struct_end {
                    let b = unsafe { *struct_base.add(offset) };
                    if b == 0 {
                        break;
                    }
                    offset += 1;
                }
                let name_len = offset - name_start;
                offset += 1; // skip null terminator
                offset = (offset + 3) & !3; // align to 4 bytes

                // Check if this is the "cpus" node (depth 1).
                if depth == 0 && name_len >= 4 {
                    let n = unsafe { core::slice::from_raw_parts(struct_base.add(name_start), 4) };
                    if n == b"cpus" {
                        in_cpus = true;
                        cpus_depth = depth + 1;
                    }
                }

                depth += 1;
            }
            FDT_END_NODE => {
                if depth > 0 {
                    depth -= 1;
                }
                if in_cpus && depth < cpus_depth {
                    in_cpus = false;
                }
            }
            FDT_PROP => {
                if offset + 8 > struct_end {
                    break;
                }
                let val_len = unsafe {
                    u32::from_be(core::ptr::read_unaligned(
                        struct_base.add(offset) as *const u32
                    ))
                } as usize;
                let name_off = unsafe {
                    u32::from_be(core::ptr::read_unaligned(
                        struct_base.add(offset + 4) as *const u32
                    ))
                } as usize;
                offset += 8;

                // Check property name in strings block.
                if in_cpus && depth == cpus_depth {
                    let prop_name = unsafe { strings_base.add(name_off) };
                    let target = b"timebase-frequency\0";
                    let mut matches = true;
                    for (i, &expected) in target.iter().enumerate() {
                        let actual = unsafe { *prop_name.add(i) };
                        if actual != expected {
                            matches = false;
                            break;
                        }
                    }
                    if matches && val_len == 4 {
                        let freq = unsafe {
                            u32::from_be(core::ptr::read_unaligned(
                                struct_base.add(offset) as *const u32
                            ))
                        };
                        return freq as u64;
                    }
                }

                offset += val_len;
                offset = (offset + 3) & !3; // align to 4 bytes
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => break,
        }
    }

    DEFAULT_FREQ
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
// PSCI Reset Handler (AArch64)
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
                in("x0") _function_id as u64,
                options(noreturn)
            );
        }

        #[cfg(not(target_arch = "aarch64"))]
        loop {
            core::hint::spin_loop();
        }
    }
}

// ---------------------------------------------------------------------------
// RISC-V SBI Timer → CrabEFI Timer adapter
// ---------------------------------------------------------------------------

/// CrabEFI [`Timer`](crabefi::Timer) backed by the RISC-V `rdtime` CSR.
///
/// Reads the `time` CSR (a read-only shadow of `mtime` in S-mode) for
/// the tick count. The frequency is obtained from the FDT
/// `/cpus/timebase-frequency` property at construction time.
///
/// Works on any RISC-V platform with SBI timer support (QEMU virt,
/// real hardware under OpenSBI/RustSBI).
pub struct RiscvSbiTimer {
    freq: u64,
}

impl RiscvSbiTimer {
    /// Create a timer with the given frequency (in Hz).
    ///
    /// The caller obtains the frequency from the FDT's
    /// `/cpus/timebase-frequency` property. On QEMU virt this is
    /// 10 MHz (10_000_000).
    pub fn new(freq: u64) -> Self {
        Self { freq }
    }

    /// Create a timer by reading the frequency from an FDT blob.
    ///
    /// Parses the FDT at `fdt_addr` to find `/cpus/timebase-frequency`.
    /// Falls back to 10 MHz (QEMU virt default) if the property is not
    /// found.
    ///
    /// # Safety
    ///
    /// `fdt_addr` must point to a valid FDT blob, or be 0.
    pub unsafe fn from_fdt(fdt_addr: u64) -> Self {
        let freq = unsafe { fdt_read_timebase_frequency(fdt_addr) };
        Self { freq }
    }
}

impl crabefi::Timer for RiscvSbiTimer {
    fn current_ticks(&self) -> u64 {
        #[cfg(target_arch = "riscv64")]
        {
            let ticks: u64;
            // SAFETY: rdtime reads the time CSR, always available in S-mode.
            unsafe {
                core::arch::asm!(
                    "rdtime {}",
                    out(reg) ticks,
                    options(nomem, nostack, preserves_flags)
                );
            }
            ticks
        }
        #[cfg(not(target_arch = "riscv64"))]
        0
    }

    fn ticks_per_second(&self) -> u64 {
        self.freq
    }
}

// ---------------------------------------------------------------------------
// SBI Reset Handler (RISC-V)
// ---------------------------------------------------------------------------

/// CrabEFI [`ResetHandler`](crabefi::ResetHandler) using SBI SRST calls.
///
/// Uses the SBI System Reset Extension (SRST, EID 0x53525354) to request
/// shutdown or reboot from OpenSBI. Works on QEMU virt and any SBI-capable
/// RISC-V platform.
pub struct SbiReset;

impl crabefi::ResetHandler for SbiReset {
    fn reset(&self, reset_type: crabefi::ResetType) -> ! {
        // SBI SRST: EID=0x53525354, FID=0, a0=type, a1=reason
        // Types: 0=shutdown, 1=cold_reboot, 2=warm_reboot
        let srst_type: u64 = match reset_type {
            crabefi::ResetType::Cold => 1,     // SRST_COLD_REBOOT
            crabefi::ResetType::Warm => 2,     // SRST_WARM_REBOOT
            crabefi::ResetType::Shutdown => 0, // SRST_SHUTDOWN
            _ => 1,                            // default to cold reboot
        };

        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!(
                "ecall",
                in("a0") srst_type,
                in("a1") 0u64,        // reason: no reason
                in("a6") 0u64,        // FID: 0
                in("a7") 0x53525354u64, // EID: SRST
                options(noreturn)
            );
        }

        #[cfg(not(target_arch = "riscv64"))]
        loop {
            core::hint::spin_loop();
        }
    }
}
