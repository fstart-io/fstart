// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native opt_param.c:1103–1352, DQ global/residual compensation.
use super::{
    Error,
    command_training::CommandContext,
    cpu::Package,
    opt_param::{OptHost, OptParameter},
    phy::force_rcomp,
    phy_registers::*,
    reut::Engine,
    training::cold_ranks,
    training_state::CalibrationState,
};
use embedded_hal::delay::DelayNs;
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{Readable, Writeable},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalComp {
    ReadOdt,
    WriteDrive,
    DqSlew,
}
fn signed(value: u32, width: u32) -> i16 {
    ((value << (32 - width)) as i32 >> (32 - width)) as i16
}
impl Engine<'_> {
    /// Exact global selector, with bounded real RCOMP and live feedback.
    /// The caller owns rollback of a multi-probe operation.
    pub fn update_comp_global_offset(
        &self,
        host: &mut OptHost,
        parameter: GlobalComp,
        offset: i16,
        update_host: bool,
        delay: &mut impl DelayNs,
    ) -> Result<(u8, u8), Error> {
        let mut c0 = LocalRegisterCopy::<u32, CompControl0::Register>::new(host.comp_control0);
        let mut c1 = LocalRegisterCopy::<u32, CompControl1::Register>::new(host.comp_control1);
        match parameter {
            GlobalComp::ReadOdt if (-16..=15).contains(&offset) => c0.modify(
                CompControl0::FIX_ODT_DOWN::CLEAR
                    + CompControl0::DQ_ODT_VREF.val(offset as u32 & 31),
            ),
            GlobalComp::WriteDrive if (-8..=7).contains(&offset) => {
                c0.modify(CompControl0::DQ_DRIVE_VREF.val(offset as u32 & 15))
            }
            GlobalComp::DqSlew if (0..=31).contains(&offset) => c1.modify(
                CompControl1::DQ_CELLS.val(offset as u32 & 15)
                    + CompControl1::DQ_CYCLE.val(offset as u32 >> 4),
            ),
            _ => return Err(Error::InvalidPolicy),
        }
        self.registers.comp_control0.set(c0.get());
        self.registers.comp_control1.set(c1.get());
        force_rcomp(self.registers, delay)?;
        let codes = match parameter {
            GlobalComp::ReadOdt => {
                let up = self.registers.data_comp1.read(DataComp1::ODT_UP);
                let down = self.registers.data_comp1.read(DataComp1::ODT_DOWN);
                c0.modify(
                    CompControl0::ODT_UP_DOWN.val(down.wrapping_sub(up) & 63)
                        + CompControl0::FIX_ODT_DOWN::SET,
                );
                self.registers.comp_control0.set(c0.get());
                (up, down)
            }
            GlobalComp::WriteDrive => (
                self.registers.data_comp0.read(DataComp0::DRIVE_UP),
                self.registers.data_comp0.read(DataComp0::DRIVE_DOWN),
            ),
            GlobalComp::DqSlew => {
                let v = self.registers.data_comp0.read(DataComp0::SLEW);
                (v, v)
            }
        };
        if update_host {
            host.comp_control0 = c0.get();
            host.comp_control1 = c1.get();
        }
        Ok((codes.0 as u8, codes.1 as u8))
    }

    fn optimize_comp_body(
        &self,
        s: &mut CalibrationState,
        host: &mut OptHost,
        ranks: [u8; 2],
        p: OptParameter,
        update_host: bool,
        delay: &mut impl DelayNs,
    ) -> Result<u8, Error> {
        let c0 = LocalRegisterCopy::<u32, CompControl0::Register>::new(host.comp_control0);
        let c1 = LocalRegisterCopy::<u32, CompControl1::Register>::new(host.comp_control1);
        let (global, current, down, vref, lo, hi, max, bits) = match p {
            OptParameter::WriteDrive => (
                GlobalComp::WriteDrive,
                self.registers.data_comp0.read(DataComp0::DRIVE_UP),
                self.registers.data_comp0.read(DataComp0::DRIVE_DOWN),
                signed(c0.read(CompControl0::DQ_DRIVE_VREF), 4),
                -8,
                7,
                63,
                6,
            ),
            OptParameter::ReadOdt => (
                GlobalComp::ReadOdt,
                self.registers.data_comp1.read(DataComp1::ODT_UP),
                self.registers.data_comp1.read(DataComp1::ODT_DOWN),
                signed(c0.read(CompControl0::DQ_ODT_VREF), 5),
                -16,
                15,
                63,
                5,
            ),
            OptParameter::Slew => (
                GlobalComp::DqSlew,
                self.registers.data_comp0.read(DataComp0::SLEW),
                self.registers.data_comp0.read(DataComp0::SLEW),
                c1.read(CompControl1::DQ_CELLS) as i16,
                4,
                15,
                31,
                5,
            ),
            _ => return Err(Error::InvalidPolicy),
        };
        let mut codes = [[[0i16; 2]; 9]; 2];
        let mut sum = 0i16;
        let mut count = 0i16;
        for ch in 0..2 {
            if ranks[ch] == 0 {
                continue;
            }
            for b in 0..host.lanes {
                let word =
                    LocalRegisterCopy::<u32, DataOffsetComp::Register>::new(host.comp[ch][b]);
                let (up, dn) = match p {
                    OptParameter::WriteDrive => (
                        word.read(DataOffsetComp::DRIVE_UP),
                        word.read(DataOffsetComp::DRIVE_DOWN),
                    ),
                    OptParameter::ReadOdt => (
                        word.read(DataOffsetComp::ODT_UP),
                        word.read(DataOffsetComp::ODT_DOWN),
                    ),
                    _ => (
                        word.read(DataOffsetComp::SLEW),
                        word.read(DataOffsetComp::SLEW),
                    ),
                };
                let up = signed(up, bits);
                sum += up;
                count += 1;
                codes[ch][b] = [current as i16 + up, down as i16 + signed(dn, bits)];
                if codes[ch][b].iter().any(|v| !(0..=max).contains(v)) {
                    return Err(Error::InvalidPolicy);
                }
            }
        }
        let avg = (sum + sum.signum() * count / 2) / count;
        if avg == 0 {
            return Ok(current as u8);
        }
        let cycle = if p == OptParameter::Slew {
            c1.read(CompControl1::DQ_CYCLE) as i16 * 16
        } else {
            0
        };
        let mut best = vref + cycle;
        let mut distance = avg.abs();
        let direction = avg.signum() * if p == OptParameter::Slew { -1 } else { 1 };
        // Finite source field range; never write the C loop's out-of-range terminal probe.
        for delta in 1..=32 {
            let probe = vref + direction * delta;
            if !(lo..=hi).contains(&probe) {
                break;
            }
            let (up, dn) =
                self.update_comp_global_offset(host, global, probe + cycle, false, delay)?;
            if !(3..=max - 3).contains(&i16::from(up)) || !(3..=max - 3).contains(&i16::from(dn)) {
                break;
            }
            let d = (current as i16 + avg - i16::from(up)).abs();
            if d >= avg.abs() {
                break;
            }
            if d < distance {
                distance = d;
                best = probe + cycle;
            }
            if d == 0 {
                break;
            }
        }
        let (up, dn) = self.update_comp_global_offset(host, global, best, update_host, delay)?;
        if !(3..=max - 3).contains(&i16::from(up)) || !(3..=max - 3).contains(&i16::from(dn)) {
            return Err(Error::InvalidPolicy);
        }
        let limit = 1i16 << (bits - 1);
        // Validate every residual before touching any byte; no clamping away a code.
        for ch in 0..2 {
            if ranks[ch] != 0 {
                for b in 0..host.lanes {
                    let v = [
                        codes[ch][b][0] - i16::from(up),
                        codes[ch][b][1] - i16::from(dn),
                    ];
                    if v.iter().any(|v| !(-limit..limit).contains(v)) {
                        return Err(Error::InvalidPolicy);
                    }
                }
            }
        }
        for ch in 0..2 {
            if ranks[ch] != 0 {
                for b in 0..host.lanes {
                    self.update_opt_param_offset(
                        s,
                        host,
                        Package::DesktopHalo,
                        ch,
                        ranks[ch],
                        b,
                        p,
                        codes[ch][b][0] - i16::from(up),
                        update_host,
                        delay,
                    )?;
                    let down_param = match p {
                        OptParameter::WriteDrive => Some(OptParameter::WriteDriveDown),
                        OptParameter::ReadOdt => Some(OptParameter::ReadOdtDown),
                        _ => None,
                    };
                    if let Some(p) = down_param {
                        // Compose the down field with the just-programmed up residual even without host publication.
                        let base = host.comp[ch][b];
                        host.comp[ch][b] = self.registers.data_lane[b][ch].offset_comp.get();
                        self.update_opt_param_offset(
                            s,
                            host,
                            Package::DesktopHalo,
                            ch,
                            ranks[ch],
                            b,
                            p,
                            codes[ch][b][1] - i16::from(dn),
                            update_host,
                            delay,
                        )?;
                        if !update_host {
                            host.comp[ch][b] = base;
                        }
                    }
                }
            }
        }
        Ok(up)
    }

    /// Native optimize_comp_offset, atomic with respect to hardware/host offsets.
    pub fn optimize_comp_offset(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        p: OptParameter,
        update_host: bool,
        delay: &mut impl DelayNs,
    ) -> Result<u8, Error> {
        self.comp_transaction(s, ctx, &[p], update_host, delay)
            .map(|v| v[0])
    }
    pub fn meminit_optimize_comp(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        delay: &mut impl DelayNs,
    ) -> Result<[u8; 3], Error> {
        self.comp_transaction(
            s,
            ctx,
            &[
                OptParameter::WriteDrive,
                OptParameter::ReadOdt,
                OptParameter::Slew,
            ],
            true,
            delay,
        )
    }
    fn comp_transaction(
        &self,
        s: &mut CalibrationState,
        ctx: &CommandContext<'_>,
        params: &[OptParameter],
        update_host: bool,
        delay: &mut impl DelayNs,
    ) -> Result<[u8; 3], Error> {
        let ranks = cold_ranks(self, ctx.mode)?;
        super::dq_si::stopped(self, s)?;
        if params.iter().any(|p| {
            !matches!(
                p,
                OptParameter::WriteDrive | OptParameter::ReadOdt | OptParameter::Slew
            )
        }) {
            return Err(Error::InvalidPolicy);
        }
        let original = self.opt_host_sync(s, ctx.policy.sdram_lanes)?;
        let host_offsets = core::array::from_fn::<_, 2, _>(|ch| s.training.ch[ch].data_comp_offset);
        let misc = self.registers.ddr_misc.get();
        let mut host = original;
        let result = (|| {
            let mut out = [0; 3];
            for (i, &p) in params.iter().enumerate() {
                out[i] = self.optimize_comp_body(s, &mut host, ranks, p, update_host, delay)?;
            }
            Ok(out)
        })();
        if result.is_err() {
            self.registers.comp_control0.set(original.comp_control0);
            self.registers.comp_control1.set(original.comp_control1);
            for ch in 0..2 {
                s.training.ch[ch].data_comp_offset = host_offsets[ch];
                for b in 0..9 {
                    self.registers.data_lane[b][ch]
                        .offset_comp
                        .set(original.comp[ch][b]);
                }
            }
            let cleanup = force_rcomp(self.registers, delay);
            self.registers.ddr_misc.set(misc);
            cleanup?;
        } else if update_host {
            // Even a zero-mean no-probe path must publish the actual hardware
            // image captured by opt_host_sync, not leave an unequal old host.
            for ch in 0..2 {
                if ranks[ch] != 0 {
                    s.training.ch[ch].data_comp_offset[..ctx.policy.sdram_lanes]
                        .copy_from_slice(&host.comp[ch][..ctx.policy.sdram_lanes]);
                }
            }
        }
        self.registers.ddr_misc.set(misc);
        result
    }
}
