//! Standard PCI configuration-space constants and bitfields.

use tock_registers::register_bitfields;

/// Vendor ID register (16-bit, offset 0x00). Returns 0xFFFF when absent.
pub const PCI_VENDOR_ID: u16 = 0x00;
/// Device ID register (16-bit, offset 0x02).
pub const PCI_DEVICE_ID: u16 = 0x02;
/// Command register (16-bit, offset 0x04).
pub const PCI_COMMAND: u16 = 0x04;
/// Status register (16-bit, offset 0x06). Contains write-1-to-clear bits.
pub const PCI_STATUS: u16 = 0x06;
/// Revision ID + Class Code (32-bit, offset 0x08).
pub const PCI_CLASS_REVISION: u16 = 0x08;
/// Header Type (8-bit, offset 0x0E). Bit 7 = multi-function.
pub const PCI_HEADER_TYPE: u16 = 0x0E;
/// Capabilities pointer in Type 0/1 headers (8-bit, offset 0x34).
pub const PCI_CAPABILITIES_PTR: u16 = 0x34;

/// Base Address Register 0 (offset 0x10).
pub const PCI_BAR0: u16 = 0x10;
/// Base Address Register 1 (offset 0x14).
pub const PCI_BAR1: u16 = 0x14;
/// Base Address Register 2 (offset 0x18).
pub const PCI_BAR2: u16 = 0x18;
/// Base Address Register 3 (offset 0x1C).
pub const PCI_BAR3: u16 = 0x1C;
/// Base Address Register 4 (offset 0x20).
pub const PCI_BAR4: u16 = 0x20;
/// Base Address Register 5 (offset 0x24).
pub const PCI_BAR5: u16 = 0x24;
/// Interrupt Line (offset 0x3C).
pub const PCI_INTERRUPT_LINE: u16 = 0x3C;
/// Interrupt Pin (offset 0x3D).
pub const PCI_INTERRUPT_PIN: u16 = 0x3D;

/// Primary Bus Number (offset 0x18, type 1 header).
pub const PCI_PRIMARY_BUS: u16 = 0x18;
/// I/O Base (offset 0x1C, type 1 header).
pub const PCI_IO_BASE: u16 = 0x1C;
/// Memory Base (offset 0x20, type 1 header).
pub const PCI_MEMORY_BASE: u16 = 0x20;
/// Prefetchable Memory Base (offset 0x24, type 1 header).
pub const PCI_PREF_MEMORY_BASE: u16 = 0x24;
/// Prefetchable Base Upper 32 bits (offset 0x28, type 1 header).
pub const PCI_PREF_BASE_UPPER32: u16 = 0x28;
/// Prefetchable Limit Upper 32 bits (offset 0x2C, type 1 header).
pub const PCI_PREF_LIMIT_UPPER32: u16 = 0x2C;

/// Type 1 (PCI-to-PCI bridge).
pub const PCI_HEADER_TYPE_BRIDGE: u8 = 0x01;
/// Type 2 (PCI-to-CardBus bridge).
pub const PCI_HEADER_TYPE_CARDBUS: u8 = 0x02;
/// Multi-function device flag (bit 7 of header type).
pub const PCI_HEADER_TYPE_MULTI_FUNC: u8 = 0x80;

/// Value read from PCI_VENDOR_ID when no device is present.
pub const PCI_VENDOR_INVALID: u32 = 0xFFFF_FFFF;

/// I/O Space Enable (bit 0).
pub const PCI_CMD_IO: u16 = 0x0001;
/// Memory Space Enable (bit 1).
pub const PCI_CMD_MEMORY: u16 = 0x0002;
/// Bus Master Enable (bit 2).
pub const PCI_CMD_BUS_MASTER: u16 = 0x0004;

register_bitfields! [u16,
    /// PCI command register bitfields.
    pub PCI_COMMAND_BITS [
        IO_SPACE OFFSET(0) NUMBITS(1) [],
        MEMORY_SPACE OFFSET(1) NUMBITS(1) [],
        BUS_MASTER OFFSET(2) NUMBITS(1) [],
        SPECIAL_CYCLES OFFSET(3) NUMBITS(1) [],
        MEM_WRITE_INVALIDATE OFFSET(4) NUMBITS(1) [],
        VGA_PALETTE_SNOOP OFFSET(5) NUMBITS(1) [],
        PARITY_ERROR_RESPONSE OFFSET(6) NUMBITS(1) [],
        SERR_ENABLE OFFSET(8) NUMBITS(1) [],
        FAST_BACK_TO_BACK OFFSET(9) NUMBITS(1) [],
        INT_DISABLE OFFSET(10) NUMBITS(1) []
    ],
    /// PCI status register bitfields. Many bits are write-1-to-clear; avoid
    /// generic read-modify-write helpers for clearing status.
    pub PCI_STATUS_BITS [
        CAPABILITIES_LIST OFFSET(4) NUMBITS(1) [],
        MASTER_DATA_PARITY_ERROR OFFSET(8) NUMBITS(1) [],
        SIGNALED_TARGET_ABORT OFFSET(11) NUMBITS(1) [],
        RECEIVED_TARGET_ABORT OFFSET(12) NUMBITS(1) [],
        RECEIVED_MASTER_ABORT OFFSET(13) NUMBITS(1) [],
        SIGNALED_SYSTEM_ERROR OFFSET(14) NUMBITS(1) [],
        DETECTED_PARITY_ERROR OFFSET(15) NUMBITS(1) []
    ],
];
