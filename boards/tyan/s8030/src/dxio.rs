//! S8030 DXIO lane configuration (BOARD_S8030 §3.1–§3.3).
//!
//! One table serves Rome and Milan: the vendor `$A26` templates have the
//! same engines (only the descriptor layout differs). [`TEMPLATE`] lists them
//! in `$A26` order, which the default devfn assignment of the DXIO layer
//! follows; the unused x8/x8/x4 alternative engines of each slot are not
//! listed, the OEM bifurcation code only ever turns them on as the split of
//! the x16 they overlap, which is [`Engine::split`].
//!
//! | PHY lanes | Engine | Dev.Fn | Speed | Device |
//! |---|---|---|---|---|
//! | 0–5 | SATA x6 | | | SATA 0–5 (J14/J22, J25) |
//! | 8–11, 12–15 | PCIe x4, x4 | 3.1, 3.2 | auto | CN2 SlimSAS: NVMe0/1 |
//! | 31–16 | PCIe x16 | auto | auto | PCIE#3 |
//! | 32–39 | SATA x8 | | | SATA 6–13 (J26/J27) |
//! | 40–43, 44–47 | PCIe x4, x4 | 1.1, 1.2 | auto | CN4 SlimSAS: NVMe2/3 |
//! | 48–63 | PCIe x16 | auto | auto | PCIE#5 |
//! | 95–80 | PCIe x16 | auto | auto | PCIE#2 |
//! | 64 / 64–67 | SATA x1 / PCIe x4 | – / 3.2 | auto | M.2 CN1 (GPIO144) |
//! | 68 / 68–71 | SATA x1 / PCIe x4 | – / 3.3 | auto | M.2 CN3 (GPIO140) |
//! | 72 | PCIe x1 | 3.1 | Gen2 | AST2500 VGA |
//! | 74, 75 | PCIe x1, x1 | 3.5, 3.6 | Gen1 | i210 LAN1, LAN2 |
//! | 76–79 | PCIe x4 | 3.4 | Gen3 | X550-BT2 (GM4NE-2T; unpopulated on GM2NE) |
//! | 127–112 | PCIe x16 | auto | auto | PCIE#1 |
//! | 111–96 | PCIe x16 | auto | auto | PCIE#4 |
//!
//! Every PCIe engine turns off unused lanes (MiscControls bit 7). The i210
//! and X550 rows also set bit 1, openSIL `LinkSafeMode` (Gen1 limit),
//! which the Rome builder does not read; they are not early-train links
//! (that is bit 2, `SbLink`, Rome `ffe02ae4`). No engine is hotpluggable (the
//! ServerHotplugDxe table is an AMD sample that does not match the board)
//! and none carries a slot number. All engines are socket 0; `$A27` is a
//! single unused placeholder.
//!
//! What is on the links ([`fstart_core::smbios::Attached`]), for the SMBIOS
//! tables: the five x16 slots as the vendor's own SMBIOS data names them
//! (`PCIE1`..`PCIE5`, Slot IDs 0-4, long), the two SlimSAS connectors
//! (two x4 links each), the two M.2 sockets, the BMC's display and the
//! i210s. The X550 is left out: nothing tells the GM4NE-2T, which has it,
//! from the GM2NE.
//!
//! [`engines`] applies the OEM runtime patches in vendor order
//! (AmdCpmTyanInitPeim `ffe45af4` bifurcation, `ffe44882` speed, `ffe449af`
//! compliance, `ffe456da` LAN, `ffe4611f` VGA, `ffe45e98` M.2, `ffe45ff1`
//! BMC; Milan OemAmdCpmInitPeim runs the same steps in another order on
//! disjoint fields).

mod tuning;

use fstart_core::smbios::{Attached, OnboardDevice, OnboardType, SystemSlot};
use fstart_driver_amd::dxio::wire::MAX_ENGINES;
use fstart_driver_amd::sp3::dxio::lanes::{
    Bifurcation, Bifurcations, Engine, Group, Lanes, PcieEngine, SataEngine, Speed, validate,
};
use fstart_driver_amd::sp3::soc::Generation;
use fstart_options::{Knob, Meta};
use heapless::Vec;

