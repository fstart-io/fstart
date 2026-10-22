// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the Zeppelin ACPI tables differ in.
use crate::acpi::{ABCD, BCDA, CDAB, DABC, Route, SocAcpi};
use fstart_acpi::{
    fadt::{Flags, PmProfile},
    platform::{X86Fadt, x86::IAPC_LEGACY_DEVICES},
};
use fstart_driver_amd::fch::taishan;

pub const ACPI: SocAcpi = SocAcpi {
    ports: taishan::ACPI_PORTS,
    fadt: X86Fadt {
        // The default SMBIOS enclosure is a desktop.
        pm_profile: PmProfile::Desktop,
        iapc_boot_arch: IAPC_LEGACY_DEVICES,
        flags: Flags::Wbinvd as u32
            | Flags::ProcC1 as u32
            | Flags::SlpButton as u32
            | Flags::RtcS4 as u32
            | Flags::TmrValExt as u32
            | Flags::PciExpWak as u32
            | Flags::UsePlatformClock as u32
            | Flags::S4RtcStsValid as u32
            | Flags::RemotePowerOnCapable as u32,
        // C2/C3 not supported here, _CST would describe them.
        p_lvl2_lat: 101,
        p_lvl3_lat: 1001,
        duty_offset: 0,
    },
    bridge_routes: &ROUTES,
    fch_intx_gsi: [16, 17, 18, 19],
};

const fn route(device: u8, function: u8, group: u8, swizzle: u8, bridge_irq: u8) -> Route {
    Route::new(device, function, group, swizzle, bridge_irq)
}

/// In bridge order: what the NBIO base table programs into
/// SMN 0x14300040 + 4 * bridge (`irq << 16 | swizzle << 4 | group`; the
/// hardware swizzle k is the ACPI swizzle (4 - k) % 4).
const ROUTES: [Route; 18] = [
    route(1, 1, 0, ABCD, 0x10),
    route(1, 2, 1, ABCD, 0x11),
    route(1, 3, 2, ABCD, 0x12),
    route(1, 4, 3, ABCD, 0x13),
    route(1, 5, 4, ABCD, 0x10),
    route(1, 6, 5, ABCD, 0x11),
    route(1, 7, 6, ABCD, 0x12),
    route(2, 1, 7, ABCD, 0x13),
    route(3, 1, 7, CDAB, 0x0c),
    route(3, 2, 6, CDAB, 0x0d),
    route(3, 3, 5, CDAB, 0x0e),
    route(3, 4, 4, CDAB, 0x0f),
    route(3, 5, 3, CDAB, 0x0c),
    route(3, 6, 2, CDAB, 0x0d),
    route(3, 7, 1, CDAB, 0x0e),
    route(4, 1, 0, CDAB, 0x0f),
    // Internal bridges A (xHCI) and B (SATA, xGbE)
    route(7, 1, 3, DABC, 0x14),
    route(8, 1, 4, BCDA, 0x08),
];

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::{nbio::zeppelin::BASE, table::Op};

    /// The ACPI routing is the one the NBIO base table programs.
    #[test]
    fn routes_match_the_nbio_ioapic_bridge_table() {
        for (bridge, route) in ROUTES.iter().enumerate() {
            let address = 0x1430_0040 + 4 * bridge as u32;
            let set = BASE
                .iter()
                .find_map(|reg| match reg.op {
                    Op::Update { set, .. } if reg.reg.address() == address => Some(set),
                    _ => None,
                })
                .unwrap();
            assert_eq!(u32::from(route.group), set & 7, "bridge {bridge}");
            assert_eq!(
                u32::from(route.swizzle),
                (4 - ((set >> 4) & 3)) % 4,
                "bridge {bridge}"
            );
            assert_eq!(
                u32::from(route.bridge_irq),
                (set >> 16) & 0x1f,
                "bridge {bridge}"
            );
        }
    }
}
