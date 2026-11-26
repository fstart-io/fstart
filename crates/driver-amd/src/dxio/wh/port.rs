// SPDX-License-Identifier: MIT OR Apache-2.0
//! A board's description of one WH MPIO port: the typed equivalent of the
//! vendor's `MPIO_PORT_DESCRIPTOR` (`$A26`/`$A27` CPM tables) with its
//! `{type, value}` port parameters folded in (MPIO.md §2.4,
//! `descriptor_map.json`), and its translation to the ASK (§2.5) and the
//! ancillary records (§2.7).
//!
//! The vendor's parameter codes are not part of this API: from `0x31` on
//! Dell (PI 1.0.0.0) and GIGABYTE (PI 0.0.8.1) number them differently
//! (Dell inserted `0x31`, every later GIGABYTE code is Dell's minus one).
//! Fields are named after Dell's, the reference; those the decoded
//! descriptor names `unknown_*` are not exposed.
//!
//! Which fields go where (per engine, [`Engine`]):
//! - the ASK (`InitAskEntryAttributes`, Dell `0xf799b0`, and
//!   `MpioTranslateLanesCallback` `0xf7a310`): lanes, controller, PERST
//!   group, SATA channel, devfn, present, early train, compliance, hotplug
//!   type, speeds, master PLL, polarity, de-emphasis, vetting, UBM
//!   connector indices, swing, SRIS skip interval, chipset link;
//! - the ancillary stream: [`PortDesc::ancillary`];
//! - the per-port straps and private registers ([`super::straps`]): the
//!   equalization, ASPM, L1 substates, clock PM, slot and hotplug fields.
//!   The vendor copies the descriptor into its port (Dell `0xf77cc0`) and
//!   [`super::straps::PortSettings::new`] does the same for these fields.

use tock_registers::LocalRegisterCopy;

use super::layout::{LaneMapping, Layout, translate_port};
use crate::dxio::rs::ask::{self, Ask, Attr, Device, Handle, Lane, Link, Phy, Transport};

/// Engine type (descriptor `+0x04`) and how `MpioParsePlatformTopologySil`
/// (Dell `0xf7ac50`, GIGABYTE `0x709c254b`; dispatch [I] both) encodes it
/// in the ASK (DEVICES.md §2.1).
///
/// The descriptor's other engine codes (2, 5 UBM host-facing connector,
/// 6 OCP3, 7 U.3) get a lanes-only ASK on WH: no controller, GPIO group,
/// attributes or ancillary index. A board describes such a port as a PCIe
/// engine with its hotplug type instead, as the vendor's UBM code does
/// ([`PortDesc::ubm`], [`PortDesc::ocp3`], [`PortDesc::u3`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Engine {
    /// Not used: the vendor drops it from the list (`MpioCopyPortDescriptors`,
    /// Dell `0xf0f410`). A board option can turn a port off this way
    /// (GIGABYTE `GbtLanCtrl`).
    Unused = 0,
    Pcie = 1,
    Sata = 3,
    /// USB4: a PCIe ASK (controller 0, PCIe attributes). The WH MPIO API
    /// call the vendor makes first (`+0x58`, Dell `0xf5d7a0`, GIGABYTE
    /// `0x709ad554`) returns 2 without doing anything, and its USB4 hotplug
    /// hook (`MpioHotplugConfigureUSB4Wh 0xf63990`) is empty [I]: the port
    /// is a PCIe root port to the host.
    Usb4 = 4,
    /// UALink (`InitUalinkAskEntryAttributes`, Dell `0xf79d50`, GIGABYTE
    /// `0x709c1f90`): controller 3 with the link's P-states
    /// ([`PortDesc::ualink`]). No root port of the host's.
    UaLink = 0x70,
}

impl Engine {
    /// An ASK fstart can build: every engine but [`Self::Unused`].
    pub const fn has_ask(self) -> bool {
        !matches!(self, Self::Unused)
    }

    /// The ASK controller (`dw0[31:28]`): PCIe 0, SATA 1, UALink 3; USB4
    /// is a PCIe one.
    pub const fn controller(self) -> u8 {
        match self {
            Self::Sata => 1,
            Self::UaLink => 3,
            Self::Unused | Self::Pcie | Self::Usb4 => 0,
        }
    }

