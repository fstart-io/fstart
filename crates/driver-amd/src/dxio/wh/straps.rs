// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-port host programming of a WH PCIe port around training: the straps
//! MPIO writes for the host (request `0x0C`) and the private registers it
//! read-modify-writes (request `0x26`), as the vendor runs them before the
//! reconfiguration (`BeforeReconfigPerPortProgramming`, Dell `0xf7e540`),
//! after it (`AfterReconfigPerPortProgramming`, `0xf82470`) and after
//! training (`AfterDxioInitPerPortProgramming`, `0xf83670`); MPIO.md §2.9,
//! `perport_programming_dell.json`. Decompiler-inferred (Dell).
//!
//! The vendor reads its values from its own port structure (`PciePort`),
//! filled with silicon defaults (`PciePortConfigDefaultsSil` `0xf6a280`,
//! `PciePortPlatformConfigDefault` `0xf37460`) and then the board's
//! descriptor (Dell `0xf77cc0`). [`PortSettings`] is that structure for
//! the fields these steps read: those a [`PortDesc`] names are taken from
//! it, the rest keep the defaults (named by their `PciePort` offset where
//! their meaning is unknown). [`Policy`] holds the platform's input-block
//! values the steps read.
//!
//! Of the core-level steps (`MpioCfgCoreBeforeBifurcationWh`, Dell
//! `0xf7be90`; `MpioCfgCoreAfterBifurcationWh`) only the AER strap is here
//! ([`core_before_reconfig`]): the others' inputs are wrapper flags whose
//! meaning is not recovered. Also left out: the fixed-coefficient
//! equalization paths whose gates come from unnamed descriptor fields (off
//! by default), the requester ID (NBIO,
//! [`crate::nbio::wh::pcie::set_requester`]), and the SRIS path's LC
//! register rewrite (`0x1C44_02EC`).
//!
//! The registers are [`crate::nbio::wh::pcie`]'s; each step takes the
//! vendor's access path for its register ([`pcie::Reg::access`]).

use heapless::Vec;
use tock_registers::RegisterLongName;
use tock_registers::fields::FieldValue;

use super::port::{Hotplug, PortDesc};
use crate::nbio::wh::pcie::config::SlotCapabilities;
use crate::nbio::wh::pcie::{
    self, Access, DeviceControl2, EqControl, FlitDrain, FlitErrorCounter, LinkControl2,
    LinkControlStatus, PcieStrapF0, PortId, SubsystemId,
};
use crate::smn::SmnReg;

/// Strap index of each strap ID on parts other than SP7 A0
/// (`WhStrapIndexBase`, Dell `SilVenicePei` `0xfcf0d0`, 128 × u16).
pub const STRAP_INDEX: [u16; 128] = [
    0x13b, 0x009, 0x140, 0x075, 0x076, 0x077, 0x078, 0x079, //
    0x07a, 0x07b, 0x07c, 0x07d, 0x07e, 0x0fe, 0x0ff, 0x100, //
    0x144, 0x0e2, 0x0fd, 0x0c8, 0x148, 0x147, 0x14b, 0x149, //
    0x008, 0x151, 0x062, 0x150, 0x0d5, 0x04b, 0x0db, 0x01d, //
    0x0dc, 0x0d6, 0x0d7, 0x0d8, 0x0d9, 0x0da, 0x101, 0x0d0, //
    0x00e, 0x010, 0x011, 0x137, 0x138, 0x139, 0x0d1, 0x160, //
    0x165, 0x158, 0x152, 0x162, 0x0fc, 0x053, 0x070, 0x071, //
    0x072, 0x05f, 0x061, 0x03e, 0x03f, 0x00c, 0x00d, 0x016, //
    0x00b, 0x012, 0x006, 0x017, 0x063, 0x0ca, 0x03d, 0x0c4, //
    0x0c5, 0x013, 0x0d0, 0x0d1, 0x0bf, 0x012, 0x0d7, 0x074, //
    0x0d4, 0x0d6, 0x0c2, 0x070, 0x00f, 0x066, 0x0d8, 0x085, //
    0x086, 0x064, 0x029, 0x016, 0x03b, 0x04a, 0x050, 0x00e, //
    0x01b, 0x015, 0x017, 0x006, 0x02a, 0x02b, 0x021, 0x07b, //
    0x07d, 0x07e, 0x07f, 0x079, 0x07c, 0x077, 0x05b, 0x05c, //
    0x0c7, 0x087, 0xffff, 0x090, 0x030, 0x133, 0x134, 0x13c, //
    0x0da, 0x11e, 0x143, 0x136, 0x13a, 0x06f, 0x120, 0x000, //
];

/// Strap index of each strap ID on SP7 A0 (Dell `0xfcf1d0`).
pub const STRAP_INDEX_A0: [u16; 128] = [
    0x147, 0x009, 0x14d, 0x086, 0x087, 0x088, 0x089, 0x08a, //
    0x08b, 0x08c, 0x08d, 0x08f, 0x090, 0x109, 0x10a, 0x10b, //
    0x153, 0x0ed, 0x108, 0x0d1, 0x158, 0x15b, 0x15a, 0x157, //
    0x008, 0x164, 0x06e, 0x163, 0x0d1, 0x04f, 0x0e6, 0x01e, //
    0x0e7, 0x0e1, 0x0e2, 0x0e3, 0x0e4, 0x0e5, 0x10c, 0x0db, //
    0x00e, 0x010, 0x011, 0x143, 0x144, 0x145, 0x0dc, 0x176, //
    0x17c, 0x16e, 0x168, 0x178, 0x107, 0x05a, 0x07f, 0x080, //
    0x081, 0x06a, 0x06c, 0x042, 0x043, 0x00c, 0x00d, 0x016, //
    0x00b, 0x012, 0x006, 0x017, 0x06f, 0x0d3, 0x03c, 0x0c0, //
    0x0c1, 0x012, 0x0cc, 0x0cd, 0x0bb, 0x011, 0x0d3, 0x073, //
    0x0d0, 0x0d2, 0x0be, 0x06f, 0x00e, 0x065, 0x0d4, 0x082, //
    0x083, 0x063, 0x028, 0x015, 0x03a, 0x049, 0x04f, 0x00d, //
    0x01a, 0x014, 0x016, 0x005, 0x029, 0x02a, 0x020, 0x07a, //
    0x07c, 0x07d, 0x07e, 0x078, 0x07b, 0x076, 0x05a, 0x05b, //
    0x0c3, 0x084, 0x093, 0x08d, 0x031, 0x13f, 0x140, 0x148, //
    0x0d6, 0x128, 0x152, 0x142, 0x146, 0x06e, 0x12a, 0x000, //
];

