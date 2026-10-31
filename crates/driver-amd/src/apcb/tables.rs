// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 electrical and frequency tables for the AGESA boot loader (ABL).
//!
//! The rows are board-facing data records, one field per quantity a board
//! could ever legitimately tune; the platform encoders turn them into the
//! APCB wire structs. The `reference` tables are AMD's MemPkg values as
//! shipped in vendor APCBs: measured identical in the Tyan S8030 (Rome and
//! Milan) and AMD EthanolX donors except where a generation has its own
//! table below. A board that retunes a table for its PCB replaces the
//! whole table in its [`super::ApcbFacts`]; none of ours do.

/// DIMM slots per channel a row applies to (a set: some AMD rows cover
/// both 1 and 2 slots per channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slots(pub u8);

impl Slots {
    pub const ONE: Self = Self(1);
    pub const TWO: Self = Self(2);
    pub const ONE_OR_TWO: Self = Self(3);
}

/// A JEDEC DDR4 transfer rate. The discriminant is its MT/s value; the
/// wire formats variously want that, a bit position in a rate set, or the
/// memory clock in MHz, all derived here rather than remapped by encoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u16)]
pub enum DdrRate {
    Ddr400 = 400,
    Ddr533 = 533,
    Ddr667 = 667,
    Ddr800 = 800,
    Ddr1066 = 1066,
    Ddr1333 = 1333,
    Ddr1600 = 1600,
    Ddr1866 = 1866,
    Ddr2133 = 2133,
    Ddr2400 = 2400,
    Ddr2667 = 2667,
    Ddr2933 = 2933,
    Ddr3200 = 3200,
}

impl DdrRate {
    pub const ALL: &[Self] = &[
        Self::Ddr400,
        Self::Ddr533,
        Self::Ddr667,
        Self::Ddr800,
        Self::Ddr1066,
        Self::Ddr1333,
        Self::Ddr1600,
        Self::Ddr1866,
        Self::Ddr2133,
        Self::Ddr2400,
        Self::Ddr2667,
        Self::Ddr2933,
        Self::Ddr3200,
    ];

    pub const fn mt_s(self) -> u16 {
        self as u16
    }

    /// The memory clock; not exactly MT/s / 2 for the odd JEDEC rates.
    pub const fn mhz(self) -> u16 {
        match self {
            Self::Ddr400 => 200,
            Self::Ddr533 => 266,
            Self::Ddr667 => 333,
            Self::Ddr800 => 400,
            Self::Ddr1066 => 533,
            Self::Ddr1333 => 667,
            Self::Ddr1600 => 800,
            Self::Ddr1866 => 933,
            Self::Ddr2133 => 1067,
            Self::Ddr2400 => 1200,
            Self::Ddr2667 => 1333,
            Self::Ddr2933 => 1467,
            Self::Ddr3200 => 1600,
        }
    }

    const fn bit(self) -> u32 {
        match self {
            Self::Ddr400 => 0,
            Self::Ddr533 => 1,
            Self::Ddr667 => 2,
            Self::Ddr800 => 3,
            Self::Ddr1066 => 4,
            Self::Ddr1333 => 5,
            Self::Ddr1600 => 6,
            Self::Ddr1866 => 7,
            Self::Ddr2133 => 8,
            Self::Ddr2400 => 9,
            Self::Ddr2667 => 10,
            Self::Ddr2933 => 11,
            Self::Ddr3200 => 12,
        }
    }
}

/// DDR transfer rates a row applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rates(pub u16);

impl Rates {
    pub const fn of(rates: &[DdrRate]) -> Self {
        let mut bits = 0u16;
        let mut i = 0;
        while i < rates.len() {
            bits |= 1 << rates[i].bit();
            i += 1;
        }
        Self(bits)
    }

    pub const fn contains(self, rate: DdrRate) -> bool {
        self.0 & (1 << rate.bit()) != 0
    }
}

/// Rank population of one DIMM slot a row applies to (a set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ranks(pub u8);

impl Ranks {
    pub const EMPTY: Self = Self(1);
    pub const SINGLE: Self = Self(2);
    pub const DUAL: Self = Self(4);
    pub const QUAD: Self = Self(8);
    /// Load-reduced (LRDIMM tables use this instead of single/dual/quad).
    pub const LR: Self = Self(16);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// CAD bus (clock/address/command) drive strength. The discriminants are
/// the PHY register encodings (all four CAD groups share them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CadDrive {
    Ohm120 = 0,
    Ohm60 = 1,
    Ohm40 = 3,
    Ohm30 = 7,
    Ohm24 = 15,
    Ohm20 = 31,
    Auto = 0xff,
}

