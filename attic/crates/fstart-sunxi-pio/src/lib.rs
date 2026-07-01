//! Allwinner sunxi PIO (GPIO) pin controller — shared utility functions.
//!
//! Provides pin function selection, pull-up/down configuration, and drive
//! strength control for all sunxi SoC generations. The register layout
//! within each port bank is identical across A10/A20 (sun4i/sun7i),
//! H3/H5 (sun8i/sun50i), and D1/T113 (sun20i/NCAT2), with two
//! generations differing only in bank stride and pull register offset.
//!
//! Modeled directly on U-Boot's `drivers/gpio/sunxi_gpio.c` primitives.
//!
//! ## Two generations
//!
//! The `SUNXI_NEW_PINCTRL` flag in U-Boot selects between:
//!
//! | Property | Legacy (A20, H3, H5, …) | NCAT2 (D1, T113, …) |
//! |----------|-------------------------|----------------------|
//! | Bank stride | 0x24 (36 bytes) | 0x30 (48 bytes) |
//! | DRV bits per pin | 2 | 4 |
//! | PULL0 offset | +0x1C | +0x24 |
//!
//! Everything else (CFG layout, DATA offset, PULL encoding, CFG encoding)
//! is identical.
//!
//! ## Port/pin addressing
//!
//! Ports are identified by index: A=0, B=1, …, H=7.  Pins within a port
//! are 0-31.  This matches U-Boot's `SUNXI_GPIO_A` through `SUNXI_GPIO_H`
//! constants and the `SUNXI_GPx(N)` macros.

#![no_std]

// ---------------------------------------------------------------------------
// Generation-dependent constants
// ---------------------------------------------------------------------------

/// PIO pin controller generation — determines bank stride and pull offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PioGen {
    /// A10, A20, H3, H5, A64, H6, H616, … — bank stride 0x24.
    Legacy,
    /// D1, T113, R528 (NCAT2) — bank stride 0x30.
    Ncat2,
}

/// Bank stride in bytes between consecutive port register blocks.
const BANK_STRIDE_LEGACY: usize = 0x24;
const BANK_STRIDE_NCAT2: usize = 0x30;

/// Offset of CFG0 register within a port bank (same for both generations).
const CFG0_OFF: usize = 0x00;

/// Offset of DATA register within a port bank (same for both generations).
#[allow(dead_code)]
const DATA_OFF: usize = 0x10;

/// Offset of DRV0 register within a port bank (same for both generations).
const DRV0_OFF: usize = 0x14;

/// Offset of PULL0 register within a port bank (varies by generation).
const PULL0_OFF_LEGACY: usize = 0x1C;
const PULL0_OFF_NCAT2: usize = 0x24;

// ---------------------------------------------------------------------------
// Pull mode
// ---------------------------------------------------------------------------

/// Pin pull-up/pull-down configuration.
///
/// Encoding is identical across all sunxi generations (2 bits per pin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pull {
    /// No pull (floating).
    Disabled = 0,
    /// Internal pull-up.
    Up = 1,
    /// Internal pull-down.
    Down = 2,
}

// ---------------------------------------------------------------------------
// Port index constants
// ---------------------------------------------------------------------------

/// Port A index.
pub const PORT_A: u8 = 0;
/// Port B index.
pub const PORT_B: u8 = 1;
/// Port C index.
pub const PORT_C: u8 = 2;
/// Port D index.
pub const PORT_D: u8 = 3;
/// Port E index.
pub const PORT_E: u8 = 4;
/// Port F index.
pub const PORT_F: u8 = 5;
/// Port G index.
pub const PORT_G: u8 = 6;
/// Port H index.
pub const PORT_H: u8 = 7;

// ---------------------------------------------------------------------------
// SunxiPio — pin controller handle
// ---------------------------------------------------------------------------

/// Handle to the sunxi PIO (GPIO) pin controller.
///
/// Constructed from a PIO base address and generation selector.
/// Provides U-Boot–compatible pin configuration primitives.
pub struct SunxiPio {
    base: usize,
    bank_stride: usize,
    pull0_off: usize,
}

impl SunxiPio {
    /// Create a new PIO handle.
    ///
    /// - `base`: PIO register base address (e.g., `0x01C2_0800` for A20/H3,
    ///   `0x0200_0000` for D1).
    /// - `gen`: PIO generation (determines bank stride and pull offset).
    pub fn new(base: usize, gen: PioGen) -> Self {
        let (bank_stride, pull0_off) = match gen {
            PioGen::Legacy => (BANK_STRIDE_LEGACY, PULL0_OFF_LEGACY),
            PioGen::Ncat2 => (BANK_STRIDE_NCAT2, PULL0_OFF_NCAT2),
        };
        Self {
            base,
            bank_stride,
            pull0_off,
        }
    }

    /// Compute the base address of a port bank.
    #[inline(always)]
    fn bank_base(&self, port: u8) -> usize {
        self.base + (port as usize) * self.bank_stride
    }