    /// The engine trains a PCIe root port of the host.
    pub const fn is_pcie(self) -> bool {
        matches!(self, Self::Pcie | Self::Usb4)
    }
}

/// One UALink P-state (descriptor `+0x50`/`+0x52`/`+0x54`/`+0x56`): a link
/// speed code (5 bits) and width (11 bits), as the ASK takes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UaLinkPstate {
    pub speed: u8,
    pub width: u16,
}

/// A UALink port's ASK attributes (`InitUalinkAskEntryAttributes`, Dell
/// `0xf79d50` [D]). The attribute bytes are named by the descriptor bit
/// they copy where their meaning is not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UaLink {
    /// Up to four P-states; the ASK counts at most four.
    pub pstates: [UaLinkPstate; 4],
    /// The P-states in use (descriptor `+0x4F[4:2]`, at most 4).
    pub pstate_count: u8,
    /// The UALink port (descriptor `+0x4E[6:2]`, 5 bits).
    pub port: u8,
    /// Attribute byte 9 bits 3..6: descriptor `+0x4E` bit 7, `+0x4E`
    /// bit 0, `+0x4F` bit 0, `+0x4E` bit 1.
    pub flags: [bool; 4],
    /// Attribute byte `0xA` bit 4: descriptor `+0x4F` bit 1.
    pub fabric_flag: bool,
}

impl UaLink {
    /// ASK words 6..8 (`+0x18..+0x23`): the P-states, then the port, the
    /// count and flags, and the fabric type 3. The attributes sit where
    /// the PCIe ones do (`InitAskEntryAttributes`' `+0x18`); that the
    /// UALink attributes start there too is read from the decompiled
    /// offsets, not checked against MPIO.
    pub fn words(&self) -> [u32; 3] {
        let p = |s: &UaLinkPstate| u32::from(s.width & 0x7ff) | u32::from(s.speed & 0x1f) << 11;
        let count = u32::from(self.pstate_count.min(4));
        let flags = self
            .flags
            .iter()
            .enumerate()
            .fold(0, |acc, (i, &on)| acc | u32::from(on) << (3 + i));
        [
            p(&self.pstates[0]) | p(&self.pstates[1]) << 16,
            p(&self.pstates[2]) | p(&self.pstates[3]) << 16,
            u32::from(self.port & 0x1f)
                | (count | flags) << 8
                | (3 | u32::from(self.fabric_flag) << 4) << 16,
        ]
    }
}

/// Link speed (descriptor `+0x0D[5:3]` capability, `+0x34[2:0]` target).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Speed {
    #[default]
    Max = 0,
    Gen1,
    Gen2,
    Gen3,
    Gen4,
    Gen5,
    Gen6,
}

/// Hotplug type (descriptor `+0x0F[7:4]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Hotplug {
    #[default]
    Disabled = 0,
    Basic = 1,
    ExpressModule = 2,
    Enhanced = 3,
    Inboard = 4,
    EnterpriseSsd = 5,
    Ubm = 6,
    Ocp = 7,
}

/// ASPM and L1 substates (descriptor `+0x0E[7:4]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Aspm {
    pub l0s: bool,
    pub l1: bool,
    pub l1_1: bool,
    pub l1_2: bool,
}

/// Compliance mode (descriptor `+0x14` bit 0, `+0x1A` bit 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compliance {
    #[default]
    Off,
    On,
    /// Parameter value 2: compliance and its second mode bit.
    Mode2,
}

/// Gen3 (8 GT/s) presets and receiver hints (descriptor `+0x24`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gen3Eq {
    pub ds_tx_preset: Option<u8>,
    pub ds_rx_hint: Option<u8>,
    pub us_tx_preset: Option<u8>,
    pub us_rx_hint: Option<u8>,
}

/// Gen4/5/6 transmitter presets (descriptor `+0x27`, `+0x29`, `+0x43`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GenEq {
    pub ds_tx_preset: Option<u8>,
    pub us_tx_preset: Option<u8>,
}

