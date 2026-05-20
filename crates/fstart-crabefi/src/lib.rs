//! Adapter layer between fstart drivers and CrabEFI platform traits.
//!
//! Bridges fstart's service traits (`Console`, `Timer`, `PciRootBus`) to the
//! trait objects that [`crabefi::PlatformConfig`] expects (`DebugOutput`,
//! `Timer`, `ResetHandler`).
//!
//! The adapter types are safe wrappers — no `unsafe` at the call site.

#![no_std]

#[cfg(test)]
extern crate std;

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
            if byte == b'\n' {
                let _ = self.0.write_byte(b'\r');
            }
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
/// Returns 10 MHz if the property is not found or the FDT is malformed.
///
/// # Safety
///
/// `fdt_addr` must point to a valid FDT blob, or be 0.
pub unsafe fn fdt_read_timebase_frequency(fdt_addr: u64) -> u64 {
    const DEFAULT_FREQ: u64 = 10_000_000;
    if fdt_addr == 0 {
        return DEFAULT_FREQ;
    }

    let ptr = fdt_addr as *const u8;
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
    if off_struct >= totalsize || off_strings >= totalsize {
        return DEFAULT_FREQ;
    }

    const FDT_BEGIN_NODE: u32 = 0x00000001;
    const FDT_END_NODE: u32 = 0x00000002;
    const FDT_PROP: u32 = 0x00000003;
    const FDT_NOP: u32 = 0x00000004;
    const FDT_END: u32 = 0x00000009;

    let struct_base = unsafe { ptr.add(off_struct) };
    let strings_base = unsafe { ptr.add(off_strings) };
    let struct_len = totalsize - off_struct;
    let strings_len = totalsize - off_strings;
    let mut offset = 0usize;
    let mut in_cpus = false;
    let mut depth = 0u32;
    let mut cpus_depth = 0u32;

    while offset + 4 <= struct_len {
        let token = unsafe {
            u32::from_be(core::ptr::read_unaligned(
                struct_base.add(offset) as *const u32
            ))
        };
        offset += 4;
        match token {
            FDT_BEGIN_NODE => {
                let name_start = offset;
                while offset < struct_len && unsafe { *struct_base.add(offset) } != 0 {
                    offset += 1;
                }
                if offset >= struct_len {
                    return DEFAULT_FREQ;
                }
                let name_len = offset - name_start;
                offset = (offset + 1 + 3) & !3;
                if depth == 1 && name_len == 4 {
                    let name =
                        unsafe { core::slice::from_raw_parts(struct_base.add(name_start), 4) };
                    if name == b"cpus" {
                        in_cpus = true;
                        cpus_depth = depth + 1;
                    }
                }
                depth += 1;
            }
            FDT_END_NODE => {
                if in_cpus && depth == cpus_depth {
                    in_cpus = false;
                }
                depth = depth.saturating_sub(1);
            }
            FDT_PROP => {
                if offset + 8 > struct_len {
                    return DEFAULT_FREQ;
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
                if offset + val_len > struct_len || name_off >= strings_len {
                    return DEFAULT_FREQ;
                }
                if in_cpus && depth == cpus_depth {
                    let target = b"timebase-frequency\0";
                    if name_off + target.len() <= strings_len {
                        let prop = unsafe {
                            core::slice::from_raw_parts(strings_base.add(name_off), target.len())
                        };
                        if prop == target && val_len == 4 {
                            let freq = unsafe {
                                u32::from_be(core::ptr::read_unaligned(
                                    struct_base.add(offset) as *const u32
                                ))
                            };
                            return freq as u64;
                        }
                    }
                }
                offset = (offset + val_len + 3) & !3;
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => return DEFAULT_FREQ,
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
    build_efi_memory_map_with_framebuffer(
        static_entries,
        ram_base,
        ram_size,
        fw_data_addr,
        fw_bss_reserve,
        fw_stack_size,
        fdt_reservation,
        None,
        buf,
    )
}

/// Build the EFI memory map and additionally reserve a GOP framebuffer.
///
/// The framebuffer is marked `Reserved` so UEFI/OS boot services consumers do
/// not allocate over scanout memory before the graphics handoff is consumed.
#[allow(clippy::too_many_arguments)]
pub fn build_efi_memory_map_with_framebuffer(
    static_entries: &[MemoryRegion],
    ram_base: u64,
    ram_size: u64,
    fw_data_addr: u64,
    fw_bss_reserve: u64,
    fw_stack_size: u64,
    fdt_reservation: Option<(u64, u64)>,
    framebuffer_reservation: Option<(u64, u64)>,
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

    // 2. RAM below firmware BSS, with optional FDT/framebuffer carved out.
    idx = append_ram_with_reservations(
        ram_base,
        fw_data_addr,
        fdt_reservation,
        framebuffer_reservation,
        buf,
        idx,
    );

    // 3. Firmware BSS/data/heap -- RuntimeServicesData.
    //    CrabEFI's EFI system table, runtime services, and ACPI pointers
    //    live in fstart's BSS/stack and must survive ExitBootServices.
    //    The caller must 2 MiB-align data_addr and stack regions to avoid
    //    NX page-table conflicts (STRICT_KERNEL_RWX marks whole 2 MiB
    //    pages containing RuntimeServicesData as NX).
    buf[idx] = MemoryRegion {
        base: fw_data_addr,
        size: fw_bss_reserve,
        region_type: MemoryType::RuntimeServicesData,
    };
    idx += 1;

    // 4. Free RAM between BSS end and stack bottom, with all reservations
    // carved out. Boards commonly place DTBs and framebuffers high in DRAM.
    idx = append_ram_with_reservations(
        fw_bss_end,
        fw_stack_bottom,
        fdt_reservation,
        framebuffer_reservation,
        buf,
        idx,
    );

    // 5. Firmware stack -- RuntimeServicesData.
    buf[idx] = MemoryRegion {
        base: fw_stack_bottom,
        size: fw_stack_size,
        region_type: MemoryType::RuntimeServicesData,
    };
    idx += 1;

    idx
}

fn append_ram_with_reservations(
    start: u64,
    end: u64,
    first: Option<(u64, u64)>,
    second: Option<(u64, u64)>,
    buf: &mut [MemoryRegion],
    mut idx: usize,
) -> usize {
    if start >= end {
        return idx;
    }

    let mut reservations = [(0u64, 0u64); 2];
    let mut count = 0usize;
    for (base, size) in [first, second].into_iter().flatten() {
        if size == 0 {
            continue;
        }
        let res_start = base.max(start);
        let res_end = base.saturating_add(size).min(end);
        if res_start < res_end {
            reservations[count] = (res_start, res_end);
            count += 1;
        }
    }

    if count == 2 && reservations[1].0 < reservations[0].0 {
        reservations.swap(0, 1);
    }

    let mut cursor = start;
    let mut i = 0usize;
    while i < count {
        let (res_start, mut res_end) = reservations[i];
        i += 1;
        if res_end <= cursor {
            continue;
        }

        while i < count && reservations[i].0 <= res_end {
            res_end = res_end.max(reservations[i].1);
            i += 1;
        }

        if cursor < res_start {
            buf[idx] = MemoryRegion {
                base: cursor,
                size: res_start - cursor,
                region_type: MemoryType::Ram,
            };
            idx += 1;
        }

        let reserved_start = cursor.max(res_start);
        if reserved_start < res_end {
            buf[idx] = MemoryRegion {
                base: reserved_start,
                size: res_end - reserved_start,
                region_type: MemoryType::Reserved,
            };
            idx += 1;
            cursor = res_end;
        }
    }

    if cursor < end {
        buf[idx] = MemoryRegion {
            base: cursor,
            size: end - cursor,
            region_type: MemoryType::Ram,
        };
        idx += 1;
    }

    idx
}

// Re-export types for codegen convenience.
pub use crabefi::RuntimeRegion;
pub use fstart_services::memory_detect::E820Entry;

/// Compute the runtime memory region from linker-provided symbols.
///
/// Splits fstart's memory into code (RuntimeServicesCode) and data
/// (RuntimeServicesData) so the OS kernel can mark them with the
/// correct page protections after ExitBootServices.
///
/// Uses `_text_start`, `_text_end`, and `_writable_end` linker symbols.
/// All boundaries are page-aligned (4 KiB). `_writable_end` is emitted after
/// the linker-allocated firmware stack, so the complete stage-owned writable
/// footprint remains reserved while CrabEFI is running.
#[cfg(target_arch = "x86_64")]
pub fn compute_runtime_region() -> RuntimeRegion {
    extern "C" {
        static _text_start: u8;
        static _text_end: u8;
        static _writable_end: u8;
    }
    const PAGE: u64 = 0x1000;
    // SAFETY: these are linker-defined symbols — their addresses (not
    // values) delimit the stage's text and writable image regions.
    let code_base = unsafe { &_text_start as *const u8 as u64 } & !(PAGE - 1);
    let code_end = (unsafe { &_text_end as *const u8 as u64 } + PAGE - 1) & !(PAGE - 1);
    let data_base = code_end;
    let data_end = (unsafe { &_writable_end as *const u8 as u64 } + PAGE - 1) & !(PAGE - 1);
    RuntimeRegion {
        code_base,
        code_size: code_end - code_base,
        data_base,
        data_size: data_end - data_base,
    }
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
        #[cfg(target_arch = "aarch64")]
        {
            let function_id: u32 = match reset_type {
                crabefi::ResetType::Cold | crabefi::ResetType::Warm => 0x8400_0009, // SYSTEM_RESET
                crabefi::ResetType::Shutdown => 0x8400_0008,                        // SYSTEM_OFF
                // ResetType is #[non_exhaustive]; default unknown variants to cold reset.
                _ => 0x8400_0009,
            };
            unsafe {
                core::arch::asm!(
                    "hvc #0",
                    in("x0") function_id as u64,
                    options(noreturn)
                );
            }
        }

        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = reset_type;
            loop {
                core::hint::spin_loop();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RISC-V SBI Timer / Reset adapters
// ---------------------------------------------------------------------------

/// CrabEFI [`Timer`](crabefi::Timer) backed by the RISC-V `rdtime` CSR.
pub struct RiscvSbiTimer {
    freq: u64,
}

impl RiscvSbiTimer {
    /// Create a timer with the given frequency in Hz.
    pub fn new(freq: u64) -> Self {
        Self { freq }
    }

    /// Create a timer by reading `/cpus/timebase-frequency` from an FDT.
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
        unsafe {
            let value: u64;
            core::arch::asm!("rdtime {}", out(reg) value, options(nomem, nostack, preserves_flags));
            value
        }

        #[cfg(not(target_arch = "riscv64"))]
        {
            0
        }
    }

    fn ticks_per_second(&self) -> u64 {
        self.freq
    }
}

/// CrabEFI [`ResetHandler`](crabefi::ResetHandler) using SBI SRST calls.
pub struct SbiReset;

impl crabefi::ResetHandler for SbiReset {
    fn reset(&self, reset_type: crabefi::ResetType) -> ! {
        #[cfg(target_arch = "riscv64")]
        {
            let srst_type: u64 = match reset_type {
                crabefi::ResetType::Cold => 1,
                crabefi::ResetType::Warm => 2,
                crabefi::ResetType::Shutdown => 0,
                _ => 1,
            };
            unsafe {
                core::arch::asm!(
                    "ecall",
                    in("a0") srst_type,
                    in("a1") 0u64,
                    in("a6") 0u64,
                    in("a7") 0x53525354u64,
                    options(noreturn)
                );
            }
        }

        #[cfg(not(target_arch = "riscv64"))]
        {
            let _ = reset_type;
            loop {
                core::hint::spin_loop();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// x86_64 TSC Timer → CrabEFI Timer adapter
// ---------------------------------------------------------------------------

/// CrabEFI [`Timer`](crabefi::Timer) backed by the x86 TSC.
///
/// Calibrates the TSC frequency using the 8254 PIT (Programmable Interval
/// Timer) channel 2. Works on QEMU and all modern x86 hardware.
///
/// The PIT runs at a fixed 1.193182 MHz. We program channel 2 for a
/// known interval, measure the TSC delta, and compute tsc_freq.
#[cfg(target_arch = "x86_64")]
pub struct TscTimer {
    tsc_freq: u64,
}

#[cfg(target_arch = "x86_64")]
impl Default for TscTimer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_arch = "x86_64")]
impl TscTimer {
    /// PIT oscillator frequency: 1.193182 MHz.
    const PIT_FREQ: u64 = 1_193_182;

    /// PIT command register.
    const PIT_CMD: u16 = 0x43;

    /// Create a new timer by calibrating TSC against the PIT.
    pub fn new() -> Self {
        let tsc_freq = Self::calibrate_tsc();
        Self { tsc_freq }
    }

    /// Read the Time Stamp Counter.
    #[inline]
    fn rdtsc() -> u64 {
        let lo: u32;
        let hi: u32;
        unsafe {
            core::arch::asm!(
                "rdtsc",
                out("eax") lo,
                out("edx") hi,
                options(nomem, nostack, preserves_flags)
            );
        }
        ((hi as u64) << 32) | (lo as u64)
    }

    /// Calibrate TSC using PIT channel 0 count-down.
    ///
    /// Programs PIT channel 0 with a known count, reads the counter
    /// back via latch commands to measure elapsed PIT ticks, and
    /// computes the TSC frequency from the ratio.
    ///
    /// This approach avoids the speaker gate (port 0x61 bit 5) which
    /// is unreliable on QEMU Q35 in KVM mode.
    fn calibrate_tsc() -> u64 {
        unsafe {
            // Program channel 0: mode 2 (rate generator), binary,
            // lobyte/hibyte access. Mode 2 counts down repeatedly.
            fstart_pio::outb(Self::PIT_CMD, 0x34); // ch0, lobyte/hibyte, mode 2, binary

            // Load a large count — 0xFFFF gives ~54.9 ms period.
            fstart_pio::outb(0x40, 0xFF); // ch0 low byte
            fstart_pio::outb(0x40, 0xFF); // ch0 high byte

            // Small delay for count to load
            for _ in 0..100 {
                core::hint::spin_loop();
            }

            // Latch channel 0 and read starting count
            fstart_pio::outb(Self::PIT_CMD, 0x00); // latch ch0
            let lo = fstart_pio::inb(0x40) as u16;
            let hi = fstart_pio::inb(0x40) as u16;
            let count_start = (hi << 8) | lo;

            let tsc_start = Self::rdtsc();

            // Busy-wait for ~25000 PIT ticks (~20.9 ms)
            let target_pit_ticks: u16 = 25000;
            loop {
                fstart_pio::outb(Self::PIT_CMD, 0x00); // latch ch0
                let lo = fstart_pio::inb(0x40) as u16;
                let hi = fstart_pio::inb(0x40) as u16;
                let count_now = (hi << 8) | lo;

                // Mode 2 counts down from loaded value. Elapsed =
                // start - now (wraps handled by u16 subtraction).
                let elapsed = count_start.wrapping_sub(count_now);
                if elapsed >= target_pit_ticks {
                    break;
                }
                core::hint::spin_loop();
            }

            let tsc_end = Self::rdtsc();

            // Latch final count for precise measurement
            fstart_pio::outb(Self::PIT_CMD, 0x00);
            let lo = fstart_pio::inb(0x40) as u16;
            let hi = fstart_pio::inb(0x40) as u16;
            let count_end = (hi << 8) | lo;

            let pit_elapsed = count_start.wrapping_sub(count_end) as u64;
            let tsc_delta = tsc_end - tsc_start;

            if pit_elapsed == 0 {
                // Fallback: assume 1 GHz TSC
                return 1_000_000_000;
            }

            // freq = tsc_delta / (pit_elapsed / PIT_FREQ)
            //      = tsc_delta * PIT_FREQ / pit_elapsed
            tsc_delta * Self::PIT_FREQ / pit_elapsed
        }
    }
}

#[cfg(target_arch = "x86_64")]
impl crabefi::Timer for TscTimer {
    fn current_ticks(&self) -> u64 {
        Self::rdtsc()
    }

    fn ticks_per_second(&self) -> u64 {
        self.tsc_freq
    }
}

// ---------------------------------------------------------------------------
// x86 Keyboard Controller Reset → CrabEFI ResetHandler
// ---------------------------------------------------------------------------

/// CrabEFI [`ResetHandler`](crabefi::ResetHandler) using the x86 keyboard
/// controller reset (port 0x64) with triple-fault fallback.
#[cfg(target_arch = "x86_64")]
pub struct X86Reset;

#[cfg(target_arch = "x86_64")]
impl crabefi::ResetHandler for X86Reset {
    fn reset(&self, reset_type: crabefi::ResetType) -> ! {
        match reset_type {
            crabefi::ResetType::Shutdown => {
                // ACPI S5 (soft-off) via QEMU's ACPI PM1a control register.
                // QEMU Q35: PM1a_CNT at I/O port 0x0404.
                unsafe {
                    fstart_pio::outw(0x0404, 0x2000); // SLP_EN (bit 13); QEMU _S5 defines SLP_TYP=0
                }
            }
            _ => {
                // Keyboard controller reset: pulse CPU reset line
                unsafe {
                    // Wait for input buffer empty
                    for _ in 0..10000 {
                        if fstart_pio::inb(0x64) & 0x02 == 0 {
                            break;
                        }
                        core::hint::spin_loop();
                    }
                    fstart_pio::outb(0x64, 0xFE); // reset command
                }
            }
        }

        // Fallback: triple fault
        unsafe {
            // Load null IDT and trigger INT3
            core::arch::asm!(
                "lidt [{null_idt}]",
                "int3",
                null_idt = in(reg) &[0u16; 5] as *const _,
                options(noreturn),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// x86 RDRAND → CrabEFI Rng adapter
// ---------------------------------------------------------------------------

/// CrabEFI [`Rng`](crabefi::Rng) backed by the x86 RDRAND instruction.
///
/// Falls back to a simple LFSR if RDRAND is not available (very old CPUs).
#[cfg(target_arch = "x86_64")]
pub struct X86Rng {
    has_rdrand: bool,
}

#[cfg(target_arch = "x86_64")]
impl Default for X86Rng {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_arch = "x86_64")]
impl X86Rng {
    /// Check CPUID for RDRAND support (ECX bit 30 of leaf 1).
    pub fn new() -> Self {
        let (_, _, ecx, _) = fstart_arch_x86::cpuid(1);
        Self {
            has_rdrand: ecx & (1 << 30) != 0,
        }
    }

    /// Read a 64-bit random value via RDRAND with retry.
    fn rdrand64() -> Option<u64> {
        let mut val: u64;
        let mut ok: u8;
        for _ in 0..10 {
            unsafe {
                core::arch::asm!(
                    "rdrand {val}",
                    "setc {ok}",
                    val = out(reg) val,
                    ok = out(reg_byte) ok,
                    options(nomem, nostack),
                );
            }
            if ok != 0 && val != !0u64 {
                return Some(val);
            }
        }
        None
    }
}

#[cfg(target_arch = "x86_64")]
impl crabefi::Rng for X86Rng {
    fn get_random(&self, buffer: &mut [u8]) -> Result<(), crabefi::RngError> {
        if !self.has_rdrand {
            return Err(crabefi::RngError::Unsupported);
        }
        let mut offset = 0;
        while offset < buffer.len() {
            let val = Self::rdrand64().ok_or(crabefi::RngError::HardwareError)?;
            let bytes = val.to_le_bytes();
            let remaining = buffer.len() - offset;
            let copy_len = remaining.min(8);
            buffer[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
            offset += copy_len;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// e820 → EFI memory map conversion (x86_64)
// ---------------------------------------------------------------------------

/// Build an EFI memory map from e820 entries, carving out firmware regions.
///
/// Converts e820 type codes to EFI memory types, adds ROM as
/// `RuntimeServicesCode`, and splits RAM regions that overlap with the
/// firmware's BSS/stack areas (marked as `RuntimeServicesData`).
///
/// Returns the number of entries written to `buf`.
///
/// # Arguments
///
/// - `e820`: slice of e820 entries from MemoryDetect
/// - `fw_data_addr`: start of firmware BSS/data in RAM
/// - `fw_data_size`: size of firmware BSS/data/heap region
/// - `fw_stack_addr`: start of firmware stack (grows down from here)
/// - `fw_stack_size`: size of firmware stack
/// - `rom_entries`: static ROM entries (flash as RuntimeServicesCode)
/// - `buf`: output buffer for EFI memory regions
#[allow(clippy::too_many_arguments)]
pub fn build_efi_memory_map_from_e820(
    e820: &[E820Entry],
    fw_data_addr: u64,
    fw_data_size: u64,
    fw_stack_addr: u64,
    fw_stack_size: u64,
    rom_entries: &[MemoryRegion],
    buf: &mut [MemoryRegion],
) -> usize {
    let mut idx = 0;

    // 1. Static ROM entries.
    for entry in rom_entries {
        if idx >= buf.len() {
            break;
        }
        buf[idx] = *entry;
        idx += 1;
    }

    // 2. Firmware reserved regions (BSS/data/heap + stack).
    let fw_data_end = fw_data_addr + fw_data_size;
    let fw_stack_bottom = fw_stack_addr;
    let fw_stack_top = fw_stack_addr + fw_stack_size;

    // 3. Convert e820 entries, splitting RAM that overlaps firmware.
    for e in e820 {
        if idx >= buf.len() {
            break;
        }
        let region_type = match e.kind {
            1 => MemoryType::Ram,
            2 => MemoryType::Reserved,
            3 => MemoryType::AcpiReclaimable,
            4 => MemoryType::AcpiNvs,
            _ => MemoryType::Reserved,
        };

        if region_type != MemoryType::Ram {
            // Non-RAM: pass through as-is.
            buf[idx] = MemoryRegion {
                base: e.addr,
                size: e.size,
                region_type,
            };
            idx += 1;
            continue;
        }

        // RAM region — need to carve out firmware areas.
        let r_start = e.addr;
        let r_end = e.addr + e.size;

        // Collect firmware holes that overlap this RAM region.
        // Sort by start address for correct splitting.
        let mut holes: [(u64, u64); 2] = [(0, 0); 2];
        let mut n_holes = 0;

        // Firmware data/BSS/heap hole
        if fw_data_addr < r_end && fw_data_end > r_start {
            let h_start = fw_data_addr.max(r_start);
            let h_end = fw_data_end.min(r_end);
            if h_start < h_end {
                holes[n_holes] = (h_start, h_end);
                n_holes += 1;
            }
        }

        // Firmware stack hole
        if fw_stack_bottom < r_end && fw_stack_top > r_start {
            let h_start = fw_stack_bottom.max(r_start);
            let h_end = fw_stack_top.min(r_end);
            if h_start < h_end {
                holes[n_holes] = (h_start, h_end);
                n_holes += 1;
            }
        }

        // Sort holes by start address
        if n_holes == 2 && holes[0].0 > holes[1].0 {
            holes.swap(0, 1);
        }

        if n_holes == 0 {
            // No firmware overlap — entire region is free RAM.
            buf[idx] = MemoryRegion {
                base: r_start,
                size: r_end - r_start,
                region_type: MemoryType::Ram,
            };
            idx += 1;
        } else {
            // Split around firmware holes.
            let mut cursor = r_start;
            for &(h_start, h_end) in holes.iter().take(n_holes) {
                // Free RAM before this hole
                if cursor < h_start && idx < buf.len() {
                    buf[idx] = MemoryRegion {
                        base: cursor,
                        size: h_start - cursor,
                        region_type: MemoryType::Ram,
                    };
                    idx += 1;
                }

                // The hole itself (firmware runtime data)
                if idx < buf.len() {
                    buf[idx] = MemoryRegion {
                        base: h_start,
                        size: h_end - h_start,
                        region_type: MemoryType::RuntimeServicesData,
                    };
                    idx += 1;
                }

                cursor = h_end;
            }

            // Free RAM after last hole
            if cursor < r_end && idx < buf.len() {
                buf[idx] = MemoryRegion {
                    base: cursor,
                    size: r_end - cursor,
                    region_type: MemoryType::Ram,
                };
                idx += 1;
            }
        }
    }

    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(fdt: Option<(u64, u64)>, framebuffer: Option<(u64, u64)>) -> [MemoryRegion; 16] {
        let mut buf = [MemoryRegion {
            base: 0,
            size: 0,
            region_type: MemoryType::Reserved,
        }; 16];
        let n = build_efi_memory_map_with_framebuffer(
            &[],
            0x8000_0000,
            0x1000_0000,
            0x8020_0000,
            0x20_0000,
            0x80_0000,
            fdt,
            framebuffer,
            &mut buf,
        );
        for entry in buf.iter_mut().skip(n) {
            entry.size = 0;
        }
        buf
    }

    fn assert_entry(entry: MemoryRegion, base: u64, size: u64, region_type: MemoryType) {
        assert_eq!(entry.base, base);
        assert_eq!(entry.size, size);
        assert_eq!(entry.region_type, region_type);
    }

    #[test]
    fn framebuffer_only_high_dram_is_reserved() {
        let map = collect(None, Some((0x87d0_0000, 0x300000)));
        assert_entry(map[2], 0x8040_0000, 0x7900_000, MemoryType::Ram);
        assert_entry(map[3], 0x87d0_0000, 0x300000, MemoryType::Reserved);
    }

    #[test]
    fn fdt_only_high_dram_is_reserved() {
        let map = collect(Some((0x87f0_0000, 0x100000)), None);
        assert_entry(map[2], 0x8040_0000, 0x7b00_000, MemoryType::Ram);
        assert_entry(map[3], 0x87f0_0000, 0x100000, MemoryType::Reserved);
    }

    #[test]
    fn fdt_and_framebuffer_high_dram_are_both_reserved() {
        let map = collect(Some((0x87f0_0000, 0x100000)), Some((0x87d0_0000, 0x100000)));
        assert_entry(map[2], 0x8040_0000, 0x7900_000, MemoryType::Ram);
        assert_entry(map[3], 0x87d0_0000, 0x100000, MemoryType::Reserved);
        assert_entry(map[4], 0x87e0_0000, 0x100000, MemoryType::Ram);
        assert_entry(map[5], 0x87f0_0000, 0x100000, MemoryType::Reserved);
    }

    #[test]
    fn adjacent_or_overlapping_reservations_are_merged() {
        let adjacent = collect(Some((0x87e0_0000, 0x100000)), Some((0x87d0_0000, 0x100000)));
        assert_entry(adjacent[3], 0x87d0_0000, 0x200000, MemoryType::Reserved);

        let overlapping = collect(Some((0x87d8_0000, 0x100000)), Some((0x87d0_0000, 0x100000)));
        assert_entry(overlapping[3], 0x87d0_0000, 0x180000, MemoryType::Reserved);
    }

    #[test]
    fn out_of_range_reservations_are_ignored() {
        let map = collect(Some((0x7000_0000, 0x100000)), Some((0x9800_0000, 0x100000)));
        assert_entry(map[2], 0x8040_0000, 0xf400_000, MemoryType::Ram);
        assert_entry(
            map[3],
            0x8f80_0000,
            0x800000,
            MemoryType::RuntimeServicesData,
        );
        assert_eq!(map[4].size, 0);
    }
}
