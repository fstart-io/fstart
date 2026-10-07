//! Intel High Definition Audio (HDA / Azalia) controller interface.
//!
//! The HDA specification (Intel Rev 1.0a, June 2010) defines a standard
//! MMIO register interface used by every HDA controller — Intel ICH/PCH,
//! AMD/ATI SB, Nvidia MCP, VIA VT8xxx, and others.  This crate provides:
//!
//! - **Controller operations**: reset, codec detection, verb programming
//!   via the Immediate Command interface (IC/IR/ICS registers).
//! - **Verb table types**: [`HdaVerbTable`] and [`HdaConfig`] for
//!   describing codec pin configurations in Rust board metadata.
//! - **Pin descriptor types**: [`PinConfig`] with named enums matching
//!   coreboot's `AZALIA_PIN_DESC` / `AZALIA_PIN_CFG` macros — readable
//!   per-pin configuration instead of raw 32-bit values.
//! - **Helper functions**: [`hda_verb`], [`hda_pin_cfg`], [`hda_pin_nc`]
//!   for raw verb encoding when needed.
//!
//! # Rust pin configuration
//!
//! ```ignore
//! use crate::southbridge::hda::{HdaConfig, HdaVerbTable, PinColor, PinConfig, PinConn, PinConnector, PinDevice, PinGeoLoc, PinLoc};
//! use fstart_core::hvec;
//!
//! let hda = HdaConfig {
//!     verbs: hvec([HdaVerbTable {
//!         vendor_id: 0x10ec0662,
//!         subsystem_id: 0x105b0d55,
//!         pins: hvec([
//!             PinConfig {
//!                 nid: 0x14,
//!                 device: PinDevice::LineOut,
//!                 conn: PinConn::Jack,
//!                 color: PinColor::Green,
//!                 loc: PinLoc::External,
//!                 geo: PinGeoLoc::Rear,
//!                 connector: PinConnector::StereoMono18,
//!                 misc: 0xC,
//!                 group: 1,
//!                 seq: 0,
//!                 nc: None,
//!             },
//!             PinConfig::not_connected(0x15),
//!         ]),
//!         extra_verbs: hvec([]),
//!     }]),
//! };
//! ```

#![allow(clippy::derivable_impls, clippy::identity_op)]

use fstart_core::{
    ConstVec,
    mmio::{MmioReadOnly, MmioReadWrite},
};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};

register_bitfields![u32, GCTL [CRST OFFSET(0) NUMBITS(1) []]];
register_bitfields![u16,
    STATESTS [CODECS OFFSET(0) NUMBITS(15) []],
    ICS [BUSY OFFSET(0) NUMBITS(1) [], VALID OFFSET(1) NUMBITS(1) []]
];

register_structs! {
    HdaRegs {
        // GCAP is RO or R/WO depending on the controller generation.
        (0x00 => capabilities: MmioReadWrite<u16>),
        (0x02 => _reserved0),
        (0x08 => control: MmioReadWrite<u32, GCTL::Register>),
        (0x0c => _reserved1),
        // W1C: acknowledge with exact writes, never modify().
        (0x0e => codec_status: MmioReadWrite<u16, STATESTS::Register>),
        (0x10 => _reserved2),
        (0x60 => command: MmioReadWrite<u32>),
        (0x64 => response: MmioReadOnly<u32>),
        // BUSY is R/W and VALID is W1C; keep snapshot-write semantics.
        (0x68 => command_status: MmioReadWrite<u16, ICS::Register>),
        (0x6a => @END),
    }
}

