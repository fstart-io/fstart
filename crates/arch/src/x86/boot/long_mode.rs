//! Long-mode half of the x86 stage runtime: identity page tables, the
//! 32-to-64-bit switch, the 64-bit IDT, the DRAM-stage and postcar entries,
//! and the 64-bit jumps.

// Writable page-table setup for QEMU-style low-RAM page tables.
// 1 GiB pages: PDPT[0..511] = 512 x 1 GiB identity-mapped pages.
// Requires PDPE1GB. Covers 512 GiB. Compact: only 2 pages total.
#[cfg(all(feature = "x86-writable-page-tables", feature = "x86-1g-pages"))]
#[cfg(target_os = "none")]
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
#[cfg(target_os = "none")]
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
#[cfg(target_os = "none")]
core::arch::global_asm!(
    ".section .text, \"ax\"",
    ".code32",
    ".global _setup_page_tables",
    "_setup_page_tables:",
    "jmp _after_page_tables",
    options(att_syntax),
);

#[cfg(all(feature = "x86-static-page-tables", feature = "x86-1g-pages"))]
#[cfg(target_os = "none")]
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
#[cfg(target_os = "none")]
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
#[cfg(target_os = "none")]
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
#[cfg(target_os = "none")]
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
#[cfg(target_os = "none")]
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
#[cfg(target_os = "none")]
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
    "pushq %rsi",
    // Zero the IDT area
    "xorl %eax, %eax",
    "movabs $_idt_table, %rdi",
    "movl $512, %ecx",
    "rep stosq",
    // Fill all 256 entries → unknown-vector stub first.
    "movabs $_exc_stub_unknown, %rbx",
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
    // Overwrite exception vectors 0..31 with vector-specific stubs.
    "movabs $_exc_stub_ptrs, %rsi",
    "movabs $_idt_table, %rdi",
    "movl $32, %ecx",
    "6:",
    "lodsq",
    "movq %rax, %rbx",
    "movw %bx, (%rdi)",
    "movw $0x18, 2(%rdi)",
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
    "jnz 6b",
    // Load the IDT
    "movabs $_idt_desc, %rax",
    "lidt (%rax)",
    "popq %rsi",
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
    // Exception stubs — normalize x86 exception frames and call Rust.
    //
    // For same-privilege exceptions in long mode, the CPU pushes:
    //   no error code: RIP, CS, RFLAGS
    //   error code:    ERR, RIP, CS, RFLAGS
    // The vector-specific stubs normalize both forms to:
    //   [rsp+0]  vector
    //   [rsp+8]  error_code (0 for no-error-code vectors)
    //   [rsp+16] RIP
    //   [rsp+24] CS
    //   [rsp+32] RFLAGS
    // and common code passes an approximate interrupted RSP as rsp+40.
    // =====================================================================
    ".align 16",
    "_exc_common:",
    // POST 0xe0: CPU exception reached the IDT stub.
    "movb $0xe0, %al",
    "outb %al, $0x80",
    "movq 0(%rsp), %rdi",  // vector
    "movq 8(%rsp), %rsi",  // error code
    "movq 16(%rsp), %rdx", // RIP
    "movq 24(%rsp), %rcx", // CS
    "movq 32(%rsp), %r8",  // RFLAGS
    "leaq 40(%rsp), %r9",  // approximate interrupted RSP
    "movq %cr2, %rax",
    // Normalized exception frames leave RSP 8 mod 16. SysV requires the caller
    // to have RSP 0 mod 16 before `call`, so reserve one stack-argument slot.
    "subq $8, %rsp",
    "movq %rax, (%rsp)", // 7th arg: CR2
    "call x86_exception_handler",
    // Should not return, but halt if it does
    "5:",
    "hlt",
    "jmp 5b",
    ".macro EXC_NOERR num",
    "  .align 16",
    "  _exc_\\num:",
    "  pushq $0",
    "  pushq $\\num",
    "  jmp _exc_common",
    ".endm",
    ".macro EXC_ERR num",
    "  .align 16",
    "  _exc_\\num:",
    "  pushq $\\num",
    "  jmp _exc_common",
    ".endm",
    "EXC_NOERR 0",
    "EXC_NOERR 1",
    "EXC_NOERR 2",
    "EXC_NOERR 3",
    "EXC_NOERR 4",
    "EXC_NOERR 5",
    "EXC_NOERR 6",
    "EXC_NOERR 7",
    "EXC_ERR 8",
    "EXC_NOERR 9",
    "EXC_ERR 10",
    "EXC_ERR 11",
    "EXC_ERR 12",
    "EXC_ERR 13",
    "EXC_ERR 14",
    "EXC_NOERR 15",
    "EXC_NOERR 16",
    "EXC_ERR 17",
    "EXC_NOERR 18",
    "EXC_NOERR 19",
    "EXC_NOERR 20",
    "EXC_ERR 21",
    "EXC_NOERR 22",
    "EXC_NOERR 23",
    "EXC_NOERR 24",
    "EXC_NOERR 25",
    "EXC_NOERR 26",
    "EXC_NOERR 27",
    "EXC_NOERR 28",
    "EXC_ERR 29",
    "EXC_ERR 30",
    "EXC_NOERR 31",
    ".align 16",
    "_exc_stub_unknown:",
    "pushq $0",
    "pushq $255",
    "jmp _exc_common",
    ".align 8",
    "_exc_stub_ptrs:",
    ".quad _exc_0, _exc_1, _exc_2, _exc_3, _exc_4, _exc_5, _exc_6, _exc_7",
    ".quad _exc_8, _exc_9, _exc_10, _exc_11, _exc_12, _exc_13, _exc_14, _exc_15",
    ".quad _exc_16, _exc_17, _exc_18, _exc_19, _exc_20, _exc_21, _exc_22, _exc_23",
    ".quad _exc_24, _exc_25, _exc_26, _exc_27, _exc_28, _exc_29, _exc_30, _exc_31",
    options(att_syntax),
);

