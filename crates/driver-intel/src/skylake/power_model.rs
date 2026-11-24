// SPDX-License-Identifier: MIT OR Apache-2.0
//! DDR4 electrical/RSM model and mutable margin policy from native/power_model.c.
//! Power is modeled, not measured; BER consumers supply measured margins.

use super::{
    Error,
    timings::{Clock, FrequencyBin},
    training_state::{CalibrationState, MarginLimit},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MarginKind {
    ReadTiming = 1,
    WriteTiming = 2,
    ReadVoltage = 4,
    WriteVoltage = 5,
    ReceiveEnableX = 11,
    CommandTiming = 12,
    CommandVoltage = 13,
    ReadFanOut2 = 16,
    WriteFanOut2 = 17,
    ReadFanOut3 = 32,
    WriteFanOut3 = 33,
}
impl TryFrom<u8> for MarginKind {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            1 => Ok(Self::ReadTiming),
            2 => Ok(Self::WriteTiming),
            4 => Ok(Self::ReadVoltage),
            5 => Ok(Self::WriteVoltage),
            11 => Ok(Self::ReceiveEnableX),
            12 => Ok(Self::CommandTiming),
            13 => Ok(Self::CommandVoltage),
            16 => Ok(Self::ReadFanOut2),
            17 => Ok(Self::WriteFanOut2),
            32 => Ok(Self::ReadFanOut3),
            33 => Ok(Self::WriteFanOut3),
            _ => Err(Error::InvalidPolicy),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Upm,
    Power,
    Retrain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainingMode {
    Power,
    Margin,
}

pub(crate) const INITIAL_LIMITS: [MarginLimit; 11] = [
    MarginLimit {
        param: 1,
        upm_limit: 240,
        pwr_limit: 320,
        retrain_limit: 90,
    },
    MarginLimit {
        param: 2,
        upm_limit: 240,
        pwr_limit: 320,
        retrain_limit: 90,
    },
    MarginLimit {
        param: 4,
        upm_limit: 400,
        pwr_limit: 520,
        retrain_limit: 160,
    },
    MarginLimit {
        param: 5,
        upm_limit: 400,
        pwr_limit: 520,
        retrain_limit: 170,
    },
    MarginLimit {
        param: 16,
        upm_limit: 240,
        pwr_limit: 480,
        retrain_limit: 0,
    },
    MarginLimit {
        param: 17,
        upm_limit: 240,
        pwr_limit: 480,
        retrain_limit: 0,
    },
    MarginLimit {
        param: 32,
        upm_limit: 192,
        pwr_limit: 384,
        retrain_limit: 0,
    },
    MarginLimit {
        param: 33,
        upm_limit: 192,
        pwr_limit: 384,
        retrain_limit: 0,
    },
    MarginLimit {
        param: 13,
        upm_limit: 1300,
        pwr_limit: 1300,
        retrain_limit: 0,
    },
    MarginLimit {
        param: 12,
        upm_limit: 1300,
        pwr_limit: 1300,
        retrain_limit: 0,
    },
    // Preserve source integer evaluation order: 134/4*10=330, not 335.
    MarginLimit {
        param: 11,
        upm_limit: 330,
        pwr_limit: 380,
        retrain_limit: 0,
    },
];

/// `meminit_init_margin_limits`: called at the source's initial-training point,
/// not before every sweep (which would erase retrain/optimizer adjustments).
pub fn initialize_margin_limits(state: &mut CalibrationState) {
    state.margin_limits = INITIAL_LIMITS;
}

/// Mutable working entry used by source retrain/optimization policy. Missing
/// initialization is an error, not an invented zero margin threshold.
pub fn working_margin_limit(
    state: &mut CalibrationState,
    parameter: MarginKind,
) -> Result<&mut MarginLimit, Error> {
    state
        .margin_limits
        .iter_mut()
        .find(|entry| entry.param == parameter as u8)
        .ok_or(Error::InvalidPolicy)
}

/// `meminit_get_upm_pwr_limit`, DDR4 path; units are source tenths of steps.
/// Account for the selected PLL speed, training objective and DDR4 WRV's
/// 7.8-mV step conversion, including its retrain limit.
pub fn margin_limit(
    state: &CalibrationState,
    parameter: MarginKind,
    kind: LimitKind,
    clock: Clock,
    mode: TrainingMode,
) -> Result<u16, Error> {
    if clock.tck_fs == 0 {
        return Err(Error::InvalidPolicy);
    }
    let entry = state
        .margin_limits
        .iter()
        .find(|entry| entry.param == parameter as u8)
        .ok_or(Error::InvalidPolicy)?;
    let mut value = u32::from(match kind {
        LimitKind::Upm => entry.upm_limit,
        LimitKind::Power => entry.pwr_limit,
        LimitKind::Retrain => entry.retrain_limit,
    });
    if kind != LimitKind::Retrain {
        if parameter == MarginKind::WriteVoltage && clock.frequency_bin() > FrequencyBin::Mts1867 {
            value += 160;
        }
        if kind == LimitKind::Power && mode == TrainingMode::Margin {
            value += 120;
        }
    }
    if parameter == MarginKind::WriteVoltage {
        value = value * 10 / 25;
    }
    u16::try_from(value).map_err(|_| Error::InvalidPolicy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skylake::timings::{ReferenceClock, select_clock};

    #[test]
    fn power_margin_limits_preserve_source_units_rounding_and_working_copy() {
        let clock = select_clock(1_071_429, ReferenceClock::Mhz133).unwrap();
        let mut state = CalibrationState::default();
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Upm,
                clock,
                TrainingMode::Power
            ),
            Err(Error::InvalidPolicy)
        );
        initialize_margin_limits(&mut state);
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Upm,
                clock,
                TrainingMode::Power
            ),
            Ok(160)
        );
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Retrain,
                clock,
                TrainingMode::Power
            ),
            Ok(68)
        );
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::ReceiveEnableX,
                LimitKind::Upm,
                clock,
                TrainingMode::Power
            ),
            Ok(330)
        );
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::ReceiveEnableX,
                LimitKind::Power,
                clock,
                TrainingMode::Power
            ),
            Ok(380)
        );
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::ReadFanOut3,
                LimitKind::Upm,
                clock,
                TrainingMode::Power
            ),
            Ok(192)
        );
        working_margin_limit(&mut state, MarginKind::WriteVoltage)
            .unwrap()
            .upm_limit = 450;
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Upm,
                clock,
                TrainingMode::Power
            ),
            Ok(180)
        );
        initialize_margin_limits(&mut state);
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Upm,
                clock,
                TrainingMode::Power
            ),
            Ok(160)
        );
    }

    #[test]
    fn power_margin_limits_apply_speed_objective_but_not_retrain_extras() {
        let low = select_clock(1_071_429, ReferenceClock::Mhz133).unwrap();
        let high = select_clock(938_000, ReferenceClock::Mhz133).unwrap();
        let mut state = CalibrationState::default();
        initialize_margin_limits(&mut state);
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Upm,
                high,
                TrainingMode::Power
            ),
            Ok(224)
        );
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Power,
                high,
                TrainingMode::Power
            ),
            Ok(272)
        );
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Power,
                high,
                TrainingMode::Margin
            ),
            Ok(320)
        );
        for clock in [low, high] {
            for mode in [TrainingMode::Power, TrainingMode::Margin] {
                assert_eq!(
                    margin_limit(
                        &state,
                        MarginKind::WriteVoltage,
                        LimitKind::Retrain,
                        clock,
                        mode
                    ),
                    Ok(68)
                );
            }
        }
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::CommandVoltage,
                LimitKind::Power,
                high,
                TrainingMode::Margin
            ),
            Ok(1420)
        );
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::ReadTiming,
                LimitKind::Power,
                high,
                TrainingMode::Margin
            ),
            Ok(440)
        );
    }

    #[test]
    fn power_margin_limits_intermediate_pll_rate_keeps_source_frequency_bin() {
        let clock = select_clock(1_000_000, ReferenceClock::Mhz100).unwrap();
        assert_eq!(clock.data_rate_mts(), 2000);
        assert_eq!(clock.frequency_bin(), FrequencyBin::Mts1867);
        let mut state = CalibrationState::default();
        initialize_margin_limits(&mut state);
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Upm,
                clock,
                TrainingMode::Power
            ),
            Ok(160)
        );
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Power,
                clock,
                TrainingMode::Power
            ),
            Ok(208)
        );
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Power,
                clock,
                TrainingMode::Margin
            ),
            Ok(256)
        );
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::WriteVoltage,
                LimitKind::Retrain,
                clock,
                TrainingMode::Margin
            ),
            Ok(68)
        );
    }

    #[test]
    fn power_margin_limits_unknown_missing_or_overflowing_policy_is_not_zero() {
        let clock = select_clock(938_000, ReferenceClock::Mhz133).unwrap();
        for code in [0, 3, 6, 10, 14, 255] {
            assert_eq!(MarginKind::try_from(code), Err(Error::InvalidPolicy));
        }
        let mut state = CalibrationState::default();
        assert!(working_margin_limit(&mut state, MarginKind::CommandTiming).is_err());
        initialize_margin_limits(&mut state);
        working_margin_limit(&mut state, MarginKind::CommandTiming)
            .unwrap()
            .pwr_limit = u16::MAX;
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::CommandTiming,
                LimitKind::Power,
                clock,
                TrainingMode::Margin
            ),
            Err(Error::InvalidPolicy)
        );
        let mut invalid = clock;
        invalid.tck_fs = 0;
        assert_eq!(
            margin_limit(
                &state,
                MarginKind::ReadTiming,
                LimitKind::Upm,
                invalid,
                TrainingMode::Power
            ),
            Err(Error::InvalidPolicy)
        );
    }
}

