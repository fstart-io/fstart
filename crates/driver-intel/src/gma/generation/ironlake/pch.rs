//! PCH transcoder and ports (libgfxinit `PCH.Transcoder`, `PCH.VGA`,
//! `PCH.LVDS`, `PCH.HDMI` and `PCH.DP`).

use super::plls::PchPll;
use super::{Caps, Display, FdiPort, LVDS_DUAL_THRESHOLD_KHZ, PchDigital, PchPort, PortConfig};
use crate::gma::dp_aux::{PreEmphasis, TrainSet, TrainingPattern, VoltageSwing};
use crate::gma::dp_training::{DpLinkConfig, DpSource, train_dp};
use crate::gma::error::GmaError;
use crate::gma::mmio::Mmio;
use crate::gma::mode::{Mode, ModeFlags};
use crate::gma::pipe::PipeConfig;
use crate::gma::regs::{
    GMCH_DATA_M, GMCH_M_N, PCH_ADPA, PCH_DP, PCH_DPLL_SEL, PCH_HDMI, PCH_LVDS,
    PCH_TRANSCODER_SELECT, TRANS_CHICKEN2, TRANS_CONF, TRANS_DP_CTL,
};

const PCH_DPLL_SEL_REG: usize = 0xc7000;
const PCH_ADPA_REG: usize = 0xe1100;
const PCH_LVDS_REG: usize = 0xe1180;
/// libgfxinit waits 50 ms for a transcoder to stop.
const TRANSCODER_OFF_TIMEOUT_US: u64 = 50_000;

const fn pch_hdmi_reg(port: PchDigital) -> usize {
    0xe1140 + 0x10 * port.index()
}

const fn pch_dp_reg(port: PchDigital) -> usize {
    0xe4100 + 0x100 * port.index()
}

/// PCH transcoder registers of one FDI port.
struct TransRegs {
    timing: usize,
    data_m: usize,
    data_n: usize,
    link_m: usize,
    link_n: usize,
    dp_ctl: usize,
    conf: usize,
    chicken2: usize,
}

const fn trans_regs(port: FdiPort) -> TransRegs {
    let stride = 0x1000 * port.index();
    TransRegs {
        timing: 0xe0000 + stride,
        data_m: 0xe0030 + stride,
        data_n: 0xe0034 + stride,
        link_m: 0xe0040 + stride,
        link_n: 0xe0044 + stride,
        dp_ctl: 0xe0300 + stride,
        conf: 0xf0008 + stride,
        chicken2: 0xf0064 + stride,
    }
}

/// `PCH_TRANSCODER_SELECT`: one bit at 30 on Ibex Peak, two at 29 after.
pub(super) const fn transcoder_select(caps: Caps, port: FdiPort) -> u32 {
    if caps.new_fdi_sink() {
        PCH_TRANSCODER_SELECT::CPT.val(port.index() as u32).value
    } else {
        PCH_TRANSCODER_SELECT::IBX.val(port.index() as u32).value
    }
}

const fn transcoder_select_mask(caps: Caps) -> u32 {
    if caps.new_fdi_sink() {
        PCH_TRANSCODER_SELECT::CPT.val(3).value
    } else {
        PCH_TRANSCODER_SELECT::IBX.val(1).value
    }
}

/// `DPLL_SEL_TRANSCODER_x_DPLL_ENABLE`.
const fn dpll_sel_enable(port: FdiPort) -> u32 {
    PCH_DPLL_SEL::TRANSCODER_A_ENABLE::SET.value << (4 * port.index())
}

/// `DPLL_SEL_TRANSCODER_x_DPLL_SEL_MASK`.
const fn dpll_sel_select(port: FdiPort) -> u32 {
    PCH_DPLL_SEL::TRANSCODER_A_PLL::SET.value << (4 * port.index())
}

/// `DPLL_SEL` bits routing `pll` to the transcoder of `port`; libgfxinit
/// `PLLs.Register_Value` is 1 for DPLL B, else 0.
const fn dpll_sel(port: FdiPort, pll: Option<PchPll>) -> u32 {
    dpll_sel_enable(port)
        | if matches!(pll, Some(PchPll::B)) {
            dpll_sel_select(port)
        } else {
            0
        }
}

const fn hsync_high(mode: Mode) -> bool {
    mode.flags.contains(ModeFlags::PHSYNC)
}

const fn vsync_high(mode: Mode) -> bool {
    mode.flags.contains(ModeFlags::PVSYNC)
}

const fn if_set(condition: bool, value: u32) -> u32 {
    if condition { value } else { 0 }
}

