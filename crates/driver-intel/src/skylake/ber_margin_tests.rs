// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use crate::{
    skylake::{
        command_training::CommandPolicy,
        io_test::DqPattern,
        registers::testing::image,
        spd::{Dimm, testing::sodimm},
    },
    sunrise_point::pmc::{MemoryBootMode, PmcRegisters},
};

#[repr(C, align(65536))]
struct PmcImage([core::cell::UnsafeCell<u32>; 0x10000 / 4]);
impl PmcImage {
    fn new() -> std::boxed::Box<Self> {
        std::boxed::Box::new(Self(
            [const { core::cell::UnsafeCell::new(0) }; 0x10000 / 4],
        ))
    }
    fn registers(&self) -> &PmcRegisters {
        unsafe { PmcRegisters::at(self.0.as_ptr() as usize) }
    }
}

fn context(pmc: &PmcRegisters) -> CommandContext<'_> {
    CommandContext {
        pmc,
        mode: MemoryBootMode::Cold,
        policy: CommandPolicy {
            sdram_lanes: 8,
            ddp_shared_clock: false,
            ca_vref_config: 2,
        },
    }
}

fn seed(e: &Engine<'_>, s: &mut CalibrationState) {
    s.dq_pat = DqPattern::BasicVa as u8;
    for ch in 0..2 {
        let ranks = rank_mask(e.dimms, ch).unwrap();
        if ranks != 0 {
            e.program_rank_map(ch, ranks).unwrap();
        }
        for rank in 0..4 {
            e.registers.clock_channel[ch].rank_pi.set(
                replace_pi(e.registers.clock_channel[ch].rank_pi.get(), rank, 64) | 0xa0000000,
            );
            s.saved_mr.rank[ch][rank] = [0x130, 1, 0x8e0, 0, 0, 0x100, 0x80d];
            for byte in 0..9 {
                s.ddr4_pda_mr6[ch][rank][byte] = 13;
                s.training.ch[ch].rank[rank].rcven_dly[byte] = 256;
                s.training.ch[ch].rx_dqs_p[rank][byte] = 32;
                s.training.ch[ch].rx_dqs_n[rank][byte] = 30;
                e.registers.data_lane[byte][ch].rx_train[rank].set(0x84400000 | 300);
                s.margin_results[ch][rank][byte] = [40; 16];
            }
        }
    }
}

#[test]
fn ber_log8_bit_shift_swap_and_interpolate_vref_source_integer_units() {
    assert_eq!(
        (0..=8).map(ber_log8).collect::<std::vec::Vec<_>>(),
        [0, 1, 3, 5, 7, 8, 9, 9, 10]
    );
    assert_eq!(ber_log8(64), 20);
    assert_eq!(ber_log8(0x7fffff), 76);
    assert_eq!(ber_bit_shift(0x1234, -8), 0x12);
    assert_eq!(ber_bit_shift(1, 31), 0x80000000);
    assert_eq!(ber_bit_shift(1, 32), 0);
    assert_eq!(ber_bit_shift(1, -32), 0);
    assert_eq!(ber_bit_shift(1, i8::MIN), 0);
    assert_eq!(ber_bit_swap(0xffffffff, 0, (-8i8) as u8, 8), 0xffffff);
    let mut stats = [0; 4];
    assert_eq!(ber_interpolate_vref(10, 2, 5, 7, &mut stats), 126);
    assert_eq!(ber_interpolate_vref(10, 5, 9, 7, &mut stats), 115);
    assert_eq!(ber_interpolate_vref(10, 9, 12, 7, &mut stats), 107);
    assert_eq!(ber_interpolate_vref(10, 9, 8, 7, &mut stats), 110);
    assert_eq!(stats, [1, 1, 1, 1]);
}

#[test]
fn meminit_calc_max_rx_margin_host_rank_intersection_and_source_scaling() {
    let raw = sodimm();
    let mut d = Dimm::decode(&raw).unwrap();
    d.ranks = 2;
    d.rank_one_mirrored = false;
    let dimms = [[None, None], [None, Some(d)]];
    let image = image();
    let e = Engine {
        registers: image.registers(),
        dimms: &dimms,
    };
    let mut s = CalibrationState::default();
    seed(&e, &mut s);
    s.training.ch[1].rx_dqs_p[2][0] = 20;
    s.training.ch[1].rx_dqs_n[3][0] = 10;
    assert_eq!(
        e.calc_max_rx_margin(&s, MarginParameter::ReadTiming, 1, 0xff, 0, 0, 31),
        Ok(10)
    );
    s.training.ch[1].rd_vref[0] = -31;
    assert_eq!(
        e.calc_max_rx_margin(&s, MarginParameter::ReadVoltage, 1, 2, 0, 0, 63),
        Ok(2)
    );
    assert_eq!(
        e.calc_max_rx_margin(&s, MarginParameter::ReadVoltage, 1, 2, 0, 1, 63),
        Ok(62)
    );
    s.training.ch[1].rank[3].rcven_dly[0] = 505;
    assert_eq!(
        e.calc_max_rx_margin(
            &s,
            MarginParameter::ReceiveEnableExtended,
            1,
            0xff,
            0,
            1,
            63
        ),
        Ok(1)
    );
    assert_eq!(
        e.calc_max_rx_margin(&s, MarginParameter::ReadTiming, 1, 0, 0, 0, 31),
        Err(Error::InvalidRank)
    );
}

