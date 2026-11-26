// SPDX-License-Identifier: MIT OR Apache-2.0
//! Firmware-first PCIe and CXL error handling, the part every SoC with an
//! SMI for its root ports' errors shares (FIRMWARE_FIRST.md §2.2, from the
//! AMD CPM's `AmdPcieLegacyRasSmiCallback` and `PcieErrorLogV1`): what a
//! root port and the functions below it hold of an error becomes a CPER
//! record in a GHES's error status block, the OS is told, the error state
//! is cleared and the board's BMC gets an event.
//!
//! The configuration ([`FirmwareFirst`]: two GHESs, one for corrected and
//! one for uncorrected errors, each with its notification) is the SoC's,
//! passed to its SMM handler; the HEST describes the same blocks
//! (`acpi::apei`). The handler runs without a heap and without panics: the
//! records are encoded into fixed buffers and appended in place
//! ([`fstart_acpi::cper::append`]).
//!
//! Per root port with an error ([`root_port`]): the functions its Root
//! Error Status names (the Error Source Identification of the first
//! ERR_COR and ERR_FATAL/NONFATAL), the port itself, and, when those
//! leave the error unexplained or several were received, the functions on
//! its secondary bus. Each function with an unmasked error gets a PCI
//! Express error section (UEFI N.2.7: device ID, the PCI Express and AER
//! capabilities, what Linux hands to its AER recovery), a CXL port or
//! device with a pending CXL RAS capability a CXL protocol error section
//! too (N.2.13), its severity from its own status (an unmasked
//! uncorrectable error fatal by its severity bit, else recoverable;
//! correctable otherwise; a root port relaying a message it cannot place,
//! from its Root Error Status as the vendor does). Then its AER, Device
//! Status and CXL RAS status are cleared, the root port's last.

use fstart_acpi::cper::{self, Severity};
use fstart_pci::aer::{Errors, Function, RootErrorStatus, Setup, Snapshot};
use fstart_pci::cxl::{self, Registers};
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::LocalRegisterCopy;

/// HEST notification of a GHES ([`Ghes::notify`]).
pub mod notify {
    /// The OS polls the block every [`super::POLL_MS`].
    pub const POLLED: u8 = 0;
    /// An SCI to the Hardware Error Device.
    pub const SCI: u8 = 3;
    /// An NMI.
    pub const NMI: u8 = 4;
}

/// The OS's polling interval of a polled GHES: the vendors'
/// (`PcdCpmPciePollInterval` 5000 ms).
pub const POLL_MS: u32 = 5000;

/// The AER setup of root ports and of the functions below them on a
/// firmware-first platform: the AMD CPM's PCD defaults (GIGABYTE
/// AmdPlatformRasWhDxe `PlatformPcieAerEnablement 0xf2d8`;
/// `PcdCpmPcieRp/DevCorrectedErrorMask` 0x2000 (advisory non-fatal),
/// `PcdCpmPcieRpUnCorrectedErrorMask` 0, `PcdCpmPcieDevUnCorrectedErrorMask`
/// 0x10_0000 (unsupported request), `...UnCorrectedErrorSeverity`
/// 0x57EF_6030; FIRMWARE_FIRST.md §2.3). A root port's Root Error Command
/// is 0: its messages reach the firmware through the IOHC, not its own
/// interrupt (the vendor's HEST for mechanism 2).
pub const ROOT_PORT: Setup = Setup {
    correctable_mask: 0x2000,
    uncorrectable_mask: 0,
    uncorrectable_severity: 0x57ef_6030,
    root_command: 0,
};
pub const DEVICE: Setup = Setup {
    correctable_mask: 0x2000,
    uncorrectable_mask: 0x10_0000,
    uncorrectable_severity: 0x57ef_6030,
    root_command: 0,
};

/// One GHES's memory: the qword the HEST's error status address names
/// (it holds the block's address) and the error status block.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ghes {
    pub status_address: u64,
    pub block: u64,
    pub length: u32,
    /// [`notify`].
    pub notify: u8,
    pub _reserved: [u8; 3],
}

impl Ghes {
    /// The HEST's notification of this source.
    pub fn hest_notify(&self) -> fstart_acpi::hest::Notify {
        use fstart_acpi::hest::Notify;
        match self.notify {
            notify::SCI => Notify::Sci,
            notify::NMI => Notify::Nmi,
            _ => Notify::Polled { ms: POLL_MS },
        }
    }
}

/// The firmware-first error sources of a SoC: the GHES of corrected
/// errors and that of uncorrected ones, and the SCI event (GPE) of a
/// source notified by SCI. All zero: none (OS-first).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FirmwareFirst {
    pub corrected: Ghes,
    pub uncorrected: Ghes,
    pub sci_event: u8,
    pub _reserved: [u8; 7],
}

impl FirmwareFirst {
    pub const NONE: Self = Self {
        corrected: Ghes {
            status_address: 0,
            block: 0,
            length: 0,
            notify: 0,
            _reserved: [0; 3],
        },
        uncorrected: Ghes {
            status_address: 0,
            block: 0,
            length: 0,
            notify: 0,
            _reserved: [0; 3],
        },
        sci_event: 0,
        _reserved: [0; 7],
    };