/// libgfxinit `PCH.Transcoder.On`.
pub(super) fn transcoder_on(
    mmio: &Mmio,
    caps: Caps,
    port: FdiPort,
    cfg: &PortConfig,
    pll: Option<PchPll>,
) {
    let regs = trans_regs(port);
    let mode = cfg.mode;
    mmio.update32(PCH_DPLL_SEL_REG, dpll_sel_select(port), dpll_sel(port, pll));
    for (offset, value) in [
        (0x00, PipeConfig::encode_range(mode.hdisplay, mode.htotal)),
        (0x04, PipeConfig::encode_range(mode.hdisplay, mode.htotal)),
        (
            0x08,
            PipeConfig::encode_range(mode.hsync_start, mode.hsync_end),
        ),
        (0x0c, PipeConfig::encode_range(mode.vdisplay, mode.vtotal)),
        (0x10, PipeConfig::encode_range(mode.vdisplay, mode.vtotal)),
        (
            0x14,
            PipeConfig::encode_range(mode.vsync_start, mode.vsync_end),
        ),
    ] {
        mmio.write32(regs.timing + offset, value);
    }
    if cfg.route.display == Display::Dp {
        let mn = cfg.dp_link.m_n(mode, u64::from(cfg.bpc));
        mmio.write32(
            regs.data_m,
            (GMCH_DATA_M::TU_SIZE.val(64 - 1) + GMCH_DATA_M::VALUE.val(mn.data_m)).value,
        );
        mmio.write32(regs.data_n, GMCH_M_N::VALUE.val(mn.data_n).value);
        mmio.write32(regs.link_m, GMCH_M_N::VALUE.val(mn.link_m).value);
        mmio.write32(regs.link_n, GMCH_M_N::VALUE.val(mn.link_n).value);
        if caps.trans_dp_ctl()
            && let PchPort::Dp(digital) = cfg.route.pch
        {
            mmio.write32(
                regs.dp_ctl,
                trans_dp_ctl(digital, cfg.dp_link, cfg.bpc, mode),
            );
        }
    }
    if caps.trans_timing_override() {
        mmio.update32(regs.chicken2, 0, TRANS_CHICKEN2::TIMING_OVERRIDE::SET.value);
    }
    mmio.write32(regs.conf, TRANS_CONF::ENABLE::SET.value);
}

/// `TRANS_DP_CTL` value for a Cougar Point DP port.
const fn trans_dp_ctl(port: PchDigital, link: DpLinkConfig, bpc: u8, mode: Mode) -> u32 {
    TRANS_DP_CTL::OUTPUT_ENABLE::SET.value
        | TRANS_DP_CTL::PORT_SELECT.val(port.index() as u32).value
        | if_set(
            link.enhanced_framing,
            TRANS_DP_CTL::ENHANCED_FRAMING::SET.value,
        )
        | TRANS_DP_CTL::BPC
            .val(match bpc {
                6 => 2,
                10 => 1,
                12 => 3,
                _ => 0,
            })
            .value
        | if_set(hsync_high(mode), TRANS_DP_CTL::HSYNC_ACTIVE_HIGH::SET.value)
        | if_set(vsync_high(mode), TRANS_DP_CTL::VSYNC_ACTIVE_HIGH::SET.value)
}

/// libgfxinit `PCH.Transcoder.Off`.
pub(super) fn transcoder_off(mmio: &Mmio, caps: Caps, port: FdiPort) {
    let regs = trans_regs(port);
    mmio.update32(regs.conf, TRANS_CONF::ENABLE::SET.value, 0);
    let _ = mmio.wait_mask(
        regs.conf,
        TRANS_CONF::TRANSCODER_STATE::SET.value,
        0,
        TRANSCODER_OFF_TIMEOUT_US,
    );
    if caps.trans_timing_override() {
        mmio.update32(regs.chicken2, TRANS_CHICKEN2::TIMING_OVERRIDE::SET.value, 0);
    }
    if caps.trans_dp_ctl() {
        mmio.write32(regs.dp_ctl, TRANS_DP_CTL::PORT_SELECT::None.value);
    }
    mmio.update32(PCH_DPLL_SEL_REG, dpll_sel_enable(port), 0);
}

