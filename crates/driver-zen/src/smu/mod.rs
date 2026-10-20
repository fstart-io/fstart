//! SMU firmware interface: V9 (Naples) and V11 (Rome, Milan) mailboxes,
//! messages and policy tables (NBIO_SMU §3.8).
//!
//! [`SmuMessages`] is the contract other blocks (DXIO, CCX thread release,
//! hotplug) program against; the mailbox implementations live here.

/// Number of argument/response dwords of an SMU message (V9 and V11).
pub const SMU_ARGS: usize = 6;

/// Why an SMU message failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmuError {
    /// The response register did not change within the timeout.
    Timeout,
    /// The firmware answered with a non-OK status (the raw response).
    Status(u32),
}

/// Sends SMU messages to one SMU instance (a ZP die on Naples, an IOD on
/// Rome and Milan).
pub trait SmuMessages {
    /// Send message `id` with `args`; on success `args` holds the response
    /// arguments.
    fn send(&self, id: u32, args: &mut [u32; SMU_ARGS]) -> Result<(), SmuError>;
}
