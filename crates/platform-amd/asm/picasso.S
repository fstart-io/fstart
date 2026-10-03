/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/* Cold-boot PSP DRAM reset entry. No Intel CAR, no flash XIP, no FSP. */
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
    xor eax, eax
    mov edi, offset _fstart_bss_start
    mov ecx, offset _fstart_bss_end
    sub ecx, edi
    rep stosb
    /* Six DRAM pages: PML4, PDPT and four 2-MiB-leaf page directories.
       Map 0..4 GiB, including FCH and ECAM. Cache remains disabled. */
    mov edi, offset _fstart_page_tables
    mov ecx, 0x6000 / 4
    rep stosd
    mov edi, offset _fstart_page_tables
    lea eax, [edi + 0x1000]
    or eax, 3
    mov [edi], eax
    lea ebx, [edi + 0x2000]
    lea edi, [edi + 0x1000]
    mov ecx, 4
1:
    mov eax, ebx
    or eax, 3
    mov [edi], eax
    add ebx, 0x1000
    add edi, 8
    loop 1b
    mov edi, offset _fstart_page_tables + 0x2000
    mov eax, 0x83
    mov ecx, 2048
2:
    mov [edi], eax
    add eax, 0x200000
    add edi, 8
    loop 2b
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
    call fstart_picasso_main
3:  cli
    hlt
    jmp 3b

.balign 8
_fstart_gdt:
    .quad 0
    .quad 0x00cf9a000000ffff /* 32-bit code */
    .quad 0x00cf92000000ffff /* flat data */
    .quad 0x00af9a000000ffff /* 64-bit code */
.global _fstart_gdt_pointer
_fstart_gdt_pointer:
    .word 4 * 8 - 1
    .long _fstart_gdt
