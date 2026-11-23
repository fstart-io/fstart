// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native M900 PCI thermal/GbE policy, not captured board thermal calibration.
//! devicetree.cb SHA256 4e865152d729b563a227b863c37fe55a4c991acdef5fc0251c6d60f1bb07d8a1.
//! The source enables both devices and leaves optional LAN LTR/K1Off and
//! memory throttling at zero. The 50-C PCH LTT default comes from the common
//! thermal code; it does not authorize programming the NCT6687D fan policy.
use fstart_driver_intel::sunrise_point::{auxiliary, interconnect, lan, serialio, thermal};
/// Source leaves PchLegacyIoLowLatency at zero. PIT gating is selected by
/// the actual payload/option-ROM plan, not guessed from the board's PC/AT bit.
pub const fn interconnect_policy(use_legacy_8254: bool) -> interconnect::Policy {
    interconnect::Policy {
        legacy_io_low_latency: false,
        use_legacy_8254,
    }
}
/// Native SerialIoDevMode: only UART2 is SkipInit; other controllers and ISH
/// remain disabled. Never translate inherited console SkipInit into PCI init.
pub static AUXILIARY: auxiliary::Config = {
    let mut config = auxiliary::Config::DISABLED;
    config.serialio.controllers[10].mode = serialio::Mode::SkipInit;
    config
};
pub const THERMAL: thermal::Config = thermal::Config::NATIVE;
pub const LAN: lan::Config = lan::Config {
    enabled: true,
    ltr: false,
    k1_off: false,
    flash_bytes: 16 * 1024 * 1024,
};
