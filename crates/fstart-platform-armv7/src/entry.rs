//! ARMv7 entry point — reset vector and early init.
//!
//! Provides the `_start` symbol placed at the reset vector by the linker script.
//! Saves the DTB address from QEMU (passed in `r2`), sets up the stack,
//! clears BSS, and jumps to `fstart_main`.
//!
//! On QEMU ARM virt with `-bios`, the CPU starts at 0x0 in SVC mode.
//! QEMU passes: r0=0, r1=machine_type, r2=DTB address.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text.entry
    .global _start
    .arm
_start:
    // Save boot arguments from QEMU before any register is clobbered.
    // QEMU ARM virt passes: r0=0, r1=machine_type, r2=DTB address.
    mov r4, r2              // save DTB address in callee-saved register

    // Disable all interrupts (mask IRQ and FIQ)
    cpsid if

    // Set up stack pointer (grows downward)
    ldr sp, =_stack_top

    // Copy .data initializers from ROM to RAM.
    // _data_load = LMA (ROM), _data_start/_data_end = VMA (RAM).
    ldr r0, =_data_load
    ldr r1, =_data_start
    ldr r2, =_data_end
1:
    cmp r1, r2
    bge 2f
    ldr r3, [r0], #4
    str r3, [r1], #4
    b 1b
2:
    // Clear BSS section (word-aligned)
    ldr r0, =_bss_start
    ldr r1, =_bss_end
    mov r2, #0
3:
    cmp r0, r1
    bge 4f
    str r2, [r0], #4
    b 3b
4:
    // Store boot DTB address to global (after BSS is cleared to zero)
    ldr r0, =BOOT_DTB_ADDR
    str r4, [r0]

    // Jump to Rust entry point
    bl fstart_main
    // Should never return; spin if it does
3:
    wfe
    b 3b
    "#
);

extern "Rust" {
    #[allow(dead_code)]
    fn fstart_main() -> !;
}
