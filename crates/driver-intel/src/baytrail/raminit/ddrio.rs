//! DDRIO (DDR3 PHY) bring-up (coreboot `romstage/ddrio.c`): MPLL, static
//! settings and starting delays, power management, DLLs, clock alignment,
//! compensation and IO buffers. Native init brings up channel 0 only and
//! keeps channel 1's byte lanes off.

use super::ddrio_regs::*;
use super::delay::{self, Delay, HALF_CLK, STROBES_PER_MODULE};
use super::{DdrFreq, Params};
use crate::baytrail::iosf::{Iosf, IosfReg};
use fstart_arch::x86::udelay;
use fstart_core::services::ServiceError;
use fstart_pci::ConfigRegionAccess;
use tock_registers::RegisterLongName;
use tock_registers::fields::Field;
use tock_registers::interfaces::{ReadWriteable, Readable};

/// Channels the PHY is brought up for; native init is single channel.
const POPULATED_CHANNELS: u32 = 1;

/// Wait until `field` of `reg` reads `done`.
fn poll<P: ConfigRegionAccess, R: RegisterLongName>(
    iosf: &Iosf<P>,
    reg: IosfReg<R>,
    field: Field<u32, R>,
    done: u32,
    what: &str,
) -> Result<(), ServiceError> {
    for _ in 0..1_000_000 {
        if iosf.reg(reg).read(field) == done {
            return Ok(());
        }
    }
    fstart_log::error!("baytrail: DDRIO {} timeout", what);
    Err(ServiceError::Timeout)
}

fn sfr_vol_sel<P: ConfigRegionAccess>(iosf: &Iosf<P>) {
    iosf.reg(mpllmisc())
        .modify(MPLLMISC::MPLL_SFR_SUPPLY_MODE.val(0x0));
}

/// MPLL bring-up. At DDR-800 the PHY runs off the 2x clock with SFR, VCO
/// and dividers off, so one predicate drives the sequence.
fn pll_init<P: ConfigRegionAccess>(iosf: &Iosf<P>, freq: DdrFreq) -> Result<(), ServiceError> {
    // Above DDR-800 the MPLL runs off the spread-spectrum reference.
    let sfr = u32::from(freq != DdrFreq::Ddr800);

    iosf.reg(mpllcsr0()).modify(MPLLCSR0::SFR_CFG.val(0xb));
    iosf.reg(mpllctrl())
        .modify(MPLLCTRL::DDR_FREQ.val(freq as u32));
    iosf.reg(mpllcsr1())
        .modify(MPLLCSR1::DTYCYC_DEL.val(0x0) + MPLLCSR1::PLL_MODE.val(0x0));
    iosf.reg(mpllmisc())
        .modify(MPLLMISC::MPLL_2XCLK_SEL.val(sfr ^ 1));
    iosf.reg(plllockdel())
        .modify(PLLLOCKDEL::PLL_WRMUP_DEL.val(0xa80));
    iosf.reg(sfrdel()).modify(SFRDEL::SFR_STBL_DEL.val(0x88));
    iosf.reg(mpllcsr0()).modify(MPLLCSR0::SFR_EN.val(sfr));
    udelay(100);

    poll(
        iosf,
        mplldftout(),
        MPLLDFTOUT::SFR_CNT_DONE,
        sfr,
        "sfr_cnt_done",
    )?;
    iosf.reg(mpllctrl()).modify(MPLLCTRL::VCO_DIS.val(sfr ^ 1));
    udelay(100);

    poll(
        iosf,
        mplldftout(),
        MPLLDFTOUT::PLL_CNT_DONE,
        sfr,
        "pll_cnt_done",
    )?;
    iosf.reg(mpllctrl()).modify(MPLLCTRL::DIVRST_N.val(sfr));
    udelay(100);

    poll(
        iosf,
        mplldftout(),
        MPLLDFTOUT::DTYCYC_DONE,
        sfr,
        "dtycyc_done",
    )?;

    iosf.reg(mpllctrl1()).modify(MPLLCTRL1::CLKGATE_EN.val(0x1));
    iosf.reg(mpllctrl1())
        .modify(MPLLCTRL1::CHANNEL1_ECC_EN.val(0x0));
    iosf.reg(mpllmisc())
        .modify(MPLLMISC::CH0_CLK_CKEBB_EN.val(0x1) + MPLLMISC::CH1_CLK_CKEBB_EN.val(0x1));
    Ok(())
}

/// CLKCTL/CMD phase-interpolator codes and pointer offsets, per DRAM clock.
struct PtrTuning {
    /// CLKCTL DLLPICODER0 grp3_b0/grp2_b0, DLLPICODER1 wclk/grp1_b1.
    clkctl_pi_a: u32,
    /// CLKCTL DLLPICODER0 grp3_b1/wctl.
    clkctl_pi_b: u32,
    /// CLKCTL DLLPICODER1 cmd_clkctl.
    clkctl_pi_cmd: u32,
    /// CLKCTL CFG_REG1 deadband_sel/ptrplus1_sel.
    clkctl_sel: u32,
    /// CLKCTL CMD_PTRCTL rdptr_offset6.
    clkctl_rdptr6: u32,
    /// CLKCTL CMD_PTRCTL cmd_deadband_sel.
    clkctl_cmd_db: u32,
    /// CLKCTL POINTER_REG rdptr_offset0..5.
    clkctl_rdptr: u32,
    /// CLKCTL CMD_PTRCTL cmd_ptrplus1_sel.
    clkctl_cmd_pp1: u32,
    /// CMD DLLPICODER0 grp2_b1/grp3_b0/wctl_cmd, DLLPICODER1 grp1_b1.
    cmd_pi: u32,
    /// CMD DLLPICODER1 cmd.
    cmd_pi_cmd: u32,
    /// CMD CFG_REG0 ckecmd_deadband_sel/ckecmd_ptrplus1_sel.
    cmd_ckecmd_sel: u32,
    /// CMD CFG_REG0 ptrplus1_sel.
    cmd_pp1: u32,
    /// CMD CFG_REG0 deadband_sel.
    cmd_db: u32,
    /// CMD POINTER_REG ckecmd_rdptr_offset0..3.
    cmd_ckecmd_rdptr: u32,
    /// CMD POINTER_REG rdptr_offset0.
    cmd_rdptr0: u32,
}

const PTR_TUNING: [PtrTuning; 3] = [
    // DDR3-800
    PtrTuning {
        clkctl_pi_a: 0x19,
        clkctl_pi_b: 0x12,
        clkctl_pi_cmd: 0x12,
        clkctl_sel: 0x00,
        clkctl_rdptr6: 0x2,
        clkctl_cmd_db: 0x0,
        clkctl_rdptr: 0x2,
        clkctl_cmd_pp1: 0x0,
        cmd_pi: 0x12,
        cmd_pi_cmd: 0x12,
        cmd_ckecmd_sel: 0x0,
        cmd_pp1: 0x0,
        cmd_db: 0x0,
        cmd_ckecmd_rdptr: 0x2,
        cmd_rdptr0: 0x2,
    },
    // DDR3-1066
    PtrTuning {
        clkctl_pi_a: 0x05,
        clkctl_pi_b: 0x06,
        clkctl_pi_cmd: 0x3c,
        clkctl_sel: 0x3f,
        clkctl_rdptr6: 0x1,
        clkctl_cmd_db: 0x1,
        clkctl_rdptr: 0x2,
        clkctl_cmd_pp1: 0x0,
        cmd_pi: 0x06,
        cmd_pi_cmd: 0x3c,
        cmd_ckecmd_sel: 0x1,
        cmd_pp1: 0x0,
        cmd_db: 0x1,
        cmd_ckecmd_rdptr: 0x2,
        cmd_rdptr0: 0x1,
    },
    // DDR3-1333
    PtrTuning {
        clkctl_pi_a: 0x20,
        clkctl_pi_b: 0x1b,
        clkctl_pi_cmd: 0x0d,
        clkctl_sel: 0x00,
        clkctl_rdptr6: 0x1,
        clkctl_cmd_db: 0x1,
        clkctl_rdptr: 0x1,
        clkctl_cmd_pp1: 0x1,
        cmd_pi: 0x1b,
        cmd_pi_cmd: 0x0d,
        cmd_ckecmd_sel: 0x0,
        cmd_pp1: 0x1,
        cmd_db: 0x1,
        cmd_ckecmd_rdptr: 0x1,
        cmd_rdptr0: 0x1,
    },
];

