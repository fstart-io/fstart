//! QuickPath (uncore ↔ PCH/DMI and memory controller clock crossing)
//! initialization: coreboot `northbridge/intel/ironlake/quickpath.c`.
//!
//! The clock-crossing registers hold ratios of the FSB, DDR and fixed
//! clocks; the arithmetic keeps coreboot's C integer conversions (`u8`
//! truncation, signed `char` casts) because the register images depend
//! on them.

use super::*;

/// coreboot `struct stru1`.
#[derive(Default, Clone, Copy)]
struct Ratios {
    freqs_reversed: u8,
    freq_diff_reduced: u8,
    freq_min_reduced: u8,
    divisor_f4_to_fmax: u8,
    divisor_f3_to_fmax: u8,
    freq4_to_max_remainder: u8,
    freq3_to_2_remainder: u8,
    freq3_to_2_remaindera: u8,
    freq4_to_2_remainder: u8,
    divisor_f3_to_f1: i32,
    divisor_f4_to_f2: i32,
    common_time_unit_ps: i32,
    freq_max_reduced: i32,
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn lcm(a: u32, b: u32) -> u32 {
    (a * b) / gcd(a, b)
}

/// C `DIV_ROUND_UP` on `int`s.
fn div_roundup(a: i32, b: i32) -> i32 {
    (a + b - 1) / b
}

/// C `(char)x`: truncate to a signed byte.
fn as_char(value: i32) -> i32 {
    i32::from(value as i8)
}

/// coreboot `compute_frequence_ratios()`.
fn compute_frequency_ratios(
    freq1: u16,
    freq2: u16,
    num_cycles_2: i32,
    num_cycles_1: i32,
    round_it: bool,
    add_freqs: bool,
) -> Ratios {
    let mut result = Ratios::default();
    let g = gcd(u32::from(freq1), u32::from(freq2)) as i32;
    let freq1_reduced = i32::from(freq1) / g;
    let freq2_reduced = i32::from(freq2) / g;
    let freq_min_reduced = freq1_reduced.min(freq2_reduced);
    let freq_max_reduced = freq1_reduced.max(freq2_reduced);

    let common_time_unit_ps = div_roundup(900_000, lcm(u32::from(freq1), u32::from(freq2)) as i32);
    let mut freq3 = div_roundup(num_cycles_2, common_time_unit_ps) - 1;
    let mut freq4 = div_roundup(num_cycles_1, common_time_unit_ps) - 1;
    if add_freqs {
        freq3 += freq2_reduced;
        freq4 += freq1_reduced;
    }

    if round_it {
        result.freq3_to_2_remainder = 0;
        result.freq3_to_2_remaindera = 0;
        result.freq4_to_max_remainder = 0;
        result.divisor_f4_to_f2 = 0;
        result.divisor_f3_to_f1 = 0;
    } else if freq2_reduced < freq1_reduced {
        let remainder = (freq3 % freq1_reduced - freq1_reduced + 1) as u8;
        result.freq3_to_2_remainder = remainder;
        result.freq3_to_2_remaindera = remainder;
        result.freq4_to_max_remainder = (-(freq4 % freq1_reduced)) as u8;
        result.divisor_f3_to_f1 = freq3 / freq1_reduced;
        result.divisor_f4_to_f2 = (freq4 - (freq1_reduced - freq2_reduced)) / freq2_reduced;
        let diff = freq1_reduced - freq2_reduced;
        result.freq4_to_2_remainder =
            (-as_char(diff + (i32::from(freq4 as u8) - diff) % i32::from(freq2_reduced as u8)))
                as u8;
    } else {
        if freq2_reduced > freq1_reduced {
            result.freq4_to_max_remainder = (freq4 % freq2_reduced - freq2_reduced + 1) as u8;
            result.freq4_to_2_remainder = (freq4 % freq_max_reduced - freq_max_reduced + 1) as u8;
        } else {
            result.freq4_to_max_remainder = (-(freq4 % freq2_reduced)) as u8;
            result.freq4_to_2_remainder = (-as_char(freq4 % freq_max_reduced)) as u8;
        }
        result.divisor_f4_to_f2 = freq4 / freq2_reduced;
        result.divisor_f3_to_f1 = (freq3 - (freq2_reduced - freq1_reduced)) / freq1_reduced;
        result.freq3_to_2_remainder = (-(freq3 % freq2_reduced)) as u8;
        let diff = freq_max_reduced - freq_min_reduced;
        result.freq3_to_2_remaindera = (-as_char(diff + (freq3 - diff) % freq1_reduced)) as u8;
    }
    result.divisor_f3_to_fmax = (freq3 / freq_max_reduced) as u8;
    result.divisor_f4_to_fmax = (freq4 / freq_max_reduced) as u8;
    if round_it {
        if freq2_reduced > freq1_reduced && freq3 % freq_max_reduced != 0 {
            result.divisor_f3_to_fmax = result.divisor_f3_to_fmax.wrapping_add(1);
        }
        if freq2_reduced < freq1_reduced && freq4 % freq_max_reduced != 0 {
            result.divisor_f4_to_fmax = result.divisor_f4_to_fmax.wrapping_add(1);
        }
    }
    result.freqs_reversed = u8::from(freq2_reduced < freq1_reduced);
    result.freq_diff_reduced = (freq_max_reduced - freq_min_reduced) as u8;
    result.freq_min_reduced = freq_min_reduced as u8;
    result.common_time_unit_ps = common_time_unit_ps;
    result.freq_max_reduced = freq_max_reduced;
    result
}

impl Raminit<'_> {
    /// coreboot `compute_274265()`.
    fn compute_274265(&mut self) {
        let halfcycle = self.info.halfcycle_ps() as i32;
        let fsbcycle = self.info.fsbcycle_ps() as i32;
        let delay_a_ps = 4 * halfcycle + 6 * fsbcycle;
        self.info.training.reg2ca9_bit0 = 0;
        for channel in 0..NUM_CHANNELS {
            let cycletime_ps = (900_000
                / lcm(
                    2 * u32::from(self.info.fsb_frequency),
                    self.info.frequency_11(),
                )) as i32;
            let max_timing = u32::from(self.get_max_timing(channel));
            let info = &*self.info;
            let mut delay_d_ps = ((info.halfcycle_ps() * max_timing) >> 6) as i32
                - i32::from(info.some_delay_3_ps_rounded)
                + 200;
            if !((info.silicon_revision == 0 || info.silicon_revision == 1) && info.revision >= 8) {
                delay_d_ps += halfcycle * 2;
            }
            delay_d_ps += halfcycle
                * (i32::from(info.revision_flag_1 == 0)
                    + i32::from(info.some_delay_2_halfcycles_ceil)
                    + 2 * i32::from(info.some_delay_1_cycle_floor)
                    + i32::from(info.clock_speed_index)
                    + 2 * i32::from(info.cas_latency)
                    - 7
                    + 11);
            delay_d_ps += if info.revision >= 8 { 2758 } else { 4428 };

            self.mch.clrsetbits32(0x140, 7 << 24, 2 << 24);
            self.mch.clrsetbits32(0x138, 7 << 24, 2 << 24);
            if self.mch.read8(0x144) & 0x1f > 0x13 {
                delay_d_ps += 650;
            }
            let delay_c_ps = delay_d_ps + 1800;
            let delay_e_ps = if delay_c_ps <= delay_a_ps {
                0
            } else {
                cycletime_ps * div_roundup(delay_c_ps - delay_a_ps, cycletime_ps)
            };
            let delay_e_over_cycle_ps = delay_e_ps % (2 * halfcycle);
            let mut delay_e_cycles = delay_e_ps / (2 * halfcycle);
            let delay_f_cycles = div_roundup(2500 - delay_e_over_cycle_ps, 2 * halfcycle);
            let info = &mut *self.info;
            if delay_f_cycles > delay_e_cycles {
                info.delay46_ps[channel] = delay_e_ps as u32;
                delay_e_cycles = 0;
            } else {
                info.delay46_ps[channel] =
                    (delay_e_over_cycle_ps + 2 * halfcycle * delay_f_cycles) as u32;
                delay_e_cycles -= delay_f_cycles;
            }
            if info.delay46_ps[channel] < 2500 {
                info.delay46_ps[channel] = 2500;
                info.training.reg2ca9_bit0 = 1;
            }
            let mut delay_b_ps = halfcycle + delay_c_ps;
            if delay_b_ps <= delay_a_ps {
                delay_b_ps = 0;
            } else {
                delay_b_ps -= delay_a_ps;
            }
            info.delay54_ps[channel] = (cycletime_ps * div_roundup(delay_b_ps, cycletime_ps)
                - 2 * halfcycle * delay_e_cycles) as u32;
            if info.delay54_ps[channel] < 2500 {
                info.delay54_ps[channel] = 2500;
            }
            info.training.reg274265[channel][0] = delay_e_cycles as u8;
            info.training.reg274265[channel][1] = if delay_d_ps + 7 * halfcycle <= 24 * halfcycle {
                0
            } else {
                (div_roundup(delay_d_ps + 7 * halfcycle, 4 * halfcycle) - 6) as u8
            };
            info.training.reg274265[channel][2] =
                (div_roundup(delay_c_ps + 3 * fsbcycle, 4 * halfcycle) + 1) as u8;
        }
    }

