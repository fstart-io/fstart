//! Read/write timing training and cached-training replay (coreboot
//! `raminit.c`: the memory test patterns through `ram_training()`).

use super::*;

/// Lane training state machine (coreboot `enum state`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    BeforeUsable,
    AtUsable,
    AtMargin,
    Complete,
}

/// coreboot `seed1`.
const SEED1: [u32; 0x18] = [
    0x3a9d_5ab5,
    0x576c_b65b,
    0x5557_73b6,
    0x2ab7_72ee,
    0x5555_56ee,
    0x3a9d_5ab5,
    0x576c_b65b,
    0x5557_73b6,
    0x2ab7_72ee,
    0x5555_56ee,
    0x5155_a555,
    0x5155_a555,
    0x5155_a555,
    0x5155_a555,
    0x3a9d_5ab5,
    0x576c_b65b,
    0x5557_73b6,
    0x2ab7_72ee,
    0x5555_56ee,
    0x55d6_b4a5,
    0x366d_6b3a,
    0x2ae5_ddbb,
    0x3b9d_dbb7,
    0x55d6_b4a5,
];

/// coreboot `get_etalon2()`: the type-1 test pattern.
fn get_etalon2(flip: bool, addr: u32) -> u32 {
    const INVMASK: [u16; 15] = [
        0xaaaa, 0x6db6, 0x4924, 0xeeee, 0xcccc, 0x8888, 0x7bde, 0x739c, 0x6318, 0x4210, 0xefbe,
        0xcf3c, 0x8e38, 0x0c30, 0x0820,
    ];
    let comp4 = addr / 480;
    let addr = addr % 480;
    let comp1 = addr & 0xf;
    let comp2 = (addr >> 4) & 1;
    let comp3 = addr >> 5;
    let mut ret = if comp4 != 0 {
        0x0101_0101u32.wrapping_shl(comp4 - 1)
    } else {
        0
    };
    if u32::from(flip) ^ ((u32::from(INVMASK[comp3 as usize]) >> comp1) ^ comp2) & 1 != 0 {
        ret = !ret;
    }
    ret
}

/// coreboot `get_seed2()`.
fn get_seed2(a: usize, b: bool) -> u32 {
    const SEED2: [u32; 5] = [
        0x5555_5555,
        0x3333_3333,
        0x2e55_5a55,
        0x5555_5555,
        0x5b6d_b6db,
    ];
    let r = SEED2[(a + usize::from(a >= 10)) / 5];
    if b { !r } else { r }
}

/// coreboot `make_shift()`.
fn make_shift(comp2: u32, comp5: usize, x: u32) -> u32 {
    const SEED3: [u8; 32] = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x38, 0x1c, 0x3c, 0x18, 0x38,
        0x38, 0x38, 0x38, 0x38, 0x38, 0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x0f, 0x0f,
        0x0f, 0x0f,
    ];
    comp2.wrapping_sub((u32::from(SEED3[comp5]) >> (x & 7)) & 1) & 0x1f
}

/// coreboot `get_etalon()`: the type-2 test pattern.
fn get_etalon(flip: bool, addr: u32) -> u32 {
    let comp1 = (addr >> 1) & 1;
    let comp2 = (addr >> 3) & 0x1f;
    let comp3 = (addr >> 8) & 0xf;
    let comp4 = (addr >> 12) & 0xf != 0;
    let comp5 = ((addr >> 16) & 0x1f) as usize;
    let mask_bit = !(0x10001u32 << comp3);
    let flip = u32::from(flip);
    let seed = SEED1[comp5 % SEED1.len()];
    let part2 = ((seed >> make_shift(comp2, comp5, (comp3 >> 3) | (comp1 << 2) | 2)) & 1) ^ flip;
    let part1 = ((seed >> make_shift(comp2, comp5, (comp3 >> 3) | (comp1 << 2))) & 1) ^ flip;
    let mut mask_byte = 0u32;
    for byte in 0..4 {
        if (get_seed2(comp5, comp4) >> make_shift(comp2, comp5, byte | (comp1 << 2))) & 1 != 0 {
            mask_byte |= 0xff << (8 * byte);
        }
    }
    (mask_bit & mask_byte) | (part1 << comp3) | (part2 << (comp3 + 16))
}

/// coreboot `read128()`: one 16-byte aligned load, as the vendor test does.
fn read128(addr: u32) -> [u32; 4] {
    let mut out = [0u32; 4];
    let mut save = [0u64; 2];
    // SAFETY: `addr` is 16-byte aligned memory under test, mapped by the
    // bootstrap identity map. xmm0 is preserved through `save`.
    unsafe {
        core::arch::asm!(
            "movdqu [{save}], xmm0",
            "movdqa xmm0, [{addr}]",
            "movdqu [{out}], xmm0",
            "movdqu xmm0, [{save}]",
            save = in(reg) save.as_mut_ptr(),
            addr = in(reg) addr as usize,
            out = in(reg) out.as_mut_ptr(),
            options(nostack, preserves_flags),
        );
    }
    out
}

