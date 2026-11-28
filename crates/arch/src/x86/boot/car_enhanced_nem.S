/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Skylake/Kaby Lake client enhanced NEM, adapted from coreboot
 * src/soc/intel/common/block/cpu/car/cache_as_ram.S.
 *
 * Enter in flat 32-bit protected mode, with interrupts disabled and the BSP
 * owning reset/ACM state. EBP is the outer continuation. No stack, calls or
 * spills are permitted until _car_enhanced_ready. Existing Core2/Atom helpers
 * below use ESP only as a register holding a JMP return address.
 *
 * The target linker must provide a naturally aligned, power-of-two CAR region
 * (>=4 KiB, below 4 GiB) and a stack top inside it. This implementation rejects
 * other geometry rather than misencoding an MTRR. Keep ROM UC during the
 * non-ACM CAR fill. The bootblock adds BIOS WP after verifying the live decode.
 */
.section .text.asm._car_enhanced_nem, "ax"
.code32
.global _car_enhanced_nem
_car_enhanced_nem:
    /* Do not apply Intel client MSRs to another vendor's matching model. */
    xorl %eax, %eax
    cpuid
    cmpl $4, %eax
    jb _car_enhanced_fail
    cmpl $0x756e6547, %ebx
    jne _car_enhanced_fail
    cmpl $0x49656e69, %edx
    jne _car_enhanced_fail
    cmpl $0x6c65746e, %ecx
    jne _car_enhanced_fail
    movl $1, %eax
    cpuid
    movl %eax, %edx
    andl $0x0fff0ff0, %eax
    andl $15, %edx
    cmpl $0x406e0, %eax
    je _car_enhanced_skl_mobile
    cmpl $0x506e0, %eax
    je _car_enhanced_skl_desktop
    cmpl $0x806e0, %eax
    je _car_enhanced_kbl
    cmpl $0x906e0, %eax
    jne _car_enhanced_fail
_car_enhanced_kbl:
    cmpl $9, %edx
    je _car_enhanced_geometry
    cmpl $10, %edx
    je _car_enhanced_geometry
    jmp _car_enhanced_fail
_car_enhanced_skl_mobile:
    cmpl $8, %edx
    je _car_enhanced_geometry
    cmpl $1, %edx
    jb _car_enhanced_fail
    cmpl $3, %edx
    jbe _car_enhanced_geometry
    jmp _car_enhanced_fail
_car_enhanced_skl_desktop:
    cmpl $8, %edx
    je _car_enhanced_geometry
    cmpl $3, %edx
    ja _car_enhanced_fail
_car_enhanced_geometry:
    movl $_car_size, %eax
    cmpl $4096, %eax
    jb _car_enhanced_fail
    leal -1(%eax), %ecx
    testl %eax, %ecx
    jnz _car_enhanced_fail
    movl $_car_base, %edx
    testl %ecx, %edx
    jnz _car_enhanced_fail
    addl %eax, %edx
    jc _car_enhanced_fail
    movl $_ecar_stack, %eax
    cmpl %edx, %eax
    ja _car_enhanced_fail
    andl $0xfffffff0, %eax    /* Validate the ESP value actually installed. */
    cmpl $_car_base, %eax
    jbe _car_enhanced_fail
    movl $0x80000000, %eax
    cpuid
    cmpl $0x80000008, %eax
    jb _car_enhanced_fail
    movl $0x80000008, %eax
    cpuid
    andl $0xff, %eax
    cmpl $33, %eax
    jb _car_enhanced_fail
    cmpl $63, %eax
    ja _car_enhanced_fail
    subl $32, %eax
    movl %eax, %ecx
    movl $1, %esi
    shll %cl, %esi
    decl %esi                 /* Physical mask high; survives MTRR helpers. */

    movl $0xfe, %ecx
    rdmsr
    testb %al, %al
    jz _car_enhanced_fail
    /* Boot Guard ACM already owns NEM and may own active MTRRs. */
    movl $0x13a, %ecx
    rdmsr
    andl $1, %eax
    movl %eax, %edi           /* ACM flag; preserved until MTRR setup. */
    jz _car_enhanced_cold
    movl $0x139, %ecx         /* Stop the ACM PBE timer. */
    movl $1, %eax
    xorl %edx, %edx
    wrmsr
    jmp _car_enhanced_mtrr
_car_enhanced_cold:
    movl $0x2ff, %ecx
    rdmsr
    testl $0xc00, %eax        /* E or FE means reset was not clean. */
    jz _car_enhanced_clear
    movw $0xcf9, %dx          /* Hard reset: SYS_RST, then RST_CPU. */
    movb $2, %al
    outb %al, %dx
    movb $6, %al
    outb %al, %dx
    jmp _car_enhanced_fail
_car_enhanced_clear:
    /* _car_clear_fixed_mtrrs and _car_clear_var_mtrrs clobber ESI. */
    movl %esi, %ebx
    movl $_car_enhanced_fixed_done, %esp
    jmp _car_clear_fixed_mtrrs
_car_enhanced_fixed_done:
    movl $_car_enhanced_variable_done, %esp
    jmp _car_clear_var_mtrrs
