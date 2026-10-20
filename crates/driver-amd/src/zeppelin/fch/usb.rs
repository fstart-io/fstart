// SPDX-License-Identifier: MIT OR Apache-2.0
//! The xHCI controller of a die (behind 7.1 of the die's root bus, function
//! 3). The vendor FchTaishanPei reset task
//! af48 programs it through SMN; Zeppelin has no SMU USB messages (sp4r2
//! report FCH_TAISHAN, cold path, vendor defaults).
use super::smn_modify;
use crate::{fam17h::registers::Smn, zeppelin::Package};
use tock_registers::register_bitfields;

const CONTROLLER: u32 = 0x16d8_0000;
const R014: u32 = CONTROLLER + 0x014;
pub(super) const RESET_CONTROL: u32 = CONTROLLER + 0x100;
const STATUS_108: u32 = CONTROLLER + 0x108;
const R118: u32 = CONTROLLER + 0x118;
const PORT_DISABLE: u32 = CONTROLLER + 0x128;
const PORT_130: u32 = CONTROLLER + 0x130;
pub(super) const R14C: u32 = CONTROLLER + 0x14c;

/// NBIF strap of the xHCI function.
pub(super) const NBIF_STRAP: u32 = 0x1013_4600;

const PHYS: u32 = 4;
const fn usb3_phy(phy: u32, register: u32) -> u32 {
    0x16d0_4000 + phy * 0x400 + register
}
const fn phy(phy: u32, register: u32) -> u32 {
    0x16d0_8000 + phy * 0x400 + register
}
const fn usb2_phy(phy: u32, register: u32) -> u32 {
    0x16d0_c000 + phy * 0x400 + register
}
/// USB2 PHY parameter of the vendor template, per die.
const USB2_PHY_PARAMETER: [u32; super::super::MAX_DIES] = [0x01b3_c953, 0x01b3_cd56];

register_bitfields![u32,
    /// The controller's reset bits; bit 0 is released first, bit 2 then
    /// bit 1 after the PHY setup.
    pub(super) ResetControl [BIT0 OFFSET(0) NUMBITS(1) [], BIT1 OFFSET(1) NUMBITS(1) [],
        BIT2 OFFSET(2) NUMBITS(1) []],
    /// USB3 PHY tuning: vendor CBS defaults, the same for every PHY.
    Usb3Tuning [LOS_BIAS OFFSET(5) NUMBITS(3) [], DEEMPH_3P5 OFFSET(18) NUMBITS(6) [],
        DEEMPH_6 OFFSET(24) NUMBITS(6) []],
    Usb3Swing [SWING OFFSET(0) NUMBITS(7) [], VBOOST OFFSET(7) NUMBITS(3) []]
];

/// Polls of the 0x108 busy wait, 1 us apart; the vendor code continues
/// after 1 ms.
const STATUS_POLLS: u32 = 1000;

fn wait_108(smn: &mut impl Smn, delay_us: &mut impl FnMut(u32)) {
    let mut polls = 0;
    while smn.read(STATUS_108) & 0x300 != 0 && polls < STATUS_POLLS {
        delay_us(1);
        polls += 1;
    }
}

fn phy_init(smn: &mut impl Smn, die: u8) {
    for n in 0..PHYS {
        smn.write(phy(n, 0x200), 0x39);
        smn.write(usb2_phy(n, 0), USB2_PHY_PARAMETER[usize::from(die)]);
        smn.update(phy(n, 0x8), 0x380, 0x100);
    }
    smn.update(R118, 1 << 12, 0); // vendor PCD 0x97
}

fn post_reset(smn: &mut impl Smn) {
    smn.write(0x16c0_c11c, 0x2004_010a);
    smn.write(0x16c0_c12c, 0x0a43_487d);
    for register in (0x16c0_c2c0..=0x16c0_c2cc).step_by(4) {
        smn.write(register, 0x010a_0002);
    }

    for n in 0..PHYS {
        smn_modify(
            smn,
            phy(n, 0x4),
            Usb3Tuning::LOS_BIAS.val(5)
                + Usb3Tuning::DEEMPH_3P5.val(0x1c)
                + Usb3Tuning::DEEMPH_6.val(0x1c),
        );
        smn_modify(
            smn,
            phy(n, 0x8),
            Usb3Swing::SWING.val(0x7f) + Usb3Swing::VBOOST.val(2),
        );
        // The vendor code first writes the rx_eq tuning value here and then
        // overwrites the field with 0xb80, which leaves rx_eq at 3.
        smn.update(usb3_phy(n, 0x18), 0xfc0, 0xb80);
        smn.write(usb3_phy(n, 0x98), 1);
    }
}

/// Bring the xHCI of die `die` out of reset (cold path). `smn` is the die's.
pub fn init(smn: &mut impl Smn, die: u8, package: Package, delay_us: &mut impl FnMut(u32)) {
    smn.update(R14C, 0, 1 << 31);

    smn.write(R014, 0);
    wait_108(smn, delay_us);
    smn_modify(smn, RESET_CONTROL, ResetControl::BIT0::SET);
    smn_modify(
        smn,
        RESET_CONTROL,
        ResetControl::BIT1::SET + ResetControl::BIT2::SET,
    );
    delay_us(100);

    // SP4 has only two of the four ports; SP4r2 keeps the reset values.
    if package == Package::Sp4 {
        smn.write(PORT_130, 0x22);
        smn.write(PORT_DISABLE, 0x000c_000c);
    }

    phy_init(smn, die);

    smn_modify(smn, RESET_CONTROL, ResetControl::BIT0::CLEAR);
    smn_modify(smn, RESET_CONTROL, ResetControl::BIT2::CLEAR);
    delay_us(200);
    smn_modify(smn, RESET_CONTROL, ResetControl::BIT1::CLEAR);

    post_reset(smn);
}