/// Variable MTRR 3 caches the region under test write-protected.
fn enable_cache_region(base: u32, size: u32) {
    let mask = !((size + 4096) & !4095).wrapping_sub(1) | (1 << 11);
    // SAFETY: MTRR 3 is free during raminit (CAR uses 0 and 1).
    unsafe {
        fstart_arch::x86::msr::wrmsr(IA32_MTRR_PHYSBASE0 + 2 * 3, u64::from(base | 5));
        fstart_arch::x86::msr::wrmsr(IA32_MTRR_PHYSMASK0 + 2 * 3, u64::from(mask) | (0xf << 32));
    }
}

fn disable_cache_region() {
    // SAFETY: as for enable_cache_region.
    unsafe {
        fstart_arch::x86::msr::wrmsr(IA32_MTRR_PHYSBASE0 + 2 * 3, 0);
        fstart_arch::x86::msr::wrmsr(IA32_MTRR_PHYSMASK0 + 2 * 3, 0);
    }
}

fn flush_cache(start: u32, size: u32) {
    let end = start + ((size + 4096) & !4095);
    for addr in (start..end).step_by(64) {
        // SAFETY: the tested range is mapped; CLFLUSH only evicts lines.
        unsafe {
            fstart_arch::x86::flush_cache_line(addr as *const u8);
        }
    }
}

/// coreboot `clear_errors()`.
fn clear_errors() {
    northbridge().write8(0xc0, 0x01);
}

/// coreboot `write_testing()`.
fn write_testing(totalrank: u32, flip: bool) {
    let base = totalrank << 28;
    let mut nwrites = 0;
    for offset in (0..9 * 480u32).step_by(2) {
        write32p(base + offset * 8, get_etalon2(flip, offset));
        write32p(base + offset * 8 + 4, get_etalon2(flip, offset));
        write32p(base + offset * 8 + 8, get_etalon2(flip, offset + 1));
        write32p(base + offset * 8 + 12, get_etalon2(flip, offset + 1));
        nwrites += 4;
        if nwrites >= 320 {
            clear_errors();
            nwrites = 0;
        }
    }
}

/// coreboot `check_testing()`: the failing byte lanes.
fn check_testing(total_rank: u32, flip: bool) -> u8 {
    let mut failmask = 0u8;
    let mut failxor = [0u32; 2];
    enable_cache_region(total_rank << 28, 1728 * 5 * 4);
    let mut comp3 = 0;
    while comp3 < 9 && failmask != 0xff {
        for comp1 in 0..4u32 {
            for comp2 in 0..60u32 {
                let current = comp3 * 8 * 60 + 2 * comp1 + 8 * comp2;
                let re = read128((total_rank << 28) | (current << 3));
                failxor[0] |= get_etalon2(flip, current) ^ re[0];
                failxor[1] |= get_etalon2(flip, current) ^ re[1];
                failxor[0] |= get_etalon2(flip, current | 1) ^ re[2];
                failxor[1] |= get_etalon2(flip, current | 1) ^ re[3];
            }
        }
        for i in 0..8 {
            if (0xff << (8 * (i % 4))) & failxor[i / 4] != 0 {
                failmask |= 1 << i;
            }
        }
        comp3 += 1;
    }
    disable_cache_region();
    flush_cache(total_rank << 28, 1728 * 5 * 4);
    failmask
}

/// coreboot `write_testing_type2()`.
fn write_testing_type2(totalrank: u32, region: u32, block: u32, flip: bool) {
    for i in 0..2048u32 {
        write32p(
            (totalrank << 28) | (region << 25) | (block << 16) | (i << 2),
            get_etalon(flip, (block << 16) | (i << 2)),
        );
    }
}

/// coreboot `check_testing_type2()`.
fn check_testing_type2(totalrank: u32, region: u32, block: u32, flip: bool) -> u8 {
    let mut failmask = 0u8;
    let mut failxor = [0u32; 2];
    enable_cache_region(totalrank << 28, 134_217_728);
    let mut comp3 = 0u32;
    while comp3 < 2 && failmask != 0xff {
        for comp1 in 0..16u32 {
            for comp2 in 0..64u32 {
                let addr = (totalrank << 28)
                    | (region << 25)
                    | (block << 16)
                    | (comp3 << 12)
                    | (comp2 << 6)
                    | (comp1 << 2);
                failxor[(comp1 & 1) as usize] |= read32p(addr) ^ get_etalon(flip, addr);
            }
        }
        for i in 0..8 {
            if (0xff << (8 * (i % 4))) & failxor[i / 4] != 0 {
                failmask |= 1 << i;
            }
        }
        comp3 += 1;
    }
    disable_cache_region();
    flush_cache((totalrank << 28) | (region << 25) | (block << 16), 16384);
    failmask
}

/// coreboot's `for (u8 reg_178 = center - span; reg_178 <= center + span;
/// reg_178 += span)`, with its `u8` wrap-around, bounded to the three
/// values it yields for any center the training can produce.
fn reg178_sweep(center: u8, span: u8) -> impl Iterator<Item = u8> {
    let end = u32::from(center) + u32::from(span);
    let mut reg = center.wrapping_sub(span);
    let mut steps = 0;
    core::iter::from_fn(move || {
        if steps == 3 || u32::from(reg) > end {
            return None;
        }
        let current = reg;
        reg = reg.wrapping_add(span);
        steps += 1;
        Some(current)
    })
}