/// The strap space (request `0x0C` argument 2), which also selects the
/// per-port stride (`WhStrapIndexPerPort`, Dell `0xf86360`/`0xf86380`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Space {
    /// Port/PL straps: stride `0xBC` (`0xCE` on A0).
    Port = 0,
    /// TL straps: stride `0x29`.
    Tl = 1,
}

impl Space {
    const fn stride(self, a0: bool) -> u16 {
        match (self, a0) {
            (Self::Port, false) => 0xbc,
            (Self::Port, true) => 0xce,
            (Self::Tl, _) => 0x29,
        }
    }
}

/// Strap index of strap `id`, of port `port` (`None`: the core's).
pub const fn strap_index(a0: bool, id: u8, port: Option<u8>, space: Space) -> u16 {
    let base = if a0 {
        STRAP_INDEX_A0[id as usize]
    } else {
        STRAP_INDEX[id as usize]
    };
    match port {
        None => base,
        Some(p) => base.wrapping_add(p as u16 * space.stride(a0)),
    }
}

/// Request `0x0C`'s first argument (`WritePcieStrapWh`, Dell `0xf61fb0`):
/// the core in bits 23:16, the strap index below.
pub const fn strap_argument(core: u8, index: u16) -> u32 {
    (core as u32) << 16 | index as u32
}

/// One programming step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Request `0x0C`: strap `index` of `core` in `space`.
    Strap {
        core: u8,
        index: u16,
        value: u32,
        space: Space,
    },
    /// Request `0x26`: `(old & !clear) | set` at SMN `address`.
    Proxy { address: u32, clear: u32, set: u32 },
    /// A direct SMN read-modify-write on the IOD.
    Smn { address: u32, clear: u32, set: u32 },
    /// A read-modify-write of the root port's PCI configuration space.
    Config {
        offset: u16,
        clear: u32,
        set: u32,
        /// 16-bit access.
        word: bool,
    },
}

impl Op {
    /// The fields of `value` in the root port's configuration register at
    /// `offset` (32-bit).
    pub fn config<R: RegisterLongName>(offset: u16, value: FieldValue<u32, R>) -> Self {
        Self::Config {
            offset,
            clear: value.mask(),
            set: value.value,
            word: false,
        }
    }

    /// The fields of `value` in the 16-bit configuration register at
    /// `offset`.
    pub fn config16<R: RegisterLongName>(offset: u16, value: FieldValue<u16, R>) -> Self {
        Self::Config {
            offset,
            clear: value.mask().into(),
            set: value.value.into(),
            word: true,
        }
    }

    /// `(old & !clear) | set` at `reg`, the vendor's way ([`Access`]).
    pub fn rmw<R: RegisterLongName>(reg: SmnReg<R>, access: Access, clear: u32, set: u32) -> Self {
        let address = reg.address();
        match access {
            Access::Proxy => Self::Proxy {
                address,
                clear,
                set,
            },
            Access::Smn => Self::Smn {
                address,
                clear,
                set,
            },
        }
    }
}

/// Most steps of one port in one phase.
pub const MAX_OPS: usize = 96;
pub type Ops = Vec<Op, MAX_OPS>;

/// A step list did not fit [`MAX_OPS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// The platform input-block values the steps read (`SilData`/`SilDataWh`
/// offsets from `input_block_offsets.json`). [`Default`]: Dell's build
/// default where known, else off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// `SilDataWh+0x0C` (`TargetPcieGenSpeed`, strap 2); `None` is the
    /// vendor's `0xF`, written as 0.
    pub target_gen_speed: Option<u16>,
    /// `AmdHotPlugHandlingMode` is one of the SFI modes and the SFI strap
    /// override is on (`SilData+0x4F7`): straps `0x1A/0x1B/0x4F..0x51`.
    pub sfi: bool,
    /// `SilData+0x16`: also strap `0x1C`.
    pub sfi_strap_1c: bool,
    /// `SilData+0x22` (strap `0x2B..0x2D`, CXL).
    pub cxl_straps: bool,
    /// `SilData+0x33`: strap `0x7C` = 1.
    pub strap_7c: bool,
    /// `SilData+0x34`: unless set, strap `0x7B` = 0.
    pub keep_strap_7b: bool,
    /// `SilData+0x5D`: `0x1A34_0608` bit 20.
    pub port_608_b20: bool,
    /// `LcMultAutoSpdChgOnLastRateEnable` (`SilData+0x30`).
    pub lc_mult_auto_spd_chg: bool,
    /// `AmdPcieFlitErrorCounterEnable`, `...EventsToCount`, `...TriggerErrorCount`.
    pub flit_error_counter: Option<(u8, u8)>,
    /// `AmdPcieFlitModeProgrammableDrainRate`, `...TriggerEventOnErrorCount`.
    pub flit_drain: Option<(u8, u16)>,
    /// `AmdPcieSubsystemDeviceID`/`VendorID`.
    pub subsystem: Option<(u16, u16)>,
    /// `CfgPcieCVTestWA` (`SilData+0x32 == 1`).
    pub cv_test_wa: bool,
    /// `SrisCfgType` bits 2:1 non-zero: SRIS lower SKP OS straps.
    pub sris_cfg: bool,
    /// `AmdHotPlugSlotPowerDefault`: the vendor's default slot power
    /// limit (`PciePortConfigDefaultsSil`: 0x4B, i.e. 75 W at scale 0).
    pub slot_power_limit: u8,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            target_gen_speed: None,
            sfi: false,
            sfi_strap_1c: false,
            cxl_straps: false,
            strap_7c: false,
            keep_strap_7b: false,
            port_608_b20: false,
            lc_mult_auto_spd_chg: false,
            flit_error_counter: None,
            flit_drain: None,
            subsystem: None,
            cv_test_wa: false,
            sris_cfg: false,
            slot_power_limit: 0x4b,
        }
    }
}