    pub const fn enabled(&self) -> bool {
        self.corrected.block != 0 && self.uncorrected.block != 0
    }

    /// Whether a source is notified by SCI (the DSDT then has the
    /// Hardware Error Device and the GPE's method).
    pub const fn sci(&self) -> bool {
        self.corrected.notify == notify::SCI || self.uncorrected.notify == notify::SCI
    }
}

/// The two error status blocks one pass appends to.
pub struct Blocks<'a> {
    pub corrected: &'a mut [u8],
    pub uncorrected: &'a mut [u8],
}

/// What one pass logged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Logged {
    /// Records in the corrected and the uncorrected block.
    pub corrected: u16,
    pub uncorrected: u16,
    /// Records a full block left out.
    pub dropped: u16,
}

impl Logged {
    pub fn add(&mut self, other: Logged) {
        self.corrected = self.corrected.saturating_add(other.corrected);
        self.uncorrected = self.uncorrected.saturating_add(other.uncorrected);
        self.dropped = self.dropped.saturating_add(other.dropped);
    }

    /// The notifications `ff` asks for after this pass: an SCI, an NMI.
    pub fn signals(&self, ff: &FirmwareFirst) -> (bool, bool) {
        let mut sci = false;
        let mut nmi = false;
        for (records, ghes) in [
            (self.corrected, &ff.corrected),
            (self.uncorrected, &ff.uncorrected),
        ] {
            if records != 0 {
                sci |= ghes.notify == notify::SCI;
                nmi |= ghes.notify == notify::NMI;
            }
        }
        (sci, nmi)
    }
}

/// A PCIe or CXL error the board's BMC hears of
/// (`smm::ras::Event::Pcie`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcieEvent {
    pub segment: u16,
    pub bus: u8,
    pub devfn: u8,
    pub severity: Severity,
}

/// The severity of what `snapshot` holds, `None` without an unmasked
/// error.
pub fn severity(snapshot: &Snapshot) -> Option<Severity> {
    if snapshot.fatal() != 0 {
        Some(Severity::Fatal)
    } else if snapshot.uncorrectable() != 0 {
        Some(Severity::Recoverable)
    } else if snapshot.correctable() != 0 {
        Some(Severity::Corrected)
    } else {
        None
    }
}

/// The severity of a message a root port received and could not place
/// (the vendor's, from its Root Error Status: fatal, non-fatal, then
/// correctable messages received).
pub fn relayed(root_status: u32) -> Option<Severity> {
    let status = LocalRegisterCopy::<u32, RootErrorStatus::Register>::new(root_status);
    if status.is_set(RootErrorStatus::FATAL_RECEIVED) {
        Some(Severity::Fatal)
    } else if status.is_set(RootErrorStatus::NONFATAL_RECEIVED)
        || status.is_set(RootErrorStatus::ERR_UNCOR)
    {
        Some(Severity::Recoverable)
    } else if status.is_set(RootErrorStatus::ERR_COR) {
        Some(Severity::Corrected)
    } else {
        None
    }
}

/// The CPER PCI Express error section of `snapshot`.
pub fn pcie_section(snapshot: &Snapshot) -> [u8; cper::PCIE_SECTION] {
    let f = &snapshot.function;
    let a = f.address;
    let class = snapshot.class.to_le_bytes();
    let device = cper::PcieDevice {
        vendor: snapshot.ids as u16,
        device: (snapshot.ids >> 16) as u16,
        class: [class[1], class[2], class[3]],
        function: a.function(),
        device_number: a.device(),
        segment: a.segment(),
        primary_bus: if f.bridge {
            snapshot.buses as u8
        } else {
            a.bus()
        },
        secondary_bus: if f.bridge {
            (snapshot.buses >> 8) as u8
        } else {
            0
        },
        slot: snapshot.slot.map_or(0, |cap| (cap >> 16) as u16 & 0xfff8),
    };
    cper::pcie(&cper::PcieError {
        port_type: Some(f.port_type.into()),
        version: Some(f.version.into()),
        command_status: Some(snapshot.command_status),
        device: Some(device),
        serial: snapshot.serial,
        bridge: f.bridge.then_some(snapshot.bridge_control_status),
        capability: snapshot
            .pcie
            .get(..usize::from(snapshot.pcie_len))
            .unwrap_or(&[]),
        aer: snapshot
            .aer
            .get(..usize::from(snapshot.aer_len))
            .unwrap_or(&[]),
    })
}

/// The FRU text of the vendor's PCIe records.
const PCIE_FRU: [u8; 20] = *b"PcieError\0\0\0\0\0\0\0\0\0\0\0";

/// The handler's view of config space, the CXL register blocks it may
/// reach (an SMM handler's: those [`Reach::block`] admits) and its records.
pub struct Pass<'a, 'b, P, R, M, E> {
    pub errors: &'a Errors<P>,
    /// The register block at a physical address, if reachable
    /// ([`Reach::block`]: the address is the OS's).
    pub registers: M,
    pub blocks: &'a mut Blocks<'b>,
    /// Each error, for the board's BMC.
    pub event: E,
    pub logged: Logged,
    pub _registers: core::marker::PhantomData<R>,
}

