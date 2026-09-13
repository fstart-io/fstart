//! DisplayPort link-training orchestration helpers.
//!
//! This module ports libgfxinit's generic DP training state machine. Helpers
//! select a link from DPCD receiver capabilities, build sink DPCD writes, inspect
//! link status, and adjust training sets. The live GMCH helper wires those pieces
//! to AUX and GMCH DP source-register programming.

use crate::dp_aux::{
    DPCD_DOWNSPREAD_CTRL, DPCD_LINK_BW_SET, DPCD_LINK_STATUS, DPCD_RECEIVER_CAPS,
    DPCD_TRAINING_PATTERN_SET, DpLinkRate, LinkStatus, PreEmphasis, ReceiverCaps, TrainSet,
    TrainingPattern, VoltageSwing, gmch_native_aux_read, gmch_native_aux_write,
};
use crate::error::GmaError;
use crate::mmio::{Mmio, delay_us};
use crate::mode::Mode;
use crate::port::{
    GmchDpLinkConfig, PortRegisterOp, dp_enable_op, dp_idle_op, dp_off_op, dp_signal_levels_op,
    dp_training_pattern_op,
};
use crate::types::{Pipe, Port};

const RECEIVER_CAPS_LEN: usize = 15;
const LINK_STATUS_LEN: usize = 6;
const CLOCK_RECOVERY_MAX_TRIES: u8 = 32;
const CLOCK_RECOVERY_MAX_SAME_VS_TRIES: u8 = 5;
const CHANNEL_EQ_MAX_TRIES: u8 = 6;

/// Selected source/sink DP link parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DpLinkConfig {
    /// Link rate to write to DPCD LINK_BW_SET.
    pub link_rate: DpLinkRate,
    /// Active lane count.
    pub lane_count: u8,
    /// Enable enhanced framing on source and sink.
    pub enhanced_framing: bool,
    /// Sink supports TP3 and source path may use it.
    pub tps3_supported: bool,
    /// Raw AUX read interval field from DPCD caps.
    pub aux_rd_interval: u8,
}

impl DpLinkConfig {
    /// Convert to the GMCH source-register link config.
    pub(crate) const fn gmch(self) -> GmchDpLinkConfig {
        GmchDpLinkConfig {
            lane_count: self.lane_count,
            enhanced_framing: self.enhanced_framing,
        }
    }

    const fn dpcd_lane_count(self) -> u8 {
        self.lane_count | if self.enhanced_framing { 0x80 } else { 0 }
    }
}

/// Writes used to initialize sink link parameters before training.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkLinkConfigWrites {
    /// LINK_BW_SET value.
    pub link_bw_set: u8,
    /// LANE_COUNT_SET value.
    pub lane_count_set: u8,
    /// DOWNSPREAD_CTRL value.
    pub downspread_ctrl: u8,
    /// MAIN_LINK_CHANNEL_CODING_SET value.
    pub main_link_channel_coding: u8,
}

/// Ordered DP link settings to try for one sink/mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DpLinkConfigCandidates {
    configs: [Option<DpLinkConfig>; MAX_LINK_CONFIG_CANDIDATES],
    len: usize,
}

impl DpLinkConfigCandidates {
    /// Number of valid candidate settings.
    pub const fn len(self) -> usize {
        self.len
    }

    /// Return true when there are no usable link settings.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Return a candidate by index.
    pub const fn get(self, index: usize) -> Option<DpLinkConfig> {
        if index < self.len {
            self.configs[index]
        } else {
            None
        }
    }
}

const MAX_LINK_CONFIG_CANDIDATES: usize = 9;

/// Select the lowest bandwidth link that can carry a 24-bpp framebuffer mode.
pub fn select_link_config(
    caps: ReceiverCaps,
    mode: Mode,
    source_tps3: bool,
) -> Result<DpLinkConfig, GmaError> {
    link_config_candidates(caps, mode, source_tps3)?
        .get(0)
        .ok_or(GmaError::ModeUnavailable)
}

