// SPDX-License-Identifier: MIT OR Apache-2.0
//! Cold DDR4 JEDEC initialization from native/training.c:meminit_jedec_init,
//! jedec_init_channel, jedec_default_tx_vref, and power_model.c's DIMM
//! impedance model. Not PHY training and not sufficient to load a DRAM stage.

use super::{
    Error,
    cpu::{Cpu, Package},
    registers::*,
    reut::{CadbCommand, Command, rank_mask},
    spd::Dimms,
    timings::Timings,
    training_state::CalibrationState,
};
use crate::sunrise_point::pmc::{MemoryBootMode, PmcRegisters};
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

register_bitfields![u16,
    Mr0 [
        CL_LOW OFFSET(2) NUMBITS(1) [],
        CL_HIGH OFFSET(4) NUMBITS(3) [],
        DLL_RESET OFFSET(8) NUMBITS(1) [],
        WR OFFSET(9) NUMBITS(3) []
    ],
    pub Mr1 [
        DIC OFFSET(1) NUMBITS(2) [],
        DLL_ENABLE OFFSET(0) NUMBITS(1) [],
        RTT_NOM OFFSET(8) NUMBITS(3) []
    ],
    Mr2 [
        CWL OFFSET(3) NUMBITS(3) [],
        ASR_SRT OFFSET(6) NUMBITS(2) [],
        RTT_WR OFFSET(9) NUMBITS(3) []
    ],
    Mr3 [ WRITE_COMMAND_LATENCY OFFSET(9) NUMBITS(2) [] ],
    Mr4 [ TEMPERATURE_CONTROLLED_REFRESH OFFSET(3) NUMBITS(1) [] ],
    Mr5 [
        CA_PARITY_LATENCY OFFSET(5) NUMBITS(1) [],
        RTT_PARK OFFSET(6) NUMBITS(3) []
    ],
    Mr6 [
        VREF_CODE OFFSET(0) NUMBITS(7) [],
        TRAINING_ENABLE OFFSET(7) NUMBITS(1) [],
        TCCD_L OFFSET(10) NUMBITS(3) []
    ]
];
const ORDER: [usize; 7] = [3, 6, 5, 4, 2, 1, 0];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardType {
    Mobile,
    Desktop,
    UltUlx,
    Server,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Termination {
    pub nominal: u8,
    pub write: u8,
    pub park: u8,
}

pub fn termination(
    ranks: [u8; 2],
    slot: usize,
    board: BoardType,
    mt_s: u32,
) -> Result<Termination, Error> {
    if slot > 1 || ranks.iter().any(|r| *r > 2) {
        return Err(Error::InvalidRank);
    }
    let (write, nominal, park) = if matches!(board, BoardType::Mobile | BoardType::UltUlx) {
        (1, 0, 0)
    } else if ranks.contains(&0) {
        (4, 0, 4)
    } else if ranks == [1, 1] {
        if mt_s >= 2667 { (4, 3, 3) } else { (2, 5, 0) }
    } else if ranks[0] == 1 {
        (2, if slot == 1 { 5 } else { 3 }, 4)
    } else if ranks[1] == 1 {
        (2, if slot == 1 { 3 } else { 5 }, 2)
    } else {
        (2, 1, 2)
    };
    Ok(Termination {
        nominal,
        write,
        park,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeRegisters {
    /// [channel][physical rank][mode-register number]. Only populated ranks
    /// contain images. MR0 retains DLL_RESET, as the source requires.
    pub rank: [[[u16; 7]; 4]; 2],
}
impl ModeRegisters {
    pub fn for_dimms(
        dimms: &Dimms<'_>,
        t: &Timings,
        cpu: Cpu,
        board: BoardType,
        vtt_odt: bool,
    ) -> Result<Self, Error> {
        const CAS: [u16; 16] = [9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 0, 17, 19, 21];
        let cl = CAS
            .iter()
            .position(|c| *c == t.cas && *c != 0)
            .ok_or(Error::InvalidTiming)? as u16;
        let wr = match t.twr {
            10 => 0,
            12 => 1,
            14 => 2,
            16 => 3,
            18 => 4,
            20 => 5,
            24 => 6,
            _ => return Err(Error::InvalidTiming),
        };
        let cwl = match t.cwl {
            9 => 0,
            10 => 1,
            11 => 2,
            12 => 3,
            14 => 4,
            16 => 5,
            18 => 6,
            _ => return Err(Error::InvalidTiming),
        };
        // Unlike native/training.c's default branch, CWL18 gets encoding 6,
        // matching src/device/dram/ddr4.c:ddr4_cwl_to_mr2_map().
        let mts = t.clock.data_rate_mts();
        let tccd = match mts {
            ..=1333 => 0,
            1334..=1867 => 1,
            1868..=2400 => 2,
            _ => 3,
        };
        let sensitive = dimms
            .iter()
            .flatten()
            .flatten()
            .any(|d| d.tcr_sensitive_hynix || d.tcr_sensitive_micron);
        let mr0 = (Mr0::CL_LOW.val(cl & 1)
            + Mr0::CL_HIGH.val((cl & 0xe) >> 1)
            + Mr0::DLL_RESET::SET
            + Mr0::WR.val(wr))
        .value;
        let mr3 = Mr3::WRITE_COMMAND_LATENCY
            .val(match mts {
                ..=1600 => 0,
                1601..=2400 => 1,
                _ => 2,
            })
            .value;
        let mr4 = Mr4::TEMPERATURE_CONTROLLED_REFRESH
            .val(!sensitive as u16)
            .value;
        let mr6 = Mr6::TCCD_L.val(tccd).value;
        let mut result = Self {
            rank: [[[0; 7]; 4]; 2],
        };
        for (ch, channel) in dimms.iter().enumerate() {
            let ranks = channel.map(|d| d.map_or(0, |d| d.ranks));
            for (slot, dimm) in channel.iter().enumerate() {
                let Some(dimm) = dimm else {
                    continue;
                };
                let odt = termination(
                    ranks,
                    slot,
                    board,
                    if t.clock.tck_fs <= 750000 { 2667 } else { mts },
                )?;
                let mr1 = (Mr1::DLL_ENABLE::SET + Mr1::RTT_NOM.val(odt.nominal.into())).value;
                let mr2 =
                    (Mr2::CWL.val(cwl) + Mr2::RTT_WR.val(odt.write.into()) + Mr2::ASR_SRT.val(3))
                        .value;
                let mr5 = (Mr5::CA_PARITY_LATENCY.val((cpu.package == Package::UltUlx) as u16)
                    + Mr5::RTT_PARK.val(if vtt_odt { 0 } else { odt.park.into() }))
                .value;
                for r in 0..usize::from(dimm.ranks) {
                    result.rank[ch][slot * 2 + r] = [mr0, mr1, mr2, mr3, mr4, mr5, mr6];
                }
            }
        }
        Ok(result)
    }
}

const RTT_NOM: [u32; 8] = [0xffff, 60, 120, 40, 240, 48, 80, 34];
const RTT_WR: [u32; 5] = [0, 120, 240, 0xffff, 80];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimmImpedance {
    WriteOdt,
    NominalOdt,
    ParkOdt,
    Ron,
}

/// `meminit_calc_dimm_impedance`, DDR4. Reserved encodings fail instead of
/// aliasing another impedance. None reads the saved image; Some is a candidate
/// raw JEDEC field, not an ohm value.
pub fn calc_dimm_impedance(
    mrs: &ModeRegisters,
    ch: usize,
    rank: usize,
    parameter: DimmImpedance,
    candidate: Option<u8>,
) -> Result<u16, Error> {
    let image = mrs
        .rank
        .get(ch)
        .and_then(|c| c.get(rank))
        .ok_or(Error::InvalidRank)?;
    let (value, table): (usize, &[u32]) = match parameter {
        DimmImpedance::WriteOdt => (
            usize::from(candidate.unwrap_or(((image[2] >> 9) & 7) as u8)),
            &RTT_WR,
        ),
        DimmImpedance::NominalOdt => (
            usize::from(candidate.unwrap_or(((image[1] >> 8) & 7) as u8)),
            &RTT_NOM,
        ),
        DimmImpedance::ParkOdt => (
            usize::from(candidate.unwrap_or(((image[5] >> 6) & 7) as u8)),
            &RTT_NOM,
        ),
        DimmImpedance::Ron => (
            usize::from(candidate.unwrap_or(((image[1] >> 1) & 3) as u8)),
            &[34, 48],
        ),
    };
    table
        .get(value)
        .map(|v| *v as u16)
        .ok_or(Error::InvalidPolicy)
}

fn parallel(a: u32, b: u32) -> u32 {
    ((u64::from(a) * u64::from(b) + u64::from(a + b) / 2) / u64::from(a + b)) as u32
}

/// `meminit_get_other_dimm`: other populated slot, otherwise this slot.
pub fn get_other_dimm(dimms: &Dimms<'_>, ch: usize, rank: usize) -> Result<usize, Error> {
    if rank >= 4 || rank_mask(dimms, ch)? & (1 << rank) == 0 {
        return Err(Error::InvalidRank);
    }
    let slot = rank / 2;
    Ok(if dimms[ch][slot ^ 1].is_some() {
        slot ^ 1
    } else {
        slot
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationKind {
    WriteTarget,
    ReadNonTarget,
}

/// Shared `meminit_eff_dimm_write_odt` model used by both JEDEC Vref and RSM.
/// The 15-ohm DIMM series resistor and finite 0xffff off representation are
/// source assumptions, including the source's finite parallel-off result.
pub fn effective_termination(
    dimms: &Dimms<'_>,
    mrs: &ModeRegisters,
    ch: usize,
    rank: usize,
    kind: TerminationKind,
) -> Result<u16, Error> {
    get_other_dimm(dimms, ch, rank)?;
    let slot = rank / 2;
    let other = slot ^ 1;
    let dimm = dimms[ch][slot].ok_or(Error::InvalidRank)?;
    let read = |r, p| calc_dimm_impedance(mrs, ch, r, p, None).map(u32::from);
    let write = read(rank, DimmImpedance::WriteOdt)?;
    let mut park = (read(rank, DimmImpedance::ParkOdt)? + 15).min(0xffff);
    if dimm.ranks == 1 {
        park = 0xffff;
    }
    let non_target = if let Some(other_dimm) = dimms[ch][other] {
        let other_park = (read(other * 2, DimmImpedance::ParkOdt)? + 15).min(0xffff);
        let other_nom = (read(other * 2, DimmImpedance::NominalOdt)? + 15).min(0xffff);
        if other_dimm.ranks == 2 {
            park = parallel(park, other_park);
        }
        parallel(park, other_nom)
    } else {
        park
    };
    let target = if write == 0 { 0xffff } else { write + 15 };
    Ok(match kind {
        TerminationKind::WriteTarget => parallel(target, non_target),
        TerminationKind::ReadNonTarget => non_target,
    } as u16)
}
fn effective_odt(
    dimms: &Dimms<'_>,
    mrs: &ModeRegisters,
    ch: usize,
    rank: usize,
) -> Result<(u32, u32), Error> {
    Ok((
        u32::from(calc_dimm_impedance(
            mrs,
            ch,
            rank,
            DimmImpedance::WriteOdt,
            None,
        )?),
        u32::from(effective_termination(
            dimms,
            mrs,
            ch,
            rank,
            TerminationKind::WriteTarget,
        )?),
    ))
}

/// DDR4 write-Vref offset encoding from training.c. Range 0 chooses either
/// JEDEC range automatically; ranges 1/2 constrain the allowed interval.
pub fn vref_code(offset: i32, range: u8) -> Result<u16, Error> {
    if range > 2 {
        return Err(Error::InvalidTiming);
    }
    let offset = offset.clamp(
        if range == 1 { -13 } else { -36 },
        if range == 2 { 14 } else { 36 },
    );
    let encoded = if range == 2 || (range == 0 && offset < -13) {
        offset - 28
    } else {
        offset + 13
    };
    Ok((encoded & 0x7f) as u16)
}
fn initial_vref(
    dimms: &Dimms<'_>,
    mrs: &ModeRegisters,
    ch: usize,
    rank: usize,
    vdd_mv: u16,
    cpu_ron: u16,
) -> Result<u16, Error> {
    if !(1..=2000).contains(&vdd_mv) || !(1..=255).contains(&cpu_ron) {
        return Err(Error::InvalidTiming);
    }
    let (write, effective) = effective_odt(dimms, mrs, ch, rank)?;
    let write = if write == 0 { 0xffff } else { write };
    let vdd = u32::from(vdd_mv);
    let ron = u32::from(cpu_ron);
    let midpoint = vdd * (2 * ron + effective) / (ron + effective) / 2;
    let tx_mv = (vdd * 15 + midpoint * write) / (15 + write);
    let delta = tx_mv as i32 * 1000 - 820000;
    let offset = (delta + if delta >= 0 { 3900 } else { -3900 }) / 7800;
    vref_code(offset, 0)
}

fn io_reset(registers: &MchRegisters, delay: &mut impl DelayNs, polls: u32) -> Result<(), Error> {
    super::reut::io_reset(registers, delay, polls, 100)
}
fn wait_compensation(
    registers: &MchRegisters,
    delay: &mut impl DelayNs,
    polls: u32,
) -> Result<(), Error> {
    for _ in 0..polls {
        if registers.rcomp_timer.is_set(Compensation::DONE) {
            return Ok(());
        }
        delay.delay_us(100);
    }
    Err(Error::CompensationTimeout)
}

// Shared by first initialization and destructive training reinitialization.
fn reset_cold(
    registers: &MchRegisters,
    pmc: &PmcRegisters,
    masks: [u8; 2],
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    io_reset(registers, delay, 100_000)?;
    wait_compensation(registers, delay, 100_000)?;
    registers.odt_multicast.write(OdtControl::OVERRIDE.val(15));
    pmc.assert_dram_reset(false);
    registers.cke_multicast.write(CkeControl::OVERRIDE.val(15));
    pmc.assert_dram_reset(true);
    delay.delay_ms(1);
    pmc.assert_dram_reset(false);
    delay.delay_ms(1);
    registers.init_state.modify(InitState::DCLK_ENABLE::SET);
    delay.delay_us(1);
    for (ch, mask) in masks.iter().enumerate() {
        registers.channel[ch]
            .cke
            .modify(CkeControl::ON.val((*mask).into()));
    }
    delay.delay_us(1);
    Ok(())
}

impl super::reut::Engine<'_> {
    /// training.c:meminit_jedec_init's already-initialized branch. Destructive
    /// cold-training reset only, NOT warm/S3 restore. `state.saved_mr` must
    /// come from successful initial JEDEC initialization. No host calibration
    /// values are recomputed, cleared, or replaced with electrical defaults.
    pub fn reinitialize_cold(
        &self,
        state: &super::training_state::CalibrationState,
        pmc: &PmcRegisters,
        mode: MemoryBootMode,
        sdram_lanes: usize,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let masks = super::training::cold_ranks(self, mode)?;
        if !(8..=9).contains(&sdram_lanes) {
            return Err(Error::InvalidPolicy);
        }
        // Validate PDA images before asserting RESET#.
        if state.ddr4_pda_enable {
            for ch in 0..2 {
                for rank in 0..4 {
                    if masks[ch] & (1 << rank) != 0
                        && state.ddr4_pda_mr6[ch][rank][..sdram_lanes]
                            .iter()
                            .any(|&code| code > 0x7f)
                    {
                        return Err(Error::InvalidTiming);
                    }
                }
            }
        }
        reset_cold(self.registers, pmc, masks, delay)?;
        let result = (|| {
            for ch in 0..2 {
                for rank in 0..4 {
                    if masks[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    for mr in ORDER {
                        self.training_mrs(
                            ch,
                            rank,
                            mr as u8,
                            state.saved_mr.rank[ch][rank][mr],
                            delay,
                        )?;
                    }
                }
            }
            for (ch, mask) in masks.into_iter().enumerate() {
                if mask != 0 {
                    CadbCommand::for_ranks(
                        self.dimms,
                        ch,
                        mask,
                        Command::ZqCalibration,
                        0,
                        [1 << 10; 2],
                        1,
                    )?
                    .issue(self.registers, ch, delay)?;
                }
            }
            delay.delay_us(2);
            for ch in 0..2 {
                for rank in 0..4 {
                    if masks[ch] & (1 << rank) == 0 {
                        continue;
                    }
                    self.mrs_mr6_vref(ch, rank, state.saved_mr.rank[ch][rank][6], delay)?;
                    if !state.ddr4_pda_enable {
                        continue;
                    }
                    let pda_result = (|| {
                        self.training_mrs(
                            ch,
                            rank,
                            5,
                            super::vref_training::park_rtt_wr(state, ch, rank),
                            delay,
                        )?;
                        for byte in 0..sdram_lanes {
                            let code = state.ddr4_pda_mr6[ch][rank][byte];
                            let mut device_result = Ok(());
                            for _ in 0..2 {
                                if device_result.is_ok() {
                                    device_result = self.mrs_pda_mr6(
                                        state,
                                        ch,
                                        rank,
                                        1 << byte,
                                        code,
                                        true,
                                        delay,
                                    );
                                    delay.delay_us(1);
                                }
                            }
                            let disable =
                                self.mrs_pda_mr6(state, ch, rank, 1 << byte, code, false, delay);
                            delay.delay_us(1);
                            device_result.and(disable)?;
                        }
                        Ok(())
                    })();
                    let restore =
                        self.training_mrs(ch, rank, 5, state.saved_mr.rank[ch][rank][5], delay);
                    pda_result.and(restore)?;
                }
            }
            Ok(())
        })();
        self.registers.odt_multicast.set(0);
        result
    }
}

/// Cold-only RESET#/CKE/MRS/ZQCL/default write-Vref sequence. The caller must
/// have completed CPU/PCH/SA pre-memory, PHY/timing/MAD setup and forced RCOMP.
/// On any error stop memory bring-up; no memory or training record is valid.
/// Retained boots are rejected *before* touching RESET#, CKE or MCHBAR.
/// Returned mode registers are initial state, not a trained-memory certificate.
#[allow(clippy::too_many_arguments)]
pub fn initialize_cold(
    registers: &MchRegisters,
    pmc: &PmcRegisters,
    dimms: &Dimms<'_>,
    t: &Timings,
    cpu: Cpu,
    board: BoardType,
    vtt_odt: bool,
    dram_odt: bool,
    vdd_mv: u16,
    cpu_ron: u16,
    boot_mode: MemoryBootMode,
    state: &mut CalibrationState,
    delay: &mut impl DelayNs,
) -> Result<ModeRegisters, Error> {
    if boot_mode != MemoryBootMode::Cold {
        return Err(Error::RetainedMemoryUnsupported);
    }
    let mut mrs = ModeRegisters::for_dimms(dimms, t, cpu, board, vtt_odt)?;
    let masks = [rank_mask(dimms, 0)?, rank_mask(dimms, 1)?];
    if masks == [0, 0] {
        return Err(Error::NoMemory);
    }
    let mut vrefs = [[0; 4]; 2];
    // Compute/validate every default before changing DRAM state.
    for (ch, channel_vrefs) in vrefs.iter_mut().enumerate() {
        for (rank, vref) in channel_vrefs.iter_mut().enumerate() {
            if masks[ch] & (1 << rank) != 0 {
                *vref = initial_vref(dimms, &mrs, ch, rank, vdd_mv, cpu_ron)?;
            }
        }
    }
    reset_cold(registers, pmc, masks, delay)?;
    for ch in 0..2 {
        if masks[ch] == 0 {
            continue;
        }
        for mr in ORDER {
            let values = [mrs.rank[ch][0][mr], mrs.rank[ch][2][mr]];
            CadbCommand::for_ranks(dimms, ch, masks[ch], Command::Mrs, mr as u8, values, 0)?
                .issue(registers, ch, delay)?;
        }
        for slot in 0..2 {
            if masks[ch] & (3 << (slot * 2)) != 0 {
                registers.channel[ch]
                    .mr2_shadow
                    .modify(Mr2Shadow::MR2.val(mrs.rank[ch][slot * 2][2].into()));
            }
        }
    }
    for (ch, mask) in masks.iter().copied().enumerate() {
        if mask != 0 {
            CadbCommand::for_ranks(dimms, ch, mask, Command::ZqCalibration, 0, [1 << 10; 2], 1)?
                .issue(registers, ch, delay)?;
        }
    }
    delay.delay_us(2);
    for ch in 0..2 {
        for rank in 0..4 {
            if masks[ch] & (1 << rank) == 0 {
                continue;
            }
            let mr6 = (mrs.rank[ch][rank][6] & !0xff) | vrefs[ch][rank];
            for iteration in 0..3 {
                let value = mr6
                    | if iteration < 2 {
                        Mr6::TRAINING_ENABLE::SET.value
                    } else {
                        0
                    };
                CadbCommand::for_ranks(dimms, ch, 1 << rank, Command::Mrs, 6, [value; 2], 0)?
                    .issue(registers, ch, delay)?;
                delay.delay_us(1);
            }
            mrs.rank[ch][rank][6] = mr6;
        }
    }
    registers.odt_multicast.set(0);
    if !dram_odt {
        for control in &registers.control_channel {
            control
                .ranks_used
                .modify(ControlRanksUsed::DIS_ODT_STATIC.val(3));
        }
    }
    // Publish ONLY after every actual RESET/MRS/ZQ/default-Vref transaction
    // succeeded. A ModeRegisters::for_dimms plan is not an initialized image.
    state.saved_mr = mrs;
    state
        .ddr4_pda_mr6
        .fill([[0; super::training_state::SDRAM_LANES]; 4]);
    for ch in 0..2 {
        for rank in 0..4 {
            if masks[ch] & (1 << rank) != 0 {
                let code = tock_registers::LocalRegisterCopy::<u16, Mr6::Register>::new(
                    mrs.rank[ch][rank][6],
                )
                .read(Mr6::VREF_CODE) as u8;
                state.ddr4_pda_mr6[ch][rank][..8].fill(code);
            }
        }
    }
    state.ddr4_pda_enable = false;
    // Source0 is automatic selection, not a guessed forced range1.
    state.ddr4_tx_vref_range.fill(0);
    Ok(mrs)
}

#[cfg(test)]
mod tests {
    use super::super::{
        spd::{
            Dimm,
            testing::{seal, sodimm},
        },
        timings,
    };
    use super::*;
    use tock_registers::LocalRegisterCopy;

    #[test]
    fn jedec_mrs_images_order_termination_and_default_vref() {
        let raw = sodimm();
        let d = Dimm::decode(&raw).unwrap();
        let dimms = [[Some(d), None], [Some(d), None]];
        let t = timings::decode(&dimms, 938000).unwrap();
        let cpu = Cpu::from_signature(0x906e9).unwrap();
        let m = ModeRegisters::for_dimms(&dimms, &t, cpu, BoardType::Desktop, false).unwrap();
        assert_eq!(ORDER, [3, 6, 5, 4, 2, 1, 0]);
        assert_eq!(m.rank[0][0], [0x730, 1, 0x8e0, 0x200, 0, 0x100, 0x800]);
        assert_eq!(effective_odt(&dimms, &m, 0, 0).unwrap(), (80, 95));
        assert_eq!(initial_vref(&dimms, &m, 0, 0, 1200, 26).unwrap(), 11);
        assert_eq!(m.rank[0][1], [0; 7]);
    }

    #[test]
    fn jedec_odt_tables_cover_all_desktop_rank_topologies() {
        for (ranks, slot, mts, expected) in [
            ([1, 0], 0, 2133, (0, 4, 4)),
            ([1, 1], 0, 2667, (3, 4, 3)),
            ([1, 1], 1, 2133, (5, 2, 0)),
            ([1, 2], 0, 2133, (3, 2, 4)),
            ([1, 2], 1, 2133, (5, 2, 4)),
            ([2, 1], 0, 2133, (5, 2, 2)),
            ([2, 1], 1, 2133, (3, 2, 2)),
            ([2, 2], 0, 2133, (1, 2, 2)),
        ] {
            let t = termination(ranks, slot, BoardType::Desktop, mts).unwrap();
            assert_eq!((t.nominal, t.write, t.park), expected);
        }
        assert_eq!(
            termination([1, 0], 0, BoardType::UltUlx, 2133).unwrap(),
            Termination {
                nominal: 0,
                write: 1,
                park: 0
            }
        );
    }

    #[test]
    fn jedec_cwl18_and_temperature_refresh_sensitivity() {
        let mut raw = sodimm();
        raw[18] = 6;
        raw[125] = 0;
        seal(&mut raw);
        let d = Dimm::decode(&raw).unwrap();
        let dimms = [[Some(d), None], [None, None]];
        let t = timings::decode(&dimms, 750000).unwrap();
        assert_eq!(t.cwl, 18);
        let m = ModeRegisters::for_dimms(
            &dimms,
            &t,
            Cpu::from_signature(0x906ea).unwrap(),
            BoardType::Desktop,
            false,
        )
        .unwrap();
        assert_eq!(
            LocalRegisterCopy::<u16, Mr2::Register>::new(m.rank[0][0][2]).read(Mr2::CWL),
            6
        );
        assert_eq!(m.rank[0][0][4], 0);
    }

    #[test]
    fn jedec_vref_ranges_and_clamps() {
        for (offset, range, code) in [
            (0, 0, 13),
            (-13, 0, 0),
            (-14, 0, 86),
            (-36, 0, 64),
            (36, 0, 49),
            (-100, 1, 0),
            (100, 2, 114),
        ] {
            assert_eq!(vref_code(offset, range).unwrap(), code);
        }
        assert_eq!(vref_code(0, 3), Err(Error::InvalidTiming));
    }
}
