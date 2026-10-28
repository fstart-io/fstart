//! The DXIO firmware wire format: the 0x28-byte engine record, the engine
//! configuration heap (heap 3) and the ancillary heap (heap 6)
//! (PCIE_DXIO §3.6).
//!
//! The record layout is shared by all three generations; the per-gen
//! encoding deltas (reset group, bdf, EQ fields, SATA channel width) live in
//! [`encode`]. Field names follow Oxide's public `zen/dxio.h`; the bits are
//! re-derived from the donor builders:
//!
//! - Rome `AmdNbioPciePei` 21be6180, sha256 d64860daa843, `ffe027e4`;
//! - Milan `AmdNbioPciePei` 3057f0e9, sha256 08e59d623e75, `ffd1509d`;
//! - Naples `AmdNbioPcieZPPei` b268252e (sp4r2 `ffde833a`, same container).

use tock_registers::LocalRegisterCopy;
use tock_registers::register_bitfields;
use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use super::DxioError;
use super::lanes::{Engine, EngineKind, Speed, WireLanes};
use crate::sp3::soc::Generation;

register_bitfields![u32,
    /// Engine `+0x10`: link capabilities, low dword.
    pub CAPS [
        PRESENT OFFSET(0) NUMBITS(1) [],
        EARLY_TRAIN OFFSET(1) NUMBITS(1) [],
        COMP_MODE OFFSET(2) NUMBITS(1) [],
        /// Link reversal as the firmware sees it (core reversal XOR
        /// board reversal on Rome/Milan).
        REVERSE OFFSET(3) NUMBITS(1) [],
        /// Three bits (6:4); fstart's Zeppelin model had two.
        MAX_SPEED OFFSET(4) NUMBITS(3) [],
        HOTPLUG OFFSET(8) NUMBITS(3) [],
        /// Always 1 on PCIe engines (`or [ebx+0x10], 0x80000`).
        EN_OFF_CONFIG OFFSET(19) NUMBITS(1) [],
        OFF_UNUSED OFFSET(20) NUMBITS(1) [],
        NTB_HOTPLUG OFFSET(21) NUMBITS(1) [],
        PRIMARY_PLL OFFSET(31) NUMBITS(1) []
    ],
    /// Engine `+0x14`: link capabilities, high dword (Rome/Milan).
    pub CAPS_HI [
        EQ_MODE OFFSET(0) NUMBITS(2) [],
        EQ_OVERRIDE OFFSET(2) NUMBITS(1) [],
        RX_POLARITY OFFSET(3) NUMBITS(1) [],
        TX_VETTING OFFSET(4) NUMBITS(1) [],
        RX_VETTING OFFSET(5) NUMBITS(1) [],
        DEEMPHASIS OFFSET(6) NUMBITS(2) [],
        DEEMPHASIS_OVERRIDE OFFSET(8) NUMBITS(1) [],
        TX_POLARITY OFFSET(9) NUMBITS(1) [],
        /// Milan only.
        TARGET_SPEED OFFSET(10) NUMBITS(3) [],
        SKIP_EQ_GEN3 OFFSET(13) NUMBITS(1) [],
        SKIP_EQ_GEN4 OFFSET(14) NUMBITS(1) []
    ],
    /// Milan engine `+0x04` dword: the `+6` byte flags.
    pub GROUPS [
        GPIO_GROUP OFFSET(0) NUMBITS(8) [],
        RESET_GROUP OFFSET(8) NUMBITS(8) [],
        SEARCH_DEPTH OFFSET(16) NUMBITS(1) [],
        KPNP_RESET OFFSET(17) NUMBITS(1) []
    ],
    /// Header dword of the ancillary heap and of each of its sub-records.
    pub ANC [
        TYPE OFFSET(0) NUMBITS(8) [],
        FORMAT OFFSET(8) NUMBITS(4) [],
        /// Heap header: total dwords − 1; sub-records: their own dwords.
        LEN OFFSET(16) NUMBITS(8) []
    ],
    /// Tuning record, format 2 (relative lanes; Rome, Milan PCD 0x92 = 0).
    pub TUNE2 [
        CODE OFFSET(0) NUMBITS(7) [],
        GROUP OFFSET(7) NUMBITS(3) [],
        LANE OFFSET(10) NUMBITS(6) [],
        VALUE OFFSET(16) NUMBITS(16) []
    ],
    /// Tuning record, format 1 (absolute wire lanes; Milan PCD 0x92 = 1).
    pub TUNE1 [
        CODE OFFSET(0) NUMBITS(8) [],
        GROUP OFFSET(8) NUMBITS(2) [],
        LANE OFFSET(10) NUMBITS(6) [],
        VALUE OFFSET(16) NUMBITS(16) []
    ]
];