/// Return all sink-supported link settings that can carry a 24-bpp mode.
///
/// The order mirrors libgfxinit's `Preferred_Link_Setting` /
/// `Next_Link_Setting` outer loop: start with the lowest bandwidth usable
/// setting and then progress through larger lane/rate combinations when a
/// training attempt fails.
pub fn link_config_candidates(
    caps: ReceiverCaps,
    mode: Mode,
    source_tps3: bool,
) -> Result<DpLinkConfigCandidates, GmaError> {
    let max_lanes = normalize_lane_count(caps.max_lane_count)?;
    let rates = candidate_rates(caps.max_link_rate);
    let mut configs = [None; MAX_LINK_CONFIG_CANDIDATES];
    let mut len = 0usize;
    let mut lane = 1;
    while lane <= max_lanes {
        let mut rate_index = 0usize;
        while rate_index < rates.len() {
            let rate = rates[rate_index];
            if link_supports_mode(rate, lane, mode) && !contains_config(&configs, len, rate, lane) {
                configs[len] = Some(DpLinkConfig {
                    link_rate: rate,
                    lane_count: lane,
                    enhanced_framing: caps.enhanced_framing,
                    tps3_supported: caps.tps3_supported && source_tps3,
                    aux_rd_interval: caps.aux_rd_interval,
                });
                len += 1;
            }
            rate_index += 1;
        }
        lane = match lane {
            1 => 2,
            2 => 4,
            _ => break,
        };
    }
    if len == 0 {
        return Err(GmaError::ModeUnavailable);
    }
    Ok(DpLinkConfigCandidates { configs, len })
}

fn contains_config(
    configs: &[Option<DpLinkConfig>; MAX_LINK_CONFIG_CANDIDATES],
    len: usize,
    rate: DpLinkRate,
    lane_count: u8,
) -> bool {
    let mut index = 0usize;
    while index < len {
        if let Some(config) = configs[index]
            && config.link_rate == rate
            && config.lane_count == lane_count
        {
            return true;
        }
        index += 1;
    }
    false
}

/// Return the DPCD writes used by libgfxinit to configure the sink link.
pub const fn sink_link_config_writes(config: DpLinkConfig) -> SinkLinkConfigWrites {
    SinkLinkConfigWrites {
        link_bw_set: config.link_rate.dpcd_code(),
        lane_count_set: config.dpcd_lane_count(),
        downspread_ctrl: 0,
        main_link_channel_coding: 1,
    }
}

/// Build the DPCD training-pattern/lane payload starting at TRAINING_PATTERN_SET.
pub fn sink_training_payload(
    pattern: TrainingPattern,
    train_set: TrainSet,
    lane_count: u8,
    out: &mut [u8; 5],
) -> Result<usize, GmaError> {
    if !matches!(lane_count, 1 | 2 | 4) {
        return Err(GmaError::InvalidConfig);
    }
    out[0] = pattern.encode();
    let lane_value = train_set.encode(max_voltage_swing(), max_pre_emphasis(train_set));
    let mut idx = 0;
    while idx < lane_count as usize {
        out[1 + idx] = lane_value;
        idx += 1;
    }
    Ok(1 + lane_count as usize)
}

/// Adjust the current train set from sink adjust-request nibbles.
pub fn adjusted_train_set(status: LinkStatus, lane_count: u8) -> Result<TrainSet, GmaError> {
    if !matches!(lane_count, 1 | 2 | 4) {
        return Err(GmaError::InvalidConfig);
    }
    let mut lane = 0;
    let mut voltage_swing = VoltageSwing::Level0;
    let mut pre_emphasis = PreEmphasis::Level0;
    while lane < lane_count {
        let requested = status.requested_train_set(lane);
        if requested.voltage_swing > voltage_swing {
            voltage_swing = requested.voltage_swing;
        }
        if requested.pre_emphasis > pre_emphasis {
            pre_emphasis = requested.pre_emphasis;
        }
        lane += 1;
    }
    Ok(clamp_train_set(TrainSet {
        voltage_swing,
        pre_emphasis,
    }))
}

/// Train a GMCH DP link using live AUX transactions and source register ops.
#[allow(dead_code)]
pub(crate) fn train_gmch_dp(
    mmio: &Mmio,
    port: Port,
    pipe: Pipe,
    mode: Mode,
) -> Result<DpLinkConfig, GmaError> {
    train_gmch_dp_with_retry(mmio, port, pipe, mode, |_| Ok(()), || {})
}