/// libgfxinit `PCH.VGA.On`.
pub(super) fn vga_on(mmio: &Mmio, caps: Caps, port: FdiPort, mode: Mode) {
    let mask = transcoder_select_mask(caps)
        | PCH_ADPA::DAC_ENABLE::SET.value
        | PCH_ADPA::VSYNC_DISABLE::SET.value
        | PCH_ADPA::HSYNC_DISABLE::SET.value
        | PCH_ADPA::VSYNC_ACTIVE_HIGH::SET.value
        | PCH_ADPA::HSYNC_ACTIVE_HIGH::SET.value;
    mmio.update32(
        PCH_ADPA_REG,
        mask,
        PCH_ADPA::DAC_ENABLE::SET.value
            | transcoder_select(caps, port)
            | if_set(hsync_high(mode), PCH_ADPA::HSYNC_ACTIVE_HIGH::SET.value)
            | if_set(vsync_high(mode), PCH_ADPA::VSYNC_ACTIVE_HIGH::SET.value),
    );
}

/// libgfxinit `PCH.VGA.Off`, with the syncs disabled (`VGA_Has_Sync_Disable`).
pub(super) fn vga_off(mmio: &Mmio) {
    mmio.update32(
        PCH_ADPA_REG,
        PCH_ADPA::DAC_ENABLE::SET.value,
        PCH_ADPA::HSYNC_DISABLE::SET.value | PCH_ADPA::VSYNC_DISABLE::SET.value,
    );
}

/// `PCH_LVDS` value of libgfxinit `PCH.LVDS.On` (18 bpp, the second channel
/// from `LVDS_Dual_Threshold`).
pub(super) const fn lvds_value(caps: Caps, port: FdiPort, mode: Mode) -> u32 {
    PCH_LVDS::ENABLE::SET.value
        | transcoder_select(caps, port)
        | if_set(
            !hsync_high(mode),
            PCH_LVDS::HSYNC_POLARITY_INVERT::SET.value,
        )
        | if_set(
            !vsync_high(mode),
            PCH_LVDS::VSYNC_POLARITY_INVERT::SET.value,
        )
        | PCH_LVDS::CLK_A_DATA_A0A2_POWER::PowerUp.value
        | if_set(
            mode.pixel_clock_khz >= LVDS_DUAL_THRESHOLD_KHZ,
            PCH_LVDS::CLK_B_POWER::PowerUp.value | PCH_LVDS::DATA_B0B2_POWER::PowerUp.value,
        )
}

/// libgfxinit `PCH.LVDS.On`.
pub(super) fn lvds_on(mmio: &Mmio, caps: Caps, port: FdiPort, mode: Mode) {
    mmio.write32(PCH_LVDS_REG, lvds_value(caps, port, mode));
}

/// libgfxinit `PCH.LVDS.Off`: all lanes powered down.
pub(super) fn lvds_off(mmio: &Mmio) {
    mmio.write32(PCH_LVDS_REG, 0);
}

const fn hdmi_mask(caps: Caps) -> u32 {
    transcoder_select_mask(caps)
        | PCH_HDMI::ENABLE::SET.value
        | PCH_HDMI::COLOR_FORMAT.val(7).value
        | PCH_HDMI::SDVO_ENCODING.val(3).value
        | PCH_HDMI::HSYNC_ACTIVE_HIGH::SET.value
        | PCH_HDMI::VSYNC_ACTIVE_HIGH::SET.value
}

/// libgfxinit `PCH.HDMI.On`, which sets the enable bit twice.
pub(super) fn hdmi_on(mmio: &Mmio, caps: Caps, port: FdiPort, hdmi: PchDigital, mode: Mode) {
    let reg = pch_hdmi_reg(hdmi);
    mmio.update32(
        reg,
        hdmi_mask(caps),
        PCH_HDMI::ENABLE::SET.value
            | transcoder_select(caps, port)
            | PCH_HDMI::SDVO_ENCODING::Hdmi.value
            | if_set(hsync_high(mode), PCH_HDMI::HSYNC_ACTIVE_HIGH::SET.value)
            | if_set(vsync_high(mode), PCH_HDMI::VSYNC_ACTIVE_HIGH::SET.value),
    );
    mmio.posting_read(reg);
    mmio.update32(reg, 0, PCH_HDMI::ENABLE::SET.value);
    mmio.posting_read(reg);
}

