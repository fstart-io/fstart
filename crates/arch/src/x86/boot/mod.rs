//! x86 stage runtime for fstart.
//!
//! Provides the entry point (reset vector through 16-bit real mode to 32-bit
//! protected mode), CAR, GDT/IDT setup and the Linux boot protocol handoff.
//! Long-mode targets (`x86_64`) then build identity page tables and switch
//! to 64-bit mode ([`long_mode`]); the opt-in 32-bit target (`x86`) stays in
//! flat protected mode without paging ([`protected_mode`]).
//!
//! # Entry Flow
//!
//! 1. Reset vector at `0xFFFFFFF0`: `jmp _start16bit`
//! 2. 16-bit real mode: load GDT, enable protected mode
//! 3. 32-bit protected mode: CAR, BSS; on `x86_64` page tables and long mode
//! 4. Set stack, set up the IDT, call `fstart_main(0)`
//!
//! # QEMU Q35
//!
//! On QEMU Q35, RAM works immediately without Cache-as-RAM setup.
//! MTRRs are no-ops. The firmware runs XIP from pflash mapped at the
//! top of the 4 GiB address space.

#![allow(clippy::doc_lazy_continuation)]

/// Reset the system through the architected x86 CF9 reset register.
///
/// Used by early stages that have no chipset driver bound (e.g. the postcar
/// rejecting an invalid S3 resume). Southbridge drivers that must clear
/// sticky reset side state override this with their own sequence.
///
/// # Safety of the operation
/// CF9 is the architected reset port on every IA-32 platform fstart supports;
/// the write sequence is the documented one (clear, then request reset).
pub fn system_reset(hard: bool) -> ! {
    unsafe {
        fstart_core::pio::outb(0xCF9, 0x00);
        fstart_core::pio::outb(0xCF9, if hard { 0x06 } else { 0x02 });
    }
    loop {
        core::hint::spin_loop();
    }
}

pub mod car;
pub mod car_teardown;
pub mod cpuid;
mod linux_boot_params;
#[cfg(target_arch = "x86_64")]
pub mod paging;
pub mod s3_wake;

/// Conventional-memory window the x86 stages use across an S3 resume and AP
/// bring-up: the postcar handoff stash
/// ([`car_teardown::POSTCAR_STASH_ADDR`], 0x2000) and the SIPI trampoline page
/// (0x8000). Platforms must exclude it from OS-visible RAM: on resume the
/// firmware rewrites these bytes while the suspended OS image is live.
///
/// Starts at 0x1000 because the real-mode IVT/BDA below it is reserved by
/// every consumer already; [`s3_wake::WAKEUP_BASE`] (0x600) is inside that
/// first page.
pub const LOW_SCRATCH_START: u64 = 0x1000;
/// End of [`LOW_SCRATCH_START`], exclusive.
pub const LOW_SCRATCH_END: u64 = 0xf000;

/// Physical address of the DRAM-backed identity page tables postcar builds.
///
/// Inside the reserved low-scratch window, above the postcar stash (0x2000) and
/// the SIPI trampoline page (0x8000), and below the EBDA. It must stay reserved:
/// the tables are live until the payload replaces CR3.
pub const PAGE_TABLES_ADDR: u64 = 0x9000;

use crate::x86::mtrr;
use fstart_core::services::memory_detect::E820Entry;
use linux_boot_params::{E820_CAPACITY, LinuxBootParams, LinuxE820Entry};
use zerocopy::FromBytes;
use zerocopy::byteorder::{LE, U32, U64};

/// Enable the BSP-local ROM cacheability MTRR for memory-mapped boot media.
///
/// MP setup installs identical RAM MTRRs on all CPUs, so the temporary ROM WP
/// MTRR must be installed after MP setup and only on the BSP when firmware is
/// about to read memory-mapped flash. It is cleared before Linux handoff.
pub fn enable_boot_media_rom_cache() {
    // SAFETY: selected stage code calls this on the BSP immediately before
    // memory-mapped boot-media reads. The MTRR is cleared before OS handoff.
    match unsafe { mtrr::set_boot_rom_wp(true) } {
        Ok(()) => fstart_log::debug!("mtrr: temporary BSP ROM WP enabled"),
        Err(_) => {
            fstart_log::info!("mtrr: ROM WP optimization unavailable; cacheability unchanged")
        }
    }
}

/// Clear the BSP-local temporary ROM cacheability MTRR before payload/OS handoff.
pub fn disable_boot_media_rom_cache_for_handoff() {
    // SAFETY: selected stage code calls this on the BSP immediately before
    // handing control to a payload/OS that expects coherent MTRR state.
    if unsafe { mtrr::set_boot_rom_wp(false) }.is_err() {
        fstart_log::error!("mtrr: could not clear temporary BSP ROM WP");
    }
}

