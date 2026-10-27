//! Typed board lane configuration, lane translation and validation
//! (PCIE_DXIO §3.5, §3.7; BOARD_S8030 §3).
//!
//! Boards describe engines by **lane group** (the SP3 pin-out groups
//! P0–P3, G0–G3 and WAFL on Rome/Milan; a die and its 32 lanes on Naples).
//! [`place`] turns that into the PHY lanes the AGESA board descriptors use
//! and the DXIO lanes the firmware wants, with the recovered SP3 translation
//! ([`SP3_LANE_MAP`], shared by Rome and Milan).
//!
//! Board-time patches (bifurcation, per-slot speed, M.2 PCIe-vs-SATA,
//! compliance, port presence; BOARD §3.3) are expressed by building the
//! engine list at runtime with [`Engine::split`], [`Engine::with_max_speed`]
//! and friends rather than by editing descriptor bytes.

use fstart_pci::devfn;

use super::hotplug::Hotplug;
use super::tables::{NAPLES_DEFAULT_DEVFN, SP3_DEFAULT_DEVFN, SP3_LANE_MAP};
use crate::soc::Generation;

/// SP3 lane groups (Rome/Milan). PHY lane base in the doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// PHY 0x00–0x0F.
    P0,
    /// PHY 0x20–0x2F.
    P1,
    /// PHY 0x50–0x5F.
    P2,
    /// PHY 0x70–0x7F.
    P3,
    /// PHY 0x10–0x1F.
    G0,
    /// PHY 0x30–0x3F.
    G1,
    /// PHY 0x40–0x4F.
    G2,
    /// PHY 0x60–0x6F.
    G3,
    /// PHY 0x80–0x81 (BMC/NIC early links).
    Wafl,
}

/// One PCIe core's lanes in the SP3 map (generated, [`SP3_LANE_MAP`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreLanes {
    pub nbio: u8,
    pub core: u8,
    pub group: Group,
    /// Inclusive PHY lane range.
    pub phy: (u8, u8),
    /// Inclusive DXIO lane range.
    pub dxio: (u8, u8),
    /// The core's lanes run opposite to the PHY numbering.
    pub reversed: bool,
}

impl CoreLanes {
    /// The SP3 core of `group`.
    pub fn of(group: Group) -> &'static CoreLanes {
        SP3_LANE_MAP
            .iter()
            .find(|c| c.group == group)
            .expect("every group has a core")
    }

    /// Lanes in the core.
    pub const fn width(&self) -> u8 {
        self.phy.1 - self.phy.0 + 1
    }

    /// PHY → DXIO (`ffe02666`): normal `x − phyStart + dxioStart`,
    /// reversed `phyStart − x + dxioEnd`.
    pub const fn translate(&self, x: u8) -> u8 {
        if self.reversed {
            self.dxio.1 - (x - self.phy.0)
        } else {
            x - self.phy.0 + self.dxio.0
        }
    }

    /// The core containing PHY lane `x`, if any.
    pub fn containing(x: u8) -> Option<&'static CoreLanes> {
        SP3_LANE_MAP
            .iter()
            .find(|c| (c.phy.0..=c.phy.1).contains(&x))
    }
}

/// Where an engine's lanes are, as the board describes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lanes {
    /// Rome/Milan: `count` lanes from `first` within `group`.
    Sp3 {
        group: Group,
        first: u8,
        count: u8,
        /// Board start lane above the end lane (a reversed link).
        reversed: bool,
    },
    /// Naples: `count` lanes from die lane `first` (0..31; core 0 = 0..15,
    /// core 1 = 16..31).
    Naples {
        die: u8,
        first: u8,
        count: u8,
        reversed: bool,
    },
}

impl Lanes {
    pub const fn sp3(group: Group, first: u8, count: u8) -> Self {
        Self::Sp3 {
            group,
            first,
            count,
            reversed: false,
        }
    }

    pub const fn naples(die: u8, first: u8, count: u8) -> Self {
        Self::Naples {
            die,
            first,
            count,
            reversed: false,
        }
    }

    /// The same lanes as a reversed link.
    pub const fn reversed(self) -> Self {
        match self {
            Self::Sp3 {
                group,
                first,
                count,
                ..
            } => Self::Sp3 {
                group,
                first,
                count,
                reversed: true,
            },
            Self::Naples {
                die, first, count, ..
            } => Self::Naples {
                die,
                first,
                count,
                reversed: true,
            },
        }
    }

