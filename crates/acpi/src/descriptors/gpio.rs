//! GPIO Connection Descriptors for ACPI Resource Templates.
//!
//! Implements [`GpioInt`] (GPIO interrupt connection) and [`GpioIo`]
//! (GPIO I/O connection) as [`Aml`] types for use inside
//! `ResourceTemplate`.
//!
//! These are essential for SoC platforms where GPIO controllers
//! expose pins to the OS via ACPI `_CRS` resources.
//!
//! # ACPI specification reference
//!
//! Section 6.4.3.8.1 -- GPIO Connection Descriptor (Type 0x8C).
//!
//! [`Aml`]: acpi_tables::Aml

use acpi_tables::{Aml, AmlSink};
use tock_registers::register_bitfields;
use zerocopy::little_endian::U16;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// GPIO Connection Descriptor tag byte.
const GPIO_DESCRIPTOR_TAG: u8 = 0x8C;

/// GPIO Connection Descriptor revision.
const GPIO_REVISION_ID: u8 = 0x01;

/// Byte offset of the Pin Table from the descriptor start: the 23-byte
/// header (ACPI 6.5 Table 6.255).
const GPIO_PIN_TABLE_OFFSET: u16 = core::mem::size_of::<GpioDescriptorHeader>() as u16;

register_bitfields![u16,
    /// GPIO Connection Descriptor General Flags (offset 0x05, 16 bits).
    GeneralFlags [
        CONSUMER OFFSET(0) NUMBITS(1) []
    ],
    /// GPIO Interrupt Connection Flags (offset 0x07, 16 bits).
    IntFlags [
        MODE OFFSET(0) NUMBITS(1) [],
        POLARITY OFFSET(1) NUMBITS(2) [],
        SHARED OFFSET(3) NUMBITS(1) []
    ],
    /// GPIO I/O Connection Flags (offset 0x07, 16 bits).
    IoFlags [
        IO_RESTRICTION OFFSET(0) NUMBITS(2) []
    ]
];

/// Fixed 23-byte header of an ACPI 6.5 GPIO Connection Descriptor (Type 0x8C).
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct GpioDescriptorHeader {
    tag: u8,
    data_length: U16,
    revision_id: u8,
    connection_type: u8,
    general_flags: U16,
    type_flags: U16,
    pin_config: u8,
    drive_strength: U16,
    debounce: U16,
    pin_table_offset: U16,
    resource_source_index: u8,
    resource_source_offset: U16,
    vendor_data_offset: U16,
    vendor_data_length: U16,
}

const _: () = assert!(core::mem::size_of::<GpioDescriptorHeader>() == 23);

/// GPIO interrupt trigger mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioIntMode {
    /// Level-triggered interrupt.
    Level = 0,
    /// Edge-triggered interrupt.
    Edge = 1,
}

/// GPIO interrupt polarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioIntPolarity {
    /// Active-high (or rising edge).
    ActiveHigh = 0,
    /// Active-low (or falling edge).
    ActiveLow = 1,
    /// Both edges (for edge-triggered) or both levels.
    ActiveBoth = 2,
}

/// GPIO pin configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioPinConfig {
    /// Default configuration (platform-specific).
    Default = 0x00,
    /// Pull-up resistor enabled.
    PullUp = 0x01,
    /// Pull-down resistor enabled.
    PullDown = 0x02,
    /// No I/O connection (pin is not connected).
    NoIo = 0x03,
}

/// GPIO I/O restriction mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioIoRestriction {
    /// No restriction -- pin can be input or output.
    None = 0,
    /// Input only.
    InputOnly = 1,
    /// Output only.
    OutputOnly = 2,
    /// Preserve existing I/O restriction.
    Preserve = 3,
}

/// GPIO Interrupt Connection Descriptor.
///
/// ACPI resource descriptor type 0x8C, connection type 0x00.
/// Used to declare GPIO-based interrupts within a `ResourceTemplate`.
///
/// # Example
///
/// ```ignore
/// use fstart_acpi::descriptors::gpio::*;
///
/// let gpio_int = GpioInt {
///     resource_source: "\\_SB.GPI0",
///     pins: &[42],
///     consumer: true,
///     mode: GpioIntMode::Edge,
///     polarity: GpioIntPolarity::ActiveLow,
///     shared: false,
///     pin_config: GpioPinConfig::PullUp,
///     debounce: 0,
/// };
/// ```
pub struct GpioInt<'a> {
    /// GPIO controller device path (e.g., `"\\_SB.GPI0"`).
    pub resource_source: &'a str,
    /// GPIO pin numbers (typically one pin per interrupt).
    pub pins: &'a [u16],
    /// Resource consumer (`true`) or producer (`false`).
    pub consumer: bool,
    /// Interrupt trigger mode.
    pub mode: GpioIntMode,
    /// Interrupt polarity.
    pub polarity: GpioIntPolarity,
    /// Whether the interrupt is shared with other devices.
    pub shared: bool,
    /// Pin configuration (pull-up, pull-down, etc.).
    pub pin_config: GpioPinConfig,
    /// Debounce timeout in units of 10 microseconds (0 = none).
    pub debounce: u16,
}

