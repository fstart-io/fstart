//! DisplayPort link-training orchestration helpers.
//!
//! This module ports libgfxinit's generic DP training state machine. Helpers
//! select a link from DPCD receiver capabilities, build sink DPCD writes, inspect
//! link status, and adjust training sets. The live GMCH helper wires those pieces
//! to AUX and GMCH DP source-register programming.

use crate::gma::dp_aux::{
    AuxRegs, DPCD_DOWNSPREAD_CTRL, DPCD_LINK_BW_SET, DPCD_LINK_STATUS, DPCD_RECEIVER_CAPS,
    DPCD_TRAINING_LANE0_SET, DPCD_TRAINING_PATTERN_SET, DpLinkRate, LinkStatus, PreEmphasis,
    ReceiverCaps, TrainSet, TrainingPattern, VoltageSwing, gmch_native_aux_read,
    gmch_native_aux_write, native_aux_read, native_aux_write,
};
use crate::gma::error::GmaError;
use crate::gma::mmio::{Mmio, delay_us};
use crate::gma::mode::Mode;
use crate::gma::port::{
    GmchDpLinkConfig, PortRegisterOp, dp_enable_op, dp_idle_op, dp_off_op, dp_signal_levels_op,
    dp_training_pattern_op,
};
use crate::gma::types::{Pipe, Port};

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

/// DisplayPort data and link M/N values (libgfxinit `DP_Info.Calculate_M_N`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DpMn {
    pub data_m: u32,
    pub data_n: u32,
    pub link_m: u32,
    pub link_n: u32,
}

/// Largest M or N value the registers hold.
const M_N_MAX: u64 = (1 << 24) - 1;
const DATA_N_MAX: u64 = 0x80_0000;
const LINK_N_MAX: u64 = 0x10_0000;

/// libgfxinit `Cancel_M_N`: round N up to a power of two at most `n_max`,
/// scale M with it and keep both within the register width.
const fn cancel_m_n(m: u64, n: u64, n_max: u64) -> (u32, u32) {
    let mut rounded = n_max;
    while rounded / 2 >= n {
        rounded /= 2;
    }
    let mut m = m * rounded / n;
    let mut n = rounded;
    while m > M_N_MAX {
        m /= 2;
        n /= 2;
    }
    (m as u32, n as u32)
}

impl DpLinkConfig {
    /// Symbol clock in Hz: a tenth of the lane bit rate.
    pub(crate) const fn symbol_rate_hz(self) -> u64 {
        match self.link_rate {
            DpLinkRate::Hbr => 270_000_000,
            DpLinkRate::Hbr2 => 540_000_000,
            _ => 162_000_000,
        }
    }

    /// M/N for a mode of `bpc` bits per colour on this link.
    pub(crate) const fn m_n(self, mode: Mode, bpc: u64) -> DpMn {
        let dotclock = mode.pixel_clock_khz as u64 * 1000;
        let (data_m, data_n) = cancel_m_n(
            3 * bpc * dotclock,
            8 * self.symbol_rate_hz() * self.lane_count as u64,
            DATA_N_MAX,
        );
        let (link_m, link_n) = cancel_m_n(dotclock, self.symbol_rate_hz(), LINK_N_MAX);
        DpMn {
            data_m,
            data_n,
            link_m,
            link_n,
        }
    }

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
    #[repr(C)]
    #[derive(Clone, Copy, zerocopy::IntoBytes, zerocopy::Immutable)]
    struct TrainingPayloadWire {
        pattern: u8,
        lanes: [u8; 4],
    }
    use zerocopy::IntoBytes;

    if !matches!(lane_count, 1 | 2 | 4) {
        return Err(GmaError::InvalidConfig);
    }
    let lane_value = train_set.encode(max_voltage_swing(), max_pre_emphasis(train_set));
    let wire = TrainingPayloadWire {
        pattern: pattern.encode(),
        lanes: [lane_value; 4],
    };
    let len = 1 + lane_count as usize;
    out[..len].copy_from_slice(&wire.as_bytes()[..len]);
    Ok(len)
}

/// Adjust the current train set from sink adjust-request nibbles, clamped to `max_vs`.
pub fn adjusted_train_set_with_max(
    status: LinkStatus,
    lane_count: u8,
    max_vs: VoltageSwing,
) -> Result<TrainSet, GmaError> {
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
        voltage_swing: voltage_swing.min(max_vs),
        pre_emphasis,
    }))
}