fn static_init<P: ConfigRegionAccess>(iosf: &Iosf<P>, freq: DdrFreq, tcl: u32, wl: u32) {
    for ch in 0..POPULATED_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_latctl0_b0(ch, m))
                .modify(DQ_LATCTL0_B0::WRCMD2DQSSTART_LATENCY_B0.val(wl - 2));
            iosf.reg(dq_latctl0_b1(ch, m))
                .modify(DQ_LATCTL0_B1::WRCMD2DQSSTART_LATENCY_B1.val(wl - 2));
            iosf.reg(dq_latctl0_b0(ch, m))
                .modify(DQ_LATCTL0_B0::RDCMD2RCVEN_LATENCY_B0.val(tcl - 3));
            iosf.reg(dq_latctl0_b1(ch, m))
                .modify(DQ_LATCTL0_B1::RDCMD2RCVEN_LATENCY_B1.val(tcl - 3));
            iosf.reg(dq_latctl0_b0(ch, m))
                .modify(DQ_LATCTL0_B0::RDCMD2DATAVALID_LATENCY_B0.val(tcl + 5));
            iosf.reg(dq_latctl0_b1(ch, m))
                .modify(DQ_LATCTL0_B1::RDCMD2DATAVALID_LATENCY_B1.val(tcl + 5));
            // Starting delays: DQ and receive-enable at zero,
            // DQS a half clock later. Setting them through the
            // delay elements also lands the clock-crossing bits.
            for strobe in m as usize * STROBES_PER_MODULE..(m as usize + 1) * STROBES_PER_MODULE {
                delay::set(iosf, Delay::Wdq, ch, strobe, 0);
                delay::set(iosf, Delay::Wdqs, ch, strobe, HALF_CLK);
                delay::set(iosf, Delay::Rcvn, ch, strobe, 0);
            }
            iosf.reg(dq_dbctl0(ch, m)).modify(
                DQ_DBCTL0::RK3_PLUS1SEL_RCVEN_B1.val(0x1)
                    + DQ_DBCTL0::RK3_PLUS1SEL_DQS_B1.val(0x1)
                    + DQ_DBCTL0::RK3_PLUS1SEL_DQ_B1.val(0x1)
                    + DQ_DBCTL0::RK3_PLUS1SEL_RCVEN_B0.val(0x1)
                    + DQ_DBCTL0::RK3_PLUS1SEL_DQS_B0.val(0x1)
                    + DQ_DBCTL0::RK3_PLUS1SEL_DQ_B0.val(0x1)
                    + DQ_DBCTL0::RK3_DBSEL_RCVEN_B1.val(0x1)
                    + DQ_DBCTL0::RK3_DBSEL_DQS_B1.val(0x1)
                    + DQ_DBCTL0::RK3_DBSEL_DQ_B1.val(0x1)
                    + DQ_DBCTL0::RK3_DBSEL_RCVEN_B0.val(0x1)
                    + DQ_DBCTL0::RK3_DBSEL_DQS_B0.val(0x1)
                    + DQ_DBCTL0::RK3_DBSEL_DQ_B0.val(0x1)
                    + DQ_DBCTL0::RK2_PLUS1SEL_RCVEN_B1.val(0x1)
                    + DQ_DBCTL0::RK2_PLUS1SEL_DQS_B1.val(0x1)
                    + DQ_DBCTL0::RK2_PLUS1SEL_DQ_B1.val(0x1)
                    + DQ_DBCTL0::RK2_PLUS1SEL_RCVEN_B0.val(0x1)
                    + DQ_DBCTL0::RK2_PLUS1SEL_DQS_B0.val(0x1)
                    + DQ_DBCTL0::RK2_PLUS1SEL_DQ_B0.val(0x1)
                    + DQ_DBCTL0::RK2_DBSEL_RCVEN_B1.val(0x1)
                    + DQ_DBCTL0::RK2_DBSEL_DQS_B1.val(0x1)
                    + DQ_DBCTL0::RK2_DBSEL_DQ_B1.val(0x1)
                    + DQ_DBCTL0::RK2_DBSEL_RCVEN_B0.val(0x1)
                    + DQ_DBCTL0::RK2_DBSEL_DQS_B0.val(0x1)
                    + DQ_DBCTL0::RK2_DBSEL_DQ_B0.val(0x1),
            );
            iosf.reg(dq_b0vrefctl(ch, m))
                .modify(DQ_B0VREFCTL::VREF_DECODE_B0.val(0xc));
            iosf.reg(dq_b1vrefctl(ch, m))
                .modify(DQ_B1VREFCTL::VREF_DECODE_B1.val(0xc));
            iosf.reg(dq_dftctl(ch, m))
                .modify(DQ_DFTCTL::PSHRT_BIAS_B0.val(0x0) + DQ_DFTCTL::PSHRT_BIAS_B1.val(0x0));
            iosf.reg(dq_ptrctl1(ch, m))
                .modify(DQ_PTRCTL1::RDPTR_INIT.val(0x2) + DQ_PTRCTL1::WRPTR_INIT.val(0x0));
            iosf.reg(dq_dqctl(ch, m)).modify(
                DQ_DQCTL::EARLY_DATAVALID_SEL_B0.val(0x1)
                    + DQ_DQCTL::EARLY_DATAVALID_SEL_B1.val(0x1),
            );
            iosf.reg(dq_dqctl(ch, m)).modify(DQ_DQCTL::BL_MODE.val(1));
            iosf.reg(dq_timing_ctrl(ch, m)).modify(
                DQ_TIMING_CTRL::DQS_OFFSET_EN.val(0x0)
                    + DQ_TIMING_CTRL::CA_TRAINING_MODE.val(0x0)
                    + DQ_TIMING_CTRL::LPDDR2_XDDR3_B.val(0x0),
            );
            iosf.reg(dq_iobonusb0(ch, m))
                .modify(DQ_IOBONUSB0::BONUS_B0.val(0x0));
            iosf.reg(dq_iobonusb1(ch, m))
                .modify(DQ_IOBONUSB1::BONUS_B1.val(0x0));
            iosf.reg(dq_ioctl3(ch, m)).modify(
                DQ_IOCTL3::DQS_PD_OFFSET_B1.val(0x0)
                    + DQ_IOCTL3::DQS_PU_OFFSET_B1.val(0x0)
                    + DQ_IOCTL3::DQS_PD_OFFSET_B0.val(0x0)
                    + DQ_IOCTL3::DQS_PU_OFFSET_B0.val(0x0),
            );
        }

        iosf.reg(cmd_timing_ctrl(ch))
            .modify(CMD_TIMING_CTRL::LPDDR2_XDDR3_B.val(0x0));
        iosf.reg(clkctl_timing_ctrl(ch))
            .modify(CLKCTL_TIMING_CTRL::LPDDR2_XDDR3_B.val(0x0));
    }

    // Byte lanes of the channels that are not populated stay disabled.
    for ch in POPULATED_CHANNELS..DQ_NUM_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_dqmod_ctl(ch, m)).modify(
                DQ_DQMOD_CTL::BYTELANE0_DIS.val(0x1) + DQ_DQMOD_CTL::BYTELANE1_DIS.val(0x1),
            );
        }
    }
    iosf.reg(dq_dqmod_ctl(1, 1))
        .modify(DQ_DQMOD_CTL::BYTELANE0_DIS.val(0x1) + DQ_DQMOD_CTL::BYTELANE1_DIS.val(0x1));
    iosf.reg(dq_dqmod_ctl(1, 2))
        .modify(DQ_DQMOD_CTL::BYTELANE0_DIS.val(0x1) + DQ_DQMOD_CTL::BYTELANE1_DIS.val(0x1));
    iosf.reg(dq_dqmod_ctl(1, 3))
        .modify(DQ_DQMOD_CTL::BYTELANE0_DIS.val(0x1) + DQ_DQMOD_CTL::BYTELANE1_DIS.val(0x1));

    for ch in 0..POPULATED_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_ioctl1(ch, m))
                .modify(DQ_IOCTL1::RXEN_B0.val(0x3) + DQ_IOCTL1::RXEN_B1.val(0x3));
            iosf.reg(dq_ioctl1(ch, m))
                .modify(DQ_IOCTL1::DDRMODE_B0.val(0x0) + DQ_IOCTL1::DDRMODE_B1.val(0x0));
            iosf.reg(dq_ioctl2(ch, m)).modify(
                DQ_IOCTL2::DLYCELL_STATIC_LEGENB_B0.val(0x0)
                    + DQ_IOCTL2::DLYCELL_STATIC_LEGENB_B1.val(0x0),
            );
        }
    }

    let t = &PTR_TUNING[freq as usize];

    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(clkctl_dllpicoder0(ch)).modify(
            CLKCTL_DLLPICODER0::PICODE_GRP3_B0.val(t.clkctl_pi_a)
                + CLKCTL_DLLPICODER0::PICODE_GRP2_B0.val(t.clkctl_pi_a),
        );
        iosf.reg(clkctl_dllpicoder1(ch)).modify(
            CLKCTL_DLLPICODER1::PICODE_WCLK.val(t.clkctl_pi_a)
                + CLKCTL_DLLPICODER1::PICODE_GRP1_B1.val(t.clkctl_pi_a),
        );
        iosf.reg(clkctl_dllpicoder0(ch)).modify(
            CLKCTL_DLLPICODER0::PICODE_GRP3_B1.val(t.clkctl_pi_b)
                + CLKCTL_DLLPICODER0::PICODE_WCTL.val(t.clkctl_pi_b),
        );
        iosf.reg(clkctl_dllpicoder1(ch))
            .modify(CLKCTL_DLLPICODER1::PICODE_CMD_CLKCTL.val(t.clkctl_pi_cmd));
        iosf.reg(clkctl_cfg_reg1(ch)).modify(
            CLKCTL_CFG_REG1::PTRPLUS1_SEL.val(t.clkctl_sel)
                + CLKCTL_CFG_REG1::DEADBAND_SEL.val(t.clkctl_sel),
        );
        iosf.reg(clkctl_cmd_ptrctl(ch)).modify(
            CLKCTL_CMD_PTRCTL::RDPTR_OFFSET6.val(t.clkctl_rdptr6)
                + CLKCTL_CMD_PTRCTL::CMD_DEADBAND_SEL.val(t.clkctl_cmd_db),
        );
        iosf.reg(clkctl_pointer_reg(ch)).modify(
            CLKCTL_POINTER_REG::RDPTR_OFFSET5.val(t.clkctl_rdptr)
                + CLKCTL_POINTER_REG::RDPTR_OFFSET4.val(t.clkctl_rdptr),
        );
        iosf.reg(clkctl_pointer_reg(ch)).modify(
            CLKCTL_POINTER_REG::RDPTR_OFFSET3.val(t.clkctl_rdptr)
                + CLKCTL_POINTER_REG::RDPTR_OFFSET2.val(t.clkctl_rdptr)
                + CLKCTL_POINTER_REG::RDPTR_OFFSET1.val(t.clkctl_rdptr)
                + CLKCTL_POINTER_REG::RDPTR_OFFSET0.val(t.clkctl_rdptr),
        );
        iosf.reg(clkctl_pointer_reg(ch)).modify(
            CLKCTL_POINTER_REG::RDPTR_INIT.val(0x2) + CLKCTL_POINTER_REG::WRPTR_INIT.val(0x0),
        );
        iosf.reg(clkctl_cmd_ptrctl(ch))
            .modify(CLKCTL_CMD_PTRCTL::CMD_PTRPLUS1_SEL.val(t.clkctl_cmd_pp1));
        iosf.reg(clkctl_cfg_reg0(ch)).modify(
            CLKCTL_CFG_REG0::POS1NEG0_SEL.val(0x0) + CLKCTL_CFG_REG0::CKLOW_XCKTRI_B.val(0x0),
        );

        iosf.reg(cmd_dllpicoder0(ch))
            .modify(CMD_DLLPICODER0::PICODE_GRP3_B1.val(0x1));
        iosf.reg(cmd_dllpicoder0(ch)).modify(
            CMD_DLLPICODER0::PICODE_GRP2_B1.val(t.cmd_pi)
                + CMD_DLLPICODER0::PICODE_GRP3_B0.val(t.cmd_pi)
                + CMD_DLLPICODER0::PICODE_WCTL_CMD.val(t.cmd_pi),
        );
        iosf.reg(cmd_dllpicoder1(ch))
            .modify(CMD_DLLPICODER1::PICODE_GRP1_B1.val(t.cmd_pi));
        iosf.reg(cmd_dllpicoder1(ch))
            .modify(CMD_DLLPICODER1::PICODE_CMD.val(t.cmd_pi_cmd));
        iosf.reg(cmd_cfg_reg0(ch)).modify(
            CMD_CFG_REG0::CKECMD_PTRPLUS1_SEL3.val(t.cmd_ckecmd_sel)
                + CMD_CFG_REG0::CKECMD_DEADBAND_SEL3.val(t.cmd_ckecmd_sel)
                + CMD_CFG_REG0::CKECMD_PTRPLUS1_SEL2.val(t.cmd_ckecmd_sel)
                + CMD_CFG_REG0::CKECMD_DEADBAND_SEL2.val(t.cmd_ckecmd_sel)
                + CMD_CFG_REG0::CKECMD_PTRPLUS1_SEL1.val(t.cmd_ckecmd_sel)
                + CMD_CFG_REG0::CKECMD_DEADBAND_SEL1.val(t.cmd_ckecmd_sel)
                + CMD_CFG_REG0::CKECMD_PTRPLUS1_SEL0.val(t.cmd_ckecmd_sel)
                + CMD_CFG_REG0::CKECMD_DEADBAND_SEL0.val(t.cmd_ckecmd_sel),
        );
        iosf.reg(cmd_cfg_reg0(ch))
            .modify(CMD_CFG_REG0::PTRPLUS1_SEL.val(t.cmd_pp1));
        iosf.reg(cmd_cfg_reg0(ch))
            .modify(CMD_CFG_REG0::DEADBAND_SEL.val(t.cmd_db));
        iosf.reg(cmd_pointer_reg(ch))
            .modify(CMD_POINTER_REG::RDPTR_OFFSET5.val(0x0) + CMD_POINTER_REG::WRPTR_INIT.val(0x0));
        iosf.reg(cmd_pointer_reg(ch)).modify(
            CMD_POINTER_REG::CKECMD_RDPTR_OFFSET3.val(t.cmd_ckecmd_rdptr)
                + CMD_POINTER_REG::CKECMD_RDPTR_OFFSET2.val(t.cmd_ckecmd_rdptr)
                + CMD_POINTER_REG::CKECMD_RDPTR_OFFSET1.val(t.cmd_ckecmd_rdptr)
                + CMD_POINTER_REG::CKECMD_RDPTR_OFFSET0.val(t.cmd_ckecmd_rdptr),
        );
        iosf.reg(cmd_pointer_reg(ch))
            .modify(CMD_POINTER_REG::RDPTR_OFFSET0.val(t.cmd_rdptr0));
    }

    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(cmd_pointer_reg(ch))
            .modify(CMD_POINTER_REG::RDPTR_INIT.val(0x2));
        iosf.reg(clkctl_bonusclk_io(ch))
            .modify(CLKCTL_BONUSCLK_IO::BONUS_CLK.val(0x0));
        iosf.reg(cmd_bonuscmd0_io(ch))
            .modify(CMD_BONUSCMD0_IO::BONUS_CMD0.val(0x0));
    }
}

