// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rs USB cold initialization. Relative-register interface checked against NFX:
//! reset ffa5363a, configuration ffa5446c, parameters ffa54ac7, cold flow
//! ffa54cb7 and completion ffa54416. Register offsets are NOT SP3's absolute
//! SMN-address USB_CONFIG_UPDATE ABI. Genoa's absolute interface is selected
//! explicitly and follows openSIL FCH/9004 and FchXhci.c, not SP3 tables.
//! No firmware pointers/DMA are submitted.
use crate::{
    smn::{SmnAccess, SmnReg},
    smu::{self, SmuMessages},
};
use core::marker::PhantomData;
use heapless::Vec;
use tock_registers::{
    RegisterLongName,
    fields::FieldValue,
    interfaces::{ReadWriteable, Readable},
    register_bitfields,
};

const CONFIG_UPDATE: u32 = 0x13;
const INIT_DONE: u32 = 0x0a;
register_bitfields![u32,
    pub Guard [
        SKIP_CONTROLLERS OFFSET(9) NUMBITS(1) [],
        SKIP_SUBSEQUENCE OFFSET(10) NUMBITS(1) []
    ],
    pub Visibility [ENABLE OFFSET(28) NUMBITS(1) []],
    Selector [
        SPACE OFFSET(0) NUMBITS(8) [],
        /// The debug pipe switch, the argument of operation D0h.
        DEBUG_PIPE OFFSET(0) NUMBITS(1) [],
        CONTROLLER OFFSET(20) NUMBITS(1) [],
        OPERATION OFFSET(24) NUMBITS(8) []
    ],
    OverCurrent [MAP OFFSET(0) NUMBITS(16) []],
    Removable [
        USB2 OFFSET(0) NUMBITS(2) [],
        USB3 OFFSET(16) NUMBITS(2) []
    ],
    RasControl [ENABLE OFFSET(12) NUMBITS(1) []],
    PortControl [
        OVER_CURRENT_LOW OFFSET(8) NUMBITS(1) [],
        /// The SuperSpeed port the USB3 PHY tuning writes go to.
        PORT_SELECT OFFSET(12) NUMBITS(4) [],
        FORCE_GEN1 OFFSET(16) NUMBITS(2) []
    ],
    Sparse [ENABLE OFFSET(17) NUMBITS(1) []],
    Usb2Tune [
        A OFFSET(0) NUMBITS(3) [],
        B OFFSET(12) NUMBITS(3) [],
        C OFFSET(16) NUMBITS(4) [],
        D OFFSET(20) NUMBITS(2) [],
        E OFFSET(23) NUMBITS(1) [],
        F OFFSET(24) NUMBITS(2) [],
        G OFFSET(28) NUMBITS(4) []
    ],
    Usb2Tune2 [
        H OFFSET(0) NUMBITS(2) [],
        I OFFSET(2) NUMBITS(2) []
    ],
    /// The USB3 PHY's 7-bit tuning registers (1242D8h, 13004Ch).
    Usb3Word [VALUE OFFSET(0) NUMBITS(7) []],
    Usb3Nibble [
        VALUE OFFSET(0) NUMBITS(4) [],
        BIT OFFSET(4) NUMBITS(1) []
    ],
    Usb3Tune [
        A OFFSET(0) NUMBITS(3) [],
        B OFFSET(3) NUMBITS(1) [],
        C OFFSET(4) NUMBITS(3) [],
        D OFFSET(7) NUMBITS(1) []
    ],
    /// A SuperSpeed link of the absolute (Genoa) interface.
    LinkControl [FORCE_GEN1 OFFSET(10) NUMBITS(1) []]
];

