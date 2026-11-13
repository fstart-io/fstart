// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCI interrupt routing of one NBIO: which IOAPIC pin a root-bus device or
//! a device behind a bridge reaches (ACPI_SMM §3.7, NBIO_SMU §3.6).
//!
//! The model is the hardware routing itself, `IOAPIC_BR_INTERRUPT_ROUTING`
//! as the NBIO base tables program it:
//!
//! * bridge `i`'s own INTx (function `f` uses INT\[(f − 1) mod 4\]) goes to
//!   pin `map`;
//! * INTx of the device behind it goes to pin `4·group + (x − swizzle) mod
//!   4`.
//!
//! Against the SP3 donor DSDTs this reproduces every socket-0 `_PRT` entry
//! of the S8026 (Naples) and S8030 (Rome, Milan) except PCI0 8.2 on
//! Rome/Milan (GSI 128 in the ASL, pin 17 here), and the socket-1 entries
//! apart from three CRB slips (S1D1 `AR06` GSI 40, S1D3 dev 1/3 swapped,
//! Naples S1D0 7.1): those are generic CRB ASL never run on these 1P
//! boards, while the routing registers are identical on every NBIO.

extern crate alloc;

use alloc::vec::Vec;

use fstart_driver_amd::nbio::ioapic::{ROUTES_RV, ROUTES_SP3, ROUTES_ZP, Route};

/// One bridge of a root bus and its route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bridge {
    pub device: u8,
    pub function: u8,
    pub route: Route,
}

impl Bridge {
    /// The bridge's own interrupt pin (0 = INTA).
    pub const fn intx(&self) -> u8 {
        (self.function + 3) % 4
    }

    /// IOAPIC pin of INTx `intx` of a device behind the bridge.
    pub const fn secondary_pin(&self, intx: u8) -> u8 {
        4 * self.route.group + (intx + 4 - self.route.swizzle % 4) % 4
    }
}

/// The bridges `at` (`device, function`) with the routes of their routing
/// registers.
const fn bridges<const N: usize>(at: [(u8, u8); N], routes: [Route; N]) -> [Bridge; N] {
    let mut bridges = [Bridge {
        device: 0,
        function: 0,
        route: routes[0],
    }; N];
    let mut i = 0;
    while i < N {
        bridges[i] = Bridge {
            device: at[i].0,
            function: at[i].1,
            route: routes[i],
        };
        i += 1;
    }
    bridges
}

/// Raven/Picasso: the PCIe ports and the internal bridges 8.1 and 8.2.
pub const BRIDGES_RV: [Bridge; 9] = bridges(
    [
        (1, 1),
        (1, 2),
        (1, 3),
        (1, 4),
        (1, 5),
        (1, 6),
        (1, 7),
        (8, 1),
        (8, 2),
    ],
    ROUTES_RV,
);

/// The ZP die (Zeppelin, Naples): two PCIe cores and the internal bridges
/// 7.1 (xHCI) and 8.1 (SATA, xGbE).
pub const BRIDGES_ZP: [Bridge; 18] = bridges(
    [
        (1, 1),
        (1, 2),
        (1, 3),
        (1, 4),
        (1, 5),
        (1, 6),
        (1, 7),
        (2, 1),
        (3, 1),
        (3, 2),
        (3, 3),
        (3, 4),
        (3, 5),
        (3, 6),
        (3, 7),
        (4, 1),
        (7, 1),
        (8, 1),
    ],
    ROUTES_ZP,
);

/// A Rome/Milan NBIO.
pub const BRIDGES_SP3: [Bridge; 22] = bridges(
    [
        (1, 1),
        (1, 2),
        (1, 3),
        (1, 4),
        (1, 5),
        (1, 6),
        (1, 7),
        (2, 1),
        (3, 1),
        (3, 2),
        (3, 3),
        (3, 4),
        (3, 5),
        (3, 6),
        (3, 7),
        (4, 1),
        (5, 1),
        (5, 2),
        (7, 1),
        (8, 1),
        (8, 2),
        (8, 3),
    ],
    ROUTES_SP3,
);

