//! Adapter layer between fstart drivers and CrabEFI platform traits.
//!
//! Bridges fstart's runtime services (`Console`, `Timer`, PCI config access) to the
//! trait objects that [`crabefi::PlatformConfig`] expects (`DebugOutput`,
//! `Timer`, `ResetHandler`).
//!
//! The adapter types are safe wrappers — no `unsafe` at the call site.

use core::{cell::Cell, fmt};

mod runtime;
use runtime::{EMPTY_REGION, firmware_reservations, runtime_platform_config};

// Type aliases used by board-owned UEFI payload configuration.
pub type MemoryRegion = crabefi::MemoryRegion;
pub type MemoryType = crabefi::MemoryType;
pub type FramebufferConfig = crabefi::FramebufferConfig;
pub use crabefi::{PciEcamRegion, RuntimeImageSource, RuntimePlatformConfig, StorageBackend};

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
    /// Optional bounded, boot-lifetime variable region. None is explicitly
    /// volatile; this adapter never discovers or unlocks whole-chip flash.
    pub variable_storage: Option<&'a mut dyn StorageBackend>,
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
    /// Actual PCI ECAM segment/bus bounds (empty enables ACPI/FDT discovery).
    pub ecam_regions: &'a [PciEcamRegion],
    /// Normalized separate runtime image and value-only platform mechanisms.
    pub runtime_image: RuntimeImageSource<'a>,
    pub runtime: RuntimePlatformConfig<'a>,
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
    /// Actual PCI ECAM segment/bus bounds (empty enables ACPI/FDT discovery).
    pub ecam_regions: &'a [PciEcamRegion],
}

fn init_platform_raw(config: PlatformConfig<'_>) -> ! {
    enable_payload_cpu_features();
    // The builder owns conditional fields (including TPM) under CrabEFI's
    // effective Cargo features, not the profile originally requested by a host.
    let defaults = crabefi::PlatformConfigBuilder::new(
        config.memory_map,
        config.timer,
        config.reset,
        config.block_devices,
        config.runtime_image,
        config.runtime,
    )
    .heap_pre_initialized(config.heap_pre_initialized)
    .build();
    crabefi::init_platform(crabefi::PlatformConfig {
        variable_storage: config.variable_storage.map_or(
            crabefi::VariableStorage::None,
            crabefi::VariableStorage::Platform,
        ),
        debug_output: config.debug_output,
        console_input: config.console_input,
        framebuffer: config.framebuffer,
        acpi_rsdp: config.acpi_rsdp,
        smbios: config.smbios,
        fdt: config.fdt,
        rng: config.rng,
        ecam_regions: config.ecam_regions,
        ..defaults
    })
}

/// Call `crabefi::init_platform()`. This is the entry point that never returns.
pub fn init_platform(config: PlatformConfig<'_>) -> ! {
    init_platform_raw(config)
}

/// Record the OpenSBI boot hart before CrabEFI installs its RISC-V EFI protocol.
#[cfg(target_arch = "riscv64")]
pub fn set_riscv_boot_hartid(hart_id: u64) {
    crabefi::efi::set_boot_hartid(hart_id);
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
    let mut reserved = [EMPTY_REGION; 16];
    let count = collect_firmware_reservations(platform_entries, &mut reserved);
    let memory_map_len =
        build_efi_memory_map_from_e820(e820, &reserved[..count], &mut memory_map_buf);
    let memory_map = &memory_map_buf[..memory_map_len];
    launch_with_adapters(launch, memory_map, &timer, &reset, Some(&rng))
}

