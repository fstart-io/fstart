// SPDX-License-Identifier: MIT OR Apache-2.0
//! ACPI tables of the SP7 SoCs: ACPI PM blocks at `pmbase`, direct ACPI mode
//! FADT, and the 24-bridge WH NBIO IOAPIC INTx routing table
//! ([`fstart_driver_amd::nbio::wh::ROUTES`]).
//!
//! Note: [`BRIDGES`] pairs the verified WH INTx routing table with the RS
//! native DevFn assignments as defaults; [`bridges_from_devfns`] builds the
//! table from live WH `IOHC_Bridge_DevFn` registers once read from silicon.

use crate::acpi::{S5_SLEEP_TYPE, SocAcpi, prt::Bridge};
use fstart_acpi::fadt::{Flags, PmProfile};
use fstart_acpi::platform::X86Fadt;
use fstart_driver_amd::dxio::rs::ports::NATIVE_DEVFNS;
use fstart_driver_amd::fch::acpimmio::AcpiPorts;
use fstart_driver_amd::nbio::wh::ROUTES;

/// Build the 24-bridge `_PRT` descriptor array from active `(device, function)`
/// pairs read from `IOHC::IOHC_Bridge_DevFn`, paired with the verified WH
/// IOAPIC INTx routing table (`SilVenicePei` `0x1430_0040..=0x1430_009C`).
pub const fn bridges_from_devfns(devfns: [(u8, u8); 24]) -> [Bridge; 24] {
    let mut bridges = [Bridge {
        device: 0,
        function: 0,
        route: ROUTES[0],
        internal: false,
    }; 24];
    let mut i = 0;
    while i < bridges.len() {
        let (device, function) = devfns[i];
        bridges[i] = Bridge {
            device,
            function,
            route: ROUTES[i],
            internal: device >= 7,
        };
        i += 1;
    }
    bridges
}

/// Default 24-bridge table using RS native DevFn values paired with verified WH
/// IOAPIC INTx routing entries.
pub const BRIDGES: [Bridge; 24] = {
    let mut devfns = [(0u8, 0u8); 24];
    let mut i = 0;
    while i < 24 {
        devfns[i] = (NATIVE_DEVFNS[i] >> 3, NATIVE_DEVFNS[i] & 7);
        i += 1;
    }
    bridges_from_devfns(devfns)
};

const FADT_FLAGS: u32 =
    Flags::Wbinvd as u32 | Flags::ProcC1 as u32 | Flags::SlpButton as u32 | Flags::TmrValExt as u32;

/// The ACPI tables of an SP7 SoC with its ACPI blocks at `pmbase`, and with
/// `legacy` LPC/eSPI devices (e.g. an AST2700 BMC Super I/O).
pub const fn soc(pmbase: u16, legacy: bool) -> SocAcpi {
    SocAcpi {
        ports: AcpiPorts {
            event: pmbase,
            control: pmbase + 4,
            timer: pmbase + 8,
            gpe: pmbase + 0x20,
        },
        fadt: X86Fadt {
            pm_profile: PmProfile::EnterpriseServer,
            iapc_boot_arch: if legacy {
                fstart_acpi::platform::x86::IAPC_LEGACY_DEVICES
            } else {
                0
            },
            flags: FADT_FLAGS,
            p_lvl2_lat: 101,
            p_lvl3_lat: 1001,
            duty_offset: 0,
            duty_width: 0,
            pm2_cnt_offset: None,
        },
        bridges: &BRIDGES,
        fch_intx_gsi: None,
        root_complex_pin: Some(1),
        wake: None,
        xhci: None,
        sleep_states: &[(0, 0), (5, S5_SLEEP_TYPE)],
        socket_distance: 12,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wh_bridges_match_native_devfns_and_verified_routes() {
        assert_eq!(BRIDGES.len(), 24);
        assert_eq!((BRIDGES[0].device, BRIDGES[0].function), (1, 1));
        assert_eq!(BRIDGES[0].route.group, 0);
        assert_eq!(BRIDGES[6].route.group, 5);
        assert_eq!(BRIDGES[6].route.swizzle, 2);
        assert_eq!(BRIDGES[23].route.map, 4);
        assert!(BRIDGES[22].internal && !BRIDGES[21].internal);

        let venice = soc(0x800, true);
        assert_eq!((venice.ports.event, venice.ports.gpe), (0x800, 0x820));
        assert_ne!(venice.fadt.iapc_boot_arch, 0);
    }
}
