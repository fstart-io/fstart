// SPDX-License-Identifier: MIT OR Apache-2.0
//! DFv4.5, the Data Fabric of the WH dual-IOD socket: built on
//! [`super::v4::Df`] (its FICAA/FICAD indirect access and register layout),
//! with what WH differs in, as the vendor's code has it (SilVenicePei,
//! AmdFabricWhDxe; FABRIC_2P §2, checked against Dell PR9XJ 1.0.8 and
//! GIGABYTE MD25-CG0 F01):
//!
//! | | DF4 ([`super::v4`]) | DFv4.5 (here) |
//! |---|---|---|
//! | nodes | one per socket, `0x18 + socket` | one per IOD, `0x18 + (socket << 1) + die` |
//! | fabric ID | 12 bits, `socket << 8` | 8 bits, `socket << 7 \| die << 6` |
//! | CfgAddressMap / MMIO / I/O pairs | 8 / 16 / 8 | 16 / 32 / 16 |
//! | DRAM rules | `F7xE00`, 20, address\[55:28\] | `F7x200`, 32 × `0x20`, unshifted 56-bit |
//!
//! Every node gets the same CfgAddressMap, MMIO and I/O maps (the vendor
//! writes each pair to every socket × die); the legacy VGA window goes to
//! die 0 of each socket only. The PSP's ABL writes the bus maps; the x86
//! reads them back ([`Node::bus_routes`]) and rewrites them only for a
//! bus-count request or a map the ECAM does not decode ([`buses`],
//! [`program_buses`]).

use fstart_pci::{ConfigRegionAccess, PciAddress};
use heapless::Vec;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

use super::plan::{IoRoute, MmioRoute};
use super::{BusRange, DfReg, DramRule};

pub mod buses;
pub mod devices;
pub use super::v4::{BusRoute, Error};
pub use devices::Devices;

/// Most sockets (openSIL allows more only for GPU sockets).
pub const MAX_SOCKETS: u8 = 2;
/// IODs per socket.
pub const MAX_DIES: u8 = 2;
/// Most DF nodes (2 sockets × 2 IODs: devices `0x18..=0x1B`).
pub const MAX_NODES: u8 = MAX_SOCKETS * MAX_DIES;
/// Fabric ID bit of the socket (topology map `SocketShift`).
pub const SOCKET_SHIFT: u8 = 7;
/// Fabric ID bit of the IOD (topology map `DieShift`).
pub const DIE_SHIFT: u8 = 6;
/// CfgAddressMap pairs (`F0xC80 + 8i`).
pub const CONFIG_PAIRS: u8 = 16;
/// MmioAddress rules (`F0xD80 + 16i`).
pub const MMIO_PAIRS: u8 = 32;
/// X86IO rules (`F0xD00 + 8i`).
pub const IO_PAIRS: u8 = 16;
/// Number of DFv4.5 DRAM address rule slots (`F7x200..F7x5F0`).
pub const DRAM_RULES: usize = 32;
/// The DRAM rule copies a CS instance holds (the vendor's NUMA code matches
/// a rule against these).
pub const CS_DRAM_RULES: usize = 12;
/// Stride in bytes between consecutive DFv4.5 DRAM rule slots.
pub const DRAM_RULE_STRIDE: u16 = 0x20;
/// Base register offset of DFv4.5 DRAM rule 0 (`F7x200`).
pub const DRAM_RULE_BASE: u16 = 0x200;
/// Fabric instances whose DRAM map the vendor reads: `0x25` for the CCX
/// (RMP split), `0x24` for Dell's NUMA HOB. On B0 these are IOMs (type
/// `0xD`); [`Devices::iom`] names the IOD's own.
pub const DRAM_INSTANCE_PRIMARY: u8 = 0x24;
pub const DRAM_INSTANCE_SECONDARY: u8 = 0x25;
/// A fabric ID an absent or unconfigured instance reports.
const INVALID_FABRIC_IDS: [u8; 2] = [0xff, 0x55];

register_bitfields![u32,
    /// F7x204 / F7x20C + 0x20*i (32-bit): upper 24 bits (`[55:32]`) of a
    /// DFv4.5 unshifted 56-bit DRAM base or inclusive limit address.
    pub DramAddressHi [
        ADDRESS OFFSET(0) NUMBITS(24) []
    ],
    /// F7x210 + 0x20*i (32-bit): DramAddressCtl.
    pub DramRuleCtl [
        VALID OFFSET(0) NUMBITS(1) [],
        /// The range has the MMIO hole below 4 GiB cut out (from F7x104).
        LEGACY_HOLE OFFSET(1) NUMBITS(1) [],
        REMAP_EN OFFSET(4) NUMBITS(1) [],
        REMAP_SEL OFFSET(5) NUMBITS(3) [],
        /// The rule's socket is bit 7.
        DESTINATION OFFSET(16) NUMBITS(8) []
    ],
    /// F7x214 + 0x20*i (32-bit): the rule's target set.
    pub DramRuleTarget [
        /// Set: a non-DRAM (CXL) target set rather than the UMCs.
        NON_DRAM OFFSET(1) NUMBITS(1) []
    ],
    /// F0xC84 + 8i (32-bit): CfgAddressMap limit, 8-bit destination.
    pub ConfigLimit [
        DESTINATION OFFSET(0) NUMBITS(8) [],
        BUS_LIMIT OFFSET(16) NUMBITS(8) []
    ],
    /// F0xC04 (32-bit, per IOS instance): the IOS's secondary bus.
    pub IosBus [
        BUS OFFSET(0) NUMBITS(8) [],
        SEGMENT OFFSET(8) NUMBITS(8) []
    ],
    /// F0xC08 (32-bit): VGAEn, 8-bit destination.
    pub Vga [
        ENABLE OFFSET(0) NUMBITS(1) [],
        NONPOSTED OFFSET(1) NUMBITS(1) [],
        CPU_DISABLE OFFSET(2) NUMBITS(1) [],
        DESTINATION OFFSET(4) NUMBITS(8) []
    ],
    /// F0xD04 + 8i (32-bit): X86IOLimitAddress, 8-bit destination.
    pub IoLimit [
        DESTINATION OFFSET(0) NUMBITS(8) [],
        ADDRESS OFFSET(16) NUMBITS(13) []
    ],
    /// F0xD88 + 16i (32-bit): MmioAddressControl, 8-bit destination.
    pub MmioControl [
        READ OFFSET(0) NUMBITS(1) [],
        WRITE OFFSET(1) NUMBITS(1) [],
        CPU_DISABLE OFFSET(2) NUMBITS(1) [],
        NONPOSTED OFFSET(3) NUMBITS(1) [],
        DESTINATION OFFSET(16) NUMBITS(8) []
    ],
    /// F0x40 (32-bit): FabricBlockInstanceCount.
    pub BlockInstanceCount [
        COUNT OFFSET(0) NUMBITS(10) []
    ],
    /// F0x44 (32-bit, per instance): FabricBlockInstanceInformation0.
    pub InstanceInfo [
        ENABLED OFFSET(6) NUMBITS(1) []
    ],
    /// F0x50 (32-bit, per instance): FabricBlockInstanceInformation3.
    pub InstanceIds [
        BLOCK_ID OFFSET(0) NUMBITS(8) [],
        FABRIC_ID OFFSET(8) NUMBITS(8) []
    ],
    /// F0xC00 (32-bit): CoreMasterAccessCtrl.
    pub CoreMasterAccess [
        /// CF8 extended (offset ≥ 0x100) configuration access.
        EN_CF8_EXT_CFG OFFSET(0) NUMBITS(1) []
    ],
    /// F3x44 (32-bit, per instance): DF global control, watchdog.
    pub GlobalControl [
        WDT_BASE_SEL OFFSET(4) NUMBITS(2) [],
        WDT_CNT_SEL OFFSET(14) NUMBITS(3) []
    ],
    /// F4x180 (32-bit): SystemCfg.
    pub SystemConfig [
        OTHER_SOCKET_PRESENT OFFSET(8) NUMBITS(1) [],
        MY_SOCKET_ID OFFSET(16) NUMBITS(8) []
    ],
    /// F4x184 (32-bit): SystemComponentCnt.
    pub SystemComponents [
        DIES OFFSET(0) NUMBITS(8) []
    ],
    /// F4x1B4 (32-bit): SystemFabricIdMask0, the shifts of a fabric ID.
    pub FabricIdShifts [
        NODE_ID_SHIFT OFFSET(0) NUMBITS(4) [],
        SOCKET_ID_SHIFT OFFSET(8) NUMBITS(4) []
    ],
    /// F4x1B8 (32-bit): SystemFabricIdMask1, the socket ID mask.
    pub FabricIdMasks [
        SOCKET_ID_MASK OFFSET(16) NUMBITS(16) []
    ]
];

