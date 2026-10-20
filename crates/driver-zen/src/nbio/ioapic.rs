//! The NBIO IOAPIC: features, bridge interrupt routing, ID and the plain
//! data ACPI needs (NBIO_SMU §3.6, ACPI_SMM §3.2).
//!
//! The routing registers are programmed by the generated NBIO tables; the
//! same routes are exposed here as data for `_PRT` and checked against the
//! tables in the tests.

use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::Nbio;
use crate::smn::SmnAddr;
use crate::soc::Generation;

/// `IOAPIC::FEATURES_ENABLE`.
pub const FEATURES_ENABLE: SmnAddr = 0x1430_0000;
/// `IOAPIC_BR_INTERRUPT_ROUTING[i]`.
pub const fn br_interrupt_routing(i: usize) -> SmnAddr {
    0x1430_0040 + 4 * i as u32
}
/// SMN alias of the IOAPIC ID register (IOAPIC index 0).
pub const ID_ALIAS: SmnAddr = 0x0280_1000;

/// Redirection entries of an NBIO IOAPIC (GSI windows of 32, ACPI_SMM
/// §3.2).
pub const NBIO_IOAPIC_PINS: u32 = 32;
/// Redirection entries of the FCH IOAPIC (GSI 0..23).
pub const FCH_IOAPIC_PINS: u32 = 24;

register_bitfields![u32,
    pub FEATURES [
        ID_EXT OFFSET(2) NUMBITS(1) [],
        /// Legacy interrupts are forwarded to the FCH IOAPIC. ACPI `_PIC(1)`
        /// clears it on every NBIO.
        FCH OFFSET(4) NUMBITS(1) [],
        SECONDARY OFFSET(5) NUMBITS(1) []
    ],
    pub ROUTING [
        GROUP OFFSET(0) NUMBITS(3) [],
        SWIZZLE OFFSET(4) NUMBITS(2) [
            Abcd = 0,
            Bcda = 1,
            Cdab = 2,
            Dabc = 3
        ],
        MAP OFFSET(16) NUMBITS(5) []
    ],
    pub ID [
        ID OFFSET(24) NUMBITS(8) []
    ]
];

/// One bridge's legacy INTx route into the NBIO IOAPIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    /// Interrupt group.
    pub group: u8,
    /// INTA..D rotation (0 = ABCD, 1 = BCDA, 2 = CDAB, 3 = DABC).
    pub swizzle: u8,
    /// First IOAPIC pin of the group (pins `map..map+3`, INF from the
    /// field name BRIDGE_MAP).
    pub map: u8,
}

impl Route {
    /// The register value (mask 0x001F0037).
    pub const fn value(&self) -> u32 {
        self.group as u32 | (self.swizzle as u32) << 4 | (self.map as u32) << 16
    }
}

const fn route(i: usize, tail: &[Route]) -> Route {
    match i {
        0..8 => Route {
            group: i as u8,
            swizzle: 0,
            map: 0x10 + (i % 4) as u8,
        },
        8..16 => Route {
            group: (15 - i) as u8,
            swizzle: 2,
            map: 0x0C + (i % 4) as u8,
        },
        _ => tail[i - 16],
    }
}

const SP3_TAIL: [Route; 6] = {
    let mut t = [Route {
        group: 0,
        swizzle: 3,
        map: 0,
    }; 6];
    let mut i = 0;
    while i < 6 {
        t[i].group = i as u8;
        t[i].map = 0x08 + (i % 4) as u8;
        i += 1;
    }
    t
};

const ZP_TAIL: [Route; 2] = [
    Route {
        group: 3,
        swizzle: 1,
        map: 0x14,
    },
    Route {
        group: 4,
        swizzle: 3,
        map: 0x08,
    },
];

const fn routes<const N: usize>(tail: &[Route]) -> [Route; N] {
    let mut r = [Route {
        group: 0,
        swizzle: 0,
        map: 0,
    }; N];
    let mut i = 0;
    while i < N {
        r[i] = route(i, tail);
        i += 1;
    }
    r
}

/// Rome/Milan: 22 bridges (identical in ssp.nbio and gn.nbio; Oxide
/// `milan_ioapic_routes` agrees).
pub const ROUTES_SP3: [Route; 22] = routes(&SP3_TAIL);
/// Naples: 18 bridges (../sp4r2 NBIO_BASE_ZP §3.4).
pub const ROUTES_NAPLES: [Route; 18] = routes(&ZP_TAIL);

/// The bridge routes of `generation`.
pub const fn routes_for(generation: Generation) -> &'static [Route] {
    match generation {
        Generation::Naples => &ROUTES_NAPLES,
        Generation::Rome | Generation::Milan => &ROUTES_SP3,
    }
}

/// Default FCH IOAPIC ID (Naples PCD 0x191 = 0x80, Rome PCD 0x26B = 0xF0).
pub const fn fch_ioapic_id(generation: Generation) -> u8 {
    match generation {
        Generation::Naples => 0x80,
        Generation::Rome | Generation::Milan => 0xF0,
    }
}

/// Default ID of the `n`-th NBIO IOAPIC in enumeration order (Naples
/// 0x81 + n, Rome/Milan 0xF1 + n; ACPI_SMM `$APIDATA`). The Rome/Milan
/// AMI root-bridge order is not decoded (NBIO_SMU §7.1): the MADT GSI order
/// must match the `_PRT` windows, which on Rome/Milan run opposite to the
/// root-bridge `_UID`s.
pub const fn nbio_ioapic_id(generation: Generation, n: u8) -> u8 {
    fch_ioapic_id(generation) + 1 + n
}

