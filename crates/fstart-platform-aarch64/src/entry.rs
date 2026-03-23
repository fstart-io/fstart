//! AArch64 entry point — reset vector and early init.
//!
//! Provides the `_start` symbol placed at the reset vector by the linker script.
//! Saves the DTB address from QEMU (passed in `x0`), sets up the stack,
//! clears BSS, and jumps to `fstart_main`.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text.entry
    .global _start
_start:
    // Save boot argument from QEMU before any register is clobbered.
    // QEMU AArch64 virt passes: x0 = DTB address.
    mov x19, x0

    // Disable all interrupts
    msr daifset, #0xf

    // ---------------------------------------------------------------
    // EL3 → EL1 transition.
    //
    // QEMU virt with secure=on starts at EL3. EL3 enforces strict
    // alignment on all memory accesses (Device-nGnRnE default memory
    // type). UEFI PE binaries use 128-bit SIMD stores that require
    // Normal memory attributes. We must drop to EL1 where we can
    // configure the MMU with proper memory types.
    //
    // Check CurrentEL first — only do the drop if we're at EL3.
    // ---------------------------------------------------------------
    mrs x0, CurrentEL
    lsr x0, x0, #2
    cmp x0, #3
    b.ne .Lel1_entry           // Already at EL1 (or EL2) — skip ERET

    // === At EL3: configure for EL1 non-secure entry ===

    // SCR_EL3: NS=0 (secure), RW=1 (AArch64 for EL2/EL1),
    //          SMD=0 (SMC enabled), HCE=0 (no HVC at Secure EL1)
    // We stay in secure world so EL1 can access the flash at 0x0
    // where our code resides. Firmware doesn't need non-secure.
    // Bits: RW(10)=1, ST(11)=1
    mov x0, #0x0C00            // RW | ST (no NS, no HCE)
    msr scr_el3, x0

    // SPSR_EL3: target EL1h (M[3:0]=0b0101), DAIF masked
    mov x0, #0x3C5             // EL1h + DAIF all masked
    msr spsr_el3, x0

    // ELR_EL3: return to .Lel1_entry (EL1 code)
    adr x0, .Lel1_entry
    msr elr_el3, x0

    // Enable FP/SIMD at EL3 for the ERET path (CPTR_EL3.TFP = 0)
    // CPTR_EL3 reset value may trap FP. Clear TFP (bit 10).
    mrs x0, cptr_el3
    bic x0, x0, #(1 << 10)
    msr cptr_el3, x0

    // Ensure GIC is in non-secure mode for EL1 access
    // (QEMU virt GICv3 should handle this, but be safe)

    isb
    eret

    // ---------------------------------------------------------------
    // EL1 entry point (reached via ERET from EL3 or directly)
    // ---------------------------------------------------------------
.Lel1_entry:

    // Enable FP/SIMD (NEON) access at EL1.
    // CPACR_EL1.FPEN [21:20] = 0b11 enables EL0+EL1 FP/SIMD access.
    mov x0, #(3 << 20)
    msr cpacr_el1, x0
    isb

    // Set up stack pointer (before MMU — stack is in Device memory but
    // 8-byte aligned STR/LDR works fine for data/BSS copy).
    ldr x0, =_stack_top
    mov sp, x0

    // Copy .data initializers from ROM to RAM.
    // _data_load = LMA (ROM), _data_start/_data_end = VMA (RAM).
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
    // Clear BSS
    ldr x0, =_bss_start
    ldr x1, =_bss_end
3:
    cmp x0, x1
    b.ge 4f
    str xzr, [x0], #8
    b 3b
4:
    // Store boot DTB address to global (after BSS is cleared to zero)
    ldr x0, =BOOT_DTB_ADDR
    str x19, [x0]

    // Clear .page_tables section (separate from BSS to prevent
    // corruption from CrabEFI or other BSS-resident statics).
    ldr x0, =_page_tables_start
    ldr x1, =_page_tables_end
5:
    cmp x0, x1
    b.ge .Lpt_cleared
    str xzr, [x0], #8
    b 5b