/// The most functions one root port's pass records: the sources, the port
/// and its secondary bus's.
const MAX_FUNCTIONS: usize = 16;

impl<'a, 'b, P, R, M, E> Pass<'a, 'b, P, R, M, E>
where
    P: ConfigRegionAccess,
    R: Registers,
    M: Fn(u64) -> Option<R>,
    E: FnMut(&PcieEvent),
{
    pub fn new(errors: &'a Errors<P>, registers: M, blocks: &'a mut Blocks<'b>, event: E) -> Self {
        Self {
            errors,
            registers,
            blocks,
            event,
            logged: Logged::default(),
            _registers: core::marker::PhantomData,
        }
    }

    /// Append one record; counts it (or its drop).
    fn append(&mut self, severity: Severity, section: &[u8; 16], data: &[u8]) {
        let (block, count) = if severity == Severity::Corrected {
            (&mut *self.blocks.corrected, &mut self.logged.corrected)
        } else {
            (&mut *self.blocks.uncorrected, &mut self.logged.uncorrected)
        };
        match cper::append(block, section, severity, data, Some(&PCIE_FRU)) {
            Ok(()) => *count = count.saturating_add(1),
            Err(cper::Full) => self.logged.dropped = self.logged.dropped.saturating_add(1),
        }
    }

    /// Record `snapshot`'s error as `severity`: its PCI Express section,
    /// a CXL protocol section if it is a CXL agent with a pending RAS
    /// capability (whose status is cleared), and its event.
    fn record(&mut self, snapshot: &Snapshot, severity: Severity) {
        let section = pcie_section(snapshot);
        self.append(severity, &cper::PCIE, &section);
        self.cxl(snapshot);
        let a = snapshot.function.address;
        (self.event)(&PcieEvent {
            segment: a.segment(),
            bus: a.bus(),
            devfn: a.device() << 3 | a.function(),
            severity,
        });
    }

    /// The CXL protocol error of a CXL root port, switch port or device:
    /// its RAS capability's log, if it holds an unmasked error.
    fn cxl(&mut self, snapshot: &Snapshot) {
        let f = &snapshot.function;
        let a = f.address;
        let (agent, dvsec_id, dvsec_dwords) = {
            let (segment, bus, devfn) = (a.segment(), a.bus(), a.device() << 3 | a.function());
            match f.port_type {
                4 => (
                    cper::CxlAgent::RootPort {
                        segment,
                        bus,
                        devfn,
                    },
                    cxl::dvsec::id::FLEX_BUS_PORT,
                    5,
                ),
                5 => (
                    cper::CxlAgent::UpstreamPort {
                        segment,
                        bus,
                        devfn,
                    },
                    cxl::dvsec::id::FLEX_BUS_PORT,
                    5,
                ),
                6 => (
                    cper::CxlAgent::DownstreamPort {
                        segment,
                        bus,
                        devfn,
                    },
                    cxl::dvsec::id::FLEX_BUS_PORT,
                    5,
                ),
                0 => (
                    cper::CxlAgent::Device {
                        segment,
                        bus,
                        devfn,
                    },
                    cxl::dvsec::id::DEVICE,
                    14,
                ),
                _ => return,
            }
        };
        let Some(dvsec_at) = self.errors.dvsec(a, cxl::VENDOR, dvsec_id) else {
            return;
        };
        let Some((regs, ras)) = cxl::ras::locate(self.errors, a, &self.registers) else {
            return;
        };
        let log = cxl::ras::snapshot(&regs, ras);
        let (uncorrectable, correctable) = cxl::ras::pending(&log);
        let severity = if uncorrectable & log[2] != 0 {
            Severity::Fatal
        } else if uncorrectable != 0 {
            Severity::Recoverable
        } else if correctable != 0 {
            Severity::Corrected
        } else {
            return;
        };
        let mut dvsec = [0u32; 14];
        let mut offset = dvsec_at;
        for dword in dvsec.iter_mut().take(dvsec_dwords) {
            *dword = self.errors.config(a, offset);
            offset = offset.wrapping_add(4);
        }
        let ids = snapshot.ids;
        let subsystem = if f.bridge {
            0
        } else {
            self.errors.config(a, 0x2c)
        };
        let class = snapshot.class.to_le_bytes();
        let mut out = [0u8; cper::CXL_PROTOCOL_HEADER + 4 * 14 + 4 * cxl::ras::DWORDS];
        let length = cper::cxl_protocol(
            &mut out,
            &cper::CxlProtocolError {
                agent,
                device: Some(cper::CxlDevice {
                    vendor: ids as u16,
                    device: (ids >> 16) as u16,
                    subsystem_vendor: subsystem as u16,
                    subsystem: (subsystem >> 16) as u16,
                    class: [class[2], class[3]],
                    slot: snapshot.slot.map_or(0, |cap| (cap >> 16) as u16 & 0xfff8),
                }),
                serial: snapshot.serial,
                capability: snapshot
                    .pcie
                    .get(..usize::from(snapshot.pcie_len))
                    .unwrap_or(&[]),
                dvsec: dvsec.get(..dvsec_dwords).unwrap_or(&[]),
                error_log: &log,
            },
        );
        if let Some(data) = length.and_then(|length| out.get(..length)) {
            self.append(severity, &cper::CXL_PROTOCOL, data);
        }
        cxl::ras::clear(&regs, ras);
    }

    /// Handle what the root port `rp` and the functions below it hold of
    /// an error (module doc). Returns whether it held one.
    pub fn root_port(&mut self, rp: &Function) -> bool {
        let port = self.errors.snapshot(rp);
        if !port.has_error() {
            return false;
        }
        let root_status = port.root_status();
        let source = port.error_source();
        let mut seen: [Option<PciAddress>; MAX_FUNCTIONS] = [None; MAX_FUNCTIONS];
        let mut placed = false;
        let a = rp.address;
        let status = LocalRegisterCopy::<u32, RootErrorStatus::Register>::new(root_status);
        let mut candidates = [None; 3];
        if status.is_set(RootErrorStatus::ERR_COR) {
            candidates[0] = Some(source as u16);
        }
        if status.is_set(RootErrorStatus::ERR_UNCOR) {
            candidates[1] = Some((source >> 16) as u16);
        }
        // The port itself.
        if let Some(severity) = severity(&port) {
            self.record(&port, severity);
            note(&mut seen, a);
            placed = true;
        }
        for id in candidates.into_iter().flatten() {
            let address = fstart_pci::address(a.segment(), (id >> 8) as u8, id as u8);
            if id == 0 || address == a || seen.contains(&Some(address)) {
                continue;
            }
            placed |= self.function(address, &mut seen);
        }
        let multiple = status.is_set(RootErrorStatus::MULTIPLE_ERR_COR)
            || status.is_set(RootErrorStatus::MULTIPLE_ERR_UNCOR);
        if (!placed || multiple)
            && let Some((secondary, _)) = self.errors.buses(rp)
        {
            for device in 0..32u8 {
                for function in 0..8u8 {
                    let address =
                        fstart_pci::address(a.segment(), secondary, device << 3 | function);
                    if function == 0 && !self.errors.present(address) {
                        break;
                    }
                    if !seen.contains(&Some(address)) {
                        placed |= self.function(address, &mut seen);
                    }
                    if function == 0 && !self.errors.multifunction(address) {
                        break;
                    }
                }
            }
        }
        if !placed && let Some(severity) = relayed(root_status) {
            // A message from a function that is gone or reads nothing: the
            // port's record says what it received.
            self.record(&port, severity);
        }
        self.errors.clear(rp);
        true
    }

    /// Record and clear the function at `address` if it holds an error.
    fn function(&mut self, address: PciAddress, seen: &mut [Option<PciAddress>]) -> bool {
        note(seen, address);
        let Some(function) = self.errors.function(address) else {
            return false;
        };
        let snapshot = self.errors.snapshot(&function);
        let Some(severity) = severity(&snapshot) else {
            return false;
        };
        self.record(&snapshot, severity);
        self.errors.clear(&function);
        true
    }
}

