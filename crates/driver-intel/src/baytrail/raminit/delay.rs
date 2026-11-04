//! DDRIO delay elements, shared by PHY init and training (coreboot
//! `ddrio_delay.c`).
//!
//! A delay is a linear value in PI steps, `2x * HALF_CLK + pi`, spread over a
//! 2x counter, a phase interpolator code and two clock-crossing selects.
//! Strobe `s` is byte lane `s % 2` of DQ module `s / 2`; CLKCTL elements are
//! per channel only.

use super::ddrio_regs::*;
use crate::baytrail::iosf::{DDRIO, Iosf, IosfReg};
use fstart_pci::ConfigRegionAccess;
use tock_registers::RegisterLongName;
use tock_registers::fields::Field;
use tock_registers::interfaces::{ReadWriteable, Readable};

pub const QTR_CLK: i32 = 32;
pub const HALF_CLK: i32 = 64;
pub const FULL_CLK: i32 = 128;
pub const STROBES_PER_MODULE: usize = 2;

/// Clock-crossing thresholds, percent of a half clock.
const CC0_LO: i32 = HALF_CLK * 25 / 100;
const CC0_HI: i32 = HALF_CLK * 72 / 100;
const CC1_HI: i32 = HALF_CLK * 25 / 100;

/// One field per byte lane at rank 0 (lane 1 unused for CLKCTL elements),
/// and how many bits the next rank's copy sits above it.
#[derive(Clone, Copy)]
pub struct Element {
    lanes: [(u32, Field<u32, ()>); 2],
    rank_bit: usize,
    per_module: bool,
}

const fn erase<R: RegisterLongName>(
    reg: IosfReg<R>,
    field: Field<u32, R>,
) -> (u32, Field<u32, ()>) {
    (reg.address(), Field::new(field.mask, field.shift))
}

/// A DQ element: byte lane 0 and byte lane 1 of DQ module 0, channel 0.
const fn dq<R0: RegisterLongName, R1: RegisterLongName>(
    b0: IosfReg<R0>,
    f0: Field<u32, R0>,
    b1: IosfReg<R1>,
    f1: Field<u32, R1>,
    rank_bit: usize,
) -> Element {
    Element {
        lanes: [erase(b0, f0), erase(b1, f1)],
        rank_bit,
        per_module: true,
    }
}

/// A CLKCTL element of channel 0.
const fn clkctl<R: RegisterLongName>(
    reg: IosfReg<R>,
    f: Field<u32, R>,
    rank_bit: usize,
) -> Element {
    Element {
        lanes: [erase(reg, f); 2],
        rank_bit,
        per_module: false,
    }
}

/// The lowest bit of a wider field (per-signal bits of a select group).
const fn bit0<R: RegisterLongName>(f: Field<u32, R>) -> Field<u32, R> {
    Field::new(1, f.shift)
}

// Receive enable: gates the DQS receiver on during a read.
const RCVN_2X: Element = dq(
    dq_ptrctl0(0, 0),
    DQ_PTRCTL0::RDPTR_OFFSET_RCVEN_B0,
    dq_ptrctl0(0, 0),
    DQ_PTRCTL0::RDPTR_OFFSET_RCVEN_B1,
    0,
);
const RCVN_PI: Element = dq(
    dq_dllpicoder0b0(0, 0),
    DQ_DLLPICODER0B0::PICODE_RCVEN_B0,
    dq_dllpicoder0b1(0, 0),
    DQ_DLLPICODER0B1::PICODE_RCVEN_B1,
    0,
);
const RCVN_CC0: Element = dq(
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_DBSEL_RCVEN_B0,
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_DBSEL_RCVEN_B1,
    16,
);
const RCVN_CC1: Element = dq(
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_PLUS1SEL_RCVEN_B0,
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_PLUS1SEL_RCVEN_B1,
    16,
);
const RCVN_SMP: Element = dq(
    dq_dqtrainsts(0, 0),
    DQ_DQTRAINSTS::SAMPLE_RCVEN_B0,
    dq_dqtrainsts(0, 0),
    DQ_DQTRAINSTS::SAMPLE_RCVEN_B1,
    0,
);
pub const RCVN_MIN: Element = dq(
    dq_rk2rk_ptrctl(0, 0),
    DQ_RK2RK_PTRCTL::RK0_MIN_RCVEN_B0,
    dq_rk2rk_ptrctl(0, 0),
    DQ_RK2RK_PTRCTL::RK0_MIN_RCVEN_B1,
    3,
);

