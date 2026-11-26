// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH PCIe hotplug (MPIO.md §2.10), decompiler-inferred (Dell):
//! - the root-port setup by hotplug type before training
//!   (`PcieHotplugInitWh`, `SilVenicePei` `0xf622b0`): [`init`];
//! - MPIO's hotplug service, which the vendor's DXE starts
//!   (`ServerHotplugInitV14Dxe`, `AmdNbioDxe` `0xcf80`): the I²C expander
//!   addresses (request `0x16`), the 0x780-byte slot table (`0x13`, every
//!   MPIO instance), the flags (`0x19`) and the enable (`0x14`):
//!   [`Table`], [`expander_addresses`], [`Policy`].
//!
//! The board describes each hotplug slot's expander wiring ([`Slot`], the
//! vendor's "legacy hotplug descriptor", 0x18 bytes); the port behind it
//! carries the hotplug type and the slot number ([`super::port::PortDesc`]).

use tock_registers::{LocalRegisterCopy, register_bitfields};

use super::port::{Hotplug, I2cSwitch};
use super::straps::{Emit, Full, Op, Ops, PortSettings};
use crate::nbio::iohc::{self, IohcBridgeCntl};
use crate::nbio::wh::RB_SMN_STRIDE;
use crate::nbio::wh::pcie::config::{self, DeviceControl, SlotCapabilities, SlotControl};
use crate::nbio::wh::pcie::{self, Access, HotplugSlot, PresenceDetect};
use crate::smn::SmnReg;

register_bitfields![u32,
    /// IOHC `0x13B2_00F4` (32-bit), unnamed: both bits are also the IOHC's
    /// RAS SMI enable (`FchSocSmmApuRasSmiEnable`,
    /// `crate::nbio::wh::RootBridge::enable_ras_smi`).
    pub IohcHotplug [
        /// Set on every hotplug port.
        HOTPLUG OFFSET(14) NUMBITS(1) [],
        /// Set with SFI.
        SFI OFFSET(16) NUMBITS(1) []
    ]
];

/// IOHC `0x13B2_00F4` ([`IohcHotplug`]).
pub const IOHC_HOTPLUG: SmnReg<IohcHotplug::Register> = SmnReg::new(0x13b2_00f4);
/// IOHC SFI_CAM (`0x13B3_1014 + bridge * 0x400`, 32-bit; the SFI
/// capability of a root port).
pub const IOHC_SFI_CAM: SmnReg = SmnReg::new(0x13b3_1014);

/// Entries per array of the slot table.
pub const SLOTS: usize = 160;
/// Size of the slot table (request `0x13` argument 2).
pub const TABLE_BYTES: usize = 3 * SLOTS * 4;

register_bitfields![u32,
    /// Slot table engine dword (`AmdNbioDxe` `0xcf80`).
    pub Engine [
        /// Expander GPIO type (board value).
        GPIO_TYPE OFFSET(0) NUMBITS(3) [],
        /// Socket (`GnbHandle+0x0A`).
        MASTER_SLAVE OFFSET(5) NUMBITS(1) [],
        /// IOD.
        DIE OFFSET(6) NUMBITS(1) [],
        /// Core-local port.
        PORT OFFSET(7) NUMBITS(4) [],
        /// PCIe core (tile).
        TILE OFFSET(11) NUMBITS(4) [],
        LOGICAL_BRIDGE OFFSET(15) NUMBITS(5) [],
        /// I²C bus segment bits 6:5.
        SEGMENT_EXT OFFSET(22) NUMBITS(2) [],
        /// Legacy switch 0 is valid.
        SEGMENT2_VALID OFFSET(31) NUMBITS(1) []
    ],
    /// Slot table function dword.
    pub Function [
        /// I²C bus segment bits 4:0.
        SEGMENT OFFSET(13) NUMBITS(5) [],
        /// Bits 18 and up of the board's function dword: the expander's I²C
        /// address (the board value).
        ADDRESS OFFSET(18) NUMBITS(8) [],
        /// Legacy switch 0 (`address << 3 | select`, valid bit 7).
        MUX OFFSET(26) NUMBITS(6) []
    ]
];

/// A board hotplug slot: the expander pins of the port on `socket` lanes
/// `start_lane..=end_lane` (the vendor's legacy hotplug descriptor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub socket: u8,
    pub start_lane: u8,
    pub end_lane: u8,
    /// Table index base plus this is the port's slot number (descriptor `+0x07`).
    pub slot_num: u8,
    /// An ExpressModule slot (flags bit 0); otherwise enterprise SSD, U.2 or the like.
    pub express_module: bool,
    /// The board's engine dword (expander GPIO type in bits 2:0; the rest is
    /// overwritten from the port).
    pub engine: u32,
    /// The board's function dword (expander address and pins).
    pub function: u32,
    /// The board's reset dword.
    pub reset: u32,
}