/// Train a GMCH DP link, trying each usable link setting with rollback between failures.
///
/// libgfxinit's `Enable_Output` loops over `Preferred_Link_Setting` and
/// `Next_Link_Setting`, allocates the PLL for each setting, and tries each DP
/// lane/rate configuration twice. `pre_try` receives the candidate link config
/// so generation code can program the matching PLL before each attempt.
pub(crate) fn train_gmch_dp_with_retry<PreTry, Rollback>(
    mmio: &Mmio,
    port: Port,
    pipe: Pipe,
    mode: Mode,
    mut pre_try: PreTry,
    mut rollback: Rollback,
) -> Result<DpLinkConfig, GmaError>
where
    PreTry: FnMut(DpLinkConfig) -> Result<(), GmaError>,
    Rollback: FnMut(),
{
    let mut caps_bytes = [0u8; RECEIVER_CAPS_LEN];
    gmch_native_aux_read(mmio, port, DPCD_RECEIVER_CAPS, &mut caps_bytes)?;
    let caps = ReceiverCaps::parse(&caps_bytes)?;
    // GMCH instantiates the generic trainer with TPS3_Supported => False.
    let candidates = link_config_candidates(caps, mode, false)?;
    let mut last_error = GmaError::ModeUnavailable;
    let mut index = 0usize;
    while let Some(config) = candidates.get(index) {
        let mut try_count = 0;
        while try_count < 2 {
            // A candidate whose PLL/pipe setup fails (for example 5.4 Gbit/s on
            // GMCH, which has no DPLL tuple) is skipped rather than retried.
            if let Err(err) = pre_try(config) {
                last_error = err;
                rollback();
                break;
            }
            match train_gmch_dp_once(mmio, port, pipe, mode, config) {
                Ok(()) => return Ok(config),
                Err(err) => {
                    last_error = err;
                    let _ = clear_sink_and_source_training(mmio, port, config);
                    let _ = dp_failure_off(mmio, port);
                    rollback();
                }
            }
            try_count += 1;
        }
        index += 1;
    }
    Err(last_error)
}

fn train_gmch_dp_once(
    mmio: &Mmio,
    port: Port,
    pipe: Pipe,
    mode: Mode,
    config: DpLinkConfig,
) -> Result<(), GmaError> {
    apply_port_op(mmio, dp_enable_op(port, pipe, mode, config.gmch())?);
    let writes = sink_link_config_writes(config);
    gmch_native_aux_write(
        mmio,
        port,
        DPCD_LINK_BW_SET,
        &[writes.link_bw_set, writes.lane_count_set],
    )?;
    gmch_native_aux_write(
        mmio,
        port,
        DPCD_DOWNSPREAD_CTRL,
        &[writes.downspread_ctrl, writes.main_link_channel_coding],
    )?;

    let mut train_set = TrainSet {
        voltage_swing: VoltageSwing::Level0,
        pre_emphasis: PreEmphasis::Level0,
    };
    train_set = clock_recovery(mmio, port, config, train_set)?;
    channel_equalization(mmio, port, config, train_set)?;
    clear_sink_and_source_training(mmio, port, config)
}

fn clock_recovery(
    mmio: &Mmio,
    port: Port,
    config: DpLinkConfig,
    mut train_set: TrainSet,
) -> Result<TrainSet, GmaError> {
    set_source_and_sink_training(mmio, port, TrainingPattern::Pattern1, train_set, config)?;
    let mut same_vs_tries = 0;
    let mut last_vs = train_set.voltage_swing;
    let mut tries = 0;
    while tries < CLOCK_RECOVERY_MAX_TRIES {
        delay_us(clock_recovery_delay_us(config));
        let status = read_link_status(mmio, port)?;
        if status.clock_recovery_done(config.lane_count) {
            return Ok(train_set);
        }
        let next = adjusted_train_set(status, config.lane_count)?;
        if next.voltage_swing == last_vs {
            same_vs_tries += 1;
        } else {
            same_vs_tries = 0;
            last_vs = next.voltage_swing;
        }
        if same_vs_tries >= CLOCK_RECOVERY_MAX_SAME_VS_TRIES
            || train_set.voltage_swing == max_voltage_swing()
        {
            break;
        }
        train_set = next;
        set_source_and_sink_training(mmio, port, TrainingPattern::Pattern1, train_set, config)?;
        tries += 1;
    }
    // libgfxinit sends the sink TP_None before touching the source pattern.
    let _ = clear_sink_and_source_training(mmio, port, config);
    let _ = dp_failure_off(mmio, port);
    Err(GmaError::HardwareError)
}