/// What the MADT/IVRS need about one NBIO IOAPIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoapicInfo {
    pub socket: u8,
    pub index: u8,
    pub id: u8,
    pub base: u64,
    /// IVRS special-device source: `bus:00.1`.
    pub bus: u8,
}

/// Assign GSI bases the way the donor MADT does: FCH IOAPIC first at 0,
/// then the NBIO IOAPICs sorted by ID, each `NBIO_IOAPIC_PINS` wide
/// (ACPI_SMM §3.2). Returns `(info, gsi_base)` in that order.
pub fn gsi_layout<const N: usize>(ioapics: &[IoapicInfo]) -> heapless::Vec<(IoapicInfo, u32), N> {
    let mut sorted: heapless::Vec<IoapicInfo, N> = ioapics.iter().copied().collect();
    sorted.sort_unstable_by_key(|i| i.id);
    sorted
        .iter()
        .enumerate()
        .map(|(k, &i)| (i, FCH_IOAPIC_PINS + NBIO_IOAPIC_PINS * k as u32))
        .collect()
}

impl<P: ConfigRegionAccess + Clone> Nbio<P> {
    /// `FEATURES_ENABLE |= ID_EXT | FCH` on socket 0's FCH NBIO, plus
    /// SECONDARY everywhere else (NBIO_SMU §3.5; Naples: die 0 vs others).
    pub fn ioapic_features(&self) {
        let set = FEATURES::ID_EXT::SET + FEATURES::FCH::SET;
        let reg = self.reg::<FEATURES::Register>(self.at(FEATURES_ENABLE));
        // AGESA ORs 0x14 / 0x34: SECONDARY is left as is on the primary.
        if self.fch && self.socket == 0 {
            reg.modify(set);
        } else {
            reg.modify(set + FEATURES::SECONDARY::SET);
        }
    }

    /// Program the bridge routes directly (the NBIO tables already do this;
    /// for flows that skip the tables).
    pub fn ioapic_routes(&self) {
        routes_for(self.generation)
            .iter()
            .enumerate()
            .for_each(|(i, r)| {
                self.reg::<ROUTING::Register>(self.at(br_interrupt_routing(i)))
                    .modify(
                        ROUTING::GROUP.val(r.group as u32)
                            + ROUTING::SWIZZLE.val(r.swizzle as u32)
                            + ROUTING::MAP.val(r.map as u32),
                    )
            });
    }

    /// Set the IOAPIC ID through its SMN alias (AMI NbDxe / PciRootBridge).
    pub fn set_ioapic_id(&self, id: u8) {
        self.reg::<ID::Register>(self.at(ID_ALIAS))
            .write(ID::ID.val(id as u32));
    }

    /// The IOAPIC's current ID.
    pub fn ioapic_id(&self) -> u8 {
        self.reg::<ID::Register>(self.at(ID_ALIAS)).read(ID::ID) as u8
    }

    /// ACPI data for this NBIO's IOAPIC, if its BAR is enabled.
    pub fn ioapic_info(&self) -> Option<IoapicInfo> {
        Some(IoapicInfo {
            socket: self.socket,
            index: self.index,
            id: self.ioapic_id(),
            base: self.ioapic_bar()?,
            bus: self.bus(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nbio::tests::nbio;

    #[cfg(any(feature = "naples", feature = "rome", feature = "milan"))]
    fn route_values(entries: impl Iterator<Item = (SmnAddr, crate::nbio::table::Op)>) -> Vec<u32> {
        use crate::nbio::table::Op;
        entries
            .filter_map(|(a, op)| match op {
                Op::Rmw {
                    clear: 0x1F_0037,
                    set,
                } if (0x1430_0040..0x1430_00A0).contains(&a) => Some(set),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn routes_match_tables() {
        #[cfg(feature = "naples")]
        assert_eq!(
            route_values(
                crate::nbio::naples::tables::BASE
                    .iter()
                    .map(|e| (e.addr, e.op))
            ),
            ROUTES_NAPLES.map(|r| r.value())
        );
        #[cfg(feature = "rome")]
        assert_eq!(
            route_values(
                crate::nbio::sp3::tables::rome::NBIO
                    .iter()
                    .map(|e| (e.addr, e.op))
            ),
            ROUTES_SP3.map(|r| r.value())
        );
        #[cfg(feature = "milan")]
        assert_eq!(
            route_values(
                crate::nbio::sp3::tables::milan::NBIO
                    .iter()
                    .map(|e| (e.addr, e.op))
            ),
            ROUTES_SP3.map(|r| r.value())
        );
    }

    #[test]
    fn features_and_id() {
        let (m, n0) = nbio(Generation::Rome, 0, 0);
        n0.ioapic_features();
        let (_, n1) = nbio(Generation::Rome, 1, 0x20);
        n1.ioapic_features();
        n1.set_ioapic_id(0xF2);
        let s = m.0.borrow();
        assert_eq!(s.smn[&(0, 0x1430_0000)], 0x14);
        drop(s);
        assert_eq!(n1.ioapic_id(), 0xF2);
    }

    #[test]
    fn gsi_order_by_id() {
        let io = |id, index| IoapicInfo {
            socket: 0,
            index,
            id,
            base: 0,
            bus: 0,
        };
        let l = gsi_layout::<8>(&[io(0xF3, 0), io(0xF1, 3)]);
        assert_eq!((l[0].0.index, l[0].1), (3, 24));
        assert_eq!((l[1].0.index, l[1].1), (0, 56));
    }
}
