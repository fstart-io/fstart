// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCH-H fabric/clock lifecycle from skylake/pch.c SHA256
//! da2f5460aa14d21335852802f08ba2cea163f98bcbf630a8b71cd90c07e736c6.
//! RP grants, relaxed ordering, function mapping and FIA stay owned by pcie.
//! VT-d routing is NOT translation, interrupt remapping or DMA protection;
//! EOI register readback is NOT a delivered-interrupt test.
use super::{
    dmi,
    gpio::MiscConfig,
    hsio,
    pcr::{self, Pcr, Port},
};
use crate::skylake::capabilities::CapidA;
use fstart_pci::{EcamDevice, PciAddress};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Must match the actual NB DMI policy.
    pub legacy_io_low_latency: bool,
    /// Payload/option-ROM dependent; do not infer this from PC/AT presence.
    pub use_legacy_8254: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Silicon(dmi::Kind),
    P2sbDecode {
        id: u32,
        bar: Option<u64>,
        command: u16,
    },
    P2sbRegister(u16),
    SaCapability {
        id: u32,
        capid: u32,
    },
    PcrRegister {
        port: Port,
        offset: u16,
    },
    Locked,
    Owned,
    Readback,
    Pcr(pcr::Error),
    Dmi(dmi::post_memory::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
impl From<dmi::post_memory::Error> for Error {
    fn from(e: dmi::post_memory::Error) -> Self {
        Self::Dmi(e)
    }
}
register_bitfields![u8,
    P2sbE2 [
        NATIVE_BIT0 OFFSET(0) NUMBITS(1) [],
        MASK_LOCK OFFSET(1) NUMBITS(1) []
    ],
    P2sbPower [
        PMC_GATING OFFSET(0) NUMBITS(3) [],
        HAE OFFSET(5) NUMBITS(1) []
    ]
];
register_bitfields![u16,P2sbLowLatency [ GATING_DISABLE OFFSET(0) NUMBITS(2) [] ]];
register_bitfields![u32,
    Rootspace [
        VTD OFFSET(0) NUMBITS(1) [],
        ADDRESS_P2P OFFSET(1) NUMBITS(1) []
    ],
    // Host PSF1–4, PCR 0x4000 (32-bit); bit 3 is reserved (Intel 332691 §§31.4–31.7).
    Global [ TRUNK_GATE OFFSET(4) NUMBITS(1) [] ],
    // CSME PSF PCR 0x4000 (32-bit), native clock recipe.
    CsmeGlobal [
        LOCAL_GATE OFFSET(3) NUMBITS(1) [],
        TRUNK_GATE OFFSET(4) NUMBITS(1) []
    ],
    EoiTarget [ NATIVE_TARGET OFFSET(0) NUMBITS(32) [] ],
    EoiControl [ NATIVE_CONTROL OFFSET(0) NUMBITS(32) [] ],
    Psth [
        DYNAMIC OFFSET(0) NUMBITS(1) [],
        SECONDARY OFFSET(1) NUMBITS(1) [],
        PRIMARY OFFSET(2) NUMBITS(1) []
    ],
    RtcClock [
        ROSIDE OFFSET(0) NUMBITS(1) [],
        PCI OFFSET(1) NUMBITS(1) []
    ],
    Rtc3f00 [ BIT8 OFFSET(8) NUMBITS(1) [] ],
    RtcUip [ NATIVE_CLEAR OFFSET(16) NUMBITS(2) [] ],
    ItssClock [ GATES OFFSET(0) NUMBITS(5) [] ]
];
const PSFS: [Port; 4] = [Port::Psf1, Port::Psf2, Port::Psf3, Port::Psf4];
fn eoi_config(kind: dmi::Kind, kbl: bool) -> (u16, u16, u32) {
    match kind {
        dmi::Kind::Lp => (0x4060, 0x4050, 0x38a00),
        dmi::Kind::H if kbl => (0x405c, 0x404c, 0x38b00),
        dmi::Kind::H => (0x4058, 0x4048, 0x38b00),
    }
}
fn itss_gates(policy: Policy) -> u32 {
    if policy.legacy_io_low_latency {
        0
    } else {
        0x1b | if policy.use_legacy_8254 { 0 } else { 4 }
    }
}
/// # Safety
/// Actual cold H/LP silicon, mapped/exclusive PCR and visible P2SB, before mask
/// lock/enumeration; NB DMI topology and matching low-latency policy applied.
/// All failures poison the chipset owner; there is no reversible power recipe.
pub(crate) unsafe fn before_root_ports(
    pcr: &Pcr,
    p2sb: &EcamDevice,
    silicon: hsio::Silicon,
    policy: Policy,
) -> Result<(), Error> {
    let id = p2sb.read32(0);
    let bar = p2sb.memory_bar(0x10);
    let command = p2sb.read16(4);
    if p2sb.address() != PciAddress::new(0, 0, 0x1f, 1)
        || id as u16 != 0x8086
        || bar != Some(u64::from(pcr::DEFAULT_BASE))
        || command & 2 == 0
    {
        return Err(Error::P2sbDecode { id, bar, command });
    }
    let e2 = unsafe { p2sb.register::<u8, P2sbE2::Register>(0xe2) };
    let power = unsafe { p2sb.register::<u8, P2sbPower::Register>(0xe4) };
    if e2.get() == u8::MAX {
        return Err(Error::P2sbRegister(0xe2));
    }
    if power.get() == u8::MAX {
        return Err(Error::P2sbRegister(0xe4));
    }
    if e2.is_set(P2sbE2::MASK_LOCK) {
        return Err(Error::Locked);
    }
    unsafe { dmi::post_memory::configure(pcr, silicon.kind(), policy.legacy_io_low_latency) }?;
    e2.modify(P2sbE2::NATIVE_BIT0::SET);
    if policy.legacy_io_low_latency {
        power.modify(P2sbPower::PMC_GATING.val(0));
        for offset in [0xe8, 0xea] {
            let r = unsafe { p2sb.register::<u16, P2sbLowLatency::Register>(offset) };
            if r.get() == u16::MAX {
                return Err(Error::P2sbRegister(offset));
            }
            r.modify(P2sbLowLatency::GATING_DISABLE.val(3));
            if r.read(P2sbLowLatency::GATING_DISABLE) != 3 {
                return Err(Error::Readback);
            }
        }
    } else {
        power.modify(P2sbPower::PMC_GATING.val(0) + P2sbPower::HAE::SET);
    }
    if !e2.is_set(P2sbE2::NATIVE_BIT0)
        || power.read(P2sbPower::PMC_GATING) != 0
        || !policy.legacy_io_low_latency && !power.is_set(P2sbPower::HAE)
    {
        return Err(Error::Readback);
    }
    Ok(())
}
pub(crate) struct Prepared {
    vtd_routing: bool,
    kind: dmi::Kind,
    kbl: bool,
    policy: Policy,
    _bsp: core::marker::PhantomData<*mut ()>,
}
#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    pub vtd_routing: bool,
    pub legacy_8254_clock_gated: bool,
}
/// # Safety
/// Own actual cold H/LP fabric after RP mapping and LAN preparation, before the
/// canonical scan and final P2SB endpoint masks. vtd_policy is the SAME policy
/// as RP initialization; actual SA fuse gates it. No DMA/AP/OS competitors.
pub(crate) unsafe fn after_root_ports(
    pcr: &Pcr,
    silicon: hsio::Silicon,
    vtd_policy: bool,
    policy: Policy,
) -> Result<Prepared, Error> {
    let sa = EcamDevice::new(0, 0, 0);
    let id = sa.read32(0);
    let capid = sa.read32(0xe4);
    if id as u16 != 0x8086 || capid == u32::MAX {
        return Err(Error::SaCapability { id, capid });
    }
    let vtd_routing = vtd_policy && !CapidA::VTD_DISABLED.is_set(capid);
    let kind = silicon.kind();
    let kbl = silicon.is_kbl_h();
    let (target, control, target_val) = eoi_config(kind, kbl);
    // All four rootspaces and both EOI controllers must respond BEFORE the
    // first routing update; an all-ones response cannot authorize a policy.
    for port in PSFS {
        for offset in [0x4010, 0x4014, 0x4000] {
            if pcr.register::<()>(port, offset)?.get() == u32::MAX {
                return Err(Error::PcrRegister { port, offset });
            }
        }
        if !vtd_routing
            && pcr
                .register::<Rootspace::Register>(port, 0x4010)?
                .is_set(Rootspace::VTD)
        {
            return Err(Error::Owned);
        }
    }
    for (port, offset) in [
        (Port::Psf1, target),
        (Port::Psf1, control),
        (Port::Psf3, 0x4054),
        (Port::Psf3, 0x404c),
        (Port::Psth, 0x1d00),
        (Port::Rtc, 0x3f04),
    ] {
        if pcr.register::<()>(port, offset)?.get() == u32::MAX {
            return Err(Error::PcrRegister { port, offset });
        }
    }
    for (port, target, control) in [(Port::Psf1, target, control), (Port::Psf3, 0x4054, 0x404c)] {
        let t = pcr.register::<EoiTarget::Register>(port, target)?;
        let c = pcr.register::<EoiControl::Register>(port, control)?;
        t.write(EoiTarget::NATIVE_TARGET.val(target_val));
        c.write(EoiControl::NATIVE_CONTROL.val(3));
        if t.get() != target_val || c.get() != 3 {
            return Err(Error::Readback);
        }
    }
    if vtd_routing {
        for port in PSFS {
            pcr.register::<Rootspace::Register>(port, 0x4010)?
                .modify(Rootspace::VTD::SET);
        }
    }
    for port in PSFS {
        for offset in [0x4010, 0x4014] {
            let r = pcr.register::<Rootspace::Register>(port, offset)?;
            r.modify(Rootspace::ADDRESS_P2P::CLEAR);
            if r.is_set(Rootspace::ADDRESS_P2P)
                || (offset == 0x4010 && r.is_set(Rootspace::VTD) != vtd_routing)
            {
                return Err(Error::Readback);
            }
        }
    }
    for port in PSFS {
        let r = pcr.register::<Global::Register>(port, 0x4000)?;
        r.modify(Global::TRUNK_GATE::SET);
        if r.get() == u32::MAX || !r.is_set(Global::TRUNK_GATE) {
            return Err(Error::Readback);
        }
    }
    // Native CSME clock tuning is not a prerequisite for host routing.
    // Don't echo an all-ones response back to a CSE-owned endpoint.
    let csme = pcr.register::<CsmeGlobal::Register>(Port::CsmePsf, 0x4000)?;
    let csme_fields = CsmeGlobal::TRUNK_GATE::SET + CsmeGlobal::LOCAL_GATE::SET;
    if csme.get() != u32::MAX {
        csme.modify(csme_fields);
    }
    if csme.get() == u32::MAX || !csme.matches_all(csme_fields) {
        fstart_log::warn!(
            "PCH interconnect: CSME PSF clock tuning unavailable (0x{:08x})",
            csme.get()
        );
    }
    let psth = pcr.register::<Psth::Register>(Port::Psth, 0x1d00)?;
    let fields = Psth::PRIMARY::CLEAR + Psth::SECONDARY::SET + Psth::DYNAMIC::SET;
    psth.modify(fields);
    if !psth.matches_all(fields) {
        return Err(Error::Readback);
    }
    // Apply coreboot's undocumented RTC tuning where the offsets respond.
    // Keep the documented UIP acknowledgement independent of that tuning.
    let rtc = pcr.register::<RtcClock::Register>(Port::Rtc, 0x3418)?;
    let rtc_fields = RtcClock::PCI::SET + RtcClock::ROSIDE::SET;
    if rtc.get() != u32::MAX {
        rtc.modify(rtc_fields);
    }
    if rtc.get() == u32::MAX || !rtc.matches_all(rtc_fields) {
        fstart_log::warn!(
            "PCH interconnect: RTC 0x3418 tuning unavailable (0x{:08x})",
            rtc.get()
        );
    }
    let rtc_misc = pcr.register::<Rtc3f00::Register>(Port::Rtc, 0x3f00)?;
    if rtc_misc.get() != u32::MAX {
        rtc_misc.modify(Rtc3f00::BIT8::SET);
    }
    if rtc_misc.get() == u32::MAX || !rtc_misc.is_set(Rtc3f00::BIT8) {
        fstart_log::warn!(
            "PCH interconnect: RTC 0x3f00 tuning unavailable (0x{:08x})",
            rtc_misc.get()
        );
    }
    // Native literal acknowledgement, not a status-echoing RMW.
    pcr.register::<RtcUip::Register>(Port::Rtc, 0x3f04)?
        .write(RtcUip::NATIVE_CLEAR.val(3));
    Ok(Prepared {
        vtd_routing,
        kind,
        kbl,
        policy,
        _bsp: core::marker::PhantomData,
    })
}
/// GPIO/ITSS portion follows the separately owned SMBus, SPI and LPC recipes.
/// # Safety
/// Exclusive actual H/LP GPIO communities/ITSS; board pad/event routing finished.
pub(crate) unsafe fn housekeeping(pcr: &Pcr, policy: Policy) -> Result<(), Error> {
    for port in [Port::Gpio0, Port::Gpio1, Port::Gpio2, Port::Gpio3] {
        let r = pcr.register::<MiscConfig::Register>(port, 0x10)?;
        if r.get() == u32::MAX {
            return Err(Error::PcrRegister { port, offset: 0x10 });
        }
        let fields = MiscConfig::DEEP_CLOCK_GATE::SET + MiscConfig::DEEP_POWER_GATE::SET;
        r.modify(fields);
        if !r.matches_all(fields) {
            return Err(Error::Readback);
        }
    }
    let r = pcr.register::<ItssClock::Register>(Port::Itss, 0x3300)?;
    if r.get() == u32::MAX {
        return Err(Error::PcrRegister {
            port: Port::Itss,
            offset: 0x3300,
        });
    }
    // Native's OR-only ~0 keep-mask cannot implement its stated clear-all
    // low-latency policy or ungate a requested PIT. Own only the five gates.
    r.modify(ItssClock::GATES.val(itss_gates(policy)));
    if r.read(ItssClock::GATES) != itss_gates(policy) {
        return Err(Error::Readback);
    }
    Ok(())
}
/// # Safety
/// Actual canonical PCI assignment and LPC/eSPI BME completion occurred.
/// Same exclusive PCR owner as the producer; no final P2SB masks/locks yet.
pub(crate) unsafe fn finish(prepared: Prepared, pcr: &Pcr) -> Result<Outcome, Error> {
    for port in PSFS {
        for offset in [0x4010, 0x4014] {
            let r = pcr.register::<Rootspace::Register>(port, offset)?;
            if r.get() == u32::MAX
                || r.is_set(Rootspace::ADDRESS_P2P)
                || (offset == 0x4010 && r.is_set(Rootspace::VTD) != prepared.vtd_routing)
            {
                return Err(Error::Readback);
            }
        }
    }
    let (target, control, target_val) = eoi_config(prepared.kind, prepared.kbl);
    for (port, target, control) in [(Port::Psf1, target, control), (Port::Psf3, 0x4054, 0x404c)] {
        if pcr.register::<EoiTarget::Register>(port, target)?.get() != target_val
            || pcr.register::<EoiControl::Register>(port, control)?.get() != 3
        {
            return Err(Error::Readback);
        }
    }
    let gates = pcr.register::<ItssClock::Register>(Port::Itss, 0x3300)?;
    if gates.read(ItssClock::GATES) != itss_gates(prepared.policy) {
        return Err(Error::Readback);
    }
    let psth = pcr.register::<Psth::Register>(Port::Psth, 0x1d00)?;
    if !psth.matches_all(Psth::PRIMARY::CLEAR + Psth::SECONDARY::SET + Psth::DYNAMIC::SET) {
        return Err(Error::Owned);
    }
    psth.modify(Psth::PRIMARY::SET);
    if !psth.is_set(Psth::PRIMARY) {
        return Err(Error::Readback);
    }
    Ok(Outcome {
        vtd_routing: prepared.vtd_routing,
        legacy_8254_clock_gated: gates.read(ItssClock::GATES) & 4 != 0,
    })
}
