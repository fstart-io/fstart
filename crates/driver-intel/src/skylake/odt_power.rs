// SPDX-License-Identifier: MIT OR Apache-2.0
//! Default pre-JEDEC ODT matrix/stretch from native/odt_power.c:38–232.
//! Runs after base timings and MAD, BEFORE the caller's forced RCOMP. This is
//! not the later DIMM ODT optimizer, power model or memory activation.

use super::{
    Error,
    cpu::Package,
    registers::{OdtMatrix, ReadReadTurnaround, Scheduler, TcOdt},
    reut::{Engine, rank_mask},
    spd::Dimms,
    training_state::CalibrationState,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use tock_registers::{LocalRegisterCopy, interfaces::Writeable};

// A bounded, source-ordered transaction plan makes BOTH channels' arithmetic
// reject before the first write/host mutation, including an overflow on CH1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PowerWrite {
    Matrix(u32),
    Scheduler(u32),
    ReadDuration(u32),
    ReadRead(u32),
    ReadWrite(u32),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChannelPlan {
    writes: [Option<PowerWrite>; 5],
}

fn matrix(dimms: &Dimms<'_>, ch: usize) -> u32 {
    let count = dimms[ch].iter().flatten().count();
    if count == 2 {
        return (OdtMatrix::ENTRY0.val(4)
            + OdtMatrix::ENTRY1.val(4)
            + OdtMatrix::ENTRY2.val(1)
            + OdtMatrix::ENTRY3.val(1)
            + OdtMatrix::ENTRY4.val(4)
            + OdtMatrix::ENTRY5.val(4)
            + OdtMatrix::ENTRY6.val(1)
            + OdtMatrix::ENTRY7.val(1))
        .value;
    }
    // Do not compact sparse physical slot1 or reinterpret the source's 4/8
    // nibbles as a simpler impedance. Only the fixed profile-1 recipe is used.
    match dimms[ch] {
        [Some(d), None] if d.ranks == 2 => {
            (OdtMatrix::ENTRY4.val(1) + OdtMatrix::ENTRY5.val(2)).value
        }
        [None, Some(d)] if d.ranks == 2 => {
            (OdtMatrix::ENTRY6.val(4) + OdtMatrix::ENTRY7.val(8)).value
        }
        _ => 0,
    }
}

fn read_duration(odt: u32, off: u32) -> Result<u32, Error> {
    let value = off.checked_sub(6).ok_or(Error::InvalidTiming)?;
    if value > 7 {
        return Err(Error::InvalidTiming);
    }
    let mut odt = LocalRegisterCopy::<u32, TcOdt::Register>::new(odt);
    odt.modify(TcOdt::READ_DURATION.val(value));
    Ok(odt.get())
}

fn stretch(rdrd: u32, rdwr: u32, delta: u32) -> Result<[u32; 2], Error> {
    let mut rdrd = LocalRegisterCopy::<u32, ReadReadTurnaround::Register>::new(rdrd);
    let mut rdwr = LocalRegisterCopy::<u32, ReadReadTurnaround::Register>::new(rdwr);
    // Native tat_field_add uses SIX bits even for RDWR SG (whose normal
    // timing model is seven bits). Preserve SG bit6 and every neighboring bit;
    // reject carry out of the six-bit slice instead of native's modulo wrap.
    for field in [ReadReadTurnaround::DR, ReadReadTurnaround::DD] {
        let value = rdrd
            .read(field)
            .checked_add(delta)
            .ok_or(Error::InvalidTiming)?;
        if value > 63 {
            return Err(Error::InvalidTiming);
        }
        rdrd.modify(field.val(value));
    }
    for field in [
        ReadReadTurnaround::DR,
        ReadReadTurnaround::DD,
        ReadReadTurnaround::SG,
        ReadReadTurnaround::DG,
    ] {
        let value = rdwr
            .read(field)
            .checked_add(delta)
            .ok_or(Error::InvalidTiming)?;
        if value > 63 {
            return Err(Error::InvalidTiming);
        }
        rdwr.modify(field.val(value));
    }
    Ok([rdrd.get(), rdwr.get()])
}

impl ChannelPlan {
    fn for_channel(
        dimms: &Dimms<'_>,
        state: &CalibrationState,
        ch: usize,
        package: Package,
    ) -> Result<Self, Error> {
        let scheduler = state.mchbar_sc_gs_cfg[ch] | Scheduler::ENABLE_ODT_MATRIX::SET.value;
        let mut plan = Self {
            writes: [
                Some(PowerWrite::Matrix(matrix(dimms, ch))),
                Some(PowerWrite::Scheduler(scheduler)),
                None,
                None,
                None,
            ],
        };
        let [Some(a), Some(b)] = dimms[ch] else {
            return Ok(plan);
        };
        let so_dimm = [a, b].iter().any(|d| matches!(d.raw[3] & 0xf, 3 | 8));
        // reference_raw_card already includes the retained SPD extension bit.
        // Extended F (37) is NOT F (5); no masking or guessed package metadata.
        let mixed_f = so_dimm
            && (a.reference_raw_card == 5 || b.reference_raw_card == 5)
            && a.reference_raw_card != b.reference_raw_card;
        let odt = read_duration(state.mchbar_tc_odt[ch], if mixed_f { 7 } else { 6 })?;
        plan.writes[2] = Some(PowerWrite::ReadDuration(odt));
        if mixed_f {
            let [rdrd, rdwr] = stretch(state.mchbar_tc_rdrd[ch], state.mchbar_tc_rdwr[ch], 1)?;
            plan.writes[3] = Some(PowerWrite::ReadRead(rdrd));
            plan.writes[4] = Some(PowerWrite::ReadWrite(rdwr));
        } else if package == Package::DesktopHalo {
            // Preserve native's two ordered TC_ODT writes: off6 then off8.
            plan.writes[3] = Some(PowerWrite::ReadDuration(read_duration(odt, 8)?));
        }
        Ok(plan)
    }
}

impl Engine<'_> {
    /// Program native's default profile-1 matrix and topology-dependent stretch.
    ///
    /// Caller obligations: this is one cold standard-DDR4, SA-GV-off attempt,
    /// with eight data lanes and exclusive MC ownership. Retain the canonical
    /// decoded DIMMs/SPD; pass the actual CPU package. PHY, base timing and MAD
    /// programming must have succeeded. BOTH channels' four `mchbar_*` words
    /// must be the genuine words published by that attempt's timing producer,
    /// not `CalibrationState::default()`, guessed seeds or later readbacks.
    /// Reset/reprogram/republish the genuine base words on a whole-attempt retry;
    /// this is not idempotent when a raw-card-F stretch is applied twice.
    ///
    /// No MC timing/scheduler/ODT hardware reads occur here: pre-RCOMP readback
    /// can hang. All cold/lane/rank/field arithmetic checks for both channels
    /// precede ANY register write or host mutation. Native writes both hardware
    /// channels, even empty channels (also for empty inventory); so do we.
    /// Success means only these writes, NOT compensation or usable memory.
    /// The caller MUST force RCOMP next, before JEDEC initialization/training.
    pub fn meminit_power_optimization(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        package: Package,
        sdram_lanes: u8,
    ) -> Result<(), Error> {
        if mode != MemoryBootMode::Cold {
            return Err(Error::RetainedMemoryUnsupported);
        }
        if sdram_lanes != 8 {
            return Err(Error::InvalidPolicy);
        }
        // Validate all physical slots before planning or publishing either CH.
        rank_mask(self.dimms, 0)?;
        rank_mask(self.dimms, 1)?;
        let plans = [
            ChannelPlan::for_channel(self.dimms, state, 0, package)?,
            ChannelPlan::for_channel(self.dimms, state, 1, package)?,
        ];
        for (ch, plan) in plans.iter().enumerate() {
            let r = &self.registers.channel[ch];
            for write in plan.writes.iter().flatten() {
                match *write {
                    PowerWrite::Matrix(word) => r.odt_matrix.set(word),
                    PowerWrite::Scheduler(word) => {
                        state.mchbar_sc_gs_cfg[ch] = word;
                        r.scheduler.set(word);
                    }
                    PowerWrite::ReadDuration(word) => {
                        state.mchbar_tc_odt[ch] = word;
                        r.tc_odt.set(word);
                    }
                    PowerWrite::ReadRead(word) => {
                        state.mchbar_tc_rdrd[ch] = word;
                        r.tc_rdrd.set(word);
                    }
                    PowerWrite::ReadWrite(word) => {
                        state.mchbar_tc_rdwr[ch] = word;
                        r.tc_rdwr.set(word);
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "odt_power_tests.rs"]
mod tests;
