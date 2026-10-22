// SPDX-License-Identifier: MIT OR Apache-2.0
//! The FCH DesignWare I2C controllers, numbered like the vendor
//! AmdI2CMasterPei instances.
use embedded_hal::delay::DelayNs;
use fstart_driver_i2c::designware::{DesignWare, Error, Speed, Timing};

pub const CONTROLLERS: [usize; 6] = [
    0xfedc_2000,
    0xfedc_3000,
    0xfedc_4000,
    0xfedc_5000,
    0xfedc_6000,
    0xfedc_b000,
];

/// The vendor code doesn't power the controllers up through AOAC or touch
/// the pads. It programs standard speed, these SCL counts (0x285 + 0x357
/// cycles per bit at 100 kHz: a 150 MHz controller clock) and IC_FS_SPKLEN 7.
pub const TIMING: Timing = Timing {
    speed: Speed::Standard,
    scl_high: 0x285,
    scl_low: 0x357,
    spike_length: Some(7),
};

/// Controller `index` with the vendor timing. Its pads must already be routed.
///
/// # Safety
/// Nothing else uses the controller while the driver exists.
pub unsafe fn controller<D: DelayNs>(index: usize, delay: D) -> Result<DesignWare<D>, Error> {
    let base = *CONTROLLERS.get(index).ok_or(Error::NotPresent)?;
    // SAFETY: an FCH MMIO block below the ACPI MMIO, decoded since reset;
    // exclusivity is the caller's.
    unsafe { DesignWare::new(base, &TIMING, delay) }
}