#[path = "power_model_tables.rs"]
mod tables;
use super::{
    cpu::Package,
    jedec::{
        DimmImpedance, ModeRegisters, TerminationKind, calc_dimm_impedance, effective_termination,
    },
    opt_param::OptParameter,
    phy::PhyPolicy,
    phy_registers::*,
    registers::MchRegisters,
    reut::{RankLane, rank_mask},
    spd::Dimms,
};
use tock_registers::interfaces::Readable;

#[derive(Debug, Clone, Copy)]
pub struct PowerCoefficient {
    pub intercept: i32,
    pub first_order: i32,
    pub second_order: i32,
    pub interactions: [i32; 6],
}
impl PowerCoefficient {
    const ZERO: Self = Self {
        intercept: 0,
        first_order: 0,
        second_order: 0,
        interactions: [0; 6],
    };
}
#[derive(Debug, Clone, Copy)]
pub struct PowerFormula {
    pub frequency: u16,
    pub termination: u16,
    pub intercept: u32,
    pub coefficients: [PowerCoefficient; 6],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerDirection {
    Read,
    Write,
}

/// `meminit_get_power_table`: exact match, else source's last eligible entry
/// (not nearest-distance interpolation). DDR4-only; RX lookup deliberately
/// uses 1333 even when the accepted clock is faster.
pub fn get_power_table(
    direction: PowerDirection,
    frequency: u16,
    termination: u16,
) -> &'static PowerFormula {
    let table = match direction {
        PowerDirection::Read => &tables::RX_FORMULAS[..],
        PowerDirection::Write => &tables::TX_FORMULAS[..],
    };
    let mut closest = &table[0];
    for formula in table {
        if formula.frequency == frequency && formula.termination == termination {
            return formula;
        }
        if formula.frequency <= frequency && formula.termination <= termination {
            closest = formula;
        }
    }
    closest
}

