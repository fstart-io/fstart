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
    b.eq .Lel3_setup           // At EL3 — do EL3→EL1 transition
    cmp x0, #2
    b.eq .Lel2_setup           // At EL2 — do EL2→EL1 transition
    b .Lel1_entry              // Already at EL1 — skip transitions

    // === At EL2: configure for EL1 entry ===
.Lel2_setup:
    // HCR_EL2: RW=1 (AArch64 for EL1), all other bits zero
    // (no trapping, no virtual interrupts)
    ldr x0, =(1 << 31)            // RW bit
    msr hcr_el2, x0

    // Enable FP/SIMD at EL2 (CPTR_EL2.TFP bit 10 = 0)
    // CPTR_EL2 reset value may trap FP. Clear TFP.
    // Also clear FPEN bits if any — RES1 bits: 0x33FF for non-VHE
    ldr x0, =0x33FF               // RES1 bits, TFP(10)=0
    msr cptr_el2, x0

    // SPSR_EL2: target EL1h (M[3:0]=0b0101), DAIF masked
    mov x0, #0x3C5                // EL1h + DAIF all masked
    msr spsr_el2, x0

    // ELR_EL2: return to .Lel1_entry
    adr x0, .Lel1_entry
    msr elr_el2, x0

    isb
    eret

    // === At EL3: configure for EL1 non-secure entry ===
.Lel3_setup:

    // SCR_EL3: NS=0 (secure), RW=1 (AArch64 for EL2/EL1),
    //          SMD=0 (SMC enabled), HCE=0 (no HVC at Secure EL1)
    // We stay in secure world so EL1 can access the flash at 0x0
    // where our code resides. The NS switch to non-secure happens
    // later at ExitBootServices via the FSTART_NS_SWITCH SMC.
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

    // Install minimal EL3 exception vectors BEFORE ERET.
    // After ERET to EL1, the Linux kernel issues SMC calls for PSCI
    // (CPU power management) and SMCCC. Without an EL3 handler these
    // trap into uninitialised vectors and crash.  The handler below
    // returns PSCI_NOT_SUPPORTED (-1) for all unknown SMC calls and
    // handles PSCI_VERSION, PSCI_FEATURES and SMCCC_VERSION.
    adr x0, .Lel3_vectors
    msr vbar_el3, x0

    isb
    eret

    // ---------------------------------------------------------------
    // Minimal EL3 exception vector table (handles SMC from lower ELs)
    // ---------------------------------------------------------------
    // AArch64 vector table: 16 entries, 0x80 bytes each, 2KB aligned.
    // We only care about "Lower EL using AArch64, Synchronous" at
    // offset 0x400 from VBAR.
    .balign 2048
.Lel3_vectors:
    // Current EL with SP0 (offsets 0x000-0x1FF) — unused
    b .                    // 0x000 Synchronous
    .balign 0x80
    b .                    // 0x080 IRQ
    .balign 0x80
    b .                    // 0x100 FIQ
    .balign 0x80
    b .                    // 0x180 SError

    // Current EL with SPx (offsets 0x200-0x3FF) — unused
    .balign 0x80
    b .                    // 0x200 Synchronous
    .balign 0x80
    b .                    // 0x280 IRQ
    .balign 0x80
    b .                    // 0x300 FIQ
    .balign 0x80
    b .                    // 0x380 SError

    // Lower EL using AArch64 (offsets 0x400-0x5FF) — SMC handler
    .balign 0x80
    b .Lel3_smc_handler    // 0x400 Synchronous (SMC lands here)
    .balign 0x80
    b .                    // 0x480 IRQ
    .balign 0x80
    b .                    // 0x500 FIQ
    .balign 0x80
    b .                    // 0x580 SError

    // Lower EL using AArch32 (offsets 0x600-0x7FF) — unused
    .balign 0x80
    b .                    // 0x600 Synchronous
    .balign 0x80
    b .                    // 0x680 IRQ
    .balign 0x80
    b .                    // 0x700 FIQ
    .balign 0x80
    b .                    // 0x780 SError

    // SMC handler: check function ID in x0 (W0), return result in x0.
    // PSCI function IDs (SMCCC calling convention):
    //   PSCI_VERSION      = 0x84000000
    //   PSCI_FEATURES     = 0x8400000A
    //   SMCCC_VERSION     = 0x80000000
    //   PSCI_CPU_ON       = 0xC4000003
    //   PSCI_SYSTEM_OFF   = 0x84000008
    //   PSCI_SYSTEM_RESET = 0x84000009
