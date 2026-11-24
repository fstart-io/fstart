// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native skylake/lan.c SHA256
//! 265596c0832c2018c4f52f98755570909e3df63cbae7f298bcf7114c36bd944c.
//! Read-only machine GbE-region observation; no NVM/MAC/PHY reset, descriptor
//! rings, packets or DMA protection are invented. Static gating changes return
//! a GLOBAL reset requirement, never execute a reset or authorize continuation.
use super::{
    hsio,
    pcr::{self, Pcr, Port},
    pmc::{self, PmcRegisters},
    spi,
};
use fstart_core::mmio::MmioReadWrite as RW;
use fstart_pci::{EcamDevice, PCI_COMMAND_BITS, PciWindow, PciWindowKind};
use tock_registers::{
    interfaces::{ReadWriteable, Readable},
    register_bitfields, register_structs,
};
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub enabled: bool,
    pub ltr: bool,
    pub k1_off: bool,
    pub flash_bytes: u32,
}
impl Config {
    pub(crate) fn validate(self, clkreq: Option<u8>) -> Result<(), Error> {
        if !(0x100000..=0x08000000).contains(&self.flash_bytes)
            || !self.flash_bytes.is_power_of_two()
            || clkreq.is_some_and(|n| n >= 16)
            || self.k1_off && clkreq.is_none()
            || !self.enabled && (self.ltr || self.k1_off)
        {
            return Err(Error::Policy);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledReason {
    Policy,
    Fused,
    NoStrap,
    NoRegion,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Device,
    Resource,
    Locked,
    Readback,
    GlobalResetRequired,
    Pcr(pcr::Error),
    Spi(spi::Error),
    Hsio(hsio::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
impl From<spi::Error> for Error {
    fn from(e: spi::Error) -> Self {
        Self::Spi(e)
    }
}
impl From<hsio::Error> for Error {
    fn from(e: hsio::Error) -> Self {
        Self::Hsio(e)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    KeepEnabled,
    KeepDisabled,
    Change { disable: bool },
}
fn gate(desired: bool, fused: bool, disabled: bool, locked: bool) -> Result<Gate, Error> {
    if fused {
        return Ok(Gate::KeepDisabled);
    }
    if desired == !disabled {
        return Ok(if desired {
            Gate::KeepEnabled
        } else {
            Gate::KeepDisabled
        });
    }
    if locked {
        return Err(Error::Locked);
    }
    Ok(Gate::Change { disable: !desired })
}
#[derive(Debug, Clone, Copy)]
enum State {
    Enabled { port: u8, device_id: u16 },
    Disabled(DisabledReason),
}
pub(crate) struct Prepared {
    config: Config,
    state: State,
    _bsp_only: core::marker::PhantomData<*mut ()>,
}
impl Prepared {
    pub(crate) fn enabled(&self) -> bool {
        matches!(self.state, State::Enabled { .. })
    }
}
/// Configuration observation, not link establishment, MAC/NVM validation or
/// a usable network service. K1Off reports the actual CSR bit, not just policy.
#[derive(Debug, Clone, Copy)]
pub enum Outcome {
    Disabled(DisabledReason),
    Configured { pcie_port: u8, k1_off: bool },
}
register_bitfields![u32,
    Strap [
        PORT OFFSET(28) NUMBITS(3) [],
        ENABLE OFFSET(31) NUMBITS(1) []
    ],
    Cpce [ HAE OFFSET(5) NUMBITS(1) [] ],
    Landis [ DISABLE OFFSET(0) NUMBITS(1) [] ],
    LandisLock [ LOCK OFFSET(0) NUMBITS(1) [] ],
    Control [
        MEMORY_ERROR_HANDLING OFFSET(19) NUMBITS(1) [],
        RESET OFFSET(26) NUMBITS(1) []
    ],
    Extension [
        WOL OFFSET(30) NUMBITS(1) [],
        VALID OFFSET(31) NUMBITS(1) []
    ],
    K1 [ ENABLE OFFSET(31) NUMBITS(1) [] ],
    Ecc [
        CORRECTABLE_COUNT OFFSET(0) NUMBITS(8) [],
        UNCORRECTABLE_COUNT OFFSET(8) NUMBITS(8) [],
        ENABLE OFFSET(16) NUMBITS(1) []
    ]
];
register_structs! {
    Registers {
        (0 => control:RW<u32,Control::Register>), (4 => _r0),
        (0x2c => extension:RW<u32,Extension::Register>), (0x30 => _r1),
        (0xf18 => k1:RW<u32,K1::Register>), (0xf1c => _r2),
        (0x100c => ecc:RW<u32,Ecc::Register>), (0x1010 => @END),
    }
}
fn port_number(kind: super::dmi::Kind, strap: u32) -> Option<u8> {
    let value = tock_registers::LocalRegisterCopy::<u32, Strap::Register>::new(strap);
    if !value.is_set(Strap::ENABLE) {
        return None;
    }
    let table: [u8; 5] = match kind {
        super::dmi::Kind::H => [4, 5, 9, 12, 13],
        super::dmi::Kind::Lp => [3, 4, 5, 9, 10],
    };
    table.get(value.read(Strap::PORT) as usize).copied()
}
/// # Safety
/// Actual authenticated cold PCH-H/LP, mapped UC PWRM/PCR/SPI and exclusive GbE
/// host config ownership, no competing AP/SMI/OS or host DMA owner. The supplied
/// physical flash size is the board's real decode, not a substitute identity.
/// Run after root-port mapping, before canonical PCI and static-disable locks.
pub(crate) unsafe fn prepare(
    config: Config,
    clkreq: Option<u8>,
    pcr: &Pcr,
    pmc: &PmcRegisters,
    spi_device: &EcamDevice,
) -> Result<Prepared, Error> {
    config.validate(clkreq)?;
    let kind = hsio::Silicon::read(&EcamDevice::new(0, 0x1f, 0))?.kind();
    if pmc.subsystem_fuse.get() == u32::MAX || pmc.function_disable.get() == u32::MAX {
        return Err(Error::Device);
    }
    let fused = pmc.subsystem_fuse.is_set(pmc::SubsystemFuse::GBE);
    let strap = pcr.register::<Strap::Register>(Port::Fia, 0x200)?.get();
    if strap == u32::MAX {
        return Err(Error::Device);
    }
    let port = port_number(kind, strap);
    let region = if !fused && port.is_some() {
        spi::gbe_region(spi_device, config.flash_bytes)?
    } else {
        None
    };
    let reason = if !config.enabled {
        Some(DisabledReason::Policy)
    } else if fused {
        Some(DisabledReason::Fused)
    } else if port.is_none() {
        Some(DisabledReason::NoStrap)
    } else if region.is_none() {
        Some(DisabledReason::NoRegion)
    } else {
        None
    };
    let action = gate(
        reason.is_none(),
        fused,
        pmc.function_disable.is_set(pmc::FunctionDisable::GBE),
        pmc.function_disable.is_set(pmc::FunctionDisable::LOCK),
    )?;
    let device = EcamDevice::new(0, 0x1f, 6);
    let state = match action {
        Gate::Change { disable } => {
            pmc.function_disable
                .modify(pmc::FunctionDisable::GBE.val(u32::from(disable)));
            if pmc.function_disable.is_set(pmc::FunctionDisable::GBE) != disable {
                return Err(Error::Readback);
            }
            // Do not attempt PCI enable/disable against an unlatched static
            // power state, nor replay/ignore the global reset requirement.
            return Err(Error::GlobalResetRequired);
        }
        Gate::KeepDisabled => {
            if device.is_present() {
                return Err(Error::Readback);
            }
            State::Disabled(reason.unwrap_or(DisabledReason::Fused))
        }
        Gate::KeepEnabled => {
            if device.vendor_id() != 0x8086 || device.read16(0xa) != 0x0200 {
                return Err(Error::Device);
            }
            if unsafe { device.register::<u32, Landis::Register>(0xa0) }.is_set(Landis::DISABLE) {
                return Err(Error::Locked);
            }
            State::Enabled {
                port: port.ok_or(Error::Device)?,
                device_id: device.device_id(),
            }
        }
    };
    Ok(Prepared {
        config,
        state,
        _bsp_only: core::marker::PhantomData,
    })
}
/// # Safety
/// Same exclusive GbE host ownership; `window` is the exact canonical BAR0
/// inventory entry mapped UC. No network driver/host transfer is active. This
/// only performs the native control recipe; it does not start RX/TX or DMA.
pub(crate) unsafe fn finish(
    prepared: Prepared,
    window: Option<PciWindow>,
    wol: bool,
) -> Result<Outcome, Error> {
    let (port, id) = match prepared.state {
        State::Disabled(reason) => return Ok(Outcome::Disabled(reason)),
        State::Enabled { port, device_id } => (port, device_id),
    };
    let device = EcamDevice::new(0, 0x1f, 6);
    if device.vendor_id() != 0x8086 || device.device_id() != id || device.read16(0xa) != 0x0200 {
        return Err(Error::Device);
    }
    if unsafe { device.register::<u32, Landis::Register>(0xa0) }.is_set(Landis::DISABLE) {
        return Err(Error::Locked);
    }
    let window = window.ok_or(Error::Resource)?;
    let base = usize::try_from(window.base).map_err(|_| Error::Resource)?;
    if window.kind != PciWindowKind::Mmio
        || fstart_pci::PCI_MEMORY_BAR::PREFETCHABLE.is_set(device.read32(0x10))
        || base == 0
        || base & 0x1ffff != 0
        || window.size < 0x20000
        || window.base.checked_add(window.size).is_none()
        || usize::try_from(window.base + window.size - 1).is_err()
        || device.memory_bar(0x10) != Some(window.base)
    {
        return Err(Error::Resource);
    }
    unsafe { device.register::<u16, PCI_COMMAND_BITS::Register>(4) }
        .modify(PCI_COMMAND_BITS::MEMORY_SPACE::SET);
    if device.read16(4) & 2 == 0 {
        return Err(Error::Readback);
    }
    let r = unsafe { &*(base as *const Registers) };
    if r.control.is_set(Control::RESET) {
        return Err(Error::Device);
    }
    unsafe { device.register::<u32, Cpce::Register>(0x80) }.modify(Cpce::HAE::SET);
    device.write8(0x94, 0x1f);
    let lock = unsafe { device.register::<u32, LandisLock::Register>(0xa4) };
    lock.modify(LandisLock::LOCK::SET);
    if prepared.config.ltr {
        device.write32(0xa8, 0x10031003);
    }
    r.extension
        .modify(Extension::WOL.val(u32::from(wol)) + Extension::VALID::SET);
    if prepared.config.k1_off {
        r.k1.modify(K1::ENABLE::SET);
    }
    // Native DWORD RMW, also used by Linux e1000e (286003048aae). Low
    // halves are ECC counters, not generic W1C command/acknowledgement bits.
    r.ecc.modify(Ecc::ENABLE::SET);
    r.control.modify(Control::MEMORY_ERROR_HANDLING::SET);
    if !lock.is_set(LandisLock::LOCK)
        || device.read8(0x94) != 0x1f
        || !r
            .extension
            .matches_all(Extension::WOL.val(u32::from(wol)) + Extension::VALID::SET)
        || !r.ecc.is_set(Ecc::ENABLE)
        || !r.control.is_set(Control::MEMORY_ERROR_HANDLING)
        || prepared.config.k1_off && !r.k1.is_set(K1::ENABLE)
    {
        return Err(Error::Readback);
    }
    Ok(Outcome::Configured {
        pcie_port: port,
        k1_off: r.k1.is_set(K1::ENABLE),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn static_gate_mismatch_requires_reset_and_locked_enable_never_succeeds() {
        assert_eq!(
            gate(true, false, true, false),
            Ok(Gate::Change { disable: false })
        );
        assert_eq!(
            gate(false, false, false, false),
            Ok(Gate::Change { disable: true })
        );
        assert_eq!(gate(true, false, true, true), Err(Error::Locked));
        assert_eq!(gate(true, false, false, true), Ok(Gate::KeepEnabled));
        assert_eq!(gate(false, true, false, true), Ok(Gate::KeepDisabled));
    }
    #[test]
    fn gbe_strap_mapping_uses_kind_specific_ports_and_rejects_reserved_selector() {
        use super::super::dmi::Kind;
        assert_eq!(port_number(Kind::H, 0), None);
        assert_eq!(port_number(Kind::H, 1 << 31), Some(4));
        assert_eq!(port_number(Kind::H, (1 << 31) | (4 << 28)), Some(13));
        assert_eq!(port_number(Kind::H, (1 << 31) | (5 << 28)), None);
        assert_eq!(port_number(Kind::Lp, 1 << 31), Some(3));
        assert_eq!(port_number(Kind::Lp, (1 << 31) | (4 << 28)), Some(10));
        assert_eq!(port_number(Kind::Lp, (1 << 31) | (5 << 28)), None);
    }
}