    /// coreboot `program_274265()`.
    fn program_274265(&self, training: &RamTraining) {
        for channel in 0..NUM_CHANNELS {
            let base = (channel as u32) << 10;
            self.mch.write32(
                base + 0x274,
                (u32::from(training.reg274265[channel][0]) << 16)
                    | u32::from(training.reg274265[channel][1]),
            );
            self.mch
                .write16(base + 0x265, u16::from(training.reg274265[channel][2]) << 8);
        }
        if training.reg2ca9_bit0 != 0 {
            self.mch.setbits8(0x2ca9, 1 << 0);
        } else {
            self.mch.clrbits8(0x2ca9, 1 << 0);
        }
    }

    /// coreboot `set_2d5x_reg()`.
    #[allow(clippy::too_many_arguments)]
    fn set_2d5x_reg(
        &self,
        reg: u32,
        freq1: u16,
        freq2: u16,
        num_cycles_2: i32,
        num_cycles_1: i32,
        num_cycles_3: i32,
        num_cycles_4: i32,
        reverse: bool,
    ) {
        let vv = compute_frequency_ratios(freq1, freq2, num_cycles_2, num_cycles_1, false, true);
        let unit = vv.common_time_unit_ps;
        let multiplier = as_char(
            div_roundup(
                (div_roundup(num_cycles_2, unit) + div_roundup(num_cycles_3, unit))
                    .max(div_roundup(num_cycles_1, unit) + div_roundup(num_cycles_4, unit))
                    + i32::from(vv.freq_min_reduced)
                    - 1,
                vv.freq_max_reduced,
            ) - 1,
        );
        let min = i32::from(vv.freq_min_reduced);
        let y = u32::from(((vv.freq_max_reduced - min) + vv.freq_max_reduced * multiplier) as u8)
            | (u32::from(vv.freqs_reversed) << 8)
            | (u32::from((min * multiplier) as u8) << 16)
            | (u32::from((min * multiplier) as u8) << 24);
        let x = (i32::from(vv.freq3_to_2_remaindera)
            | (i32::from(vv.freq4_to_2_remainder) << 8)
            | (vv.divisor_f3_to_f1 << 16)
            | (vv.divisor_f4_to_f2 << 20)
            | (min << 24)) as u32;
        if reverse {
            self.mch.write32(reg, y);
            self.mch.write32(reg + 4, x);
        } else {
            self.mch.write32(reg + 4, y);
            self.mch.write32(reg, x);
        }
    }