.Lel3_smc_handler:
    // Check EC in ESR_EL3 to confirm this is an SMC
    mrs x9, esr_el3
    lsr x9, x9, #26
    cmp x9, #0x17              // EC=0x17 = SMC from AArch64
    b.ne .Lel3_smc_unknown

    // PSCI_VERSION (0x84000000) → return 1.1 (0x00010001)
    ldr w9, =0x84000000
    cmp w0, w9
    b.ne 1f
    ldr w0, =0x00010001        // PSCI v1.1
    b .Lel3_smc_return
1:
    // SMCCC_VERSION (0x80000000) → return 1.2 (0x00010002)
    ldr w9, =0x80000000
    cmp w0, w9
    b.ne 2f
    ldr w0, =0x00010002        // SMCCC v1.2
    b .Lel3_smc_return
2:
    // PSCI_FEATURES (0x8400000A) → return NOT_SUPPORTED for all
    ldr w9, =0x8400000A
    cmp w0, w9
    b.ne 3f
    mov x0, #-1                // NOT_SUPPORTED
    b .Lel3_smc_return
3:
    // PSCI_SYSTEM_OFF (0x84000008) → halt
    ldr w9, =0x84000008
    cmp w0, w9
    b.ne 4f
    wfi
    b .
4:
    // PSCI_SYSTEM_RESET (0x84000009) → halt (could do reset)
    ldr w9, =0x84000009
    cmp w0, w9
    b.ne 5f
    wfi
    b .
5:
    // FSTART_BOOT_BL31 (0xC2000002) — branch to BL31 at EL3.
    //
    // Called from EL1 to hand off control to TF-A BL31 at EL3.
    // Convention:  x1 = BL31 entry address, x2 = &BlParams.
    // The handler branches to BL31 with x0 = &BlParams (TF-A convention).
    // BL31 initialises the secure world, GIC, and PSCI, then ERETs to
    // the BL33 entry specified in BlParams (at EL2h or EL1h NS).
    movz x9, #0x0002
    movk x9, #0xC200, lsl #16 // 0xC2000002
    cmp x0, x9
    b.ne 6f
    dsb sy
    isb
    mov x0, x2                 // x0 = &BlParams (TF-A convention)
    br  x1                     // branch to BL31 at EL3
    // (BL31 never returns — it ERETs to BL33)

6:
    // FSTART_NS_SWITCH (0xC2000000) — switch caller to Non-Secure EL1.
    //
    // Used by CrabEFI's ExitBootServices trampoline: after all firmware
    // code is done (EBS cleanup, SetVirtualAddressMap), the trampoline
    // issues this SMC from a RAM page. The EL3 handler sets SCR_EL3.NS=1
    // and ERETs back. The caller (EFI stub / kernel) now runs at NS-EL1
    // where GICv3 Non-Secure Group 1 interrupts (MSI-X/LPIs) arrive as
    // IRQ instead of FIQ — fixing the NVMe interrupt routing issue.
    movz x9, #0xC200, lsl #16 // 0xC2000000 — no literal pool needed
    cmp x0, x9
    b.ne .Lel3_smc_unknown
    mrs x9, scr_el3
    orr x9, x9, #1            // NS=1 (bit 0)
    msr scr_el3, x9
    isb
    mov x0, #0                 // SUCCESS
    eret

.Lel3_smc_unknown:
    mov x0, #-1                // NOT_SUPPORTED
.Lel3_smc_return:
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
    /// Rust entry point generated by fstart-stage from Rust board metadata.
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

#[no_mangle]
#[link_section = ".page_tables"]
static mut MMU_L0_TABLE: PageTable = PageTable([0u64; 512]);

#[no_mangle]
#[link_section = ".page_tables"]
static mut MMU_L1_TABLE: PageTable = PageTable([0u64; 512]);

#[no_mangle]
#[link_section = ".page_tables"]
static mut MMU_L1_HIGH_TABLE: PageTable = PageTable([0u64; 512]);

