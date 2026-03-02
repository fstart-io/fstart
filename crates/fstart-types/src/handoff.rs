//! Inter-stage handoff data.
//!
//! When a multi-stage firmware boots (e.g., bootblock → main), the
//! outgoing stage serializes a `StageHandoff` struct to a DRAM buffer
//! and passes its address in `r0` (ARMv7) when jumping to the next
//! stage. The incoming stage deserializes it and uses the data for
//! runtime-discovered parameters (DRAM size, etc.).
//!
//! The handoff is serialized with `postcard` (compact, no_std, serde).
//! The same struct definition is shared by both stages (same build),
//! so self-describing format is unnecessary. A magic + version header
//! provides validation and forward-compatibility.

use serde::{Deserialize, Serialize};

/// Magic number for StageHandoff validation.
///
/// ASCII "FSTH" (fstart handoff). If the incoming stage reads a buffer
/// whose first 4 bytes don't match this, there is no valid handoff
/// (e.g., first stage loaded by BROM, or jump from non-fstart code).
pub const HANDOFF_MAGIC: u32 = 0x4653_5448;

/// Current handoff struct version.
///
/// Increment when fields are added or the layout changes. The incoming
/// stage should reject versions it doesn't understand.
pub const HANDOFF_VERSION: u16 = 1;

/// Maximum serialized size of a StageHandoff.
///
/// Postcard encodes the current struct in ~20 bytes. 256 bytes provides
/// generous headroom for future fields without risking stack overflow.
pub const HANDOFF_MAX_SIZE: usize = 256;

/// Inter-stage handoff data.
///
/// Carries runtime-discovered parameters from one stage to the next.
/// Device init state is determined at compile time by the codegen
/// (which can see all stages' capabilities in the board RON), so it
/// is NOT included here — only truly dynamic data belongs in the handoff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageHandoff {
    /// Magic number — must be [`HANDOFF_MAGIC`].
    pub magic: u32,
    /// Struct version — must be [`HANDOFF_VERSION`].
    pub version: u16,
    /// DRAM size in bytes, discovered by DRAM training.
    ///
    /// 0 means DRAM size was not determined (e.g., QEMU, or DRAM init
    /// was not performed by the previous stage).
    pub dram_size: u64,
}

impl StageHandoff {
    /// Create a new handoff with the given DRAM size.
    pub fn new(dram_size: u64) -> Self {
        Self {
            magic: HANDOFF_MAGIC,
            version: HANDOFF_VERSION,
            dram_size,
        }
    }

    /// Validate the magic and version fields.
    pub fn is_valid(&self) -> bool {
        self.magic == HANDOFF_MAGIC && self.version == HANDOFF_VERSION
    }
}
