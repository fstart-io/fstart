// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sunrise Point-H/LP post-memory DMI / OP-DMI recipe, skylake/pch.c.
//! Intel 332691-002, 31.2.16: PCH LCTL[6:2] is reserved RO; unlike the
//! CPU-side DMI register, bit5 is NOT Retrain. Omit native's reserved-bit
//! write and never echo the adjacent LSTS into a control RMW.
use super::super::pcr::{self, Pcr, Port};
use super::{Kind, LinkCap};
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
    Reg2300 [ BIT1 OFFSET(1) NUMBITS(1) [] ],
    Reg2348 [ LOW OFFSET(0) NUMBITS(4) [] ],
    Pll [
        BIT1 OFFSET(1) NUMBITS(1) [],
        BIT4 OFFSET(4) NUMBITS(1) [],
        CLEAR OFFSET(5) NUMBITS(2) []
    ],
    Reg232c [
        BIT0 OFFSET(0) NUMBITS(1) [],
        BIT20 OFFSET(20) NUMBITS(1) []
    ],
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
    Dmic [ LOCK OFFSET(31) NUMBITS(1) [] ],
    OpDmiLctl [ BIT3 OFFSET(3) NUMBITS(1) [] ],
    OpDmiLcfg [
        BIT25 OFFSET(25) NUMBITS(1) [],
        BIT27 OFFSET(27) NUMBITS(1) []
    ],
    OpDmiLpmc [
        BIT8 OFFSET(8) NUMBITS(1) [],
        BIT10 OFFSET(10) NUMBITS(1) [],
        LOW_CLEAR OFFSET(16) NUMBITS(5) [],
        BIT21 OFFSET(21) NUMBITS(1) [],
        HIGH_CLEAR OFFSET(22) NUMBITS(2) [],
        SET_HIGH OFFSET(25) NUMBITS(6) []
    ],
    OpiPhy0110 [ BIT20 OFFSET(20) NUMBITS(1) [] ],
    OpiPhy0118 [
        BIT8 OFFSET(8) NUMBITS(1) [],
        BIT12 OFFSET(12) NUMBITS(1) [],
        BIT13 OFFSET(13) NUMBITS(1) []
    ],
    OpiPhy011c [ BIT14 OFFSET(14) NUMBITS(1) [] ],
    OpiPhy0354 [ BIT1 OFFSET(1) NUMBITS(1) [] ],
    OpiPhyB104 [ BIT3 OFFSET(3) NUMBITS(1) [] ],
    OpiPhyB10c [
        BIT5 OFFSET(5) NUMBITS(1) [],
        BIT6 OFFSET(6) NUMBITS(1) []
    ]
];
register_bitfields![u16,
    LinkControl [ ASPM OFFSET(0) NUMBITS(2) [] ],
    OpDmiStc [ VALUE OFFSET(0) NUMBITS(16) [] ]
];
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
/// Own the actual cold PCH-H/LP DMI PCR after authenticated memory import and
/// SA topology/link setup, before DMIC.SRL. The NB uses the SAME low-latency
/// policy. This programs power/latency controls, not link training or a proof
/// of physical DMI margins. Any partial failure is fatal; LCAP contains RWO.
pub(crate) unsafe fn configure(pcr: &Pcr, kind: Kind, low_latency: bool) -> Result<(), Error> {
    match kind {
        Kind::H => unsafe { configure_h(pcr, low_latency) },
        Kind::Lp => unsafe { configure_lp(pcr) },
    }
}

