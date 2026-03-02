//! ARMv7-A entry point — reset vector and early init.
//!
//! Provides the `_start` symbol placed at the entry point by the linker
//! script.  The entire pre-Rust boot sequence matches U-Boot SPL for
//! Allwinner sunxi (sun7i / A20) instruction-for-instruction.
//!
//! Boot flow (mirrors U-Boot `arch/arm/cpu/armv7/start.S`):
//!
//!   1. `save_boot_params` — save BROM state for FEL return
//!   2. Virtualization Extensions check (LPAE)
//!   3. HYP mode check, switch to SVC mode, disable IRQ/FIQ
//!   4. Clear SCTLR.V, set VBAR = `_start`
//!   5. `cpu_init_cp15` — ACTLR.SMP, invalidate caches/TLB/BP, SCTLR
//!   6. `cpu_init_crit` → `lowlevel_init` → `s_init` (NOP on sunxi)
//!   7. Stack setup, BSS clear, `bl fstart_main`
//!
//! FEL support (mirrors `arch/arm/cpu/armv7/sunxi/fel_utils.S`):
//!
//!   - `save_boot_params`: saves sp, lr, CPSR, SCTLR, VBAR to `fel_stash`
//!   - `return_to_fel`: restores BROM state and returns via saved lr
//!
//! On Allwinner SoCs the BROM jumps to the eGON header's branch
//! instruction at offset 0x00 which branches to `_start` at offset 0x60.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text.entry
    .global _start
    .global save_boot_params_ret
    .global return_to_fel
    .global fel_stash
    .arm

    // ===============================================================
    // ARM exception vector table — placed at _start (0x60).
    //
    // The eGON header at 0x00 branches here.  VBAR is set to _start
    // so these vectors are live for any exception during boot.
    //
    // Matches U-Boot SPL exactly: reset uses direct branch, all other
    // vectors use indirect ldr pc through an address table.
    // ===============================================================
_start:
    b       reset                       @ 0x00: Reset
    ldr     pc, _undefined_addr         @ 0x04: Undefined Instruction
    ldr     pc, _swi_addr               @ 0x08: Software Interrupt (SVC)
    ldr     pc, _prefetch_addr          @ 0x0C: Prefetch Abort
    ldr     pc, _data_addr              @ 0x10: Data Abort
    ldr     pc, _reserved_addr          @ 0x14: Reserved
    ldr     pc, _irq_addr               @ 0x18: IRQ
    ldr     pc, _fiq_addr               @ 0x1C: FIQ

    // Handler address table — all point to _exception_hang.
    // U-Boot SPL: 7 entries at +0x20 through +0x38, then 0xdeadbeef.
_undefined_addr:    .word _exception_hang
_swi_addr:          .word _exception_hang
_prefetch_addr:     .word _exception_hang
_data_addr:         .word _exception_hang
_reserved_addr:     .word _exception_hang
_irq_addr:          .word _exception_hang
_fiq_addr:          .word _exception_hang
                    .word 0xdeadbeef    @ U-Boot marker

_exception_hang:
    b       _exception_hang

    // ===============================================================
    // Reset handler
    //
    // Matches U-Boot arch/arm/cpu/armv7/start.S:reset exactly.
    //
    // Flow: save_boot_params -> LPAE/VirtEx check -> mode setup ->
    //       SCTLR/VBAR -> cpu_init_cp15 -> cpu_init_crit -> main
    // ===============================================================
reset:
    @ Save handoff pointer from r0 BEFORE anything else.
    @ For the first stage (loaded by BROM), r0 is garbage — the
    @ receiving fstart_main checks the handoff magic to detect this.
    @ For subsequent stages, jump_to_with_handoff sets r0 to the
    @ serialized StageHandoff address in DRAM.
    @ r6 is callee-saved and not clobbered until _fstart_crt0.
    mov     r6, r0

    @ Allow the board to save important registers
    b       save_boot_params
save_boot_params_ret:

    // --- Virtualization Extensions check (CONFIG_ARMV7_LPAE) ---
    //
    // Cortex-A7 has VirtEx, so this check matches and branches to
    // switch_to_hypervisor (which is a NOP — just branches back).
    // Matches U-Boot binary at save_boot_params_ret.
    mrc     p15, 0, r0, c0, c1, 1      @ read ID_PFR1
    and     r0, r0, #0xf000            @ mask virtualization bits [15:12]
    cmp     r0, #0x1000                @ VirtEx == 1?
    beq     switch_to_hypervisor
