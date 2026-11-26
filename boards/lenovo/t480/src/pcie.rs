// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 Sunrise Point-LP PCIe root-port policy from
//! `mainboard/lenovo/sklkbl_thinkpad/variants/t480/overridetree.cb`:
//!
//! - Controller 1 (1x4): PCIE 1-4 -> RP1 (dGPU), CLKOUT0 / CLKREQ0
//! - Controller 2 (2x1+1x2, lane reversal):
//!   - PCIE 5 -> GbE (clobbers RP8), CLKOUT1 / CLKREQ1
//!   - PCIE 6 -> RP7 (M.2 WLAN x1), CLKOUT2 / CLKREQ2
//!   - PCIE 7-8 -> RP5 (M.2 WWAN x2), CLKOUT3 / CLKREQ3
//! - Controller 3 (2x2):
//!   - PCIE 9-10 -> RP9 (Thunderbolt 3 Alpine Ridge LP x2, hotplug), CLKOUT4 / CLKREQ4
//!   - PCIE 11-12 -> RP11 (M.2 2280 SSD caddy x2), CLKOUT5 / CLKREQ5

use fstart_driver_intel::sunrise_point::pcie::{Config, PortPolicy};

pub static CONFIG: Config = {
    let mut config = Config::DEFAULT;
    // RP1: optional discrete GPU (x4).
    config.ports[0] = PortPolicy {
        enabled: true,
        aer: true,
        ltr: true,
        clkreq: Some(0),
        clock_source: 0,
        ..PortPolicy::DISABLED
    };
    // RP5: M.2 WWAN slot (x2).
    config.ports[4] = PortPolicy {
        enabled: true,
        aer: true,
        ltr: true,
        clkreq: Some(3),
        clock_source: 3,
        ..PortPolicy::DISABLED
    };
    // RP7: M.2 WLAN slot (x1).
    config.ports[6] = PortPolicy {
        enabled: true,
        aer: true,
        ltr: true,
        clkreq: Some(2),
        clock_source: 2,
        ..PortPolicy::DISABLED
    };
    // RP9: Thunderbolt 3 (Alpine Ridge LP, x2, hotplug).
    config.ports[8] = PortPolicy {
        enabled: true,
        hotplug: true,
        aer: true,
        ltr: true,
        clkreq: Some(4),
        clock_source: 4,
        ..PortPolicy::DISABLED
    };
    // RP11: M.2 2280 NVMe SSD caddy (x2).
    config.ports[10] = PortPolicy {
        enabled: true,
        aer: true,
        ltr: true,
        clkreq: Some(5),
        clock_source: 5,
        ..PortPolicy::DISABLED
    };
    // GbE LAN uses CLKREQ1.
    config.lan_clkreq = Some(1);
    config
};
