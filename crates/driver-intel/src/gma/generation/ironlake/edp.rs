//! CPU eDP port A (libgfxinit `Connectors.EDP`).

use super::{Caps, PortConfig};
use crate::gma::dp_aux::{DpLinkRate, PreEmphasis, TrainSet, TrainingPattern, VoltageSwing};
use crate::gma::dp_training::{DpLinkConfig, DpSource, train_dp};
use crate::gma::error::GmaError;
use crate::gma::mmio::{Mmio, delay_us};
use crate::gma::mode::ModeFlags;
use crate::gma::regs::CPU_DP_CTL;
use crate::gma::types::{Pipe, Port};

const DP_CTL_A: usize = super::DP_CTL_A;

const ENABLE: u32 = CPU_DP_CTL::DISPLAYPORT_ENABLE::SET.value;
const PLL_ENABLE: u32 = CPU_DP_CTL::PLL_ENABLE::SET.value;
const LINK_TRAIN_MASK: u32 = CPU_DP_CTL::LINK_TRAIN.val(3).value;
const VSWING_EMPH_MASK: u32 = CPU_DP_CTL::VSWING_EMPH_SET.val(0x3f).value;

/// `DP_CTL_VSWING_x_EMPH_y`: the 6-bit field at 27:22.
const fn vswing_emph(swing: VoltageSwing, emphasis: PreEmphasis) -> u32 {
    let (level, emph) = match (swing, emphasis) {
        (VoltageSwing::Level0, PreEmphasis::Level1) => (2, 2),
        (VoltageSwing::Level0, PreEmphasis::Level2) => (3, 3),
        (VoltageSwing::Level1, PreEmphasis::Level0) => (4, 0),
        (VoltageSwing::Level1, PreEmphasis::Level1) => (5, 2),
        (VoltageSwing::Level2, PreEmphasis::Level0) => (6, 0),
        (VoltageSwing::Level2, PreEmphasis::Level1) => (7, 2),
        (VoltageSwing::Level1, _) => (4, 0),
        (VoltageSwing::Level2, _) => (6, 0),
        _ => (1, 0),
    };
    CPU_DP_CTL::VSWING_EMPH_SET
        .val((1 << 5) | (level << 2) | emph)
        .value
}

/// Training pattern bits; idle replaces "none" until training succeeded.
const fn link_train(pattern: TrainingPattern) -> u32 {
    match pattern {
        TrainingPattern::Pattern1 => CPU_DP_CTL::LINK_TRAIN::Pattern1.value,
        TrainingPattern::Pattern2 | TrainingPattern::Pattern3 => {
            CPU_DP_CTL::LINK_TRAIN::Pattern2.value
        }
        TrainingPattern::None => CPU_DP_CTL::LINK_TRAIN::Idle.value,
    }
}

/// `DP_CTL_PIPE_SELECT`. libgfxinit uses the Ivy Bridge field at 30:29 for
/// every CPU; Ironlake and Sandy Bridge select pipe B with bit 30 instead.
const fn pipe_select(caps: Caps, pipe: Pipe) -> u32 {
    let index = pipe as u32;
    if caps.ivybridge() {
        CPU_DP_CTL::PIPE_SELECT_IVB.val(index).value
    } else {
        CPU_DP_CTL::PIPE_SELECT.val(index).value
    }
}

/// `DP_CTL_A` without the PLL enable for libgfxinit `EDP.Pre_On`.
pub(super) const fn pre_on_value(caps: Caps, pipe: Pipe, cfg: &PortConfig) -> u32 {
    let link = cfg.dp_link;
    let mode = cfg.mode;
    pipe_select(caps, pipe)
        | CPU_DP_CTL::PORT_WIDTH
            .val(match link.lane_count {
                1 => 0,
                2 => 1,
                _ => 3,
            })
            .value
        | if link.enhanced_framing {
            CPU_DP_CTL::ENHANCED_FRAMING_ENABLE::SET.value
        } else {
            0
        }
        | match link.link_rate {
            DpLinkRate::Rbr => CPU_DP_CTL::PLL_FREQUENCY::Rate162MHz.value,
            DpLinkRate::Hbr => CPU_DP_CTL::PLL_FREQUENCY::Rate270MHz.value,
            // libgfxinit leaves the field alone (270 MHz) for other rates.
            _ => 0,
        }
        | if mode.flags.contains(ModeFlags::PVSYNC) {
            CPU_DP_CTL::VSYNC_ACTIVE_HIGH::SET.value
        } else {
            0
        }
        | if mode.flags.contains(ModeFlags::PHSYNC) {
            CPU_DP_CTL::HSYNC_ACTIVE_HIGH::SET.value
        } else {
            0
        }
}

/// libgfxinit `EDP.Pre_On`: configure the port, then start its PLL.
pub(super) fn pre_on(mmio: &Mmio, caps: Caps, pipe: Pipe, cfg: &PortConfig) {
    let value = pre_on_value(caps, pipe, cfg);
    mmio.write32(DP_CTL_A, value);
    mmio.write32(DP_CTL_A, PLL_ENABLE | value);
    mmio.posting_read(DP_CTL_A);
    delay_us(20);
}