fn channel_equalization(
    mmio: &Mmio,
    port: Port,
    config: DpLinkConfig,
    mut train_set: TrainSet,
) -> Result<(), GmaError> {
    let pattern = if config.tps3_supported {
        TrainingPattern::Pattern3
    } else {
        TrainingPattern::Pattern2
    };
    set_source_and_sink_training(mmio, port, pattern, train_set, config)?;
    let mut tries = 0;
    while tries < CHANNEL_EQ_MAX_TRIES {
        delay_us(channel_eq_delay_us(config));
        let status = read_link_status(mmio, port)?;
        if status.channel_eq_done(config.lane_count) {
            return Ok(());
        }
        if !status.clock_recovery_done(config.lane_count) {
            break;
        }
        train_set = adjusted_train_set(status, config.lane_count)?;
        set_source_and_sink_training(mmio, port, pattern, train_set, config)?;
        tries += 1;
    }
    let _ = clear_sink_and_source_training(mmio, port, config);
    let _ = dp_failure_off(mmio, port);
    Err(GmaError::HardwareError)
}

fn clear_sink_and_source_training(
    mmio: &Mmio,
    port: Port,
    config: DpLinkConfig,
) -> Result<(), GmaError> {
    let train_set = TrainSet {
        voltage_swing: VoltageSwing::Level0,
        pre_emphasis: PreEmphasis::Level0,
    };
    set_sink_training(
        mmio,
        port,
        TrainingPattern::None,
        train_set,
        config.lane_count,
    )?;
    apply_port_op(mmio, dp_training_pattern_op(port, TrainingPattern::None)?);
    Ok(())
}

fn set_source_and_sink_training(
    mmio: &Mmio,
    port: Port,
    pattern: TrainingPattern,
    train_set: TrainSet,
    config: DpLinkConfig,
) -> Result<(), GmaError> {
    apply_port_op(mmio, dp_training_pattern_op(port, pattern)?);
    apply_port_op(mmio, dp_signal_levels_op(port, train_set)?);
    set_sink_training(mmio, port, pattern, train_set, config.lane_count)
}

fn set_sink_training(
    mmio: &Mmio,
    port: Port,
    pattern: TrainingPattern,
    train_set: TrainSet,
    lane_count: u8,
) -> Result<(), GmaError> {
    let mut payload = [0u8; 5];
    let len = sink_training_payload(pattern, train_set, lane_count, &mut payload)?;
    gmch_native_aux_write(mmio, port, DPCD_TRAINING_PATTERN_SET, &payload[..len])
}

fn read_link_status(mmio: &Mmio, port: Port) -> Result<LinkStatus, GmaError> {
    let mut bytes = [0u8; LINK_STATUS_LEN];
    gmch_native_aux_read(mmio, port, DPCD_LINK_STATUS, &mut bytes)?;
    LinkStatus::parse(&bytes)
}

fn dp_failure_off(mmio: &Mmio, port: Port) -> Result<(), GmaError> {
    apply_port_op(mmio, dp_idle_op(port)?);
    apply_port_op(mmio, dp_off_op(port)?);
    Ok(())
}

fn apply_port_op(mmio: &Mmio, op: PortRegisterOp) {
    match op {
        PortRegisterOp::Write { register, value } => mmio.write32(register, value),
        PortRegisterOp::Update {
            register,
            mask_unset,
            mask_set,
        } => mmio.update32(register, mask_unset, mask_set),
    }
}

const fn max_voltage_swing() -> VoltageSwing {
    VoltageSwing::Level3
}

const fn max_pre_emphasis(train_set: TrainSet) -> PreEmphasis {
    match train_set.voltage_swing {
        VoltageSwing::Level0 => PreEmphasis::Level3,
        VoltageSwing::Level1 => PreEmphasis::Level2,
        VoltageSwing::Level2 => PreEmphasis::Level1,
        VoltageSwing::Level3 => PreEmphasis::Level0,
    }
}

