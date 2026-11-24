// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native skylake/ish.c SHA256
//! bb143d1752f8b559cb9a40b738bd13c6bf37e3de441ae525b154200da96a01ce.
//! Cold PCH-H visibility/pad policy only, not ISH firmware or sensor service.
use super::{
    dmi,
    gpio::{self, Gpio, Pad},
    hsio,
    pcr::{self, Pcr, Port},
    pmc::{self, PmcRegisters},
};
use fstart_pci::{EcamDevice, PciAddress, PciEcam};
use tock_registers::{
    interfaces::{ReadWriteable, Readable},
    register_bitfields,
};
#[derive(Debug, Clone, Copy)]
pub struct Pads {
    pub spi: bool,
    pub uart: [bool; 2],
    pub i2c: [bool; 3],
    pub gp: u8,
}
impl Pads {
    pub const NONE: Self = Self {
        spi: false,
        uart: [false; 2],
        i2c: [false; 3],
        gp: 0,
    };
}
#[derive(Debug, Clone, Copy)]
pub enum Policy {
    Disabled,
    Pci(Pads),
}
impl Policy {
    pub const fn enabled(self) -> bool {
        matches!(self, Self::Pci(_))
    }
    pub(crate) fn pads_for(self, kind: dmi::Kind) -> Result<heapless::Vec<(Pad, u8), 32>, Error> {
        const GP_H: [(u16, u8); 8] = [
            (18, 1),
            (19, 1),
            (20, 1),
            (21, 1),
            (22, 1),
            (23, 1),
            (12, 2),
            (17, 1),
        ];
        const GP_LP: [(u16, u8); 8] = [
            (18, 1),
            (19, 1),
            (20, 1),
            (21, 1),
            (22, 1),
            (23, 1),
            (12, 2),
            (17, 2),
        ];
        const I2C_H: [[(u16, u8); 2]; 3] = [
            [(176, 1), (177, 1)],
            [(178, 1), (179, 1)],
            [(76, 1), (95, 1)],
        ];
        const I2C_LP: [[(u16, u8); 2]; 3] =
            [[(77, 1), (78, 1)], [(79, 1), (80, 1)], [(130, 2), (131, 2)]];
        const UART: [[(u16, u8); 4]; 2] = [
            [(85, 1), (86, 1), (87, 1), (88, 1)],
            [(60, 2), (61, 2), (62, 2), (63, 2)],
        ];
        const SPI: [(u16, u8); 4] = [(81, 1), (82, 1), (83, 1), (84, 1)];
        let (gp_table, i2c_table) = match kind {
            dmi::Kind::H => (&GP_H, &I2C_H),
            dmi::Kind::Lp => (&GP_LP, &I2C_LP),
        };
        let mut result = heapless::Vec::new();
        if let Self::Pci(p) = self {
            for (pins, enabled) in i2c_table.iter().zip(p.i2c) {
                if enabled {
                    for (pad, function) in pins {
                        result
                            .push((Pad::from_number_for(kind, *pad)?, *function))
                            .map_err(|_| Error::Policy)?;
                    }
                }
            }
            for (pins, enabled) in UART.iter().zip(p.uart) {
                if enabled {
                    for (pad, function) in pins {
                        result
                            .push((Pad::from_number_for(kind, *pad)?, *function))
                            .map_err(|_| Error::Policy)?;
                    }
                }
            }
            if p.spi {
                for (pad, function) in SPI {
                    result
                        .push((Pad::from_number_for(kind, pad)?, function))
                        .map_err(|_| Error::Policy)?;
                }
            }
            for (index, (pad, function)) in gp_table.iter().enumerate() {
                if p.gp & (1 << index) != 0 {
                    result
                        .push((Pad::from_number_for(kind, *pad)?, *function))
                        .map_err(|_| Error::Policy)?;
                }
            }
        }
        Ok(result)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Device,
    Locked,
    Owned,
    Readback,
    GlobalResetRequired,
    Pcr(pcr::Error),
    Gpio(gpio::Error),
    Hsio(hsio::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
impl From<gpio::Error> for Error {
    fn from(e: gpio::Error) -> Self {
        Self::Gpio(e)
    }
}
impl From<hsio::Error> for Error {
    fn from(e: hsio::Error) -> Self {
        Self::Hsio(e)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Disabled,
    Fused,
    Pci,
}
register_bitfields![u32,
    Shadow [
        DISABLE OFFSET(8) NUMBITS(1) [],
        BAR23_DISABLE OFFSET(18) NUMBITS(2) []
    ],
    Control [ BAR1_DISABLE OFFSET(7) NUMBITS(1) [] ],Power [ ENABLE OFFSET(0) NUMBITS(6) [] ]
];
/// # Safety
/// Exclusive actual cold PCH-H/LP ISH/host config, PCR/PWRM and requested pads,
/// before static-disable locks or PCI enumeration. No OS/AP/SMM or host DMA
/// competes. CSE-owned firmware is not replaced or represented as initialized.
pub(crate) unsafe fn prepare(
    policy: Policy,
    pcr: &Pcr,
    pmc: &PmcRegisters,
) -> Result<Outcome, Error> {
    let kind = hsio::Silicon::read(&EcamDevice::new(0, 0x1f, 0))?.kind();
    if pmc.subsystem_fuse.get() == u32::MAX || pmc.function_disable.get() == u32::MAX {
        return Err(Error::Device);
    }
    let fused = pmc.subsystem_fuse.is_set(pmc::SubsystemFuse::ISH);
    let disabled = pmc.function_disable.is_set(pmc::FunctionDisable::ISH);
    let locked = pmc.function_disable.is_set(pmc::FunctionDisable::LOCK);
    let shadow = pcr.register::<Shadow::Register>(Port::Psf3, 0x81c)?;
    if shadow.get() == u32::MAX {
        return Err(Error::Device);
    }
    let device = EcamDevice::new(0, 0x13, 0);
    if device.is_present() {
        if device.vendor_id() != 0x8086 || device.read8(0x0e) & 0x7f != 0 {
            return Err(Error::Device);
        }
        if device.read16(4) & 7 != 0
            || device.memory_bar(0x10).is_some()
            || device.memory_bar(0x18).is_some()
        {
            return Err(Error::Owned);
        }
    }
    if policy.enabled() && !fused && disabled {
        if locked {
            return Err(Error::Locked);
        }
        pmc.function_disable
            .modify(pmc::FunctionDisable::ISH::CLEAR);
        if pmc.function_disable.is_set(pmc::FunctionDisable::ISH) {
            return Err(Error::Readback);
        }
        return Err(Error::GlobalResetRequired);
    }
    if !policy.enabled() || fused {
        // Match native disable order: hide the PCI function, then request
        // PMC disable if applicable. A fused or already absent function
        // need not latch a redundant disable request.
        shadow.modify(Shadow::DISABLE::SET);
        if !fused && !locked && !disabled {
            pmc.function_disable.modify(pmc::FunctionDisable::ISH::SET);
        }
        // Never pass a still-visible disabled function to the PCI allocator.
        // finish() checks that it also stayed absent during enumeration.
        if device.is_present() {
            return Err(Error::Readback);
        }
        return Ok(if fused {
            Outcome::Fused
        } else {
            Outcome::Disabled
        });
    }
    let pads = policy.pads_for(kind)?;
    // Validate each mux before changing the device's visibility.
    let gpio = Gpio {
        pcr,
        kind,
        program_itss_polarity: false,
    };
    for (pad, function) in pads {
        gpio.native_function(pad, function)?;
    }
    shadow.modify(Shadow::DISABLE::CLEAR + Shadow::BAR23_DISABLE.val(3));
    let control = pcr.register::<Control::Register>(Port::Ish, 0x200)?;
    let power = pcr.register::<Power::Register>(Port::Ish, 0x1d0)?;
    if control.get() == u32::MAX || power.get() == u32::MAX {
        return Err(Error::Device);
    }
    control.modify(Control::BAR1_DISABLE::SET);
    power.modify(Power::ENABLE.val(0x3f));
    if device.vendor_id() != 0x8086
        || shadow.is_set(Shadow::DISABLE)
        || shadow.read(Shadow::BAR23_DISABLE) != 3
        || !control.is_set(Control::BAR1_DISABLE)
        || power.read(Power::ENABLE) != 0x3f
    {
        return Err(Error::Readback);
    }
    Ok(Outcome::Pci)
}
pub(crate) fn finish(outcome: Outcome, pci: &PciEcam) -> Result<(), Error> {
    let address = PciAddress::new(0, 0, 0x13, 0);
    if outcome == Outcome::Pci {
        if !pci.devices().any(|a| a == address)
            || pci.assigned_memory_bar(address, 0x10).is_none()
            || pci.assigned_memory_bar(address, 0x18).is_some()
        {
            return Err(Error::Device);
        }
    } else if pci.devices().any(|a| a == address) {
        return Err(Error::Device);
    }
    Ok(())
}
