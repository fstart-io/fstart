// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Skylake/Kaby Lake system agent: native DDR4 training, the memory
//! map, DMI/PEG/IGD and the post-memory SA setup (`driver`).
//!
//! Derived from coreboot's `src/soc/intel/skylake/romstage/native/` and
//! `romstage_native.c` in coreboot_KabyLakeNative.
// Ported from per-channel/rank/lane C loops over hardware register arrays,
// with the source's argument lists.
#![allow(dead_code, clippy::needless_range_loop, clippy::too_many_arguments)]

pub mod capabilities;
pub mod cpu;
pub mod cse;
pub mod dmi_registers;
pub mod early_sa;
pub mod memory_map;
pub mod package_power;
pub mod phy_registers;
pub mod registers;
pub mod reut_registers;
pub mod sa_dmi;
pub mod sa_graphics;
pub mod sa_graphics_pm;
pub mod sa_peg;
pub mod sa_post_mem;
pub mod sa_pre_mem;
pub(crate) mod sa_registers;
pub mod smram;
pub mod spd;
pub mod timings;
pub mod uncore_prmrr;

/// Identifies the failed analog calibration, not a pipeline completion state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainingStep {
    Mpr,
    ReceiveEnable,
    ReadTiming,
    WriteLeveling,
    WriteLevelingCleanup,
    WriteTiming,
    WriteVoltage,
    CommandTiming,
    CommandVoltage,
    BerMargin,
}

/// Failure before DRAM can be considered usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TrainingFailed {
        step: TrainingStep,
        channel: u8,
        rank: u8,
        lane: u8,
    },
    UnsupportedCpu,
    HostBridgeUnavailable,
    Ddr4FusedOff,
    ChannelCapacityExceeded,
    EccRequired,
    Spd(fstart_driver_spd::ddr4::Error),
    UnsupportedDimm,
    InvalidTiming,
    NoMemory,
    NoCasLatency,
    ClockTooSlow,
    ClockOutsideDimmRange,
    PllAlreadyConfigured,
    PllTimeout,
    PllMismatch,
    CompensationTimeout,
    InvalidPolicy,
    MailboxTimeout,
    MailboxCompletion(u8),
    MailboxInconsistent,
    InvalidRank,
    ReutTimeout,
    InvalidReutConfig,
    ReutTestTimeout {
        channels: u8,
        done: u8,
    },
    ReutError,
    IoResetTimeout,
    MrCommandTimeout,
    CaVrefTimeout,
    CaVrefReadback {
        expected: u32,
        observed: u32,
        mask: u32,
    },
    RetainedMemoryUnsupported,
}

impl From<fstart_driver_spd::ddr4::Error> for Error {
    fn from(value: fstart_driver_spd::ddr4::Error) -> Self {
        Self::Spd(value)
    }
}
