//! 32-bit protected-mode half of the x86 stage runtime, for CPUs without
//! long mode (Core Duo and older).
//!
//! Every stage runs flat (4 GiB code/data segments from the entry GDT) with
//! paging off, so physical and linear addresses are identical without any
//! page tables. This module supplies what long mode does in
//! [`super::long_mode`]: the bootblock runtime start, the 32-bit IDT, the
//! DRAM-stage and postcar entries, and the jumps out of firmware.
//!
//! Entry GDT selectors: 0x08 flat 32-bit code, 0x10 flat data.

// `fstart_main(0)` with the stack 16-byte aligned at the call, as the i386
// psABI and the target data layout (S128) require. Never returns.
macro_rules! call_fstart_main {
    () => {
        concat!(
            "subl $12, %esp\n",
            "pushl $0\n",
            "xorl %edi, %edi\n",
            "call fstart_main\n",
            "1:\n",
            "hlt\n",
            "jmp 1b\n",
        )
    };
}

// Bootblock continuation after CAR and BSS (`enter_runtime!`): stack,
// .data copy, IDT, then Rust.
#[cfg(all(not(test), target_os = "none", not(fstart_stage_env = "postcar")))]
core::arch::global_asm!(
    ".section .text, \"ax\"",
    ".code32",
    ".global _start32_runtime",
    "_start32_runtime:",
    // Enable OS support for FXSAVE/FXRSTOR, as the long-mode entry does.
    "movl %cr4, %eax",
    "orl $0x200, %eax", // CR4.OSFXSR
    "movl %eax, %cr4",
    // POST 0x31: CR4.OSFXSR written.
    "movb $0x31, %al",
    "outb %al, $0x80",
    "movl $_stack_top, %esp",
    "andl $-16, %esp",
    // Copy .data initializers from ROM (LMA) to CAR (VMA).
    "movl $_data_load, %esi",
    "movl $_data_start, %edi",
    "movl $_data_end, %ecx",
    "subl %edi, %ecx",
    "cmpl %esi, %edi",
    "je 2f",
    "rep movsb",
    "2:",
    "call _setup_idt",
    // POST 0x33: runtime setup complete; entering Rust.
    "movb $0x33, %al",
    "outb %al, $0x80",
    call_fstart_main!(),
    options(att_syntax),
);

