// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCH-H auxiliary-device ownership. SerialIO and ISH are real native setup;
//! SCS is LP-only and MUST NOT be accessed on H (skylake/scs.c SHA256
//! e32117bff8a32dd24152c4f29b0ca0f665f4eb861550d25d4eb95479dc39da40).
//! power::configure_native owns the CIO2 H-only CPPM[12] branch; no duplicate
//! CAM FLS/PSF LP recipe or camera initialization is performed here
//! (cio2.c 08ba739c90c082b52b557b888dc4f92125376f5ff252bd6a6c068ae2081af5ff).
use super::{hsio, ish, pcr::Pcr, pmc::PmcRegisters, serialio};
use fstart_pci::{EcamDevice, PciEcam, PciExcludedFunctions, PciFixedBar};
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
    pub(crate) fn validate(&self, interrupt_devices: &[u8]) -> Result<(), Error> {
        self.serialio.validate()?;
        let mut seen = [false; 204];
        for pad in self.serialio.pads()? {
            if core::mem::replace(&mut seen[usize::from(pad.number())], true) {
                return Err(Error::PadConflict);
            }
        }
        for (pad, _) in self.ish.pads()? {
            if core::mem::replace(&mut seen[usize::from(pad.number())], true) {
                return Err(Error::PadConflict);
            }
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
            || [0xa3, 0xf4, 0xf5, 0xf6]
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
    SerialIo(serialio::Error),
    Ish(ish::Error),
    Hsio(hsio::Error),
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
/// # Safety
/// Actual authenticated cold H silicon, exclusive engines/pads/decodes under
/// the SerialIO/ISH ownership contracts. Config validation (including global
/// pad conflicts and IRQ inventory) preceded hardware writes. No PCI scan yet.
pub(crate) unsafe fn prepare(
    config: &Config,
    pcr: &Pcr,
    pmc: &PmcRegisters,
) -> Result<Prepared, Error> {
    if hsio::Silicon::read(&EcamDevice::new(0, 0x1f, 0))?.kind() != super::dmi::Kind::H {
        return Err(Error::Device);
    }
    let serialio = unsafe { serialio::prepare(config.serialio, pcr, pmc) }?;
    let ish = unsafe { ish::prepare(config.ish, pcr, pmc) }?;
    // No SCS access here: native returns before accessing PID_SCS on H.
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