/// libgfxinit `PCH.HDMI.Off`. On Ibex Peak a port left on transcoder B is
/// pulsed on and off again so it releases the transcoder.
pub(super) fn hdmi_off(mmio: &Mmio, caps: Caps, hdmi: PchDigital) {
    let reg = pch_hdmi_reg(hdmi);
    let transcoder_b = PCH_HDMI::ENABLE::SET.value | transcoder_select(caps, FdiPort::B);
    let with_transcoder_b = !caps.trans_dp_ctl() && mmio.read32(reg) & transcoder_b == transcoder_b;
    mmio.update32(
        reg,
        hdmi_mask(caps),
        PCH_HDMI::HSYNC_ACTIVE_HIGH::SET.value | PCH_HDMI::VSYNC_ACTIVE_HIGH::SET.value,
    );
    mmio.posting_read(reg);
    if with_transcoder_b {
        mmio.update32(reg, 0, PCH_HDMI::ENABLE::SET.value);
        mmio.posting_read(reg);
        mmio.update32(reg, 0, PCH_HDMI::ENABLE::SET.value);
        mmio.posting_read(reg);
        mmio.update32(reg, PCH_HDMI::ENABLE::SET.value, 0);
        mmio.posting_read(reg);
    }
}

/// `DP_CTL_LINK_TRAIN`: bits 10:8 with `TRANS_DP_CTL` (Cougar Point), else
/// bits 29:28. TP3 is sent as TP2.
const fn dp_link_train(caps: Caps, pattern: TrainingPattern) -> u32 {
    let value = match pattern {
        TrainingPattern::Pattern1 => 0,
        TrainingPattern::Pattern2 | TrainingPattern::Pattern3 => 1,
        TrainingPattern::None => 3,
    };
    if caps.trans_dp_ctl() {
        PCH_DP::LINK_TRAIN_CPT.val(value).value
    } else {
        PCH_DP::LINK_TRAIN.val(value).value
    }
}

const fn dp_link_train_mask(caps: Caps) -> u32 {
    if caps.trans_dp_ctl() {
        PCH_DP::LINK_TRAIN_CPT.val(7).value
    } else {
        PCH_DP::LINK_TRAIN.val(3).value
    }
}

const fn dp_link_train_idle(caps: Caps) -> u32 {
    if caps.trans_dp_ctl() {
        PCH_DP::LINK_TRAIN_CPT::Idle.value
    } else {
        PCH_DP::LINK_TRAIN::Idle.value
    }
}

/// libgfxinit `PCH.DP.Pre_Training`. Cougar Point takes the transcoder and
/// enhanced framing from `TRANS_DP_CTL` instead.
pub(super) fn dp_pre_training(
    mmio: &Mmio,
    caps: Caps,
    port: FdiPort,
    dp: PchDigital,
    link: DpLinkConfig,
) {
    let ibx = !caps.trans_dp_ctl();
    mmio.write32(
        pch_dp_reg(dp),
        PCH_DP::DISPLAY_PORT_ENABLE::SET.value
            | if_set(ibx, transcoder_select(caps, port))
            | PCH_DP::PORT_WIDTH.val(u32::from(link.lane_count) - 1).value
            | if_set(ibx, PCH_DP::ENHANCED_FRAMING_ENABLE::SET.value)
            | dp_link_train(caps, TrainingPattern::Pattern1),
    );
}

/// libgfxinit `PCH.DP.Off`, including the Ibex Peak transcoder B release.
pub(super) fn dp_off(mmio: &Mmio, caps: Caps, dp: PchDigital) {
    let reg = pch_dp_reg(dp);
    let enable = PCH_DP::DISPLAY_PORT_ENABLE::SET.value;
    let transcoder_b = enable | transcoder_select(caps, FdiPort::B);
    let with_transcoder_b = !caps.trans_dp_ctl() && mmio.read32(reg) & transcoder_b == transcoder_b;
    mmio.update32(reg, dp_link_train_mask(caps), dp_link_train_idle(caps));
    mmio.posting_read(reg);
    mmio.update32(reg, enable, 0);
    mmio.posting_read(reg);
    if with_transcoder_b {
        mmio.update32(
            reg,
            transcoder_select_mask(caps) | dp_link_train_mask(caps),
            enable
                | transcoder_select(caps, FdiPort::A)
                | dp_link_train(caps, TrainingPattern::Pattern1),
        );
        mmio.posting_read(reg);
        mmio.update32(reg, enable, 0);
        mmio.posting_read(reg);
    }
}

/// The PCH DP source for [`train_dp`].
struct PchDpSource<'a> {
    mmio: &'a Mmio,
    caps: Caps,
    port: PchDigital,
}