/// A CXL component register block [`Reach::block`] admitted. Its accesses
/// stay inside the block: an offset beyond it (or unaligned) reads all
/// ones and drops the write, whatever the device's capability pointers say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mmio(u64);

impl Mmio {
    /// The address of the dword at `offset`, if it is in the block.
    fn at(&self, offset: u32) -> Option<u64> {
        (offset & 3 == 0 && u64::from(offset) + 4 <= cxl::component::BLOCK_SIZE)
            .then(|| self.0 + u64::from(offset))
    }
}

impl Registers for Mmio {
    fn read32(&self, offset: u32) -> u32 {
        let Some(at) = self.at(offset) else {
            return u32::MAX;
        };
        // SAFETY: `Reach::block`'s check: the block is in a PCI window of
        // its root bridge, mapped uncached, outside SMRAM.
        unsafe { fstart_core::mmio::read32(at as *const u32) }
    }
    fn write32(&self, offset: u32, value: u32) {
        if let Some(at) = self.at(offset) {
            // SAFETY: as `read32`.
            unsafe { fstart_core::mmio::write32(at as *mut u32, value) }
        }
    }
}

/// Where the CXL register blocks of one root bridge's functions may be.
///
/// A block's address comes from a memory BAR, which the OS programs at
/// run time, and from a Register Locator offset the device reports: an SMM
/// handler that followed it unchecked would read SMRAM into a GHES block
/// the OS reads, and write into SMRAM, the PSP's or the FCH's MMIO. A
/// block is reached only if it lies wholly inside one of `windows`, the
/// root bridge's PCI windows (what its allocator may give a BAR: DRAM, the
/// NBIO's own MMIO and the ECAM are none of them) as the firmware recorded
/// them before SMM was locked, and shares no byte with any of `protected`
/// (SMRAM, the GHES blocks, the handler's report). The boot's
/// configuration of the ports ([`configure`]) goes through the same check.
#[derive(Debug, Clone, Copy)]
pub struct Reach<'a> {
    pub windows: &'a [fstart_smm::Region],
    pub protected: &'a [fstart_smm::Region],
}