// Write DQS: the strobe driven out with write data.
const WDQS_2X: Element = dq(
    dq_ptrctl0(0, 0),
    DQ_PTRCTL0::RDPTR_OFFSET_DQS_B0,
    dq_ptrctl0(0, 0),
    DQ_PTRCTL0::RDPTR_OFFSET_DQS_B1,
    0,
);
const WDQS_PI: Element = dq(
    dq_dllpicoder0b0(0, 0),
    DQ_DLLPICODER0B0::PICODE_DQS_B0,
    dq_dllpicoder0b1(0, 0),
    DQ_DLLPICODER0B1::PICODE_DQS_B1,
    0,
);
const WDQS_CC0: Element = dq(
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_DBSEL_DQS_B0,
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_DBSEL_DQS_B1,
    16,
);
const WDQS_CC1: Element = dq(
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_PLUS1SEL_DQS_B0,
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_PLUS1SEL_DQS_B1,
    16,
);
const WDQS_SMP: Element = dq(
    dq_dqtrainsts(0, 0),
    DQ_DQTRAINSTS::SAMPLE_DQS_B0,
    dq_dqtrainsts(0, 0),
    DQ_DQTRAINSTS::SAMPLE_DQS_B1,
    0,
);
pub const WDQS_MIN: Element = dq(
    dq_rk2rk_ptrctl(0, 0),
    DQ_RK2RK_PTRCTL::RK0_MIN_DQS_B0,
    dq_rk2rk_ptrctl(0, 0),
    DQ_RK2RK_PTRCTL::RK0_MIN_DQS_B1,
    3,
);

// Write DQ: the write data itself. Not sampled.
const WDQ_2X: Element = dq(
    dq_ptrctl0(0, 0),
    DQ_PTRCTL0::RDPTR_OFFSET_DQ_B0,
    dq_ptrctl0(0, 0),
    DQ_PTRCTL0::RDPTR_OFFSET_DQ_B1,
    0,
);
const WDQ_PI: Element = dq(
    dq_dllpicoder0b0(0, 0),
    DQ_DLLPICODER0B0::PICODE_DQ_B0,
    dq_dllpicoder0b1(0, 0),
    DQ_DLLPICODER0B1::PICODE_DQ_B1,
    0,
);
const WDQ_CC0: Element = dq(
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_DBSEL_DQ_B0,
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_DBSEL_DQ_B1,
    16,
);
const WDQ_CC1: Element = dq(
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_PLUS1SEL_DQ_B0,
    dq_dbctl1(0, 0),
    DQ_DBCTL1::RK0_PLUS1SEL_DQ_B1,
    16,
);
pub const WDQ_MIN: Element = dq(
    dq_rk2rk_ptrctl(0, 0),
    DQ_RK2RK_PTRCTL::RK0_MIN_DQ_B0,
    dq_rk2rk_ptrctl(0, 0),
    DQ_RK2RK_PTRCTL::RK0_MIN_DQ_B1,
    3,
);

/// Read DQS: the receiver strobe position. PI only.
pub const RDQS_PI: Element = dq(
    dq_rxdqspicodeb0(0, 0),
    DQ_RXDQSPICODEB0::PICODE_RXDQS_B0,
    dq_rxdqspicodeb1(0, 0),
    DQ_RXDQSPICODEB1::PICODE_RXDQS_B1,
    0,
);