unsafe fn configure_h(pcr: &Pcr, low_latency: bool) -> Result<(), Error> {
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

unsafe fn configure_lp(pcr: &Pcr) -> Result<(), Error> {
    for offset in [
        0x2088, 0x20ac, 0x2234, 0x223c, 0x2300, 0x2320, 0x232c, 0x2334, 0x234c, 0x2600, 0x260c,
        0x2614, 0x2618,
    ] {
        if pcr.register::<()>(Port::Dmi, offset)?.get() == u32::MAX {
            return Err(Error::Unavailable);
        }
    }
    for offset in [0x0110, 0x0118, 0x011c, 0x0354, 0xb104, 0xb10c] {
        if pcr.register::<()>(Port::OpiPhy, offset)?.get() == u32::MAX {
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
    let r2300 = pcr.register::<Reg2300::Register>(Port::Dmi, 0x2300)?;
    r2300.modify(Reg2300::BIT1::SET);

    let opi_b104 = pcr.register::<OpiPhyB104::Register>(Port::OpiPhy, 0xb104)?;
    opi_b104.modify(OpiPhyB104::BIT3::SET);
    let opi_0110 = pcr.register::<OpiPhy0110::Register>(Port::OpiPhy, 0x0110)?;
    let opi_b10c = pcr.register::<OpiPhyB10c::Register>(Port::OpiPhy, 0xb10c)?;
    let set_b10c = opi_0110.is_set(OpiPhy0110::BIT20);
    if set_b10c {
        opi_b10c.modify(OpiPhyB10c::BIT6::SET + OpiPhyB10c::BIT5::SET);
    }
    let opi_0118 = pcr.register::<OpiPhy0118::Register>(Port::OpiPhy, 0x0118)?;
    let opi_0118_fields = OpiPhy0118::BIT13::SET + OpiPhy0118::BIT12::SET + OpiPhy0118::BIT8::SET;
    opi_0118.modify(opi_0118_fields);
    let opi_0354 = pcr.register::<OpiPhy0354::Register>(Port::OpiPhy, 0x0354)?;
    opi_0354.modify(OpiPhy0354::BIT1::SET);
    let opi_011c = pcr.register::<OpiPhy011c::Register>(Port::OpiPhy, 0x011c)?;
    opi_011c.modify(OpiPhy011c::BIT14::SET);

    let pll = pcr.register::<Pll::Register>(Port::Dmi, 0x2320)?;
    pll.modify(Pll::CLEAR.val(0) + Pll::BIT4::SET);
    let r232c = pcr.register::<Reg232c::Register>(Port::Dmi, 0x232c)?;
    r232c.modify(Reg232c::BIT0::SET);
    let stc = pcr.register16::<OpDmiStc::Register>(Port::Dmi, 0x260c)?;
    stc.write(OpDmiStc::VALUE.val(0x0014));
    let lcfg = pcr.register::<OpDmiLcfg::Register>(Port::Dmi, 0x2618)?;
    lcfg.modify(OpDmiLcfg::BIT27::SET);
    let lpmc = pcr.register::<OpDmiLpmc::Register>(Port::Dmi, 0x2614)?;
    let lpmc_fields = OpDmiLpmc::HIGH_CLEAR.val(0)
        + OpDmiLpmc::LOW_CLEAR.val(0)
        + OpDmiLpmc::SET_HIGH.val(0x3f)
        + OpDmiLpmc::BIT21::SET
        + OpDmiLpmc::BIT10::SET
        + OpDmiLpmc::BIT8::SET;
    lpmc.modify(lpmc_fields);
    let lctl = pcr.register::<OpDmiLctl::Register>(Port::Dmi, 0x2600)?;
    lctl.modify(OpDmiLctl::BIT3::SET);
    let power = pcr.register::<Power::Register>(Port::Dmi, 0x2334)?;
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
    lcfg.modify(OpDmiLcfg::BIT25::SET);

    if mask.get() != mask_fields.value
        || !rec.is_set(ErrorPolicy::UNSUPPORTED_TRANSACTION)
        || !r2300.is_set(Reg2300::BIT1)
        || !opi_b104.is_set(OpiPhyB104::BIT3)
        || (set_b10c && !opi_b10c.matches_all(OpiPhyB10c::BIT6::SET + OpiPhyB10c::BIT5::SET))
        || !opi_0118.matches_all(opi_0118_fields)
        || !opi_0354.is_set(OpiPhy0354::BIT1)
        || !opi_011c.is_set(OpiPhy011c::BIT14)
        || !pll.matches_all(Pll::CLEAR.val(0) + Pll::BIT4::SET)
        || !r232c.is_set(Reg232c::BIT0)
        || stc.get() != 0x0014
        || !lcfg.matches_all(OpDmiLcfg::BIT27::SET + OpDmiLcfg::BIT25::SET)
        || !lpmc.matches_all(lpmc_fields)
        || !lctl.is_set(OpDmiLctl::BIT3)
        || !power.is_set(Power::BIT1)
        || !r234c.matches_all(Reg234c::CLEAR.val(0) + Reg234c::BIT5::SET)
        || !clocks.matches_all(clock_fields)
        || !iosf.matches_all(iosf_fields)
    {
        return Err(Error::Readback);
    }
    Ok(())
}
