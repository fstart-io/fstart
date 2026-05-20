//! Pure Sandy Bridge training state shared by native raminit phases.
//!
//! This mirrors the non-hardware parts of coreboot's `ram_rank_timings` and
//! the training-related constants in `raminit_common.h`.  Keeping these as pure
//! data structures lets the later receive-enable/write/read/command-training
//! ports share one layout without touching MCHBAR yet.

use super::state::ControllerTopology;
use super::timing::{TimingParams, TCK_1066MHZ, TCK_533MHZ, TCK_666MHZ, TCK_800MHZ, TCK_933MHZ};
use super::NUM_CHANNELS;

/// Sandy Bridge has four slot-ranks per channel.
pub const NUM_SLOTRANKS: usize = 4;
/// Sandy Bridge has eight data byte lanes plus one ECC lane slot.
pub const NUM_LANES: usize = 9;
/// X220 SO-DIMMs are non-ECC, so native raminit trains only byte lanes 0..7.
pub const NUM_DATA_LANES: usize = 8;

/// 1 QCLK is one half of a full memory clock and 64 PI ticks.
pub const QCLK_PI: i16 = 64;
/// Maximum command/control/clock PI value used by coreboot.
pub const CCC_MAX_PI: i16 = 2 * QCLK_PI - 1;
/// Maximum edge timing accepted by the native training algorithms.
pub const MAX_EDGE_TIMING: u8 = 71;
/// Maximum TX DQ timing accepted by the native training algorithms.
pub const MAX_TX_DQ: i16 = 127;
/// Maximum TX DQS timing accepted by the native training algorithms.
pub const MAX_TX_DQS: u16 = 511;
/// Maximum receive-enable timing accepted by the native training algorithms.
pub const MAX_RCVEN: u16 = 127;

/// Per-byte-lane training values for a single slot-rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LaneTimings {
    /// GDCR RX receive-enable PI code.
    pub rcven: u16,
    /// GDCR RX positive DQS PI code.
    pub rx_dqs_p: u8,
    /// GDCR RX negative DQS PI code.
    pub rx_dqs_n: u8,
    /// GDCR TX DQ PI code. Signed while training sweeps around zero.
    pub tx_dq: i16,
    /// GDCR TX DQS PI code.
    pub tx_dqs: u16,
}

/// Per-slot-rank training values, matching coreboot `ram_rank_timings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankTimings {
    /// ROUNDT_LAT register value for this slot-rank.
    pub roundtrip_latency: u8,
    /// IO_LATENCY nibble value for this slot-rank.
    pub io_latency: u8,
    /// Command/control phase-interpolator coding.
    pub pi_coding: i16,
    /// Lane timings for data lanes 0..7 and ECC lane 8.
    pub lanes: [LaneTimings; NUM_LANES],
}

impl Default for RankTimings {
    fn default() -> Self {
        Self {
            roundtrip_latency: 0,
            io_latency: 0,
            pi_coding: 0,
            lanes: [LaneTimings::default(); NUM_LANES],
        }
    }
}

/// Pure training-state container indexed by channel and slot-rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrainingState {
    pub ranks: [[RankTimings; NUM_SLOTRANKS]; NUM_CHANNELS],
    pub active_lanes: usize,
    pub populated_rankmap: [u8; NUM_CHANNELS],
    pub edge_offset: [u8; 3],
    pub tx_dq_offset: [u8; 3],
    pub pi_coding_threshold: u8,
    pub pi_code_offset: u8,
    pub ref_card_offset: [u8; NUM_CHANNELS],
    pub cmd_stretch: [u8; NUM_CHANNELS],
}

impl TrainingState {
    /// Create an empty training state sized for the current topology.
    pub fn for_topology(topology: &ControllerTopology) -> Self {
        Self {
            ranks: [[RankTimings::default(); NUM_SLOTRANKS]; NUM_CHANNELS],
            active_lanes: NUM_DATA_LANES,
            populated_rankmap: topology.rankmap,
            ..Self::default()
        }
    }

