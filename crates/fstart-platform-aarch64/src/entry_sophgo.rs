//! AArch64 entry point for Sophgo SG2042 (Milk-V Pioneer).
//!
//! Same EL3 bootstrap and exception vector table as `entry.rs`, but with
//! SG2042-correct MMU page tables covering the full 40-bit peripheral address
//! space (all registers live in the 0x70xx_xxxx_xx range, which is beyond
//! the 4GB coverage of the default QEMU entry).
//!
//! # SG2042 address space overview (40-bit)
//!
//! With T0SZ=22 (42-bit VA / 4TB) the L1 table has 512 entries covering
//! 0 – 512 GB. Each entry covers 1 GiB. Key SG2042 ranges:
//!
//! | L1 index | Physical range            | Content |
//! |----------|---------------------------|---------|
//! | 0        | 0x00000000 – 0x3FFFFFFF   | DDR0 (unmapped in Phase 1) |
//! | 448      | 0x7000000000 – 0x703FFFFFFF | SRAM0 + peripheral cluster 1 |
//! | 449      | 0x7040000000 – 0x707FFFFFFF | UART, PCIe cfg, CMN, DDR ctrl |
//! | 450      | 0x7080000000 – 0x70BFFFFFFF | PLIC, CLINT, RPU, FAU |
//!
//! L1[448] contains both SRAM0 (where we execute — needs Normal WB) and
//! device peripherals (WDT, I2C, GPIO, SYS_CTRL — need Device-nGnRnE).
//! This is resolved by pointing L1[448] at an L2 table (`MMU_L2_SRAM_TABLE`)
//! that maps SRAM0 as Normal WB and everything else as Device.
//!
//! # Address arithmetic
//!
//! - L1 index for 0x7000000000 = 0x7000000000 / 1GiB = 448
//! - L2 index for SRAM0 (BL2_BASE=0x7010000000) within L1[448]:
//!   offset = 0x7010000000 - 0x7000000000 = 0x10000000 = 256 MiB
//!   L2 index = 256 MiB / 2 MiB = 128

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text.entry
    .global _start
_start:
    // Save boot argument (x0 = unused on SG2042 — SCP boots from BootROM,
    // not from QEMU. Preserved for compatibility.
    mov x19, x0

    // Disable all interrupts
    msr daifset, #0xf

    // ---------------------------------------------------------------
    // EL detection and EL3 → EL1 transition (identical to entry.rs)
    // ---------------------------------------------------------------
    mrs x0, CurrentEL
    lsr x0, x0, #2
    cmp x0, #3
    b.eq .Lsg_el3_setup
    cmp x0, #2
    b.eq .Lsg_el2_setup
    b .Lsg_el1_entry

    // === At EL2: configure for EL1 entry ===
.Lsg_el2_setup:
    ldr x0, =(1 << 31)
    msr hcr_el2, x0
    ldr x0, =0x33FF
    msr cptr_el2, x0
    mov x0, #0x3C5
    msr spsr_el2, x0
    adr x0, .Lsg_el1_entry
    msr elr_el2, x0
    isb
    eret

    // === At EL3: configure for EL1 entry (secure world) ===
.Lsg_el3_setup:
    // SCR_EL3: RW=1 (AArch64), ST=1, NS=0 (stay secure — we ARE the SCP BL2)
    mov x0, #0x0C00
    msr scr_el3, x0
    mov x0, #0x3C5
    msr spsr_el3, x0
    adr x0, .Lsg_el1_entry
    msr elr_el3, x0
    mrs x0, cptr_el3
    bic x0, x0, #(1 << 10)
    msr cptr_el3, x0

    // Install EL3 vector table (same SMC handler as entry.rs)
    adr x0, .Lsg_el3_vectors
    msr vbar_el3, x0
    isb
    eret

    // ---------------------------------------------------------------
    // EL3 exception vector table
    // ---------------------------------------------------------------
    .balign 2048
.Lsg_el3_vectors:
    b .                    // 0x000 Current EL SP0 Sync
    .balign 0x80
    b .                    // 0x080 IRQ
    .balign 0x80
    b .                    // 0x100 FIQ
    .balign 0x80
    b .                    // 0x180 SError
    .balign 0x80
    b .                    // 0x200 Current EL SPx Sync
    .balign 0x80
    b .                    // 0x280 IRQ
    .balign 0x80
    b .                    // 0x300 FIQ
    .balign 0x80
    b .                    // 0x380 SError
    .balign 0x80
    b .Lsg_el3_smc_handler // 0x400 Lower EL AArch64 Sync (SMC)
    .balign 0x80
    b .                    // 0x480 IRQ
    .balign 0x80
    b .                    // 0x500 FIQ
    .balign 0x80
    b .                    // 0x580 SError
    .balign 0x80
    b .                    // 0x600 Lower EL AArch32 Sync
    .balign 0x80
    b .                    // 0x680 IRQ
    .balign 0x80
    b .                    // 0x700 FIQ
    .balign 0x80
    b .                    // 0x780 SError