/// The bounds a `reg_178` value indexes; coreboot's array has 64 entries.
fn bounds_at(timings: &[TimingBounds; 64], reg_178: u8) -> Option<&TimingBounds> {
    timings.get(usize::from(reg_178))
}

fn check_bounded(vals: &[u16; 8], bound: u16) -> bool {
    vals.iter().all(|value| *value >= bound)
}

fn validate_state(state: &[State; 8]) -> bool {
    state.iter().all(|state| *state == State::Complete)
}

/// coreboot `do_fsm()`: advance each lane's search for a passing window
/// of at least `margin` values.
#[allow(clippy::too_many_arguments)]
fn do_fsm(
    state: &mut [State; 8],
    counter: &mut [u16; 8],
    fail_mask: u8,
    margin: u16,
    uplimit: u8,
    res_low: &mut [u8; 8],
    res_high: &mut [u8; 8],
    val: u8,
) {
    for lane in 0..8 {
        let is_fail = (fail_mask >> lane) & 1 != 0;
        match state[lane] {
            State::BeforeUsable => {
                if !is_fail {
                    counter[lane] = 1;
                    state[lane] = State::AtUsable;
                } else {
                    counter[lane] = 0;
                    state[lane] = State::BeforeUsable;
                }
            }
            State::AtUsable => {
                if !is_fail {
                    counter[lane] += 1;
                    if counter[lane] >= margin {
                        state[lane] = State::AtMargin;
                        res_low[lane] = val.wrapping_sub(margin as u8).wrapping_add(1);
                    } else {
                        state[lane] = State::AtUsable;
                    }
                } else {
                    counter[lane] = 0;
                    state[lane] = State::BeforeUsable;
                }
            }
            State::AtMargin => {
                if is_fail {
                    state[lane] = State::Complete;
                    res_high[lane] = val.wrapping_sub(1);
                } else {
                    counter[lane] += 1;
                    state[lane] = State::AtMargin;
                    if val == uplimit {
                        state[lane] = State::Complete;
                        res_high[lane] = uplimit;
                    }
                }
            }
            State::Complete => {}
        }
    }
}