// AArch64 platform support.
//
// Provides the reset vector entry point, stack setup, BSS clearing,
// and architecture-specific helpers.
//
// Three entry paths are supported:
//
// - **Default** (`entry.rs`): Standard AArch64 entry for platforms that
//   start directly in AArch64 mode (e.g., QEMU virt). Captures the DTB
//   address from `x0` at reset.
//
// - **Sunxi** (`entry_sunxi.rs`, behind `sunxi` feature): Entry for
//   Allwinner sun50i SoCs (H5, A64) that boot in AArch32 from the BROM.
//   Implements the ARMv8 RMR warm-reset sequence to switch into AArch64,
//   with FEL state saving for USB debug mode return.
//
// - **EL2 relocate** (`entry_relocate.rs`, behind `aarch64-el2-relocate-entry`):
//   Entry for TF-A/EL2-style boards entered from ROM/flash but linked to
//   execute from DRAM. This path expects an FDT pointer in `x0` and uses EL2
//   system registers during early relocation.

use core::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Boot parameters — written by _start assembly, read by Rust code
// ---------------------------------------------------------------------------

/// DTB address saved from `x0` at reset (written by `_start` assembly).
#[no_mangle]
static BOOT_DTB_ADDR: AtomicU64 = AtomicU64::new(0);

/// Return the DTB address passed by QEMU/firmware at reset (`x0`).
pub fn boot_dtb_addr() -> u64 {
    BOOT_DTB_ADDR.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// ARM Trusted Firmware (ATF) BL31 boot protocol
// ---------------------------------------------------------------------------

/// ATF parameter header — common header for all ATF parameter structs.
///
/// Reference: TF-A `include/common/ep_info.h`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ParamHeader {
    /// Type of parameter: `PARAM_EP` (1), `PARAM_IMAGE_BINARY` (2), etc.
    pub param_type: u8,
    /// Version of the struct.
    pub version: u8,
    /// Size of the struct in bytes.
    pub size: u16,
    /// Attributes bitfield.
    pub attr: u32,
}

/// Arguments passed to a BL image on entry (x0..x7).
///
/// For BL33 (Linux): `arg0` = DTB pointer, rest are 0.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Aapcs64Params {
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
    pub arg4: u64,
    pub arg5: u64,
    pub arg6: u64,
    pub arg7: u64,
}

/// Entry point information for a BL image.
///
/// Describes where and how to enter a particular firmware image.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EntryPointInfo {
    /// Header: type = `PARAM_EP` (1), version = 2.
    pub h: ParamHeader,
    /// Entry point address (PC).
    pub pc: u64,
    /// Saved Program Status Register value for the target EL.
    pub spsr: u32,
    /// Padding for alignment.
    pub _pad: u32,
    /// Arguments to pass (x0..x7).
    pub args: Aapcs64Params,
}

/// BL image node in the BL image execution sequence.
///
/// TF-A's `bl_params_node_t` — uses POINTERS to `image_info` and `ep_info`.
/// This differs from `image_desc_t` which embeds them inline.
#[repr(C)]
pub struct BlParamsNode {
    /// Image ID: `BL33_IMAGE_ID` = 5.
    pub image_id: u32,
    pub _pad: u32,
    /// Pointer to `ImageInfo` (null if not needed).
    pub image_info: u64,
    /// Pointer to `EntryPointInfo` for this image.
    pub ep_info: u64,
    /// Pointer to the next `BlParamsNode` (null for last).
    pub next_params_info: u64,
}

/// Top-level BL params structure passed to BL31 in `x0`.
///
/// TF-A's `bl_params_t` — contains a pointer to the head of the
/// `BlParamsNode` linked list.
///
/// Layout: `ParamHeader` (8 bytes) + `head` pointer (8 bytes) = 16 bytes.
/// No padding needed — `head` is naturally aligned at offset 8.
#[repr(C)]
pub struct BlParams {
    /// Header: type = `PARAM_BL_PARAMS` (0x05), version = 2.
    pub h: ParamHeader,
    /// Pointer to the head of the params node list.
    pub head: u64,
}

/// ATF constants.
pub mod atf {
    /// Parameter type: entry point info.
    pub const PARAM_EP: u8 = 0x01;
    /// Parameter type: BL params (PARAM_BL_PARAMS = 0x05 in TF-A).
    pub const PARAM_BL_PARAMS: u8 = 0x05;
    /// Struct version.
    pub const VERSION_2: u8 = 0x02;
    /// Image ID for BL33 (non-secure payload = Linux).
    pub const BL33_IMAGE_ID: u32 = 5;
    /// Attribute: non-secure image (bit 0 of ep_info.h security field).
    pub const EP_NON_SECURE: u32 = 0x1;
    /// Attribute: little-endian (EP_EE_LITTLE from ep_info.h).
    pub const EP_EE_LITTLE: u32 = 0x0;

