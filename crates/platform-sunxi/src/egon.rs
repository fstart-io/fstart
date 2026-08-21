//! Allwinner eGON boot header support for the A20 MMC boot flow.

/// eGON checksum sentinel replaced by image assembly.
pub const EGON_STAMP_CHECKSUM: u32 = 0x5F0A_6C39;
/// eGON boot-header magic.
pub const EGON_MAGIC: [u8; 8] = *b"eGON.BT0";

/// Allwinner eGON.BT0 header, excluding the preceding branch instruction.
#[repr(C)]
pub struct EgonHead {
    pub magic: [u8; 8],
    pub checksum: u32,
    pub length: u32,
    pub spl_signature: [u8; 4],
    pub _reserved1: [u32; 3],
    pub _dram_size: u32,
    pub boot_media: u32,
    pub next_stage_offset: u32,
    pub next_stage_size: u32,
    pub ffs_total_size: u32,
    pub _reserved2: [u32; 10],
}

impl EgonHead {
    #[must_use]
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

impl Default for EgonHead {
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!((core::mem::size_of::<EgonHead>() + 4).is_multiple_of(32));

/// Boot source written by the BROM into the in-SRAM eGON header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootDevice {
    Mmc0,
    Nand,
    Mmc2,
    Spi,
    Mmc0High,
    Mmc2High,
    Fel,
    Unknown(u8),
}

/// Read the BROM boot-medium byte from an in-SRAM eGON header.
#[inline]
pub fn boot_media_at(sram_base: usize) -> u8 {
    // SAFETY: the A20 BROM loads the eGON header at the supplied SRAM base.
    unsafe { core::ptr::read_volatile((sram_base + 0x28) as *const u8) }
}

/// Decode the BROM boot device from an in-SRAM eGON header.
pub fn boot_device_at(sram_base: usize) -> BootDevice {
    // SAFETY: the eGON header at SRAM base is readable after BROM load.
    let magic = unsafe { core::ptr::read_volatile((sram_base + 4) as *const [u8; 8]) };
    if magic != EGON_MAGIC {
        return BootDevice::Fel;
    }

    match boot_media_at(sram_base) {
        0x00 => BootDevice::Mmc0,
        0x01 => BootDevice::Nand,
        0x02 => BootDevice::Mmc2,
        0x03 => BootDevice::Spi,
        0x10 => BootDevice::Mmc0High,
        0x12 => BootDevice::Mmc2High,
        value => BootDevice::Unknown(value),
    }
}

/// Read the eGON next-stage offset from the in-SRAM header.
#[inline]
pub fn next_stage_offset_at(sram_base: usize) -> u32 {
    // SAFETY: BROM-loaded eGON header is readable at this fixed offset.
    unsafe { core::ptr::read_volatile((sram_base + 0x2c) as *const u32) }
}

/// Read the eGON next-stage size from the in-SRAM header.
#[inline]
pub fn next_stage_size_at(sram_base: usize) -> u32 {
    // SAFETY: BROM-loaded eGON header is readable at this fixed offset.
    unsafe { core::ptr::read_volatile((sram_base + 0x30) as *const u32) }
}

/// Read the total FFS image size from the in-SRAM eGON header.
#[inline]
pub fn ffs_total_size_at(sram_base: usize) -> u32 {
    // SAFETY: BROM-loaded eGON header is readable at this fixed offset.
    unsafe { core::ptr::read_volatile((sram_base + 0x34) as *const u32) }
}

#[cfg(all(feature = "stage", target_arch = "arm", fstart_stage_env = "car"))]
core::arch::global_asm!(
    r#"
    .section .head.text, "ax"
    .global _head_jump
    .arm
_head_jump:
    b _start
    "#
);

// 64-bit sunxi BROMs still enter the eGON image in AArch32; the head word is
// the pre-assembled ARM32 branch to `_start` at offset 0x60 (the aarch64
// assembler cannot emit ARM32 instructions).
// 0xEA000016 = ARM32 `b .+0x60` from offset 0 (offset = (0x60 - 8) / 4).
#[cfg(all(feature = "stage", target_arch = "aarch64", fstart_stage_env = "car"))]
core::arch::global_asm!(
    r#"
    .section .head.text, "ax"
    .global _head_jump
_head_jump:
    .word 0xEA000016
    "#
);

// The D1 BROM enters the eGON image directly in RISC-V M-mode.
#[cfg(all(feature = "stage", target_arch = "riscv64", fstart_stage_env = "car"))]
core::arch::global_asm!(
    r#"
    .section .head.text, "ax"
    .global _head_jump
_head_jump:
    j _start
    "#
);

/// The header immediately following the eGON branch instruction.
#[cfg(all(feature = "stage", fstart_stage_env = "car"))]
#[used]
#[cfg_attr(
    any(target_arch = "arm", target_arch = "aarch64", target_arch = "riscv64"),
    unsafe(link_section = ".head.egon")
)]
pub static EGON_HEAD: EgonHead = EgonHead::new();