impl DpSource for PchDpSource<'_> {
    fn max_voltage_swing(&self) -> VoltageSwing {
        VoltageSwing::Level3
    }

    fn max_pre_emphasis(&self, swing: VoltageSwing) -> PreEmphasis {
        match swing {
            VoltageSwing::Level0 => PreEmphasis::Level3,
            VoltageSwing::Level1 => PreEmphasis::Level2,
            VoltageSwing::Level2 => PreEmphasis::Level1,
            VoltageSwing::Level3 => PreEmphasis::Level0,
        }
    }

    fn set_pattern(&mut self, pattern: TrainingPattern) {
        self.mmio.update32(
            pch_dp_reg(self.port),
            dp_link_train_mask(self.caps),
            dp_link_train(self.caps, pattern),
        );
    }

    fn set_signal_levels(&mut self, train_set: TrainSet) {
        self.mmio.update32(
            pch_dp_reg(self.port),
            PCH_DP::VSWING_LEVEL_SET.val(7).value | PCH_DP::PREEMPH_LEVEL_SET.val(7).value,
            PCH_DP::VSWING_LEVEL_SET
                .val(train_set.voltage_swing as u32)
                .value
                | PCH_DP::PREEMPH_LEVEL_SET
                    .val(train_set.pre_emphasis as u32)
                    .value,
        );
    }

    fn off(&mut self) {
        dp_off(self.mmio, self.caps, self.port);
    }
}

/// libgfxinit `PCH.DP.Train`.
pub(super) fn dp_train(
    mmio: &Mmio,
    caps: Caps,
    port: PchDigital,
    cfg: &PortConfig,
) -> Result<(), GmaError> {
    let aux = super::aux_regs(caps.cpu, cfg.route.port)?;
    let mut source = PchDpSource { mmio, caps, port };
    train_dp(mmio, aux, &mut source, cfg.dp_link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gma::dp_aux::DpLinkRate;
    use crate::gma::types::Cpu;

    #[test]
    fn transcoder_select_follows_the_fdi_sink() {
        let ilk = Caps::new(Cpu::Ironlake).unwrap();
        let snb = Caps::new(Cpu::Sandybridge).unwrap();
        assert_eq!(transcoder_select(ilk, FdiPort::B), 1 << 30);
        assert_eq!(transcoder_select(snb, FdiPort::B), 1 << 29);
        assert_eq!(transcoder_select(snb, FdiPort::C), 2 << 29);
        assert_eq!(transcoder_select_mask(snb), 3 << 29);
    }

    #[test]
    fn lvds_matches_libgfxinit_on_cougar_point() {
        let snb = Caps::new(Cpu::Sandybridge).unwrap();
        let mut mode = Mode {
            pixel_clock_khz: 76_300,
            ..Mode::XGA_1024X768_60
        };
        // Negative syncs, one channel, transcoder A.
        assert_eq!(
            lvds_value(snb, FdiPort::A, mode),
            (1 << 31) | (1 << 21) | (1 << 20) | (3 << 8)
        );
        mode.pixel_clock_khz = LVDS_DUAL_THRESHOLD_KHZ;
        mode.flags = ModeFlags::PHSYNC | ModeFlags::PVSYNC;
        assert_eq!(
            lvds_value(snb, FdiPort::B, mode),
            (1 << 31) | (1 << 29) | (3 << 8) | (3 << 4) | (3 << 2)
        );
    }

    #[test]
    fn dpll_sel_and_trans_dp_ctl_encodings() {
        assert_eq!(dpll_sel(FdiPort::B, Some(PchPll::B)), 0x90);
        assert_eq!(dpll_sel(FdiPort::C, Some(PchPll::A)), 0x800);
        assert_eq!(dpll_sel_select(FdiPort::C), 0x100);
        let link = DpLinkConfig {
            link_rate: DpLinkRate::Hbr,
            lane_count: 2,
            enhanced_framing: true,
            tps3_supported: false,
            aux_rd_interval: 0,
        };
        let mode = Mode {
            flags: ModeFlags::PHSYNC | ModeFlags::NVSYNC,
            ..Mode::XGA_1024X768_60
        };
        assert_eq!(
            trans_dp_ctl(PchDigital::C, link, 6, mode),
            (1 << 31) | (1 << 29) | (1 << 18) | (2 << 9) | (1 << 3)
        );
    }

    #[test]
    fn dp_training_pattern_moves_with_trans_dp_ctl() {
        let ilk = Caps::new(Cpu::Ironlake).unwrap();
        let snb = Caps::new(Cpu::Sandybridge).unwrap();
        assert_eq!(dp_link_train(ilk, TrainingPattern::Pattern3), 1 << 28);
        assert_eq!(dp_link_train(snb, TrainingPattern::None), 3 << 8);
        assert_eq!(dp_link_train_mask(snb), 7 << 8);
        assert_eq!(pch_dp_reg(PchDigital::D), 0xe4300);
        assert_eq!(pch_hdmi_reg(PchDigital::C), 0xe1150);
    }
}
