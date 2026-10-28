// SPDX-License-Identifier: MIT OR Apache-2.0
//! The FCH DesignWare I2C controllers at the SoC's [`Soc::I2C`] bases,
//! numbered like the vendor AmdI2CMasterPei instances.
use crate::soc::Soc;
use embedded_hal::delay::DelayNs;
use fstart_driver_i2c::designware::{DesignWare, Error};
pub use fstart_driver_i2c::designware::{Speed, Timing};

/// Controller `index` of SoC `S` with `timing`. Its pads must already be
/// routed.
///
/// # Safety
/// Nothing else uses the controller while the driver exists.
pub unsafe fn controller<S: Soc, D: DelayNs>(
    index: usize,
    timing: &Timing,
    delay: D,
) -> Result<DesignWare<D>, Error> {
    let base = *S::I2C.get(index).ok_or(Error::NotPresent)?;
    // SAFETY: an FCH MMIO block below the ACPI MMIO, decoded since reset;
    // exclusivity is the caller's.
    unsafe { DesignWare::new(base, timing, delay) }
}