.Lsg_el3_smc_handler:
    mrs x9, esr_el3
    lsr x9, x9, #26
    cmp x9, #0x17
    b.ne .Lsg_smc_unknown
    // PSCI_VERSION (0x84000000)
    ldr w9, =0x84000000
    cmp w0, w9
    b.ne 1f
    ldr w0, =0x00010001
    b .Lsg_smc_return
1:  // SMCCC_VERSION (0x80000000)
    ldr w9, =0x80000000
    cmp w0, w9
    b.ne 2f
    ldr w0, =0x00010002
    b .Lsg_smc_return
2:  // PSCI_FEATURES (0x8400000A)
    ldr w9, =0x8400000A
    cmp w0, w9
    b.ne 3f
    mov x0, #-1
    b .Lsg_smc_return
3:  // PSCI_SYSTEM_OFF (0x84000008)
    ldr w9, =0x84000008
    cmp w0, w9
    b.ne 4f
    wfi
    b .
4:  // PSCI_SYSTEM_RESET (0x84000009)
    ldr w9, =0x84000009
    cmp w0, w9
    b.ne 5f
    wfi
    b .
5:
.Lsg_smc_unknown:
    mov x0, #-1
.Lsg_smc_return:
    eret

    // ---------------------------------------------------------------
    // EL1 entry — early init, page tables, MMU enable
    // ---------------------------------------------------------------
.Lsg_el1_entry:
    // Enable FP/SIMD
    mov x0, #(3 << 20)
    msr cpacr_el1, x0
    isb

    // Set up stack
    ldr x0, =_stack_top
    mov sp, x0

    // Copy .data initializers (LMA → VMA)
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
    // Store boot DTB (unused on SG2042, kept for ABI compatibility)
    ldr x0, =BOOT_DTB_ADDR
    str x19, [x0]

    // Clear .page_tables section
    ldr x0, =_page_tables_start
    ldr x1, =_page_tables_end
5:
    cmp x0, x1
    b.ge .Lsg_pt_cleared
    str xzr, [x0], #8
    b 5b