impl Aml for GpioInt<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        let pin_table_size = (self.pins.len() * 2) as u16;
        let resource_source_size = (self.resource_source.len() + 1) as u16; // null-terminated

        let resource_source_offset = GPIO_PIN_TABLE_OFFSET + pin_table_size;
        let vendor_data_offset = resource_source_offset + resource_source_size;

        // Length = total descriptor size - 3 (tag + 2-byte length)
        let data_length = vendor_data_offset - 3;

        let general_flags = if self.consumer {
            GeneralFlags::CONSUMER::SET.value
        } else {
            0
        };
        let int_flags = (IntFlags::MODE.val(self.mode as u16)
            + IntFlags::POLARITY.val(self.polarity as u16)
            + IntFlags::SHARED.val(u16::from(self.shared)))
        .value;

        let header = GpioDescriptorHeader {
            tag: GPIO_DESCRIPTOR_TAG,
            data_length: U16::new(data_length),
            revision_id: GPIO_REVISION_ID,
            connection_type: 0x00, // Interrupt
            general_flags: U16::new(general_flags),
            type_flags: U16::new(int_flags),
            pin_config: self.pin_config as u8,
            drive_strength: U16::ZERO, // N/A for interrupt
            debounce: U16::new(self.debounce),
            pin_table_offset: U16::new(GPIO_PIN_TABLE_OFFSET),
            resource_source_index: 0,
            resource_source_offset: U16::new(resource_source_offset),
            vendor_data_offset: U16::new(vendor_data_offset),
            vendor_data_length: U16::ZERO,
        };
        sink.vec(header.as_bytes());

        // Pin Table
        for &pin in self.pins {
            sink.vec(U16::new(pin).as_bytes());
        }

        // Resource Source Name (null-terminated ASCII)
        sink.vec(self.resource_source.as_bytes());
        sink.byte(0);
    }
}

/// GPIO I/O Connection Descriptor.
///
/// ACPI resource descriptor type 0x8C, connection type 0x01.
/// Used to declare GPIO I/O pins within a `ResourceTemplate`.
///
/// # Example
///
/// ```ignore
/// use fstart_acpi::descriptors::gpio::*;
///
/// let gpio_io = GpioIo {
///     resource_source: "\\_SB.GPI0",
///     pins: &[10, 11, 12, 13],
///     consumer: true,
///     io_restriction: GpioIoRestriction::None,
///     pin_config: GpioPinConfig::Default,
///     drive_strength: 0,
///     debounce: 0,
/// };
/// ```
pub struct GpioIo<'a> {
    /// GPIO controller device path (e.g., `"\\_SB.GPI0"`).
    pub resource_source: &'a str,
    /// GPIO pin numbers.
    pub pins: &'a [u16],
    /// Resource consumer (`true`) or producer (`false`).
    pub consumer: bool,
    /// I/O restriction mode.
    pub io_restriction: GpioIoRestriction,
    /// Pin configuration (pull-up, pull-down, etc.).
    pub pin_config: GpioPinConfig,
    /// Output drive strength in units of 10 microamps (0 = default).
    pub drive_strength: u16,
    /// Debounce timeout in units of 10 microseconds (0 = none).
    pub debounce: u16,
}

