//! The DXIO firmware wire format every Zen generation shares: the
//! 0x28-byte engine record and the engine configuration heap (heap 3) with
//! the ancillary heap (heap 6) next to it (PCIE_DXIO §3.6). Picasso,
//! Zeppelin, Naples, Rome and Milan take the same record; how a board engine
//! becomes one is per SoC.
//!
//! Field names follow Oxide's public `zen/dxio.h`.

use tock_registers::LocalRegisterCopy;
use tock_registers::register_bitfields;
use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

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

    /// The firmware kept the engine (it clears PRESENT of ports it could not
    /// use).
    pub fn present(&self) -> bool {
        self.caps().is_set(CAPS::PRESENT)
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

/// The two heaps of one DXIO instance, built in place.
pub struct Heaps {
    pub engines: [u8; ENGINE_HEAP_MAX],
    pub engines_len: usize,
    pub ancillary: [u8; ANCILLARY_MAX],
    /// 0 when no ancillary data exists (heap 6 is then not sent).
    pub ancillary_len: usize,
}

impl Heaps {
    /// Heap 3 with `engines` (header, records, 8-byte tail) and heap 6
    /// holding `ancillary` as is (empty: not sent).
    pub fn from_engines(engines: &[FwEngine], ancillary: &[u8]) -> Option<Self> {
        if engines.len() > MAX_ENGINES
            || ancillary.len() > ANCILLARY_MAX
            || !ancillary.len().is_multiple_of(4)
        {
            return None;
        }
        let mut h = Heaps {
            engines: [0; ENGINE_HEAP_MAX],
            engines_len: size_of::<EngineHeader>() + engines.len() * 0x28 + 8,
            ancillary: [0; ANCILLARY_MAX],
            ancillary_len: ancillary.len(),
        };
        let header = EngineHeader {
            count: U16::new(engines.len() as u16),
            ..EngineHeader::default()
        };
        h.engines[..0x10].copy_from_slice(header.as_bytes());
        h.engines[0x10..0x10 + engines.len() * 0x28].copy_from_slice(engines.as_bytes());
        h.ancillary[..ancillary.len()].copy_from_slice(ancillary);
        Some(h)
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