/// The vendor's `PciePort` fields the steps read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortSettings {
    /// Core-local port (`PciePort+0x1F[7:4]`, from the ASK).
    pub port: u8,
    pub core: u8,
    pub rc: u8,
    /// `+0x50`: Gen3 DS/US presets and hints `[13:0]`, 8 GT/s mask `[23:14]`.
    pub gen3: u32,
    /// `+0x54`: Gen4 DS `[3:0]`, US `[7:4]`, 16 GT/s mask `[17:8]`.
    pub gen4: u32,
    /// `+0x58`: as Gen4 for Gen5, 32 GT/s; bit 19 strap `0x11`.
    pub gen5: u32,
    /// `+0x82`: as Gen4 for Gen6, 64 GT/s; bit 18 strap `0x12`.
    pub gen6: u32,
    /// `+0x4C`: straps `0x21..0x26`.
    pub pp4c: u32,
    pub l1_1: bool,
    pub l1_2: bool,
    /// `+0x33`: straps `0x47`/`0x48`.
    pub pp33: u8,
    /// `+0x71`: strap `0x77`.
    pub pp71: u8,
    /// `+0x3E`: strap `0x4A`.
    pub pp3e: u8,
    /// `+0x6F`: strap `0x4B`.
    pub pp6f: u8,
    /// `+0x2A`: strap `0x4C`.
    pub pp2a: u8,
    /// `+0x2B`: strap `0x13` (bit 1).
    pub pp2b: u8,
    /// `+0x3F`: straps `0x19`/`0x4E`.
    pub pp3f: u8,
    /// `+0x60`: bit 0 strap `0x1D` (inverted); bits 1..3 Gen3..5 fixed preset set.
    pub pp60: u8,
    /// Fixed presets Gen3/4/5 (`+0x62/+0x63/+0x64[7:4]`) and Gen6 (`+0x86[3:0]`).
    pub fixed_preset: [u8; 4],
    /// Gen6 fixed preset set (`+0x6A` bit 5).
    pub gen6_fixed: bool,
    /// `+0x7F`: SRIS enable (low nibble) and autodetect (high nibble) modes.
    pub sris: u8,
    /// `+0x5C`/`+0x5D`: SRIS lower SKP OS generation/reception support.
    pub sris_lower_skp: (u8, u8),
    /// `+0x15`: hotplug type.
    pub hotplug: Hotplug,
    /// `+0x16`: physical slot number.
    pub slot_num: u16,
    /// `+0x26`: slot power limit, `+0x5F[6:5]` scale.
    pub slot_power: (u8, u8),
    /// `+0x7A`: turn off unused lanes.
    pub turn_off_unused_lanes: bool,
    /// `+0x5F` bit 2: a CXL port.
    pub cxl: bool,
    /// `+0x28[3:0]` strap `0x27`, bit 7 `0x1C44_0480` bit 0.
    pub pp28: u8,
    /// `+0x81[6:4]`: strap `0x7E` and `0x1C44_0408[30:28]`.
    pub pp81: u8,
    /// `+0x87`: bit 3 strap `0x78`, bit 2 `0x1C44_032C` bit 22.
    pub pp87: u8,
    /// `+0x89[5:0]`: `0x1A34_0648`; bit 6 the `0x904/0x60C/cfg 0x80` bit.
    pub pp89: u8,
    /// `+0x76`: `0x1C44_02D8` bit 4.
    pub pp76: u8,
}

const fn field(word: u32, shift: u32, bits: u32, value: Option<u8>) -> u32 {
    match value {
        Some(v) => {
            (word & !(((1 << bits) - 1) << shift)) | (((v as u32) & ((1 << bits) - 1)) << shift)
        }
        None => word,
    }
}

const fn mask(word: u32, shift: u32, value: Option<u16>) -> u32 {
    match value {
        Some(v) => (word & !(0x3ff << shift)) | (((v as u32) & 0x3ff) << shift),
        None => word,
    }
}