/// Lane equalization; `None` keeps the silicon default
/// ([`super::straps::PortSettings`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Eq {
    pub gen3: Gen3Eq,
    pub gen4: GenEq,
    pub gen5: GenEq,
    pub gen6: GenEq,
    /// Fixed preset for Gen3, Gen4, Gen5, Gen6 (descriptor `+0x1B..+0x1D`).
    pub fixed_preset: [Option<u8>; 4],
    /// 10-bit preset masks at 8, 16, 32 and 64 GT/s (descriptor `+0x2B`, `+0x2F`).
    pub preset_mask: [Option<u16>; 4],
    /// Fixed transmitter coefficients at 8, 16 and 32 GT/s, as the
    /// coefficients of a preset P0..P9 (descriptor `+0x47` bits 4..6 with
    /// the preset in `+0x48[3:0]`, `+0x48[7:4]`, `+0x49[3:0]`; Dell port
    /// parameters `0x76..0x78`): [`super::coefficients`].
    pub coefficients: [Option<u8>; 3],
}

/// SRIS (descriptor `+0x34..+0x38`). The vendor applies the global SRIS
/// policy instead where the port leaves a field unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sris {
    /// Enable mode (`+0x35[3:0]`).
    pub enable: Option<u8>,
    /// Autodetect mode (`+0x35[7:4]`).
    pub autodetect: Option<u8>,
    /// Skip interval (`+0x34[7:5]`, ASK `+0x16[10:8]`).
    pub skip_interval: u8,
    pub skp_interval_sel: Option<u8>,
    pub autodetect_factor: Option<u8>,
    pub lower_skp_os_gen: Option<u8>,
    pub lower_skp_os_rcv: Option<u8>,
}

/// A legacy I²C switch on the way to the slot's hotplug expander
/// (descriptor `+0x39`/`+0x3A`: address bits 5:3, select bits 2:0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct I2cSwitch {
    pub address: u8,
    pub select: u8,
}

/// Slot power limit (descriptor `+0x10`, scale `+0x11[1:0]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotPower {
    pub limit: u8,
    pub scale: u8,
}

/// NPEM (descriptor `+0x3C`/`+0x3E`, 12 bits each).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Npem {
    pub enable: u16,
    pub capability: u16,
}

/// PSPP policy (descriptor `+0x1E`, `+0x20`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pspp {
    pub dc: u16,
    pub ac: u16,
}

/// The port's NBIO interrupt routing (descriptor `+0x15..+0x17`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Apic {
    pub group_map: u8,
    pub swizzle: u8,
    pub bridge_int: u8,
}

/// An ancillary record for the port's ancillary block (MPIO.md §2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AncOverride {
    /// Type 3: a PHY override value.
    Phy(u32),
    /// Type 7: a speed override value.
    Speed(u32),
}

/// One port of a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortDesc<'a> {
    pub engine: Engine,
    /// First socket lane (`0..=135`); `start_lane > end_lane` reverses the link.
    pub start_lane: u8,
    pub end_lane: u8,
    /// Translate as if the core were reversed (Dell parameter `0x31`).
    pub force_core_reversed: bool,
    /// GPIO (PERST#) group (ASK `+0x04[7:0]`); every donor table uses 1.
    pub perst_group: u8,
    /// A device is expected (descriptor `+0x0C` bit 0).
    pub present: bool,
    /// Requested `(device, function)`; `None` keeps the slot's default.
    pub devfn: Option<(u8, u8)>,
    pub max_speed: Speed,
    pub target_speed: Speed,
    /// Train early with the chipset link (descriptor `+0x14` bit 2): target
    /// Gen1, MPIO phase 3.
    pub early_train: bool,
    /// Chipset link type (descriptor `+0x12[15:13]`); 2 marks the ASK early.
    pub cs_link: u8,
    pub compliance: Compliance,
    pub safe_mode: bool,
    pub clk_pm: bool,
    /// CLKREQ# mapping (descriptor `+0x0F[3:0]`).
    pub clkreq: u8,
    pub aspm: Aspm,
    /// SATA channel type (descriptor `+0x14[6:4]`, ASK `+0x04[15:8]`).
    pub channel_type: u8,
    pub turn_off_unused_lanes: bool,
    pub master_pll: bool,
    pub hotplug: Hotplug,
    /// Physical slot number (13 bits), Slot Capabilities `[31:19]`.
    pub slot_num: Option<u16>,
    pub slot_power: Option<SlotPower>,
    pub legacy_i2c: [Option<I2cSwitch>; 2],
    /// UBM host- and drive-facing connector indices (ASK `+0x14`/`+0x15`).
    pub ubm_hfc: Option<u8>,
    pub ubm_dfc: Option<u8>,
    pub npem: Option<Npem>,
    /// Transmitter swing mode (3 bits, ASK `+0x16[7:5]`).
    pub swing_mode: Option<u8>,
    pub invert_rx: bool,
    pub invert_tx: bool,
    pub tx_vetting: bool,
    pub rx_vetting: bool,
    /// Gen2 transmitter de-emphasis override (2 bits).
    pub tx_deemphasis: Option<u8>,
    pub eq: Eq,
    pub sris: Sris,
    pub pspp: Option<Pspp>,
    /// Extended speed mode speed bump (descriptor `+0x19`).
    pub esm: Option<u8>,
    /// CXL on this port (descriptor `+0x1A` bit 0). The CXL owner reads it
    /// to build MPIO's CXL device list; the ASK is a PCIe one.
    pub cxl: bool,
    /// Keep the root port visible when nothing trained (descriptor `+0x3B`).
    pub always_expose: Option<bool>,
    pub apic: Apic,
    /// The port's ancillary records (sent when the platform enables them).
    pub ancillary: &'a [AncOverride],
    /// A UALink port's attributes ([`Engine::UaLink`] only).
    pub ualink: Option<UaLink>,
}