/// Common ICH7/ICH8 ESD/link/virtual-channel programming, before codec reset.
/// Clock detection and generation-specific R/WO writes stay in the chipset.
#[cfg(any(feature = "ich7", feature = "ich8"))]
pub(crate) fn setup_ich_link(device: fstart_pci::ecam::EcamDevice) {
    const ESD: u16 = 0x134;
    const LINK1_DESCRIPTION: u16 = 0x140;
    const LINK_TYPE_MASK: u32 = 0xff << 16;
    const AZALIA_LINK: u32 = 2 << 16;
    const VC0_RESOURCE_CONTROL: u16 = 0x114;
    const VC1_TRAFFIC_CLASS: u16 = 0x44;
    const VC1_RESOURCE_CONTROL: u16 = 0x120;
    const VC_ENABLE: u32 = 1 << 31;
    const VC_ID_1: u32 = 1 << 24;
    const TC7_MAPPING: u32 = 1 << 7;
    device.modify32(ESD, !LINK_TYPE_MASK, AZALIA_LINK);
    device.modify32(LINK1_DESCRIPTION, !LINK_TYPE_MASK, AZALIA_LINK);
    device.modify32(VC0_RESOURCE_CONTROL, !0xff, 1);
    device.or8(VC1_TRAFFIC_CLASS, 7);
    device.or32(VC1_RESOURCE_CONTROL, VC_ENABLE | VC_ID_1 | TC7_MAPPING);
}

/// Maximum number of codec addresses (0..14, spec limit).
pub const MAX_CODECS: u8 = 15;

// ---------------------------------------------------------------------------
// Pin Configuration Default — typed enums (HDA Spec §7.3.3.31)
// ---------------------------------------------------------------------------

/// Port connectivity (bits [31:30]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinConn {
    /// External jack.
    Jack,
    /// No physical connection.
    Nc,
    /// Fixed / integrated device (e.g. internal speaker).
    Integrated,
    /// Both jack and integrated.
    JackAndIntegrated,
}

impl PinConn {
    const fn bits(self) -> u32 {
        match self {
            Self::Jack => 0,
            Self::Nc => 1,
            Self::Integrated => 2,
            Self::JackAndIntegrated => 3,
        }
    }
}

impl Default for PinConn {
    fn default() -> Self {
        Self::Jack
    }
}

/// Gross location (bits [29:28] of location field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinLoc {
    /// External, on primary chassis.
    External,
    /// Internal, not user-accessible.
    Internal,
    /// Separate chassis (e.g. docking station).
    SeparateChassis,
    /// Other location.
    Other,
}

impl Default for PinLoc {
    fn default() -> Self {
        Self::External
    }
}

/// Geometric location (bits [27:24] — fine location within gross).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinGeoLoc {
    NA,
    Rear,
    Front,
    Left,
    Right,
    Top,
    Bottom,
    /// Special 7: Rear panel (External) / Riser (Internal) / Mobile lid inside (Other).
    Special7,
    /// Special 8: Drive bay (External) / Digital display (Internal) / Mobile lid outside (Other).
    Special8,
    /// Special 9: ATAPI (Internal only).
    Special9,
}

impl Default for PinGeoLoc {
    fn default() -> Self {
        Self::NA
    }
}

/// Encode the 6-bit location field from gross + geometric.
const fn encode_location(gross: PinLoc, geo: PinGeoLoc) -> u32 {
    let g = match gross {
        PinLoc::External => 0x00,
        PinLoc::Internal => 0x10,
        PinLoc::SeparateChassis => 0x20,
        PinLoc::Other => 0x30,
    };
    let f = match geo {
        PinGeoLoc::NA => 0,
        PinGeoLoc::Rear => 1,
        PinGeoLoc::Front => 2,
        PinGeoLoc::Left => 3,
        PinGeoLoc::Right => 4,
        PinGeoLoc::Top => 5,
        PinGeoLoc::Bottom => 6,
        PinGeoLoc::Special7 => 7,
        PinGeoLoc::Special8 => 8,
        PinGeoLoc::Special9 => 9,
    };
    g | f
}

/// Default device function (bits [23:20]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDevice {
    LineOut,
    Speaker,
    HpOut,
    Cd,
    SpdifOut,
    DigitalOtherOut,
    ModemLineSide,
    ModemHandsetSide,
    LineIn,
    Aux,
    MicIn,
    Telephony,
    SpdifIn,
    DigitalOtherIn,
    DeviceOther,
}