    pub const fn first(&self) -> u8 {
        match *self {
            Self::Sp3 { first, .. } | Self::Naples { first, .. } => first,
        }
    }

    pub const fn count(&self) -> u8 {
        match *self {
            Self::Sp3 { count, .. } | Self::Naples { count, .. } => count,
        }
    }

    pub const fn is_reversed(&self) -> bool {
        match *self {
            Self::Sp3 { reversed, .. } | Self::Naples { reversed, .. } => reversed,
        }
    }

    const fn with(self, first: u8, count: u8) -> Self {
        match self {
            Self::Sp3 {
                group, reversed, ..
            } => Self::Sp3 {
                group,
                first,
                count,
                reversed,
            },
            Self::Naples { die, reversed, .. } => Self::Naples {
                die,
                first,
                count,
                reversed,
            },
        }
    }

    /// Split into `width`-lane links, in board order (a reversed link's
    /// highest lanes first, as the OEM bifurcation rows list them).
    pub fn split(self, width: u8) -> impl Iterator<Item = Lanes> {
        let n = self.count().checked_div(width).unwrap_or(0);
        (0..n).map(move |i| {
            let k = if self.is_reversed() { n - 1 - i } else { i };
            self.with(self.first() + k * width, width)
        })
    }
}

/// Naples board lane number: die `n` owns lanes `32n..32n+31` of its
/// socket. Lanes are numbered per socket (0–127), with one DXIO complex per
/// socket on 2P (ROOTBRIDGE_2P §2, decoded from the live `$A27` tables), so
/// the socket is not part of the number.
pub const fn naples_socket_lane(die: u8, lane: u8) -> u16 {
    die as u16 * 32 + lane as u16
}

/// Link speed cap (board `+0xD` bits 5:3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Speed {
    Auto = 0,
    Gen1 = 1,
    Gen2 = 2,
    Gen3 = 3,
    Gen4 = 4,
}

/// PCIe TX/RX equalization presets, written to the core straps
/// 0x9F–0xA4 at state 0xF (Rome `ffdfd4e4`, Milan `ffd0f76b`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EqPresets {
    pub ds_tx: u8,
    pub ds_rx_hint: u8,
    pub us_tx: u8,
    pub us_rx_hint: u8,
    pub ds_tx_16gt: u8,
    pub us_tx_16gt: u8,
}

impl EqPresets {
    /// The port template defaults used when the board gives none (Rome
    /// `ffdfb789`: `+0x41 = 0x1BB7`, `+0x45 = 0x57`); openSIL
    /// `PcieEngineConfigDefaults` and Oxide's Milan straps agree.
    pub const DONOR: Self = Self {
        ds_tx: 7,
        ds_rx_hint: 3,
        us_tx: 7,
        us_rx_hint: 3,
        ds_tx_16gt: 7,
        us_tx_16gt: 5,
    };
}

/// Rarely used PCIe engine fields; all off by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PcieExtra {
    /// Compliance mode (Rome board `+0x12` bit 0, Milan `+0x14` bit 0).
    pub compliance: bool,
    /// CCIX ESM port: forces Gen3 in the record (Rome `ffe02b19`).
    pub esm: bool,
    /// EQ search mode override (0..3).
    pub eq_search_mode: Option<u8>,
    /// TX de-emphasis override (0..3).
    pub deemphasis: Option<u8>,
    pub rx_polarity: bool,
    pub tx_polarity: bool,
    pub tx_vetting: bool,
    pub rx_vetting: bool,
    pub ntb_hotplug: bool,
    pub primary_pll: bool,
    /// Milan target speed (0..7).
    pub target_speed: u8,
    pub skip_eq_gen3: bool,
    pub skip_eq_gen4: bool,
    /// Milan `+0xB` bit 0.
    pub search_depth: bool,
    /// Milan slot type `(u16 & 0xE000) == 0x2000`.
    pub kpnp_reset: bool,
}