impl Reach<'_> {
    /// The component register block at `base`, if it may be reached.
    pub fn block(&self, base: u64) -> Option<Mmio> {
        (base.is_multiple_of(4)
            && fstart_smm::buffer::permitted(
                base,
                cxl::component::BLOCK_SIZE,
                self.windows,
                self.protected,
            ))
        .then_some(Mmio(base))
    }
}

/// Remember `address` in the first free slot of `seen`.
fn note(seen: &mut [Option<PciAddress>], address: PciAddress) {
    if let Some(slot) = seen.iter_mut().find(|slot| slot.is_none()) {
        *slot = Some(address);
    }
}

/// The root ports on the root bus `root` (segment, bus) of a root bridge,
/// each to `f`.
pub fn root_ports<P: ConfigRegionAccess>(
    errors: &Errors<P>,
    root: PciAddress,
    mut f: impl FnMut(&Function),
) {
    for device in 1..32u8 {
        for function in 0..8u8 {
            let address = fstart_pci::address(root.segment(), root.bus(), device << 3 | function);
            if function == 0 && !errors.present(address) {
                break;
            }
            if let Some(port) = errors.function(address).filter(|f| f.is_root_port()) {
                f(&port);
            }
            if function == 0 && !errors.multifunction(address) {
                break;
            }
        }
    }
}