impl PortSettings {
    /// The vendor's defaults for a port of `core`, then the board's `desc`
    /// (`enable_2spc_gen5`: `SilDataWh` byte 0, which selects the default
    /// of straps `0x25`).
    pub fn new(
        desc: &PortDesc<'_>,
        port: u8,
        core: u8,
        rc: u8,
        policy: &Policy,
        enable_2spc_gen5: bool,
    ) -> Self {
        let bonus = core == 6;
        // PciePortConfigDefaultsSil, then PciePortPlatformConfigDefault.
        let gen3 = 7 | 3 << 4 | 7 << 7 | 3 << 11 | (if bonus { 0x38 } else { 0x10 }) << 14;
        let gen4 = 7 | 4 << 4 | (if bonus { 0x38 } else { 0x10 }) << 8;
        let gen5 = 5 | 4 << 4 | (if bonus { 0 } else { 0x100 }) << 8;
        let gen6 = 6 | 5 << 4 | (if bonus { 0 } else { 0x42 }) << 8;
        let pp4c = 1 << 2
            | (if bonus { 2 } else { 1 }) << 4
            | (if bonus {
                0
            } else if enable_2spc_gen5 {
                1
            } else {
                2
            }) << 6
            | 2 << 8;
        let eq = &desc.eq;
        let gen3 = field(gen3, 0, 4, eq.gen3.ds_tx_preset);
        let gen3 = field(gen3, 4, 3, eq.gen3.ds_rx_hint);
        let gen3 = field(gen3, 7, 4, eq.gen3.us_tx_preset);
        let gen3 = field(gen3, 11, 3, eq.gen3.us_rx_hint);
        let gen3 = mask(gen3, 14, eq.preset_mask[0]);
        let gen4 = mask(
            field(
                field(gen4, 0, 4, eq.gen4.ds_tx_preset),
                4,
                4,
                eq.gen4.us_tx_preset,
            ),
            8,
            eq.preset_mask[1],
        );
        let gen5 = mask(
            field(
                field(gen5, 0, 4, eq.gen5.ds_tx_preset),
                4,
                4,
                eq.gen5.us_tx_preset,
            ),
            8,
            eq.preset_mask[2],
        );
        let gen6 = mask(
            field(
                field(gen6, 0, 4, eq.gen6.ds_tx_preset),
                4,
                4,
                eq.gen6.us_tx_preset,
            ),
            8,
            eq.preset_mask[3],
        );
        let mut pp60 = 1;
        for (i, p) in eq.fixed_preset[..3].iter().enumerate() {
            if p.is_some() {
                pp60 |= 2 << i;
            }
        }
        let sris_nibble = |v: Option<u8>| v.unwrap_or(0xf) & 0xf;
        Self {
            port,
            core,
            rc,
            gen3,
            gen4,
            gen5,
            gen6,
            pp4c,
            l1_1: desc.aspm.l1_1,
            l1_2: desc.aspm.l1_2,
            pp33: 3,
            pp71: 1,
            pp3e: 1,
            pp6f: 1,
            pp2a: 2,
            pp2b: 3,
            pp3f: 1,
            pp60,
            fixed_preset: eq.fixed_preset.map(|p| p.unwrap_or(0) & 0xf),
            gen6_fixed: eq.fixed_preset[3].is_some(),
            sris: sris_nibble(desc.sris.enable) | sris_nibble(desc.sris.autodetect) << 4,
            sris_lower_skp: (
                desc.sris.lower_skp_os_gen.unwrap_or(0),
                desc.sris.lower_skp_os_rcv.unwrap_or(0),
            ),
            hotplug: desc.hotplug,
            slot_num: desc.slot_num.unwrap_or(0),
            slot_power: desc
                .slot_power
                .map_or((policy.slot_power_limit, 0), |p| (p.limit, p.scale)),
            turn_off_unused_lanes: desc.turn_off_unused_lanes,
            cxl: desc.cxl,
            pp28: 0,
            pp81: 7,
            pp87: 2 | 4,
            pp89: 0,
            pp76: 0,
        }
    }

    /// The port's core, RC aperture and port.
    pub const fn id(&self) -> PortId {
        PortId {
            core: self.core,
            rc: self.rc,
            port: self.port,
        }
    }
}

/// A step list being built; one that overflows is refused whole
/// ([`Self::finish`]).
pub(super) struct Emit {
    ops: Ops,
    id: PortId,
    a0: bool,
    full: bool,
}

impl Emit {
    pub(super) const fn new(id: PortId, a0: bool) -> Self {
        Self {
            ops: Vec::new(),
            id,
            a0,
            full: false,
        }
    }
    pub(super) fn push(&mut self, op: Op) {
        self.full |= self.ops.push(op).is_err();
    }
    pub(super) fn finish(self) -> Result<Ops, Full> {
        if self.full { Err(Full) } else { Ok(self.ops) }
    }
    /// `(old & !clear) | set` at `reg` of this port.
    pub(super) fn rmw<R: RegisterLongName>(&mut self, reg: pcie::Reg<R>, clear: u32, set: u32) {
        self.push(Op::rmw(reg.at(self.id), reg.access(), clear, set));
    }
    /// The fields of `value` at `reg` of this port.
    pub(super) fn field<R: RegisterLongName>(
        &mut self,
        reg: pcie::Reg<R>,
        value: FieldValue<u32, R>,
    ) {
        self.rmw(reg, value.mask(), value.value);
    }
    fn port_strap(&mut self, id: u8, space: Space, value: u32) {
        let index = strap_index(self.a0, id, Some(self.id.port), space);
        self.push(Op::Strap {
            core: self.id.core,
            index,
            value,
            space,
        });
    }
    fn core_strap(&mut self, id: u8, space: Space, value: u32) {
        let index = strap_index(self.a0, id, None, space);
        self.push(Op::Strap {
            core: self.id.core,
            index,
            value,
            space,
        });
    }
}

/// Core flags the vendor sets per wrapper from the NBIO input block
/// (`PcieFixupPlatformConfig` Dell `0xf36e60`): bit 5 straps `0x18`/`0x4D`,
/// bit 11 straps `1`/`0x49`. Both default to "auto" (`0xFE`), i.e. 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreFlags {
    pub strap_18: bool,
    pub strap_1: bool,
    /// Wrapper flag bit 6: `PCIE_STRAP_F0[5]`, the AER capability of the
    /// core's ports ([`core_before_reconfig`]). Its input-block source is
    /// not recovered; on, as fstart always enabled it.
    pub aer: bool,
}

impl Default for CoreFlags {
    fn default() -> Self {
        Self {
            strap_18: true,
            strap_1: true,
            aer: true,
        }
    }
}