/// Adjust the current train set from sink adjust-request nibbles.
pub fn adjusted_train_set(status: LinkStatus, lane_count: u8) -> Result<TrainSet, GmaError> {
    adjusted_train_set_with_max(status, lane_count, max_voltage_swing())
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

    let initial = TrainSet {
        voltage_swing: VoltageSwing::Level0,
        pre_emphasis: PreEmphasis::Level0,
    };
    set_source_and_sink_training(mmio, port, TrainingPattern::Pattern1, initial, config)?;
    let train_set = clock_recovery_loop(
        &mut |levels| {
            apply_port_op(mmio, dp_signal_levels_op(port, levels)?);
            let lane = levels.encode(max_voltage_swing(), max_pre_emphasis(levels));
            let data = [lane; 4];
            gmch_native_aux_write(
                mmio,
                port,
                DPCD_TRAINING_LANE0_SET,
                &data[..usize::from(config.lane_count)],
            )
        },
        &mut || read_link_status(mmio, port),
        config,
        max_voltage_swing(),
        initial,
    )?;
    channel_equalization(
        &mut |pattern, levels| set_source_and_sink_training(mmio, port, pattern, levels, config),
        &mut |levels| {
            apply_port_op(mmio, dp_signal_levels_op(port, levels)?);
            let lane = levels.encode(max_voltage_swing(), max_pre_emphasis(levels));
            let data = [lane; 4];
            gmch_native_aux_write(
                mmio,
                port,
                DPCD_TRAINING_LANE0_SET,
                &data[..usize::from(config.lane_count)],
            )
        },
        &mut || read_link_status(mmio, port),
        config,
        max_voltage_swing(),
        train_set,
    )?;
    clear_sink_and_source_training(mmio, port, config)
}

/// Train a selected DDI link. The generation owns the source buffer and
/// pattern registers, including the idle-to-stream transition and rollback.
pub(crate) fn train_ddi_link(
    mmio: &Mmio,
    port: Port,
    config: DpLinkConfig,
    max_vs: VoltageSwing,
    mut set_source_pattern: impl FnMut(TrainingPattern) -> Result<(), GmaError>,
    mut set_source_levels: impl FnMut(TrainSet) -> Result<(), GmaError>,
) -> Result<(), GmaError> {
    if !matches!(config.lane_count, 1 | 2 | 4) {
        return Err(GmaError::InvalidConfig);
    }
    let aux = AuxRegs::gen9_for_port(port)?;
    let lane_count = usize::from(config.lane_count);
    let write = |address, bytes: &[u8]| native_aux_write(mmio, aux, address, bytes);
    let clamp = |levels: TrainSet| {
        clamp_train_set(TrainSet {
            voltage_swing: levels.voltage_swing.min(max_vs),
            pre_emphasis: levels.pre_emphasis,
        })
    };
    let initial = TrainSet {
        voltage_swing: VoltageSwing::Level0,
        pre_emphasis: PreEmphasis::Level0,
    };
    // Match libgfxinit hw-gfx-dp_training.adb: program source TP1 and enable
    // DDI_BUF_CTL at Level0/Level0 once before configuring the sink, then tell
    // the sink to enter TP1 and update only signal levels (DDI_BUF_CTL + DPCD
    // TRAINING_LANEx_SET) on CR/EQ retries.
    set_source_pattern(TrainingPattern::Pattern1)?;
    set_source_levels(initial)?;
    let writes = sink_link_config_writes(config);
    write(
        DPCD_LINK_BW_SET,
        &[writes.link_bw_set, writes.lane_count_set],
    )?;
    write(
        DPCD_DOWNSPREAD_CTRL,
        &[writes.downspread_ctrl, writes.main_link_channel_coding],
    )?;
    let result = (|| {
        let mut init_payload = [initial.encode(max_vs, max_pre_emphasis(initial)); 5];
        init_payload[0] = TrainingPattern::Pattern1.encode();
        write(DPCD_TRAINING_PATTERN_SET, &init_payload[..1 + lane_count])?;

        let mut set_levels = |levels: TrainSet| {
            let levels = clamp(levels);
            set_source_levels(levels)?;
            let lane = levels.encode(max_vs, max_pre_emphasis(levels));
            let data = [lane; 4];
            write(DPCD_TRAINING_LANE0_SET, &data[..lane_count])
        };
        let mut status = || {
            let mut data = [0; LINK_STATUS_LEN];
            native_aux_read(mmio, aux, DPCD_LINK_STATUS, &mut data)?;
            LinkStatus::parse(&data)
        };
        let train_set = clock_recovery_loop(&mut set_levels, &mut status, config, max_vs, initial)?;
        channel_equalization(
            &mut |pattern, levels| {
                let levels = clamp(levels);
                set_source_pattern(pattern)?;
                let lane = levels.encode(max_vs, max_pre_emphasis(levels));
                let mut data = [lane; 5];
                data[0] = pattern.encode();
                write(DPCD_TRAINING_PATTERN_SET, &data[..1 + lane_count])
            },
            &mut set_levels,
            &mut status,
            config,
            max_vs,
            train_set,
        )
    })();
    let clear = write(DPCD_TRAINING_PATTERN_SET, &[0]);
    result.and(clear)
}

