//! Timing selection, controller programming, JEDEC init and the memory map
//! (coreboot `raminit.c`: `calculate_timings()` through
//! `program_total_memory_map()`).

use super::*;

impl Raminit<'_> {
    /// coreboot `collect_system_info()`.
    pub(super) fn collect_system_info(&mut self) {
        let nb = northbridge();
        let capid0: [u32; 3] = core::array::from_fn(|i| nb.read32(CAPID0 | ((i as u16) << 2)));
        for (index, value) in capid0.iter().enumerate() {
            fstart_log::info!("ironlake: CAPID0[{}] = {:#010x}", index, *value);
        }
        let info = &mut *self.info;
        info.revision = nb.read8(0x08);
        fstart_log::info!(
            "ironlake: revision {:#x}, device {:#x}",
            u32::from(info.revision),
            u32::from(nb.read16(0x02))
        );
        info.max_supported_clock_speed_index = (!capid0[1] & 7) as u8;
        info.uma_enabled = if (capid0[1] >> 11) & 1 != 0 {
            0
        } else {
            nb.read8(DEVEN) & 8
        };
        info.silicon_revision = 0;
        if capid0[2] & 2 != 0 {
            info.silicon_revision = 0;
            info.max_supported_clock_speed_index = 2;
            for channel in 0..NUM_CHANNELS {
                if info.populated_ranks[channel][0][0] != 0
                    && info.spd[channel][0][spd::MODULE_TYPE] & 0xf == 3
                {
                    info.silicon_revision = 2;
                    info.max_supported_clock_speed_index = 1;
                }
            }
        } else {
            match ((capid0[2] >> 18) & 1) + 2 * ((capid0[1] >> 3) & 1) {
                1 | 2 => info.silicon_revision = 3,
                3 => info.silicon_revision = 0,
                _ => info.silicon_revision = 2,
            }
            match nb.read16(0x02) {
                0x40 => info.silicon_revision = 0,
                0x48 => info.silicon_revision = 1,
                _ => {}
            }
        }
    }

    /// coreboot `calculate_timings()`: the common CAS latency and clock.
    pub(super) fn calculate_timings(&mut self) -> Result<(), ServiceError> {
        let info = &mut *self.info;
        let mut supported: u32 = 0x3fe;
        for channel in 0..NUM_CHANNELS {
            for slot in 0..NUM_SLOTS {
                if info.populated_ranks[channel][slot][0] != 0 {
                    let spd = &info.spd[channel][slot];
                    supported &= 2
                        * (u32::from(spd[spd::CAS_LATENCIES_LSB])
                            | (u32::from(spd[spd::CAS_LATENCIES_MSB]) << 8));
                }
            }
        }
        let max_clock_index = usize::from(info.max_supported_clock_speed_index.min(3));
        let mut cycletime = u32::from(MIN_CYCLETIME[max_clock_index]);
        let mut cas_latency_time = u32::from(MIN_CAS_LATENCY_TIME[max_clock_index]);
        for channel in 0..NUM_CHANNELS {
            for slot in 0..NUM_SLOTS {
                if info.populated_ranks[channel][slot][0] != 0 {
                    let spd = &info.spd[channel][slot];
                    let divisor = u32::from(spd[spd::TIMEBASE_DIVISOR]);
                    if divisor == 0 {
                        fstart_log::error!("ironlake: SPD timebase divisor is zero");
                        return Err(ServiceError::HardwareError);
                    }
                    let timebase = 1000 * u32::from(spd[spd::TIMEBASE_DIVIDEND]) / divisor;
                    cycletime = cycletime.max(timebase * u32::from(spd[spd::CYCLETIME]));
                    cas_latency_time =
                        cas_latency_time.max(timebase * u32::from(spd[spd::CAS_LATENCY_TIME]));
                }
            }
        }
        if cycletime > u32::from(MIN_CYCLETIME[0]) {
            fstart_log::error!(
                "ironlake: SPD DRAM frequency is slower than the controller minimum"
            );
            return Err(ServiceError::NotSupported);
        }
        let mut clock_speed_index = 0usize;
        while clock_speed_index < 3 {
            let min = u32::from(MIN_CYCLETIME[clock_speed_index]);
            if cycletime == min {
                break;
            }
            if cycletime > min {
                clock_speed_index -= 1;
                cycletime = u32::from(MIN_CYCLETIME[clock_speed_index]);
                break;
            }
            clock_speed_index += 1;
        }
        let min_cas_latency = cas_latency_time.div_ceil(cycletime);
        let mut cas_latency = 0u32;
        while supported != 0 {
            let highest = find_highest_bit_set(supported as u16);
            cas_latency = (highest + 3) as u32;
            if cas_latency <= min_cas_latency {
                break;
            }
            supported &= !(1 << highest);
        }
        if cas_latency != min_cas_latency && clock_speed_index != 0 {
            clock_speed_index -= 1;
        }
        if cas_latency * u32::from(MIN_CYCLETIME[clock_speed_index]) > 20000 {
            fstart_log::error!("ironlake: couldn't configure DRAM");
            return Err(ServiceError::NotSupported);
        }
        info.clock_speed_index = clock_speed_index as u16;
        info.cas_latency = cas_latency as u8;
        fstart_log::info!(
            "ironlake: DDR3-{} CL{}",
            (clock_speed_index as u32 + 3) * 2 * 400 / 3,
            cas_latency
        );
        Ok(())
    }

    /// coreboot `program_base_timings()`.
    pub(super) fn program_base_timings(&mut self) {
        let ext = self.info.extended_silicon_revision();
        let clock = usize::from(self.info.clock_speed_index);
        for channel in 0..NUM_CHANNELS {
            let mode = usize::from(self.info.mode4030[channel]);
            let max4048 = self.info.max4048[channel];
            for slot in 0..NUM_SLOTS {
                // coreboot bounds ranks by NUM_SLOTS here (both are 2).
                for rank in 0..NUM_SLOTS {
                    if !self.info.populated(channel, slot, rank) {
                        continue;
                    }
                    let spd = self.info.spd[channel][slot];
                    for lane in 0..9 {
                        let mut card_timing: u16 = 0;
                        if spd[spd::MODULE_TYPE] & 0xf == 3 {
                            match spd[spd::REFERENCE_RAW_CARD_USED] & 0x1f {
                                3 => card_timing = U16_FFD1188[0][lane][clock],
                                5 => card_timing = U16_FFD1188[1][lane][clock],
                                _ => {}
                            }
                        }
                        let timings = &mut self.info.training.lane_timings;
                        timings[0][channel][slot][rank][lane] = u16::from(U8_FFFD1218[clock]);
                        timings[1][channel][slot][rank][lane] = 256;
                        for tm in 2..4 {
                            timings[tm][channel][slot][rank][lane] =
                                u16::from(U8_FFFD1240[channel][ext][lane][2 * slot + rank][clock])
                                    .wrapping_add(max4048)
                                    .wrapping_add(u16::from(
                                        U8_FFFD0C78[channel][ext][mode][slot][rank][clock],
                                    ))
                                    .wrapping_add(card_timing);
                        }
                        for tm in 0..4 {
                            let value =
                                self.info.training.lane_timings[tm][channel][slot][rank][lane];
                            self.write_500(
                                channel,
                                u32::from(value),
                                get_timing_register_addr(lane, tm, slot, rank),
                                9,
                                0,
                            );
                        }
                    }
                    let mut card_timing_2: u32 = 0;
                    if ext == 4 && self.info.populated_ranks_mask[channel] & 5 != 5 {
                        match spd[spd::REFERENCE_RAW_CARD_USED] & 0x1f {
                            3 => card_timing_2 = u32::from(U16_FFFE0EB8[0][clock]),
                            5 => card_timing_2 = u32::from(U16_FFFE0EB8[1][clock]),
                            _ => {}
                        }
                    }
                    for i in 0..3 {
                        self.write_500(
                            channel,
                            card_timing_2
                                + u32::from(max4048)
                                + u32::from(U8_FFFD0EF8[channel][ext][mode][clock]),
                            U16_FFFD0C50[i][slot][rank],
                            8,
                            1,
                        );
                    }
                    self.write_500(
                        channel,
                        u32::from(max4048)
                            + u32::from(U8_FFFD0C78[channel][ext][mode][slot][rank][clock]),
                        U16_FFFD0C70[slot][rank],
                        7,
                        1,
                    );
                }
            }
            if self.info.populated_ranks_mask[channel] == 0 {
                continue;
            }
            for i in 0..3 {
                self.write_500(
                    channel,
                    u32::from(max4048)
                        + u32::from(self.info.avg4044[channel])
                        + u32::from(U8_FFFD17E0[channel][ext][mode][clock]),
                    U16_FFFD0C68[i],
                    8,
                    1,
                );
            }
        }
    }

    /// coreboot `compute_derived_timings()`.
    pub(super) fn compute_derived_timings(&mut self) {
        let ext = self.info.extended_silicon_revision();
        let info = &mut *self.info;
        let halfcycle = info.halfcycle_ps();
        let cycle = info.cycle_ps();
        let mut some_delay_3_ps_rounded = 0u32;
        if info.board_lane_delay[7] < 5 {
            info.board_lane_delay[7] = 5;
        }
        info.revision_flag_1 = 2;
        if info.silicon_revision == 2 || info.silicon_revision == 3 {
            info.revision_flag_1 = 0;
        }
        if info.revision < 16 {
            info.revision_flag_1 = 0;
        }
        if info.revision < 8 {
            info.revision_flag_1 = 0;
        }
        let early_silicon = info.silicon_revision == 0 || info.silicon_revision == 1;
        let mut some_delay_2_ps: u32 = if info.revision >= 8 && early_silicon {
            735
        } else {
            750
        };
        let some_delay_1_ps: u32 = if info.revision >= 0x10 && early_silicon {
            3929
        } else {
            3490
        };
        let mut floor = some_delay_1_ps / cycle;
        let mut ceil = some_delay_1_ps / cycle;
        if !some_delay_1_ps.is_multiple_of(cycle) {
            ceil += 1;
        } else {
            floor = floor.wrapping_sub(1);
        }
        info.some_delay_1_cycle_floor = floor as u8;
        if info.revision_flag_1 != 0 {
            some_delay_2_ps = halfcycle >> 6;
        }
        some_delay_2_ps += (some_delay_1_ps - 30).max(
            (2 * halfcycle)
                .wrapping_mul(ceil.wrapping_sub(1))
                .wrapping_add(1000),
        ) + 375;
        let some_delay_3_ps = halfcycle - some_delay_2_ps % halfcycle;
        if info.revision_flag_1 != 0 && some_delay_3_ps >= 150 {
            let halfcycles = (some_delay_3_ps << 6) / halfcycle;
            some_delay_3_ps_rounded = (halfcycle * halfcycles) >> 6;
        }
        let mut some_delay_2_halfcycles_ceil = some_delay_2_ps
            .div_ceil(halfcycle)
            .wrapping_sub(2 * ceil.wrapping_sub(1));
        if info.revision_flag_1 != 0 && some_delay_3_ps < 150 {
            some_delay_2_halfcycles_ceil = some_delay_2_halfcycles_ceil.wrapping_add(1);
        }
        info.some_delay_2_halfcycles_ceil = some_delay_2_halfcycles_ceil as u8;
        info.some_delay_3_ps_rounded = some_delay_3_ps_rounded as u8;
        info.max_slots_used_in_channel = if (info.populated(0, 0, 0) && info.populated(0, 1, 0))
            || (info.populated(1, 0, 0) && info.populated(1, 1, 0))
        {
            2
        } else {
            1
        };
        for channel in 0..NUM_CHANNELS as u32 {
            self.mch.write32(
                0x244 + (channel << 10),
                (if info.revision < 8 { 1 } else { 0x200 })
                    | ((2 - u32::from(info.max_slots_used_in_channel)) << 17)
                    | (channel << 21)
                    | (u32::from(info.some_delay_1_cycle_floor) << 18)
                    | 0x9510,
            );
        }
        if info.max_slots_used_in_channel == 1 {
            info.mode4030[0] = u8::from(info.count_ranks_in_channel(0) == 2);
            info.mode4030[1] = u8::from(info.count_ranks_in_channel(1) == 2);
        } else {
            for channel in 0..NUM_CHANNELS {
                let ranks = info.count_ranks_in_channel(channel);
                info.mode4030[channel] = if ranks == 1 || ranks == 2 { 2 } else { 3 };
            }
        }
        let clock = usize::from(info.clock_speed_index);
        for channel in 0..NUM_CHANNELS {
            if info.populated_ranks_mask[channel] == 0 {
                continue;
            }
            let mode = usize::from(info.mode4030[channel]);
            let mut max_of_unk: i32 = 0;
            let mut min_of_unk_2: i32 = 32767;
            let mut sum: i32 = 0;
            let mut count: i32 = 0;
            for i in 0..3 {
                let unk1: i32 = if info.revision < 8 {
                    i32::from(U8_FFFD1891[0][channel][clock][i])
                } else if !(info.revision >= 0x10 || info.revision_flag_1 != 0) {
                    i32::from(U8_FFFD1891[1][channel][clock][i])
                } else {
                    0
                };
                for slot in 0..NUM_SLOTS {
                    for rank in 0..NUM_RANKS {
                        if !info.populated(channel, slot, rank) {
                            continue;
                        }
                        let mut a: i32 = 0;
                        let mut b: i32 = 0;
                        if ext == 4 && info.populated_ranks_mask[channel] & 5 != 5 {
                            match info.spd[channel][slot][spd::REFERENCE_RAW_CARD_USED] & 0x1f {
                                3 => {
                                    a = i32::from(U16_FFD1178[0][clock]);
                                    b = i32::from(U16_FE0EB8[0][clock]);
                                }
                                5 => {
                                    a = i32::from(U16_FFD1178[1][clock]);
                                    b = i32::from(U16_FE0EB8[1][clock]);
                                }
                                _ => {}
                            }
                        }
                        min_of_unk_2 = min_of_unk_2.min(a).min(b);
                        if rank == 0 {
                            sum += a;
                            count += 1;
                        }
                        let t = b + i32::from(U8_FFFD0EF8[channel][ext][mode][clock]);
                        if unk1 >= t {
                            max_of_unk = max_of_unk.max(unk1 - t);
                        }
                    }
                }
                let t = i32::from(U8_FFFD17E0[channel][ext][mode][clock]) + min_of_unk_2;
                if unk1 >= t {
                    max_of_unk = max_of_unk.max(unk1 - t);
                }
            }
            if count == 0 {
                die("no memory ranks found for a channel");
            }
            info.avg4044[channel] = (sum / count) as u16;
            info.max4048[channel] = max_of_unk as u16;
        }
    }

    /// coreboot `jedec_read()`: a mode-register write through a read.
    pub(super) fn jedec_read(
        &self,
        channel: usize,
        slot: usize,
        rank: usize,
        total_rank: u32,
        mut addr3: u8,
        mut value: u32,
    ) {
        // Mirrored rank-1 address mapping.
        if rank & 1 != 0 && self.info.spd[channel][slot][spd::RANK1_ADDRESS_MAPPING] & 1 != 0 {
            addr3 = (addr3 & 0xcf) | ((addr3 & 0x10) << 1) | ((addr3 >> 1) & 0x10);
            value = (value & !0x1f8) | ((value >> 1) & 0xa8) | ((value & 0xa8) << 1);
        }
        self.mch.clrsetbits8(0x271, 0x1f << 1, addr3);
        self.mch.clrsetbits8(0x671, 0x1f << 1, addr3);
        read32p((value << 3) | (total_rank << 28));
        self.mch.clrsetbits8(0x271, 0x1f << 1, 1 << 1);
        self.mch.clrsetbits8(0x671, 0x1f << 1, 1 << 1);
        read32p(total_rank << 28);
    }

    /// coreboot `jedec_init()`: MR2, MR3, MR1, MR0 for every rank.
    pub(super) fn jedec_init(&self) {
        const MR1_RZQ12: u32 = 512;
        const MR1_RZQ2: u32 = 64;
        const MR1_RZQ4: u32 = 4;
        const MR1_ODS34OHM: u32 = 2;
        const MR0_BT_INTERLEAVED: u32 = 8;
        const MR0_DLL_RESET_ON: u32 = 256;
        const MR2_RTT_WR_DISABLED: u32 = 0;
        const MR2_RZQ2: u32 = 1 << 10;
        let info = &*self.info;
        let lane3 = u32::from(info.board_lane_delay[3]);
        let write_recovery = if lane3 <= 10 {
            if lane3 <= 8 { lane3 - 4 } else { 5 }
        } else {
            6
        };
        let mut auto_self_refresh = 1u32;
        let mut self_refresh_temperature = 1u32;
        for (channel, slot, _) in populated_ranks(info) {
            let thermal = u32::from(info.spd[channel][slot][spd::THERMAL_AND_REFRESH]);
            auto_self_refresh &= (thermal >> 2) & 1;
            self_refresh_temperature &= thermal & 1;
        }
        if auto_self_refresh == 1 {
            self_refresh_temperature = 0;
        }
        let late_silicon = info.silicon_revision == 2 || info.silicon_revision == 3;
        let dll_on = u32::from(
            !late_silicon
                || (info.populated(0, 0, 0) && info.populated(0, 1, 0))
                || (info.populated(1, 0, 0) && info.populated(1, 1, 0)),
        );
        let mut total_rank = 0;
        for channel in (0..NUM_CHANNELS).rev() {
            let mut rtt_wr = MR2_RTT_WR_DISABLED;
            let (rtt, rzq_reg58e) = if late_silicon {
                if info.clock_speed_index != 0 {
                    let rtt = if info.populated_ranks_mask[channel] == 3 {
                        MR1_RZQ4
                    } else {
                        MR1_RZQ2
                    };
                    (rtt, 4)
                } else {
                    (MR1_RZQ2, 64)
                }
            } else if info.populated_ranks_mask[channel] & 5 == 5 {
                rtt_wr = MR2_RZQ2;
                (MR1_RZQ12, 64)
            } else {
                (MR1_RZQ4, 4)
            };
            let base = (channel as u32) << 10;
            self.mch.write16(0x588 + base, 0);
            self.mch.write16(0x58a + base, 4);
            self.mch.write16(0x58c + base, (rtt | MR1_ODS34OHM) as u16);
            self.mch.write16(0x58e + base, (rzq_reg58e | 0x82) as u16);
            self.mch.write16(0x590 + base, 0x1282);
            for slot in 0..NUM_SLOTS {
                for rank in 0..NUM_RANKS {
                    if !info.populated(channel, slot, rank) {
                        continue;
                    }
                    self.jedec_read(
                        channel,
                        slot,
                        rank,
                        total_rank,
                        0x28,
                        rtt_wr
                            | (u32::from(info.clock_speed_index) << 3)
                            | (auto_self_refresh << 6)
                            | (self_refresh_temperature << 7),
                    );
                    self.jedec_read(channel, slot, rank, total_rank, 0x38, 0);
                    self.jedec_read(channel, slot, rank, total_rank, 0x18, rtt | MR1_ODS34OHM);
                    self.jedec_read(
                        channel,
                        slot,
                        rank,
                        total_rank,
                        6,
                        (dll_on << 12)
                            | (write_recovery << 9)
                            | ((u32::from(info.cas_latency) - 4) << 4)
                            | MR0_BT_INTERLEAVED
                            | MR0_DLL_RESET_ON,
                    );
                    total_rank += 1;
                }
            }
        }
    }

    /// coreboot `program_modules_memory_map()`.
    pub(super) fn program_modules_memory_map(&mut self, pre_jedec: bool) {
        let info = &mut *self.info;
        let mut total_mb = [0u32; NUM_CHANNELS];
        for channel in 0..NUM_CHANNELS {
            for slot in 0..NUM_SLOTS {
                for rank in 0..NUM_RANKS {
                    if info.populated(channel, slot, rank) {
                        total_mb[channel] += if pre_jedec {
                            256
                        } else {
                            (256 << info.density[channel][slot])
                                >> info.is_x16_module[channel][slot]
                        };
                        let config = if pre_jedec {
                            1 | ((1 + 1) << 1)
                        } else {
                            info.is_x16_module[channel][slot]
                                | ((info.density[channel][slot] + 1) << 1)
                        };
                        self.mch.write8(
                            0x208 + (rank + 2 * slot) as u32 + ((channel as u32) << 10),
                            config | 0x80,
                        );
                    }
                    self.mch.write16(
                        0x200 + ((channel as u32) << 10) + 4 * slot as u32 + 2 * rank as u32,
                        (total_mb[channel] >> 6) as u16,
                    );
                }
            }
        }
        info.total_memory_mb = total_mb[0] + total_mb[1];
        info.interleaved_part_mb = if pre_jedec {
            0
        } else {
            2 * total_mb[0].min(total_mb[1])
        };
        info.non_interleaved_part_mb = total_mb[0] + total_mb[1] - info.interleaved_part_mb;
        let channel_0_non_interleaved = total_mb[0] - info.interleaved_part_mb / 2;
        self.mch.write32(
            0x100,
            channel_0_non_interleaved | (info.non_interleaved_part_mb << 16),
        );
        if !pre_jedec {
            self.mch.write16(0x104, info.interleaved_part_mb as u16);
        }
    }

    /// coreboot `program_board_delay()`.
    #[allow(clippy::too_many_lines)]
    pub(super) fn program_board_delay(&mut self) {
        let mut high_multiplier = 0u32;
        let mut some_delay_ns = 200u32;
        let mut some_delay_3_half_cycles = 4u32;
        let silrev = self.info.silicon_revision;
        let mut cas_latency_shift = u32::from(silrev == 0 || silrev == 1);
        if self.info.revision < 8 {
            some_delay_ns = 600;
            cas_latency_shift = 0;
        }
        let clock = u32::from(self.info.clock_speed_index);
        let cas = u32::from(self.info.cas_latency);
        let use_ecc = u32::from(self.info.use_ecc);
        {
            let speed_bit = u32::from(clock > 1 || (silrev != 2 && silrev != 3))
                ^ u32::from(self.info.revision >= 0x10);
            self.write_500(0, speed_bit | (u32::from(use_ecc == 0) << 1), 0x60e, 3, 1);
            self.write_500(1, speed_bit | (u32::from(use_ecc == 0) << 1), 0x60e, 3, 1);
            if self.info.revision >= 0x10 && clock <= 1 && (silrev == 2 || silrev == 3) {
                self.rmw_1d0(0x116, 5, 2, 4);
            }
        }
        let info = &*self.info;
        let max_slots = u32::from(info.max_slots_used_in_channel);
        self.mch
            .write32(0x120, (1 << (max_slots + 28)) | 0x188e_7f9f);
        self.mch.write8(
            0x124,
            info.board_lane_delay[4].wrapping_add(info.frequency_01().div_ceil(1000) as u8),
        );
        self.mch.write16(0x125, 0x1360);
        self.mch.write8(0x127, 0x40);
        let freq11 = info.frequency_11();
        let fsb = u32::from(info.fsb_frequency);
        if fsb < freq11 / 2 {
            high_multiplier = 1;
            let some_delay_2_half_cycles = info.ps_to_halfcycles(
                ((3 * info.fsbcycle_ps()) >> 1)
                    + ((info.halfcycle_ps() * u32::from(REG178_MIN[clock as usize])) >> 6)
                    + 4 * info.halfcycle_ps()
                    + 2230,
            );
            let scaled = (freq11 * 2).wrapping_mul(28u32.wrapping_sub(some_delay_2_half_cycles))
                / (freq11 * 2).wrapping_sub(4 * fsb);
            some_delay_3_half_cycles = (some_delay_2_half_cycles.wrapping_add(scaled) >> 3).min(7);
        }
        if self.mch.read8(0x2ca9) & 1 != 0 {
            some_delay_3_half_cycles = 3;
        }
        for channel in 0..NUM_CHANNELS as u32 {
            let base = channel << 10;
            self.mch.setbits32(0x220 + base, 0x1800_1117);
            let value = ((max_slots as i32 - 1)
                | ((cas as i32 - 5 - clock as i32) << 21)
                | ((max_slots as i32 + cas as i32 - cas_latency_shift as i32 - 4) << 16)
                | ((cas as i32 - cas_latency_shift as i32 - 4) << 26)
                | ((cas as i32 - clock as i32 + max_slots as i32 - 6) << 8))
                as u32;
            self.mch.write32(0x224 + base, value);
            self.mch.write32(0x228 + base, max_slots);
            self.mch.write8(0x239 + base, 32);
            self.mch.write32(
                0x248 + base,
                (high_multiplier << 24) | (some_delay_3_half_cycles << 25) | 0x84_0000,
            );
            self.mch.write32(0x278 + base, 0x0c36_2042);
            self.mch.write32(0x27c + base, 0x8b00_0062);
            self.mch.write32(
                0x24c + base,
                (u32::from(clock != 0) << 17)
                    | ((2 + clock - u32::from(clock != 0)) << 12)
                    | 0x10200,
            );
            self.mch.write8(0x267 + base, 4);
            self.mch.write16(0x272 + base, 0x155);
            self.mch.clrsetbits32(0x2bc + base, 0xff_ffff, 0x70_7070);
            let ranks = &self.info.populated_ranks[channel as usize];
            let mask = u32::from(ranks[1][1] == 0)
                | (u32::from(ranks[1][0] == 0) << 1)
                | (u32::from(ranks[0][1] == 0) << 2)
                | (u32::from(ranks[0][0] == 0) << 3);
            self.write_500(channel as usize, mask, 0x4c9, 4, 1);
        }
        self.mch
            .write8(0x2c4, ((1 + u8::from(clock != 0)) << 6) | 0xc);
        {
            let freq11 = self.info.frequency_11();
            let fsb = u32::from(self.info.fsb_frequency);
            let freq_divisor: u32 = if fsb == freq11 {
                3
            } else if 2 * fsb < 3 * (freq11 / 2) {
                1
            } else {
                2
            };
            self.mch.write32(0x2c0, (freq_divisor << 11) | 0x6009_c400);
        }
        let lane3 = u32::from(self.info.board_lane_delay[3]);
        let lane_3_delay = if lane3 <= 10 {
            if lane3 <= 8 { lane3 } else { 10 }
        } else {
            12
        };
        let mut cas_latency_derived = cas + 2 - clock;
        if clock > 1 {
            cas_latency_derived += 1;
        }
        for channel in 0..NUM_CHANNELS as u32 {
            let base = channel << 10;
            self.mch.write32(
                0x240 + base,
                (u32::from(clock == 0) * 0x11000) | 0x0100_2100 | ((2 + clock) << 4) | (cas - 3),
            );
            self.write_500(channel as usize, (clock << 1) | 1, 0x609, 6, 1);
            self.write_500(channel as usize, clock + 2 * cas - 7, 0x601, 6, 1);
            let bld = self.info.board_lane_delay.map(u32::from);
            self.mch.write32(
                0x250 + base,
                ((lane_3_delay + clock + 9) << 6)
                    | (bld[7] << 2)
                    | (bld[4] << 16)
                    | (bld[1] << 25)
                    | (bld[1] << 29)
                    | 1,
            );
            self.mch.write32(
                0x254 + base,
                (bld[1] >> 3)
                    | ((bld[8] + 4 * use_ecc) << 6)
                    | 0x80
                    | (bld[6] << 1)
                    | (bld[2] << 28)
                    | (cas_latency_derived << 16)
                    | 0x0470_0000,
            );
            self.mch.write32(
                0x258 + base,
                ((bld[5] + clock + 9) << 12)
                    | (((clock as i32 - cas as i32 + 12) as u32) << 8)
                    | (bld[2] << 17)
                    | (bld[4] << 24)
                    | 0x47,
            );
            self.mch
                .write32(0x25c + base, (bld[1] << 1) | (bld[0] << 8) | 0x1da5_0000);
            self.mch.write8(0x264 + base, 0xff);
            self.mch
                .write8(0x5f8 + base, ((cas_latency_shift << 3) | use_ecc) as u8);
        }

        self.program_modules_memory_map(true);

        let cycles = (self.info.ns_to_cycles(some_delay_ns) / 2).min(127);
        self.mch
            .clrsetbits16(0x610, 0xfe3c, ((cycles << 9) | 0x3c) as u16);
        self.mch.setbits16(0x612, 1 << 8);
        self.mch.setbits16(0x214, 0x3e00);
        let sad = self.qpi_sad();
        for i in 0..8u16 {
            sad.write32(
                sad_dram_rule(i),
                (self.info.total_memory_mb - 64) | u32::from(i == 0) | 2,
            );
            sad.write32(sad_interleave_list(i), 0);
        }
    }

    /// coreboot `program_total_memory_map()`: TOM, TOLUD, TOUUD, remap,
    /// IGD/GTT stolen memory, TSEG and the uncore DRAM rules.
    pub(super) fn program_total_memory_map(&mut self) {
        const UMA_SIZES_GTT: [u32; 16] = [0, 1, 0, 2, 0, 0, 0, 0, 0, 2, 3, 4, 42, 42, 42, 42];
        const UMA_SIZES_IGD: [u32; 16] = [
            0, 0, 0, 0, 0, 32, 48, 64, 128, 256, 96, 160, 224, 352, 256, 512,
        ];
        let nb = northbridge();
        let info = &*self.info;
        let (uma_size_igd, uma_size_gtt) = if info.uma_enabled != 0 {
            let ggc = nb.read16(GGC);
            (
                UMA_SIZES_IGD[usize::from((ggc >> 4) & 0xf)],
                UMA_SIZES_GTT[usize::from((ggc >> 8) & 0xf)],
            )
        } else {
            (0, 0)
        };
        let align_down = |value: u32, align: u32| value & !(align - 1);
        let align_up = |value: u32, align: u32| (value + align - 1) & !(align - 1);
        let mut memory_map = [0u32; 8];
        let mut tom = info.total_memory_mb;
        if tom == 4096 {
            tom = 4032;
        }
        let mut touud = align_down(tom - info.memory_reserved_for_heci_mb, 64);
        let tolud = align_down(
            (4096 - DEFAULT_PCI_MMIO_SIZE_MIB + align_up(uma_size_igd + uma_size_gtt, 64))
                .min(touud),
            64,
        );
        let mut remap_base = 0;
        let memory_remap = touud - tolud > 64;
        if memory_remap {
            remap_base = 4096u32.max(touud);
            touud = touud - tolud + 4096;
        }
        if touud > 4096 {
            memory_map[2] = touud | 1;
        }
        let mut quickpath_reserved = 0u32;
        let t = self.qpi_sad().read32(0x68);
        if t & 0x800 != 0 {
            let shift = t >> 20;
            if shift == 0 {
                die("QuickPath value is 0");
            }
            quickpath_reserved = 1 << find_lowest_bit_set32(shift);
        }
        if memory_remap {
            touud -= quickpath_reserved;
        }
        let uma_base_igd = tolud - uma_size_igd;
        let uma_base_gtt = uma_base_igd - uma_size_gtt;
        let mut tseg_base = align_down(uma_base_gtt, 64) - TSEG_SIZE_MIB;
        if !memory_remap {
            tseg_base -= quickpath_reserved;
        }
        tseg_base = align_down(tseg_base, 8);

        nb.write16(TOLUD, (tolud << 4) as u16);
        nb.write16(TOM, (tom >> 6) as u16);
        if memory_remap {
            nb.write16(REMAPBASE, (remap_base >> 6) as u16);
            nb.write16(REMAPLIMIT, ((touud - 64) >> 6) as u16);
        }
        nb.write16(TOUUD, touud as u16);
        if info.uma_enabled != 0 {
            nb.write32(IGD_BASE, uma_base_igd << 20);
            nb.write32(GTT_BASE, uma_base_gtt << 20);
        }
        nb.write32(TSEG, tseg_base << 20);

        let mut current_limit = 0u32;
        memory_map[0] = align_down(uma_base_gtt, 64) | 1;
        memory_map[1] = 4096;
        let sad = self.qpi_sad();
        for (i, rule) in memory_map.iter().enumerate() {
            current_limit = current_limit.max(rule & !1);
            sad.write32(
                sad_dram_rule(i as u16),
                (rule & 1) | align_down(current_limit.wrapping_sub(1), 64) | 2,
            );
            sad.write32(sad_interleave_list(i as u16), 0);
        }
        fstart_log::info!(
            "ironlake: TOM {} MiB, TOLUD {} MiB, TOUUD {} MiB, TSEG {} MiB",
            tom,
            tolud,
            touud,
            tseg_base
        );
    }

    /// coreboot `have_match_ranks()`.
    pub(super) fn have_match_ranks(&self, channel: usize, ranks: u32) -> bool {
        let info = &*self.info;
        let r = &info.populated_ranks[channel];
        let in_channel =
            u32::from(r[0][0]) + u32::from(r[0][1]) + u32::from(r[1][0]) + u32::from(r[1][1]);
        if in_channel == 0 {
            return true;
        }
        if in_channel != ranks {
            return false;
        }
        if r[0][0] != r[1][0] || r[0][1] != r[1][1] {
            return true;
        }
        info.is_x16_module[channel][0] == info.is_x16_module[channel][1]
            && info.density[channel][0] == info.density[channel][1]
    }

    /// coreboot `save_timings()`: the verbatim registers an S3 resume
    /// restores.
    pub(super) fn save_timings(&mut self) -> RamTraining {
        let mut train = self.info.training;
        for (channel, slot, rank) in populated_ranks(self.info).collect::<heapless::Vec<_, 8>>() {
            for lane in 0..9 {
                for tm in 0..4 {
                    train.lane_timings[tm][channel][slot][rank][lane] =
                        self.read_500(channel, get_timing_register_addr(lane, tm, slot, rank), 9);
                }
            }
        }
        train.reg_178 = self.read_1d0(0x178, 7);
        train.reg_10b = self.read_1d0(0x10b, 6);
        for channel in 0..NUM_CHANNELS {
            let reg32 = self.mch.read32(((channel as u32) << 10) + 0x274);
            train.reg274265[channel][0] = (reg32 >> 16) as u8;
            train.reg274265[channel][1] = reg32 as u8;
            train.reg274265[channel][2] =
                (self.mch.read16(((channel as u32) << 10) + 0x265) >> 8) as u8;
        }
        train.reg2ca9_bit0 = self.mch.read8(0x2ca9) & 1;
        train.reg_6dc = self.mch.read32(0x6dc);
        train.reg_6e8 = self.mch.read32(0x6e8);
        train
    }

    /// coreboot `write_training_data()`: the cached lane timings.
    pub(super) fn write_training_data(&mut self, cached: &RamTraining) {
        if self.info.revision < 8 {
            return;
        }
        for tm in 0..4 {
            for channel in 0..NUM_CHANNELS {
                for slot in 0..NUM_SLOTS {
                    for rank in 0..NUM_RANKS {
                        for lane in 0..9 {
                            self.write_500(
                                channel,
                                u32::from(cached.lane_timings[tm][channel][slot][rank][lane]),
                                get_timing_register_addr(lane, tm, slot, rank),
                                9,
                                0,
                            );
                        }
                    }
                }
            }
        }
        self.write_1d0(u32::from(cached.reg_178), 0x178, 7, 1);
        self.write_1d0(u32::from(cached.reg_10b), 0x10b, 6, 1);
    }
}
