//! ASPEED address maps shared by the build plans and the stages.
//!
//! The flash windows are the same on every SoC; DRAM and SRAM move between
//! generations, so each SoC has a [`Layout`].

/// CE0 is mapped at the reset vector; the bootblock executes it there.
pub const FLASH_BASE: u64 = 0;
/// CE0's decode window, through which the stages read the FFS: address 0
/// is no valid buffer address.
pub const FLASH_ALIAS: u64 = 0x2000_0000;
/// What 3-byte SPI addressing reaches after reset.
pub const FIRMWARE_WINDOW_SIZE: u64 = 16 * 1024 * 1024;
/// Flash the bootblock reads to calibrate the SPI read timing, as an
/// offset into CE0: 4 KiB of its own code.
pub const FLASH_CALIBRATION: u64 = 0x1000;
pub const FLASH_CALIBRATION_SIZE: u64 = 0x1000;
/// The boot timestamp table at the end of SRAM.
pub const TIMESTAMPS_SIZE: u64 = 0x400;

/// Where one SoC keeps DRAM, SRAM and the stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// DRAM starts here whatever its size.
    pub dram_base: u64,
    /// The DRAM main stage: code, data, heap and stack. The bootblock
    /// refuses DRAM configurations whose usable part ends below it.
    pub main_stage_base: u64,
    pub main_stage_size: u64,
    /// The bootblock's handoff to the main stage, just below it.
    pub handoff_addr: u64,
    /// On-chip SRAM: the bootblock's data, heap and stack, then the boot
    /// timestamp table, which the main stage takes over.
    pub sram_base: u64,
    pub sram_size: u64,
}

impl Layout {
    /// The boot timestamp table, the last [`TIMESTAMPS_SIZE`] bytes of SRAM.
    pub const fn timestamps(&self) -> u64 {
        self.sram_base + self.sram_size - TIMESTAMPS_SIZE
    }
}

/// The main stage 32 MiB into DRAM and the handoff page below it, out of
/// the way of what DRAM training tests at its start.
const fn dram_stages(dram_base: u64, sram_base: u64, sram_size: u64) -> Layout {
    Layout {
        dram_base,
        main_stage_base: dram_base + 0x0200_0000,
        main_stage_size: 0x0020_0000,
        handoff_addr: dram_base + 0x01FF_F000,
        sram_base,
        sram_size,
    }
}

/// AST2400 and AST1250: DDR2/DDR3 at 1 GiB, 32 KiB of SRAM.
pub const AST2400: Layout = dram_stages(0x4000_0000, 0x1E72_0000, 0x8000);
/// AST2500: DDR3/DDR4 at 2 GiB, 36 KiB of SRAM.
pub const AST2500: Layout = dram_stages(0x8000_0000, 0x1E72_0000, 0x9000);
/// AST2600: DDR4 at 2 GiB, 89 KiB of SRAM at 256 MiB. Not used by any
/// stage yet.
pub const AST2600: Layout = dram_stages(0x8000_0000, 0x1000_0000, 0x1_6400);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ast2500_keeps_its_addresses() {
        assert_eq!(AST2500.main_stage_base, 0x8200_0000);
        assert_eq!(AST2500.handoff_addr, 0x81FF_F000);
        assert_eq!(AST2500.timestamps(), 0x1E72_8C00);
    }

    #[test]
    fn the_timestamp_table_stays_in_sram() {
        for l in [AST2400, AST2500, AST2600] {
            assert!(l.timestamps() >= l.sram_base);
            assert_eq!(l.timestamps() + TIMESTAMPS_SIZE, l.sram_base + l.sram_size);
            assert!(l.handoff_addr + 0x1000 <= l.main_stage_base);
        }
    }
}
