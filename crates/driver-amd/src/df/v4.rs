// SPDX-License-Identifier: MIT OR Apache-2.0
//! DF4 Rs discovery registers (Siena/Genoa), not DF2/DF3 resource policy.
//! The shared transport selects DF4's FICAA3 protocol at F4x8C/B8, with
//! register-number bits10:1, not DF2/3's F4x5C/98 and bits10:2.
//! Source: silicon-fabric contracts rs.df.sockets / bus_base and the
//! independently recovered Siena IOS-fuse/device-map contracts.
pub mod dram;
pub mod io;
pub mod mmio;
use super::DfReg;
use crate::smn::SmnReg;
use heapless::Vec;
use tock_registers::{LocalRegisterCopy, register_bitfields};

/// The common DF transport with the DF4 indirect encoding and pair.
pub type Df<P> = super::Fabric<P, true>;

register_bitfields![u32,
    pub CoreMasterAccess [CF8_EXTENDED OFFSET(0) NUMBITS(1) [],
        DISABLE_CONFIG_IO OFFSET(1) NUMBITS(1) []],
    pub ConfigAddressControl [SECONDARY_BUS OFFSET(0) NUMBITS(8) []],
    /// DF4 F3x044; field layout independently witnessed in NFX FabricRsPei.
    pub GlobalControl [
        DIS_IMMEDIATE_SYNC_FLOOD OFFSET(3) NUMBITS(1) [],
        WDT_BASE OFFSET(4) NUMBITS(2) [],
        PIE_WDT_COUNT OFFSET(8) NUMBITS(3) [],
        IOM_WDT_COUNT OFFSET(12) NUMBITS(3) [],
        CCM_WDT_COUNT OFFSET(16) NUMBITS(3) []
    ],
    pub SystemConfig [OTHER_SOCKET_PRESENT OFFSET(8) NUMBITS(1) []],
    pub ConfigBase [
        READ_ENABLE OFFSET(0) NUMBITS(1) [],
        WRITE_ENABLE OFFSET(1) NUMBITS(1) [],
        SEGMENT OFFSET(8) NUMBITS(8) [],
        BUS_BASE OFFSET(16) NUMBITS(8) []
    ],
    pub ConfigLimit [
        DESTINATION OFFSET(0) NUMBITS(12) [],
        BUS_LIMIT OFFSET(16) NUMBITS(8) []
    ],
    pub IosDisable [
        IOS0 OFFSET(17) NUMBITS(1) [],
        IOS1 OFFSET(0) NUMBITS(1) [],
        IOS2 OFFSET(27) NUMBITS(1) [],
        IOS3 OFFSET(9) NUMBITS(1) []
    ]
];
pub const SYSTEM_CONFIG: DfReg<SystemConfig::Register> = DfReg::new(4, 0x180);
/// DF4 F0xC00, not legacy DF2's configuration-access register.
pub const CORE_MASTER_ACCESS: DfReg<CoreMasterAccess::Register> = DfReg::new(0, 0xc00);
pub const CONFIG_ADDRESS_CONTROL: DfReg<ConfigAddressControl::Register> = DfReg::new(0, 0xc04);
pub const GLOBAL_CONTROL: DfReg<GlobalControl::Register> = DfReg::new(3, 0x044);
// Diagnostic reads only: preserve assert and response-queue watchdog evidence.
// Writes to these status/log registers can clear evidence.
pub const HARDWARE_ASSERT_LOW: DfReg = DfReg::new(3, 0x8f0);
pub const HARDWARE_ASSERT_HIGH: DfReg = DfReg::new(3, 0x8f4);
pub const HARDWARE_ASSERT_MASK_LOW: DfReg = DfReg::new(3, 0x8f8);
pub const HARDWARE_ASSERT_MASK_HIGH: DfReg = DfReg::new(3, 0x8fc);
pub const RESPONSE_WDT_IO_LOW: DfReg = DfReg::new(3, 0x900);
pub const RESPONSE_WDT_IO_HIGH: DfReg = DfReg::new(3, 0x904);
pub const CONFIG_BASE: DfReg<ConfigBase::Register> = DfReg::new(0, 0xc80);
pub const CONFIG_LIMIT: DfReg<ConfigLimit::Register> = DfReg::new(0, 0xc84);
pub const CONFIG_PAIRS: u8 = 8;
pub const FUSED_CPUID: SmnReg = SmnReg::new(0x5a86c);
/// Siena (class8000h) always uses5D27C. Early Genoa's5D280 policy is NOT
/// selected by this API, even though the packed DF registers are shared.
pub const SIENA_IOS_DISABLE: SmnReg<IosDisable::Register> = SmnReg::new(0x5d27c);
/// Early Genoa selects a different fuse word. Its nonzero layout is not
/// assumed to match Siena's; callers must enforce their stepping contract.
pub const GENOA_EARLY_IOS_DISABLE: SmnReg = SmnReg::new(0x5d280);
pub const PRIMARY_IOS: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ios {
    /// Unfiltered physical IOS index, not a compacted root-list index.
    pub physical_index: u8,
    pub fabric_id: u16,
    pub instance: u8,
}
/// Physical Rs IOS identities before fuse filtering or bus-route binding.
pub const PHYSICAL_IOS: [Ios; 4] = {
    let mut out = [Ios {
        physical_index: 0,
        fabric_id: 0,
        instance: 0,
    }; 4];
    let mut i = 0;
    while i < 4 {
        out[i] = Ios {
            physical_index: i as u8,
            fabric_id: 0x20 + i as u16,
            instance: 0x24 + i as u8,
        };
        i += 1;
    }
    out
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    InvalidRoute,
    MissingRoute,
    DuplicateRoute,
    OverlappingBuses,
    UnknownDestination,
}

