//! Adapter layer between fstart drivers and CrabEFI platform traits.
//!
//! Bridges fstart's service traits (`Console`, `Timer`, `PciRootBus`) to the
//! trait objects that [`crabefi::PlatformConfig`] expects (`DebugOutput`,
//! `Timer`, `ResetHandler`).
//!
//! The adapter types are safe wrappers — no `unsafe` at the call site.

#![no_std]

use core::{cell::Cell, fmt};

// Type aliases used by board-owned UEFI payload configuration.
pub type MemoryRegion = crabefi::MemoryRegion;
pub type MemoryType = crabefi::MemoryType;
pub type FramebufferConfig = crabefi::FramebufferConfig;
pub type RuntimeRegion = crabefi::RuntimeRegion;

/// fstart-owned UEFI payload configuration.
///
/// This keeps fstart stage code independent of CrabEFI internals and only
/// exposes the fields fstart boards currently support.
pub struct PlatformConfig<'a> {
    /// Physical memory map describing RAM, MMIO, and reserved regions.
    pub memory_map: &'a [MemoryRegion],
    /// Monotonic timer for `Stall()` and EFI timer events.
    pub timer: &'a dyn crabefi::Timer,
    /// System reset handler for EFI reset services.
    pub reset: &'a dyn crabefi::ResetHandler,
    /// Block devices to expose to EFI.
    pub block_devices: &'a mut [&'a mut dyn crabefi::BlockDevice],
    /// Variable persistence backend.
    pub variable_backend: Option<&'a mut dyn crabefi::VariableBackend>,
    /// Debug/log output.
    pub debug_output: Option<&'a mut dyn crabefi::DebugOutput>,
    /// Console input.
    pub console_input: Option<&'a mut dyn crabefi::ConsoleInput>,
    /// Framebuffer configuration for GOP.
    pub framebuffer: Option<FramebufferConfig>,
    /// ACPI RSDP physical address.
    pub acpi_rsdp: Option<u64>,
    /// SMBIOS entry point physical address.
    pub smbios: Option<u64>,
    /// Flattened Device Tree blob.
    pub fdt: Option<&'a [u8]>,
    /// Hardware random number generator.
    pub rng: Option<&'a dyn crabefi::Rng>,
    /// PCI ECAM base address.
    pub ecam_base: Option<u64>,
    /// Runtime-services code/data region.
    pub runtime_region: Option<RuntimeRegion>,
    /// Whether CrabEFI heap/environment initialization already ran.
    pub heap_pre_initialized: bool,
}

/// Common fstart-owned UEFI launch inputs.
pub struct UefiLaunchConfig<'a> {
    /// Console selected by board policy for UEFI debug/input adapters.
    pub console: Option<&'a dyn fstart_core::services::Console>,
    /// Framebuffer configuration for GOP.
    pub framebuffer: Option<FramebufferConfig>,
    /// ACPI RSDP physical address.
    pub acpi_rsdp: Option<u64>,
    /// SMBIOS entry point physical address.
    pub smbios: Option<u64>,
    /// Flattened Device Tree blob.
    pub fdt: Option<&'a [u8]>,
    /// PCI ECAM base address.
    pub ecam_base: Option<u64>,
    /// Runtime-services code/data region.
    pub runtime_region: Option<RuntimeRegion>,
}

fn init_platform_raw(config: PlatformConfig<'_>) -> ! {
    enable_payload_cpu_features();
    crabefi::init_platform(crabefi::PlatformConfig {
        memory_map: config.memory_map,
        timer: config.timer,
        reset: config.reset,
        block_devices: config.block_devices,
        variable_backend: config.variable_backend,
        debug_output: config.debug_output,
        console_input: config.console_input,
        framebuffer: config.framebuffer,
        acpi_rsdp: config.acpi_rsdp,
        smbios: config.smbios,
        fdt: config.fdt,
        rng: config.rng,
        ecam_base: config.ecam_base,
        deferred_buffer: None,
        runtime_region: config.runtime_region,
        heap_pre_initialized: config.heap_pre_initialized,
    })
}

/// Call `crabefi::init_platform()`. This is the entry point that never returns.
pub fn init_platform(config: PlatformConfig<'_>) -> ! {
    init_platform_raw(config)
}