/// The core part of `MpioCfgCoreBeforeBifurcationWh` (Dell `0xf7be90`)
/// for PCIe core `core` (RC aperture `rc`), after its ports'
/// [`before_reconfig`]: `PCIE_STRAP_F0[5]` (AER, `0xf7bfd1`) through the
/// proxy. Its other writes (`PCIE_STRAP_F0` bits 6, 7, 14:13,
/// `0x1A38_0204[13]`, strap `0x46`) come from wrapper flags and input
/// bytes whose meaning is not recovered, and are not done.
pub fn core_before_reconfig(core: u8, rc: u8, flags: &CoreFlags) -> Result<Ops, Full> {
    let mut e = Emit::new(PortId { core, rc, port: 0 }, false);
    e.field(pcie::STRAP_F0, PcieStrapF0::AER_EN.val(flags.aer.into()));
    e.finish()
}

/// `BeforeReconfigPerPortProgramming` (Dell `0xf7e540`) for a PCIe port,
/// between the map and the reconfiguration.
pub fn before_reconfig(
    s: &PortSettings,
    core: &CoreFlags,
    policy: &Policy,
    a0: bool,
) -> Result<Ops, Full> {
    let mut e = Emit::new(s.id(), a0);
    let bonus = s.core == 6;
    // 0xf7c150
    if s.pp33 & 1 != 0 {
        e.port_strap(0x47, Space::Tl, 1);
    }
    if s.pp33 & 2 != 0 {
        e.port_strap(0x48, Space::Tl, 1);
    }
    // 0xf7c2a0
    if !bonus {
        e.port_strap(0, Space::Port, 1);
        e.port_strap(0x77, Space::Port, s.pp71.into());
    }
    // 0xf7c350: PciePort+0x75 is 0 unless the descriptor sets it.
    e.rmw(
        pcie::LC_288,
        0x4020_0000,
        u32::from(s.turn_off_unused_lanes) << 30,
    );
    // 0xf7c3d0
    e.core_strap(1, Space::Port, core.strap_1.into());
    e.core_strap(0x49, Space::Tl, core.strap_1.into());
    e.port_strap(0x4a, Space::Tl, s.pp3e.into());
    // 0xf7c4b0, 0xf7c510, 0xf7c610
    e.port_strap(0x4b, Space::Tl, s.pp6f.into());
    e.port_strap(
        2,
        Space::Port,
        policy
            .target_gen_speed
            .filter(|&v| v != 0xf)
            .unwrap_or(0)
            .into(),
    );
    e.port_strap(0x4c, Space::Tl, s.pp2a.into());
    // 0xf7c670..0xf7cb00: equalization. The Gen3..Gen6 presets are core
    // straps (the last port of a core wins), the masks per port.
    e.core_strap(3, Space::Port, s.gen3 >> 4 & 7);
    e.core_strap(4, Space::Port, s.gen3 >> 11 & 7);
    e.core_strap(5, Space::Port, s.gen3 & 0xf);
    e.core_strap(6, Space::Port, s.gen3 >> 7 & 0xf);
    e.core_strap(7, Space::Port, s.gen4 & 0xf);
    e.core_strap(8, Space::Port, s.gen4 >> 4 & 0xf);
    e.core_strap(9, Space::Port, s.gen5 & 0xf);
    e.core_strap(10, Space::Port, s.gen5 >> 4 & 0xf);
    e.core_strap(0xb, Space::Port, s.gen6 & 0xf);
    e.core_strap(0xc, Space::Port, s.gen6 >> 4 & 0xf);
    e.port_strap(0xd, Space::Port, s.gen3 >> 14 & 0x3ff);
    e.port_strap(0xe, Space::Port, s.gen4 >> 8 & 0x3ff);
    e.port_strap(0xf, Space::Port, s.gen5 >> 8 & 0x3ff);
    e.port_strap(0x10, Space::Port, s.gen6 >> 8 & 0x3ff);
    e.port_strap(0x11, Space::Port, s.gen5 >> 19 & 1);
    e.port_strap(0x12, Space::Port, s.gen6 >> 18 & 1);
    // 0xf7cb60..0xf7cc80: descriptor +0x23 bits 0..3, unnamed, 0.
    for eq in pcie::EQ_CONTROL {
        e.field(eq, EqControl::BIT6::CLEAR);
    }
    // 0xf7cce0, 0xf7cd40: L1 substates.
    e.port_strap(0x13, Space::Port, u32::from(s.pp2b & 2));
    e.port_strap(0x14, Space::Port, 0);
    e.port_strap(0x15, Space::Port, 0);
    if s.l1_1 || s.l1_2 {
        e.port_strap(0x15, Space::Port, 1);
        if s.l1_2 {
            e.port_strap(0x16, Space::Port, 1);
            e.port_strap(0x17, Space::Port, 1);
        }
        if s.l1_1 {
            e.port_strap(0x16, Space::Port, 1);
        }
    }
    // 0xf7dd80 (CXL), 0xf7cf30
    if policy.cxl_straps && !bonus {
        let v = u32::from(s.pp87 & 2 != 0);
        for id in [0x2b, 0x2c, 0x2d] {
            e.port_strap(id, Space::Port, v);
        }
    }
    e.core_strap(0x18, Space::Port, core.strap_18.into());
    e.core_strap(0x4d, Space::Tl, core.strap_18.into());
    e.port_strap(0x19, Space::Port, s.pp3f.into());
    e.port_strap(0x4e, Space::Tl, s.pp3f.into());
    // 0xf7d050: SFI hotplug.
    if policy.sfi
        && matches!(
            s.hotplug,
            Hotplug::EnterpriseSsd | Hotplug::Ubm | Hotplug::Ocp
        )
    {
        e.port_strap(0x1a, Space::Port, 1);
        e.port_strap(0x4f, Space::Tl, 1);
        e.port_strap(0x1b, Space::Port, 1);
        e.port_strap(0x50, Space::Tl, 1);
        e.port_strap(0x51, Space::Tl, 1);
        if policy.sfi_strap_1c {
            e.port_strap(0x1c, Space::Tl, 1);
        }
    }
    // 0xf7d2a0, SrisFeature 0xf7d300
    e.core_strap(0x1d, Space::Port, u32::from(s.pp60 & 1 == 0));
    if s.sris & 0xf == 1 {
        e.port_strap(0x1e, Space::Port, u32::from(s.sris >> 4 != 1));
    }
    if s.sris >> 4 == 1 {
        e.port_strap(0x20, Space::Port, 1);
    }
    // 0xf7d6f0..0xf7d8e0
    e.port_strap(0x21, Space::Port, s.pp4c & 1);
    e.port_strap(0x22, Space::Port, s.pp4c >> 1 & 1);
    e.port_strap(0x23, Space::Port, s.pp4c >> 2 & 3);
    e.port_strap(0x24, Space::Port, s.pp4c >> 4 & 3);
    e.port_strap(0x25, Space::Port, s.pp4c >> 6 & 3);
    e.port_strap(0x26, Space::Port, s.pp4c >> 8 & 3);
    // 0xf7d940, 0xf7e260, 0xf7e300, 0xf7e360, 0xf7e430, 0xf7e3c0
    e.port_strap(0x52, Space::Tl, 1);
    if !bonus {
        e.port_strap(0x34, Space::Port, 1);
        e.core_strap(0x55, Space::Tl, 1);
    }
    e.port_strap(0x56, Space::Tl, 0);
    e.port_strap(0x78, Space::Tl, u32::from(s.pp87 >> 3 & 1));
    e.port_strap(0x79, Space::Port, 7);
    e.port_strap(0x45, Space::Port, 7);
    // Every port, PCIe engine or not: 0xf7d990, 0xf7d9f0, 0xf7da50.
    e.port_strap(0x27, Space::Port, u32::from(s.pp28 & 0xf));
    if policy.strap_7c {
        e.port_strap(0x7c, Space::Port, 1);
    }
    if !policy.keep_strap_7b {
        e.port_strap(0x7b, Space::Port, 0);
    }
    // CXL ports (PciePort+0x5F bit 2) off the bonus core: 0xf7deb0, 0xf7dc90
    // need the CXL policy; the CXL owner adds them.
    // 0xf7e1a0, 0xf7e1f0, 0xf7e4e0, 0xf7e480
    e.rmw(pcie::PORT_608, 1 << 15, 0);
    e.rmw(
        pcie::PORT_608,
        1 << 20,
        u32::from(policy.port_608_b20) << 20,
    );
    e.port_strap(0x7a, Space::Port, 0);
    e.port_strap(0x7e, Space::Port, u32::from(s.pp81 >> 4 & 7));
    if a0 {
        e.core_strap(0x35, Space::Port, 0x83);
    }
    e.finish()
}