/// Set up `port` and every function below it for firmware-first reporting
/// (the vendor's `PlatformPcieAerEnablement`, [`ROOT_PORT`], [`DEVICE`]);
/// a CXL root port's RAS capability gets poison received masked
/// (`Cxl2p0AerConfig`). Bounded to the buses the port forwards;
/// `registers` checks the RAS block's address as the handler does
/// ([`Reach::block`]).
pub fn configure<P: ConfigRegionAccess, R: Registers>(
    errors: &Errors<P>,
    port: &Function,
    registers: impl Fn(u64) -> Option<R>,
) {
    errors.configure(port, &ROOT_PORT);
    if errors
        .dvsec(port.address, cxl::VENDOR, cxl::dvsec::id::FLEX_BUS_PORT)
        .is_some()
        && let Some((regs, ras)) = cxl::ras::locate(errors, port.address, &registers)
    {
        cxl::ras::mask(
            &regs,
            ras,
            cxl::ras::UNCORRECTABLE_POISON_RECEIVED,
            cxl::ras::CORRECTABLE_MEMORY_POISON_RECEIVED,
        );
        cxl::ras::clear(&regs, ras);
    }
    let Some((secondary, subordinate)) = errors.buses(port) else {
        return;
    };
    let segment = port.address.segment();
    for bus in secondary..=subordinate {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let address = fstart_pci::address(segment, bus, device << 3 | function);
                if function == 0 && !errors.present(address) {
                    break;
                }
                if let Some(f) = errors.function(address) {
                    errors.configure(&f, &DEVICE);
                }
                if function == 0 && !errors.multifunction(address) {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;
    use std::collections::BTreeMap;

    /// Config space: dwords by (bus, devfn, offset), all ones elsewhere;
    /// every write stores (status registers then hold what was written,
    /// enough to see them cleared: the handler writes back what it read).
    #[derive(Default)]
    struct Space(RefCell<BTreeMap<(u8, u8, u16), u32>>);

    impl Space {
        fn set(&self, bus: u8, devfn: u8, offset: u16, value: u32) {
            self.0.borrow_mut().insert((bus, devfn, offset), value);
        }
        fn get(&self, bus: u8, devfn: u8, offset: u16) -> u32 {
            *self
                .0
                .borrow()
                .get(&(bus, devfn, offset))
                .unwrap_or(&u32::MAX)
        }
        /// A PCIe function of `port_type`; PCIe cap at 0x40, AER at 0x100.
        fn function(&self, bus: u8, devfn: u8, port_type: u32, bridge: bool) {
            self.set(bus, devfn, 0, 0x1234_1022);
            self.set(bus, devfn, 0x04, 0x0010_0006);
            self.set(bus, devfn, 0x08, 0x0108_0200);
            self.set(bus, devfn, 0x0c, u32::from(bridge) << 16);
            self.set(bus, devfn, 0x34, 0x40);
            self.set(bus, devfn, 0x40, (port_type << 4 | 2) << 16 | 0x10);
            self.set(bus, devfn, 0x48, 0x2810);
            self.set(bus, devfn, 0x100, 0x0002_0001);
            for offset in (0x104..0x148).step_by(4) {
                self.set(bus, devfn, offset, 0);
            }
            if bridge {
                self.set(bus, devfn, 0x18, 0x0003_0300);
                self.set(bus, devfn, 0x1c, 0);
                self.set(bus, devfn, 0x3c, 0);
            }
        }
    }

    impl ConfigRegionAccess for &Space {
        unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
            self.get(
                address.bus(),
                address.device() << 3 | address.function(),
                offset,
            )
        }
        unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
            let devfn = address.device() << 3 | address.function();
            let w1c = [0x104, 0x110, 0x130].contains(&offset) || offset == 0x04;
            let old = self.get(address.bus(), devfn, offset);
            let new = if w1c && offset != 0x04 {
                old & !value
            } else {
                value
            };
            self.set(address.bus(), devfn, offset, new);
        }
    }

    struct NoRegisters;
    impl Registers for NoRegisters {
        fn read32(&self, _: u32) -> u32 {
            0
        }
        fn write32(&self, _: u32, _: u32) {}
    }

    /// A decoder of a status block, from ACPI 6.5 table 18.12/18.13: the
    /// entries' severity, section and data.
    fn entries(block: &[u8]) -> Vec<(u32, [u8; 16], Vec<u8>)> {
        let u32_at = |at: usize| u32::from_le_bytes(block[at..at + 4].try_into().unwrap());
        let mut out = Vec::new();
        if u32_at(0) == 0 {
            return out;
        }
        let end = 20 + u32_at(12) as usize;
        let mut at = 20;
        while at < end {
            let length = u32_at(at + 24) as usize;
            out.push((
                u32_at(at + 16),
                block[at..at + 16].try_into().unwrap(),
                block[at + 72..at + 72 + length].to_vec(),
            ));
            at += 72 + length;
        }
        assert_eq!(at, end);
        assert_eq!((u32_at(0) >> 4 & 0x3ff) as usize, out.len());
        out
    }

    /// A root port 0:1.1 that received ERR_COR and a non-fatal ERR_UNCOR
    /// from the endpoint 3:0.0 behind it: the endpoint's two errors are
    /// recorded where they belong, a PCIe section each with its AER
    /// registers, the BMC hears of both, everything is cleared, the
    /// notifications follow the sources' kinds.
    #[test]
    fn a_root_port_relays_an_endpoints_errors() {
        let space = Space::default();
        space.function(0, 0x09, 4, true);
        space.set(0, 0x09, 0x130, 0b0010_0101);
        space.set(0, 0x09, 0x134, 0x0300_0300);
        space.function(3, 0, 0, false);
        // Endpoint: a masked and an unmasked correctable error; an
        // unsupported request (non-fatal, severity clear).
        space.set(3, 0, 0x104, 1 << 20);
        space.set(3, 0, 0x110, 1 << 6 | 1 << 13);
        space.set(3, 0, 0x114, 1 << 13);
        space.set(3, 0, 0x4c, 0x0000_0000);
        let errors = unsafe { Errors::new(&space) };
        let mut corrected = [0u8; 4096];
        let mut uncorrected = [0u8; 4096];
        let mut blocks = Blocks {
            corrected: &mut corrected,
            uncorrected: &mut uncorrected,
        };
        let mut events = Vec::new();
        let rp = errors.function(PciAddress::new(0, 0, 1, 1)).unwrap();
        let logged = {
            let mut pass = Pass::new(
                &errors,
                |_| None::<NoRegisters>,
                &mut blocks,
                |e: &PcieEvent| events.push(*e),
            );
            assert!(pass.root_port(&rp));
            pass.logged
        };
        assert_eq!(
            logged,
            Logged {
                corrected: 0,
                uncorrected: 1,
                dropped: 0
            }
        );
        // One function, one error class: the worst; its record names it.
        let found = entries(blocks.uncorrected);
        assert_eq!(found.len(), 1);
        let (severity, section, data) = &found[0];
        assert_eq!(
            (*severity, *section),
            (Severity::Recoverable as u32, cper::PCIE)
        );
        // Device ID valid, AER valid; bus 3, device 0.
        assert_eq!(data[0] & 0x88, 0x88);
        assert_eq!((data[31], data[32], data[35]), (0, 0, 3));
        // AER uncorrectable status in the record.
        assert_eq!(&data[116..120], &(1u32 << 20).to_le_bytes());
        assert_eq!(
            events,
            [PcieEvent {
                segment: 0,
                bus: 3,
                devfn: 0,
                severity: Severity::Recoverable
            }]
        );
        // Cleared: the endpoint's status, the port's root status.
        assert_eq!(space.get(3, 0, 0x104), 0);
        assert_eq!(space.get(3, 0, 0x110) & 1 << 6, 0);
        assert_eq!(space.get(0, 0x09, 0x130) & 0x7f, 0);
        let ff = FirmwareFirst {
            corrected: Ghes {
                notify: notify::SCI,
                ..Ghes::default()
            },
            uncorrected: Ghes {
                notify: notify::NMI,
                ..Ghes::default()
            },
            ..FirmwareFirst::NONE
        };
        assert_eq!(logged.signals(&ff), (false, true));
        // Nothing left: the next pass finds nothing.
        let mut pass = Pass::new(
            &errors,
            |_| None::<NoRegisters>,
            &mut blocks,
            |_: &PcieEvent| {},
        );
        assert!(!pass.root_port(&rp));
    }

    /// A root port whose message source is gone (reads all ones): its own
    /// record, with the severity its Root Error Status received; a
    /// correctable error goes to the corrected block.
    #[test]
    fn a_message_from_a_function_that_is_gone() {
        let space = Space::default();
        space.function(0, 0x09, 4, true);
        // Fatal received; source 3:0.0 absent; nothing on bus 3.
        space.set(0, 0x09, 0x130, 0b0101_0100);
        space.set(0, 0x09, 0x134, 0x0300_0000);
        let errors = unsafe { Errors::new(&space) };
        let mut corrected = [0u8; 1024];
        let mut uncorrected = [0u8; 1024];
        let mut blocks = Blocks {
            corrected: &mut corrected,
            uncorrected: &mut uncorrected,
        };
        let rp = errors.function(PciAddress::new(0, 0, 1, 1)).unwrap();
        let mut pass = Pass::new(
            &errors,
            |_| None::<NoRegisters>,
            &mut blocks,
            |_: &PcieEvent| {},
        );
        assert!(pass.root_port(&rp));
        assert_eq!(pass.logged.uncorrected, 1);
        let found = entries(blocks.uncorrected);
        assert_eq!(found[0].0, Severity::Fatal as u32);
        // The port's own record: bus 0 device 1 function 1, secondary 3.
        assert_eq!((found[0].2[31], found[0].2[32], found[0].2[36]), (1, 1, 3));
        // A correctable error of the port itself.
        space.set(0, 0x09, 0x110, 1);
        let mut pass = Pass::new(
            &errors,
            |_| None::<NoRegisters>,
            &mut blocks,
            |_: &PcieEvent| {},
        );
        assert!(pass.root_port(&rp));
        assert_eq!(pass.logged.corrected, 1);
        assert_eq!(entries(blocks.corrected)[0].0, Severity::Corrected as u32);
    }

    use fstart_smm::Region;

    /// SP7-like: TSEG at 0x7F00_0000 (16 MiB), a root bridge's PCI windows
    /// below and above 4 GiB, the GHES page and the report in DRAM.
    const TSEG: Region = Region::new(0x7f00_0000, 0x100_0000);
    const WINDOWS: [Region; 2] = [
        Region::new(0xe000_0000, 0x1000_0000),
        Region::new(0x30_0000_0000, 0x100_0000_0000),
    ];
    const GHES_PAGE: Region = Region::new(0x7600_0000, 0x3000);

    /// Hostile register-block addresses (BAR + Register Locator offset,
    /// both the OS's or the device's) are refused: in TSEG, straddling it
    /// or a window's end, in DRAM, wrapping, unaligned; also when a window
    /// wrongly covers TSEG. Blocks wholly inside a window are reached.
    #[test]
    fn hostile_register_blocks_are_refused() {
        let protected = [TSEG, GHES_PAGE];
        let reach = Reach {
            windows: &WINDOWS,
            protected: &protected,
        };
        let block = cxl::component::BLOCK_SIZE;
        assert!(reach.block(0xe000_0000).is_some());
        assert!(reach.block(0xf000_0000 - block).is_some());
        assert!(reach.block(0x30_0000_0000 + 0x12_3450_0000).is_some());
        for hostile in [
            0,
            TSEG.base,
            TSEG.base + 0x8000,
            TSEG.base - block / 2,
            TSEG.end().unwrap() - 4,
            GHES_PAGE.base,
            0xf000_0000 - block / 2,
            0xf000_0000,
            0x1_0000_0000,
            0x30_0000_0000 - 4,
            0x130_0000_0000 - 4,
            u64::MAX - 0xfff,
            0xe000_0002,
        ] {
            assert!(reach.block(hostile).is_none(), "{hostile:#x}");
        }
        // A window that (wrongly) covers TSEG does not make TSEG reachable.
        let wide = [Region::new(0x7000_0000, 0x9000_0000)];
        let reach = Reach {
            windows: &wide,
            protected: &protected,
        };
        assert!(reach.block(TSEG.base + 0x1_0000).is_none());
        assert!(reach.block(0x7000_0000).is_some());
        // No windows: nothing.
        let reach = Reach {
            windows: &[],
            protected: &protected,
        };
        assert!(reach.block(0xe000_0000).is_none());
    }

    /// An admitted block's accesses stay inside it: an offset at or past
    /// its end, or unaligned, is never an address.
    #[test]
    fn register_offsets_stay_in_the_block() {
        let block = Mmio(0xe000_0000);
        assert_eq!(block.at(0), Some(0xe000_0000));
        assert_eq!(block.at(0xfffc), Some(0xe000_fffc));
        for offset in [0x1_0000, 0xfffe, 2, u32::MAX - 3, u32::MAX] {
            assert_eq!(block.at(offset), None, "{offset:#x}");
        }
    }

    /// Fake CXL component registers: a capability array with the RAS
    /// capability at CACHE_MEM + 0x40, an uncorrectable error pending;
    /// every access recorded.
    struct Component(RefCell<BTreeMap<u32, u32>>, RefCell<Vec<u32>>);
    impl Component {
        fn new() -> Self {
            let regs = BTreeMap::from([
                (cxl::component::CACHE_MEM, cxl::component::array_header(1)),
                (
                    cxl::component::CACHE_MEM + 4,
                    cxl::component::capability_header(cxl::component::id::RAS, 2, 0x40),
                ),
                (cxl::component::CACHE_MEM + 0x40, 1 << 3),
            ]);
            Self(RefCell::new(regs), RefCell::new(Vec::new()))
        }
    }
    impl Registers for &Component {
        fn read32(&self, offset: u32) -> u32 {
            *self.0.borrow().get(&offset).unwrap_or(&0)
        }
        fn write32(&self, offset: u32, value: u32) {
            self.1.borrow_mut().push(offset);
            let old = self.read32(offset);
            self.0.borrow_mut().insert(offset, old & !value);
        }
    }

    /// A CXL root port 0:1.1 with an uncorrectable error of its own, its
    /// component registers at BAR0 (64-bit) + 64 KiB by its Register
    /// Locator, its RAS capability with an error pending.
    fn cxl_root_port(space: &Space, bar: u64) {
        space.function(0, 0x09, 4, true);
        space.set(0, 0x09, 0x104, 1 << 20);
        space.set(0, 0x09, 0x10, bar as u32 | 4);
        space.set(0, 0x09, 0x14, (bar >> 32) as u32);
        // AER, then the Flex Bus port DVSEC at 0x148, then the Register
        // Locator at 0x160.
        space.set(0, 0x09, 0x100, 0x148 << 20 | 2 << 16 | 1);
        space.set(0, 0x09, 0x148, 0x160 << 20 | 1 << 16 | 0x23);
        space.set(
            0,
            0x09,
            0x14c,
            0x014 << 20 | 1 << 16 | u32::from(cxl::VENDOR),
        );
        space.set(0, 0x09, 0x150, cxl::dvsec::id::FLEX_BUS_PORT.into());
        space.set(0, 0x09, 0x160, 1 << 16 | 0x23);
        space.set(
            0,
            0x09,
            0x164,
            0x014 << 20 | 1 << 16 | u32::from(cxl::VENDOR),
        );
        space.set(0, 0x09, 0x168, cxl::dvsec::id::REGISTER_LOCATOR.into());
        // Component block (id 1), BAR0, offset 64 KiB.
        space.set(0, 0x09, 0x16c, 0x0001_0100);
        space.set(0, 0x09, 0x170, 0);
    }

    /// The handler's pass over a CXL root port: with its BAR in a window
    /// of the root bridge, the CXL protocol error is recorded from the
    /// block and cleared; with the OS's BAR aimed at TSEG (or at DRAM) the
    /// block is refused before any access, and only the PCIe record is
    /// written.
    #[test]
    fn a_hostile_cxl_bar_is_never_dereferenced() {
        let protected = [TSEG, GHES_PAGE];
        let reach = Reach {
            windows: &WINDOWS,
            protected: &protected,
        };
        for (bar, reached) in [
            (0xe010_0000, true),
            (TSEG.base - 0x1_0000, false),
            (TSEG.base + 0x10_0000, false),
            (0x2_0000_0000, false),
        ] {
            let space = Space::default();
            cxl_root_port(&space, bar);
            let errors = unsafe { Errors::new(&space) };
            let component = Component::new();
            let asked = RefCell::new(Vec::new());
            let mut corrected = [0u8; 4096];
            let mut uncorrected = [0u8; 4096];
            let mut blocks = Blocks {
                corrected: &mut corrected,
                uncorrected: &mut uncorrected,
            };
            let rp = errors.function(PciAddress::new(0, 0, 1, 1)).unwrap();
            let mut pass = Pass::new(
                &errors,
                |base| {
                    asked.borrow_mut().push(base);
                    reach.block(base).map(|_| &component)
                },
                &mut blocks,
                |_: &PcieEvent| {},
            );
            assert!(pass.root_port(&rp));
            assert_eq!(*asked.borrow(), [bar + 0x1_0000], "{bar:#x}");
            let found = entries(blocks.uncorrected);
            let sections: Vec<_> = found.iter().map(|entry| entry.1).collect();
            if reached {
                assert_eq!(sections, [cper::PCIE, cper::CXL_PROTOCOL]);
                assert!(!component.1.borrow().is_empty(), "the RAS status cleared");
            } else {
                assert_eq!(sections, [cper::PCIE], "{bar:#x}");
                assert!(component.1.borrow().is_empty());
            }
        }
    }

    /// The firmware-first setup of a port and the endpoint behind it; the
    /// root ports of a root bus are found.
    #[test]
    fn configure_a_hierarchy() {
        let space = Space::default();
        // WH's dummy host bridge 0:1.0 (multi-function), its port 0:1.1.
        space.set(0, 0x08, 0, 0x1482_1022);
        space.set(0, 0x08, 0x04, 0);
        space.set(0, 0x08, 0x0c, 0x0080_0000);
        space.function(0, 0x09, 4, true);
        space.function(3, 0, 0, false);
        let errors = unsafe { Errors::new(&space) };
        let mut ports = Vec::new();
        root_ports(&errors, PciAddress::new(0, 0, 0, 0), |p| {
            ports.push(p.address)
        });
        assert_eq!(ports, [PciAddress::new(0, 0, 1, 1)]);
        let rp = errors.function(ports[0]).unwrap();
        configure(&errors, &rp, |_| None::<NoRegisters>);
        assert_eq!(space.get(0, 0x09, 0x114), 0x2000);
        assert_eq!(space.get(0, 0x09, 0x10c), 0x57ef_6030);
        assert_eq!(space.get(3, 0, 0x108), 0x10_0000);
        assert_eq!(space.get(3, 0, 0x48) & 7, 7);
    }
}