const fn clamp_train_set(train_set: TrainSet) -> TrainSet {
    let max_pe = max_pre_emphasis(train_set);
    TrainSet {
        voltage_swing: train_set.voltage_swing,
        pre_emphasis: if (train_set.pre_emphasis as u8) > (max_pe as u8) {
            max_pe
        } else {
            train_set.pre_emphasis
        },
    }
}

const fn clock_recovery_delay_us(config: DpLinkConfig) -> u32 {
    if matches!(config.link_rate, DpLinkRate::Hbr2) && config.aux_rd_interval != 0 {
        (config.aux_rd_interval as u32) * 4_000
    } else {
        100
    }
}

const fn channel_eq_delay_us(config: DpLinkConfig) -> u32 {
    if config.aux_rd_interval != 0 {
        (config.aux_rd_interval as u32) * 4_000
    } else {
        400
    }
}

const fn normalize_lane_count(raw: u8) -> Result<u8, GmaError> {
    match raw & 0x1f {
        1 => Ok(1),
        2 => Ok(2),
        4 => Ok(4),
        _ => Err(GmaError::InvalidConfig),
    }
}

fn candidate_rates(max: DpLinkRate) -> [DpLinkRate; 3] {
    match max {
        DpLinkRate::Rbr => [DpLinkRate::Rbr, DpLinkRate::Rbr, DpLinkRate::Rbr],
        DpLinkRate::Hbr => [DpLinkRate::Rbr, DpLinkRate::Hbr, DpLinkRate::Hbr],
        DpLinkRate::Hbr2 => [DpLinkRate::Rbr, DpLinkRate::Hbr, DpLinkRate::Hbr2],
        DpLinkRate::Unknown(_) => [DpLinkRate::Rbr, DpLinkRate::Hbr, DpLinkRate::Hbr],
    }
}

const fn link_supports_mode(rate: DpLinkRate, lane_count: u8, mode: Mode) -> bool {
    let required_kbps = mode.pixel_clock_khz as u64 * 24;
    let link_kbps = link_payload_kbps_per_lane(rate) * lane_count as u64;
    required_kbps <= link_kbps
}

