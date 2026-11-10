//! AST2500 address map shared by the build plan and the stages.

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
/// DRAM starts here whatever its size.
pub const DRAM_BASE: u64 = 0x8000_0000;
/// The DRAM main stage: code, data, heap and stack. The bootblock refuses
/// DRAM configurations whose usable part ends below it.
pub const MAIN_STAGE_BASE: u64 = 0x8200_0000;
pub const MAIN_STAGE_SIZE: u64 = 0x0020_0000;
/// The bootblock's handoff to the main stage, just below it.
pub const HANDOFF_ADDR: u64 = 0x81FF_F000;
/// On-chip SRAM: the bootblock's data, heap and stack, then the boot
/// timestamp table, which the main stage takes over.
pub const SRAM_BASE: u64 = 0x1E72_0000;
pub const SRAM_SIZE: u64 = 0x9000;
pub const TIMESTAMPS_SIZE: u64 = 0x400;
pub const TIMESTAMPS: u64 = SRAM_BASE + SRAM_SIZE - TIMESTAMPS_SIZE;
