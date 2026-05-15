//! x86_64 platform support for fstart.
//!
//! Provides the entry point (reset vector through 16-bit real mode,
//! 32-bit protected mode, to 64-bit long mode), GDT setup, identity-mapped
//! page tables, and the Linux boot protocol handoff.
//!
//! # Entry Flow
//!
//! 1. Reset vector at `0xFFFFFFF0`: `jmp _start16bit`
//! 2. 16-bit real mode: load GDT, enable protected mode
//! 3. 32-bit protected mode: set up page tables, enable long mode
//! 4. 64-bit long mode: set stack, clear BSS, call `fstart_main(0)`
//!
//! # QEMU Q35
//!
//! On QEMU Q35, RAM works immediately without Cache-as-RAM setup.
//! MTRRs are no-ops. The firmware runs XIP from pflash mapped at the
//! top of the 4 GiB address space.

#![no_std]

pub mod car;
pub mod car_teardown;
pub mod cpuid;

use fstart_arch_x86::mtrr;
use fstart_services::memory_detect::E820Entry;

/// Enable the BSP-local ROM cacheability MTRR for memory-mapped boot media.
///
/// MP setup installs identical RAM MTRRs on all CPUs, so the temporary ROM WP
/// MTRR must be installed after MP setup and only on the BSP when firmware is
/// about to read memory-mapped flash. It is cleared before Linux handoff.
pub fn enable_boot_media_rom_cache() {
    fstart_log::info!("mtrr: enabling temporary BSP ROM WP for memory-mapped boot media");
    // SAFETY: generated stage code calls this on the BSP immediately before
    // memory-mapped boot-media reads. The MTRR is cleared before OS handoff.
    unsafe { mtrr::set_boot_rom_wp(true) };
}

// ---------------------------------------------------------------------------
// Entry point — 16-bit real mode → 32-bit protected mode → 64-bit long mode
// ---------------------------------------------------------------------------

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
    // walks. QEMU is the exception: its board RON supplies a writable
    // `page_table_addr`, enabling `x86-writable-page-tables`, so the
    // setup routine below builds the tables in low RAM.
    //
    // Do not rely on multiple `global_asm!` blocks being laid out in Rust
    // source order. Branch to a named setup routine; it branches back to
    // `_after_page_tables` when complete.
    // POST 0x23: page-table setup/selection begins.
    "movb $0x23, %al",
    "outb %al, $0x80",
    "jmp _setup_page_tables",
    options(att_syntax),
);

// Writable page-table setup for QEMU-style low-RAM page tables.
// 1 GiB pages: PDPT[0..511] = 512 x 1 GiB identity-mapped pages.
// Requires PDPE1GB. Covers 512 GiB. Compact: only 2 pages total.
#[cfg(all(feature = "x86-writable-page-tables", feature = "x86-1g-pages"))]
core::arch::global_asm!(
    ".section .text, \"ax\"",
    ".code32",
    ".global _setup_page_tables",
    "_setup_page_tables:",
    // Clear page table area first.
    "movl $_page_tables_start, %edi",
    "movl $_page_tables_end, %ecx",
    "subl %edi, %ecx",
    "shrl $2, %ecx",
    "xorl %eax, %eax",
    "rep stosl",
    // PML4[0] = address of PDPT | User | Accessed | Present | Writable.
    "movl $_page_tables_start, %edi",
    "leal 0x1027(%edi), %eax",
    "movl %eax, (%edi)",
    "leal 0x1000(%edi), %esi", // ESI = PDPT base
    "xorl %edx, %edx",         // EDX = high 32 bits of PA (starts at 0)
    "xorl %eax, %eax",         // EAX = low 32 bits of PA (starts at 0)
    "orl $0xe7, %eax",         // PS=1, D=1, A=1, US=1, RW=1, P=1
    "movl $512, %ecx",         // 512 entries x 1 GiB = 512 GiB
    "1:",
    "movl %eax, (%esi)",
    "movl %edx, 4(%esi)",
    "addl $0x40000000, %eax",
    "adcl $0, %edx",
    "addl $8, %esi",
    "decl %ecx",
    "jnz 1b",
    "jmp _after_page_tables",
    options(att_syntax),
);

