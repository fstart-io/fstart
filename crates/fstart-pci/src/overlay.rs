//! Tock-registers overlays for standard PCI configuration headers.
//!
//! These overlays are intentionally conservative. Discovery, BAR sizing,
//! capability traversal, write-1-to-clear status handling, and exact-write
//! errata sequences should continue to use raw config access helpers.

use fstart_mmio::{MmioReadOnly, MmioReadWrite};
use tock_registers::register_structs;

use crate::config::{PCI_COMMAND_BITS, PCI_STATUS_BITS};

register_structs! {
    /// Conservative Type 0 PCI configuration header overlay.
    ///
    /// BARs and status are exposed as raw registers: do not use generic
    /// read-modify-write operations for BAR sizing or W1C status handling.
    pub PciType0Config {
        (0x00 => pub vendor_id: MmioReadOnly<u16>),
        (0x02 => pub device_id: MmioReadOnly<u16>),
        (0x04 => pub command: MmioReadWrite<u16, PCI_COMMAND_BITS::Register>),
        (0x06 => pub status_raw: MmioReadWrite<u16, PCI_STATUS_BITS::Register>),
        (0x08 => pub revision_id: MmioReadOnly<u8>),
        (0x09 => pub prog_if: MmioReadOnly<u8>),
        (0x0a => pub subclass: MmioReadOnly<u8>),
        (0x0b => pub class_code: MmioReadOnly<u8>),
        (0x0c => pub cache_line_size: MmioReadWrite<u8>),
        (0x0d => pub latency_timer: MmioReadWrite<u8>),
        (0x0e => pub header_type: MmioReadOnly<u8>),
        (0x0f => pub bist: MmioReadWrite<u8>),
        (0x10 => pub bar: [MmioReadWrite<u32>; 6]),
        (0x28 => pub cardbus_cis_pointer: MmioReadOnly<u32>),
        (0x2c => pub subsystem_vendor_id: MmioReadOnly<u16>),
        (0x2e => pub subsystem_id: MmioReadOnly<u16>),
        (0x30 => pub expansion_rom_base_raw: MmioReadWrite<u32>),
        (0x34 => pub capabilities_ptr: MmioReadOnly<u8>),
        (0x35 => _reserved0),
        (0x3c => pub interrupt_line: MmioReadWrite<u8>),
        (0x3d => pub interrupt_pin: MmioReadOnly<u8>),
        (0x3e => pub min_grant: MmioReadOnly<u8>),
        (0x3f => pub max_latency: MmioReadOnly<u8>),
        (0x40 => @END),
    }
}

register_structs! {
    /// Conservative Type 1 PCI-to-PCI bridge configuration header overlay.
    pub PciType1Config {
        (0x00 => pub vendor_id: MmioReadOnly<u16>),
        (0x02 => pub device_id: MmioReadOnly<u16>),
        (0x04 => pub command: MmioReadWrite<u16, PCI_COMMAND_BITS::Register>),
        (0x06 => pub status_raw: MmioReadWrite<u16, PCI_STATUS_BITS::Register>),
        (0x08 => pub revision_id: MmioReadOnly<u8>),
        (0x09 => pub prog_if: MmioReadOnly<u8>),
        (0x0a => pub subclass: MmioReadOnly<u8>),
        (0x0b => pub class_code: MmioReadOnly<u8>),
        (0x0c => pub cache_line_size: MmioReadWrite<u8>),
        (0x0d => pub latency_timer: MmioReadWrite<u8>),
        (0x0e => pub header_type: MmioReadOnly<u8>),
        (0x0f => pub bist: MmioReadWrite<u8>),
        (0x10 => pub bar: [MmioReadWrite<u32>; 2]),
        (0x18 => pub primary_bus: MmioReadWrite<u8>),
        (0x19 => pub secondary_bus: MmioReadWrite<u8>),
        (0x1a => pub subordinate_bus: MmioReadWrite<u8>),
        (0x1b => pub secondary_latency_timer: MmioReadWrite<u8>),
        (0x1c => pub io_base: MmioReadWrite<u8>),
        (0x1d => pub io_limit: MmioReadWrite<u8>),
        (0x1e => pub secondary_status_raw: MmioReadWrite<u16, PCI_STATUS_BITS::Register>),
        (0x20 => pub memory_base: MmioReadWrite<u16>),
        (0x22 => pub memory_limit: MmioReadWrite<u16>),
        (0x24 => pub prefetchable_memory_base: MmioReadWrite<u16>),
        (0x26 => pub prefetchable_memory_limit: MmioReadWrite<u16>),
        (0x28 => pub prefetchable_base_upper32: MmioReadWrite<u32>),
        (0x2c => pub prefetchable_limit_upper32: MmioReadWrite<u32>),
        (0x30 => pub io_base_upper16: MmioReadWrite<u16>),
        (0x32 => pub io_limit_upper16: MmioReadWrite<u16>),
        (0x34 => pub capabilities_ptr: MmioReadOnly<u8>),
        (0x35 => _reserved0),
        (0x38 => pub expansion_rom_base_raw: MmioReadWrite<u32>),
        (0x3c => pub interrupt_line: MmioReadWrite<u8>),
        (0x3d => pub interrupt_pin: MmioReadOnly<u8>),
        (0x3e => pub bridge_control: MmioReadWrite<u16>),
        (0x40 => @END),
    }
}