/// The port a [`Slot`] is on, after training.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotPort {
    pub socket: u8,
    pub die: u8,
    pub core: u8,
    pub port: u8,
    pub logical_bridge: u8,
    pub hotplug: Hotplug,
    /// The port's legacy I²C switches.
    pub legacy_i2c: [Option<I2cSwitch>; 2],
}

/// The I²C segment base and switch count of legacy switch 1 by address
/// (`AmdNbioDxe` `0x52d10`).
const SEGMENTS: [(u8, u8); 4] = [(4, 0x11), (4, 0x15), (4, 0x19), (4, 0x1c)];

const fn switch_byte(s: Option<I2cSwitch>) -> u8 {
    match s {
        Some(s) => 0x80 | (s.address & 7) << 3 | (s.select & 7),
        None => 0,
    }
}

/// Errors while building the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Slot number below the base or past the table.
    SlotNumber(u8),
    /// The slot's type does not match its port's hotplug type.
    Type,
    /// Legacy switch 1 is invalid (the vendor skips its segment).
    Switch,
}

/// The slot table of request `0x13`: engine, function and reset dwords of
/// every slot, indexed by slot number minus the base (PCD `0x3D0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    words: [u32; 3 * SLOTS],
    entries: usize,
    /// Some slot uses legacy switch 1: four more expander addresses.
    pub second_switch: bool,
}

impl Default for Table {
    fn default() -> Self {
        Self {
            words: [0; 3 * SLOTS],
            entries: 0,
            second_switch: false,
        }
    }
}

impl Table {
    /// Whether a slot of type `express_module` with board GPIO type `gpio`
    /// may serve a port of `hotplug` type (the vendor's match, `0xcf80`).
    pub const fn matches(express_module: bool, gpio: u32, hotplug: Hotplug) -> bool {
        if matches!(hotplug, Hotplug::Disabled) {
            return false;
        }
        if express_module {
            matches!(hotplug, Hotplug::ExpressModule)
        } else {
            !matches!(gpio & 7, 1 | 3) || matches!(hotplug, Hotplug::EnterpriseSsd)
        }
    }

    /// Add `slot`, behind `port`, with slot numbers based at `base`.
    pub fn add(&mut self, slot: &Slot, port: &SlotPort, base: u8) -> Result<(), Error> {
        if !Self::matches(slot.express_module, slot.engine, port.hotplug) {
            return Err(Error::Type);
        }
        let index = slot
            .slot_num
            .checked_sub(base)
            .map(usize::from)
            .filter(|&i| i < SLOTS)
            .ok_or(Error::SlotNumber(slot.slot_num))?;
        let mux0 = switch_byte(port.legacy_i2c[0]);
        let mut engine = LocalRegisterCopy::<u32, Engine::Register>::new(slot.engine);
        engine.modify(
            Engine::TILE.val(port.core.into())
                + Engine::PORT.val(port.port.into())
                + Engine::MASTER_SLAVE.val(u32::from(port.socket & 1))
                + Engine::DIE.val(u32::from(port.die & 1))
                + Engine::LOGICAL_BRIDGE.val(port.logical_bridge.into())
                + Engine::SEGMENT2_VALID.val(u32::from(mux0 >> 7)),
        );
        let mut function = LocalRegisterCopy::<u32, Function::Register>::new(slot.function);
        function.modify(Function::MUX.val(mux0.into()));
        if let Some(s) = port.legacy_i2c[1] {
            let (addr, select) = (s.address & 7, s.select & 7);
            // As decompiled: below address 4 the select must reach the
            // count (bit 2 set); its low bits pick the channel.
            let segment = match SEGMENTS.get(usize::from(addr)) {
                Some(&(count, _)) if select < count => return Err(Error::Switch),
                Some(&(_, first)) => first + (select & 3),
                None => return Err(Error::Switch),
            };
            function.modify(Function::SEGMENT.val(u32::from(segment & 0x1f)));
            engine.modify(Engine::SEGMENT_EXT.val(u32::from(segment >> 5 & 3)));
            self.second_switch = true;
        }
        self.words[index] = engine.get();
        self.words[SLOTS + index] = function.get();
        self.words[2 * SLOTS + index] = slot.reset;
        self.entries += 1;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.entries == 0
    }

