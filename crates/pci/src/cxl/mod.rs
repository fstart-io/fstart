//! Compute Express Link: the spec-level registers firmware touches on a CXL
//! root port, switch or device (CXL 3.1), independent of any host.
//!
//! - [`dvsec`]: the CXL DVSECs in PCI configuration space (vendor
//!   [`VENDOR`]): the device's ranges and enables (ID 0), the Flex Bus port
//!   status of a link (ID 7), the Register Locator (ID 8) that places the
//!   memory-mapped register blocks in a BAR;
//! - [`component`]: the component register block (64 KiB) and its
//!   CXL.cache/mem capability array;
//! - [`hdm`]: the HDM decoder capability and its interleave encodings
//!   (shared with the ACPI CEDT's CFMWS);
//! - [`rcrb`]: a CXL 1.1 downstream port's RCRB;
//! - [`doe`] and [`cdat`]: the DOE mailbox and the device's CDAT read
//!   through it;
//! - [`ras`]: the RAS capability of a port or device (error status, masks,
//!   header log), for firmware-first error records;
//! - [`mailbox`]: a memory device's primary mailbox (CEL, Dynamic
//!   Capacity configuration).
//!
//! What a host bridge's own component registers are and how memory windows
//! get to a host bridge is the platform's (AMD: firmware-emulated, see
//! `fstart-driver-amd`'s `cxl`).

pub mod cdat;
pub mod component;
pub mod doe;
pub mod dvsec;
pub mod hdm;
pub mod mailbox;
pub mod ras;
pub mod rcrb;

/// The PCI-SIG vendor ID the CXL consortium's DVSECs carry.
pub const VENDOR: u16 = 0x1e98;

use crate::EcamDevice;

/// Dword access to a memory-mapped CXL register block (component
/// registers, a memory device's registers) by byte offset: an
/// [`fstart_core::mmio::MmioWindow`] on hardware, a model in tests.
pub trait Registers {
    fn read32(&self, offset: u32) -> u32;
    fn write32(&self, offset: u32, value: u32);
}

impl Registers for fstart_core::mmio::MmioWindow {
    fn read32(&self, offset: u32) -> u32 {
        fstart_core::mmio::MmioWindow::read32(*self, offset)
    }
    fn write32(&self, offset: u32, value: u32) {
        fstart_core::mmio::MmioWindow::write32(*self, offset, value)
    }
}

/// The CXL DVSEC `id` of `device` with at least its CXL 2.0 length.
pub fn find_dvsec(device: &EcamDevice, id: u16) -> Option<u16> {
    device.find_dvsec(VENDOR, id, dvsec::min_bytes(id))
}

/// The CXL protocols a link negotiated (Flex Bus Port DVSEC status), or
/// `None` when the port has no Flex Bus DVSEC: a plain PCIe link.
pub fn flex_bus_status(
    port: &EcamDevice,
) -> Option<tock_registers::LocalRegisterCopy<u16, dvsec::FlexBusPort::Register>> {
    let dvsec = find_dvsec(port, dvsec::id::FLEX_BUS_PORT)?;
    Some(tock_registers::LocalRegisterCopy::new(
        port.read16(dvsec + dvsec::FLEX_BUS_STATUS),
    ))
}
