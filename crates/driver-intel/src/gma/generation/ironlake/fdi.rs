//! FDI link: CPU transmitter and PCH receiver (libgfxinit
//! `Connectors.FDI` and `PCH.FDI`).

use super::{Caps, FdiPort, FdiTraining, PortConfig};
use crate::gma::error::GmaError;
use crate::gma::mmio::{Mmio, delay_us};
use crate::gma::regs::{
    FDI_RX_CTL, FDI_RX_IIR, FDI_RX_MISC, FDI_RX_TUSIZE1, FDI_TX_CTL, PCH_FDI_CHICKEN_B_C,
};

const PCH_FDI_CHICKEN_B_C_REG: usize = 0xc2000;

const TX_ENABLE: u32 = FDI_TX_CTL::FDI_TX_ENABLE::SET.value;
const TX_VP_MASK: u32 = FDI_TX_CTL::VP.val(0x3f).value;
const TX_PORT_WIDTH_4: u32 = FDI_TX_CTL::PORT_WIDTH_SEL.val(3).value;
const TX_ENHANCED_FRAMING: u32 = FDI_TX_CTL::ENHANCED_FRAMING_ENABLE::SET.value;
const TX_PLL_ENABLE: u32 = FDI_TX_CTL::FDI_PLL_ENABLE::SET.value;
const TX_COMPOSITE_SYNC_SELECT: u32 = FDI_TX_CTL::COMPOSITE_SYNC_SELECT::SET.value;
const TX_AUTO_TRAIN_ENABLE: u32 = FDI_TX_CTL::AUTO_TRAIN_ENABLE::SET.value;
const TX_AUTO_TRAIN_DONE: u32 = FDI_TX_CTL::AUTO_TRAIN_DONE::SET.value;

const RX_ENABLE: u32 = FDI_RX_CTL::FDI_RX_ENABLE::SET.value;
const RX_FS_ERROR_CORRECTION: u32 = FDI_RX_CTL::FS_ERROR_CORRECTION_ENABLE::SET.value;
const RX_FE_ERROR_CORRECTION: u32 = FDI_RX_CTL::FE_ERROR_CORRECTION_ENABLE::SET.value;
const RX_PLL_ENABLE: u32 = FDI_RX_CTL::FDI_PLL_ENABLE::SET.value;
const RX_COMPOSITE_SYNC_SELECT: u32 = FDI_RX_CTL::COMPOSITE_SYNC_SELECT::SET.value;
const RX_AUTO_TRAIN: u32 = FDI_RX_CTL::FDI_AUTO_TRAIN::SET.value;
const RX_ENHANCED_FRAMING: u32 = FDI_RX_CTL::ENHANCED_FRAMING_ENABLE::SET.value;
const RX_RAWCLK_TO_PCDCLK_MASK: u32 = FDI_RX_CTL::RAWCLK_TO_PCDCLK_SEL::SET.value;
const RX_RAWCLK_TO_PCDCLK_PCDCLK: u32 = FDI_RX_CTL::RAWCLK_TO_PCDCLK_SEL::SET.value;

const IIR_INTERLANE_ALIGNMENT: u32 = FDI_RX_IIR::INTERLANE_ALIGNMENT::SET.value;
const IIR_SYMBOL_LOCK: u32 = FDI_RX_IIR::SYMBOL_LOCK::SET.value;
const IIR_BIT_LOCK: u32 = FDI_RX_IIR::BIT_LOCK::SET.value;
const IIR_ALL: u32 = IIR_INTERLANE_ALIGNMENT | IIR_SYMBOL_LOCK | IIR_BIT_LOCK;

/// `FDI_TX_CTL_VSWING_PREEMPH`, tried twice each by full and auto training.
const VSWING_PREEMPH: [u32; 4] = [0x00, 0x3a, 0x39, 0x38];

