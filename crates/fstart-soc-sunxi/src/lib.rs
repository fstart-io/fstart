//! Allwinner SoC-specific support (sunxi family).
//!
//! Contains boot ROM format definitions, FEL mode support, and other
//! SoC-specific utilities that are not generic to the ARM architecture.
//!
//! Currently supports the eGON boot header used by Allwinner A20 (sun7i)
//! and other sunxi SoCs.

#![no_std]

// ---------------------------------------------------------------------------
// eGON boot header — Allwinner SoC boot ROM format
// ---------------------------------------------------------------------------

/// Allwinner eGON.BT0 boot header.
///
/// The boot ROM (BROM) scans the boot medium for the `"eGON.BT0"` magic
/// signature. When found, it validates the checksum and loads `length`
/// bytes into SRAM.
///
/// Layout (96 bytes total, 32-byte aligned):
/// - Offset 0x00: branch instruction (4 bytes) — jumps over the header
/// - Offset 0x04: this struct (92 bytes)
///
/// Field offsets below are relative to the start of the image (including
/// the 4-byte branch instruction), matching U-Boot's `boot_file_head`
/// in `include/sunxi_image.h`.
///
/// The branch instruction is emitted separately via `global_asm!` in
/// the generated stage code. This struct is placed immediately after it
/// via `#[link_section = ".head.egon"]`.
///
/// `length` and `checksum` are placeholders, patched by xtask post-build.
/// `next_stage_offset` and `next_stage_size` are patched by the FFS
/// assembler after both stages are built.
#[repr(C)]
pub struct EgonHead {
    /// Magic signature: `"eGON.BT0"` (8 bytes). Image offset 0x04.
    pub magic: [u8; 8],
    /// Checksum over the image (word-add). Image offset 0x0C.
    /// Initially `EGON_STAMP_CHECKSUM`; patched by xtask.
    pub checksum: u32,
    /// Total image length in bytes (8K-aligned). Image offset 0x10.
    /// Patched by xtask.
    pub length: u32,
    /// SPL signature: `"SPL\x02"`. Image offset 0x14.
    /// Written by xtask eGON patching (U-Boot compatible).
    pub spl_signature: [u8; 4],
    /// Reserved (U-Boot: fel_script_address, fel_uEnv_length,
    /// dt_name_offset). Image offsets 0x18, 0x1C, 0x20.
    pub _reserved1: [u32; 3],
    /// DRAM size in bytes (U-Boot compat). Image offset 0x24.
    pub _dram_size: u32,
    /// Boot medium type — **written by the BROM** after loading the
    /// image into SRAM. Image offset 0x28.
    ///
    /// Values (low 8 bits): 0=MMC0, 1=NAND, 2=MMC2, 3=SPI,
    /// 0x10=MMC0-high, 0x12=MMC2-high.
    pub boot_media: u32,
    /// Byte offset of the next stage within the FFS image. Image
    /// offset 0x2C. Patched by the FFS assembler.
    pub next_stage_offset: u32,
    /// Size (bytes) of the next stage flat binary. Image offset 0x30.
    /// Patched by the FFS assembler.
    pub next_stage_size: u32,
    /// Total FFS image size in bytes. Image offset 0x34.
    /// Patched by the FFS assembler. Used by subsequent stages to
    /// locate the FFS anchor at `ffs_total_size - ANCHOR_SIZE`.
    pub ffs_total_size: u32,
    /// Remaining padding. Image offsets 0x38–0x5F.
    pub _reserved2: [u32; 10],
}

/// Sentinel value for the checksum field before patching.
///
/// The BROM replaces this with the actual checksum during verification.
/// Xtask uses this to locate and patch the checksum.
pub const EGON_STAMP_CHECKSUM: u32 = 0x5F0A6C39;

/// eGON magic bytes: `"eGON.BT0"`.
pub const EGON_MAGIC: [u8; 8] = *b"eGON.BT0";

impl Default for EgonHead {
    fn default() -> Self {
        Self::new()
    }
}

impl EgonHead {
    /// Create a header with placeholder length and checksum fields.
    ///
    /// Fields patched post-build:
    /// - `length`, `checksum`, `spl_signature` — by xtask eGON patching.
    /// - `next_stage_offset`, `next_stage_size` — by the FFS assembler.
    /// - `boot_media` — by the BROM at runtime (not compile-time).
    ///
    /// `checksum` is set to `EGON_STAMP_CHECKSUM` as a sentinel so
    /// xtask can verify the header is present before patching.
    pub const fn new() -> Self {
        Self {
            magic: EGON_MAGIC,
            checksum: EGON_STAMP_CHECKSUM,
            length: 0,
            spl_signature: [0; 4],
            _reserved1: [0; 3],
            _dram_size: 0,
            boot_media: 0,
            next_stage_offset: 0,
            next_stage_size: 0,
            ffs_total_size: 0,
            _reserved2: [0; 10],
        }
    }
}