// ---------------------------------------------------------------------------
// Entry point — 16-bit real mode → 32-bit protected mode → 64-bit long mode
// ---------------------------------------------------------------------------

// Where the shared 32-bit entry continues once CAR and BSS are live: build
// or select the identity page tables and enter long mode, or stay in 32-bit
// protected mode and call Rust directly.
#[cfg(target_arch = "x86_64")]
#[allow(unused_macros)]
macro_rules! enter_runtime {
    () => {
        "jmp _setup_page_tables"
    };
}
#[cfg(target_arch = "x86")]
#[allow(unused_macros)]
macro_rules! enter_runtime {
    () => {
        "jmp _start32_runtime"
    };
}

// The entry sequence is written as `global_asm!` because it transitions
// through three CPU modes before reaching Rust code. The `_start16bit`
// label is placed in `.text.entry` by the linker script.
//
// GDT layout (matches coreboot's early x86 GDT):
// - 0x00: null descriptor
// - 0x08: 32-bit flat code (used for 16→32 bit transition)
// - 0x10: flat data (used in both 32-bit mode and long mode)
// - 0x18: 64-bit code (Long mode, Execute/Read)
//
// Page tables: identity-mapped 2 MiB pages covering 4 GiB.
// PML4 → 1 PDPT → 4 PDTs → 512 × 2 MiB pages each.
//
// Reset-vector entry only: postcar and ramstage enter via direct 64-bit
// jumps (`_start_postcar` / `_start_ram`), so this block is compiled out of
// those stages to keep postcar small.
#[cfg(all(not(test), target_os = "none", not(fstart_stage_env = "postcar")))]
core::arch::global_asm!(
    // Use AT&T syntax throughout — matches coreboot convention and is
    // the natural syntax for 16-bit / mixed-mode x86 assembly.
    // =====================================================================
    // Entire entry sequence in .text.entry section.
    //
    // The .reset section contains ONLY the reset vector jump (16 bytes at
    // 0xFFFFFFF0). The rest of the 16-bit/32-bit/64-bit entry code lives
    // in .text.entry which is placed by the linker script at the start
    // of the ROM image. The reset vector uses an absolute far jump (via
    // raw bytes) to reach _start16bit regardless of distance.
    // =====================================================================

    // --- Reset vector (pinned to 0xFFFFFFF0 by linker) ---
    ".section .reset, \"ax\"",
    ".code16",
    ".global _start",
    "_start:",
    // Long jump to the 16-bit entry. We encode this as raw bytes because
    // the target may be >32KB away (outside 16-bit relative range).
    // EA xx xx xx xx 08 00 = ljmpw $0x08, $abs32
    // But we are still in real mode before GDT is loaded, so we use a
    // simple near relative jump. The linker will compute the 16-bit
    // offset. If it's out of range, we fall back to a long form.
    // Actually: at reset, CS=0xF000 and IP=0xFFF0 (flat = 0xFFFF_FFF0).
    // CS base is 0xFFFF_0000 in the hidden portion. A near jmp to
    // _start16bit works if it's within 64K below 0xFFFF_FFF0, i.e.
    // anywhere in 0xFFFF_0000..0xFFFF_FFFF. So we place .text.entry
    // near the end of ROM (within the last 64K).
    "jmp _start16bit",
    ".align 16",
    // --- 16-bit entry code (must be within 4K of reset vector) ---
    //
    // Follows coreboot entry16.S conventions:
    //   - GAS mnemonics only (no manual .byte encoding)
    //   - CS-relative addressing for lgdtl/lidt via runtime offset
    //     computation, avoiding the 0x67 address-size prefix that
    //     breaks KVM real-mode instruction emulation
    //   - linker resolves label offsets within the boot block
    ".section .x86boot, \"ax\"",
    ".code16",
    ".global _start16bit",
    "_start16bit:",
    "cli",
    "movl %eax, %ebp",
    // Keep the reset-vector TSC in MM0/MM1 for the first boot timestamp,
    // like coreboot. Nothing before Rust uses MMX, and firmware Rust is
    // built without it.
    "rdtsc",
    "movd %eax, %mm0",
    "movd %edx, %mm1",
    // POST 0x01: reset vector reached the 16-bit entry.
    "movb $0x01, %al",
    "outb %al, $0x80",
    // Invalidate TLB
    "xorl %eax, %eax",
    "movl %eax, %cr3",
    // Compute CS-relative base for descriptor lookups.
    // At reset CS = 0xF000, hidden base = 0xFFFF0000.
    // shlw $4 on 0xF000 overflows to 0 (16-bit), so the subtraction
    // below is effectively a no-op — but keeps the code relocatable
    // for AP startup where CS may differ (same pattern as coreboot).
    "movw %cs, %ax",
    "shlw $4, %ax",
    // Load null IDT — CPU will shutdown on any exception before the
    // 64-bit IDT is set up in _setup_idt. No 'l' suffix: 16-bit
    // operand size loads a 5-byte descriptor (24-bit base = 0).
    "movw $(_null_idt - _start16bit + 0xf000), %bx",
    "subw %ax, %bx",
    "lidt %cs:(%bx)",
    // Load GDT — 'l' suffix forces 32-bit operand size so the full
    // 32-bit GDT base address is loaded from the 6-byte descriptor.
    "movw $(_gdt_desc - _start16bit + 0xf000), %bx",
    "subw %ax, %bx",
    "lgdtl %cs:(%bx)",
    // POST 0x10: about to enter 32-bit protected mode.
    "movb $0x10, %al",
    "outb %al, $0x80",
    // Enable protected mode: set PE, disable caching (CD+NW)
    "movl %cr0, %eax",
    "andl $0x7FFAFFD1, %eax", // PG,AM,WP,NE,TS,EM,MP = 0
    "orl $0x60000001, %eax",  // CD, NW, PE = 1
    "movl %eax, %cr0",
    // Far jump to 32-bit protected mode (GDT selector 0x08)
    "ljmpl $0x08, $_start32bit",
    // =====================================================================
    // Null IDT descriptor (limit=0, base=0)
    // =====================================================================
    ".align 4",
    "_null_idt:",
    ".word 0", // limit
    ".long 0", // base
    ".word 0", // padding
    // =====================================================================
    // GDT and descriptor
    // =====================================================================
    ".align 4",
    "_gdt:",
    // Entry 0x00: null descriptor
    ".word 0x0000, 0x0000",
    ".byte 0x00, 0x00, 0x00, 0x00",
    // Entry 0x08: 32-bit flat code (base=0, limit=4G, G=1, D=1)
    // Used only for the 16-bit → 32-bit ljmpl transition.
    ".word 0xffff, 0x0000",
    ".byte 0x00, 0x9b, 0xcf, 0x00",
    // Entry 0x10: flat data (base=0, limit=4G, G=1, B=1)
    // Used as the data segment in both 32-bit protected mode and long mode.
    ".word 0xffff, 0x0000",
    ".byte 0x00, 0x93, 0xcf, 0x00",
    // Entry 0x18: 64-bit code (L=1, D=0, base=0, limit=4G)
    ".word 0xffff, 0x0000",
    ".byte 0x00, 0x9b, 0xaf, 0x00",
    "_gdt_end:",
    "_gdt_desc:",
    ".word _gdt_end - _gdt - 1", // limit
    ".long _gdt",                // base
    // =====================================================================
    // 32-bit protected mode entry — in .text (linked at ROM base)
    //
    // The far jump from 16-bit code uses an absolute 32-bit address so
    // this can be anywhere in the 4 GiB address space.
    // =====================================================================
    ".section .text, \"ax\"",
    ".code32",
    ".global _start32bit",
    "_start32bit:",
    // POST 0x20: protected-mode entry reached.
    "movb $0x20, %al",
    "outb %al, $0x80",
    // Load data segment selectors (0x10 = flat data, coreboot GDT_DATA_SEG)
    "movw $0x10, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movw %ax, %fs",
    "movw %ax, %gs",
    // ---- Very early BSP microcode update ----
    // Uses only registers and the anchor-patched microcode offset.  It runs
    // before CAR/Rust so CPU errata fixes are present for the rest of init.
    "movl $_after_early_microcode, %esp",
    "jmp _early_intel_microcode",
    "_after_early_microcode:",
    // ---- CAR setup (real hardware only) ----
    // On boards with Cache-as-RAM, we must enable it BEFORE any memory
    // writes (BSS clear, page tables, stack pushes) because all writable
    // memory lives in cache.  On QEMU / non-CAR boards, _has_car == 0
    // and we skip straight to BSS clear.
    //
    // _car_setup returns via jmp *%ebp.
    "movl $_has_car, %eax",
    "testl %eax, %eax",
    "je _post_car",
    // POST 0x21: entering Cache-as-RAM setup.
    "movb $0x21, %al",
    "outb %al, $0x80",
    "movl $_post_car, %ebp",
    "jmp _car_setup",
    "_post_car:",
    // POST 0x22: CAR is available (or not required).
    "movb $0x22, %al",
    "outb %al, $0x80",
    // Clear BSS.  On CAR boards this zeroes the CAR-backed BSS region
    // (now live after _car_setup).  On QEMU it zeroes regular RAM.
    "movl $_bss_start, %edi",
    "movl $_bss_end, %ecx",
    "subl %edi, %ecx",
    "shrl $2, %ecx", // count in dwords
    "xorl %eax, %eax",
    "rep",
    "stosl",
    // Set up identity-mapped page tables for long mode.
    //
    // Real XIP boards keep prebuilt page tables in ROM: CR3 only needs
    // the physical address and the CPU reads the descriptors during page
    // walks. QEMU is the exception: its board metadata supplies a writable
    // `page_table_addr`, enabling `x86-writable-page-tables`, so the
    // setup routine below builds the tables in low RAM.
    //
    // Do not rely on multiple `global_asm!` blocks being laid out in Rust
    // source order. Branch to a named setup routine; it branches back to
    // `_after_page_tables` when complete.
    // POST 0x23: page-table setup/selection begins.
    "movb $0x23, %al",
    "outb %al, $0x80",
    enter_runtime!(),
    options(att_syntax),
);

