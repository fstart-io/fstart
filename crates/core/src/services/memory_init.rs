//! Memory-init block.

use super::ServiceError;

/// Bring DRAM up.
///
/// One block for every memory controller; what it needs and reports
/// differ:
///
/// - Controllers whose configuration comes from the board (timings, ODT,
///   bus width) take no input (`()`): they program the controller, train
///   it, size the memory and report the DRAM [`Range`](crate::description::Range).
/// - DIMM chipsets take the bus their SPDs are on and their training cache
///   (`fstart_driver_intel::SpdTraining`), and report what they captured.
///
/// Memory that came up before fstart ran (PSP, a vendor ROM, QEMU) needs
/// no block.
pub trait MemoryInit {
    /// What bring-up needs beyond the block's own configuration.
    type Input<'a>;
    /// What a successful bring-up reports.
    type Output;

    /// Train or find the memory.
    fn bring_up(&mut self, input: Self::Input<'_>) -> Result<Self::Output, ServiceError>;
}