    /// coreboot `set_6d_reg()`.
    fn set_6d_reg(&self, reg: u32, freq1: u16, freq2: u16, cycles: [i32; 4]) {
        let ratios2 = compute_frequency_ratios(freq1, freq2, cycles[0], cycles[1], false, true);
        let ratios1 = compute_frequency_ratios(freq1, freq2, cycles[2], cycles[3], false, true);
        self.mch.write32(
            reg,
            u32::from(ratios1.freq4_to_max_remainder)
                | (u32::from(ratios2.freq4_to_max_remainder) << 8)
                | (u32::from(ratios1.divisor_f4_to_fmax) << 16)
                | (u32::from(ratios2.divisor_f4_to_fmax) << 20),
        );
    }

    /// coreboot `set_2dx8_reg()`.
    #[allow(clippy::too_many_arguments)]
    fn set_2dx8_reg(
        &self,
        reg: u32,
        mode: u8,
        freq1: u16,
        freq2: u16,
        num_cycles_2: i32,
        num_cycles_1: i32,
        round_it: bool,
        add_freqs: bool,
    ) {
        let r = compute_frequency_ratios(
            freq1,
            freq2,
            num_cycles_2,
            num_cycles_1,
            round_it,
            add_freqs,
        );
        let b = |v: u8| u32::from(v);
        match mode {
            0 => {
                self.mch
                    .write32(reg + 4, b(r.freq_diff_reduced) | (b(r.freqs_reversed) << 8));
                self.mch.write32(
                    reg,
                    b(r.freq3_to_2_remainder)
                        | (b(r.freq4_to_max_remainder) << 8)
                        | (b(r.divisor_f3_to_fmax) << 16)
                        | (b(r.divisor_f4_to_fmax) << 20)
                        | (b(r.freq_min_reduced) << 24),
                );
            }
            1 => self.mch.write32(
                reg,
                b(r.freq3_to_2_remainder) | (b(r.divisor_f3_to_fmax) << 16),
            ),
            2 => self.mch.write32(
                reg,
                b(r.freq3_to_2_remainder)
                    | (b(r.freq4_to_max_remainder) << 8)
                    | (b(r.divisor_f3_to_fmax) << 16)
                    | (b(r.divisor_f4_to_fmax) << 20),
            ),
            4 => self.mch.write32(
                reg,
                (b(r.divisor_f3_to_fmax) << 4)
                    | (b(r.divisor_f4_to_fmax) << 8)
                    | (b(r.freqs_reversed) << 12)
                    | (b(r.freq_min_reduced) << 16)
                    | (b(r.freq_diff_reduced) << 24),
            ),
            _ => {}
        }
    }