/// FDI training pattern (libgfxinit `PCH.FDI.Training_Pattern`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Pattern {
    /// Training pattern 1 (bit lock).
    Tp1 = 0,
    /// Training pattern 2 (symbol lock).
    Tp2 = 1,
    /// Idle pattern.
    #[allow(dead_code)]
    Idle = 2,
    /// Normal operation.
    None = 3,
}

/// How far `off` shuts the link down (libgfxinit `Connectors.FDI.Off_Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Off {
    /// Transmitter and receiver off, clocks running.
    Link,
    /// Also stop the FDI PLLs.
    Clock,
}

/// How far `rx_off` shuts the receiver down (libgfxinit `PCH.FDI.Off_Type`;
/// `Lanes_Off` only matters with `Has_FDI_RX_Power_Down`, i.e. Haswell).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RxOff {
    Rx,
    Clock,
}

/// `FDI_TX_CTL` of the CPU transmitter feeding `port`.
pub(super) const fn tx_ctl(port: FdiPort) -> usize {
    0x60100 + 0x1000 * port.index()
}

/// PCH receiver register block of `port`.
struct RxRegs {
    ctl: usize,
    misc: usize,
    iir: usize,
    imr: usize,
    tusize: usize,
}

const fn rx_regs(port: FdiPort) -> RxRegs {
    let base = 0xf000c + 0x1000 * port.index();
    RxRegs {
        ctl: base,
        misc: base + 0x4,
        iir: base + 0x8,
        imr: base + 0xc,
        tusize: base + 0x24,
    }
}

/// Training pattern bits, at 9:8 with the new source/sink, else at 29:28.
const fn pattern_bits(new: bool, pattern: Pattern) -> u32 {
    (pattern as u32) << if new { 8 } else { 28 }
}

const fn pattern_mask(new: bool) -> u32 {
    3 << if new { 8 } else { 28 }
}

const fn tx_pattern(caps: Caps, pattern: Pattern) -> u32 {
    pattern_bits(caps.new_fdi_source(), pattern)
}

const fn tx_pattern_mask(caps: Caps) -> u32 {
    pattern_mask(caps.new_fdi_source())
}

const fn rx_pattern(caps: Caps, pattern: Pattern) -> u32 {
    pattern_bits(caps.new_fdi_sink(), pattern)
}

const fn rx_pattern_mask(caps: Caps) -> u32 {
    pattern_mask(caps.new_fdi_sink())
}

const fn tx_port_width(lanes: u8) -> u32 {
    FDI_TX_CTL::PORT_WIDTH_SEL.val(lanes as u32 - 1).value
}

const fn rx_port_width(lanes: u8) -> u32 {
    FDI_RX_CTL::PORT_WIDTH_SEL.val(lanes as u32 - 1).value
}

/// `FDI_RX_CTL_BPC` (`Has_FDI_BPC`).
const fn rx_bpc(bpc: u8) -> u32 {
    FDI_RX_CTL::BPC
        .val(match bpc {
            6 => 2,
            10 => 1,
            12 => 3,
            _ => 0,
        })
        .value
}

/// Whether the transmitter of `port` is on.
pub(super) fn tx_enabled(mmio: &Mmio, port: FdiPort) -> bool {
    mmio.read32(tx_ctl(port)) & TX_ENABLE != 0
}

/// Whether the transmitter of `port` is on with all four lanes.
pub(super) fn tx_uses_four_lanes(mmio: &Mmio, port: FdiPort) -> bool {
    let mask = TX_ENABLE | TX_PORT_WIDTH_4;
    mmio.read32(tx_ctl(port)) & mask == mask
}