/// Wire engine types (board `+4`).
pub mod kind {
    pub const UNUSED: u8 = 0;
    pub const PCIE: u8 = 1;
    pub const SATA: u8 = 3;
    pub const ETHERNET: u8 = 0x10;
}

/// One 0x28-byte wire engine. Bytes from `+0x18` are filled by the firmware
/// and read back with GetEngineCfg (0x14).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout,
)]
#[repr(C)]
pub struct FwEngine {
    pub kind: u8,
    /// Bit 0: hotplug.
    pub flags: u8,
    /// DXIO lanes, low ≤ high.
    pub start_lane: u8,
    pub end_lane: u8,
    /// `+4..+7`, see [`GROUPS`].
    pub groups: U32,
    /// SATA channel type.
    pub channel_type: u8,
    pub _rsvd9: u8,
    /// Offset of this engine's tuning record in the ancillary heap, dwords.
    pub ancillary_offset: U16,
    /// Requested devfn (Naples, Milan) or board `start << 8 | end` PHY
    /// lanes (Rome; the firmware fills in the real BDF).
    pub bdf_num: U32,
    /// [`CAPS`].
    pub caps: U32,
    /// [`CAPS_HI`].
    pub caps_hi: U32,
    pub mac_id: u8,
    /// Hardware port within the PCIe core (returned).
    pub mac_port_id: u8,
    pub _returned: [u8; 10],
    /// LTSSM summary (returned).
    pub train_state: u8,
    pub _rsvd25: [u8; 3],
}

const _: () = assert!(size_of::<FwEngine>() == 0x28);

impl FwEngine {
    pub fn caps(&self) -> LocalRegisterCopy<u32, CAPS::Register> {
        LocalRegisterCopy::new(self.caps.get())
    }

    pub fn caps_hi(&self) -> LocalRegisterCopy<u32, CAPS_HI::Register> {
        LocalRegisterCopy::new(self.caps_hi.get())
    }

    pub fn is_pcie(&self) -> bool {
        self.kind == kind::PCIE
    }
}

/// Heap 3 header (`+0`: type 0 = EPYC, `+0xC`: engine count).
#[derive(Debug, Clone, Copy, Default, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct EngineHeader {
    pub kind: U16,
    pub _rsvd: [u8; 10],
    pub count: U16,
    pub _rsvd2: [u8; 2],
}

/// Most engines one DXIO instance takes. 34 descriptors fill the S8030's
/// `$A26`; the firmware limit is not known.
pub const MAX_ENGINES: usize = 40;
/// Heap 3: 0x10 header, engines, 8-byte tail.
pub const ENGINE_HEAP_MAX: usize = 0x18 + 0x28 * MAX_ENGINES;
/// Heap 6 budget (two SATA x8 engines with 15 per-lane IDs take 0x3D0).
pub const ANCILLARY_MAX: usize = 0x800;

/// Per-generation encoding inputs that are platform policy, not board data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EncodePolicy {
    /// Platform `+0x27` PSPP policy; 2 forces Gen1 (Rome/Milan).
    pub pspp_policy: u8,
    /// Milan PCD 0x95 (default 0): reset group 1 instead of the board's.
    pub milan_reset_group_one: bool,
    /// Milan PCD 0x92 (default 0): tuning format 1 (absolute lanes).
    pub milan_tuning_absolute: bool,
}

