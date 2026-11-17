// SPDX-License-Identifier: MIT OR Apache-2.0
//! DF4, the Data Fabric of the RS I/O die (Genoa, Siena): one node per
//! socket whose root bridges are its IOS (I/O stops), each with a fixed
//! fabric ID. DF4 moved the routing registers and the indirect access pair
//! (openSIL `SilFabricRegistersDfX.h`; Siena FabricRsPei and
//! AmdSocSp5RsPei, instruction-checked):
//!
//! | | DF2/DF3 | DF4 |
//! |---|---|---|
//! | FICAA/FICAD | F4x5C/F4x98, register\[10:2\] | F4x8C/F4xB8, register\[10:1\] |
//! | bus maps | F0xA0 | F0xC80 base, F0xC84 limit |
//! | VGA | F0x80 | F0xC08 |
//! | I/O | F0xC0/F0xC4 | F0xD00/F0xD04, base\[28:16\] |
//! | MMIO | F0x200 | F0xD80 base, D84 limit, D88 control, D8C \[55:48\] |
//! | DRAM | F0x110 | F7xE00 base, E04 limit, E08 control |
//!
//! The PSP leaves the bus maps; the x86 routes MMIO and I/O itself, with
//! the planner of the earlier fabrics ([`super::plan`]) and [`program`].

use core::marker::PhantomData;
use fstart_pci::{ConfigRegionAccess, ConfigRegister};
use heapless::Vec;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{LocalRegisterCopy, RegisterLongName, register_bitfields};

use super::plan::{IoRoute, MmioRoute};
use super::{BusRange, DfReg, DramRule, RootBridge, node_address};
use crate::smn::SmnReg;

register_bitfields![u32,
    /// F4x8C FabricIndirectConfigAccessAddress (DF4).
    pub Ficaa [
        INSTANCE_ACCESS OFFSET(0) NUMBITS(1) [],
        REGISTER OFFSET(1) NUMBITS(10) [],
        FUNCTION OFFSET(11) NUMBITS(3) [],
        INSTANCE OFFSET(16) NUMBITS(8) []
    ],
    /// F4x180 SystemCfg.
    pub SystemConfig [
        OTHER_SOCKET_PRESENT OFFSET(8) NUMBITS(1) []
    ],
    /// F0xC80 + 8i CfgAddressMap base.
    pub ConfigBase [
        READ_ENABLE OFFSET(0) NUMBITS(1) [],
        WRITE_ENABLE OFFSET(1) NUMBITS(1) [],
        SEGMENT OFFSET(8) NUMBITS(8) [],
        BUS_BASE OFFSET(16) NUMBITS(8) []
    ],
    /// F0xC84 + 8i CfgAddressMap limit.
    pub ConfigLimit [
        DESTINATION OFFSET(0) NUMBITS(12) [],
        BUS_LIMIT OFFSET(16) NUMBITS(8) []
    ],
    /// F0xC08 VGAEn.
    pub Vga [
        ENABLE OFFSET(0) NUMBITS(1) [],
        NONPOSTED OFFSET(1) NUMBITS(1) [],
        CPU_DISABLE OFFSET(2) NUMBITS(1) [],
        DESTINATION OFFSET(4) NUMBITS(12) []
    ],
    /// F0xD00 + 8i X86IOBaseAddress.
    pub IoBase [
        READ OFFSET(0) NUMBITS(1) [],
        WRITE OFFSET(1) NUMBITS(1) [],
        ISA OFFSET(5) NUMBITS(1) [],
        ADDRESS OFFSET(16) NUMBITS(13) []
    ],
    /// F0xD04 + 8i X86IOLimitAddress.
    pub IoLimit [
        DESTINATION OFFSET(0) NUMBITS(12) [],
        ADDRESS OFFSET(16) NUMBITS(13) []
    ],
    /// F0xD88 + 16i MmioAddressControl.
    pub MmioControl [
        READ OFFSET(0) NUMBITS(1) [],
        WRITE OFFSET(1) NUMBITS(1) [],
        CPU_DISABLE OFFSET(2) NUMBITS(1) [],
        NONPOSTED OFFSET(3) NUMBITS(1) [],
        DESTINATION OFFSET(16) NUMBITS(12) []
    ],
    /// F0xD8C + 16i: address\[55:48\] of the base and the limit.
    pub MmioExtension [
        BASE OFFSET(0) NUMBITS(8) [],
        LIMIT OFFSET(16) NUMBITS(8) []
    ],
    /// F7xE00 / F7xE04 + 16i DramBase/LimitAddress: address\[55:28\].
    pub DramAddress [
        ADDRESS OFFSET(0) NUMBITS(28) []
    ],
    /// F7xE08 + 16i DramAddressCtl.
    pub DramControl [
        VALID OFFSET(0) NUMBITS(1) [],
        LEGACY_HOLE OFFSET(1) NUMBITS(1) [],
        /// The target CS (the first one of an interleave set).
        DESTINATION OFFSET(16) NUMBITS(12) []
    ],
    /// F7x104 DramHoleControl.
    pub DramHole [
        VALID OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(24) NUMBITS(8) []
    ],
    /// SMN 0x5D27C: the IOS a Siena part has fused off.
    pub IosDisable [
        IOS1 OFFSET(0) NUMBITS(1) [],
        IOS3 OFFSET(9) NUMBITS(1) [],
        IOS0 OFFSET(17) NUMBITS(1) [],
        IOS2 OFFSET(27) NUMBITS(1) []
    ]
];

