//! AArch64 entry point for Allwinner sun50i SoCs (H5, A64).
//!
//! All 64-bit Allwinner SoCs boot in AArch32 from the Boot ROM (BROM).
//! This module implements the ARMv8 RMR (Reset Management Register)
//! warm-reset sequence to switch into AArch64.
//!
//! ## Binary layout
//!
//! The Allwinner eGON header is placed before `_start` by codegen:
//!
//! ```text
//! [0x00] ARM32 branch to _start (eGON header, .head.text)
//! [0x04] eGON.BT0 magic + header (.head.egon, 92 bytes)
//! [0x60] _start (.text.entry):
//!          dual-mode instruction: ARM32 "b start32" / AArch64 "tst x0,x0"
//!          b aa64_entry  (AArch64 branch past RMR switch data)
//!          .space 0x78   (padding, required by dual-mode encoding)
//!          RMR switch code (ARM32, encoded as .word directives)
//!          configuration data (RVBAR address, SRAMC base, _start addr)
//!        aa64_entry:
//!          normal AArch64 init → fstart_main
//! ```
//!
//! ## Cold boot flow
//!
//! 1. BROM (ARM32) loads the eGON image into SRAM, jumps to offset 0x00.
//! 2. The eGON branch at 0x00 jumps to `_start` at offset 0x60.
//! 3. In ARM32, the dual-mode instruction at `_start` branches to the
//!    RMR switch code at `_start + 0x84`.
//! 4. The RMR switch saves BROM state (for FEL return), writes `_start`
//!    to the writable RVBAR alias at `0x017000A0`, and triggers a warm
//!    reset requesting AArch64 mode.
//! 5. After warm reset, the CPU starts in AArch64 at RVBAR = `_start`.
//! 6. In AArch64, the dual-mode instruction is a harmless `tst x0, x0`.
//! 7. The next instruction `b aa64_entry` branches to the real AArch64
//!    entry point past all the ARM32 data.
//! 8. `aa64_entry` sets up the stack, clears BSS, and calls `fstart_main`.
//!
//! ## Dual-mode instruction
//!
//! The encoding `0xEA00001F` is carefully chosen to be valid in both
//! execution states:
//! - ARM32: `B #0x84` (branch forward 0x84 bytes to the RMR switch code)
//! - AArch64: `ANDS XZR, X0, X0` (harmless flag-set, falls through)
//!
//! Reference: U-Boot `arch/arm/mach-sunxi/rmr_switch.S` and
//! `arch/arm/include/asm/arch-sunxi/boot0.h`.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text.entry
    .global _start
    .global fel_stash
    .global return_to_fel

    // ===============================================================
    // _start — dual-mode entry point (AArch32 / AArch64)
    //
    // First entry (from BROM, ARM32):
    //   0xEA00001F = "b .+0x84" → branches to start32 (RMR switch)
    //
    // Second entry (after warm reset, AArch64):
    //   0xEA00001F = "tst x0, x0" → harmless NOP, falls through
    //   Next instruction: "b aa64_entry" → jumps to AArch64 init
    // ===============================================================
