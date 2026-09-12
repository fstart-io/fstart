/* SPDX-License-Identifier: MIT */
/*
 * PIC x86 SMM entry stub for fstart.
 *
 * The loader copies this blob to SMBASE + 0x8000 and patches the
 * SmmEntryParams block at fstart_smm_stub_params.  Execution starts in
 * SMM real mode with IP=0x8000.  The CS hidden base is the full SMBASE, but
 * the visible selector may be truncated for high TSEG placements, so this
 * code loads the absolute copied-entry address from entry_base instead of
 * deriving it from CS.
 */

.set SMM_ENTRY_ORIGIN, 0x8000
.set CODE32_SEL, 0x08
.set DATA_SEL, 0x10
.set CODE64_SEL, 0x18
.set CR0_CLEAR_FLAGS, ((1 << 30) | (1 << 29) | (1 << 31) | (1 << 18) | (1 << 16) | (1 << 5) | (1 << 3) | (1 << 2) | (1 << 1))
.set CR4_PAE, (1 << 5)
.set EFER_MSR, 0xc0000080
.set EFER_LME, (1 << 8)
.set CR0_PG, (1 << 31)

/* Offsets in fstart_smm::runtime::SmmEntryParams. */
.set PARAM_STACK_TOP, 8
.set PARAM_COMMON_ENTRY, 16
.set PARAM_CR3, 40
.set PARAM_ENTRY_BASE, 48

.section .text, "ax"
.balign 16
.global fstart_smm_stub_start
.global fstart_smm_stub_params
.global fstart_smm_stub_end

.code16
fstart_smm_stub_start:
	cli

	/* EBX = absolute address where this entry stub was copied. */
	movl %cs:(SMM_ENTRY_ORIGIN + fstart_smm_stub_params - fstart_smm_stub_start + PARAM_ENTRY_BASE), %ebx

	/* Runtime-patch the GDT base and protected-mode far pointer. */
	movl %ebx, %eax
	addl $(gdt - fstart_smm_stub_start), %eax
	movl %eax, %cs:(SMM_ENTRY_ORIGIN + gdtr + 2 - fstart_smm_stub_start)

	movl %ebx, %eax
	addl $(prot32 - fstart_smm_stub_start), %eax
	movl %eax, %cs:(SMM_ENTRY_ORIGIN + pm_far_ptr - fstart_smm_stub_start)

	lgdtl %cs:(SMM_ENTRY_ORIGIN + gdtr - fstart_smm_stub_start)

	movl %cr0, %eax
	andl $~CR0_CLEAR_FLAGS, %eax
	orb $1, %al
	movl %eax, %cr0

	ljmpl *%cs:(SMM_ENTRY_ORIGIN + pm_far_ptr - fstart_smm_stub_start)

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

	movl %cr4, %eax
	orl $CR4_PAE, %eax
	movl %eax, %cr4

	movl (fstart_smm_stub_params - fstart_smm_stub_start + PARAM_CR3)(%ebx), %eax
	movl %eax, %cr3

	movl $EFER_MSR, %ecx
	rdmsr
	orl $EFER_LME, %eax
	wrmsr

	movl %cr0, %eax
	orl $CR0_PG, %eax
	movl %eax, %cr0

	leal (long64 - fstart_smm_stub_start)(%ebx), %eax
	movl %eax, (lm_far_ptr - fstart_smm_stub_start)(%ebx)
	ljmpl *(lm_far_ptr - fstart_smm_stub_start)(%ebx)

.balign 16
.code64
long64:
	movw $DATA_SEL, %ax
	movw %ax, %ds
	movw %ax, %es
	movw %ax, %ss

	movq fstart_smm_stub_params + PARAM_STACK_TOP(%rip), %rsp
	andq $-16, %rsp
	xorq %rbp, %rbp

	leaq fstart_smm_stub_params(%rip), %rdi
	movq fstart_smm_stub_params + PARAM_COMMON_ENTRY(%rip), %rax
	testq %rax, %rax
	jz 1f
	call *%rax
1:
	rsm
	hlt
	jmp . - 1

.balign 8
gdtr:
	.word gdt_end - gdt - 1
	.long 0

gdt:
	.quad 0x0000000000000000
	.quad 0x00cf9a000000ffff /* 32-bit flat code */
	.quad 0x00cf92000000ffff /* flat data */
	.quad 0x00af9a0000000000 /* 64-bit flat code */
gdt_end:

pm_far_ptr:
	.long 0
	.word CODE32_SEL

lm_far_ptr:
	.long 0
	.word CODE64_SEL

.balign 16
fstart_smm_stub_params:
	.zero 96 /* sizeof(SmmEntryParams) */

fstart_smm_stub_end:
