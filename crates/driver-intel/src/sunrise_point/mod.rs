// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sunrise Point PCH register interfaces. No ICH9 compatibility is implied.
// Ported from per-channel/rank/lane C loops over hardware register arrays,
// with the source's argument lists.
#![allow(dead_code, clippy::needless_range_loop, clippy::too_many_arguments)]
pub mod dmi;
pub mod gpio;
pub mod pcr;
pub mod pmc;
