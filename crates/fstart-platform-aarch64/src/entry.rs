//! AArch64 entry point — reset vector and early init.
//!
//! Provides the `_start` symbol placed at the reset vector by the linker script.
//! Saves the DTB address from QEMU (passed in `x0`), sets up the stack,
//! clears BSS, and jumps to `fstart_main`.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text.entry
    .global _start
_start:
    // Save boot argument from QEMU before any register is clobbered.
    // QEMU AArch64 virt passes: x0 = DTB address.
    mov x19, x0

    // Disable all interrupts
    msr daifset, #0xf

    // Set up stack pointer
    ldr x0, =_stack_top
    mov sp, x0

    // Copy .data initializers from ROM to RAM.
    // _data_load = LMA (ROM), _data_start/_data_end = VMA (RAM).
    ldr x0, =_data_load
    ldr x1, =_data_start
    ldr x2, =_data_end
1:
    cmp x1, x2
    b.ge 2f
    ldr x3, [x0], #8
    str x3, [x1], #8
    b 1b
2:
    // Clear BSS
    ldr x0, =_bss_start
    ldr x1, =_bss_end
3:
    cmp x0, x1
    b.ge 4f
    str xzr, [x0], #8
    b 3b
4:
    // Store boot DTB address to global (after BSS is cleared to zero)
    ldr x0, =BOOT_DTB_ADDR
    str x19, [x0]

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