/// Launch CrabEFI from a board-provided flat RAM/static memory description.
pub fn launch_flat_uefi(
    launch: UefiLaunchConfig<'_>,
    static_entries: &[MemoryRegion],
    ram_base: u64,
    ram_size: u64,
    fdt_reservation: Option<(u64, u64)>,
) -> ! {
    #[cfg(not(target_arch = "riscv64"))]
    let timer = ArmGenericTimer::new();
    #[cfg(not(target_arch = "riscv64"))]
    let reset = PsciReset;
    let mut memory_map_buf: [MemoryRegion; 16] = [MemoryRegion {
        base: 0,
        size: 0,
        region_type: MemoryType::Reserved,
    }; 16];
    let memory_map_len = build_efi_memory_map(
        static_entries,
        ram_base,
        ram_size,
        fdt_reservation,
        &mut memory_map_buf,
    );
    let memory_map = &memory_map_buf[..memory_map_len];

    #[cfg(target_arch = "aarch64")]
    launch_with_adapters(launch, memory_map, &timer, &reset, None);

    #[cfg(target_arch = "riscv64")]
    {
        let timer = RiscvTimer;
        let reset = RiscvSbiReset;
        launch_with_adapters(launch, memory_map, &timer, &reset, None)
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
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
    let mut debug_output = launch.console.map(ConsoleAdapter::new);
    let mut console_input = launch.console.map(ConsoleAdapter::new);
    if let Some(output) = debug_output.as_mut() {
        b"fstart: CrabEFI variables have no persistent backend or retained journal\r\n"
            .iter()
            .for_each(|byte| crabefi::DebugOutput::write_byte(output, *byte));
        #[cfg(not(target_arch = "x86_64"))]
        b"fstart: runtime wall-clock unsupported; no RTC runtime MMIO mapping\r\n"
            .iter()
            .for_each(|byte| crabefi::DebugOutput::write_byte(output, *byte));
    }
    init_platform_raw(PlatformConfig {
        memory_map,
        timer,
        reset,
        block_devices: &mut block_devices,
        variable_storage: None,
        debug_output: debug_output
            .as_mut()
            .map(|d| d as &mut dyn crabefi::DebugOutput),
        console_input: console_input
            .as_mut()
            .map(|c| c as &mut dyn crabefi::ConsoleInput),
        framebuffer: launch.framebuffer,
        acpi_rsdp: launch.acpi_rsdp,
        smbios: launch.smbios,
        fdt: launch.fdt,
        rng,
        ecam_regions: launch.ecam_regions,
        // This constant follows the effective dependency features, including
        // full/basic unification. The loader checks ABI and exact capabilities.
        runtime_image: crabefi::BUNDLED_RUNTIME_IMAGE,
        runtime: runtime_platform_config(),
        heap_pre_initialized: false,
    })
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

/// Build the EFI map without exposing boot firmware or the FDT as allocatable
/// RAM. The separate runtime loader owns runtime-code/data reservations.
pub fn build_efi_memory_map(
    static_entries: &[MemoryRegion],
    ram_base: u64,
    ram_size: u64,
    fdt_reservation: Option<(u64, u64)>,
    buf: &mut [MemoryRegion],
) -> usize {
    let mut reserved = [EMPTY_REGION; 16];
    let mut count = collect_firmware_reservations(static_entries, &mut reserved);
    if let Some((base, size)) = fdt_reservation.filter(|(_, size)| *size > 0) {
        reserved[count] = MemoryRegion {
            base,
            size,
            region_type: MemoryType::Reserved,
        };
        count += 1;
    }
    let ram = E820Entry {
        addr: ram_base,
        size: ram_size,
        kind: 1,
    };
    build_efi_memory_map_from_e820(&[ram], &reserved[..count], buf)
}

// Re-export types used by the CrabEFI adapter.
pub use fstart_core::services::memory_detect::E820Entry;

fn collect_firmware_reservations(entries: &[MemoryRegion], buf: &mut [MemoryRegion]) -> usize {
    let firmware = firmware_reservations();
    let mut count = 0;
    for entry in entries
        .iter()
        .chain(&firmware)
        .filter(|entry| entry.size != 0)
    {
        let end = entry
            .base
            .checked_add(entry.size)
            .expect("memory region overflow");
        if buf[..count].iter().any(|previous| {
            previous.region_type == entry.region_type
                && previous.base <= entry.base
                && previous.base + previous.size >= end
        }) {
            continue;
        }
        buf[count] = *entry;
        count += 1;
    }
    count
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
// RISC-V time/SBI reset adapters
// ---------------------------------------------------------------------------

/// CrabEFI [`Timer`](crabefi::Timer) backed by QEMU virt's 10 MHz `time` CSR.
#[cfg(target_arch = "riscv64")]
pub struct RiscvTimer;

#[cfg(target_arch = "riscv64")]
impl crabefi::Timer for RiscvTimer {
    fn current_ticks(&self) -> u64 {
        let ticks: u64;
        // SAFETY: `rdtime` reads the supervisor-visible monotonic counter.
        unsafe {
            core::arch::asm!(
                "rdtime {}",
                out(reg) ticks,
                options(nomem, nostack, preserves_flags),
            );
        }
        ticks
    }

    fn ticks_per_second(&self) -> u64 {
        10_000_000
    }
}

/// CrabEFI [`ResetHandler`](crabefi::ResetHandler) using OpenSBI SRST.
#[cfg(target_arch = "riscv64")]
pub struct RiscvSbiReset;

#[cfg(target_arch = "riscv64")]
impl crabefi::ResetHandler for RiscvSbiReset {
    fn reset(&self, reset_type: crabefi::ResetType) -> ! {
        let reset_type = match reset_type {
            crabefi::ResetType::Shutdown => 0,
            crabefi::ResetType::Cold => 1,
            crabefi::ResetType::Warm => 2,
            _ => 1,
        };
        fstart_arch::riscv64::sbi_system_reset(reset_type)
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
/// Converts e820 type codes and carves platform/boot-firmware reservations
/// out of RAM. Only CrabEFI's separate loader allocates runtime code/data.
///
/// Returns the number of entries written to `buf`.
///
/// # Arguments
///
/// - `e820`: slice of e820 entries from MemoryDetect
/// - `rom_entries`: platform and firmware reservations
/// - `buf`: output buffer for EFI memory regions
///
/// Panics if bounded buffers cannot represent the map; never drops a reservation.
pub fn build_efi_memory_map_from_e820(
    e820: &[E820Entry],
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
        buf[idx] = *entry;
        idx += 1;
    }

    // 2. Convert e820 entries, splitting RAM that overlaps firmware or
    // platform table allocations (ACPI/SMBIOS) supplied as static entries.
    for e in e820 {
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

        let mut holes: [(u64, u64, MemoryType); 16] = [(0, 0, MemoryType::Reserved); 16];
        let mut n_holes = 0;

        for entry in rom_entries {
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
            if cursor < h_start {
                buf[idx] = MemoryRegion {
                    base: cursor,
                    size: h_start - cursor,
                    region_type: MemoryType::Ram,
                };
                idx += 1;
            }

            if h_end > cursor {
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

        if cursor < r_end {
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