fn static_perf<P: ConfigRegionAccess>(iosf: &Iosf<P>) {
    for ch in 0..POPULATED_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_ovrctl_b0(ch, m)).modify(
                DQ_OVRCTL_B0::DQ_MCHODT_OVR_B0.val(0x0)
                    + DQ_OVRCTL_B0::DM_MCHODT_OVR_B0.val(0x0)
                    + DQ_OVRCTL_B0::DQS_MCHODT_OVR_B0.val(0x0)
                    + DQ_OVRCTL_B0::DQ_MCHODT_OVR_VAL_B0.val(0x0)
                    + DQ_OVRCTL_B0::DM_MCHODT_OVR_VAL_B0.val(0x0)
                    + DQ_OVRCTL_B0::DQS_MCHODT_OVR_VAL_B0.val(0x0)
                    + DQ_OVRCTL_B0::DQ_DIFFAMPEN_OVR_B0.val(0x0)
                    + DQ_OVRCTL_B0::DM_DIFFAMPEN_OVR_B0.val(0x0)
                    + DQ_OVRCTL_B0::DQS_DIFFAMPEN_OVR_B0.val(0x0)
                    + DQ_OVRCTL_B0::DQ_DIFFAMPEN_OVR_VAL_B0.val(0x1)
                    + DQ_OVRCTL_B0::DM_DIFFAMPEN_OVR_VAL_B0.val(0x1)
                    + DQ_OVRCTL_B0::DQS_DIFFAMPEN_OVR_VAL_B0.val(0x1)
                    + DQ_OVRCTL_B0::RXDLL_EN_OVR.val(0x1)
                    + DQ_OVRCTL_B0::RXPI_EN_OVR_VAL.val(0x1)
                    + DQ_OVRCTL_B0::RXPI_EN_OVR.val(0x1)
                    + DQ_OVRCTL_B0::RXDLL_EN_OVR_VAL.val(0x1),
            );
            iosf.reg(dq_ovrctl_b1(ch, m)).modify(
                DQ_OVRCTL_B1::DQ_MCHODT_OVR_B1.val(0x0)
                    + DQ_OVRCTL_B1::DM_MCHODT_OVR_B1.val(0x0)
                    + DQ_OVRCTL_B1::DQS_MCHODT_OVR_B1.val(0x0)
                    + DQ_OVRCTL_B1::DQS_MCHODT_OVR_VAL_B1.val(0x0)
                    + DQ_OVRCTL_B1::DQ_MCHODT_OVR_VAL_B1.val(0x0)
                    + DQ_OVRCTL_B1::DM_MCHODT_OVR_VAL_B1.val(0x0)
                    + DQ_OVRCTL_B1::DQ_DIFFAMPEN_OVR_B1.val(0x0)
                    + DQ_OVRCTL_B1::DM_DIFFAMPEN_OVR_B1.val(0x0)
                    + DQ_OVRCTL_B1::DQS_DIFFAMPEN_OVR_B1.val(0x0)
                    + DQ_OVRCTL_B1::DQ_DIFFAMPEN_OVR_VAL_B1.val(0x1)
                    + DQ_OVRCTL_B1::DM_DIFFAMPEN_OVR_VAL_B1.val(0x1)
                    + DQ_OVRCTL_B1::DQS_DIFFAMPEN_OVR_VAL_B1.val(0x1),
            );
            iosf.reg(dq_ondurctl_b0(ch, m))
                .modify(DQ_ONDURCTL_B0::MCHODT_LENGTH_B0.val(0x8));
            iosf.reg(dq_latctl1(ch, m)).modify(
                DQ_LATCTL1::MCHODT_DELAY_B0.val(0x8) + DQ_LATCTL1::MCHODT_DELAY_B1.val(0x8),
            );
            iosf.reg(dq_ondurctl_b1(ch, m))
                .modify(DQ_ONDURCTL_B1::MCHODT_LENGTH_B1.val(0x8));
            iosf.reg(dq_latctl1(ch, m)).modify(
                DQ_LATCTL1::DIFFAMPEN_DELAY_B1.val(0x4) + DQ_LATCTL1::DIFFAMPEN_DELAY_B0.val(0x4),
            );
            iosf.reg(dq_ondurctl_b0(ch, m))
                .modify(DQ_ONDURCTL_B0::DIFFAMPEN_LENGTH_B0.val(0x8));
            iosf.reg(dq_ondurctl_b1(ch, m))
                .modify(DQ_ONDURCTL_B1::DIFFAMPEN_LENGTH_B1.val(0x8));
            iosf.reg(dq_timing_ctrl(ch, m)).modify(
                DQ_TIMING_CTRL::SEL_FAST_PATH_B0.val(0x0)
                    + DQ_TIMING_CTRL::SEL_FAST_PATH_B1.val(0x0),
            );
        }
    }

    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(clkctl_timing_ctrl(ch)).modify(
            CLKCTL_TIMING_CTRL::SEL_FAST_PATH.val(0x0)
                + CLKCTL_TIMING_CTRL::CLK_SEL_FAST_PATH.val(0x0),
        );
        iosf.reg(cmd_timing_ctrl(ch)).modify(
            CMD_TIMING_CTRL::CKECMD_SEL_FAST_PATH3.val(0x0)
                + CMD_TIMING_CTRL::CKECMD_SEL_FAST_PATH2.val(0x0)
                + CMD_TIMING_CTRL::CKECMD_SEL_FAST_PATH1.val(0x0)
                + CMD_TIMING_CTRL::CKECMD_SEL_FAST_PATH0.val(0x0)
                + CMD_TIMING_CTRL::SEL_FAST_PATH.val(0x0),
        );
    }
}