fn clock_recovery_loop(
    set_levels: &mut impl FnMut(TrainSet) -> Result<(), GmaError>,
    status: &mut impl FnMut() -> Result<LinkStatus, GmaError>,
    config: DpLinkConfig,
    max_vs: VoltageSwing,
    mut train_set: TrainSet,
) -> Result<TrainSet, GmaError> {
    let mut same_vs_tries = 0;
    let mut last_status = [0u8; 6];
    let mut tries = 0;
    while tries < CLOCK_RECOVERY_MAX_TRIES {
        delay_us(clock_recovery_delay_us(config));
        let st = status()?;
        last_status = st.raw_bytes();
        if st.clock_recovery_done(config.lane_count) {
            return Ok(train_set);
        }
        let next = adjusted_train_set_with_max(st, config.lane_count, max_vs)?;
        if next.voltage_swing == train_set.voltage_swing {
            if same_vs_tries >= CLOCK_RECOVERY_MAX_SAME_VS_TRIES {
                break;
            }
            same_vs_tries += 1;
        } else {
            if train_set.voltage_swing == max_vs {
                break;
            }
            same_vs_tries = 0;
        }
        train_set = next;
        set_levels(train_set)?;
        tries += 1;
    }
    fstart_log::warn!(
        "intel-gma: DP CR failed rate={:#x} lanes={} vs={} pe={} max_vs={} st={:#x},{:#x},{:#x} adj={:#x},{:#x}",
        config.link_rate.dpcd_code(),
        config.lane_count,
        train_set.voltage_swing as u8,
        train_set.pre_emphasis as u8,
        max_vs as u8,
        last_status[0],
        last_status[1],
        last_status[2],
        last_status[4],
        last_status[5]
    );
    Err(GmaError::HardwareError)
}