// Very-early Intel microcode update for BSP.
//
// Pre-CAR bootblock only: postcar/ramstage get microcode through the FFS
// blob (MP init), so this is compiled out of postcar to keep it small.
// Entry/return convention matches coreboot's no-stack helpers: `%esp` contains
// the absolute return address. The routine may clobber all general registers.
#[cfg(feature = "early-ffs-anchor")]
#[cfg(all(target_os = "none", not(fstart_stage_env = "postcar")))]
core::arch::global_asm!(
    ".section .text, \"ax\"",
    ".code32",
    ".global _early_intel_microcode",
    "_early_intel_microcode:",
    "cmpl $0, _fstart_early_microcode_enabled",
    "je 9f",
    "movl $_fstart_anchor_early, %esi",
    // Anchor layout (u32 offsets): anchor_offset=24, microcode_offset=28,
    // microcode_size=32. Reconstruct image base from link-time anchor addr.
    "movl 28(%esi), %eax",
    "testl %eax, %eax",
    "jz 9f",
    "movl 32(%esi), %ecx",
    "testl %ecx, %ecx",
    "jz 9f",
    "movl %esi, %edi",
    "subl 24(%esi), %edi",
    "addl %edi, %eax",
    "movl %eax, %esi", // ESI = first microcode record
    "addl %eax, %ecx",
    "movl %ecx, %edi", // EDI = end
    // Platform flags: 1 << ((IA32_PLATFORM_ID.hi >> 18) & 7).
    "movl $0x17, %ecx",
    "rdmsr",
    "shrl $18, %edx",
    "andl $7, %edx",
    "movl $1, %eax",
    "movl %edx, %ecx",
    "shll %cl, %eax",
    "movl %eax, %ebp", // EBP = platform flag mask
    // Current microcode revision -> EDX, CPUID(1).EAX signature -> EBX.
    "xorl %eax, %eax",
    "xorl %edx, %edx",
    "movl $0x8b, %ecx",
    "wrmsr",
    "movl $1, %eax",
    "cpuid",
    "movl %eax, %ebx",
    "movl $0x8b, %ecx",
    "rdmsr",
    "1:",
    // Stop if fewer than the 48-byte Intel header remain.
    "movl %edi, %eax",
    "subl %esi, %eax",
    "cmpl $48, %eax",
    "jb 9f",
    // Match processor signature and platform flags.
    "cmpl 12(%esi), %ebx",
    "jne 2f",
    "movl 24(%esi), %eax",
    "testl %ebp, %eax",
    "jz 2f",
    // Only load if update revision is newer than the currently installed one.
    "cmpl 4(%esi), %edx",
    "jge 9f",
    "leal 48(%esi), %eax",
    "xorl %edx, %edx",
    "movl $0x79, %ecx",
    "wrmsr",
    "jmp 9f",
    "2:",
    // Advance by total_size, or 2048 for old updates with total_size == 0.
    "movl 32(%esi), %eax",
    "testl %eax, %eax",
    "jnz 3f",
    "movl $2048, %eax",
    "3:",
    "addl %eax, %esi",
    "cmpl %esi, %edi",
    "ja 1b",
    "9:",
    "jmp *%esp",
    options(att_syntax),
);