impl PinDevice {
    const fn bits(self) -> u32 {
        match self {
            Self::LineOut => 0x0,
            Self::Speaker => 0x1,
            Self::HpOut => 0x2,
            Self::Cd => 0x3,
            Self::SpdifOut => 0x4,
            Self::DigitalOtherOut => 0x5,
            Self::ModemLineSide => 0x6,
            Self::ModemHandsetSide => 0x7,
            Self::LineIn => 0x8,
            Self::Aux => 0x9,
            Self::MicIn => 0xA,
            Self::Telephony => 0xB,
            Self::SpdifIn => 0xC,
            Self::DigitalOtherIn => 0xD,
            Self::DeviceOther => 0xF,
        }
    }
}

impl Default for PinDevice {
    fn default() -> Self {
        Self::LineOut
    }
}

/// Connection type / connector (bits [19:16]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinConnector {
    Unknown,
    /// 1/8" (3.5mm) stereo/mono jack.
    StereoMono18,
    /// 1/4" (6.35mm) stereo/mono jack.
    StereoMono14,
    /// ATAPI internal connector.
    AtapiInternal,
    Rca,
    Optical,
    OtherDigital,
    OtherAnalog,
    MultichannelAnalog,
    Xlr,
    Rj11,
    Combination,
    ConnectorOther,
}

impl PinConnector {
    const fn bits(self) -> u32 {
        match self {
            Self::Unknown => 0x0,
            Self::StereoMono18 => 0x1,
            Self::StereoMono14 => 0x2,
            Self::AtapiInternal => 0x3,
            Self::Rca => 0x4,
            Self::Optical => 0x5,
            Self::OtherDigital => 0x6,
            Self::OtherAnalog => 0x7,
            Self::MultichannelAnalog => 0x8,
            Self::Xlr => 0x9,
            Self::Rj11 => 0xA,
            Self::Combination => 0xB,
            Self::ConnectorOther => 0xF,
        }
    }
}

