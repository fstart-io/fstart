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

use fstart_services::memory_detect::E820Entry;

// ---------------------------------------------------------------------------
// Entry point — 16-bit real mode → 32-bit protected mode → 64-bit long mode
// ---------------------------------------------------------------------------

/// The entry sequence is written as `global_asm!` because it transitions
/// through three CPU modes before reaching Rust code. The `_start16bit`
/// label is placed in `.text.entry` by the linker script.
///
/// GDT layout (matches Linux __BOOT_CS/DS expectations):
/// - 0x00: null descriptor
/// - 0x08: 32-bit flat code (used only for 16→32 bit transition)
/// - 0x10: 64-bit code (__BOOT_CS, Long mode, Execute/Read)
/// - 0x18: flat data (__BOOT_DS, 4 GiB, Read/Write)
///
/// Page tables: identity-mapped 2 MiB pages covering 4 GiB.
/// PML4 → 1 PDPT → 4 PDTs → 512 × 2 MiB pages each.
core::arch::global_asm!(
    // Use AT&T syntax throughout — matches coreboot convention and is
    // the natural syntax for 16-bit / mixed-mode x86 assembly.
    ".att_syntax prefix",
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
    // Save BIST result
    "movl %eax, %ebp",
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
    // Entry 0x10: 64-bit code (L=1, D=0, base=0, limit=4G)
    // Linux __BOOT_CS = 0x10 — must be at this selector.
    ".word 0xffff, 0x0000",
    ".byte 0x00, 0x9b, 0xaf, 0x00",
    // Entry 0x18: 64-bit/32-bit flat data (base=0, limit=4G, G=1, B=1)
    // Linux __BOOT_DS = 0x18 — must be at this selector.
    // Also used as the 32-bit data segment during early init.
    ".word 0xffff, 0x0000",
    ".byte 0x00, 0x93, 0xcf, 0x00",
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
    // Load data segment selectors (0x18 = flat data, __BOOT_DS)
    "movw $0x18, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movw %ax, %fs",
    "movw %ax, %gs",
    // Clear the early-RAM region (CAR on real hardware, just RAM on QEMU).
    // This ensures BSS is zero before we touch any Rust statics.
    // The linker provides _bss_start and _bss_end.
    "movl $_bss_start, %edi",
    "movl $_bss_end, %ecx",
    "subl %edi, %ecx",
    "shrl $2, %ecx", // count in dwords
    "xorl %eax, %eax",
    "rep",
    "stosl",
    // Set up identity-mapped page tables for long mode.
    //
    // Uses 1 GiB pages (PDPE1GB) for a compact 2-page layout:
    //   PML4[0] → PDPT, PDPT[0..511] = 512 × 1 GiB identity pages.
    // Covers 512 GiB — enough for any QEMU or real hardware config.
    //
    // 1 GiB pages (bit 7 = PS in PDPT entries) are supported by all
    // x86_64 CPUs that QEMU can emulate (KVM host, TCG -cpu max).
    //
    // Layout: 2 pages total.
    //   page_tables_start + 0x0000: PML4 (1 page)
    //   page_tables_start + 0x1000: PDPT (1 page)
    //
    // Clear page table area first
    "movl $_page_tables_start, %edi",
    "movl $_page_tables_end, %ecx",
    "subl %edi, %ecx",
    "shrl $2, %ecx",
    "xorl %eax, %eax",
    "rep",
    "stosl",
    // PML4[0] = address of PDPT | Present | Writable
    "movl $_page_tables_start, %edi",
    "leal 0x1003(%edi), %eax", // PDPT = page_tables + 0x1000, flags = 0x3
    "movl %eax, (%edi)",
    // Fill PDPT[0..511] with 1 GiB identity-mapped pages.
    // Each entry: physical_addr | PageSize(1GB) | Writable | Present
    // 1 GiB page flag = bit 7 (PS) in PDPT entry = 0x83.
    "leal 0x1000(%edi), %esi", // ESI = PDPT base
    "xorl %edx, %edx",         // EDX = high 32 bits of PA (starts at 0)
    "xorl %eax, %eax",         // EAX = low 32 bits of PA (starts at 0)
    "orl $0x83, %eax",         // PS=1, RW=1, P=1
    "movl $512, %ecx",         // 512 entries × 1 GiB = 512 GiB
    "1:",
    "movl %eax, (%esi)",      // low 32 bits
    "movl %edx, 4(%esi)",     // high 32 bits
    "addl $0x40000000, %eax", // next 1 GiB page (low 32 bits)
    "adcl $0, %edx",          // carry into high 32 bits
    "addl $8, %esi",
    "decl %ecx",
    "jnz 1b",
    // Enable PAE (bit 5), OSFXSR (bit 9), OSXMMEXCPT (bit 10).
    // OSFXSR + OSXMMEXCPT enable SSE/SSE2 (compiler_builtins memcpy).
    "movl %cr4, %eax",
    "orl $0x620, %eax", // PAE | OSFXSR | OSXMMEXCPT
    "movl %eax, %cr4",
    // Load PML4 base into CR3
    "movl $_page_tables_start, %eax",
    "movl %eax, %cr3",
    // Enable long mode + NX support:
    //   IA32_EFER.LME (bit 8) — Long Mode Enable
    //   IA32_EFER.NXE (bit 11) — No-Execute Enable
    // NXE is required by CrabEFI and the Linux kernel for marking
    // data pages as non-executable. Without it, bit 63 in page
    // table entries is reserved and triggers #PF.
    "movl $0xC0000080, %ecx", // IA32_EFER MSR
    "rdmsr",
    "orl $0x900, %eax", // LME | NXE
    "wrmsr",
    // Enable paging + SSE/AVX in one CR0 write:
    //   Set:   PG (bit 31), MP (bit 1)
    //   Clear: CD (bit 30), NW (bit 29), EM (bit 2)
    "movl %cr0, %eax",
    "andl $0x9FFFFFFB, %eax", // clear CD, NW, EM
    "orl $0x80000002, %eax",  // set PG, MP
    "movl %eax, %cr0",
    // Far jump to 64-bit code segment (GDT selector 0x10 = __BOOT_CS)
    ".byte 0xea",        // ljmpl opcode
    ".long _start64bit", // 32-bit offset
    ".word 0x10",        // 64-bit code segment selector
    // =====================================================================
    // 64-bit long mode entry
    // =====================================================================
    ".code64",
    ".global _start64bit",
    "_start64bit:",
    // Reload data segments with 64-bit data selector (0x18 = __BOOT_DS)
    "movw $0x18, %ax",
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
    "movw $0x10, 2(%rdi)", // CS = 0x10 (__BOOT_CS, 64-bit code)
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
    "movq (%rsp), %rdi",   // rdi = faulting RIP
    "movq 24(%rsp), %rsi", // rsi = pre-exception RSP
    "movq %cr2, %rdx",     // rdx = CR2 (page fault address)
    "call x86_exception_handler",
    // Should not return, but halt if it does
    "5:",
    "hlt",
    "jmp 5b",
);

