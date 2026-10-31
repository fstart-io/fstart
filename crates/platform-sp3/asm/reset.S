/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * SP3 reset entry: the PSP-copied reset image (BIOS directory entry 0x62).
 *
 * This file is a `core::arch::global_asm!` template (AT&T syntax), included
 * by src/entry.rs. Brace-delimited names are const operands holding
 * layout constants from src/layout.rs (so comments here use no braces); `_sp3_*` section placement comes from linker/reset.ld.in.
 *
 * Start state (FIRMWARE_IMAGE §5, CCX §3.7):
 *   - The PSP has trained DRAM, copied the image to [dest, 0x76000000) and
 *     releases the BSP in 16-bit real mode at dest + size - 0x10 =
 *     0x75fffff0 with CS base 0x75ff0000, IP 0xfff0. The SMU releases APs
 *     (AGESA: SMN 0x30081018 + CCD * 0x2000000, ZP 0x5a818) into the same
 *     vector; AGESA patches it to reach its AP stub, we branch on
 *     APIC_BAR.BSP instead.
 *   - Caches are assumed disabled (CR0.CD set); the vendor SEC clears CD/NW
 *     only after programming its MTRRs.
 *
 * What the vendor SEC does first (Rome S8030 v4.04 offsets in the 64 KiB
 * segment; same code in all seven donors, the flash and DRAM copies are
 * byte-identical):
 *   0xfff0  nop; nop; jmp 0xef98                   (16-bit)
 *   0xef98  cli; out 0x80, 0; esp = eax (BIST); ebp = edx
 *           lgdt cs:[0xfb50]  (GDT base 0xfffffb08: the *flash* copy)
 *           CR0.PE = 1; ds..gs = 0x08; ljmp 0x10:0xffffefd0 (flash)
 *   flash   mov eax, 0xffffefd0; and eax, 0x75ffffff; add eax, 0xf;
 *           jmp eax                  -> 0x75ffefdf, the next instruction
 *                                        of the DRAM copy
 *   DRAM    MTRR 0x20e/0x20f = 0x75000006 / 0xffff_ff000800 (16 MiB WB at
 *           0x75000000), CPUID family check, APIC_BAR.BSP split of BSP and
 *           AP paths, MTRRdefType |= E|FE, SYSCFG |= MtrrFixDramEn |
 *           MtrrVarDramEn, &= ~MtrrFixDramModEn, CR0 &= ~(PG|CD|NW)
 *           (Rome 0x75fff6de..0x75fff713).
 *
 * fstart keeps the vendor's essentials without the detour through flash:
 * CS-relative 16-bit code (valid for a DRAM or a flash CS base, as the two
 * copies are identical), a GDT addressed at its DRAM link address, the same
 * MTRR window and SYSCFG bits, then page tables and long mode, and the arch
 * crate's 64-bit RAM entry `_start_ram` (BSS, IDT, fstart_main(0)).
 *
 * Section placement (linker/reset.ld.in):
 *   .sp3.entry16       at END - 0x1000 = segment offset 0xf000
 *   .sp3.reset_vector  at END - 0x10   = segment offset 0xfff0
 */

.set SEG_ENTRY16, 0xf000          /* offset of _sp3_entry16 in CS */
.set CODE32_SEL, 0x08
.set DATA_SEL, 0x10
.set CODE64_SEL, 0x18

.set MSR_APIC_BAR, 0x1b
.set APIC_BAR_BSP, 8
.set MSR_MTRR_BASE7, 0x20e
.set MSR_MTRR_MASK7, 0x20f
.set MSR_MTRR_DEF_TYPE, 0x2ff
.set MTRR_DEF_TYPE_E, 11
.set MTRR_TYPE_WB, 6
.set MTRR_MASK_VALID, 0x800
.set MSR_SYSCFG, 0xc0010010
.set SYSCFG_FIX_DRAM_EN, 18
.set SYSCFG_FIX_DRAM_MOD_EN, 19
.set SYSCFG_VAR_DRAM_EN, 20
.set MSR_EFER, 0xc0000080
.set EFER_LME, 8

.set CR0_PE, 0x00000001
.set CR0_EM, 0x00000004
.set CR0_TS, 0x00000008
.set CR0_NW, 0x20000000
.set CR0_CD, 0x40000000
.set CR0_PG, 0x80000000
.set CR4_PAE, 0x020
.set CR4_OSFXSR, 0x200
.set CR4_OSXMMEXCPT, 0x400


/* ------------------------------------------------------------------------
 * Reset vector: the last 16 bytes of the image.
 * ------------------------------------------------------------------------ */
.section .sp3.reset_vector, "ax"
.code16
.global _sp3_reset_vector
_sp3_reset_vector:
	cli
	jmp	_sp3_entry16
	.fill	16 - (. - _sp3_reset_vector), 1, 0xf4

/* ------------------------------------------------------------------------
 * 16-bit entry, GDT and the AP route, all within the real-mode segment.
 * ------------------------------------------------------------------------ */