impl Default for PinConnector {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Jack color (bits [15:12]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinColor {
    ColorUnknown,
    Black,
    Grey,
    Blue,
    Green,
    Red,
    Orange,
    Yellow,
    Purple,
    Pink,
    White,
    ColorOther,
}

impl PinColor {
    const fn bits(self) -> u32 {
        match self {
            Self::ColorUnknown => 0x0,
            Self::Black => 0x1,
            Self::Grey => 0x2,
            Self::Blue => 0x3,
            Self::Green => 0x4,
            Self::Red => 0x5,
            Self::Orange => 0x6,
            Self::Yellow => 0x7,
            Self::Purple => 0x8,
            Self::Pink => 0x9,
            Self::White => 0xE,
            Self::ColorOther => 0xF,
        }
    }
}

impl Default for PinColor {
    fn default() -> Self {
        Self::ColorUnknown
    }
}

/// Misc field values (bits [11:8]).
///
/// The HDA spec only defines bit 0 (jack presence detect override).
/// BIOS-extracted configs often have codec-specific bits set in the
/// upper 3 bits. Use the raw `u8` value in [`PinConfig::misc`].
pub const MISC_PRESENCE_DETECT: u8 = 0;
pub const MISC_NO_PRESENCE_DETECT: u8 = 1;

// ---------------------------------------------------------------------------
// Per-pin descriptor
// ---------------------------------------------------------------------------

/// Per-pin configuration for a single HDA widget node.
///
/// Matches coreboot's `AZALIA_PIN_CFG` / `AZALIA_PIN_DESC` with named
/// enum fields instead of raw hex values.  Produces four
/// SET_CONFIGURATION_DEFAULT verbs (0x71C–0x71F) when expanded.
///
/// # Not-connected shorthand
///
/// For unused pins, set `nc` to a sequence number:
/// ```text
/// ( nid: 0x15, nc: 0 )
/// ```
/// This produces the standard `0x411111f0 | seq` value.
///
/// # Full explicit form
///
/// ```text
/// ( nid: 0x14, device: LineOut, conn: Jack, color: Green,
///   loc: Rear, connector: StereoMono18, group: 1, seq: 0 )
/// ```
#[derive(Debug, Clone, Copy)]
pub struct PinConfig {
    /// Widget node ID (NID) — the pin number on the codec (e.g. 0x14).
    pub nid: u8,
    /// Not-connected shorthand — if `Some(seq)`, produces NC config.
    /// All other fields are ignored when this is set.
    pub nc: Option<u8>,
    /// Port connectivity.
    pub conn: PinConn,
    /// Gross location.
    pub loc: PinLoc,
    /// Geometric location (fine position within gross location).
    pub geo: PinGeoLoc,
    /// Default device function.
    pub device: PinDevice,
    /// Connection type (physical connector).
    pub connector: PinConnector,
    /// Jack color.
    pub color: PinColor,
    /// Misc field (bits [11:8]). Only bit 0 is defined by the HDA spec
    /// (0 = jack presence detect, 1 = no presence detect). BIOS-extracted
    /// configs may have additional codec-specific bits set.
    pub misc: u8,
    /// Default association group (bits [7:4], 1–15). 0 = reserved.
    pub group: u8,
    /// Sequence within the association group (bits [3:0], 0–15).
    pub seq: u8,
}

impl PinConfig {
    /// Encode the 32-bit pin configuration value.
    ///
    /// Equivalent to coreboot's `AZALIA_PIN_DESC(conn, loc, dev, type, color, misc, assoc, seq)`.
    pub const fn encode(&self) -> u32 {
        if let Some(s) = self.nc {
            return 0x411111f0 | (s as u32 & 0xF);
        }
        let conn = self.conn.bits();
        let location = encode_location(self.loc, self.geo);
        let dev = self.device.bits();
        let ctype = self.connector.bits();
        let color = self.color.bits();
        let misc = (self.misc as u32) & 0xF;
        let assoc = (self.group as u32) & 0xF;
        let seq = (self.seq as u32) & 0xF;

        (conn << 30)
            | (location << 24)
            | (dev << 20)
            | (ctype << 16)
            | (color << 12)
            | (misc << 8)
            | (assoc << 4)
            | seq
    }

    /// Expand into four SET_CONFIGURATION_DEFAULT verbs for codec address 0.
    pub const fn to_verbs(&self) -> [u32; 4] {
        hda_pin_cfg(0, self.nid as u32, self.encode())
    }
}

// ---------------------------------------------------------------------------
// Verb encoding helpers
// ---------------------------------------------------------------------------

/// Encode a single 12-bit HDA verb.
///
/// Equivalent to coreboot's `AZALIA_VERB_12B(codec, nid, verb_id, payload)`.
///
/// Format: `[31:28]=codec, [27:20]=nid, [19:8]=verb_id, [7:0]=payload`.
#[inline]
pub const fn hda_verb(codec: u32, nid: u32, verb_id: u32, payload: u32) -> u32 {
    (codec << 28) | (nid << 20) | (verb_id << 8) | payload
}

/// Expand a pin configuration value into four SET_CONFIGURATION_DEFAULT verbs.
///
/// Equivalent to coreboot's `AZALIA_PIN_CFG(codec, pin, val)`.
#[inline]
pub const fn hda_pin_cfg(codec: u32, pin: u32, val: u32) -> [u32; 4] {
    [
        hda_verb(codec, pin, 0x71c, (val >> 0) & 0xff),
        hda_verb(codec, pin, 0x71d, (val >> 8) & 0xff),
        hda_verb(codec, pin, 0x71e, (val >> 16) & 0xff),
        hda_verb(codec, pin, 0x71f, (val >> 24) & 0xff),
    ]
}

/// Pin configuration value for "not connected".
///
/// Equivalent to coreboot's `AZALIA_PIN_CFG_NC(seq)`.
#[inline]
pub const fn hda_pin_nc(seq: u32) -> u32 {
    0x411111f0 | (seq & 0xf)
}

/// Encode a SET_SUBSYSTEM_ID verb sequence (4 verbs writing to NID 1).
///
/// Equivalent to coreboot's `AZALIA_SUBVENDOR(codec, val)`.
#[inline]
pub const fn hda_subvendor(codec: u32, val: u32) -> [u32; 4] {
    [
        hda_verb(codec, 1, 0x720, (val >> 0) & 0xff),
        hda_verb(codec, 1, 0x721, (val >> 8) & 0xff),
        hda_verb(codec, 1, 0x722, (val >> 16) & 0xff),
        hda_verb(codec, 1, 0x723, (val >> 24) & 0xff),
    ]
}

/// GET_PARAMETER verb for reading a parameter from a widget.
///
/// Equivalent to coreboot's `AZALIA_VERB_GET_VENDOR_ID(codec)` when
/// called with `param = 0x00`.
#[inline]
pub const fn hda_get_param(codec: u32, nid: u32, param: u32) -> u32 {
    hda_verb(codec, nid, 0xF00, param)
}

// ---------------------------------------------------------------------------
// Verb table types used by Rust board metadata
// ---------------------------------------------------------------------------

/// A single codec's verb table entry.
///
/// Each entry describes one HDA codec's pin configuration and optional
/// extra verb commands.
///
/// # Example
///
/// ```rust,ignore
/// HdaVerbTable::new(0x10ec_0662, 0x105b_0d55)
///     .pin(pin_config(
///         0x14, PinDevice::LineOut, PinConn::Jack, PinLoc::External,
///         PinGeoLoc::Rear, PinConnector::StereoMono18, PinColor::Green,
///         0x0c, 1, 0,
///     ))
///     .pin(pin_not_connected(0x15, 0))
///     .verb(0x00c3_b027)
/// ```
#[derive(Debug, Clone, Copy)]
pub struct HdaVerbTable {
    /// Codec vendor/device ID (e.g. `0x10ec0662` for Realtek ALC662).
    pub vendor_id: u32,
    /// Subsystem ID to write to the codec.
    pub subsystem_id: u32,
    /// Per-pin configurations. Expanded into SET_CONFIGURATION_DEFAULT
    /// verbs at setup time.
    pub pins: ConstVec<PinConfig, 16>,
    /// Additional raw 32-bit verbs (amp gains, power states, EAPD, etc.)
    /// sent after pin configs.
    pub extra_verbs: ConstVec<u32, 32>,
}

/// HD Audio configuration block for a board.
///
/// Contains verb tables for all codecs present on the board.
/// Placed in the southbridge/chipset driver config in Rust board metadata.
#[derive(Debug, Clone, Copy)]
pub struct HdaConfig {
    /// Codec verb tables.  Up to 4 codecs on one HDA link.
    pub verbs: ConstVec<HdaVerbTable, 4>,
}

impl HdaConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            verbs: ConstVec::new(HdaVerbTable::new(0, 0)),
        }
    }

    #[must_use]
    pub const fn verb(mut self, verb: HdaVerbTable) -> Self {
        self.verbs = self.verbs.push(verb);
        self
    }
}