/// `AfterReconfigPerPortProgramming` (Dell `0xf82470`) for a PCIe port,
/// between the reconfiguration and PERST.
pub fn after_reconfig(s: &PortSettings, policy: &Policy, a0: bool) -> Result<Ops, Full> {
    let mut e = Emit::new(s.id(), a0);
    let bonus = s.core == 6;
    // 0xf80e80, 0xf81110, 0xf813a0: Gen3/4/5 equalization control, fixed
    // preset when the board gave one (the coefficient tables' gate is an
    // unnamed descriptor field: off).
    for (i, eq) in pcie::EQ_CONTROL[..3].iter().copied().enumerate() {
        e.field(eq, EqControl::BIT0::CLEAR);
        if s.pp60 & (2 << i) != 0 {
            e.field(eq, EqControl::FIXED_PRESET_EN::SET);
            e.field(eq, EqControl::FIXED_PRESET.val(s.fixed_preset[i].into()));
        }
    }
    // 0xf81690: Gen6.
    let gen6 = pcie::EQ_CONTROL[3];
    e.field(gen6, EqControl::BIT0.val(u32::from(s.pp28 >> 7)));
    if s.gen6_fixed {
        e.field(gen6, EqControl::FIXED_PRESET_EN::SET);
        e.field(gen6, EqControl::FIXED_PRESET.val(s.fixed_preset[3].into()));
    }
    // 0xf817a0 (PciePort+0x79 is 0 unless a speed cap set it).
    e.rmw(pcie::LC_414, 1 << 12, 0);
    if policy.lc_mult_auto_spd_chg {
        e.rmw(pcie::LC_2D4, 1 << 14, 1 << 14);
    }
    // 0xf81850 (forced speed, PciePort+0x86[5:4]: none) and 0xf81be0
    // (PciePort+0x89[5:0]: 0) do nothing by default.
    // 0xf82020: SRIS enabled.
    if s.sris & 0xf == 1 {
        e.field(pcie::LINK_CONTROL_STATUS, LinkControlStatus::SRIS::SET);
    }
    // 0xf82090
    let b = u32::from(s.pp89 >> 6 & 1);
    e.rmw(pcie::PORT_904, 1 << 2, b << 2);
    e.rmw(pcie::PORT_60C, 1, b);
    e.field(pcie::DEVICE_CONTROL2, DeviceControl2::BIT6.val(b));
    // 0xf81e60: flit error counter (direct SMN).
    let (counter, events, trigger) = match policy.flit_error_counter {
        Some((events, trigger)) => (1, events, trigger),
        None => (0, 0, 0),
    };
    let flit = pcie::FLIT_ERROR_COUNTER;
    e.field(flit, FlitErrorCounter::ENABLE.val(counter));
    e.field(flit, FlitErrorCounter::EVENTS.val(events.into()));
    e.field(flit, FlitErrorCounter::TRIGGER.val(trigger.into()));
    // 0xf82190
    if !a0 && !bonus {
        e.rmw(pcie::LC_46C, 1 << 27, 1 << 27);
        e.field(pcie::CXL_PM, pcie::CxlPm::RESPONSE_B::SET);
        e.field(pcie::CXL_PM, pcie::CxlPm::BIT17::CLEAR);
        e.rmw(pcie::LC_47C, 0x1c, 4);
        e.rmw(pcie::LC_47C, 2, 2);
    }
    // 0xf81f50: flit drain rate.
    if let Some((rate, count)) = policy.flit_drain.filter(|&(_, c)| c != 0) {
        e.field(pcie::CORE_FLIT_DRAIN, pcie::CoreFlitDrain::ENABLE::SET);
        e.field(pcie::FLIT_DRAIN, FlitDrain::RATE.val(rate.into()));
        e.field(pcie::FLIT_DRAIN, FlitDrain::COUNT.val(count.into()));
    }
    // 0xf82340, 0xf82390, 0xf823f0
    e.rmw(pcie::LC_A54, 1, 0);
    e.rmw(pcie::LC_408, 0x7000_0000, u32::from(s.pp81 >> 4 & 7) << 28);
    e.rmw(pcie::LC_32C, 1 << 22, u32::from(s.pp87 >> 2 & 1) << 22);
    e.finish()
}

