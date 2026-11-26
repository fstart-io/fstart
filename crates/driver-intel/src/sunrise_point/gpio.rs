// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sunrise Point-H/LP pad programming, from Skylake gpio.c and common/block/gpio/gpio.c.
//!
//! Pad tables belong to boards. This module does not transfer CSME/ISH pad
//! ownership or implement the non-posted SBI configuration-lock operation.

use super::{
    dmi,
    pcr::{self, Pcr, Port},
};
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
/// short on PCH-H and Group G is short on PCH-LP, so group registers and
/// pad-config offsets use different strides.
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
    /// SMI/NMI banks exist only for B/C/D/E/I on PCH-H and B/C/D/E on PCH-LP.
    /// Other groups' offsets in those windows are reserved.
    pub const fn has_smi_nmi_registers_for(self, kind: dmi::Kind) -> bool {
        match kind {
            dmi::Kind::H => matches!(self, Self::B | Self::C | Self::D | Self::E | Self::I),
            dmi::Kind::Lp => matches!(self, Self::B | Self::C | Self::D | Self::E),
        }
    }

    pub const fn info_for(self, kind: dmi::Kind) -> Option<GroupInfo> {
        let (port, first, count, community_first, index, pmc_selector) = match (kind, self) {
            (dmi::Kind::H, Self::A) => (Port::Gpio0, 0, 24, 0, 0, 0),
            (dmi::Kind::H, Self::B) => (Port::Gpio0, 24, 24, 0, 1, 1),
            (dmi::Kind::H, Self::C) => (Port::Gpio1, 48, 24, 48, 0, 2),
            (dmi::Kind::H, Self::D) => (Port::Gpio1, 72, 24, 48, 1, 3),
            (dmi::Kind::H, Self::E) => (Port::Gpio1, 96, 13, 48, 2, 4),
            (dmi::Kind::H, Self::F) => (Port::Gpio1, 109, 24, 48, 3, 5),
            (dmi::Kind::H, Self::G) => (Port::Gpio1, 133, 24, 48, 4, 6),
            (dmi::Kind::H, Self::H) => (Port::Gpio1, 157, 24, 48, 5, 7),
            (dmi::Kind::H, Self::I) => (Port::Gpio3, 181, 11, 181, 0, 8),
            (dmi::Kind::H, Self::Gpd) => (Port::Gpio2, 192, 12, 192, 0, 9),
            (dmi::Kind::Lp, Self::A) => (Port::Gpio0, 0, 24, 0, 0, 0),
            (dmi::Kind::Lp, Self::B) => (Port::Gpio0, 24, 24, 0, 1, 1),
            (dmi::Kind::Lp, Self::C) => (Port::Gpio1, 48, 24, 48, 0, 2),
            (dmi::Kind::Lp, Self::D) => (Port::Gpio1, 72, 24, 48, 1, 3),
            (dmi::Kind::Lp, Self::E) => (Port::Gpio1, 96, 24, 48, 2, 4),
            (dmi::Kind::Lp, Self::F) => (Port::Gpio3, 120, 24, 120, 0, 5),
            (dmi::Kind::Lp, Self::G) => (Port::Gpio3, 144, 8, 120, 1, 6),
            (dmi::Kind::Lp, Self::Gpd) => (Port::Gpio2, 152, 12, 152, 0, 7),
            (dmi::Kind::Lp, Self::H | Self::I) => return None,
        };
        Some(GroupInfo {
            port,
            first,
            count,
            community_first,
            index,
            pmc_selector,
        })
    }

    const fn max_count(self) -> u8 {
        match self {
            Self::A | Self::B | Self::C | Self::D | Self::E | Self::F | Self::G | Self::H => 24,
            Self::I => 11,
            Self::Gpd => 12,
        }
    }

    /// Construct a board pad identifier. Exact PCH-H/LP bounds are validated
    /// when programmed against a concrete silicon `dmi::Kind`.
    pub const fn pad(self, pin: u8) -> Result<Pad, Error> {
        if pin >= self.max_count() {
            return Err(Error::InvalidPad);
        }
        Ok(Pad { group: self, pin })
    }

    pub const fn pad_for(self, kind: dmi::Kind, pin: u8) -> Result<Pad, Error> {
        match self.info_for(kind) {
            Some(info) if pin < info.count => Ok(Pad { group: self, pin }),
            _ => Err(Error::InvalidPad),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad {
    group: Group,
    pin: u8,
}
impl Pad {
    pub const fn from_number_for(kind: dmi::Kind, number: u16) -> Result<Self, Error> {
        const GROUPS: [Group; 10] = [
            Group::A,
            Group::B,
            Group::C,
            Group::D,
            Group::E,
            Group::F,
            Group::G,
            Group::H,
            Group::I,
            Group::Gpd,
        ];
        let mut i = 0;
        while i < GROUPS.len() {
            if let Some(info) = GROUPS[i].info_for(kind)
                && number >= info.first
                && number < info.first + info.count as u16
            {
                return Ok(Self {
                    group: GROUPS[i],
                    pin: (number - info.first) as u8,
                });
            }
            i += 1;
        }
        Err(Error::InvalidPad)
    }

    pub const fn group(self) -> Group {
        self.group
    }

    pub const fn pin(self) -> u8 {
        self.pin
    }

    pub const fn number_for(self, kind: dmi::Kind) -> Result<u16, Error> {
        match self.group.info_for(kind) {
            Some(info) if self.pin < info.count => Ok(info.first + self.pin as u16),
            _ => Err(Error::InvalidPad),
        }
    }

    pub const fn info_for(self, kind: dmi::Kind) -> Result<GroupInfo, Error> {
        match self.group.info_for(kind) {
            Some(info) if self.pin < info.count => Ok(info),
            _ => Err(Error::InvalidPad),
        }
    }

    pub const fn config_offset_for(self, kind: dmi::Kind) -> Result<u16, Error> {
        match self.info_for(kind) {
            Ok(info) => Ok(0x400 + 8 * (info.first + self.pin as u16 - info.community_first)),
            Err(error) => Err(error),
        }
    }

    pub const fn group_bit(self) -> u32 {
        1 << self.pin
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
    info: GroupInfo,
    offset: u16,
    dw0: u32,
    dw1: u32,
    irq: Option<u8>,
}

pub struct Gpio<'a> {
    pub pcr: &'a Pcr,
    pub kind: dmi::Kind,
    /// Matches source SOC_INTEL_COMMON_BLOCK_GPIO_ITSS_POL_CFG policy.
    pub program_itss_polarity: bool,
}
impl Gpio<'_> {
    fn prepare(&self, config: &PadConfig) -> Result<Prepared, Error> {
        let info = config.pad.info_for(self.kind)?;
        let offset = config.pad.config_offset_for(self.kind)?;
        let dw0 = LocalRegisterCopy::<u32, PadDw0::Register>::new(config.dw0);
        // Do not write a group's reserved SMI/NMI window merely because
        // another bank in this community implements these interrupt types.
        if (dw0.is_set(PadDw0::ROUTE_SMI) || dw0.is_set(PadDw0::ROUTE_NMI))
            && !config.pad.group().has_smi_nmi_registers_for(self.kind)
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
        let mut dw1 = (old1 & !DW1_WRITABLE) | (config.dw1 & DW1_WRITABLE);
        // Sunrise Point-LP erratum/datasheet: GPP_F4..GPP_F11 have no 1.8V
        // tolerance control bit; keep PADTOL clear (coreboot skylake/gpio.c).
        if self.kind == dmi::Kind::Lp
            && config.pad.group() == Group::F
            && (4..=11).contains(&config.pad.pin())
        {
            dw1 &= !PadDw1::TOLERANT_1V8::SET.value;
        }
        Ok(Prepared {
            info,
            offset,
            dw0: (old0 & !DW0_WRITABLE) | (mapped.get() & DW0_WRITABLE),
            dw1,
            irq,
        })
    }

    /// Read the current input level (`RX_STATE`) of a GPIO pad.
    pub fn read_rx_state(&self, pad: Pad) -> Result<bool, Error> {
        let info = pad.info_for(self.kind)?;
        let offset = pad.config_offset_for(self.kind)?;
        Ok(self
            .pcr
            .register::<PadDw0::Register>(info.port, offset)?
            .is_set(PadDw0::RX_STATE))
    }

    /// Update the driven output level (`TX_STATE`) of an unlocked GPIO pad.
    pub fn write_tx_state(&self, pad: Pad, high: bool) -> Result<(), Error> {
        let info = pad.info_for(self.kind)?;
        let offset = pad.config_offset_for(self.kind)?;
        let lock_offset = 0x90 + 8 * u16::from(info.index);
        let locked = self.pcr.register::<()>(info.port, lock_offset)?.get()
            | self.pcr.register::<()>(info.port, lock_offset + 4)?.get();
        if locked & pad.group_bit() != 0 {
            return Err(Error::PadLocked);
        }
        let register = self.pcr.register::<PadDw0::Register>(info.port, offset)?;
        let mut value = register.extract();
        value.modify(PadDw0::TX_STATE.val(u32::from(high)));
        register.set(value.get());
        if register.is_set(PadDw0::TX_STATE) != high {
            return Err(Error::Pcr(pcr::Error::Readback));
        }
        Ok(())
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
        let info = pad.info_for(self.kind)?;
        if info.port == Port::Gpio2 {
            return Err(Error::InvalidPad);
        }
        let offset = pad.config_offset_for(self.kind)?;
        let register = self.pcr.register::<PadDw0::Register>(info.port, offset)?;
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
            .register::<PadDw1::Register>(info.port, offset + 4)?
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
            let info = prepared.info;
            let offset = prepared.offset;
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
            // The supplied SPT descriptors omit the GPE offsets. The PCH
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
    fn pch_h_and_lp_pad_groups_match_silicon_layouts() {
        let f_h = Group::F.pad_for(dmi::Kind::H, 0).unwrap();
        assert_eq!(f_h.number_for(dmi::Kind::H), Ok(109));
        assert_eq!(f_h.config_offset_for(dmi::Kind::H), Ok(0x5e8));
        assert_eq!(f_h.group_bit(), 1);
        assert_eq!(f_h.info_for(dmi::Kind::H).unwrap().index, 3);
        assert_eq!(
            Group::H
                .pad_for(dmi::Kind::H, 23)
                .unwrap()
                .config_offset_for(dmi::Kind::H),
            Ok(0x820)
        );
        assert_eq!(
            Group::I
                .pad_for(dmi::Kind::H, 10)
                .unwrap()
                .config_offset_for(dmi::Kind::H),
            Ok(0x450)
        );
        assert_eq!(
            Group::Gpd
                .pad_for(dmi::Kind::H, 11)
                .unwrap()
                .config_offset_for(dmi::Kind::H),
            Ok(0x458)
        );
        assert_eq!(Group::E.pad_for(dmi::Kind::H, 13), Err(Error::InvalidPad));
        assert_eq!(Group::I.pad_for(dmi::Kind::H, 11), Err(Error::InvalidPad));
        assert_eq!(
            Pad::from_number_for(dmi::Kind::H, 204),
            Err(Error::InvalidPad)
        );
        for n in 0..204 {
            let pad = Pad::from_number_for(dmi::Kind::H, n).unwrap();
            let info = pad.info_for(dmi::Kind::H).unwrap();
            assert!(n >= info.first && n < info.first + u16::from(info.count));
        }

        // Sunrise Point-LP: E has 24 pins, F lives in Community 3 at 120, G has 8 pins,
        // GPD lives in Community 2 at 152, and H/I do not exist.
        assert_eq!(
            Group::E
                .pad_for(dmi::Kind::Lp, 23)
                .unwrap()
                .config_offset_for(dmi::Kind::Lp),
            Ok(0x638)
        );
        let f_lp = Group::F.pad_for(dmi::Kind::Lp, 0).unwrap();
        assert_eq!(f_lp.number_for(dmi::Kind::Lp), Ok(120));
        assert_eq!(f_lp.info_for(dmi::Kind::Lp).unwrap().port, Port::Gpio3);
        assert_eq!(f_lp.config_offset_for(dmi::Kind::Lp), Ok(0x400));
        let g_lp = Group::G.pad_for(dmi::Kind::Lp, 7).unwrap();
        assert_eq!(g_lp.number_for(dmi::Kind::Lp), Ok(151));
        assert_eq!(g_lp.config_offset_for(dmi::Kind::Lp), Ok(0x4f8));
        assert_eq!(Group::G.pad_for(dmi::Kind::Lp, 8), Err(Error::InvalidPad));
        assert_eq!(Group::H.pad_for(dmi::Kind::Lp, 0), Err(Error::InvalidPad));
        assert_eq!(Group::I.pad_for(dmi::Kind::Lp, 0), Err(Error::InvalidPad));
        assert_eq!(
            Group::Gpd
                .pad_for(dmi::Kind::Lp, 11)
                .unwrap()
                .number_for(dmi::Kind::Lp),
            Ok(163)
        );
        assert_eq!(
            Pad::from_number_for(dmi::Kind::Lp, 164),
            Err(Error::InvalidPad)
        );
        for n in 0..164 {
            let pad = Pad::from_number_for(dmi::Kind::Lp, n).unwrap();
            let info = pad.info_for(dmi::Kind::Lp).unwrap();
            assert!(n >= info.first && n < info.first + u16::from(info.count));
        }
    }
}
