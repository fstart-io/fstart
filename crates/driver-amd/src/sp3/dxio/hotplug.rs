//! PCIe hotplug: slot descriptors, the SMU hotplug tables and the
//! per-generation SMU message sets (PCIE_DXIO §3.10).
//!
//! The SMU firmware runs hotplug from a table in DRAM: one map, function
//! and reset dword per slot. Boards supply the I²C expander wiring
//! ([`Hotplug`]); the topology fields of the map dword come from the port
//! mapping ([`super::ports::MappedPort`]), as AGESA patches them.
//!
//! - Rome/Milan (V11, `SmuV11Dxe` 871349b8 `0x3FE8`, `SmuV11DxeGN` 526f13b1
//!   `0x3EFC`): 0x480 bytes, 96 × 3 dwords (same layout as Oxide's
//!   `smu_hotplug_table_t`).
//! - Naples (V9, `AmdNbioSmuV9Dxe` 5ad83885, sha256 55c4714747ae, decoded
//!   here from `0x3770..0x41f5`, instruction level): 0x400 bytes, 64 × 4
//!   dwords, other map bit positions.

use tock_registers::LocalRegisterCopy;
use tock_registers::register_bitfields;
use zerocopy::little_endian::U32;
use zerocopy::{FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout};

use super::lanes::Engine;
use super::ports::MappedPort;
use crate::sp3::smu::{SmuError, SmuMessages, send_addr, send1};
use crate::sp3::soc::Generation;

/// PCIe hotplug type (board LinkHotplug; wire caps bits 10:8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HotplugKind {
    Basic = 1,
    ExpressModule = 2,
    Enhanced = 3,
    /// Inboard: bridge shown only when trained.
    Inboard = 4,
    EnterpriseSsd = 5,
}

impl HotplugKind {
    /// The wire value: Naples has no EntSSD type and sends ExpressModule
    /// (sp4r2 `ffde833a`, `5 → 2`).
    pub const fn wire(self, generation: Generation) -> u8 {
        match (generation, self) {
            (Generation::Naples, Self::EnterpriseSsd) => Self::ExpressModule as u8,
            _ => self as u8,
        }
    }

    /// Bridges of these types stay visible even when untrained (§3.8).
    pub const fn keeps_bridge(self) -> bool {
        !matches!(self, Self::Inboard)
    }
}

/// I²C expander types (Oxide: PCA9539-style 9535 and 9506).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Expander {
    Pca9539 = 0,
    Pca9535 = 1,
    Pca9506 = 2,
}

/// The slot's presence/attention pins on an I²C GPIO expander.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotplugFunction {
    /// Bit within the byte (I/O block of 8 pins).
    pub bit: u8,
    pub byte: u8,
    /// 7-bit address low bits (5 bits; the expander base is fixed).
    pub address: u8,
    pub expander: Expander,
    /// Downstream port of the I²C switch.
    pub bus: u8,
    /// Pins of the block not used by this slot.
    pub mask: u8,
}

/// The slot's reset (PERST) pin on an expander.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotplugReset {
    pub byte: u8,
    pub address: u8,
    pub expander: Expander,
    pub bus: u8,
    pub mask: u8,
}

/// A hotplug slot on a PCIe engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotplug {
    pub kind: HotplugKind,
    /// Slot number; the table index is `slot − HotplugPolicy::slot_base`.
    pub slot: u8,
    /// Map dword format bits 2:0 (SMU slot-signal format, board data).
    pub format: u8,
    pub function: HotplugFunction,
    pub reset: Option<HotplugReset>,
}

impl Hotplug {
    /// A slot without I²C wiring yet (tests, placeholders).
    pub const fn new(kind: HotplugKind, slot: u8) -> Self {
        Self {
            kind,
            slot,
            format: 0,
            function: HotplugFunction {
                bit: 0,
                byte: 0,
                address: 0,
                expander: Expander::Pca9539,
                bus: 0,
                mask: 0,
            },
            reset: None,
        }
    }
}

