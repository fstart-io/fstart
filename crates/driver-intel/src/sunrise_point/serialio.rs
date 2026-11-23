// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native skylake/serialio.c SHA256
//! fd9b64ea09f4ba87ee35349c577e998800dbb1e837b7c228819418d7682a2b28.
//! Cold PCH-H Disabled/SkipInit/PCI modes. ACPI/hidden/legacy-UART modes and
//! PCH-LP controllers are not offered. No transfer engine or DMA ring starts.
use super::{
    gpio::{self, Gpio, Pad, PadConfig, PadDw0, PadDw1},
    hsio,
    pcr::{self, Pcr, Port},
    pmc::{self, PmcRegisters},
};
use crate::generic::temporary_bar::{self, TemporaryBar};
use fstart_core::mmio::MmioReadWrite as RW;
use fstart_pci::{
    EcamDevice, PciAddress, PciEcam, PciExcludedFunctions, PciFixedBar, PciFixedBarType,
    PciFixedBars,
};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Disabled,
    SkipInit,
    Pci,
}
#[derive(Debug, Clone, Copy)]
pub struct ControllerPolicy {
    pub mode: Mode,
    pub i2c_1v8: bool,
    pub spi_active_high: bool,
    pub uart_flow: bool,
}
impl ControllerPolicy {
    pub const DISABLED: Self = Self {
        mode: Mode::Disabled,
        i2c_1v8: false,
        spi_active_high: false,
        uart_flow: false,
    };
}
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub controllers: [ControllerPolicy; 11],
}
impl Config {
    pub const DISABLED: Self = Self {
        controllers: [ControllerPolicy::DISABLED; 11],
    };
    pub(crate) fn validate(&self) -> Result<(), Error> {
        for (index, p) in self.controllers.iter().enumerate() {
            if !supported(index) && p.mode != Mode::Disabled
                || p.i2c_1v8 && index >= 4
                || p.spi_active_high && !(6..8).contains(&index)
                || p.uart_flow && index < 8
                || p.mode != Mode::Pci && (p.i2c_1v8 || p.spi_active_high || p.uart_flow)
            {
                return Err(Error::Policy);
            }
        }
        Ok(())
    }
    pub(crate) fn pads(&self) -> Result<heapless::Vec<Pad, 36>, Error> {
        let mut result = heapless::Vec::new();
        for (index, p) in self.controllers.iter().enumerate() {
            if p.mode == Mode::Pci {
                for config in pad_configs(index, *p)? {
                    result.push(config.pad).map_err(|_| Error::Policy)?;
                }
            }
        }
        Ok(result)
    }
    /// Enabled controllers, as native devintconfig sees them: SkipInit is an
    /// enabled PCI device firmware does not initialize, and still gets its
    /// PCICFGCTRL interrupt pin/IRQ like native kabylake_native_irq_init().
    pub(crate) fn interrupt_devfns(&self) -> impl Iterator<Item = u8> + '_ {
        self.controllers
            .iter()
            .enumerate()
            .filter(|(i, p)| supported(*i) && p.mode != Mode::Disabled)
            .map(|(i, _)| DESCRIPTORS[i].devfn)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Device,
    Owned,
    Resource,
    Locked,
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
impl From<temporary_bar::Error> for Error {
    fn from(e: temporary_bar::Error) -> Self {
        match e {
            temporary_bar::Error::Readback => Self::Readback,
            temporary_bar::Error::Owned => Self::Owned,
            _ => Self::Resource,
        }
    }
}
struct Descriptor {
    devfn: u8,
    control: u16,
    psf: u16,
    disable: u32,
}
const DESCRIPTORS: [Descriptor; 11] = [
    Descriptor {
        devfn: 0xa8,
        control: 0x200,
        psf: 0x1100,
        disable: 1,
    },
    Descriptor {
        devfn: 0xa9,
        control: 0x204,
        psf: 0x1000,
        disable: 2,
    },
    Descriptor {
        devfn: 0xaa,
        control: 0x208,
        psf: 0x0f00,
        disable: 4,
    },
    Descriptor {
        devfn: 0xab,
        control: 0x20c,
        psf: 0xe00,
        disable: 8,
    },
    Descriptor {
        devfn: 0xca,
        control: 0x210,
        psf: 0,
        disable: 16,
    },
    Descriptor {
        devfn: 0xc9,
        control: 0x214,
        psf: 0,
        disable: 32,
    },
    Descriptor {
        devfn: 0xf2,
        control: 0x224,
        psf: 0xa00,
        disable: 1 << 9,
    },
    Descriptor {
        devfn: 0xf3,
        control: 0x228,
        psf: 0x900,
        disable: 1 << 10,
    },
    Descriptor {
        devfn: 0xf0,
        control: 0x218,
        psf: 0xc00,
        disable: 1 << 6,
    },
    Descriptor {
        devfn: 0xf1,
        control: 0x21c,
        psf: 0xb00,
        disable: 1 << 7,
    },
    Descriptor {
        devfn: 0xc8,
        control: 0x220,
        psf: 0xd00,
        disable: 1 << 8,
    },
];
const H_MASK: u32 = 0x7cf; // I2C4/5 bits4/5 are NOT implemented on PCH-H.
const fn supported(index: usize) -> bool {
    index < 11 && index != 4 && index != 5
}
fn desired_mask(old: u32, config: &Config) -> u32 {
    let mut value = old;
    for (i, p) in config.controllers.iter().enumerate() {
        if supported(i) {
            match p.mode {
                Mode::Disabled => value |= DESCRIPTORS[i].disable,
                Mode::Pci => value &= !DESCRIPTORS[i].disable,
                Mode::SkipInit => {} // Preserve the inherited static power state too.
            }
        }
    }
    value
}
#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    pub pci: u16,
    pub disabled: u16,
    pub skipped: u16,
    pub absent: u16,
}
pub(crate) struct Prepared {
    config: Config,
    outcome: Outcome,
    fixed: PciFixedBars,
    excluded: PciExcludedFunctions,
    _bsp: core::marker::PhantomData<*mut ()>,
}
impl Prepared {
    pub(crate) fn outcome(&self) -> Outcome {
        self.outcome
    }
    pub(crate) fn fixed(&self) -> &[PciFixedBar] {
        self.fixed.as_slice()
    }
    pub(crate) fn excluded(&self) -> PciExcludedFunctions {
        self.excluded.clone()
    }
}
register_bitfields![u32,
    DevicePower [ ENABLE OFFSET(16) NUMBITS(3) [] ],
    ConfigControl [
        HIDE OFFSET(0) NUMBITS(1) [],
        ACPI_IRQ OFFSET(1) NUMBITS(1) [],
        BAR1_DISABLE OFFSET(7) NUMBITS(1) []
    ],
    Shadow [
        DISABLE OFFSET(8) NUMBITS(1) [],
        BAR23_DISABLE OFFSET(18) NUMBITS(2) []
    ],
    Visibility [ HIDE OFFSET(0) NUMBITS(1) [] ],
    PowerGate [ ENABLE OFFSET(0) NUMBITS(6) [] ],
    ClockGate [
        BIT1 OFFSET(1) NUMBITS(1) [],
        BIT11 OFFSET(11) NUMBITS(1) []
    ],
    Resets [
        FUNCTION OFFSET(0) NUMBITS(1) [],
        APB OFFSET(1) NUMBITS(1) [],
        IDMA OFFSET(2) NUMBITS(1) []
    ],
    Clock [
        ENABLE OFFSET(0) NUMBITS(1) [],
        M OFFSET(1) NUMBITS(15) [],
        N OFFSET(16) NUMBITS(15) [],
        UPDATE OFFSET(31) NUMBITS(1) []
    ],
    SpiConfig [ INVERT OFFSET(16) NUMBITS(1) [] ],
    ChipSelect [
        MODE OFFSET(0) NUMBITS(1) [],
        STATE OFFSET(1) NUMBITS(1) []
    ],
    SdaHold [
        TX OFFSET(0) NUMBITS(16) [],
        RX OFFSET(16) NUMBITS(8) []
    ],
    ByteAddress [ UARTS OFFSET(0) NUMBITS(3) [] ]
];
register_bitfields![u16,PowerState [
    STATE OFFSET(0) NUMBITS(2) [],
    PME_STATUS OFFSET(15) NUMBITS(1) []
]];
register_structs! {
    Registers {
        (0 => _r0), (4 => spi:RW<u32,SpiConfig::Register>), (8 => _r1),
        (0x7c => sda_hold:RW<u32,SdaHold::Register>), (0x80 => _r2), (0x200 => clock:RW<u32,Clock::Register>),
        (0x204 => resets:RW<u32,Resets::Register>), (0x208 => _r3), (0x224 => cs:RW<u32,ChipSelect::Register>), (0x228 => @END),
    }
}
fn pad_configs(index: usize, p: ControllerPolicy) -> Result<heapless::Vec<PadConfig, 4>, Error> {
    const I2C: [[(u16, u32); 2]; 4] = [
        [(64, 1), (65, 1)],
        [(66, 1), (67, 1)],
        [(85, 3), (86, 3)],
        [(76, 2), (95, 2)],
    ];
    const SPI: [[(u16, u32); 4]; 2] = [
        [(39, 1), (40, 1), (41, 1), (42, 1)],
        [(43, 1), (44, 1), (45, 1), (46, 1)],
    ];
    const UART: [[(u16, u32); 4]; 3] = [
        [(56, 1), (57, 1), (58, 1), (59, 1)],
        [(60, 1), (61, 1), (62, 1), (63, 1)],
        [(68, 1), (69, 1), (70, 1), (71, 1)],
    ];
    let pins: &[(u16, u32)] = match index {
        0..=3 => &I2C[index],
        6..=7 => &SPI[index - 6],
        8..=10 => &UART[index - 8][..if p.uart_flow { 4 } else { 2 }],
        _ => return Err(Error::Policy),
    };
    let mut result = heapless::Vec::new();
    for &(pin, function) in pins {
        result
            .push(PadConfig {
                pad: Pad::from_number(pin)?,
                dw0: (PadDw0::RESET.val(1) + PadDw0::MODE.val(function)).value,
                dw1: PadDw1::TOLERANT_1V8.val(u32::from(p.i2c_1v8)).value,
            })
            .map_err(|_| Error::Policy)?;
    }
    Ok(result)
}
fn address(index: usize) -> PciAddress {
    PciAddress::new(
        0,
        0,
        DESCRIPTORS[index].devfn >> 3,
        DESCRIPTORS[index].devfn & 7,
    )
}
fn preserve(
    index: usize,
    device: EcamDevice,
    pcr: &Pcr,
    fixed: &mut PciFixedBars,
    excluded: &mut PciExcludedFunctions,
) -> Result<(), Error> {
    // No device control writes. BAR geometry is the actual LPSS 4-KiB pair;
    // the canonical fixed-BAR machinery reserves live addresses before scan.
    let command = device.read16(4);
    for reg in [0x10, 0x18] {
        let low = device.read32(reg);
        let kind = match low & 7 {
            0 => PciFixedBarType::Memory32,
            4 => PciFixedBarType::Memory64,
            _ => return Err(Error::Resource),
        };
        let Some(base) = device.memory_bar(reg) else {
            let control = pcr
                .register::<ConfigControl::Register>(Port::SerialIo, DESCRIPTORS[index].control)?;
            if control.get() == u32::MAX {
                return Err(Error::Device);
            }
            let bar1_disabled = reg == 0x18 && control.is_set(ConfigControl::BAR1_DISABLE);
            if command & 2 != 0 && !bar1_disabled {
                return Err(Error::Resource);
            }
            continue;
        };
        // Only the native inherited aperture is supported. A custom/hidden
        // mapping requires an authenticated resource handoff, not a guess at
        // its size or a reservation that might alias trained RAM.
        if base != 0xfe020000 + index as u64 * 0x2000 + u64::from(reg == 0x18) * 0x1000 {
            return Err(Error::Resource);
        }
        if base & 0xfff != 0 {
            return Err(Error::Resource);
        }
        fixed
            .push(PciFixedBar {
                address: address(index),
                register: reg,
                kind,
                base,
                size: 0x1000,
                prefetchable: low & 8 != 0,
            })
            .map_err(|_| Error::Resource)?;
    }
    excluded.push(address(index)).map_err(|_| Error::Resource)?;
    Ok(())
}
/// # Safety
/// Exclusive actual cold PCH-H PCR/PMC/PCI/GPIO plus reserved UC native LPSS
/// temporary windows FE020000..FE036000, before enumeration/AP/SMM/OS. PCI
/// mode controllers have no existing decode/DMA owner. SkipInit is inherited
/// ownership: no writes, and ALL its live BARs must stay reserved/unprobed.
pub(crate) unsafe fn prepare(
    config: Config,
    pcr: &Pcr,
    pmc: &PmcRegisters,
) -> Result<Prepared, Error> {
    config.validate()?;
    if hsio::Silicon::read(&EcamDevice::new(0, 0x1f, 0))?.kind() != super::dmi::Kind::H {
        return Err(Error::Device);
    }
    let old = pmc.serial_disable.get();
    if old == u32::MAX {
        return Err(Error::Device);
    }
    let next = desired_mask(old, &config);
    if (old ^ next) & H_MASK != 0 && pmc.function_disable.is_set(pmc::FunctionDisable::LOCK) {
        return Err(Error::Locked);
    }
    let mut result = Prepared {
        config,
        outcome: Outcome {
            pci: 0,
            disabled: 0,
            skipped: 0,
            absent: 0,
        },
        fixed: PciFixedBars::new(),
        excluded: PciExcludedFunctions::new(),
        _bsp: core::marker::PhantomData,
    };
    // Validate ownership for the entire policy before any controller changes.
    for (i, p) in config.controllers.iter().enumerate() {
        if supported(i) {
            if p.mode == Mode::Disabled {
                // Also teach canonical discovery about a disabled function0:
                // independent PCI-mode siblings must remain discoverable.
                result
                    .excluded
                    .push(address(i))
                    .map_err(|_| Error::Resource)?;
            }
            let device = EcamDevice::new_address(address(i));
            if !device.is_present() {
                let d = &DESCRIPTORS[i];
                let control = pcr.register::<ConfigControl::Register>(Port::SerialIo, d.control)?;
                let shadow = pcr.register::<Shadow::Register>(Port::Psf3, d.psf + 0x1c)?;
                let visibility = pcr.register::<Visibility::Register>(Port::Psf3, d.psf + 0x38)?;
                if control.get() == u32::MAX
                    || shadow.get() == u32::MAX
                    || visibility.get() == u32::MAX
                {
                    return Err(Error::Device);
                }
                // Hidden is not absent. We cannot discover/reserve a hidden
                // console's live BARs through unreadable config space.
                if !shadow.is_set(Shadow::DISABLE)
                    && (control.is_set(ConfigControl::HIDE) || visibility.is_set(Visibility::HIDE))
                {
                    return Err(Error::Owned);
                }
                if p.mode == Mode::Pci && old & DESCRIPTORS[i].disable == 0 {
                    return Err(Error::Device);
                }
                result.outcome.absent |= 1 << i;
                continue;
            }
            if device.vendor_id() != 0x8086 || device.read8(0xe) & 0x7f != 0 {
                return Err(Error::Device);
            }
            if p.mode == Mode::SkipInit {
                preserve(i, device, pcr, &mut result.fixed, &mut result.excluded)?;
            } else if device.read16(4) & 7 != 0
                || p.mode == Mode::Pci
                    && unsafe { device.register::<u16, PowerState::Register>(0x84) }
                        .read(PowerState::STATE)
                        != 0
                || device.memory_bar(0x10).is_some_and(|b| b != 0)
                || device.memory_bar(0x18).is_some_and(|b| b != 0)
            {
                return Err(Error::Owned);
            }
        }
    }
    // Leave capacity for the concrete owner's existing P2SB/SPI/SMBus/HECI
    // inventory; never silently omit a preserved SkipInit aperture.
    if result.fixed.len() + 4 > fstart_pci::MAX_PCI_FIXED_BARS {
        return Err(Error::Resource);
    }
    if old & !next & H_MASK != 0 {
        pmc.serial_disable.set(next);
        if (pmc.serial_disable.get() ^ next) & H_MASK != 0 {
            return Err(Error::Readback);
        }
        return Err(Error::GlobalResetRequired);
    }
    // The native shared IP clock/power recipe still applies; SkipInit
    // preserves its function/PCR/pads/static bit, not global fabric policy.
    let clocks = pcr.register::<ClockGate::Register>(Port::SerialIo, 0x604)?;
    let gating = pcr.register::<PowerGate::Register>(Port::SerialIo, 0x1d0)?;
    if clocks.get() == u32::MAX || gating.get() == u32::MAX {
        return Err(Error::Device);
    }
    clocks.modify(ClockGate::BIT1::SET + ClockGate::BIT11::SET);
    gating.modify(PowerGate::ENABLE.val(0x3f));
    if !clocks.matches_all(ClockGate::BIT1::SET + ClockGate::BIT11::SET)
        || gating.read(PowerGate::ENABLE) != 0x3f
    {
        return Err(Error::Readback);
    }
    for (i, p) in config.controllers.iter().enumerate() {
        if supported(i) {
            if p.mode == Mode::SkipInit {
                result.outcome.skipped |= 1 << i;
                continue;
            }
            if result.outcome.absent & (1 << i) != 0 {
                continue;
            }
            let d = &DESCRIPTORS[i];
            let device = EcamDevice::new_address(address(i));
            let power_gate = unsafe { device.register::<u32, DevicePower::Register>(0xa0) };
            if power_gate.get() == u32::MAX {
                return Err(Error::Device);
            }
            power_gate.modify(DevicePower::ENABLE.val(7));
            if power_gate.read(DevicePower::ENABLE) != 7 {
                return Err(Error::Readback);
            }
            if p.mode == Mode::Disabled {
                let power = unsafe { device.register::<u16, PowerState::Register>(0x84) };
                if power.get() == u16::MAX {
                    return Err(Error::Device);
                }
                power.modify(PowerState::STATE.val(3) + PowerState::PME_STATUS::CLEAR);
                if power.read(PowerState::STATE) != 3 {
                    return Err(Error::Readback);
                }
                let shadow = pcr.register::<Shadow::Register>(Port::Psf3, d.psf + 0x1c)?;
                shadow.modify(Shadow::DISABLE::SET);
                if !shadow.is_set(Shadow::DISABLE) || device.is_present() {
                    return Err(Error::Readback);
                }
                result.outcome.disabled |= 1 << i;
                continue;
            }
            let shadow = pcr.register::<Shadow::Register>(Port::Psf3, d.psf + 0x1c)?;
            if shadow.is_set(Shadow::DISABLE) {
                return Err(Error::Locked);
            }
            pcr.register::<Visibility::Register>(Port::Psf3, d.psf + 0x38)?
                .modify(Visibility::HIDE::CLEAR);
            let control = pcr.register::<ConfigControl::Register>(Port::SerialIo, d.control)?;
            control.modify(
                ConfigControl::HIDE::CLEAR
                    + ConfigControl::ACPI_IRQ::CLEAR
                    + ConfigControl::BAR1_DISABLE::SET,
            );
            shadow.modify(Shadow::BAR23_DISABLE.val(3));
            if !control.matches_all(
                ConfigControl::HIDE::CLEAR
                    + ConfigControl::ACPI_IRQ::CLEAR
                    + ConfigControl::BAR1_DISABLE::SET,
            ) || shadow.read(Shadow::BAR23_DISABLE) != 3
                || pcr
                    .register::<Visibility::Register>(Port::Psf3, d.psf + 0x38)?
                    .is_set(Visibility::HIDE)
            {
                return Err(Error::Readback);
            }
            let lease = unsafe {
                TemporaryBar::<Registers>::bind(device, 0x10, 0xfe020000 + i * 0x2000, 0x1000)
            }?;
            let programmed = (|| {
                let r = lease.registers();
                r.resets
                    .modify(Resets::FUNCTION::SET + Resets::APB::SET + Resets::IDMA::SET);
                if i >= 8 {
                    r.clock.write(
                        Clock::UPDATE::SET
                            + Clock::N.val(0xc35)
                            + Clock::M.val(0x30)
                            + Clock::ENABLE::SET,
                    );
                    let byte_address =
                        pcr.register::<ByteAddress::Register>(Port::SerialIo, 0x618)?;
                    let fields =
                        tock_registers::fields::FieldValue::<u32, ByteAddress::Register>::new(
                            1,
                            i - 8,
                            0,
                        );
                    byte_address.modify(fields);
                    if !byte_address.matches_all(fields) {
                        return Err(Error::Readback);
                    }
                    // Divider readback, not a clock-stable/baud-ready ACK.
                    if !r
                        .clock
                        .matches_all(Clock::N.val(0xc35) + Clock::M.val(0x30) + Clock::ENABLE::SET)
                    {
                        return Err(Error::Readback);
                    }
                } else if i < 4 {
                    // Native OR, not an invented exact hold-time calibration.
                    let fields = SdaHold::TX.val(r.sda_hold.read(SdaHold::TX) | 0x2c)
                        + SdaHold::RX.val(r.sda_hold.read(SdaHold::RX) | 0x2c);
                    r.sda_hold.modify(fields);
                    if !r.sda_hold.matches_all(fields) {
                        return Err(Error::Readback);
                    }
                } else {
                    r.spi
                        .modify(SpiConfig::INVERT.val(u32::from(p.spi_active_high)));
                    let fields = ChipSelect::MODE::SET
                        + ChipSelect::STATE.val(u32::from(!p.spi_active_high));
                    r.cs.modify(fields);
                    if !r.cs.matches_all(fields)
                        || !r
                            .spi
                            .matches_all(SpiConfig::INVERT.val(u32::from(p.spi_active_high)))
                    {
                        return Err(Error::Readback);
                    }
                }
                let pads = pad_configs(i, *p)?;
                Gpio {
                    pcr,
                    program_itss_polarity: false,
                }
                .configure_pads(pads.as_slice())?;
                if !r
                    .resets
                    .matches_all(Resets::FUNCTION::SET + Resets::APB::SET + Resets::IDMA::SET)
                {
                    return Err(Error::Readback);
                }
                Ok(())
            })();
            lease.close()?;
            programmed?;
            result.outcome.pci |= 1 << i;
        }
    }
    pmc.serial_disable.set(next);
    if (pmc.serial_disable.get() ^ next) & H_MASK != 0 {
        return Err(Error::Readback);
    }
    Ok(result)
}
pub(crate) fn finish(prepared: &Prepared, pci: &PciEcam) -> Result<(), Error> {
    for (i, p) in prepared.config.controllers.iter().enumerate() {
        if !supported(i) {
            continue;
        }
        if p.mode == Mode::SkipInit {
            if pci.devices().any(|a| a == address(i)) {
                return Err(Error::Owned);
            }
            continue;
        }
        if prepared.outcome.pci & (1 << i) != 0 {
            let device = pci.device(address(i)).ok_or(Error::Device)?;
            let bar = pci
                .assigned_memory_bar(address(i), 0x10)
                .ok_or(Error::Resource)?;
            if !pci.devices().any(|a| a == address(i))
                || device.vendor_id() != 0x8086
                || bar.size < 0x1000
                || bar.base & 0xfff != 0
                || fstart_pci::PCI_MEMORY_BAR::PREFETCHABLE.is_set(device.read32(0x10))
                || device.memory_bar(0x10) != Some(bar.base)
                || pci.assigned_memory_bar(address(i), 0x18).is_some()
            {
                return Err(Error::Resource);
            }
            let command = device.read16(4);
            if command & 5 != 0 {
                return Err(Error::Owned);
            }
            device.write16(4, command | 2); // MSE only; no DMA owner/rings exist.
            if device.read16(4) & 7 != 2 {
                return Err(Error::Readback);
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skip_init_preserves_static_state_and_h_policy_never_changes_lp_only_bits() {
        let mut config = Config::DISABLED;
        config.controllers[10].mode = Mode::SkipInit;
        assert_eq!(desired_mask(0, &config), 0x6cf);
        assert_eq!(desired_mask(0x130, &config), 0x7ff);
        config.controllers[0].mode = Mode::Pci;
        assert_eq!(desired_mask(0x130, &config), 0x7fe);
    }
    #[test]
    fn unsupported_h_i2c_and_cross_kind_pad_settings_reject_before_hardware() {
        let mut config = Config::DISABLED;
        config.controllers[4].mode = Mode::Pci;
        assert_eq!(config.validate(), Err(Error::Policy));
        config = Config::DISABLED;
        config.controllers[8].i2c_1v8 = true;
        assert_eq!(config.validate(), Err(Error::Policy));
    }
}