    /// The table as MPIO reads it.
    pub fn bytes(&self) -> [u8; TABLE_BYTES] {
        let mut out = [0; TABLE_BYTES];
        for (chunk, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.words) {
            chunk.copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}

/// The I²C expander addresses of request `0x16`, in order: address
/// `first + i` with index `i` in bits 15:8, four, or eight with legacy
/// switch 1 (`AmdNbioDxe` `0xcf80`).
pub fn expander_addresses(first: u8, second_switch: bool) -> impl Iterator<Item = u32> {
    let n = if second_switch { 8u32 } else { 4 };
    (0..n).map(move |i| (u32::from(first) + i) & 0xff | i << 8)
}

/// MPIO hotplug service policy (the vendor's PCDs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// First expander I²C address (PCD `0x3CF`).
    pub i2c_address: u8,
    /// Slot number of table entry 0 (PCD `0x3D0`).
    pub slot_base: u8,
    /// Request `0x19` argument (sent if non-zero; the vendor's default has
    /// bit 2 set).
    pub flags: u32,
    /// Request `0x14` argument bits other than the slot base (`[3:0]`)
    /// and SFI mode (bit 7).
    pub enable: u32,
    /// `AmdHotPlugHandlingMode` 5, 6 or 7.
    pub sfi: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            i2c_address: 0,
            slot_base: 0,
            flags: 4,
            enable: 0,
            sfi: false,
        }
    }
}

impl Policy {
    /// Request `0x14`'s argument.
    pub const fn enable_argument(&self) -> u32 {
        (self.enable & !0x8f) | (self.slot_base as u32 & 0xf) | (self.sfi as u32) << 7
    }
}

/// The root-port platform values `init` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitPolicy {
    /// `AmdHotPlugHandlingMode` 5, 6 or 7 (SFI).
    pub sfi: bool,
    /// `AmdPresenceDetectSelectMode` (`None`: `0xFF`).
    pub presence_detect_select: Option<u8>,
    /// `AmdHotPlugPDSettle` (`SilDataWh+0x05`).
    pub pd_settle: bool,
    /// `AmdAllowCompliance` (`SilDataWh+0x0C` non-zero).
    pub allow_compliance: bool,
    /// The ExpressModule port-reset PCD (`SilDataWh+0x06 == 1`).
    pub express_module_reset: bool,
    /// `SilData+0x17`: Slot Control bit 14.
    pub slot_control_b14: bool,
    /// `AmdHotPlugNvmeDefaultMaxPayload` (`None`: `0xFF`).
    pub nvme_max_payload: Option<u8>,
    /// SP7 A0 (`MST_PORT_CTRL_2` at `0x1A34_0684`, else `0x06C4`).
    pub a0: bool,
}

