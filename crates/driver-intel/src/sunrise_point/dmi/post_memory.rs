// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCH-H post-memory DMI recipe, skylake/pch.c SHA256
//! da2f5460aa14d21335852802f08ba2cea163f98bcbf630a8b71cd90c07e736c6.
//! Intel 332691-002, 31.2.16: PCH LCTL[6:2] is reserved RO; unlike the
//! CPU-side DMI register, bit5 is NOT Retrain. Omit native's reserved-bit
//! write and never echo the adjacent LSTS into a control RMW.
use super::super::pcr::{self, Pcr, Port};
use super::LinkCap;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    Locked,
    Readback,
    Pcr(pcr::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
register_bitfields![u32,
    ErrorMask [
        POISON OFFSET(12) NUMBITS(1) [],
        ABORT OFFSET(15) NUMBITS(1) [],
        UNSUPPORTED OFFSET(20) NUMBITS(1) []
    ],
    ErrorPolicy [ UNSUPPORTED_TRANSACTION OFFSET(30) NUMBITS(1) [] ],
    Reg2348 [ LOW OFFSET(0) NUMBITS(4) [] ],
    Pll [
        BIT1 OFFSET(1) NUMBITS(1) [],
        BIT4 OFFSET(4) NUMBITS(1) [],
        CLEAR OFFSET(5) NUMBITS(2) []
    ],
    Reg232c [ BIT20 OFFSET(20) NUMBITS(1) [] ],
    LinkConfig [
        BIT10 OFFSET(10) NUMBITS(1) [],
        BIT16 OFFSET(16) NUMBITS(1) [],
        BIT17 OFFSET(17) NUMBITS(1) [],
        BIT18 OFFSET(18) NUMBITS(1) [],
        BIT30 OFFSET(30) NUMBITS(1) [],
        BIT31 OFFSET(31) NUMBITS(1) []
    ],
    Gen3L0s [
        LOW OFFSET(0) NUMBITS(8) [],
        HIGH OFFSET(24) NUMBITS(8) []
    ],
    L0s [ HIGH OFFSET(16) NUMBITS(16) [] ],
    Nfts [
        LOW OFFSET(0) NUMBITS(8) [],
        HIGH OFFSET(16) NUMBITS(8) []
    ],
    Config [
        BIT21 OFFSET(21) NUMBITS(1) [],
        BIT27 OFFSET(27) NUMBITS(1) [],
        BIT30 OFFSET(30) NUMBITS(1) []
    ],
    Reg2314 [
        BIT5 OFFSET(5) NUMBITS(1) [],
        BIT21 OFFSET(21) NUMBITS(1) [],
        CLEAR OFFSET(22) NUMBITS(2) []
    ],
    Width [
        BIT0 OFFSET(0) NUMBITS(1) [],
        BIT8 OFFSET(8) NUMBITS(1) [],
        CLEAR OFFSET(9) NUMBITS(2) [],
        BIT11 OFFSET(11) NUMBITS(1) []
    ],
    Power [
        BIT0 OFFSET(0) NUMBITS(1) [],
        BIT1 OFFSET(1) NUMBITS(1) [],
        BIT8 OFFSET(8) NUMBITS(1) []
    ],
    Reg234c [
        CLEAR OFFSET(0) NUMBITS(3) [],
        BIT5 OFFSET(5) NUMBITS(1) []
    ],
    Dmic [ LOCK OFFSET(31) NUMBITS(1) [] ]
];
register_bitfields![u16,LinkControl [ ASPM OFFSET(0) NUMBITS(2) [] ]];
register_bitfields![u8,
    DmiClock [
        BACKBONE_GATE OFFSET(0) NUMBITS(1) [],
        LINK_GATE OFFSET(1) NUMBITS(1) [],
        BACKBONE_REQUEST OFFSET(2) NUMBITS(1) [],
        LINK_REQUEST OFFSET(3) NUMBITS(1) [],
        OSC_GATE OFFSET(4) NUMBITS(1) []
    ],
    IosfClock [
        CLEAR OFFSET(2) NUMBITS(2) [],
        BIT5 OFFSET(5) NUMBITS(1) [],
        BIT6 OFFSET(6) NUMBITS(1) []
    ]
];
/// # Safety
/// Own the actual cold PCH-H DMI PCR after authenticated memory import and
/// SA topology/link setup, before DMIC.SRL. The NB uses the SAME low-latency
/// policy. This programs power/latency controls, not link training or a proof
/// of physical DMI margins. Any partial failure is fatal; LCAP contains RWO.
pub(crate) unsafe fn configure(pcr: &Pcr, low_latency: bool) -> Result<(), Error> {
    for offset in [
        0x2088, 0x20ac, 0x21a4, 0x21a8, 0x2234, 0x223c, 0x2238, 0x2304, 0x2310, 0x2314, 0x2320,
        0x232c, 0x2334, 0x2340, 0x2344, 0x2348, 0x234c, 0x2478,
    ] {
        if pcr.register::<()>(Port::Dmi, offset)?.get() == u32::MAX {
            return Err(Error::Unavailable);
        }
    }
    if pcr
        .register::<Dmic::Register>(Port::Dmi, 0x2234)?
        .is_set(Dmic::LOCK)
    {
        return Err(Error::Locked);
    }
    let mask = pcr.register::<ErrorMask::Register>(Port::Dmi, 0x2088)?;
    let mask_fields = ErrorMask::POISON::SET + ErrorMask::ABORT::SET + ErrorMask::UNSUPPORTED::SET;
    mask.write(mask_fields);
    let rec = pcr.register::<ErrorPolicy::Register>(Port::Dmi, 0x20ac)?;
    rec.modify(ErrorPolicy::UNSUPPORTED_TRANSACTION::SET);
    let r2348 = pcr.register::<Reg2348::Register>(Port::Dmi, 0x2348)?;
    r2348.modify(Reg2348::LOW.val(0));
    let pll = pcr.register::<Pll::Register>(Port::Dmi, 0x2320)?;
    pll.modify(Pll::CLEAR.val(0) + Pll::BIT4::SET);
    let r232c = pcr.register::<Reg232c::Register>(Port::Dmi, 0x232c)?;
    r232c.modify(Reg232c::BIT20::SET);
    pll.modify(Pll::BIT1::SET);
    let lc = pcr.register::<LinkConfig::Register>(Port::Dmi, 0x2304)?;
    let lc_fields = LinkConfig::BIT18::CLEAR
        + LinkConfig::BIT31::SET
        + LinkConfig::BIT30::SET
        + LinkConfig::BIT17::SET
        + LinkConfig::BIT16::SET;
    lc.modify(lc_fields);
    let gen3 = pcr.register::<Gen3L0s::Register>(Port::Dmi, 0x2478)?;
    gen3.modify(Gen3L0s::HIGH.val(0x28));
    let l0s = pcr.register::<L0s::Register>(Port::Dmi, 0x2344)?;
    l0s.modify(L0s::HIGH.val(0x1414));
    let cap = pcr.register::<LinkCap::Register>(Port::Dmi, 0x21a4)?;
    cap.modify(LinkCap::L0_EXIT.val(4) + LinkCap::L1_EXIT.val(4));
    let nfts = pcr.register::<Nfts::Register>(Port::Dmi, 0x2340)?;
    nfts.modify(Nfts::HIGH.val(0x5b) + Nfts::LOW.val(0x2d));
    gen3.modify(Gen3L0s::LOW.val(0x2c));
    // Native's 21a8[5] write omitted: reserved RO on this PCH, not CPU RL.
    let cfg = pcr.register::<Config::Register>(Port::Dmi, 0x2310)?;
    cfg.modify(Config::BIT30::SET + Config::BIT27::SET);
    let r2314 = pcr.register::<Reg2314::Register>(Port::Dmi, 0x2314)?;
    let r2314_fields = Reg2314::CLEAR.val(0) + Reg2314::BIT21::SET + Reg2314::BIT5::SET;
    r2314.modify(r2314_fields);
    cfg.modify(Config::BIT21::CLEAR);
    let width = pcr.register::<Width::Register>(Port::Dmi, 0x2238)?;
    let width_fields =
        Width::CLEAR.val(0) + Width::BIT11::SET + Width::BIT8::SET + Width::BIT0::SET;
    width.modify(width_fields);
    let power = pcr.register::<Power::Register>(Port::Dmi, 0x2334)?;
    power.modify(Power::BIT8::SET + Power::BIT0::SET);
    power.modify(Power::BIT1::SET);
    let r234c = pcr.register::<Reg234c::Register>(Port::Dmi, 0x234c)?;
    r234c.modify(Reg234c::CLEAR.val(0) + Reg234c::BIT5::SET);
    let clocks = pcr.register8::<DmiClock::Register>(Port::Dmi, 0x2234)?;
    let clock_fields = DmiClock::BACKBONE_GATE::SET
        + DmiClock::LINK_GATE::SET
        + DmiClock::BACKBONE_REQUEST::SET
        + DmiClock::LINK_REQUEST::SET
        + DmiClock::OSC_GATE::SET;
    clocks.modify(clock_fields);
    let iosf = pcr.register8::<IosfClock::Register>(Port::Dmi, 0x223e)?;
    let iosf_fields = IosfClock::CLEAR.val(0) + IosfClock::BIT5::SET + IosfClock::BIT6::SET;
    iosf.modify(iosf_fields);
    let control = pcr.register16::<LinkControl::Register>(Port::Dmi, 0x21a8)?;
    if !low_latency {
        lc.modify(LinkConfig::BIT10::CLEAR);
        cap.modify(LinkCap::ASPM.val(3));
        control.modify(LinkControl::ASPM.val(3));
        lc.modify(LinkConfig::BIT10::SET);
    } else {
        // Implement the source's stated keep-disabled policy, including when
        // an earlier producer left ASPM set. Never preserve it accidentally.
        control.modify(LinkControl::ASPM.val(0));
    }
    if mask.get() != mask_fields.value
        || !rec.is_set(ErrorPolicy::UNSUPPORTED_TRANSACTION)
        || r2348.read(Reg2348::LOW) != 0
        || !pll.matches_all(Pll::CLEAR.val(0) + Pll::BIT4::SET + Pll::BIT1::SET)
        || !r232c.is_set(Reg232c::BIT20)
        || !lc.matches_all(lc_fields)
        || (!low_latency && !lc.is_set(LinkConfig::BIT10))
        || !gen3.matches_all(Gen3L0s::HIGH.val(0x28) + Gen3L0s::LOW.val(0x2c))
        || l0s.read(L0s::HIGH) != 0x1414
        || !cap.matches_all(LinkCap::L0_EXIT.val(4) + LinkCap::L1_EXIT.val(4))
        || (!low_latency && cap.read(LinkCap::ASPM) != 3)
        || !nfts.matches_all(Nfts::HIGH.val(0x5b) + Nfts::LOW.val(0x2d))
        || !cfg.matches_all(Config::BIT30::SET + Config::BIT27::SET + Config::BIT21::CLEAR)
        || !r2314.matches_all(r2314_fields)
        || !width.matches_all(width_fields)
        || !power.matches_all(Power::BIT0::SET + Power::BIT1::SET + Power::BIT8::SET)
        || !r234c.matches_all(Reg234c::CLEAR.val(0) + Reg234c::BIT5::SET)
        || !clocks.matches_all(clock_fields)
        || !iosf.matches_all(iosf_fields)
        || control.read(LinkControl::ASPM) != if low_latency { 0 } else { 3 }
    {
        return Err(Error::Readback);
    }
    Ok(())
}