switch_to_hypervisor_ret:

    // --- SVC mode, interrupts disabled ---
    //
    // Check for HYP mode first — must not use cpsid because that
    // cannot switch OUT of HYP mode.  Matches U-Boot exactly.
    mrs     r0, cpsr
    and     r1, r0, #0x1f              @ mask mode bits
    teq     r1, #0x1a                  @ test for HYP mode
    bicne   r0, r0, #0x1f             @ clear all mode bits (if not HYP)
    orrne   r0, r0, #0x13             @ set SVC mode
    orr     r0, r0, #0xc0             @ disable FIQ and IRQ
    msr     cpsr, r0

    // --- Set V=0 in CP15 SCTLR register (for VBAR to point to vector) ---
    mrc     p15, 0, r0, c1, c0, 0      @ Read CP15 SCTLR Register
    bic     r0, r0, #0x2000            @ V = 0
    mcr     p15, 0, r0, c1, c0, 0      @ Write CP15 SCTLR Register

    // --- Set vector address in CP15 VBAR register ---
    ldr     r0, =_start
    mcr     p15, 0, r0, c12, c0, 0     @ Set VBAR

    // --- The mask ROM code should have PLL and others stable ---
    bl      cpu_init_cp15
    bl      cpu_init_crit

    bl      _fstart_crt0

    // ---------------------------------------------------------------
    // switch_to_hypervisor — NOP on sunxi.
    //
    // Matches U-Boot: WEAK(switch_to_hypervisor) just branches back.
    // Placed here (before cpu_init_cp15) to match U-Boot binary layout.
    // ---------------------------------------------------------------
switch_to_hypervisor:
    b       switch_to_hypervisor_ret

    // ===============================================================
    // cpu_init_cp15 — Setup CP15 registers (cache, MMU, TLBs)
    //
    // Matches U-Boot arch/arm/cpu/armv7/start.S:cpu_init_cp15 exactly
    // for a Cortex-A7 build (ARMV7_SET_CORTEX_SMPEN, no errata).
    // ===============================================================
cpu_init_cp15:
    // The Arm Cortex-A7 TRM says this bit must be enabled before
    // any cache or TLB maintenance operations are performed.
    mrc     p15, 0, r0, c1, c0, 1      @ read auxiliary control register
    orr     r0, r0, #(1 << 6)          @ set SMP bit to enable coherency
    mcr     p15, 0, r0, c1, c0, 1      @ write auxiliary control register

    // Invalidate L1 I/D
    mov     r0, #0                      @ set up for MCR
    mcr     p15, 0, r0, c8, c7, 0      @ invalidate TLBs
    mcr     p15, 0, r0, c7, c5, 0      @ invalidate icache
    mcr     p15, 0, r0, c7, c5, 6      @ invalidate BP array
    dsb
    isb

    // Disable MMU stuff and caches
    mrc     p15, 0, r0, c1, c0, 0
    bic     r0, r0, #0x00002000         @ clear bits 13 (--V-)
    bic     r0, r0, #0x00000007         @ clear bits 2:0 (-CAM)
    orr     r0, r0, #0x00000002         @ set bit 1 (--A-) Align
    orr     r0, r0, #0x00000800         @ set bit 11 (Z---) BTB
    orr     r0, r0, #0x00001000         @ set bit 12 (I) I-cache
    mcr     p15, 0, r0, c1, c0, 0

    // Read MIDR — U-Boot extracts variant + revision for errata.
    // No errata apply to Cortex-A7, but we match the sequence.
    mov     r5, lr                      @ Store my Caller
    mrc     p15, 0, r1, c0, c0, 0      @ r1 = Read Main ID Register (MIDR)
    mov     r3, r1, lsr #20            @ get variant field
    and     r3, r3, #0xf               @ r3 has CPU variant
    and     r4, r1, #0xf               @ r4 has CPU revision
    mov     r2, r3, lsl #4             @ shift variant field for combined value
    orr     r2, r4, r2                  @ r2 has combined CPU variant + revision

    // Early stack for ERRATA that need to call C code
    // SYS_INIT_SP_ADDR = SRAM base + 32K = 0x8000 on sunxi
    ldr     r0, =0x8000
    bic     r0, r0, #7                 @ 8-byte alignment for ABI compliance
    mov     sp, r0

    mov     pc, r5                      @ back to my caller

    // ===============================================================
    // cpu_init_crit -> lowlevel_init -> s_init
    //
    // Matches U-Boot arch/arm/cpu/armv7/start.S:cpu_init_crit and
    // arch/arm/cpu/armv7/lowlevel_init.S exactly for sunxi SPL.
    //
    // cpu_init_crit: just branches to lowlevel_init
    // lowlevel_init: sets sp, r9, calls s_init
    // s_init: empty on sunxi (just bx lr)
    // ===============================================================
cpu_init_crit:
    b       lowlevel_init               @ go setup pll,mux,memory

lowlevel_init:
    // Setup a temporary stack. Global data is not available yet.
    ldr     sp, =0x8000                 @ SYS_INIT_SP_ADDR
    bic     sp, sp, #7                  @ 8-byte alignment for ABI compliance

    // fstart has no global data struct — clear r9.
    // U-Boot SPL would do: ldr r9, =gdata
    mov     r9, #0

    // Save the old lr (passed in ip) and the current lr to stack
    push    {{ip, lr}}

    // Call the very early init function. On sunxi SPL, s_init is
    // empty — just bx lr.
    bl      s_init
    pop     {{ip, pc}}

