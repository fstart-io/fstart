//! DisplayPort link setting and training.
//!
//! This ports libgfxinit's `DP_Info` link-setting rules (`Preferred_Link_Setting`,
//! `Next_Link_Setting`, `Calculate_M_N`), its connector preparation
//! (`Connector_Info.Preferred_Link_Setting`, LSPCON switching) and the generic
//! `DP_Training.Train_DP` state machine. Sources (GMCH DP, PCH DP, CPU eDP)
//! plug in through [`DpSource`].

use crate::gma::dp_aux::{
    AuxRegs, DPCD_DOWNSPREAD_CTRL, DPCD_LINK_BW_SET, DPCD_LINK_STATUS, DPCD_RECEIVER_CAPS,
    DPCD_TRAINING_LANE0_SET, DPCD_TRAINING_PATTERN_SET, DpLinkRate, LinkStatus, PreEmphasis,
    ReceiverCaps, TrainSet, TrainingPattern, VoltageSwing, i2c_read, i2c_read_byte, i2c_write_byte,
    native_aux_read, native_aux_write,
};
use crate::gma::error::GmaError;
use crate::gma::mmio::{Mmio, delay_us, now_us};
use crate::gma::mode::Mode;
use crate::gma::port::{
    GmchDpLinkConfig, PortRegisterOp, dp_enable_op, dp_idle_op, dp_off_op, dp_signal_levels_op,
    dp_training_pattern_op,
};
use crate::gma::types::{Pipe, Port};

const RECEIVER_CAPS_LEN: usize = 15;
const LINK_STATUS_LEN: usize = 6;
const CLOCK_RECOVERY_MAX_TRIES: u8 = 32;
/// `Max_Retry`: clock recovery gives up after this many repeats of one swing.
const CLOCK_RECOVERY_MAX_RETRY: u8 = 4;
const CHANNEL_EQ_MAX_TRIES: u8 = 6;
/// DPCD `SINK_COUNT`/power state register written to wake DPCD 1.1+ sinks.
const DPCD_SINK_CONTROL: u32 = 0x00600;
/// DPCD eDP revision register and the start of the eDP link-rate table.
const DPCD_EDP_REV: u32 = 0x00700;
const DPCD_SUPPORTED_LINK_RATES: u32 = 0x00010;
const EDP_REV_1_4: u8 = 3;

/// Selected source/sink DP link parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DpLinkConfig {
    /// Link rate to write to DPCD LINK_BW_SET.
    pub link_rate: DpLinkRate,
    /// Active lane count.
    pub lane_count: u8,
    /// Enable enhanced framing on source and sink.
    pub enhanced_framing: bool,
    /// Sink supports TP3 (the source decides whether to use it).
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

const fn symbol_rate_hz(rate: DpLinkRate) -> u64 {
    match rate {
        DpLinkRate::Hbr => 270_000_000,
        DpLinkRate::Hbr2 => 540_000_000,
        _ => 162_000_000,
    }
}

