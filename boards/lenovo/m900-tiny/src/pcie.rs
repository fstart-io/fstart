// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native M900 devicetree.cb root-port policy, not measured slot/link evidence.
//! SHA256 4e865152d729b563a227b863c37fe55a4c991acdef5fc0251c6d60f1bb07d8a1.
//! Source enables RP7 WLAN and RP17 storage. CLKREQ support remains the native
//! zero-policy value; clock-source inventory is NOT a guessed CLKREQ number.
use fstart_driver_intel::sunrise_point::pcie::{Config, PortPolicy};
pub static CONFIG: Config = {
    let mut config = Config::DEFAULT;
    config.ports[6] = PortPolicy {
        enabled: true,
        hotplug: true,
        aer: true,
        ltr: true,
        clock_source: 1,
        ..PortPolicy::DISABLED
    };
    config.ports[16] = PortPolicy {
        enabled: true,
        hotplug: true,
        aer: true,
        ltr: true,
        clock_source: 7,
        ..PortPolicy::DISABLED
    };
    config
};
