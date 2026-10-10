/* SPDX-License-Identifier: MIT */
/*
 * x86 AP SIPI trampoline for fstart.
 *
 * Copied by the BSP to SIPI_VECTOR_ADDR.  APs start in real mode at
 * CS = SIPI_VECTOR_ADDR >> 4, IP = 0.  The trampoline enters 32-bit
 * protected mode, claims a 0-based AP index with an atomic xadd, enables
 * long mode using the BSP-provided CR3, sets a per-AP stack, and tail-calls
 * fstart_ap_entry(index).
 *
 * With FSTART_PROTECTED_MODE (the 32-bit target) the AP stays in flat
 * protected mode without paging and tail-calls the cdecl entry instead.
 */

.set SIPI_BASE, 0x8000
.set CODE32_SEL, 0x08
.set DATA_SEL, 0x10
.set CODE64_SEL, 0x18
.set CR0_PE, 1
.set CR0_NW, (1 << 29)
.set CR0_CD, (1 << 30)
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
.global fstart_sipi_ap_limit
.global fstart_sipi_ap_counter
.global fstart_sipi_msr_count
.global fstart_sipi_msr_table

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
	/* Match coreboot's startup vector: transfer directly to the flat code
	 * selector without another memory access in the mode-switch window.
	 */
	ljmpl $CODE32_SEL, $(SIPI_BASE + prot32 - fstart_sipi_start)

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

	/* Broadcast SIPIs wake every AP, including CPUs beyond firmware's static
	 * capacity. Reset on an excess responder before it indexes the stack
	 * arena: it would never be SMBASE-relocated, so it must not outlive MP
	 * bring-up. */
	cmpl SIPI_BASE + fstart_sipi_ap_limit - fstart_sipi_start, %eax
	jae excess_ap

	/* ESP = stack_base + (index + 1) * stack_size. */
	movl %eax, %ebx
	incl %ebx
	movl SIPI_BASE + fstart_sipi_stack_size - fstart_sipi_start, %ecx
	imull %ecx, %ebx
	addl SIPI_BASE + fstart_sipi_stack_base - fstart_sipi_start, %ebx
#ifdef FSTART_PROTECTED_MODE
	/* cdecl tail call: the index argument plus a null return address, with
	 * ESP % 16 == 12 at entry as if called with an aligned stack.
	 */
	andl $-16, %ebx
	subl $12, %ebx
	movl %ebx, %esp
	pushl %esi
	pushl $0
#else
	/* A tail jump into the Rust entry must present the SysV function-entry
	 * alignment (RSP % 16 == 8), as if a return address had been pushed.
	 */
	andl $-16, %ebx
	subl $8, %ebx
	movl %ebx, %esp
#endif

	/* Replay the BSP's fixed/variable MTRRs and PAT with caching disabled.
	 * INIT can retain MSRs, so disable MTRRs explicitly before replay. The
	 * final table record restores IA32_MTRR_DEF_TYPE (including enable).
	 * ESI retains the AP ordinal; EDI/EBP are scratch until long64.
	 */
	movl %cr0, %eax
	orl $CR0_CD, %eax
	andl $~CR0_NW, %eax
	movl %eax, %cr0
	movl $0x2ff, %ecx
	rdmsr
	andl $~(1 << 11), %eax
	wrmsr
	movl $SIPI_BASE + fstart_sipi_msr_table - fstart_sipi_start, %edi
	movl SIPI_BASE + fstart_sipi_msr_count - fstart_sipi_start, %ebp
msr_replay:
	testl %ebp, %ebp
	jz msr_replay_done
	movl 0(%edi), %ecx
	movl 4(%edi), %eax
	movl 8(%edi), %edx
	wrmsr
	addl $12, %edi
	decl %ebp
	jmp msr_replay
msr_replay_done:

#ifdef FSTART_PROTECTED_MODE
	/* No paging: join cache coherency now that the BSP's MTRRs/PAT are in
	 * place, then enter Rust.
	 */
	movl %cr0, %eax
	andl $~(CR0_CD | CR0_NW), %eax
	movl %eax, %cr0
	xorl %ebp, %ebp
	movl SIPI_BASE + fstart_sipi_entry - fstart_sipi_start, %eax
	jmp *%eax
#else
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

	/* The BSP's MTRRs/PAT and page tables are now installed. Only now join
	 * normal cache coherency, before entering Rust or fetching microcode.
	 */
	andl $~(CR0_CD | CR0_NW), %eax
	movl %eax, %cr0

	ljmpl $CODE64_SEL, $(SIPI_BASE + long64 - fstart_sipi_start)
#endif

excess_ap:
	/* Hard reset through the ICH/Q35 reset control register. A halted CPU
	 * still services SMI at the default SMBASE, so halting is not safe. */
	cli
	movw $0x0cf9, %dx
	movb $0x06, %al
	outb %al, %dx
1:	hlt
	jmp 1b

#ifndef FSTART_PROTECTED_MODE
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
#endif

.balign 8
gdtr:
	.word gdt_end - gdt - 1
	.long SIPI_BASE + gdt - fstart_sipi_start

gdt:
	.quad 0x0000000000000000
	/* Keep the accessed bit set, matching coreboot/Linux startup GDTs. This
	 * avoids a descriptor-table write during the fragile AP mode switch. */
	.quad 0x00cf9b000000ffff /* 32-bit flat code */
	.quad 0x00cf93000000ffff /* flat data */
	.quad 0x00af9b0000000000 /* 64-bit flat code */
gdt_end:

.balign 8
fstart_sipi_cr3:
	.quad 0
fstart_sipi_entry:
	.quad 0
fstart_sipi_stack_base:
	.quad 0
fstart_sipi_stack_size:
	.long 0
fstart_sipi_ap_limit:
	.long 0
fstart_sipi_ap_counter:
	.long 0
fstart_sipi_msr_count:
	.long 0
.balign 4
fstart_sipi_msr_table:
	/* The remainder of the reserved SIPI page holds packed MSR records.
	 * Rust checks the actual linked capacity before copying the snapshot.
	 */
	.space 4096 - (. - fstart_sipi_start)
fstart_sipi_end:
