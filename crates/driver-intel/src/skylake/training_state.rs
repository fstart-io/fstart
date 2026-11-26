// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host calibration mirrors from native/native_init.h:rank_training,
//! channel_training, training_output and the calibration fields of meminit_ctx.
//! These are working data, NOT proof of successful training or a cache format.

use super::jedec::ModeRegisters;

pub const CHANNELS: usize = 2;
pub const RANKS: usize = 4;
pub const BYTE_LANES: usize = 8;
pub const SDRAM_LANES: usize = 9;
pub const DQ_BITS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankTraining {
    pub wl_dly: [u16; SDRAM_LANES],
    pub rd_dqs_dly: [u8; DQ_BITS],
    /// Per-bit RX timing deskew, packed eight four-bit codes per byte in hardware.
    pub rd_dq_dly: [u8; DQ_BITS],
    /// Per-bit RX voltage offsets; eight DQ lanes only, not ECC feedback.
    pub rx_vref_per_bit: [u8; DQ_BITS],
    pub rcven_dly: [u16; SDRAM_LANES],
    pub tx_dq_dly: [u16; SDRAM_LANES],
    pub wr_dq_dly: [u8; DQ_BITS],
    pub wr_dqs_dly: [u8; DQ_BITS],
    pub rt_latency: u8,
    pub io_latency: u8,
}
impl RankTraining {
    pub const ZERO: Self = Self {
        wl_dly: [0; SDRAM_LANES],
        rd_dqs_dly: [0; DQ_BITS],
        rd_dq_dly: [0; DQ_BITS],
        rx_vref_per_bit: [0; DQ_BITS],
        rcven_dly: [0; SDRAM_LANES],
        tx_dq_dly: [0; SDRAM_LANES],
        wr_dq_dly: [0; DQ_BITS],
        wr_dqs_dly: [0; DQ_BITS],
        rt_latency: 0,
        io_latency: 0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelTraining {
    pub rank: [RankTraining; RANKS],
    pub cmd_dly: u8,
    pub rt_iocomp: u8,
    /// Raw DATAOFFSETCOMP host mirror; broadcast writes make live reads an
    /// unreliable substitute. The consuming optimization must initialize it.
    pub data_comp_offset: [u32; SDRAM_LANES],
    /// Signed RX Vref, distinct from DATAOFFSETCOMP.
    pub rd_vref: [i8; SDRAM_LANES],
    /// Late read-2D per-physical-rank RX Vref delta from the common byte center.
    /// Sum with rd_vref in widened signed arithmetic before six-bit encoding.
    pub rx_vref_rank_offset: [[i8; SDRAM_LANES]; RANKS],
    /// Full host values, masked to six bits only when written to RXTRAIN.
    pub rx_dqs_p: [[u8; SDRAM_LANES]; RANKS],
    pub rx_dqs_n: [[u8; SDRAM_LANES]; RANKS],
}
impl ChannelTraining {
    pub const ZERO: Self = Self {
        rank: [RankTraining::ZERO; RANKS],
        cmd_dly: 0,
        rt_iocomp: 0,
        data_comp_offset: [0; SDRAM_LANES],
        rd_vref: [0; SDRAM_LANES],
        rx_vref_rank_offset: [[0; SDRAM_LANES]; RANKS],
        rx_dqs_p: [[0; SDRAM_LANES]; RANKS],
        rx_dqs_n: [[0; SDRAM_LANES]; RANKS],
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrainingOutput {
    pub ch: [ChannelTraining; CHANNELS],
    pub final_freq_khz: u32,
    pub final_cl: u16,
    pub final_cwl: u16,
    pub final_trcd: u16,
    pub final_trp: u16,
    pub final_tras: u16,
}
impl TrainingOutput {
    pub const ZERO: Self = Self {
        ch: [ChannelTraining::ZERO; CHANNELS],
        final_freq_khz: 0,
        final_cl: 0,
        final_cwl: 0,
        final_trcd: 0,
        final_trp: 0,
        final_tras: 0,
    };
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MarginLimit {
    pub param: u8,
    pub upm_limit: u16,
    pub pwr_limit: u16,
    pub retrain_limit: u16,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OdtPowerSaving {
    pub base_flag: bool,
    pub base_saving_rd: u16,
    pub base_saving_wr: u16,
    pub base_saving_cmd: u16,
    pub meminit_saving_rd: u16,
    pub meminit_saving_wr: u16,
    pub meminit_saving_cmd: u16,
}

/// Shared calibration state from meminit_ctx, separate from pipeline policy.
/// Caller owns its
/// storage (not a small CAR-stack temporary). Zero initialization is only
/// workspace allocation: no final values or success state are inferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalibrationState {
    pub training: TrainingOutput,
    /// The same per-physical-rank images produced by cold JEDEC setup.
    pub saved_mr: ModeRegisters,
    pub saved_rtl: [u32; CHANNELS],
    pub saved_iol: [u32; CHANNELS],
    pub margin_limits: [MarginLimit; 11],
    /// [channel][rank][byte][result type * 2 + side], in 1/10 units.
    pub margin_results: [[[[u32; 16]; SDRAM_LANES]; RANKS]; CHANNELS],
    pub retrain_iteration: u8,
    pub upm_pwr_retrain_flag: u8,
    pub ddr4_pda_enable: bool,
    pub en_dummy_reads: u8,
    pub dq_pat: u8,
    pub dq_pat_lc: u16,
    pub ddr4_tx_vref_range: [u8; CHANNELS],
    pub ddr4_pda_mr6: [[[u8; SDRAM_LANES]; RANKS]; CHANNELS],
    pub mchbar_tc_rdrd: [u32; CHANNELS],
    pub mchbar_tc_rdwr: [u32; CHANNELS],
    pub mchbar_tc_odt: [u32; CHANNELS],
    pub mchbar_sc_gs_cfg: [u32; CHANNELS],
    pub odt_power_saving: OdtPowerSaving,
    pub spd_security_status: u8,
}
impl CalibrationState {
    /// Clear caller-owned per-attempt storage BEFORE initial PHY/timing/MR
    /// producers publish their actual programmed values. Native clears after
    /// PHY and inadvertently erases its new P/N and electrical Vref mirrors.
    ///
    /// This does NOT stop/reset/reinitialize hardware or authorize a retry.
    /// Caller must own stopped, unlocked cold hardware; only a genuine source
    /// retrain request permits attempt1. Margin policy (including increases)
    /// and SPD security status survive; immutable dummy-read policy is supplied
    /// afresh. No measurement, cache validity or memory-ready result is created.
    pub fn clear_cold_attempt(
        &mut self,
        mode: crate::sunrise_point::pmc::MemoryBootMode,
        iteration: u8,
        dummy_reads: bool,
    ) -> Result<(), super::Error> {
        if !matches!(
            mode,
            crate::sunrise_point::pmc::MemoryBootMode::Cold
                | crate::sunrise_point::pmc::MemoryBootMode::S3
        ) {
            return Err(super::Error::RetainedMemoryUnsupported);
        }
        // native training.c MAX_RETRAIN_ITERATIONS=1 means two TOTAL attempts.
        if iteration > 1 {
            return Err(super::Error::InvalidPolicy);
        }
        self.training = TrainingOutput::ZERO;
        self.saved_mr.rank.fill([[0; 7]; RANKS]);
        self.saved_rtl.fill(0);
        self.saved_iol.fill(0);
        self.margin_results.fill([[[0; 16]; SDRAM_LANES]; RANKS]);
        self.retrain_iteration = iteration;
        self.upm_pwr_retrain_flag = 1;
        self.ddr4_pda_enable = false;
        self.en_dummy_reads = u8::from(dummy_reads);
        self.dq_pat = 255;
        self.dq_pat_lc = 0;
        self.ddr4_tx_vref_range.fill(0);
        self.ddr4_pda_mr6.fill([[0; SDRAM_LANES]; RANKS]);
        self.mchbar_tc_rdrd.fill(0);
        self.mchbar_tc_rdwr.fill(0);
        self.mchbar_tc_odt.fill(0);
        self.mchbar_sc_gs_cfg.fill(0);
        self.odt_power_saving = OdtPowerSaving::default();
        Ok(())
    }
}

impl CalibrationState {
    pub const INITIAL: Self = Self {
        training: TrainingOutput::ZERO,
        saved_mr: ModeRegisters {
            rank: [[[0; 7]; RANKS]; CHANNELS],
        },
        saved_rtl: [0; CHANNELS],
        saved_iol: [0; CHANNELS],
        margin_limits: [MarginLimit {
            param: 0,
            upm_limit: 0,
            pwr_limit: 0,
            retrain_limit: 0,
        }; 11],
        margin_results: [[[[0; 16]; SDRAM_LANES]; RANKS]; CHANNELS],
        retrain_iteration: 0,
        upm_pwr_retrain_flag: 0,
        ddr4_pda_enable: false,
        en_dummy_reads: 0,
        dq_pat: 0,
        dq_pat_lc: 0,
        ddr4_tx_vref_range: [0; CHANNELS],
        ddr4_pda_mr6: [[[0; SDRAM_LANES]; RANKS]; CHANNELS],
        mchbar_tc_rdrd: [0; CHANNELS],
        mchbar_tc_rdwr: [0; CHANNELS],
        mchbar_tc_odt: [0; CHANNELS],
        mchbar_sc_gs_cfg: [0; CHANNELS],
        odt_power_saving: OdtPowerSaving {
            base_flag: false,
            base_saving_rd: 0,
            base_saving_wr: 0,
            base_saving_cmd: 0,
            meminit_saving_rd: 0,
            meminit_saving_wr: 0,
            meminit_saving_cmd: 0,
        },
        spd_security_status: 0,
    };

    /// Initialize exclusive reserved storage; this is not trained calibration.
    /// # Safety
    /// Destination must be aligned, writable, exclusive, nonoverlapping with the
    /// initialization image, and live throughout use. Its old value needs no drop.
    pub unsafe fn initialize_at(destination: *mut Self) {
        const {
            assert!(!core::mem::needs_drop::<Self>());
        }
        static IMAGE: CalibrationState = CalibrationState::INITIAL;
        unsafe { core::ptr::copy_nonoverlapping(&IMAGE, destination, 1) };
    }
}

impl Default for CalibrationState {
    fn default() -> Self {
        Self::INITIAL
    }
}