impl Aml for GpioIo<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        let pin_table_size = (self.pins.len() * 2) as u16;
        let resource_source_size = (self.resource_source.len() + 1) as u16;

        let resource_source_offset = GPIO_PIN_TABLE_OFFSET + pin_table_size;
        let vendor_data_offset = resource_source_offset + resource_source_size;
        let data_length = vendor_data_offset - 3;

        let general_flags = if self.consumer {
            GeneralFlags::CONSUMER::SET.value
        } else {
            0
        };
        let io_flags = IoFlags::IO_RESTRICTION
            .val(self.io_restriction as u16)
            .value;

        let header = GpioDescriptorHeader {
            tag: GPIO_DESCRIPTOR_TAG,
            data_length: U16::new(data_length),
            revision_id: GPIO_REVISION_ID,
            connection_type: 0x01, // I/O
            general_flags: U16::new(general_flags),
            type_flags: U16::new(io_flags),
            pin_config: self.pin_config as u8,
            drive_strength: U16::new(self.drive_strength),
            debounce: U16::new(self.debounce),
            pin_table_offset: U16::new(GPIO_PIN_TABLE_OFFSET),
            resource_source_index: 0,
            resource_source_offset: U16::new(resource_source_offset),
            vendor_data_offset: U16::new(vendor_data_offset),
            vendor_data_length: U16::ZERO,
        };
        sink.vec(header.as_bytes());

        for &pin in self.pins {
            sink.vec(U16::new(pin).as_bytes());
        }

        sink.vec(self.resource_source.as_bytes());
        sink.byte(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    extern crate alloc;

    #[test]
    fn test_gpio_int_descriptor() {
        let gpio = GpioInt {
            resource_source: "\\_SB.GPI0",
            pins: &[42],
            consumer: true,
            mode: GpioIntMode::Edge,
            polarity: GpioIntPolarity::ActiveLow,
            shared: false,
            pin_config: GpioPinConfig::PullUp,
            debounce: 0,
        };

        let mut bytes = Vec::new();
        gpio.to_aml_bytes(&mut bytes);

        // Tag
        assert_eq!(bytes[0], GPIO_DESCRIPTOR_TAG);

        // Revision
        assert_eq!(bytes[3], GPIO_REVISION_ID);

        // Connection type: interrupt
        assert_eq!(bytes[4], 0x00);

        // General flags: consumer = bit 0 set
        let gf = u16::from_le_bytes([bytes[5], bytes[6]]);
        assert_eq!(gf & 1, 1);

        // Interrupt flags: edge (bit 0), active-low (bits 1-2 = 0b01),
        // exclusive (bit 3)
        assert_eq!(u16::from_le_bytes([bytes[7], bytes[8]]), 0b011);

        // Pin config: pull-up
        assert_eq!(bytes[9], GpioPinConfig::PullUp as u8);

        // Pin table offset, right after the 23-byte header
        let pto = u16::from_le_bytes([bytes[14], bytes[15]]);
        assert_eq!(pto, 23);
        assert_eq!(pto, GPIO_PIN_TABLE_OFFSET);

        // Pin value at pin table offset
        let pin = u16::from_le_bytes([bytes[pto as usize], bytes[pto as usize + 1]]);
        assert_eq!(pin, 42);

        // Resource source should be present after pin table
        let rs_offset = u16::from_le_bytes([bytes[17], bytes[18]]) as usize;
        let rs_end = bytes[rs_offset..].iter().position(|&b| b == 0).unwrap() + rs_offset;
        let rs_name = core::str::from_utf8(&bytes[rs_offset..rs_end]).unwrap();
        assert_eq!(rs_name, "\\_SB.GPI0");
    }

    #[test]
    fn test_gpio_io_descriptor() {
        let gpio = GpioIo {
            resource_source: "\\_SB.GPI0",
            pins: &[10, 11],
            consumer: true,
            io_restriction: GpioIoRestriction::None,
            pin_config: GpioPinConfig::Default,
            drive_strength: 0,
            debounce: 0,
        };

        let mut bytes = Vec::new();
        gpio.to_aml_bytes(&mut bytes);

        // Tag
        assert_eq!(bytes[0], GPIO_DESCRIPTOR_TAG);

        // Connection type: I/O
        assert_eq!(bytes[4], 0x01);

        // Should have 2 pins at offset 23
        let pin0 = u16::from_le_bytes([bytes[23], bytes[24]]);
        let pin1 = u16::from_le_bytes([bytes[25], bytes[26]]);
        assert_eq!(pin0, 10);
        assert_eq!(pin1, 11);
    }

    #[test]
    fn test_gpio_int_length() {
        let gpio = GpioInt {
            resource_source: "GPI0",
            pins: &[0],
            consumer: false,
            mode: GpioIntMode::Level,
            polarity: GpioIntPolarity::ActiveHigh,
            shared: false,
            pin_config: GpioPinConfig::Default,
            debounce: 0,
        };

        let mut bytes = Vec::new();
        gpio.to_aml_bytes(&mut bytes);

        // Verify length field matches actual data
        let data_len = u16::from_le_bytes([bytes[1], bytes[2]]) as usize;
        assert_eq!(data_len + 3, bytes.len());
    }
}