pub fn siena_ios(raw: u32) -> Result<Vec<Ios, 4>, Error> {
    if raw == u32::MAX {
        return Err(Error::Unavailable);
    }
    let disabled = LocalRegisterCopy::<u32, IosDisable::Register>::new(raw);
    let masks = [
        IosDisable::IOS0,
        IosDisable::IOS1,
        IosDisable::IOS2,
        IosDisable::IOS3,
    ];
    let mut out = Vec::new();
    for (index, field) in masks.into_iter().enumerate() {
        if !disabled.is_set(field) {
            // Capacity equals the fixed four physical IOS slots.
            out.push(PHYSICAL_IOS[index]).unwrap();
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusRoute {
    pub segment: u8,
    pub base: u8,
    pub limit: u8,
    pub destination: u16,
}
impl BusRoute {
    /// Both enables clear is an unused pair. Partial enables, unavailable
    /// reads and reversed intervals are errors, not fabricated root buses.
    pub fn decode(base: u32, limit: u32) -> Result<Option<Self>, Error> {
        if base == u32::MAX || limit == u32::MAX {
            return Err(Error::Unavailable);
        }
        let base = LocalRegisterCopy::<u32, ConfigBase::Register>::new(base);
        let limit = LocalRegisterCopy::<u32, ConfigLimit::Register>::new(limit);
        let read = base.is_set(ConfigBase::READ_ENABLE);
        let write = base.is_set(ConfigBase::WRITE_ENABLE);
        if !read && !write {
            return Ok(None);
        }
        let lo = base.read(ConfigBase::BUS_BASE) as u8;
        let hi = limit.read(ConfigLimit::BUS_LIMIT) as u8;
        if read != write || lo > hi {
            return Err(Error::InvalidRoute);
        }
        Ok(Some(Self {
            segment: base.read(ConfigBase::SEGMENT) as u8,
            base: lo,
            limit: hi,
            destination: limit.read(ConfigLimit::DESTINATION) as u16,
        }))
    }
}

/// A live IOS bound to its validated config route. Physical IOS/lane-table
/// identity is not replaced by its position in the fuse-filtered vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Root {
    pub ios: Ios,
    pub route: BusRoute,
}

/// Bind one Rs socket's IOS list to actual enabled config routes. Refuse
/// stale/duplicate destinations, missing IOS routes and overlapping buses;
/// never manufacture bus0 for an unfound or fused-off root.
pub fn bind_routes(socket: u8, ios: &[Ios], routes: &[BusRoute]) -> Result<Vec<Root, 4>, Error> {
    if socket >= 2 || ios.len() > 4 || routes.len() > usize::from(CONFIG_PAIRS) {
        return Err(Error::InvalidRoute);
    }
    if ios.is_empty() {
        return Err(Error::MissingRoute);
    }
    for (i, root) in ios.iter().enumerate() {
        if root.physical_index >= 4
            || root.fabric_id != 0x20 + u16::from(root.physical_index)
            || root.instance != 0x24 + root.physical_index
            || ios[..i]
                .iter()
                .any(|old| old.physical_index == root.physical_index)
        {
            return Err(Error::InvalidRoute);
        }
    }
    for (i, route) in routes.iter().enumerate() {
        if route.base > route.limit {
            return Err(Error::InvalidRoute);
        }
        if !ios
            .iter()
            .any(|root| route.destination == (u16::from(socket) << 8 | root.fabric_id))
        {
            return Err(Error::UnknownDestination);
        }
        for old in &routes[..i] {
            if old.destination == route.destination {
                return Err(Error::DuplicateRoute);
            }
            if old.segment == route.segment && route.base <= old.limit && old.base <= route.limit {
                return Err(Error::OverlappingBuses);
            }
        }
    }
    let mut out = Vec::new();
    for &root in ios {
        let route = routes
            .iter()
            .find(|r| r.destination == (u16::from(socket) << 8 | root.fabric_id))
            .ok_or(Error::MissingRoute)?;
        out.push(Root {
            ios: root,
            route: *route,
        })
        .map_err(|_| Error::InvalidRoute)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rs_config_bus_and_watchdog_fields_decode_without_changing_policy() {
        use super::*;
        let bus = LocalRegisterCopy::<u32, ConfigAddressControl::Register>::new(0x1234_56c0);
        assert_eq!(bus.read(ConfigAddressControl::SECONDARY_BUS), 0xc0);
        let control = LocalRegisterCopy::<u32, GlobalControl::Register>::new(0x0006_6628);
        assert!(control.is_set(GlobalControl::DIS_IMMEDIATE_SYNC_FLOOD));
        assert_eq!(control.read(GlobalControl::WDT_BASE), 2);
        assert_eq!(control.read(GlobalControl::PIE_WDT_COUNT), 6);
        assert_eq!(control.read(GlobalControl::IOM_WDT_COUNT), 6);
        assert_eq!(control.read(GlobalControl::CCM_WDT_COUNT), 6);
        assert_eq!(GLOBAL_CONTROL.function(), 3);
        assert_eq!(GLOBAL_CONTROL.register(), 0x44);
    }

    #[test]
    fn binds_the_live_three_root_shape_without_compacting_lane_indices() {
        let ios = siena_ios(0x800f7).unwrap();
        let routes = [
            BusRoute {
                segment: 0,
                base: 0,
                limit: 0x3f,
                destination: 0x22,
            },
            BusRoute {
                segment: 0,
                base: 0x40,
                limit: 0x7f,
                destination: 0x23,
            },
            BusRoute {
                segment: 0,
                base: 0xc0,
                limit: 0xff,
                destination: 0x20,
            },
        ];
        let roots = bind_routes(0, &ios, &routes).unwrap();
        assert_eq!(
            roots
                .iter()
                .map(|r| (r.ios.physical_index, r.route.base))
                .collect::<std::vec::Vec<_>>(),
            [(0, 0xc0), (2, 0), (3, 0x40)]
        );
        assert_eq!(bind_routes(0, &ios, &routes[..2]), Err(Error::MissingRoute));
        let mut invalid = routes;
        invalid[1].destination = 0x21;
        assert_eq!(
            bind_routes(0, &ios, &invalid),
            Err(Error::UnknownDestination)
        );
        invalid = routes;
        invalid[1].destination = 0x22;
        assert_eq!(bind_routes(0, &ios, &invalid), Err(Error::DuplicateRoute));
        invalid = routes;
        invalid[1].base = 0x3f;
        assert_eq!(bind_routes(0, &ios, &invalid), Err(Error::OverlappingBuses));
        assert_eq!(
            bind_routes(1, &ios, &routes),
            Err(Error::UnknownDestination)
        );
        let remote = routes.map(|r| BusRoute {
            destination: r.destination | 0x100,
            ..r
        });
        assert!(bind_routes(1, &ios, &remote).is_ok());
        assert!(bind_routes(0, &[], &[]).is_err());
    }

    use super::*;
    #[test]
    fn indirect_access_uses_df4_pair_and_all_ten_register_bits() {
        use tock_registers::interfaces::{Readable, Writeable};
        let world = super::super::testing::DfSpace::default();
        let df = unsafe { Df::new(&world) };
        let register = DfReg::<()>::new(7, 0xe00);
        assert_eq!(super::super::ficaa4(register, 0x22).value, 0x0022_3f01);
        df.instance(register, 0x22).set(0x1234_5678);
        assert_eq!(
            world.regs.borrow().get(&(7, 0xe00, Some(0x22))),
            Some(&0x1234_5678)
        );
        assert_eq!(df.instance(register, 0x22).get(), 0x1234_5678);
        let legacy = world.df();
        legacy.instance(DfReg::<()>::new(7, 0x600), 1).set(0xabcd);
        assert_eq!(world.regs.borrow().get(&(7, 0x600, Some(1))), Some(&0xabcd));
        assert_eq!(df.instance(register, 0x22).get(), 0x1234_5678);
    }
    #[test]
    fn all_fuse_combinations_keep_physical_and_filtered_indices_distinct() {
        let masks = [1 << 17, 1, 1 << 27, 1 << 9];
        for combination in 0..16 {
            let raw = masks
                .iter()
                .enumerate()
                .filter(|(i, _)| combination & (1 << i) != 0)
                .fold(0, |raw, (_, mask)| raw | mask);
            let roots = siena_ios(raw).unwrap();
            let expected: std::vec::Vec<_> =
                (0..4u8).filter(|i| combination & (1 << i) == 0).collect();
            assert_eq!(
                roots
                    .iter()
                    .map(|r| r.physical_index)
                    .collect::<std::vec::Vec<_>>(),
                expected
            );
            for root in roots {
                assert_eq!(
                    (root.fabric_id, root.instance),
                    (
                        0x20 + u16::from(root.physical_index),
                        0x24 + root.physical_index
                    )
                );
            }
        }
        assert_eq!(siena_ios(u32::MAX), Err(Error::Unavailable));
    }
    #[test]
    fn bus_routes_are_not_invented_for_absent_or_invalid_pairs() {
        assert_eq!(BusRoute::decode(0, 0), Ok(None));
        let route = BusRoute::decode(3 | (2 << 8) | (0x40 << 16), 0x22 | (0x7f << 16))
            .unwrap()
            .unwrap();
        assert_eq!(
            route,
            BusRoute {
                segment: 2,
                base: 0x40,
                limit: 0x7f,
                destination: 0x22
            }
        );
        for (base, limit) in [
            (u32::MAX, 0),
            (0, u32::MAX),
            (1, 0),
            (2, 0),
            (3 | (0x80 << 16), 0x7f << 16),
        ] {
            assert!(BusRoute::decode(base, limit).is_err());
        }
    }
}
