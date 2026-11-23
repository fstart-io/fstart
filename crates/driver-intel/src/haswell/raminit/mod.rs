//! Native DDR3/DDR3L controller initialization, following coreboot's
//! `northbridge/intel/haswell/native_raminit`.

// Training loops use a common lane index for the DDRIO address, register
// shadow and REUT feedback, as in the GM45/Ironlake implementations.
#![allow(clippy::needless_range_loop)]

pub mod activate;
mod flow;
mod info;
pub mod io_comp;
pub mod late_command;
pub mod memory_scrub;
pub mod spd;
pub use flow::*;
pub mod centering;
pub mod change_margin;
pub mod configure_mc;
pub mod ddr3;
pub mod memory_map;
pub mod ranges;
pub mod receive_enable;
pub mod reut;
pub mod save_restore;
pub mod sense_amp;
pub mod testing_io;
#[cfg(test)]
mod tests;
pub mod timings;
pub mod wdb;
pub mod write_leveling;

use super::MchBar;
use super::registers::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NoMemory,
    UnsupportedMemory,
    Spd(fstart_driver_spd::ddr3::Ddr3SpdError),
    Bus(fstart_core::services::ServiceError),
    Pll,
    /// MC_BIOS_REQ is still outstanding. Do not retry without a reset.
    PllBusy,
    SpdCrc,
    Timeout,
    Reut,
    SenseAmp,
    ReadMpr,
    ReceiveEnable,
    WriteLevel,
    Margin,
    LateCommand,
    Scrub,
    InvalidCache,
    ResumeRequiresReset,
    UnsupportedCpu,
    InvalidMap,
}

/// Address in the channel-local MCMAIN register bank.
const fn mc(offset: u32, channel: usize) -> u32 {
    offset + 0x400 * channel as u32
}

/// Wait for the initial compensation cycle (`RCOMP_TIMER`, `0x5084`, bit 16).
pub fn wait_for_first_rcomp(mch: MchBar) -> Result<(), Error> {
    wait_for_first_rcomp_timeout(mch, 2_000_000)
}

pub(crate) fn wait_for_first_rcomp_timeout(mch: MchBar, timeout_us: u32) -> Result<(), Error> {
    poll(|| mch.read32(0x5084) & (1 << 16) != 0, timeout_us)
}

pub fn io_reset(mch: MchBar) -> Result<(), Error> {
    io_reset_with(mch, |done| poll(done, 2_000_000))
}

pub(crate) fn io_reset_with(
    mch: MchBar,
    mut wait: impl FnMut(&mut dyn FnMut() -> bool) -> Result<(), Error>,
) -> Result<(), Error> {
    let reset = MC_INIT_STATE_G::RESET_IO::SET.value;
    mch.setbits32(0x5030, reset);
    wait(&mut || mch.read32(0x5030) & reset == 0)
}

fn poll(done: impl FnMut() -> bool, timeout_us: u32) -> Result<(), Error> {
    fstart_arch::x86::wait_us(timeout_us, done)
        .then_some(())
        .ok_or(Error::Timeout)
}