// Writable page-table setup for QEMU-style low-RAM page tables.
// 2 MiB pages (default): PDPT[0..3] -> PD0..PD3, each PD 512 x 2 MiB.
#[cfg(all(feature = "x86-writable-page-tables", not(feature = "x86-1g-pages")))]
core::arch::global_asm!(
    ".section .text, \"ax\"",
    ".code32",
    ".global _setup_page_tables",
    "_setup_page_tables:",
    // Clear page table area first.
    "movl $_page_tables_start, %edi",
    "movl $_page_tables_end, %ecx",
    "subl %edi, %ecx",
    "shrl $2, %ecx",
    "xorl %eax, %eax",
    "rep stosl",
    "movl $_page_tables_start, %edi",
    // PML4[0] = address of PDPT | User | Accessed | Present | Writable.
    "leal 0x1027(%edi), %eax",
    "movl %eax, (%edi)",
    // PDPT[0..3] = address of PD0..PD3 | User | Accessed | Present | Writable.
    "leal 0x1000(%edi), %esi",
    "leal 0x2027(%edi), %eax",
    "movl %eax, 0(%esi)",
    "addl $0x1000, %eax",
    "movl %eax, 8(%esi)",
    "addl $0x1000, %eax",
    "movl %eax, 16(%esi)",
    "addl $0x1000, %eax",
    "movl %eax, 24(%esi)",
    // Fill PD0..PD3 with 2048 x 2 MiB identity-mapped pages.
    "leal 0x2000(%edi), %esi",
    "xorl %edx, %edx",
    "xorl %eax, %eax",
    "orl $0xe7, %eax",
    "movl $2048, %ecx",
    "2:",
    "movl %eax, (%esi)",
    "movl %edx, 4(%esi)",
    "addl $0x200000, %eax",
    "adcl $0, %edx",
    "addl $8, %esi",
    "decl %ecx",
    "jnz 2b",
    "jmp _after_page_tables",
    options(att_syntax),
);

// Page table storage is defined by this assembly, not by the linker script.
//
// - Real XIP boards emit prebuilt page tables into ordinary `.rodata` and
//   load CR3 from the assembly-defined `PML4E`, matching coreboot's
//   `setup_longmode $PML4E` convention.
// - QEMU/writable-PT boards reserve BSS storage here; `_setup_page_tables`
//   fills it at runtime.
#[cfg(not(feature = "x86-writable-page-tables"))]
core::arch::global_asm!(
    ".section .text, \"ax\"",
    ".code32",
    ".global _setup_page_tables",
    "_setup_page_tables:",
    "jmp _after_page_tables",
    options(att_syntax),
);

#[cfg(all(feature = "x86-static-page-tables", feature = "x86-1g-pages"))]
core::arch::global_asm!(
    ".section .rodata, \"a\"",
    ".balign 4096",
    ".global PML4E",
    ".global _page_tables_start",
    ".global _page_tables_end",
    "PML4E:",
    "_page_tables_start:",
    ".quad .Lpt_pdpt_1g + 0x027",
    ".zero 4096 - 8",
    ".Lpt_pdpt_1g:",
    ".set .Lpt_addr_1g, 0",
    ".rept 512",
    ".quad .Lpt_addr_1g + 0x0e7",
    ".set .Lpt_addr_1g, .Lpt_addr_1g + 0x40000000",
    ".endr",
    "_page_tables_end:",
    options(att_syntax),
);

#[cfg(all(feature = "x86-writable-page-tables", feature = "x86-1g-pages"))]
core::arch::global_asm!(
    ".section .bss, \"aw\", @nobits",
    ".balign 4096",
    ".global PML4E",
    ".global _page_tables_start",
    ".global _page_tables_end",
    "PML4E:",
    "_page_tables_start:",
    ".skip 0x2000",
    "_page_tables_end:",
    options(att_syntax),
);

#[cfg(all(feature = "x86-writable-page-tables", not(feature = "x86-1g-pages")))]
core::arch::global_asm!(
    ".section .bss, \"aw\", @nobits",
    ".balign 4096",
    ".global PML4E",
    ".global _page_tables_start",
    ".global _page_tables_end",
    "PML4E:",
    "_page_tables_start:",
    ".skip 0x6000",
    "_page_tables_end:",
    options(att_syntax),
);

#[cfg(not(any(
    feature = "x86-static-page-tables",
    feature = "x86-writable-page-tables"
)))]
core::arch::global_asm!(
    ".section .bss, \"aw\", @nobits",
    ".balign 4096",
    ".global PML4E",
    ".global _page_tables_start",
    ".global _page_tables_end",
    "PML4E:",
    "_page_tables_start:",
    "_page_tables_end:",
    options(att_syntax),
);

#[cfg(all(feature = "x86-static-page-tables", not(feature = "x86-1g-pages")))]
core::arch::global_asm!(
    ".section .rodata, \"a\"",
    ".balign 4096",
    ".global PML4E",
    ".global _page_tables_start",
    ".global _page_tables_end",
    "PML4E:",
    "_page_tables_start:",
    ".quad .Lpt_pdpt_2m + 0x027",
    ".zero 4096 - 8",
    // Match coreboot's ROM PT order: PML4E, PDT, then PDPT.
    ".Lpt_pdt_2m:",
    ".set .Lpt_addr_2m, 0",
    ".rept 2048",
    ".quad .Lpt_addr_2m + 0x0e7",
    ".set .Lpt_addr_2m, .Lpt_addr_2m + 0x200000",
    ".endr",
    ".balign 4096",
    ".Lpt_pdpt_2m:",
    ".quad .Lpt_pdt_2m + 0x027",
    ".quad .Lpt_pdt_2m + 0x1000 + 0x027",
    ".quad .Lpt_pdt_2m + 0x2000 + 0x027",
    ".quad .Lpt_pdt_2m + 0x3000 + 0x027",
    ".zero 4096 - 32",
    "_page_tables_end:",
    options(att_syntax),
);