/// libgfxinit `EDP.Pre_Training`.
pub(super) fn pre_training(mmio: &Mmio) {
    mmio.update32(
        DP_CTL_A,
        LINK_TRAIN_MASK,
        link_train(TrainingPattern::Pattern1) | ENABLE,
    );
}

/// The eDP source for [`train_dp`].
struct EdpSource<'a> {
    mmio: &'a Mmio,
}

impl DpSource for EdpSource<'_> {
    fn max_voltage_swing(&self) -> VoltageSwing {
        VoltageSwing::Level2
    }

    fn max_pre_emphasis(&self, swing: VoltageSwing) -> PreEmphasis {
        match swing {
            VoltageSwing::Level0 => PreEmphasis::Level2,
            VoltageSwing::Level1 | VoltageSwing::Level2 => PreEmphasis::Level1,
            VoltageSwing::Level3 => PreEmphasis::Level0,
        }
    }

    fn set_pattern(&mut self, pattern: TrainingPattern) {
        self.mmio
            .update32(DP_CTL_A, LINK_TRAIN_MASK, link_train(pattern));
    }

    fn set_signal_levels(&mut self, train_set: TrainSet) {
        self.mmio.update32(
            DP_CTL_A,
            VSWING_EMPH_MASK,
            vswing_emph(train_set.voltage_swing, train_set.pre_emphasis),
        );
    }

    fn off(&mut self) {
        off(self.mmio);
    }
}

/// libgfxinit `EDP.Post_On`: train, then switch to normal operation.
pub(super) fn post_on(mmio: &Mmio, caps: Caps, link: DpLinkConfig) -> Result<(), GmaError> {
    let aux = super::aux_regs(caps.cpu, Port::Edp)?;
    train_dp(mmio, aux, &mut EdpSource { mmio }, link)?;
    mmio.update32(
        DP_CTL_A,
        LINK_TRAIN_MASK,
        CPU_DP_CTL::LINK_TRAIN::Normal.value,
    );
    Ok(())
}

/// libgfxinit `EDP.Off`: idle, disable, then clear the register and let the
/// PLL wind down.
pub(super) fn off(mmio: &Mmio) {
    mmio.update32(
        DP_CTL_A,
        LINK_TRAIN_MASK,
        CPU_DP_CTL::LINK_TRAIN::Idle.value,
    );
    mmio.posting_read(DP_CTL_A);
    mmio.update32(DP_CTL_A, ENABLE, 0);
    let pll_was_on = mmio.read32(DP_CTL_A) & PLL_ENABLE != 0;
    mmio.write32(DP_CTL_A, 0);
    mmio.posting_read(DP_CTL_A);
    if pll_was_on {
        delay_us(20);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gma::mode::Mode;
    use crate::gma::types::Cpu;

    #[test]
    fn vswing_emphasis_table_matches_libgfxinit() {
        let encode = |vs, pe| vswing_emph(vs, pe) >> 22;
        assert_eq!(encode(VoltageSwing::Level0, PreEmphasis::Level0), 0x24);
        assert_eq!(encode(VoltageSwing::Level0, PreEmphasis::Level2), 0x2f);
        assert_eq!(encode(VoltageSwing::Level1, PreEmphasis::Level1), 0x36);
        assert_eq!(encode(VoltageSwing::Level2, PreEmphasis::Level1), 0x3e);
        // Unsupported combinations fall back to the level's first entry.
        assert_eq!(encode(VoltageSwing::Level2, PreEmphasis::Level3), 0x38);
        assert_eq!(encode(VoltageSwing::Level3, PreEmphasis::Level0), 0x24);
    }

    #[test]
    fn pre_on_selects_pipe_lanes_rate_and_syncs() {
        let snb = Caps::new(Cpu::Sandybridge).unwrap();
        let ivb = Caps::new(Cpu::Ivybridge).unwrap();
        let mut cfg = super::super::PortConfig::fill(
            snb,
            Pipe::B,
            Port::Edp,
            Mode {
                flags: ModeFlags::PHSYNC | ModeFlags::PVSYNC,
                ..Mode::XGA_1024X768_60
            },
        )
        .unwrap();
        cfg.dp_link = DpLinkConfig {
            link_rate: DpLinkRate::Rbr,
            lane_count: 4,
            enhanced_framing: true,
            tps3_supported: false,
            aux_rd_interval: 0,
        };
        assert_eq!(
            pre_on_value(snb, Pipe::B, &cfg),
            (1 << 30) | (3 << 19) | (1 << 18) | (1 << 16) | (1 << 4) | (1 << 3)
        );
        assert_eq!(pre_on_value(ivb, Pipe::C, &cfg) & (3 << 29), 2 << 29);
    }
}