.Lsg_pt_cleared:

    // ---------------------------------------------------------------
    // MMU setup for SG2042 address space.
    //
    // MAIR and TCR are identical to entry.rs (T0SZ=22, 42-bit VA, 4KB
    // granule).  The L1 table content is different.
    // ---------------------------------------------------------------
    ldr x0, =0x000000FF440C0400    // MAIR: same as entry.rs
    msr mair_el1, x0

    // TCR_EL1: T0SZ=22 (42-bit VA), IPS=0b011 (42-bit PA), EPD1=1
    ldr x0, =((22) | (1 << 8) | (1 << 10) | (3 << 12) | (1 << 23) | (1 << 31) | (3 << 32))
    msr tcr_el1, x0

    // === L0 table ===
    // L0[0] → L1_LOW (0 – 512 GB)
    // L0[1..3] remain zero (invalid) — nothing mapped above 512 GB in Phase 1
    ldr x1, =MMU_SG_L0_TABLE
    ldr x2, =MMU_SG_L1_LOW_TABLE
    orr x2, x2, #3                 // table descriptor
    str x2, [x1, #0]

    // === L1_LOW ===
    ldr x1, =MMU_SG_L1_LOW_TABLE

    // Entry 0: 0x00000000-0x3FFFFFFF — Device (DDR0 unmapped; Phase 1)
    // AttrIdx=0 (Device-nGnRnE), AF=1, SH=0 (non-shareable for Device)
    // descriptor = valid(1) | block(bit1=0) | AttrIdx(0<<2) | SH(2<<8) | AF(1<<10)
    ldr x2, =(1 | (0 << 2) | (2 << 8) | (1 << 10) | (0 << 30))
    str x2, [x1, #0]

    // Entry 448: 0x7000000000-0x703FFFFFFF → TABLE (→ L2 for SRAM0)
    // 448 * 8 = 3584 = 0xE00 byte offset into L1 table
    ldr x2, =MMU_SG_L2_SRAM_TABLE
    orr x2, x2, #3                 // table descriptor
    str x2, [x1, #0xE00]

    // Entry 449: 0x7040000000-0x707FFFFFFF — Device
    // (UART1, SPI, eMMC, ETH, HS-DMA, DDR ctrl, PCIe cfg, CMN-600)
    // Physical address for block descriptor: 0x7040000000 = 449 << 30
    // descriptor = device_attrs | (449 << 30)
    mov x6, #0x601                 // Device-nGnRnE block: valid|AF|SH=outer
    mov x4, #449
    lsl x2, x4, #30
    orr x2, x2, x6
    str x2, [x1, #(449*8)]

    // Entry 450: 0x7080000000-0x70BFFFFFFF — Device
    // (PLIC, CLINT IPI, RP CPU CLINT Timer, RPU, FAU0/1/2)
    mov x4, #450
    lsl x2, x4, #30
    orr x2, x2, x6
    str x2, [x1, #(450*8)]

    // === L2_SRAM_TABLE: covers 0x7000000000-0x703FFFFFFF (1 GiB, 512×2MB) ===
    //
    // All entries default to Device-nGnRnE (set below).
    // Entry 128 (SRAM0 at 0x7010000000) is overridden to Normal WB.
    //
    // Device 2MB block descriptor template: valid(1) | AttrIdx(0)=Device | AF | SH=outer
    //   = 1 | (0<<2) | (2<<8) | (1<<10) = 0x601
    // Normal WB 2MB block: valid(1) | AttrIdx(4) | AF | SH=inner-shareable
    //   = 1 | (4<<2) | (3<<8) | (1<<10) = 0x711
    //
    // L2 entry physical address: base = 0x7000000000 + index * 0x200000
    // For Device entries: PA = base
    // For SRAM0 (index 128): PA = 0x7000000000 + 128 * 0x200000 = 0x7010000000

    ldr x1, =MMU_SG_L2_SRAM_TABLE
    mov x6, #0x601                 // Device attrs
    mov x5, #0x200                 // 0x200 << 30 = base of 0x7000000000 range

    // Fill all 512 entries as Device (index 0..511)
    mov x4, #0
.Lsg_l2_fill:
    // PA = (0x200 + index) << 21 — wait, need to be careful:
    // each L2 entry covers 2 MiB = 2^21 bytes, so OA bits[47:21]
    // For index i: PA = 0x7000000000 + i * 0x200000
    //            = (0x700 + i) << 21? No:
    //            0x7000000000 = 0x380 << 21 (= 896 << 21)
    mov x2, #0x380
    add x2, x2, x4
    lsl x2, x2, #21                // physical address [47:21] in bits[47:21]
    orr x2, x2, x6                 // merge Device attrs (bits[1:0]=01 for block)
    lsl x3, x4, #3                 // entry offset = index * 8
    str x2, [x1, x3]
    add x4, x4, #1
    cmp x4, #512
    b.lt .Lsg_l2_fill

    // Override entry 128 with Normal WB (SRAM0 at 0x7010000000)
    // PA = 0x7010000000 = (0x380 + 128) * 2MiB = 0x400 * 2MiB = 0x400 << 21
    mov x2, #0x400
    lsl x2, x2, #21                // 0x7010000000
    ldr x6, =0x711                 // Normal WB inner-shareable block descriptor attrs
    orr x2, x2, x6
    str x2, [x1, #(128*8)]         // entry 128, offset = 128 * 8 = 0x400

    // TTBR0_EL1 = L0 table base
    ldr x0, =MMU_SG_L0_TABLE
    msr ttbr0_el1, x0

    // TLB flush before enabling MMU
    tlbi vmalle1
    dsb sy
    isb

    // Enable MMU + caches, disable alignment checks (same as entry.rs)
    mrs x0, sctlr_el1
    ldr x1, =0x30D00800            // RES1 bits
    orr x0, x0, x1
    orr x0, x0, #(1 << 0)         // M: MMU enable
    orr x0, x0, #(1 << 2)         // C: data cache
    orr x0, x0, #(1 << 6)         // nAA: permit unaligned SIMD/FP
    orr x0, x0, #(1 << 12)        // I: instruction cache
    bic x0, x0, #(1 << 1)         // ~A: no GP alignment check
    bic x0, x0, #(1 << 3)         // ~SA: no SP alignment check
    msr sctlr_el1, x0
    isb

    // Jump to Rust entry point (handoff_ptr = 0 for first stage)
    mov x0, #0
    bl fstart_main
    // Should never return
6:
    wfe
    b 6b
    "#
);

extern "Rust" {
    #[allow(dead_code)]
    fn fstart_main(handoff_ptr: usize) -> !;
}

/// Three-level page table for SG2042 MMU setup.
///
/// Covers the SG2042 40-bit physical address space with identity mapping.
/// Must be 4 KiB aligned; placed in `.page_tables` section.
#[repr(C, align(4096))]
struct PageTable([u64; 512]);

#[no_mangle]
#[link_section = ".page_tables"]
static mut MMU_SG_L0_TABLE: PageTable = PageTable([0u64; 512]);

#[no_mangle]
#[link_section = ".page_tables"]
static mut MMU_SG_L1_LOW_TABLE: PageTable = PageTable([0u64; 512]);

/// L2 table for the 0x7000000000-0x703FFFFFFF range.
///
/// Entry 128 = SRAM0 (Normal WB).  All other entries = Device-nGnRnE.
/// The assembly code fills this table at runtime.
#[no_mangle]
#[link_section = ".page_tables"]
static mut MMU_SG_L2_SRAM_TABLE: PageTable = PageTable([0u64; 512]);