/// A PCIe engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcieEngine {
    /// Port present (board `+0xC` bit 0; the S8030 LAN/VGA knobs clear it).
    pub present: bool,
    /// Requested devfn; `None` takes the next free default (§3.8).
    pub devfn: Option<u8>,
    pub max_speed: Speed,
    pub hotplug: Option<Hotplug>,
    /// Physical slot number (bridge cfg `0x6C`[31:19], slot implemented).
    pub slot: Option<u16>,
    pub eq: Option<EqPresets>,
    /// MiscControls bit 7 (set on every S8030 PCIe engine).
    pub turn_off_unused: bool,
    /// MiscControls bit 2 (openSIL `SbLink`): wire caps bit 1, Rome builder
    /// `ffe02ae4`. Bit 1 is `LinkSafeMode`, not early training.
    pub early_train: bool,
    pub extra: PcieExtra,
}

impl Default for PcieEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PcieEngine {
    pub const fn new() -> Self {
        Self {
            present: true,
            devfn: None,
            max_speed: Speed::Auto,
            hotplug: None,
            slot: None,
            eq: None,
            turn_off_unused: false,
            early_train: false,
            extra: PcieExtra {
                compliance: false,
                esm: false,
                eq_search_mode: None,
                deemphasis: None,
                rx_polarity: false,
                tx_polarity: false,
                tx_vetting: false,
                rx_vetting: false,
                ntb_hotplug: false,
                primary_pll: false,
                target_speed: 0,
                skip_eq_gen3: false,
                skip_eq_gen4: false,
                search_depth: false,
                kpnp_reset: false,
            },
        }
    }

    pub const fn devfn(self, device: u8, function: u8) -> Self {
        Self {
            devfn: Some(devfn(device, function)),
            ..self
        }
    }

    pub const fn max_speed(self, max_speed: Speed) -> Self {
        Self { max_speed, ..self }
    }

    pub const fn turn_off_unused(self) -> Self {
        Self {
            turn_off_unused: true,
            ..self
        }
    }

    pub const fn early_train(self) -> Self {
        Self {
            early_train: true,
            ..self
        }
    }

    pub const fn slot(self, slot: u16) -> Self {
        Self {
            slot: Some(slot),
            ..self
        }
    }

    pub const fn eq(self, eq: EqPresets) -> Self {
        Self {
            eq: Some(eq),
            ..self
        }
    }

    pub const fn with_hotplug(self, hotplug: Hotplug) -> Self {
        Self {
            hotplug: Some(hotplug),
            ..self
        }
    }

    pub const fn esm(self) -> Self {
        let mut s = self;
        s.extra.esm = true;
        s
    }

    pub const fn extra(self, extra: PcieExtra) -> Self {
        Self { extra, ..self }
    }
}

/// A SATA engine (wire `+8` channel type: Rome/Milan MiscControls 6:4,
/// Naples `+0x11` 6:5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SataEngine {
    pub channel: u8,
    pub rx_polarity: bool,
    pub tx_polarity: bool,
}

impl SataEngine {
    pub const fn new(channel: u8) -> Self {
        Self {
            channel,
            rx_polarity: false,
            tx_polarity: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Pcie(PcieEngine),
    Sata(SataEngine),
}

/// One board engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Engine<'a> {
    pub socket: u8,
    pub lanes: Lanes,
    /// Board GpioGroupId (1 on every S8030 engine): Naples and Milan wire
    /// reset group.
    pub gpio_group: u8,
    pub kind: EngineKind,
    /// PHY parameter `{id, value}` list (ancillary type 3; S8030 SATA).
    pub tuning: &'a [(u8, u8)],
}

impl<'a> Engine<'a> {
    pub const fn pcie(socket: u8, lanes: Lanes, pcie: PcieEngine) -> Self {
        Self {
            socket,
            lanes,
            gpio_group: 1,
            kind: EngineKind::Pcie(pcie),
            tuning: &[],
        }
    }

    pub const fn sata(socket: u8, lanes: Lanes, sata: SataEngine) -> Self {
        Self {
            socket,
            lanes,
            gpio_group: 1,
            kind: EngineKind::Sata(sata),
            tuning: &[],
        }
    }

    pub const fn tuning(self, tuning: &'a [(u8, u8)]) -> Self {
        Self { tuning, ..self }
    }

    pub fn pcie_config(&self) -> Option<&PcieEngine> {
        match &self.kind {
            EngineKind::Pcie(p) => Some(p),
            EngineKind::Sata(_) => None,
        }
    }