/// Power-management and power-gating delays that scale with the DRAM
/// clock. PM_DLYREG2 programs one value to all three of its delay fields.
struct PmTiming {
    /// PGSTAGGER clkgate_wait_timer.
    pgstagger_wait: u32,
    /// PGSTAGGER ana2dig_pgdis_stg_delay.
    pgstagger_pgdis: u32,
    /// PM_DLYREG0.
    mpll_ungate: u32,
    mdll_en_stg: u32,
    dllpi_en: u32,
    iobufact: u32,
    /// PM_DLYREG1.
    mdll_vregoff: u32,
    mdll_dis: u32,
    dllpi_dis: u32,
    /// PM_DLYREG2 sfr_dis/mpll_dis/mpll_gate.
    mpll_off: u32,
    /// PM_DLYREG3.
    mdll_vregon: u32,
    mdll_en: u32,
    /// PM_DLYREG4.
    sfr_en: u32,
    mpll_en: u32,
    /// TIMING_CTRL.
    mod2mod_pgdis: u32,
    rst_wait: u32,
    isol2pg: u32,
}

const PM_TIMING: [PmTiming; 3] = [
    // DDR3-800
    PmTiming {
        pgstagger_wait: 0x08,
        pgstagger_pgdis: 0x14,
        mpll_ungate: 0x14,
        mdll_en_stg: 0x08,
        dllpi_en: 0x24,
        iobufact: 0x18,
        mdll_vregoff: 0x08,
        mdll_dis: 0x30,
        dllpi_dis: 0x1c,
        mpll_off: 0x08,
        mdll_vregon: 0x50,
        mdll_en: 0x78,
        sfr_en: 0x190,
        mpll_en: 0x1018,
        mod2mod_pgdis: 0x1c,
        rst_wait: 0x08,
        isol2pg: 0x08,
    },
    // DDR3-1066
    PmTiming {
        pgstagger_wait: 0x0b,
        pgstagger_pgdis: 0x1b,
        mpll_ungate: 0x1b,
        mdll_en_stg: 0x0a,
        dllpi_en: 0x30,
        iobufact: 0x20,
        mdll_vregoff: 0x0b,
        mdll_dis: 0x40,
        dllpi_dis: 0x26,
        mpll_off: 0x0b,
        mdll_vregon: 0x6b,
        mdll_en: 0xa0,
        sfr_en: 0x215,
        mpll_en: 0x1572,
        mod2mod_pgdis: 0x26,
        rst_wait: 0x0b,
        isol2pg: 0x0b,
    },
    // DDR3-1333
    PmTiming {
        pgstagger_wait: 0x0e,
        pgstagger_pgdis: 0x22,
        mpll_ungate: 0x22,
        mdll_en_stg: 0x0d,
        dllpi_en: 0x3c,
        iobufact: 0x28,
        mdll_vregoff: 0x0e,
        mdll_dis: 0x50,
        dllpi_dis: 0x2f,
        mpll_off: 0x0e,
        mdll_vregon: 0x86,
        mdll_en: 0xc8,
        sfr_en: 0x29b,
        mpll_en: 0x1ad1,
        mod2mod_pgdis: 0x2f,
        rst_wait: 0x0e,
        isol2pg: 0x0e,
    },
];

