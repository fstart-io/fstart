// Allwinner D1 BROM enters the eGON image at 0x0002_0000 in M-mode.
// Configure the C906's memory attributes before MMIO (as oreboot/U-Boot do).
// MHCR/MHINT retain BROM defaults. Later stages repeat this idempotent setup.
.equ CSR_MXSTATUS, 0x7C0
.equ CSR_MCOR, 0x7C2

.section .text.entry
.global _start
_start:
    csrw mie, zero
    // MXSTATUS: T-Head ISA extensions and memory attribute extension enable.
    li t1, 0x1 << 22 | 0x1 << 21
    csrs CSR_MXSTATUS, t1
    // Invalidate I-cache, D-cache, BTB and BHT.
    li t2, 0x30013
    csrw CSR_MCOR, t2
    // Preserve the handoff pointer and secondary boot argument through init.
    mv s1, a0
    csrw mscratch, a1
    la sp, _stack_top
    la t0, _data_load
    la t1, _data_start
    la t2, _data_end
1:
    bgeu t1, t2, 2f
    ld t3, 0(t0)
    sd t3, 0(t1)
    addi t0, t0, 8
    addi t1, t1, 8
    j 1b
2:
    la t0, _bss_start
    la t1, _bss_end
3:
    bgeu t0, t1, 4f
    sd zero, 0(t0)
    addi t0, t0, 8
    j 3b
4:
    mv a0, s1
    call fstart_main
5:
    wfi
    j 5b