s_init:
    bx      lr

    // ===============================================================
    // _fstart_crt0 — C/Rust runtime entry (replaces U-Boot's _main)
    //
    // Sets up final stack, clears BSS, calls fstart_main.
    //
    // U-Boot's _main does GD allocation + board_init_f_alloc_reserve
    // + board_init_f_init_reserve before calling board_init_f.  fstart
    // handles all init in Rust, so we just set sp and clear BSS.
    // ===============================================================
_fstart_crt0:
    // Set up initial C/Rust runtime environment.
    // _stack_top is defined by the linker script at the top of the
    // memory region containing this stage.  For the bootblock (SRAM)
    // this resolves to 0x8000; for later stages (DRAM) it is at the
    // top of the DRAM region.
    ldr     r0, =_stack_top
    bic     r0, r0, #7                 @ 8-byte alignment for ABI compliance
    mov     sp, r0

    // Clear BSS — matches U-Boot crt0.S CLEAR_BSS macro
    ldr     r0, =_bss_start
    ldr     r1, =_bss_end
    mov     r2, #0x00000000             @ prepare zero to clear BSS
1:
    cmp     r0, r1                      @ while not at end of BSS
    strlo   r2, [r0]                    @ clear 32-bit BSS word
    addlo   r0, r0, #4                 @ move to next
    blo     1b

    // Call Rust entry point with handoff pointer (saved in r6 at reset).
    // For the first stage this is garbage (BROM value) — fstart_main
    // validates via magic check.  For subsequent stages this is the
    // serialized StageHandoff address set by jump_to_with_handoff.
    mov     r0, r6
    bl      fstart_main

    // Should never return — halt
2:
    wfe
    b       2b

    // ===============================================================
    // save_boot_params — Allwinner sunxi FEL mode support
    //
    // Saves the BROM's state (sp, lr, CPSR, SCTLR, VBAR) so we can
    // return to FEL mode later via return_to_fel.
    //
    // Matches U-Boot arch/arm/cpu/armv7/sunxi/fel_utils.S exactly.
    //
    // Called as the FIRST thing from the reset handler, BEFORE any
    // other initialization.  Stack is not yet initialized — must not
    // save anything to stack even if compiled with -O0.
    // ===============================================================
save_boot_params:
    ldr     r0, =fel_stash
    str     sp, [r0, #0]
    str     lr, [r0, #4]
    mrs     lr, cpsr                    @ Read CPSR
    str     lr, [r0, #8]
    mrc     p15, 0, lr, c1, c0, 0      @ Read CP15 SCTLR Register
    str     lr, [r0, #12]
    mrc     p15, 0, lr, c12, c0, 0     @ Read VBAR
    str     lr, [r0, #16]
    b       save_boot_params_ret

    // ===============================================================
    // return_to_fel — Return to BROM FEL mode
    //
    // Restores the saved BROM state and returns via saved lr.
    // Called from Rust: return_to_fel(saved_sp: u32, saved_lr: u32)
    //
    // Matches U-Boot arch/arm/cpu/armv7/sunxi/fel_utils.S exactly.
    // ===============================================================
return_to_fel:
    mov     sp, r0
    mov     lr, r1
    ldr     r0, =fel_stash
    ldr     r1, [r0, #16]
    mcr     p15, 0, r1, c12, c0, 0     @ Write VBAR
    ldr     r1, [r0, #12]
    mcr     p15, 0, r1, c1, c0, 0      @ Write CP15 SCTLR Register
    ldr     r1, [r0, #8]
    msr     cpsr, r1                    @ Write CPSR
    bx      lr

    // Literal pool for all ldr =<const/sym> above
    .ltorg

    // ===============================================================
    // fel_stash — saved BROM state for FEL mode return
    //
    // Must be in .data (not .bss) because save_boot_params writes
    // to it BEFORE BSS is cleared.
    //
    // Layout: sp, lr, cpsr, sctlr, vbar — 5 words.
    // ===============================================================
    .section .data
    .align 2
fel_stash:
    .space 20                           @ 5 words: sp, lr, cpsr, sctlr, vbar
    "#
);

extern "Rust" {
    /// Rust entry point — generated by fstart-stage from board.ron capabilities.
    ///
    /// `handoff_ptr` is the address of a serialized [`StageHandoff`] in DRAM,
    /// passed via `r0` by the previous stage's `jump_to_with_handoff`. For
    /// the first stage (loaded by BROM), this is garbage — the generated code
    /// validates it via magic check before use.
    #[allow(dead_code)]
    fn fstart_main(handoff_ptr: usize) -> !;
}
