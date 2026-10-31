// SPDX-License-Identifier: MIT OR Apache-2.0
//! The APCB in board terms: what a board states, what AMD's reference
//! data supplies, and nothing about the wire format.
//!
//! The PSP's AGESA boot loader (ABL) trains DRAM from the APCB before
//! any x86 code runs, so fstart must generate one per board. Every
//! quantity in the format belongs to exactly one of three owners:
//!
//! - **ABI mechanics** (token identities and widths, entry ordering,
//!   checksums, the active/backup/CBS copies): the platform encoder.
//!   Board code never sees them (see `platform-sp3`'s host APCB).
//! - **AMD reference data** (AGESA token defaults, DDR4 electrical
//!   tables): [`tables`] here and the platform token catalogs, shared by
//!   every board on a generation.
//! - **Board wiring and policy** ([`ApcbFacts`]): the board crate. SPD
//!   bus topology, the board-ID source, channel wiring, and deltas over
//!   [`Policy::default()`]. Measured on the S8030 and EthanolX donors of
//!   the same SoC, that is a handful of lines per board; adding a board
//!   on a supported SoC changes no driver, platform or tool code.

pub mod policy;
pub mod tables;

pub use policy::{
    AblConsole, AblSerialPort, DimmTypes, MemoryPolicy, NumaPerSocket, Policy, XgmiSpeed,
};
pub use tables::{ElectricalTables, FreqTables};

/// One DIMM slot's SPD address on the board's SMBus (what ABL reads to
/// find SPD before x86 runs). Addresses are the 8-bit SMBus forms the
/// wire format takes: SPD EEPROM at `0xA0..0xAE`, mux at `0xE0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpdSlot {
    pub present: bool,
    pub socket: u8,
    pub channel: u8,
    pub dimm: u8,
    pub address: u8,
    pub mux: u8,
    pub mux_channel: u8,
    pub mux_control: u8,
}

impl SpdSlot {
    /// A present slot behind `mux` on `mux_channel`; `mux_control`
    /// is the byte written to the mux's control register (0 on the
    /// S8030, 3 on EthanolX).
    pub const fn muxed(
        socket: u8,
        channel: u8,
        dimm: u8,
        address: u8,
        mux: u8,
        mux_channel: u8,
        mux_control: u8,
    ) -> Self {
        Self {
            present: true,
            socket,
            channel,
            dimm,
            address,
            mux,
            mux_channel,
            mux_control,
        }
    }
}

/// How ABL tells board instances (variants sharing one flash image)
/// apart. The encoder emits per-instance token/table entries masked to
/// each index a board declares in [`ApcbFacts::instances`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardId {
    /// No board-ID structure: one instance, the global entries only.
    None,
    /// An EEPROM on an FCH I2C controller: the ID byte at `id_offset`
    /// (and revision at `revision_offset`) maps to a board-instance
    /// index through `mapping`.
    Eeprom {
        controller: u16,
        /// 8-bit device address, e.g. `0xA0`... written as on the wire;
        /// the donors use `80` (0x50), the 7-bit form. Kept as decoded.
        address: u16,
        id_offset: u16,
        revision_offset: u16,
        mapping: &'static [IdMapping],
    },
}

/// One EEPROM ID-to-instance mapping row, as the board's ID EEPROM
/// contents define them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdMapping {
    pub mask: u8,
    pub id: u8,
    pub revision: Option<u8>,
    pub instance: u8,
}

/// The board's memory-channel wiring (the PSP platform-specific
/// overrides): which package pins carry each channel's clock/CKE/ODT/CS
/// (tri-stated for unpopulated channels) and the channel geometry.
/// `None` pin maps mean the AGESA built-in connectivity, which is what
/// the Milan donors ship; the Rome donor spells the maps out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelWiring {
    pub memclk: Option<[u8; 8]>,
    pub cke: Option<[u8; 4]>,
    pub odt: Option<[u8; 4]>,
    pub cs: Option<[u8; 8]>,
    pub dimms_per_channel: u8,
    pub channels_per_socket: u8,
}

/// A board instance that needs different ABL settings from the board's
/// base facts (a variant the board-ID selects at runtime, e.g. the
/// S8030's two carrier variants). The policy is complete, not a delta:
/// write `Policy { ..base }` at the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instance {
    pub index: u8,
    pub policy: Policy,
    /// Replacement speed-limit tables for this instance, if its routing
    /// derates memory (the common case for variants).
    pub freq: Option<FreqTables>,
}

/// Everything a board states about memory and ABL, SoC-independent.
/// The SP3 v3 encoder consumes this today; the v2 (Zeppelin/Picasso)
/// encoders consume the same facts when they land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApcbFacts {
    pub policy: Policy,
    pub console: AblConsole,
    /// The board's own slots (a 1P board lists 16, not the 32 a copied
    /// AMD reference table carries). The encoder pads what ABL expects.
    pub spd: &'static [SpdSlot],
    pub board_id: BoardId,
    pub wiring: ChannelWiring,
    pub electrical: ElectricalTables,
    pub freq: FreqTables,
    pub instances: &'static [Instance],
    /// `FchRom3BaseHigh`: the board's flash address above 4 GiB as ABL
    /// must see it (the S8030's ROM3 window). 0 where the board has no
    /// ROM3 mapping (EthanolX).
    pub rom3_base_high: u32,
    /// `PcieResetPinSelect`: which of the SoC's PCIe reset pin schemes
    /// the board wires. A wiring fact, not a tuning knob.
    pub pcie_reset_pin_select: u8,
    /// `Dimm3DsSensorUpper`: the 3DS DIMM temperature sensor upper
    /// threshold the board's thermal design uses, in degrees C.
    pub sensor_3ds_upper: u16,
}
