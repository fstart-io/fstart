// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native src/soc/intel/skylake/xhci.c, cold PCH-H host mode. LP XRAM/SSIC,
//! retained resume, device mode and a firmware USB/DMA stack are not emulated.
//! xhci.c SHA256 8df910fe467bd94678d36167e212dc243faffecb0d84b539e0060aa0a5541e82.
mod registers;
mod xdci;
pub(crate) use xdci::prepare;
#[cfg(test)]
mod tests;
use super::{
    dmi, hsio,
    pcr::{self, Pcr, Port},
    pmc,
};
use crate::{
    p2sb::{self, Message, Opcode, Sbi},
    southbridge::xhci::{CHANGE_MASK, PORTSC, portsc_value},
};
use embedded_hal::delay::DelayNs;
use fstart_core::mmio::MmioRegister;
use fstart_pci::{EcamDevice, PCI_COMMAND_BITS, PciWindow, PciWindowKind};
use registers::*;
use tock_registers::{
    LocalRegisterCopy, RegisterLongName,
    fields::FieldValue,
    interfaces::{ReadWriteable, Readable, Writeable},
};

#[derive(Debug, Clone, Copy)]
pub struct Usb2Port {
    pub enabled: bool,
    pub overcurrent: Option<u8>,
    pub tx_bias: u8,
    pub emphasis: u8,
    pub pre_bias: u8,
    pub half_bit: bool,
}
impl Usb2Port {
    /// C devicetree's zero-initialized unused array entry, not an invented AFE.
    pub const DISABLED: Self = Self {
        enabled: false,
        overcurrent: None,
        tx_bias: 0,
        emphasis: 0,
        pre_bias: 0,
        half_bit: false,
    };
    pub const fn mid(overcurrent: Option<u8>) -> Self {
        Self {
            enabled: true,
            overcurrent,
            tx_bias: 0,
            emphasis: 2,
            pre_bias: 7,
            half_bit: true,
        }
    }
    pub const fn short(overcurrent: Option<u8>) -> Self {
        Self {
            enabled: true,
            overcurrent,
            tx_bias: 5,
            emphasis: 3,
            pre_bias: 5,
            half_bit: false,
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Usb3Port {
    pub enabled: bool,
    pub overcurrent: Option<u8>,
    pub de_emphasis: u8,
    pub downscale: u8,
}
impl Usb3Port {
    pub const DISABLED: Self = Self {
        enabled: false,
        overcurrent: None,
        de_emphasis: 0,
        downscale: 0,
    };
    pub const fn standard(overcurrent: Option<u8>) -> Self {
        Self {
            enabled: true,
            overcurrent,
            de_emphasis: 0x29,
            downscale: 0,
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub usb2: [Usb2Port; 16],
    pub usb3: [Usb3Port; 10],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Device,
    Resource,
    Locked,
    ActiveHost,
    Protocol,
    Capabilities,
    DebugPortConflict,
    PortDisableTimeout,
    PortResetTimeout,
    HostModeTimeout,
    DeviceD3Timeout,
    PdoLockedResetRequired,
    Readback,
    Pcr(pcr::Error),
    Sbi(p2sb::Error),
    Hsio(hsio::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
impl From<p2sb::Error> for Error {
    fn from(e: p2sb::Error) -> Self {
        Self::Sbi(e)
    }
}
impl From<hsio::Error> for Error {
    fn from(e: hsio::Error) -> Self {
        Self::Hsio(e)
    }
}
#[derive(Debug, Clone, Copy)]
struct Maps {
    usb2_disable: u32,
    usb3_disable: u32,
    usb2_oc: [u32; 8],
    usb3_oc: [u32; 8],
}
impl Config {
    fn maps_for(&self, kind: dmi::Kind) -> Result<Maps, Error> {
        let (usb2_phys, usb2_total, usb3_total) = match kind {
            dmi::Kind::H => (14, 16, 10),
            dmi::Kind::Lp => (10, 12, 6),
        };
        let mut maps = Maps {
            usb2_disable: 0,
            usb3_disable: 0,
            usb2_oc: [0; 8],
            usb3_oc: [0; 8],
        };
        for (index, port) in self.usb2.iter().enumerate() {
            if port.tx_bias > 7
                || port.pre_bias > 7
                || port.emphasis > 3
                || port.overcurrent.is_some_and(|pin| pin >= 8)
                || (index >= usb2_total && port.enabled)
            {
                return Err(Error::Policy);
            }
            if !port.enabled {
                if index < usb2_phys {
                    maps.usb2_disable |= 1 << index;
                }
                continue;
            }
            if let Some(pin) = port.overcurrent {
                maps.usb2_oc[usize::from(pin)] |= 1 << index;
            }
        }
        for (index, port) in self.usb3.iter().enumerate() {
            if port.de_emphasis > 63
                || port.downscale > 63
                || port.overcurrent.is_some_and(|pin| pin >= 8)
                || (index >= usb3_total && port.enabled)
            {
                return Err(Error::Policy);
            }
            if !port.enabled {
                if index < usb3_total {
                    maps.usb3_disable |= 1 << index;
                }
                continue;
            }
            if let Some(pin) = port.overcurrent {
                maps.usb3_oc[usize::from(pin)] |= 1 << index;
            }
        }
        Ok(maps)
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        self.maps_for(dmi::Kind::H).map(|_| ())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceMode {
    Absent,
    Disabled,
}
/// Move-only result of actual pre-probe host-role/xDCI shutdown, not a
/// serialized completion flag or permission to replay a retained boot.
pub(crate) struct Prepared {
    device_mode: DeviceMode,
    _bsp_only: core::marker::PhantomData<*mut ()>,
}
/// Observed configuration only; not connector/overcurrent, negotiated-speed,
/// transfer-ring, USB-device readiness or analog-margin validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub device_mode: DeviceMode,
    pub usb2_disabled: u16,
    pub usb3_disabled: u16,
    pub super_speed_ports: u8,
    pub preserved_debug_port: Option<u8>,
}

const fn usb2_message(offset: u16, opcode: Opcode) -> Message {
    Message {
        port: Port::Usb2 as u8,
        offset,
        opcode,
        posted: matches!(opcode, Opcode::PcrWrite),
        byte_enable: 15,
        bar: 0,
        function: 0,
    }
}
fn afe_modify<R: RegisterLongName>(
    sbi: &mut Sbi<'_>,
    offset: u16,
    fields: FieldValue<u32, R>,
    delay: &mut impl DelayNs,
) -> Result<u32, Error> {
    let mut data = 0;
    sbi.execute(usb2_message(offset, Opcode::PcrRead), &mut data, delay)?;
    let mut value = LocalRegisterCopy::<u32, R>::new(data);
    value.modify(fields);
    data = value.get();
    sbi.execute(usb2_message(offset, Opcode::PcrWrite), &mut data, delay)?;
    Ok(data)
}
/// Write the LP PMC XRAM USB2 AFE replay table so the PMC re-applies the
/// programmed USB2 PHY registers when exiting power-gated states.
unsafe fn write_pmc_xram_usb2(pmc_dev: &EcamDevice, regs: &[u16], vals: &[u32]) {
    const PMC_PM_DATA_BAR: u16 = 0x10;
    const PMC_PM_DATA_BAR_MASK: u32 = 0xffff_c000;
    const PMC_XRAM_BASE: u32 = 0xfe05_0000;
    const HEADER_SIZE: usize = 32;
    const END_MARKER: u32 = 0x0f;

    let old_bar = pmc_dev.read32(PMC_PM_DATA_BAR);
    let old_cmd = pmc_dev.read16(4);
    let masked = old_bar & PMC_PM_DATA_BAR_MASK;
    let temporary = masked == 0 || masked == PMC_PM_DATA_BAR_MASK;
    let base = if temporary {
        pmc_dev.write16(4, old_cmd & !6);
        pmc_dev.write32(PMC_PM_DATA_BAR, PMC_XRAM_BASE);
        pmc_dev.write16(4, (old_cmd & !6) | 2);
        PMC_XRAM_BASE as usize
    } else {
        pmc_dev.write16(4, old_cmd | 2);
        masked as usize
    };

    let mut offset = HEADER_SIZE;
    // Delay command: 7 x 100 us (size = (4/2) - 1 = 1, opcode = 2).
    unsafe {
        MmioRegister::<u8>::new(base + offset).set(2 | (1 << 3));
        MmioRegister::<u8>::new(base + offset + 1).set(2);
        MmioRegister::<u8>::new(base + offset + 2).set(7);
        MmioRegister::<u8>::new(base + offset + 3).set(0);
    }
    offset += 4;

    // Posted SBI write commands (size = (12/2) - 1 = 5, opcode = 0).
    for (&reg, &val) in regs.iter().zip(vals.iter()) {
        unsafe {
            MmioRegister::<u8>::new(base + offset).set(5 << 3);
            MmioRegister::<u8>::new(base + offset + 1).set(Port::Usb2 as u8);
            MmioRegister::<u8>::new(base + offset + 2).set(0);
            MmioRegister::<u8>::new(base + offset + 3).set(0);
            MmioRegister::<u8>::new(base + offset + 4).set(0x0f);
            MmioRegister::<u8>::new(base + offset + 5).set(0);
            MmioRegister::<u16>::new(base + offset + 6).set(reg);
            MmioRegister::<u32>::new(base + offset + 8).set(val);
        }
        offset += 12;
    }

    unsafe {
        MmioRegister::<u32>::new(base + offset).set(END_MARKER);
        // Header word 0: USB2 group (1), row 0, 0 end markers, 0 delays => 0x0100.
        MmioRegister::<u16>::new(base).set(0x0100);
        // Header word 1: end of header table => 0x0f00.
        MmioRegister::<u16>::new(base + 2).set(0x0f00);
    }

    if temporary {
        pmc_dev.write16(4, old_cmd & !6);
        pmc_dev.write32(PMC_PM_DATA_BAR, old_bar);
        pmc_dev.write16(4, old_cmd);
    }
    unsafe { pmc_dev.register::<u32, pmc::GenPmconB::Register>(0xa4) }
        .modify(pmc::GenPmconB::DATA_BAR_DISABLE::SET + pmc::GenPmconB::HOST_RESET::CLEAR);
}

fn afe(
    config: &Config,
    kind: dmi::Kind,
    pmc_dev: &EcamDevice,
    sbi: &mut Sbi<'_>,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    let mut xram_regs = [0u16; 64];
    let mut xram_vals = [0u32; 64];
    let mut xram_count = 0usize;
    let mut record = |reg: u16, val: u32| {
        if xram_count < xram_regs.len() {
            xram_regs[xram_count] = reg;
            xram_vals[xram_count] = val;
            xram_count += 1;
        }
    };

    // These offsets are full-dword SBI addresses, including 402Bh and 7F03h.
    // They MUST NOT become unaligned/byte-addressed PCR MMIO accesses.
    let v = afe_modify(sbi, 0x402b, Global::VBUS::SET, delay)?;
    record(0x402b, v);
    let v = afe_modify(sbi, 0x4001, Global::PORT::SET, delay)?;
    record(0x4001, v);
    for (offset, value) in [
        (0x7034, 0x50000000),
        (0x7038, 0x0a70fcf6),
        (0x703c, 0x1fbbc000),
    ] {
        let mut data = value;
        sbi.execute(usb2_message(offset, Opcode::PcrWrite), &mut data, delay)?;
        record(offset, value);
    }
    let mut pll = 0;
    sbi.execute(usb2_message(0x7f03, Opcode::PcrRead), &mut pll, delay)?;
    if pll & Pll::LOCK::SET.value == 0 {
        let v = afe_modify(sbi, 0x7f03, Pll::FORCE::SET, delay)?;
        record(0x7f03, v);
    }
    let v = afe_modify(sbi, 0x7f04, Comp::BIT9::SET + Comp::BIT10::SET, delay)?;
    record(0x7f04, v);
    let phys_ports = match kind {
        dmi::Kind::H => 14,
        dmi::Kind::Lp => 10,
    };
    for (index, port) in config.usb2[..phys_ports].iter().enumerate() {
        let base = 0x4000 | ((index as u16 + 1) << 8);
        let v = afe_modify(
            sbi,
            base,
            Afe1::TX.val(u32::from(port.tx_bias))
                + Afe1::PRE.val(u32::from(port.pre_bias))
                + Afe1::HALF.val(u32::from(port.half_bit))
                + Afe1::HIGH.val(63),
            delay,
        )?;
        record(base, v);
        let v = afe_modify(
            sbi,
            base | 0x26,
            Afe2::EMP.val(u32::from(port.emphasis)),
            delay,
        )?;
        record(base | 0x26, v);
        let v = afe_modify(
            sbi,
            base | 8,
            Utmi::BIT13::CLEAR
                + Utmi::BIT10::CLEAR
                + Utmi::BIT12::SET
                + Utmi::BIT11::SET
                + Utmi::BIT9::SET
                + Utmi::BIT7::SET,
            delay,
        )?;
        record(base | 8, v);
    }
    if kind == dmi::Kind::Lp && xram_count > 0 {
        // SAFETY: PMC XRAM replay table programming is LP-only and executes
        // exclusively before PMC lockdown.
        unsafe { write_pmc_xram_usb2(pmc_dev, &xram_regs[..xram_count], &xram_vals[..xram_count]) };
    }
    Ok(())
}
fn tune_ss(config: &Config, silicon: hsio::Silicon, pcr: &Pcr) -> Result<(), Error> {
    let max_port = match silicon.kind() {
        dmi::Kind::H => 10,
        dmi::Kind::Lp => 6,
    };
    for (port, policy) in config.usb3[..max_port].iter().enumerate() {
        if !policy.enabled || policy.de_emphasis == 0 && policy.downscale == 0 {
            continue;
        }
        if silicon.lane_owner(pcr, port as u8)? != 1 {
            continue;
        }
        let lane = silicon.lane(port as u8)?;
        if policy.de_emphasis != 0 {
            pcr.register::<Tx::Register>(lane.port, lane.base + 0x94)?
                .modify(Tx::VALUE.val(u32::from(policy.de_emphasis)));
        }
        if policy.downscale != 0 {
            pcr.register::<Tx::Register>(lane.port, lane.base + 0xa0)?
                .modify(Tx::VALUE.val(u32::from(policy.downscale)));
        }
        let calculate = pcr.register::<TxCalc::Register>(lane.port, lane.base + 0xcc)?;
        calculate.modify(TxCalc::INIT::CLEAR);
        calculate.modify(TxCalc::INIT::SET);
    }
    Ok(())
}
struct HostPhase {
    device: Option<EcamDevice>,
    pcr: Pcr,
    kind: dmi::Kind,
    command: u16,
    pll_touched: bool,
}
impl HostPhase {
    fn enter(device: EcamDevice, pcr: Pcr, kind: dmi::Kind) -> Result<Self, Error> {
        let result = Self {
            device: Some(device),
            pcr,
            kind,
            command: device.read16(4),
            pll_touched: false,
        };
        // MSE permits non-writing controller preflight. Do not newly enable
        // bus mastering on an unvalidated/running host.
        unsafe { device.register::<u16, PCI_COMMAND_BITS::Register>(4) }
            .modify(PCI_COMMAND_BITS::MEMORY_SPACE::SET);
        if device.read16(4) & 2 == 0 {
            return Err(Error::Readback);
        }
        Ok(result)
    }
    fn activate(&mut self) -> Result<(), Error> {
        if self.kind == dmi::Kind::H {
            let pll = self.pcr.register::<Mipi::Register>(Port::Modphy0, 0x8088)?;
            self.pll_touched = true;
            pll.modify(Mipi::BIT3::CLEAR + Mipi::BIT5::CLEAR);
        }
        let device = self.device.ok_or(Error::Device)?;
        unsafe { device.register::<u16, PCI_COMMAND_BITS::Register>(4) }
            .modify(PCI_COMMAND_BITS::BUS_MASTER::SET);
        if device.read16(4) & 6 != 6 {
            return Err(Error::Readback);
        }
        Ok(())
    }
    fn restore(&self, device: EcamDevice) -> Result<(), Error> {
        device.write16(4, self.command);
        let command_ok = device.read16(4) == self.command;
        if self.pll_touched {
            let pll = self.pcr.register::<Mipi::Register>(Port::Modphy0, 0x8088)?;
            pll.modify(Mipi::BIT3::SET + Mipi::BIT5::SET);
            if !pll.matches_all(Mipi::BIT3::SET + Mipi::BIT5::SET) {
                return Err(Error::Readback);
            }
        }
        if command_ok {
            Ok(())
        } else {
            Err(Error::Readback)
        }
    }
    fn close(mut self) -> Result<(), Error> {
        let device = self.device.take().ok_or(Error::Device)?;
        self.restore(device)
    }
}
impl Drop for HostPhase {
    fn drop(&mut self) {
        if let Some(device) = self.device.take() {
            let _ = self.restore(device);
        }
    }
}
fn old_stepping(kind: dmi::Kind, id: u16, step: u8) -> bool {
    match kind {
        dmi::Kind::H => {
            step < 0x30
                && matches!(id,0xa141|0xa142|0xa143..=0xa14b|0xa14d|0xa14e|0xa150..=0xa154|0xa156|0xa2c0|0xa2d0)
        }
        dmi::Kind::Lp => step < 0x20 && matches!(id, 0x9d41 | 0x9d43 | 0x9d46 | 0x9d48),
    }
}
fn debug_port(
    base: usize,
    size: u64,
    r: &Registers,
    ss_offset: u8,
    count: u8,
) -> Result<Option<u8>, Error> {
    let mut offset = u64::from(r.hcc.read(Hcc::XECP)) * 4;
    for _ in 0..64 {
        if offset == 0 {
            return Ok(None);
        }
        if offset.checked_add(4).is_none_or(|end| end > size) {
            return Err(Error::Capabilities);
        }
        let cap = unsafe { MmioRegister::<u32>::new(base + offset as usize) }.get();
        if cap & 255 == 10 {
            if offset.checked_add(0x28).is_none_or(|end| end > size) {
                return Err(Error::Capabilities);
            }
            let control = unsafe { MmioRegister::<u32>::new(base + offset as usize + 0x20) }.get();
            if control & 1 == 0 {
                return Ok(None);
            }
            let port = (unsafe { MmioRegister::<u32>::new(base + offset as usize + 0x24) }.get()
                >> 24) as u8;
            return if port >= ss_offset && port < ss_offset + count {
                Ok(Some(port - ss_offset))
            } else {
                Ok(None)
            };
        }
        let next = (cap >> 8) & 255;
        if next == 0 {
            return Ok(None);
        }
        offset = offset
            .checked_add(u64::from(next) * 4)
            .ok_or(Error::Capabilities)?;
    }
    Err(Error::Capabilities)
}
fn pdo(
    r: &Registers,
    kind: dmi::Kind,
    maps: Maps,
    count: u8,
    owned: u16,
    debug: Option<u8>,
    delay: &mut impl DelayNs,
) -> Result<(u16, u16), Error> {
    let (usb2_mask, usb3_mask, max_usb3) = match kind {
        dmi::Kind::H => (0x7fffu32, 0x3ffu32, 10usize),
        dmi::Kind::Lp => (0x3ffu32, 0x3fu32, 6usize),
    };
    let disabled2 = (r.usb2_pdo.get() | maps.usb2_disable) & usb2_mask;
    let mut disabled3 = r.usb3_pdo.get() & usb3_mask;
    if debug.is_some_and(|port| maps.usb3_disable & (1 << port) != 0) {
        return Err(Error::DebugPortConflict);
    }
    for port in 0..max_usb3 {
        if maps.usb3_disable & (1 << port) == 0 || disabled3 & (1 << port) != 0 {
            continue;
        }
        // Only reported, actually USB3-owned FIA lanes have a link to
        // disable. Do not demand a PLS transition from a PCIe/SATA/fused
        // lane's inactive PORTSC. PDO still covers the full port mask.
        if port < usize::from(count) && owned & (1 << port) != 0 {
            let status = &r.ss_port(kind, port).status;
            status.set(portsc_value(status.get(), PORTSC::PED::SET));
            let mut disabled = false;
            for sample in 0..=100 {
                if status.read(PORTSC::PLS) == 4 {
                    disabled = true;
                    break;
                }
                if sample != 100 {
                    delay.delay_us(10);
                }
            }
            if !disabled {
                return Err(Error::PortDisableTimeout);
            }
            status.set(portsc_value(
                status.get(),
                PORTSC::PLS.val(5) + PORTSC::LWS::SET,
            ));
        }
        disabled3 |= 1 << port;
    }
    r.usb2_pdo.set(disabled2);
    r.usb3_pdo.set(disabled3);
    if r.usb2_pdo.get() & usb2_mask != disabled2 || r.usb3_pdo.get() & usb3_mask != disabled3 {
        return Err(Error::PdoLockedResetRequired);
    }
    Ok((disabled2 as u16, disabled3 as u16))
}
fn pci_clock(device: &EcamDevice, clock: u32) {
    unsafe { device.register::<u8, Config1::Register>(0x41) }.modify(Config1::BIT0::SET);
    // XHCC1[23] is RW1C even during the native byte updates.
    unsafe { device.register::<u8, Config2::Register>(0x42) }.modify(
        Config2::BIT2::SET + Config2::BIT4::SET + Config2::BIT5::SET + Config2::STATUS::CLEAR,
    );
    let control = unsafe { device.register::<u32, Word::Register>(0x44) };
    control.set(control.get() | 0x000f_c688);
    device.write32(0x50, clock);
}
fn program_host(
    device: &EcamDevice,
    r: &Registers,
    kind: dmi::Kind,
    old: bool,
    ss_offset: u8,
    count: u8,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    pci_clock(device, 0x0fce6e5f);
    if kind == dmi::Kind::Lp {
        r.aux_ctrl.set(r.aux_ctrl.get() & !(1 << 10));
    }
    unsafe { device.register::<u16, Power::Register>(0x74) }
        .modify(Power::STATE.val(3) + Power::PME_STATUS::CLEAR);
    delay.delay_us(20);
    unsafe { device.register::<u8, PowerByte::Register>(0x74) }.modify(PowerByte::STATE.val(0));
    if device.read8(0x74) & 3 != 0 {
        return Err(Error::Readback);
    }
    pci_clock(device, 0x0fce6e5b);
    unsafe { device.register::<u16, Pce::Register>(0xa2) }
        .modify(Pce::BIT2::CLEAR + Pce::BIT3::CLEAR);
    for (offset, clear, set) in [
        (0xac, 0, 1 << 18),
        (0xa4, 1 << 13, (1 << 12) | (1 << 11)),
        (0xa8, 0, (1 << 17) | (1 << 14)),
    ] {
        let register = unsafe { device.register::<u32, ()>(offset) };
        register.set((register.get() & !clear) | set);
    }
    r.hcs1
        .modify(Hcs1::PORTS.val(u32::from(ss_offset - 1) + u32::from(count)));
    r.hcs3.set((r.hcs3.get() & !0xffff00ff) | 0x0200000a);
    r.usb2_protocol.set(r.usb2_protocol.get() & !(1 << 19));
    // Literal native clock/event recipes, not measured power/analog values.
    for (register, clear, set) in [
        (&r.pmctrl, 1 << 16, 0x4980009c),
        (&r.pgcbctrl, 0, 1 << 24),
        (&r.aux1, (1 << 22) | (1 << 9), 1 << 6),
        (&r.link4, 0, 1 << 25),
        (&r.link1, 1 << 20, 0),
        (&r.aux2, 1 << 21, 1 << 13),
        (&r.aux_clock, 0x3f00, 0x000e401c),
    ] {
        register.set((register.get() & !clear) | set);
    }
    r.if_power0.set(if old { 0xff08003c } else { 0xff00f03c });
    for (register, clear, set) in [
        (&r.usb2pm, 12, (1 << 11) | (1 << 8) | 3),
        (&r.scheduler, 0, (1 << 23) | (1 << 22) | (1 << 6)),
        (&r.idma, 1 << 31, 0),
        (
            &r.misc,
            (1 << 24) | (1 << 18) | (1 << 17) | (1 << 16),
            1 << 23,
        ),
    ] {
        register.set((register.get() & !clear) | set);
    }
    if old {
        r.misc2.set(r.misc2.get() | (1 << 5) | (1 << 2));
    }
    let usb3_mask = if kind == dmi::Kind::H { 0x3ff } else { 0x3f };
    r.ss_enable.set(
        (r.ss_enable.get() & !usb3_mask)
            | SsEnable::BIT30::SET.value
            | (!r.usb3_pdo.get() & usb3_mask),
    );
    for (register, clear, set) in [
        (&r.bw_hs, 0x00ffffff, 0xa000),
        (&r.port_link, 0, (1 << 19) | (1 << 17)),
        (&r.trm2, 1 << 2, (1 << 20) | (1 << 11)),
        (&r.bw_max, 0x00fff000, 0x00753000),
        (&r.if_power1, 0, 1 << 8),
        (&r.usb2_phy, 0, 0xfc),
        (&r.latency_control, 0xfff, 0x01000c0a),
        (&r.latency_high, 0, 0x033200a3),
        (&r.latency_medium, 0, 0x00cb0028),
        (&r.latency_low, 0, 0x0064001e),
        (&r.command1, 1 << 8, 1 << 16),
        (&r.command3, 0, 1 << 25),
        (
            &r.throttle,
            (1 << 11) | (1 << 10) | (1 << 7) | (1 << 1),
            0x0011737d,
        ),
        (&r.throttle2, 0x3ff, 0),
        (&r.d0i2, 0, 1 << 31),
        (&r.debug, 0, 1),
    ] {
        register.set((register.get() & !clear) | set);
    }
    if kind == dmi::Kind::Lp {
        // Mark both LP SSIC ports unused + programming done.
        const SSIC_DISABLED: u32 = (1 << 31) | (1 << 30);
        r.ssic_p1_cfg2.set(r.ssic_p1_cfg2.get() | SSIC_DISABLED);
        r.ssic_p2_cfg2.set(r.ssic_p2_cfg2.get() | SSIC_DISABLED);
    }
    Ok(())
}
fn reset_ports(
    r: &Registers,
    kind: dmi::Kind,
    count: u8,
    debug: Option<u8>,
    resume: bool,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    if !resume {
        for port in 0..count {
            if Some(port) == debug {
                continue;
            }
            let status = &r.ss_port(kind, usize::from(port)).status;
            status.set(portsc_value(status.get(), PORTSC::WPR::SET));
        }
    }
    r.aux1.set(r.aux1.get() & !(1 << 16));
    for sample in 0..=12000 {
        let mut pending = false;
        for port in 0..count {
            if Some(port) == debug {
                continue;
            }
            let status = &r.ss_port(kind, usize::from(port)).status;
            // Unlike a PR-only check, do not report completion while the
            // requested USB3 warm reset itself is still asserted.
            pending |= status.is_set(PORTSC::PR) || status.is_set(PORTSC::WPR);
        }
        if !pending {
            for port in 0..count {
                if Some(port) == debug {
                    continue;
                }
                let status = &r.ss_port(kind, usize::from(port)).status;
                let observed = status.get();
                status.set(portsc_value(
                    observed,
                    FieldValue::<u32, PORTSC::Register>::new(
                        CHANGE_MASK,
                        0,
                        observed & CHANGE_MASK,
                    ),
                ));
            }
            return Ok(());
        }
        if sample != 12000 {
            delay.delay_us(10);
        }
    }
    Err(Error::PortResetTimeout)
}
/// # Safety
/// Actual PCH-H/LP, assigned complete xHCI BAR mapped UC, before final
/// SBI/xHCI/PMC locks. Own P2SB SBI, USB2/USB3 PHY, PMC XRAM and PCI engines
/// exclusively; controller/DMAs are quiescent and no OS/AP/SMI/other USB driver
/// competes. Errors are fatal partial initialization, never a reset/replay
/// permission.
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn initialize(
    prepared: Prepared,
    config: &Config,
    pcr: &Pcr,
    p2sb: &EcamDevice,
    pmc_dev: &EcamDevice,
    window: PciWindow,
    resume: bool,
    delay: &mut impl DelayNs,
) -> Result<Outcome, Error> {
    if EcamDevice::new(0, 0x14, 1).is_present() {
        return Err(Error::Device);
    }
    let lpc = EcamDevice::new(0, 0x1f, 0);
    let silicon = hsio::Silicon::read(&lpc)?;
    let kind = silicon.kind();
    let maps = config.maps_for(kind)?;
    let device = EcamDevice::new(0, 0x14, 0);
    if device.vendor_id() != 0x8086 || device.read32(8) >> 8 != 0x0c0330 {
        return Err(Error::Device);
    }
    let xhcc1 = device.read32(0x40);
    let xhcc2 = device.read32(0x44);
    if xhcc1 & (1 << 31) != 0 || xhcc2 & (1 << 31) != 0 {
        if !resume {
            fstart_log::error!(
                "xHCI: locked before init (XHCC1={} XHCC2={})",
                fstart_log::Hex(u64::from(xhcc1)),
                fstart_log::Hex(u64::from(xhcc2)),
            );
            return Err(Error::Locked);
        }
        // XHCC1.ACCTRL / XHCC2.OCCFDONE sit in the suspend well on Sunrise Point
        // and stay latched across S3 (PLTRST# without RSMRST#). Hardware ignores
        // writes to the already-programmed locked fields while letting the rest
        // of the controller recipe run, matching native coreboot xhci.c.
        fstart_log::info!(
            "xHCI: retained lock across S3 (XHCC1={} XHCC2={})",
            fstart_log::Hex(u64::from(xhcc1)),
            fstart_log::Hex(u64::from(xhcc2)),
        );
    }
    let base = usize::try_from(window.base).map_err(|_| Error::Resource)?;
    if window.kind != PciWindowKind::Mmio
        || fstart_pci::PCI_MEMORY_BAR::PREFETCHABLE.is_set(device.read32(0x10))
        || base == 0
        || base & 0xffff != 0
        || window.size < size_of::<Registers>() as u64
        || window.base.checked_add(window.size).is_none()
        || usize::try_from(window.base + window.size - 1).is_err()
        || device.memory_bar(0x10) != Some(window.base)
    {
        return Err(Error::Resource);
    }
    let p2sb_e0 = p2sb.read32(0xe0);
    if p2sb.vendor_id() != 0x8086 || p2sb_e0 & (1 << 31) != 0 {
        fstart_log::error!(
            "xHCI: P2SB unavailable or SBI locked (vid={} E0={})",
            fstart_log::Hex(u64::from(p2sb.vendor_id())),
            fstart_log::Hex(u64::from(p2sb_e0)),
        );
        return Err(Error::Locked);
    }
    let mut sbi = unsafe { Sbi::new(p2sb) }?;
    let mut phase = HostPhase::enter(device, *pcr, kind)?;
    let result = (|| {
        let r = unsafe { &*(base as *const Registers) };
        if r.caplength.get() != 0x80
            || r.command.is_set(Command::RUN)
            || r.command.is_set(Command::RESET)
            || !r.status.is_set(Status::HALTED)
            || r.status.is_set(Status::NOT_READY)
        {
            return Err(Error::ActiveHost);
        }
        let (ss_offset, max_usb3) = match kind {
            dmi::Kind::H => (17u8, 10u8),
            dmi::Kind::Lp => (13u8, 6u8),
        };
        let count = r.usb3_protocol.read(SsPorts::COUNT) as u8;
        if count == 0
            || count > max_usb3
            || r.usb3_protocol.read(SsPorts::OFFSET) != u32::from(ss_offset)
        {
            return Err(Error::Protocol);
        }
        let debug = debug_port(base, window.size, r, ss_offset, count)?;
        if debug.is_some_and(|port| maps.usb3_disable & (1 << port) != 0) {
            return Err(Error::DebugPortConflict);
        }
        afe(config, kind, pmc_dev, &mut sbi, delay)?;
        tune_ss(config, silicon, pcr)?;
        phase.activate()?;
        let mut owned = 0u16;
        for port in 0..max_usb3 {
            if silicon.lane_owner(pcr, port)? == 1 {
                owned |= 1 << port;
            }
        }
        let (usb2_disabled, usb3_disabled) = pdo(r, kind, maps, count, owned, debug, delay)?;
        program_host(
            &device,
            r,
            kind,
            old_stepping(kind, lpc.device_id(), lpc.read8(8)),
            ss_offset,
            count,
            delay,
        )?;
        // DE can change during the source host recipe. Re-observe the actual
        // debug capability before the warm-port reset pass.
        let debug = debug_port(base, window.size, r, ss_offset, count)?;
        reset_ports(r, kind, count, debug, resume, delay)?;
        for pin in 0..8u16 {
            device.write32(0xb0 + 4 * pin, maps.usb2_oc[usize::from(pin)]);
            device.write32(0xd0 + 4 * pin, maps.usb3_oc[usize::from(pin)]);
        }
        Ok(Outcome {
            device_mode: prepared.device_mode,
            usb2_disabled,
            usb3_disabled,
            super_speed_ports: count,
            preserved_debug_port: debug,
        })
    })();
    phase.close()?;
    result
}
