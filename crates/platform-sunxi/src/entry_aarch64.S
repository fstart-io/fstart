// Allwinner sun50i (H5/A64) AArch32 BROM to AArch64 RMR entry.
// Ported from U-Boot arch/arm/mach-sunxi/rmr_switch.S and boot0.h.
// Later stages enter in AArch64 with a handoff pointer in x0.
.section .text.entry
.global _start
.global fstart_sunxi_fel_stash
_start:
    // Dual-mode: ARM32 b .+0x84 / AArch64 ands xzr,x0,x0.
    .word 0xEA00001F
    b aa64_entry
    .space 0x78
    .word fstart_sunxi_fel_stash - .

    // Save BROM SP, LR, CPSR, SCTLR, VBAR and SP_irq before warm reset.
    .word 0xe24f000c
    .word 0xe51f1010
    .word 0xe0800001
    .word 0xe580d000
    .word 0xe580e004
    .word 0xe10fe000
    .word 0xe580e008
    .word 0xe101e300
    .word 0xe580e014
    .word 0xee11ef10
    .word 0xe580e00c
    .word 0xee1cef10
    .word 0xe580e010
    .word 0xe59f1034
    .word 0xe59f0034
    .word 0xe5900024
    .word 0xe21000ff
    .word 0x159f102c
    .word 0xe59f002c
    .word 0xe5810000
    .word 0xf57ff04f
    .word 0xf57ff06f
    .word 0xee1c0f50
    .word 0xe3800003
    .word 0xee0c0f50
    .word 0xf57ff06f
    .word 0xe320f003
    .word 0xeafffffd

    .word 0x017000A0   // writable RVBAR alias (sun50i)
    .word 0x01C00000   // SRAMC base, die-variant check
    .word 0x017000A0   // alternative RVBAR (same on H5)
    .word _start

    .balign 8
aa64_entry:
    mov x19, x0
    msr daifset, #0xf
    ldr x0, =_stack_top
    mov sp, x0
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
    ldr x0, =_bss_start
    ldr x1, =_bss_end
3:
    cmp x0, x1
    b.ge 4f
    str xzr, [x0], #8
    b 3b
4:
    mov x0, x19
    bl fstart_main
5:
    wfe
    b 5b

    // Must survive BSS clear: SP, LR, CPSR, SCTLR, VBAR, SP_irq, 2 reserved.
    .section .data
    .balign 4
fstart_sunxi_fel_stash:
    .space 32