/// A x16 slot `PCIEn`: Gen4 on Rome and Milan and as wide electrically as
/// physically (type B8h with the x16 data bus width; the vendor's A5h is
/// Gen1), long, 3.3 V, split by its bifurcation knob.
const fn x16_slot(designation: &'static str, id: u16) -> Attached<'static> {
    Attached::Slot(SystemSlot {
        designation,
        slot_type: 0xb8,
        data_bus_width: 0x0d,
        length: 0x04,
        id,
        characteristics: [0x04, 0x08],
    })
}

/// `PCIE1`..`PCIE5`.
static SLOTS: [Attached<'static>; 5] = [
    x16_slot("PCIE1", 0),
    x16_slot("PCIE2", 1),
    x16_slot("PCIE3", 2),
    x16_slot("PCIE4", 3),
    x16_slot("PCIE5", 4),
];

/// A SlimSAS x8 connector, two x4 links: a cable goes in it, and SMBIOS
/// has no slot type of its own for it.
const fn slimsas(designation: &'static str) -> Attached<'static> {
    Attached::Slot(SystemSlot {
        designation,
        slot_type: 0x01,
        data_bus_width: 0x0b,
        length: 0x01,
        id: 0,
        characteristics: [0x04, 0],
    })
}

static CN2: Attached<'static> = slimsas("CN2");
static CN4: Attached<'static> = slimsas("CN4");

/// An M.2 socket, M key (Socket 3): PCIe x4 or SATA.
const fn m2(designation: &'static str) -> Attached<'static> {
    Attached::Slot(SystemSlot {
        designation,
        slot_type: 0x17,
        data_bus_width: 0x0a,
        length: 0x02,
        id: 0,
        characteristics: [0x04, 0],
    })
}

static CN1: Attached<'static> = m2("CN1");
static CN3: Attached<'static> = m2("CN3");

static BMC_VGA: Attached<'static> = Attached::Onboard(OnboardDevice {
    designation: "BMC VGA",
    kind: OnboardType::Video,
});
static LAN: [Attached<'static>; 2] = [
    Attached::Onboard(OnboardDevice {
        designation: "LAN1",
        kind: OnboardType::Ethernet,
    }),
    Attached::Onboard(OnboardDevice {
        designation: "LAN2",
        kind: OnboardType::Ethernet,
    }),
];

/// The five x16 slots, in Setup order (`PCIE#1..5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Pcie1,
    Pcie2,
    Pcie3,
    Pcie4,
    Pcie5,
}

impl Slot {
    pub const ALL: [Slot; 5] = [
        Self::Pcie1,
        Self::Pcie2,
        Self::Pcie3,
        Self::Pcie4,
        Self::Pcie5,
    ];

    /// Index into [`Options::slots`] and the Setup slot arrays.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The slot's x16 lanes in board order (the OEM slot rows: x16 at
    /// 127/95/31/111/48).
    pub const fn lanes(self) -> Lanes {
        match self {
            Self::Pcie1 => Lanes::sp3(Group::P3, 0, 16).reversed(),
            Self::Pcie2 => Lanes::sp3(Group::P2, 0, 16).reversed(),
            Self::Pcie3 => Lanes::sp3(Group::G0, 0, 16).reversed(),
            Self::Pcie4 => Lanes::sp3(Group::G3, 0, 16).reversed(),
            Self::Pcie5 => Lanes::sp3(Group::G1, 0, 16),
        }
    }

    /// Manual connector, assuming manual slots A–E are `PCIE#1..5`
    /// (inferred, §3.2).
    pub const fn connector(self) -> &'static str {
        match self {
            Self::Pcie1 => "J12",
            Self::Pcie2 => "J9",
            Self::Pcie3 => "J11",
            Self::Pcie4 => "J10",
            Self::Pcie5 => "J13",
        }
    }

    const fn engine(self) -> Engine<'static> {
        Engine::pcie(0, self.lanes(), PcieEngine::new().turn_off_unused())
            .attached(&SLOTS[self.index()])
    }
}

/// Slot bifurcation (Rome `Setup[0x194 + slot]`, Milan
/// `OemPciSlotConfigSetup[slot]`; default x16) from its Setup value: 1, 2
/// or 4 (the IFR offers only these). Values 0 (GPIO detect) and 3 (GPIO
/// low, then x8x8) need a slot GPIO flag no S8030 row has; Milan's cases
/// 5–11 are not reachable from Setup.
pub const fn bifurcation_from_setup(value: u8) -> Option<Bifurcation> {
    match value {
        1 => Some(Bifurcation::X16),
        2 => Some(Bifurcation::X8x8),
        4 => Some(Bifurcation::X4x4x4x4),
        _ => None,
    }
}