/// Encode one board engine. `phy` is the board-order PHY lane pair
/// (start > end when reversed), `wire` the translated DXIO lanes.
pub fn encode(
    generation: Generation,
    engine: &Engine<'_>,
    phy: (u8, u8),
    wire: WireLanes,
    policy: &EncodePolicy,
) -> FwEngine {
    let mut e = FwEngine {
        flags: engine.firmware_hotplug as u8,
        start_lane: wire.start,
        end_lane: wire.end,
        ..FwEngine::default()
    };
    let gpio_group = engine.gpio_group;
    let reset_group = match generation {
        // Rome never writes +5.
        Generation::Rome => 0,
        Generation::Milan if policy.milan_reset_group_one => 1,
        Generation::Milan | Generation::Naples => gpio_group,
    };
    let mut groups = LocalRegisterCopy::<u32, GROUPS::Register>::new(0);
    // +4 is always 1, whatever the board's group (all builders).
    groups.modify(GROUPS::GPIO_GROUP.val(1) + GROUPS::RESET_GROUP.val(reset_group as u32));
    let mut caps = LocalRegisterCopy::<u32, CAPS::Register>::new(0);
    let mut hi = LocalRegisterCopy::<u32, CAPS_HI::Register>::new(0);

    match &engine.kind {
        EngineKind::Pcie(p) => {
            e.kind = kind::PCIE;
            let x = &p.extra;
            let early = p.early_train;
            // Early train or PSPP policy 2 force Gen1; ESM then forces Gen3
            // (Rome `ffe02b19`, Milan same order).
            let speed = if early || (generation != Generation::Naples && policy.pspp_policy == 2) {
                Speed::Gen1
            } else {
                p.max_speed
            };
            let speed = if x.esm { Speed::Gen3 } else { speed };
            let hotplug = p.hotplug.map_or(0, |h| h.kind.wire(generation));
            caps.modify(
                CAPS::PRESENT.val(p.present as u32)
                    + CAPS::EARLY_TRAIN.val(early as u32)
                    + CAPS::COMP_MODE.val(x.compliance as u32)
                    + CAPS::REVERSE.val(wire.reverse as u32)
                    + CAPS::MAX_SPEED.val(speed as u32)
                    + CAPS::HOTPLUG.val(hotplug as u32)
                    + CAPS::EN_OFF_CONFIG::SET
                    + CAPS::OFF_UNUSED.val(p.turn_off_unused as u32)
                    + CAPS::NTB_HOTPLUG.val(x.ntb_hotplug as u32)
                    + CAPS::PRIMARY_PLL.val(x.primary_pll as u32),
            );
            e.bdf_num.set(match generation {
                Generation::Rome => (phy.0 as u32) << 8 | phy.1 as u32,
                Generation::Naples | Generation::Milan => p.devfn.unwrap_or(0) as u32,
            });
            if generation != Generation::Naples {
                // The SP3 Naples build has no EQ/vetting fields (§3.3).
                if let Some(mode) = x.eq_search_mode {
                    hi.modify(CAPS_HI::EQ_OVERRIDE::SET + CAPS_HI::EQ_MODE.val(mode as u32));
                }
                if let Some(d) = x.deemphasis {
                    hi.modify(
                        CAPS_HI::DEEMPHASIS_OVERRIDE::SET + CAPS_HI::DEEMPHASIS.val(d as u32),
                    );
                }
                hi.modify(
                    CAPS_HI::RX_POLARITY.val(x.rx_polarity as u32)
                        + CAPS_HI::TX_POLARITY.val(x.tx_polarity as u32)
                        + CAPS_HI::TX_VETTING.val(x.tx_vetting as u32)
                        + CAPS_HI::RX_VETTING.val(x.rx_vetting as u32),
                );
            }
            if generation == Generation::Milan {
                hi.modify(
                    CAPS_HI::TARGET_SPEED.val(x.target_speed as u32)
                        + CAPS_HI::SKIP_EQ_GEN3.val(x.skip_eq_gen3 as u32)
                        + CAPS_HI::SKIP_EQ_GEN4.val(x.skip_eq_gen4 as u32),
                );
                groups.modify(
                    GROUPS::SEARCH_DEPTH.val(x.search_depth as u32)
                        + GROUPS::KPNP_RESET.val(x.kpnp_reset as u32),
                );
            }
        }
        // RXM100CB builder ffd265f7..ffd26604 emits the Ethernet header
        // but skips the PCIe/SATA capabilities and PHY-override paths.
        EngineKind::Ethernet => e.kind = kind::ETHERNET,
        EngineKind::Sata(s) => {
            e.kind = kind::SATA;
            e.channel_type = s.channel;
            if generation != Generation::Naples {
                hi.modify(
                    CAPS_HI::RX_POLARITY.val(s.rx_polarity as u32)
                        + CAPS_HI::TX_POLARITY.val(s.tx_polarity as u32),
                );
            }
        }
    }
    e.groups.set(groups.get());
    e.caps.set(caps.get());
    e.caps_hi.set(hi.get());
    e
}