    fn map_pcie(self, f: impl FnOnce(PcieEngine) -> PcieEngine) -> Self {
        match self.kind {
            EngineKind::Pcie(p) => Self {
                kind: EngineKind::Pcie(f(p)),
                ..self
            },
            EngineKind::Sata(_) => self,
        }
    }

    /// The OEM speed knob (BOARD §3.3): a PCIe speed cap, SATA unchanged.
    pub fn with_max_speed(self, max_speed: Speed) -> Self {
        self.map_pcie(|p| PcieEngine { max_speed, ..p })
    }

    /// The OEM LAN/VGA knobs: clear port present.
    pub fn with_present(self, present: bool) -> Self {
        self.map_pcie(|p| PcieEngine { present, ..p })
    }

    pub fn with_compliance(self, compliance: bool) -> Self {
        self.map_pcie(|mut p| {
            p.extra.compliance = compliance;
            p
        })
    }

    /// Bifurcate into `width`-lane PCIe links. The children take the next
    /// free default devfn and no slot/hotplug (the Rome OEM path clears
    /// dev/fn; a slot number on every child would repeat it).
    pub fn split(self, width: u8) -> impl Iterator<Item = Engine<'a>> {
        self.lanes.split(width).map(move |lanes| {
            Engine { lanes, ..self }.map_pcie(|p| PcieEngine {
                devfn: None,
                slot: None,
                hotplug: None,
                ..p
            })
        })
    }
}

/// DXIO lanes of one engine as the firmware sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireLanes {
    /// Low ≤ high.
    pub start: u8,
    pub end: u8,
    pub reverse: bool,
}

/// An engine resolved to its NBIO and PCIe core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// NBIO quadrant (Rome/Milan) or die (Naples) within the socket.
    pub nbio: u8,
    /// PCIe core within the NBIO (Rome/Milan WAFL = 2).
    pub core: u8,
    /// Board-order lanes: PHY lanes (Rome/Milan) or die lanes (Naples);
    /// start > end for a reversed link.
    pub phy: (u8, u8),
    pub wire: WireLanes,
}

/// Why a board engine list is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneError {
    /// Lanes of the other generation's form.
    WrongGeneration,
    /// Socket or die out of range.
    Location,
    /// Empty, or past the end of the group/die.
    Range,
    /// Crosses a PCIe core.
    CrossesCore,
    /// PCIe width not 1/2/4/8/16 (WAFL: 1/2), SATA wider than 8.
    Width,
    /// PCIe link not aligned to its width (inferred hardware rule).
    Alignment,
    /// Shares lanes with engine `.0`.
    Overlap(usize),
    /// More PCIe ports than the core has (8, WAFL 2).
    PortsPerCore,
    /// Requested devfn not in the NBIO's remap defaults.
    Devfn,
    /// Same devfn as engine `.0` on the same NBIO.
    DuplicateDevfn(usize),
    /// SATA on WAFL, or a channel type out of range.
    Sata,
    /// Gen4 on Naples, or an EQ/target-speed field out of range.
    Field,
    /// More engines on one DXIO instance than the heap takes.
    TooManyEngines,
}