/// F4x8C FICAA and F4xB8 FICAD_LO.
pub const FICAA: u16 = 0x8c;
pub const FICAD_LO: u16 = 0xb8;
pub const SYSTEM_CONFIG: DfReg<SystemConfig::Register> = DfReg::new(4, 0x180);
pub const CONFIG_BASE: DfReg<ConfigBase::Register> = DfReg::new(0, 0xc80);
pub const CONFIG_LIMIT: DfReg<ConfigLimit::Register> = DfReg::new(0, 0xc84);
pub const CONFIG_STRIDE: u16 = 8;
pub const CONFIG_PAIRS: u8 = 8;
pub const VGA: DfReg<Vga::Register> = DfReg::new(0, 0xc08);
pub const IO_BASE: DfReg<IoBase::Register> = DfReg::new(0, 0xd00);
pub const IO_LIMIT: DfReg<IoLimit::Register> = DfReg::new(0, 0xd04);
pub const IO_STRIDE: u16 = 8;
pub const MMIO_BASE: DfReg = DfReg::new(0, 0xd80);
pub const MMIO_LIMIT: DfReg = DfReg::new(0, 0xd84);
pub const MMIO_CONTROL: DfReg<MmioControl::Register> = DfReg::new(0, 0xd88);
pub const MMIO_EXTENSION: DfReg<MmioExtension::Register> = DfReg::new(0, 0xd8c);
pub const MMIO_STRIDE: u16 = 16;
pub const MMIO_PAIRS: u8 = 16;
pub const IO_PAIRS: u8 = 8;
pub const DRAM_BASE: DfReg<DramAddress::Register> = DfReg::new(7, 0xe00);
pub const DRAM_LIMIT: DfReg<DramAddress::Register> = DfReg::new(7, 0xe04);
pub const DRAM_CONTROL: DfReg<DramControl::Register> = DfReg::new(7, 0xe08);
pub const DRAM_STRIDE: u16 = 0x10;
pub const DRAM_RULES: u8 = 20;
pub const DRAM_HOLE: DfReg<DramHole::Register> = DfReg::new(7, 0x104);
/// The primary IOMS (instance 34), whose copy of the DRAM map openSIL
/// reads; a broadcast read merges the instances' differing copies.
pub const DRAM_MAP_INSTANCE: u8 = 34;

/// SMN 0x5A86C: the fused CPUID signature.
pub const FUSED_CPUID: SmnReg = SmnReg::new(0x5a86c);
/// Siena's IOS fuses. Genoa's parts fuse no IOS off; their word
/// (0x5D280) has another layout and is not decoded.
pub const IOS_DISABLE: SmnReg<IosDisable::Register> = SmnReg::new(0x5d27c);