/// libgfxinit `PCH.FDI.Pre_Train`: receiver setup and its PLL.
fn rx_pre_train(mmio: &Mmio, caps: Caps, port: FdiPort, cfg: &PortConfig) {
    let regs = rx_regs(port);
    mmio.write32(
        regs.misc,
        FDI_RX_MISC::TP1_TO_TP2_TIME.val(2).value | FDI_RX_MISC::FDI_DELAY.val(0x90).value,
    );
    mmio.write32(regs.tusize, FDI_RX_TUSIZE1::TU_SIZE.val(64 - 1).value);
    mmio.update32(regs.imr, IIR_ALL, 0);
    mmio.posting_read(regs.imr);
    mmio.write32(regs.iir, IIR_ALL);
    let settings = rx_port_width(cfg.fdi_link.lane_count)
        | rx_bpc(cfg.bpc)
        | if caps.fdi_composite_sel() {
            RX_COMPOSITE_SYNC_SELECT
        } else {
            0
        }
        | if cfg.fdi_link.enhanced_framing {
            RX_ENHANCED_FRAMING
        } else {
            0
        };
    mmio.write32(regs.ctl, RX_PLL_ENABLE | settings);
    mmio.posting_read(regs.ctl);
    delay_us(220);
    mmio.update32(regs.ctl, 0, RX_RAWCLK_TO_PCDCLK_PCDCLK);
}

/// libgfxinit `PCH.FDI.Train`: switch the receiver to `pattern` and check
/// for its lock.
fn rx_train(mmio: &Mmio, caps: Caps, port: FdiPort, pattern: Pattern) -> bool {
    let regs = rx_regs(port);
    mmio.update32(
        regs.ctl,
        rx_pattern_mask(caps),
        RX_ENABLE | rx_pattern(caps, pattern),
    );
    mmio.posting_read(regs.ctl);
    match pattern {
        Pattern::Tp1 => {
            delay_us(1);
            check_lock(mmio, regs.iir, IIR_BIT_LOCK)
        }
        Pattern::Tp2 => {
            delay_us(1);
            check_lock(mmio, regs.iir, IIR_SYMBOL_LOCK)
                && check_lock(mmio, regs.iir, IIR_INTERLANE_ALIGNMENT)
        }
        _ => {
            delay_us(31);
            true
        }
    }
}

/// `Check_Lock`: up to five polls, acknowledging the lock bit.
fn check_lock(mmio: &Mmio, iir: usize, bit: u32) -> bool {
    for _ in 0..5 {
        if mmio.read32(iir) & bit != 0 {
            mmio.write32(iir, bit);
            return true;
        }
        delay_us(1);
    }
    false
}

/// libgfxinit `PCH.FDI.Auto_Train`.
fn rx_auto_train(mmio: &Mmio, port: FdiPort) {
    let regs = rx_regs(port);
    mmio.update32(regs.ctl, 0, RX_ENABLE | RX_AUTO_TRAIN);
    mmio.posting_read(regs.ctl);
    delay_us(5);
}

/// libgfxinit `PCH.FDI.Enable_EC`.
fn rx_enable_ec(mmio: &Mmio, port: FdiPort) {
    mmio.update32(
        rx_regs(port).ctl,
        0,
        RX_FS_ERROR_CORRECTION | RX_FE_ERROR_CORRECTION,
    );
}

/// libgfxinit `PCH.FDI.Off`.
fn rx_off(mmio: &Mmio, port: FdiPort, off: RxOff) {
    let regs = rx_regs(port);
    mmio.update32(regs.ctl, RX_ENABLE | RX_AUTO_TRAIN, 0);
    if off >= RxOff::Clock {
        mmio.update32(regs.ctl, RX_RAWCLK_TO_PCDCLK_MASK, 0);
        mmio.update32(regs.ctl, RX_PLL_ENABLE, 0);
    }
}

/// Set the transmitter's training pattern.
fn tx_set_pattern(mmio: &Mmio, caps: Caps, port: FdiPort, pattern: Pattern) {
    mmio.update32(
        tx_ctl(port),
        tx_pattern_mask(caps),
        tx_pattern(caps, pattern),
    );
    mmio.posting_read(tx_ctl(port));
}

