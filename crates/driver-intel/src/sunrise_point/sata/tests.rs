// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[test]
fn final_map_keeps_hotplug_but_requires_presence_for_other_configured_ports() {
    let mut policy = AhciPolicy {
        ports: [PortPolicy::DISABLED; 8],
        speed: SpeedLimit::Gen3,
        salp: true,
    };
    policy.ports[0] = PortPolicy {
        hotplug: true,
        ..PortPolicy::ENABLED
    };
    policy.ports[1] = PortPolicy::ENABLED;
    policy.ports[4] = PortPolicy::ENABLED;
    // Unconfigured port7 presence must not become an enabled port. Empty
    // port0 remains exposed for hot-plug; empty non-hotplug port1 is removed.
    assert_eq!(
        policy.final_ports((1 << 4) | (1 << 7)),
        Ok((1 << 0) | (1 << 4))
    );
    assert_eq!(policy.final_ports(0), Ok(1));
    assert_eq!(
        policy.final_ports((1 << 1) | (1 << 4)),
        Ok((1 << 0) | (1 << 1) | (1 << 4))
    );
}