/// Tuning ID classes of the per-engine PHY parameter list (am4-re
/// DXIO_ANCILLARY, Rome `ffe03600`, Milan `ffd15f71`, same switch).
/// Returns `(code, per_lane)`.
fn tuning_code(id: u8) -> Option<(u8, bool)> {
    Some(match id {
        1 => (0x0D, true),
        2 => (0x0E, false),
        3 => (0x10, false),
        4 => (0x11, false),
        0xC5 => (0x05, false),
        0xC6 => (0x06, false),
        // Groups 1..3 in the top two bits.
        0x40..=0xFF => match id & 0x3F {
            0x00..=0x02 | 0x0A | 0x0B => (id & 0x3F, true),
            0x03 | 0x04 | 0x07..=0x09 | 0x0C => (id & 0x3F, false),
            0x0D => (0x0F, false),
            0x12 => (0x14, true),
            0x13 => (0x15, true),
            _ => return None,
        },
        _ => return None,
    })
}

/// Encode one engine's tuning list (`{id, value}` pairs, an id of 0 ends
/// it) as an ancillary type-3 record into `out`; returns the bytes written
/// (0 for an empty list). Unknown IDs fail closed (the donor sizer and
/// encoder disagree on 0x45/0x46/0x85/0x86 and ID 1's length; here every
/// record has one dword per emitted lane and the header counts exactly
/// what was written).
pub fn encode_tuning(
    tuning: &[(u8, u8)],
    wire: WireLanes,
    absolute: bool,
    out: &mut [u8],
) -> Result<usize, DxioError> {
    let pairs = tuning.iter().take_while(|&&(id, _)| id != 0);
    if pairs.clone().next().is_none() {
        return Ok(0);
    }
    let (lo, hi) = (wire.start.min(wire.end), wire.start.max(wire.end));
    let (first, last) = if absolute { (lo, hi) } else { (0, hi - lo) };
    let record = |code: u8, group: u8, lane: u8, value: u8| -> u32 {
        if absolute {
            let mut r = LocalRegisterCopy::<u32, TUNE1::Register>::new(0);
            r.modify(
                TUNE1::CODE.val(code as u32)
                    + TUNE1::GROUP.val(group as u32)
                    + TUNE1::LANE.val(lane as u32)
                    + TUNE1::VALUE.val(value as u32),
            );
            r.get()
        } else {
            let mut r = LocalRegisterCopy::<u32, TUNE2::Register>::new(0);
            r.modify(
                TUNE2::CODE.val(code as u32)
                    + TUNE2::GROUP.val(group as u32)
                    + TUNE2::LANE.val(lane as u32)
                    + TUNE2::VALUE.val(value as u32),
            );
            r.get()
        }
    };
    if last >= 64 {
        return Err(DxioError::Tuning(0));
    }
    let mut at = 4;
    for &(id, value) in pairs {
        let (code, per_lane) = tuning_code(id).ok_or(DxioError::Tuning(id))?;
        let lanes = if per_lane {
            first..=last
        } else {
            first..=first
        };
        for lane in lanes {
            let dst = out.get_mut(at..at + 4).ok_or(DxioError::Buffer)?;
            dst.copy_from_slice(&record(code, id >> 6, lane, value).to_le_bytes());
            at += 4;
        }
    }
    let mut h = LocalRegisterCopy::<u32, ANC::Register>::new(0);
    h.modify(
        ANC::TYPE.val(3)
            + ANC::FORMAT.val(if absolute { 1 } else { 2 })
            + ANC::LEN.val((at / 4) as u32),
    );
    out[..4].copy_from_slice(&h.get().to_le_bytes());
    Ok(at)
}

