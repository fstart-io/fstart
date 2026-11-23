// SPDX-License-Identifier: MIT OR Apache-2.0
//! Destructive native memory_check.c and post_training.c full-rank scrub.
//! These tests consume WDB ownership, not a usable-memory certificate. Eight
//! data lanes only; ECC must remain disabled until a separate supported flow.
use super::{
    Error,
    io_test::{Address, AddressSetup, StopOnError, encode_rate},
    opt_param::sweep::TestSnapshot,
    registers::{InitState, MadIntra, MrCommand, ReutControl, ReutSequence},
    reut::{Engine, io_reset},
    reut_registers::{CadbControl, ErrorControl, Subsequence, WdbControl, WdbMux},
    spd::Dimm,
    training::{channels, cold_ranks},
    training_state::CalibrationState,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use embedded_hal::delay::DelayNs;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// Independent readable state, never unreadable WDB/CADB contents.
pub struct MemoryCheckWorkspace {
    tests: [TestSnapshot; 2],
    seeds: [[u32; 11]; 2],
    pcit: [u32; 2],
    init: u32,
}
impl MemoryCheckWorkspace {
    pub const ZERO: Self = Self {
        tests: [TestSnapshot::ZERO; 2],
        seeds: [[0; 11]; 2],
        pcit: [0; 2],
        init: 0,
    };
    /// Initialize reserved scratch from the valid ROM image, with no large
    /// stack temporary and no fabricated controller/test completion.
    /// # Safety
    /// Aligned, exclusive writable storage for Self; no old drop state or
    /// overlap with the ROM image. This initializes STORAGE, not hardware.
    pub unsafe fn initialize_at(destination: *mut Self) {
        static IMAGE: MemoryCheckWorkspace = MemoryCheckWorkspace::ZERO;
        unsafe { core::ptr::copy_nonoverlapping(&IMAGE, destination, 1) };
    }
    fn save(&mut self, e: &Engine<'_>) -> Result<(), Error> {
        self.init = e.registers.init_state.get();
        for ch in 0..2 {
            self.tests[ch] = TestSnapshot::load(e, ch)?;
            let c = &e.registers.channel[ch];
            self.pcit[ch] = c.pcit.get();
            for i in 0..3 {
                self.seeds[ch][i] = c.wdb_read_seed[i].get();
                self.seeds[ch][i + 3] = c.wdb_write_seed[i].get();
                self.seeds[ch][i + 6] = c.cadb_seed[i].get();
            }
            self.seeds[ch][9] = c.wdb_lmn.get();
            self.seeds[ch][10] = c.cadb_lmn.get();
        }
        Ok(())
    }
    fn restore(&self, e: &Engine<'_>, s: &mut CalibrationState, delay: &mut impl DelayNs) {
        e.stop(delay);
        e.registers.init_state.set(self.init);
        for ch in 0..2 {
            self.tests[ch].restore(e, ch);
            let c = &e.registers.channel[ch];
            c.pcit.set(self.pcit[ch]);
            for i in 0..3 {
                c.wdb_read_seed[i].set(self.seeds[ch][i]);
                c.wdb_write_seed[i].set(self.seeds[ch][i + 3]);
                c.cadb_seed[i].set(self.seeds[ch][i + 6]);
            }
            c.wdb_lmn.set(self.seeds[ch][9]);
            c.cadb_lmn.set(self.seeds[ch][10]);
            // Pattern memories were consumed; do not restore runnable identity.
            e.registers.reut_sequence[ch][0].modify(ReutSequence::GLOBAL_CONTROL::CLEAR);
        }
        s.dq_pat = u8::MAX;
        s.dq_pat_lc = 0;
    }
}

/// Reject another owner's independent command engine as well as REUT/CADB.
pub(crate) fn stopped(e: &Engine<'_>) -> Result<(), Error> {
    if e.registers.reut_global_control.is_set(ReutControl::START)
        || e.registers.init_state.is_set(InitState::IO_RESET)
        || (0..2).any(|ch| {
            e.registers.reut_control[ch].is_set(ReutControl::START)
                || e.registers.channel[ch]
                    .cadb_control
                    .is_set(CadbControl::DESELECT)
                || e.registers.reut_sequence[ch][0].read(ReutSequence::INIT_MODE) == 2
                || e.registers.channel[ch].mr_command.is_set(MrCommand::BUSY)
        })
    {
        return Err(Error::InvalidReutConfig);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
    columns: u32,
    rows: u32,
    banks: u32,
}
impl Geometry {
    fn checked(d: &Dimm<'_>) -> Result<Self, Error> {
        if Dimm::decode(d.raw)? != *d {
            return Err(Error::UnsupportedDimm);
        }
        const DENSITY: [u32; 13] = [
            256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 12288, 24576, 3072, 6144, 18432,
        ];
        // REUT row address is 17 bits, not the source's truncated u16.
        if d.column_bits != 10
            || !(14..=17).contains(&d.row_bits)
            || !matches!(d.device_width, 8 | 16)
            || d.banks != 4
            || !matches!(d.bank_groups, 2 | 4)
            || !(1..=2).contains(&d.ranks)
        {
            return Err(Error::UnsupportedDimm);
        }
        let g = Self {
            columns: 1 << d.column_bits,
            rows: 1 << d.row_bits,
            banks: u32::from(d.banks) * u32::from(d.bank_groups),
        };
        let capacity = ((u64::from(g.columns) * u64::from(g.rows)) >> 20)
            * u64::from(g.banks)
            * u64::from(d.device_width);
        if DENSITY
            .get(d.density_index as usize)
            .copied()
            .map(u64::from)
            != Some(capacity)
            || u64::from(g.columns)
                * u64::from(g.rows)
                * u64::from(g.banks)
                * 8
                * u64::from(d.ranks)
                / (1 << 20)
                != u64::from(d.size_mib)
        {
            return Err(Error::UnsupportedDimm);
        }
        encode_rate(g.rows * (g.columns / 8), 31)?;
        Ok(g)
    }
    fn sweep(self) -> AddressSetup {
        AddressSetup {
            start: Address::default(),
            end: Address {
                rank: 0,
                bank: (self.banks - 1) as u8,
                row: self.rows - 1,
                column: (self.columns - 8) as u16,
            },
            order: [0; 4],
            increment_rate: [0, self.rows * (self.columns / 8), self.columns / 8, 0],
            increment: [0, 1, 1, 1],
        }
    }
    fn alias(self) -> AddressSetup {
        AddressSetup {
            start: Address {
                bank: 1,
                ..Address::default()
            },
            end: Address {
                bank: 1,
                row: self.rows - 1,
                column: (self.columns - 256) as u16,
                rank: 0,
            },
            order: [0; 4],
            increment_rate: [0, 0, self.columns / 256 - 1, 0],
            increment: [0, 0, 1024, 32],
        }
    }
}
#[derive(Clone, Copy)]
enum Check {
    Alias,
    Full,
    Zero,
}
impl Engine<'_> {
    fn check_preflight(&self, mode: MemoryBootMode, ecc: bool) -> Result<[u8; 2], Error> {
        let ranks = cold_ranks(self, mode)?;
        if ecc
            || self
                .registers
                .mad_intra
                .iter()
                .any(|r| r.read(MadIntra::ECC) != 0)
        {
            return Err(Error::EccRequired);
        }
        stopped(self)?;
        for d in self.dimms.iter().flatten().flatten() {
            Geometry::checked(d)?;
        }
        Ok(ranks)
    }
    /// Density/address contradictions reject before any destructive writes.
    pub fn meminit_alias_check(
        &self,
        s: &mut CalibrationState,
        mode: MemoryBootMode,
        ecc: bool,
        w: &mut MemoryCheckWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.memory_check(s, mode, ecc, Check::Alias, w, delay)
    }
    /// Four genuine full-rank passes: write/read and inverted write/read.
    pub fn meminit_memory_test(
        &self,
        s: &mut CalibrationState,
        mode: MemoryBootMode,
        ecc: bool,
        w: &mut MemoryCheckWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.memory_check(s, mode, ecc, Check::Full, w, delay)
    }
    /// Source clean-memory path. ECC/ninth-lane operation is unsupported.
    pub fn memory_scrub(
        &self,
        s: &mut CalibrationState,
        mode: MemoryBootMode,
        ecc: bool,
        clean_memory: bool,
        w: &mut MemoryCheckWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        self.check_preflight(mode, ecc)?;
        if !clean_memory {
            return Ok(());
        }
        self.memory_check(s, mode, ecc, Check::Zero, w, delay)
    }
    fn memory_check(
        &self,
        s: &mut CalibrationState,
        mode: MemoryBootMode,
        ecc: bool,
        kind: Check,
        w: &mut MemoryCheckWorkspace,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let ranks = self.check_preflight(mode, ecc)?;
        w.save(self)?;
        let result = (|| {
            // VA setup is not a WDB population operation.
            self.setup_wdb(channels(ranks))?;
            if matches!(kind, Check::Zero) {
                self.registers
                    .init_state
                    .modify(InitState::REFRESH_ENABLE::SET);
            }
            for rank in 0..4 {
                if matches!(kind, Check::Alias) && rank % 2 != 0 {
                    continue;
                }
                let mut active = 0;
                for ch in 0..2 {
                    let selected = ranks[ch] & (1 << rank);
                    if ranks[ch] == 0 {
                        self.registers.reut_sequence[ch][0]
                            .modify(ReutSequence::GLOBAL_CONTROL::CLEAR);
                        continue;
                    }
                    self.program_rank_map(ch, selected)?;
                    if selected == 0 {
                        continue;
                    }
                    active |= 1 << ch;
                    let g = Geometry::checked(
                        self.dimms[ch][rank / 2]
                            .as_ref()
                            .ok_or(Error::InvalidRank)?,
                    )?;
                    self.program_address(
                        ch,
                        &if matches!(kind, Check::Alias) {
                            g.alias()
                        } else {
                            g.sweep()
                        },
                    )?;
                    self.registers.reut_sequence[ch][0].write(
                        ReutSequence::GLOBAL_CONTROL::SET
                            + ReutSequence::INIT_MODE::Testing
                            + ReutSequence::SUBSEQ_END.val(u32::from(matches!(kind, Check::Alias))),
                    );
                    self.registers.reut_sequence[ch][1].set(2);
                    self.set_loop_count(
                        ch,
                        if matches!(kind, Check::Alias) {
                            1
                        } else {
                            g.banks * g.rows
                        },
                    )?;
                    self.setup_error_counters(ch)?;
                    self.set_error_masks(ch, 0, 0xff)?;
                    self.set_soe(ch, StopOnError::Never)?;
                    let c = &self.registers.channel[ch];
                    // Never inherit stopped diagnostic selection/inversion:
                    // complete traversal must compare every chunk/cacheline.
                    c.error_control.modify(
                        ErrorControl::NTH.val(1)
                            + ErrorControl::CHUNKS.val(255)
                            + ErrorControl::CACHELINES.val(255),
                    );
                    c.wdb_invert.set(0);
                    c.wdb_control.write(WdbControl::LINEAR::SET);
                    c.wdb_mux.write(
                        WdbMux::ECC_SOURCE::SET
                            + WdbMux::MUX0.val(2)
                            + WdbMux::MUX1.val(2)
                            + WdbMux::MUX2.val(2)
                            + WdbMux::LFSR.val(3),
                    );
                    if matches!(kind, Check::Zero) {
                        self.write_wdb_data(ch, 0, 0, delay)?;
                        c.wdb_mux.set(0);
                        self.save_and_set_pcit(ch)?;
                    }
                }
                if active == 0 {
                    continue;
                }
                for pass in 0..if matches!(kind, Check::Full) { 4 } else { 1 } {
                    for ch in 0..2 {
                        if active & (1 << ch) == 0 {
                            continue;
                        }
                        let g = Geometry::checked(
                            self.dimms[ch][rank / 2]
                                .as_ref()
                                .ok_or(Error::InvalidRank)?,
                        )?;
                        let count = if matches!(kind, Check::Alias) {
                            (g.rows / 1024 * (g.columns / 256)).ilog2()
                        } else {
                            (g.columns / 8).ilog2()
                        };
                        let sub = self.subsequences(ch)?;
                        sub[0].write(
                            Subsequence::COUNT.val(count)
                                + Subsequence::KIND.val(if pass % 2 == 0 { 1 } else { 0 })
                                + Subsequence::INVERT.val(u32::from(pass >= 2)),
                        );
                        if matches!(kind, Check::Alias) {
                            sub[1].write(
                                Subsequence::COUNT.val(count)
                                    + Subsequence::KIND::Read
                                    + Subsequence::RESET_BASE::SET,
                            );
                        }
                    }
                    io_reset(self.registers, delay, 100_000, 100)?;
                    self.start(active, true)?;
                    delay.delay_us(1);
                    let feedback = self.poll(active, 20_000_000, delay)?;
                    if feedback.error_channels != 0
                        || (0..2).any(|ch| {
                            active & (1 << ch) != 0
                                && self.read_errors(ch).is_ok_and(|e| e.failing_dq != 0)
                        })
                    {
                        return Err(Error::ReutError);
                    }
                }
            }
            Ok(())
        })();
        w.restore(self, s, delay);
        if result.is_ok() && matches!(kind, Check::Zero) {
            // Refresh enable is the scrub's persistent source configuration,
            // not a transient reset bit to erase after a successful clear.
            self.registers
                .init_state
                .modify(InitState::REFRESH_ENABLE::SET);
        }
        result
    }
}