pub const CONFIG_LIMIT: DfReg<ConfigLimit::Register> = DfReg::new(0, 0xc84);
pub const IOS_BUS: DfReg<IosBus::Register> = DfReg::new(0, 0xc04);
pub const VGA: DfReg<Vga::Register> = DfReg::new(0, 0xc08);
pub const IO_LIMIT: DfReg<IoLimit::Register> = DfReg::new(0, 0xd04);
pub const MMIO_CONTROL: DfReg<MmioControl::Register> = DfReg::new(0, 0xd88);
pub const BLOCK_INSTANCE_COUNT: DfReg<BlockInstanceCount::Register> = DfReg::new(0, 0x40);
pub const INSTANCE_INFO: DfReg<InstanceInfo::Register> = DfReg::new(0, 0x44);
pub const INSTANCE_IDS: DfReg<InstanceIds::Register> = DfReg::new(0, 0x50);
pub const CORE_MASTER_ACCESS: DfReg<CoreMasterAccess::Register> = DfReg::new(0, 0xc00);
pub const GLOBAL_CONTROL: DfReg<GlobalControl::Register> = DfReg::new(3, 0x44);
/// F3x8F0/F3x8F4: PIE hardware-assert status, low and high.
pub const HW_ASSERT_STATUS: [DfReg; 2] = [DfReg::new(3, 0x8f0), DfReg::new(3, 0x8f4)];
/// F3x8F8/F3x8FC: PIE hardware-assert mask, low and high.
pub const HW_ASSERT_MASK: [DfReg; 2] = [DfReg::new(3, 0x8f8), DfReg::new(3, 0x8fc)];
pub const SYSTEM_CONFIG: DfReg<SystemConfig::Register> = DfReg::new(4, 0x180);
pub const SYSTEM_COMPONENTS: DfReg<SystemComponents::Register> = DfReg::new(4, 0x184);
pub const FABRIC_ID_SHIFTS: DfReg<FabricIdShifts::Register> = DfReg::new(4, 0x1b4);
pub const FABRIC_ID_MASKS: DfReg<FabricIdMasks::Register> = DfReg::new(4, 0x1b8);

/// Where the socket is in a fabric ID, as `F4x1B4`/`F4x1B8` say (Dell
/// `InstallPeiMemory` `0xd15de6`, 2P_COMPLETE §2.15; AMD's own code takes
/// bit [`SOCKET_SHIFT`] instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketIdLayout {
    /// `F4x1B4[3:0]`.
    pub node_shift: u8,
    /// `F4x1B4[11:8]`.
    pub socket_shift: u8,
    /// `F4x1B8[31:16]`.
    pub socket_mask: u16,
}

impl SocketIdLayout {
    /// The socket of an 8-bit fabric ID (a DRAM rule's destination):
    /// `(fid & (mask << node_shift) & 0xFF) >> socket_shift`.
    pub const fn socket_of(&self, fabric_id: u8) -> u8 {
        let mask = ((self.socket_mask as u32) << self.node_shift) & 0xff;
        ((fabric_id as u32 & mask) >> self.socket_shift) as u8
    }

    /// The layout puts the socket where [`fabric_id_socket`] reads it.
    pub const fn is_wh(&self) -> bool {
        self.socket_of(1 << SOCKET_SHIFT) == 1 && self.socket_of(!(1 << SOCKET_SHIFT)) == 0
    }
}

/// PCI device number on bus 0 for `(socket, die)`: `0x18 + (socket << 1) + die`.
pub const fn pci_device(socket: u8, die: u8) -> Option<u8> {
    match node_index(socket, die) {
        Some(node) => Some(0x18 + node),
        None => None,
    }
}

/// Linear DF node index `(socket << 1) | die` (`0..4`), which is also the
/// vendor's IOD index (`socket * 2 + die`) for SMN routing.
pub const fn node_index(socket: u8, die: u8) -> Option<u8> {
    if socket >= MAX_SOCKETS || die >= MAX_DIES {
        return None;
    }
    Some((socket << 1) | die)
}

/// PCI configuration address for DF node `(socket, die)` function `func`.
pub fn node_address(socket: u8, die: u8, func: u8) -> Option<PciAddress> {
    let dev = pci_device(socket, die)?;
    if func >= 8 {
        return None;
    }
    Some(PciAddress::new(0, 0, dev, func))
}

/// The fabric ID bits of `(socket, die)` (`DieFabricIdOffset`, `0xf2cfa0`).
pub const fn fabric_id_offset(socket: u8, die: u8) -> u8 {
    (socket << SOCKET_SHIFT) | (die << DIE_SHIFT)
}

/// The socket a fabric ID belongs to.
pub const fn fabric_id_socket(fabric_id: u16) -> u8 {
    ((fabric_id >> SOCKET_SHIFT) & 1) as u8
}

/// What the APOB (`0xC/0x22`) and the DF say of the system's sockets and
/// IODs (`0xf2cdf0`, `0xf53320`, `0xf53bb0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Topology {
    pub sockets: u8,
    pub dies_per_socket: u8,
}

impl Topology {
    /// From the APOB's CPU socket count and total CPU IOD (node) count,
    /// the latter falling back to `F4x184[7:0]` (`df_dies`). The vendor
    /// has no DF fallback for the socket count.
    pub const fn new(sockets: u8, nodes: Option<u8>, df_dies: u8) -> Option<Self> {
        let nodes = match nodes {
            Some(nodes) => nodes,
            None => df_dies,
        };
        if sockets == 0 || sockets > MAX_SOCKETS || nodes == 0 || nodes % sockets != 0 {
            return None;
        }
        let dies_per_socket = nodes / sockets;
        if dies_per_socket > MAX_DIES {
            return None;
        }
        Some(Self {
            sockets,
            dies_per_socket,
        })
    }

    /// Every `(socket, die)`, socket-major.
    pub fn nodes(self) -> impl Iterator<Item = (u8, u8)> {
        (0..self.sockets).flat_map(move |s| (0..self.dies_per_socket).map(move |d| (s, d)))
    }
}

/// One IOS of an IOD on socket 0 with its fabric ID (`F0x50[15:8]` of the
/// instance), from which every socket's root bridges are named: socket `s`
/// has the same IOS at fabric ID `+ s << 7` (`0xf2c4c0`, `0xf53ce0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ios {
    pub die: u8,
    /// Physical root bridge index: the IOS instance minus `Ios0`.
    pub rb: u8,
    /// Fabric ID on socket 0 (die bit included).
    pub fabric_id: u8,
}

/// Where a CfgAddressMap destination lands: `(socket, die, rb)`.
pub fn locate(ios: &[Ios], destination: u16) -> Option<(u8, u8, u8)> {
    if destination > 0xff {
        return None;
    }
    let socket = fabric_id_socket(destination);
    let on_socket0 = destination as u8 & !(1 << SOCKET_SHIFT);
    ios.iter()
        .find(|i| i.fabric_id == on_socket0)
        .map(|i| (socket, i.die, i.rb))
}