    /// Compute SPSR for EL2h entry (AArch64).
    ///
    /// SPSR_EL3: M[3:0] = 0b1001 (EL2h), DAIF masked.
    pub const fn spsr_el2h() -> u32 {
        let el2h: u32 = 0b1001; // EL2h mode
        let daif: u32 = 0xF << 6; // mask D, A, I, F
        el2h | daif
    }
}

/// Build BL params for booting Linux via ATF BL31.
///
/// All output structs must live as long as the `BlParams` is used (all
/// are typically stack-allocated before the jump).
///
/// # Arguments
/// - `kernel_addr` — Linux kernel entry point
/// - `dtb_addr` — Patched DTB address (passed as x0 to Linux)
/// - `bl33_ep` — Output: filled-in EntryPointInfo for BL33
/// - `bl33_node` — Output: filled-in BlParamsNode for BL33
/// - `bl_params` — Output: filled-in BlParams
pub fn prepare_bl_params(
    kernel_addr: u64,
    dtb_addr: u64,
    bl33_ep: &mut EntryPointInfo,
    bl33_node: &mut BlParamsNode,
    bl_params: &mut BlParams,
) {
    // Fill in BL33 entry point info
    *bl33_ep = EntryPointInfo {
        h: ParamHeader {
            param_type: atf::PARAM_EP,
            version: atf::VERSION_2,
            size: core::mem::size_of::<EntryPointInfo>() as u16,
            attr: atf::EP_NON_SECURE | atf::EP_EE_LITTLE,
        },
        pc: kernel_addr,
        spsr: atf::spsr_el2h(),
        _pad: 0,
        args: Aapcs64Params {
            arg0: dtb_addr, // x0 = DTB pointer for Linux
            ..Default::default()
        },
    };

    // Fill in BL33 params node (TF-A uses pointers, not embedded structs)
    bl33_node.image_id = atf::BL33_IMAGE_ID;
    bl33_node._pad = 0;
    bl33_node.image_info = 0; // no image info needed
    bl33_node.ep_info = bl33_ep as *mut EntryPointInfo as u64;
    bl33_node.next_params_info = 0; // no more images

    // Fill in top-level bl_params
    bl_params.h = ParamHeader {
        param_type: atf::PARAM_BL_PARAMS,
        version: atf::VERSION_2,
        size: core::mem::size_of::<BlParams>() as u16,
        attr: 0,
    };
    bl_params.head = bl33_node as *mut BlParamsNode as u64;
}

/// Boot Linux via ATF BL31 with automatic parameter setup.
///
/// Convenience wrapper that allocates the required BL params structs on
/// the stack, fills them via [`prepare_bl_params`], and calls
/// [`boot_linux_atf`]. This encapsulates the three `mem::zeroed()` calls
/// and struct setup that callers would otherwise need to inline.
///
/// # Arguments
///
/// Unified Linux boot entry point.
///
/// Delegates to [`boot_linux_atf_prepared`] with the standard ATF
/// BL31 → Linux handoff.
///
/// Required fields: `kernel_addr`, `dtb_addr`, `fw_addr`.
/// Ignored fields: `rsdp_addr`, `bootargs`, `e820_entries`,
/// `zero_page_addr`, `hart_id`.
pub fn boot_linux(params: &fstart_core::services::BootLinuxParams<'_>) -> ! {
    boot_linux_atf_prepared(params.kernel_addr, params.dtb_addr, params.fw_addr)
}

pub fn boot_linux_atf_prepared(kernel_addr: u64, dtb_addr: u64, fw_addr: u64) -> ! {
    // SAFETY: all three structs are repr(C) with only integer fields,
    // so zeroed bytes form a valid (if empty) representation.
    let mut bl33_ep: EntryPointInfo = unsafe { core::mem::zeroed() };
    let mut bl33_node: BlParamsNode = unsafe { core::mem::zeroed() };
    let mut bl_params: BlParams = unsafe { core::mem::zeroed() };
    prepare_bl_params(
        kernel_addr,
        dtb_addr,
        &mut bl33_ep,
        &mut bl33_node,
        &mut bl_params,
    );
    boot_linux_atf(fw_addr, &bl_params)
}

/// FSTART_BOOT_BL31 SMC function ID.
///
/// Issues an SMC to the EL3 handler which branches to BL31 at EL3.
/// Convention: x0 = function ID, x1 = BL31 address, x2 = &BlParams.
const FSTART_BOOT_BL31: u64 = 0xC200_0002;