/// Platform-wide ancillary inputs.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ancillary<'a> {
    /// Platform `+0x22` PSPP data, sent when [`EncodePolicy::pspp_policy`]
    /// is non-zero.
    pub pspp: [u8; 4],
    /// PHY override blob (Platform `+0x16`; presumably APOB fabric
    /// PHY_OVERRIDE, PCIE_DXIO §3.6 open item). Length a multiple of 4.
    pub phy_override: Option<&'a [u8]>,
}

/// The two heaps of one DXIO instance, built in place.
pub struct Heaps {
    pub engines: [u8; ENGINE_HEAP_MAX],
    pub engines_len: usize,
    pub ancillary: [u8; ANCILLARY_MAX],
    /// 0 when no ancillary data exists (heap 6 is then not sent).
    pub ancillary_len: usize,
}

impl Heaps {
    /// Build heap 3 and heap 6 from `(engine, tuning, wire lanes)` triples.
    pub fn build<'t>(
        generation: Generation,
        engines: impl Iterator<Item = (FwEngine, &'t [(u8, u8)], WireLanes)> + Clone,
        anc: &Ancillary<'_>,
        policy: &EncodePolicy,
    ) -> Result<Self, DxioError> {
        let mut h = Heaps {
            engines: [0; ENGINE_HEAP_MAX],
            engines_len: 0,
            ancillary: [0; ANCILLARY_MAX],
            ancillary_len: 0,
        };
        let pspp = policy.pspp_policy != 0;
        let phy = anc.phy_override.filter(|b| !b.is_empty());
        if phy.is_some_and(|b| b.len() % 4 != 0 || b.len() + 8 > ANCILLARY_MAX) {
            return Err(DxioError::Buffer);
        }
        // Rome emits the PHY record only without PSPP; Milan emits both
        // (`ffe027e4` / `ffd1509d`).
        let emit_phy = phy.filter(|_| generation == Generation::Milan || !pspp);
        let absolute = generation == Generation::Milan && policy.milan_tuning_absolute;

        let mut at = 4;
        let sub = |ty: u8, body: &[u8], at: &mut usize, buf: &mut [u8]| {
            let mut r = LocalRegisterCopy::<u32, ANC::Register>::new(0);
            r.modify(
                ANC::TYPE.val(ty as u32)
                    + ANC::FORMAT.val(1)
                    + ANC::LEN.val((body.len() / 4 + 1) as u32),
            );
            buf[*at..*at + 4].copy_from_slice(&r.get().to_le_bytes());
            buf[*at + 4..*at + 4 + body.len()].copy_from_slice(body);
            *at += 4 + body.len();
        };
        if pspp {
            sub(4, &anc.pspp, &mut at, &mut h.ancillary);
        }
        if let Some(b) = emit_phy {
            sub(5, b, &mut at, &mut h.ancillary);
        }

        let mut count = 0usize;
        let mut off = size_of::<EngineHeader>();
        for (mut e, tuning, wire) in engines {
            if count == MAX_ENGINES {
                return Err(DxioError::TooManyEngines);
            }
            // Leave room for the heap tail.
            let room = ANCILLARY_MAX - 4;
            let n = encode_tuning(tuning, wire, absolute, &mut h.ancillary[at..room])?;
            if n != 0 {
                e.ancillary_offset.set((at / 4) as u16);
                at += n;
            }
            h.engines[off..off + 0x28].copy_from_slice(e.as_bytes());
            off += 0x28;
            count += 1;
        }
        let header = EngineHeader {
            count: U16::new(count as u16),
            ..EngineHeader::default()
        };
        h.engines[..0x10].copy_from_slice(header.as_bytes());
        h.engines_len = off + 8;

        if at > 4 {
            let total = at + 4;
            let mut r = LocalRegisterCopy::<u32, ANC::Register>::new(0);
            let ty = if generation == Generation::Milan {
                6
            } else {
                0
            };
            r.modify(ANC::TYPE.val(ty) + ANC::LEN.val((total / 4 - 1) as u32));
            h.ancillary[..4].copy_from_slice(&r.get().to_le_bytes());
            h.ancillary_len = total;
        }
        Ok(h)
    }