    /// Set the alternate function for a single pin.
    ///
    /// Each pin has a 4-bit function field in one of four CFG registers
    /// (CFG0–CFG3) per port bank.
    ///
    /// Matches U-Boot's `sunxi_gpio_set_cfgbank()`.
    pub fn set_function(&self, port: u8, pin: u8, func: u8) {
        let bank = self.bank_base(port);
        let reg_index = (pin >> 3) as usize; // which CFG register (0-3)
        let bit_offset = ((pin & 0x7) as u32) << 2; // bit position within register
        let addr = (bank + CFG0_OFF + reg_index * 4) as *mut u32;

        // SAFETY: addr is a valid PIO MMIO register at a fixed hardware
        // address computed from the board-provided PIO base.
        unsafe {
            let val = fstart_mmio::read32(addr as *const u32);
            let val = (val & !(0xF << bit_offset)) | ((func as u32 & 0xF) << bit_offset);
            fstart_mmio::write32(addr, val);
        }
    }

    /// Set the pull-up/down mode for a single pin.
    ///
    /// Each pin has a 2-bit pull field in one of two PULL registers
    /// per port bank.  The PULL register offset within the bank differs
    /// between Legacy (0x1C) and NCAT2 (0x24) generations.
    ///
    /// Matches U-Boot's `sunxi_gpio_set_pull_bank()`.
    pub fn set_pull(&self, port: u8, pin: u8, pull: Pull) {
        let bank = self.bank_base(port);
        let reg_index = (pin >> 4) as usize; // which PULL register (0-1)
        let bit_offset = ((pin & 0xF) as u32) << 1; // bit position
        let addr = (bank + self.pull0_off + reg_index * 4) as *mut u32;

        // SAFETY: addr is a valid PIO MMIO register at a fixed hardware
        // address computed from the board-provided PIO base.
        unsafe {
            let val = fstart_mmio::read32(addr as *const u32);
            let val = (val & !(0x3 << bit_offset)) | ((pull as u32) << bit_offset);
            fstart_mmio::write32(addr, val);
        }
    }

    /// Set the drive strength for a single pin.
    ///
    /// On Legacy SoCs: 2 bits per pin (16 pins per register).
    /// On NCAT2 SoCs: 4 bits per pin (8 pins per register).
    ///
    /// The `gen` is needed to select the correct bit width.
    /// `strength` is the raw register value (0–3 for Legacy, 0–15 for NCAT2).
    ///
    /// Matches U-Boot's `sunxi_gpio_set_drv_bank()`.
    pub fn set_drive(&self, port: u8, pin: u8, strength: u8, gen: PioGen) {
        let bank = self.bank_base(port);
        let (reg_index, bit_offset, mask) = match gen {
            PioGen::Legacy => {
                let idx = (pin >> 4) as usize;
                let off = ((pin & 0xF) as u32) << 1;
                (idx, off, 0x3u32)
            }
            PioGen::Ncat2 => {
                let idx = (pin >> 3) as usize;
                let off = ((pin & 0x7) as u32) << 2;
                (idx, off, 0xFu32)
            }
        };
        let addr = (bank + DRV0_OFF + reg_index * 4) as *mut u32;

        // SAFETY: addr is a valid PIO MMIO register at a fixed hardware
        // address computed from the board-provided PIO base.
        unsafe {
            let val = fstart_mmio::read32(addr as *const u32);
            let val = (val & !(mask << bit_offset)) | ((strength as u32 & mask) << bit_offset);
            fstart_mmio::write32(addr, val);
        }
    }

    /// Bulk-set pin functions for consecutive pins within one CFG register.
    ///
    /// Writes the entire 32-bit CFG register directly.  Useful for MMC
    /// setup where 6+ pins in the same port share the same function.
    pub fn write_cfg_raw(&self, port: u8, cfg_index: usize, val: u32) {
        let addr = (self.bank_base(port) + CFG0_OFF + cfg_index * 4) as *mut u32;
        // SAFETY: addr is a valid PIO MMIO register.
        unsafe { fstart_mmio::write32(addr, val) }
    }

    /// Bulk-set drive strength for a port's DRV register.
    pub fn write_drv_raw(&self, port: u8, drv_index: usize, val: u32) {
        let addr = (self.bank_base(port) + DRV0_OFF + drv_index * 4) as *mut u32;
        // SAFETY: addr is a valid PIO MMIO register.
        unsafe { fstart_mmio::write32(addr, val) }
    }

    /// Bulk-set pull modes for a port's PULL register.
    pub fn write_pull_raw(&self, port: u8, pull_index: usize, val: u32) {
        let addr = (self.bank_base(port) + self.pull0_off + pull_index * 4) as *mut u32;
        // SAFETY: addr is a valid PIO MMIO register.
        unsafe { fstart_mmio::write32(addr, val) }
    }
}