    /// coreboot `set_2dxx_series()`.
    fn set_2dxx_series(&self, s3resume: bool) {
        let fsb = self.info.fsb_frequency;
        let f11 = self.info.frequency_11() as u16;
        let half = f11 / 2;
        self.set_2dx8_reg(0x2d00, 0, 0x78, half, 1359, 1005, false, true);
        self.set_2dx8_reg(0x2d08, 0, 0x78, 0x78, 3273, 5033, true, true);
        self.set_2dx8_reg(0x2d10, 0, 0x78, fsb, 1475, 1131, false, true);
        self.set_2dx8_reg(0x2d18, 0, 2 * fsb, f11, 1231, 1524, false, true);
        self.set_2dx8_reg(0x2d20, 0, 2 * fsb, half, 1278, 2008, false, true);
        self.set_2dx8_reg(0x2d28, 0, fsb, f11, 1167, 1539, false, true);
        self.set_2dx8_reg(0x2d30, 0, fsb, half, 1403, 1318, false, true);
        self.set_2dx8_reg(0x2d38, 0, fsb, 0x78, 3460, 5363, true, true);
        self.set_2dx8_reg(0x2d40, 0, fsb, 0x3c, 2792, 5178, true, true);
        self.set_2dx8_reg(0x2d48, 0, 2 * fsb, 0x78, 2738, 4610, true, true);
        self.set_2dx8_reg(0x2d50, 0, fsb, 0x78, 2819, 5932, true, true);
        self.set_2dx8_reg(0x6d4, 1, fsb, half, 4000, 0, false, false);
        self.set_2dx8_reg(0x6d8, 2, fsb, half, 4000, 4000, false, false);

        match self.info.cached_training.filter(|_| s3resume) {
            Some(cached) => self.mch.write32(0x6dc, cached.reg_6dc),
            None => self.set_6d_reg(
                0x6dc,
                2 * fsb,
                f11,
                [
                    0,
                    self.info.delay46_ps[0] as i32,
                    0,
                    self.info.delay54_ps[0] as i32,
                ],
            ),
        }
        self.set_2dx8_reg(0x6e0, 1, 2 * fsb, f11, 2500, 0, false, false);
        self.set_2dx8_reg(0x6e4, 1, 2 * fsb, half, 3500, 0, false, false);
        match self.info.cached_training.filter(|_| s3resume) {
            Some(cached) => self.mch.write32(0x6e8, cached.reg_6e8),
            None => self.set_6d_reg(
                0x6e8,
                2 * fsb,
                f11,
                [
                    0,
                    self.info.delay46_ps[1] as i32,
                    0,
                    self.info.delay54_ps[1] as i32,
                ],
            ),
        }
        self.set_2d5x_reg(0x2d58, 0x78, 0x78, 864, 1195, 762, 786, false);
        self.set_2d5x_reg(0x2d60, 0x195, fsb, 1352, 725, 455, 470, false);
        self.set_2d5x_reg(0x2d68, 0x195, 0x3c, 2707, 5632, 3277, 2207, false);
        self.set_2d5x_reg(0x2d70, 0x195, half, 1276, 758, 454, 459, false);
        self.set_2d5x_reg(0x2d78, 0x195, 0x78, 1021, 799, 510, 513, false);
        self.set_2d5x_reg(0x2d80, fsb, 0xe1, 0, 2862, 2579, 2588, false);
        self.set_2d5x_reg(0x2d88, fsb, 0xe1, 0, 2690, 2405, 2405, false);
        self.set_2d5x_reg(0x2da0, 0x78, 0xe1, 0, 2560, 2264, 2251, false);
        self.set_2d5x_reg(0x2da8, 0x195, f11, 1060, 775, 484, 480, false);
        self.set_2d5x_reg(0x2db0, 0x195, 0x78, 4183, 6023, 2217, 2048, false);
        self.mch.write32(0x2dbc, (u32::from(half) - 1) | 0xe0_0000);
        self.mch
            .write32(0x2db8, ((u32::from(fsb) - 1) << 16) | 0x77);
    }