register_bitfields![u32,
    /// V11 map dword. DIE/PORT/TILE/BRIDGE from the donor patch code
    /// (PCIE_DXIO §3.10); FORMAT/RST_VALID/ACTIVE are Oxide field names.
    pub MAP [
        FORMAT OFFSET(0) NUMBITS(3) [],
        RST_VALID OFFSET(5) NUMBITS(1) [],
        ACTIVE OFFSET(6) NUMBITS(1) [],
        DIE OFFSET(8) NUMBITS(1) [],
        PORT OFFSET(9) NUMBITS(3) [],
        /// `core << 2 | nbio`.
        TILE OFFSET(12) NUMBITS(3) [],
        BRIDGE OFFSET(15) NUMBITS(5) []
    ],
    /// Naples V9 map dword patch fields (`0x3b28..0x3b6c`).
    pub MAP_V9 [
        FORMAT OFFSET(0) NUMBITS(3) [],
        RST_VALID OFFSET(5) NUMBITS(1) [],
        ACTIVE OFFSET(6) NUMBITS(1) [],
        /// Wrapper byte `+0xD` (inferred: the PCIe core).
        TILE OFFSET(8) NUMBITS(4) [],
        /// Port `+0x2B` bits 6:4.
        PORT OFFSET(12) NUMBITS(3) [],
        /// Silicon `+0xA` (die).
        DIE OFFSET(15) NUMBITS(1) []
    ],
    pub FUNCTION [
        I2C_BIT OFFSET(0) NUMBITS(3) [],
        I2C_BYTE OFFSET(3) NUMBITS(3) [],
        I2C_ADDR OFFSET(6) NUMBITS(5) [],
        I2C_TYPE OFFSET(11) NUMBITS(2) [],
        I2C_BUS OFFSET(13) NUMBITS(5) [],
        MASK OFFSET(18) NUMBITS(8) [],
        /// V9 only: port `+0x31` logical bridge (`0x3b6f`).
        V9_BRIDGE OFFSET(24) NUMBITS(5) []
    ],
    pub RESET [
        I2C_BYTE OFFSET(3) NUMBITS(3) [],
        I2C_ADDR OFFSET(6) NUMBITS(5) [],
        I2C_TYPE OFFSET(11) NUMBITS(2) [],
        I2C_BUS OFFSET(13) NUMBITS(5) [],
        MASK OFFSET(18) NUMBITS(8) []
    ]
];

/// Slots in the V11 table.
pub const V11_SLOTS: usize = 96;
/// Slots in the V9 table.
pub const V9_SLOTS: usize = 64;

/// The Rome/Milan SMU hotplug table.
#[derive(Clone, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct HotplugTableV11 {
    pub map: [U32; V11_SLOTS],
    pub function: [U32; V11_SLOTS],
    pub reset: [U32; V11_SLOTS],
}

const _: () = assert!(size_of::<HotplugTableV11>() == 0x480);

/// The Naples SMU hotplug table (the fourth array is board `+0x14`, meaning
/// unknown, sent as 0).
#[derive(Clone, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct HotplugTableV9 {
    pub map: [U32; V9_SLOTS],
    pub function: [U32; V9_SLOTS],
    pub reset: [U32; V9_SLOTS],
    pub extra: [U32; V9_SLOTS],
}