/// Jump to ATF BL31 via SMC, which then boots the BL33 payload at EL2.
///
/// This function issues an SMC to the EL3 handler, which branches to
/// BL31 at EL3. BL31 initialises the secure world (GIC, PSCI, etc.)
/// then `eret`s to the BL33 entry specified in `params` at EL2.
///
/// # Safety
///
/// The caller must ensure BL31 is loaded at `bl31_addr` and that
/// `params` is valid and will remain so until BL31 reads it.
pub fn boot_linux_atf(bl31_addr: u64, params: &BlParams) -> ! {
    unsafe {
        // Flush all pending stores before handing off to BL31.
        // SMC traps to EL3 where the handler branches to BL31.
        core::arch::asm!(
            "dsb sy",
            "isb",
            "smc #0",
            in("x0") FSTART_BOOT_BL31,
            in("x1") bl31_addr,
            in("x2") params as *const BlParams as u64,
            options(noreturn),
        );
    }
}

// ---------------------------------------------------------------------------
// BL31 boot-and-resume (for UEFI / non-terminal BL31 integration)
// ---------------------------------------------------------------------------

/// Saved callee-saved register context for BL31 resume.
///
/// Layout: x19-x29 (11 regs), x30/LR (1 reg), SP (1 reg) = 13 × 8 = 104 bytes.
#[repr(C, align(16))]
struct ResumeContext {
    regs: [u64; 13],
}

/// Static context for the BL31 resume trampoline.
///
/// Written by [`boot_bl31_and_resume`] before the SMC, read by the
/// trampoline after BL31 ERETs to BL33.
#[no_mangle]
static mut BL31_RESUME_CTX: ResumeContext = ResumeContext { regs: [0; 13] };

/// Static BL params for BL31 boot-and-resume.
///
/// Must be static because the SMC handler reads them after the caller's
/// stack frame is conceptually gone (BL31 runs asynchronously from the
/// caller's perspective).
#[no_mangle]
static mut BL31_RESUME_EP: EntryPointInfo = EntryPointInfo {
    h: ParamHeader {
        param_type: atf::PARAM_EP,
        version: atf::VERSION_2,
        size: core::mem::size_of::<EntryPointInfo>() as u16,
        attr: atf::EP_NON_SECURE | atf::EP_EE_LITTLE,
    },
    pc: 0,
    spsr: 0,
    _pad: 0,
    args: Aapcs64Params {
        arg0: 0,
        arg1: 0,
        arg2: 0,
        arg3: 0,
        arg4: 0,
        arg5: 0,
        arg6: 0,
        arg7: 0,
    },
};

#[no_mangle]
static mut BL31_RESUME_NODE: BlParamsNode = BlParamsNode {
    image_id: atf::BL33_IMAGE_ID,
    _pad: 0,
    image_info: 0,
    ep_info: 0,
    next_params_info: 0,
};

#[no_mangle]
static mut BL31_RESUME_PARAMS: BlParams = BlParams {
    h: ParamHeader {
        param_type: atf::PARAM_BL_PARAMS,
        version: atf::VERSION_2,
        size: core::mem::size_of::<BlParams>() as u16,
        attr: 0,
    },
    head: 0,
};

/// Boot BL31 and resume execution after BL31 initialises.
///
/// This function appears to "return" from the caller's perspective,
/// but control actually flows through BL31:
///
/// 1. Saves callee-saved registers (x19-x30, SP) to a static.
/// 2. Prepares BL params with BL33 = resume trampoline.
/// 3. Issues `SMC FSTART_BOOT_BL31` → EL3 handler → BL31 at EL3.
/// 4. BL31 initialises GIC, PSCI, secure world.
/// 5. BL31 ERETs to the resume trampoline at EL2h (Non-Secure).
/// 6. Trampoline restores registers and returns to the caller.
///
/// After return, the caller runs at EL2 Non-Secure with a fully
/// initialised GIC and PSCI implementation.
pub fn boot_bl31_and_resume(bl31_addr: u64, dtb_addr: u64) {
    extern "C" {
        fn _bl31_resume_trampoline();
        fn _bl31_save_and_smc(func_id: u64, bl31_addr: u64, params: u64);
    }

    // SAFETY: firmware boot is single-threaded. The statics are only
    // written here and read by the trampoline (which runs after BL31
    // returns control). No concurrent access.
    unsafe {
        // Fill BL33 entry point: target = trampoline, mode = EL2h NS
        BL31_RESUME_EP.pc = (_bl31_resume_trampoline as *const ()) as u64;
        BL31_RESUME_EP.spsr = atf::spsr_el2h();
        BL31_RESUME_EP.args.arg0 = dtb_addr;

        // Link the params chain
        BL31_RESUME_NODE.ep_info = &raw const BL31_RESUME_EP as *const EntryPointInfo as u64;
        BL31_RESUME_PARAMS.head = &raw const BL31_RESUME_NODE as *const BlParamsNode as u64;

        // Save callee-saved registers, SMC to BL31, resume via trampoline.
        _bl31_save_and_smc(
            FSTART_BOOT_BL31,
            bl31_addr,
            &raw const BL31_RESUME_PARAMS as *const BlParams as u64,
        );
    }
}

