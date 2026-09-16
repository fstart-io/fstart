//! S3 wake trampoline: from 64-bit long mode back to 16-bit real mode, then
//! a far jump to the OS firmware waking vector (ACPI S3 resume).
//!
//! Mirrors coreboot's `arch/x86/wakeup.S`, but the blob carries its own GDT
//! instead of depending on firmware GDT slots. The blob is position
//! independent: it is copied to [`WAKEUP_BASE`] and executes there, with the
//! OS vector patched into its final `ljmp` by [`jump_to_wakeup_vector`].
//!
//! Requires the identity-mapped page tables all x86 stages run with.

use core::arch::global_asm;

global_asm!(
    ".section .text.s3wake, \"ax\"",
    ".code64",
    ".global _s3wake_blob_start",
    "_s3wake_blob_start:",
    "cli",
    // Load the embedded GDT. Plain RIP-relative references stay valid after
    // the copy: the blob is relocated as a whole, so instruction-to-target
    // distances are unchanged.
    "lea _s3wake_gdt_desc(%rip), %rax",
    "lgdt (%rax)",
    // Far return into the 32-bit compat segment (selector 0x08): pushes CS
    // then RIP; lretq pops RIP first.
    "pushq $0x08",
    "lea _s3wake_compat(%rip), %rax",
    "pushq %rax",
    "lretq",
    ".code32",
    "_s3wake_compat:",
    "mov $0x10, %eax",
    "mov %eax, %ds",
    "mov %eax, %es",
    "mov %eax, %ss",
    // Paging off (CR0.PG = 0); execution continues at the same low physical
    // address, which the identity map covers.
    "mov %cr0, %eax",
    "andl $0x7fffffff, %eax",
    "mov %eax, %cr0",
    // Long mode off (EFER.LME = 0).
    "mov $0xc0000080, %ecx",
    "rdmsr",
    "andl $0xfffffeff, %eax",
    "wrmsr",
    // Into 16-bit protected mode (selector 0x18, limit 64 KiB, base 0).
    "ljmp $0x18, $(_s3wake_prot16 - _s3wake_blob_start + 0x600)",
    ".code16",
    "_s3wake_prot16:",
    "mov $0x20, %ax",
    "mov %ax, %ds",
    "mov %ax, %es",
    "mov %ax, %ss",
    "mov %ax, %fs",
    "mov %ax, %gs",
    // Protection off (CR0.PE = 0) → real mode.
    "movl %cr0, %eax",
    "andl $0xfffffffe, %eax",
    "movl %eax, %cr0",
    // Far jump to load CS = 0 (ptr16:16).
    ".byte 0xea",
    ".word (_s3wake_real - _s3wake_blob_start + 0x600)",
    ".word 0x0000",
    "_s3wake_real:",
    "xorw %ax, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movw %ax, %fs",
    "movw %ax, %gs",
    // Bring-up marker: prove real mode plus a working COM1 before handing
    // control to the OS vector. The console left COM1 at 8N1.
    "movw $0x03f8, %dx",
    "movb $'W', %al",
    "outb %al, %dx",
    // Far jump to the OS waking vector (offset/segment patched at runtime).
    ".byte 0xea",
    ".global _s3wake_vec_off",
    "_s3wake_vec_off:",
    ".word 0x0000",
    ".global _s3wake_vec_seg",
    "_s3wake_vec_seg:",
    ".word 0x0000",
    // Embedded GDT. .align within the blob is safe: the blob base 0x600 is
    // 16-byte aligned, and the section is linked 8-aligned.
    ".align 8",
    "_s3wake_gdt:",
    ".quad 0x0000000000000000", // 0x00 null
    ".quad 0x00cf9a000000ffff", // 0x08 code32 compat: base 0, 4 GiB, D=1, L=0
    ".quad 0x00cf92000000ffff", // 0x10 data32: base 0, 4 GiB
    ".quad 0x00009a000000ffff", // 0x18 code16: base 0, 64 KiB, D=0
    ".quad 0x000092000000ffff", // 0x20 data16: base 0, 64 KiB
    "_s3wake_gdt_end:",
    ".align 8",
    // 64-bit lgdt operand: 2-byte limit + 8-byte base.
    "_s3wake_gdt_desc:",
    ".word _s3wake_gdt_end - _s3wake_gdt - 1",
    ".quad (_s3wake_gdt - _s3wake_blob_start + 0x600)",
    ".global _s3wake_blob_end",
    "_s3wake_blob_end:",
    options(att_syntax),
);

/// Physical address the trampoline executes from. Clear of the IVT/BDA (below
/// 0x500) and the SIPI page (0x8000); matches coreboot's `WAKEUP_BASE`.
pub const WAKEUP_BASE: usize = 0x600;

unsafe extern "C" {
    static _s3wake_blob_start: u8;
    static _s3wake_blob_end: u8;
    static _s3wake_vec_off: u8;
    static _s3wake_vec_seg: u8;
}

/// Copy the trampoline to [`WAKEUP_BASE`], patch in the OS wake vector and
/// transfer control to it. Never returns; on return the vector was invalid
/// for the CPU and the machine is in an undefined state.
///
/// `vector` is the physical address of the OS's 16-bit real-mode resume code
/// (`FACS.firmware_waking_vector`); must be below 1 MiB.
pub fn jump_to_wakeup_vector(vector: u32) -> ! {
    assert!(vector != 0 && vector < 0x10_0000);
    let start = core::ptr::addr_of!(_s3wake_blob_start) as usize;
    let end = core::ptr::addr_of!(_s3wake_blob_end) as usize;
    let size = end - start;
    assert!(WAKEUP_BASE + size <= 0x8000);
    // SAFETY: WAKEUP_BASE is conventional memory reserved by the platform
    // e820; the blob fits below the SIPI page. Patching happens before entry.
    unsafe {
        core::ptr::copy_nonoverlapping(start as *const u8, WAKEUP_BASE as *mut u8, size);
        let off = WAKEUP_BASE + (core::ptr::addr_of!(_s3wake_vec_off) as usize - start);
        let seg = WAKEUP_BASE + (core::ptr::addr_of!(_s3wake_vec_seg) as usize - start);
        (off as *mut u16).write_volatile((vector & 0xF) as u16);
        (seg as *mut u16).write_volatile((vector >> 4) as u16);
        core::arch::asm!("jmp {0}", in(reg) WAKEUP_BASE as u64, options(noreturn));
    }
}
