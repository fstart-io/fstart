//! South and north cluster PCI functions: the per-device init coreboot
//! runs from its device drivers, and the function enable/disable common
//! to the south cluster.

pub mod gfx;
pub mod hda;
pub mod lpe;
pub mod lpss;
pub mod modphy;
pub mod pcie;
pub mod perf_power;
pub mod sata;
pub mod scc;
pub mod usb;

use super::iosf::{Iosf, IosfPort};
use super::pmc::{self, FUNC_DIS, FUNC_DIS2};
use super::{TEMP_BASE, dev};
use crate::MmioBar;
use fstart_core::services::ServiceError;
use fstart_pci::ConfigRegionAccess;
use fstart_pci::ecam::EcamDevice;
use tock_registers::fields::FieldValue;
use tock_registers::interfaces::{ReadWriteable, Readable};

/// A memory BAR as enumeration assigned it.
#[derive(Debug, Clone, Copy)]
pub struct Bar(pub usize);

impl MmioBar for Bar {
    fn base(self) -> usize {
        self.0
    }
}

impl Bar {
    /// BAR0 of `device`, when enumeration assigned it.
    pub fn of(device: &EcamDevice) -> Option<Self> {
        device.memory_bar(0x10).map(|base| Self(base as usize))
    }
}

/// Wait up to `timeout_us` for `bar[offset] & mask == value`.
pub fn poll_bar(
    bar: Bar,
    offset: u32,
    mask: u32,
    value: u32,
    timeout_us: u32,
) -> Result<(), ServiceError> {
    if fstart_arch::x86::wait_us(timeout_us, || bar.read32(offset) & mask == value) {
        Ok(())
    } else {
        fstart_log::error!("baytrail: register {:#x} poll timeout", offset);
        Err(ServiceError::Timeout)
    }
}

/// A function firmware switched to ACPI mode: hidden from PCI and
/// described in the DSDT at the BARs enumeration gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiModeDevice {
    pub function: Function,
    pub bar0: u32,
    pub bar1: u32,
    /// Function-specific: the LPE firmware window.
    pub extra: u32,
}

impl AcpiModeDevice {
    /// Record the BARs before the function disappears from PCI.
    pub fn capture(function: Function, device: &EcamDevice) -> Self {
        let bar = |reg| device.memory_bar(reg).map_or(0, |base| base as u32);
        Self {
            function,
            bar0: bar(0x10),
            bar1: bar(0x14),
            extra: 0,
        }
    }
}

/// A sideband read-modify-write: `reg = (reg & keep) | set`, or a plain
/// write when nothing is kept.
#[derive(Debug, Clone, Copy)]
pub struct Rmw {
    port: IosfPort,
    reg: u32,
    keep: u32,
    set: u32,
}

impl Rmw {
    pub const fn new(port: IosfPort, reg: u32, keep: u32, set: u32) -> Self {
        Self {
            port,
            reg,
            keep,
            set,
        }
    }

    /// Set bits `msb..=lsb` to `value`.
    pub const fn field(port: IosfPort, reg: u32, msb: u32, lsb: u32, value: u32) -> Self {
        let mask = ((1u64 << (msb + 1)) - 1) & !((1u64 << lsb) - 1);
        Self::new(
            port,
            reg,
            !(mask as u32),
            ((value as u64) << lsb) as u32 & mask as u32,
        )
    }

    pub fn apply<P: ConfigRegionAccess>(&self, iosf: &Iosf<P>) {
        if self.keep == 0 {
            iosf.write(self.port, self.reg, self.set);
        } else {
            iosf.write(
                self.port,
                self.reg,
                (iosf.read(self.port, self.reg) & self.keep) | self.set,
            );
        }
    }
}

/// The south cluster's PCI functions (devices above graphics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    Mmc,
    Sdio,
    Sd,
    Sata,
    Xhci,
    Lpe,
    Mmc45,
    SioDma1,
    /// I2C controller 1-7.
    I2c(u8),
    Txe,
    Hda,
    /// PCIe root port 1-4.
    PciePort(u8),
    Ehci,
    SioDma2,
    Pwm1,
    Pwm2,
    Hsuart1,
    Hsuart2,
    Spi,
    Smbus,
}

impl Function {
    /// Every function, in device order.
    pub const ALL: [Function; 29] = [
        Function::Mmc,
        Function::Sdio,
        Function::Sd,
        Function::Sata,
        Function::Xhci,
        Function::Lpe,
        Function::Mmc45,
        Function::SioDma1,
        Function::I2c(1),
        Function::I2c(2),
        Function::I2c(3),
        Function::I2c(4),
        Function::I2c(5),
        Function::I2c(6),
        Function::I2c(7),
        Function::Txe,
        Function::Hda,
        Function::PciePort(1),
        Function::PciePort(2),
        Function::PciePort(3),
        Function::PciePort(4),
        Function::Ehci,
        Function::SioDma2,
        Function::Pwm1,
        Function::Pwm2,
        Function::Hsuart1,
        Function::Hsuart2,
        Function::Spi,
        Function::Smbus,
    ];