/// A hardware assert the PIE latched on an instance, as the vendor logs it
/// (status without the masked bits) before clearing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HwAssert {
    pub instance: u16,
    pub status: u64,
}

/// One DFv4.5 node (one IOD) accessed through PCI configuration space.
///
/// Intentionally neither `Copy` nor `Clone` so exclusive FICAA/FICAD ownership
/// cannot be aliased.
pub struct Node<P> {
    df: super::v4::Df<P>,
    socket: u8,
    die: u8,
}

impl<P: ConfigRegionAccess> Node<P> {
    /// Construct a handle for `(socket, die)` (`socket < 2`, `die < 2`).
    ///
    /// # Safety
    /// Caller owns exclusive DF indirect register (FICAA/FICAD) access for this node.
    pub unsafe fn new(pci: P, socket: u8, die: u8) -> Option<Self> {
        let node = node_index(socket, die)?;
        Some(Self {
            // SAFETY: caller owns exclusive FICAA/FICAD access on `node`.
            df: unsafe { super::v4::Df::new(pci, node) },
            socket,
            die,
        })
    }

    pub const fn socket(&self) -> u8 {
        self.socket
    }

    pub const fn die(&self) -> u8 {
        self.die
    }

    pub const fn device(&self) -> u8 {
        0x18 + (self.socket << 1) + self.die
    }

    /// Underlying DF4 register handle.
    pub const fn df(&self) -> &super::v4::Df<P> {
        &self.df
    }

    /// Direct read of a DF register on this node.
    pub fn read<R: tock_registers::RegisterLongName>(&self, reg: DfReg<R>) -> u32 {
        self.df.reg(reg).get()
    }

    /// Direct write of a DF register on this node.
    pub fn write<R: tock_registers::RegisterLongName>(&self, reg: DfReg<R>, value: u32) {
        self.df.reg(reg).set(value);
    }

    /// Instance-targeted read via DF4 FICAA (`F4x08C`) / FICAD_LO (`F4x0B8`).
    pub fn read_instance(&self, func: u8, offset: u16, instance: u8) -> u32 {
        self.df
            .instance(DfReg::<()>::new(func, offset), instance)
            .get()
    }

    /// `F4x180[8]`: another socket is present (a cross-check only; the
    /// vendor takes the socket count from the APOB).
    pub fn other_socket_present(&self) -> bool {
        self.df
            .reg(SYSTEM_CONFIG)
            .is_set(SystemConfig::OTHER_SOCKET_PRESENT)
    }

    /// `F4x184[7:0]`: the system's IODs, the vendor's fallback when the
    /// APOB has no node count.
    pub fn total_dies(&self) -> u8 {
        self.df.reg(SYSTEM_COMPONENTS).read(SystemComponents::DIES) as u8
    }

    /// `F4x1B4`/`F4x1B8`: where the socket is in a fabric ID.
    pub fn socket_id_layout(&self) -> SocketIdLayout {
        let shifts = self.df.reg(FABRIC_ID_SHIFTS).extract();
        let masks = self.df.reg(FABRIC_ID_MASKS).extract();
        SocketIdLayout {
            node_shift: shifts.read(FabricIdShifts::NODE_ID_SHIFT) as u8,
            socket_shift: shifts.read(FabricIdShifts::SOCKET_ID_SHIFT) as u8,
            socket_mask: masks.read(FabricIdMasks::SOCKET_ID_MASK) as u16,
        }
    }

    /// `F0x40[9:0]`: fabric block instances on this node.
    pub fn block_instances(&self) -> u16 {
        self.df
            .reg(BLOCK_INSTANCE_COUNT)
            .read(BlockInstanceCount::COUNT) as u16
    }

    /// The fabric ID of `instance` if it is enabled (`F0x44[6]`) and has a
    /// valid one (`F0x50[15:8]`, not `0xFF` or `0x55`; `0xf2c380`).
    pub fn instance_fabric_id(&self, instance: u8) -> Option<u8> {
        let info = self.df.instance(INSTANCE_INFO, instance).get();
        if info == u32::MAX || !InstanceInfo::ENABLED.is_set(info) {
            return None;
        }
        let ids = self.df.instance(INSTANCE_IDS, instance).get();
        let fabric_id = InstanceIds::FABRIC_ID.read(ids) as u8;
        (ids != u32::MAX && !INVALID_FABRIC_IDS.contains(&fabric_id)).then_some(fabric_id)
    }

    /// The enabled IOS of this IOD with their fabric IDs, physical order.
    pub fn ios(&self, devices: &Devices) -> Vec<Ios, 4> {
        devices
            .ios
            .instances()
            .zip(0..)
            .filter_map(|(instance, rb)| {
                Some(Ios {
                    die: self.die,
                    rb,
                    fabric_id: self.instance_fabric_id(instance)?,
                })
            })
            .take(4)
            .collect()
    }

    /// The IOMs of this IOD as the vendor's fabric topology map lists them
    /// (`WhBaseFabricTopology` `0xf2c750` → `WhProcessDeviceTypeInstances`
    /// `0xf2c630` → `0xf2c4c0`): of type `0xD` ([`Devices::iom`]) then
    /// `0x14` ([`Devices::iom_ext`]), each enabled with a valid fabric ID
    /// (both types check it), and a type `0xD` IOM not harvested: bit `i`
    /// of `harvest` (APOB `0xA/0x78`, the IOD's IOHC harvest mask) drops
    /// the `i`-th of the type (`WhIsInstanceHarvested 0xf2c410` [D], which
    /// treats IOS and IOM alike; type `0x14` is never harvested).
    pub fn ioms(&self, devices: &Devices, harvest: u32) -> Vec<u8, 8> {
        let iom = devices
            .iom
            .instances()
            .zip(0u32..)
            .filter(|&(_, i)| i >= 32 || harvest & 1 << i == 0);
        let ext = devices.iom_ext.instances().zip(32u32..);
        iom.chain(ext)
            .filter(|&(instance, _)| self.instance_fabric_id(instance).is_some())
            .map(|(instance, _)| instance)
            .take(8)
            .collect()
    }

    /// Read DFv4.5 56-bit unshifted DRAM rule `slot` (`0..32`) from `instance`.
    /// The legacy MMIO hole flag is `F7x210[1]`; [`Self::dram_hole_base`]
    /// says where it starts.
    pub fn dram_rule(&self, instance: u8, slot: usize) -> Option<DramRule> {
        if slot >= DRAM_RULES {
            return None;
        }
        let base_off = DRAM_RULE_BASE + (slot as u16) * DRAM_RULE_STRIDE;
        let ctl = self.read_instance(7, base_off + 0x10, instance);
        if ctl == u32::MAX || !DramRuleCtl::VALID.is_set(ctl) {
            return None;
        }
        let base_lo = u64::from(self.read_instance(7, base_off, instance));
        let base_hi = u64::from(DramAddressHi::ADDRESS.read(self.read_instance(
            7,
            base_off + 0x04,
            instance,
        )));
        let limit_lo = u64::from(self.read_instance(7, base_off + 0x08, instance));
        let limit_hi = u64::from(DramAddressHi::ADDRESS.read(self.read_instance(
            7,
            base_off + 0x0c,
            instance,
        )));
        let base = base_lo | (base_hi << 32);
        let limit_inclusive = limit_lo | (limit_hi << 32);
        let limit = limit_inclusive.checked_add(1)?;
        if limit <= base {
            return None;
        }
        Some(DramRule {
            base,
            end: limit,
            dst: DramRuleCtl::DESTINATION.read(ctl) as u16,
            hole: DramRuleCtl::LEGACY_HOLE.is_set(ctl),
            socket_interleave: false,
            die_interleave: 0,
            channel_interleave: 0,
        })
    }

