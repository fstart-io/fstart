// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed DDR4 optimizer controls from native/opt_param.c. These controls are
//! distinct from BER margin parameters. No training dispatcher is implied.

use super::{
    Error,
    cpu::Package,
    jedec::{ModeRegisters, Mr1, get_other_dimm},
    phy_registers::*,
    power_model::TrainingMode,
    reut::{Engine, RankLane, rank_mask},
    timings::{Clock, FrequencyBin},
    training_state::CalibrationState,
};
use embedded_hal::delay::DelayNs;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
};

pub const MAX_OPT_POINTS: usize = 200;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OptParameter {
    WriteDrive,
    Slew,
    Tco,
    TxEq,
    ReadOdt,
    RxEq,
    RxBias,
    RxCb,
    DimmOdt,
    DimmWriteOdt,
    DimmNominalOdt,
    DimmNominalOdtNonTarget,
    DimmParkOdt,
    DimmParkOdtNonTarget,
    DimmParkSlot1,
    DimmOdtCombined,
    DimmOdtCombined1R1R,
    DimmRon,
    WriteDriveUp,
    WriteDriveDown,
    WriteDriveUpCoarse,
    WriteDriveDownCoarse,
    ReadOdtUp,
    ReadOdtDown,
    ControlDrive,
    ClockDrive,
    RxBiasCb,
    TxEqWriteDrive,
    CommandSlew,
    ControlSlew,
    ClockSlew,
    CommandDrive,
    CommandTxEq,
    Default,
}
impl OptParameter {
    /// Search selectors and dedicated command controls are not DQ writes.
    pub fn directly_programmable(self) -> bool {
        matches!(
            self,
            Self::WriteDrive
                | Self::Slew
                | Self::Tco
                | Self::TxEq
                | Self::ReadOdt
                | Self::RxEq
                | Self::RxBias
                | Self::RxCb
                | Self::DimmOdt
                | Self::DimmWriteOdt
                | Self::DimmNominalOdt
                | Self::DimmNominalOdtNonTarget
                | Self::DimmParkOdt
                | Self::DimmParkOdtNonTarget
                | Self::DimmRon
                | Self::WriteDriveUp
                | Self::WriteDriveDown
                | Self::WriteDriveUpCoarse
                | Self::WriteDriveDownCoarse
                | Self::ReadOdtUp
                | Self::ReadOdtDown
        )
    }
    pub fn channel_global(self) -> bool {
        matches!(
            self,
            Self::DimmOdt
                | Self::DimmWriteOdt
                | Self::DimmNominalOdt
                | Self::DimmNominalOdtNonTarget
                | Self::DimmParkOdt
                | Self::DimmParkOdtNonTarget
                | Self::DimmRon
        )
    }
}