/// Clock gating, the CMD power-management message timings (without them
/// the PHY never wakes from a C6 power gate) and power-gating overrides.
fn clock_gating<P: ConfigRegionAccess>(iosf: &Iosf<P>, freq: DdrFreq) {
    let t = &PM_TIMING[freq as usize];

    for ch in 0..POPULATED_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_clock_gate(ch, m)).modify(
                DQ_CLOCK_GATE::RAWCLK_CG_EN.val(0x0)
                    + DQ_CLOCK_GATE::TX_CG_EN.val(0x0)
                    + DQ_CLOCK_GATE::RX_CG_EN.val(0x0),
            );
            iosf.reg(dq_clock_gate2(ch, m)).modify(
                DQ_CLOCK_GATE2::TXDQDQS_DLL_DYNPI_EN_B0.val(0x0)
                    + DQ_CLOCK_GATE2::TXDQDQS_DLL_DYNPI_EN_B1.val(0x0)
                    + DQ_CLOCK_GATE2::TXRCVEN_DLL_DYNPI_EN_B0.val(0x0)
                    + DQ_CLOCK_GATE2::TXRCVEN_DLL_DYNPI_EN_B1.val(0x0),
            );
            iosf.reg(dq_clock_gate(ch, m)).modify(
                DQ_CLOCK_GATE::TXFIFO_CG_CNT.val(0x18) + DQ_CLOCK_GATE::RXFIFO_CG_CNT.val(0x18),
            );
            iosf.reg(dq_clock_gate2(ch, m)).modify(
                DQ_CLOCK_GATE2::TXDQDQS_DLL_PIDIS_MAXCNT_B1.val(0x18)
                    + DQ_CLOCK_GATE2::TXDQDQS_DLL_PIDIS_MAXCNT_B0.val(0x18)
                    + DQ_CLOCK_GATE2::TXDLL_DYNPI_EN_STG_B0.val(0x0)
                    + DQ_CLOCK_GATE2::TXDLL_DYNPI_EN_STG_B1.val(0x0)
                    + DQ_CLOCK_GATE2::TXRCVEN_DLL_PIDIS_MAXCNT_B0.val(0x18)
                    + DQ_CLOCK_GATE2::TXRCVEN_DLL_PIDIS_MAXCNT_B1.val(0x18),
            );
            iosf.reg(dq_clock_gate3(ch, m)).modify(
                DQ_CLOCK_GATE3::TX_PI_DRV_START_GB.val(0x1)
                    + DQ_CLOCK_GATE3::RX_PI_DRV_START_GB.val(0x1),
            );
            iosf.reg(dq_clock_gate3(ch, m)).modify(
                DQ_CLOCK_GATE3::TX_PI_DRV_STOP_GB.val(0x2)
                    + DQ_CLOCK_GATE3::RX_PI_DRV_STOP_GB.val(0x2),
            );
        }
    }

    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(clkctl_cfg_reg0(ch)).modify(
            CLKCTL_CFG_REG0::CS_BUF_CONTROL.val(0xa)
                + CLKCTL_CFG_REG0::CKE_BUF_CONTROL.val(0x0)
                + CLKCTL_CFG_REG0::ODT_BUF_CONTROL.val(0xa)
                + CLKCTL_CFG_REG0::CLK_BUF_CONTROL.val(0x5),
        );
        iosf.reg(cmd_cfg_reg0(ch)).modify(
            CMD_CFG_REG0::CKE_BUF_CONTROL.val(0x0)
                + CMD_CFG_REG0::CKECMD_SENSE_AMP_CONTROL.val(0x0)
                + CMD_CFG_REG0::SENSE_AMP_CONTROL.val(0x0)
                + CMD_CFG_REG0::TRI_STATE_OVR_VAL.val(0x1)
                + CMD_CFG_REG0::TRI_STATE_OVR_EN.val(0x1),
        );
        iosf.reg(clkctl_clkgate(ch))
            .modify(CLKCTL_CLKGATE::CMDSLICE_EN_B.val(0x0));
        iosf.reg(clkctl_clkgate(ch)).modify(
            CLKCTL_CLKGATE::M1XCLK_GATE_EN.val(0x0) + CLKCTL_CLKGATE::RAWCLK_GATE_EN.val(0x0),
        );
        iosf.reg(clkctl_cmd_cfg_reg2(ch)).modify(
            CLKCTL_CMD_CFG_REG2::CMD_TRI_STATE_OVR_EN.val(0x1)
                + CLKCTL_CMD_CFG_REG2::CMD_TRI_STATE_OVR_VAL.val(0x1),
        );

        iosf.reg(cmd_pgstagger(ch))
            .modify(CMD_PGSTAGGER::IOPWRGATE_DIS.val(0x1));
        iosf.reg(cmd_pgstagger(ch)).modify(
            CMD_PGSTAGGER::PGSTG_MOD_GRP.val(0x0) + CMD_PGSTAGGER::DIG2ANA_PGEN_STG_DELAY.val(0x4),
        );
        iosf.reg(cmd_pgstagger(ch)).modify(
            CMD_PGSTAGGER::CLKGATE_WAIT_TIMER.val(t.pgstagger_wait)
                + CMD_PGSTAGGER::ANA2DIG_PGDIS_STG_DELAY.val(t.pgstagger_pgdis),
        );
        iosf.reg(cmd_clkalign_reg0(ch))
            .modify(CMD_CLKALIGN_REG0::PG_CLKALIGN_EN.val(0x0));

        iosf.reg(cmd_pm_config0(ch)).modify(
            CMD_PM_CONFIG0::PMBLOCK_IOBUFACT_OVR.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_SIGDLL_EN_OVR.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_REFDLL_EN_OVR.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_MDLL_EN_OVR.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_MDLL_VREGON_OVR.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_MPLL_GATE_OVR.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_MPLL_EN_OVR.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_SFR_EN_OVR.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_CLKALIGN_OVR.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_PTR_EN_OVR.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_MPLL_DIVRST_OVR.val(0x0)
                + CMD_PM_CONFIG0::PM_REQ.val(0x0)
                + CMD_PM_CONFIG0::PM_MSG.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_SET_PM_ACK.val(0x0)
                + CMD_PM_CONFIG0::PMBLOCK_SET_PM_NACK.val(0x0)
                + CMD_PM_CONFIG0::PMMSG_WAITTIME.val(0x6)
                + CMD_PM_CONFIG0::PMBLOCK_DIFFAMP_EN_OVR.val(0x0)
                + CMD_PM_CONFIG0::PMMSG_CLK_SYNC_MODE.val(0x0)
                + CMD_PM_CONFIG0::PMMSG_CLKSEL.val(0x0)
                + CMD_PM_CONFIG0::PMMSG_NUM_MODULES.val(0xc),
        );

        iosf.reg(cmd_pm_dlyreg0(ch)).modify(
            CMD_PM_DLYREG0::PMMSG_MPLL_UNGATE_DELAY.val(t.mpll_ungate)
                + CMD_PM_DLYREG0::PMMSG_MDLL_EN_STG_DELAY.val(t.mdll_en_stg)
                + CMD_PM_DLYREG0::PMMSG_DLLPI_EN_DELAY.val(t.dllpi_en)
                + CMD_PM_DLYREG0::PMMSG_IOBUFACT_DELAY.val(t.iobufact),
        );
        iosf.reg(cmd_pm_dlyreg1(ch)).modify(
            CMD_PM_DLYREG1::PMMSG_MDLL_VREGOFF_DELAY.val(t.mdll_vregoff)
                + CMD_PM_DLYREG1::PMMSG_MDLL_DIS_DELAY.val(t.mdll_dis)
                + CMD_PM_DLYREG1::PMMSG_DLLPI_DIS_DELAY.val(t.dllpi_dis),
        );
        iosf.reg(cmd_pm_dlyreg1(ch))
            .modify(CMD_PM_DLYREG1::PMMSG_MDLL_DIS_STG_DELAY.val(0x4));
        iosf.reg(cmd_pm_dlyreg2(ch))
            .modify(CMD_PM_DLYREG2::PMMSG_MPLL_DIVRST_CNT.val(0x10));
        iosf.reg(cmd_pm_dlyreg2(ch)).modify(
            CMD_PM_DLYREG2::PMMSG_SFR_DIS_DELAY.val(t.mpll_off)
                + CMD_PM_DLYREG2::PMMSG_MPLL_DIS_DELAY.val(t.mpll_off)
                + CMD_PM_DLYREG2::PMMSG_MPLL_GATE_DELAY.val(t.mpll_off),
        );
        iosf.reg(cmd_pm_dlyreg3(ch)).modify(
            CMD_PM_DLYREG3::PMMSG_ASRT_IOBUFACT_ALLON0_DELAY.val(0x0)
                + CMD_PM_DLYREG3::PMMSG_ALLON_IOBUFACT_DELAY.val(0x0),
        );
        iosf.reg(cmd_pm_dlyreg3(ch)).modify(
            CMD_PM_DLYREG3::PMMSG_MDLL_VREGON_DELAY.val(t.mdll_vregon)
                + CMD_PM_DLYREG3::PMMSG_MDLL_EN_DELAY.val(t.mdll_en),
        );
        iosf.reg(cmd_pm_dlyreg4(ch)).modify(
            CMD_PM_DLYREG4::PMMSG_SFR_EN_DELAY.val(t.sfr_en)
                + CMD_PM_DLYREG4::PMMSG_MPLL_EN_DELAY.val(t.mpll_en),
        );

        iosf.reg(cmd_clkgate(ch))
            .modify(CMD_CLKGATE::M1XCLK_GATE_EN.val(0x0) + CMD_CLKGATE::RAWCLK_GATE_EN.val(0x0));
        iosf.reg(cmd_clkgate(ch))
            .modify(CMD_CLKGATE::CMDSLICE_EN_B.val(0x0));
        iosf.reg(cmd_timing_ctrl(ch)).modify(
            CMD_TIMING_CTRL::MOD2MOD_PGDIS_STG_DELAY.val(t.mod2mod_pgdis)
                + CMD_TIMING_CTRL::RST_WAIT_TIMER.val(t.rst_wait)
                + CMD_TIMING_CTRL::ISOL2PG_DELAY.val(t.isol2pg),
        );
        iosf.reg(cmd_timing_ctrl(ch)).modify(
            CMD_TIMING_CTRL::MOD2MOD_PGEN_STG_DELAY.val(0x4)
                + CMD_TIMING_CTRL::GLOBAL_PWRGATE_EN.val(0x0),
        );
        iosf.reg(cmd_pgovr(ch)).modify(
            CMD_PGOVR::CMD_PLLCOMP_PWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_CLKCTL_PWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_CMD_PWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_DQ3_PWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_DQ2_PWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_DQ1_PWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_DQ0_PWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_PLLCOMP_IOPWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_CLKCTL_IOPWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_CMD_IOPWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_DQ3_IOPWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_DQ2_IOPWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_DQ1_IOPWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::CMD_DQ0_IOPWRGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::PMBLOCK_DDLL_EN_OVR.val(0x0)
                + CMD_PGOVR::IOISOLEN_OVR.val(0x0)
                + CMD_PGOVR::ISOLEN_OVR.val(0x0)
                + CMD_PGOVR::CLKGATE_EN_OVR.val(0x0)
                + CMD_PGOVR::PGPHYRST_B_OVR.val(0x0)
                + CMD_PGOVR::COMP_TESTCLK_OVR.val(0x0),
        );
    }

    iosf.reg(cmpctrl()).modify(CMPCTRL::CLKGATE_DIS.val(0x1));
}