// ---------------------------------------------------------------------------
// RAM-stage entry (64-bit only — no 16-bit/32-bit transition)
// ---------------------------------------------------------------------------

// Entry point for the x86_64 ramstage, entered from postcar after CAR
// teardown with caching already enabled. This entry only sets up its own
// stack, zeroes BSS, copies .data initializers (harmless no-op when
// src == dst), sets up the IDT, then calls `fstart_main(0)`.
//
// It must NOT tear down CAR: post-`INVD` execution belongs to fresh
// programs only, and postcar already performed the noreturn transition.
//
// Placed in `.text.entry` so `KEEP(*(.text.entry))` in the linker script
// ensures it's at the start of the binary (= the load address that
// postcar's jump targets).
//
// Compiled out of the postcar stage (which carries `_start_postcar` in
// `.text.entry` instead) so the flat image always starts at its own entry.
#[cfg(all(target_os = "none", not(fstart_stage_env = "postcar")))]
core::arch::global_asm!(
    ".section .text.entry, \"ax\"",
    ".code64",
    ".global _start_ram",
    "_start_ram:",
    // POST 0x40: RAM-stage 64-bit entry reached.
    "movb $0x40, %al",
    "outb %al, $0x80",
    // Caching is already on (postcar transition); claim the DRAM stack.
    "movabs $_stack_top, %rsp",
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
    // Call fstart_main(handoff_ptr=0).  The x86 StageLoad paths do not yet
    // serialize StageHandoff data for RAM stages; passing through an arbitrary
    // caller `%rdi` can fault before ConsoleInit installs the logger.
    "xorl %edi, %edi",
    "call fstart_main",
    // Should never return
    "2:",
    "hlt",
    "jmp 2b",
    options(att_syntax),
);

// ---------------------------------------------------------------------------
// Postcar entry (64-bit; Cut-B CAR teardown + jump)
// ---------------------------------------------------------------------------