    /// The raw DramAddressCtl (`F7x210 + 0x20 * slot`) of `instance`.
    pub fn dram_rule_ctl(&self, instance: u8, slot: usize) -> u32 {
        self.read_instance(
            7,
            DRAM_RULE_BASE + 0x10 + slot as u16 * DRAM_RULE_STRIDE,
            instance,
        )
    }

    /// `F7x214[1]` of rule `slot`: it targets a non-DRAM (CXL) set.
    pub fn dram_rule_non_dram(&self, instance: u8, slot: usize) -> bool {
        let raw = self.read_instance(
            7,
            DRAM_RULE_BASE + 0x14 + slot as u16 * DRAM_RULE_STRIDE,
            instance,
        );
        raw != u32::MAX && DramRuleTarget::NON_DRAM.is_set(raw)
    }

    /// The legacy MMIO hole's base, `F7x104[31:24] << 24` of `instance`, if
    /// the hole is enabled.
    pub fn dram_hole_base(&self, instance: u8) -> Option<u64> {
        let hole = self.df.instance(super::v4::DRAM_HOLE, instance).extract();
        (hole.get() != u32::MAX && hole.is_set(super::v4::DramHole::VALID))
            .then(|| u64::from(hole.read(super::v4::DramHole::BASE)) << 24)
    }

    /// Decode all valid CfgAddressMap pairs (`0..16`) on this node; the
    /// destination is 8 bits on WH.
    pub fn bus_routes(&self) -> Result<Vec<BusRoute, { CONFIG_PAIRS as usize }>, Error> {
        let mut routes = super::v4::bus_routes_n::<{ CONFIG_PAIRS as usize }, _>(&self.df)?;
        for route in routes.iter_mut() {
            route.destination &= 0xff;
        }
        Ok(routes)
    }

    /// Program CfgAddressMap pair `index` (`0..16`): `buses` of `segment` to
    /// `destination` (`SetCfgAddrMapRegWh`, `0xf3e310`).
    pub fn set_config_route(
        &self,
        index: u8,
        segment: u8,
        buses: BusRange,
        destination: u16,
    ) -> Option<()> {
        if index >= CONFIG_PAIRS || buses.base > buses.limit || destination > 0xff {
            return None;
        }
        let base = super::v4::CONFIG_BASE.at(index, super::v4::CONFIG_STRIDE);
        let limit = CONFIG_LIMIT.at(index, super::v4::CONFIG_STRIDE);
        self.df.reg(base).write(
            super::v4::ConfigBase::READ_ENABLE::SET
                + super::v4::ConfigBase::WRITE_ENABLE::SET
                + super::v4::ConfigBase::SEGMENT.val(segment.into())
                + super::v4::ConfigBase::BUS_BASE.val(u32::from(buses.base)),
        );
        self.df.reg(limit).write(
            ConfigLimit::DESTINATION.val(u32::from(destination))
                + ConfigLimit::BUS_LIMIT.val(u32::from(buses.limit)),
        );
        Some(())
    }

    /// Disable CfgAddressMap pair `index` (`0..16`): base and limit zero.
    pub fn clear_config_route(&self, index: u8) -> Option<()> {
        if index >= CONFIG_PAIRS {
            return None;
        }
        let base = super::v4::CONFIG_BASE.at(index, super::v4::CONFIG_STRIDE);
        self.df.reg(base).set(0);
        self.df
            .reg(CONFIG_LIMIT.at(index, super::v4::CONFIG_STRIDE))
            .set(0);
        Some(())
    }

    /// `F0xC04` of IOS `instance`: its secondary bus, which the vendor
    /// rewrites with the bus maps (`FabricRcInitWh`, `0xf3e930`).
    pub fn set_ios_bus(&self, instance: u8, segment: u8, bus: u8) {
        self.df
            .instance(IOS_BUS, instance)
            .write(IosBus::SEGMENT.val(segment.into()) + IosBus::BUS.val(bus.into()));
    }