    /// coreboot `quickpath_configure_pll_ratio()`.
    fn quickpath_configure_pll_ratio(&self, x2ca8: u8) -> u16 {
        const QPI_PLL_STATUS: u16 = 0x50;
        const QPI_PLL_RATIO: u16 = 0x54;
        self.mch.setbits32(0x18b4, (1 << 21) | (1 << 16));
        self.mch.setbits32(0x1890, 1 << 25);
        self.mch.setbits32(0x18b4, 1 << 15);

        // The maximum supported PLL ratio, adjusted if invalid.
        let mut ratio = ((self.qpi_phy0().read32(QPI_PLL_STATUS) >> 24) & 0x7f) as u16;
        if ratio == 0 || ratio > 40 {
            ratio = 40;
        }
        if ratio == 16 {
            ratio = 12;
        }
        while ratio >= 12 {
            if u32::from(ratio) <= (u32::from(self.info.clock_speed_index) + 3) * 8 {
                break;
            }
            ratio -= 2;
        }
        self.qpi_phy0().write8(QPI_PLL_RATIO, ratio as u8);

        let csipll0 = self.mch.read16(0x2c10);
        self.mch
            .write16(0x2c10, (u16::from(ratio > 26) << 11) | (1 << 10) | ratio);
        if csipll0 != self.mch.read16(0x2c10) && x2ca8 == 0 {
            self.mch.setbits8(0x2ca8, 1 << 0);
        }
        self.mch.setbits16(0x2c12, 1 << 8);
        ratio
    }