_car_enhanced_variable_done:
    movl %ebx, %esi
    movl $0x2ff, %ecx
    rdmsr
    andl $0xfffff300, %eax
    wrmsr
_car_enhanced_mtrr:
    /* Find a free variable MTRR without destroying ACM-owned mappings. */
    movl $0xfe, %ecx
    rdmsr
    movzbl %al, %ebx
    movl $0x201, %ecx
_car_enhanced_find_mtrr:
    testl %ebx, %ebx
    jz _car_enhanced_fail
    rdmsr
    testl $0x800, %eax
    jz _car_enhanced_free_mtrr
    addl $2, %ecx
    decl %ebx
    jmp _car_enhanced_find_mtrr
_car_enhanced_free_mtrr:
    decl %ecx
    movl $_car_base, %eax
    orl $6, %eax
    xorl %edx, %edx
    wrmsr
    incl %ecx
    movl $_car_size, %eax
    negl %eax
    orl $0x800, %eax
    movl %esi, %edx
    wrmsr
    testl %edi, %edi
    jz _car_enhanced_enable
    /* The ACM route must not reprogram its NEM/CLOS state. */
    movl $_car_base, %edi
    movl $_car_size, %ecx
    shrl $6, %ecx
    xorl %eax, %eax
_car_enhanced_acm_fill:
    movl %eax, (%edi)
    sfence
    addl $64, %edi
    loop _car_enhanced_acm_fill
    movl $_car_enhanced_ready, %esp
    jmp _car_fill_stosl
_car_enhanced_enable:
    movl $_car_enhanced_cache, %esp
    jmp _car_enable_mtrrs
_car_enhanced_cache:
    movl $_car_enhanced_setup, %esp
    jmp _car_enable_cache
_car_enhanced_setup:
    movl $0x2e0, %ecx
    rdmsr
    orl $1, %eax              /* SETUP precedes CLOS/mask programming. */
    wrmsr
    xorl %edi, %edi
_car_enhanced_find_llc:
    cmpl $32, %edi            /* Finite enumeration, unlike source loop. */
    jae _car_enhanced_fail
    movl %edi, %ecx
    movl $4, %eax
    cpuid
    incl %edi
    movl %eax, %edx
    andl $31, %edx
    jz _car_enhanced_fail
    andl $0xe0, %eax
    cmpl $0x60, %eax
    jne _car_enhanced_find_llc
    cmpl $3, %edx            /* Require a unified L3 cache. */
    jne _car_enhanced_fail
    /* Way size = (line+1)*(sets+1)*(partitions+1), without rounding. */
    movl %ebx, %eax
    andl $0xfff, %eax
    incl %eax
    incl %ecx
    jz _car_enhanced_fail
    mull %ecx
    testl %edx, %edx
    jnz _car_enhanced_fail
    movl %ebx, %edx
    shrl $12, %edx
    andl $0x3ff, %edx
    incl %edx
    mull %edx
    testl %edx, %edx
    jnz _car_enhanced_fail
    testl %eax, %eax
    jz _car_enhanced_fail
    shrl $22, %ebx
    incl %ebx
    cmpl $32, %ebx
    ja _car_enhanced_fail
    movl %eax, %ecx
    movl $_car_size, %eax
    xorl %edx, %edx
    divl %ecx
    testl %edx, %edx
    jz _car_enhanced_data_ways
    incl %eax
_car_enhanced_data_ways:
    testl %eax, %eax
    jz _car_enhanced_fail
    /* Keep at least one non-data way for subsequent allocations. */
    cmpl %ebx, %eax
    jae _car_enhanced_fail
    movl %eax, %ecx
    movl $1, %esi
    shll %cl, %esi
    decl %esi                 /* Data mask; data_ways is at most 31. */
    movl %ebx, %ecx
    movl $-1, %eax
    cmpl $32, %ecx
    je _car_enhanced_masks
    movl $1, %eax
    shll %cl, %eax
    decl %eax
_car_enhanced_masks:
    xorl %esi, %eax           /* all_mask & ~data_mask */
    xorl %edx, %edx
    movl $0xc91, %ecx
    wrmsr
    movl %esi, %eax
    movl $0xc92, %ecx
    wrmsr
    movl $2, %eax             /* SKL/KBL CLOS is in the LOW dword. */
    movl $0xc8f, %ecx
    wrmsr
    movl $_car_enhanced_filled, %esp
    jmp _car_fill_stosl
_car_enhanced_filled:
    movl $1, %eax
    xorl %edx, %edx
    movl $0xc8f, %ecx
    wrmsr
    movl $0x2e0, %ecx
    rdmsr
    orl $2, %eax              /* RUN only after protected CAR is populated. */
    wrmsr
_car_enhanced_ready:
    movl $_ecar_stack, %esp
    andl $0xfffffff0, %esp
    jmp _car_done
_car_enhanced_fail:
    movb $0xed, %al
    outb %al, $0x80
    cli
_car_enhanced_halt:
    hlt
    jmp _car_enhanced_halt