/// Errors in a port description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The engine has no ASK fstart builds.
    Engine(Engine),
    /// A field does not fit its descriptor bits.
    Field(&'static str),
    Lanes(ask::Error),
}

impl PortDesc<'static> {
    /// A present PCIe port on socket lanes `start..=end`, every other
    /// field the vendor's default.
    pub const fn pcie(start: u8, end: u8) -> Self {
        Self {
            engine: Engine::Pcie,
            start_lane: start,
            end_lane: end,
            force_core_reversed: false,
            perst_group: 1,
            present: true,
            devfn: None,
            max_speed: Speed::Max,
            target_speed: Speed::Max,
            early_train: false,
            cs_link: 0,
            compliance: Compliance::Off,
            safe_mode: false,
            clk_pm: false,
            clkreq: 0,
            aspm: Aspm {
                l0s: false,
                l1: false,
                l1_1: false,
                l1_2: false,
            },
            channel_type: 0,
            turn_off_unused_lanes: true,
            master_pll: false,
            hotplug: Hotplug::Disabled,
            slot_num: None,
            slot_power: None,
            legacy_i2c: [None; 2],
            ubm_hfc: None,
            ubm_dfc: None,
            npem: None,
            swing_mode: None,
            invert_rx: false,
            invert_tx: false,
            tx_vetting: false,
            rx_vetting: false,
            tx_deemphasis: None,
            eq: Eq {
                gen3: Gen3Eq {
                    ds_tx_preset: None,
                    ds_rx_hint: None,
                    us_tx_preset: None,
                    us_rx_hint: None,
                },
                gen4: GenEq {
                    ds_tx_preset: None,
                    us_tx_preset: None,
                },
                gen5: GenEq {
                    ds_tx_preset: None,
                    us_tx_preset: None,
                },
                gen6: GenEq {
                    ds_tx_preset: None,
                    us_tx_preset: None,
                },
                fixed_preset: [None; 4],
                preset_mask: [None; 4],
                coefficients: [None; 3],
            },
            sris: Sris {
                enable: None,
                autodetect: None,
                skip_interval: 0,
                skp_interval_sel: None,
                autodetect_factor: None,
                lower_skp_os_gen: None,
                lower_skp_os_rcv: None,
            },
            pspp: None,
            esm: None,
            cxl: false,
            always_expose: None,
            apic: Apic {
                group_map: 0,
                swizzle: 0,
                bridge_int: 0,
            },
            ancillary: &[],
            ualink: None,
        }
    }

    /// A SATA port on socket lanes `start..=end`.
    pub const fn sata(start: u8, end: u8) -> Self {
        Self {
            engine: Engine::Sata,
            ..Self::pcie(start, end)
        }
    }

    /// A USB4 port on socket lanes `start..=end` (a PCIe root port).
    pub const fn usb4(start: u8, end: u8) -> Self {
        Self {
            engine: Engine::Usb4,
            ..Self::pcie(start, end)
        }
    }

    /// A UBM backplane's host-facing connector on socket lanes
    /// `start..=end`: a PCIe port with hotplug type 6 and its UBM host- and
    /// drive-facing connector indices (ASK `+0x14/+0x15`).
    pub const fn ubm(start: u8, end: u8, hfc: u8, dfc: u8) -> Self {
        Self {
            hotplug: Hotplug::Ubm,
            ubm_hfc: Some(hfc),
            ubm_dfc: Some(dfc),
            ..Self::pcie(start, end)
        }
    }

    /// An OCP 3.0 slot: a PCIe port with hotplug type 7; an empty one
    /// (`present` false) gets a zero-width ASK ("Found empty OCP Port").
    pub const fn ocp3(start: u8, end: u8, present: bool) -> Self {
        Self {
            hotplug: Hotplug::Ocp,
            present,
            ..Self::pcie(start, end)
        }
    }

    /// A U.3 (SFF-TA-1001) drive bay: a PCIe port with the enterprise SSD
    /// hotplug type 5, which the SFI hotplug modes extend
    /// ([`super::straps::Policy::sfi`]).
    pub const fn u3(start: u8, end: u8) -> Self {
        Self {
            hotplug: Hotplug::EnterpriseSsd,
            ..Self::pcie(start, end)
        }
    }

    /// A UALink port on socket lanes `start..=end`.
    pub const fn ualink(start: u8, end: u8, ualink: UaLink) -> Self {
        Self {
            engine: Engine::UaLink,
            ualink: Some(ualink),
            ..Self::pcie(start, end)
        }
    }
}