// Continue the entry sequence after page table setup.
core::arch::global_asm!(
    ".section .text, \"ax\"",
    ".code32",
    ".global _after_page_tables",
    "_after_page_tables:",
    // Match coreboot's `setup_longmode $PML4E`: load CR3 from the
    // assembler-defined PML4E label.
    "movl $PML4E, %eax",
    "movl %eax, %cr3",
    "movl %cr4, %eax",
    "btsl $5, %eax", // CR4.PAE
    "movl %eax, %cr4",
    // Enable long mode:
    //   IA32_EFER.LME (bit 8) — Long Mode Enable
    // Keep NXE disabled here to match coreboot's minimal pre-IDT path;
    // platforms that need NX can enable it later after CPUID checks.
    "movl $0xC0000080, %ecx", // IA32_EFER MSR
    "rdmsr",
    "btsl $8, %eax", // LME
    "wrmsr",
    // Enable paging. Match coreboot's setup_longmode path: set only PG here,
    // then immediately far jump to reload CS with the 64-bit code selector.
    "movl %cr0, %eax",
    "btsl $31, %eax", // CR0.PG
    "movl %eax, %cr0",
    // Far jump to 64-bit code segment (0x18 = coreboot GDT_CODE_SEG64)
    ".byte 0xea",        // ljmpl opcode
    ".long _start64bit", // 32-bit offset
    ".word 0x18",        // 64-bit code segment selector
    // =====================================================================
    // 64-bit long mode entry
    // =====================================================================
    ".code64",
    ".global _start64bit",
    "_start64bit:",
    // Enable OS support for FXSAVE/FXRSTOR after long-mode entry.
    "movq %cr4, %rax",
    "btsq $9, %rax", // CR4.OSFXSR
    "movq %rax, %cr4",
    // POST 0x31: CR4.OSFXSR written.
    "movb $0x31, %al",
    "outb %al, $0x80",
    // Reload data segments with the shared flat data selector (0x10).
    "movw $0x10, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "xorw %ax, %ax",
    "movw %ax, %fs",
    "movw %ax, %gs",
    // Set up stack (grows down from _stack_top)
    "movabs $_stack_top, %rsp",
    // Copy .data initializers from ROM (LMA) to RAM (VMA).
    // If src == dst (RAM-only build), the copy is a harmless no-op.
    // Uses byte copy (rep movsb) instead of qword copy to handle
    // .ldata sections that may be < 8 bytes (e.g., a single u8).
    "movabs $_data_load, %rsi",
    "movabs $_data_start, %rdi",
    "movabs $_data_end, %rcx",
    "subq %rdi, %rcx",
    "cmpq %rsi, %rdi",
    "je 2f",
    "rep movsb",
    "2:",
    // Set up IDT with exception handlers before calling Rust code.
    // Without an IDT, any exception causes a triple fault + silent reset.
    "call _setup_idt",
    // POST 0x33: long-mode runtime setup complete; entering Rust.
    "movb $0x33, %al",
    "outb %al, $0x80",
    // Call fstart_main(handoff_ptr=0)
    "xorl %edi, %edi",
    "call fstart_main",
    // Should never return — halt
    "3:",
    "hlt",
    "jmp 3b",
    // =====================================================================
    // IDT setup — builds a 256-entry IDT pointing to _exc_stub.
    // =====================================================================
    "_setup_idt:",
    "pushq %rax",
    "pushq %rbx",
    "pushq %rcx",
    "pushq %rdi",
    // Zero the IDT area
    "xorl %eax, %eax",
    "movabs $_idt_table, %rdi",
    "movl $512, %ecx",
    "rep stosq",
    // Fill all 256 entries → _exc_stub
    "movabs $_exc_stub, %rbx",
    "movabs $_idt_table, %rdi",
    "movl $256, %ecx",
    "4:",
    "movw %bx, (%rdi)",
    "movw $0x18, 2(%rdi)", // CS = 0x18 (64-bit code)
    "movb $0, 4(%rdi)",
    "movb $0x8E, 5(%rdi)",
    "movl %ebx, %eax",
    "shrl $16, %eax",
    "movw %ax, 6(%rdi)",
    "movq %rbx, %rax",
    "shrq $32, %rax",
    "movl %eax, 8(%rdi)",
    "movl $0, 12(%rdi)",
    "addq $16, %rdi",
    "decl %ecx",
    "jnz 4b",
    // Load the IDT
    "movabs $_idt_desc, %rax",
    "lidt (%rax)",
    "popq %rdi",
    "popq %rcx",
    "popq %rbx",
    "popq %rax",
    "ret",
    ".align 16",
    "_idt_desc:",
    ".word 256 * 16 - 1",
    ".quad _idt_table",
    // =====================================================================
    // Exception stub — saves frame and calls the Rust exception handler.
    //
    // The CPU pushes SS, RSP, RFLAGS, CS, RIP (and error code for some
    // vectors). Since we use a single stub for all vectors, we can't
    // distinguish error-code vs no-error-code exceptions by vector.
    // The Rust handler reads CR2 directly for page faults.
    //
    // We pass RIP, RSP, and CR2 as arguments to the Rust handler:
    //   rdi = RIP (from exception frame)
    //   rsi = RSP at exception time
    //   rdx = CR2 (page fault address)
    // =====================================================================
    ".align 16",
    "_exc_stub:",
    // POST 0xe0: CPU exception reached the IDT stub.
    "movb $0xe0, %al",
    "outb %al, $0x80",
    "movq (%rsp), %rdi",   // rdi = faulting RIP
    "movq 24(%rsp), %rsi", // rsi = pre-exception RSP
    "movq %cr2, %rdx",     // rdx = CR2 (page fault address)
    "call x86_exception_handler",
    // Should not return, but halt if it does
    "5:",
    "hlt",
    "jmp 5b",
    options(att_syntax),
);