.Lpt_cleared:

    // ---------------------------------------------------------------
    // Set up identity-mapped MMU (after page table memory is zeroed).
    //
    // Without MMU, AArch64 treats ALL memory as Device-nGnRnE which
    // enforces strict alignment. UEFI PE binaries use 128-bit SIMD
    // stores like `stur q0, [sp, #0x3a]` that require Normal memory.
    //
    // Level 1 table, 1GB blocks, 4KB granule, 39-bit VA space.
    // ---------------------------------------------------------------

    // MAIR_EL1: match U-Boot's attribute layout exactly.
    // Attr0=0x00 (Device-nGnRnE), Attr1=0x04 (Device-nGnRE),
    // Attr2=0x0C (Device-GRE), Attr3=0x44 (Normal NC),
    // Attr4=0xFF (Normal WB RA/WA inner+outer)
    ldr x0, =0x000000FF440C0400
    msr mair_el1, x0

    // TCR_EL1: T0SZ=22 (42-bit VA = 4TB), 4KB granule, WB cacheable
    // IPS=0b011 (42-bit PA, 4TB)
    // 42-bit VA with 4KB granule: L0 has 4 entries, each → L1 (512 entries)
    // EPD1=1: disable TTBR1 walks — we only use the lower VA range.
    // Bit 31 is RES1 (architecturally required, matches U-Boot).
    ldr x0, =((22) | (1 << 8) | (1 << 10) | (3 << 12) | (1 << 23) | (1 << 31) | (3 << 32))
    msr tcr_el1, x0

    // === L0 table (4 entries, each pointing to an L1 table) ===
    ldr x1, =MMU_L0_TABLE

    // L0[0] → L1_LOW (covers 0x0 - 0x7FFFFFFFFF = 0-512GB)
    ldr x2, =MMU_L1_TABLE
    orr x2, x2, #3            // valid=1, table=1 (bits [1:0] = 0b11)
    str x2, [x1, #0]

    // L0[1] → L1_HIGH (covers 0x8000000000 - 0xFFFFFFFFFF = 512GB-1TB)
    ldr x2, =MMU_L1_HIGH_TABLE
    orr x2, x2, #3            // valid=1, table=1
    str x2, [x1, #8]

    // L0[2] and L0[3] stay zero (invalid) — not needed

    // === L1_LOW: covers 0 - 512GB (entries 0-511, 1GB each) ===
    ldr x1, =MMU_L1_TABLE

    // Entry 0: 0x00000000-0x3FFFFFFF = Device (flash, UART, GIC, PCIe)
    // Block descriptor: valid=1, block(not table)=0, AttrIdx=0, AF=1, SH=outer
    ldr x2, =(1 | (0 << 2) | (2 << 8) | (1 << 10) | (0 << 30))
    str x2, [x1, #0]

    // Entry 1: 0x40000000-0x7FFFFFFF = Normal WB Cacheable (RAM)
    // AttrIdx=4 (MAIR Attr4=0xFF, matches U-Boot MT_NORMAL)
    ldr x2, =(1 | (4 << 2) | (3 << 8) | (1 << 10) | (1 << 30))
    str x2, [x1, #8]

    // Entry 2: 0x80000000-0xBFFFFFFF = Normal (RAM)
    ldr x2, =(1 | (4 << 2) | (3 << 8) | (1 << 10) | (2 << 30))
    str x2, [x1, #16]

    // Entry 3: 0xC0000000-0xFFFFFFFF = Normal (RAM)
    ldr x2, =(1 | (4 << 2) | (3 << 8) | (1 << 10) | (3 << 30))
    str x2, [x1, #24]

    // Entry 4: 0x100000000-0x13FFFFFFF = Normal (RAM, 4-5GB)
    ldr x2, =(1 | (4 << 2) | (3 << 8) | (1 << 10) | (4 << 30))
    str x2, [x1, #32]

    // Entries 256-257: PCI ECAM at 0x4010000000 (index 256 = 256GB)
    // Device memory for MMIO config space reads/writes.
    // Block descriptor low bits: valid=1, AttrIdx=0, SH=outer(2<<8), AF(1<<10)
    // = 1 | (2 << 8) | (1 << 10) = 0x601
    mov x6, #0x601             // low-bits template for Device block
    mov x4, #256               // start index
6:
    lsl x2, x4, #30           // output address [47:30]
    orr x2, x2, x6            // merge with low bits
    lsl x3, x4, #3            // table offset = index * 8
    str x2, [x1, x3]
    add x4, x4, #1
    cmp x4, #258              // 2 entries: 256, 257
    b.lt 6b

    // === L1_HIGH: covers 512GB - 1TB (for PCI MMIO64 at 0x8000000000) ===
    // L1_HIGH[0] = 0x8000000000 (512GB) = Device (PCI MMIO64)
    // We map the first 256GB of this range as Device memory.
    ldr x1, =MMU_L1_HIGH_TABLE
    mov x6, #0x601             // Device block descriptor low bits
    mov x4, #0                 // L1_HIGH index 0 = physical 0x8000000000
7:
    // Compute physical address: 0x8000000000 + index * 0x40000000
    mov x2, #0x200             // 0x200 << 30 = 0x8000000000
    add x2, x2, x4             // add index
    lsl x2, x2, #30            // shift to [47:30]
    orr x2, x2, x6             // merge Device attrs
    lsl x3, x4, #3             // table offset
    str x2, [x1, x3]
    add x4, x4, #1
    cmp x4, #256               // first 256GB of this L0 entry
    b.lt 7b

    // TTBR0_EL1 = L0 table base (not L1!)
    ldr x0, =MMU_L0_TABLE
    msr ttbr0_el1, x0

    // Invalidate all TLB entries BEFORE enabling MMU.
    // Before MMU enable, QEMU's softmmu may have cached TLB entries
    // with Device-nGnRnE attributes (the default when MMU is off).
    // Those stale entries enforce strict alignment on all accesses,
    // including SIMD stores, even after MMU enable changes memory
    // types to Normal WB.  This matches U-Boot's sequence.
    tlbi vmalle1
    dsb sy
    isb

    // Enable MMU + caches, disable alignment checks.
    //
    // Read-modify-write to preserve QEMU's reset defaults (nTWI,
    // nTWE, etc.) while setting RES1 bits + our functional bits.
    // RES1 bits (ARMv8): 29,28,23,22,20,11 = 0x30D00800
    mrs x0, sctlr_el1
    ldr x1, =0x30D00800       // RES1 bits
    orr x0, x0, x1            // ensure RES1 bits are set
    orr x0, x0, #(1 << 0)    // M:   MMU enable
    orr x0, x0, #(1 << 2)    // C:   data cache
    orr x0, x0, #(1 << 6)    // nAA: permit unaligned SIMD/FP (FEAT_LSE2)
    orr x0, x0, #(1 << 12)   // I:   instruction cache
    bic x0, x0, #(1 << 1)    // ~A:  no GP alignment check
    bic x0, x0, #(1 << 3)    // ~SA: no SP alignment check
    msr sctlr_el1, x0
    isb

    // Jump to Rust entry point.
    // x0 = handoff_ptr = 0 (no inter-stage handoff on AArch64 yet).
    mov x0, #0
    bl fstart_main
    // Should never return
3:
    wfe
    b 3b
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

/// Page table for identity-mapped MMU.
///
/// With T0SZ=22 (42-bit VA), we use:
/// - L0: 4 entries, each pointing to an L1 table (512GB per entry)
/// - L1_LOW: 512 entries for 0-512GB (RAM + low MMIO + ECAM)
/// - L1_HIGH: 512 entries for 512GB-1TB (PCI MMIO64)
///
/// Each L1 entry maps a 1 GiB block. Must be 4 KiB aligned.
#[repr(C, align(4096))]
struct PageTable([u64; 512]);

/// Small table for L0 (only 4 entries used, but 4KB aligned)
#[repr(C, align(4096))]
struct L0Table([u64; 512]);

#[no_mangle]
#[link_section = ".page_tables"]
static mut MMU_L0_TABLE: L0Table = L0Table([0u64; 512]);

#[no_mangle]
#[link_section = ".page_tables"]
static mut MMU_L1_TABLE: PageTable = PageTable([0u64; 512]);

#[no_mangle]
#[link_section = ".page_tables"]
static mut MMU_L1_HIGH_TABLE: PageTable = PageTable([0u64; 512]);