/// The IOS with the FCH: the primary root bridge.
pub const PRIMARY_IOS: u8 = 2;
/// The IOS of one socket.
pub const IOS_COUNT: u8 = 4;
/// The fabric ID of IOS 0; IOS `n` is `+ n`, a socket's `+ socket << 8`.
const IOS0_FABRIC_ID: u16 = 0x20;
/// The fabric instance of IOS 0's IOMS.
const IOS0_INSTANCE: u8 = 0x24;

/// The Data Fabric of a DF4 node: its own registers, and an instance's
/// through the DF4 indirect pair (F4x8C/F4xB8), which the BSP owns while
/// the silicon init runs.
pub struct Df<P> {
    pci: P,
    node: u8,
}

impl<P: ConfigRegionAccess> Df<P> {
    /// The DF of `socket`'s node (device 0x18 + socket on bus 0).
    ///
    /// # Safety
    /// The node exists, and the caller owns its indirect pair.
    pub unsafe fn new(pci: P, socket: u8) -> Self {
        Self { pci, node: socket }
    }

    fn config<R: RegisterLongName>(&self, function: u8, offset: u16) -> ConfigRegister<&P, R> {
        // SAFETY: the constructor's contract.
        unsafe { ConfigRegister::new(&self.pci, node_address(self.node, function), offset) }
    }

    /// The node's own register (AGESA's broadcast).
    pub fn reg<R: RegisterLongName>(&self, reg: DfReg<R>) -> ConfigRegister<&P, R> {
        self.config(reg.function(), reg.register())
    }

    /// The register of fabric instance `instance`.
    pub fn instance<R: RegisterLongName>(&self, reg: DfReg<R>, instance: u8) -> Instance<'_, P, R> {
        Instance {
            df: self,
            selector: Ficaa::INSTANCE_ACCESS::SET
                + Ficaa::REGISTER.val(u32::from(reg.register() >> 2))
                + Ficaa::FUNCTION.val(u32::from(reg.function()))
                + Ficaa::INSTANCE.val(u32::from(instance)),
            _reg: PhantomData,
        }
    }
}

/// An instance's register: FICAA, then FICAD.
pub struct Instance<'a, P, R: RegisterLongName> {
    df: &'a Df<P>,
    selector: tock_registers::fields::FieldValue<u32, Ficaa::Register>,
    _reg: PhantomData<R>,
}

impl<P: ConfigRegionAccess, R: RegisterLongName> Instance<'_, P, R> {
    fn data(&self) -> ConfigRegister<&P, R> {
        self.df
            .config::<Ficaa::Register>(4, FICAA)
            .write(self.selector);
        self.df.config(4, FICAD_LO)
    }
}

impl<P: ConfigRegionAccess, R: RegisterLongName> Readable for Instance<'_, P, R> {
    type T = u32;
    type R = R;
    fn get(&self) -> u32 {
        self.data().get()
    }
}

impl<P: ConfigRegionAccess, R: RegisterLongName> Writeable for Instance<'_, P, R> {
    type T = u32;
    type R = R;
    fn set(&self, value: u32) {
        self.data().set(value)
    }
}