impl Default for HdaConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl HdaVerbTable {
    #[must_use]
    pub const fn new(vendor_id: u32, subsystem_id: u32) -> Self {
        Self {
            vendor_id,
            subsystem_id,
            pins: ConstVec::new(pin_not_connected(0, 0)),
            extra_verbs: ConstVec::new(0),
        }
    }

    #[must_use]
    pub const fn pin(mut self, pin: PinConfig) -> Self {
        self.pins = self.pins.push(pin);
        self
    }

    #[must_use]
    pub const fn extra_verb(mut self, verb: u32) -> Self {
        self.extra_verbs = self.extra_verbs.push(verb);
        self
    }
}

/// Construct a typed HDA pin configuration for board metadata.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub const fn pin_config(
    nid: u8,
    device: PinDevice,
    conn: PinConn,
    loc: PinLoc,
    geo: PinGeoLoc,
    connector: PinConnector,
    color: PinColor,
    misc: u8,
    group: u8,
    seq: u8,
) -> PinConfig {
    PinConfig {
        nid,
        nc: None,
        conn,
        loc,
        geo,
        device,
        connector,
        color,
        misc,
        group,
        seq,
    }
}

/// Construct a not-connected HDA pin descriptor.
#[must_use]
pub const fn pin_not_connected(nid: u8, seq: u8) -> PinConfig {
    PinConfig {
        nid,
        nc: Some(seq),
        conn: PinConn::Nc,
        loc: PinLoc::External,
        geo: PinGeoLoc::NA,
        device: PinDevice::LineOut,
        connector: PinConnector::Unknown,
        color: PinColor::ColorUnknown,
        misc: 0,
        group: 0,
        seq: 0,
    }
}

