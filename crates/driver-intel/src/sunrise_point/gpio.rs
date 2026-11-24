// SPDX-License-Identifier: MIT OR Apache-2.0
//! PCH-H pad programming, from Skylake gpio.c and common/block/gpio/gpio.c.
//!
//! PCH-LP has a different numbering/layout and is deliberately not accepted.
//! Pad tables belong to boards. This module does not transfer CSME/ISH pad
//! ownership or implement the non-posted SBI configuration-lock operation.

use super::pcr::{self, Pcr, Port};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u32,
    pub PadDw0 [
        TX_STATE OFFSET(0) NUMBITS(1) [],
        RX_STATE OFFSET(1) NUMBITS(1) [],
        TX_DISABLE OFFSET(8) NUMBITS(1) [],
        RX_DISABLE OFFSET(9) NUMBITS(1) [],
        MODE OFFSET(10) NUMBITS(3) [],
        ROUTE_NMI OFFSET(17) NUMBITS(1) [],
        ROUTE_SMI OFFSET(18) NUMBITS(1) [],
        ROUTE_SCI OFFSET(19) NUMBITS(1) [],
        ROUTE_IOAPIC OFFSET(20) NUMBITS(1) [],
        RX_TERMINATION OFFSET(21) NUMBITS(2) [],
        RX_INVERT OFFSET(23) NUMBITS(1) [],
        GLITCH_FILTER OFFSET(24) NUMBITS(1) [],
        TRIGGER OFFSET(25) NUMBITS(2) [],
        NATIVE_VIRTUAL_WIRE OFFSET(27) NUMBITS(1) [],
        RX_RAW OFFSET(28) NUMBITS(1) [],
        RX_PAD_STATE_SELECT OFFSET(29) NUMBITS(1) [],
        RESET OFFSET(30) NUMBITS(2) []
    ],
    pub PadDw1 [
        IRQ OFFSET(0) NUMBITS(8) [],
        IO_STANDBY_TERMINATION OFFSET(8) NUMBITS(2) [],
        PULL OFFSET(10) NUMBITS(4) [],
        IO_STANDBY_STATE OFFSET(14) NUMBITS(4) [],
        TOLERANT_1V8 OFFSET(25) NUMBITS(1) []
    ],
    pub MiscConfig [
        DEEP_CLOCK_GATE OFFSET(0) NUMBITS(1) [],
        DEEP_POWER_GATE OFFSET(1) NUMBITS(1) [],
        IRQ15 OFFSET(3) NUMBITS(1) [],
        GPE0_DW0 OFFSET(8) NUMBITS(4) [],
        GPE0_DW1 OFFSET(12) NUMBITS(4) [],
        GPE0_DW2 OFFSET(16) NUMBITS(4) []
    ]
];

pub(crate) const SMI_STATUS: u16 = 0x180;
pub(crate) const SMI_ENABLE: u16 = 0x1a0;

const DW0_WRITABLE: u32 = 0xfffe_1f01;
// SPT selects PADTOL, but not IOSTANDBY. Preserve IRQ and IOSSTATE fields.
const DW1_WRITABLE: u32 = 0x0200_3f00;
const SOURCE_DRIVER_OWNER: u32 = 1 << 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Pcr(pcr::Error),
    InvalidPad,
    InvalidFunction,
    UnsupportedReset,
    UnsupportedInterruptRoute,
    DuplicateGpeGroup,
    InvalidIoapicIrq,
    PadLocked,
}
impl From<pcr::Error> for Error {
    fn from(value: pcr::Error) -> Self {
        Self::Pcr(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    Gpd,
}

/// Exact group metadata, not a division of the pad number by 24. Group E is
/// short, so F/G/H group registers and pad-config offsets use different strides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupInfo {
    pub port: Port,
    pub first: u16,
    pub count: u8,
    pub community_first: u16,
    pub index: u8,
    pub pmc_selector: u8,
}
impl Group {
    /// PCH-H SMI/NMI banks exist only for B/C/D/E/I. Other groups' offsets
    /// in those windows are reserved (Intel 332691-002EN, chapter 25).
    pub const fn has_smi_nmi_registers(self) -> bool {
        matches!(self, Self::B | Self::C | Self::D | Self::E | Self::I)
    }

