//! P-unit (power management unit) registers, IOSF port 0x04.

use super::iosf::{Iosf, IosfReg, PUNIT};
use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::ReadWriteable;
use tock_registers::register_bitfields;

register_bitfields![u32,
    pub SB_BIOS_CONFIG [
        USB_CACHING_EN OFFSET(0) NUMBITS(1) [],
        PCIE_PLLOFFOK OFFSET(1) NUMBITS(1) [],
        PS2_EN_VCC OFFSET(2) NUMBITS(1) [],
        PS2_EN_VNN OFFSET(3) NUMBITS(1) [],
        GFX_TURBO_DIS OFFSET(7) NUMBITS(1) [],
        DDRIO_PWRGATE OFFSET(8) NUMBITS(1) [],
        PDM_MODE OFFSET(16) NUMBITS(1) [],
        PERF_MODE OFFSET(17) NUMBITS(1) [],
        EFF_DUAL_CH_DIS OFFSET(28) NUMBITS(1) [],
        EFF_ECC OFFSET(29) NUMBITS(1) [],
        DUAL_CH_DIS OFFSET(30) NUMBITS(1) [],
        ECC_EN OFFSET(31) NUMBITS(1) []
    ],
    pub BIOS_RESET_CPL [ RESET_DONE OFFSET(0) NUMBITS(1) [], ALL_DONE OFFSET(1) NUMBITS(1) [] ],
    /// Island power gating: two bits per island, 3 = power gated.
    pub PWRGT [ ISLANDS OFFSET(0) NUMBITS(32) [] ]
];

pub const SOC_POWER_BUDGET: IosfReg = PUNIT.reg(0x02);
pub const SOC_ENERGY_CREDIT: IosfReg = PUNIT.reg(0x03);
pub const BIOS_RESET_CPL_REG: IosfReg<BIOS_RESET_CPL::Register> = PUNIT.reg(0x05);
pub const SB_BIOS_CONFIG_REG: IosfReg<SB_BIOS_CONFIG::Register> = PUNIT.reg(0x06);
pub const PWRGT_CONTROL: IosfReg<PWRGT::Register> = PUNIT.reg(0x60);
pub const PWRGT_STATUS: IosfReg<PWRGT::Register> = PUNIT.reg(0x61);
pub const PTMC: IosfReg = PUNIT.reg(0x80);
pub const GFXT: IosfReg = PUNIT.reg(0x88);
pub const VEDT: IosfReg = PUNIT.reg(0x89);
pub const ISPT: IosfReg = PUNIT.reg(0x8c);
pub const PTPS: IosfReg = PUNIT.reg(0xb2);
pub const TE_AUX: [IosfReg; 4] = [
    PUNIT.reg(0xb5),
    PUNIT.reg(0xb6),
    PUNIT.reg(0xb7),
    PUNIT.reg(0xb8),
];
pub const TTE_VRICCMAX: IosfReg = PUNIT.reg(0xb9);
pub const TTE_VRHOT: IosfReg = PUNIT.reg(0xba);
pub const TTE_XXPROCHOT: IosfReg = PUNIT.reg(0xbb);
pub const TTE_SLM0: IosfReg = PUNIT.reg(0xbc);
pub const TTE_SLM1: IosfReg = PUNIT.reg(0xbd);
pub const TTE_SWT: IosfReg = PUNIT.reg(0xbf);
pub const GPU_EC_VIRUS: IosfReg = PUNIT.reg(0xd2);

/// Voltage regulator PS2 low-power mode, C0 stepping and later.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VrPs2 {
    pub vnn: bool,
    pub vcc: bool,
}

/// Start the P-unit: performance and PDM mode, the optional VR PS2 modes,
/// then BIOS reset complete (coreboot romstage `punit_init`).
pub fn init<P: ConfigRegionAccess>(iosf: &Iosf<P>, ps2: VrPs2) {
    let mut set = SB_BIOS_CONFIG::PERF_MODE::SET + SB_BIOS_CONFIG::PDM_MODE::SET;
    if super::stepping() >= super::Stepping::C0 && (ps2.vnn || ps2.vcc) {
        fstart_log::debug!("baytrail: VR PS2 mode vnn={} vcc={}", ps2.vnn, ps2.vcc);
        if ps2.vnn {
            set += SB_BIOS_CONFIG::PS2_EN_VNN::SET;
        }
        if ps2.vcc {
            set += SB_BIOS_CONFIG::PS2_EN_VCC::SET;
        }
    }
    iosf.reg(SB_BIOS_CONFIG_REG).modify(set);
    // BIOS_RESET_CPL takes a single-byte write.
    iosf.write_bytes(
        PUNIT,
        BIOS_RESET_CPL_REG.address(),
        (BIOS_RESET_CPL::ALL_DONE::SET + BIOS_RESET_CPL::RESET_DONE::SET).value,
        0x1,
    );
}