// CLKCTL write clock: the reference write leveling starts from.
const WCLK_2X: Element = clkctl(clkctl_pointer_reg(0), CLKCTL_POINTER_REG::RDPTR_OFFSET0, 4);
const WCLK_PI: Element = clkctl(clkctl_dllpicoder1(0), CLKCTL_DLLPICODER1::PICODE_WCLK, 0);
const WCLK_CC0: Element = clkctl(clkctl_cfg_reg1(0), bit0(CLKCTL_CFG_REG1::DEADBAND_SEL), 1);
const WCLK_CC1: Element = clkctl(clkctl_cfg_reg1(0), bit0(CLKCTL_CFG_REG1::PTRPLUS1_SEL), 1);

/// Not a delay: the per-module DQ FIFO reset, active low.
pub const FIFO_RST: Element = dq(
    dq_ptrctl1(0, 0),
    DQ_PTRCTL1::DQ_FIFO_RST_N,
    dq_ptrctl1(0, 0),
    DQ_PTRCTL1::DQ_FIFO_RST_N,
    0,
);

/// A trained delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delay {
    /// Receive enable.
    Rcvn,
    /// Write DQS.
    Wdqs,
    /// Write DQ.
    Wdq,
    /// The CLKCTL write clock; read-only reference.
    Wclk,
}

impl Delay {
    /// 2x, PI, CC0 and CC1 elements.
    const fn elements(self) -> [Element; 4] {
        match self {
            Delay::Rcvn => [RCVN_2X, RCVN_PI, RCVN_CC0, RCVN_CC1],
            Delay::Wdqs => [WDQS_2X, WDQS_PI, WDQS_CC0, WDQS_CC1],
            Delay::Wdq => [WDQ_2X, WDQ_PI, WDQ_CC0, WDQ_CC1],
            Delay::Wclk => [WCLK_2X, WCLK_PI, WCLK_CC0, WCLK_CC1],
        }
    }
}

/// `e`'s copy for `ch`, `rank` and `strobe`.
fn locate(e: Element, ch: u32, rank: u32, strobe: usize) -> (IosfReg, Field<u32, ()>) {
    let byte = strobe % STROBES_PER_MODULE;
    let (reg, field) = if e.per_module {
        e.lanes[byte]
    } else {
        e.lanes[0]
    };
    let reg = if e.per_module {
        let module = (strobe / STROBES_PER_MODULE) as u32;
        reg + ch * DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE
    } else {
        reg + ch * CLKCTL_CHANNEL_STRIDE
    };
    let shift = field.shift + e.rank_bit * rank as usize;
    (DDRIO.reg(reg), Field::new(field.mask, shift))
}

pub fn read<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    e: Element,
    ch: u32,
    rank: u32,
    strobe: usize,
) -> u32 {
    let (reg, field) = locate(e, ch, rank, strobe);
    iosf.reg(reg).read(field)
}

pub fn write<P: ConfigRegionAccess>(
    iosf: &Iosf<P>,
    e: Element,
    ch: u32,
    rank: u32,
    strobe: usize,
    value: u32,
) {
    let (reg, field) = locate(e, ch, rank, strobe);
    iosf.reg(reg).modify(field.val(value));
}

/// The linear delay. The 2x and PI counters are shared by every rank.
pub fn get<P: ConfigRegionAccess>(iosf: &Iosf<P>, d: Delay, ch: u32, strobe: usize) -> i32 {
    let [twox, pi, ..] = d.elements();
    read(iosf, twox, ch, 0, strobe) as i32 * HALF_CLK + read(iosf, pi, ch, 0, strobe) as i32
}

/// Set the linear delay, clamped to the counters, and the clock crossings
/// of every rank (they all read the same counters).
pub fn set<P: ConfigRegionAccess>(iosf: &Iosf<P>, d: Delay, ch: u32, strobe: usize, value: i32) {
    let [twox_e, pi_e, cc0_e, cc1_e] = d.elements();
    let value = value.max(0);
    let (twox, pi) = if value / HALF_CLK > 15 {
        (15, HALF_CLK - 1)
    } else {
        (value / HALF_CLK, value % HALF_CLK)
    };
    write(iosf, twox_e, ch, 0, strobe, twox as u32);
    write(iosf, pi_e, ch, 0, strobe, pi as u32);
    let cc0 = !(CC0_LO..CC0_HI).contains(&pi);
    let cc1 = pi < CC1_HI;
    for rank in 0..super::NUM_RANKS {
        write(iosf, cc0_e, ch, rank, strobe, u32::from(cc0));
        write(iosf, cc1_e, ch, rank, strobe, u32::from(cc1));
    }
}