/// `meminit_calc_power_rsm`. Wide checked intermediates reject arithmetic
/// overflow instead of relying on C signed-overflow UB; final units retain
/// the source's integer division and coefficient scale (1e6).
pub fn calc_power_rsm(formula: &PowerFormula, vector: &[u16], scale: u8) -> Result<i32, Error> {
    if vector.is_empty() || vector.len() > 6 || scale == 0 {
        return Err(Error::InvalidPolicy);
    }
    let mut result = i128::from(formula.intercept);
    for (i, &x) in vector.iter().enumerate() {
        let c = &formula.coefficients[i];
        let x = i128::from(x);
        let dx = x - i128::from(c.intercept);
        let add = x * i128::from(c.first_order) + dx * dx * i128::from(c.second_order);
        result = result.checked_add(add).ok_or(Error::InvalidPolicy)?;
        for (j, &y) in vector[..i].iter().enumerate() {
            let interaction = dx * (i128::from(y) - i128::from(formula.coefficients[j].intercept));
            result = result
                .checked_add(
                    interaction
                        .checked_mul(i128::from(c.interactions[j]))
                        .ok_or(Error::InvalidPolicy)?,
                )
                .ok_or(Error::InvalidPolicy)?;
        }
    }
    i32::try_from(result / (1_000_000 / i128::from(scale))).map_err(|_| Error::InvalidPolicy)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompSelection {
    Up,
    Down,
    Combined,
}
/// `meminit_get_comp_code`; unsupported selectors reject, not a zero code.
pub fn get_comp_code(
    r: &MchRegisters,
    p: OptParameter,
    selection: CompSelection,
    vtt_odt: bool,
) -> Result<u8, Error> {
    use OptParameter::*;
    let (up, down) = match p {
        WriteDrive | TxEq => (
            r.data_comp0.read(DataComp0::DRIVE_UP),
            r.data_comp0.read(DataComp0::DRIVE_DOWN),
        ),
        WriteDriveUpCoarse => {
            let n = r.data_comp0.read(DataComp0::DRIVE_UP) / 4;
            (n, n)
        }
        WriteDriveDownCoarse => {
            let n = r.data_comp0.read(DataComp0::DRIVE_DOWN) / 4;
            (n, n)
        }
        CommandDrive => (
            r.command_comp.read(CommandComp::DRIVE_UP),
            r.command_comp.read(CommandComp::DRIVE_DOWN),
        ),
        CommandSlew => {
            let n = r.command_comp.read(CommandComp::SLEW);
            (n, n)
        }
        ControlDrive => (
            r.control_comp.read(CommandComp::DRIVE_UP),
            r.control_comp.read(CommandComp::DRIVE_DOWN),
        ),
        ClockDrive => (
            r.clock_comp.read(ClockComp::DRIVE_UP),
            r.clock_comp.read(ClockComp::DRIVE_DOWN),
        ),
        ReadOdt => (
            r.data_comp1.read(DataComp1::ODT_UP),
            r.data_comp1.read(DataComp1::ODT_DOWN),
        ),
        Slew => {
            let n = r.data_comp0.read(DataComp0::SLEW);
            (n, n)
        }
        Tco => {
            let n = r.data_comp0.read(DataComp0::TCO);
            (n, n)
        }
        _ => return Err(Error::InvalidPolicy),
    };
    Ok(if p == ReadOdt && vtt_odt {
        up
    } else {
        match selection {
            CompSelection::Up => up,
            CompSelection::Down => down,
            CompSelection::Combined => {
                if up > 32 {
                    up.max(down)
                } else {
                    up.min(down)
                }
            }
        }
    } as u8)
}
fn signed_field(value: u32, width: u32) -> i32 {
    ((value << (32 - width)) as i32) >> (32 - width)
}
fn rounded(n: i64, d: i64) -> Result<i64, Error> {
    if d == 0 {
        return Err(Error::InvalidPolicy);
    }
    Ok(if (n > 0) == (d > 0) {
        (n + d / 2) / d
    } else {
        (n - d / 2) / d
    })
}
#[derive(Debug, Clone, Copy)]
pub struct CompensationOverride {
    pub vref: i8,
    pub code: u8,
}
/// Explicit model resources: accepted PLL, resolved electrical policy, actual
/// population/saved MRs and live DDRIO. No implicit board defaults or callbacks.
pub struct PowerModel<'a, 'd> {
    pub registers: &'a MchRegisters,
    pub dimms: &'a Dimms<'d>,
    pub saved_mr: &'a ModeRegisters,
    pub clock: Clock,
    pub package: Package,
    pub phy: &'a PhyPolicy,
}
impl PowerModel<'_, '_> {
    fn validate(&self, point: RankLane) -> Result<(), Error> {
        if point.rank >= 4
            || point.lane >= 9
            || rank_mask(self.dimms, point.channel)? & (1 << point.rank) == 0
        {
            return Err(Error::InvalidRank);
        }
        if self.clock.tck_fs == 0
            || !(1..=2000).contains(&self.phy.vdd_mv)
            || !(1..=2000).contains(&self.phy.vccio_mv)
            || self.phy.rcomp_resistor.contains(&0)
        {
            return Err(Error::InvalidPolicy);
        }
        Ok(())
    }
    /// `meminit_calc_cpu_impedance`, signed digital fields and static legs.
    pub fn calc_cpu_impedance(
        &self,
        point: RankLane,
        p: OptParameter,
        offset: Option<i8>,
        comp: Option<CompensationOverride>,
    ) -> Result<u16, Error> {
        self.validate(point)?;
        let r = self.registers;
        let lane = &r.data_lane[point.lane][point.channel];
        let ctl = &r.comp_control0;
        let (code, vref) = if let Some(c) = comp {
            (i32::from(c.code), i32::from(c.vref))
        } else {
            let v = match p {
                OptParameter::ReadOdt => signed_field(ctl.read(CompControl0::DQ_ODT_VREF), 5),
                OptParameter::WriteDrive | OptParameter::TxEq => {
                    signed_field(ctl.read(CompControl0::DQ_DRIVE_VREF), 4)
                }
                _ => return Err(Error::InvalidPolicy),
            };
            (
                i32::from(get_comp_code(r, p, CompSelection::Up, self.phy.vtt_odt)?),
                v,
            )
        };
        let (legs, step, resistor, segments, static_on, digital) = match p {
            OptParameter::ReadOdt => (
                5,
                96,
                0,
                if self.phy.vtt_odt { 1 } else { 2 },
                i32::from(!ctl.is_set(CompControl0::DISABLE_STATIC_ODT)),
                offset.map(i32::from).unwrap_or_else(|| {
                    signed_field(lane.offset_comp.read(DataOffsetComp::ODT_UP), 5)
                }),
            ),
            OptParameter::WriteDrive | OptParameter::TxEq => {
                let eq = lane.tx_train[point.rank].read(TxTrain::EQ);
                let segments = if self.package == Package::UltUlx {
                    2
                } else {
                    3
                };
                let digital = offset.map(i32::from).unwrap_or_else(|| {
                    if p == OptParameter::WriteDrive {
                        signed_field(lane.offset_comp.read(DataOffsetComp::DRIVE_UP), 6)
                    } else if self.package == Package::UltUlx {
                        ((eq & 15) as i32 * 2 + 2) / 3
                    } else {
                        (eq & 15) as i32
                    }
                });
                (
                    ((eq >> 4) as i32 + 2) * segments,
                    64,
                    1,
                    segments,
                    1,
                    digital,
                )
            }
            _ => return Err(Error::InvalidPolicy),
        };
        if step / 2 + vref <= 0 || step / 2 - vref < 0 {
            return Err(Error::InvalidPolicy);
        }
        let global = i64::from(self.phy.rcomp_resistor[resistor]) * i64::from(step / 2 - vref)
            / i64::from(step / 2 + vref);
        let leg = global * i64::from(static_on * legs * 46 + code * 10);
        let impedance = if p == OptParameter::TxEq {
            if digital == 0 {
                return Ok(u16::MAX);
            }
            rounded(leg, i64::from(46 * digital))?
        } else {
            let divider = static_on * legs * 46 + 10 * (code + digital);
            if divider < 0 {
                return Err(Error::InvalidPolicy);
            }
            rounded(leg, i64::from(divider.max(1) * segments))?
        };
        u16::try_from(impedance).map_err(|_| Error::InvalidPolicy)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Power {
    pub read: u32,
    pub write: u32,
    pub total: u32,
}
#[derive(Debug, Clone, Copy)]
pub struct PowerCandidate {
    pub parameter: OptParameter,
    pub offset: i8,
    pub compensation: u8,
}

fn percent_power(value: u32, exponent: u8) -> Result<u32, Error> {
    let mut result = 100u32;
    for _ in 0..exponent {
        result = result.checked_mul(value).ok_or(Error::InvalidPolicy)?;
        if result != value {
            result /= 100;
        }
    }
    Ok(result)
}
impl PowerModel<'_, '_> {
    /// `meminit_calc_opt_power_byte`: genuine source RSM + CPU bias model.
    /// Accumulates scaled mW; 60/40 weighted total. The 1000/200 trace vector
    /// entries are source model assumptions, NOT measured M900 routing.
    pub fn calc_opt_power_byte(
        &self,
        power: &mut Power,
        point: RankLane,
        candidates: &[PowerCandidate],
        scale: u8,
    ) -> Result<(), Error> {
        self.validate(point)?;
        if scale == 0
            || candidates.len() > 2
            || candidates.iter().enumerate().any(|(i, c)| {
                !c.parameter.directly_programmable()
                    || candidates[..i].iter().any(|x| x.parameter == c.parameter)
            })
        {
            return Err(Error::InvalidPolicy);
        }
        let lane = &self.registers.data_lane[point.lane][point.channel];
        let find = |p| candidates.iter().find(|c| c.parameter == p);
        // Preserve the source raw, unsigned DQ-drive Vref in override paths.
        let raw_vref = self
            .registers
            .comp_control0
            .read(CompControl0::DQ_DRIVE_VREF) as i8;
        let impedance = |p| {
            let c = find(p);
            self.calc_cpu_impedance(
                point,
                p,
                c.map(|x| x.offset),
                c.map(|x| CompensationOverride {
                    vref: raw_vref,
                    code: x.compensation,
                }),
            )
        };
        let ron = impedance(OptParameter::WriteDrive)?.clamp(35, 65);
        let slew = find(OptParameter::Slew)
            .map(|x| i32::from(x.offset))
            .unwrap_or_else(|| signed_field(lane.offset_comp.read(DataOffsetComp::SLEW), 5));
        let cells = self.registers.comp_control1.read(CompControl1::DQ_CELLS) + 1;
        let stages = cells
            * if self.registers.comp_control1.is_set(CompControl1::DQ_CYCLE) {
                1
            } else {
                2
            };
        let qclk = self.clock.tck_fs / 2000;
        if qclk == 0 || qclk > u16::MAX as u32 {
            return Err(Error::InvalidPolicy);
        }
        let stage = rounded(i64::from(qclk), i64::from(stages))? as u32;
        let delay = (stage
            * percent_power(if slew < 0 { 95 } else { 105 }, slew.unsigned_abs() as u8)?
            / 100)
            .clamp(10, 100) as u16;
        // The C vector doubles a u16 and then clamps. Preserve that explicit
        // finite-width operation, including the off EQ 0xffff sentinel.
        let eq = impedance(OptParameter::TxEq)?
            .wrapping_mul(2)
            .clamp(100, 700);
        let non_target = effective_termination(
            self.dimms,
            self.saved_mr,
            point.channel,
            point.rank,
            TerminationKind::ReadNonTarget,
        )?;
        let tx = [delay, ron, eq, non_target.clamp(100, 500), 1000, 200];
        let rx = [
            impedance(OptParameter::ReadOdt)?.clamp(60, 500),
            non_target.clamp(60, 500),
            1000,
            200,
        ];
        let c = find(OptParameter::DimmWriteOdt).or_else(|| find(OptParameter::DimmOdt));
        let write_code = c.map(|x| {
            if x.parameter == OptParameter::DimmOdt {
                (x.offset as u8) >> 4
            } else {
                x.offset as u8
            }
        });
        let mut write_odt = calc_dimm_impedance(
            self.saved_mr,
            point.channel,
            point.rank,
            DimmImpedance::WriteOdt,
            write_code,
        )?;
        if write_odt == u16::MAX {
            write_odt = 0;
        } else if write_odt == 0 {
            // Packed ODT programs NOM from its low nibble, independently of
            // WR in the high nibble. Preserve the distinct native standalone
            // WR override convention, but do not reuse packed WR as NOM.
            let nominal_code = c.map(|x| {
                if x.parameter == OptParameter::DimmOdt {
                    x.offset as u8 & 15
                } else {
                    x.offset as u8
                }
            });
            write_odt = calc_dimm_impedance(
                self.saved_mr,
                point.channel,
                point.rank,
                DimmImpedance::NominalOdt,
                nominal_code,
            )?;
        }
        let dimm_ron = calc_dimm_impedance(
            self.saved_mr,
            point.channel,
            point.rank,
            DimmImpedance::Ron,
            find(OptParameter::DimmRon).map(|x| x.offset as u8),
        )?;
        let bias = find(OptParameter::RxBias)
            .map(|x| x.offset.clamp(0, 15) as u32)
            .unwrap_or_else(|| lane.control1.read(DataControl1::RX_BIAS));
        let (vdd_current, vccio_current) = if self.phy.vccio_mv < 900 {
            (1288, 900)
        } else {
            (1525, 1105)
        };
        let cpu = (u32::from(self.phy.vdd_mv) * vdd_current / 100
            + u32::from(self.phy.vccio_mv) * vccio_current / 100)
            * (bias + 1)
            * 125
            / 1125
            / 1000;
        let tx_formula = get_power_table(
            PowerDirection::Write,
            self.clock.frequency_bin() as u16,
            write_odt,
        );
        let rx_formula = get_power_table(PowerDirection::Read, 1333, dimm_ron);
        let wr = u32::try_from(calc_power_rsm(tx_formula, &tx, scale)?)
            .map_err(|_| Error::InvalidPolicy)?;
        let rd = u32::try_from(calc_power_rsm(rx_formula, &rx, scale)?)
            .map_err(|_| Error::InvalidPolicy)?
            + cpu * u32::from(scale);
        let read = power.read.checked_add(rd).ok_or(Error::InvalidPolicy)?;
        let write = power.write.checked_add(wr).ok_or(Error::InvalidPolicy)?;
        let total = (u64::from(read) * 60 + u64::from(write) * 40) / 100;
        *power = Power {
            read,
            write,
            total: u32::try_from(total).map_err(|_| Error::InvalidPolicy)?,
        };
        Ok(())
    }
}

/// `meminit_normalize_power_to_margins`; inverted power uses max power in
/// the denominator (not the range). Equal-power candidates have zero reward.
pub fn normalize_power_to_margins(
    rows: &mut [[u16; 200]; 5],
    points: usize,
    tests: usize,
) -> Result<(), Error> {
    if points == 0 || points > 200 || !(1..=4).contains(&tests) {
        return Err(Error::InvalidPolicy);
    }
    let average = rows[..tests]
        .iter()
        .map(|r| u32::from(*r[..points].iter().max().unwrap()))
        .sum::<u32>()
        / tests as u32;
    let power = &mut rows[tests][..points];
    let max = u32::from(*power.iter().max().unwrap());
    let min = u32::from(*power.iter().min().unwrap());
    for p in power {
        let x = if max == min {
            0
        } else {
            100 - 100 * (u32::from(*p) - min) / max
        };
        *p = (average * x / 100) as u16;
    }
    Ok(())
}

/// Source `meminit_calc_sys_power` outputs, in tenths of modeled mW.
/// Command power is explicitly unmodeled, not measured as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemPower {
    pub read: u16,
    pub write: u16,
    pub command: u16,
}
impl PowerModel<'_, '_> {
    /// Sum actual SDRAM byte lanes, then average populated physical ranks.
    /// Unlike native's empty-topology fallback, no ranks is an error.
    pub fn calc_sys_power(&self, lanes: usize) -> Result<SystemPower, Error> {
        if !matches!(lanes, 8 | 9) {
            return Err(Error::InvalidPolicy);
        }
        let mut read = 0u64;
        let mut write = 0u64;
        let mut count = 0;
        for channel in 0..2 {
            let ranks = rank_mask(self.dimms, channel)?;
            for rank in 0..4 {
                if ranks & (1 << rank) == 0 {
                    continue;
                }
                let mut power = Power::default();
                for lane in 0..lanes {
                    self.calc_opt_power_byte(
                        &mut power,
                        RankLane {
                            channel,
                            rank,
                            lane,
                        },
                        &[],
                        10,
                    )?;
                }
                read += u64::from(power.read);
                write += u64::from(power.write);
                count += 1;
            }
        }
        if count == 0 {
            return Err(Error::NoMemory);
        }
        Ok(SystemPower {
            read: u16::try_from(read / count).map_err(|_| Error::InvalidPolicy)?,
            write: u16::try_from(write / count).map_err(|_| Error::InvalidPolicy)?,
            command: 0,
        })
    }

    /// `meminit_power_saving_meter_update`: modeled RSM baseline/delta, NOT
    /// physical telemetry. State lifetime and dispatcher policy are caller-owned.
    pub fn power_saving_meter_update(
        &self,
        state: &mut super::training_state::OdtPowerSaving,
        lanes: usize,
    ) -> Result<SystemPower, Error> {
        let current = self.calc_sys_power(lanes)?;
        record_power_saving(self.registers, state, current);
        Ok(current)
    }
}
fn saving_fraction(base: u16, current: u16) -> u32 {
    if current >= base {
        0
    } else {
        // A full 256/256 saving cannot fit in eight bits. Saturate, don't wrap.
        (u32::from(base - current) * 256 / u32::from(base)).min(255)
    }
}
fn record_power_saving(
    r: &MchRegisters,
    state: &mut super::training_state::OdtPowerSaving,
    current: SystemPower,
) {
    use tock_registers::interfaces::ReadWriteable;
    if !state.base_flag {
        state.base_saving_rd = current.read;
        state.base_saving_wr = current.write;
        state.base_saving_cmd = 0;
        state.base_flag = true;
    } else {
        r.odt_power_saving.modify(
            OdtPowerSavingControl::READ.val(saving_fraction(state.base_saving_rd, current.read))
            + OdtPowerSavingControl::WRITE.val(saving_fraction(state.base_saving_wr, current.write))
            // Native DDR4 has no command model, even if stale caller state
            // contained a nonzero command baseline.
            + OdtPowerSavingControl::COMMAND.val(0),
        );
    }
    state.meminit_saving_rd = current.read;
    state.meminit_saving_wr = current.write;
    state.meminit_saving_cmd = 0;
}

#[cfg(test)]
#[path = "power_meter_tests.rs"]
mod meter_tests;
