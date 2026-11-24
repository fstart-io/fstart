// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native src/soc/intel/skylake/hda.c, PCH-H.
//! Source SHA256: 96a1c47d93cf59cf1e70733f8ccc0cbb0362e31789c12c3d17485632ace346bb.
//! The standard reset, codec
//! commands and verb programming remain in southbridge::hda. IGD operations
//! consume the NB's scoped real Graphics binding, never another BAR owner.
use super::{
    pcr::{self, Pcr, Port},
    pmc::{self, PmcRegisters},
};
use crate::{
    skylake::sa_graphics_pm::{self, Graphics},
    southbridge::hda::{ControllerError, HdaConfig, HdaController},
};
use embedded_hal::delay::DelayNs;
use fstart_core::mmio::MmioReadWrite as RW;
use fstart_pci::{EcamDevice, PCI_COMMAND_BITS, PciWindow, PciWindowKind};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dsp {
    Disabled,
    Uaa,
    Adsp,
}
#[derive(Debug, Clone, Copy)]
pub enum Policy {
    Disabled,
    Enabled {
        dsp: Dsp,
        vc1: bool,
        verbs: &'static HdaConfig,
    },
}
impl Policy {
    pub const fn enabled(self) -> bool {
        matches!(self, Self::Enabled { .. })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Device,
    Resource,
    Locked,
    MissingDisplayAudio,
    LinkInactive,
    LinkActive,
    Frequency,
    Pcr(pcr::Error),
    Codec(ControllerError),
    Graphics(sa_graphics_pm::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
impl From<ControllerError> for Error {
    fn from(e: ControllerError) -> Self {
        Self::Codec(e)
    }
}
impl From<sa_graphics_pm::Error> for Error {
    fn from(e: sa_graphics_pm::Error) -> Self {
        Self::Graphics(e)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Disabled,
    NoCodecs,
    Parked6Mhz { detected: u16 },
    Active { detected: u16, programmed: u16 },
}
register_bitfields![u32,
    ClockGate [ SROTCGE OFFSET(18) NUMBITS(1) [] ],
    Sem1 [
        FIFO OFFSET(9) NUMBITS(2) [],
        TMODE OFFSET(12) NUMBITS(1) [],
        LFLCS OFFSET(24) NUMBITS(1) []
    ],
    Sem2 [
        DUM OFFSET(23) NUMBITS(1) [],
        VC1 OFFSET(24) NUMBITS(1) [],
        BSMT OFFSET(26) NUMBITS(2) []
    ],
    Sem3 [ EXTEND OFFSET(20) NUMBITS(2) [] ],
    Sem4 [ EXTEND OFFSET(20) NUMBITS(2) [] ],
    Function [
        HDASD OFFSET(0) NUMBITS(1) [],
        ADSPD OFFSET(2) NUMBITS(1) [],
        CGD OFFSET(3) NUMBITS(1) [],
        BCLD OFFSET(4) NUMBITS(1) [],
        PGD OFFSET(5) NUMBITS(1) []
    ],
    Apll0 [
        VALUE OFFSET(16) NUMBITS(7) [],
        TOP OFFSET(26) NUMBITS(6) []
    ],
    Apll1 [ VALUE OFFSET(8) NUMBITS(6) [] ],
    Apll2 [
        VALUE OFFSET(0) NUMBITS(5) [],
        TOP OFFSET(8) NUMBITS(4) []
    ],
    PowerTiming [ WIDTH OFFSET(4) NUMBITS(3) [] ],
    TrafficClass [ HCDT OFFSET(1) NUMBITS(1) [] ],
    Shadow [ DISABLE OFFSET(8) NUMBITS(1) [] ],
    Processing [ ENABLE OFFSET(30) NUMBITS(1) [] ],
    Link [
        FREQUENCY OFFSET(0) NUMBITS(4) [],
        SPA OFFSET(16) NUMBITS(1) [],
        CPA OFFSET(23) NUMBITS(1) []
    ]
];
register_bitfields![u16,
    PowerCapability [ PME_STATES OFFSET(11) NUMBITS(5) [] ],
    PowerState [
        STATE OFFSET(0) NUMBITS(2) [],
        PME_STATUS OFFSET(15) NUMBITS(1) []
    ]
];
register_bitfields![u8,
    Latency [ VALUE OFFSET(0) NUMBITS(7) [] ],
    PowerControl [
        LOW OFFSET(0) NUMBITS(3) [],
        BIT3 OFFSET(3) NUMBITS(1) [],
        BIT5 OFFSET(5) NUMBITS(1) []
    ]
];
register_structs! {
    LinkRegisters { (0 => control: RW<u32,Link::Register>), (4 => _reserved), (0x40 => @END), },
    Registers {
        (0 => _reserved0),
        (4 => outpay: RW<u16>), (6 => inpay: RW<u16>), (8 => _reserved1),
        (0x804 => processing: RW<u32,Processing::Register>), (0x808 => _reserved2),
        (0xc44 => links: [LinkRegisters;2]), (0xcc4 => _reserved3),
        (0x1048 => latency: RW<u8,Latency::Register>), (0x1049 => _reserved4),
        (0x104b => power: RW<u8,PowerControl::Register>), (0x104c => @END),
    }
}
fn require(device: &EcamDevice) -> Result<(), Error> {
    if device.address().segment() != 0
        || device.bus() != 0
        || device.dev() != 0x1f
        || device.func() != 3
        || device.vendor_id() != 0x8086
        || device.read16(0x0a) != 0x0403
    {
        return Err(Error::Device);
    }
    Ok(())
}
/// Native disable/hide is BEFORE PCI enumeration. No software-unhide or late
/// decode takeover is attempted for a disabled/locked function.
/// # Safety
/// Own actual mapped PCH-H PCI/PCR/PWRM in the authenticated cold successor,
/// before ST/NST function-disable lockdown, with no other device/SMI owner.
pub(crate) unsafe fn prepare(pcr: &Pcr, pmc: &PmcRegisters, policy: Policy) -> Result<(), Error> {
    let device = EcamDevice::new(0, 0x1f, 3);
    let function = pcr.register::<Function::Register>(Port::Dsp, 0x530)?;
    if function.is_set(Function::BCLD) || pmc.function_disable.is_set(pmc::FunctionDisable::LOCK) {
        return Err(Error::Locked);
    }
    if policy.enabled() {
        require(&device)?;
        if function.is_set(Function::HDASD)
            || pmc.non_static_disable.is_set(pmc::NonStaticDisable::ADSP)
        {
            return Err(Error::Locked);
        }
        return Ok(());
    }
    if device.is_present() {
        require(&device)?;
        unsafe { device.register::<u16, PowerState::Register>(0x54) }
            .modify(PowerState::STATE.val(3) + PowerState::PME_STATUS::CLEAR);
    }
    pmc.non_static_disable
        .modify(pmc::NonStaticDisable::ADSP::SET);
    function.modify(Function::CGD::CLEAR);
    function.modify(
        Function::PGD::CLEAR + Function::BCLD::SET + Function::ADSPD::SET + Function::HDASD::SET,
    );
    let shadow = pcr.register::<Shadow::Register>(Port::Psf3, 0x191c)?;
    shadow.modify(Shadow::DISABLE::SET);
    if !pmc.non_static_disable.is_set(pmc::NonStaticDisable::ADSP)
        || !function.matches_all(
            Function::CGD::CLEAR
                + Function::PGD::CLEAR
                + Function::BCLD::SET
                + Function::ADSPD::SET
                + Function::HDASD::SET,
        )
        || !shadow.is_set(Shadow::DISABLE)
    {
        return Err(Error::Locked);
    }
    Ok(())
}

struct NativeHda<'a> {
    device: EcamDevice,
    regs: &'a Registers,
    codec: HdaController,
    pcr: &'a Pcr,
}
impl NativeHda<'_> {
    fn configure(&self, dsp: Dsp, vc1: bool) -> Result<(), Error> {
        let d = &self.device;
        let function = self.pcr.register::<Function::Register>(Port::Dsp, 0x530)?;
        if function.is_set(Function::BCLD) || function.is_set(Function::HDASD) {
            return Err(Error::Locked);
        }
        match dsp {
            Dsp::Disabled => {}
            Dsp::Uaa => d.write8(9, 0x80),
            Dsp::Adsp => d.write8(0xa, 1),
        }
        unsafe { d.register::<u16, PCI_COMMAND_BITS::Register>(4) }
            .modify(PCI_COMMAND_BITS::MEMORY_SPACE::SET);
        unsafe { d.register::<u16, PowerCapability::Register>(0x52) }
            .modify(PowerCapability::PME_STATES.val(0x18));
        unsafe { d.register::<u32, ClockGate::Register>(0x48) }.modify(ClockGate::SROTCGE::CLEAR);
        unsafe { d.register::<u32, Sem1::Register>(0xc0) }
            .modify(Sem1::LFLCS::CLEAR + Sem1::FIFO.val(3));
        unsafe { d.register::<u32, Sem2::Register>(0xc4) }
            .modify(Sem2::BSMT.val(1) + Sem2::DUM::SET + Sem2::VC1.val(u32::from(vc1)));
        unsafe { d.register::<u32, Sem3::Register>(0xc8) }.modify(Sem3::EXTEND.val(2));
        unsafe { d.register::<u32, Sem4::Register>(0xd0) }.modify(Sem4::EXTEND.val(3));
        self.pcr
            .register::<Apll0::Register>(Port::Dsp, 0x610)?
            .modify(Apll0::TOP.val(0x3f) + Apll0::VALUE.val(0x1e));
        self.pcr
            .register::<Apll1::Register>(Port::Dsp, 0x614)?
            .modify(Apll1::VALUE.val(0x1e));
        self.pcr
            .register::<Apll2::Register>(Port::Dsp, 0x618)?
            .modify(Apll2::TOP.val(1) + Apll2::VALUE.val(0x1d));
        // PCH-H deliberately has NO LP I/O-buffer ownership-select write.
        self.pcr
            .register::<PowerTiming::Register>(Port::Dsp, 0x628)?
            .modify(PowerTiming::WIDTH.val(6));
        if vc1 {
            self.pcr
                .register::<TrafficClass::Register>(Port::Dsp, 0xe4)?
                .modify(TrafficClass::HCDT::SET);
        }
        function.modify(
            Function::PGD::CLEAR
                + Function::CGD::CLEAR
                + Function::BCLD::SET
                + Function::ADSPD.val(u32::from(dsp == Dsp::Disabled)),
        );
        if !function.matches_all(
            Function::PGD::CLEAR
                + Function::CGD::CLEAR
                + Function::BCLD::SET
                + Function::ADSPD.val(u32::from(dsp == Dsp::Disabled)),
        ) {
            return Err(Error::Locked);
        }
        if dsp != Dsp::Disabled {
            self.regs.processing.modify(Processing::ENABLE::SET);
        }
        Ok(())
    }
    fn wait_link(&self, link: usize, active: bool, delay: &mut impl DelayNs) -> bool {
        for sample in 0..=1000 {
            if self.regs.links[link].control.is_set(Link::CPA) == active {
                return true;
            }
            if sample != 1000 {
                delay.delay_us(1);
            }
        }
        false
    }
    fn set_frequency(
        &self,
        link: usize,
        frequency: u32,
        graphics: Option<&Graphics<'_>>,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        let control = &self.regs.links[link].control;
        control.modify(Link::SPA::CLEAR);
        if !self.wait_link(link, false, delay) {
            return Err(Error::LinkInactive);
        }
        if link == 0 {
            self.regs.outpay.set(0x3c);
            self.regs.inpay.set(0x1c);
        } else {
            graphics
                .ok_or(Error::MissingDisplayAudio)?
                .configure_audio_frequency(true, true)?;
            unsafe { self.device.register::<u32, Sem1::Register>(0xc0) }.modify(Sem1::TMODE::CLEAR);
        }
        control.modify(Link::FREQUENCY.val(frequency));
        control.modify(Link::SPA::SET);
        // Unlike the source's warning-only path, a missing CPA is not success.
        if !self.wait_link(link, true, delay) {
            return Err(Error::LinkActive);
        }
        Ok(())
    }
    fn frequencies(
        &self,
        detected: u16,
        graphics: Option<&Graphics<'_>>,
        delay: &mut impl DelayNs,
    ) -> Result<(), Error> {
        for sdi in 0..3u32 {
            if detected & (1 << sdi) == 0 || sdi == 2 && graphics.is_none() {
                continue;
            }
            let (link, frequency) = if sdi == 2 { (1, 4) } else { (0, 2) };
            if link == 0 {
                self.regs.inpay.set(0x1c);
            }
            let support = self
                .codec
                .send_verb_with_delay((sdi << 28) | 0x000f0016, delay)?;
            if support & (1 << frequency) == 0 {
                continue;
            }
            let query = (sdi << 28) | 0x000f3700;
            if self.codec.send_verb_with_delay(query, delay)? & (1 << frequency) != 0 {
                continue;
            }
            self.set_frequency(link, frequency, graphics, delay)?;
            delay.delay_us(600);
            if self.codec.send_verb_with_delay(query, delay)? & (1 << frequency) == 0 {
                // Source default restore (24 MHz analog / 96 MHz display),
                // with actual re-verification rather than presumed recovery.
                self.set_frequency(link, frequency, graphics, delay)?;
                delay.delay_us(600);
                if self.codec.send_verb_with_delay(query, delay)? & (1 << frequency) == 0 {
                    return Err(Error::Frequency);
                }
            }
        }
        Ok(())
    }
}
/// Actual allocated BAR and scoped NB engine only; no hardcoded temporary BAR.
/// # Safety
/// Native cold mainstage owns PCI/PCR/HDA and the complete allocator-provided
/// BAR mapping as UC. No OS, DMA, AP or SMI client competes. All errors after
/// writes are fatal partial initialization, not a retry/rollback token.
pub(crate) unsafe fn initialize(
    pcr: &Pcr,
    policy: Policy,
    window: Option<PciWindow>,
    audio: Option<(&Graphics<'_>, bool, bool)>,
    delay: &mut impl DelayNs,
) -> Result<Outcome, Error> {
    let Policy::Enabled { dsp, vc1, verbs } = policy else {
        return Ok(Outcome::Disabled);
    };
    let device = EcamDevice::new(0, 0x1f, 3);
    require(&device)?;
    let window = window.ok_or(Error::Resource)?;
    let base = usize::try_from(window.base).map_err(|_| Error::Resource)?;
    if window.kind != PciWindowKind::Mmio
        || fstart_pci::PCI_MEMORY_BAR::PREFETCHABLE.is_set(device.read32(0x10))
        || base == 0
        || base & 0xfff != 0
        || window.size < size_of::<Registers>() as u64
        || window.base.checked_add(window.size).is_none()
        || base.checked_add(size_of::<Registers>() - 1).is_none()
        || device.memory_bar(0x10) != Some(window.base)
    {
        return Err(Error::Resource);
    }
    // The native HDA BAR assignment is below 4 GiB. Never truncate a canonical
    // 64-bit allocation as the source does when it clears HDAUBA.
    if window.base >= 1 << 32 {
        return Err(Error::Resource);
    }
    if EcamDevice::new(0, 2, 0).is_present() && audio.is_none() {
        return Err(Error::MissingDisplayAudio);
    }
    let native = NativeHda {
        device,
        regs: unsafe { &*(base as *const Registers) },
        codec: HdaController::new(base),
        pcr,
    };
    native.configure(dsp, vc1)?;
    // Source-order GPU display power-up belongs HERE, before codec discovery,
    // not a second independent owner or an earlier duplicate SA invocation.
    if let Some((graphics, mhz96, tmode2t)) = audio {
        graphics.init_display_audio(delay)?;
        graphics.configure_audio_frequency(mhz96, tmode2t)?;
    }
    let graphics = audio.map(|(graphics, _, _)| graphics);
    let detected = native.codec.detect_codecs_checked(delay)?;
    if detected == 0 {
        return Ok(Outcome::NoCodecs);
    }
    native.frequencies(detected, graphics, delay)?;
    native.regs.latency.modify(Latency::VALUE.val(0x29));
    native
        .regs
        .power
        .modify(PowerControl::LOW.val(0) + PowerControl::BIT3::SET + PowerControl::BIT5::SET);
    if detected & 1 != 0 && native.codec.send_verb_with_delay(0x000f3700, delay)? & 1 != 0 {
        native.codec.reset_with_delay(true, delay)?;
        unsafe { device.register::<u16, PowerState::Register>(0x54) }
            .modify(PowerState::STATE.val(3) + PowerState::PME_STATUS::CLEAR);
        return Ok(Outcome::Parked6Mhz { detected });
    }
    // Shared azalia_audio_init's second discovery, then the real board verbs.
    let detected = native.codec.detect_codecs_checked(delay)?;
    if detected == 0 {
        return Ok(Outcome::NoCodecs);
    }
    let programmed = native
        .codec
        .program_verb_tables_checked(verbs, detected, delay)?;
    Ok(Outcome::Active {
        detected,
        programmed,
    })
}
