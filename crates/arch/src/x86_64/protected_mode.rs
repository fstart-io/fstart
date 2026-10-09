//! Leave long mode for a 32-bit protected-mode callee.
//!
//! coreboot payloads expect to be called like coreboot's x86_64
//! `protected_mode_call_1arg` calls them: flat 32-bit protected mode, paging
//! off, interrupts off, with one cdecl argument on the stack (the coreboot
//! table). The trampoline runs in place: like every x86 stage it lives in the
//! identity-mapped first 4 GiB, so switching paging off keeps it addressable.

use core::arch::global_asm;

global_asm!(
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