// The save-context/SMC/trampoline is written in global_asm to avoid
// complex inline asm clobber lists.  The trampoline is the BL33 entry
// that BL31 ERETs to — it restores callee-saved registers and returns
// to the caller of boot_bl31_and_resume().
global_asm!(
    r#"
    .section .text
    .global _bl31_save_and_smc
    .type _bl31_save_and_smc, @function

    // _bl31_save_and_smc(x0=func_id, x1=bl31_addr, x2=&BlParams)
    //
    // Saves callee-saved registers (x19-x30, SP) to BL31_RESUME_CTX,
    // then issues SMC FSTART_BOOT_BL31.  Control never falls through —
    // the EL3 handler branches to BL31 which eventually ERETs to the
    // trampoline below.
_bl31_save_and_smc:
    adrp x3, BL31_RESUME_CTX
    add  x3, x3, :lo12:BL31_RESUME_CTX
    stp x19, x20, [x3, #0]
    stp x21, x22, [x3, #16]
    stp x23, x24, [x3, #32]
    stp x25, x26, [x3, #48]
    stp x27, x28, [x3, #64]
    stp x29, x30, [x3, #80]
    mov x4, sp
    str x4, [x3, #96]

    dsb sy
    isb
    smc #0
    // --- unreachable: EL3 handler branches to BL31 ---

    // BL31 resume trampoline — BL33 entry point.
    // BL31 ERETs here at EL2h NS after completing init.
    // Restores callee-saved registers and returns to the caller
    // of boot_bl31_and_resume().
    .global _bl31_resume_trampoline
    .type _bl31_resume_trampoline, @function
_bl31_resume_trampoline:
    adrp x3, BL31_RESUME_CTX
    add  x3, x3, :lo12:BL31_RESUME_CTX
    ldp x19, x20, [x3, #0]
    ldp x21, x22, [x3, #16]
    ldp x23, x24, [x3, #32]
    ldp x25, x26, [x3, #48]
    ldp x27, x28, [x3, #64]
    ldp x29, x30, [x3, #80]
    ldr x4, [x3, #96]
    mov sp, x4
    ret   // returns to caller of boot_bl31_and_resume via saved LR
"#
);

// ---------------------------------------------------------------------------
// Basic helpers
// ---------------------------------------------------------------------------

/// Halt the processor.
#[inline(always)]
pub fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfe");
        }
    }
}

/// Jump to an address, transferring control unconditionally.
///
/// Used by `StageLoad` and `PayloadLoad` to transfer control to the
/// next stage or payload after loading it into memory.
///
/// # Safety
///
/// The caller must ensure:
/// - `addr` points to valid executable code
/// - The stack and BSS will be set up by the target code (its own `_start`)
/// - This function never returns
#[inline(always)]
pub fn jump_to(addr: u64) -> ! {
    unsafe {
        core::arch::asm!(
            "br {0}",
            in(reg) addr,
            options(noreturn),
        );
    }
}

/// Jump to the next stage, passing the handoff address in x0.
///
/// # Safety
///
/// The caller must ensure:
/// - `addr` points to valid AArch64 executable code
/// - `handoff_addr` points to a valid serialized `StageHandoff` (or 0)
/// - This function never returns
#[inline(always)]
pub fn jump_to_with_handoff(addr: u64, handoff_addr: usize) -> ! {
    unsafe {
        core::arch::asm!(
            "br {addr}",
            addr = in(reg) addr,
            in("x0") handoff_addr,
            options(noreturn),
        );
    }
}
