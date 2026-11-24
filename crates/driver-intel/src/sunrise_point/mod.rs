// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sunrise Point PCH register interfaces. No ICH9 compatibility is implied.
// Ported from per-channel/rank/lane C loops over hardware register arrays,
// with the source's argument lists.
#![allow(dead_code, clippy::needless_range_loop, clippy::too_many_arguments)]
pub mod auxiliary;
pub mod dmi;
pub mod driver;
pub mod interconnect;
pub mod ish;
pub mod serialio;
pub mod smbus;
pub use driver::IntelSunrisePoint;
pub mod early;
pub mod espi;
pub mod finalize;
pub mod gpio;
pub mod gpio_config;
pub mod hda;
pub mod hsio;
pub mod irq;
pub mod lan;
pub mod lpc;
pub mod me;
pub mod pcie;
pub mod pcr;
pub mod pmc;
pub mod pmio;
pub mod power;
pub mod sata;
pub mod smi;
pub mod spi;
pub mod tco;
pub mod thermal;
pub mod usb;