fn ddr3_reset<P: ConfigRegionAccess>(iosf: &Iosf<P>) {
    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(clkctl_ddr3resetctl(ch)).modify(
            CLKCTL_DDR3RESETCTL::DDR3_RESET_SET.val(0x0)
                + CLKCTL_DDR3RESETCTL::DDR3_RESET_RESET.val(0x1),
        );
        udelay(10);
        iosf.reg(clkctl_ddr3resetctl(ch)).modify(
            CLKCTL_DDR3RESETCTL::DDR3_RESET_SET.val(0x1)
                + CLKCTL_DDR3RESETCTL::DDR3_RESET_RESET.val(0x0),
        );
    }
}

fn dll_init<P: ConfigRegionAccess>(iosf: &Iosf<P>) {
    iosf.reg(masterrstn()).modify(MASTERRSTN::PHYRST_N.val(0x1));
    udelay(25);

    // Master DLL regulators out of bypass.
    for ch in 0..POPULATED_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_mdllctl(ch, m))
                .modify(DQ_MDLLCTL::VREG_BYPASS.val(0x0));
        }
        iosf.reg(cmd_mdllctl(ch))
            .modify(CMD_MDLLCTL::VREG_BYPASS.val(0x0));
        iosf.reg(clkctl_mdllctl(ch))
            .modify(CLKCTL_MDLLCTL::VREG_BYPASS.val(0x0));
    }

    // Regulator reference trim.
    for ch in 0..POPULATED_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_mdllctl(ch, m))
                .modify(DQ_MDLLCTL::VREF_TUNING.val(0x8) + DQ_MDLLCTL::VREG_CUR_SEL.val(0x3));
        }
        iosf.reg(clkctl_mdllctl(ch))
            .modify(CLKCTL_MDLLCTL::VREF_TUNING.val(0x8) + CLKCTL_MDLLCTL::VREG_CUR_SEL.val(0x3));
        iosf.reg(cmd_mdllctl(ch))
            .modify(CMD_MDLLCTL::VREF_TUNING.val(0x8) + CMD_MDLLCTL::VREG_CUR_SEL.val(0x3));
    }

    // Enable the regulators, then the master DLLs.
    for ch in 0..POPULATED_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_mdllctl(ch, m))
                .modify(DQ_MDLLCTL::VREG_EN.val(0x1));
        }
        iosf.reg(cmd_mdllctl(ch))
            .modify(CMD_MDLLCTL::VREG_EN_H.val(0x1));
        iosf.reg(clkctl_mdllctl(ch))
            .modify(CLKCTL_MDLLCTL::VREG_EN_H.val(0x1));
    }
    for ch in 0..POPULATED_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_mdllctl(ch, m))
                .modify(DQ_MDLLCTL::MDLL_EN.val(0x1));
        }
        iosf.reg(clkctl_mdllctl(ch))
            .modify(CLKCTL_MDLLCTL::MDLL_EN.val(0x1));
        iosf.reg(cmd_mdllctl(ch))
            .modify(CMD_MDLLCTL::MDLL_EN.val(0x1));
    }

    // Leave DLL-less mode.
    for ch in 0..POPULATED_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_dllctl1(ch, m))
                .modify(DQ_DLLCTL1::NODLL_EN.val(0x0));
        }
        iosf.reg(clkctl_mdllctl(ch))
            .modify(CLKCTL_MDLLCTL::NODLL_EN.val(0x0));
        iosf.reg(cmd_ioctlb0(ch))
            .modify(CMD_IOCTLB0::NODLL_EN.val(0x0));
    }

    // Enable the per-group TX phase interpolators and output drivers.
    for ch in 0..POPULATED_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_dlltxctl(ch, m)).modify(
                DQ_DLLTXCTL::GRP_PI_EN_B0.val(0xf)
                    + DQ_DLLTXCTL::GRP_PI_EN_B1.val(0xe)
                    + DQ_DLLTXCTL::GRP_OUT_EN_B0.val(0xf),
            );
            iosf.reg(dq_dlltxctl(ch, m))
                .modify(DQ_DLLTXCTL::GRP_OUT_EN_B1.val(0xe));
        }
        iosf.reg(clkctl_dlltxctl(ch)).modify(
            CLKCTL_DLLTXCTL::GRP_PI_EN_B0.val(0xb)
                + CLKCTL_DLLTXCTL::GRP_PI_EN_B1.val(0xd)
                + CLKCTL_DLLTXCTL::GRP_OUT_EN_B0.val(0xb),
        );
        iosf.reg(clkctl_dlltxctl(ch))
            .modify(CLKCTL_DLLTXCTL::GRP_OUT_EN_B1.val(0xd));
        iosf.reg(cmd_dlltxctl(ch)).modify(
            CMD_DLLTXCTL::GRP_PI_EN_B0.val(0xf)
                + CMD_DLLTXCTL::GRP_PI_EN_B1.val(0x6)
                + CMD_DLLTXCTL::GRP_OUT_EN_B0.val(0xf),
        );
        iosf.reg(cmd_dlltxctl(ch))
            .modify(CMD_DLLTXCTL::GRP_OUT_EN_B1.val(0x6));
    }

    // Enable the DQ receivers and hold the RX DLL/PI on.
    for ch in 0..POPULATED_CHANNELS {
        for m in 0..DQ_NUM_MODULES {
            iosf.reg(dq_dllrxctl(ch, m)).modify(
                DQ_DLLRXCTL::RXDQ_OUT_EN_B0.val(0x1)
                    + DQ_DLLRXCTL::RXDQ_OUT_EN_B1.val(0x1)
                    + DQ_DLLRXCTL::RXDQS_OUT_EN_B0.val(0x1)
                    + DQ_DLLRXCTL::RXDQS_OUT_EN_B1.val(0x1),
            );
            iosf.reg(dq_ovrctl_b0(ch, m))
                .modify(DQ_OVRCTL_B0::RXPI_EN_OVR.val(0x1));
            iosf.reg(dq_ovrctl_b0(ch, m))
                .modify(DQ_OVRCTL_B0::RXPI_EN_OVR_VAL.val(0x1));
            iosf.reg(dq_ovrctl_b0(ch, m))
                .modify(DQ_OVRCTL_B0::RXDLL_EN_OVR_VAL.val(0x1));
            iosf.reg(dq_ovrctl_b0(ch, m))
                .modify(DQ_OVRCTL_B0::RXDLL_EN_OVR.val(0x1));
            iosf.reg(dq_dllctl1(ch, m))
                .modify(DQ_DLLCTL1::TX_DIG_DLL_DLYCELL_EN.val(0x0));
        }
        iosf.reg(clkctl_dllctl1(ch))
            .modify(CLKCTL_DLLCTL1::TX_DIG_DLL_DLYCELL_EN.val(0x0));
        iosf.reg(cmd_dllctl1(ch))
            .modify(CMD_DLLCTL1::TX_DIG_DLL_DLYCELL_EN.val(0x0));
    }
}