impl Raminit<'_> {
    /// coreboot `read_4090()`: the estimation of the training start points.
    fn read_4090(&mut self) {
        for channel in 0..2 {
            for slot in 0..NUM_SLOTS {
                for rank in 0..NUM_RANKS {
                    for lane in 0..9 {
                        self.info.training.lane_timings[0][channel][slot][rank][lane] = 32;
                    }
                }
            }
        }
        for tm in 1..4 {
            for channel in 0..NUM_CHANNELS {
                for slot in 0..NUM_SLOTS {
                    for rank in 0..NUM_RANKS {
                        for lane in 0..9 {
                            let value = self.read_500(
                                channel,
                                get_timing_register_addr(lane, tm, slot, rank),
                                9,
                            ) + if tm == 1 { 11 } else { 0 };
                            self.info.training.lane_timings[tm][channel][slot][rank][lane] = value;
                        }
                    }
                }
            }
        }
    }

    /// Program the tm 0/1 pair of a lane from a tm-0 value.
    fn write_timing01(&mut self, channel: usize, slot: usize, rank: usize, lane: usize, tm0: u32) {
        let lt = &self.info.training.lane_timings;
        let tm1 = tm0
            .wrapping_add(u32::from(lt[1][channel][slot][rank][lane]))
            .wrapping_sub(u32::from(lt[0][channel][slot][rank][lane]));
        self.write_500(
            channel,
            tm0,
            get_timing_register_addr(lane, 0, slot, rank),
            9,
            1,
        );
        self.write_500(
            channel,
            tm1,
            get_timing_register_addr(lane, 1, slot, rank),
            9,
            1,
        );
    }

    /// coreboot `train_ram_at_178()`.
    #[allow(clippy::too_many_arguments)]
    fn train_ram_at_178(
        &mut self,
        channel: usize,
        slot: usize,
        rank: usize,
        total_rank: u32,
        reg_178: u8,
        first_run: bool,
        niter: u32,
        timings: &mut [TimingBounds; 64],
    ) {
        if bounds_at(timings, reg_178).is_none() {
            fstart_log::error!(
                "ironlake: reg178 {} outside the training table",
                u32::from(reg_178)
            );
            return;
        }
        let reg_178 = usize::from(reg_178);
        let mut state = [State::BeforeUsable; 8];
        let mut count = [0u16; 8];
        let mut lower_usable = [0u8; 8];
        let mut upper_usable = [0u8; 8];
        let mut checked = [0u16; 8];

        if !first_run {
            let mut all_ok = true;
            for lane in 0..8 {
                let bound = &mut timings[reg_178][channel][slot][rank][lane];
                if bound.smallest == bound.largest {
                    *bound = Bound::default();
                    all_ok = false;
                }
            }
            if all_ok {
                state = [State::Complete; 8];
            }
        }

        let mut reg1b3 = 0u8;
        while reg1b3 < 0x30 && !validate_state(&state) {
            self.write_1d0(u32::from(reg1b3 ^ 32), 0x1b3, 6, 1);
            self.write_1d0(u32::from(reg1b3 ^ 32), 0x1a3, 6, 1);
            let failmask = check_testing(total_rank, false);
            self.mch.setbits32(0xfb0, 3 << 16);
            do_fsm(
                &mut state,
                &mut count,
                failmask,
                5,
                47,
                &mut lower_usable,
                &mut upper_usable,
                reg1b3,
            );
            reg1b3 += 1;
        }

        if reg1b3 != 0 {
            self.write_1d0(0, 0x1b3, 6, 1);
            self.write_1d0(0, 0x1a3, 6, 1);
            for lane in 0..8 {
                if state[lane] == State::Complete {
                    let base = (self.info.training.lane_timings[0][channel][slot][rank][lane]
                        & 0x3f) as i32
                        - 32;
                    let bound = &mut timings[reg_178][channel][slot][rank][lane];
                    bound.smallest = (i32::from(lower_usable[lane]) + base) as u8;
                    bound.largest = (i32::from(upper_usable[lane]) + base) as u8;
                }
            }
        }

        if first_run {
            return;
        }

        for (step, increase) in [(2u16, true), (3u16, false)] {
            for lane in 0..8 {
                if state[lane] == State::Complete {
                    let bound = timings[reg_178][channel][slot][rank][lane];
                    let edge = if increase {
                        bound.smallest
                    } else {
                        bound.largest
                    };
                    self.write_timing01(channel, slot, rank, lane, u32::from(edge));
                    checked[lane] = 0;
                } else {
                    checked[lane] = 0xffff;
                }
            }
            loop {
                let mut failmask = 0u8;
                for i in 0..niter {
                    if failmask == 0xff {
                        break;
                    }
                    failmask |= check_testing_type2(total_rank, 2, i, false);
                    failmask |= check_testing_type2(total_rank, 3, i, true);
                }
                self.mch.setbits32(0xfb0, 3 << 16);
                for lane in 0..8 {
                    if checked[lane] == 0xffff {
                        continue;
                    }
                    if (1 << lane) & failmask != 0 {
                        let bound = &mut timings[reg_178][channel][slot][rank][lane];
                        if bound.largest <= bound.smallest {
                            checked[lane] = 0xffff;
                        } else {
                            checked[lane] = 0;
                            let edge = if increase {
                                bound.smallest += 1;
                                bound.smallest
                            } else {
                                bound.largest -= 1;
                                bound.largest
                            };
                            self.write_timing01(channel, slot, rank, lane, u32::from(edge));
                        }
                    } else {
                        checked[lane] += 1;
                    }
                }
                if check_bounded(&checked, step) {
                    break;
                }
            }
        }

        for lane in 0..8 {
            let lt = self.info.training.lane_timings;
            self.write_500(
                channel,
                u32::from(lt[0][channel][slot][rank][lane]),
                get_timing_register_addr(lane, 0, slot, rank),
                9,
                1,
            );
            self.write_500(
                channel,
                u32::from(lt[1][channel][slot][rank][lane]),
                get_timing_register_addr(lane, 1, slot, rank),
                9,
                1,
            );
            let bound = &mut timings[reg_178][channel][slot][rank][lane];
            if bound.largest <= bound.smallest {
                *bound = Bound::default();
            }
        }
    }

    /// coreboot `set_10b()`.
    pub(super) fn set_10b(&mut self, val: u8) {
        if self.read_1d0(0x10b, 6) == u16::from(val) {
            return;
        }
        self.write_1d0(u32::from(val), 0x10b, 6, 1);
        let lut = u16::from(LUT16[usize::from(self.info.clock_speed_index)]);
        for (channel, slot, rank) in
            populated_ranks_backwards(self.info).collect::<heapless::Vec<_, 8>>()
        {
            for lane in 0..9 {
                let addr = get_timing_register_addr(lane, 0, slot, rank);
                let mut reg_500 = self.read_500(channel, addr, 9);
                if val == 1 {
                    reg_500 = reg_500.saturating_sub(lut);
                } else {
                    reg_500 += lut;
                }
                self.write_500(channel, u32::from(reg_500), addr, 9, 1);
            }
        }
    }

    fn set_ecc(&self, on: bool) {
        for channel in 0..NUM_CHANNELS as u32 {
            let reg = (channel << 10) + 0x5f8;
            let t = self.mch.read8(reg);
            self.mch.write8(reg, if on { t | 1 } else { t & !1 });
        }
    }

    fn set_178(&self, val: u8) {
        let val = if val >= 31 { val - 31 } else { 63 - val };
        self.write_1d0(2 * u32::from(val), 0x178, 7, 1);
    }

    fn write_500_timings_type(&mut self, channel: usize, slot: usize, rank: usize, tm: usize) {
        for lane in 0..8 {
            let value = self.info.training.lane_timings[tm][channel][slot][rank][lane];
            self.write_500(
                channel,
                u32::from(value),
                get_timing_register_addr(lane, tm, slot, rank),
                9,
                0,
            );
        }
    }

    /// coreboot `try_timing_offsets()`.
    fn try_timing_offsets(&mut self, channel: usize, slot: usize, rank: usize, totalrank: u32) {
        let mut count = [0u16; 8];
        let mut state = [State::BeforeUsable; 8];
        let mut lower_usable = [0u8; 8];
        let mut upper_usable = [0u8; 8];
        let mut flip = true;
        for lane in 0..8 {
            let value = self.info.training.lane_timings[2][channel][slot][rank][lane] + 32;
            self.write_500(
                channel,
                u32::from(value),
                get_timing_register_addr(lane, 3, slot, rank),
                9,
                1,
            );
        }
        let mut timing_offset = 0u8;
        while !validate_state(&state) && timing_offset < 64 {
            self.write_1d0(u32::from(timing_offset ^ 32), 0x1bb, 6, 1);
            let mut failmask = 0u8;
            for _ in 0..2 {
                if failmask == 0xff {
                    break;
                }
                flip = !flip;
                write_testing(totalrank, flip);
                failmask |= check_testing(totalrank, flip);
            }
            do_fsm(
                &mut state,
                &mut count,
                failmask,
                10,
                63,
                &mut lower_usable,
                &mut upper_usable,
                timing_offset,
            );
            timing_offset += 1;
        }
        self.write_1d0(0, 0x1bb, 6, 1);
        if !validate_state(&state) {
            die("couldn't discover DRAM timings (1)");
        }
        for lane in 0..8 {
            let mut bias = 0u8;
            if self.info.silicon_revision != 0 {
                let usable_length = i32::from(upper_usable[lane]) - i32::from(lower_usable[lane]);
                if usable_length >= 20 {
                    bias = ((usable_length / 2 - 10) as u8).min(2);
                }
            }
            let tm2 = self.info.training.lane_timings[2][channel][slot][rank][lane];
            let value = u32::from(tm2)
                + (u32::from(upper_usable[lane]) + u32::from(lower_usable[lane])) / 2
                - u32::from(bias);
            self.write_500(
                channel,
                value,
                get_timing_register_addr(lane, 3, slot, rank),
                9,
                1,
            );
            let training = &mut self.info.training;
            training.timing2_bounds[channel][slot][rank][lane][0] =
                tm2 + u16::from(lower_usable[lane]);
            training.timing2_bounds[channel][slot][rank][lane][1] =
                tm2 + u16::from(upper_usable[lane]);
            training.timing2_offset[channel][slot][rank][lane] = tm2;
        }
    }

    /// coreboot `choose_training()`.
    #[allow(clippy::too_many_arguments)]
    fn choose_training(
        &self,
        channel: usize,
        slot: usize,
        rank: usize,
        lane: usize,
        timings: &[TimingBounds; 64],
        center_178: u8,
    ) -> u8 {
        let info = &*self.info;
        let mut span: u32 = 12;
        let mut central_weight: u32 = 20;
        let mut side_weight: u32 = 20;
        if info.silicon_revision == 1 && channel == 1 {
            central_weight = 5;
            side_weight = 20;
            if (info.populated_ranks_mask[1] ^ (info.populated_ranks_mask[1] >> 2)) & 1 != 0 {
                span = 18;
            }
        }
        if info.populated_ranks_mask[0] & 5 == 5 {
            central_weight = 20;
            side_weight = 20;
        }
        if info.clock_speed_index >= 2 && info.populated_ranks_mask[0] & 5 == 5 && slot == 1 {
            if info.silicon_revision == 1 {
                match channel {
                    0 if lane == 1 => {
                        central_weight = 10;
                        side_weight = 20;
                    }
                    1 if lane == 6 => {
                        side_weight = 5;
                        central_weight = 20;
                    }
                    _ => {}
                }
            }
            if info.silicon_revision == 0 && channel == 0 && lane == 0 {
                side_weight = 5;
                central_weight = 20;
            }
        }
        let mut sum = 0u32;
        let mut count = 0u32;
        for reg_178 in reg178_sweep(center_178, span as u8) {
            let Some(bounds) = bounds_at(timings, reg_178) else {
                continue;
            };
            let bound = bounds[channel][slot][rank][lane];
            if i32::from(bound.largest) - i32::from(bound.smallest) + 1 >= 5 {
                let weight = if reg_178 == center_178 {
                    central_weight
                } else {
                    side_weight
                };
                sum += weight * (u32::from(bound.largest) + u32::from(bound.smallest));
                count += weight;
            }
        }
        if count == 0 {
            die("couldn't discover DRAM timings (2)");
        }
        let mut result = (sum / (2 * count)) as u8;
        let center_bound = bounds_at(timings, center_178)
            .map_or(Bound::default(), |bounds| bounds[channel][slot][rank][lane]);
        let lower_margin = result.wrapping_sub(center_bound.smallest);
        let upper_margin = center_bound.largest.wrapping_sub(result);
        if upper_margin < 10 && lower_margin > 10 {
            result = result.wrapping_sub((lower_margin - 10).min(10 - upper_margin));
        }
        if upper_margin > 10 && lower_margin < 10 {
            result = result.wrapping_add((upper_margin - 10).min(10 - lower_margin));
        }
        result
    }

    /// coreboot `choose_reg178()`.
    fn choose_reg178(&mut self, timings: &[TimingBounds; 64]) -> u8 {
        const STANDARD_MIN_MARGIN: u16 = 5;
        let clock = usize::from(self.info.clock_speed_index);
        let (min, max, step) = (REG178_MIN[clock], REG178_MAX[clock], REG178_STEP[clock]);
        let mut margin = [0u16; 64];
        let mut count: u32 = 0;
        let mut sum: u32 = 0;
        let ranks = populated_ranks_backwards(self.info).collect::<heapless::Vec<_, 8>>();
        let mut reg178 = min;
        while reg178 < max {
            margin[usize::from(reg178)] = 0xffff;
            for &(channel, slot, rank) in &ranks {
                for lane in 0..8 {
                    let bound = timings[usize::from(reg178)][channel][slot][rank][lane];
                    let current = (i32::from(bound.largest) - i32::from(bound.smallest) + 1) as u16;
                    if current < margin[usize::from(reg178)] {
                        margin[usize::from(reg178)] = current;
                    }
                }
            }
            if margin[usize::from(reg178)] >= STANDARD_MIN_MARGIN {
                let weight = u32::from(margin[usize::from(reg178)] - STANDARD_MIN_MARGIN);
                sum += weight * u32::from(reg178);
                count += weight;
            }
            reg178 += step;
        }
        if count == 0 {
            die("couldn't discover DRAM timings (3)");
        }
        for threshold in (5..=30u16).rev() {
            let mut usable_length = 0u32;
            let mut smallest_found = false;
            let mut reg178 = min;
            while reg178 < max {
                if margin[usize::from(reg178)] >= threshold {
                    usable_length += u32::from(step);
                    self.info.training.reg178_largest = reg178.wrapping_sub(2 * step);
                    if !smallest_found {
                        smallest_found = true;
                        self.info.training.reg178_smallest = reg178 + step;
                    }
                }
                reg178 += step;
            }
            if usable_length >= 0x21 {
                break;
            }
        }
        (sum / count) as u8
    }

    /// coreboot `check_cached_sanity()`: the cached delays still fit this
    /// boot's estimates.
    fn check_cached_sanity(&self) -> bool {
        let Some(cached) = self.info.cached_training.as_ref() else {
            return false;
        };
        for channel in 0..NUM_CHANNELS {
            for slot in 0..NUM_SLOTS {
                for rank in 0..NUM_RANKS {
                    for lane in 0..8 + usize::from(self.info.use_ecc) {
                        let cached_value = cached.lane_timings[1][channel][slot][rank][lane];
                        if (0x18..=0x1e7).contains(&cached_value) {
                            let estimate =
                                self.info.training.lane_timings[1][channel][slot][rank][lane];
                            if estimate < cached_value - 24 || estimate > cached_value + 24 {
                                return false;
                            }
                        }
                    }
                }
            }
        }
        true
    }

    /// coreboot `try_cached_training()`: replay the cached training if the
    /// edges it found still pass and fail where they did.
    fn try_cached_training(&mut self) -> bool {
        if !self.check_cached_sanity() {
            return false;
        }
        let cached = self.info.cached_training.expect("checked above");
        self.info.training.reg178_center = cached.reg178_center;
        self.info.training.reg178_smallest = cached.reg178_smallest;
        self.info.training.reg178_largest = cached.reg178_largest;
        self.info.training.timing_bounds = cached.timing_bounds;
        self.info.training.timing_offset = cached.timing_offset;

        self.write_1d0(2, 0x142, 3, 1);
        let saved_243 = [self.mch.read8(0x243), self.mch.read8(0x643)];
        self.mch.write8(0x243, saved_243[0] | 2);
        self.mch.write8(0x643, saved_243[1] | 2);
        self.set_ecc(false);
        northbridge().write16(0xc8, 3);
        if self.read_1d0(0x10b, 6) & 1 != 0 {
            self.set_10b(0);
        }
        let ranks = populated_ranks_backwards(self.info).collect::<heapless::Vec<_, 8>>();
        let mut flip = true;
        let mut ok = true;
        'outer: for tm in 0..2 {
            self.set_178(if tm != 0 {
                cached.reg178_largest
            } else {
                cached.reg178_smallest
            });
            // i: smallest (0) or largest (1) bound; j: on the bound (0, must
            // pass) or just outside it (1, must fail).
            for (totalrank, &(channel, slot, rank)) in ranks.iter().enumerate() {
                for i in 0..2 {
                    for lane in 0..8 {
                        self.write_500(
                            channel,
                            u32::from(cached.timing2_bounds[channel][slot][rank][lane][i]),
                            get_timing_register_addr(lane, 3, slot, rank),
                            9,
                            1,
                        );
                        if i == 0 {
                            self.write_500(
                                channel,
                                u32::from(cached.timing2_offset[channel][slot][rank][lane]),
                                get_timing_register_addr(lane, 2, slot, rank),
                                9,
                                1,
                            );
                        }
                        let bound = cached.timing_bounds[tm][channel][slot][rank][lane];
                        let edge = u32::from(if i != 0 {
                            bound.largest
                        } else {
                            bound.smallest
                        });
                        self.write_500(
                            channel,
                            edge,
                            get_timing_register_addr(lane, 0, slot, rank),
                            9,
                            1,
                        );
                        self.write_500(
                            channel,
                            (u32::from(cached.timing_offset[channel][slot][rank][lane]) + edge)
                                .wrapping_sub(64),
                            get_timing_register_addr(lane, 1, slot, rank),
                            9,
                            1,
                        );
                    }
                    for j in 0..2 {
                        let outside = u8::from(j == 1) + 4;
                        let reg1b3 = if j == i {
                            outside
                        } else {
                            outside.wrapping_neg() & 0x3f
                        };
                        self.write_1d0(u32::from(reg1b3), 0x1bb, 6, 1);
                        self.write_1d0(u32::from(reg1b3), 0x1b3, 6, 1);
                        self.write_1d0(u32::from(reg1b3), 0x1a3, 6, 1);
                        flip = !flip;
                        write_testing(totalrank as u32, flip);
                        let failmask = check_testing(totalrank as u32, flip);
                        let expected = if j == 0 { 0x00 } else { 0xff };
                        if failmask != expected {
                            ok = false;
                            break 'outer;
                        }
                    }
                }
            }
        }

        if ok {
            self.set_178(cached.reg178_center);
            if self.info.use_ecc != 0 {
                self.set_ecc(true);
            }
            self.write_training_data(&cached);
            self.write_1d0(0, 322, 3, 1);
            self.info.training = cached;
        } else {
            for (channel, slot, rank) in populated_ranks(self.info).collect::<heapless::Vec<_, 8>>()
            {
                for tm in 1..4 {
                    self.write_500_timings_type(channel, slot, rank, tm);
                }
            }
        }
        self.write_1d0(0, 0x1bb, 6, 1);
        self.write_1d0(0, 0x1b3, 6, 1);
        self.write_1d0(0, 0x1a3, 6, 1);
        self.mch.write8(0x243, saved_243[0]);
        self.mch.write8(0x643, saved_243[1]);
        ok
    }

    /// coreboot `do_ram_training()`.
    fn do_ram_training(&mut self, timings: &mut [TimingBounds; 64]) {
        self.write_1d0(2, 0x142, 3, 1);
        let saved_243 = [self.mch.read8(0x243), self.mch.read8(0x643)];
        self.mch.write8(0x243, saved_243[0] | 2);
        self.mch.write8(0x643, saved_243[1] | 2);
        let clock = usize::from(self.info.clock_speed_index);
        let niter = match clock {
            0 => 5,
            1 => 10,
            _ => 19,
        };
        self.set_ecc(false);

        let ranks = populated_ranks_backwards(self.info).collect::<heapless::Vec<_, 8>>();
        for (totalrank, &(channel, slot, rank)) in ranks.iter().enumerate() {
            self.write_500_timings_type(channel, slot, rank, 0);
            write_testing(totalrank as u32, false);
            for i in 0..niter {
                write_testing_type2(totalrank as u32, 2, i, false);
                write_testing_type2(totalrank as u32, 3, i, true);
            }
            northbridge().write8(0xc0, 0x01);
        }

        let (min, max, step) = (REG178_MIN[clock], REG178_MAX[clock], REG178_STEP[clock]);
        for bounds in &mut timings[usize::from(min)..usize::from(max)] {
            *bounds = Default::default();
        }
        let mut reg_178 = min;
        while reg_178 < max {
            self.set_178(reg_178);
            let mut totalrank = 0;
            for channel in (0..NUM_CHANNELS).rev() {
                for slot in 0..NUM_SLOTS {
                    for rank in 0..NUM_RANKS {
                        timings[usize::from(reg_178)][channel][slot][rank] = Default::default();
                        if self.info.populated(channel, slot, rank) {
                            self.train_ram_at_178(
                                channel, slot, rank, totalrank, reg_178, true, niter, timings,
                            );
                            totalrank += 1;
                        }
                    }
                }
            }
            reg_178 += step;
        }

        let reg178_center = self.choose_reg178(timings);
        let smallest = usize::from(self.info.training.reg178_smallest) % 64;
        let largest = usize::from(self.info.training.reg178_largest) % 64;
        for &(channel, slot, rank) in &ranks {
            for lane in 0..8 {
                let training = &mut self.info.training;
                training.timing_bounds[0][channel][slot][rank][lane] =
                    timings[smallest][channel][slot][rank][lane];
                training.timing_bounds[1][channel][slot][rank][lane] =
                    timings[largest][channel][slot][rank][lane];
                training.timing_offset[channel][slot][rank][lane] = training.lane_timings[1]
                    [channel][slot][rank][lane]
                    .wrapping_sub(training.lane_timings[0][channel][slot][rank][lane])
                    .wrapping_add(64);
            }
        }

        if self.info.silicon_revision == 1
            && (self.info.populated_ranks_mask[1] ^ (self.info.populated_ranks_mask[1] >> 2)) & 1
                != 0
        {
            let mut ranks_after_channel1 = 0;
            for reg in reg178_sweep(reg178_center, 18) {
                let mut totalrank = 0;
                self.set_178(reg);
                for slot in 0..NUM_SLOTS {
                    for rank in 0..NUM_RANKS {
                        if self.info.populated(1, slot, rank) {
                            self.train_ram_at_178(
                                1, slot, rank, totalrank, reg, false, niter, timings,
                            );
                            totalrank += 1;
                        }
                    }
                }
                ranks_after_channel1 = totalrank;
            }
            for reg in reg178_sweep(reg178_center, 12) {
                let mut totalrank = ranks_after_channel1;
                self.set_178(reg);
                for slot in 0..NUM_SLOTS {
                    for rank in 0..NUM_RANKS {
                        if self.info.populated(0, slot, rank) {
                            self.train_ram_at_178(
                                0, slot, rank, totalrank, reg, false, niter, timings,
                            );
                            totalrank += 1;
                        }
                    }
                }
            }
        } else {
            for reg in reg178_sweep(reg178_center, 12) {
                self.set_178(reg);
                for (totalrank, &(channel, slot, rank)) in ranks.iter().enumerate() {
                    self.train_ram_at_178(
                        channel,
                        slot,
                        rank,
                        totalrank as u32,
                        reg,
                        false,
                        niter,
                        timings,
                    );
                }
            }
        }

        self.set_178(reg178_center);
        for &(channel, slot, rank) in &ranks {
            for lane in 0..8 {
                let tm0 = self.choose_training(channel, slot, rank, lane, timings, reg178_center);
                self.write_timing01(channel, slot, rank, lane, u32::from(tm0));
            }
        }

        for (totalrank, &(channel, slot, rank)) in ranks.iter().enumerate() {
            self.try_timing_offsets(channel, slot, rank, totalrank as u32);
        }
        self.mch.write8(0x243, saved_243[0]);
        self.mch.write8(0x643, saved_243[1]);
        self.write_1d0(0, 0x142, 3, 1);
        self.info.training.reg178_center = reg178_center;
    }

    /// coreboot `ram_training()`.
    pub(super) fn ram_training(
        &mut self,
        timings: &mut [TimingBounds; 64],
    ) -> Result<(), ServiceError> {
        let saved_fc4 = self.mch.read16(0xfc4);
        self.mch.write16(0xfc4, 0xffff);
        if self.info.revision >= 8 {
            self.read_4090();
        }
        if self.try_cached_training() {
            fstart_log::info!("ironlake: cached training verified");
        } else {
            fstart_log::info!("ironlake: training");
            self.do_ram_training(timings);
        }
        if (self.info.silicon_revision == 2 || self.info.silicon_revision == 3)
            && self.info.clock_speed_index < 2
        {
            self.set_10b(1);
        }
        self.mch.write16(0xfc4, saved_fc4);
        Ok(())
    }

    /// coreboot `get_max_timing()`.
    pub(super) fn get_max_timing(&mut self, channel: usize) -> u16 {
        if self.mch.read8(0x2ca8) >> 2 < 1 {
            return 384;
        }
        if self.info.revision < 8 {
            return 256;
        }
        let mut ret = 0;
        for slot in 0..NUM_SLOTS {
            for rank in 0..NUM_RANKS {
                if self.info.populated(channel, slot, rank) {
                    for lane in 0..8 + usize::from(self.info.use_ecc) {
                        ret = ret.max(self.read_500(
                            channel,
                            get_timing_register_addr(lane, 0, slot, rank),
                            9,
                        ));
                    }
                }
            }
        }
        ret
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_coreboot() {
        // Hand-evaluated against coreboot's C for a few points.
        assert_eq!(get_etalon2(false, 0), 0);
        assert_eq!(get_etalon2(true, 0), 0xffff_ffff);
        assert_eq!(get_etalon2(false, 480), 0x0101_0101);
        assert_eq!(get_seed2(0, false), 0x5555_5555);
        assert_eq!(get_seed2(10, true), !0x2e55_5a55);
        assert_eq!(make_shift(0, 0, 0), 0);
        assert_eq!(make_shift(0, 10, 3), 0x1f);
    }

    #[test]
    fn fsm_finds_a_window() {
        let mut state = [State::BeforeUsable; 8];
        let mut count = [0u16; 8];
        let (mut low, mut high) = ([0u8; 8], [0u8; 8]);
        for val in 0..20u8 {
            let fail = if (3..15).contains(&val) { 0 } else { 0xff };
            do_fsm(
                &mut state, &mut count, fail, 5, 47, &mut low, &mut high, val,
            );
        }
        assert!(validate_state(&state));
        assert_eq!((low[0], high[0]), (3, 14));
    }
}