/// Program the MMIO and I/O rules of a [`super::plan::Plan`] in DF4's
/// layout, base, limit, then control (FabricRsPei's writer `ffa6d46d`),
/// and clear every other enabled rule: the x86 routes the whole space.
pub fn program<P: ConfigRegionAccess>(df: &Df<P>, mmio: &[MmioRoute], io: &[IoRoute]) {
    for pair in 0..MMIO_PAIRS {
        let control = df.reg(MMIO_CONTROL.at(pair, MMIO_STRIDE));
        match mmio.iter().find(|r| r.pair == pair) {
            Some(r) => {
                let base = r.window.base.next_multiple_of(0x1_0000);
                let limit = base + r.window.size - 1;
                df.reg(MMIO_BASE.at(pair, MMIO_STRIDE))
                    .set((base >> 16) as u32);
                df.reg(MMIO_LIMIT.at(pair, MMIO_STRIDE))
                    .set((limit >> 16) as u32);
                df.reg(MMIO_EXTENSION.at(pair, MMIO_STRIDE)).write(
                    MmioExtension::BASE.val((base >> 48) as u32 & 0xff)
                        + MmioExtension::LIMIT.val((limit >> 48) as u32 & 0xff),
                );
                control.write(
                    MmioControl::READ::SET
                        + MmioControl::WRITE::SET
                        + MmioControl::DESTINATION.val(r.fabric_id.into()),
                );
            }
            None if control.get() != 0 => control.set(0),
            None => {}
        }
    }
    for pair in 0..IO_PAIRS {
        let base = df.reg(IO_BASE.at(pair, IO_STRIDE));
        match io.iter().find(|r| r.pair == pair) {
            Some(r) => {
                base.write(
                    IoBase::READ::SET
                        + IoBase::WRITE::SET
                        + IoBase::ADDRESS.val((r.window.base >> 12) as u32),
                );
                df.reg(IO_LIMIT.at(pair, IO_STRIDE)).write(
                    IoLimit::ADDRESS.val(((r.window.end() - 1) >> 12) as u32)
                        + IoLimit::DESTINATION.val(r.fabric_id.into()),
                );
            }
            None if base.get() != 0 => base.set(0),
            None => {}
        }
    }
}

/// Route the legacy VGA window (0xA0000–0xBFFFF, the VGA ports) to the IOS
/// with fabric ID `fabric_id`.
pub fn route_vga<P: ConfigRegionAccess>(df: &Df<P>, fabric_id: u16) {
    df.reg(VGA)
        .write(Vga::ENABLE::SET + Vga::DESTINATION.val(fabric_id.into()));
}

/// One IOS of a socket by its physical index, which the lane tables and
/// the SMN addresses of its NBIO use (`index << 20`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ios {
    pub index: u8,
}

impl Ios {
    pub const fn fabric_id(self, socket: u8) -> u16 {
        (socket as u16) << 8 | (IOS0_FABRIC_ID + self.index as u16)
    }
    /// The fabric instance of the IOS's IOMS.
    pub const fn instance(self) -> u8 {
        IOS0_INSTANCE + self.index
    }
    pub const fn primary(self) -> bool {
        self.index == PRIMARY_IOS
    }
}

/// Every IOS of a socket.
pub const IOS: [Ios; IOS_COUNT as usize] = [
    Ios { index: 0 },
    Ios { index: 1 },
    Ios { index: 2 },
    Ios { index: 3 },
];

/// The IOS a Siena part has, from its fuse word [`IOS_DISABLE`]; `None` if
/// the word reads as all ones.
pub fn present_ios(disable: u32) -> Option<Vec<Ios, { IOS_COUNT as usize }>> {
    if disable == u32::MAX {
        return None;
    }
    let disable = LocalRegisterCopy::<u32, IosDisable::Register>::new(disable);
    let fused = [
        IosDisable::IOS0,
        IosDisable::IOS1,
        IosDisable::IOS2,
        IosDisable::IOS3,
    ];
    Some(
        IOS.into_iter()
            .zip(fused)
            .filter(|&(_, field)| !disable.is_set(field))
            .map(|(ios, _)| ios)
            .collect(),
    )
}

/// An enabled CfgAddressMap pair: its segment, buses and destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusRoute {
    pub segment: u8,
    pub buses: BusRange,
    pub destination: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A register read as all ones.
    Unavailable,
    /// A bus map with one enable, or a limit below its base.
    InvalidRoute,
    /// An IOS without a bus map, or a map to no present IOS.
    MissingRoute,
    /// Two maps to one IOS, or overlapping buses.
    DuplicateRoute,
    /// The FCH's IOS is absent.
    NoPrimary,
}