impl DpLinkConfig {
    /// Symbol clock in Hz: a tenth of the lane bit rate.
    pub(crate) const fn symbol_rate_hz(self) -> u64 {
        symbol_rate_hz(self.link_rate)
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
}

/// libgfxinit `Minimum_Lane_Count`: the fewest lanes (1, 2 or 4) that carry
/// `mode` at `bpc` bits per colour on `rate`.
pub(crate) const fn minimum_lane_count(rate: DpLinkRate, mode: Mode, bpc: u8) -> Option<u8> {
    let lane_pixels_per_second = symbol_rate_hz(rate) * 8 / 3 / bpc as u64;
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

/// libgfxinit `DP_Info.Read_Caps`.
pub(crate) fn read_caps(mmio: &Mmio, aux: AuxRegs) -> Result<ReceiverCaps, GmaError> {
    let mut bytes = [0u8; RECEIVER_CAPS_LEN];
    native_aux_read(mmio, aux, DPCD_RECEIVER_CAPS, &mut bytes)?;
    ReceiverCaps::parse(&bytes)
}

/// libgfxinit `DP_Info.Read_eDP_Rates`: eDP 1.4 sinks may list their rates
/// in a table instead of `MAX_LINK_RATE`. Failures keep `caps` unchanged.
pub(crate) fn read_edp_rates(mmio: &Mmio, aux: AuxRegs, caps: ReceiverCaps) -> ReceiverCaps {
    let mut edp = [0u8; 3];
    if native_aux_read(mmio, aux, DPCD_EDP_REV, &mut edp).is_err() || edp[0] < EDP_REV_1_4 {
        return caps;
    }
    let mut table = [0u8; 16];
    if native_aux_read(mmio, aux, DPCD_SUPPORTED_LINK_RATES, &mut table).is_err() {
        return caps;
    }
    edp_max_link_rate(&table).map_or(caps, |rate| ReceiverCaps {
        max_link_rate: rate,
        ..caps
    })
}

/// Highest rate of an eDP 1.4 `SUPPORTED_LINK_RATES` table (units of 200 kHz),
/// mapped down to the rates the source knows.
const fn edp_max_link_rate(table: &[u8; 16]) -> Option<DpLinkRate> {
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
    if khz >= 540_000 {
        Some(DpLinkRate::Hbr2)
    } else if khz >= 270_000 {
        Some(DpLinkRate::Hbr)
    } else if khz >= 162_000 {
        Some(DpLinkRate::Rbr)
    } else {
        None
    }
}

const DUAL_MODE_I2C_ADDR: u8 = 0x40;
const DUAL_MODE_ADAPTOR_ID: u8 = 0x10;
const LSPCON_MODE_CHANGE: u8 = 0x40;
const LSPCON_CURRENT_MODE: u8 = 0x41;
const LSPCON_MODE_PCON: u8 = 1;
const LSPCON_ADAPTOR_ID: u8 = 0xa8;
const DUAL_MODE_HDMI_ID: [u8; 16] = *b"DP-HDMI ADAPTOR\x04";
const LSPCON_RETRIES: usize = 6;
const LSPCON_MODE_TIMEOUT_US: u64 = 2_000_000;

fn lspcon_mode(mmio: &Mmio, aux: AuxRegs) -> Result<u8, GmaError> {
    let mut result = Err(GmaError::HardwareError);
    for _ in 0..LSPCON_RETRIES {
        result = i2c_read_byte(mmio, aux, DUAL_MODE_I2C_ADDR, LSPCON_CURRENT_MODE);
        if result.is_ok() {
            break;
        }
        delay_us(500);
    }
    result
}

fn set_lspcon_mode(mmio: &Mmio, aux: AuxRegs, mode: u8) -> Result<(), GmaError> {
    let mut result = Err(GmaError::HardwareError);
    for _ in 0..LSPCON_RETRIES {
        result = i2c_write_byte(mmio, aux, DUAL_MODE_I2C_ADDR, LSPCON_MODE_CHANGE, mode);
        if result.is_ok() {
            break;
        }
        delay_us(500);
    }
    result
}

/// libgfxinit `DP_Dual_Mode.Switch_LSPCON`: put a level shifter/protocol
/// converter behind a DP++ port into PCON (DP) mode.
pub(crate) fn switch_lspcon(mmio: &Mmio, aux: AuxRegs) {
    let mut id = [0u8; 16];
    if i2c_read(mmio, aux, DUAL_MODE_I2C_ADDR, &mut id).is_err()
        || id != DUAL_MODE_HDMI_ID
        || i2c_read_byte(mmio, aux, DUAL_MODE_I2C_ADDR, DUAL_MODE_ADAPTOR_ID)
            != Ok(LSPCON_ADAPTOR_ID)
    {
        return;
    }
    match lspcon_mode(mmio, aux) {
        Ok(LSPCON_MODE_PCON) | Err(_) => return,
        Ok(_) => {}
    }
    if set_lspcon_mode(mmio, aux, LSPCON_MODE_PCON).is_err() {
        return;
    }
    let deadline = now_us().saturating_add(LSPCON_MODE_TIMEOUT_US);
    loop {
        match lspcon_mode(mmio, aux) {
            Ok(LSPCON_MODE_PCON) | Err(_) => return,
            Ok(_) if now_us() >= deadline => return,
            Ok(_) => {}
        }
    }
}

/// The DP part of libgfxinit `Connector_Info.Preferred_Link_Setting` after
/// panel power: switch an LSPCON, read the caps and eDP rates, pick the
/// preferred link and wake a DPCD 1.1+ sink. Returns the caps for
/// [`next_link_setting`].
pub(crate) fn prepare_sink(
    mmio: &Mmio,
    aux: AuxRegs,
    mode: Mode,
    bpc: u8,
) -> Result<(ReceiverCaps, DpLinkConfig), GmaError> {
    switch_lspcon(mmio, aux);
    let caps = read_edp_rates(mmio, aux, read_caps(mmio, aux)?);
    let link = preferred_link_setting(caps, mode, bpc).ok_or(GmaError::ModeUnavailable)?;
    if caps.dpcd_revision >= 0x11 {
        // Set D0; libgfxinit ignores the result.
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

/// libgfxinit `Training_Set`: DPCD `TRAINING_LANEx_SET` with the max flags
/// set at the source's limits.
fn training_set<S: DpSource>(source: &S, train_set: TrainSet) -> u8 {
    let mut value = (train_set.voltage_swing as u8) | ((train_set.pre_emphasis as u8) << 3);
    if train_set.voltage_swing == source.max_voltage_swing() {
        value |= 0x04;
    }
    if train_set.pre_emphasis == source.max_pre_emphasis(train_set.voltage_swing) {
        value |= 0x20;
    }
    value
}

/// libgfxinit `Sink_Init`: LINK_BW_SET/LANE_COUNT_SET, then no downspread
/// and 8b/10b coding.
fn sink_init(mmio: &Mmio, aux: AuxRegs, link: DpLinkConfig) -> Result<(), GmaError> {
    let lanes = link.lane_count | if link.enhanced_framing { 0x80 } else { 0 };
    native_aux_write(
        mmio,
        aux,
        DPCD_LINK_BW_SET,
        &[link.link_rate.dpcd_code(), lanes],
    )?;
    native_aux_write(mmio, aux, DPCD_DOWNSPREAD_CTRL, &[0x00, 0x01])
}

/// libgfxinit `Sink_Set_Training_Pattern`: lane sets follow the pattern only
/// while training (TP1..TP3).
fn sink_set_training_pattern<S: DpSource>(
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
        let set = training_set(source, train_set);
        for byte in data.iter_mut().skip(1).take(usize::from(link.lane_count)) {
            *byte = set;
        }
        len += usize::from(link.lane_count);
    }
    native_aux_write(mmio, aux, DPCD_TRAINING_PATTERN_SET, &data[..len])
}

/// libgfxinit `Sink_Set_Signal_Levels`: `TRAINING_LANEx_SET` only.
fn sink_set_signal_levels<S: DpSource>(
    mmio: &Mmio,
    aux: AuxRegs,
    source: &S,
    link: DpLinkConfig,
    train_set: TrainSet,
) -> Result<(), GmaError> {
    let data = [training_set(source, train_set); 4];
    native_aux_write(
        mmio,
        aux,
        DPCD_TRAINING_LANE0_SET,
        &data[..usize::from(link.lane_count)],
    )
}

/// Status of one libgfxinit `Sink_Adjust_Training` step.
struct AdjustResult {
    success: bool,
    cr_done: bool,
    eq_done: bool,
}

/// libgfxinit `Sink_Adjust_Training`: read the link status and take the
/// highest requested levels, clamped to the source's limits.
fn sink_adjust_training<S: DpSource>(
    mmio: &Mmio,
    aux: AuxRegs,
    source: &S,
    link: DpLinkConfig,
    train_set: &mut TrainSet,
    cr_was_done: bool,
) -> AdjustResult {
    let mut bytes = [0u8; LINK_STATUS_LEN];
    let status = native_aux_read(mmio, aux, DPCD_LINK_STATUS, &mut bytes)
        .and_then(|()| LinkStatus::parse(&bytes));
    let Ok(status) = status else {
        return AdjustResult {
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
    AdjustResult {
        success,
        cr_done,
        eq_done,
    }
}

const fn clock_recovery_delay_us(link: DpLinkConfig) -> u32 {
    if matches!(link.link_rate, DpLinkRate::Hbr2) && link.aux_rd_interval != 0 {
        (link.aux_rd_interval as u32) * 4_000
    } else {
        100
    }
}

const fn channel_eq_delay_us(link: DpLinkConfig) -> u32 {
    if link.aux_rd_interval != 0 {
        (link.aux_rd_interval as u32) * 4_000
    } else {
        400
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
    let mut success = sink_init(mmio, aux, link).is_ok()
        && sink_set_training_pattern(
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
        let mut retries = 0;
        for _ in 0..CLOCK_RECOVERY_MAX_TRIES {
            delay_us(clock_recovery_delay_us(link));
            let last = train_set;
            let adjust = sink_adjust_training(mmio, aux, source, link, &mut train_set, cr_done);
            (success, cr_done, eq_done) = (adjust.success, adjust.cr_done, adjust.eq_done);
            if cr_done || !success {
                break;
            }
            if train_set.voltage_swing == last.voltage_swing {
                if retries == CLOCK_RECOVERY_MAX_RETRY {
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
            success = sink_set_signal_levels(mmio, aux, source, link, train_set).is_ok();
            if !success {
                break;
            }
        }
    }

    success = success && cr_done;
    if success {
        source.set_pattern(eq_pattern);
        success = sink_set_training_pattern(mmio, aux, source, link, eq_pattern, train_set).is_ok();
    }
    if success {
        for _ in 0..CHANNEL_EQ_MAX_TRIES {
            delay_us(channel_eq_delay_us(link));
            let adjust = sink_adjust_training(mmio, aux, source, link, &mut train_set, cr_done);
            (success, cr_done, eq_done) = (adjust.success, adjust.cr_done, adjust.eq_done);
            if eq_done || !success {
                break;
            }
            source.set_signal_levels(train_set);
            success = sink_set_signal_levels(mmio, aux, source, link, train_set).is_ok();
            if !success {
                break;
            }
        }
    }

    // libgfxinit overwrites the status with that of the final TP_None write.
    success = sink_set_training_pattern(mmio, aux, source, link, TrainingPattern::None, train_set)
        .is_ok();
    source.set_pattern(TrainingPattern::None);
    if success && eq_done {
        Ok(())
    } else {
        source.off();
        Err(GmaError::HardwareError)
    }
}

/// The GMCH DP source (libgfxinit `GMCH.DP`).
struct GmchDpSource<'a> {
    mmio: &'a Mmio,
    port: Port,
}

impl GmchDpSource<'_> {
    fn apply(&self, op: Result<PortRegisterOp, GmaError>) {
        if let Ok(op) = op {
            apply_port_op(self.mmio, op);
        }
    }
}

impl DpSource for GmchDpSource<'_> {
    fn max_voltage_swing(&self) -> VoltageSwing {
        VoltageSwing::Level3
    }

    fn max_pre_emphasis(&self, swing: VoltageSwing) -> PreEmphasis {
        match swing {
            VoltageSwing::Level0 => PreEmphasis::Level3,
            VoltageSwing::Level1 => PreEmphasis::Level2,
            VoltageSwing::Level2 => PreEmphasis::Level1,
            VoltageSwing::Level3 => PreEmphasis::Level0,
        }
    }

    fn set_pattern(&mut self, pattern: TrainingPattern) {
        self.apply(dp_training_pattern_op(self.port, pattern));
    }

    fn set_signal_levels(&mut self, train_set: TrainSet) {
        self.apply(dp_signal_levels_op(self.port, train_set));
    }

    fn off(&mut self) {
        for op in [dp_idle_op(self.port), dp_off_op(self.port)]
            .into_iter()
            .flatten()
        {
            apply_port_op(self.mmio, op);
            self.mmio.posting_read(port_op_register(op));
        }
    }
}

/// Train a GMCH DP link the way libgfxinit's `Enable_Output` does: the
/// preferred link setting first, each setting tried twice, then the next
/// lower rate.
///
/// `pre_try` receives the candidate link so generation code can allocate the
/// matching PLL and set up the pipe; `rollback` undoes it after a failure.
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
    let aux = AuxRegs::gmch_for_port(port, crate::gma::power::gmch_raw_clock_hz(mmio))?;
    // libgfxinit `Default_BPC`: 8 bpc for GMCH DP.
    let (caps, mut link) = prepare_sink(mmio, aux, mode, 8)?;
    let mut last_error = GmaError::ModeUnavailable;
    loop {
        for _ in 0..2 {
            // No PLL for this rate (5.4 Gbit/s on GMCH): try the next one.
            if let Err(err) = pre_try(link) {
                last_error = err;
                rollback();
                break;
            }
            apply_port_op(mmio, dp_enable_op(port, pipe, mode, link.gmch())?);
            let mut source = GmchDpSource { mmio, port };
            match train_dp(mmio, aux, &mut source, link) {
                Ok(()) => return Ok(link),
                Err(err) => last_error = err,
            }
            rollback();
        }
        link = next_link_setting(caps, link, mode, 8).ok_or(last_error)?;
    }
}

const fn port_op_register(op: PortRegisterOp) -> usize {
    match op {
        PortRegisterOp::Write { register, .. } | PortRegisterOp::Update { register, .. } => {
            register
        }
    }
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
    fn preferred_link_uses_the_maximum_rate_and_fewest_lanes() {
        let mode = Mode::XGA_1024X768_60;
        let link = preferred_link_setting(caps(DpLinkRate::Hbr2, 4), mode, 8).unwrap();
        assert_eq!((link.link_rate, link.lane_count), (DpLinkRate::Hbr2, 1));
        assert!(link.enhanced_framing);
        assert_eq!(link.aux_rd_interval, 2);
    }

    #[test]
    fn next_link_steps_down_the_rate_and_recomputes_lanes() {
        // 1920x1080@60: 148.5 MHz at 8 bpc.
        let mode = Mode {
            pixel_clock_khz: 148_500,
            ..Mode::XGA_1024X768_60
        };
        let caps = caps(DpLinkRate::Hbr2, 4);
        let first = preferred_link_setting(caps, mode, 8).unwrap();
        assert_eq!((first.link_rate, first.lane_count), (DpLinkRate::Hbr2, 1));
        let second = next_link_setting(caps, first, mode, 8).unwrap();
        assert_eq!((second.link_rate, second.lane_count), (DpLinkRate::Hbr, 2));
        let third = next_link_setting(caps, second, mode, 8).unwrap();
        assert_eq!((third.link_rate, third.lane_count), (DpLinkRate::Rbr, 4));
        assert_eq!(next_link_setting(caps, third, mode, 8), None);
    }

    #[test]
    fn lane_count_honours_bpc_and_sink_limits() {
        let mode = Mode {
            pixel_clock_khz: 148_500,
            ..Mode::XGA_1024X768_60
        };
        // 6 bpc on 1.62 Gbit/s needs 3 lanes, rounded to 4.
        assert_eq!(minimum_lane_count(DpLinkRate::Rbr, mode, 6), Some(4));
        assert_eq!(minimum_lane_count(DpLinkRate::Hbr, mode, 6), Some(2));
        assert_eq!(
            preferred_link_setting(caps(DpLinkRate::Rbr, 2), mode, 8),
            None
        );
        let huge = Mode {
            pixel_clock_khz: 600_000,
            ..mode
        };
        assert_eq!(minimum_lane_count(DpLinkRate::Rbr, huge, 8), None);
    }

    #[test]
    fn edp_rate_table_maps_to_known_rates() {
        let mut table = [0u8; 16];
        // 162000 kHz / 200 kHz = 8100, 270000 kHz -> 13500.
        table[..4].copy_from_slice(&[0xa4, 0x1f, 0xbc, 0x34]);
        assert_eq!(edp_max_link_rate(&table), Some(DpLinkRate::Hbr));
        assert_eq!(edp_max_link_rate(&[0; 16]), None);
    }

    #[test]
    fn m_n_matches_libgfxinit_for_fdi() {
        let link = DpLinkConfig {
            link_rate: DpLinkRate::Hbr,
            lane_count: 4,
            enhanced_framing: true,
            tps3_supported: false,
            aux_rd_interval: 0,
        };
        // 1366x768 panel at 76.3 MHz, 6 bpc.
        let mode = Mode {
            pixel_clock_khz: 76_300,
            ..Mode::XGA_1024X768_60
        };
        let mn = link.m_n(mode, 6);
        assert_eq!(mn.data_n, 0x80_0000);
        assert_eq!(mn.link_n, 0x10_0000);
        assert_eq!(
            u64::from(mn.data_m),
            3 * 6 * 76_300_000 * 0x80_0000 / (8 * 270_000_000 * 4)
        );
    }

    struct TestSource;

    impl DpSource for TestSource {
        fn max_voltage_swing(&self) -> VoltageSwing {
            VoltageSwing::Level2
        }
        fn max_pre_emphasis(&self, swing: VoltageSwing) -> PreEmphasis {
            match swing {
                VoltageSwing::Level0 => PreEmphasis::Level2,
                VoltageSwing::Level1 | VoltageSwing::Level2 => PreEmphasis::Level1,
                VoltageSwing::Level3 => PreEmphasis::Level0,
            }
        }
        fn set_pattern(&mut self, _: TrainingPattern) {}
        fn set_signal_levels(&mut self, _: TrainSet) {}
        fn off(&mut self) {}
    }

    #[test]
    fn training_set_flags_the_source_limits() {
        let source = TestSource;
        let set = |voltage_swing, pre_emphasis| TrainSet {
            voltage_swing,
            pre_emphasis,
        };
        assert_eq!(
            training_set(&source, set(VoltageSwing::Level0, PreEmphasis::Level0)),
            0x00
        );
        assert_eq!(
            training_set(&source, set(VoltageSwing::Level2, PreEmphasis::Level1)),
            0x2e
        );
        assert_eq!(
            training_set(&source, set(VoltageSwing::Level0, PreEmphasis::Level2)),
            0x30
        );
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