/// Clock alignment walks a daisy chain of six modules per channel, visited
/// in the order DQ0, DQ1, CMD, CLKCTL, DQ2, DQ3 with IDs 1, 2, 0, 3, 4, 5.
fn hmc_init<P: ConfigRegionAccess>(iosf: &Iosf<P>) -> Result<(), ServiceError> {
    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(cmd_clkalign_reg0(ch))
            .modify(CMD_CLKALIGN_REG0::CKALIGN_MODE.val(0x2));
    }

    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(dq_clkalign_reg2(ch, 0))
            .modify(DQ_CLKALIGN_REG2::CKALIGN_MODID.val(0x1));
        iosf.reg(dq_clkalign_reg2(ch, 1))
            .modify(DQ_CLKALIGN_REG2::CKALIGN_MODID.val(0x2));
        iosf.reg(cmd_clkalign_reg2(ch)).modify(
            CMD_CLKALIGN_REG2::CKALIGN_MODID.val(0x0)
                + CMD_CLKALIGN_REG2::CKALIGN_NUMMOD.val(0x6)
                + CMD_CLKALIGN_REG2::CKALIGN_SDIRMOD.val(0x3),
        );
        iosf.reg(clkctl_clkalign_reg2(ch))
            .modify(CLKCTL_CLKALIGN_REG2::CKALIGN_MODID.val(0x3));
        iosf.reg(dq_clkalign_reg2(ch, 2))
            .modify(DQ_CLKALIGN_REG2::CKALIGN_MODID.val(0x4));
        iosf.reg(dq_clkalign_reg2(ch, 3))
            .modify(DQ_CLKALIGN_REG2::CKALIGN_MODID.val(0x5));
    }

    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(cmd_clkalign_reg1(ch)).modify(
            CMD_CLKALIGN_REG1::SAMPLE_SIZE.val(0x18)
                + CMD_CLKALIGN_REG1::MAXSAMPLE_CNT.val(0x12)
                + CMD_CLKALIGN_REG1::MACRO_PISTEP.val(0x8)
                + CMD_CLKALIGN_REG1::MICRO_PISTEP.val(0x1),
        );
        iosf.reg(cmd_clkalign_reg2(ch))
            .modify(CMD_CLKALIGN_REG2::DLL_WT_CNT.val(0x1f));
    }

    // Leave alignment idle; identical at every supported frequency.
    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(cmd_clkalign_reg0(ch))
            .modify(CMD_CLKALIGN_REG0::PG_CLKALIGN_EN.val(0x0));
    }
    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(cmd_clkalign_reg0(ch))
            .modify(CMD_CLKALIGN_REG0::START_CLKALIGN.val(0x0));
        poll(
            iosf,
            cmd_clkalign_reg0(ch),
            CMD_CLKALIGN_REG0::START_CLKALIGN,
            0,
            "clock alignment",
        )?;
    }
    Ok(())
}

