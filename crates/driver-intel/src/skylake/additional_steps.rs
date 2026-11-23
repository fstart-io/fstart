// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 training setup from coreboot native/additional_steps.c.
//!
//! These operations must follow real JEDEC initialization and precede sense-amp
//! and eye training. They do not establish usable memory.

use super::{
    Error,
    cpu::Package,
    jedec::ModeRegisters,
    phy_registers::DataControl2,
    reut::{Engine, RankLane},
    training::{channels, cold_ranks},
    training_state::SDRAM_LANES,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use tock_registers::interfaces::ReadWriteable;

/// Source additional_steps.c:meminit_force_oltm policy, not a thermal-ready flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OltmOverride {
    OpenLoop,
    Refresh2x,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OltmPolicy {
    pub override_mode: OltmOverride,
    pub enable_oltm: bool,
    pub enable_cltm: bool,
    pub refresh_2x: u8,
    pub user_power_weights: bool,
    pub ddr_thermal_sensor: bool,
}

impl Engine<'_> {
    /// 2DPC Hynix pre-2015wk45 DDR4 workaround. Inventory dates are source
    /// BCD bytes, not host dates or invented board thermal policy. No activation.
    pub fn meminit_force_oltm(
        &self,
        mode: MemoryBootMode,
        policy: &mut OltmPolicy,
    ) -> Result<bool, Error> {
        use super::registers::{MaintenanceSpare, MrParams, SchedulerSecondCbit};
        use tock_registers::interfaces::{Readable, Writeable};
        cold_ranks(self, mode)?;
        if policy.refresh_2x > 2 {
            return Err(Error::InvalidPolicy);
        }
        let needed = self.dimms.iter().any(|channel| {
            channel.iter().all(Option::is_some)
                && channel.iter().flatten().any(|d| {
                    let module = u16::from_le_bytes([d.raw[320], d.raw[321]]);
                    let dram = u16::from_le_bytes([d.raw[350], d.raw[351]]);
                    let date = u16::from_be_bytes([d.raw[323], d.raw[324]]);
                    (module == 0xad80 || dram == 0xad80) && date < 0x1545
                })
        });
        if !needed {
            return Ok(false);
        }
        let baseline = self.registers.channel[0].sched_second_cbit.get();
        self.registers
            .sched_second_cbit
            .set(baseline | SchedulerSecondCbit::DIS_SRX_MR4::SET.value);
        // The native physical controller count is two, including an empty channel.
        for c in &self.registers.channel {
            c.mr_params
                .modify(MrParams::MR4_PERIOD.val(0) + MrParams::DDR4_TS_READOUT::CLEAR);
            if policy.override_mode == OltmOverride::Refresh2x {
                c.maintenance_spare
                    .modify(MaintenanceSpare::FORCE_X2REF::SET);
            }
        }
        if policy.override_mode == OltmOverride::OpenLoop {
            policy.enable_oltm = true;
            policy.enable_cltm = false;
            policy.refresh_2x = 2;
            policy.user_power_weights = false;
        }
        policy.ddr_thermal_sensor = false;
        Ok(true)
    }

    /// `additional_steps.c:meminit_pre_training`, DDR4 path.
    ///
    /// The mode-register images must be those actually programmed by JEDEC
    /// initialization. MR2[10:9] controls whether the rank needs initial TX EQ
    /// and, on DT/Halo, constant-Z termination. Program all nine SDRAM lanes,
    /// including the ninth lane on non-ECC DIMMs, as the hardware source does.
    /// WDB initialization is unconditional for populated channels.
    pub fn pre_training(
        &self,
        mode: MemoryBootMode,
        package: Package,
        saved_mr: &ModeRegisters,
    ) -> Result<(), Error> {
        let ranks = cold_ranks(self, mode)?;
        for (ch, mask) in ranks.into_iter().enumerate() {
            for (rank, mr) in saved_mr.rank[ch].iter().enumerate() {
                if mask & (1 << rank) == 0 || mr[2] & (3 << 9) == 0 {
                    continue;
                }
                for lane in 0..SDRAM_LANES {
                    self.set_tx_eq(
                        RankLane {
                            channel: ch,
                            rank,
                            lane,
                        },
                        0x3f,
                    )?;
                    if package == Package::DesktopHalo {
                        self.registers.data_lane[lane][ch]
                            .control2
                            .modify(DataControl2::CONSTANT_Z_TX::SET);
                    }
                }
            }
        }
        self.setup_wdb(channels(ranks))
    }
}