/// Launch CrabEFI on x86 using fstart runtime state.
#[cfg(target_arch = "x86_64")]
pub fn launch_x86_uefi(
    launch: UefiLaunchConfig<'_>,
    e820: &[E820Entry],
    platform_entries: &[MemoryRegion],
) -> ! {
    let timer = TscTimer::new();
    let reset = X86Reset;
    let rng = X86Rng::new();
    let mut memory_map_buf: [MemoryRegion; 64] = [MemoryRegion {
        base: 0,
        size: 0,
        region_type: MemoryType::Reserved,
    }; 64];
    let memory_map_len =
        build_efi_memory_map_from_e820(e820, 0, 0, 0, 0, platform_entries, &mut memory_map_buf);
    let memory_map = &memory_map_buf[..memory_map_len];
    launch_with_adapters(launch, memory_map, &timer, &reset, Some(&rng))
}

/// Launch CrabEFI from a board-provided flat RAM/static memory description.
pub fn launch_flat_uefi(
    launch: UefiLaunchConfig<'_>,
    static_entries: &[MemoryRegion],
    ram_base: u64,
    ram_size: u64,
    fw_data_addr: u64,
    fw_stack_size: u64,
    fdt_reservation: Option<(u64, u64)>,
) -> ! {
    let timer = ArmGenericTimer::new();
    let reset = PsciReset;
    let mut memory_map_buf: [MemoryRegion; 12] = [MemoryRegion {
        base: 0,
        size: 0,
        region_type: MemoryType::Reserved,
    }; 12];
    let memory_map_len = build_efi_memory_map(
        static_entries,
        ram_base,
        ram_size,
        fw_data_addr,
        fw_stack_size,
        fw_stack_size,
        fdt_reservation,
        &mut memory_map_buf,
    );
    let memory_map = &memory_map_buf[..memory_map_len];
    launch_with_adapters(launch, memory_map, &timer, &reset, None)
}

fn launch_with_adapters(
    launch: UefiLaunchConfig<'_>,
    memory_map: &[MemoryRegion],
    timer: &dyn crabefi::Timer,
    reset: &dyn crabefi::ResetHandler,
    rng: Option<&dyn crabefi::Rng>,
) -> ! {
    let mut block_devices: [&mut dyn crabefi::BlockDevice; 0] = [];
    match launch.console {
        Some(console) => {
            let mut debug_output = ConsoleAdapter::new(console);
            let mut console_input = ConsoleAdapter::new(console);
            init_platform_raw(PlatformConfig {
                memory_map,
                timer,
                reset,
                block_devices: &mut block_devices,
                variable_backend: None,
                debug_output: Some(&mut debug_output),
                console_input: Some(&mut console_input),
                framebuffer: launch.framebuffer,
                acpi_rsdp: launch.acpi_rsdp,
                smbios: launch.smbios,
                fdt: launch.fdt,
                rng,
                ecam_base: launch.ecam_base,
                runtime_region: launch.runtime_region,
                heap_pre_initialized: false,
            })
        }
        None => init_platform_raw(PlatformConfig {
            memory_map,
            timer,
            reset,
            block_devices: &mut block_devices,
            variable_backend: None,
            debug_output: None,
            console_input: None,
            framebuffer: launch.framebuffer,
            acpi_rsdp: launch.acpi_rsdp,
            smbios: launch.smbios,
            fdt: launch.fdt,
            rng,
            ecam_base: launch.ecam_base,
            runtime_region: launch.runtime_region,
            heap_pre_initialized: false,
        }),
    }
}

/// Enable architectural CPU features expected by common UEFI applications.
#[cfg(target_arch = "x86_64")]
fn enable_payload_cpu_features() {
    // GRUB's x86_64 EFI binary may use SSE instructions. Coreboot's CrabEFI
    // entry path enables SSE before entering Rust; fstart enters CrabEFI as a
    // library, so do the equivalent setup here before launching EFI payloads.
    unsafe {
        core::arch::asm!(
            "cld",
            "mov rax, cr0",
            "and rax, 0xfffffffffffffff3", // clear EM (bit 2) and TS (bit 3)
            "or  rax, 0x2",                // set MP (bit 1)
            "mov cr0, rax",
            "mov rax, cr4",
            "or  rax, 0x200",              // OSFXSR (bit 9)
            "or  rax, 0x400",              // OSXMMEXCPT (bit 10)
            "mov cr4, rax",
            out("rax") _,
            options(nostack, preserves_flags),
        );
    }
}

