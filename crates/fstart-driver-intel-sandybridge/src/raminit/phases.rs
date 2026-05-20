//! High-level native raminit phase glue after JEDEC initialization.

use fstart_arch_x86::udelay;
use fstart_services::ServiceError;

use super::iosav;
use super::mchbar;
use super::RaminitState;

const TC_RAP_BASE: usize = 0x4004;

#[inline]
const fn cx(base: usize, channel: usize) -> usize {
    base + (channel << 10)
}

/// Prepare the memory controller for receive/read/write/command training.
pub fn prepare_training(mchbar_base: usize, state: &RaminitState) -> Result<(), ServiceError> {
    for channel in 0..2 {
        if state.topology.rankmap[channel] != 0 {
            // Always drive command bus during training.
            mchbar::setbits32(mchbar_base + cx(TC_RAP_BASE, channel), 1 << 29);
        }
    }

    udelay(1);

    for channel in 0..2 {
        if state.topology.rankmap[channel] != 0 {
            iosav::wait(mchbar_base, channel)?;
        }
    }

    Ok(())
}
