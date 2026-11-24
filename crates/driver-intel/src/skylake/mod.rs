// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Skylake/Kaby Lake system agent: native DDR4 training, the memory
//! map, DMI/PEG/IGD and the post-memory SA setup (`driver`).
//!
//! Derived from coreboot's `src/soc/intel/skylake/romstage/native/` and
//! `romstage_native.c` in coreboot_KabyLakeNative.
// Ported from per-channel/rank/lane C loops over hardware register arrays,
// with the source's argument lists.
#![allow(dead_code, clippy::needless_range_loop, clippy::too_many_arguments)]

pub mod activation;
pub mod additional_steps;
pub mod address_decode;
pub mod ber_margin;
pub mod capabilities;
pub mod cold_controller;
pub mod cold_handoff;
pub mod cold_training;
pub mod command_si;
pub mod command_training;
pub mod cpu;
pub mod cpu_memory_test;
pub mod cse;
pub mod dmi_registers;
pub mod dq_comp;
pub mod dq_si;
pub mod early_sa;
pub mod energy_perf_gain;
pub mod frequency;
pub mod io_test;
pub mod jedec;
pub mod mc_timing;
pub mod memory_check;
pub mod memory_map;
pub mod odt_power;
pub mod opt_param;
pub mod opt_tradeoff;
pub mod overrides;
pub mod package_power;
pub mod phy;
pub mod phy_registers;
pub mod post_services;
pub mod post_training;
pub mod power_model;
pub mod read_centering;
pub mod read_training;
pub mod registers;
pub mod reut;
pub mod reut_registers;
pub mod rtl_training;
pub mod sa_dmi;
pub mod sa_graphics;
pub mod sa_graphics_pm;
pub mod sa_peg;
pub mod sa_post_mem;
pub mod sa_pre_mem;
pub(crate) mod sa_registers;
pub mod sense_amp;
pub mod smram;
pub mod spd;
pub mod timings;
pub mod training;
pub mod training_cache;
pub mod training_state;
pub mod turnaround_training;
pub mod tx_vref_range;
pub mod uncore_prmrr;
pub mod vref_training;
pub mod write_centering;
pub mod write_leveling;
pub mod write_training;

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