/// `PcieHotplugInitWh` (Dell `0xf622b0`) for a hotplug port: `iohc` its
/// root bridge's IOHC, `logical_bridge` its bridge, `endpoint` whether
/// Link Status reports the data link up (`0x1C40_0068` bit 29), `npem` the
/// port's NPEM enable and capability. Types 1, 3 and 4 get the common part
/// only (the vendor asserts).
pub fn init(
    s: &PortSettings,
    policy: &InitPolicy,
    iohc: u8,
    logical_bridge: u8,
    endpoint: bool,
    npem: Option<(u16, u16)>,
) -> Result<Ops, Full> {
    let mut e = Emit::new(s.id(), policy.a0);
    // The IOHC's registers: `iohc << 20` (the bridge's at `(iohc & 3) <<
    // 20` as the vendor masks it), host SMN.
    let iohc_reg = |reg: SmnReg| reg.offset(u32::from(iohc) * RB_SMN_STRIDE);
    let bridge_shift = u32::from(iohc & 3) * RB_SMN_STRIDE;
    let smn = |reg: SmnReg, clear, set| Op::rmw(reg, Access::Smn, clear, set);
    let cntl = iohc::iohc_bridge_cntl(logical_bridge.into()).offset(bridge_shift);
    let sfi_cam = IOHC_SFI_CAM.offset(bridge_shift + u32::from(logical_bridge) * 0x400);
    let hp = s.hotplug;
    if hp == Hotplug::Disabled {
        return e.finish();
    }
    let sfi_port = policy.sfi && hp != Hotplug::ExpressModule;
    e.push(Op::config(
        config::SLOT_CAPABILITIES,
        SlotCapabilities::HOTPLUG_CAPABLE::SET,
    ));
    if let Some(mode) = policy.presence_detect_select {
        e.field(
            pcie::PRESENCE_DETECT,
            PresenceDetect::SELECT_MODE.val(u32::from(mode & 3)),
        );
    }
    if sfi_port {
        e.rmw(pcie::PORT_368, !0xfffa_ff7a, 0x40000);
    } else if policy.pd_settle {
        e.rmw(pcie::PORT_368, !0xfffa_ff7a, 0x50080);
    }
    e.rmw(pcie::CORE_100, 3 << 23, 3 << 23);
    if sfi_port {
        if hp != Hotplug::Ocp {
            let sfi = IohcHotplug::SFI::SET.value;
            e.push(smn(iohc_reg(SmnReg::new(IOHC_HOTPLUG.address())), sfi, sfi));
        }
        e.push(Op::config16(
            config::SLOT_CONTROL,
            SlotControl::PRESENCE_DETECT_CHANGED_EN::SET,
        ));
    }
    let hotplug = IohcHotplug::HOTPLUG::SET.value;
    e.push(smn(
        iohc_reg(SmnReg::new(IOHC_HOTPLUG.address())),
        hotplug,
        hotplug,
    ));
    let compliance = if policy.allow_compliance {
        0xe0_0000
    } else {
        0
    };
    e.rmw(pcie::LC_40C, 0xf8_0000, compliance);
    e.rmw(pcie::LC_300, 1 << 12, 1 << 12);
    let port_bit = 1u32 << s.port;
    match hp {
        Hotplug::ExpressModule => {
            let cap = if policy.express_module_reset {
                0x2003a
            } else {
                0x2001b
            };
            e.push(Op::Config {
                offset: config::SLOT_CAPABILITIES,
                clear: !0xfff9_ffc0,
                set: cap,
                word: false,
            });
            e.rmw(pcie::PORT_040, 1 << 4, 1 << 4);
            e.rmw(pcie::CORE_438, port_bit, 0);
            if !endpoint {
                e.push(Op::rmw(
                    cntl,
                    Access::Smn,
                    IohcBridgeCntl::BRIDGE_DIS::SET.mask(),
                    0,
                ));
            }
        }
        Hotplug::EnterpriseSsd | Hotplug::Ubm | Hotplug::Ocp => {
            if hp == Hotplug::Ubm
                && let Some((enable, cap)) = npem
            {
                e.rmw(pcie::NPEM_CAPABILITY, u32::MAX, u32::from(cap & 0xfff));
                e.rmw(pcie::NPEM_CONTROL, u32::MAX, u32::from(enable & 0xfff));
            }
            if matches!(hp, Hotplug::Ubm | Hotplug::Ocp) {
                // The physical slot number into the port's hotplug control.
                e.rmw(
                    pcie::HOTPLUG_SLOT,
                    u32::MAX,
                    (HotplugSlot::SLOT.val(s.slot_num.into()) + HotplugSlot::VALID::SET).value,
                );
            }
            e.push(Op::Config {
                offset: config::SLOT_CAPABILITIES,
                clear: !0xfff9_ffc0,
                set: 0x40020,
                word: false,
            });
            e.push(Op::Config {
                offset: config::SLOT_CAPABILITIES2,
                clear: 1,
                set: 1,
                word: false,
            });
            e.push(Op::config16(
                config::SLOT_CONTROL,
                SlotControl::BIT14.val(policy.slot_control_b14.into()),
            ));
            e.rmw(pcie::CORE_438, port_bit, 0);
            e.rmw(pcie::PORT_040, 1 << 4, 1 << 4);
            e.rmw(pcie::PORT_1A0, 0xff00_0000, 0x2000_0000);
            if policy.sfi {
                e.push(smn(sfi_cam, !0xffff_000e, 0x5540));
                let mst = if policy.a0 {
                    pcie::MST_PORT_CTRL_2_A0
                } else {
                    pcie::MST_PORT_CTRL_2
                };
                e.rmw(mst, !0xfffe_fc00, 0x155);
            }
            let bridge_dis = IohcBridgeCntl::BRIDGE_DIS::SET.mask();
            if !endpoint {
                if !policy.sfi {
                    e.push(Op::rmw(cntl, Access::Smn, 7, bridge_dis));
                }
                if let Some(mps) = policy.nvme_max_payload {
                    e.push(Op::config16(
                        config::DEVICE_CONTROL,
                        DeviceControl::MAX_PAYLOAD.val(u16::from(mps & 7)),
                    ));
                }
            } else if policy.sfi {
                e.push(Op::Config {
                    offset: config::SFI_54C,
                    clear: 0x14,
                    set: 0x14,
                    word: true,
                });
            } else {
                e.push(Op::rmw(cntl, Access::Smn, 7, 0));
            }
        }
        _ => {}
    }
    e.finish()
}

