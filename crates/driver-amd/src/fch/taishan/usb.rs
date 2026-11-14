// SPDX-License-Identifier: MIT OR Apache-2.0
//! The xHCI controller of a die (behind 7.1 of the die's root bus, function
//! 3). The vendor FchTaishanPei reset task
//! af48 programs it through SMN; Zeppelin has no SMU USB messages (sp4r2
//! report FCH_TAISHAN, cold path, vendor defaults).
use crate::{
    cpu::Package,
    smn::{Smn, SmnReg},
};
use embedded_hal::delay::DelayNs;
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    /// The controller's reset bits; bit 0 is released first, bit 2 then
    /// bit 1 after the PHY setup.
    pub(super) ResetControl [
        BIT0 OFFSET(0) NUMBITS(1) [],
        BIT1 OFFSET(1) NUMBITS(1) [],
        BIT2 OFFSET(2) NUMBITS(1) []
    ],
    pub(super) R14c [BIT31 OFFSET(31) NUMBITS(1) []],
    Status108 [BUSY OFFSET(8) NUMBITS(2) []],
    R118 [
        /// Vendor PCD 0x97.
        PCD_97 OFFSET(12) NUMBITS(1) []
    ],
    /// USB3 PHY tuning: vendor CBS defaults, the same for every PHY.
    Usb3Tuning [
        LOS_BIAS OFFSET(5) NUMBITS(3) [],
        DEEMPH_3P5 OFFSET(18) NUMBITS(6) [],
        DEEMPH_6 OFFSET(24) NUMBITS(6) []
    ],
    Usb3Swing [
        SWING OFFSET(0) NUMBITS(7) [],
        VBOOST OFFSET(7) NUMBITS(3) []
    ],
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
/// USB2 PHY parameter of the vendor template, per die of a socket.
const USB2_PHY_PARAMETER: [u32; 2] = [0x01b3_c953, 0x01b3_cd56];

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
            .set(USB2_PHY_PARAMETER[usize::from(die & 1)]);
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

    // SP4 and SP3 have only two of the four ports; SP4r2 keeps the reset
    // values.
    if matches!(package, Package::Sp4 | Package::Sp3) {
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

/// What a die's xHCI does (the FchMultiFchPei defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DieUsb {
    /// Function strapped off only (2P dies 4/5 at the SP3 donor default).
    Off,
    /// Strapped off and held in reset (odd die pairs: 2, 3, 6, 7).
    HeldInReset,
    /// Brought up ([`init`]).
    Enabled,
}

impl DieUsb {
    /// The SP3 donor default of die `die` (0..7, socket 1 = 4..7).
    pub const fn default_for(die: u8) -> Self {
        match die {
            0 | 1 => Self::Enabled,
            4 | 5 => Self::Off,
            _ => Self::HeldInReset,
        }
    }
}

/// FchTaishanPei af48 for die `die` of the system: [`init`] or switched off
/// as `usb` says. `smn` is the die's.
pub fn reset(
    smn: &Smn<impl ConfigRegionAccess>,
    die: u8,
    usb: DieUsb,
    package: Package,
    delay: &mut impl DelayNs,
) {
    smn.reg(R14C).modify(R14c::BIT31::SET);
    match usb {
        DieUsb::Off => smn.reg(NBIF_STRAP).modify(super::NbifStrap::ENABLE::CLEAR),
        DieUsb::HeldInReset => {
            smn.reg(NBIF_STRAP).modify(super::NbifStrap::ENABLE::CLEAR);
            smn.reg(RESET_CONTROL).modify(
                ResetControl::BIT0::SET + ResetControl::BIT1::SET + ResetControl::BIT2::SET,
            );
        }
        DieUsb::Enabled => init(smn, die, package, delay),
    }
}

/// Handshake with the SMU around S3 (unnamed SMN `0004D520`/`0004D528`).
const SMU_REQ: SmnReg = SmnReg::new(0x0004_d520);
const SMU_ACK: SmnReg = SmnReg::new(0x0004_d528);
const SX_CTL: SmnReg = SmnReg::new(CONTROLLER + 0x104);

/// One request/acknowledge step of the SMU handshake: write `request`,
/// then repeatedly post 107h and wait for `ack & 3 == want` (1000 × 1 µs,
/// as the vendor code; a timeout continues like it does).
fn handshake(
    smn: &Smn<impl ConfigRegionAccess>,
    request: u32,
    want: u32,
    delay: &mut impl DelayNs,
) -> bool {
    smn.reg(SMU_REQ).set(request);
    (0..STATUS_POLLS).any(|_| {
        smn.reg(SMU_REQ).set(0x107);
        let done = smn.reg(SMU_ACK).get() & 3 == want;
        if !done {
            delay.delay_us(1);
        }
        done
    })
}

/// The S3 entry handshake of a die's controller (FchTaishanSmmInit Sx
/// path): wait for 0x108\[9:8\] = 3, request 101h, wait for the SMU, then
/// 0x104 = C8h. Returns whether both waits completed; the vendor code
/// continues either way.
pub fn sx_handshake(smn: &Smn<impl ConfigRegionAccess>, delay: &mut impl DelayNs) -> bool {
    let idle = (0..STATUS_POLLS).any(|_| {
        let done = smn.reg(STATUS_108).read(Status108::BUSY) == 3;
        if !done {
            delay.delay_us(1);
        }
        done
    });
    let acked = handshake(smn, 0x101, 2, delay);
    smn.reg(SX_CTL).set(200);
    idle && acked
}
