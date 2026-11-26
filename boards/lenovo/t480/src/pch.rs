// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 PCH thermal, GbE, auxiliary, and interconnect policy from
//! coreboot `mainboard/lenovo/sklkbl_thinkpad/{devicetree.cb,variants/t480/overridetree.cb}`.

use fstart_driver_intel::sunrise_point::{auxiliary, interconnect, lan, thermal};

/// Legacy I/O low-latency remains disabled; 8254 PIT gating is selected by the
/// caller's payload plan.
pub const fn interconnect_policy(use_legacy_8254: bool) -> interconnect::Policy {
    interconnect::Policy {
        legacy_io_low_latency: false,
        use_legacy_8254,
    }
}

/// SerialIO controllers and ISH remain disabled in T480 devicetree/overridetree.
pub static AUXILIARY: auxiliary::Config = auxiliary::Config::DISABLED;

pub const THERMAL: thermal::Config = thermal::Config::NATIVE;

/// Integrated Intel GbE on PCIE lane 5 (clobbers RP8), CLKREQ1, LTR and K1-Off
/// enabled per T480 `overridetree.cb`.
pub const LAN: lan::Config = lan::Config {
    enabled: true,
    ltr: true,
    k1_off: true,
    flash_bytes: 16 * 1024 * 1024,
};