/// Waits about 100 ms the way the vendor's DXE does between the slot table
/// and the hotplug enable (`ServerHotplugInitV14Dxe`, AmdNbioDxe Dell
/// `0xe1e3..0xe2de`): `count` reads the free-running counter of the first
/// PCIe core's RC aperture ([`pcie::CORE_COUNTER`], through the proxy)
/// until it advanced by 100 000, at most 1000 reads.
pub fn settle<E>(mut count: impl FnMut() -> Result<u32, E>) -> Result<(), E> {
    const TICKS: u32 = 100_000;
    const READS: u32 = 1000;
    let start = count()?;
    for _ in 0..READS {
        if count()?.wrapping_sub(start) >= TICKS {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxio::wh::port::PortDesc;
    use crate::dxio::wh::straps;

    fn port(hotplug: Hotplug) -> SlotPort {
        SlotPort {
            socket: 1,
            die: 1,
            core: 2,
            port: 3,
            logical_bridge: 11,
            hotplug,
            legacy_i2c: [
                Some(I2cSwitch {
                    address: 2,
                    select: 1,
                }),
                Some(I2cSwitch {
                    address: 1,
                    select: 6,
                }),
            ],
        }
    }

    #[test]
    fn slot_table_dwords_carry_the_port() {
        let slot = Slot {
            socket: 1,
            start_lane: 0,
            end_lane: 3,
            slot_num: 0x12,
            express_module: false,
            engine: 2,
            function: 0x20 << 18,
            reset: 0x55,
        };
        let mut t = Table::default();
        t.add(&slot, &port(Hotplug::EnterpriseSsd), 0x10).unwrap();
        let w = |i: usize| u32::from_le_bytes(t.bytes()[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!(
            w(2),
            2 | 1 << 5 | 1 << 6 | 3 << 7 | 2 << 11 | 11 << 15 | 1 << 31
        );
        // Switch 1 at address 1, select 6: segment 0x15 + 2.
        assert_eq!(w(SLOTS + 2), 0x20 << 18 | 0x17 << 13 | 0x11 << 26);
        assert_eq!(w(2 * SLOTS + 2), 0x55);
        assert!(t.second_switch);
        assert_eq!(
            expander_addresses(0x20, true).collect::<std::vec::Vec<_>>(),
            [0x20, 0x121, 0x222, 0x323, 0x424, 0x525, 0x626, 0x727]
        );
        // An ExpressModule slot serves only ExpressModule ports.
        let em = Slot {
            express_module: true,
            ..slot
        };
        assert_eq!(
            t.add(&em, &port(Hotplug::EnterpriseSsd), 0x10),
            Err(Error::Type)
        );
        assert_eq!(
            t.add(&slot, &port(Hotplug::EnterpriseSsd), 0x13),
            Err(Error::SlotNumber(0x12))
        );
        assert_eq!(
            Policy {
                slot_base: 0x12,
                sfi: true,
                ..Policy::default()
            }
            .enable_argument(),
            0x82
        );
    }

    #[test]
    fn enterprise_ssd_root_port_setup() {
        let desc = PortDesc::pcie(0, 3).slot(Hotplug::EnterpriseSsd, 7);
        let s = straps::PortSettings::new(&desc, 1, 2, 3, &straps::Policy::default(), false);
        let policy = InitPolicy {
            sfi: false,
            presence_detect_select: None,
            pd_settle: true,
            allow_compliance: false,
            express_module_reset: false,
            slot_control_b14: false,
            nvme_max_payload: Some(1),
            a0: false,
        };
        let ops = init(&s, &policy, 3, 9, false, None).unwrap();
        assert!(ops.contains(&Op::Config {
            offset: 0x6c,
            clear: 0x0006_003f,
            set: 0x40020,
            word: false
        }));
        assert!(ops.contains(&Op::Smn {
            address: 0x13b3_1004 + 0x30_0000 + 9 * 0x400,
            clear: 7,
            set: 1
        }));
        assert!(ops.contains(&Op::Proxy {
            address: 0x1a34_0368 + 0x20_0000 + 0x1000,
            clear: 0x0005_0085,
            set: 0x50080
        }));
        assert!(
            init(
                &straps::PortSettings::new(
                    &PortDesc::pcie(0, 3),
                    1,
                    2,
                    3,
                    &straps::Policy::default(),
                    false
                ),
                &policy,
                3,
                9,
                false,
                None
            )
            .unwrap()
            .is_empty()
        );
    }
}
