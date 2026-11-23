// SPDX-License-Identifier: MIT OR Apache-2.0
//! M900 NCT6687D configuration matching the native coreboot board.
//!
//! m900_tiny/devicetree.cb supplies no HWM assignments; its generated
//! `superio_nuvoton_nct6687d_config` is `{}`. The driver still enables
//! monitoring, writes zero sensor selectors and locks sensor configuration.
//! FAN_IGNORE=0 skips each fan engine: this does NOT install a fan curve,
//! prove inherited cooling safe, or mean the EC receives no writes.
use fstart_driver_superio::nct6687d::hwm::Config;

/// Source devicetree TACH/PWM logical-device configuration, not EC HWM writes.
pub const TACH_PWM_ASSIGNMENTS: [(u8, u8); 2] = [(0xe4, 0x10), (0xe5, 0x09)];
/// Exact source empty chip_info semantics, not an approved/calibrated curve.
pub const CONFIG: Config = Config::SOURCE_ZERO;