fn wr_ptr_init<P: ConfigRegionAccess>(iosf: &Iosf<P>) {
    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(cmd_pointer_reg(ch))
            .modify(CMD_POINTER_REG::WRPTR_EN.val(0x1));
        iosf.reg(clkctl_pointer_reg(ch))
            .modify(CLKCTL_POINTER_REG::WRPTR_EN.val(0x1));
    }
}

/// Delay-compensation selects, per DRAM clock.
struct CompTuning {
    /// DLYSEL rxdll/txdll.
    dll_dcmp: u32,
    /// DLYSEL dq/dqs/clk/ctl.
    sig_dcmp: u32,
    /// DLYSEL cmd.
    cmd_dcmp: u32,
}

const COMP_TUNING: [CompTuning; 3] = [
    // DDR3-800
    CompTuning {
        dll_dcmp: 0xe,
        sig_dcmp: 0xc,
        cmd_dcmp: 0xa,
    },
    // DDR3-1066
    CompTuning {
        dll_dcmp: 0xb,
        sig_dcmp: 0x9,
        cmd_dcmp: 0x8,
    },
    // DDR3-1333
    CompTuning {
        dll_dcmp: 0x9,
        sig_dcmp: 0x7,
        cmd_dcmp: 0x5,
    },
];

fn comp_init<P: ConfigRegionAccess>(iosf: &Iosf<P>, freq: DdrFreq) {
    let t = &COMP_TUNING[freq as usize];

    iosf.reg(cmpctrl())
        .modify(CMPCTRL::STL_DELAY.val(0xa) + CMPCTRL::STEP_CNT.val(0x8));
    iosf.reg(nmscntrl()).modify(NMSCNTRL::NMS_CNT.val(0x80));
    iosf.reg(latch1ctl()).modify(LATCH1CTL::CHNL_CNT.val(0x1));
    iosf.reg(tcocntctrl())
        .modify(TCOCNTCTRL::TCO_CNT_VAL.val(0x1));
    iosf.reg(chnlbufstatic()).modify(
        CHNLBUFSTATIC::CC_STAT_DRV_SEG_EN.val(0xf)
            + CHNLBUFSTATIC::CC_STAT_ODT_SEG_EN.val(0x3)
            + CHNLBUFSTATIC::DQ_STAT_DRV_SEG_EN.val(0xf)
            + CHNLBUFSTATIC::DQ_STAT_ODT_SEG_EN.val(0x3),
    );

    // One compensation engine serves both channels, so its per-channel pad
    // targets are programmed whether or not the channel is populated.
    for ch in 0..COMP_NUM_CHANNELS {
        iosf.reg(dqvref(ch)).modify(
            DQVREF::EXT_VREF_SEL.val(0x0)
                + DQVREF::VREF_SEL.val(0x0)
                + DQVREF::DQ_DRV_PD_INT.val(0xc)
                + DQVREF::DQ_ODT_PU_EXT.val(0x1a)
                + DQVREF::DQ_ODT_PD_INT.val(0xc),
        );
        iosf.reg(dqvref(ch)).modify(DQVREF::DQ_DRV_PD_EXT.val(0xd));
        iosf.reg(cmdvref(ch))
            .modify(CMDVREF::CMD_DRV_PD_EXT.val(0x15));
        iosf.reg(cmdvref(ch))
            .modify(CMDVREF::CMD_DRV_PD_INT.val(0xc));
        iosf.reg(clkvref(ch))
            .modify(CLKVREF::CLK_DRV_PD_EXT.val(0x14));
        iosf.reg(clkvref(ch)).modify(
            CLKVREF::CLK_DRV_PD_INT.val(0xc)
                + CLKVREF::CLK_ODT_PU_EXT.val(0x1a)
                + CLKVREF::CLK_ODT_PD_INT.val(0xc),
        );
        iosf.reg(dqsvref(ch))
            .modify(DQSVREF::DQS_DRV_PD_EXT.val(0xd));
        iosf.reg(dqsvref(ch)).modify(
            DQSVREF::DQS_DRV_PD_INT.val(0xc)
                + DQSVREF::DQS_ODT_PU_EXT.val(0x1a)
                + DQSVREF::DQS_ODT_PD_INT.val(0xc),
        );
        iosf.reg(ctlvref(ch))
            .modify(CTLVREF::CTL_DRV_PD_EXT.val(0xf));
        iosf.reg(ctlvref(ch))
            .modify(CTLVREF::CTL_DRV_PD_INT.val(0xc));
        iosf.reg(tcovref(ch)).modify(
            TCOVREF::CLK_TCO_VREF.val(0xc)
                + TCOVREF::DQS_TCO_VREF.val(0xc)
                + TCOVREF::DQ_TCO_VREF.val(0xc),
        );
        iosf.reg(dlysel(ch)).modify(
            DLYSEL::TXDLL_DCMP_SEL.val(t.dll_dcmp) + DLYSEL::RXDLL_DCMP_SEL.val(t.dll_dcmp),
        );
        iosf.reg(dlysel(ch)).modify(
            DLYSEL::CTL_DCMP_SEL.val(t.sig_dcmp)
                + DLYSEL::CMD_DCMP_SEL.val(t.cmd_dcmp)
                + DLYSEL::CLK_DCMP_SEL.val(t.sig_dcmp)
                + DLYSEL::DQS_DCMP_SEL.val(t.sig_dcmp)
                + DLYSEL::DQ_DCMP_SEL.val(t.sig_dcmp),
        );
        iosf.reg(tcodrambufodt(ch)).modify(
            TCODRAMBUFODT::TCO_DBUF_PU_STATIC.val(0x1) + TCODRAMBUFODT::TCO_DBUF_PD_STATIC.val(0x0),
        );
    }

    iosf.reg(compen4()).modify(COMPEN4::CMDCTL_TCO_EN.val(0x0));
}

fn iobuf_act<P: ConfigRegionAccess>(iosf: &Iosf<P>) {
    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(cmd_cfg_reg0(ch))
            .modify(CMD_CFG_REG0::IOBUFACT_RST_N.val(0x1));
    }
}

/// The PHY: MPLL, static settings and starting delays, power management,
/// DDR3 reset, DLLs, clock alignment, pointers, compensation and IO buffers.
pub fn init<P: ConfigRegionAccess>(iosf: &Iosf<P>, p: &Params) -> Result<(), ServiceError> {
    fstart_log::info!("baytrail: DDRIO PHY init");
    sfr_vol_sel(iosf);
    pll_init(iosf, p.freq)?;
    static_init(iosf, p.freq, p.cas_latency, p.write_latency);
    static_perf(iosf);
    clock_gating(iosf, p.freq);
    ddr3_reset(iosf);
    dll_init(iosf);
    hmc_init(iosf)?;
    wr_ptr_init(iosf);
    comp_init(iosf, p.freq);
    iobuf_act(iosf);
    Ok(())
}

/// Release the DQ FIFOs and tell CMD power management the sideband init
/// is done.
pub fn pre_jedec<P: ConfigRegionAccess>(iosf: &Iosf<P>) {
    // Release the DQ FIFOs.
    for m in 0..DQ_NUM_MODULES {
        for ch in 0..POPULATED_CHANNELS {
            iosf.reg(dq_ptrctl1(ch, m))
                .modify(DQ_PTRCTL1::DQ_FIFO_RST_N.val(0x1));
        }
    }

    for ch in 0..POPULATED_CHANNELS {
        iosf.reg(cmd_pm_config0(ch))
            .modify(CMD_PM_CONFIG0::SPID_INIT_COMPLETE.val(0x1));
    }
}