// Compile-time size check: branch(4) + struct(92) = 96 bytes = 3 * 32.
const _: () = {
    assert!(
        (core::mem::size_of::<EgonHead>() + 4).is_multiple_of(32),
        "eGON header + branch must be 32-byte aligned"
    );
};

// ---------------------------------------------------------------------------
// Boot medium detection (sunxi-specific)
// ---------------------------------------------------------------------------

/// Boot medium: MMC0 (SD card at 8 KB offset).
pub const BOOT_MEDIA_MMC0: u8 = 0x00;
/// Boot medium: NAND flash.
pub const BOOT_MEDIA_NAND: u8 = 0x01;
/// Boot medium: MMC2 (eMMC at 8 KB offset).
pub const BOOT_MEDIA_MMC2: u8 = 0x02;
/// Boot medium: SPI NOR flash.
pub const BOOT_MEDIA_SPI: u8 = 0x03;
/// Boot medium: MMC0, SPL at 128 KB offset (newer SoCs).
pub const BOOT_MEDIA_MMC0_HIGH: u8 = 0x10;
/// Boot medium: MMC2, SPL at 128 KB offset (newer SoCs).
pub const BOOT_MEDIA_MMC2_HIGH: u8 = 0x12;

/// Read the boot medium type from the eGON header in SRAM.
///
/// The BROM writes the boot device type at image offset 0x28 after
/// loading the bootblock into SRAM. Since the BROM patches the
/// in-SRAM copy, this must be read via volatile.
///
/// Returns one of the `BOOT_MEDIA_*` constants.
#[inline]
pub fn boot_media() -> u8 {
    // SAFETY: The eGON header starts at SRAM address 0x00 (for A20).
    // Offset 0x28 is the boot_media field, written by the BROM.
    unsafe { core::ptr::read_volatile(0x28 as *const u8) }
}

/// Read the next-stage offset from the eGON header in SRAM.
///
/// This field is patched by the FFS assembler. Must be read via volatile
/// because the compiler sees the source-level value as 0.
#[inline]
pub fn next_stage_offset() -> u32 {
    unsafe { core::ptr::read_volatile(0x2C as *const u32) }
}

/// Read the next-stage size from the eGON header in SRAM.
///
/// This field is patched by the FFS assembler. Must be read via volatile
/// because the compiler sees the source-level value as 0.
#[inline]
pub fn next_stage_size() -> u32 {
    unsafe { core::ptr::read_volatile(0x30 as *const u32) }
}

/// Read the total FFS image size from the eGON header in SRAM.
///
/// This field is patched by the FFS assembler. Subsequent stages use
/// this to locate the FFS anchor at `ffs_total_size - ANCHOR_SIZE`
/// on the boot medium.
#[inline]
pub fn ffs_total_size() -> u32 {
    unsafe { core::ptr::read_volatile(0x34 as *const u32) }
}

// ---------------------------------------------------------------------------
// FEL mode support (sunxi-specific)
// ---------------------------------------------------------------------------

/// Saved BROM state for FEL mode return.
///
/// The `save_boot_params` assembly routine (called as the very first thing
/// after reset) saves the BROM's sp, lr, CPSR, SCTLR, and VBAR into this
/// struct.  `return_to_fel` restores them to return to the BROM's FEL
/// handler.
///
/// Placed in `.data` (not `.bss`) because it is written before BSS is cleared.
#[repr(C)]
pub struct FelStash {
    /// Stack pointer at BROM handoff.
    pub sp: u32,
    /// Link register (return address) at BROM handoff.
    pub lr: u32,
    /// CPSR at BROM handoff.
    pub cpsr: u32,
    /// SCTLR (System Control Register) at BROM handoff.
    pub sctlr: u32,
    /// VBAR (Vector Base Address Register) at BROM handoff.
    pub vbar: u32,
}

extern "C" {
    /// Saved BROM state, written by `save_boot_params` in entry.rs.
    ///
    /// # Safety
    ///
    /// This is written once during early boot (before BSS clear) and
    /// read when returning to FEL mode.  It must not be placed in BSS.
    #[link_name = "fel_stash"]
    pub static FEL_STASH: FelStash;

    /// Return to BROM FEL mode.
    ///
    /// Restores the saved BROM state (VBAR, SCTLR, CPSR) from `fel_stash`
    /// and returns via the saved lr.  This function never returns.
    ///
    /// # Safety
    ///
    /// Must only be called after `save_boot_params` has run.  The saved
    /// sp and lr values must be passed from `FEL_STASH`.
    pub fn return_to_fel(sp: u32, lr: u32) -> !;
}
