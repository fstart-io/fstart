//! Generated register fields and the initialization sequence's preservation masks.
//!
//! Register descriptions live in `data/registers/intel/i945.yaml`. These
//! masks describe whole-word initialization policy, not additional fields.

pub use super::registers::*;

/// DCC command mask: bits the controller requires cleared around COMMAND.
pub const DCC_CMD_MASK: u32 = (3 << 21) | (1 << 20) | (1 << 19) | (7 << 16);
/// WCC reset-state base preserved by pre-JEDEC init.
pub const WCC_BASE_MASK: u32 = 0x113f_f3ff;
/// SBOCC reset-state base preserved by post-JEDEC init.
pub const SBOCC_BASE_MASK: u32 = 0xffbd_b6ff;
