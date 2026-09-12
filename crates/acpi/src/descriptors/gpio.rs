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

/// GPIO Connection Descriptor tag byte.
const GPIO_DESCRIPTOR_TAG: u8 = 0x8C;

/// GPIO Connection Descriptor revision.
const GPIO_REVISION_ID: u8 = 0x01;

/// Byte offset of the Pin Table from the descriptor start.
const GPIO_PIN_TABLE_OFFSET: u16 = 22;

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

        // General Flags: bit 0 = consumer/producer
        let general_flags: u16 = if self.consumer { 1 } else { 0 };

        // Interrupt flags:
        //   Bits 0-1: Mode (level/edge)
        //   Bits 2-3: Polarity
        //   Bit 4: Sharing
        let int_flags: u8 = (self.mode as u8)
            | ((self.polarity as u8) << 2)
            | (if self.shared { 1 << 4 } else { 0 });

        // Header
        sink.byte(GPIO_DESCRIPTOR_TAG);
        sink.word(data_length);
        sink.byte(GPIO_REVISION_ID);
        sink.byte(0x00); // Connection Type: Interrupt
        sink.word(general_flags);
        sink.byte(int_flags);
        sink.byte(self.pin_config as u8);
        sink.word(0); // Output Drive Strength (N/A for interrupt)
        sink.word(self.debounce);
        sink.word(GPIO_PIN_TABLE_OFFSET);
        sink.byte(0); // Resource Source Index
        sink.word(resource_source_offset);
        sink.word(vendor_data_offset);
        sink.word(0); // Vendor Data Length

        // Pin Table
        for &pin in self.pins {
            sink.word(pin);
        }

        // Resource Source Name (null-terminated ASCII)
        for &b in self.resource_source.as_bytes() {
            sink.byte(b);
        }
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

        let general_flags: u16 = if self.consumer { 1 } else { 0 };

        // I/O flags: bits 0-1 = IO restriction
        let io_flags: u8 = self.io_restriction as u8;

        sink.byte(GPIO_DESCRIPTOR_TAG);
        sink.word(data_length);
        sink.byte(GPIO_REVISION_ID);
        sink.byte(0x01); // Connection Type: I/O
        sink.word(general_flags);
        sink.byte(io_flags);
        sink.byte(self.pin_config as u8);
        sink.word(self.drive_strength);
        sink.word(self.debounce);
        sink.word(GPIO_PIN_TABLE_OFFSET);
        sink.byte(0); // Resource Source Index
        sink.word(resource_source_offset);
        sink.word(vendor_data_offset);
        sink.word(0); // Vendor Data Length

        for &pin in self.pins {
            sink.word(pin);
        }

        for &b in self.resource_source.as_bytes() {
            sink.byte(b);
        }
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

        // Interrupt flags: edge (bit 0) | active-low (bit 2-3 = 0b01)
        assert_eq!(bytes[7] & 0x03, 0x01); // edge
        assert_eq!((bytes[7] >> 2) & 0x03, 0x01); // active-low

        // Pin config: pull-up
        assert_eq!(bytes[8], GpioPinConfig::PullUp as u8);

        // Pin table offset
        let pto = u16::from_le_bytes([bytes[13], bytes[14]]);
        assert_eq!(pto, GPIO_PIN_TABLE_OFFSET);

        // Pin value at pin table offset
        let pin = u16::from_le_bytes([bytes[pto as usize], bytes[pto as usize + 1]]);
        assert_eq!(pin, 42);

        // Resource source should be present after pin table
        let rs_offset = u16::from_le_bytes([bytes[16], bytes[17]]) as usize;
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

        // Should have 2 pins at offset 22
        let pin0 = u16::from_le_bytes([bytes[22], bytes[23]]);
        let pin1 = u16::from_le_bytes([bytes[24], bytes[25]]);
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
