// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCH-H auxiliary-device ownership. SerialIO and ISH are real native setup;
//! SCS is LP-only and MUST NOT be accessed on H (skylake/scs.c SHA256
//! e32117bff8a32dd24152c4f29b0ca0f665f4eb861550d25d4eb95479dc39da40).
//! power::configure_native owns the CIO2 H-only CPPM[12] branch; no duplicate
//! CAM FLS/PSF LP recipe or camera initialization is performed here
//! (cio2.c 08ba739c90c082b52b557b888dc4f92125376f5ff252bd6a6c068ae2081af5ff).
use super::{
    dmi, hsio, ish,
    pcr::{self, Pcr, Port},
    pmc::{self, PmcRegisters},
    serialio,
};
use fstart_pci::{EcamDevice, PciEcam, PciExcludedFunctions, PciFixedBar};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub serialio: serialio::Config,
    pub ish: ish::Policy,
}
impl Config {
    pub const DISABLED: Self = Self {
        serialio: serialio::Config::DISABLED,
        ish: ish::Policy::Disabled,
    };
    fn check_pad_conflicts(&self, kind: dmi::Kind) -> Result<(), Error> {
        let mut seen = [false; 204];
        for pad in self.serialio.pads_for(kind)? {
            let number = pad.number_for(kind).map_err(|_| Error::Policy)?;
            if core::mem::replace(&mut seen[usize::from(number)], true) {
                return Err(Error::PadConflict);
            }
        }
        for (pad, _) in self.ish.pads_for(kind)? {
            let number = pad.number_for(kind).map_err(|_| Error::Policy)?;
            if core::mem::replace(&mut seen[usize::from(number)], true) {
                return Err(Error::PadConflict);
            }
        }
        Ok(())
    }
    pub(crate) fn validate(&self, interrupt_devices: &[u8]) -> Result<(), Error> {
        self.serialio.validate()?;
        if self.serialio.validate_for(dmi::Kind::H).is_ok() {
            self.check_pad_conflicts(dmi::Kind::H)?;
        }
        if self.serialio.validate_for(dmi::Kind::Lp).is_ok() {
            self.check_pad_conflicts(dmi::Kind::Lp)?;
        }
        for devfn in [
            0xa8, 0xa9, 0xaa, 0xab, 0xca, 0xc9, 0xf2, 0xf3, 0xf0, 0xf1, 0xc8,
        ] {
            if self.serialio.interrupt_devfns().any(|value| value == devfn)
                != interrupt_devices.contains(&devfn)
            {
                return Err(Error::Policy);
            }
        }
        if self.ish.enabled() != interrupt_devices.contains(&0x98)
            || [0xa3, 0xf4, 0xf5, 0xf6, 0xf7]
                .iter()
                .any(|devfn| interrupt_devices.contains(devfn))
        {
            return Err(Error::Policy);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Device,
    PadConflict,
    Readback,
    Pcr(pcr::Error),
    SerialIo(serialio::Error),
    Ish(ish::Error),
    Hsio(hsio::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
impl From<serialio::Error> for Error {
    fn from(e: serialio::Error) -> Self {
        Self::SerialIo(e)
    }
}
impl From<ish::Error> for Error {
    fn from(e: ish::Error) -> Self {
        Self::Ish(e)
    }
}
impl From<hsio::Error> for Error {
    fn from(e: hsio::Error) -> Self {
        Self::Hsio(e)
    }
}
register_bitfields![u32,
    Shadow [ DISABLE OFFSET(8) NUMBITS(1) [] ],
    ScsPmCtl [ PWR_GATING OFFSET(0) NUMBITS(6) [] ],
    ScsGpprvrw2 [
        EMMC_DIS OFFSET(1) NUMBITS(1) [],
        SDIO_SDCARD_DIS OFFSET(2) NUMBITS(1) []
    ],
    CamChcFncfg [ BCLD OFFSET(0) NUMBITS(1) [] ]
];
pub(crate) struct Prepared {
    serialio: serialio::Prepared,
    ish: ish::Outcome,
}
#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    pub serialio: serialio::Outcome,
    pub ish: ish::Outcome,
}
impl Prepared {
    pub(crate) fn outcome(&self) -> Outcome {
        Outcome {
            serialio: self.serialio.outcome(),
            ish: self.ish,
        }
    }
    pub(crate) fn fixed(&self) -> &[PciFixedBar] {
        self.serialio.fixed()
    }
    pub(crate) fn excluded(&self) -> PciExcludedFunctions {
        self.serialio.excluded()
    }
}
/// Disable LP-only Storage and Communications Subsystem (SDIO/eMMC/SDCard)
/// per coreboot `skylake/scs.c`.
fn disable_scs_lp(lpc_rev: u8, pcr: &Pcr, pmc: &PmcRegisters) -> Result<(), Error> {
    const SCS_CLOCK_GATING: u32 = 0x800d_e7e7;
    const SCS_HIGH_SPEED_TRUNK_REQ: u32 = 1 << 3;
    pcr.register::<ScsPmCtl::Register>(Port::Scs, 0x1d0)?
        .write(ScsPmCtl::PWR_GATING.val(0x3f));
    let clock_gating = SCS_CLOCK_GATING
        | if lpc_rev > 0x20 {
            SCS_HIGH_SPEED_TRUNK_REQ
        } else {
            0
        };
    pcr.register::<()>(Port::Scs, 0x600)?.set(clock_gating);

    // Order matches coreboot scs.c: SDIO (0:1e:5), eMMC (0:1e:4), SDCard (0:1e:6).
    let gpprvrw2 = pcr.register::<ScsGpprvrw2::Register>(Port::Scs, 0x604)?;
    for (func, psf_base, is_sdcard, is_emmc) in [
        (5u8, 0x400u16, false, false),
        (4u8, 0x500u16, false, true),
        (6u8, 0x300u16, true, false),
    ] {
        let dev = EcamDevice::new(0, 0x1e, func);
        if dev.is_present() {
            dev.write16(0xa2, dev.read16(0xa2) | 0xf);
        }
        if is_sdcard {
            gpprvrw2.modify(ScsGpprvrw2::SDIO_SDCARD_DIS::CLEAR);
        } else if is_emmc {
            gpprvrw2.modify(ScsGpprvrw2::EMMC_DIS::SET);
        } else {
            gpprvrw2.modify(ScsGpprvrw2::SDIO_SDCARD_DIS::SET);
        }
        if dev.is_present() {
            dev.write32(0x84, dev.read32(0x84) | 3);
            let _ = dev.read32(0x84);
        }
        pcr.register::<Shadow::Register>(Port::Psf2, psf_base + 0x1c)?
            .modify(Shadow::DISABLE::SET);
    }
    // Also hide the LP-only SCC UFS function (0:1e:7, PSF2 T0 shadow at 0x200)
    // so it does not remain visible to config-space scans without an INTx route.
    pcr.register::<Shadow::Register>(Port::Psf2, 0x200 + 0x1c)?
        .modify(Shadow::DISABLE::SET);
    pmc.cppm_control.modify(pmc::CppmControl::SCS_DISABLE::SET);
    pmc.non_static_disable
        .modify(pmc::NonStaticDisable::SCC::SET);
    Ok(())
}
/// Disable LP-only CIO2 camera controller (0:14:3) per coreboot `skylake/cio2.c`.
fn disable_cio2_lp(pcr: &Pcr, pmc: &PmcRegisters) -> Result<(), Error> {
    let shadow = pcr.register::<Shadow::Register>(Port::Psf1, 0x31c)?;
    let fused = pmc.subsystem_fuse.is_set(pmc::SubsystemFuse::CAM);
    if !fused {
        let dev = EcamDevice::new(0, 0x14, 3);
        if dev.is_present() {
            dev.write32(0xd4, dev.read32(0xd4) | 3);
        }
    }
    shadow.modify(Shadow::DISABLE::SET);
    if !fused
        && !pmc.function_disable.is_set(pmc::FunctionDisable::LOCK)
        && !pmc.function_disable.is_set(pmc::FunctionDisable::CAM)
    {
        pmc.function_disable.modify(pmc::FunctionDisable::CAM::SET);
    }
    pcr.register::<CamChcFncfg::Register>(Port::CamChc, 0x0c)?
        .modify(CamChcFncfg::BCLD::SET);
    if EcamDevice::new(0, 0x14, 3).is_present() {
        return Err(Error::Readback);
    }
    Ok(())
}
/// # Safety
/// Actual authenticated cold H/LP silicon, exclusive engines/pads/decodes under
/// the SerialIO/ISH/SCS/CIO2 ownership contracts. Config validation (including
/// global pad conflicts and IRQ inventory) preceded hardware writes. No PCI scan yet.
pub(crate) unsafe fn prepare(
    config: &Config,
    pcr: &Pcr,
    pmc: &PmcRegisters,
) -> Result<Prepared, Error> {
    let lpc = EcamDevice::new(0, 0x1f, 0);
    let kind = hsio::Silicon::read(&lpc)?.kind();
    config.check_pad_conflicts(kind)?;
    let serialio = unsafe { serialio::prepare(config.serialio, pcr, pmc) }?;
    let ish = unsafe { ish::prepare(config.ish, pcr, pmc) }?;
    if kind == dmi::Kind::Lp {
        disable_scs_lp(lpc.read8(8), pcr, pmc)?;
        disable_cio2_lp(pcr, pmc)?;
    }
    Ok(Prepared { serialio, ish })
}
pub(crate) fn finish(prepared: &Prepared, pci: &PciEcam) -> Result<(), Error> {
    serialio::finish(&prepared.serialio, pci)?;
    ish::finish(prepared.ish, pci)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conflicting_ish_uart_and_host_i2c_pad_owners_reject_before_hardware() {
        let mut config = Config::DISABLED;
        config.serialio.controllers[2].mode = serialio::Mode::Pci;
        let mut pads = ish::Pads::NONE;
        pads.uart[0] = true;
        config.ish = ish::Policy::Pci(pads);
        assert_eq!(config.validate(&[0xaa, 0x98]), Err(Error::PadConflict));
        config.ish = ish::Policy::Disabled;
        assert_eq!(config.validate(&[0xaa]), Ok(()));
        assert_eq!(config.validate(&[0xaa, 0xf4]), Err(Error::Policy));
    }
}