/// Per-slot policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotPolicy {
    pub bifurcation: Bifurcation,
    /// Link speed cap of every port of the slot (Rome `Setup[0x199 + slot]`,
    /// Milan `OemPciSlotConfigSetup[5 + slot]`; 0 = auto, 1–4 = Gen1–4).
    pub max_speed: Speed,
}

/// The board knobs behind the OEM patches; [`Default`] is the S8030 setup
/// default (`setup-defaults-{rome,milan}-s8030-v404.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub slots: [SlotPolicy; 5],
    /// "PCIe Compliance Mode" (Rome `Setup[0x19e]`, Milan
    /// `OemPciSlotConfigSetup[10]`; default off). Milan sets the compliance
    /// bit on every PCIe engine but the BMC's; Rome only on the slots, and
    /// into MiscControls bit 0, which the Rome builder copies to the wire
    /// compliance bit. Here both set the compliance bit with the vendor's
    /// scope.
    pub compliance: bool,
    /// i210 LAN1/LAN2 ports present (Rome `Setup[0x1b9/0x1ba]`, Milan
    /// `Setup[0x16a/0x16b]`; default on).
    pub lan: [bool; 2],
    /// "Onboard VGA": the BMC port present (Rome `Setup[0x1c2]`, Milan
    /// `Setup[0x170]`; default on).
    pub onboard_vga: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Options {
    pub const DEFAULT: Self = Self {
        slots: [SlotPolicy {
            bifurcation: Bifurcation::X16,
            max_speed: Speed::Auto,
        }; 5],
        compliance: false,
        lan: [true; 2],
        onboard_vga: true,
    };

    /// The options of this build: the Setup defaults, or the
    /// `pcie3-x4x4x4x4` variant's PCIE#3 x4x4x4x4 (the bench's NVMe carrier; its vendor
    /// NVRAM has `Setup[0x194..]` = `01 01 04 01 01`). The settings menu
    /// overrides them per machine ([`KNOBS`]).
    pub const BUILD: Self = if cfg!(feature = "pcie3-x4x4x4x4") {
        Self::DEFAULT.with_bifurcation("11411")
    } else {
        Self::DEFAULT
    };

    /// Slot bifurcation from five Setup values (`1`, `2` or `4`, in
    /// `PCIE#1..5` order); anything else panics (fails const evaluation).
    pub const fn with_bifurcation(self, setup: &str) -> Self {
        let b = setup.as_bytes();
        assert!(b.len() == 5, "five slot values, PCIE#1..5");
        let mut s = self;
        let mut i = 0;
        while i < 5 {
            s.slots[i].bifurcation = match bifurcation_from_setup(b[i].wrapping_sub(b'0')) {
                Some(x) => x,
                None => panic!("slot bifurcation is 1, 2 or 4"),
            };
            i += 1;
        }
        s
    }
}

/// The slots' bifurcation is their Setup value; there is no Auto.
impl Bifurcations for Options {
    fn bifurcation(&self, index: usize) -> Option<Bifurcation> {
        Some(self.slots[index].bifurcation)
    }
    fn set_bifurcation(&mut self, index: usize, bifurcation: Option<Bifurcation>) {
        if let Some(b) = bifurcation {
            self.slots[index].bifurcation = b;
        }
    }
}