/// Root-bus `_PRT` entries `(device, intx, ioapic pin)`, one per device and
/// pin (functions sharing a pin share the route).
pub fn root_entries(bridges: &[Bridge]) -> Vec<(u8, u8, u8)> {
    bridges.iter().fold(Vec::new(), |mut v, b| {
        let e = (b.device, b.intx(), b.route.map);
        if !v.iter().any(|&(d, p, _)| (d, p) == (e.0, e.1)) {
            v.push(e);
        }
        v
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn root_gsis(bridges: &[Bridge], base: u32) -> BTreeSet<(u8, u8, u32)> {
        root_entries(bridges)
            .into_iter()
            .map(|(d, p, pin)| (d, p, base + u32::from(pin)))
            .collect()
    }

    fn behind(bridges: &[Bridge], base: u32, device: u8, function: u8) -> Vec<u32> {
        let b = bridges
            .iter()
            .find(|b| (b.device, b.function) == (device, function))
            .unwrap();
        (0..4)
            .map(|x| base + u32::from(b.secondary_pin(x)))
            .collect()
    }

    /// Rome/Milan S8030 DSDT `\_SB.S0D0` (GSI 24–55): `AR03`, `G003` (3.1),
    /// `G015` (7.1), `G019` (8.1), `G01F` (8.2).
    #[test]
    fn sp3_matches_the_donor_s0d0() {
        let ar03: BTreeSet<_> = [
            (3, 0, 36),
            (3, 1, 37),
            (3, 2, 38),
            (3, 3, 39),
            (4, 0, 39),
            (1, 0, 40),
            (1, 1, 41),
            (1, 2, 42),
            (1, 3, 43),
            (2, 0, 43),
            (5, 0, 32),
            (5, 1, 33),
            (7, 0, 34),
            (8, 0, 35),
            (8, 1, 32),
            (8, 2, 33),
        ]
        .into_iter()
        .collect();
        let b = &BRIDGES_SP3;
        assert_eq!(root_gsis(b, 24), ar03);
        assert_eq!(behind(b, 24, 3, 1), [54, 55, 52, 53]);
        assert_eq!(behind(b, 24, 7, 1)[..2], [33, 34]);
        assert_eq!(behind(b, 24, 8, 1), [37, 38, 39, 36]);
        assert_eq!(behind(b, 24, 8, 2)[0], 41);
        assert_eq!(behind(b, 24, 1, 2)[0], 28);
    }

    /// Naples S8026 DSDT `\_SB.PCI0` (GSI 24–55): `AR00`, `G003` (1.1),
    /// `G00B` (3.1), `G013` (7.1), `G017` (8.1).
    #[test]
    fn zp_matches_the_naples_donor_pci0() {
        let ar00: BTreeSet<_> = [
            (1, 0, 40),
            (1, 1, 41),
            (1, 2, 42),
            (1, 3, 43),
            (2, 0, 43),
            (3, 0, 36),
            (3, 1, 37),
            (3, 2, 38),
            (3, 3, 39),
            (4, 0, 39),
            (7, 0, 44),
            (8, 0, 32),
        ]
        .into_iter()
        .collect();
        let b = &BRIDGES_ZP;
        assert_eq!(root_gsis(b, 24), ar00);
        assert_eq!(behind(b, 24, 1, 1), [24, 25, 26, 27]);
        assert_eq!(behind(b, 24, 3, 1), [54, 55, 52, 53]);
        assert_eq!(behind(b, 24, 7, 1)[..3], [39, 36, 37]);
        assert_eq!(behind(b, 24, 8, 1)[..3], [41, 42, 43]);
    }

    /// Picasso's internal bridges: 8.1 on group 7 as the last PCIe port's
    /// register has it, 8.2 on the next register's.
    #[test]
    fn rv_internal_bridges() {
        let b = &BRIDGES_RV;
        assert_eq!(behind(b, 24, 8, 1), [52, 53, 54, 55]);
        assert_eq!(behind(b, 24, 8, 2), [54, 55, 52, 53]);
        assert!(root_gsis(b, 24).is_superset(&[(8, 0, 43), (8, 1, 36)].into()));
    }
}