// IDT setup and exception stubs. 32-bit interrupt gates are 8 bytes; the
// 4 KiB `_idt_table` reserved by the shared runtime holds all 256.
#[cfg(target_os = "none")]
core::arch::global_asm!(
    ".section .text, \"ax\"",
    ".code32",
    ".global _setup_idt",
    "_setup_idt:",
    "pushl %eax",
    "pushl %ebx",
    "pushl %ecx",
    "pushl %edx",
    "pushl %esi",
    "pushl %edi",
    // Every vector -> unknown-vector stub.
    "movl $_exc_stub_unknown, %ebx",
    "movl $_idt_table, %edi",
    "movl $256, %ecx",
    "1:",
    "call _idt_write_gate",
    "addl $8, %edi",
    "decl %ecx",
    "jnz 1b",
    // Exception vectors 0..31 -> vector-specific stubs.
    "movl $_exc_stub_ptrs, %esi",
    "movl $_idt_table, %edi",
    "movl $32, %ecx",
    "2:",
    "movl (%esi), %ebx",
    "call _idt_write_gate",
    "addl $4, %esi",
    "addl $8, %edi",
    "decl %ecx",
    "jnz 2b",
    "lidt _idt_desc",
    "popl %edi",
    "popl %esi",
    "popl %edx",
    "popl %ecx",
    "popl %ebx",
    "popl %eax",
    "ret",
    // Write a present ring-0 32-bit interrupt gate for handler EBX at EDI.
    "_idt_write_gate:",
    "movl %ebx, %eax",
    "andl $0xffff, %eax",
    "orl $0x00080000, %eax", // selector 0x08
    "movl %eax, (%edi)",
    "movl %ebx, %eax",
    "andl $0xffff0000, %eax",
    "orl $0x8e00, %eax", // P=1, DPL=0, 32-bit interrupt gate
    "movl %eax, 4(%edi)",
    "ret",
    ".align 4",
    "_idt_desc:",
    ".word 256 * 8 - 1",
    ".long _idt_table",
    // Exception stubs normalize both CPU frame shapes to
    //   [esp+0] vector, [esp+4] error code, [esp+8] EIP, [esp+12] CS,
    //   [esp+16] EFLAGS
    // and pass the handler's u64 arguments in cdecl order.
    ".align 16",
    "_exc_common:",
    // POST 0xe0: CPU exception reached the IDT stub.
    "movb $0xe0, %al",
    "outb %al, $0x80",
    "movl %esp, %ebp",
    "andl $-16, %esp",
    "subl $8, %esp", // 14 pushed dwords + 8 bytes keep the call 16-aligned
    "movl %cr2, %eax",
    "pushl $0",
    "pushl %eax", // cr2
    "leal 20(%ebp), %eax",
    "pushl $0",
    "pushl %eax", // interrupted ESP (approximate)
    "pushl $0",
    "pushl 16(%ebp)", // EFLAGS
    "pushl $0",
    "pushl 12(%ebp)", // CS
    "pushl $0",
    "pushl 8(%ebp)", // EIP
    "pushl $0",
    "pushl 4(%ebp)", // error code
    "pushl $0",
    "pushl 0(%ebp)", // vector
    "call x86_exception_handler",
    "5:",
    "hlt",
    "jmp 5b",
    ".macro EXC_NOERR num",
    "  .align 16",
    "  _exc_\\num:",
    "  pushl $0",
    "  pushl $\\num",
    "  jmp _exc_common",
    ".endm",
    ".macro EXC_ERR num",
    "  .align 16",
    "  _exc_\\num:",
    "  pushl $\\num",
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
    "pushl $0",
    "pushl $255",
    "jmp _exc_common",
    ".align 4",
    "_exc_stub_ptrs:",
    ".long _exc_0, _exc_1, _exc_2, _exc_3, _exc_4, _exc_5, _exc_6, _exc_7",
    ".long _exc_8, _exc_9, _exc_10, _exc_11, _exc_12, _exc_13, _exc_14, _exc_15",
    ".long _exc_16, _exc_17, _exc_18, _exc_19, _exc_20, _exc_21, _exc_22, _exc_23",
    ".long _exc_24, _exc_25, _exc_26, _exc_27, _exc_28, _exc_29, _exc_30, _exc_31",
    options(att_syntax),
);

// DRAM-stage entry, entered from postcar with caching already enabled. Like
// the long-mode entry it sets up only its own stack, BSS, .data and IDT; CAR
// teardown belongs to postcar alone.
#[cfg(all(target_os = "none", not(fstart_stage_env = "postcar")))]
core::arch::global_asm!(
    ".section .text.entry, \"ax\"",
    ".code32",
    ".global _start_ram",
    "_start_ram:",
    // POST 0x40: RAM-stage entry reached.
    "movb $0x40, %al",
    "outb %al, $0x80",
    "movl $_stack_top, %esp",
    "andl $-16, %esp",
    "movl $_bss_start, %edi",
    "movl $_bss_end, %ecx",
    "subl %edi, %ecx",
    "xorl %eax, %eax",
    "rep stosb",
    "movl $_data_load, %esi",
    "movl $_data_start, %edi",
    "movl $_data_end, %ecx",
    "subl %edi, %ecx",
    "cmpl %esi, %edi",
    "je 2f",
    "rep movsb",
    "2:",
    "call _setup_idt",
    // POST 0x43: RAM-stage runtime setup complete; entering Rust.
    "movb $0x43, %al",
    "outb %al, $0x80",
    call_fstart_main!(),
    options(att_syntax),
);