// IDT table at a fixed low address (after page tables).
// Page tables: 0x1000..0x3000 (2 pages: PML4 + PDPT, 1 GiB pages).
// IDT: placed after page tables (1 page, 256 entries × 16 bytes).
core::arch::global_asm!(
    ".section .idt_table, \"aw\", @nobits",
    ".align 4096",
    ".global _idt_table",
    "_idt_table:",
    ".skip 4096",
);

// ---------------------------------------------------------------------------
// RAM-stage entry (64-bit only — no 16-bit/32-bit transition)
// ---------------------------------------------------------------------------

/// Entry point for non-first x86_64 stages that run from RAM.
///
/// The bootblock already transitioned to 64-bit long mode with identity-
/// mapped page tables. This entry zeros BSS, copies .data initializers
/// (harmless no-op when src == dst), sets up the IDT and stack, then
/// calls `fstart_main(0)`.
///
/// Placed in `.text.entry` so `KEEP(*(.text.entry))` in the linker script
/// ensures it's at the start of the binary (= the load address that the
/// bootblock's `jump_to()` targets).
core::arch::global_asm!(
    ".att_syntax prefix",
    ".section .text.entry, \"ax\"",
    ".code64",
    ".global _start_ram",
    "_start_ram:",
    // Zero BSS (64-bit mode)
    "movabs $_bss_start, %rdi",
    "movabs $_bss_end, %rcx",
    "subq %rdi, %rcx",
    "shrq $3, %rcx", // count in qwords
    "xorl %eax, %eax",
    "rep stosq",
    // Set up stack
    "movabs $_stack_top, %rsp",
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
    // Call fstart_main(handoff_ptr=0)
    "xorl %edi, %edi",
    "call fstart_main",
    // Should never return
    "2:",
    "hlt",
    "jmp 2b",
);