    /// coreboot `early_quickpath_init()`.
    pub(super) fn early_quickpath_init(&mut self, x2ca8: u8) {
        const QPI_QPILCL: u16 = 0x48;
        const QPI_DEF_RMT_VN_CREDITS: u16 = 0x58;
        const QPI_PHY_CONTROL: u16 = 0x6c;
        const QPI_PHY_PRIM_TIMEOUT: u16 = 0x94;
        const QPI_PHY_PWR_MGMT: u16 = 0xd0;
        const QPI_PHY_EP_SELECT: u16 = 0xe0;
        const QPI_PHY_EP_MCTR: u16 = 0xf4;
        let mch = self.mch;
        let phy = self.qpi_phy0();

        // DDR MPLL first.
        if x2ca8 == 0 {
            mch.clrsetbits8(
                0x164,
                0x26,
                if self.info.clock_speed_index == 0 {
                    0x24
                } else {
                    0x26
                },
            );
            // DDR MPLL feedback divider ratio.
            mch.write16(0x2c20, (self.info.clock_speed_index + 3) * 4);
        }

        let ratio = self.quickpath_configure_pll_ratio(x2ca8);

        mch.clrsetbits32(0x1804, 0x3, 0x0840_0080);
        phy.modify32(QPI_PHY_CONTROL, 0xffff_fffc, 0x40_0080);

        let x1c04 = mch.read32(0x1c04) & 0xc0_1080;
        let x1804 = mch.read32(0x1804) & 0xc0_1080;
        if x1c04 != x1804 && x2ca8 == 0 {
            mch.setbits8(0x2ca8, 1 << 0);
        }

        let mut reg32 = 0x300_0000;
        if self.info.revision >= 0x18 && ratio <= 12 {
            // TDP limit in 1/8 W.
            // SAFETY: model 2065x implements MSR_TURBO_POWER_CURRENT_LIMIT.
            let limit =
                unsafe { fstart_arch::x86::msr::rdmsr(TURBO_POWER_CURRENT_LIMIT.index()) } as u32;
            if limit & 0x7fff <= 90 {
                reg32 = 0;
            }
        }
        mch.write32(0x18d8, 0x12_0000);
        mch.write32(0x18dc, reg32 | 0xa484a);

        let reg32 = if ratio > 20 { 8 } else { 16 };
        phy.write32(QPI_PHY_EP_SELECT, 0x0);
        phy.write32(QPI_PHY_EP_MCTR, 0x9404a | (reg32 << 7));

        mch.write32(0x18d8, 0x4_0000);
        mch.write32(0x18dc, 0xb00_0000);
        phy.write32(QPI_PHY_EP_SELECT, 0x6_0000);
        phy.write32(QPI_PHY_EP_MCTR, 0x0);
        mch.write32(0x18d8, 0x18_0000);
        mch.write32(0x18dc, 0xc000_0142);
        phy.write32(QPI_PHY_EP_SELECT, 0x2_0000);
        phy.write32(QPI_PHY_EP_MCTR, 0x142);
        mch.write32(0x18d8, 0x1e_0000);

        let x18dc = mch.read32(0x18dc);
        mch.write32(0x18dc, if ratio < 18 { 2 } else { 3 });
        if x18dc != mch.read32(0x18dc) && x2ca8 == 0 {
            mch.setbits8(0x2ca8, 1 << 0);
        }

        let reg8: u32 = if ratio > 20 { 10 } else { 9 };
        mch.write32(0x188c, 0x20_bc00 | reg8);
        phy.write32(QPI_PHY_PWR_MGMT, 0x40b_0c00 | reg8);

        let reg8: u32 = if ratio <= 14 {
            0x33
        } else if ratio <= 22 {
            0x42
        } else {
            0x51
        };
        self.info.fsb_frequency = ratio * 15;
        mch.write32(0x1a10, (reg8 << 24) | u32::from(self.info.fsb_frequency));

        if self.info.silicon_revision == 2 || self.info.silicon_revision == 3 {
            mch.setbits32(0x18b8, 0x200);
            mch.setbits32(0x1918, 0x300);
        }
        if self.info.revision > 0x17 {
            mch.setbits32(0x18b8, 0xc00);
        }

        let reg32 = (u32::from(ratio > 20) + 1) << 16;
        mch.clrsetbits32(0x182c, !0xfff0_f0ff, reg32 | 0x200);
        phy.modify32(QPI_PHY_PRIM_TIMEOUT, 0xfff0_f0ff, reg32 | 0x200);
        mch.clrbits32(0x1a1c, 7 << 28);
        mch.setbits32(0x1a70, 1 << 20);

        mch.clrbits32(0x18b4, 1 << 15);
        mch.clrsetbits32(0x1a68, 0x0014_3fc0, 0x14_3800);

        let x1e68 = mch.read32(0x1e68) & 0x14_3fc0;
        let x1a68 = mch.read32(0x1a68) & 0x14_3fc0;
        if x1e68 != x1a68 && x2ca8 == 0 {
            mch.setbits8(0x2ca8, 1 << 0);
        }

        let link = self.qpi_link0();
        link.modify32(QPI_QPILCL, 0xffff_ff3f, 0x14_0000);
        let credits = link.read32(QPI_DEF_RMT_VN_CREDITS);
        link.write32(QPI_DEF_RMT_VN_CREDITS, (credits & 0xfffe_4555) | 0x6_4555);
        if credits != link.read32(QPI_DEF_RMT_VN_CREDITS) && x2ca8 == 0 {
            mch.setbits8(0x2ca8, 1 << 0);
        }

        self.qpi_non_core().modify32(MIRROR_PORT_CTL, !3, 0x80 * 3);

        let af0 = mch.read32(0x1af0);
        mch.write32(0x1af0, (af0 & 0xfd_ffcf) | 0x1f02_0000);
        if af0 != mch.read32(0x1af0) && x2ca8 == 0 {
            mch.setbits8(0x2ca8, 1 << 0);
        }

        mch.clrbits32(0x1890, 1 << 25);
        mch.clrsetbits32(0x18b4, 0xf << 12, 0x6 << 12);
        mch.write32(0x18a4, 0x2222_2222);
        mch.write32(0x18a8, 0x2222_2222);
        mch.write32(0x18ac, 0x22222);
    }

