// SPDX-License-Identifier: MIT OR Apache-2.0
//! The xHCI controller of a die (behind 7.1 of the die's root bus, function
//! 3). The vendor FchTaishanPei reset task
//! af48 programs it through SMN; Zeppelin has no SMU USB messages (sp4r2
//! report FCH_TAISHAN, cold path, vendor defaults).
use crate::{
    smn::{Smn, SmnReg},
    soc::zeppelin::Package,
};
use crate::ccx::tsc::DelayNs;
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    /// The controller's reset bits; bit 0 is released first, bit 2 then
    /// bit 1 after the PHY setup.
    pub(super) ResetControl [BIT0 OFFSET(0) NUMBITS(1) [], BIT1 OFFSET(1) NUMBITS(1) [],
        BIT2 OFFSET(2) NUMBITS(1) []],
    pub(super) R14c [BIT31 OFFSET(31) NUMBITS(1) []],
    Status108 [BUSY OFFSET(8) NUMBITS(2) []],
    R118 [
        /// Vendor PCD 0x97.
        PCD_97 OFFSET(12) NUMBITS(1) []
    ],
    /// USB3 PHY tuning: vendor CBS defaults, the same for every PHY.
    Usb3Tuning [LOS_BIAS OFFSET(5) NUMBITS(3) [], DEEMPH_3P5 OFFSET(18) NUMBITS(6) [],
        DEEMPH_6 OFFSET(24) NUMBITS(6) []],
    Usb3Swing [SWING OFFSET(0) NUMBITS(7) [], VBOOST OFFSET(7) NUMBITS(3) []],
    Usb3RxEq [CONTROL OFFSET(6) NUMBITS(6) []]
];

const CONTROLLER: u32 = 0x16d8_0000;
const R014: SmnReg = SmnReg::new(CONTROLLER + 0x014);
pub(super) const RESET_CONTROL: SmnReg<ResetControl::Register> = SmnReg::new(CONTROLLER + 0x100);
const STATUS_108: SmnReg<Status108::Register> = SmnReg::new(CONTROLLER + 0x108);
const R118: SmnReg<R118::Register> = SmnReg::new(CONTROLLER + 0x118);
const PORT_DISABLE: SmnReg = SmnReg::new(CONTROLLER + 0x128);
const PORT_130: SmnReg = SmnReg::new(CONTROLLER + 0x130);
pub(super) const R14C: SmnReg<R14c::Register> = SmnReg::new(CONTROLLER + 0x14c);

/// NBIF strap of the xHCI function.
pub(super) const NBIF_STRAP: SmnReg<super::NbifStrap::Register> = SmnReg::new(0x1013_4600);

const PHYS: u32 = 4;
const fn usb3_phy<R: tock_registers::RegisterLongName>(phy: u32, register: u32) -> SmnReg<R> {
    SmnReg::new(0x16d0_4000 + phy * 0x400 + register)
}
const fn phy<R: tock_registers::RegisterLongName>(phy: u32, register: u32) -> SmnReg<R> {
    SmnReg::new(0x16d0_8000 + phy * 0x400 + register)
}
const fn usb2_phy(phy: u32) -> SmnReg {
    SmnReg::new(0x16d0_c000 + phy * 0x400)
}
/// USB2 PHY parameter of the vendor template, per die.
const USB2_PHY_PARAMETER: [u32; crate::soc::zeppelin::MAX_DIES] = [0x01b3_c953, 0x01b3_cd56];

/// Polls of the 0x108 busy wait, 1 us apart; the vendor code continues
/// after 1 ms.
const STATUS_POLLS: u32 = 1000;

fn wait_108(smn: &Smn<impl ConfigRegionAccess>, delay: &mut impl DelayNs) {
    let mut polls = 0;
    while smn.reg(STATUS_108).read(Status108::BUSY) != 0 && polls < STATUS_POLLS {
        delay.delay_us(1);
        polls += 1;
    }
}

fn phy_init(smn: &Smn<impl ConfigRegionAccess>, die: u8) {
    for n in 0..PHYS {
        smn.reg(phy::<()>(n, 0x200)).set(0x39);
        smn.reg(usb2_phy(n))
            .set(USB2_PHY_PARAMETER[usize::from(die)]);
        smn.reg(phy(n, 0x8)).modify(Usb3Swing::VBOOST.val(2));
    }
    smn.reg(R118).modify(R118::PCD_97::CLEAR);
}

/// Vendor values of unknown meaning, written after the reset.
const POST_RESET: [(u32, u32); 6] = [
    (0x16c0_c11c, 0x2004_010a),
    (0x16c0_c12c, 0x0a43_487d),
    (0x16c0_c2c0, 0x010a_0002),
    (0x16c0_c2c4, 0x010a_0002),
    (0x16c0_c2c8, 0x010a_0002),
    (0x16c0_c2cc, 0x010a_0002),
];

fn post_reset(smn: &Smn<impl ConfigRegionAccess>) {
    for (address, value) in POST_RESET {
        smn.reg(SmnReg::<()>::new(address)).set(value);
    }

    for n in 0..PHYS {
        smn.reg(phy(n, 0x4)).modify(
            Usb3Tuning::LOS_BIAS.val(5)
                + Usb3Tuning::DEEMPH_3P5.val(0x1c)
                + Usb3Tuning::DEEMPH_6.val(0x1c),
        );
        smn.reg(phy(n, 0x8))
            .modify(Usb3Swing::SWING.val(0x7f) + Usb3Swing::VBOOST.val(2));
        // The vendor code first writes the rx_eq tuning value here and then
        // overwrites the field with 0xb80 >> 6, which leaves rx_eq at 3.
        smn.reg(usb3_phy(n, 0x18))
            .modify(Usb3RxEq::CONTROL.val(0xb80 >> 6));
        smn.reg(usb3_phy::<()>(n, 0x98)).set(1);
    }
}

/// Bring the xHCI of die `die` out of reset (cold path). `smn` is the die's.
pub fn init(
    smn: &Smn<impl ConfigRegionAccess>,
    die: u8,
    package: Package,
    delay: &mut impl DelayNs,
) {
    smn.reg(R14C).modify(R14c::BIT31::SET);

    smn.reg(R014).set(0);
    wait_108(smn, delay);
    let reset = smn.reg(RESET_CONTROL);
    reset.modify(ResetControl::BIT0::SET);
    reset.modify(ResetControl::BIT1::SET + ResetControl::BIT2::SET);
    delay.delay_us(100);

    // SP4 has only two of the four ports; SP4r2 keeps the reset values.
    if package == Package::Sp4 {
        smn.reg(PORT_130).set(0x22);
        smn.reg(PORT_DISABLE).set(0x000c_000c);
    }

    phy_init(smn, die);

    reset.modify(ResetControl::BIT0::CLEAR);
    reset.modify(ResetControl::BIT2::CLEAR);
    delay.delay_us(200);
    reset.modify(ResetControl::BIT1::CLEAR);

    post_reset(smn);
}
