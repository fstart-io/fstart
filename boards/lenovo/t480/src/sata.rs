// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 AHCI SATA policy from `sklkbl_thinkpad/devicetree.cb`:
//! only SATA port 2 is wired (2.5" SATA bay / M.2 caddy) with DevSlp enabled.

use fstart_driver_intel::sunrise_point::sata::{AhciPolicy, Policy, PortPolicy, SpeedLimit};

pub const POLICY: Policy = Policy::Ahci(AhciPolicy {
    ports: [
        PortPolicy::DISABLED,
        PortPolicy::DISABLED,
        PortPolicy {
            devslp: true,
            ..PortPolicy::ENABLED
        },
        PortPolicy::DISABLED,
        PortPolicy::DISABLED,
        PortPolicy::DISABLED,
        PortPolicy::DISABLED,
        PortPolicy::DISABLED,
    ],
    speed: SpeedLimit::Gen3,
    salp: true,
});