_start:
    // Dual-mode instruction: ARM32 branch / AArch64 NOP.
    // Must be assembled as raw .word — the AArch64 assembler cannot
    // emit ARM32 branch instructions.
    .word 0xEA00001F

    // AArch64-only: branch past RMR switch data to real entry.
    // Never executed in ARM32 (already branched at the .word above).
    b aa64_entry

    // Padding — required by the dual-mode encoding.
    // The ARM32 branch at _start targets _start + 0x84.
    // We fill the gap: 4 (dual) + 4 (b) + 0x78 (space) = 0x80,
    // then 4 bytes for the fel_stash offset = 0x84.
    .space 0x78

    // ---------------------------------------------------------------
    // Relative pointer to the fel_stash buffer.
    // Used by the ARM32 code at start32 to locate fel_stash without
    // needing an absolute address (position-independent).
    // ---------------------------------------------------------------
    .word fel_stash - .

    // ===============================================================
    // start32 — ARM32 RMR switch code
    //
    // All instructions below are pre-assembled ARM32 machine code
    // embedded as .word directives.  The AArch64 assembler treats
    // them as data.
    //
    // This code:
    //   1. Saves BROM state (SP, LR, CPSR, SCTLR, VBAR, SP_irq) to
    //      the fel_stash buffer for potential FEL return.
    //   2. Reads the SRAM version register to detect die variants
    //      that may need an alternate RVBAR address.
    //   3. Writes the address of _start to the writable RVBAR alias
    //      at 0x017000A0 (Allwinner sun50i specific).
    //   4. Triggers an RMR warm reset requesting AArch64 mode.
    //   5. Spins in WFI — the warm reset occurs before waking.
    //
    // Ported from U-Boot arch/arm/include/asm/arch-sunxi/boot0.h
    // (CONFIG_ARM_BOOT_HOOK_RMR path, without A523 ICC code).
    // ===============================================================

    // --- FEL stash: save BROM state for potential FEL return ---
    // sub  r0, pc, #12      → r0 = address of the fel_stash offset word
    .word 0xe24f000c
    // ldr  r1, [pc, #-16]   → r1 = (fel_stash - .) relative offset
    .word 0xe51f1010
    // add  r0, r0, r1       → r0 = real address of fel_stash
    .word 0xe0800001
    // str  sp, [r0]         → save BROM SP
    .word 0xe580d000
    // str  lr, [r0, #4]     → save BROM LR (return address)
    .word 0xe580e004
    // mrs  lr, CPSR         → save CPSR
    .word 0xe10fe000
    // str  lr, [r0, #8]
    .word 0xe580e008
    // mrs  lr, SP_irq       → save IRQ stack pointer
    .word 0xe101e300
    // str  lr, [r0, #20]
    .word 0xe580e014
    // mrc  p15, 0, lr, c1, c0, 0  → save SCTLR
    .word 0xee11ef10
    // str  lr, [r0, #12]
    .word 0xe580e00c
    // mrc  p15, 0, lr, c12, c0, 0 → save VBAR
    .word 0xee1cef10
    // str  lr, [r0, #16]
    .word 0xe580e010

    // --- RVBAR setup: write _start address and trigger warm reset ---
    // ldr  r1, [pc, #52]    → r1 = CONFIG_SUNXI_RVBAR_ADDRESS (0x017000A0)
    .word 0xe59f1034
    // ldr  r0, [pc, #52]    → r0 = SUNXI_SRAMC_BASE (0x01C00000)
    .word 0xe59f0034
    // ldr  r0, [r0, #36]    → r0 = SRAM_VER_REG (die variant check)
    .word 0xe5900024
    // ands r0, r0, #0xFF    → mask low byte
    .word 0xe21000ff
    // ldrne r1, [pc, #44]   → if non-zero: r1 = RVBAR_ALTERNATIVE
    .word 0x159f102c
    // ldr  r0, [pc, #44]    → r0 = _start (target address for RVBAR)
    .word 0xe59f002c
    // str  r0, [r1]         → write RVBAR
    .word 0xe5810000
    // dsb  sy               → ensure RVBAR write completes
    .word 0xf57ff04f
    // isb  sy               → synchronise pipeline
    .word 0xf57ff06f
    // mrc  p15, 0, r0, cr12, cr0, 2  → read RMR register
    .word 0xee1c0f50
    // orr  r0, r0, #3       → bit 0: request AArch64; bit 1: request reset
    .word 0xe3800003
    // mcr  p15, 0, r0, cr12, cr0, 2  → write RMR (triggers warm reset)
    .word 0xee0c0f50
    // isb  sy
    .word 0xf57ff06f
    // wfi                   → wait for warm reset
    .word 0xe320f003
    // b    @wfi             → loop (reset occurs before waking)
    .word 0xeafffffd

    // --- Configuration data (loaded by the ldr instructions above) ---
    // Writable RVBAR alias register (Allwinner sun50i specific).
    .word 0x017000A0
    // SUNXI_SRAMC_BASE — SRAM controller base for die-variant check.
    .word 0x01C00000
    // RVBAR alternative address for die variants (same for H5).
    .word 0x017000A0
    // Target address for RVBAR — linker resolves _start to the
    // absolute SRAM address where the image is loaded.
    .word _start

    // ===============================================================
    // aa64_entry — AArch64 entry point (after warm reset)
    //
    // Reached via the "b aa64_entry" instruction at _start + 0x04.
    // This is the real AArch64 initialisation sequence.
    // ===============================================================
    .balign 8
aa64_entry:
    // Disable all interrupts (DAIF: Debug, Abort, IRQ, FIQ).
    msr daifset, #0xf

    // Set up stack pointer from linker-provided symbol.
    ldr x0, =_stack_top
    mov sp, x0

    // Copy .data initializers from LMA (ROM) to VMA (RAM).
    // For XIP this copies from flash; for RAM-only it's a no-op
    // (LMA == VMA, zero-length copy).
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
    // Clear BSS.
    ldr x0, =_bss_start
    ldr x1, =_bss_end
3:
    cmp x0, x1
    b.ge 4f
    str xzr, [x0], #8
    b 3b
4:
    // Jump to Rust entry point.
    // x0 = handoff_ptr = 0 (no inter-stage handoff on first stage).
    mov x0, #0
    bl fstart_main

    // Should never return — halt.
5:
    wfe
    b 5b

    // ===============================================================
    // return_to_fel — switch back to AArch32 and return to BROM FEL
    //
    // NOT YET IMPLEMENTED for AArch64 sunxi.
    //
    // Returning to FEL from AArch64 requires:
    //   1. Writing an AArch32 FEL-return stub address to RVBAR
    //   2. Triggering RMR with AA64 bit cleared (request AArch32)
    //   3. The AArch32 stub restoring BROM state from fel_stash
    //
    // Reference: U-Boot arch/arm/cpu/armv8/fel_utils.S
    //
    // For now this halts — FEL return is a future enhancement.
    // ===============================================================
return_to_fel:
    wfe
    b return_to_fel

    // ===============================================================
    // fel_stash — saved BROM state for FEL mode return
    //
    // Written by the ARM32 RMR switch code BEFORE the mode switch
    // (and before BSS clear).  Must be in .data, not .bss.
    //
    // Layout (matches U-Boot and fstart-soc-sunxi::FelStash):
    //   +0x00: SP
    //   +0x04: LR
    //   +0x08: CPSR
    //   +0x0C: SCTLR
    //   +0x10: VBAR
    //   +0x14: SP_irq
    //   +0x18: ICC_PMR   (unused on H5, reserved)
    //   +0x1C: ICC_IGRPEN1 (unused on H5, reserved)
    // ===============================================================
    .section .data
    .balign 4
fel_stash:
    .space 32       // 8 words: SP, LR, CPSR, SCTLR, VBAR, SP_irq, +2 reserved
    "#
);

extern "Rust" {
    /// Rust entry point — generated by fstart-stage from board.ron capabilities.
    ///
    /// `handoff_ptr` is the address of a serialized `StageHandoff` from a
    /// previous stage, or 0 if this is the first/only stage.
    #[allow(dead_code)]
    fn fstart_main(handoff_ptr: usize) -> !;
}