const _: () = assert!(size_of::<HotplugTableV9>() == 0x400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotplugError {
    /// Slot number below the base or past the table.
    Slot(u8),
    /// Two engines claim one slot.
    DuplicateSlot(u8),
    /// A hotplug engine did not map to a port.
    Unmapped(usize),
    Smu(SmuError),
}

impl From<SmuError> for HotplugError {
    fn from(e: SmuError) -> Self {
        Self::Smu(e)
    }
}

/// Hotplug platform policy. AGESA takes these from DXE PCDs that were not
/// decoded (PCIE_DXIO §6); `Default` is all off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HotplugPolicy {
    /// I²C switch base address (Rome PCD 0x27B, Naples 0x184).
    pub i2c_switch_base: u8,
    /// First slot number (Rome 0x27C, Naples 0x185); also the low nibble
    /// of the enable message.
    pub slot_base: u8,
    /// Rome 0x277 / Naples 0x180: enable bit 4; Naples also sends the poll
    /// interval.
    pub poll: bool,
    /// Naples PCD32 0x181.
    pub poll_interval: u32,
    /// Naples PCD 0x182: send message 0x46.
    pub naples_gpio_polling: bool,
    /// V11 0x1D: 1 (RAS 0x500A6), 4 (RAS 0x500A1 == 0), 8 (0x500A0),
    /// Milan 0x80 (0x500AE). Naples 0x45: PCD 0x1F..0x23 bits 0..4.
    pub debug_flags: u32,
    /// RAS 0x500A1 == 3 (firmware-first): enable bit 5.
    pub firmware_first: bool,
    /// RAS 0x500A7 == 1: enable bit 6.
    pub enable_bit6: bool,
}

impl HotplugPolicy {
    /// The V11 0x18 / V9 0x14 argument.
    pub const fn enable_arg(&self) -> u32 {
        (self.slot_base & 0xF) as u32
            | (self.poll as u32) << 4
            | (self.firmware_first as u32) << 5
            | (self.enable_bit6 as u32) << 6
    }
}

fn function_dword(f: &HotplugFunction) -> LocalRegisterCopy<u32, FUNCTION::Register> {
    let mut r = LocalRegisterCopy::new(0);
    r.modify(
        FUNCTION::I2C_BIT.val(f.bit as u32)
            + FUNCTION::I2C_BYTE.val(f.byte as u32)
            + FUNCTION::I2C_ADDR.val(f.address as u32)
            + FUNCTION::I2C_TYPE.val(f.expander as u32)
            + FUNCTION::I2C_BUS.val(f.bus as u32)
            + FUNCTION::MASK.val(f.mask as u32),
    );
    r
}

fn reset_dword(r: &Option<HotplugReset>) -> u32 {
    r.map_or(0, |r| {
        let mut d = LocalRegisterCopy::<u32, RESET::Register>::new(0);
        d.modify(
            RESET::I2C_BYTE.val(r.byte as u32)
                + RESET::I2C_ADDR.val(r.address as u32)
                + RESET::I2C_TYPE.val(r.expander as u32)
                + RESET::I2C_BUS.val(r.bus as u32)
                + RESET::MASK.val(r.mask as u32),
        );
        d.get()
    })
}

/// Hotplug engines with their mapped port and table index.
fn slots<'e>(
    engines: &'e [Engine<'_>],
    ports: &'e [MappedPort],
    slot_base: u8,
    capacity: usize,
) -> impl Iterator<Item = Result<(usize, &'e Hotplug, &'e MappedPort), HotplugError>> + 'e {
    engines
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            e.pcie_config()
                .and_then(|p| p.hotplug.as_ref())
                .map(|h| (i, h))
        })
        .map(move |(i, h)| {
            let port = ports
                .iter()
                .find(|p| p.engine == i)
                .ok_or(HotplugError::Unmapped(i))?;
            let index = h
                .slot
                .checked_sub(slot_base)
                .map(usize::from)
                .filter(|&s| s < capacity)
                .ok_or(HotplugError::Slot(h.slot))?;
            Ok((index, h, port))
        })
}