/// `AfterDxioInitPerPortProgramming` (Dell `0xf83670`) for a PCIe port
/// after training: `trained` (MPIO state 6) or a hotplug port other than
/// inboard gets the slot and link settings; every port its subsystem IDs.
/// `link_speed_cap` is the root port's Link Capabilities speed
/// (`0x1C40_0064[3:0]`) for the CV test workaround.
pub fn after_training(
    s: &PortSettings,
    policy: &Policy,
    trained: bool,
    link_speed_cap: u8,
) -> Result<Ops, Full> {
    let mut e = Emit::new(s.id(), false);
    let hotplug = !matches!(s.hotplug, Hotplug::Disabled | Hotplug::Inboard);
    if trained || hotplug {
        // 0xf82c60: Slot Implemented, physical slot, power limit/scale.
        e.push(Op::config16(
            pcie::config::PCIE_CAPABILITIES,
            fstart_pci::PCI_EXPRESS_CAPABILITIES::SLOT_IMPLEMENTED::SET,
        ));
        if s.slot_num != 0 {
            e.push(Op::config(
                pcie::config::SLOT_CAPABILITIES,
                SlotCapabilities::PHYSICAL_SLOT.val(s.slot_num.into()),
            ));
        }
        if s.slot_power.0 != 0 {
            e.push(Op::config(
                pcie::config::SLOT_CAPABILITIES,
                SlotCapabilities::POWER_LIMIT.val(s.slot_power.0.into())
                    + SlotCapabilities::POWER_SCALE.val(u32::from(s.slot_power.1 & 3)),
            ));
        }
        // 0xf82d70: completion timeout, LC controls.
        e.field(
            pcie::DEVICE_CONTROL2,
            DeviceControl2::COMPLETION_TIMEOUT.val(6),
        );
        e.rmw(pcie::LC_280, 1 << 23, 1 << 23);
        e.rmw(pcie::LC_288, 1 << 19, 1 << 19);
        e.rmw(pcie::LC_288, 1 << 10, 1 << 10);
        e.rmw(pcie::LC_288, 1 << 17, 1 << 17);
        e.rmw(pcie::LC_2C4, 1 << 13, 1 << 13);
        e.rmw(pcie::LC_2C4, 0xc000, 0x4000);
        e.rmw(pcie::LC_2D4, 1 << 12, 1 << 12);
        e.rmw(pcie::LC_284, 1 << 6, 1 << 6);
        // 0xf82ff0: Link Control 2 bits 9:7 (+0x28[6:4]).
        e.field(
            pcie::LINK_CONTROL2,
            LinkControl2::TRANSMIT_MARGIN.val(u32::from(s.pp28 >> 4 & 7)),
        );
    }
    // ProgramPcieSubSystemId 0xf834b0
    if let Some((device, vendor)) = policy.subsystem {
        if device != 0 {
            e.field(pcie::SUBSYSTEM_ID, SubsystemId::DEVICE.val(device.into()));
        }
        if vendor != 0 {
            e.field(pcie::SUBSYSTEM_ID, SubsystemId::VENDOR.val(vendor.into()));
        }
    }
    // 0xf83610, 0xf833e0
    e.rmw(pcie::LC_2D8, 1 << 4, u32::from(s.pp76 & 1) << 4);
    if policy.cv_test_wa && link_speed_cap > 2 {
        e.rmw(pcie::LC_2C4, 1 << 8, 1 << 8);
    }
    e.finish()
}