/// The Setup knobs behind the OEM patches, as user options.
pub static KNOBS: [Knob<Options>; 9] = [
    Bifurcation::knob::<Options, { Slot::Pcie1.index() }>(
        "s8030_pcie1_bifurcation",
        "PCIE#1 (J12) bifurcation",
        false,
    ),
    Bifurcation::knob::<Options, { Slot::Pcie2.index() }>(
        "s8030_pcie2_bifurcation",
        "PCIE#2 (J9) bifurcation",
        false,
    ),
    Bifurcation::knob::<Options, { Slot::Pcie3.index() }>(
        "s8030_pcie3_bifurcation",
        "PCIE#3 (J11) bifurcation",
        false,
    ),
    Bifurcation::knob::<Options, { Slot::Pcie4.index() }>(
        "s8030_pcie4_bifurcation",
        "PCIE#4 (J10) bifurcation",
        false,
    ),
    Bifurcation::knob::<Options, { Slot::Pcie5.index() }>(
        "s8030_pcie5_bifurcation",
        "PCIE#5 (J13) bifurcation",
        false,
    ),
    Knob::new(
        Meta::bool(
            "s8030_pcie_compliance",
            "PCIe",
            "PCIe compliance mode",
            "Sets the compliance bit of the PCIe engines, for link testing.",
        ),
        |o| o.compliance.into(),
        |o, v| o.compliance = v != 0,
    ),
    Knob::new(
        Meta::bool(
            "s8030_lan1",
            "PCIe",
            "Onboard LAN1 (i210)",
            "Trains the link of the first i210.",
        ),
        |o| o.lan[0].into(),
        |o, v| o.lan[0] = v != 0,
    ),
    Knob::new(
        Meta::bool(
            "s8030_lan2",
            "PCIe",
            "Onboard LAN2 (i210)",
            "Trains the link of the second i210.",
        ),
        |o| o.lan[1].into(),
        |o, v| o.lan[1] = v != 0,
    ),
    Knob::new(
        Meta::bool(
            "s8030_onboard_vga",
            "PCIe",
            "Onboard VGA (AST2500)",
            "Trains the link of the BMC's VGA.",
        ),
        |o| o.onboard_vga.into(),
        |o, v| o.onboard_vga = v != 0,
    ),
];

/// Whether `options` give the SoC a valid engine list with `detected`.
pub fn check(
    generation: Generation,
    options: &Options,
    detected: &Detected,
) -> Result<(), &'static str> {
    validate(
        generation.silicon(),
        &engines(generation, options, detected),
    )
    .map_err(|_| "the lane map does not validate")
}

/// The M.2 sockets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum M2Socket {
    /// Lanes 64–67, detect GPIO144.
    Cn1,
    /// Lanes 68–71, detect GPIO140.
    Cn3,
}

/// What an M.2 socket carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum M2Mode {
    /// PCIe x4.
    Pcie,
    /// SATA on the socket's first lane.
    Sata,
}

impl M2Mode {
    /// From the detect pin ([`crate::gpio::m2_detect_pads`]): high = PCIe.
    pub const fn from_detect(high: bool) -> Self {
        if high { Self::Pcie } else { Self::Sata }
    }
}

/// Runtime inputs of the patches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    /// CN1, CN3.
    pub m2: [M2Mode; 2],
    /// The BMC answered the Tyan OEM M.2 query with 0
    /// ([`crate::bmc::m2_gen3_cap`]): cap the M.2 links at Gen3.
    pub m2_gen3: bool,
}

/// What a fixed engine is, for the patch scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Storage,
    Bmc,
    Lan(usize),
    Nic,
}

#[derive(Debug, Clone, Copy)]
enum Row {
    Fixed(Role, Engine<'static>),
    Slot(Slot),
    M2(M2Socket),
}

const fn port(dev: u8, func: u8) -> PcieEngine {
    PcieEngine::new().devfn(dev, func).turn_off_unused()
}

const fn pcie(group: Group, first: u8, count: u8, pcie: PcieEngine) -> Engine<'static> {
    Engine::pcie(0, Lanes::sp3(group, first, count), pcie)
}