    /// The engine heap as sent (header, engines, tail).
    pub fn engine_bytes(&self) -> &[u8] {
        &self.engines[..self.engines_len]
    }

    pub fn ancillary_bytes(&self) -> &[u8] {
        &self.ancillary[..self.ancillary_len]
    }

    /// The engines of heap 3.
    pub fn fw_engines(&self) -> &[FwEngine] {
        engines_of(self.engine_bytes())
    }
}

/// The engines of a heap-3 image (or a GetEngineCfg readback).
pub fn engines_of(heap: &[u8]) -> &[FwEngine] {
    let count = EngineHeader::ref_from_prefix(heap).map_or(0, |(h, _)| h.count.get() as usize);
    heap.get(0x10..)
        .and_then(|b| <[FwEngine]>::ref_from_prefix_with_elems(b, count).ok())
        .map_or(&[], |(e, _)| e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::dxio::hotplug::{Hotplug, HotplugKind};
    use crate::sp3::dxio::lanes::{Group, Lanes, PcieEngine, SataEngine};

    fn wire(start: u8, end: u8, reverse: bool) -> WireLanes {
        WireLanes {
            start,
            end,
            reverse,
        }
    }

    /// S8030 `$A26` engine 1 (Rome, VA ffe4716c): NVMe0 x4 on PHY 8–11,
    /// 3.1, misc 0x80 (turn off unused lanes). Board bytes +0x08..:
    /// `01 00 00 00 19 01 00 00 00 00 00 00 00 00 80 00` → +0xC = 0x19
    /// (present, device 3), +0xD = 0x01 (function 1, auto speed).
    #[test]
    fn rome_nvme_record_vs_donor_descriptor() {
        let e = Engine::pcie(
            0,
            Lanes::sp3(Group::P0, 8, 4),
            PcieEngine::new().devfn(3, 1).turn_off_unused(),
        );
        // P0 is NBIO0 core 1, normal: PHY 8 → DXIO 0x2A + 8.
        let fw = encode(
            Generation::Rome,
            &e,
            (8, 11),
            wire(0x32, 0x35, false),
            &EncodePolicy::default(),
        );
        let b = fw.as_bytes();
        assert_eq!(b[..4], [1, 0, 0x32, 0x35]);
        assert_eq!(b[4..6], [1, 0], "gpio group 1, Rome reset group 0");
        assert_eq!(fw.bdf_num.get(), 0x080B, "Rome: board start<<8 | end");
        // present | en_off_config | off_unused; speed auto.
        assert_eq!(fw.caps.get(), 0x0018_0001);
        assert_eq!(fw.caps_hi.get(), 0);

        let m = encode(
            Generation::Milan,
            &e,
            (8, 11),
            wire(0x32, 0x35, false),
            &EncodePolicy::default(),
        );
        assert_eq!(m.bdf_num.get(), 0x19, "Milan: devfn");
        assert_eq!(m.as_bytes()[5], 1, "Milan: board GPIO group");
    }

    #[test]
    fn ethernet_is_not_a_pcie_port() {
        let e = Engine::ethernet(0, Lanes::sp3(Group::P0, 4, 1));
        super::super::lanes::validate(Generation::Milan, &[e]).unwrap();
        assert!(e.pcie_config().is_none());
        let fw = encode(
            Generation::Milan,
            &e,
            (4, 4),
            wire(0x2e, 0x2e, false),
            &EncodePolicy::default(),
        );
        assert_eq!(fw.kind, kind::ETHERNET);
        assert_eq!((fw.start_lane, fw.end_lane), (0x2e, 0x2e));
        assert_eq!(fw.groups.get(), 0x101);
        assert_eq!(
            (fw.flags, fw.bdf_num.get(), fw.caps.get(), fw.caps_hi.get()),
            (0, 0, 0, 0)
        );
        let wide = Engine::ethernet(0, Lanes::sp3(Group::P0, 4, 2));
        assert!(matches!(
            super::super::lanes::validate(Generation::Milan, &[wide]),
            Err((0, super::super::lanes::LaneError::Width))
        ));
    }

    #[test]
    fn pcie_caps_bits() {
        let p = PcieEngine::new()
            .max_speed(Speed::Gen4)
            .with_hotplug(Hotplug::new(HotplugKind::EnterpriseSsd))
            .early_train();
        let e = Engine::pcie(0, Lanes::sp3(Group::G0, 0, 16).reversed(), p);
        let fw = encode(
            Generation::Milan,
            &e,
            (31, 16),
            wire(16, 31, false),
            &EncodePolicy::default(),
        );
        let c = fw.caps();
        assert_eq!(c.read(CAPS::MAX_SPEED), 1, "early train forces Gen1");
        assert_eq!(c.read(CAPS::HOTPLUG), 5);
        assert!(c.is_set(CAPS::EARLY_TRAIN) && !c.is_set(CAPS::REVERSE));
        assert_eq!(fw.flags, 0, "native PCIe hotplug is not firmware-managed");
        let firmware_managed = Engine {
            firmware_hotplug: true,
            ..e
        };
        let fw = encode(
            Generation::Milan,
            &firmware_managed,
            (31, 16),
            wire(16, 31, false),
            &EncodePolicy::default(),
        );
        assert_eq!(fw.flags, 1);
        assert_eq!(fw.caps().read(CAPS::HOTPLUG), 5);
        // Naples maps EntSSD to ExpressModule and takes the devfn.
        let z = encode(
            Generation::Naples,
            &e,
            (31, 16),
            wire(16, 31, false),
            &EncodePolicy::default(),
        );
        assert_eq!(z.caps().read(CAPS::HOTPLUG), 2);

        let esm = Engine::pcie(0, Lanes::sp3(Group::P1, 0, 4), PcieEngine::new().esm());
        let pspp2 = EncodePolicy {
            pspp_policy: 2,
            ..Default::default()
        };
        let fw = encode(
            Generation::Rome,
            &esm,
            (32, 35),
            wire(0x3A, 0x3D, false),
            &pspp2,
        );
        assert_eq!(
            fw.caps().read(CAPS::MAX_SPEED),
            3,
            "ESM wins over PSPP Gen1"
        );
    }

    /// S8030 `$A26` engine 0: SATA x6 on PHY 0–5 with the parameter list at
    /// +0x48 (`c1 00 c2 0a c0 00 d2 01 d3 01 81 01 82 00 80 00 92 01 93 01
    /// 41 05 42 00 40 00 52 01 53 01 00 00`).
    const S8030_SATA: &[(u8, u8)] = &[
        (0xC1, 0x00),
        (0xC2, 0x0A),
        (0xC0, 0x00),
        (0xD2, 0x01),
        (0xD3, 0x01),
        (0x81, 0x01),
        (0x82, 0x00),
        (0x80, 0x00),
        (0x92, 0x01),
        (0x93, 0x01),
        (0x41, 0x05),
        (0x42, 0x00),
        (0x40, 0x00),
        (0x52, 0x01),
        (0x53, 0x01),
        (0, 0),
    ];

    #[test]
    fn sata_tuning_and_heaps() {
        let sata =
            Engine::sata(0, Lanes::sp3(Group::P0, 0, 6), SataEngine::new(0)).tuning(S8030_SATA);
        let w = wire(0x2A, 0x2F, false);
        let fw = encode(Generation::Rome, &sata, (0, 5), w, &EncodePolicy::default());
        assert_eq!((fw.kind, fw.caps.get()), (kind::SATA, 0));

        let h = Heaps::build(
            Generation::Rome,
            [(fw, S8030_SATA, w)].into_iter(),
            &Ancillary::default(),
            &EncodePolicy::default(),
        )
        .unwrap();
        assert_eq!(h.engines_len, 0x18 + 0x28);
        assert_eq!(h.fw_engines()[0].ancillary_offset.get(), 1);
        // 15 per-lane IDs × 6 lanes + record header, + heap header/tail.
        let record = 4 + 15 * 6 * 4;
        assert_eq!(h.ancillary_len, 4 + record + 4);
        let dw = |i: usize| u32::from_le_bytes(h.ancillary[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!(
            dw(0),
            ((h.ancillary_len / 4 - 1) as u32) << 16,
            "Rome header type 0"
        );
        assert_eq!(dw(1), 0x203 | (record as u32 / 4) << 16, "type 3, format 2");
        // First pair 0xC1/0x00: code 1, group 3, lane 0; then lane 1.
        assert_eq!(dw(2), 1 | 3 << 7);
        assert_eq!(dw(3), 1 | 3 << 7 | 1 << 10);
        // 0xC2 = 0x0A: code 2, value in the high word.
        assert_eq!(dw(8), 2 | 3 << 7 | 0x000A_0000);
        // 0xD2 → code 0x14.
        assert_eq!(dw(20) & 0x7F, 0x14);

        let m = Heaps::build(
            Generation::Milan,
            [(fw, S8030_SATA, w)].into_iter(),
            &Ancillary {
                pspp: [1, 2, 3, 4],
                phy_override: Some(&[9; 8]),
            },
            &EncodePolicy {
                pspp_policy: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(m.ancillary[0], 6, "Milan header type 6");
        assert_eq!(m.ancillary[4..12], [4, 1, 2, 0, 1, 2, 3, 4], "PSPP record");
        assert_eq!(
            m.ancillary[12..16],
            [5, 1, 3, 0],
            "PHY record follows (Milan)"
        );
        assert_eq!(m.fw_engines()[0].ancillary_offset.get(), (4 + 8 + 12) / 4);
    }

    #[test]
    fn tuning_fails_closed() {
        let w = wire(0, 3, false);
        let mut out = [0u8; 64];
        assert_eq!(
            encode_tuning(&[(0x45, 0)], w, false, &mut out),
            Err(DxioError::Tuning(0x45))
        );
        assert_eq!(
            encode_tuning(&[(0, 1), (0x40, 1)], w, false, &mut out),
            Ok(0)
        );
        // Scalar IDs use the low lane: absolute format → the wire start.
        let n = encode_tuning(&[(0xC5, 7)], wire(0x24, 0x25, false), true, &mut out).unwrap();
        assert_eq!(n, 8);
        assert_eq!(
            u32::from_le_bytes(out[4..8].try_into().unwrap()),
            5 | 3 << 8 | 0x24 << 10 | 7 << 16
        );
    }
}
