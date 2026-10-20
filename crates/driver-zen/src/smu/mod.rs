//! SMU firmware interface: V9 (Naples) and V11 (Rome, Milan) mailboxes,
//! messages and policy tables (NBIO_SMU §3.8).
//!
//! [`SmuMessages`] is the contract other blocks (DXIO, CCX thread release,
//! hotplug) program against; the mailbox implementations live here:
//! [`mailbox::Mailbox`] with the [`v9::REGS`] (Naples, per die) or
//! [`v11::REGS`] (Rome/Milan, per socket) register set. [`v9`] and [`v11`]
//! hold the message IDs, feature masks, the CCX-done sequences and the
//! policy tables (V9 policy table, V11 PPTable).

pub mod mailbox;
#[cfg(any(feature = "rome", feature = "milan"))]
pub mod v11;
#[cfg(feature = "naples")]
pub mod v9;

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

/// Send `id` with `arg0` (other arguments zero); returns response arg 0.
pub fn send1<M: SmuMessages + ?Sized>(smu: &M, id: u32, arg0: u32) -> Result<u32, SmuError> {
    let mut args = [arg0, 0, 0, 0, 0, 0];
    smu.send(id, &mut args)?;
    Ok(args[0])
}

/// Send `id` with a 64-bit address as `(lo, hi)` in args 0/1 (V11 DRAM
/// address messages).
pub fn send_addr<M: SmuMessages + ?Sized>(smu: &M, id: u32, addr: u64) -> Result<(), SmuError> {
    let mut args = [addr as u32, (addr >> 32) as u32, 0, 0, 0, 0];
    smu.send(id, &mut args)
}