/// The DQS sample the last CPGC run latched for `d` (receive enable or
/// write DQS).
pub fn sample<P: ConfigRegionAccess>(iosf: &Iosf<P>, d: Delay, ch: u32, strobe: usize) -> u32 {
    let e = match d {
        Delay::Rcvn => RCVN_SMP,
        _ => WDQS_SMP,
    };
    read(iosf, e, ch, 0, strobe)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The typed elements cover coreboot's `delay_element` table: base
    /// register, start bit and width of lane 0, lane 1's byte and bit
    /// offsets, and the rank stride.
    #[test]
    fn elements_match_the_reference_layout() {
        let table: [(Element, u32, usize, u32, i32, i32, usize); 19] = [
            (RCVN_2X, 0x70, 8, 4, 0, 12, 0),
            (RCVN_PI, 0x3c, 24, 6, -4, 0, 0),
            (RCVN_CC0, 0x7c, 2, 1, 0, 3, 16),
            (RCVN_CC1, 0x7c, 8, 1, 0, 3, 16),
            (RCVN_SMP, 0x34, 1, 1, 0, -1, 0),
            (RCVN_MIN, 0xac, 18, 1, 0, -16, 3),
            (WDQS_2X, 0x70, 4, 4, 0, 12, 0),
            (WDQS_PI, 0x3c, 16, 6, -4, 0, 0),
            (WDQS_CC0, 0x7c, 1, 1, 0, 3, 16),
            (WDQS_CC1, 0x7c, 7, 1, 0, 3, 16),
            (WDQS_SMP, 0x34, 9, 1, 0, -1, 0),
            (WDQS_MIN, 0xac, 17, 1, 0, -16, 3),
            (WDQ_2X, 0x70, 0, 4, 0, 12, 0),
            (WDQ_PI, 0x3c, 8, 6, -4, 0, 0),
            (WDQ_CC0, 0x7c, 0, 1, 0, 3, 16),
            (WDQ_CC1, 0x7c, 6, 1, 0, 3, 16),
            (WDQ_MIN, 0xac, 16, 1, 0, -16, 3),
            (RDQS_PI, 0x5c, 0, 6, -4, 0, 0),
            (FIFO_RST, 0x74, 8, 1, 0, 0, 0),
        ];
        for (e, reg, shift, width, b1_byte, b1_bit, rank_bit) in table {
            for strobe in 0..8 {
                for rank in 0..2 {
                    let module = (strobe / 2) as u32;
                    let byte = (strobe % 2) as i32;
                    let (r, f) = locate(e, 1, rank, strobe);
                    assert_eq!(
                        r.address(),
                        (reg + DQ_CHANNEL_STRIDE + module * DQ_MOD_STRIDE)
                            .wrapping_add_signed(b1_byte * byte)
                    );
                    assert_eq!(
                        f.shift,
                        (shift as i32 + b1_bit * byte) as usize + rank_bit * rank as usize
                    );
                    assert_eq!(f.mask, (1 << width) - 1);
                }
            }
        }
        // CLKCTL: per channel, 0x800 apart.
        for (e, reg, shift, width, rank_bit) in [
            (WCLK_2X, 0x5844, 8, 4, 4),
            (WCLK_PI, 0x5824, 16, 6, 0),
            (WCLK_CC0, 0x5840, 0, 1, 1),
            (WCLK_CC1, 0x5840, 8, 1, 1),
        ] {
            let (r, f) = locate(e, 1, 1, 5);
            assert_eq!(r.address(), reg + 0x800);
            assert_eq!(f.shift, shift + rank_bit);
            assert_eq!(f.mask, (1 << width) - 1);
        }
    }
}