    /// coreboot `late_quickpath_init()`.
    pub(super) fn late_quickpath_init(&mut self, s3resume: bool) {
        let deven = northbridge().read16(DEVEN);
        match self.info.cached_training.filter(|_| s3resume) {
            Some(cached) => self.program_274265(&cached),
            None => {
                self.compute_274265();
                let training = self.info.training;
                self.program_274265(&training);
            }
        }
        self.set_2dxx_series(s3resume);

        let mch = self.mch;
        if deven & 8 == 0 {
            mch.clrsetbits32(0x2cb0, !0, 0x40);
        }
        fstart_arch::x86::udelay(1000);

        if deven & 8 != 0 {
            mch.setbits32(0xff8, 3 << 11);
            mch.clrbits32(0x2cb0, !0);
            gma().read8(0x4c);
            gma().read8(0x4c);
            gma().read8(0x4e);
            mch.read8(0x1150);
            mch.read8(0x1151);
            mch.read8(0x1022);
            mch.read8(0x16d0);
            for (offset, value) in IGD_PM_TABLE_1300 {
                mch.write32(*offset, *value);
            }
            mch.write8(0x11cc, 0x4e);
            for (offset, value) in IGD_PM_TABLE_1110 {
                mch.write32(*offset, *value);
            }
            mch.write32(0x1190, 0x0900_080a);
            mch.write16(0x11c0, 0xc40b);
            mch.write16(0x11c2, 0x303);
            mch.write16(0x11c4, 0x301);
            mch.clrsetbits32(0x1190, !0, 0x8900_080a);
            mch.write32(0x11b8, 0x070c_3000);
            mch.write8(0x11ec, 0xa);
            mch.write16(0x1100, 0x800);
            mch.clrsetbits32(0x11bc, !0, 0x1e8_4800);
            mch.write16(0x11ca, 0xfa);
            mch.write32(0x11e4, 0x4e20);
            mch.write8(0x11bc, 0xf);
            mch.write16(0x11da, 0x19);
            mch.write16(0x11ba, 0x470c);
            for (offset, value) in IGD_PM_TABLE_1680 {
                mch.write32(*offset, *value);
            }
            mch.write32(0x1290, 0x500_0000);
        }

        for (offset, value) in UNCORE_PM_TABLE_124C {
            mch.write32(*offset, *value);
        }

        if deven & 8 != 0 {
            mch.write16(0x1214, 0x320);
            mch.write32(0x1600, 0x4000_0000);
            mch.clrsetbits32(0x11f4, !0, 1 << 28);
            mch.clrsetbits16(0x1230, !0, 1 << 15);
            for (index, value) in IGD_PM_TABLE_1400.iter().enumerate() {
                mch.write32(0x1400 + 4 * index as u32, *value);
            }
            mch.write16(0x1222, 0x220a);
            mch.write16(0x123c, 0x1fc0);
            mch.write16(0x1220, 0x1388);
        }
    }
}

/// `late_quickpath_init()` IGD power-management values, MCHBAR 0x1300..0x137c.
const IGD_PM_TABLE_1300: &[(u32, u32)] = &[
    (0x1300, 0x6060_6060),
    (0x1304, 0x6060_6060),
    (0x1308, 0x7879_7a7b),
    (0x130c, 0x7c7d_7e7f),
    (0x1310, 0x6060_6060),
    (0x1314, 0x6060_6060),
    (0x1318, 0x6060_6060),
    (0x131c, 0x6060_6060),
    (0x1320, 0x5051_5253),
    (0x1324, 0x5455_5657),
    (0x1328, 0x5859_5a5b),
    (0x132c, 0x5c5d_5e5f),
    (0x1330, 0x4041_4243),
    (0x1334, 0x4445_4647),
    (0x1338, 0x4849_4a4b),
    (0x133c, 0x4c4d_4e4f),
    (0x1340, 0x3031_3233),
    (0x1344, 0x3435_3637),
    (0x1348, 0x3839_3a3b),
    (0x134c, 0x3c3d_3e3f),
    (0x1350, 0x2021_2223),
    (0x1354, 0x2425_2627),
    (0x1358, 0x2829_2a2b),
    (0x135c, 0x2c2d_2e2f),
    (0x1360, 0x1011_1213),
    (0x1364, 0x1415_1617),
    (0x1368, 0x1819_1a1b),
    (0x136c, 0x1c1d_1e1f),
    (0x1370, 0x0001_0203),
    (0x1374, 0x0405_0607),
    (0x1378, 0x0809_0a0b),
    (0x137c, 0x0c0d_0e0f),
];

/// MCHBAR 0x1110..0x1140.
const IGD_PM_TABLE_1110: &[(u32, u32)] = &[
    (0x1110, 0x7397_0404),
    (0x1114, 0x7296_0404),
    (0x1118, 0x6f95_0404),
    (0x111c, 0x6d94_0404),
    (0x1120, 0x6a93_0404),
    (0x1124, 0x68a4_1404),
    (0x1128, 0x66a2_1404),
    (0x112c, 0x63a0_1404),
    (0x1130, 0x609e_1404),
    (0x1134, 0x5f9c_1404),
    (0x1138, 0x5c96_1404),
    (0x113c, 0x58a0_2404),
    (0x1140, 0x5494_2404),
];