/// RTT_NOM / RTT_PARK termination (JEDEC DDR4 MR1/MR5 encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RttNom {
    Off = 0,
    Ohm60 = 1,
    Ohm120 = 2,
    Ohm40 = 3,
    Ohm240 = 4,
    Ohm48 = 5,
    Ohm80 = 6,
    Ohm34 = 7,
}

/// RTT_WR termination (JEDEC DDR4 MR2 encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RttWr {
    Off = 0,
    Ohm120 = 1,
    Ohm240 = 2,
    Floating = 3,
    Ohm80 = 4,
}

/// DRAM VrefDQ as the raw MR6 calibration code. RDIMM rows write it as
/// [`VrefDq::percent`] of VDDQ in hundredths of a percent (`7495` is
/// 74.95%, JEDEC range 1: 60.00% + code * 0.65%); the LRDIMM donor
/// tables carry the code itself, so their rows do too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VrefDq(pub u32);

impl VrefDq {
    pub const fn percent(hundredths: u32) -> Self {
        Self((hundredths - 6000) / 65)
    }
}

/// One command/address-bus tuning row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CadRow {
    pub slots: Slots,
    pub rates: Rates,
    pub dimm0: Ranks,
    pub dimm1: Ranks,
    pub gear_down: bool,
    pub slow: bool,
    /// Packed address/command timing-and-drive control word, as the PHY
    /// register takes it (AMD's table value; boards do not compose it).
    pub addr_cmd_control: u32,
    /// CKE, CS/ODT, address/command and clock drive strengths.
    pub drive: [CadDrive; 4],
}

/// One data-bus tuning row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataRow {
    pub slots: Slots,
    pub rates: Rates,
    pub dimm0: Ranks,
    pub dimm1: Ranks,
    pub rtt_nom: RttNom,
    pub rtt_wr: RttWr,
    pub rtt_park: RttNom,
    /// DQ, DQS and ODT drive strength codes.
    pub drive: [u32; 3],
    pub pmu_vref: u32,
    pub dram_vref: VrefDq,
}

/// One ODT pattern row: which chip selects terminate on reads/writes, per
/// population of the three slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OdtRow {
    pub dimms: [Ranks; 3],
    /// Per chip select 0..4: [read pattern, write pattern] bitmaps.
    pub patterns: [[u8; 2]; 4],
}

/// One speed-limit row: with this population on a channel, do not train
/// above `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreqRow {
    pub slots: Slots,
    pub dimms: u16,
    pub single_rank: u16,
    pub dual_rank: u16,
    pub quad_rank: u16,
    pub max: DdrRate,
}

/// The electrical tables (everything except speed limits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElectricalTables {
    pub rdimm_odt: &'static [OdtRow],
    pub rdimm_cad: &'static [CadRow],
    pub rdimm_data: &'static [DataRow],
    pub lrdimm_odt: &'static [OdtRow],
    pub lrdimm_cad: &'static [CadRow],
    pub lrdimm_data: &'static [DataRow],
    pub three_ds_data: &'static [DataRow],
}

/// The speed-limit tables. `stretch` is AMD's relaxed set used with
/// stretch training. These are the tables a board most plausibly derates
/// for its own routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreqTables {
    pub rdimm_max: &'static [FreqRow],
    pub rdimm_stretch: &'static [FreqRow],
    pub three_ds_max: &'static [FreqRow],
    pub three_ds_stretch: &'static [FreqRow],
    pub lrdimm_max: &'static [FreqRow],
}

#[cfg(feature = "sp3")]
mod milan;
#[cfg(feature = "sp3")]
mod rome;

/// AMD's reference tables for an SP3 generation (see the module docs for
/// provenance). Naples is Zeppelin silicon and has no DDR4 v3 tables.
#[cfg(feature = "sp3")]
pub const fn electrical_reference(generation: crate::sp3::soc::Generation) -> ElectricalTables {
    match generation {
        crate::sp3::soc::Generation::Rome => rome::ELECTRICAL,
        crate::sp3::soc::Generation::Milan => milan::ELECTRICAL,
        crate::sp3::soc::Generation::Naples => rome::ELECTRICAL,
    }
}

/// AMD's reference speed limits for an SP3 generation.
#[cfg(feature = "sp3")]
pub const fn freq_reference(generation: crate::sp3::soc::Generation) -> FreqTables {
    match generation {
        crate::sp3::soc::Generation::Rome => rome::FREQ,
        crate::sp3::soc::Generation::Milan => milan::FREQ,
        crate::sp3::soc::Generation::Naples => rome::FREQ,
    }
}