impl HotplugTableV11 {
    /// Build the table from the board engines and the port map
    /// (`SmuV11Dxe 0x3FE8`). `None` when no engine has hotplug.
    pub fn build(
        engines: &[Engine<'_>],
        ports: &[MappedPort],
        slot_base: u8,
    ) -> Result<Option<Self>, HotplugError> {
        let mut t = Self::new_zeroed();
        let mut any = false;
        for s in slots(engines, ports, slot_base, V11_SLOTS) {
            let (i, h, p) = s?;
            if t.map[i].get() != 0 {
                return Err(HotplugError::DuplicateSlot(h.slot));
            }
            let mut map = LocalRegisterCopy::<u32, MAP::Register>::new(0);
            map.modify(
                MAP::FORMAT.val(h.format as u32)
                    + MAP::RST_VALID.val(h.reset.is_some() as u32)
                    + MAP::ACTIVE::SET
                    + MAP::DIE.val(p.socket as u32)
                    + MAP::PORT.val(p.port as u32)
                    + MAP::TILE.val((p.core as u32) << 2 | p.nbio as u32)
                    + MAP::BRIDGE.val(p.bridge as u32),
            );
            t.map[i].set(map.get());
            t.function[i].set(function_dword(&h.function).get());
            t.reset[i].set(reset_dword(&h.reset));
            any = true;
        }
        Ok(any.then_some(t))
    }
}

impl HotplugTableV9 {
    /// Build the Naples table (`0x3891..0x3b8a`).
    pub fn build(
        engines: &[Engine<'_>],
        ports: &[MappedPort],
        slot_base: u8,
    ) -> Result<Option<Self>, HotplugError> {
        let mut t = Self::new_zeroed();
        let mut any = false;
        for s in slots(engines, ports, slot_base, V9_SLOTS) {
            let (i, h, p) = s?;
            if t.map[i].get() != 0 {
                return Err(HotplugError::DuplicateSlot(h.slot));
            }
            let mut map = LocalRegisterCopy::<u32, MAP_V9::Register>::new(0);
            map.modify(
                MAP_V9::FORMAT.val(h.format as u32)
                    + MAP_V9::RST_VALID.val(h.reset.is_some() as u32)
                    + MAP_V9::ACTIVE::SET
                    + MAP_V9::TILE.val(p.core as u32)
                    + MAP_V9::PORT.val(p.port as u32)
                    + MAP_V9::DIE.val(p.nbio as u32 & 1),
            );
            let mut f = function_dword(&h.function);
            f.modify(FUNCTION::V9_BRIDGE.val(p.bridge as u32));
            t.map[i].set(map.get());
            t.function[i].set(f.get());
            t.reset[i].set(reset_dword(&h.reset));
            any = true;
        }
        Ok(any.then_some(t))
    }
}

/// SMU hotplug message IDs. Local constants where the SMU module's names
/// disagree with the donor (Naples: see [`v9`]).
pub mod msg {
    /// V11 (Rome, Milan).
    pub mod v11 {
        pub const SET_I2C_SWITCH_BASE: u32 = 0x1A;
        pub const ROME_SET_HOTPLUG_TABLE_ADDR: u32 = 0x3C;
        pub const MILAN_SET_HOTPLUG_TABLE_ADDR: u32 = 0x47;
        pub const TRANSFER_HOTPLUG_TABLE: u32 = 0x12;
        pub const SET_HOTPLUG_DEBUG_FLAGS: u32 = 0x1D;
        pub const ENABLE_HOTPLUG: u32 = 0x18;
    }
    /// V9 (Naples). The failure strings and arguments in `SmuV9Dxe`
    /// `0x3bc5..0x41c0` show 0x14 is EnableHotPlug (argument shaped like
    /// V11 0x18) and 0x3E the poll interval (argument PCD32 0x181); the
    /// SMU module names them the other way round.
    pub mod v9 {
        pub const SET_I2C_SWITCH_BASE: u32 = 0x17;
        pub const SET_BIOS_DRAM_ADDR_LO: u32 = 0x1E;
        pub const SET_BIOS_DRAM_ADDR_HI: u32 = 0x1D;
        /// Argument 1 selects the hotplug table (0 is the policy table).
        pub const TRANSFER_TABLE: u32 = 0x22;
        /// Per die, argument = flags.
        pub const SET_HOTPLUG_FLAGS: u32 = 0x45;
        pub const ENABLE_GPIO_POLLING: u32 = 0x46;
        pub const SET_POLL_INTERVAL: u32 = 0x3E;
        pub const ENABLE_HOTPLUG: u32 = 0x14;
    }
}

/// `addr + n | n << 8` for the four switch ports, to the first SMU.
fn i2c_switch<M: SmuMessages + ?Sized>(smu: &M, id: u32, base: u8) -> Result<(), SmuError> {
    (0..4u32).try_for_each(|n| send1(smu, id, ((base as u32 + n) & 0xFF) | n << 8).map(|_| ()))
}

/// Rome/Milan, first half (`0x3FE8`): I²C switch base to socket 0, then
/// the table address and transfer to every socket. The caller then runs
/// the per-port hotplug setup ([`super::ports`]) and the settle wait, and
/// calls [`enable_v11`]. `sockets[0]` is socket 0.
pub fn load_v11<M: SmuMessages + ?Sized>(
    generation: Generation,
    sockets: &[&M],
    table_phys: u64,
    policy: &HotplugPolicy,
) -> Result<(), SmuError> {
    use msg::v11::*;
    let first = sockets.first().ok_or(SmuError::Status(0))?;
    i2c_switch(*first, SET_I2C_SWITCH_BASE, policy.i2c_switch_base)?;
    let addr_msg = match generation {
        Generation::Milan => MILAN_SET_HOTPLUG_TABLE_ADDR,
        _ => ROME_SET_HOTPLUG_TABLE_ADDR,
    };
    sockets.iter().try_for_each(|smu| {
        send_addr(*smu, addr_msg, table_phys)?;
        send1(*smu, TRANSFER_HOTPLUG_TABLE, 0).map(|_| ())
    })
}

/// Rome/Milan, second half: debug flags and enable, socket 0 only.
pub fn enable_v11<M: SmuMessages + ?Sized>(
    socket0: &M,
    policy: &HotplugPolicy,
) -> Result<(), SmuError> {
    use msg::v11::*;
    send1(socket0, SET_HOTPLUG_DEBUG_FLAGS, policy.debug_flags)?;
    send1(socket0, ENABLE_HOTPLUG, policy.enable_arg()).map(|_| ())
}

/// Naples, first half: switch base, table address and transfer to the
/// first die.
pub fn load_v9<M: SmuMessages + ?Sized>(
    die0: &M,
    table_phys: u64,
    policy: &HotplugPolicy,
) -> Result<(), SmuError> {
    use msg::v9::*;
    i2c_switch(die0, SET_I2C_SWITCH_BASE, policy.i2c_switch_base)?;
    send1(die0, SET_BIOS_DRAM_ADDR_LO, table_phys as u32)?;
    send1(die0, SET_BIOS_DRAM_ADDR_HI, (table_phys >> 32) as u32)?;
    send1(die0, TRANSFER_TABLE, 1).map(|_| ())
}

/// Naples, second half: flags to every die, then polling and enable to the
/// first. `dies[0]` is the first die.
pub fn enable_v9<M: SmuMessages + ?Sized>(
    dies: &[&M],
    policy: &HotplugPolicy,
) -> Result<(), SmuError> {
    use msg::v9::*;
    let first = dies.first().ok_or(SmuError::Status(0))?;
    dies.iter()
        .try_for_each(|smu| send1(*smu, SET_HOTPLUG_FLAGS, policy.debug_flags).map(|_| ()))?;
    if policy.naples_gpio_polling {
        send1(*first, ENABLE_GPIO_POLLING, 0)?;
    }
    if policy.poll {
        send1(*first, SET_POLL_INTERVAL, policy.poll_interval)?;
    }
    send1(*first, ENABLE_HOTPLUG, policy.enable_arg()).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::dxio::lanes::{Group, Lanes, PcieEngine};
    use crate::sp3::dxio::tests::FakeSmu;

    fn engines() -> [Engine<'static>; 2] {
        let hp = Hotplug {
            format: 1,
            function: HotplugFunction {
                bit: 2,
                byte: 1,
                address: 3,
                expander: Expander::Pca9535,
                bus: 4,
                mask: 0xF0,
            },
            reset: Some(HotplugReset {
                byte: 1,
                address: 3,
                expander: Expander::Pca9535,
                bus: 4,
                mask: 0x0F,
            }),
            ..Hotplug::new(HotplugKind::EnterpriseSsd, 12)
        };
        [
            Engine::pcie(
                0,
                Lanes::sp3(Group::P1, 0, 4),
                PcieEngine::new().with_hotplug(hp),
            ),
            Engine::pcie(1, Lanes::sp3(Group::P2, 4, 4), PcieEngine::new()),
        ]
    }

    fn port(engine: usize) -> MappedPort {
        MappedPort {
            engine,
            socket: 1,
            nbio: 3,
            core: 1,
            port: 2,
            devfn: 0x1B,
            bridge: 10,
            trained: false,
        }
    }

    #[test]
    fn v11_table() {
        let t = HotplugTableV11::build(&engines(), &[port(0)], 10)
            .unwrap()
            .unwrap();
        let map = t.map[2].get();
        assert_eq!(
            map,
            1 | 1 << 5 | 1 << 6 | 1 << 8 | 2 << 9 | 0b111 << 12 | 10 << 15
        );
        assert_eq!(
            t.function[2].get(),
            2 | 1 << 3 | 3 << 6 | 1 << 11 | 4 << 13 | 0xF0 << 18
        );
        assert_eq!(
            t.reset[2].get(),
            1 << 3 | 3 << 6 | 1 << 11 | 4 << 13 | 0x0F << 18
        );
        assert!(
            t.map
                .iter()
                .enumerate()
                .all(|(i, m)| i == 2 || m.get() == 0)
        );
        assert_eq!(
            HotplugTableV11::build(&engines(), &[port(0)], 13).err(),
            Some(HotplugError::Slot(12))
        );
        assert_eq!(
            HotplugTableV11::build(&engines(), &[], 0).err(),
            Some(HotplugError::Unmapped(0))
        );
        assert!(
            HotplugTableV11::build(&engines()[1..], &[], 0)
                .unwrap()
                .is_none()
        );

        let v9 = HotplugTableV9::build(&engines(), &[port(0)], 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            v9.map[12].get(),
            1 | 1 << 5 | 1 << 6 | 1 << 8 | 2 << 12 | 1 << 15
        );
        assert_eq!(v9.function[12].get() >> 24, 10);
    }

    #[test]
    fn message_sets() {
        let policy = HotplugPolicy {
            i2c_switch_base: 0x70,
            slot_base: 2,
            poll: true,
            poll_interval: 500,
            debug_flags: 4,
            ..Default::default()
        };
        let (s0, s1) = (FakeSmu::default(), FakeSmu::default());
        load_v11(Generation::Milan, &[&s0, &s1], 0x1_2345_6000, &policy).unwrap();
        enable_v11(&s0, &policy).unwrap();
        assert_eq!(
            s0.sent(),
            [
                (0x1A, 0x70),
                (0x1A, 0x171),
                (0x1A, 0x272),
                (0x1A, 0x373),
                (0x47, 0x2345_6000),
                (0x12, 0),
                (0x1D, 4),
                (0x18, 0x12),
            ]
        );
        assert_eq!(s0.args()[4][1], 1, "address high in arg 1");
        assert_eq!(s1.sent(), [(0x47, 0x2345_6000), (0x12, 0)]);

        let rome = FakeSmu::default();
        load_v11(Generation::Rome, &[&rome], 0x1000, &policy).unwrap();
        assert_eq!(rome.sent()[4], (0x3C, 0x1000));

        let (d0, d1) = (FakeSmu::default(), FakeSmu::default());
        load_v9(&d0, 0x1_0000_2000, &policy).unwrap();
        enable_v9(&[&d0, &d1], &policy).unwrap();
        assert_eq!(
            d0.sent()[4..],
            [
                (0x1E, 0x2000),
                (0x1D, 1),
                (0x22, 1),
                (0x45, 4),
                (0x3E, 500),
                (0x14, 0x12)
            ]
        );
        assert_eq!(d1.sent(), [(0x45, 4)]);
    }
}