// ---------------------------------------------------------------------------
// HdaController — generic HDA MMIO interface
// ---------------------------------------------------------------------------

/// HDA controller register interface.
///
/// Operates on any HDA-compliant controller through BAR0 MMIO.
/// PCI configuration is separate: shared ICH link mechanisms are above,
/// while generation-specific policy remains in the chipset drivers.
pub struct HdaController {
    base: usize,
}

// SAFETY: MMIO-based, single-threaded firmware context.
unsafe impl Send for HdaController {}
unsafe impl Sync for HdaController {}

impl HdaController {
    /// Construct from the MMIO base address (BAR0 with low bits masked).
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    /// Return the MMIO base address.
    pub const fn base(&self) -> usize {
        self.base
    }

    fn regs(&self) -> &HdaRegs {
        // SAFETY: constructor receives the mapped, aligned controller BAR0.
        unsafe { &*(self.base as *const HdaRegs) }
    }

    // ---- Controller reset ----

    /// Enter reset (clear CRST, active-low).
    pub fn enter_reset(&self) -> bool {
        self.regs().control.modify(GCTL::CRST::CLEAR);
        for _ in 0..50_000 {
            if !self.regs().control.is_set(GCTL::CRST) {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Exit reset (set CRST).
    pub fn exit_reset(&self) -> bool {
        self.regs().control.modify(GCTL::CRST::SET);
        for _ in 0..50_000 {
            if self.regs().control.is_set(GCTL::CRST) {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    // ---- Codec detection ----

    /// Detect codecs on the HDA link.
    ///
    /// Performs a full reset cycle and returns a bitmask of detected
    /// codec addresses (bits [14:0]).
    pub fn detect_codecs(&self) -> u16 {
        let regs = self.regs();
        let gcap = regs.capabilities.get();
        regs.capabilities.set(gcap);
        regs.codec_status.write(STATESTS::CODECS.val(0x7fff));

        if !self.enter_reset() {
            fstart_log::error!("hda: enter reset timeout");
            return 0;
        }
        if !self.exit_reset() {
            fstart_log::error!("hda: exit reset timeout");
            self.enter_reset();
            return 0;
        }

        for _ in 0..600 {
            core::hint::spin_loop();
        }

        let mask = regs.codec_status.read(STATESTS::CODECS);
        if mask == 0 {
            self.enter_reset();
            fstart_log::info!("hda: no codecs detected");
        }
        mask
    }

    // ---- Immediate command interface ----

    /// Send a single verb and return the response. Returns `None` on timeout.
    pub fn send_verb(&self, verb: u32) -> Option<u32> {
        let regs = self.regs();
        for _ in 0..10_000 {
            if !regs.command_status.is_set(ICS::BUSY) {
                break;
            }
            core::hint::spin_loop();
        }
        if regs.command_status.is_set(ICS::BUSY) {
            return None;
        }

        regs.command.set(verb);
        let mut status = regs.command_status.extract();
        // Preserve the existing VALID acknowledgement while starting BUSY.
        status.modify(ICS::BUSY::SET);
        regs.command_status.set(status.get());

        for _ in 0..10_000 {
            let status = regs.command_status.extract();
            if status.is_set(ICS::VALID) || !status.is_set(ICS::BUSY) {
                return Some(regs.response.get());
            }
            core::hint::spin_loop();
        }
        None
    }

    /// Read a codec's vendor/device ID.
    pub fn read_codec_vendor_id(&self, codec_addr: u8) -> Option<u32> {
        self.send_verb(hda_get_param(codec_addr as u32, 0, 0x00))
    }

    // ---- Verb table programming ----

    /// Program verb tables for all detected codecs.
    ///
    /// For each codec address set in `codec_mask`, reads the vendor/device
    /// ID and searches `config.verbs` for a matching [`HdaVerbTable`].
    /// Sends the subsystem ID, pin configs (expanded from [`PinConfig`]),
    /// and any extra raw verbs.
    pub fn program_verb_tables(&self, config: &HdaConfig, codec_mask: u16) -> u8 {
        let mut programmed = 0u8;

        for addr in 0u8..MAX_CODECS {
            if codec_mask & (1 << addr) == 0 {
                continue;
            }

            let viddid = match self.read_codec_vendor_id(addr) {
                Some(id) => id,
                None => {
                    fstart_log::error!("hda: codec {} vendor ID read timeout", addr);
                    continue;
                }
            };

            let mut found = false;
            for table in config.verbs.as_slice() {
                if table.vendor_id == viddid {
                    fstart_log::info!(
                        "hda: codec {} = {:#010x}, {} pins + {} extra verbs",
                        addr,
                        viddid,
                        table.pins.len() as u32,
                        table.extra_verbs.len() as u32,
                    );

                    // 1. Set subsystem ID.
                    let sub = hda_subvendor(addr as u32, table.subsystem_id);
                    for &v in &sub {
                        self.send_verb(v);
                    }

                    // 2. Program pin configs from typed descriptors.
                    for pin in table.pins.as_slice() {
                        let verbs = hda_pin_cfg(addr as u32, pin.nid as u32, pin.encode());
                        for &v in &verbs {
                            if self.send_verb(v).is_none() {
                                fstart_log::error!("hda: pin {:#x} verb timeout", pin.nid);
                            }
                        }
                    }

                    // 3. Send extra raw verbs (amp gains, power states, etc.)
                    for &verb in table.extra_verbs.as_slice() {
                        if self.send_verb(verb).is_none() {
                            fstart_log::error!("hda: extra verb {:#010x} timeout", verb);
                        }
                    }

                    programmed += 1;
                    found = true;
                    break;
                }
            }

            if !found {
                fstart_log::info!("hda: codec {} = {:#010x}, no verb table", addr, viddid,);
            }
        }

        programmed
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_preserves_other_control_bits_and_neighboring_registers() {
        let mut backing = [0xa5a5_a5a5u32; 28];
        let controller = HdaController::new(backing.as_mut_ptr() as usize);
        let before = controller.regs().control.get();
        assert!(controller.enter_reset());
        assert_eq!(controller.regs().control.get(), before & !1);
        assert!(controller.exit_reset());
        assert_eq!(controller.regs().control.get(), before);
        assert!(backing.iter().all(|&value| value == 0xa5a5_a5a5));
    }

    #[test]
    fn pin_cfg_nc() {
        assert_eq!(hda_pin_nc(0), 0x411111f0);
        assert_eq!(hda_pin_nc(5), 0x411111f5);

        // PinConfig NC shorthand should match.
        let p = PinConfig {
            nid: 0x15,
            nc: Some(0),
            conn: PinConn::default(),
            loc: PinLoc::default(),
            geo: PinGeoLoc::default(),
            device: PinDevice::default(),
            connector: PinConnector::default(),
            color: PinColor::default(),
            misc: 0,
            group: 0,
            seq: 0,
        };
        assert_eq!(p.encode(), 0x411111f0);
    }

    /// Verify pin descriptor encoding matches coreboot's AZALIA_PIN_DESC.
    ///
    /// Test case: NID 0x14 from foxconn-d41s:
    ///   AZALIA_PIN_CFG(0, 0x14, 0x01014c10)
    ///   conn=Jack, loc=External|Rear, dev=LineOut, type=StereoMono18,
    ///   color=Green, misc=0xC (BIOS-extracted), group=1, seq=0
    #[test]
    fn pin_desc_foxconn_line_out() {
        let p = PinConfig {
            nid: 0x14,
            nc: None,
            conn: PinConn::Jack,
            loc: PinLoc::External,
            geo: PinGeoLoc::Rear,
            device: PinDevice::LineOut,
            connector: PinConnector::StereoMono18,
            color: PinColor::Green,
            misc: 0xC,
            group: 1,
            seq: 0,
        };
        assert_eq!(p.encode(), 0x01014c10, "line-out pin config mismatch");
    }

    /// Rear mic-in: AZALIA_PIN_CFG(0, 0x18, 0x01a19c30)
    #[test]
    fn pin_desc_foxconn_mic_in() {
        let p = PinConfig {
            nid: 0x18,
            nc: None,
            conn: PinConn::Jack,
            loc: PinLoc::External,
            geo: PinGeoLoc::Rear,
            device: PinDevice::MicIn,
            connector: PinConnector::StereoMono18,
            color: PinColor::Pink,
            misc: 0xC,
            group: 3,
            seq: 0,
        };
        assert_eq!(p.encode(), 0x01a19c30, "mic-in pin config mismatch");
    }

    /// Front headphone: AZALIA_PIN_CFG(0, 0x1b, 0x02214c1f)
    #[test]
    fn pin_desc_foxconn_hp_out() {
        let p = PinConfig {
            nid: 0x1b,
            nc: None,
            conn: PinConn::Jack,
            loc: PinLoc::External,
            geo: PinGeoLoc::Front,
            device: PinDevice::HpOut,
            connector: PinConnector::StereoMono18,
            color: PinColor::Green,
            misc: 0xC,
            group: 1,
            seq: 15,
        };
        assert_eq!(p.encode(), 0x02214c1f, "hp-out pin config mismatch");
    }

    /// Front mic: AZALIA_PIN_CFG(0, 0x19, 0x02a19c31)
    #[test]
    fn pin_desc_foxconn_front_mic() {
        let p = PinConfig {
            nid: 0x19,
            nc: None,
            conn: PinConn::Jack,
            loc: PinLoc::External,
            geo: PinGeoLoc::Front,
            device: PinDevice::MicIn,
            connector: PinConnector::StereoMono18,
            color: PinColor::Pink,
            misc: 0xC,
            group: 3,
            seq: 1,
        };
        assert_eq!(p.encode(), 0x02a19c31, "front-mic pin config mismatch");
    }

    /// SPDIF out: AZALIA_PIN_CFG(0, 0x1e, 0x99430120)
    #[test]
    fn pin_desc_foxconn_spdif() {
        let pin = PinConfig {
            nid: 0x1e,
            nc: None,
            conn: PinConn::Integrated,
            loc: PinLoc::Internal,
            geo: PinGeoLoc::Special9,
            device: PinDevice::SpdifOut,
            connector: PinConnector::AtapiInternal,
            color: PinColor::ColorUnknown,
            misc: 1,
            group: 2,
            seq: 0,
        };
        assert_eq!(pin.encode(), 0x99430120, "spdif pin config mismatch");
    }

    /// Line-in: AZALIA_PIN_CFG(0, 0x1a, 0x0181343f)
    #[test]
    fn pin_desc_foxconn_line_in() {
        let p = PinConfig {
            nid: 0x1a,
            nc: None,
            conn: PinConn::Jack,
            loc: PinLoc::External,
            geo: PinGeoLoc::Rear,
            device: PinDevice::LineIn,
            connector: PinConnector::StereoMono18,
            color: PinColor::Blue,
            misc: 0x4,
            group: 3,
            seq: 15,
        };
        assert_eq!(p.encode(), 0x0181343f, "line-in pin config mismatch");
    }
}