/// Stop the transmitter and receiver after a failed attempt.
fn link_off(mmio: &Mmio, caps: Caps, port: FdiPort) {
    mmio.update32(
        tx_ctl(port),
        TX_ENABLE | TX_AUTO_TRAIN_ENABLE | tx_pattern_mask(caps),
        tx_pattern(caps, Pattern::Tp1),
    );
    rx_off(mmio, port, RxOff::Rx);
}

/// Stop both FDI PLLs after training failed for good.
fn clock_off(mmio: &Mmio, port: FdiPort) {
    mmio.update32(tx_ctl(port), TX_PLL_ENABLE, 0);
    rx_off(mmio, port, RxOff::Clock);
}

/// libgfxinit `Connectors.FDI.Pre_On`: lane split, receiver and both PLLs.
pub(super) fn pre_on(mmio: &Mmio, caps: Caps, port: FdiPort, cfg: &PortConfig) {
    if caps.has_fdi_c() {
        let chicken = PCH_FDI_CHICKEN_B_C::B_AND_C::SET.value;
        if port == FdiPort::C || (port == FdiPort::B && cfg.fdi_link.lane_count <= 2) {
            mmio.update32(PCH_FDI_CHICKEN_B_C_REG, 0, chicken);
        } else if port == FdiPort::B {
            mmio.update32(PCH_FDI_CHICKEN_B_C_REG, chicken, 0);
        }
    }
    rx_pre_train(mmio, caps, port, cfg);
    mmio.write32(
        tx_ctl(port),
        tx_port_width(cfg.fdi_link.lane_count)
            | TX_ENHANCED_FRAMING
            | TX_PLL_ENABLE
            | if caps.fdi_composite_sel() {
                TX_COMPOSITE_SYNC_SELECT
            } else {
                0
            }
            | tx_pattern(caps, Pattern::Tp1),
    );
    mmio.posting_read(tx_ctl(port));
    delay_us(100);
}

/// libgfxinit `Simple_Training` (Ironlake).
fn simple_training(mmio: &Mmio, caps: Caps, port: FdiPort) -> bool {
    mmio.update32(
        tx_ctl(port),
        tx_pattern_mask(caps),
        TX_ENABLE | tx_pattern(caps, Pattern::Tp1),
    );
    mmio.posting_read(tx_ctl(port));
    let mut success = rx_train(mmio, caps, port, Pattern::Tp1);
    if success {
        tx_set_pattern(mmio, caps, port, Pattern::Tp2);
        success = rx_train(mmio, caps, port, Pattern::Tp2);
    }
    if success {
        tx_set_pattern(mmio, caps, port, Pattern::None);
        rx_train(mmio, caps, port, Pattern::None)
    } else {
        link_off(mmio, caps, port);
        clock_off(mmio, port);
        false
    }
}

/// libgfxinit `Full_Training` (Sandy Bridge): each voltage swing/pre-emphasis
/// setting twice, receiver off between attempts.
fn full_training(mmio: &Mmio, caps: Caps, port: FdiPort) -> bool {
    let mut success = false;
    for attempt in 0..VSWING_PREEMPH.len() * 2 {
        mmio.update32(
            tx_ctl(port),
            TX_VP_MASK | tx_pattern_mask(caps),
            TX_ENABLE
                | FDI_TX_CTL::VP.val(VSWING_PREEMPH[attempt / 2]).value
                | tx_pattern(caps, Pattern::Tp1),
        );
        mmio.posting_read(tx_ctl(port));
        success = rx_train(mmio, caps, port, Pattern::Tp1);
        if success {
            tx_set_pattern(mmio, caps, port, Pattern::Tp2);
            success = rx_train(mmio, caps, port, Pattern::Tp2);
        }
        if success {
            break;
        }
        link_off(mmio, caps, port);
    }
    if success {
        tx_set_pattern(mmio, caps, port, Pattern::None);
        rx_train(mmio, caps, port, Pattern::None)
    } else {
        clock_off(mmio, port);
        false
    }
}