#[cfg(not(feature = "early-ffs-anchor"))]
#[cfg(target_os = "none")]
core::arch::global_asm!(
    ".section .text, \"ax\"",
    ".code32",
    ".global _early_intel_microcode",
    "_early_intel_microcode:",
    "jmp *%esp",
    options(att_syntax),
);

// IDT table storage. It is ordinary writable BSS: bootblock XIP boards
// place it in CAR-backed BSS, and RAM stages place it in DRAM-backed BSS.
// The assembly labels are sufficient; the linker script does not need a
// dedicated IDT output section or linker-provided IDT symbols.
#[cfg(target_os = "none")]
core::arch::global_asm!(
    ".section .bss, \"aw\", @nobits",
    ".align 4096",
    ".global _idt_table",
    "_idt_table:",
    ".skip 4096",
);

#[cfg(feature = "x86-postcar-uart-debug")]
#[allow(unused_macros)]
macro_rules! postcar_uart_checkpoint {
    ($code:literal) => {
        concat!(
            "movw $0x3f8, %dx\n",
            "movb $",
            $code,
            ", %al\n",
            "outb %al, %dx\n",
        )
    };
}

#[cfg(not(feature = "x86-postcar-uart-debug"))]
#[allow(unused_macros)]
macro_rules! postcar_uart_checkpoint {
    ($code:literal) => {
        ""
    };
}