const fn sata(group: Group, first: u8, count: u8, tuning: &'static [(u8, u8)]) -> Engine<'static> {
    Engine::sata(0, Lanes::sp3(group, first, count), SataEngine::new(0)).tuning(tuning)
}

/// The i210/X550 rows: MiscControls `0x82` (safe mode, turn off unused
/// lanes), no early training.
const fn lan(
    role: Role,
    group: Group,
    first: u8,
    count: u8,
    dev: u8,
    func: u8,
    speed: Speed,
) -> Row {
    let engine = pcie(group, first, count, port(dev, func).max_speed(speed));
    match role {
        Role::Lan(i) => Row::Fixed(role, engine.attached(&LAN[i])),
        _ => Row::Fixed(role, engine),
    }
}

/// An x4 link of the SlimSAS connector `connector`.
const fn slimsas_link(
    group: Group,
    first: u8,
    dev: u8,
    func: u8,
    connector: &'static Attached<'static>,
) -> Row {
    Row::Fixed(
        Role::Storage,
        pcie(group, first, 4, port(dev, func)).attached(connector),
    )
}

/// The `$A26` template (Rome `0xffe46fb8`, Milan `0xffd99094`; observed
/// bytes, identical engines).
static TEMPLATE: [Row; 17] = [
    Row::Fixed(Role::Storage, sata(Group::P0, 0, 6, tuning::SATA_0_5)),
    slimsas_link(Group::P0, 8, 3, 1, &CN2),
    slimsas_link(Group::P0, 12, 3, 2, &CN2),
    Row::Slot(Slot::Pcie3),
    Row::Fixed(Role::Storage, sata(Group::P1, 0, 8, tuning::SATA_32_39)),
    slimsas_link(Group::P1, 8, 1, 1, &CN4),
    slimsas_link(Group::P1, 12, 1, 2, &CN4),
    Row::Slot(Slot::Pcie5),
    Row::Slot(Slot::Pcie2),
    Row::M2(M2Socket::Cn1),
    Row::M2(M2Socket::Cn3),
    Row::Fixed(
        Role::Bmc,
        pcie(Group::G2, 8, 1, port(3, 1).max_speed(Speed::Gen2)).attached(&BMC_VGA),
    ),
    lan(Role::Lan(0), Group::G2, 10, 1, 3, 5, Speed::Gen1),
    lan(Role::Lan(1), Group::G2, 11, 1, 3, 6, Speed::Gen1),
    lan(Role::Nic, Group::G2, 12, 4, 3, 4, Speed::Gen3),
    Row::Slot(Slot::Pcie1),
    Row::Slot(Slot::Pcie4),
];

impl M2Socket {
    /// In SATA mode the detect pin saw a SATA module: the socket is in use
    /// without a PCIe link.
    const fn engine(self, mode: M2Mode) -> Engine<'static> {
        match (self, mode) {
            (Self::Cn1, M2Mode::Pcie) => pcie(Group::G2, 0, 4, port(3, 2)).attached(&CN1),
            (Self::Cn3, M2Mode::Pcie) => pcie(Group::G2, 4, 4, port(3, 3)).attached(&CN3),
            (Self::Cn1, M2Mode::Sata) => sata(Group::G2, 0, 1, tuning::SATA_64).attached(&CN1),
            (Self::Cn3, M2Mode::Sata) => sata(Group::G2, 4, 1, tuning::SATA_68).attached(&CN3),
        }
    }
}

/// The engines of one template row after the OEM patches.
fn expand(
    generation: Generation,
    options: &Options,
    detected: &Detected,
    row: &Row,
) -> Vec<Engine<'static>, 4> {
    let milan = generation == Generation::Milan;
    match *row {
        Row::Fixed(role, engine) => {
            let engine = match role {
                Role::Lan(i) => engine.with_present(options.lan[i]),
                Role::Bmc => engine.with_present(options.onboard_vga),
                Role::Storage | Role::Nic => engine,
            };
            // Milan: every PCIe engine but the BMC's (`ffd974f3` skips lane 72).
            let compliance = options.compliance && milan && role != Role::Bmc;
            core::iter::once(engine.with_compliance(compliance)).collect()
        }
        Row::Slot(slot) => {
            let policy = options.slots[slot.index()];
            slot.engine()
                .split(policy.bifurcation.width())
                .map(|e| {
                    e.with_max_speed(policy.max_speed)
                        .with_compliance(options.compliance)
                })
                .collect()
        }
        Row::M2(socket) => {
            let engine = socket.engine(detected.m2[socket as usize]);
            // The BMC Gen3 rows name lanes 64 and 68 (`0xffe46a64`): the
            // speed field of whichever engine starts there.
            let engine = if detected.m2_gen3 {
                engine.with_max_speed(Speed::Gen3)
            } else {
                engine
            };
            core::iter::once(engine.with_compliance(options.compliance && milan)).collect()
        }
    }
}