    /// Device and function number on bus 0.
    pub const fn devfn(self) -> (u8, u8) {
        match self {
            Function::Mmc => (dev::MMC, 0),
            Function::Sdio => (dev::SDIO, 0),
            Function::Sd => (dev::SD, 0),
            Function::Sata => (dev::SATA, 0),
            Function::Xhci => (dev::XHCI, 0),
            Function::Lpe => (dev::LPE, 0),
            Function::Mmc45 => (dev::MMC45, 0),
            Function::SioDma1 => (dev::SIO1, 0),
            Function::I2c(n) => (dev::SIO1, n),
            Function::Txe => (dev::TXE, 0),
            Function::Hda => (dev::HDA, 0),
            Function::PciePort(n) => (dev::PCIE, n - 1),
            Function::Ehci => (dev::EHCI, 0),
            Function::SioDma2 => (dev::SIO2, 0),
            Function::Pwm1 => (dev::SIO2, 1),
            Function::Pwm2 => (dev::SIO2, 2),
            Function::Hsuart1 => (dev::SIO2, 3),
            Function::Hsuart2 => (dev::SIO2, 4),
            Function::Spi => (dev::SIO2, 5),
            Function::Smbus => (dev::PCU, 3),
        }
    }

    /// A bit unique to this function, for function sets: its index in
    /// [`Self::ALL`].
    pub const fn bit(self) -> u32 {
        let index = match self {
            Function::Mmc => 0,
            Function::Sdio => 1,
            Function::Sd => 2,
            Function::Sata => 3,
            Function::Xhci => 4,
            Function::Lpe => 5,
            Function::Mmc45 => 6,
            Function::SioDma1 => 7,
            Function::I2c(n) => 7 + n as u32,
            Function::Txe => 15,
            Function::Hda => 16,
            Function::PciePort(n) => 16 + n as u32,
            Function::Ehci => 21,
            Function::SioDma2 => 22,
            Function::Pwm1 => 23,
            Function::Pwm2 => 24,
            Function::Hsuart1 => 25,
            Function::Hsuart2 => 26,
            Function::Spi => 27,
            Function::Smbus => 28,
        };
        1 << index
    }

    pub fn device(self) -> EcamDevice {
        let (device, function) = self.devfn();
        EcamDevice::new(0, device, function)
    }

    /// The PMC function disable bits that hide this function.
    fn disable_bits(
        self,
    ) -> (
        FieldValue<u32, FUNC_DIS::Register>,
        FieldValue<u32, FUNC_DIS2::Register>,
    ) {
        let none1 = FieldValue::<u32, FUNC_DIS::Register>::new(0, 0, 0);
        let none2 = FieldValue::<u32, FUNC_DIS2::Register>::new(0, 0, 0);
        let one = |field: FieldValue<u32, FUNC_DIS::Register>| (field, none2);
        match self {
            Function::Mmc => one(FUNC_DIS::MMC::SET),
            Function::Sdio => one(FUNC_DIS::SDIO::SET),
            Function::Sd => one(FUNC_DIS::SD::SET),
            Function::Sata => one(FUNC_DIS::SATA::SET),
            // The SuperSpeed PHY goes with the xHCI.
            Function::Xhci => (FUNC_DIS::XHCI::SET, FUNC_DIS2::USH_SS_PHY::SET),
            Function::Lpe => one(FUNC_DIS::LPE::SET),
            Function::Mmc45 => one(FUNC_DIS::MMC45::SET),
            Function::SioDma1 => one(FUNC_DIS::SIO_DMA1::SET),
            Function::I2c(1) => one(FUNC_DIS::I2C1::SET),
            Function::I2c(2) => one(FUNC_DIS::I2C2::SET),
            Function::I2c(3) => one(FUNC_DIS::I2C3::SET),
            Function::I2c(4) => one(FUNC_DIS::I2C4::SET),
            Function::I2c(5) => one(FUNC_DIS::I2C5::SET),
            Function::I2c(6) => one(FUNC_DIS::I2C6::SET),
            Function::I2c(7) => one(FUNC_DIS::I2C7::SET),
            Function::I2c(_) => (none1, none2),
            Function::Txe => one(FUNC_DIS::TXE::SET),
            Function::Hda => one(FUNC_DIS::HDA::SET),
            Function::PciePort(1) => one(FUNC_DIS::PCIE_PORT1::SET),
            Function::PciePort(2) => one(FUNC_DIS::PCIE_PORT2::SET),
            Function::PciePort(3) => one(FUNC_DIS::PCIE_PORT3::SET),
            Function::PciePort(4) => one(FUNC_DIS::PCIE_PORT4::SET),
            Function::PciePort(_) => (none1, none2),
            Function::Ehci => one(FUNC_DIS::EHCI::SET),
            Function::SioDma2 => one(FUNC_DIS::SIO_DMA2::SET),
            Function::Pwm1 => one(FUNC_DIS::PWM1::SET),
            Function::Pwm2 => one(FUNC_DIS::PWM2::SET),
            Function::Hsuart1 => one(FUNC_DIS::HSUART1::SET),
            Function::Hsuart2 => one(FUNC_DIS::HSUART2::SET),
            Function::Spi => one(FUNC_DIS::SPI::SET),
            Function::Smbus => (none1, FUNC_DIS2::SMBUS::SET),
        }
    }