    /// Program the MMIO and I/O rules of a plan in DFv4.5's 32 MMIO and 16
    /// I/O pairs in the vendor's order, and clear every other enabled rule.
    /// MMIO (`0xf458f0`): base, limit, the address\[55:48\] extension only
    /// when nonzero, then RE/WE/destination into the control register,
    /// keeping its CpuDis/NP. I/O (`0xf45fd0`): limit and destination, then
    /// the base with RE/WE.
    pub fn program(&self, mmio: &[MmioRoute], io: &[IoRoute]) -> Option<()> {
        use super::v4::{IO_BASE, IO_STRIDE, IoBase, MMIO_BASE, MMIO_EXTENSION, MMIO_LIMIT};
        use super::v4::{MMIO_STRIDE, MmioExtension};
        if mmio
            .iter()
            .any(|r| r.pair >= MMIO_PAIRS || r.fabric_id > 0xff)
            || io.iter().any(|r| r.pair >= IO_PAIRS || r.fabric_id > 0xff)
        {
            return None;
        }
        for pair in 0..MMIO_PAIRS {
            let control = self.df.reg(MMIO_CONTROL.at(pair, MMIO_STRIDE));
            match mmio.iter().find(|r| r.pair == pair) {
                Some(r) => {
                    let base = r.window.base.next_multiple_of(0x1_0000);
                    let limit = base + r.window.size - 1;
                    self.df
                        .reg(MMIO_BASE.at(pair, MMIO_STRIDE))
                        .set((base >> 16) as u32);
                    self.df
                        .reg(MMIO_LIMIT.at(pair, MMIO_STRIDE))
                        .set((limit >> 16) as u32);
                    let extension = MmioExtension::BASE.val((base >> 48) as u32 & 0xff)
                        + MmioExtension::LIMIT.val((limit >> 48) as u32 & 0xff);
                    let ext = self.df.reg(MMIO_EXTENSION.at(pair, MMIO_STRIDE));
                    if extension.value != 0 || ext.get() != 0 {
                        ext.write(extension);
                    }
                    control.modify(
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
            let base = self.df.reg(IO_BASE.at(pair, IO_STRIDE));
            match io.iter().find(|r| r.pair == pair) {
                Some(r) => {
                    self.df.reg(IO_LIMIT.at(pair, IO_STRIDE)).write(
                        IoLimit::DESTINATION.val(r.fabric_id.into())
                            + IoLimit::ADDRESS.val(((r.window.end() - 1) >> 12) as u32),
                    );
                    base.write(
                        IoBase::READ::SET
                            + IoBase::WRITE::SET
                            + IoBase::ADDRESS.val((r.window.base >> 12) as u32),
                    );
                }
                None if base.get() != 0 => base.set(0),
                None => {}
            }
        }
        Some(())
    }

    /// The enabled MMIO rules of this node as `(pair, base, inclusive limit)`.
    pub fn mmio_rules(&self) -> Vec<(u8, u64, u64), { MMIO_PAIRS as usize }> {
        use super::v4::{MMIO_BASE, MMIO_EXTENSION, MMIO_LIMIT, MMIO_STRIDE, MmioExtension};
        let mut rules = Vec::new();
        for pair in 0..MMIO_PAIRS {
            let control = self.df.reg(MMIO_CONTROL.at(pair, MMIO_STRIDE)).extract();
            if control.get() == u32::MAX
                || !(control.is_set(MmioControl::READ) || control.is_set(MmioControl::WRITE))
            {
                continue;
            }
            let ext = self.df.reg(MMIO_EXTENSION.at(pair, MMIO_STRIDE)).extract();
            let base = u64::from(self.df.reg(MMIO_BASE.at(pair, MMIO_STRIDE)).get()) << 16
                | u64::from(ext.read(MmioExtension::BASE)) << 48;
            let limit = (u64::from(self.df.reg(MMIO_LIMIT.at(pair, MMIO_STRIDE)).get()) << 16)
                | 0xffff
                | u64::from(ext.read(MmioExtension::LIMIT)) << 48;
            let _ = rules.push((pair, base, limit));
        }
        rules
    }

    /// The B0 MMIO-gap workaround (`RcMgrWhPatchMmioGaps`, `0xf1fd50`;
    /// GIGABYTE `0x7099412c`): the IOMs `ioms` (the vendor's are socket 0
    /// die 0's topology-map list for every node, [`Self::ioms`]) get this
    /// node's MMIO rules without gaps. Sorted by base, the first
    /// rule below 4 GiB starts at the DRAM hole (`F7x104[31:24] << 24`), the
    /// first above at `tom2`; each rule reaches the next one's base, and the
    /// last of each region the region's top (4 GiB, the 56-bit top). Only
    /// the IOM instances' copies of base, limit and extension are written;
    /// routing (control) is untouched.
    pub fn patch_mmio_gaps(&self, ioms: &[u8], tom2: u64) {
        use super::v4::{MMIO_BASE, MMIO_EXTENSION, MMIO_LIMIT, MMIO_STRIDE, MmioExtension};
        let mut rules = self.mmio_rules();
        rules.sort_unstable_by_key(|&(_, base, _)| base);
        let hole = self.df.reg(super::v4::DRAM_HOLE).extract();
        let hole = (hole.get() != u32::MAX && hole.is_set(super::v4::DramHole::VALID))
            .then(|| u64::from(hole.read(super::v4::DramHole::BASE)) << 24);
        let below = rules.iter().take_while(|r| r.2 < 1 << 32).count();
        let (low, high) = rules.split_at_mut(below);
        for (region, start, top) in [
            (low, hole, (1u64 << 32) - 1),
            (high, (tom2 >= 1 << 32).then_some(tom2), (1u64 << 56) - 1),
        ] {
            if let (Some(first), Some(start)) = (region.first_mut(), start) {
                first.1 = first.1.min(start & !0xffff);
            }
            for i in 0..region.len() {
                let next = region.get(i + 1).map(|next| next.1);
                region[i].2 = match next {
                    Some(next) => next.max(region[i].1 + 1) - 1,
                    None => top,
                };
            }
        }
        for &instance in ioms {
            for &(pair, base, limit) in &rules {
                let reg = |r: DfReg| self.df.instance(r.at(pair, MMIO_STRIDE), instance);
                reg(MMIO_BASE).set((base >> 16) as u32);
                reg(MMIO_LIMIT).set((limit >> 16) as u32);
                reg(MMIO_EXTENSION.erase()).set(
                    (MmioExtension::BASE.val((base >> 48) as u32 & 0xff)
                        + MmioExtension::LIMIT.val((limit >> 48) as u32 & 0xff))
                    .value,
                );
            }
        }
    }

    /// Route the legacy VGA window to `destination` (`FabricEnableVgaMmio`:
    /// `F0xC08 = (old & 0xFFFF_F00B) | 1 | dst << 4`). The vendor writes it
    /// on die 0 of each socket only.
    pub fn route_legacy_vga(&self, destination: u16) {
        self.df.reg(VGA).modify(
            Vga::ENABLE::SET
                + Vga::CPU_DISABLE::CLEAR
                + Vga::DESTINATION.val(u32::from(destination & 0xff)),
        );
    }

    /// Clear `F0xC00[0]` (CF8 extended configuration access), as
    /// `FabricReadyToBoot` does on every node.
    pub fn ready_to_boot(&self) {
        self.df
            .reg(CORE_MASTER_ACCESS)
            .modify(CoreMasterAccess::EN_CF8_EXT_CFG::CLEAR);
    }

    /// `Df4WhPieRasInit`, its per-node part (`0xf52fa0`): every instance
    /// below `F0x40[9:0]` whose PIE hardware-assert status
    /// (`F3x8F0`/`F3x8F4`) is nonzero is reported (without the bits
    /// `F3x8F8`/`F3x8FC` mask) and cleared.
    pub fn clear_hw_asserts<const N: usize>(&self) -> Vec<HwAssert, N> {
        let mut asserts = Vec::new();
        for instance in 0..self.block_instances().min(0x100) {
            let instance = instance as u8;
            let read = |reg: DfReg| self.df.instance(reg, instance).get();
            let status = [read(HW_ASSERT_STATUS[0]), read(HW_ASSERT_STATUS[1])];
            if status == [0, 0] || status.contains(&u32::MAX) {
                continue;
            }
            let mask = [read(HW_ASSERT_MASK[0]), read(HW_ASSERT_MASK[1])];
            let word = |w: [u32; 2]| u64::from(w[0]) | u64::from(w[1]) << 32;
            let _ = asserts.push(HwAssert {
                instance: instance.into(),
                status: word(status) & !word(mask),
            });
            for reg in HW_ASSERT_STATUS {
                self.df.instance(reg, instance).set(0);
            }
        }
        asserts
    }

    /// The DF watchdog's time base (`F3x44[5:4]`, `< 3`) on every instance
    /// of `devices`' PIE list; the vendor writes it on die 0 of each socket.
    pub fn set_watchdog_base(&self, devices: &Devices, base: u8) -> Option<()> {
        if base >= WDT_BASE_NS.len() as u8 {
            return None;
        }
        for &(first, last) in devices.pie {
            for instance in first..=last {
                self.df
                    .instance(GLOBAL_CONTROL, instance)
                    .modify(GlobalControl::WDT_BASE_SEL.val(base.into()));
            }
        }
        Some(())
    }
}

/// The DF watchdog: `F3x44[5:4]` time base (ns) and `F3x44[16:14]` count
/// (`0xfc5f48`, `0xfc5f68`).
pub const WDT_BASE_NS: [u64; 3] = [1_310_000, 1_280, 10];
pub const WDT_COUNT: [u32; 8] = [31, 127, 255, 511, 1023, 2047, 4095, 16383];

/// The DF watchdog timeout a `F3x44` value programs, in ns.
pub fn watchdog_ns(global_control: u32) -> Option<u64> {
    let reg = LocalRegisterCopy::<u32, GlobalControl::Register>::new(global_control);
    let base = WDT_BASE_NS.get(reg.read(GlobalControl::WDT_BASE_SEL) as usize)?;
    Some(base * u64::from(WDT_COUNT[reg.read(GlobalControl::WDT_CNT_SEL) as usize]))
}

/// Write a bus plan ([`buses::plan`]) as `FabricRcInitWh` does: every
/// CfgAddressMap pair of every node and `F0xC04` of every IOS instance
/// (`devices.ios`) of every node zeroed, then pair `k` of the plan written
/// to every node and `F0xC04` of its root bridge's IOS on its own node
/// set to its global first bus. `nodes` must be every live node.
pub fn program_buses<P: ConfigRegionAccess>(
    nodes: &[Node<P>],
    devices: &Devices,
    plan: &[buses::Assign],
) -> Option<()> {
    if plan
        .iter()
        .any(|a| a.rb >= devices.ios.count || a.pair >= CONFIG_PAIRS)
    {
        return None;
    }
    for node in nodes {
        for pair in 0..CONFIG_PAIRS {
            node.clear_config_route(pair)?;
        }
    }
    for node in nodes {
        for instance in devices.ios.instances() {
            node.set_ios_bus(instance, 0, 0);
        }
    }
    for assign in plan {
        for node in nodes {
            node.set_config_route(assign.pair, assign.segment, assign.buses, assign.fabric_id)?;
        }
        let owner = nodes
            .iter()
            .find(|n| (n.socket, n.die) == (assign.socket, assign.die))?;
        owner.set_ios_bus(
            devices.ios.first + assign.rb,
            assign.segment,
            assign.buses.base,
        );
    }
    Some(())
}

/// Broadcast a CfgAddressMap pair across all populated dual-IOD `nodes`.
pub fn broadcast_config_route<P: ConfigRegionAccess>(
    nodes: &[Node<P>],
    index: u8,
    segment: u8,
    buses: BusRange,
    destination: u16,
) -> Option<()> {
    if index >= CONFIG_PAIRS || buses.base > buses.limit || destination > 0xff {
        return None;
    }
    for node in nodes {
        node.set_config_route(index, segment, buses, destination)?;
    }
    Some(())
}

/// The channels a DRAM rule's interleave set spans on one IOD: rule copy
/// `ctl` (`F7x210`) is matched against the copies `cs[c][j]` CS instance
/// `c` holds (valid, same destination, same remap enable and select), and
/// every CS that has it sets channel `c / 2` (AmdFabricWhDxe `FUN_70e0`).
pub fn rule_channels(ctl: u32, cs: &[[u32; CS_DRAM_RULES]]) -> u16 {
    let key = |raw: u32| {
        let reg = LocalRegisterCopy::<u32, DramRuleCtl::Register>::new(raw);
        (
            reg.read(DramRuleCtl::DESTINATION),
            reg.read(DramRuleCtl::REMAP_EN),
            reg.read(DramRuleCtl::REMAP_SEL),
        )
    };
    if !DramRuleCtl::VALID.is_set(ctl) {
        return 0;
    }
    cs.iter()
        .enumerate()
        .filter(|(_, copies)| {
            copies
                .iter()
                .any(|&c| c != u32::MAX && DramRuleCtl::VALID.is_set(c) && key(c) == key(ctl))
        })
        .fold(0, |mask, (c, _)| mask | 1 << (c / 2))
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::df::plan::Window;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct MockPci {
        regs: RefCell<BTreeMap<(u8, u8, u16), u32>>,
        indirect: RefCell<BTreeMap<(u8, u8, u8, u16), u32>>,
        last_ficaa: RefCell<BTreeMap<u8, u32>>,
    }

    impl MockPci {
        fn reg(&self, dev: u8, func: u8, off: u16) -> u32 {
            *self.regs.borrow().get(&(dev, func, off)).unwrap_or(&0)
        }
        fn inst(&self, dev: u8, inst: u8, func: u8, off: u16) -> u32 {
            *self
                .indirect
                .borrow()
                .get(&(dev, inst, func, off))
                .unwrap_or(&0)
        }
    }

    impl ConfigRegionAccess for &MockPci {
        unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
            let dev = address.device();
            let func = address.function();
            if func == 4 && offset == super::super::v4::FICAD_LO {
                let ficaa = *self.last_ficaa.borrow().get(&dev).unwrap_or(&0);
                let f = super::super::v4::Ficaa::FUNCTION.read(ficaa) as u8;
                let r = (super::super::v4::Ficaa::REGISTER.read(ficaa) as u16) << 2;
                let inst = super::super::v4::Ficaa::INSTANCE.read(ficaa) as u8;
                return self.inst(dev, inst, f, r);
            }
            self.reg(dev, func, offset)
        }

        unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
            let dev = address.device();
            let func = address.function();
            if func == 4 && offset == super::super::v4::FICAA {
                self.last_ficaa.borrow_mut().insert(dev, value);
                return;
            }
            if func == 4 && offset == super::super::v4::FICAD_LO {
                let ficaa = *self.last_ficaa.borrow().get(&dev).unwrap_or(&0);
                let f = super::super::v4::Ficaa::FUNCTION.read(ficaa) as u8;
                let r = (super::super::v4::Ficaa::REGISTER.read(ficaa) as u16) << 2;
                let inst = super::super::v4::Ficaa::INSTANCE.read(ficaa) as u8;
                self.indirect.borrow_mut().insert((dev, inst, f, r), value);
                return;
            }
            self.regs.borrow_mut().insert((dev, func, offset), value);
        }
    }

    #[test]
    fn pci_devices_and_fabric_ids_map_dual_iod_sockets() {
        assert_eq!(pci_device(0, 0), Some(0x18));
        assert_eq!(pci_device(0, 1), Some(0x19));
        assert_eq!(pci_device(1, 0), Some(0x1a));
        assert_eq!(pci_device(1, 1), Some(0x1b));
        assert_eq!(pci_device(2, 0), None);
        assert_eq!(pci_device(0, 2), None);
        assert_eq!(fabric_id_offset(1, 1), 0xc0);
        assert_eq!(fabric_id_offset(0, 1), 0x40);
        assert_eq!(fabric_id_socket(0xa6), 1);
    }

    #[test]
    fn topology_comes_from_the_apob_with_the_df_die_fallback() {
        let t = Topology::new(2, Some(4), 0).unwrap();
        assert_eq!(t.dies_per_socket, 2);
        assert_eq!(
            t.nodes().collect::<std::vec::Vec<_>>(),
            [(0, 0), (0, 1), (1, 0), (1, 1)]
        );
        assert_eq!(Topology::new(1, None, 2).unwrap().dies_per_socket, 2);
        assert_eq!(Topology::new(0, Some(2), 2), None);
        assert_eq!(Topology::new(2, Some(3), 2), None);
        assert_eq!(Topology::new(1, Some(4), 2), None);
        assert_eq!(Topology::new(3, Some(6), 2), None);
    }

    #[test]
    fn ios_are_named_by_their_instance_fabric_ids() {
        let pci = MockPci::default();
        {
            let mut ind = pci.indirect.borrow_mut();
            // IOD1 of socket 0: IOS 0x26 and 0x28 enabled, 0x27 disabled.
            for (inst, fid, en) in [
                (0x26, 0x66u32, true),
                (0x27, 0x67, false),
                (0x28, 0x68, true),
            ] {
                ind.insert((0x19, inst, 0, 0x44), if en { 1 << 6 } else { 0 });
                ind.insert((0x19, inst, 0, 0x50), fid << 8 | 0x0e);
            }
            // A 0x55 fabric ID is not a valid one.
            ind.insert((0x18, 0x26, 0, 0x44), 1 << 6);
            ind.insert((0x18, 0x26, 0, 0x50), 0x55 << 8);
        }
        let node = unsafe { Node::new(&pci, 0, 1) }.unwrap();
        let ios = node.ios(&devices::B0);
        assert_eq!(
            ios.as_slice(),
            [
                Ios {
                    die: 1,
                    rb: 0,
                    fabric_id: 0x66
                },
                Ios {
                    die: 1,
                    rb: 2,
                    fabric_id: 0x68
                }
            ]
        );
        assert!(
            unsafe { Node::new(&pci, 0, 0) }
                .unwrap()
                .ios(&devices::B0)
                .is_empty()
        );
        assert_eq!(locate(&ios, 0xe8), Some((1, 1, 2)));
        assert_eq!(locate(&ios, 0x66), Some((0, 1, 0)));
        assert_eq!(locate(&ios, 0x67), None);
        assert_eq!(locate(&ios, 0x166), None);
    }

    #[test]
    fn harvested_ioms_are_left_out() {
        let pci = MockPci::default();
        {
            let mut ind = pci.indirect.borrow_mut();
            // B0 IOD0: IOMs 0x23..0x25 (type 0xD) and 0x22 (0x14) enabled,
            // 0x24 without a valid fabric ID.
            for (inst, fid) in [(0x22, 0x62u32), (0x23, 0x63), (0x24, 0xff), (0x25, 0x65)] {
                ind.insert((0x18, inst, 0, 0x44), 1 << 6);
                ind.insert((0x18, inst, 0, 0x50), fid << 8);
            }
        }
        let node = unsafe { Node::new(&pci, 0, 0) }.unwrap();
        assert_eq!(node.ioms(&devices::B0, 0).as_slice(), [0x23, 0x25, 0x22]);
        // Bit 2 harvests the third IOM of type 0xD (0x25), never the 0x14.
        assert_eq!(node.ioms(&devices::B0, 0b100).as_slice(), [0x23, 0x22]);
        assert_eq!(node.ioms(&devices::B0, !0).as_slice(), [0x22]);
    }

    #[test]
    fn decodes_56bit_unshifted_dram_rule_hole_and_masks_reserved_bits() {
        let pci = MockPci::default();
        let slot = 3usize;
        let off = DRAM_RULE_BASE + (slot as u16) * DRAM_RULE_STRIDE;
        let base: u64 = 0x0001_0000_0000;
        let limit_inc: u64 = 0x0002_ffff_ffff;
        let mut ind = pci.indirect.borrow_mut();
        ind.insert((0x19, DRAM_INSTANCE_PRIMARY, 7, off), base as u32);
        ind.insert(
            (0x19, DRAM_INSTANCE_PRIMARY, 7, off + 4),
            0xff00_0000 | (base >> 32) as u32,
        );
        ind.insert((0x19, DRAM_INSTANCE_PRIMARY, 7, off + 8), limit_inc as u32);
        ind.insert(
            (0x19, DRAM_INSTANCE_PRIMARY, 7, off + 0x0c),
            0xaa00_0000 | (limit_inc >> 32) as u32,
        );
        ind.insert(
            (0x19, DRAM_INSTANCE_PRIMARY, 7, off + 0x10),
            0xff00_0000
                | (DramRuleCtl::VALID::SET
                    + DramRuleCtl::LEGACY_HOLE::SET
                    + DramRuleCtl::DESTINATION.val(0x25))
                .value,
        );
        ind.insert((0x19, DRAM_INSTANCE_PRIMARY, 7, off + 0x14), 2);
        ind.insert((0x19, DRAM_INSTANCE_PRIMARY, 7, 0x104), 0x8000_0001);
        drop(ind);

        let node = unsafe { Node::new(&pci, 0, 1) }.unwrap();
        let rule = node.dram_rule(DRAM_INSTANCE_PRIMARY, slot).unwrap();
        assert_eq!(rule.base, 0x0001_0000_0000);
        assert_eq!(rule.end, 0x0003_0000_0000);
        assert!(rule.hole);
        assert_eq!(rule.dst, 0x25);
        assert!(node.dram_rule_non_dram(DRAM_INSTANCE_PRIMARY, slot));
        assert_eq!(
            node.dram_hole_base(DRAM_INSTANCE_PRIMARY),
            Some(0x8000_0000)
        );
        assert_eq!(node.dram_hole_base(DRAM_INSTANCE_SECONDARY), None);
    }

    #[test]
    fn broadcasts_and_reads_back_pair_15_across_nodes_with_segment() {
        let pci = MockPci::default();
        let nodes = [
            unsafe { Node::new(&pci, 0, 0) }.unwrap(),
            unsafe { Node::new(&pci, 0, 1) }.unwrap(),
            unsafe { Node::new(&pci, 1, 0) }.unwrap(),
            unsafe { Node::new(&pci, 1, 1) }.unwrap(),
        ];
        let range = BusRange {
            base: 0xe0,
            limit: 0xff,
        };
        broadcast_config_route(&nodes, 15, 0, range, 0xe6).unwrap();
        assert!(broadcast_config_route(&nodes, 16, 0, BusRange { base: 0, limit: 1 }, 0).is_none());
        assert!(broadcast_config_route(&nodes, 0, 0, BusRange { base: 5, limit: 4 }, 0).is_none());
        assert!(broadcast_config_route(&nodes, 0, 0, range, 0x100).is_none());

        for node in &nodes {
            let routes = node.bus_routes().unwrap();
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].buses, range);
            assert_eq!(routes[0].destination, 0xe6);
        }
        // A 12-bit field's upper bits do not leak into the WH destination.
        pci.regs.borrow_mut().insert((0x18, 0, 0xc80), 3);
        pci.regs.borrow_mut().insert((0x18, 0, 0xc84), 0x0000_0f26);
        assert_eq!(nodes[0].bus_routes().unwrap()[0].destination, 0x26);
        nodes[1].set_config_route(0, 1, range, 0x26).unwrap();
        assert_eq!(nodes[1].bus_routes().unwrap()[0].segment, 1);
    }

    #[test]
    fn programs_32_mmio_and_16_io_pairs_with_8bit_destinations() {
        let pci = MockPci::default();
        let node = unsafe { Node::new(&pci, 1, 0) }.unwrap();
        let dev = 0x1a;
        // Stale pairs beyond DF4's bounds, and a CpuDis the RMW keeps.
        pci.regs.borrow_mut().insert((dev, 0, 0xd88 + 31 * 16), 3);
        pci.regs.borrow_mut().insert((dev, 0, 0xd00 + 15 * 8), 3);
        pci.regs.borrow_mut().insert((dev, 0, 0xd88 + 20 * 16), 4);
        node.program(
            &[
                MmioRoute {
                    pair: 20,
                    window: Window::new(0x100_0000_0000, 0x40_0000_0000),
                    fabric_id: 0xa7,
                },
                MmioRoute {
                    pair: 3,
                    window: Window::new(0x1_0000_0000_0000, 0x1_0000_0000),
                    fabric_id: 0x26,
                },
            ],
            &[IoRoute {
                pair: 11,
                window: Window::new(0x4000, 0x1000),
                fabric_id: 0xe8,
            }],
        )
        .unwrap();
        let mmio = |pair: u16, off: u16| pci.reg(dev, 0, 0xd80 + pair * 16 + off);
        assert_eq!(mmio(20, 0), 0x0100_0000);
        assert_eq!(mmio(20, 4), 0x013f_ffff);
        assert_eq!(mmio(20, 8), 0xa7 << 16 | 7);
        // No extension write when it is zero.
        assert!(
            !pci.regs
                .borrow()
                .contains_key(&(dev, 0, 0xd80 + 20 * 16 + 0xc))
        );
        assert_eq!(mmio(3, 0xc), 0x0001_0001);
        assert_eq!(mmio(31, 8), 0);
        assert_eq!(pci.reg(dev, 0, 0xd00 + 11 * 8), 4 << 16 | 3);
        assert_eq!(pci.reg(dev, 0, 0xd04 + 11 * 8), 4 << 16 | 0xe8);
        assert_eq!(pci.reg(dev, 0, 0xd00 + 15 * 8), 0);
        // Out-of-range pairs and fabric IDs are refused.
        let bad = IoRoute {
            pair: 16,
            window: Window::new(0, 0x1000),
            fabric_id: 0,
        };
        assert!(node.program(&[], &[bad]).is_none());
    }

    #[test]
    fn vga_ready_to_boot_and_pie_follow_the_vendor() {
        let pci = MockPci::default();
        let node = unsafe { Node::new(&pci, 0, 0) }.unwrap();
        pci.regs.borrow_mut().insert((0x18, 0, 0xc08), 0xaaaa_5ff6);
        node.route_legacy_vga(0x126);
        assert_eq!(
            pci.reg(0x18, 0, 0xc08),
            (0xaaaa_5ff6 & 0xffff_f00b) | 1 | 0x26 << 4
        );
        pci.regs.borrow_mut().insert((0x18, 0, 0xc00), 0x13);
        node.ready_to_boot();
        assert_eq!(pci.reg(0x18, 0, 0xc00), 0x12);

        pci.regs.borrow_mut().insert((0x18, 0, 0x40), 0x30);
        {
            let mut ind = pci.indirect.borrow_mut();
            ind.insert((0x18, 5, 3, 0x8f0), 0x11);
            ind.insert((0x18, 5, 3, 0x8f8), 0x01);
            ind.insert((0x18, 0x2f, 3, 0x8f4), 2);
            // Beyond the block count: not looked at.
            ind.insert((0x18, 0x30, 3, 0x8f0), 1);
        }
        let asserts = node.clear_hw_asserts::<8>();
        assert_eq!(
            asserts.as_slice(),
            [
                HwAssert {
                    instance: 5,
                    status: 0x10
                },
                HwAssert {
                    instance: 0x2f,
                    status: 2 << 32
                }
            ]
        );
        assert_eq!(pci.inst(0x18, 5, 3, 0x8f0), 0);
        assert_eq!(pci.inst(0x18, 0x2f, 3, 0x8f4), 0);
        assert_eq!(pci.inst(0x18, 0x30, 3, 0x8f0), 1);

        pci.indirect
            .borrow_mut()
            .insert((0x18, 0x32, 3, 0x44), 0x1c000);
        node.set_watchdog_base(&devices::B0, 1).unwrap();
        assert_eq!(pci.inst(0x18, 0x32, 3, 0x44), 0x1c010);
        assert_eq!(pci.inst(0x18, 0x80, 3, 0x44), 0x10);
        assert_eq!(pci.inst(0x18, 0x81, 3, 0x44), 0);
        assert!(node.set_watchdog_base(&devices::B0, 3).is_none());
        assert_eq!(watchdog_ns(0x1c010), Some(1_280 * 16383));
    }

    #[test]
    fn b0_mmio_gaps_are_closed_on_the_ioms_only() {
        let pci = MockPci::default();
        let node = unsafe { Node::new(&pci, 0, 1) }.unwrap();
        let (dev, routes) = (
            0x19,
            [
                MmioRoute {
                    pair: 0,
                    window: Window::new(0xc000_0000, 0x1000_0000),
                    fabric_id: 0x26,
                },
                MmioRoute {
                    pair: 2,
                    window: Window::new(0xf000_0000, 0x0e00_0000),
                    fabric_id: 0x27,
                },
                MmioRoute {
                    pair: 1,
                    window: Window::new(0x200_0000_0000, 0x10_0000_0000),
                    fabric_id: 0x26,
                },
                MmioRoute {
                    pair: 3,
                    window: Window::new(0x300_0000_0000, 0x10_0000_0000),
                    fabric_id: 0x27,
                },
            ],
        );
        node.program(&routes, &[]).unwrap();
        pci.regs.borrow_mut().insert((dev, 7, 0x104), 0x8000_0003);
        let ioms = [0x23, 0x24, 0x25, 0x22];
        node.patch_mmio_gaps(&ioms, 0x180_0000_0000);
        let get = |inst: u8, pair: u16, off: u16| pci.inst(dev, inst, 0, 0xd80 + pair * 16 + off);
        for inst in [0x22, 0x23, 0x25] {
            // Below 4 GiB: from the hole, abutting, up to 4 GiB.
            assert_eq!((get(inst, 0, 0), get(inst, 0, 4)), (0x8000, 0xefff));
            assert_eq!((get(inst, 2, 0), get(inst, 2, 4)), (0xf000, 0xffff));
            // Above: from TOM2, abutting, up to 2^56.
            assert_eq!(
                (get(inst, 1, 0), get(inst, 1, 4)),
                (0x0180_0000, 0x02ff_ffff)
            );
            assert_eq!(
                (get(inst, 3, 0), get(inst, 3, 4), get(inst, 3, 0xc)),
                (0x0300_0000, 0xffff_ffff, 0x00ff_0000)
            );
        }
        // Not the IOS, nor the node's own rules.
        assert_eq!(get(0x26, 0, 0), 0);
        assert_eq!(pci.reg(dev, 0, 0xd80), 0xc000);
    }

    #[test]
    fn rule_channels_match_cs_copies() {
        let ctl = |dst: u32, sel: u32| {
            (DramRuleCtl::VALID::SET
                + DramRuleCtl::REMAP_SEL.val(sel)
                + DramRuleCtl::DESTINATION.val(dst))
            .value
        };
        let mut cs = [[0u32; CS_DRAM_RULES]; 16];
        // A 4-channel interleave (CS 0..8) and a one-channel rule on CS 9.
        for c in cs.iter_mut().take(8) {
            c[0] = ctl(0x00, 0);
        }
        cs[9][11] = ctl(0x09, 0);
        cs[10][1] = ctl(0x00, 1);
        assert_eq!(rule_channels(ctl(0x00, 0), &cs), 0b1111);
        assert_eq!(rule_channels(ctl(0x09, 0), &cs), 0b1_0000);
        assert_eq!(rule_channels(ctl(0x00, 1), &cs), 0b10_0000);
        assert_eq!(rule_channels(0, &cs), 0);
    }

    /// A bus plan reaches every node: pairs zeroed then written everywhere,
    /// `F0xC04` of each root bridge's IOS on its own node only.
    #[test]
    fn bus_plans_are_written_to_every_node() {
        let pci = MockPci::default();
        // A stale pair 5 on every node and stale F0xC04s.
        for dev in 0x18..0x1c {
            pci.regs
                .borrow_mut()
                .insert((dev, 0, 0xc80 + 5 * 8), 0x0003);
            pci.indirect
                .borrow_mut()
                .insert((dev, 0x27, 0, 0xc04), 0x55);
        }
        let nodes: std::vec::Vec<_> = [(0, 0), (0, 1), (1, 0), (1, 1)]
            .into_iter()
            .map(|(s, d)| unsafe { Node::new(&pci, s, d) }.unwrap())
            .collect();
        let rbs: std::vec::Vec<buses::Request> = nodes
            .iter()
            .flat_map(|n| {
                (0..3).map(move |rb| buses::Request {
                    socket: n.socket(),
                    die: n.die(),
                    rb,
                    fabric_id: u16::from(fabric_id_offset(n.socket(), n.die()))
                        | (0x26 + u16::from(rb)),
                    live: None,
                    target: 40,
                })
            })
            .collect();
        let plan = buses::plan(&rbs, 0, 512).unwrap().unwrap();
        assert!(program_buses(&nodes, &devices::B0, &plan).is_some());
        for dev in 0x18..0x1c {
            // Every pair of the plan, on every node.
            for a in &plan {
                let off = 0xc80 + u16::from(a.pair) * 8;
                let base = pci.reg(dev, 0, off);
                assert_eq!(
                    base,
                    3 | u32::from(a.segment) << 8 | u32::from(a.buses.base) << 16
                );
                assert_eq!(
                    pci.reg(dev, 0, off + 4),
                    u32::from(a.fabric_id) | u32::from(a.buses.limit) << 16
                );
            }
            assert_eq!(pci.reg(dev, 0, 0xc80 + 13 * 8), 0);
        }
        // S1D1 rb 1 (IOS 0x27) on node 0x1b only; the stale value elsewhere
        // is cleared.
        let a = plan
            .iter()
            .find(|a| (a.socket, a.die, a.rb) == (1, 1, 1))
            .unwrap();
        assert_eq!(pci.inst(0x1b, 0x27, 0, 0xc04), u32::from(a.global_base()));
        assert_eq!(
            pci.inst(0x1a, 0x27, 0, 0xc04),
            u32::from(plan[7].global_base())
        );
        assert_eq!(pci.inst(0x18, 0x26, 0, 0xc04), 0);
        // A plan naming a fourth IOS on B0 is refused.
        let mut bad = plan.clone();
        bad[1].rb = 3;
        assert!(program_buses(&nodes, &devices::B0, &bad).is_none());
    }

    #[test]
    fn socket_id_layout_from_f4x1b4_and_1b8() {
        let pci = MockPci::default();
        pci.regs.borrow_mut().insert((0x18, 4, 0x1b4), 7 << 8);
        pci.regs.borrow_mut().insert((0x18, 4, 0x1b8), 1 << 23);
        let node = unsafe { Node::new(&pci, 0, 0) }.unwrap();
        let layout = node.socket_id_layout();
        assert_eq!(
            layout,
            SocketIdLayout {
                node_shift: 0,
                socket_shift: 7,
                socket_mask: 0x80
            }
        );
        assert!(layout.is_wh());
        assert_eq!((layout.socket_of(0xa6), layout.socket_of(0x66)), (1, 0));
        // A layout with the socket elsewhere is not WH's.
        let other = SocketIdLayout {
            node_shift: 0,
            socket_shift: 6,
            socket_mask: 0x40,
        };
        assert!(!other.is_wh());
    }
}