impl<'a> PortDesc<'a> {
    /// Speed capability and target.
    pub const fn speed(mut self, max: Speed, target: Speed) -> Self {
        self.max_speed = max;
        self.target_speed = target;
        self
    }

    /// Requested device and function.
    pub const fn devfn(mut self, device: u8, function: u8) -> Self {
        self.devfn = Some((device, function));
        self
    }

    /// Hotplug type and physical slot number.
    pub const fn slot(mut self, hotplug: Hotplug, slot: u16) -> Self {
        self.hotplug = hotplug;
        self.slot_num = Some(slot);
        self
    }

    /// The port's ancillary records.
    pub const fn with_ancillary<'b>(self, ancillary: &'b [AncOverride]) -> PortDesc<'b> {
        PortDesc { ancillary, ..self }
    }

    /// Lowest and highest socket lane.
    pub const fn lanes(&self) -> (u8, u8) {
        if self.start_lane > self.end_lane {
            (self.end_lane, self.start_lane)
        } else {
            (self.start_lane, self.end_lane)
        }
    }

    /// Wrapper and MPIO lanes under `layout`.
    pub fn mapping(&self, layout: Layout) -> Result<LaneMapping, ask::Error> {
        translate_port(
            layout,
            self.start_lane,
            self.end_lane,
            self.force_core_reversed,
        )
    }

    /// Check every field against its descriptor bits.
    pub fn validate(&self) -> Result<(), Error> {
        let fits = |v: u32, bits: u32, name| {
            if v >> bits == 0 {
                Ok(())
            } else {
                Err(Error::Field(name))
            }
        };
        if !self.engine.has_ask() {
            return Err(Error::Engine(self.engine));
        }
        match (self.engine, &self.ualink) {
            (Engine::UaLink, None) => return Err(Error::Field("ualink")),
            (Engine::UaLink, Some(u)) => {
                fits(u.port.into(), 5, "ualink.port")?;
                fits(u.pstate_count.into(), 3, "ualink.pstate_count")?;
                for p in &u.pstates {
                    fits(p.speed.into(), 5, "ualink.speed")?;
                    fits(p.width.into(), 11, "ualink.width")?;
                }
                if !self.ancillary.is_empty() {
                    return Err(Error::Field("ancillary"));
                }
            }
            (_, Some(_)) => return Err(Error::Field("ualink")),
            (_, None) => {}
        }
        if let Some((d, f)) = self.devfn {
            fits(d.into(), 5, "device")?;
            fits(f.into(), 3, "function")?;
        }
        fits(self.cs_link.into(), 3, "cs_link")?;
        fits(self.clkreq.into(), 4, "clkreq")?;
        fits(self.channel_type.into(), 3, "channel_type")?;
        fits(self.slot_num.unwrap_or(0).into(), 13, "slot_num")?;
        if let Some(p) = self.slot_power {
            fits(p.scale.into(), 2, "slot_power.scale")?;
        }
        for s in self.legacy_i2c.iter().flatten() {
            fits(s.address.into(), 3, "legacy_i2c.address")?;
            fits(s.select.into(), 3, "legacy_i2c.select")?;
        }
        if let Some(n) = self.npem {
            fits(n.enable.into(), 12, "npem.enable")?;
            fits(n.capability.into(), 12, "npem.capability")?;
        }
        fits(self.swing_mode.unwrap_or(0).into(), 3, "swing_mode")?;
        fits(self.tx_deemphasis.unwrap_or(0).into(), 2, "tx_deemphasis")?;
        fits(self.sris.skip_interval.into(), 3, "sris.skip_interval")?;
        let eq = &self.eq;
        for p in [
            eq.gen3.ds_tx_preset,
            eq.gen3.us_tx_preset,
            eq.gen4.ds_tx_preset,
            eq.gen4.us_tx_preset,
            eq.gen5.ds_tx_preset,
            eq.gen5.us_tx_preset,
            eq.gen6.ds_tx_preset,
            eq.gen6.us_tx_preset,
        ]
        .into_iter()
        .chain(eq.fixed_preset)
        .chain(eq.coefficients)
        .flatten()
        {
            fits(p.into(), 4, "eq preset")?;
        }
        for h in [eq.gen3.ds_rx_hint, eq.gen3.us_rx_hint]
            .into_iter()
            .flatten()
        {
            fits(h.into(), 3, "eq rx hint")?;
        }
        for m in eq.preset_mask.into_iter().flatten() {
            fits(m.into(), 10, "eq preset mask")?;
        }
        Ok(())
    }

    /// The ASK of the port (`InitAskEntryAttributes`, Dell `0xf799b0`):
    /// `map` its lanes, `ancillary` the dword index of its ancillary link
    /// record (0: none).
    pub fn ask(&self, map: &LaneMapping, ancillary: u16) -> Result<Ask, Error> {
        self.validate()?;
        let sata = self.engine == Engine::Sata;
        let width = map.width();
        if ![1, 2, 4, 8, 16].contains(&width) {
            return Err(Error::Lanes(ask::Error::Width(width)));
        }
        // An empty OCP slot has no lanes (Dell `0xf7ac50`).
        let width = if self.hotplug == Hotplug::Ocp && !self.present {
            0
        } else {
            width
        };
        let mut lane = LocalRegisterCopy::<u32, Lane::Register>::new(0);
        lane.write(
            Lane::START.val(map.start.into())
                + Lane::COUNT.val(width.into())
                + Lane::REVERSED.val(map.reversed.into())
                + Lane::CONTROLLER.val(self.engine.controller().into()),
        );
        // A UALink ASK: the GPIO group and the UALink attributes only.
        if let (Engine::UaLink, Some(ualink)) = (self.engine, &self.ualink) {
            let mut handle = LocalRegisterCopy::<u32, Handle::Register>::new(0);
            handle.write(Handle::GPIO.val(self.perst_group.into()));
            let mut words = [0; 13];
            words[0] = lane.get();
            words[1] = handle.get();
            words[6..9].copy_from_slice(&ualink.words());
            return Ok(Ask::from_dwords(words));
        }
        let mut handle = LocalRegisterCopy::<u32, Handle::Register>::new(0);
        handle.write(
            if sata {
                Handle::CHANNEL.val(self.channel_type.into())
            } else {
                Handle::GPIO.val(self.perst_group.into())
            } + Handle::ANCILLARY.val(ancillary.into()),
        );
        let (dev, func) = self.devfn.unwrap_or((0, 0));
        let mut device = LocalRegisterCopy::<u32, Device::Register>::new(0);
        device.write(Device::DEVICE.val(dev.into()) + Device::FUNCTION.val(func.into()));
        let target = if self.early_train {
            Speed::Gen1
        } else {
            self.target_speed
        };
        let mut link = LocalRegisterCopy::<u32, Link::Register>::new(0);
        link.write(
            Link::PRESENT.val(self.present.into())
                + Link::EARLY.val(self.early_train.into())
                + Link::COMPLIANCE.val((self.compliance != Compliance::Off).into())
                + Link::HOTPLUG.val(self.hotplug as u32)
                + Link::MAX_SPEED.val(self.max_speed as u32)
                + Link::TARGET_SPEED.val(target as u32)
                + Link::MASTER_PLL.val(self.master_pll.into())
                + Link::RX_POLARITY.val(self.invert_rx.into())
                + Link::TX_POLARITY.val(self.invert_tx.into()),
        );
        let mut phy = LocalRegisterCopy::<u32, Phy::Register>::new(0);
        phy.write(
            Phy::TX_VETTING.val(self.tx_vetting.into())
                + Phy::RX_VETTING.val(self.rx_vetting.into()),
        );
        if let Some(d) = self.tx_deemphasis {
            phy.modify(Phy::DEEMPHASIS_OVERRIDE::SET + Phy::DEEMPHASIS.val(d.into()));
        }
        let mut transport = LocalRegisterCopy::<u32, Transport::Register>::new(0);
        transport.write(
            Transport::HFC.val(self.ubm_hfc.unwrap_or(0).into())
                + Transport::DFC.val(self.ubm_dfc.unwrap_or(0).into())
                + Transport::SWING.val(self.swing_mode.unwrap_or(0).into())
                + Transport::SRIS_SKIP_INTERVAL.val(self.sris.skip_interval.into()),
        );
        let mut attr = LocalRegisterCopy::<u32, Attr::Register>::new(0);
        attr.write(Attr::EARLY.val((self.early_train || self.cs_link == 2).into()));
        let mut words = [0; 13];
        words[..7].copy_from_slice(&[
            lane.get(),
            handle.get(),
            device.get(),
            link.get(),
            phy.get(),
            transport.get(),
            attr.get(),
        ]);
        Ok(Ask::from_dwords(words))
    }

    /// The port's type-3 (PHY) and type-7 (speed) ancillary values.
    pub fn ancillary_values<const N: usize>(
        &self,
    ) -> (heapless::Vec<u32, N>, heapless::Vec<u32, N>) {
        let mut phy = heapless::Vec::new();
        let mut speed = heapless::Vec::new();
        for a in self.ancillary {
            let _ = match *a {
                AncOverride::Phy(v) => phy.push(v),
                AncOverride::Speed(v) => speed.push(v),
            };
        }
        (phy, speed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxio::rs::ask::Kind;

    #[test]
    fn md25_bmc_ask_is_lane_66_reversed_gen3_on_die_1() {
        let bmc = PortDesc::pcie(129, 129)
            .speed(Speed::Gen3, Speed::Gen3)
            .devfn(5, 2);
        let map = bmc.mapping(Layout::Sp7B0).unwrap();
        let ask = bmc.ask(&map, 0).unwrap();
        let w = ask.dwords();
        assert_eq!(w[0], 66 | 1 << 16 | 1 << 22);
        assert_eq!(w[1], 1);
        assert_eq!(w[2], 5 << 3 | 2);
        assert_eq!(w[3], 1 | 3 << 8 | 3 << 12);
        assert_eq!(ask.kind(), Some(Kind::Pcie));
        assert_eq!(map.iod, 1);
    }

    #[test]
    fn every_ask_field_lands_on_the_donor_bit() {
        let port = PortDesc {
            early_train: true,
            compliance: Compliance::On,
            hotplug: Hotplug::EnterpriseSsd,
            master_pll: true,
            invert_rx: true,
            invert_tx: true,
            tx_vetting: true,
            rx_vetting: true,
            tx_deemphasis: Some(2),
            ubm_hfc: Some(3),
            ubm_dfc: Some(4),
            swing_mode: Some(5),
            sris: Sris {
                skip_interval: 6,
                ..Sris::default()
            },
            perst_group: 2,
            ..PortDesc::pcie(16, 19).speed(Speed::Gen5, Speed::Gen4)
        };
        let map = port.mapping(Layout::Sp7B0).unwrap();
        let w = port.ask(&map, 0x1234).unwrap().dwords();
        assert_eq!(w[0], 4 << 16);
        assert_eq!(w[1], 0x1234_0002);
        // Early train forces a Gen1 target.
        assert_eq!(w[3], 0b111 | 5 << 4 | 5 << 8 | 1 << 12 | 7 << 26);
        assert_eq!(w[4], 2 << 16 | 1 << 18 | 1 << 19 | 1 << 20);
        assert_eq!(w[5], 3 | 4 << 8 | 5 << 21 | 6 << 24);
        assert_eq!(w[6], 2);
        assert!(w[7..].iter().all(|&x| x == 0));

        let sata = PortDesc {
            channel_type: 5,
            ..PortDesc::sata(64, 67)
        };
        let w = sata
            .ask(&sata.mapping(Layout::Sp7B0).unwrap(), 0)
            .unwrap()
            .dwords();
        assert_eq!(w[0], 4 << 16 | 1 << 28);
        assert_eq!(w[1], 5 << 8);

        let ocp = PortDesc {
            hotplug: Hotplug::Ocp,
            present: false,
            ..PortDesc::pcie(0, 7)
        };
        let w = ocp
            .ask(&ocp.mapping(Layout::Sp7B0).unwrap(), 0)
            .unwrap()
            .dwords();
        assert_eq!(w[0] >> 16 & 0x3f, 0);
    }

    /// USB4 is a PCIe ASK; UBM, OCP3 and U.3 are PCIe ports with their
    /// hotplug type; UALink is controller 3 with its P-states.
    #[test]
    fn every_engine_gets_the_vendors_encoding() {
        let map = PortDesc::pcie(0, 3).mapping(Layout::Sp7B0).unwrap();
        let pcie = PortDesc::pcie(0, 3).ask(&map, 5).unwrap().dwords();
        let usb4 = PortDesc::usb4(0, 3).ask(&map, 5).unwrap().dwords();
        assert_eq!(usb4, pcie);
        let ubm = PortDesc::ubm(0, 3, 2, 9).ask(&map, 0).unwrap().dwords();
        assert_eq!(ubm[0] >> 28, 0);
        assert_eq!(ubm[3] >> 4 & 0xf, 6);
        assert_eq!(ubm[5] & 0xffff, 2 | 9 << 8);
        let ocp = PortDesc::ocp3(0, 3, true).ask(&map, 0).unwrap().dwords();
        assert_eq!((ocp[0] >> 16 & 0x3f, ocp[3] >> 4 & 0xf), (4, 7));
        let u3 = PortDesc::u3(0, 3).ask(&map, 0).unwrap().dwords();
        assert_eq!(u3[3] >> 4 & 0xf, 5);
        let link = UaLink {
            pstates: [
                UaLinkPstate { speed: 5, width: 4 },
                UaLinkPstate { speed: 3, width: 2 },
                UaLinkPstate::default(),
                UaLinkPstate::default(),
            ],
            pstate_count: 2,
            port: 7,
            flags: [true, false, false, true],
            fabric_flag: true,
        };
        let ua = PortDesc::ualink(0, 3, link);
        let w = ua.ask(&map, 0x99).unwrap().dwords();
        assert_eq!(w[0] >> 28, 3);
        assert_eq!(w[1], 1, "GPIO group, no ancillary index");
        assert_eq!(w[6], (4 | 5 << 11) | (2 | 3 << 11) << 16);
        assert_eq!(w[7], 0);
        assert_eq!(w[8], 7 | (2 | 1 << 3 | 1 << 6) << 8 | (3 | 1 << 4) << 16);
        assert!(w[2..6].iter().all(|&x| x == 0));
        assert!(Engine::Usb4.is_pcie() && !Engine::UaLink.is_pcie());
    }

    #[test]
    fn invalid_fields_and_engines_are_refused() {
        let map = PortDesc::pcie(0, 3).mapping(Layout::Sp7B0).unwrap();
        let bad = PortDesc::pcie(0, 3).devfn(32, 0);
        assert_eq!(bad.ask(&map, 0).unwrap_err(), Error::Field("device"));
        let unused = PortDesc {
            engine: Engine::Unused,
            ..PortDesc::pcie(0, 3)
        };
        assert_eq!(unused.validate(), Err(Error::Engine(Engine::Unused)));
        let ualink = PortDesc {
            engine: Engine::UaLink,
            ..PortDesc::pcie(0, 3)
        };
        assert_eq!(ualink.validate(), Err(Error::Field("ualink")));
        let mut eq = PortDesc::pcie(0, 3);
        eq.eq.preset_mask[1] = Some(0x400);
        assert_eq!(eq.validate(), Err(Error::Field("eq preset mask")));
        let three = PortDesc::pcie(0, 2);
        let map = three.mapping(Layout::Sp7B0).unwrap();
        assert_eq!(
            three.ask(&map, 0).unwrap_err(),
            Error::Lanes(ask::Error::Width(3))
        );
    }
}
