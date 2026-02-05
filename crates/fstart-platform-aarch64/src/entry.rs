//! AArch64 entry point.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text.entry
    .global _start
_start:
    // Disable all interrupts
    msr daifset, #0xf

    // Set up stack pointer
    ldr x0, =_stack_top
    mov sp, x0

    // Clear BSS
    ldr x0, =_bss_start
    ldr x1, =_bss_end
1:
    cmp x0, x1
    b.ge 2f
    str xzr, [x0], #8
    b 1b
2:
    // Jump to Rust entry point
    bl fstart_main
    // Should never return
3:
    wfe
    b 3b
    "#
);

extern "Rust" {
    #[allow(dead_code)]
    fn fstart_main() -> !;
}