const fn link_payload_kbps_per_lane(rate: DpLinkRate) -> u64 {
    match rate {
        DpLinkRate::Rbr => 1_296_000,
        DpLinkRate::Hbr => 2_160_000,
        DpLinkRate::Hbr2 => 4_320_000,
        DpLinkRate::Unknown(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dp_aux::{
        DPCD_LANE_COUNT_SET, DPCD_MAIN_LINK_CHANNEL_CODING_SET, DPCD_TRAINING_LANE0_SET,
    };

    fn caps(rate: DpLinkRate, lanes: u8) -> ReceiverCaps {
        ReceiverCaps {
            dpcd_revision: 0x12,
            max_link_rate: rate,
            max_lane_count: lanes,
            tps3_supported: true,
            enhanced_framing: true,
            no_aux_handshake: false,
            aux_rd_interval: 2,
        }
    }

    #[test]
    fn selects_lowest_link_config_that_fits_mode() {
        let config = select_link_config(caps(DpLinkRate::Hbr, 4), Mode::XGA_1024X768_60, true).unwrap();
        assert_eq!(config.link_rate, DpLinkRate::Hbr);
        assert_eq!(config.lane_count, 1);
        assert!(config.enhanced_framing);
        assert_eq!(config.aux_rd_interval, 2);
    }

    #[test]
    fn link_config_candidates_progress_to_higher_bandwidth_settings() {
        let configs =
            link_config_candidates(caps(DpLinkRate::Hbr, 4), Mode::XGA_1024X768_60, true).unwrap();
        assert_eq!(configs.len(), 5);
        assert_eq!(configs.get(0).unwrap().link_rate, DpLinkRate::Hbr);
        assert_eq!(configs.get(0).unwrap().lane_count, 1);
        assert_eq!(configs.get(1).unwrap().link_rate, DpLinkRate::Rbr);
        assert_eq!(configs.get(1).unwrap().lane_count, 2);
        assert_eq!(configs.get(4).unwrap().link_rate, DpLinkRate::Hbr);
        assert_eq!(configs.get(4).unwrap().lane_count, 4);
        assert_eq!(configs.get(5), None);
    }

    #[test]
    fn gmch_source_capability_gates_training_pattern_three() {
        // Sink advertises TP3, but the GMCH source does not implement it.
        let with_source = link_config_candidates(caps(DpLinkRate::Hbr, 1), Mode::XGA_1024X768_60, true)
            .unwrap()
            .get(0)
            .unwrap();
        assert!(with_source.tps3_supported);
        let gmch = link_config_candidates(caps(DpLinkRate::Hbr, 1), Mode::XGA_1024X768_60, false)
            .unwrap()
            .get(0)
            .unwrap();
        assert!(!gmch.tps3_supported);
    }

    #[test]
    fn rejects_modes_that_exceed_sink_bandwidth() {
        let mut mode = Mode::XGA_1024X768_60;
        mode.pixel_clock_khz = 600_000;
        assert_eq!(
            select_link_config(caps(DpLinkRate::Rbr, 1), mode, true),
            Err(GmaError::ModeUnavailable)
        );
    }

    #[test]
    fn prepares_sink_link_config_writes() {
        let config = DpLinkConfig {
            link_rate: DpLinkRate::Hbr,
            lane_count: 4,
            enhanced_framing: true,
            tps3_supported: false,
            aux_rd_interval: 0,
        };
        let writes = sink_link_config_writes(config);
        assert_eq!(writes.link_bw_set, 0x0a);
        assert_eq!(writes.lane_count_set, 0x84);
        assert_eq!(writes.downspread_ctrl, 0);
        assert_eq!(writes.main_link_channel_coding, 1);
        assert_eq!(config.gmch().lane_count, 4);
    }

    #[test]
    fn prepares_training_payload_for_active_lanes() {
        let mut payload = [0u8; 5];
        let len = sink_training_payload(
            TrainingPattern::Pattern1,
            TrainSet {
                voltage_swing: VoltageSwing::Level3,
                pre_emphasis: PreEmphasis::Level2,
            },
            2,
            &mut payload,
        )
        .unwrap();
        assert_eq!(len, 3);
        assert_eq!(payload[0], 0x21);
        // VS level 3 sets max-swing; level 3 permits only PE0, so PE2 clamps
        // before encoding through max_pre_emphasis in the orchestration path.
        assert_eq!(payload[1], 0x37);
        assert_eq!(payload[2], 0x37);
    }

    #[test]
    fn adjusts_train_set_from_link_status_requests() {
        let status = LinkStatus::parse(&[0, 0, 0, 0, 0x95, 0]).unwrap();
        let adjusted = adjusted_train_set(status, 2).unwrap();
        assert_eq!(adjusted.voltage_swing, VoltageSwing::Level1);
        assert_eq!(adjusted.pre_emphasis, PreEmphasis::Level2);
    }

    #[test]
    fn delay_rules_match_libgfxinit_training_phases() {
        let hbr2 = DpLinkConfig {
            link_rate: DpLinkRate::Hbr2,
            lane_count: 4,
            enhanced_framing: true,
            tps3_supported: true,
            aux_rd_interval: 3,
        };
        assert_eq!(clock_recovery_delay_us(hbr2), 12_000);
        assert_eq!(channel_eq_delay_us(hbr2), 12_000);
        let rbr = DpLinkConfig {
            link_rate: DpLinkRate::Rbr,
            ..hbr2
        };
        assert_eq!(clock_recovery_delay_us(rbr), 100);
        assert_eq!(
            channel_eq_delay_us(DpLinkConfig {
                aux_rd_interval: 0,
                ..hbr2
            }),
            400
        );
    }

    #[test]
    fn dpcd_addresses_are_the_expected_training_sequence() {
        assert_eq!(DPCD_TRAINING_PATTERN_SET + 1, DPCD_TRAINING_LANE0_SET);
        assert_eq!(DPCD_LINK_BW_SET + 1, DPCD_LANE_COUNT_SET);
        assert_eq!(DPCD_DOWNSPREAD_CTRL + 1, DPCD_MAIN_LINK_CHANNEL_CODING_SET);
    }
}