// Entry point for the x86_64 postcar stage, entered via direct jump from the
// bootblock while Cache-as-RAM is still live, running on the inherited CAR
// stack. Cut-B shape (coreboot `exit_car.S` + change 95145):
//
//  1. `call _car_teardown` — CR0.CD=1, MTRRs off, Atom NEM cleared.
//     The return address is popped into a register before CAR is disabled;
//     no CAR-backed stack access occurs after that point.
//  2. Program variable MTRRs from the UC-DRAM stash at `POSTCAR_STASH_ADDR`
//     (written pre-transition by the bootblock, `INVD`-proof), enable MTRRs.
//  3. Clear CR0.CD/NW, `INVD`, switch to the fresh DRAM stack (`_stack_top`
//     is an immediate — never a stale memory read).
//  4. Zero BSS / copy .data / set up IDT as a fresh program, then call
//     `fstart_main(0)`, which dispatches to the postcar loader that FFS-loads
//     and decompresses the ramstage cached.
//
// The pre-transition half is pure asm (~30 instructions); everything
// substantive (FFS, LZ4, console) runs post-transition as a fresh program
// naming only its own image/stack/ROM/hardware.
//
// The linker script selects this entry (`ENTRY(_start_postcar)`) for the
// stage named "postcar"; it also lands first via `.text.entry`. The
// `postcar` stage_env gate keeps `_start_ram` out of the postcar binary so
// the flat image starts here, at the address the bootblock jumps to.
//
// These are compile-time strings rather than a callable helper: the first
// checkpoint runs on the inherited CAR stack, so a call would violate the
// transition's no-CAR-stack rule.
#[cfg(all(target_os = "none", fstart_stage_env = "postcar"))]
core::arch::global_asm!(
    ".section .text.entry, \"ax\"",
    ".code64",
    ".global _start_postcar",
    "_start_postcar:",
    // POST 0x70: postcar entry reached (CAR still live, inherited stack).
    "movb $0x70, %al",
    "outb %al, $0x80",
    // Optional raw COM1 checkpoints: p=entry, t=teardown returned,
    // m=MTRRs enabled, c=INVD complete, r=Rust entry, !=invalid stash.
    // They use no stack or logger, but are omitted from production images.
    postcar_uart_checkpoint!("0x70"), // p
    // Step 1: tear down CAR (CD=1, MTRRs off, NEM cleared on Atom).
    "call _car_teardown",
    postcar_uart_checkpoint!("0x74"), // t
    // Step 2: program variable MTRRs from the UC stash.
    // Stash layout (car_teardown::PostcarMtrrStash): u32 magic, u32 count,
    // then count x (u64 base_msr, u64 mask_msr) programmed as MTRR 0..n.
    "movabs ${postcar_stash}, %rsi",
    "cmpl $0x54534350, (%rsi)", // POSTCAR_STASH_MAGIC ("PCST")
    "jne _postcar_stash_fail",
    "movl 4(%rsi), %r15d",
    // Cap the entry count: programming past the valid variable-MTRR pairs
    // would hit reserved MSRs and #GP. The writer never emits more than 8.
    "cmpl $8, %r15d",
    "ja _postcar_stash_fail",
    "addq $8, %rsi",
    "movl $0x200, %ebx", // IA32_MTRR_PHYSBASE0; +2 per entry
    "1:",
    "testl %r15d, %r15d",
    "jz 2f",
    "movl %ebx, %ecx",
    "movq (%rsi), %rax",
    "movq %rax, %rdx",
    "shrq $32, %rdx",
    "wrmsr", // PHYSBASE
    "incl %ebx",
    "movl %ebx, %ecx",
    "movq 8(%rsi), %rax",
    "movq %rax, %rdx",
    "shrq $32, %rdx",
    "wrmsr", // PHYSMASK
    "incl %ebx",
    "addq $16, %rsi",
    "decl %r15d",
    "jmp 1b",
    "2:",
    // Enable MTRRs, preserving the default type (UC, FIX_EN off — matches
    // the pre-transition CAR-phase layout; ramstage refines this later).
    "movl $0x2ff, %ecx",
    "rdmsr",
    "orl $0x800, %eax", // MTRR_DEF_TYPE_EN
    "wrmsr",
    postcar_uart_checkpoint!("0x6d"), // m
    // Step 3: re-enable caching, flush stale CAR lines, take fresh stack.
    "movq %cr0, %rax",
    // Clear CD|NW. The mask is written as a sign-extended imm32
    // (0x9FFFFFFF does not fit an unsigned imm32).
    "andq $0xFFFFFFFF9FFFFFFF, %rax",
    "movq %rax, %cr0",
    "invd",
    postcar_uart_checkpoint!("0x63"), // c
    // POST 0x71: transition done; CAR stack abandoned, DRAM stack next.
    "movb $0x71, %al",
    "outb %al, $0x80",
    "movabs $_stack_top, %rsp",
    "andq $0xfffffffffffffff0, %rsp",
    // Step 4: fresh-program init — BSS, .data, IDT, then Rust.
    "movabs $_bss_start, %rdi",
    "movabs $_bss_end, %rcx",
    "subq %rdi, %rcx",
    "shrq $3, %rcx",
    "xorl %eax, %eax",
    "rep stosq",
    "movabs $_data_load, %rsi",
    "movabs $_data_start, %rdi",
    "movabs $_data_end, %rcx",
    "subq %rdi, %rcx",
    "cmpq %rsi, %rdi",
    "je 3f",
    "rep movsb",
    "3:",
    "call _setup_idt",
    // POST 0x72: postcar runtime setup complete; entering Rust.
    "movb $0x72, %al",
    "outb %al, $0x80",
    postcar_uart_checkpoint!("0x72"), // r
    "xorl %edi, %edi",
    "call fstart_main",
    "4:",
    "hlt",
    "jmp 4b",
    // Stash validation failed: POST 0xee and halt (no stack to trust).
    "_postcar_stash_fail:",
    postcar_uart_checkpoint!("0x21"), // !
    "movb $0xee, %al",
    "outb %al, $0x80",
    "5:",
    "hlt",
    "jmp 5b",
    postcar_stash = const super::car_teardown::POSTCAR_STASH_ADDR,
    options(att_syntax),
);

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