/// A register the SMU's configuration-update message writes: its space
/// and its offset in a controller (the relative interface; the absolute one
/// adds the controller's SMN base).
#[derive(Clone, Copy)]
struct UsbReg<R: RegisterLongName> {
    space: u32,
    offset: u32,
    _register: PhantomData<R>,
}
impl<R: RegisterLongName> UsbReg<R> {
    const fn new(space: u32, offset: u32) -> Self {
        Self {
            space,
            offset,
            _register: PhantomData,
        }
    }
    /// The `index`th of a register array `stride` bytes apart.
    const fn at(self, index: usize, stride: u32) -> Self {
        Self::new(self.space, self.offset + index as u32 * stride)
    }
}
const OVER_CURRENT: UsbReg<OverCurrent::Register> = UsbReg::new(1, 0x18_0120);
const REMOVABLE: UsbReg<Removable::Register> = UsbReg::new(1, 0x18_0114);
const RAS_CONTROL: UsbReg<RasControl::Register> = UsbReg::new(1, 0x18_0118);
const PORT_CONTROL: UsbReg<PortControl::Register> = UsbReg::new(1, 0x18_0130);
const SPARSE: UsbReg<Sparse::Register> = UsbReg::new(3, 0xc12c);
/// Per USB2 port, 400h apart.
const USB2_TUNE: UsbReg<Usb2Tune::Register> = UsbReg::new(1, 0x11_c000);
const USB2_TUNE2: UsbReg<Usb2Tune2::Register> = UsbReg::new(1, 0x11_c004);
const USB3_1242D8: UsbReg<Usb3Word::Register> = UsbReg::new(2, 0x12_42d8);
const USB3_12C064: UsbReg<Usb3Nibble::Register> = UsbReg::new(2, 0x12_c064);
const USB3_13004C: UsbReg<Usb3Word::Register> = UsbReg::new(2, 0x13_004c);
const USB3_TUNE: UsbReg<Usb3Tune::Register> = UsbReg::new(2, 0x12_0088);
/// Per SuperSpeed link, 80h apart.
const LINK_CONTROL: UsbReg<LinkControl::Register> = UsbReg::new(3, 0xd024);
pub const GUARD: SmnReg<Guard::Register> = SmnReg::new(0x0381_0a84);
pub const VISIBILITY: [SmnReg<Visibility::Register>; 2] =
    [SmnReg::new(0x1033_4800), SmnReg::new(0x1013_4800)];