/// Resolve an engine's lanes for `generation` (lane translation
/// `ffe02666`: the start lane picks the core; here the end lane is checked
/// too).
pub fn place(generation: Generation, engine: &Engine<'_>) -> Result<Placement, LaneError> {
    if engine.socket as usize >= Generation::MAX_SOCKETS {
        return Err(LaneError::Location);
    }
    let (first, count, reversed) = (
        engine.lanes.first(),
        engine.lanes.count(),
        engine.lanes.is_reversed(),
    );
    if count == 0 {
        return Err(LaneError::Range);
    }
    let last = first.checked_add(count - 1).ok_or(LaneError::Range)?;
    let board = |lo: u8, hi: u8| if reversed { (hi, lo) } else { (lo, hi) };
    match (generation, engine.lanes) {
        (Generation::Rome | Generation::Milan, Lanes::Sp3 { group, .. }) => {
            let core = CoreLanes::of(group);
            if last >= core.width() {
                return Err(LaneError::Range);
            }
            let (lo, hi) = (core.phy.0 + first, core.phy.0 + last);
            let (a, b) = (core.translate(lo), core.translate(hi));
            Ok(Placement {
                nbio: core.nbio,
                core: core.core,
                phy: board(lo, hi),
                wire: WireLanes {
                    start: a.min(b),
                    end: a.max(b),
                    reverse: core.reversed ^ reversed,
                },
            })
        }
        (Generation::Naples, Lanes::Naples { die, .. }) => {
            if die as usize >= Generation::Naples.dies_per_socket() {
                return Err(LaneError::Location);
            }
            if last >= 32 {
                return Err(LaneError::Range);
            }
            if first / 16 != last / 16 {
                return Err(LaneError::CrossesCore);
            }
            // Core 1 (die lanes 16–31) is wire 20–35 (sp4r2 `ffde833a`).
            let shift = if first > 15 { 4 } else { 0 };
            Ok(Placement {
                nbio: die,
                core: first / 16,
                phy: board(first, last),
                wire: WireLanes {
                    start: first + shift,
                    end: last + shift,
                    reverse: reversed,
                },
            })
        }
        _ => Err(LaneError::WrongGeneration),
    }
}

/// The DXIO instance an engine belongs to: the socket on Rome/Milan,
/// `(socket, die)` on Naples.
pub fn unit_of(generation: Generation, engine: &Engine<'_>) -> (u8, u8) {
    match (generation, engine.lanes) {
        (Generation::Naples, Lanes::Naples { die, .. }) => (engine.socket, die),
        _ => (engine.socket, 0),
    }
}

/// NB_PROG_DEVICE_REMAP defaults of one NBIO.
pub fn default_devfns(generation: Generation) -> &'static [u8] {
    match generation {
        Generation::Naples => &NAPLES_DEFAULT_DEVFN,
        Generation::Rome | Generation::Milan => &SP3_DEFAULT_DEVFN,
    }
}

fn check_engine(generation: Generation, e: &Engine<'_>) -> Result<(), LaneError> {
    let count = e.lanes.count();
    let wafl = matches!(
        e.lanes,
        Lanes::Sp3 {
            group: Group::Wafl,
            ..
        }
    );
    match &e.kind {
        EngineKind::Pcie(pcie) => {
            let widths: &[u8] = if wafl { &[1, 2] } else { &[1, 2, 4, 8, 16] };
            if !widths.contains(&count) {
                return Err(LaneError::Width);
            }
            if !e.lanes.first().is_multiple_of(count) {
                return Err(LaneError::Alignment);
            }
            if pcie
                .devfn
                .is_some_and(|d| !default_devfns(generation).contains(&d))
            {
                return Err(LaneError::Devfn);
            }
            let x = &pcie.extra;
            let eq_ok = pcie.eq.is_none_or(|q| {
                q.ds_tx <= 10
                    && q.us_tx <= 10
                    && q.ds_tx_16gt <= 10
                    && q.us_tx_16gt <= 10
                    && q.ds_rx_hint <= 6
                    && q.us_rx_hint <= 6
            });
            let fields_ok = eq_ok
                && x.eq_search_mode.is_none_or(|m| m <= 3)
                && x.deemphasis.is_none_or(|d| d <= 3)
                && x.target_speed <= 7
                && !(generation == Generation::Naples && pcie.max_speed > Speed::Gen3);
            if !fields_ok {
                return Err(LaneError::Field);
            }
        }
        EngineKind::Sata(s) => {
            let max_channel = if generation == Generation::Naples {
                3
            } else {
                7
            };
            if wafl || s.channel > max_channel {
                return Err(LaneError::Sata);
            }
            if count > 8 {
                return Err(LaneError::Width);
            }
        }
    }
    Ok(())
}