// Postcar entry: the long-mode Cut-B sequence (see `long_mode`) with 32-bit
// registers. Runs on the inherited CAR stack until the stack switch below.
#[cfg(all(target_os = "none", fstart_stage_env = "postcar"))]
core::arch::global_asm!(
    ".section .text.entry, \"ax\"",
    ".code32",
    ".global _start_postcar",
    "_start_postcar:",
    // POST 0x70: postcar entry reached (CAR still live, inherited stack).
    "movb $0x70, %al",
    "outb %al, $0x80",
    postcar_uart_checkpoint!("0x70"), // p
    "call _car_teardown",
    postcar_uart_checkpoint!("0x74"), // t
    // Stash layout (car_teardown::PostcarMtrrStash): u32 magic, u32 count,
    // then count x (u64 base_msr, u64 mask_msr) programmed as MTRR 0..n.
    "movl ${postcar_stash}, %esi",
    "cmpl $0x54534350, (%esi)", // POSTCAR_STASH_MAGIC ("PCST")
    "jne _postcar_stash_fail",
    "movl 4(%esi), %ebp",
    "cmpl $8, %ebp",
    "ja _postcar_stash_fail",
    "addl $8, %esi",
    "movl $0x200, %ebx", // IA32_MTRR_PHYSBASE0; +2 per entry
    "1:",
    "testl %ebp, %ebp",
    "jz 2f",
    "movl %ebx, %ecx",
    "movl (%esi), %eax",
    "movl 4(%esi), %edx",
    "wrmsr", // PHYSBASE
    "incl %ebx",
    "movl %ebx, %ecx",
    "movl 8(%esi), %eax",
    "movl 12(%esi), %edx",
    "wrmsr", // PHYSMASK
    "incl %ebx",
    "addl $16, %esi",
    "decl %ebp",
    "jmp 1b",
    "2:",
    // Enable MTRRs, preserving the default type.
    "movl $0x2ff, %ecx",
    "rdmsr",
    "orl $0x800, %eax", // MTRR_DEF_TYPE_EN
    "wrmsr",
    postcar_uart_checkpoint!("0x6d"), // m
    // Re-enable caching, flush stale CAR lines, take the fresh DRAM stack.
    "movl %cr0, %eax",
    "andl $0x9fffffff, %eax", // clear CD|NW
    "movl %eax, %cr0",
    "invd",
    postcar_uart_checkpoint!("0x63"), // c
    // POST 0x71: transition done; CAR stack abandoned, DRAM stack next.
    "movb $0x71, %al",
    "outb %al, $0x80",
    "movl $_stack_top, %esp",
    "andl $-16, %esp",
    "movl $_bss_start, %edi",
    "movl $_bss_end, %ecx",
    "subl %edi, %ecx",
    "xorl %eax, %eax",
    "rep stosb",
    "movl $_data_load, %esi",
    "movl $_data_start, %edi",
    "movl $_data_end, %ecx",
    "subl %edi, %ecx",
    "cmpl %esi, %edi",
    "je 3f",
    "rep movsb",
    "3:",
    "call _setup_idt",
    // POST 0x72: postcar runtime setup complete; entering Rust.
    "movb $0x72, %al",
    "outb %al, $0x80",
    postcar_uart_checkpoint!("0x72"), // r
    call_fstart_main!(),
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

/// Jump to an absolute address (never returns). The address must be below
/// 4 GiB; there is no other address space without long mode.
pub fn jump_to(addr: u64) -> ! {
    let addr = u32::try_from(addr).expect("protected-mode jump target above 4 GiB");
    // SAFETY: caller guarantees `addr` points to valid executable code.
    unsafe {
        core::arch::asm!("jmp {0}", in(reg) addr, options(noreturn));
    }
}

/// Jump to an absolute address with a handoff pointer in `%edi`, the
/// register the long-mode variant uses (stage entries currently clear it).
pub fn jump_to_with_handoff(addr: u64, handoff_addr: usize) -> ! {
    let addr = u32::try_from(addr).expect("protected-mode jump target above 4 GiB");
    // SAFETY: caller guarantees `addr` points to a loaded RAM stage and
    // `handoff_addr` is either 0 or points to a valid serialized StageHandoff.
    unsafe {
        core::arch::asm!(
            "jmp *%eax",
            in("eax") addr,
            in("edi") handoff_addr,
            options(noreturn, nostack, preserves_flags, att_syntax),
        );
    }
}

/// Enter the kernel through the 32-bit boot protocol entry (`code32_start`).
///
/// The protocol requires flat segments with `__BOOT_CS` = 0x10 and
/// `__BOOT_DS` = 0x18, paging off, interrupts off and `%esi` = boot_params.
/// The firmware GDT uses 0x08/0x10, so load a dedicated one first.
pub(super) fn enter_linux(pm_kernel_addr: u64, zero_page: u64) -> ! {
    let entry = u32::try_from(pm_kernel_addr).expect("kernel entry above 4 GiB");
    let zero_page = u32::try_from(zero_page).expect("boot_params above 4 GiB");
    // SAFETY: the zero page is fully populated and the protected-mode kernel
    // was copied to `entry` by the caller.
    unsafe {
        core::arch::asm!(
            "cli",
            "movl {zero_page}, %esi",
            "lgdt ({gdt})",
            "movl $0x18, %ecx",
            "movw %cx, %ds",
            "movw %cx, %es",
            "movw %cx, %ss",
            "movw %cx, %fs",
            "movw %cx, %gs",
            // Far jump through a stack pointer to reload CS = __BOOT_CS.
            "pushl $0x10",
            "pushl %eax",
            "xorl %eax, %eax",
            "xorl %ebx, %ebx",
            "xorl %ecx, %ecx",
            "xorl %edx, %edx",
            "xorl %ebp, %ebp",
            "xorl %edi, %edi",
            "lret",
            gdt = in(reg) core::ptr::addr_of!(LINUX_GDT_DESC),
            zero_page = in(reg) zero_page,
            in("eax") entry,
            options(noreturn, att_syntax),
        );
    }
}

/// Null, null, `__BOOT_CS` (0x10) and `__BOOT_DS` (0x18) flat descriptors.
#[repr(C, align(8))]
struct LinuxGdt([u64; 4]);

static LINUX_GDT: LinuxGdt = LinuxGdt([
    0,
    0,
    0x00cf_9b00_0000_ffff, // 0x10: flat 32-bit code, accessed
    0x00cf_9300_0000_ffff, // 0x18: flat data, accessed
]);

#[repr(C, packed)]
struct GdtDescriptor {
    limit: u16,
    base: &'static LinuxGdt,
}

static LINUX_GDT_DESC: GdtDescriptor = GdtDescriptor {
    limit: (core::mem::size_of::<LinuxGdt>() - 1) as u16,
    base: &LINUX_GDT,
};

/// Call `entry(argument)` cdecl. Stages already run flat in 32-bit protected
/// mode with paging off, as coreboot payloads expect. Does not return:
/// should the callee return, the CPU halts.
///
/// # Safety
/// `entry` must be 32-bit code that is loaded and owns the machine from
/// here on. The callee inherits the current stack.
pub unsafe fn protected_mode_call(entry: u32, argument: u32) -> ! {
    // SAFETY: caller contract above. No stale register state reaches the
    // callee: SeaBIOS for one reads EAX/EBX as a possible multiboot handoff.
    unsafe {
        core::arch::asm!(
            "cli",
            "cld",
            "andl $-16, %esp",
            "subl $12, %esp",
            "pushl %eax",
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
            in("ecx") entry,
            in("eax") argument,
            options(noreturn, att_syntax),
        );
    }
}