// Make sure the linker pulls in the entry code
extern "Rust" {
    fn fstart_main(handoff_ptr: usize) -> !;
}

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

/// Boot a Linux kernel using the x86 64-bit boot protocol.
///
/// Uses the 64-bit entry point at `code32_start + 0x200` (available since
/// boot protocol 2.12 when `XLF_KERNEL_64` is set in `xload_flags`).
/// This avoids the complex long-mode → protected-mode teardown needed
/// by the 32-bit protocol: we stay in long mode, set `%rsi` to the
/// zero page, and jump directly to the kernel's `startup_64`.
///
/// Constructs the zero page (boot_params) at `0x90000`, fills e820
/// entries and ACPI RSDP address, then jumps to the kernel.
///
/// # Arguments
/// - `kernel_addr`: physical address of the loaded kernel (typically `0x100000`)
/// - `rsdp_addr`: physical address of the ACPI RSDP (from AcpiLoad)
/// - `e820_entries`: slice of e820 memory map entries (from MemoryDetect)
pub fn boot_linux(
    kernel_addr: u64,
    rsdp_addr: u64,
    e820_entries: &[E820Entry],
    bootargs: &str,
) -> ! {
    // The zero page is at a well-known location in conventional memory.
    const ZERO_PAGE: u64 = 0x90000;
    const CMD_LINE: u64 = 0x91000;

    // SAFETY: these addresses are in conventional memory below 640K,
    // cleared by the entry code, and not used by any other code at
    // this point.
    let params = unsafe { &mut *(ZERO_PAGE as *mut [u8; 4096]) };
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
    let cmdline = unsafe { &mut *(CMD_LINE as *mut [u8; 4096]) };
    let args_bytes = bootargs.as_bytes();
    let copy_len = args_bytes.len().min(4095); // leave room for NUL
    cmdline[..copy_len].copy_from_slice(&args_bytes[..copy_len]);
    cmdline[copy_len] = 0; // NUL terminator
    params[0x228..0x22C].copy_from_slice(&(CMD_LINE as u32).to_le_bytes());

    // ACPI RSDP address (offset 0x070, protocol 2.14+)
    params[0x070..0x078].copy_from_slice(&rsdp_addr.to_le_bytes());

    // e820 map: count at 0x1E8, entries at 0x2D0 (20 bytes each)
    let count = e820_entries.len().min(128) as u8;
    params[0x1E8] = count;
    for (i, entry) in e820_entries.iter().take(128).enumerate() {
        let offset = 0x2D0 + i * 20;
        params[offset..offset + 8].copy_from_slice(&entry.addr.to_le_bytes());
        params[offset + 8..offset + 16].copy_from_slice(&entry.size.to_le_bytes());
        params[offset + 16..offset + 20].copy_from_slice(&entry.kind.to_le_bytes());
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
    fstart_log::info!("  zero_page @ {:#x}", ZERO_PAGE);
    fstart_log::info!("  rsdp @ {:#x}", rsdp_addr);
    fstart_log::info!("  e820 count: {}", e820_entries.len());

    // Log critical setup header fields for debugging.
    let init_size = u32::from_le_bytes(params[0x260..0x264].try_into().unwrap_or([0; 4]));
    let kernel_alignment = u32::from_le_bytes(params[0x230..0x234].try_into().unwrap_or([0; 4]));
    let xloadflags = u16::from_le_bytes(params[0x236..0x238].try_into().unwrap_or([0; 2]));
    fstart_log::info!("  init_size: {:#x}", init_size);
    fstart_log::info!("  kernel_alignment: {:#x}", kernel_alignment);
    fstart_log::info!("  xloadflags: {:#x}", xloadflags);

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
            zero_page = in(reg) ZERO_PAGE,
            entry = in(reg) entry64,
            options(noreturn),
        );
    }
}