/// `opt_param_limit_value`, DDR4. Missing source limits are errors.
pub fn opt_param_limits(
    p: OptParameter,
    clock: Clock,
    mode: TrainingMode,
) -> Result<[i8; 2], Error> {
    use OptParameter::*;
    if clock.tck_fs == 0 {
        return Err(Error::InvalidPolicy);
    }
    let (normal, slow, margin) = match p {
        ReadOdt => ([-15, 15], [-15, 15], [-5, 10]),
        RxBias => ([0, 10], [8, 15], [8, 15]),
        RxCb => ([0, 3], [0, 3], [0, 3]),
        RxEq => ([0, 8], [0, 8], [0, 8]),
        TxEq => ([5, 10], [4, 8], [6, 15]),
        WriteDrive => ([-13, 2], [-11, 0], [-9, 10]),
        WriteDriveDownCoarse | WriteDriveUpCoarse => ([-13, 2], [-11, 0], [-7, 7]),
        Slew => ([-15, 5], [-15, 5], [-15, 10]),
        CommandSlew => ([2, 8], [2, 8], [2, 8]),
        CommandDrive => ([-6, 7], [-6, 7], [-6, 7]),
        CommandTxEq => ([0, 1], [0, 1], [0, 1]),
        _ => return Err(Error::InvalidPolicy),
    };
    Ok(if clock.frequency_bin() < FrequencyBin::Mts1600 {
        slow
    } else if mode == TrainingMode::Margin {
        margin
    } else {
        normal
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridMode {
    ChessOdd,
    ChessEven,
    Full,
    Half,
    CustomSlew,
    Reversed1D,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub start: [i8; 2],
    pub lengths: [u16; 2],
    pub dimensions: usize,
    pub mode: GridMode,
}
impl Grid {
    pub(crate) fn validate_axes(&self) -> Result<(), Error> {
        if !(1..=2).contains(&self.dimensions)
            || self.lengths[0] == 0
            || (self.dimensions == 2 && self.lengths[1] == 0)
            || (self.dimensions == 2
                && matches!(
                    self.mode,
                    GridMode::Half | GridMode::CustomSlew | GridMode::Reversed1D
                ))
            || (self.dimensions == 1
                && matches!(self.mode, GridMode::ChessOdd | GridMode::ChessEven))
        {
            return Err(Error::InvalidPolicy);
        }
        for d in 0..self.dimensions {
            if i32::from(self.start[d]) + i32::from(self.lengths[d]) - 1 > i32::from(i8::MAX) {
                return Err(Error::InvalidPolicy);
            }
        }
        Ok(())
    }
    pub fn point_count(&self) -> Result<usize, Error> {
        self.validate_axes()?;
        let n = usize::from(self.lengths[0])
            * if self.dimensions == 2 {
                usize::from(self.lengths[1])
            } else {
                1
            };
        if n > MAX_OPT_POINTS {
            return Err(Error::InvalidPolicy);
        }
        Ok(n)
    }
    /// `get_params_xyz`: source index order, with skipped holes explicit.
    pub fn get_params_xyz(&self, index: usize) -> Result<([i8; 2], bool), Error> {
        if index >= self.point_count()? {
            return Err(Error::InvalidPolicy);
        }
        let x = index % usize::from(self.lengths[0]);
        let y = index / usize::from(self.lengths[0]);
        let skip = match self.mode {
            GridMode::Half => !index.is_multiple_of(2),
            GridMode::ChessOdd => (x + y).is_multiple_of(2),
            GridMode::ChessEven => !(x + y).is_multiple_of(2),
            _ => false,
        };
        let mut offsets = [0, 0];
        // Addition in i16 avoids intermediate i8 overflow for wide grids.
        offsets[0] = (i16::from(self.start[0]) + x as i16) as i8;
        if self.dimensions == 2 {
            offsets[1] = (i16::from(self.start[1]) + y as i16) as i8;
        }
        if self.mode == GridMode::CustomSlew && index == 0 {
            offsets[0] = -17;
        }
        if self.mode == GridMode::Reversed1D {
            offsets[0] =
                (i16::from(self.start[0]) + self.lengths[0] as i16 - 1 - index as i16) as i8;
        }
        Ok((offsets, skip))
    }
}

/// Caller-owned nonaccumulating host baselines. Hardware rollback must retain
/// original live words separately from independently different host mirrors.
#[derive(Debug, Clone, Copy)]
pub struct OptHost {
    pub comp: [[u32; 9]; 2],
    pub control1: [[u32; 9]; 2],
    pub control2: [[u32; 9]; 2],
    pub comp_control0: u32,
    pub comp_control1: u32,
    pub mr: ModeRegisters,
    pub lanes: usize,
}
/// Pure saved-image transformation shared by hardware programming and the
/// temporary BER/model baseline. Never mutates another rank's MR neighbours.
pub(crate) fn opt_mr_images(
    dimms: &super::spd::Dimms<'_>,
    mr: &ModeRegisters,
    ch: usize,
    ranks: u8,
    parameter: OptParameter,
    off: i16,
) -> Result<(u8, u8, [[u16; 7]; 4]), Error> {
    use OptParameter::*;
    let valid = rank_mask(dimms, ch)?;
    let mut selected = ranks & valid;
    if selected == 0 {
        return Err(Error::InvalidRank);
    }
    if matches!(parameter, DimmNominalOdtNonTarget | DimmParkOdtNonTarget) {
        selected =
            (3 << (2 * get_other_dimm(dimms, ch, selected.trailing_zeros() as usize)?)) & valid;
    }
    let mask = match parameter {
        DimmRon | DimmNominalOdt | DimmNominalOdtNonTarget => 1 << 1,
        DimmOdt => (1 << 1) | (1 << 2),
        DimmWriteOdt => 1 << 2,
        DimmParkOdt | DimmParkOdtNonTarget => 1 << 5,
        _ => return Err(Error::InvalidPolicy),
    };
    let mut images = mr.rank[ch];
    for rank in 0..4 {
        if selected & (1 << rank) == 0 {
            continue;
        }
        if parameter == DimmRon {
            let mut value = LocalRegisterCopy::<u16, Mr1::Register>::new(images[rank][1]);
            value.modify(Mr1::DIC.val(off.clamp(0, 1) as u16));
            images[rank][1] = value.get();
        } else {
            if mask & (1 << 2) != 0 {
                let code = if parameter == DimmOdt { off >> 4 } else { off }.clamp(0, 4) as u16;
                images[rank][2] = (images[rank][2] & !(7 << 9)) | (code << 9);
            }
            if mask & ((1 << 1) | (1 << 5)) != 0 {
                let (m, shift) = if mask & (1 << 5) != 0 { (5, 6) } else { (1, 8) };
                images[rank][m] =
                    (images[rank][m] & !(7 << shift)) | (((off & 15).min(7) as u16) << shift);
            }
        }
    }
    Ok((selected, mask, images))
}

impl Engine<'_> {
    /// `opt_host_sync`, without mutating CalibrationState before success.
    pub fn opt_host_sync(&self, state: &CalibrationState, lanes: usize) -> Result<OptHost, Error> {
        if !matches!(lanes, 8 | 9) {
            return Err(Error::InvalidPolicy);
        }
        Ok(OptHost {
            comp: core::array::from_fn(|ch| {
                core::array::from_fn(|b| self.registers.data_lane[b][ch].offset_comp.get())
            }),
            control1: core::array::from_fn(|ch| {
                core::array::from_fn(|b| self.registers.data_lane[b][ch].control1.get())
            }),
            control2: core::array::from_fn(|ch| {
                core::array::from_fn(|b| self.registers.data_lane[b][ch].control2.get())
            }),
            comp_control0: self.registers.comp_control0.get(),
            comp_control1: self.registers.comp_control1.get(),
            mr: state.saved_mr,
            lanes,
        })
    }

    /// `update_opt_param_offset`, all directly programmable source DDR4 DQ
    /// cases. Bounds/unsupported requests reject before writes. Commit only
    /// follows successful MR transactions; an outer sweep owns rollback.
    pub fn update_opt_param_offset(
        &self,
        state: &mut CalibrationState,
        host: &mut OptHost,
        package: Package,
        ch: usize,
        ranks: u8,
        byte: usize,
        parameter: OptParameter,
        off: i16,
        update_host: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        use OptParameter::*;
        if !parameter.directly_programmable()
            || !matches!(host.lanes, 8 | 9)
            || byte >= host.lanes
            || ranks == 0
            || ranks & !15 != 0
        {
            return Err(Error::InvalidPolicy);
        }
        let valid = rank_mask(self.dimms, ch)?;
        let mut selected = ranks & valid;
        if selected == 0 {
            return Err(Error::InvalidRank);
        }
        if matches!(parameter, DimmNominalOdtNonTarget | DimmParkOdtNonTarget) {
            let rank = selected.trailing_zeros() as usize;
            selected = (3 << (2 * get_other_dimm(self.dimms, ch, rank)?)) & valid;
        }
        if parameter.channel_global() {
            let (selected, mask, images) =
                opt_mr_images(self.dimms, &host.mr, ch, ranks, parameter, off)?;
            for rank in 0..4 {
                if selected & (1 << rank) == 0 {
                    continue;
                }
                // Source packed ODT writes WR before NOM.
                for mr in [2, 1, 5] {
                    if mask & (1 << mr) == 0 {
                        continue;
                    }
                    self.training_mrs(ch, rank, mr as u8, images[rank][mr], delay)?;
                }
            }
            if update_host {
                host.mr.rank[ch] = images;
                state.saved_mr.rank[ch] = images;
            }
            return Ok(());
        }
        let lane = &self.registers.data_lane[byte][ch];
        if matches!(parameter, TxEq | RxEq) {
            for rank in 0..4 {
                if selected & (1 << rank) == 0 {
                    continue;
                }
                let p = RankLane {
                    channel: ch,
                    rank,
                    lane: byte,
                };
                if parameter == TxEq {
                    let off = i32::from(off);
                    let temp = if package == Package::UltUlx {
                        off + (off - 1) / 2
                    } else {
                        off
                    };
                    self.set_tx_eq(p, temp.clamp(0, 15) as u8 | 0x30)?;
                } else {
                    let o = off.clamp(0, 16);
                    let value = if o == 0 {
                        0
                    } else {
                        (((o - 1) / 4) << 3) + (((o - 1) % 4) << 1) + 1
                    };
                    self.set_rx_eq(p, value as u8)?;
                }
            }
            return Ok(());
        }
        if parameter == RxCb {
            let mut ctl =
                LocalRegisterCopy::<u32, DataControl2::Register>::new(host.control2[ch][byte]);
            ctl.modify(DataControl2::CBEN.val(off.clamp(0, 7) as u32));
            lane.control2.set(ctl.get());
            if update_host {
                host.control2[ch][byte] = ctl.get();
                let sum: u32 = host.control2[ch][..host.lanes]
                    .iter()
                    .map(|v| {
                        LocalRegisterCopy::<u32, DataControl2::Register>::new(*v)
                            .read(DataControl2::CBEN)
                    })
                    .sum();
                self.registers
                    .comp_control2
                    .modify(CompControl2::CBEN.val(sum / host.lanes as u32));
            }
            return Ok(());
        }
        if parameter == RxBias {
            let mut ctl =
                LocalRegisterCopy::<u32, DataControl1::Register>::new(host.control1[ch][byte]);
            ctl.modify(DataControl1::RX_BIAS.val(off.clamp(0, 15) as u32));
            lane.control1.set(ctl.get());
            if update_host {
                host.control1[ch][byte] = ctl.get();
            }
            return Ok(());
        }
        let mut comp = LocalRegisterCopy::<u32, DataOffsetComp::Register>::new(host.comp[ch][byte]);
        let fine = off.clamp(-32, 31) as u32 & 63;
        let coarse = (4 * off.clamp(-8, 7)) as u32 & 63;
        let odt = off.clamp(-16, 15) as u32 & 31;
        match parameter {
            WriteDrive => comp
                .modify(DataOffsetComp::DRIVE_UP.val(fine) + DataOffsetComp::DRIVE_DOWN.val(fine)),
            WriteDriveUp => comp.modify(DataOffsetComp::DRIVE_UP.val(fine)),
            WriteDriveDown => comp.modify(DataOffsetComp::DRIVE_DOWN.val(fine)),
            WriteDriveUpCoarse => comp.modify(DataOffsetComp::DRIVE_UP.val(coarse)),
            WriteDriveDownCoarse => comp.modify(DataOffsetComp::DRIVE_DOWN.val(coarse)),
            ReadOdt => {
                comp.modify(DataOffsetComp::ODT_UP.val(odt) + DataOffsetComp::ODT_DOWN.val(odt))
            }
            ReadOdtUp => comp.modify(DataOffsetComp::ODT_UP.val(odt)),
            ReadOdtDown => comp.modify(DataOffsetComp::ODT_DOWN.val(odt)),
            Tco => comp.modify(DataOffsetComp::TCO.val(odt)),
            Slew => {
                let off = off.clamp(-17, 15);
                let mut ctl =
                    LocalRegisterCopy::<u32, DataControl2::Register>::new(host.control2[ch][byte]);
                ctl.modify(DataControl2::SLEW_BYPASS.val(u32::from(off == -17)));
                if off != -17 {
                    comp.modify(DataOffsetComp::SLEW.val(off as u32 & 31));
                }
                lane.control2.set(ctl.get());
                if update_host {
                    host.control2[ch][byte] = ctl.get();
                }
            }
            _ => return Err(Error::InvalidPolicy),
        }
        lane.offset_comp.set(comp.get());
        if update_host {
            host.comp[ch][byte] = comp.get();
            state.training.ch[ch].data_comp_offset[byte] = comp.get();
        }
        // DDRIO latch trigger is NOT the bounded PCU force_rcomp handshake.
        self.registers
            .ddr_misc
            .modify(DdrMisc::DATA_CLOCK_GATE_DISABLE::SET + DdrMisc::FORCE_COMP_UPDATE::SET);
        Ok(())
    }
}

#[path = "opt_sweep.rs"]
pub(crate) mod sweep;
pub use sweep::{OptResult, OptSweep, OptWorkspace};

#[cfg(test)]
#[path = "opt_param_tests.rs"]
pub(crate) mod tests;
