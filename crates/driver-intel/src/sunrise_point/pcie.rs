// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native cold PCH-H PCIe initialization, before canonical enumeration.
//! Sources skylake/pcie_rp.c SHA256
//! 1921e69440fc265a31f7a005861a1a87febdc7901b49d025230e2a3824a79b90;
//! skylake/pch.c da2f5460aa14d21335852802f08ba2cea163f98bcbf630a8b71cd90c07e736c6.
//! No endpoint driver/DMA engine, PERST# invention, retained replay or
//! synthetic equalization. Lane ownership is observed, never forced.
mod equalize;
mod fabric;
mod policy;
mod port;
mod registers;
use super::{
    hsio,
    pcr::{self, Pcr},
    pmc::{self, PmcRegisters},
};
use embedded_hal::delay::DelayNs;
pub(crate) use fabric::Fabric;
use fstart_pci::EcamDevice;
pub use policy::{Aspm, Config, Equalization, L1Substates, PortPolicy, Speed};
use registers::*;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Device,
    Topology,
    Owned,
    Locked,
    Readback,
    Capability,
    Endpoint,
    LinkTimeout,
    DisableTimeout,
    PmcTimeout,
    ClockDelay,
    NoMargin,
    MarginLinkChanged,
    Pcr(pcr::Error),
    Sbi(crate::p2sb::Error),
    Hsio(hsio::Error),
    Gpio(super::gpio::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
impl From<crate::p2sb::Error> for Error {
    fn from(e: crate::p2sb::Error) -> Self {
        Self::Sbi(e)
    }
}
impl From<hsio::Error> for Error {
    fn from(e: hsio::Error) -> Self {
        Self::Hsio(e)
    }
}
impl From<super::gpio::Error> for Error {
    fn from(e: super::gpio::Error) -> Self {
        Self::Gpio(e)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Disabled,
    Empty,
    Active {
        speed: u8,
        width: u8,
        gen1_fallback: bool,
    },
}
/// Observed root-port configuration/link state, NOT endpoint/media readiness,
/// analog certification, DMA drain or IOMMU protection.
#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    pub physical_ports: u8,
    pub enabled: u32,
    pub unavailable: u32,
    pub empty_disabled: u32,
    pub links: [Link; 24],
}
pub(crate) struct Prepared {
    config: &'static Config,
    pub(crate) silicon: hsio::Silicon,
    _bsp_only: core::marker::PhantomData<*mut ()>,
}
pub(crate) struct Discovered {
    config: &'static Config,
    silicon: hsio::Silicon,
    ports: [Option<HardwarePort>; 24],
    unavailable: u32,
    _bsp_only: core::marker::PhantomData<*mut ()>,
}
impl Discovered {
    /// Do not let global IRQ setup issue ECAM cycles to a disabled or
    /// mux-owned controller. Retained entries keep their ORIGINAL functions.
    pub(crate) fn interrupt_devices(
        &self,
        original: &[u8],
    ) -> Result<heapless::Vec<u8, 64>, Error> {
        let mut result = heapless::Vec::new();
        for devfn in original {
            let rp = (0..24u8).find(|rp| fstart_pci::devfn(fabric::slot(*rp), *rp % 8) == *devfn);
            if rp.is_some_and(|rp| self.ports[usize::from(rp)].is_none()) {
                continue;
            }
            result.push(*devfn).map_err(|_| Error::Policy)?;
        }
        Ok(result)
    }
}
pub(crate) struct Ports {
    count: u8,
    routes: [Option<u8>; 24],
    outcome: Outcome,
    _bsp_only: core::marker::PhantomData<*mut ()>,
}
impl Ports {
    pub(crate) fn outcome(&self) -> Outcome {
        self.outcome
    }
    pub(crate) fn port_number(&self, address: fstart_pci::PciAddress) -> Option<u8> {
        (0..self.count)
            .find(|rp| {
                self.routes[usize::from(*rp)].is_some_and(|function| {
                    address == fstart_pci::PciAddress::new(0, 0, fabric::slot(*rp), function)
                })
            })
            .map(|rp| rp + 1)
    }
}
struct HardwarePort {
    device: EcamDevice,
    r: &'static Registers,
    physical: u8,
    max_width: u8,
}
/// # Safety
/// Actual authenticated cold PCH-H, exclusive ECAM/PCR/PMC/root ports and
/// endpoints; no AP/SMI/DMA/OS owner. All decodes mapped UC. Bus2 is reserved
/// exclusively for bounded pre-enumeration endpoint access. No controller
/// function/PSF/FIA/PMC locks may have been taken. Must precede SATA remap
/// probing, interrupt routing, function swapping and canonical PCI scanning.
pub(crate) unsafe fn prepare(
    config: &'static Config,
    pcr: &Pcr,
    pmc: &PmcRegisters,
    buses: u16,
) -> Result<Prepared, Error> {
    if buses < 3 {
        return Err(Error::Policy);
    }
    let silicon = fabric::restore_defaults(config, pcr, pmc)?;
    Ok(Prepared {
        config,
        silicon,
        _bsp_only: core::marker::PhantomData,
    })
}
/// # Safety
/// Same exclusive cold ownership as prepare, after actual PSF grants. Observe
/// physical ownership over SBI BEFORE any global IRQ ECAM access. Consumes
/// the real default mapping producer; this is not a policy-only ready token.
pub(crate) unsafe fn discover(
    prepared: Prepared,
    fabric: &mut Fabric<'_>,
    delay: &mut impl DelayNs,
) -> Result<Discovered, Error> {
    if prepared.silicon != fabric.silicon
        || hsio::Silicon::read(&EcamDevice::new(0, 0x1f, 0))? != prepared.silicon
    {
        return Err(Error::Topology);
    }
    let config = prepared.config;
    let count = fabric.controllers * 4;
    let mut ports: [Option<HardwarePort>; 24] = core::array::from_fn(|_| None);
    let mut unavailable = 0u32;
    for rp in 0..count {
        let policy = config.ports[usize::from(rp)];
        if !policy.enabled {
            continue;
        }
        if !fabric.standalone(rp) {
            unavailable |= 1 << rp;
            continue;
        }
        let lcap = fabric.read::<Lcap::Register>(rp, 0x4c, delay)?;
        let width = lcap.read(Lcap::WIDTH) as u8;
        if width == 0
            || width > 4
            || !(1..=3).contains(&lcap.read(Lcap::SPEED))
            || lcap.read(Lcap::PORT) != u32::from(rp) + 1
        {
            return Err(Error::Topology);
        }
        let mut owned = true;
        for lane in 0..width {
            owned &= fabric.pcie_owner(rp, lane)?;
        }
        if !owned {
            unavailable |= 1 << rp;
            continue;
        }
        // Only now is direct config access to this enabled controller safe.
        let dev = EcamDevice::new(0, fabric::slot(rp), rp % 8);
        if !dev.is_present() {
            unavailable |= 1 << rp;
            continue;
        }
        if dev.vendor_id() != 0x8086
            || dev.read16(0x0a) != 0x0604
            || dev.find_capability(0x10) != Some(0x40)
            || dev.read32(0x4c) != lcap.get()
        {
            return Err(Error::Device);
        }
        if dev.read16(4) & 7 != 0 || dev.read32(0x18) & 0x00ffffff != 0 {
            return Err(Error::Owned);
        }
        if unsafe { dev.register::<u32, Mpc::Register>(0xd8) }.is_set(Mpc::LOCK) {
            return Err(Error::Locked);
        }
        let r = unsafe { dev.regs::<Registers>() };
        let port = HardwarePort {
            device: dev,
            r,
            physical: rp,
            max_width: width,
        };
        ports[usize::from(rp)] = Some(port);
    }
    Ok(Discovered {
        config,
        silicon: prepared.silicon,
        ports,
        unavailable,
        _bsp_only: core::marker::PhantomData,
    })
}
/// # Safety
/// The discovered bindings remain exclusively owned. Original-function IRQ
/// routing has completed against interrupt_devices(); no canonical PCI scan
/// or competing access has begun. Partial failures cannot be replayed.
pub(crate) unsafe fn initialize(
    discovered: Discovered,
    fabric: &mut Fabric<'_>,
    pmc: &PmcRegisters,
    delay: &mut impl DelayNs,
) -> Result<Ports, Error> {
    if discovered.silicon != fabric.silicon {
        return Err(Error::Topology);
    }
    let config = discovered.config;
    let unavailable = discovered.unavailable;
    let mut ports = discovered.ports;
    let count = fabric.controllers * 4;
    fabric::clocks(config, fabric.pcr, fabric.silicon.is_kbl_h())?;
    let mut enabled = 0u32;
    let mut empty = 0u32;
    let mut pg = 0u32;
    for rp in 0..count {
        if let Some(port) = ports[usize::from(rp)].as_ref() {
            port.r.flags.modify(Flags::SLOT::SET);
            if !config.ports[usize::from(rp)].hotplug && !port.present() {
                empty |= 1 << rp;
                ports[usize::from(rp)] = None;
            } else {
                enabled |= 1 << rp;
            }
        }
        if ports[usize::from(rp)].is_none() {
            pg |= 1 << rp;
        }
    }
    for rp in 0..count {
        if config.ports[usize::from(rp)].clkreq.is_some() {
            pg |= 1 << rp;
        }
    }
    fabric.power_management(pg, delay)?;
    let mut active = false;
    for sample in 0..=10000 {
        active = ports
            .iter()
            .flatten()
            .all(|p| !p.present() || p.r.lsta.is_set(LinkSta::ACTIVE));
        if active {
            break;
        }
        if sample != 10000 {
            delay.delay_us(100);
        }
    }
    if !active {
        return Err(Error::LinkTimeout);
    }
    let mut links = [Link::Disabled; 24];
    for port in ports.iter().flatten() {
        port.r.cfg2.set(port.r.cfg2.get() | (1 << 22)); // CRS replay before endpoint access
        links[usize::from(port.physical)] = port.initialize(config, fabric, delay)?;
    }
    let mut pins = [0u8; 24];
    for port in ports.iter().flatten() {
        // Preserve the original-function IRQ pin, including RP13+ native
        // defaults; do not invent LP-only interrupt-table records for them.
        pins[usize::from(port.physical)] = port.device.read8(fstart_pci::PCI_INTERRUPT_PIN);
        port.lock()?;
    }
    fabric.fix_lanes(pmc, delay)?;
    // No stored ECAM overlay may be used after function mapping changes.
    #[allow(clippy::drop_non_drop)]
    drop(ports);
    let functions = fabric.mapping(enabled, &pins, pmc, delay)?;
    let routes = core::array::from_fn(|rp| {
        if enabled & (1 << rp) != 0 {
            Some(functions[rp])
        } else {
            None
        }
    });
    Ok(Ports {
        count,
        routes,
        outcome: Outcome {
            physical_ports: count,
            enabled,
            unavailable,
            empty_disabled: empty,
            links,
        },
        _bsp_only: core::marker::PhantomData,
    })
}
/// Consume canonical enumerator inventory, not a second endpoint scanner.
pub(crate) fn finish(
    config: &Config,
    ports: &Ports,
    pcr: &Pcr,
    pci: &fstart_pci::PciEcam,
) -> Result<(), Error> {
    port::post_enumeration(config, ports, pcr, pci)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coalescing_swaps_only_first_enabled_function_and_preserves_other_rps() {
        let routes = fabric::function_map((1 << 6) | (1 << 7) | (1 << 16), 20);
        assert_eq!(routes[6], 0);
        assert_eq!(routes[0], 6);
        assert_eq!(routes[7], 7);
        assert_eq!(routes[16], 0);
        let mut seen = 0u16;
        for function in &routes[..8] {
            assert_eq!(seen & (1 << function), 0);
            seen |= 1 << function;
        }
        assert_eq!(seen, 255);
    }
    #[test]
    fn bifurcation_and_fia_mapping_never_authorize_aggregated_sibling_or_lp_lane() {
        assert!(!fabric::standalone(3, 17));
        assert!(fabric::standalone(3, 16));
        assert!(!fabric::standalone(2, 7));
        assert!(fabric::standalone(2, 6));
        assert_eq!(fabric::fia_lane(6), Some(12));
        assert_eq!(fabric::fia_lane(16), Some(26));
        assert_eq!(fabric::fia_lane(23), Some(33));
        assert_eq!(fabric::fia_lane(24), None);
        assert_eq!(fabric::controller_count(false), 5);
        assert_eq!(fabric::controller_count(true), 6);
    }
    #[test]
    fn clock_delay_uses_real_units_and_rejects_unrepresentable_or_reserved_tpower() {
        assert_eq!(port::delay_index(port::tpower(22, 0).unwrap()), Ok(9));
        assert_eq!(port::delay_index(port::tpower(7, 1).unwrap()), Ok(12));
        assert_eq!(port::delay_index(101), Err(Error::ClockDelay));
        assert_eq!(port::tpower(1, 3), Err(Error::Capability));
    }
}