// IDT table storage. It is ordinary writable BSS: bootblock XIP boards
// place it in CAR-backed BSS, and RAM stages place it in DRAM-backed BSS.
// The assembly labels are sufficient; the linker script does not need a
// dedicated IDT output section or linker-provided IDT symbols.
core::arch::global_asm!(
    ".section .bss, \"aw\", @nobits",
    ".align 4096",
    ".global _idt_table",
    "_idt_table:",
    ".skip 4096",
);

// ---------------------------------------------------------------------------
// RAM-stage entry (64-bit only — no 16-bit/32-bit transition)
// ---------------------------------------------------------------------------

// Entry point for non-first x86_64 stages that run from RAM.
//
// The bootblock already transitioned to 64-bit long mode with identity-
// mapped page tables. This entry zeros BSS, copies .data initializers
// (harmless no-op when src == dst), sets up the IDT and stack, then
// calls `fstart_main(0)`.
//
// Placed in `.text.entry` so `KEEP(*(.text.entry))` in the linker script
// ensures it's at the start of the binary (= the load address that the
// bootblock's `jump_to()` targets).
core::arch::global_asm!(
    ".section .text.entry, \"ax\"",
    ".code64",
    ".global _start_ram",
    "_start_ram:",
    // POST 0x40: RAM-stage 64-bit entry reached.
    "movb $0x40, %al",
    "outb %al, $0x80",
    // Set up the DRAM-backed stack first.  Non-first x86 stages are
    // entered after DRAM training; if the previous stage used CAR, the
    // teardown routine needs a real stack before it disables NEM/MTRR0.
    "movabs $_stack_top, %rsp",
    // Tear down Cache-as-RAM before touching BSS/data in the RAM stage.
    // `_has_car` is a linker-provided absolute symbol (0 or 1).
    "movl $_has_car, %eax",
    "testl %eax, %eax",
    "je 0f",
    "call _car_teardown",
    "0:",
    // Zero BSS (64-bit mode)
    "movabs $_bss_start, %rdi",
    "movabs $_bss_end, %rcx",
    "subq %rdi, %rcx",
    "shrq $3, %rcx", // count in qwords
    "xorl %eax, %eax",
    "rep stosq",
    // Copy .data initializers (skip if src == dst, i.e., RAM-only)
    "movabs $_data_load, %rsi",
    "movabs $_data_start, %rdi",
    "movabs $_data_end, %rcx",
    "subq %rdi, %rcx",
    "cmpq %rsi, %rdi",
    "je 1f",
    "rep movsb",
    "1:",
    // Set up IDT
    "call _setup_idt",
    // POST 0x43: RAM-stage runtime setup complete; entering Rust.
    "movb $0x43, %al",
    "outb %al, $0x80",
    // Call fstart_main(handoff_ptr=0)
    "xorl %edi, %edi",
    "call fstart_main",
    // Should never return
    "2:",
    "hlt",
    "jmp 2b",
    options(att_syntax),
);

// Make sure the linker pulls in the entry code. This symbol is called from
// `global_asm!`, which Rust's dead-code analysis cannot see.
#[allow(dead_code)]
extern "Rust" {
    fn fstart_main(handoff_ptr: usize) -> !;
}

// Cache-as-RAM (NEM) setup has moved to car.rs.
// See car::car_setup() and car::car_teardown().

// ---------------------------------------------------------------------------
// Exception handler — called from the IDT stub with register state
// ---------------------------------------------------------------------------