    /// The power-management capability where the capability list does not
    /// point at it (coreboot `place_device_in_d3hot`).
    const fn pm_cap_fallback(self) -> u16 {
        match self {
            Function::Sata | Function::Xhci | Function::Ehci => 0x70,
            Function::Hda | Function::Smbus => 0x50,
            Function::PciePort(_) => 0xa0,
            _ => 0x80,
        }
    }
}

/// A set of south cluster functions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Functions(u32);

impl Functions {
    pub const NONE: Self = Self(0);

    pub const fn with(self, function: Function) -> Self {
        Self(self.0 | function.bit())
    }

    pub const fn contains(self, function: Function) -> bool {
        self.0 & function.bit() != 0
    }

    /// Whether this set hides a function 0 (SIO DMA 1 at 18.0, SIO DMA 2
    /// at 1e.0, PCIe port 1 at 1c.0) whose device keeps another function
    /// enabled: PCI enumeration stops at a missing function 0, so those
    /// siblings would get no resources.
    pub const fn hides_function_zero_of_enabled(self) -> bool {
        let mut n = 1;
        while n <= 7 {
            if self.contains(Function::SioDma1) && !self.contains(Function::I2c(n)) {
                return true;
            }
            n += 1;
        }
        let mut port = 2;
        while port <= 4 {
            if self.contains(Function::PciePort(1)) && !self.contains(Function::PciePort(port)) {
                return true;
            }
            port += 1;
        }
        let sio2 = [
            Function::Pwm1,
            Function::Pwm2,
            Function::Hsuart1,
            Function::Hsuart2,
            Function::Spi,
        ];
        let mut i = 0;
        while i < sio2.len() {
            if self.contains(Function::SioDma2) && !self.contains(sio2[i]) {
                return true;
            }
            i += 1;
        }
        false
    }
}

/// Put `function` in D3hot. The TXE cannot enter D3hot and the HDA powers
/// part of LPE audio, so it is only brought out of reset (coreboot
/// `place_device_in_d3hot` and `hda_work_around`).
fn place_in_d3hot(function: Function, device: &EcamDevice) {
    if function == Function::Txe {
        return;
    }
    if function == Function::Hda {
        // Magic register, then GCTL.CRST through a temporary BAR0.
        device.write8(0x43, 0xd7);
        device.write32(0x10, TEMP_BASE);
        device.write32(0x14, 0);
        device.write16(0x04, 0x2);
        Bar(TEMP_BASE as usize).setbits32(0x8, 1);
        device.write16(0x04, 0);
        device.write32(0x10, 0);
        return;
    }
    use fstart_pci::capability::PciCapability;
    use fstart_pci::ecam::GlobalEcam;
    use fstart_pci::{EndpointHeader, PciHeader};
    let offset = EndpointHeader::from_header(PciHeader::new(device.address()), GlobalEcam)
        .and_then(|header| {
            header
                .capabilities(GlobalEcam)
                .find_map(|capability| match capability {
                    PciCapability::PowerManagement(address) => Some(address.offset),
                    _ => None,
                })
        })
        .unwrap_or(function.pm_cap_fallback());
    fstart_log::debug!("baytrail: PM capability at {:#x}", offset);
    let pmcsr = offset + 4;
    device.write8(pmcsr, device.read8(pmcsr) | 0x3);
}

/// Hide a disabled function: decode off, D3hot, then the PMC function
/// disable (coreboot `southcluster_enable_dev` for a disabled device). An
/// enabled function gets SERR reporting.
pub fn set_enabled(function: Function, enabled: bool) {
    let device = function.device();
    if !device.is_present() {
        return;
    }
    if enabled {
        device.or16(0x04, 1 << 8);
        return;
    }
    let (devnum, func) = function.devfn();
    fstart_log::debug!("baytrail: disabling {:02x}.{}", devnum, func);
    device.and16(0x04, !0x7);
    place_in_d3hot(function, &device);
    let (dis, dis2) = function.disable_bits();
    let regs = pmc::regs();
    if dis.mask() != 0 {
        regs.func_dis.modify(dis);
        // Flush the posted write.
        let _ = regs.func_dis.get();
    }
    if dis2.mask() != 0 {
        regs.func_dis2.modify(dis2);
        let _ = regs.func_dis2.get();
    }
}