fn channel_equalization(
    set_pattern: &mut impl FnMut(TrainingPattern, TrainSet) -> Result<(), GmaError>,
    set_levels: &mut impl FnMut(TrainSet) -> Result<(), GmaError>,
    status: &mut impl FnMut() -> Result<LinkStatus, GmaError>,
    config: DpLinkConfig,
    max_vs: VoltageSwing,
    mut train_set: TrainSet,
) -> Result<(), GmaError> {
    let pattern = if config.tps3_supported {
        TrainingPattern::Pattern3
    } else {
        TrainingPattern::Pattern2
    };
    set_pattern(pattern, train_set)?;
    let mut last_status = [0u8; 6];
    let mut tries = 0;
    while tries < CHANNEL_EQ_MAX_TRIES {
        delay_us(channel_eq_delay_us(config));
        let st = status()?;
        last_status = st.raw_bytes();
        if st.channel_eq_done(config.lane_count) {
            return Ok(());
        }
        if !st.clock_recovery_done(config.lane_count) {
            break;
        }
        train_set = adjusted_train_set_with_max(st, config.lane_count, max_vs)?;
        set_levels(train_set)?;
        tries += 1;
    }
    fstart_log::warn!(
        "intel-gma: DP EQ failed rate={:#x} lanes={} pat={} vs={} pe={} st={:#x},{:#x},{:#x} adj={:#x},{:#x}",
        config.link_rate.dpcd_code(),
        config.lane_count,
        pattern.encode(),
        train_set.voltage_swing as u8,
        train_set.pre_emphasis as u8,
        last_status[0],
        last_status[1],
        last_status[2],
        last_status[4],
        last_status[5]
    );
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

/// libgfxinit `Minimum_Lane_Count`: the fewest lanes (1, 2 or 4) that carry
/// `mode` at `bpc` bits per colour on `rate`.
pub(crate) const fn minimum_lane_count(rate: DpLinkRate, mode: Mode, bpc: u8) -> Option<u8> {
    let symbol_rate_hz = match rate {
        DpLinkRate::Hbr => 270_000_000u64,
        DpLinkRate::Hbr2 => 540_000_000u64,
        _ => 162_000_000u64,
    };
    let lane_pixels_per_second = symbol_rate_hz * 8 / 3 / bpc as u64;
    let dotclock = mode.pixel_clock_khz as u64 * 1000;
    match dotclock.div_ceil(lane_pixels_per_second) {
        1 => Some(1),
        2 => Some(2),
        3 | 4 => Some(4),
        _ => None,
    }
}

const fn link_setting(
    caps: ReceiverCaps,
    rate: DpLinkRate,
    mode: Mode,
    bpc: u8,
) -> Option<DpLinkConfig> {
    match minimum_lane_count(rate, mode, bpc) {
        Some(lanes) if lanes <= caps.max_lane_count => Some(DpLinkConfig {
            link_rate: rate,
            lane_count: lanes,
            enhanced_framing: caps.enhanced_framing,
            tps3_supported: caps.tps3_supported,
            aux_rd_interval: caps.aux_rd_interval,
        }),
        _ => None,
    }
}

/// libgfxinit `DP_Info.Preferred_Link_Setting`: the sink's maximum rate with
/// the fewest lanes that carry the mode.
pub(crate) const fn preferred_link_setting(
    caps: ReceiverCaps,
    mode: Mode,
    bpc: u8,
) -> Option<DpLinkConfig> {
    link_setting(caps, caps.max_link_rate, mode, bpc)
}

/// libgfxinit `DP_Info.Next_Link_Setting`: the next lower rate, again with
/// the fewest lanes. `None` once the lowest rate failed.
pub(crate) const fn next_link_setting(
    caps: ReceiverCaps,
    current: DpLinkConfig,
    mode: Mode,
    bpc: u8,
) -> Option<DpLinkConfig> {
    let rate = match current.link_rate {
        DpLinkRate::Hbr2 => DpLinkRate::Hbr,
        DpLinkRate::Hbr => DpLinkRate::Rbr,
        _ => return None,
    };
    link_setting(caps, rate, mode, bpc)
}

const DPCD_EDP_REV: u32 = 0x00700;
const DPCD_SUPPORTED_LINK_RATES: u32 = 0x00010;
const DPCD_SINK_CONTROL: u32 = 0x00600;
const EDP_REV_1_4: u8 = 0x03;

fn read_caps(mmio: &Mmio, aux: AuxRegs) -> Result<ReceiverCaps, GmaError> {
    let mut bytes = [0u8; RECEIVER_CAPS_LEN];
    native_aux_read(mmio, aux, DPCD_RECEIVER_CAPS, &mut bytes)?;
    ReceiverCaps::parse(&bytes)
}

fn read_edp_rates(mmio: &Mmio, aux: AuxRegs, caps: ReceiverCaps) -> ReceiverCaps {
    let mut edp = [0u8; 3];
    if native_aux_read(mmio, aux, DPCD_EDP_REV, &mut edp).is_err() || edp[0] < EDP_REV_1_4 {
        return caps;
    }
    let mut table = [0u8; 16];
    if native_aux_read(mmio, aux, DPCD_SUPPORTED_LINK_RATES, &mut table).is_err() {
        return caps;
    }
    let mut max = 0u32;
    let mut index = 0;
    while index < 8 {
        let rate = u16::from_le_bytes([table[2 * index], table[2 * index + 1]]) as u32;
        if rate == 0 {
            break;
        }
        if rate > max {
            max = rate;
        }
        index += 1;
    }
    let khz = max * 20;
    let rate = if khz >= 540_000 {
        Some(DpLinkRate::Hbr2)
    } else if khz >= 270_000 {
        Some(DpLinkRate::Hbr)
    } else if khz >= 162_000 {
        Some(DpLinkRate::Rbr)
    } else {
        None
    };
    rate.map_or(caps, |max_link_rate| ReceiverCaps {
        max_link_rate,
        ..caps
    })
}

/// The DP part of libgfxinit `Connector_Info.Preferred_Link_Setting` after
/// panel power: read the caps and eDP rates, pick the preferred link and wake
/// a DPCD 1.1+ sink. Returns the caps for [`next_link_setting`].
pub(crate) fn prepare_sink(
    mmio: &Mmio,
    aux: AuxRegs,
    mode: Mode,
    bpc: u8,
) -> Result<(ReceiverCaps, DpLinkConfig), GmaError> {
    let caps = read_edp_rates(mmio, aux, read_caps(mmio, aux)?);
    let link = preferred_link_setting(caps, mode, bpc).ok_or(GmaError::ModeUnavailable)?;
    if caps.dpcd_revision >= 0x11 {
        let _ = native_aux_write(mmio, aux, DPCD_SINK_CONTROL, &[1]);
    }
    Ok((caps, link))
}

/// A DP source port driven by [`train_dp`] (libgfxinit's `DP_Training`
/// generic parameters).
pub(crate) trait DpSource {
    /// `TPS3_Supported`: the source can send training pattern 3.
    const TPS3_SUPPORTED: bool = false;
    /// `Max_V_Swing`.
    fn max_voltage_swing(&self) -> VoltageSwing;
    /// `Max_Pre_Emph` for the swing level of a train set.
    fn max_pre_emphasis(&self, swing: VoltageSwing) -> PreEmphasis;
    /// `Set_Pattern`.
    fn set_pattern(&mut self, pattern: TrainingPattern);
    /// `Set_Signal_Levels`.
    fn set_signal_levels(&mut self, train_set: TrainSet);
    /// `Off`, called when training failed.
    fn off(&mut self);
}

fn source_training_set<S: DpSource>(source: &S, train_set: TrainSet) -> u8 {
    let mut value = (train_set.voltage_swing as u8) | ((train_set.pre_emphasis as u8) << 3);
    if train_set.voltage_swing == source.max_voltage_swing() {
        value |= 0x04;
    }
    if train_set.pre_emphasis == source.max_pre_emphasis(train_set.voltage_swing) {
        value |= 0x20;
    }
    value
}

fn source_sink_init(mmio: &Mmio, aux: AuxRegs, link: DpLinkConfig) -> Result<(), GmaError> {
    let lanes = link.lane_count | if link.enhanced_framing { 0x80 } else { 0 };
    native_aux_write(
        mmio,
        aux,
        DPCD_LINK_BW_SET,
        &[link.link_rate.dpcd_code(), lanes],
    )?;
    native_aux_write(mmio, aux, DPCD_DOWNSPREAD_CTRL, &[0x00, 0x01])
}

fn source_sink_set_training_pattern<S: DpSource>(
    mmio: &Mmio,
    aux: AuxRegs,
    source: &S,
    link: DpLinkConfig,
    pattern: TrainingPattern,
    train_set: TrainSet,
) -> Result<(), GmaError> {
    let mut data = [0u8; 5];
    data[0] = pattern.encode();
    let mut len = 1;
    if pattern != TrainingPattern::None {
        let set = source_training_set(source, train_set);
        for byte in data.iter_mut().skip(1).take(usize::from(link.lane_count)) {
            *byte = set;
        }
        len += usize::from(link.lane_count);
    }
    native_aux_write(mmio, aux, DPCD_TRAINING_PATTERN_SET, &data[..len])
}

fn source_sink_set_signal_levels<S: DpSource>(
    mmio: &Mmio,
    aux: AuxRegs,
    source: &S,
    link: DpLinkConfig,
    train_set: TrainSet,
) -> Result<(), GmaError> {
    let data = [source_training_set(source, train_set); 4];
    native_aux_write(
        mmio,
        aux,
        DPCD_TRAINING_LANE0_SET,
        &data[..usize::from(link.lane_count)],
    )
}

struct SourceAdjustResult {
    success: bool,
    cr_done: bool,
    eq_done: bool,
}

fn source_sink_adjust_training<S: DpSource>(
    mmio: &Mmio,
    aux: AuxRegs,
    source: &S,
    link: DpLinkConfig,
    train_set: &mut TrainSet,
    cr_was_done: bool,
) -> SourceAdjustResult {
    let mut bytes = [0u8; LINK_STATUS_LEN];
    let status = native_aux_read(mmio, aux, DPCD_LINK_STATUS, &mut bytes)
        .and_then(|()| LinkStatus::parse(&bytes));
    let Ok(status) = status else {
        return SourceAdjustResult {
            success: false,
            cr_done: false,
            eq_done: false,
        };
    };
    let cr_done = status.clock_recovery_done(link.lane_count);
    let eq_done = status.channel_eq_done(link.lane_count);
    let success = cr_done || !cr_was_done;
    if success && !eq_done {
        let mut swing = VoltageSwing::Level0;
        let mut emphasis = PreEmphasis::Level0;
        for lane in 0..link.lane_count {
            let request = status.requested_train_set(lane);
            swing = swing.max(request.voltage_swing);
            emphasis = emphasis.max(request.pre_emphasis);
        }
        train_set.voltage_swing = swing.min(source.max_voltage_swing());
        train_set.pre_emphasis = emphasis.min(source.max_pre_emphasis(train_set.voltage_swing));
    }
    SourceAdjustResult {
        success,
        cr_done,
        eq_done,
    }
}

/// libgfxinit `DP_Training.Train_DP`.
pub(crate) fn train_dp<S: DpSource>(
    mmio: &Mmio,
    aux: AuxRegs,
    source: &mut S,
    link: DpLinkConfig,
) -> Result<(), GmaError> {
    let eq_pattern = if S::TPS3_SUPPORTED && link.tps3_supported {
        TrainingPattern::Pattern3
    } else {
        TrainingPattern::Pattern2
    };
    let mut train_set = TrainSet {
        voltage_swing: VoltageSwing::Level0,
        pre_emphasis: PreEmphasis::Level0,
    };
    source.set_pattern(TrainingPattern::Pattern1);
    source.set_signal_levels(train_set);
    let mut success = source_sink_init(mmio, aux, link).is_ok()
        && source_sink_set_training_pattern(
            mmio,
            aux,
            source,
            link,
            TrainingPattern::Pattern1,
            train_set,
        )
        .is_ok();

    let mut cr_done = false;
    let mut eq_done = false;
    if success {
        let mut retries = 0u8;
        for _ in 0..CLOCK_RECOVERY_MAX_TRIES {
            delay_us(clock_recovery_delay_us(link));
            let last = train_set;
            let adjust =
                source_sink_adjust_training(mmio, aux, source, link, &mut train_set, cr_done);
            (success, cr_done, eq_done) = (adjust.success, adjust.cr_done, adjust.eq_done);
            if cr_done || !success {
                break;
            }
            if train_set.voltage_swing == last.voltage_swing {
                if retries == CLOCK_RECOVERY_MAX_SAME_VS_TRIES {
                    break;
                }
                retries += 1;
            } else {
                if last.voltage_swing == source.max_voltage_swing() {
                    break;
                }
                retries = 0;
            }
            source.set_signal_levels(train_set);
            success = source_sink_set_signal_levels(mmio, aux, source, link, train_set).is_ok();
            if !success {
                break;
            }
        }
    }

    success = success && cr_done;
    if success {
        source.set_pattern(eq_pattern);
        success = source_sink_set_training_pattern(mmio, aux, source, link, eq_pattern, train_set)
            .is_ok();
    }
    if success {
        for _ in 0..CHANNEL_EQ_MAX_TRIES {
            delay_us(channel_eq_delay_us(link));
            let adjust =
                source_sink_adjust_training(mmio, aux, source, link, &mut train_set, cr_done);
            (success, cr_done, eq_done) = (adjust.success, adjust.cr_done, adjust.eq_done);
            if eq_done || !success {
                break;
            }
            source.set_signal_levels(train_set);
            success = source_sink_set_signal_levels(mmio, aux, source, link, train_set).is_ok();
            if !success {
                break;
            }
        }
    }

    success =
        source_sink_set_training_pattern(mmio, aux, source, link, TrainingPattern::None, train_set)
            .is_ok();
    source.set_pattern(TrainingPattern::None);
    if success && eq_done {
        Ok(())
    } else {
        source.off();
        Err(GmaError::HardwareError)
    }
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
        let config =
            select_link_config(caps(DpLinkRate::Hbr, 4), Mode::XGA_1024X768_60, true).unwrap();
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
        let with_source =
            link_config_candidates(caps(DpLinkRate::Hbr, 1), Mode::XGA_1024X768_60, true)
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

        let high_vs = LinkStatus::parse(&[0, 0, 0, 0, 0x33, 0]).unwrap();
        let clamped = adjusted_train_set_with_max(high_vs, 2, VoltageSwing::Level2).unwrap();
        assert_eq!(clamped.voltage_swing, VoltageSwing::Level2);
        assert_eq!(clamped.pre_emphasis, PreEmphasis::Level0);

        let full = adjusted_train_set_with_max(high_vs, 2, VoltageSwing::Level3).unwrap();
        assert_eq!(full.voltage_swing, VoltageSwing::Level3);
        assert_eq!(full.pre_emphasis, PreEmphasis::Level0);
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
}
