// SPDX-License-Identifier: MIT OR Apache-2.0
//! The NBIO IOAPIC of a die or IOD quadrant, in SMN. Its MMIO BAR is the
//! IOHC's ([`super::iohc::IOAPIC_BAR`]).
use crate::smn::SmnReg;
use tock_registers::register_bitfields;

register_bitfields![u32,
    /// The vendor code sets 0x14 on the IOAPIC with the FCH and 0x34 on
    /// the others.
    pub IoapicFeatures [
        ID_EXT OFFSET(2) NUMBITS(1) [],
        /// Legacy interrupts are forwarded to the FCH IOAPIC. ACPI `_PIC(1)`
        /// clears it on every NBIO.
        FCH OFFSET(4) NUMBITS(1) [],
        SECONDARY OFFSET(5) NUMBITS(1) []
    ],
    /// How a bridge's INTx reach the IOAPIC.
    pub BridgeRouting [
        GROUP OFFSET(0) NUMBITS(3) [],
        /// INTA..D rotation.
        SWIZZLE OFFSET(4) NUMBITS(2) [
            Abcd = 0,
            Bcda = 1,
            Cdab = 2,
            Dabc = 3
        ],
        /// First IOAPIC pin of the group.
        MAP OFFSET(16) NUMBITS(5) []
    ],
    pub IoapicId [ID OFFSET(24) NUMBITS(8) []]
];

pub const IOAPIC_FEATURES_ENABLE: SmnReg<IoapicFeatures::Register> = SmnReg::new(0x1430_0000);
/// IOAPIC_BR_INTERRUPT_ROUTING of bridge `i`.
pub const fn br_interrupt_routing(i: usize) -> SmnReg<BridgeRouting::Register> {
    SmnReg::new(0x1430_0040 + 4 * i as u32)
}
/// The SMN alias of the IOAPIC's ID register (IOAPIC index 0).
pub const IOAPIC_ID: SmnReg<IoapicId::Register> = SmnReg::new(0x0280_1000);

/// Redirection entries of an NBIO IOAPIC: its GSI window.
pub const NBIO_IOAPIC_PINS: u32 = 32;
/// Redirection entries of the FCH IOAPIC (GSI 0..23).
pub const FCH_IOAPIC_PINS: u32 = 24;

/// One bridge's legacy INTx route into the NBIO IOAPIC: what its
/// [`br_interrupt_routing`] register holds.
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

/// Every NBIO routes its first sixteen bridges alike; the internal
/// bridges after them are `tail`.
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

/// Raven/Picasso: 9 bridges.
pub const ROUTES_RV: [Route; 9] = routes(&[]);
/// The ZP die, Zeppelin's and Naples': 18 bridges (../sp4r2 NBIO_BASE_ZP
/// §3.4).
pub const ROUTES_ZP: [Route; 18] = routes(&ZP_TAIL);
/// Rome/Milan: 22 bridges (identical in ssp.nbio and gn.nbio; Oxide
/// `milan_ioapic_routes` agrees).
pub const ROUTES_SP3: [Route; 22] = routes(&SP3_TAIL);

/// The OS's switch to APIC mode in ACPI (`_PIC(1)`): AGESA's `NAPE` clears
/// [`IOAPIC_FEATURES_ENABLE`]`.FCH` on every NBIO, so the bridges'
/// interrupts reach their NBIO IOAPIC only.
#[cfg(feature = "acpi")]
pub mod acpi {
    extern crate alloc;
    use super::{IOAPIC_FEATURES_ENABLE, IoapicFeatures};
    use alloc::{string::String, vec::Vec};
    use fstart_acpi::acpi_dsl;
    use fstart_acpi::{
        Aml, AmlError,
        aml::{Arg, If, Method, MethodCall, Path, Store},
        aml_linker::scope_vec,
    };

    /// `Device(NBIO)`, the root complex (D0F0) of the root bridge it is
    /// in, with `NAPE`: clear the FCH bit through the SMN pair at D0F0
    /// 0xB8/0xBC. `features_smn` is the register's SMN address as that
    /// root complex reaches it.
    pub fn nbio_aml(features_smn: u32) -> Result<Vec<u8>, AmlError> {
        let keep = !IoapicFeatures::FCH::SET.mask();
        let mut v = Vec::new();
        acpi_dsl! {
            Device("NBIO") {
                Name("_ADR", 0u8);
                OperationRegion("NAPC", PciConfig, 0xB8u8, 8u8);
                Field("NAPC", DWordAcc, NoLock, Preserve) {
                    NAPX, 32,
                    NAPD, 32,
                }
                Method("NAPE", 0, Serialized) {
                    NAPX = #{dword features_smn};
                    NAPD = NAPD & #{dword keep};
                }
            }
        }
        .emit(&mut v)?;
        Ok(v)
    }

    /// The `NBIO` of a root bridge with the register at its usual SMN
    /// address.
    pub fn nbio_default_aml() -> Result<Vec<u8>, AmlError> {
        nbio_aml(IOAPIC_FEATURES_ENABLE.address())
    }

    /// `\PICM` and `\_PIC`, which runs every `NAPE` in `nbios` (absolute
    /// paths of the [`nbio_aml`] devices) when the OS selects APIC mode.
    pub fn pic_aml(nbios: &[String]) -> Result<Vec<u8>, AmlError> {
        let paths: Vec<String> = nbios.iter().map(|n| alloc::format!("{n}.NAPE")).collect();
        let calls: Vec<MethodCall> = paths
            .iter()
            .map(|p| MethodCall::new(Path::new(p), Vec::new()))
            .collect();
        let picm = Path::new("PICM");
        let store = Store::new(&picm, &Arg(0));
        let nape = If::new(&Arg(0), calls.iter().map(|c| c as &dyn Aml).collect());
        let mut pic = Vec::new();
        Method::new(Path::new("_PIC"), 1, false, alloc::vec![&store, &nape]).to_aml_bytes(&mut pic);
        let picm: Vec<u8> = acpi_dsl! { Name("PICM", 0u8); }.into();
        scope_vec("\\", &[picm, pic].concat())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nbio::zp::tables::BASE;
    use crate::table::Op;

    /// The routes the `_PRT`s describe are the ones the ZP base table
    /// programs on Zeppelin and Naples.
    #[test]
    fn zp_base_programs_the_ioapic_routes() {
        let programmed: Vec<u32> = BASE
            .iter()
            .filter(|entry| (0x1430_0040..0x1430_00a0).contains(&entry.addr))
            .map(|entry| match entry.op {
                Op::Update {
                    clear: 0x001f_0037,
                    set,
                } => set,
                op => panic!("{op:?}"),
            })
            .collect();
        assert_eq!(programmed, ROUTES_ZP.map(|route| route.value()));
    }
}