/// `PcieHotplugPreInitWh` (Dell `0xf62070`): before the hotplug setup, on
/// every port of a hotplug-capable core.
pub fn hotplug_pre_init(s: &PortSettings) -> Result<Ops, Full> {
    let mut e = Emit::new(s.id(), false);
    e.rmw(pcie::PORT_348, 3, 0);
    e.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxio::wh::port::{Aspm, Speed};

    #[test]
    fn strap_indices_follow_the_vendor_tables() {
        assert_eq!(strap_index(false, 3, None, Space::Port), 0x75);
        assert_eq!(
            strap_index(false, 0xd, Some(2), Space::Port),
            0xfe + 2 * 0xbc
        );
        assert_eq!(
            strap_index(true, 0xd, Some(2), Space::Port),
            0x109 + 2 * 0xce
        );
        assert_eq!(strap_index(false, 0x47, Some(1), Space::Tl), 0x0c4 + 0x29);
        assert_eq!(strap_argument(6, 0x1234), 0x0006_1234);
    }

    #[test]
    fn defaults_and_board_eq_reach_the_straps() {
        let mut desc = PortDesc::pcie(0, 15).speed(Speed::Gen5, Speed::Gen5);
        desc.eq.gen4.ds_tx_preset = Some(9);
        desc.eq.preset_mask[2] = Some(0x155);
        desc.aspm = Aspm {
            l1_2: true,
            ..Aspm::default()
        };
        let s = PortSettings::new(&desc, 1, 2, 3, &Policy::default(), false);
        assert_eq!(s.gen3, 7 | 3 << 4 | 7 << 7 | 3 << 11 | 0x10 << 14);
        let ops = before_reconfig(&s, &CoreFlags::default(), &Policy::default(), false).unwrap();
        let strap = |id: u8, port: Option<u8>, space| {
            let index = strap_index(false, id, port, space);
            ops.iter().find_map(|op| match *op {
                Op::Strap {
                    index: i,
                    value,
                    core,
                    space: sp,
                } if i == index && sp == space => {
                    assert_eq!(core, 2);
                    Some(value)
                }
                _ => None,
            })
        };
        assert_eq!(strap(7, None, Space::Port), Some(9));
        assert_eq!(strap(5, None, Space::Port), Some(7));
        assert_eq!(strap(0xf, Some(1), Space::Port), Some(0x155));
        assert_eq!(strap(0x17, Some(1), Space::Port), Some(1));
        assert_eq!(strap(0x24, Some(1), Space::Port), Some(1));
        assert_eq!(strap(0x25, Some(1), Space::Port), Some(2));
        assert_eq!(strap(0x35, None, Space::Port), None);
        assert!(ops.contains(&Op::Proxy {
            address: 0x1c44_0288 + 0x30_0000 + 0x1000,
            clear: 0x4020_0000,
            set: 1 << 30,
        }));
        assert!(ops.len() < MAX_OPS);
        // The bonus core skips strap 0 and its masks default differently.
        let b = PortSettings::new(
            &PortDesc::pcie(129, 129),
            2,
            6,
            5,
            &Policy::default(),
            false,
        );
        assert_eq!(b.gen4 >> 8 & 0x3ff, 0x38);
        let ops = before_reconfig(&b, &CoreFlags::default(), &Policy::default(), true).unwrap();
        assert!(ops.iter().any(|op| matches!(op, Op::Strap { index, value: 0x83, .. } if *index == STRAP_INDEX_A0[0x35])));
    }

    #[test]
    fn slot_and_subsystem_after_training() {
        let desc = PortDesc::pcie(0, 15).slot(Hotplug::Disabled, 12);
        let policy = Policy {
            subsystem: Some((0x1234, 0x1458)),
            ..Policy::default()
        };
        let s = PortSettings::new(&desc, 0, 2, 3, &policy, false);
        let ops = after_training(&s, &policy, true, 5).unwrap();
        assert!(ops.contains(&Op::Config {
            offset: 0x6c,
            clear: 0xfff8_0000,
            set: 12 << 19,
            word: false
        }));
        assert!(ops.contains(&Op::Config {
            offset: 0x6c,
            clear: 0x1ff80,
            set: 0x4b << 7,
            word: false
        }));
        assert!(ops.contains(&Op::Proxy {
            address: 0x1c40_004c + 0x30_0000,
            clear: 0xffff,
            set: 0x1458
        }));
        // An untrained port without hotplug only gets its IDs and 0x2D8.
        assert_eq!(after_training(&s, &policy, false, 5).unwrap().len(), 3);
        assert!(after_reconfig(&s, &policy, false).unwrap().len() < MAX_OPS);
        // The core's AER strap, through the proxy.
        let core = core_before_reconfig(2, 3, &CoreFlags::default()).unwrap();
        assert_eq!(
            core[..],
            [Op::Proxy {
                address: 0x1a38_02c0 + 0x20_0000,
                clear: 1 << 5,
                set: 1 << 5
            }]
        );
    }

    /// Every step of a port with every policy bit, hotplug and CXL on fits
    /// [`MAX_OPS`]; one that does not is refused, not cut.
    #[test]
    fn the_longest_lists_fit() {
        use crate::dxio::wh::hotplug::{self, InitPolicy};
        use crate::dxio::wh::port::Speed;
        let mut desc = PortDesc::pcie(0, 15)
            .speed(Speed::Gen5, Speed::Gen5)
            .slot(Hotplug::Ubm, 3);
        desc.aspm = Aspm {
            l1_1: true,
            l1_2: true,
            ..Aspm::default()
        };
        desc.eq.fixed_preset = [Some(1); 4];
        desc.cxl = true;
        let policy = Policy {
            target_gen_speed: Some(5),
            sfi: true,
            sfi_strap_1c: true,
            cxl_straps: true,
            strap_7c: true,
            keep_strap_7b: false,
            port_608_b20: true,
            lc_mult_auto_spd_chg: true,
            flit_error_counter: Some((1, 2)),
            flit_drain: Some((1, 2)),
            subsystem: Some((1, 2)),
            cv_test_wa: true,
            sris_cfg: true,
            slot_power_limit: 0x4b,
        };
        let s = PortSettings::new(&desc, 1, 2, 3, &policy, false);
        for a0 in [false, true] {
            let before = before_reconfig(&s, &CoreFlags::default(), &policy, a0).unwrap();
            assert!(before.len() < MAX_OPS, "{}", before.len());
            assert!(after_reconfig(&s, &policy, a0).unwrap().len() < MAX_OPS);
        }
        let after = after_training(&s, &policy, true, 5).unwrap();
        let hp = InitPolicy {
            sfi: true,
            presence_detect_select: Some(1),
            pd_settle: true,
            allow_compliance: true,
            express_module_reset: true,
            slot_control_b14: true,
            nvme_max_payload: Some(1),
            a0: false,
        };
        let init = hotplug::init(&s, &hp, 3, 9, false, Some((1, 2))).unwrap();
        let pre = hotplug_pre_init(&s).unwrap();
        // The platform runs these two as one list.
        assert!(after.len() < MAX_OPS && pre.len() + init.len() < MAX_OPS);
        let mut e = Emit::new(s.id(), false);
        for _ in 0..=MAX_OPS {
            e.rmw(pcie::LC_288, 1, 1);
        }
        assert_eq!(e.finish(), Err(Full));
    }
}