impl BusRoute {
    /// Decode a CfgAddressMap pair; `None` if it is unused (no enable).
    pub fn decode(base: u32, limit: u32) -> Result<Option<Self>, Error> {
        if base == u32::MAX || limit == u32::MAX {
            return Err(Error::Unavailable);
        }
        let base = LocalRegisterCopy::<u32, ConfigBase::Register>::new(base);
        let limit = LocalRegisterCopy::<u32, ConfigLimit::Register>::new(limit);
        match (
            base.is_set(ConfigBase::READ_ENABLE),
            base.is_set(ConfigBase::WRITE_ENABLE),
        ) {
            (false, false) => return Ok(None),
            (true, true) => {}
            _ => return Err(Error::InvalidRoute),
        }
        let buses = BusRange {
            base: base.read(ConfigBase::BUS_BASE) as u8,
            limit: limit.read(ConfigLimit::BUS_LIMIT) as u8,
        };
        if buses.base > buses.limit {
            return Err(Error::InvalidRoute);
        }
        Ok(Some(Self {
            segment: base.read(ConfigBase::SEGMENT) as u8,
            buses,
            destination: limit.read(ConfigLimit::DESTINATION) as u16,
        }))
    }
}

/// The root bridges of `socket`: each of its present IOS (`ios`, physical
/// order) with the buses the bus map routes to it. Every present IOS needs
/// exactly one map, and every map an IOS; the FCH's IOS must be present.
pub fn root_bridges(
    socket: u8,
    ios: &[Ios],
    routes: &[BusRoute],
) -> Result<Vec<RootBridge, { IOS_COUNT as usize }>, Error> {
    for (i, route) in routes.iter().enumerate() {
        if !ios
            .iter()
            .any(|io| io.fabric_id(socket) == route.destination)
        {
            return Err(Error::MissingRoute);
        }
        let earlier = &routes[..i];
        if earlier.iter().any(|r| {
            r.destination == route.destination
                || (r.segment == route.segment
                    && r.buses.base <= route.buses.limit
                    && route.buses.base <= r.buses.limit)
        }) {
            return Err(Error::DuplicateRoute);
        }
    }
    if !ios.iter().any(|io| io.primary()) {
        return Err(Error::NoPrimary);
    }
    ios.iter()
        .map(|&io| {
            let route = routes
                .iter()
                .find(|r| r.destination == io.fabric_id(socket))
                .ok_or(Error::MissingRoute)?;
            Ok(RootBridge {
                socket,
                index: io.index,
                fabric_id: io.fabric_id(socket),
                bus_base: route.buses.base,
                bus_limit: route.buses.limit,
                primary: socket == 0 && io.primary(),
            })
        })
        .collect()
}

/// The bus maps of node `df`, in pair order.
pub fn bus_routes<P: fstart_pci::ConfigRegionAccess>(
    df: &Df<P>,
) -> Result<Vec<BusRoute, { CONFIG_PAIRS as usize }>, Error> {
    let mut routes = Vec::new();
    for pair in 0..CONFIG_PAIRS {
        let base = df.reg(CONFIG_BASE.at(pair, CONFIG_STRIDE)).get();
        let limit = df.reg(CONFIG_LIMIT.at(pair, CONFIG_STRIDE)).get();
        if let Some(route) = BusRoute::decode(base, limit)? {
            // CONFIG_PAIRS is the capacity.
            let _ = routes.push(route);
        }
    }
    Ok(routes)
}

/// Decode a DRAM rule (address\[55:28\]); `None` if it is not valid.
pub fn decode_dram(base: u32, limit: u32, control: u32) -> Option<DramRule> {
    let control = LocalRegisterCopy::<u32, DramControl::Register>::new(control);
    if control.get() == u32::MAX || !control.is_set(DramControl::VALID) {
        return None;
    }
    let address = |raw| {
        u64::from(
            LocalRegisterCopy::<u32, DramAddress::Register>::new(raw).read(DramAddress::ADDRESS),
        ) << 28
    };
    Some(DramRule {
        base: address(base),
        end: address(limit) + (1 << 28),
        dst: control.read(DramControl::DESTINATION) as u16,
        hole: control.is_set(DramControl::LEGACY_HOLE),
        socket_interleave: false,
        die_interleave: 0,
        channel_interleave: 0,
    })
}

