/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/* Cold-boot PSP DRAM reset entry. No Intel CAR, no flash XIP, no FSP.
 * Shared by the AMD SoCs whose PSP copies the BIOS reset image to DRAM; the
 * platform defines _fstart_amd_long_mode_entry, entered in long mode with the
 * stack, BSS and the 4-GiB identity map set up. */
.section .reset,"ax",@progbits
.code16
.global _start
_start:
    jmp _fstart_start16

.section .init16,"ax",@progbits
.code16
_fstart_start16:
    cli
    cld
    /* Architectural APICBASE.BSP identifies this logical CPU, not an arrival
       ordinal. Decide before any stack/BSS/page-table initialization. Native
       SMU release enters this PSP-copied reset vector; never patch it.
       The platform may release APs ONLY after publishing/reserving the shared
       architectural bootstrap at 0x8000 and excluding early NMI/SMI delivery. */
.global _fstart_reset_dispatch
_fstart_reset_dispatch:
    mov ecx, 0x1b /* IA32_APIC_BASE */
    rdmsr
    test eax, 0x100 /* BSP bit 8 */
    jnz _fstart_bsp_start16
.global _fstart_ap_reset_jump
_fstart_ap_reset_jump:
    /* Real-mode CS:IP = 0800:0000, physical 0x8000. No stack or image writes. */
    .byte 0xea
    .word 0
    .word {ap_reset_segment}
.global _fstart_bsp_start16
_fstart_bsp_start16:
    xor eax, eax
    mov cr3, eax
    /* The reset CS hidden base is redirected by PSP into this DRAM segment.
       Use a CS-relative pointer. */
    mov ax, cs
    shl ax, 4
    mov bx, offset _fstart_gdt_offset
    sub bx, ax
    .byte 0x66 /* 32-bit GDT base: DRAM is above 16 MiB */
    lgdt cs:[bx]
    mov eax, cr0
    and eax, 0x7ffaffd1
    or eax, 0x60000001
    mov cr0, eax
    /* Far protected-mode transfer; selector 8 is flat 32-bit code. */
    .byte 0x66, 0xea
    .long _fstart_start32
    .word 8

.code32
_fstart_start32:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    mov esp, offset _fstart_stack_top
    /* .data is linked into the image and runs from the data region. */
    mov esi, offset _data_load
    mov edi, offset _data_start
    mov ecx, offset _data_end
    sub ecx, edi
    rep movsb
    xor eax, eax
    mov edi, offset _fstart_bss_start
    mov ecx, offset _fstart_bss_end
    sub ecx, edi
    rep stosb
    /* Identity map 512 GiB with 1 GiB pages (the low 4 GiB with 2 MiB
       pages without them), including FCH and ECAM, into the six DRAM
       pages at _fstart_page_tables. Cache remains disabled. */
    mov edi, offset _fstart_page_tables
    mov ecx, 1
    mov ebp, offset .Lpage_tables_done
    jmp _fstart_identity_tables
.Lpage_tables_done:
    mov eax, 0x620 /* PAE, OSFXSR, OSXMMEXCPT */
    mov cr4, eax
    mov eax, offset _fstart_page_tables
    mov cr3, eax
    mov ecx, 0xc0000080 /* EFER */
    rdmsr
    or eax, 0x100 /* LME */
    wrmsr
    mov eax, cr0
    or eax, 0x80000000 /* PG */
    mov cr0, eax
    .byte 0xea
    .long _fstart_start64
    .word 0x18

.code64
_fstart_start64:
    mov rsp, offset _fstart_stack_top
    xor rbp, rbp
    jmp _fstart_amd_long_mode_entry

.balign 8
_fstart_gdt:
    .quad 0
    /* Pre-set descriptor Accessed bits: segment loads must not modify
       the PSP-copied reset image (same rule as the shared AP trampoline). */
    .quad 0x00cf9b000000ffff /* 32-bit code, accessed */
    .quad 0x00cf93000000ffff /* flat data, accessed */
    .quad 0x00af9b000000ffff /* 64-bit code, accessed */
.global _fstart_gdt_pointer
_fstart_gdt_pointer:
    .word 4 * 8 - 1
    .long _fstart_gdt