/// Validate a board engine list: each engine on its own (ranges, widths,
/// alignment, fields) and against the others (overlaps, ports per core,
/// devfn use per NBIO, engines per DXIO instance). The error carries the
/// index of the offending engine.
pub fn validate(generation: Generation, engines: &[Engine<'_>]) -> Result<(), (usize, LaneError)> {
    let placed = |i: usize| place(generation, &engines[i]).map_err(|e| (i, e));
    for (i, e) in engines.iter().enumerate() {
        let p = placed(i)?;
        check_engine(generation, e).map_err(|err| (i, err))?;
        let unit = unit_of(generation, e);
        // Lanes in one comparable space: wire lanes are unique per unit.
        let span = |p: &Placement| (p.wire.start, p.wire.end);
        for (j, o) in engines.iter().enumerate().take(i) {
            if unit_of(generation, o) != unit {
                continue;
            }
            let q = placed(j)?;
            let ((a0, a1), (b0, b1)) = (span(&p), span(&q));
            if a0 <= b1 && b0 <= a1 {
                return Err((i, LaneError::Overlap(j)));
            }
            let same_nbio = q.nbio == p.nbio;
            if let (Some(x), Some(y)) = (e.pcie_config(), o.pcie_config())
                && same_nbio
                && x.devfn.is_some()
                && x.devfn == y.devfn
            {
                return Err((i, LaneError::DuplicateDevfn(j)));
            }
        }
        if e.pcie_config().is_some() {
            let ports = engines
                .iter()
                .enumerate()
                .take(i + 1)
                .filter(|(_, o)| o.pcie_config().is_some() && unit_of(generation, o) == unit)
                .filter(|&(j, _)| placed(j).is_ok_and(|q| (q.nbio, q.core) == (p.nbio, p.core)))
                .count();
            let max = if p.core == 2 { 2 } else { 8 };
            if ports > max {
                return Err((i, LaneError::PortsPerCore));
            }
        }
        let in_unit = engines
            .iter()
            .take(i + 1)
            .filter(|o| unit_of(generation, o) == unit)
            .count();
        if in_unit > super::engine::MAX_ENGINES {
            return Err((i, LaneError::TooManyEngines));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(g: Group, first: u8, count: u8, rev: bool) -> (Placement, (u8, u8)) {
        let l = Lanes::sp3(g, first, count);
        let l = if rev { l.reversed() } else { l };
        let p = place(Generation::Rome, &Engine::pcie(0, l, PcieEngine::new())).unwrap();
        (p, (p.wire.start, p.wire.end))
    }

    /// PCIE_DXIO §3.7 table, the four reversed cores and WAFL.
    #[test]
    fn sp3_translation() {
        // G0 (0.0, reversed): PHY 16–31 → DXIO 31..16.
        let (p, w) = wire(Group::G0, 0, 16, false);
        assert_eq!(
            (p.nbio, p.core, p.phy, w, p.wire.reverse),
            (0, 0, (16, 31), (16, 31), true)
        );
        // The S8030 PCIE#3 slot: board 31..16 (reversed) cancels the core.
        let (p, _) = wire(Group::G0, 0, 16, true);
        assert_eq!((p.phy, p.wire.reverse), ((31, 16), false));
        // G0 x4 on PHY 16–19 lands at the top of the DXIO range.
        assert_eq!(wire(Group::G0, 0, 4, false).1, (0x1C, 0x1F));
        // G1 (1.1, reversed): PHY 48–63 → DXIO 15..0.
        assert_eq!(wire(Group::G1, 0, 1, false).1, (0x0F, 0x0F));
        assert_eq!(wire(Group::G1, 12, 4, false).1, (0x00, 0x03));
        // P3 (2.1, reversed): PHY 112–127 → 0x69..0x5A.
        assert_eq!(wire(Group::P3, 0, 8, false).1, (0x62, 0x69));
        // P2 (3.0, reversed): PHY 80–95 → 0x59..0x4A.
        assert_eq!(wire(Group::P2, 15, 1, false).1, (0x4A, 0x4A));
        // Normal cores.
        assert_eq!(wire(Group::P0, 8, 4, false).1, (0x32, 0x35));
        assert_eq!(wire(Group::P1, 0, 16, false).1, (0x3A, 0x49));
        assert_eq!(wire(Group::G3, 0, 16, false).1, (0x72, 0x81));
        // S8030 BMC x1 on PHY 72 (G2 = 3.1, normal): 0x82 + 8.
        let (p, w) = wire(Group::G2, 8, 1, false);
        assert_eq!((p.nbio, p.core, p.phy, w), (3, 1, (72, 72), (0x8A, 0x8A)));
        // WAFL: NBIO0 core 2, PHY 0x80–0x81 → 0x24–0x25.
        let (p, w) = wire(Group::Wafl, 0, 2, false);
        assert_eq!(
            (p.nbio, p.core, p.phy, w),
            (0, 2, (0x80, 0x81), (0x24, 0x25))
        );
        // Every core maps its PHY range onto its DXIO range.
        for c in SP3_LANE_MAP {
            let ends = (c.translate(c.phy.0), c.translate(c.phy.1));
            let d = if c.reversed {
                (c.dxio.1, c.dxio.0)
            } else {
                c.dxio
            };
            assert_eq!(ends, d, "{:?}", c.group);
        }
    }

    /// The S8030 `$A26` lane map (BOARD_S8030 §3.2) in typed form, with the
    /// M.2 PCIe/SATA choice and slot bifurcation the OEM code applies at
    /// runtime (§3.3).
    fn s8030(m2_pcie: [bool; 2], pcie1: u8) -> std::vec::Vec<Engine<'static>> {
        use Group::*;
        let slot = |g, rev: bool, n: u16| {
            let l = Lanes::sp3(g, 0, 16);
            Engine::pcie(
                0,
                if rev { l.reversed() } else { l },
                PcieEngine::new().turn_off_unused().slot(n),
            )
        };
        let dev = |g, f, c, d, func| {
            Engine::pcie(
                0,
                Lanes::sp3(g, f, c),
                PcieEngine::new().devfn(d, func).turn_off_unused(),
            )
        };
        let mut v = vec![
            Engine::sata(0, Lanes::sp3(P0, 0, 6), SataEngine::new(0)),
            dev(P0, 8, 4, 3, 1),
            dev(P0, 12, 4, 3, 2),
            slot(G0, true, 3),
            Engine::sata(0, Lanes::sp3(P1, 0, 8), SataEngine::new(0)),
            dev(P1, 8, 4, 1, 1),
            dev(P1, 12, 4, 1, 2),
            slot(G1, false, 5),
            dev(G2, 8, 1, 3, 1).with_max_speed(Speed::Gen2),
            Engine::pcie(
                0,
                Lanes::sp3(G2, 10, 1),
                PcieEngine::new()
                    .devfn(3, 5)
                    .max_speed(Speed::Gen1)
                    .early_train(),
            ),
            Engine::pcie(
                0,
                Lanes::sp3(G2, 11, 1),
                PcieEngine::new()
                    .devfn(3, 6)
                    .max_speed(Speed::Gen1)
                    .early_train(),
            ),
            Engine::pcie(
                0,
                Lanes::sp3(G2, 12, 4),
                PcieEngine::new()
                    .devfn(3, 4)
                    .max_speed(Speed::Gen3)
                    .early_train(),
            ),
            slot(P2, true, 2),
            slot(G3, true, 4),
        ];
        for (i, pcie) in m2_pcie.into_iter().enumerate() {
            let first = 4 * i as u8;
            v.push(if pcie {
                dev(G2, first, 4, 3, 2 + i as u8)
            } else {
                Engine::sata(0, Lanes::sp3(G2, first, 1), SataEngine::new(0))
            });
        }
        v.extend(slot(P3, true, 1).split(16 / pcie1));
        v
    }

    #[test]
    fn s8030_lane_map() {
        for m2 in [[true, true], [false, true], [false, false]] {
            for split in [1, 2, 4] {
                assert_eq!(
                    validate(Generation::Rome, &s8030(m2, split)),
                    Ok(()),
                    "{m2:?} x{split}"
                );
                assert_eq!(validate(Generation::Milan, &s8030(m2, split)), Ok(()));
            }
        }
        let b = s8030([true, false], 1);
        let phy = |i: usize| place(Generation::Rome, &b[i]).unwrap().phy;
        assert_eq!(
            [phy(1), phy(3), phy(8), phy(9), phy(11), phy(13)],
            [(8, 11), (31, 16), (72, 72), (74, 74), (76, 79), (111, 96)]
        );
        assert_eq!(phy(15), (68, 68), "M.2 CN3 as SATA x1");
        assert_eq!(phy(16), (127, 112), "PCIE#1");
    }

    #[test]
    fn naples_die_lanes() {
        let p = |first, count| {
            place(
                Generation::Naples,
                &Engine::pcie(0, Lanes::naples(2, first, count), PcieEngine::new()),
            )
        };
        let a = p(0, 8).unwrap();
        assert_eq!((a.nbio, a.core, a.wire.start, a.wire.end), (2, 0, 0, 7));
        let b = p(16, 16).unwrap();
        assert_eq!((b.core, b.wire.start, b.wire.end), (1, 20, 35));
        assert_eq!(p(12, 8), Err(LaneError::CrossesCore));
        assert_eq!(naples_socket_lane(2, 5), 64 + 5);
        let sp3 = Engine::pcie(0, Lanes::sp3(Group::P0, 0, 4), PcieEngine::new());
        assert_eq!(
            place(Generation::Naples, &sp3),
            Err(LaneError::WrongGeneration)
        );
    }

    #[test]
    fn bifurcation_split() {
        let slot = Engine::pcie(
            0,
            Lanes::sp3(Group::G0, 0, 16).reversed(),
            PcieEngine::new().slot(3),
        );
        let x8: Vec<_> = slot
            .split(8)
            .map(|e| place(Generation::Rome, &e).unwrap().phy)
            .collect();
        assert_eq!(x8, [(31, 24), (23, 16)], "OEM rows x8a 31, x8b 23");
        let x4: Vec<_> = slot.split(4).collect();
        assert_eq!(x4.len(), 4);
        assert!(x4.iter().all(|e| e.pcie_config().unwrap().slot.is_none()));
        assert_eq!(validate(Generation::Rome, &x4), Ok(()));
        let gen3 = slot.with_max_speed(Speed::Gen3);
        assert_eq!(gen3.pcie_config().unwrap().max_speed, Speed::Gen3);
    }

    #[test]
    fn validation_rules() {
        let pcie = |g, f, c| Engine::pcie(0, Lanes::sp3(g, f, c), PcieEngine::new());
        let v = |e: &[Engine<'_>]| validate(Generation::Milan, e);
        assert_eq!(v(&[pcie(Group::P0, 0, 3)]), Err((0, LaneError::Width)));
        assert_eq!(v(&[pcie(Group::P0, 2, 4)]), Err((0, LaneError::Alignment)));
        assert_eq!(v(&[pcie(Group::P0, 8, 16)]), Err((0, LaneError::Range)));
        assert_eq!(v(&[pcie(Group::Wafl, 0, 4)]), Err((0, LaneError::Range)));
        assert_eq!(
            v(&[pcie(Group::P0, 0, 8), pcie(Group::P0, 4, 4)]),
            Err((1, LaneError::Overlap(0)))
        );
        let nine: Vec<_> = (0..9).map(|i| pcie(Group::P1, i, 1)).collect();
        assert_eq!(v(&nine), Err((8, LaneError::PortsPerCore)));
        let dev = |g, f| Engine::pcie(0, Lanes::sp3(g, f, 4), PcieEngine::new().devfn(3, 1));
        // 3.1 twice on one NBIO (P0 and G0 are both NBIO0) fails; the S8030
        // reuses 3.1 on NBIO0 (lanes 8) and NBIO3 (lane 72).
        assert_eq!(
            v(&[dev(Group::P0, 8), dev(Group::G0, 0)]),
            Err((1, LaneError::DuplicateDevfn(0)))
        );
        assert_eq!(v(&[dev(Group::P0, 8), dev(Group::G2, 8)]), Ok(()));
        let bad = Engine::pcie(
            0,
            Lanes::sp3(Group::P0, 0, 1),
            PcieEngine::new().devfn(7, 1),
        );
        assert_eq!(v(&[bad]), Err((0, LaneError::Devfn)));
        let sata = Engine::sata(0, Lanes::sp3(Group::Wafl, 0, 1), SataEngine::new(0));
        assert_eq!(v(&[sata]), Err((0, LaneError::Sata)));
        // Socket 1 has its own lane space.
        let s1 = Engine::pcie(1, Lanes::sp3(Group::P0, 0, 8), PcieEngine::new());
        assert_eq!(v(&[pcie(Group::P0, 0, 8), s1]), Ok(()));
        let g4 = Engine::pcie(
            0,
            Lanes::naples(0, 0, 4),
            PcieEngine::new().max_speed(Speed::Gen4),
        );
        assert_eq!(
            validate(Generation::Naples, &[g4]),
            Err((0, LaneError::Field))
        );
    }
}
