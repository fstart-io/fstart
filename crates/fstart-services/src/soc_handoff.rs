//! SoC handoff service trait for heterogeneous processor release.

/// Hand off control to a secondary processor subsystem and halt.
///
/// Implementations perform all steps needed to release the secondary
/// processor: loading its firmware image, writing its reset vector to
/// hardware registers, enabling its subsystem, then parking the
/// implementing processor in a WFI (wait-for-interrupt) idle loop.
///
/// The method is diverging (`-> !`) by contract — the implementing
/// processor must not return to the firmware after calling `handoff()`.
/// Codegen relies on this: no `halt()` is emitted after the call site.
///
/// # Example use case
///
/// On the Sophgo SG2042 (Milk-V Pioneer), the ARM Cortex-A53 SCP
/// implements this trait. `handoff()` loads the RISC-V ZSBL from SPI
/// flash, writes the reset vector to TOP registers, enables all 16
/// RISC-V C920 clusters, then parks the A53 in a WFI loop.
pub trait SocHandoff {
    /// Release the secondary processor subsystem and halt this core.
    ///
    /// This method must not return.
    fn handoff(&mut self) -> !;
}