/// The socket-0 engine list for the DXIO layer (`dxio::Unit::new`), with
/// the board knobs and the runtime M.2 / BMC inputs applied.
pub fn engines(
    generation: Generation,
    options: &Options,
    detected: &Detected,
) -> Vec<Engine<'static>, MAX_ENGINES> {
    TEMPLATE
        .iter()
        .flat_map(|row| expand(generation, options, detected, row))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_driver_amd::sp3::dxio::lanes::{EngineKind, place};

    const PCIE: Detected = Detected {
        m2: [M2Mode::Pcie; 2],
        m2_gen3: false,
    };

    fn phys(generation: Generation, e: &[Engine<'_>]) -> std::vec::Vec<(u8, u8)> {
        e.iter()
            .map(|e| place(generation.silicon(), e).unwrap().phy)
            .collect()
    }

    fn pcie_of(e: &Engine<'_>) -> PcieEngine {
        *e.pcie_config().expect("PCIe engine")
    }

    #[test]
    fn default_lane_map() {
        for g in [Generation::Rome, Generation::Milan] {
            let e = engines(g, &Options::default(), &PCIE);
            assert_eq!(validate(g.silicon(), &e), Ok(()));
            assert_eq!(
                phys(g, &e),
                [
                    (0, 5),
                    (8, 11),
                    (12, 15),
                    (31, 16),
                    (32, 39),
                    (40, 43),
                    (44, 47),
                    (48, 63),
                    (95, 80),
                    (64, 67),
                    (68, 71),
                    (72, 72),
                    (74, 74),
                    (75, 75),
                    (76, 79),
                    (127, 112),
                    (111, 96),
                ]
            );
            let speeds: std::vec::Vec<_> = e[11..15].iter().map(|e| pcie_of(e).max_speed).collect();
            assert_eq!(speeds, [Speed::Gen2, Speed::Gen1, Speed::Gen1, Speed::Gen3]);
            // MiscControls 0x82 is not early training (bit 2 is).
            assert!(
                e.iter()
                    .filter_map(|e| e.pcie_config())
                    .all(|p| !p.early_train)
            );
            assert!(
                e.iter()
                    .filter_map(|e| e.pcie_config())
                    .all(|p| p.turn_off_unused && p.present)
            );
            assert_eq!(e[0].tuning, tuning::SATA_0_5);
        }
    }

    /// Every bifurcation with every M.2 choice is a valid engine list on
    /// both generations. Slots never share a PCIe core, so bifurcating all
    /// five alike covers the mixes.
    #[test]
    fn every_combination_validates() {
        let bifs = [Bifurcation::X16, Bifurcation::X8x8, Bifurcation::X4x4x4x4];
        let m2s = [M2Mode::Pcie, M2Mode::Sata];
        for g in [Generation::Rome, Generation::Milan] {
            for b in bifs {
                let options = Options {
                    slots: [SlotPolicy {
                        bifurcation: b,
                        max_speed: Speed::Gen4,
                    }; 5],
                    ..Options::default()
                };
                for m2 in m2s.iter().flat_map(|&a| m2s.map(|b| [a, b])) {
                    let e = engines(g, &options, &Detected { m2, m2_gen3: true });
                    assert_eq!(validate(g.silicon(), &e), Ok(()), "{g:?} {b:?} {m2:?}");
                }
            }
        }
    }

    #[test]
    fn bifurcation_from_setup_values() {
        let o = Options::DEFAULT.with_bifurcation("11411");
        let b: std::vec::Vec<_> = o.slots.iter().map(|s| s.bifurcation).collect();
        use Bifurcation::*;
        assert_eq!(b, [X16, X16, X4x4x4x4, X16, X16]);
        assert_eq!(
            Options::DEFAULT.with_bifurcation("12111").slots[1].bifurcation,
            X8x8
        );
    }

    /// What the links carry: every link of a split slot its slot, both
    /// x4 links of a SlimSAS connector theirs, an M.2 socket in either
    /// mode, the BMC's display and the i210s; the X550 nothing.
    #[test]
    fn attached_links() {
        let mut options = Options::default();
        options.slots[Slot::Pcie3.index()].bifurcation = Bifurcation::X4x4x4x4;
        let detected = Detected {
            m2: [M2Mode::Pcie, M2Mode::Sata],
            m2_gen3: false,
        };
        let e = engines(Generation::Rome, &options, &detected);
        let on = |attached: &'static Attached<'static>| {
            e.iter()
                .filter(|e| e.attached.is_some_and(|a| core::ptr::eq(a, attached)))
                .count()
        };
        assert_eq!(on(&SLOTS[Slot::Pcie3.index()]), 4);
        assert_eq!(on(&SLOTS[Slot::Pcie1.index()]), 1);
        assert_eq!((on(&CN2), on(&CN4)), (2, 2));
        assert_eq!((on(&CN1), on(&CN3), on(&BMC_VGA)), (1, 1, 1));
        assert_eq!((on(&LAN[0]), on(&LAN[1])), (1, 1));
        let sata_m2 = e
            .iter()
            .find(|e| e.attached.is_some_and(|a| core::ptr::eq(a, &CN3)))
            .unwrap();
        assert!(sata_m2.pcie_config().is_none());
        // The X550 at 3.4 carries nothing.
        assert!(e.iter().all(|e| {
            e.pcie_config().and_then(|p| p.devfn) != Some(3 << 3 | 4) || e.attached.is_none()
        }));
    }

    #[test]
    fn build_options() {
        let pcie3 = Options::BUILD.slots[Slot::Pcie3.index()].bifurcation;
        let want = if cfg!(feature = "pcie3-x4x4x4x4") {
            Bifurcation::X4x4x4x4
        } else {
            Bifurcation::X16
        };
        assert_eq!(pcie3, want);
    }

    #[test]
    fn bifurcation_and_speed_stay_in_the_slot() {
        let mut options = Options::default();
        options.slots[Slot::Pcie1.index()] = SlotPolicy {
            bifurcation: Bifurcation::X4x4x4x4,
            max_speed: Speed::Gen3,
        };
        options.slots[Slot::Pcie5.index()].bifurcation = Bifurcation::X8x8;
        let e = engines(Generation::Rome, &options, &PCIE);
        let p = phys(Generation::Rome, &e);
        // The OEM x4 rows: s, s-4, s-8, s-12 of the reversed slot.
        let at = p.iter().position(|&l| l == (127, 124)).unwrap();
        assert_eq!(
            &p[at..at + 4],
            [(127, 124), (123, 120), (119, 116), (115, 112)]
        );
        assert!(e[at..at + 4].iter().all(|e| pcie_of(e).devfn.is_none()));
        let at5 = p.iter().position(|&l| l == (48, 55)).unwrap();
        assert_eq!(p[at5 + 1], (56, 63));
        // Gen3: the four PCIE#1 ports and the X550 (template speed).
        let gen3: std::vec::Vec<_> = e
            .iter()
            .zip(&p)
            .filter(|(e, _)| e.pcie_config().is_some_and(|p| p.max_speed == Speed::Gen3))
            .map(|(_, &l)| l)
            .collect();
        assert_eq!(
            gen3,
            [(76, 79), (127, 124), (123, 120), (119, 116), (115, 112)]
        );
    }

    const fn devfn(dev: u8, func: u8) -> u8 {
        dev << 3 | func
    }

    #[test]
    fn m2_choice_and_bmc_cap() {
        let detected = Detected {
            m2: [M2Mode::Sata, M2Mode::Pcie],
            m2_gen3: true,
        };
        let e = engines(Generation::Milan, &Options::default(), &detected);
        assert!(matches!(e[9].kind, EngineKind::Sata(_)));
        assert_eq!(
            place(Generation::Milan.silicon(), &e[9]).unwrap().phy,
            (64, 64)
        );
        assert_eq!(e[9].tuning, tuning::SATA_64);
        assert_eq!(pcie_of(&e[10]).max_speed, Speed::Gen3);
        assert_eq!(pcie_of(&e[10]).devfn, Some(devfn(3, 3)));
        assert_eq!(M2Mode::from_detect(true), M2Mode::Pcie);
    }

    #[test]
    fn lan_vga_and_compliance_scopes() {
        let options = Options {
            compliance: true,
            lan: [false, true],
            onboard_vga: false,
            ..Options::default()
        };
        let compliant = |g| -> std::vec::Vec<bool> {
            engines(g, &options, &PCIE)
                .iter()
                .filter_map(|e| e.pcie_config())
                .map(|p| p.extra.compliance)
                .collect()
        };
        // PCIe engines in order: NVMe0/1, PCIE#3, NVMe2/3, PCIE#5, PCIE#2,
        // M.2 CN1/CN3, BMC, LAN1/2, X550, PCIE#1, PCIE#4.
        let slots = [2, 5, 6, 13, 14];
        let rome = compliant(Generation::Rome);
        assert!(
            rome.iter()
                .enumerate()
                .all(|(i, &c)| c == slots.contains(&i))
        );
        let milan = compliant(Generation::Milan);
        assert!(
            milan.iter().enumerate().all(|(i, &c)| c == (i != 9)),
            "all but the BMC"
        );
        let e = engines(Generation::Rome, &options, &PCIE);
        assert!(!pcie_of(&e[11]).present && !pcie_of(&e[12]).present && pcie_of(&e[13]).present);
    }
}