#[test]
fn meminit_calc_max_vref_margin_saved_rank_pda_and_ca_saturation_not_transient_hardware() {
    let raw = sodimm();
    let mut d = Dimm::decode(&raw).unwrap();
    d.ranks = 2;
    let dimms = [[None, None], [None, Some(d)]];
    let image = image();
    let pmc = PmcImage::new();
    let e = Engine {
        registers: image.registers(),
        dimms: &dimms,
    };
    let mut state = CalibrationState::default();
    seed(&e, &mut state);
    state.saved_mr.rank[1][2][6] = vref_code(-30, 0).unwrap();
    state.saved_mr.rank[1][3][6] = vref_code(34, 0).unwrap();
    state.ddr4_pda_mr6[1][2][4] = vref_code(35, 0).unwrap() as u8;
    let ctx = context(pmc.registers());
    let base = e.ber_baseline(&ctx).unwrap();
    assert_eq!(
        e.calc_max_vref_margin(
            &state,
            &base,
            1,
            12,
            0,
            MarginParameter::WriteVoltage,
            0,
            36,
            false,
            &ctx
        ),
        Ok(6)
    );
    assert_eq!(
        e.calc_max_vref_margin(
            &state,
            &base,
            1,
            12,
            0,
            MarginParameter::WriteVoltage,
            1,
            36,
            false,
            &ctx
        ),
        Ok(2)
    );
    assert_eq!(
        e.calc_max_vref_margin(
            &state,
            &base,
            1,
            4,
            4,
            MarginParameter::WriteVoltage,
            1,
            36,
            true,
            &ctx
        ),
        Ok(1)
    );
    e.registers.data_lane[0][0].vref_adjust.set(60 << 7);
    assert_eq!(
        e.calc_max_vref_margin(
            &state,
            &base,
            1,
            0,
            0,
            MarginParameter::CommandVoltage,
            1,
            31,
            false,
            &ctx
        ),
        Ok(31)
    );
    assert_eq!(ber_vref_saturation(-61, 0, 63), Ok(2));
    assert_eq!(ber_vref_saturation(62, 1, 63), Ok(1));
    assert_eq!(ber_vref_saturation(64, 1, 63), Err(Error::InvalidTiming));
    state.ddr4_tx_vref_range[1] = 1;
    assert_eq!(
        e.calc_max_vref_margin(
            &state,
            &base,
            1,
            4,
            0,
            MarginParameter::WriteVoltage,
            0,
            36,
            false,
            &ctx
        ),
        Err(Error::InvalidTiming)
    );
}

#[test]
fn meminit_calc_max_rx_margin_rank_specific_voltage_offsets_intersect_sparse_rank23() {
    let raw = sodimm();
    let mut d = Dimm::decode(&raw).unwrap();
    d.ranks = 2;
    let dimms = [[None, None], [None, Some(d)]];
    let image = image();
    let e = Engine {
        registers: image.registers(),
        dimms: &dimms,
    };
    let mut s = CalibrationState::default();
    s.training.ch[1].rx_vref_rank_offset[2][0] = 30;
    s.training.ch[1].rx_vref_rank_offset[3][0] = -30;
    assert_eq!(
        e.calc_max_rx_margin(&s, MarginParameter::ReadVoltage, 1, 2, 0, 1, 63),
        Ok(2)
    );
    assert_eq!(
        e.calc_max_rx_margin(&s, MarginParameter::ReadVoltage, 1, 3, 0, 0, 63),
        Ok(4)
    );
    assert_eq!(
        e.calc_max_rx_margin(&s, MarginParameter::ReadVoltage, 1, 0xff, 0, 0, 63),
        Ok(4)
    );
    assert_eq!(
        e.calc_max_rx_margin(&s, MarginParameter::ReadVoltage, 1, 0xff, 0, 1, 63),
        Ok(2)
    );
}