/// Define a flat Type 0 config-space overlay with standard header fields plus
/// driver-specific fields at offsets `0x40..`.
#[macro_export]
macro_rules! pci_type0_config {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($field:tt)* }) => {
        $crate::fstart_mmio::tock_registers::register_structs! {
            $(#[$meta])*
            $vis $name {
                (0x00 => pub vendor_id: $crate::fstart_mmio::MmioReadOnly<u16>),
                (0x02 => pub device_id: $crate::fstart_mmio::MmioReadOnly<u16>),
                (0x04 => pub command: $crate::fstart_mmio::MmioReadWrite<u16, $crate::PCI_COMMAND_BITS::Register>),
                (0x06 => pub status_raw: $crate::fstart_mmio::MmioReadWrite<u16, $crate::PCI_STATUS_BITS::Register>),
                (0x08 => pub revision_id: $crate::fstart_mmio::MmioReadOnly<u8>),
                (0x09 => pub prog_if: $crate::fstart_mmio::MmioReadOnly<u8>),
                (0x0a => pub subclass: $crate::fstart_mmio::MmioReadOnly<u8>),
                (0x0b => pub class_code: $crate::fstart_mmio::MmioReadOnly<u8>),
                (0x0c => pub cache_line_size: $crate::fstart_mmio::MmioReadWrite<u8>),
                (0x0d => pub latency_timer: $crate::fstart_mmio::MmioReadWrite<u8>),
                (0x0e => pub header_type: $crate::fstart_mmio::MmioReadOnly<u8>),
                (0x0f => pub bist: $crate::fstart_mmio::MmioReadWrite<u8>),
                (0x10 => pub bar: [$crate::fstart_mmio::MmioReadWrite<u32>; 6]),
                (0x28 => pub cardbus_cis_pointer: $crate::fstart_mmio::MmioReadOnly<u32>),
                (0x2c => pub subsystem_vendor_id: $crate::fstart_mmio::MmioReadOnly<u16>),
                (0x2e => pub subsystem_id: $crate::fstart_mmio::MmioReadOnly<u16>),
                (0x30 => pub expansion_rom_base_raw: $crate::fstart_mmio::MmioReadWrite<u32>),
                (0x34 => pub capabilities_ptr: $crate::fstart_mmio::MmioReadOnly<u8>),
                (0x35 => _reserved0),
                (0x3c => pub interrupt_line: $crate::fstart_mmio::MmioReadWrite<u8>),
                (0x3d => pub interrupt_pin: $crate::fstart_mmio::MmioReadOnly<u8>),
                (0x3e => pub min_grant: $crate::fstart_mmio::MmioReadOnly<u8>),
                (0x3f => pub max_latency: $crate::fstart_mmio::MmioReadOnly<u8>),
                $($field)*
            }
        }
    };
}