/// Non-x86 platforms currently need no additional CPU-feature setup here.
#[cfg(not(target_arch = "x86_64"))]
fn enable_payload_cpu_features() {}

// ---------------------------------------------------------------------------
// Console → DebugOutput adapter
// ---------------------------------------------------------------------------

/// Wraps an fstart [`Console`](fstart_core::services::Console) as a CrabEFI
/// [`DebugOutput`](crabefi::DebugOutput).
///
/// fstart's `Console` uses `&self` (MMIO is inherently interior-mutable)
/// and returns `Result`. CrabEFI's `DebugOutput` uses `&mut self` and
/// ignores errors. The adapter bridges both differences.
pub struct ConsoleAdapter<'a, C: fstart_core::services::Console + ?Sized> {
    console: &'a C,
    pending: Cell<Option<u8>>,
}

impl<'a, C: fstart_core::services::Console + ?Sized> ConsoleAdapter<'a, C> {
    /// Create a new adapter around an fstart console.
    pub const fn new(console: &'a C) -> Self {
        Self {
            console,
            pending: Cell::new(None),
        }
    }

    fn read_pending_or_console(&self) -> Option<u8> {
        self.pending
            .take()
            .or_else(|| self.console.read_byte().ok().flatten())
    }

    fn has_pending_or_console_input(&self) -> bool {
        if self.pending.get().is_some() {
            return true;
        }
        if let Some(byte) = self.console.read_byte().ok().flatten() {
            self.pending.set(Some(byte));
            true
        } else {
            false
        }
    }
}

impl<C: fstart_core::services::Console + ?Sized> crabefi::DebugOutput for ConsoleAdapter<'_, C> {
    fn write_byte(&mut self, byte: u8) {
        let _ = self.console.write_byte(byte);
    }

    fn try_read_byte(&self) -> Option<u8> {
        self.read_pending_or_console()
    }

    fn has_input(&self) -> bool {
        self.has_pending_or_console_input()
    }
}

impl<C: fstart_core::services::Console + ?Sized> crabefi::ConsoleInput for ConsoleAdapter<'_, C> {
    fn read_key(&mut self) -> Option<crabefi::Key> {
        self.read_pending_or_console().map(|byte| crabefi::Key {
            scancode: 0,
            unicode_char: byte as u16,
        })
    }

    fn has_key(&self) -> bool {
        self.has_pending_or_console_input()
    }
}

// `crabefi::DebugOutput` has `core::fmt::Write` as a supertrait,
// so this impl is required — not optional.
impl<C: fstart_core::services::Console + ?Sized> fmt::Write for ConsoleAdapter<'_, C> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                let _ = self.console.write_byte(b'\r');
            }
            let _ = self.console.write_byte(byte);
        }
        Ok(())
    }
}

// SAFETY: fstart's Console is Send + Sync (required by the trait bound).
// ConsoleAdapter holds an immutable reference to it, which is Send.
unsafe impl<C: fstart_core::services::Console + ?Sized> Send for ConsoleAdapter<'_, C> {}

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

    // 4. Free RAM between BSS end and stack bottom.
    if fw_stack_bottom > fw_bss_end {
        buf[idx] = MemoryRegion {
            base: fw_bss_end,
            size: fw_stack_bottom - fw_bss_end,
            region_type: MemoryType::Ram,
        };
        idx += 1;
    }

    // 5. Firmware stack -- RuntimeServicesData.
    buf[idx] = MemoryRegion {
        base: fw_stack_bottom,
        size: fw_stack_size,
        region_type: MemoryType::RuntimeServicesData,
    };
    idx += 1;

    idx
}

// Re-export types for codegen convenience.
pub use fstart_core::services::memory_detect::E820Entry;

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
// x86_64 TSC Timer → CrabEFI Timer adapter
// ---------------------------------------------------------------------------