/// Jump to an absolute 64-bit address with a handoff pointer in `%rdi`.
///
/// The current x86 RAM-stage entry clears the handoff before `fstart_main`
/// because x86 stage loaders do not serialize handoff data yet. Keep this
/// primitive for payloads or future x86 stage entries that consume `%rdi`
/// directly.
pub fn jump_to_with_handoff(addr: u64, handoff_addr: usize) -> ! {
    // SAFETY: caller guarantees `addr` points to a loaded x86_64 RAM stage and
    // `handoff_addr` is either 0 or points to a valid serialized StageHandoff.
    unsafe {
        core::arch::asm!(
            "jmp *%rax",
            in("rax") addr,
            in("rdi") handoff_addr,
            options(noreturn, nostack, preserves_flags, att_syntax),
        );
    }
}

/// Enter the kernel through the 64-bit boot protocol entry
/// (`code32_start + 0x200`, protocol 2.12+ with `XLF_KERNEL_64`).
pub(super) fn enter_linux(pm_kernel_addr: u64, zero_page: u64) -> ! {
    let entry64 = pm_kernel_addr + 0x200;
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

// Leave long mode for a 32-bit protected-mode callee. coreboot payloads expect
// to be called like coreboot's x86_64 `protected_mode_call_1arg` calls them:
// flat 32-bit protected mode, paging off, interrupts off, with one cdecl
// argument on the stack (the coreboot table). The trampoline runs in place:
// like every x86 stage it lives in the identity-mapped first 4 GiB, so
// switching paging off keeps it addressable.
core::arch::global_asm!(
    ".section .text.pm32call, \"ax\"",
    ".code64",
    ".global _fstart_pm32_call",
    // rdi = entry, rsi = argument; both below 4 GiB.
    "_fstart_pm32_call:",
    "cli",
    "cld",
    // lgdt with a stack descriptor: no absolute relocation in the blob.
    "subq $16, %rsp",
    "movw $(_pm32_gdt_end - _pm32_gdt - 1), (%rsp)",
    "leaq _pm32_gdt(%rip), %rax",
    "movq %rax, 2(%rsp)",
    "lgdt (%rsp)",
    "addq $16, %rsp",
    // Far return into the 32-bit code segment (compatibility mode).
    "pushq $0x08",
    "leaq 1f(%rip), %rax",
    "pushq %rax",
    "lretq",
    ".code32",
    "1:",
    "movl $0x10, %eax",
    "movl %eax, %ds",
    "movl %eax, %es",
    "movl %eax, %ss",
    "movl %eax, %fs",
    "movl %eax, %gs",
    // Paging off, then long mode off, then PAE off.
    "movl %cr0, %eax",
    "andl $0x7fffffff, %eax",
    "movl %eax, %cr0",
    "movl $0xc0000080, %ecx",
    "rdmsr",
    "andl $0xfffffeff, %eax",
    "wrmsr",
    "movl %cr4, %eax",
    "andl $0xffffffdf, %eax",
    "movl %eax, %cr4",
    // entry(argument), cdecl, with no stale register state: SeaBIOS for
    // one reads EAX/EBX as a possible multiboot handoff.
    "pushl %esi",
    "movl %edi, %ecx",
    "xorl %eax, %eax",
    "xorl %ebx, %ebx",
    "xorl %edx, %edx",
    "xorl %esi, %esi",
    "xorl %edi, %edi",
    "xorl %ebp, %ebp",
    "calll *%ecx",
    "2:",
    "cli",
    "hlt",
    "jmp 2b",
    // Accessed bits preset: the CPU then never writes the GDT, which may
    // sit in flash (XIP stages).
    ".align 8",
    "_pm32_gdt:",
    ".quad 0x0000000000000000", // 0x00 null
    ".quad 0x00cf9b000000ffff", // 0x08 code32: base 0, 4 GiB
    ".quad 0x00cf93000000ffff", // 0x10 data32: base 0, 4 GiB
    "_pm32_gdt_end:",
    ".code64",
    options(att_syntax),
);

unsafe extern "C" {
    fn _fstart_pm32_call(entry: u64, argument: u64) -> !;
}

/// Call `entry(argument)` in flat 32-bit protected mode with paging off.
/// Does not return: should the callee return, the CPU halts.
///
/// # Safety
/// `entry` must be 32-bit code that is loaded and owns the machine from
/// here on. The current stack must lie below 4 GiB; the callee inherits it.
pub unsafe fn protected_mode_call(entry: u32, argument: u32) -> ! {
    let stack: u64;
    // SAFETY: reads the stack pointer only.
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) stack, options(nomem, nostack)) };
    assert!(
        stack < 1 << 32,
        "protected-mode call needs a stack below 4 GiB"
    );
    // SAFETY: caller contract above.
    unsafe { _fstart_pm32_call(entry.into(), argument.into()) }
}