    pub const fn info(self) -> GroupInfo {
        let (port, first, count, community_first, index, pmc_selector) = match self {
            Self::A => (Port::Gpio0, 0, 24, 0, 0, 0),
            Self::B => (Port::Gpio0, 24, 24, 0, 1, 1),
            Self::C => (Port::Gpio1, 48, 24, 48, 0, 2),
            Self::D => (Port::Gpio1, 72, 24, 48, 1, 3),
            Self::E => (Port::Gpio1, 96, 13, 48, 2, 4),
            Self::F => (Port::Gpio1, 109, 24, 48, 3, 5),
            Self::G => (Port::Gpio1, 133, 24, 48, 4, 6),
            Self::H => (Port::Gpio1, 157, 24, 48, 5, 7),
            Self::I => (Port::Gpio3, 181, 11, 181, 0, 8),
            Self::Gpd => (Port::Gpio2, 192, 12, 192, 0, 9),
        };
        GroupInfo {
            port,
            first,
            count,
            community_first,
            index,
            pmc_selector,
        }
    }
    pub const fn pad(self, pin: u8) -> Result<Pad, Error> {
        let info = self.info();
        if pin >= info.count {
            return Err(Error::InvalidPad);
        }
        Ok(Pad(info.first + pin as u16))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad(u16);
impl Pad {
    pub const fn from_number(number: u16) -> Result<Self, Error> {
        if number > 203 {
            return Err(Error::InvalidPad);
        }
        Ok(Self(number))
    }
    pub const fn number(self) -> u16 {
        self.0
    }
    pub const fn group(self) -> Group {
        match self.0 {
            0..=23 => Group::A,
            24..=47 => Group::B,
            48..=71 => Group::C,
            72..=95 => Group::D,
            96..=108 => Group::E,
            109..=132 => Group::F,
            133..=156 => Group::G,
            157..=180 => Group::H,
            181..=191 => Group::I,
            _ => Group::Gpd,
        }
    }
    pub const fn config_offset(self) -> u16 {
        0x400 + 8 * (self.0 - self.group().info().community_first)
    }
    pub const fn group_bit(self) -> u32 {
        1 << (self.0 - self.group().info().first)
    }
}

/// A source-format board pad entry. DW0 reset bits use the source logical
/// encoding (PWROK=0, DEEP=1, PLTRST=2, RSMRST=3), translated before writing.
/// DW1 bit 4 is ownership metadata, never a writable pad IRQ-selection bit.
#[derive(Debug, Clone, Copy)]
pub struct PadConfig {
    pub pad: Pad,
    pub dw0: u32,
    pub dw1: u32,
}

struct Prepared {
    dw0: u32,
    dw1: u32,
    irq: Option<u8>,
}

pub struct Gpio<'a> {
    pub pcr: &'a Pcr,
    /// Matches source SOC_INTEL_COMMON_BLOCK_GPIO_ITSS_POL_CFG policy.
    pub program_itss_polarity: bool,
}
impl Gpio<'_> {
    fn prepare(&self, config: &PadConfig) -> Result<Prepared, Error> {
        let info = config.pad.group().info();
        let dw0 = LocalRegisterCopy::<u32, PadDw0::Register>::new(config.dw0);
        // Do not write a group's reserved SMI/NMI window merely because
        // another bank in this community implements these interrupt types.
        if (dw0.is_set(PadDw0::ROUTE_SMI) || dw0.is_set(PadDw0::ROUTE_NMI))
            && !config.pad.group().has_smi_nmi_registers()
        {
            return Err(Error::UnsupportedInterruptRoute);
        }
        let reset = match (info.port == Port::Gpio2, dw0.read(PadDw0::RESET)) {
            (true, value) => value,
            (false, 3) => 0,
            (false, 1) => 1,
            (false, 2) => 2,
            _ => return Err(Error::UnsupportedReset),
        };
        let lock_offset = 0x90 + 8 * u16::from(info.index);
        let locked = self.pcr.register::<()>(info.port, lock_offset)?.get()
            | self.pcr.register::<()>(info.port, lock_offset + 4)?.get();
        if locked & config.pad.group_bit() != 0 {
            return Err(Error::PadLocked);
        }
        let offset = config.pad.config_offset();
        let old0 = self
            .pcr
            .register::<PadDw0::Register>(info.port, offset)?
            .get();
        let old1 = self
            .pcr
            .register::<PadDw1::Register>(info.port, offset + 4)?
            .get();
        let irq = if dw0.is_set(PadDw0::ROUTE_IOAPIC) {
            let irq = LocalRegisterCopy::<u32, PadDw1::Register>::new(old1).read(PadDw1::IRQ);
            if irq == 0 || irq > 119 {
                return Err(Error::InvalidIoapicIrq);
            }
            Some(irq as u8)
        } else {
            None
        };
        let mut mapped = dw0;
        mapped.modify(PadDw0::RESET.val(reset));
        Ok(Prepared {
            dw0: (old0 & !DW0_WRITABLE) | (mapped.get() & DW0_WRITABLE),
            dw1: (old1 & !DW1_WRITABLE) | (config.dw1 & DW1_WRITABLE),
            irq,
        })
    }

