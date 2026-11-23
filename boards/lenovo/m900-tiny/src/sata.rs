// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native mainboard/lenovo/m900_tiny/devicetree.cb AHCI policy.
//! Source SHA256: 4e865152d729b563a227b863c37fe55a4c991acdef5fc0251c6d60f1bb07d8a1.
//! Source enables
//! ports 0..4 and hot-plug 0/1; no DEVSLP, staggered-spin or RST remap request.
//! Physical connectors and lane26 operation have NOT been validated. The
//! silicon driver observes FIA ownership instead of changing it to fit this.
use fstart_driver_intel::sunrise_point::sata::{AhciPolicy, Policy, PortPolicy, SpeedLimit};

pub const POLICY: Policy = Policy::Ahci(AhciPolicy {
    ports: [
        PortPolicy {
            hotplug: true,
            ..PortPolicy::ENABLED
        },
        PortPolicy {
            hotplug: true,
            ..PortPolicy::ENABLED
        },
        PortPolicy::ENABLED,
        PortPolicy::ENABLED,
        PortPolicy::ENABLED,
        PortPolicy::DISABLED,
        PortPolicy::DISABLED,
        PortPolicy::DISABLED,
    ],
    speed: SpeedLimit::Gen3,
    salp: true,
});