/// Rust exception handler called from the assembly IDT stub.
///
/// Prints exception details using fstart_log (which uses the already-
/// initialized NS16550 UART), then halts. This gives useful diagnostics
/// instead of a silent triple-fault reset.
///
/// # Arguments
/// - `rip`: instruction pointer at the time of the exception
/// - `rsp`: stack pointer at the time of the exception
/// - `cr2`: CR2 register (faulting address for page faults)
#[no_mangle]
pub extern "C" fn x86_exception_handler(rip: u64, rsp: u64, cr2: u64) -> ! {
    fstart_log::error!("*** x86 EXCEPTION ***");
    fstart_log::error!("  RIP = {:#x}", rip);
    fstart_log::error!("  RSP = {:#x}", rsp);
    fstart_log::error!("  CR2 = {:#x}", cr2);

    // Also print via raw PIO in case the log infrastructure is broken
    // (e.g., exception during console init).
    unsafe {
        let msg = b"\r\n!EXCEPTION HALT!\r\n";
        for &b in msg {
            fstart_pio::outb(0x3F8, b);
        }
    }

    loop {
        unsafe { core::arch::asm!("hlt", options(nostack, nomem, preserves_flags)) };
    }
}

// ---------------------------------------------------------------------------
// Public API — consumed by generated stage code via fstart_platform:: alias
// ---------------------------------------------------------------------------

/// Halt the processor in a low-power wait state (never returns).
pub fn halt() -> ! {
    loop {
        // SAFETY: `hlt` puts the CPU in halt state until next interrupt.
        unsafe { core::arch::asm!("hlt", options(nostack, nomem, preserves_flags)) };
    }
}

/// Jump to an absolute 64-bit address (never returns).
///
/// Used for generic payload/stage jumps.
pub fn jump_to(addr: u64) -> ! {
    // SAFETY: caller guarantees `addr` points to valid executable code.
    unsafe {
        core::arch::asm!(
            "jmp {0}",
            in(reg) addr,
            options(noreturn),
        );
    }
}

#[inline]
fn read_cr0() -> u64 {
    let value: u64;
    // SAFETY: reading CR0 is side-effect free in firmware context.
    unsafe { core::arch::asm!("mov {}, cr0", out(reg) value, options(nomem, nostack)) };
    value
}

#[inline]
fn read_cr3() -> u64 {
    let value: u64;
    // SAFETY: reading CR3 is side-effect free in firmware context.
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) value, options(nomem, nostack)) };
    value
}