/// CrabEFI [`Timer`](crabefi::Timer) backed by the x86 TSC.
///
/// Obtains the TSC frequency from [`fstart_arch::x86::tsc_frequency_hz()`]
/// and uses that value for TSC-to-time conversion.
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
    /// Create a new timer from the platform TSC frequency hint.
    pub fn new() -> Self {
        Self {
            tsc_freq: fstart_arch::x86::tsc_frequency_hz(),
        }
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
                    fstart_core::pio::outw(0x0404, 0x2000); // SLP_EN (bit 13); QEMU _S5 defines SLP_TYP=0
                }
            }
            _ => {
                // Keyboard controller reset: pulse CPU reset line
                unsafe {
                    // Wait for input buffer empty
                    for _ in 0..10000 {
                        if fstart_core::pio::inb(0x64) & 0x02 == 0 {
                            break;
                        }
                        core::hint::spin_loop();
                    }
                    fstart_core::pio::outb(0x64, 0xFE); // reset command
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
        let (_, _, ecx, _) = fstart_arch::x86::cpuid(1);
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

    // 1. Static entries outside RAM (flash, MMIO apertures, etc.).  Static
    // entries that overlap RAM are emitted while splitting the RAM e820 range
    // below; copying them here would create overlapping EFI descriptors.
    for entry in rom_entries {
        let entry_end = entry.base.saturating_add(entry.size);
        let overlaps_ram = e820.iter().any(|e| {
            e.kind == 1 && entry.base < e.addr.saturating_add(e.size) && entry_end > e.addr
        });
        let overlaps_non_ram = e820.iter().any(|e| {
            e.kind != 1 && entry.base < e.addr.saturating_add(e.size) && entry_end > e.addr
        });
        if overlaps_ram || overlaps_non_ram {
            continue;
        }
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

    // 3. Convert e820 entries, splitting RAM that overlaps firmware or
    // platform table allocations (ACPI/SMBIOS) supplied as static entries.
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

        let r_start = e.addr;
        let r_end = e.addr + e.size;

        let mut holes: [(u64, u64, MemoryType); 8] = [(0, 0, MemoryType::Reserved); 8];
        let mut n_holes = 0;

        if fw_data_addr < r_end && fw_data_end > r_start && n_holes < holes.len() {
            let h_start = fw_data_addr.max(r_start);
            let h_end = fw_data_end.min(r_end);
            if h_start < h_end {
                holes[n_holes] = (h_start, h_end, MemoryType::RuntimeServicesData);
                n_holes += 1;
            }
        }

        if fw_stack_bottom < r_end && fw_stack_top > r_start && n_holes < holes.len() {
            let h_start = fw_stack_bottom.max(r_start);
            let h_end = fw_stack_top.min(r_end);
            if h_start < h_end {
                holes[n_holes] = (h_start, h_end, MemoryType::RuntimeServicesData);
                n_holes += 1;
            }
        }

        for entry in rom_entries {
            if n_holes >= holes.len() {
                break;
            }
            let h_start = entry.base.max(r_start);
            let h_end = entry.base.saturating_add(entry.size).min(r_end);
            if h_start < h_end {
                holes[n_holes] = (h_start, h_end, entry.region_type);
                n_holes += 1;
            }
        }

        for i in 1..n_holes {
            let mut j = i;
            while j > 0 && holes[j - 1].0 > holes[j].0 {
                holes.swap(j - 1, j);
                j -= 1;
            }
        }

        let mut cursor = r_start;
        for &(h_start, h_end, h_type) in holes.iter().take(n_holes) {
            if cursor < h_start && idx < buf.len() {
                buf[idx] = MemoryRegion {
                    base: cursor,
                    size: h_start - cursor,
                    region_type: MemoryType::Ram,
                };
                idx += 1;
            }

            if h_end > cursor && idx < buf.len() {
                let start = h_start.max(cursor);
                buf[idx] = MemoryRegion {
                    base: start,
                    size: h_end - start,
                    region_type: h_type,
                };
                idx += 1;
                cursor = h_end;
            }
        }

        if cursor < r_end && idx < buf.len() {
            buf[idx] = MemoryRegion {
                base: cursor,
                size: r_end - cursor,
                region_type: MemoryType::Ram,
            };
            idx += 1;
        }
    }

    idx
}