/// Semantic tuning coefficients, not a packed vendor structure or pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usb2Tuning(pub [u8; 9]);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usb3Tuning {
    pub word_1242d8: u8,
    pub bit_12c064: bool,
    pub nibble_12c064: u8,
    pub word_13004c: u8,
    pub bit_120088: bool,
    pub mid_120088: u8,
    pub low_bit_120088: bool,
    pub low_120088: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub enabled: [bool; 2],
    pub over_current: [u16; 2],
    pub device_removable: u32,
    pub ras: bool,
    pub over_current_low: bool,
    pub sparse: bool,
    pub debug_pipe_switch: bool,
    /// Two bonded SuperSpeed ports per controller.
    pub force_gen1: u8,
    pub usb2: Option<[[Usb2Tuning; 2]; 2]>,
    pub usb3: Option<[[Usb3Tuning; 2]; 2]>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Policy,
    Unavailable,
    Disabled,
    Consumed,
    Smu(smu::Error),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Step {
    id: u32,
    args: [u32; 6],
    subsequence: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interface {
    Relative,
    Absolute,
}

/// One cold-boot attempt. Every failure consumes it; dropping it does NOT
/// reset the controllers. A new attempt requires a real platform reset.
pub struct ColdBoot {
    steps: Vec<Step, 64>,
    enabled: [bool; 2],
    attempted: bool,
    interface: Interface,
}
impl ColdBoot {
    /// Validate the complete scalar stream before the first hardware access.
    pub fn new(policy: &Policy) -> Result<Self, Error> {
        Self::for_interface(policy, Interface::Relative)
    }
    /// Explicit retained-firmware ABI, selected before any access. Absolute
    /// Genoa does not issue NFX's D0/C0 operations or guess its USB3 PHY ABI.
    pub fn for_interface(policy: &Policy, interface: Interface) -> Result<Self, Error> {
        if interface == Interface::Absolute && policy.usb3.is_some() {
            return Err(Error::Policy);
        }
        if !policy.enabled.iter().any(|&e| e) || policy.device_removable & !0x000f000f != 0 {
            return Err(Error::Policy);
        }
        let mut this = Self {
            steps: Vec::new(),
            enabled: policy.enabled,
            attempted: false,
            interface,
        };
        if interface == Interface::Relative {
            let selector = (Selector::OPERATION.val(0xd0)
                + Selector::DEBUG_PIPE.val(policy.debug_pipe_switch.into()))
            .modify(0);
            this.push(0, 0, 0, selector, false)?;
        } else if policy.debug_pipe_switch {
            return Err(Error::Policy);
        }
        for (c, &map) in policy.over_current.iter().enumerate() {
            this.update(c, OVER_CURRENT, OverCurrent::MAP.val(map.into()))?;
        }
        for c in 0..2 {
            let removable = policy.device_removable >> (2 * c);
            this.update(
                c,
                REMOVABLE,
                Removable::USB2.val(removable & 3) + Removable::USB3.val((removable >> 16) & 3),
            )?;
        }
        for c in 0..2 {
            this.update(c, RAS_CONTROL, RasControl::ENABLE.val(policy.ras.into()))?;
        }
        for c in 0..2 {
            this.update(
                c,
                PORT_CONTROL,
                PortControl::OVER_CURRENT_LOW.val(policy.over_current_low.into()),
            )?;
        }
        for c in 0..2 {
            this.update(c, SPARSE, Sparse::ENABLE.val(policy.sparse.into()))?;
        }
        if let Some(controllers) = policy.usb2 {
            let maxima = [7, 7, 15, 3, 1, 3, 15, 3, 3];
            for (c, ports) in controllers.iter().enumerate() {
                for (p, tune) in ports.iter().enumerate() {
                    let t = tune.0;
                    if t.iter().zip(maxima).any(|(&v, m)| v > m) {
                        return Err(Error::Policy);
                    }
                    this.update(
                        c,
                        USB2_TUNE.at(p, 0x400),
                        Usb2Tune::A.val(t[0].into())
                            + Usb2Tune::B.val(t[1].into())
                            + Usb2Tune::C.val(t[2].into())
                            + Usb2Tune::D.val(t[3].into())
                            + Usb2Tune::E.val(t[4].into())
                            + Usb2Tune::F.val(t[5].into())
                            + Usb2Tune::G.val(t[6].into()),
                    )?;
                    this.update(
                        c,
                        USB2_TUNE2.at(p, 0x400),
                        Usb2Tune2::H.val(t[7].into()) + Usb2Tune2::I.val(t[8].into()),
                    )?;
                }
            }
        }
        if let Some(controllers) = policy.usb3 {
            for (c, ports) in controllers.iter().enumerate() {
                for (p, t) in ports.iter().enumerate() {
                    if t.word_1242d8 > 0x7f
                        || t.nibble_12c064 > 15
                        || t.word_13004c > 0x7f
                        || t.mid_120088 > 7
                        || t.low_120088 > 7
                    {
                        return Err(Error::Policy);
                    }
                    this.update(c, PORT_CONTROL, PortControl::PORT_SELECT.val(p as u32))?;
                    this.update(c, USB3_1242D8, Usb3Word::VALUE.val(t.word_1242d8.into()))?;
                    this.update(
                        c,
                        USB3_12C064,
                        Usb3Nibble::VALUE.val(t.nibble_12c064.into())
                            + Usb3Nibble::BIT.val(t.bit_12c064.into()),
                    )?;
                    this.update(c, USB3_13004C, Usb3Word::VALUE.val(t.word_13004c.into()))?;
                    this.update(
                        c,
                        USB3_TUNE,
                        Usb3Tune::A.val(t.low_120088.into())
                            + Usb3Tune::B.val(t.low_bit_120088.into())
                            + Usb3Tune::C.val(t.mid_120088.into())
                            + Usb3Tune::D.val(t.bit_120088.into()),
                    )?;
                }
            }
        }
        for c in 0..2 {
            this.update(
                c,
                PORT_CONTROL,
                PortControl::FORCE_GEN1.val((u32::from(policy.force_gen1) >> (2 * c)) & 3),
            )?;
        }
        if interface == Interface::Relative {
            for (c, &enabled) in policy.enabled.iter().enumerate() {
                if enabled {
                    this.push(0, 0, 0, Self::selector(c, 2, 0xc0), true)?;
                }
            }
        } else {
            // Genoa FchUsb3PortForceGen1 also updates each physical link.
            for c in 0..2 {
                for port in 0..2 {
                    let force = (policy.force_gen1 >> (2 * c + port)) & 1;
                    this.update(
                        c,
                        LINK_CONTROL.at(port, 0x80),
                        LinkControl::FORCE_GEN1.val(force.into()),
                    )?;
                }
            }
        }
        let mask = u32::from(policy.enabled[0]) | (u32::from(policy.enabled[1]) << 1);
        this.steps
            .push(Step {
                id: INIT_DONE,
                args: [mask, 0, 0, 0, 0, 0],
                subsequence: false,
            })
            .map_err(|_| Error::Policy)?;
        Ok(this)
    }
    fn selector(controller: usize, space: u32, operation: u32) -> u32 {
        (Selector::SPACE.val(space)
            + Selector::CONTROLLER.val(controller as u32)
            + Selector::OPERATION.val(operation))
        .modify(0)
    }
    /// Write `field` of `reg` of controller `c` through the SMU. The policy
    /// is range-checked before: `val` keeps only the field's bits.
    fn update<R: RegisterLongName>(
        &mut self,
        c: usize,
        reg: UsbReg<R>,
        field: FieldValue<u32, R>,
    ) -> Result<(), Error> {
        let register = match self.interface {
            Interface::Relative => reg.offset,
            Interface::Absolute => 0x16c0_0000 + c as u32 * 0x20_0000 + reg.offset,
        };
        self.push(
            register,
            field.mask(),
            field.value,
            Self::selector(c, reg.space, 1),
            false,
        )
    }
    fn push(
        &mut self,
        offset: u32,
        mask: u32,
        value: u32,
        selector: u32,
        subsequence: bool,
    ) -> Result<(), Error> {
        self.steps
            .push(Step {
                id: CONFIG_UPDATE,
                args: [offset, mask, value, selector, 0, 0],
                subsequence,
            })
            .map_err(|_| Error::Policy)
    }
    /// BSP-exclusive, cold boot only. This changes controller configuration,
    /// not storage contents. It does not enumerate devices or prove a link.
    pub fn run(&mut self, smn: &impl SmnAccess, smu: &impl SmuMessages) -> Result<(), Error> {
        if self.attempted {
            return Err(Error::Consumed);
        }
        self.attempted = true;
        let guard = smn.reg(GUARD).extract();
        if self.interface == Interface::Relative {
            if guard.get() == u32::MAX {
                return Err(Error::Unavailable);
            }
            if guard.is_set(Guard::SKIP_CONTROLLERS) {
                return Err(Error::Disabled);
            }
        } else {
            // FchSNXhciIohcPmeDisable's two actual IOHC banks. Preflight
            // before issuing any SMU command; NFX's guard bits are not assumed.
            for bank in [0, 0x20_0000] {
                if smn.reg(crate::nbio::iohc::NB_PCI_CTRL.offset(bank)).get() == u32::MAX {
                    return Err(Error::Unavailable);
                }
            }
        }
        for (c, &enabled) in self.enabled.iter().enumerate() {
            if !enabled {
                smn.reg(VISIBILITY[c]).modify(Visibility::ENABLE::CLEAR);
            }
        }
        let mut skip_subsequence = None;
        for step in &self.steps {
            if step.subsequence {
                if skip_subsequence.is_none() {
                    let value = smn.reg(GUARD).extract();
                    if value.get() == u32::MAX {
                        return Err(Error::Unavailable);
                    }
                    skip_subsequence = Some(value.is_set(Guard::SKIP_SUBSEQUENCE));
                }
                if skip_subsequence == Some(true) {
                    continue;
                }
            }
            let mut args = step.args;
            fstart_log::info!(
                "usb: SMU id={:#x} offset={:#x} selector={:#x}",
                step.id,
                args[0],
                args[3]
            );
            smu.send(step.id, &mut args).map_err(Error::Smu)?;
        }
        if self.interface == Interface::Absolute {
            for bank in [0, 0x20_0000] {
                let reg = smn.reg(crate::nbio::iohc::NB_PCI_CTRL.offset(bank));
                reg.modify(crate::nbio::iohc::NbPciCtrl::USB_PME_DISABLE::SET);
                if !reg.is_set(crate::nbio::iohc::NbPciCtrl::USB_PME_DISABLE) {
                    return Err(Error::Unavailable);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
