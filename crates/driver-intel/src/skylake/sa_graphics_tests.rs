// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::super::{
    sa_dmi::tests::{cpu, policy as dmi_policy},
    sa_peg::tests::Rig,
};
use super::*;

fn policy() -> Policy {
    Policy {
        igd_enabled: true,
        primary: PrimaryDisplay::Auto,
        stolen: 2,
        aperture: 1,
        skip_external_scan: false,
    }
}
fn igd(rig: &Rig) {
    let d = rig.dev(0, 2, 0);
    d.write32(0, 0x1912_8086);
    d.write32(0x60, 0xa5e0_1234);
    rig.resources().sa().unwrap().write32(0x54, 0xa5ff_ffff);
}
fn scan_root(rig: &Rig) {
    rig.port(0, 16);
    rig.dev(0, 1, 0).write16(0x11a, 0);
    rig.resources().sa().unwrap().write32(0x54, 8);
    rig.endpoint(3, 16);
}

#[test]
fn peg_bridge_has_vga_actual_bounded_downstream_scan_restores_every_bus() {
    for integrated in [false, true] {
        let rig = Rig::with_buses(false, 4);
        scan_root(&rig);
        igd(&rig);
        rig.resources().sa().unwrap().write32(0x54, 8);
        rig.dev(1, 0, 0).write16(0xa, 0x0604);
        rig.dev(1, 0, 0).write32(0x18, 0xa500_0000);
        let bridge = rig.dev(2, 0, 0);
        bridge.write32(0, 0x1234_8086);
        bridge.write16(0xa, 0x0604);
        bridge.write32(0x18, 0x5a00_0000);
        let vga = rig.dev(3, 7, 0);
        vga.write32(0, 0x7890_abcd);
        vga.write16(0xa, 0x0300);
        let mut p = policy();
        if integrated {
            p.primary = PrimaryDisplay::Integrated;
        }
        let out = rig
            .resources()
            .scan_vga(
                &rig.dmibar.dmi(),
                cpu(),
                MemoryBootMode::Cold,
                dmi_policy(),
                p,
                &mut ScanWorkspace::default(),
            )
            .unwrap();
        assert_eq!(out.is_some(), !integrated);
        assert_eq!(rig.dev(0, 1, 0).read32(0x18), 0);
        assert_eq!(rig.dev(1, 0, 0).read32(0x18), 0xa500_0000);
        assert_eq!(bridge.read32(0x18), 0x5a00_0000);
    }
}

#[test]
fn peg_vga_bus_exhaustion_and_downstream_ownership_restore_partial_topology() {
    for capacity in [true, false] {
        let rig = Rig::new(false);
        scan_root(&rig);
        rig.dev(1, 0, 0).write16(0xa, 0x0604);
        rig.dev(1, 0, 0)
            .write32(0x18, if capacity { 0 } else { 0x20200 });
        let before = rig.dev(1, 0, 0).read32(0x18);
        let result = rig.resources().scan_vga(
            &rig.dmibar.dmi(),
            cpu(),
            MemoryBootMode::Cold,
            dmi_policy(),
            policy(),
            &mut ScanWorkspace::default(),
        );
        assert!(matches!(result, Err(Error::Capacity | Error::BusInUse)));
        assert_eq!(rig.dev(0, 1, 0).read32(0x18), 0);
        assert_eq!(rig.dev(1, 0, 0).read32(0x18), before);
    }
}