/// MCHBAR 0x1680..0x169c.
const IGD_PM_TABLE_1680: &[(u32, u32)] = &[
    (0x1680, 0xe6ff_e4ff),
    (0x1684, 0xdeff_daff),
    (0x1688, 0xd4ff_d0ff),
    (0x168c, 0xccff_c6ff),
    (0x1690, 0xc0ff_beff),
    (0x1694, 0xb8ff_b0ff),
    (0x1698, 0xa8ff_0000),
    (0x169c, 0xc00),
];

/// MCHBAR 0x124c..0x1278, written whether or not the IGD is enabled.
const UNCORE_PM_TABLE_124C: &[(u32, u32)] = &[
    (0x124c, 0x1504_0d00),
    (0x1250, 0x7f_0000),
    (0x1254, 0x1e22_0004),
    (0x1258, 0x400_0004),
    (0x1278, 0x0),
    (0x125c, 0x0),
    (0x1260, 0x0),
    (0x1264, 0x0),
    (0x1268, 0x0),
    (0x126c, 0x0),
    (0x1270, 0x0),
    (0x1274, 0x0),
];

/// MCHBAR 0x1400..=0x1568, consecutive dwords.
const IGD_PM_TABLE_1400: [u32; 91] = [
    0x1304_0020,
    0x0e09_0120,
    0x0512_0220,
    0x0512_0330,
    0x0e09_0220,
    0x0101_0001,
    0x0111_0000,
    0x0902_0020,
    0x0d09_0220,
    0x0209_0220,
    0x0209_0330,
    0x0d09_0220,
    0x0101_0001,
    0x0111_0000,
    0x1104_0020,
    0x0403_0220,
    0x0106_0220,
    0x0106_0330,
    0x0403_0220,
    0x0101_0001,
    0x0111_0000,
    0x0401_0020,
    0x0b09_0220,
    0x0109_0220,
    0x0109_0330,
    0x0b09_0220,
    0x0101_0001,
    0x0111_0000,
    0x0f04_0020,
    0x0a09_0220,
    0x0112_0220,
    0x0112_0330,
    0x0a09_0220,
    0x0101_0001,
    0x0111_0000,
    0x0702_0020,
    0x0101_0220,
    0x0001_0210,
    0x0001_0320,
    0x0101_0220,
    0x0101_0001,
    0x0111_0000,
    0x0d04_0020,
    0x0809_0220,
    0x0111_1310,
    0x0111_1420,
    0x0809_0220,
    0x0101_0001,
    0x0111_0000,
    0x0301_0020,
    0x0709_0220,
    0x0108_1310,
    0x0108_1420,
    0x0709_0220,
    0x0101_0001,
    0x0111_0000,
    0x0b04_0020,
    0x0203_0220,
    0x0105_1310,
    0x0105_1420,
    0x0203_0220,
    0x0101_0001,
    0x0111_0000,
    0x0502_0020,
    0x0509_0220,
    0x0207_1310,
    0x0207_1420,
    0x0509_0220,
    0x0101_0001,
    0x0111_0000,
    0x0704_0120,
    0x0209_0220,
    0x070b_1210,
    0x070b_1310,
    0x0209_0220,
    0x0101_0001,
    0x0111_0000,
    0x0101_0110,
    0x0108_1310,
    0x0504_1200,
    0x0504_1310,
    0x0108_1310,
    0x0101_0001,
    0x0111_0000,
    0x0104_0120,
    0x0405_1210,
    0x0d05_1200,
    0x0d05_1200,
    0x0405_1210,
    0x0101_0001,
    0x0111_0000,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_1400_ends_at_1568() {
        assert_eq!(0x1400 + 4 * (IGD_PM_TABLE_1400.len() as u32 - 1), 0x1568);
    }

    #[test]
    fn ratios_match_hand_evaluated_coreboot() {
        // freq1 = freq2: no reduction, round_it with add_freqs.
        let r = compute_frequency_ratios(0x78, 0x78, 3273, 5033, true, true);
        assert_eq!(r.freq_min_reduced, 1);
        assert_eq!(r.freq_diff_reduced, 0);
        assert_eq!(r.freqs_reversed, 0);
        assert_eq!(r.common_time_unit_ps, 7500);
        assert_eq!(r.divisor_f3_to_fmax, 1);
        assert_eq!(r.divisor_f4_to_fmax, 1);
    }
}
