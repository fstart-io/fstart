//! AST2500 address map shared by the build plan and the stages.

/// CE0 is mapped at the reset vector; the bootblock executes it there.
pub const FLASH_BASE: u64 = 0;
/// CE0's decode window, through which the stages read the FFS: address 0
/// is no valid buffer address.
pub const FLASH_ALIAS: u64 = 0x2000_0000;
/// What 3-byte SPI addressing reaches after reset.
pub const FIRMWARE_WINDOW_SIZE: u64 = 16 * 1024 * 1024;
/// DRAM starts here whatever its size.
pub const DRAM_BASE: u64 = 0x8000_0000;
/// The DRAM main stage: code, data, heap and stack. The bootblock refuses
/// DRAM configurations whose usable part ends below it.
pub const MAIN_STAGE_BASE: u64 = 0x8200_0000;
pub const MAIN_STAGE_SIZE: u64 = 0x0020_0000;
/// The bootblock's handoff to the main stage, just below it.
pub const HANDOFF_ADDR: u64 = 0x81FF_F000;