/// libgfxinit `Auto_Training` (Ivy Bridge).
fn auto_training(mmio: &Mmio, caps: Caps, port: FdiPort) -> bool {
    let mut success = false;
    for attempt in 0..VSWING_PREEMPH.len() * 2 {
        mmio.update32(
            tx_ctl(port),
            TX_VP_MASK | tx_pattern_mask(caps),
            TX_ENABLE
                | FDI_TX_CTL::VP.val(VSWING_PREEMPH[attempt / 2]).value
                | TX_AUTO_TRAIN_ENABLE
                | tx_pattern(caps, Pattern::Tp1),
        );
        mmio.posting_read(tx_ctl(port));
        rx_auto_train(mmio, port);
        for poll in 0..4 {
            success = mmio.read32(tx_ctl(port)) & TX_AUTO_TRAIN_DONE != 0;
            if success || poll == 3 {
                break;
            }
            delay_us(1);
        }
        if success {
            break;
        }
        link_off(mmio, caps, port);
    }
    if success {
        rx_enable_ec(mmio, port);
    } else {
        clock_off(mmio, port);
    }
    success
}

/// libgfxinit `Connectors.FDI.Post_On`: train the link.
pub(super) fn post_on(mmio: &Mmio, caps: Caps, port: FdiPort) -> Result<(), GmaError> {
    let trained = match caps.fdi_training() {
        FdiTraining::Simple => simple_training(mmio, caps, port),
        FdiTraining::Full => full_training(mmio, caps, port),
        FdiTraining::Auto => auto_training(mmio, caps, port),
    };
    if trained {
        Ok(())
    } else {
        fstart_log::warn!(
            "intel-gma: FDI training failed: tx={:#x} rx={:#x} iir={:#x}",
            mmio.read32(tx_ctl(port)),
            mmio.read32(rx_regs(port).ctl),
            mmio.read32(rx_regs(port).iir)
        );
        Err(GmaError::HardwareError)
    }
}

/// libgfxinit `Connectors.FDI.Off`.
pub(super) fn off(mmio: &Mmio, caps: Caps, port: FdiPort, off: Off) {
    link_off(mmio, caps, port);
    if off >= Off::Clock {
        clock_off(mmio, port);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gma::types::Cpu;

    #[test]
    fn training_patterns_move_with_the_new_source_and_sink() {
        let ilk = Caps::new(Cpu::Ironlake).unwrap();
        let snb = Caps::new(Cpu::Sandybridge).unwrap();
        let ivb = Caps::new(Cpu::Ivybridge).unwrap();
        // Ironlake: both ends at 29:28.
        assert_eq!(tx_pattern(ilk, Pattern::Tp2), 1 << 28);
        assert_eq!(rx_pattern(ilk, Pattern::Tp2), 1 << 28);
        // Cougar Point moved the receiver to 9:8, Ivy Bridge the transmitter.
        assert_eq!(tx_pattern(snb, Pattern::None), 3 << 28);
        assert_eq!(rx_pattern(snb, Pattern::None), 3 << 8);
        assert_eq!(tx_pattern(ivb, Pattern::Tp2), 1 << 8);
        assert_eq!(rx_pattern_mask(ivb), 3 << 8);
    }

    #[test]
    fn registers_match_libgfxinit() {
        assert_eq!(tx_ctl(FdiPort::C), 0x62100);
        let b = rx_regs(FdiPort::B);
        assert_eq!(
            [b.ctl, b.misc, b.iir, b.imr, b.tusize],
            [0xf100c, 0xf1010, 0xf1014, 0xf1018, 0xf1030]
        );
        assert_eq!(rx_bpc(6), 2 << 16);
        assert_eq!(tx_port_width(4), 3 << 19);
    }
}
