// SPDX-License-Identifier: MIT OR Apache-2.0
//! Source-backed post-training prerequisites, not MC normal-mode activation.

use super::{
    Error,
    mc_timing::txp,
    registers::{RoundtripLatency, Scheduler, TcPowerDown},
    reut::Engine,
    timings::Timings,
    training::cold_ranks,
    training_state::CalibrationState,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
};

impl Engine<'_> {
    /// `post_training.c:meminit_cmdn_update`, DDR4: leave initial 3N training
    /// for the resolved command rate before late command training.
    ///
    /// The caller must supply the actual receive-training RTL host values and
    /// fused/topology-resolved final command rate. No final/usable-memory state
    /// is produced. Validate all arithmetic before the first register write;
    /// unlike the C source, negative RTL cannot wrap to 255 and then clamp.
    pub fn update_command_rate(
        &self,
        state: &mut CalibrationState,
        mode: MemoryBootMode,
        ch: usize,
        command_rate: u8,
        timings: &Timings,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        if ch >= ranks.len() || ranks[ch] == 0 {
            return Err(Error::InvalidRank);
        }
        if !(1..=3).contains(&command_rate) {
            return Err(Error::InvalidPolicy);
        }
        let r = &self.registers.channel[ch];
        let mut scheduler = LocalRegisterCopy::<u32, Scheduler::Register>::new(r.scheduler.get());
        // Source old_n_val=2 assumes initial 3N command stretch. N_TO_ONE is
        // a ratio (initialized to 3), NOT its enable state; preserve it.
        // Reject repeated adjustment rather than subtract twice from RTL.
        if scheduler.read(Scheduler::COMMAND_STRETCH) != 2 {
            return Err(Error::InvalidPolicy);
        }
        let diff = 2 * (i16::from(command_rate) - 3);
        let mut new_rtl = [0u8; 4];
        let mut rtl =
            LocalRegisterCopy::<u32, RoundtripLatency::Register>::new(r.roundtrip_latency.get());
        let fields = [
            RoundtripLatency::RANK0,
            RoundtripLatency::RANK1,
            RoundtripLatency::RANK2,
            RoundtripLatency::RANK3,
        ];
        for (rank, field) in fields.into_iter().enumerate() {
            if ranks[ch] & (1 << rank) == 0 {
                continue;
            }
            let value = i16::from(state.training.ch[ch].rank[rank].rt_latency) + diff;
            if !(0..=127).contains(&value) {
                return Err(Error::InvalidTiming);
            }
            new_rtl[rank] = value as u8;
            rtl.modify(field.val(value as u32));
        }
        scheduler.modify(Scheduler::COMMAND_STRETCH.val(u32::from(command_rate - 1)));
        r.scheduler.set(scheduler.get());
        r.tc_power_down
            .modify(TcPowerDown::TXP.val(txp(timings.clock.data_rate_mts()).clamp(5, 15)));
        r.roundtrip_latency.set(rtl.get());
        for (rank, value) in new_rtl.into_iter().enumerate() {
            if ranks[ch] & (1 << rank) != 0 {
                state.training.ch[ch].rank[rank].rt_latency = value;
            }
        }
        // Subsequent tests/optimizers use a host scheduler baseline. Keep it
        // equal to the value just programmed, not the initial 3N shadow.
        state.mchbar_sc_gs_cfg[ch] = scheduler.get();
        Ok(())
    }
}