#[inline]
fn read_cr4() -> u64 {
    let value: u64;
    // SAFETY: reading CR4 is side-effect free in firmware context.
    unsafe { core::arch::asm!("mov {}, cr4", out(reg) value, options(nomem, nostack)) };
    value
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
        let cap = fstart_arch_x86::msr::rdmsr(mtrr::IA32_MTRR_CAP);
        let def_type = fstart_arch_x86::msr::rdmsr(mtrr::IA32_MTRR_DEF_TYPE);
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
                fstart_log::info!(
                    "  fixed MTRR {:#x}={:#x}",
                    msr,
                    fstart_arch_x86::msr::rdmsr(msr)
                );
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

/// Boot a Linux kernel using the x86 64-bit boot protocol.
///
/// Uses the 64-bit entry point at `code32_start + 0x200` (available since
/// boot protocol 2.12 when `XLF_KERNEL_64` is set in `xload_flags`).
/// This avoids the complex long-mode -> protected-mode teardown needed
/// by the 32-bit protocol: we stay in long mode, set `%rsi` to the
/// zero page, and jump directly to the kernel's `startup_64`.
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
pub fn boot_linux(params: &fstart_services::boot::BootLinuxParams<'_>) -> ! {
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
    let setup_sects = bzimage[0x1F1] as u64;
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

    // Override fields that the bootloader must set.
    params[0x210] = 0xFF; // type_of_loader = unregistered
    params[0x211] |= 0x01; // loadflags |= LOADED_HIGH

    // heap_end_ptr (offset 0x224): end of the setup heap relative to
    // the start of the real-mode code. Set to end of boot_params.
    // This is loadflags.CAN_USE_HEAP dependent; set it defensively.
    params[0x211] |= 0x80; // loadflags |= CAN_USE_HEAP
    params[0x224..0x226].copy_from_slice(&0xFE00u16.to_le_bytes());

    // Relocate the protected-mode kernel to pref_address.
    //
    // pref_address (offset 0x258) is where the kernel prefers to be
    // loaded, typically 0x1000000, aligned to kernel_alignment (2 MiB).
    // The PM kernel sits at kernel_addr + pm_kernel_offset within the
    // loaded bzImage. Move it to pref_address so the decompressor's
    // alignment requirements are satisfied.
    //
    // startup_64 uses RIP-relative addressing (leaq startup_32(%rip))
    // to discover its own address. The PM kernel must be at an aligned
    // address so the kernel's relocation calculation works correctly.
    let pref_address = u64::from_le_bytes(bzimage[0x258..0x260].try_into().unwrap_or([0; 8]));
    let pm_kernel_src = kernel_addr + pm_kernel_offset;

    // syssize (offset 0x1F4): protected-mode code size in 16-byte
    // paragraphs. This is the exact amount to copy.
    let syssize =
        u32::from_le_bytes(bzimage[0x1F4..0x1F8].try_into().unwrap_or([0; 4])) as u64 * 16;
    let copy_len = syssize;

    let pm_kernel_addr = if pref_address != 0 && pref_address != pm_kernel_src {
        // SAFETY: pref_address (0x1000000) is below pm_kernel_src
        // (kernel_load_addr + setup_size), so a forward copy is safe
        // (no overlap corruption). Both addresses are in identity-mapped
        // RAM covered by our page tables.
        unsafe {
            core::ptr::copy(
                pm_kernel_src as *const u8,
                pref_address as *mut u8,
                copy_len as usize,
            );
        }
        pref_address
    } else {
        pm_kernel_src
    };

    // code32_start (offset 0x214): tell the kernel where the PM code is.
    params[0x214..0x218].copy_from_slice(&(pm_kernel_addr as u32).to_le_bytes());

    // vid_mode (offset 0x1FA) — 0xFFFF = "normal" (no video mode change)
    params[0x1FA..0x1FC].copy_from_slice(&0xFFFFu16.to_le_bytes());

    // cmd_line_ptr (offset 0x228)
    let cmdline = unsafe { &mut *(cmd_line as *mut [u8; 4096]) };
    let args_bytes = bootargs.as_bytes();
    let copy_len = args_bytes.len().min(4095); // leave room for NUL
    cmdline[..copy_len].copy_from_slice(&args_bytes[..copy_len]);
    cmdline[copy_len] = 0; // NUL terminator
    params[0x228..0x22C].copy_from_slice(&(cmd_line as u32).to_le_bytes());

    // ACPI RSDP address (offset 0x070, protocol 2.14+)
    params[0x070..0x078].copy_from_slice(&rsdp_addr.to_le_bytes());

    // e820 map: count at 0x1E8, entries at 0x2D0 (20 bytes each).
    // Real-hardware boards may not have a MemoryDetect provider yet; provide
    // a conservative fallback map so Linux can choose a decompression area.
    let fallback = [
        E820Entry::new(
            0x0000_0000,
            0x0009_f000,
            fstart_services::memory_detect::E820Kind::Ram,
        ),
        E820Entry::new(
            0x0009_f000,
            0x0000_1000,
            fstart_services::memory_detect::E820Kind::Reserved,
        ),
        E820Entry::new(
            0x000f_0000,
            0x0001_0000,
            fstart_services::memory_detect::E820Kind::Reserved,
        ),
        E820Entry::new(
            0x0010_0000,
            0x3e50_0000,
            fstart_services::memory_detect::E820Kind::Ram,
        ),
        E820Entry::new(
            0x3e60_0000,
            0x01a0_0000,
            fstart_services::memory_detect::E820Kind::Reserved,
        ),
    ];
    let e820 = if e820_entries.is_empty() {
        &fallback[..]
    } else {
        e820_entries
    };
    let count = e820.len().min(128) as u8;
    params[0x1E8] = count;
    for (i, entry) in e820.iter().take(128).enumerate() {
        // SAFETY: E820Entry is #[repr(C, packed)] to match Linux's ABI.
        // Read fields unaligned before copying/logging them.
        let addr = unsafe { core::ptr::addr_of!(entry.addr).read_unaligned() };
        let size = unsafe { core::ptr::addr_of!(entry.size).read_unaligned() };
        let kind = unsafe { core::ptr::addr_of!(entry.kind).read_unaligned() };
        let offset = 0x2D0 + i * 20;
        params[offset..offset + 8].copy_from_slice(&addr.to_le_bytes());
        params[offset + 8..offset + 16].copy_from_slice(&size.to_le_bytes());
        params[offset + 16..offset + 20].copy_from_slice(&kind.to_le_bytes());
    }

    // 64-bit entry = protected-mode kernel base + 0x200
    let entry64 = pm_kernel_addr + 0x200;

    fstart_log::info!("  setup_sects: {}", setup_sects);
    fstart_log::info!(
        "  pm_kernel @ {:#x} (syssize {:#x})",
        pm_kernel_addr,
        syssize
    );
    fstart_log::info!("  pref_address: {:#x}", pref_address);
    fstart_log::info!("  entry64 @ {:#x}", entry64);
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
    let init_size = u32::from_le_bytes(params[0x260..0x264].try_into().unwrap_or([0; 4]));
    let kernel_alignment = u32::from_le_bytes(params[0x230..0x234].try_into().unwrap_or([0; 4]));
    let xloadflags = u16::from_le_bytes(params[0x236..0x238].try_into().unwrap_or([0; 2]));
    fstart_log::info!("  init_size: {:#x}", init_size);
    fstart_log::info!("  kernel_alignment: {:#x}", kernel_alignment);
    fstart_log::info!("  xloadflags: {:#x}", xloadflags);

    if print_x86_mtrrs {
        log_bsp_x86_cache_state("before ROM WP clear");
    }

    fstart_log::info!("mtrr: clearing temporary BSP ROM WP before payload handoff");
    // The temporary BSP-only ROM WP MTRR must not leak to Linux: APs do not
    // carry it, and OSes require coherent MTRR state across CPUs.
    // SAFETY: this is the BSP immediately before payload handoff.
    unsafe { mtrr::set_boot_rom_wp(false) };

    if print_x86_mtrrs {
        log_bsp_x86_cache_state("before Linux jump");
    }

    log_x86_irq_handoff_state();

    // Jump to the kernel's 64-bit entry.
    //
    // The 64-bit boot protocol (boot protocol 2.12+, XLF_KERNEL_64) expects:
    //   - CPU in 64-bit long mode with paging enabled
    //   - Identity-mapped page tables covering all of physical memory
    //     the kernel might access during early boot (we map 4 GiB)
    //   - %rsi = physical address of the boot_params (zero page)
    //   - Interrupts disabled (cli)
    //   - GDT with __BOOT_CS (0x10) and __BOOT_DS (0x18) valid
    //
    // Our firmware already satisfies all of these: we're in long mode
    // with identity-mapped 2 MiB pages over 4 GiB, interrupts are
    // disabled (cli in _start16bit), and we have the correct GDT.
    //
    // SAFETY: all zero page fields have been populated above.
    // kernel_addr points to a previously loaded and relocated bzImage.
    unsafe {
        core::arch::asm!(
            // Consume the input operands FIRST — the compiler may have
            // placed them in any GPR, and the xor sequence below would
            // destroy them if we zeroed first.
            "cli",
            "mov rsi, {zero_page}",
            "mov rdi, {entry}",
            // Now zero all other GPRs to give the kernel a clean slate.
            // rsi = boot_params, rdi = entry (consumed by jmp below).
            // Leave rsp as-is — kernel sets its own stack.
            "xor eax, eax",
            "xor ebx, ebx",
            "xor ecx, ecx",
            "xor edx, edx",
            "xor ebp, ebp",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            "xor r12d, r12d",
            "xor r13d, r13d",
            "xor r14d, r14d",
            "xor r15d, r15d",
            "jmp rdi",
            zero_page = in(reg) zero_page,
            entry = in(reg) entry64,
            options(noreturn),
        );
    }
}

#[cfg(target_arch = "x86_64")]
fn log_x86_irq_handoff_state() {
    unsafe {
        use fstart_pio::{inb, inl, outb, outl, outw};

        unsafe fn pci_cfg_addr(dev: u8, func: u8, reg: u8) -> u32 {
            0x8000_0000 | ((dev as u32) << 11) | ((func as u32) << 8) | ((reg as u32) & 0xfc)
        }
        unsafe fn pci_read8(dev: u8, func: u8, reg: u8) -> u8 {
            outl(0xcf8, pci_cfg_addr(dev, func, reg));
            ((inl(0xcfc) >> ((reg & 3) * 8)) & 0xff) as u8
        }
        unsafe fn pci_read16(dev: u8, func: u8, reg: u8) -> u16 {
            outl(0xcf8, pci_cfg_addr(dev, func, reg));
            ((inl(0xcfc) >> ((reg & 2) * 8)) & 0xffff) as u16
        }
        unsafe fn pci_read32(dev: u8, func: u8, reg: u8) -> u32 {
            outl(0xcf8, pci_cfg_addr(dev, func, reg));
            inl(0xcfc)
        }
        unsafe fn pm_read16(pm: u16, off: u16) -> u16 {
            fstart_pio::inw(pm + off)
        }
        unsafe fn pm_read32(pm: u16, off: u16) -> u32 {
            fstart_pio::inl(pm + off)
        }
        unsafe fn pm_write16(pm: u16, off: u16, val: u16) {
            outw(pm + off, val)
        }
        unsafe fn pm_write32(pm: u16, off: u16, val: u32) {
            outl(pm + off, val)
        }
        unsafe fn ioapic_read(reg: u32) -> u32 {
            core::ptr::write_volatile(0xfec0_0000 as *mut u32, reg);
            core::ptr::read_volatile(0xfec0_0010 as *const u32)
        }

        let lpc_id = pci_read32(0x1f, 0, 0x00);
        let pm = (pci_read32(0x1f, 0, 0x40) & 0xff80) as u16;

        // ICH7/NM10 handoff: keep ACPI SCI/GPE policy programmed by the
        // southbridge driver.  An earlier debug path masked GPE0, PM1_EN, and
        // PM1_CNT.SCI_EN here; Linux then saw ACPI tables that advertised SCI9
        // while the chipset was left in a non-coreboot state.  Match the
        // coreboot handoff more closely: disable SMI sources when no SMM handler
        // is active, but only clear stale W1C status and leave SCI/GPE enables
        // intact for the OS ACPI driver.
        if lpc_id == 0x27bc_8086 && pm != 0 {
            pm_write32(pm, 0x30, 0); // SMI_EN: no SMM handler active.
            pm_write32(pm, 0x2c, 0); // GPE0_EN: no AML GPE methods yet.
            pm_write16(pm, 0x02, (1 << 8) | (1 << 5)); // PM1_EN: power/global only.
            pm_write16(pm, 0x00, pm_read16(pm, 0x00)); // PM1_STS W1C.
            pm_write32(pm, 0x28, pm_read32(pm, 0x28)); // GPE0_STS W1C.
            pm_write32(pm, 0x34, pm_read32(pm, 0x34)); // SMI_STS W1C.
            pm_write32(pm, 0x04, (pm_read32(pm, 0x04) & !0x1c00) | 0x03); // ACPI mode.
            let tco_sts = pm_read32(pm, 0x64);
            pm_write32(pm, 0x64, tco_sts & !(1 << 18)); // TCO_STS except BOOT_STS.
            if tco_sts & (1 << 18) != 0 {
                pm_write32(pm, 0x64, 1 << 18);
            }
        }

        fstart_log::info!(
            "irq-handoff: LPC SERIRQ={:#04x} LPC_IO_DEC={:#06x} LPC_EN={:#06x}",
            pci_read8(0x1f, 0, 0x64),
            pci_read16(0x1f, 0, 0x80),
            pci_read16(0x1f, 0, 0x82),
        );
        fstart_log::info!(
            "irq-handoff: GEN1={:#010x} GEN2={:#010x} GEN3={:#010x} GEN4={:#010x}",
            pci_read32(0x1f, 0, 0x84),
            pci_read32(0x1f, 0, 0x88),
            pci_read32(0x1f, 0, 0x8c),
            pci_read32(0x1f, 0, 0x90),
        );
        fstart_log::info!(
            "irq-handoff: PIC masks master={:#04x} slave={:#04x} ELCR={:#04x}/{:#04x}",
            inb(0x21),
            inb(0xa1),
            inb(0x4d0),
            inb(0x4d1),
        );
        fstart_log::info!(
            "irq-handoff: PMBASE={:#06x} PM1_STS={:#06x} PM1_EN={:#06x} PM1_CNT={:#010x}",
            pm,
            pm_read16(pm, 0x00),
            pm_read16(pm, 0x02),
            pm_read32(pm, 0x04),
        );
        fstart_log::info!(
            "irq-handoff: GPE0_STS={:#010x} GPE0_EN={:#010x} SMI_EN={:#010x} SMI_STS={:#010x}",
            pm_read32(pm, 0x28),
            pm_read32(pm, 0x2c),
            pm_read32(pm, 0x30),
            pm_read32(pm, 0x34),
        );
        fstart_log::info!(
            "irq-handoff: TCO1_STS={:#06x} TCO2_STS={:#06x}",
            pm_read16(pm, 0x64),
            pm_read16(pm, 0x66),
        );
        fstart_log::info!(
            "irq-handoff: IOAPIC redir1={:#010x}/{:#010x} redir4={:#010x}/{:#010x}",
            ioapic_read(0x12),
            ioapic_read(0x13),
            ioapic_read(0x18),
            ioapic_read(0x19),
        );
        fstart_log::info!(
            "irq-handoff: IOAPIC redir9={:#010x}/{:#010x} redir12={:#010x}/{:#010x}",
            ioapic_read(0x22),
            ioapic_read(0x23),
            ioapic_read(0x28),
            ioapic_read(0x29),
        );

        let com1 = 0x3f8u16;
        let lcr = inb(com1 + 3);
        outb(com1 + 3, lcr & !0x80);
        for _ in 0..16 {
            if inb(com1 + 5) & 1 == 0 {
                break;
            }
            let _ = inb(com1);
        }
        fstart_log::info!(
            "irq-handoff: COM1 IER={:#04x} IIR={:#04x} LCR={:#04x} MCR={:#04x} LSR={:#04x} MSR={:#04x}",
            inb(com1 + 1), inb(com1 + 2), inb(com1 + 3), inb(com1 + 4), inb(com1 + 5), inb(com1 + 6),
        );
        fstart_log::info!(
            "irq-handoff: KBC status={:#04x} data={:#04x}",
            inb(0x64),
            inb(0x60)
        );
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn log_x86_irq_handoff_state() {}