#[cfg(target_arch = "x86_64")]
mod long_mode;
#[cfg(target_arch = "x86")]
mod protected_mode;
#[cfg(target_arch = "x86_64")]
use long_mode as mode;
pub use mode::{jump_to, jump_to_with_handoff, protected_mode_call};
#[cfg(target_arch = "x86")]
use protected_mode as mode;

// Make sure the linker pulls in the entry code. This symbol is called from
// `global_asm!`, which Rust's dead-code analysis cannot see.
#[cfg(all(not(test), target_os = "none"))]
#[allow(dead_code)]
unsafe extern "Rust" {
    fn fstart_main(handoff_ptr: usize) -> !;
}

// Cache-as-RAM (NEM) setup has moved to car.rs.
// See car::car_setup() and car_teardown::_car_teardown.

// ---------------------------------------------------------------------------
// Exception handler — called from the IDT stub with register state
// ---------------------------------------------------------------------------

fn x86_raw_serial_byte(byte: u8) {
    unsafe { fstart_core::pio::outb(0x3F8, byte) };
}

fn x86_raw_serial_str(s: &str) {
    for byte in s.bytes() {
        if byte == b'\n' {
            x86_raw_serial_byte(b'\r');
        }
        x86_raw_serial_byte(byte);
    }
}

fn x86_raw_serial_hex(mut value: u64) {
    x86_raw_serial_str("0x");
    let mut buf = [0u8; 16];
    for idx in (0..16).rev() {
        let nibble = (value & 0xf) as u8;
        buf[idx] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        };
        value >>= 4;
    }
    for byte in buf {
        x86_raw_serial_byte(byte);
    }
}

/// Rust exception handler called from the assembly IDT stub.
///
/// First prints a minimal raw COM1 diagnostic that does not depend on the log
/// infrastructure, then prints the same data through fstart_log when possible.
///
/// # Arguments
/// - `vector`: exception vector number, or 255 for non-exception vectors
/// - `error_code`: x86 exception error code, or 0 for vectors without one
/// - `rip`: instruction pointer at the time of the exception
/// - `cs`: code segment selector at the time of the exception
/// - `rflags`: flags at the time of the exception
/// - `rsp`: approximate stack pointer at the time of the exception
/// - `cr2`: CR2 register (faulting address for page faults)
#[unsafe(no_mangle)]
pub extern "C" fn x86_exception_handler(
    vector: u64,
    error_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    cr2: u64,
) -> ! {
    x86_raw_serial_str("\n!X86 EXCEPTION vector=");
    x86_raw_serial_hex(vector);
    x86_raw_serial_str(" error=");
    x86_raw_serial_hex(error_code);
    x86_raw_serial_str(" rip=");
    x86_raw_serial_hex(rip);
    x86_raw_serial_str(" rsp=");
    x86_raw_serial_hex(rsp);
    x86_raw_serial_str(" cr2=");
    x86_raw_serial_hex(cr2);
    x86_raw_serial_str("!\n");

    fstart_log::error!(
        "*** x86 EXCEPTION vector={} error={:#x} ***",
        vector,
        error_code
    );
    fstart_log::error!("  RIP = {:#x} CS = {:#x} RFLAGS = {:#x}", rip, cs, rflags);
    fstart_log::error!("  RSP = {:#x}", rsp);
    fstart_log::error!("  CR2 = {:#x}", cr2);

    loop {
        unsafe { core::arch::asm!("hlt", options(nostack, nomem, preserves_flags)) };
    }
}

// ---------------------------------------------------------------------------
// Public API — consumed by selected stage code via fstart_platform:: alias
// ---------------------------------------------------------------------------