/// The DRAM map of `df`, from the primary IOMS's copy
/// ([`DRAM_MAP_INSTANCE`]); `None` if more rules are valid than one map
/// holds.
pub fn dram<P: fstart_pci::ConfigRegionAccess>(df: &Df<P>) -> Option<super::dram::DramMap> {
    let read = |reg: DfReg<_>| df.instance(reg, DRAM_MAP_INSTANCE).get();
    let mut rules = Vec::new();
    for i in 0..DRAM_RULES {
        if let Some(rule) = decode_dram(
            read(DRAM_BASE.at(i, DRAM_STRIDE)),
            read(DRAM_LIMIT.at(i, DRAM_STRIDE)),
            df.instance(DRAM_CONTROL.at(i, DRAM_STRIDE), DRAM_MAP_INSTANCE)
                .get(),
        ) {
            rules.push(rule).ok()?;
        }
    }
    let hole = df.instance(DRAM_HOLE, DRAM_MAP_INSTANCE).extract();
    Some(super::dram::DramMap {
        rules,
        hole_base: hole
            .is_set(DramHole::VALID)
            .then(|| u64::from(hole.read(DramHole::BASE)) << 24),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tock_registers::interfaces::Writeable;

    /// (function, offset, instance).
    type Key = (u8, u16, Option<u8>);

    /// The registers of node 0 by (function, offset), FICAA4/FICAD4
    /// resolved to (function, offset, instance).
    #[derive(Default)]
    struct Node {
        ficaa: core::cell::Cell<u32>,
        regs: core::cell::RefCell<std::collections::BTreeMap<Key, u32>>,
    }
    impl Node {
        fn key(&self, address: fstart_pci::PciAddress, offset: u16) -> Key {
            if (address.function(), offset) == (4, FICAD_LO) {
                let f = LocalRegisterCopy::<u32, Ficaa::Register>::new(self.ficaa.get());
                assert!(f.is_set(Ficaa::INSTANCE_ACCESS));
                return (
                    f.read(Ficaa::FUNCTION) as u8,
                    (f.read(Ficaa::REGISTER) << 2) as u16,
                    Some(f.read(Ficaa::INSTANCE) as u8),
                );
            }
            (address.function(), offset, None)
        }
        fn get(&self, key: (u8, u16, Option<u8>)) -> u32 {
            self.regs.borrow().get(&key).copied().unwrap_or(0)
        }
    }
    impl ConfigRegionAccess for Node {
        unsafe fn read(&self, address: fstart_pci::PciAddress, offset: u16) -> u32 {
            self.get(self.key(address, offset))
        }
        unsafe fn write(&self, address: fstart_pci::PciAddress, offset: u16, value: u32) {
            if (address.function(), offset) == (4, FICAA) {
                self.ficaa.set(value);
                return;
            }
            let key = self.key(address, offset);
            self.regs.borrow_mut().insert(key, value);
        }
    }

    #[test]
    fn instances_go_through_the_df4_pair() {
        let node = Node::default();
        // SAFETY: a host model.
        let df = unsafe { Df::new(&node, 0) };
        df.instance(DfReg::<()>::new(7, 0xe00), 0x22)
            .set(0x1234_5678);
        assert_eq!(node.ficaa.get(), 0x0022_3f01);
        assert_eq!(node.get((7, 0xe00, Some(0x22))), 0x1234_5678);
        df.reg(DfReg::<()>::new(0, 0xd80)).set(1);
        assert_eq!(node.get((0, 0xd80, None)), 1);
    }

    #[test]
    fn siena_fuses_keep_physical_indices() {
        // The S8040's part: IOS1 fused off.
        let ios = present_ios(0x800f7).unwrap();
        assert_eq!(
            ios.iter().map(|i| i.index).collect::<Vec<_, 4>>(),
            [0, 2, 3]
        );
        for raw in 0..16u32 {
            let disable = [1 << 17, 1, 1 << 27, 1 << 9]
                .iter()
                .enumerate()
                .filter(|(i, _)| raw & 1 << i != 0)
                .fold(0, |w, (_, bit)| w | bit);
            let ios = present_ios(disable).unwrap();
            assert!(
                ios.iter()
                    .map(|i| i.index)
                    .eq((0..4).filter(|i| raw & 1 << i == 0))
            );
        }
        assert_eq!(present_ios(u32::MAX), None);
        assert_eq!(IOS[3].fabric_id(1), 0x123);
        assert_eq!(IOS[3].instance(), 0x27);
    }

    #[test]
    fn binds_the_s8040_bus_maps() {
        // The live S8040 pairs (pair 2 and 4..7 unused).
        let raw = [
            (0x3, 0x3f_0022),
            (0x40_0003, 0x7f_0023),
            (0, 0x20),
            (0xc0_0003, 0xff_0020),
        ];
        let routes: std::vec::Vec<_> = raw
            .iter()
            .filter_map(|&(b, l)| BusRoute::decode(b, l).unwrap())
            .collect();
        let ios = present_ios(0x800f7).unwrap();
        let rbs = root_bridges(0, &ios, &routes).unwrap();
        assert_eq!(
            rbs.iter()
                .map(|r| (r.index, r.fabric_id, r.bus_base, r.bus_limit, r.primary))
                .collect::<std::vec::Vec<_>>(),
            [
                (0, 0x20, 0xc0, 0xff, false),
                (2, 0x22, 0x00, 0x3f, true),
                (3, 0x23, 0x40, 0x7f, false)
            ]
        );
        assert_eq!(
            root_bridges(0, &ios, &routes[..2]),
            Err(Error::MissingRoute)
        );
        let mut twice = routes.clone();
        twice[1].destination = 0x22;
        assert_eq!(root_bridges(0, &ios, &twice), Err(Error::DuplicateRoute));
        let mut overlap = routes.clone();
        overlap[1].buses.base = 0x3f;
        assert_eq!(root_bridges(0, &ios, &overlap), Err(Error::DuplicateRoute));
        assert_eq!(
            root_bridges(0, &ios[..1], &routes[2..]),
            Err(Error::NoPrimary)
        );
        for (base, limit) in [(u32::MAX, 0), (1, 0), (3 | 0x80 << 16, 0x7f << 16)] {
            assert!(BusRoute::decode(base, limit).is_err());
        }
    }

    #[test]
    fn the_plan_is_written_in_the_df4_layout() {
        use crate::df::plan::Window;
        let node = Node::default();
        // A stale rule in a pair the plan leaves out.
        node.regs.borrow_mut().insert((0, 0xd88 + 5 * 16, None), 3);
        // SAFETY: a host model.
        let df = unsafe { Df::new(&node, 0) };
        program(
            &df,
            &[MmioRoute {
                pair: 1,
                window: Window::new(0x100_0000_0000, 0x40_0000_0000),
                fabric_id: 0x23,
            }],
            &[IoRoute {
                pair: 0,
                window: Window::new(0x4000, 0x4000),
                fabric_id: 0x20,
            }],
        );
        assert_eq!(node.get((0, 0xd90, None)), 0x0100_0000);
        assert_eq!(node.get((0, 0xd94, None)), 0x013f_ffff);
        assert_eq!(node.get((0, 0xd98, None)), 0x23 << 16 | 3);
        assert_eq!(node.get((0, 0xd9c, None)), 0);
        assert_eq!(node.get((0, 0xd88 + 5 * 16, None)), 0);
        assert_eq!(node.get((0, 0xd00, None)), 4 << 16 | 3);
        assert_eq!(node.get((0, 0xd04, None)), 7 << 16 | 0x20);
        route_vga(&df, 0x22);
        assert_eq!(node.get((0, 0xc08, None)), 0x221);
    }

    #[test]
    fn dram_rules_decode_bits_55_28() {
        assert_eq!(decode_dram(0, 0x1f4, 0x0003_0000), None);
        let low = decode_dram(0, 0x204, 0x0004_0003).unwrap();
        assert_eq!(
            (low.base, low.end, low.dst, low.hole),
            (0, 0x20_5000_0000, 4, true)
        );
    }
}