    pub fn apply_timing(&mut self, timing: &TimingParams) {
        let (edge, tx_dq, threshold) = timing_offsets(timing.tck_256ns);
        self.edge_offset = edge;
        self.tx_dq_offset = tx_dq;
        self.pi_coding_threshold = threshold;
        self.pi_code_offset = ((256_000 / timing.tck_256ns) / 66) as u8;
    }

    /// Iterate populated `(channel, slotrank)` pairs in coreboot order.
    pub fn populated_ranks(&self) -> PopulatedRanks<'_> {
        PopulatedRanks {
            state: self,
            channel: 0,
            slotrank: 0,
        }
    }

    /// Clamp signed TX DQ values to the range accepted by Sandy Bridge training.
    pub const fn clamp_tx_dq(value: i16) -> i16 {
        if value < -MAX_TX_DQ {
            -MAX_TX_DQ
        } else if value > MAX_TX_DQ {
            MAX_TX_DQ
        } else {
            value
        }
    }

    /// Clamp CCC PI codes to the hardware range used by `program_timings()`.
    pub const fn clamp_ccc_pi(value: i16) -> i16 {
        if value < -CCC_MAX_PI {
            -CCC_MAX_PI
        } else if value > CCC_MAX_PI {
            CCC_MAX_PI
        } else {
            value
        }
    }
}

impl Default for TrainingState {
    fn default() -> Self {
        Self {
            ranks: [[RankTimings::default(); NUM_SLOTRANKS]; NUM_CHANNELS],
            active_lanes: NUM_DATA_LANES,
            populated_rankmap: [0; NUM_CHANNELS],
            edge_offset: [0; 3],
            tx_dq_offset: [0; 3],
            pi_coding_threshold: 0,
            pi_code_offset: 0,
            ref_card_offset: [0; NUM_CHANNELS],
            cmd_stretch: [0; NUM_CHANNELS],
        }
    }
}

fn timing_offsets(tck: u32) -> ([u8; 3], [u8; 3], u8) {
    if tck <= TCK_1066MHZ {
        ([16, 7, 7], [18, 7, 7], 13)
    } else if tck <= TCK_933MHZ {
        ([14, 6, 6], [15, 6, 6], 15)
    } else if tck <= TCK_800MHZ {
        ([13, 5, 5], [14, 5, 5], 15)
    } else if tck <= TCK_666MHZ {
        ([10, 4, 4], [11, 4, 4], 16)
    } else if tck <= TCK_533MHZ {
        ([8, 3, 3], [9, 3, 3], 17)
    } else {
        ([6, 2, 2], [6, 2, 2], 17)
    }
}

/// Iterator over populated ranks in channel-major, slotrank-major order.
pub struct PopulatedRanks<'a> {
    state: &'a TrainingState,
    channel: usize,
    slotrank: usize,
}

impl Iterator for PopulatedRanks<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<Self::Item> {
        while self.channel < NUM_CHANNELS {
            while self.slotrank < NUM_SLOTRANKS {
                let slotrank = self.slotrank;
                self.slotrank += 1;
                if (self.state.populated_rankmap[self.channel] & (1 << slotrank)) != 0 {
                    return Some((self.channel, slotrank));
                }
            }
            self.channel += 1;
            self.slotrank = 0;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iterates_populated_ranks_in_coreboot_order() {
        let topology = ControllerTopology {
            rankmap: [0b0101, 0b0011],
            ..ControllerTopology::default()
        };
        let state = TrainingState::for_topology(&topology);
        let ranks: heapless::Vec<_, 8> = state.populated_ranks().collect();
        assert_eq!(ranks.as_slice(), &[(0, 0), (0, 2), (1, 0), (1, 1)]);
        assert_eq!(state.active_lanes, NUM_DATA_LANES);
    }

    #[test]
    fn clamps_training_pi_values_to_coreboot_limits() {
        assert_eq!(TrainingState::clamp_tx_dq(140), MAX_TX_DQ);
        assert_eq!(TrainingState::clamp_tx_dq(-140), -MAX_TX_DQ);
        assert_eq!(TrainingState::clamp_ccc_pi(200), CCC_MAX_PI);
        assert_eq!(TrainingState::clamp_ccc_pi(-200), -CCC_MAX_PI);
    }
}