    /// Native peripheral mux update preserving DW1, ownership and pending
    /// events. Used by CLKREQ; it is not a whole board-pad reconfiguration.
    pub fn native_function_deep(&self, pad: Pad, function: u8) -> Result<(), Error> {
        self.native_mux(pad, function, true)
    }
    /// Change only native mux mode, preserving reset, buffers, DW1, ownership
    /// and event routing. ISH native pad assignments use this operation.
    pub fn native_function(&self, pad: Pad, function: u8) -> Result<(), Error> {
        self.native_mux(pad, function, false)
    }
    fn native_mux(&self, pad: Pad, function: u8, deep: bool) -> Result<(), Error> {
        if function == 0 || function > 7 {
            return Err(Error::InvalidFunction);
        }
        let info = pad.group().info();
        if info.port == Port::Gpio2 {
            return Err(Error::InvalidPad);
        }
        let register = self
            .pcr
            .register::<PadDw0::Register>(info.port, pad.config_offset())?;
        let mut value = register.extract();
        value.modify(PadDw0::MODE.val(u32::from(function)));
        if deep {
            value.modify(PadDw0::RESET.val(1) + PadDw0::TX_DISABLE::SET + PadDw0::RX_DISABLE::SET);
        } else {
            // prepare() consumes source-logical reset encoding. Physical
            // GPP RSMRST=0 must be expressed as source RSMRST=3, not PWROK.
            match value.read(PadDw0::RESET) {
                0 => value.modify(PadDw0::RESET.val(3)),
                1 | 2 => {}
                _ => return Err(Error::UnsupportedReset),
            }
        }
        let dw1 = self
            .pcr
            .register::<PadDw1::Register>(info.port, pad.config_offset() + 4)?
            .get();
        let prepared = self.prepare(&PadConfig {
            pad,
            dw0: value.get(),
            dw1,
        })?;
        register.set(prepared.dw0);
        if (register.get() ^ prepared.dw0) & DW0_WRITABLE != 0 {
            return Err(Error::Pcr(pcr::Error::Readback));
        }
        Ok(())
    }

    /// Validate the whole table before any writes, then program source order.
    /// Caller owns pad configuration and any ITSS/SMI/NMI routing during this
    /// operation; this is not an SMM or concurrent CSME ownership-transfer API.
    /// Existing SBI locks are honored; this does not lock pads after setup.
    pub fn configure_pads(&self, configs: &[PadConfig]) -> Result<(), Error> {
        for config in configs {
            self.prepare(config)?;
        }
        for config in configs {
            let prepared = self.prepare(config)?;
            let info = config.pad.group().info();
            let offset = config.pad.config_offset();
            self.pcr
                .register::<PadDw0::Register>(info.port, offset)?
                .set(prepared.dw0);
            self.pcr
                .register::<PadDw1::Register>(info.port, offset + 4)?
                .set(prepared.dw1);
            if self.program_itss_polarity
                && let Some(irq) = prepared.irq
            {
                let register = self
                    .pcr
                    .register::<()>(Port::Itss, 0x3200 + 4 * u16::from(irq / 32))?;
                let bit = 1u32 << (irq % 32);
                let invert = config.dw0 & PadDw0::RX_INVERT::SET.value != 0;
                register.set((register.get() & !bit) | if invert { bit } else { 0 });
            }
            let bit = config.pad.group_bit();
            let group = 4 * u16::from(info.index);
            let owner = self.pcr.register::<()>(info.port, 0xd0 + group)?;
            owner.set(
                (owner.get() & !bit)
                    | if config.dw1 & SOURCE_DRIVER_OWNER != 0 {
                        bit
                    } else {
                        0
                    },
            );
            // The supplied SPT descriptors omit the GPE offsets. The PCH-H
            // register map explicitly defines STS=0x140, EN=0x160, group*4
            // (Intel 332691-002EN, 25.1–25.4), not offset zero.
            for (route, status, enable) in [
                (PadDw0::ROUTE_SCI::SET.value, 0x140, 0x160),
                (PadDw0::ROUTE_SMI::SET.value, SMI_STATUS, SMI_ENABLE),
                (PadDw0::ROUTE_NMI::SET.value, 0x1c0, 0x1e0),
            ] {
                if config.dw0 & route == 0 {
                    continue;
                }
                let status = self.pcr.register::<()>(info.port, status + group)?;
                // W1C: acknowledge only the target pad's observed status.
                status.set(status.get() & bit);
                let enable = self.pcr.register::<()>(info.port, enable + group)?;
                enable.set(enable.get() | bit);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pch_h_pad_groups_do_not_confuse_short_e_group_with_register_stride() {
        let f = Group::F.pad(0).unwrap();
        assert_eq!(f.number(), 109);
        assert_eq!(f.config_offset(), 0x5e8);
        assert_eq!(f.group_bit(), 1);
        assert_eq!(f.group().info().index, 3);
        assert_eq!(Group::H.pad(23).unwrap().config_offset(), 0x820);
        assert_eq!(Group::I.pad(10).unwrap().config_offset(), 0x450);
        assert_eq!(Group::Gpd.pad(11).unwrap().config_offset(), 0x458);
        assert_eq!(Group::E.pad(13), Err(Error::InvalidPad));
        assert_eq!(Group::I.pad(11), Err(Error::InvalidPad));
        assert_eq!(Pad::from_number(204), Err(Error::InvalidPad));
        for n in 0..204 {
            let pad = Pad::from_number(n).unwrap();
            let info = pad.group().info();
            assert!(n >= info.first && n < info.first + u16::from(info.count));
        }
    }
}