.section .sp3.entry16, "awx"
.code16
.global _sp3_entry16
_sp3_entry16:
	/* EAX = BIST result (kept in EBP like the vendor's ESP). */
	movl	%eax, %ebp
	cld

	/* APs (SMU release) and the BSP share this vector. */
	movl	$MSR_APIC_BAR, %ecx
	rdmsr
	btl	$APIC_BAR_BSP, %eax
	jc	.Lbsp

	/* AP: follow the route once the MP code armed it, else park.
	 * The BSP writes the route back to DRAM before releasing APs
	 * (they fetch with caches off). */
	movw	$(_sp3_ap_route - _sp3_entry16 + SEG_ENTRY16), %bx
	cmpw	$0, %cs:2(%bx)
	je	.Lpark
	ljmpw	*%cs:(%bx)
.Lpark:
	cli
	hlt
	jmp	.Lpark

.Lbsp:
	movw	$(_sp3_null_idt - _sp3_entry16 + SEG_ENTRY16), %bx
	lidt	%cs:(%bx)
	movw	$(_sp3_gdt_desc - _sp3_entry16 + SEG_ENTRY16), %bx
	lgdtl	%cs:(%bx)
	movl	%cr0, %eax
	orl	$CR0_PE, %eax
	movl	%eax, %cr0
	ljmpl	$CODE32_SEL, $_sp3_entry32

.balign 8
_sp3_null_idt:
	.word	0
	.long	0
	.word	0

.balign 8
.global _sp3_gdt
_sp3_gdt:
	.quad	0
	.quad	0x00cf9b000000ffff	/* 0x08: 32-bit flat code */
	.quad	0x00cf93000000ffff	/* 0x10: flat data */
	.quad	0x00af9b000000ffff	/* 0x18: 64-bit code */
_sp3_gdt_end:
_sp3_gdt_desc:
	.word	_sp3_gdt_end - _sp3_gdt - 1
	.long	_sp3_gdt		/* DRAM link address */

/* m16:16 far pointer for APs: offset, then segment (0 = park). */
.balign 4
.global _sp3_ap_route
_sp3_ap_route:
	.word	0
	.word	0

/* BSP EAX at release (BIST); 0 = passed. */
.global _sp3_bsp_bist
_sp3_bsp_bist:
	.long	0

/* ------------------------------------------------------------------------
 * 32-bit protected mode: cache window, page tables, long mode.
 * ------------------------------------------------------------------------ */
.section .text.sp3_entry32, "ax"
.code32
_sp3_entry32:
	movw	$DATA_SEL, %ax
	movw	%ax, %ds
	movw	%ax, %es
	movw	%ax, %ss
	movw	%ax, %fs
	movw	%ax, %gs
	movl	%ebp, _sp3_bsp_bist

	/* Variable MTRR 7: write-back over the early region and the image,
	 * the vendor's window. The mask width follows CPUID 8000_0008
	 * (the vendor hardcodes 48 bits). */
	movl	$0x80000008, %eax
	cpuid
	movzbl	%al, %ecx
	subl	$32, %ecx
	movl	$1, %edx
	shll	%cl, %edx
	decl	%edx
	movl	$MSR_MTRR_MASK7, %ecx
	movl	${cache_mask}, %eax
	orl	$MTRR_MASK_VALID, %eax
	wrmsr
	movl	$MSR_MTRR_BASE7, %ecx
	movl	$({cache_base} | MTRR_TYPE_WB), %eax
	xorl	%edx, %edx
	wrmsr
	movl	$MSR_MTRR_DEF_TYPE, %ecx
	rdmsr
	btsl	$MTRR_DEF_TYPE_E, %eax
	wrmsr
	movl	$MSR_SYSCFG, %ecx
	rdmsr
	btsl	$SYSCFG_FIX_DRAM_EN, %eax
	btsl	$SYSCFG_VAR_DRAM_EN, %eax
	btrl	$SYSCFG_FIX_DRAM_MOD_EN, %eax
	wrmsr
	movl	%cr0, %eax
	andl	$~(CR0_CD | CR0_NW | CR0_EM | CR0_TS), %eax
	movl	%eax, %cr0

	/* Identity map the 47-bit space with 1 GiB pages: PML4 at
	 * _sp3_page_tables, then the PDPTs (_fstart_identity_tables). */
	movl	$_sp3_page_tables, %edi
	movl	${pdpts}, %ecx
	movl	$1f, %ebp
	jmp	_fstart_identity_tables
1:

	movl	$_sp3_page_tables, %eax
	movl	%eax, %cr3
	movl	%cr4, %eax
	orl	$(CR4_PAE | CR4_OSFXSR | CR4_OSXMMEXCPT), %eax
	movl	%eax, %cr4
	movl	$MSR_EFER, %ecx
	rdmsr
	btsl	$EFER_LME, %eax
	wrmsr
	movl	%cr0, %eax
	orl	$CR0_PG, %eax
	movl	%eax, %cr0
	/* arch: stack, BSS, IDT, fstart_main(0). */
	ljmpl	$CODE64_SEL, $_start_ram
