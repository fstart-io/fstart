//! ACPI table provider service trait.
//!
//! Implemented by devices that can supply pre-built ACPI tables to firmware,
//! such as QEMU's fw_cfg device. The firmware calls `load_acpi_tables()` to
//! load the tables into a buffer, then passes the RSDP address to the OS
//! via the boot protocol (e.g., x86 zero page, UEFI system table).
//!
//! For platforms that generate their own ACPI tables from the board RON,
//! the `AcpiPrepare` capability is used instead — it does not go through
//! this trait.

use crate::ServiceError;

/// A device that can provide pre-built ACPI tables.
///
/// The provider loads ACPI tables into the caller's buffer, processes
/// any relocation or checksumming required, and returns the physical
/// address of the RSDP (Root System Description Pointer).
pub trait AcpiTableProvider {
    /// Load ACPI tables into `buffer`.
    ///
    /// The implementation may use the buffer as scratch space for table
    /// placement and patching (e.g., the QEMU table-loader protocol).
    /// The buffer contents must remain valid after this call returns
    /// (the caller will leak/forget the buffer so the OS can access
    /// the tables).
    ///
    /// Returns the physical address of the RSDP within the buffer.
    fn load_acpi_tables(&self, buffer: &mut [u8]) -> Result<u64, ServiceError>;
}