/// TSC value the reset vector saved in MM0/MM1.
///
/// Only meaningful in the bootblock, before anything else touches MMX
/// state. Encoded as raw bytes: firmware Rust is built without MMX.
#[cfg(target_os = "none")]
pub fn reset_tsc() -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: MOVD from MMX registers only reads them.
    unsafe {
        core::arch::asm!(
            ".byte 0x0f, 0x7e, 0xc0", // movd eax, mm0
            ".byte 0x0f, 0x7e, 0xca", // movd edx, mm1
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Halt the processor in a low-power wait state (never returns).
pub fn halt() -> ! {
    loop {
        // SAFETY: `hlt` puts the CPU in halt state until next interrupt.
        unsafe { core::arch::asm!("hlt", options(nostack, nomem, preserves_flags)) };
    }
}

#[inline]
fn read_cr0() -> u64 {
    // SAFETY: reading CR0 is side-effect free in firmware context.
    unsafe { crate::x86::controlregs::cr0().bits() as u64 }
}

#[inline]
fn read_cr3() -> u64 {
    // SAFETY: reading CR3 is side-effect free in firmware context.
    unsafe { crate::x86::controlregs::cr3() }
}

#[inline]
fn read_cr4() -> u64 {
    // SAFETY: reading CR4 is side-effect free in firmware context.
    unsafe { crate::x86::controlregs::cr4().bits() as u64 }
}

fn log_bsp_x86_cache_state(label: &str) {
    fstart_log::info!("BSP x86 cache/MTRR state: {}", label);
    fstart_log::info!(
        "  CR0={:#x} CR3={:#x} CR4={:#x}",
        read_cr0(),
        read_cr3(),
        read_cr4()
    );

    // SAFETY: called on x86_64 BSP immediately before payload handoff.
    unsafe {
        let cap = crate::x86::msr::rdmsr(mtrr::IA32_MTRR_CAP);
        let def_type = crate::x86::msr::rdmsr(mtrr::IA32_MTRR_DEF_TYPE);
        fstart_log::info!("  IA32_MTRR_CAP={:#x}", cap);
        fstart_log::info!("  IA32_MTRR_DEF_TYPE={:#x}", def_type);

        if mtrr::fixed_supported() {
            let fixed_msrs = [
                mtrr::IA32_MTRR_FIX64K_00000,
                mtrr::IA32_MTRR_FIX16K_80000,
                mtrr::IA32_MTRR_FIX16K_A0000,
                mtrr::IA32_MTRR_FIX4K_C0000,
                0x269,
                0x26a,
                0x26b,
                0x26c,
                0x26d,
                0x26e,
                0x26f,
            ];
            for msr in fixed_msrs {
                fstart_log::info!("  fixed MTRR {:#x}={:#x}", msr, crate::x86::msr::rdmsr(msr));
            }
        } else {
            fstart_log::info!("  fixed MTRRs unsupported");
        }

        let variable_count = mtrr::variable_count();
        for index in 0..variable_count {
            let (base, mask) = mtrr::read_variable(index);
            if mtrr::is_valid_mask(mask) {
                fstart_log::info!(
                    "  var MTRR{}: base_msr={:#x} mask_msr={:#x} base={:#x} size={:#x} type={:#x}",
                    index,
                    base,
                    mask,
                    mtrr::decode_base(base),
                    mtrr::decode_size(mask),
                    mtrr::decode_type(base),
                );
            } else {
                fstart_log::info!(
                    "  var MTRR{}: base_msr={:#x} mask_msr={:#x} disabled",
                    index,
                    base,
                    mask
                );
            }
        }
    }
}

/// Boot a Linux kernel using the x86 boot protocol.
///
/// Long-mode builds use the 64-bit entry at `code32_start + 0x200` (boot
/// protocol 2.12, `XLF_KERNEL_64`) and stay in long mode; protected-mode
/// builds use the 32-bit entry at `code32_start` itself.
///
/// Constructs the zero page (boot_params), fills e820 entries and ACPI
/// RSDP address, then jumps to the kernel.
///
/// # Arguments
/// - `kernel_addr`: physical address of the loaded kernel (typically `0x100000`)
/// - `rsdp_addr`: physical address of the ACPI RSDP (from AcpiLoad)
/// - `e820_entries`: slice of e820 memory map entries (from MemoryDetect)
/// - `bootargs`: kernel command line string
/// - `zero_page_addr`: physical address for boot_params (0x90000 on QEMU,
///   should be in e820-reported free conventional memory on real hardware)
/// Unified Linux boot entry point.
///
/// All fields are used: `kernel_addr`, `rsdp_addr`, `e820_entries`,
/// `bootargs`, `zero_page_addr`.
/// Ignored fields: `dtb_addr`, `fw_addr`, `hart_id`.
pub fn boot_linux(params: &fstart_core::services::boot::BootLinuxParams<'_>) -> ! {
    boot_linux_direct(
        params.kernel_addr,
        params.rsdp_addr,
        params.e820_entries,
        params.bootargs,
        params.zero_page_addr,
        params.print_x86_mtrrs,
    )
}

pub fn boot_linux_direct(
    kernel_addr: u64,
    rsdp_addr: u64,
    e820_entries: &[E820Entry],
    bootargs: &str,
    zero_page_addr: u64,
    print_x86_mtrrs: bool,
) -> ! {
    let zero_page = zero_page_addr;
    let cmd_line = zero_page + 0x1000; // command line follows zero page

    // SAFETY: zero_page_addr is in conventional memory (provided by
    // the board config), cleared by the entry code, and not used by
    // any other code at this point.
    let params = unsafe { &mut *(zero_page as *mut [u8; 4096]) };
    params.fill(0);

    // The loaded image is a raw bzImage: real-mode boot sector + setup
    // header, followed by the protected-mode (compressed) kernel.
    //
    // The boot_params struct mirrors the bzImage layout: the setup_header
    // lives at offset 0x1F1 within boot_params, exactly where it sits in
    // the on-disk image. We must copy the ENTIRE setup header (not just
    // the first 0x200 bytes) so the kernel can read critical fields like
    // kernel_alignment (0x230), init_size (0x260), xloadflags (0x236),
    // relocatable_kernel (0x234), etc.
    let bzimage = unsafe { core::slice::from_raw_parts(kernel_addr as *const u8, 0x80_0000) };

    // Read setup_sects (offset 0x1F1) to determine the size of the
    // real-mode portion of the bzImage.
    let image_params = LinuxBootParams::ref_from_bytes(&bzimage[..4096])
        .expect("bzImage setup header must fit in its first page");
    // Refuse anything that is not a bzImage new enough to carry the fields
    // used below (init_size and pref_address arrived with protocol 2.10).
    const SETUP_HEADER_MAGIC: u32 = u32::from_le_bytes(*b"HdrS");
    const MIN_BOOT_PROTOCOL: u16 = 0x020a;
    if image_params.header.get() != SETUP_HEADER_MAGIC
        || image_params.version.get() < MIN_BOOT_PROTOCOL
    {
        fstart_log::error!(
            "linux: payload is not a bzImage with boot protocol >= {:#x} (magic {:#x}, version {:#x})",
            MIN_BOOT_PROTOCOL,
            image_params.header.get(),
            image_params.version.get(),
        );
        halt();
    }
    // A zero setup_sects field means the historical default of 4 sectors.
    let setup_sects = match image_params.setup_sects {
        0 => 4,
        n => u64::from(n),
    };
    let setup_size = (setup_sects + 1) * 512; // bytes of real-mode code + header
    let pm_kernel_offset = setup_size;

    // Copy the entire real-mode setup area into boot_params.
    // The setup area can be up to ~60 sectors (30 KiB) but boot_params
    // is only 4 KiB. The setup_header fields that the kernel reads are
    // all within the first 0x290 bytes (end of the header area, before
    // the e820 table at 0x2D0). Copy up to 0x290 bytes from the bzImage
    // to ensure ALL setup header fields are present.
    let copy_end = (setup_size as usize).min(0x290).min(bzimage.len());
    params[..copy_end].copy_from_slice(&bzimage[..copy_end]);

    // View the copied setup header and the cleared remainder as one typed
    // zero-page. All fields are byte-aligned little-endian wire values.
    let params = LinuxBootParams::mut_from_bytes(&mut params[..])
        .expect("boot_params page has exactly the Linux wire size");
    params.type_of_loader = 0xFF; // unregistered
    params.loadflags |= 0x01 | 0x80; // LOADED_HIGH | CAN_USE_HEAP
    params.heap_end_ptr.set(0xFE00);

    // e820 map: count at 0x1E8, entries at 0x2D0 (20 bytes each).
    // Real-hardware boards may not have a MemoryDetect provider yet; provide
    // a conservative fallback map so Linux can choose a decompression area.
    let fallback = [
        E820Entry::new(
            0x0000_0000,
            0x0009_f000,
            fstart_core::services::memory_detect::E820Kind::Ram,
        ),
        E820Entry::new(
            0x0009_f000,
            0x0000_1000,
            fstart_core::services::memory_detect::E820Kind::Reserved,
        ),
        E820Entry::new(
            0x000f_0000,
            0x0001_0000,
            fstart_core::services::memory_detect::E820Kind::Reserved,
        ),
        E820Entry::new(
            0x0010_0000,
            0x3e50_0000,
            fstart_core::services::memory_detect::E820Kind::Ram,
        ),
        E820Entry::new(
            0x3e60_0000,
            0x01a0_0000,
            fstart_core::services::memory_detect::E820Kind::Reserved,
        ),
    ];
    let e820 = if e820_entries.is_empty() {
        &fallback[..]
    } else {
        e820_entries
    };
    // Move the protected-mode kernel to where it will run. Linux prefers
    // `pref_address` (16 MiB), but only memory the e820 map hands over as RAM
    // is the kernel's: firmware keeps reserved windows there and rewrites them
    // on S3 resume. A relocatable kernel runs at any `kernel_alignment`-aligned
    // base, so take the lowest one whose `init_size` footprint is all RAM.
    let pref_address = image_params.pref_address.get();
    let pm_kernel_src = kernel_addr + pm_kernel_offset;
    // syssize (offset 0x1F4): protected-mode code size in 16-byte paragraphs.
    let syssize = image_params.syssize.get() as u64 * 16;
    let footprint = u64::from(image_params.init_size.get()).max(syssize);
    let Some(pm_kernel_addr) = linux_boot_params::place_kernel(
        e820,
        pref_address,
        u64::from(image_params.kernel_alignment.get()),
        footprint,
        image_params.relocatable_kernel != 0,
    ) else {
        fstart_log::error!(
            "linux: no e820 RAM for the kernel ({:#x} bytes from {:#x})",
            footprint,
            pref_address
        );
        halt();
    };
    if pm_kernel_addr != pm_kernel_src {
        // SAFETY: the destination is e820 RAM inside the identity map and
        // nothing else lives there yet; `copy` tolerates overlap with the
        // loaded bzImage.
        unsafe {
            core::ptr::copy(
                pm_kernel_src as *const u8,
                pm_kernel_addr as *mut u8,
                syssize as usize,
            );
        }
    }

    // code32_start (offset 0x214): tell the kernel where the PM code is.
    params.code32_start.set(pm_kernel_addr as u32);

    // vid_mode (offset 0x1FA) — 0xFFFF = "normal" (no video mode change)
    params.vid_mode.set(0xFFFF);

    // cmd_line_ptr (offset 0x228)
    let cmdline = unsafe { &mut *(cmd_line as *mut [u8; 4096]) };
    let args_bytes = bootargs.as_bytes();
    let copy_len = args_bytes.len().min(4095); // leave room for NUL
    cmdline[..copy_len].copy_from_slice(&args_bytes[..copy_len]);
    cmdline[copy_len] = 0; // NUL terminator
    params.cmd_line_ptr.set(cmd_line as u32);

    // ACPI RSDP address (offset 0x070, protocol 2.14+)
    params.acpi_rsdp_addr.set(rsdp_addr);

    assert!(
        e820.len() <= E820_CAPACITY,
        "e820 map exceeds Linux boot_params capacity"
    );
    params.e820_count = e820.len() as u8;
    for (wire, entry) in params.e820_table.iter_mut().zip(e820) {
        // SAFETY: E820Entry is packed; copy its scalar fields by value.
        let addr = unsafe { core::ptr::addr_of!(entry.addr).read_unaligned() };
        let size = unsafe { core::ptr::addr_of!(entry.size).read_unaligned() };
        let kind = unsafe { core::ptr::addr_of!(entry.kind).read_unaligned() };
        *wire = LinuxE820Entry {
            addr: U64::<LE>::new(addr),
            size: U64::<LE>::new(size),
            kind: U32::<LE>::new(kind),
        };
    }

    fstart_log::info!("  setup_sects: {}", setup_sects);
    fstart_log::info!(
        "  pm_kernel @ {:#x} (syssize {:#x})",
        pm_kernel_addr,
        syssize
    );
    fstart_log::info!("  pref_address: {:#x}", pref_address);
    fstart_log::info!("  code32_start @ {:#x}", pm_kernel_addr);
    fstart_log::info!("  zero_page @ {:#x}", zero_page);
    fstart_log::info!("  rsdp @ {:#x}", rsdp_addr);
    fstart_log::info!("  e820 count: {}", e820.len());
    for entry in e820 {
        // SAFETY: E820Entry is packed for ABI compatibility.
        let addr = unsafe { core::ptr::addr_of!(entry.addr).read_unaligned() };
        let size = unsafe { core::ptr::addr_of!(entry.size).read_unaligned() };
        let kind = unsafe { core::ptr::addr_of!(entry.kind).read_unaligned() };
        let kind_name = match kind {
            1 => "RAM",
            2 => "Reserved",
            3 => "ACPI Reclaim",
            4 => "ACPI NVS",
            5 => "Unusable",
            _ => "Unknown",
        };
        fstart_log::info!(
            "  e820: base={:#x} size={:#x} kind={} ({})",
            addr,
            size,
            kind,
            kind_name
        );
    }

    // Log critical setup header fields for debugging.
    let init_size = params.init_size.get();
    let kernel_alignment = params.kernel_alignment.get();
    let xloadflags = params.xloadflags.get();
    fstart_log::info!("  init_size: {:#x}", init_size);
    fstart_log::info!("  kernel_alignment: {:#x}", kernel_alignment);
    fstart_log::info!("  xloadflags: {:#x}", xloadflags);

    if print_x86_mtrrs {
        log_bsp_x86_cache_state("before ROM WP clear");
    }

    // The temporary BSP-only ROM WP MTRR must not leak to Linux: APs do not
    // carry it, and OSes require coherent MTRR state across CPUs.
    disable_boot_media_rom_cache_for_handoff();

    if print_x86_mtrrs {
        log_bsp_x86_cache_state("before Linux jump");
    }

    log_x86_irq_handoff_state();

    mode::enter_linux(pm_kernel_addr, zero_page)
}

fn log_x86_irq_handoff_state() {}
