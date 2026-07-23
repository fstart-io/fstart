/* SPDX-License-Identifier: MIT */
/*
 * x86 AP SIPI trampoline for fstart.
 *
 * Copied by the BSP to SIPI_VECTOR_ADDR.  APs start in real mode at
 * CS = SIPI_VECTOR_ADDR >> 4, IP = 0.  The trampoline enters 32-bit
 * protected mode, claims a 0-based AP index with an atomic xadd, enables
 * long mode using the BSP-provided CR3, sets a per-AP stack, and tail-calls
 * fstart_ap_entry(index).
 */

.set SIPI_BASE, 0x8000
.set CODE32_SEL, 0x08
.set DATA_SEL, 0x10
.set CODE64_SEL, 0x18
.set CR0_PE, 1
.set CR4_PAE, (1 << 5)
.set EFER_MSR, 0xc0000080
.set EFER_LME, (1 << 8)
.set CR0_PG, (1 << 31)

.section .text, "ax"
.balign 16
.global fstart_sipi_start
.global fstart_sipi_end
.global fstart_sipi_cr3
.global fstart_sipi_entry
.global fstart_sipi_stack_base
.global fstart_sipi_stack_size
.global fstart_sipi_ap_counter

.code16
fstart_sipi_start:
	cli
	cld
	xorw %ax, %ax
	movw %ax, %ss
	movw $0x7000, %sp

	lgdtl %cs:(gdtr - fstart_sipi_start)
	movl %cr0, %eax
	orl $CR0_PE, %eax
	movl %eax, %cr0
	ljmpl *%cs:(pm_far_ptr - fstart_sipi_start)

.balign 4
.code32
prot32:
	movw $DATA_SEL, %ax
	movw %ax, %ds
	movw %ax, %es
	movw %ax, %ss
	xorl %eax, %eax
	movw %ax, %fs
	movw %ax, %gs

	/* EAX/ESI = unique 0-based AP index.  Keep it in a register; the
	 * trampoline page is shared by all APs, so a memory slot would race when
	 * multiple cores arrive at once.
	 */
	movl $1, %eax
	lock xaddl %eax, SIPI_BASE + fstart_sipi_ap_counter - fstart_sipi_start
	movl %eax, %esi

	/* ESP = stack_base + (index + 1) * stack_size. */
	movl %eax, %ebx
	incl %ebx
	movl SIPI_BASE + fstart_sipi_stack_size - fstart_sipi_start, %ecx
	imull %ecx, %ebx
	addl SIPI_BASE + fstart_sipi_stack_base - fstart_sipi_start, %ebx
	movl %ebx, %esp

	movl %cr4, %eax
	orl $CR4_PAE, %eax
	movl %eax, %cr4

	movl SIPI_BASE + fstart_sipi_cr3 - fstart_sipi_start, %eax
	movl %eax, %cr3

	movl $EFER_MSR, %ecx
	rdmsr
	orl $EFER_LME, %eax
	wrmsr

	movl %cr0, %eax
	orl $CR0_PG, %eax
	movl %eax, %cr0

	ljmpl *(SIPI_BASE + lm_far_ptr - fstart_sipi_start)

.balign 16
.code64
long64:
	movw $DATA_SEL, %ax
	movw %ax, %ds
	movw %ax, %es
	movw %ax, %ss
	xorq %rbp, %rbp
	movl %esi, %edi
	movq fstart_sipi_entry(%rip), %rax
	jmp *%rax

.balign 8
gdtr:
	.word gdt_end - gdt - 1
	.long SIPI_BASE + gdt - fstart_sipi_start

gdt:
	.quad 0x0000000000000000
	.quad 0x00cf9a000000ffff /* 32-bit flat code */
	.quad 0x00cf92000000ffff /* flat data */
	.quad 0x00af9a0000000000 /* 64-bit flat code */
gdt_end:

pm_far_ptr:
	.long SIPI_BASE + prot32 - fstart_sipi_start
	.word CODE32_SEL

lm_far_ptr:
	.long SIPI_BASE + long64 - fstart_sipi_start
	.word CODE64_SEL

.balign 8
fstart_sipi_cr3:
	.quad 0
fstart_sipi_entry:
	.quad 0
fstart_sipi_stack_base:
	.quad 0
fstart_sipi_stack_size:
	.long 0
fstart_sipi_ap_counter:
	.long 0

fstart_sipi_end:
