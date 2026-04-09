//! SPI Serial Bus Connection Descriptor for ACPI Resource Templates.
//!
//! Implements [`SpiSerialBus`] as an [`Aml`] type for use inside
//! `ResourceTemplate`, declaring SPI device connections.
//!
//! # ACPI specification reference
//!
//! Section 6.4.3.8.2 -- Serial Bus Connection Descriptors (Type 0x8E),
//! SPI Serial Bus subtype (0x02).
//!
//! [`Aml`]: acpi_tables::Aml

use acpi_tables::{Aml, AmlSink};

/// Serial Bus Connection Descriptor tag byte.
const SERIAL_BUS_DESCRIPTOR_TAG: u8 = 0x8E;

/// Descriptor revision ID.
const SERIAL_BUS_REVISION_ID: u8 = 0x01;

/// SPI serial bus type code.
const SPI_SERIAL_BUS_TYPE: u8 = 0x02;

/// SPI type-specific data length:
/// connection_speed(4) + data_bit_length(1) + clock_phase(1)
/// + clock_polarity(1) + device_selection(2) = 9 bytes.
const SPI_TYPE_DATA_LENGTH: u16 = 9;

/// SPI clock phase (CPHA).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpiClockPhase {
    /// Data captured on leading clock edge, changed on trailing edge.
    First = 0,
    /// Data captured on trailing clock edge, changed on leading edge.
    Second = 1,
}

/// SPI clock polarity (CPOL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpiClockPolarity {
    /// Clock idle low (CPOL=0).
    Low = 0,
    /// Clock idle high (CPOL=1).
    High = 1,
}

/// SPI wire mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpiWireMode {
    /// Standard four-wire SPI (MOSI, MISO, CLK, CS).
    FourWire = 0,
    /// Three-wire SPI (shared data line).
    ThreeWire = 1,
}

/// SPI chip-select polarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpiDevicePolarity {
    /// Chip select is active-low (most common).
    ActiveLow = 0,
    /// Chip select is active-high.
    ActiveHigh = 1,
}

/// SPI Serial Bus Connection Descriptor.
///
/// Declares an SPI device connection within a `ResourceTemplate`.
/// The descriptor identifies the SPI controller, chip-select,
/// clock parameters, and bus speed.
///
/// # Example
///
/// ```ignore
/// use fstart_acpi::descriptors::spi::*;
///
/// let spi_dev = SpiSerialBus {
///     resource_source: "\\_SB.SPI0",
///     connection_speed: 10_000_000,  // 10 MHz
///     data_bit_length: 8,
///     clock_phase: SpiClockPhase::First,
///     clock_polarity: SpiClockPolarity::Low,
///     wire_mode: SpiWireMode::FourWire,
///     device_polarity: SpiDevicePolarity::ActiveLow,
///     device_selection: 0,
///     consumer: true,
/// };
/// ```
pub struct SpiSerialBus<'a> {
    /// SPI controller device path (e.g., `"\\_SB.SPI0"`).
    pub resource_source: &'a str,
    /// Connection speed in Hz (e.g., 10_000_000 for 10 MHz).
    pub connection_speed: u32,
    /// Data bit length per transfer (typically 8).
    pub data_bit_length: u8,
    /// SPI clock phase (CPHA).
    pub clock_phase: SpiClockPhase,
    /// SPI clock polarity (CPOL).
    pub clock_polarity: SpiClockPolarity,
    /// Wire mode (3-wire or 4-wire).
    pub wire_mode: SpiWireMode,
    /// Chip-select polarity.
    pub device_polarity: SpiDevicePolarity,
    /// Device chip-select number (0-based).
    pub device_selection: u16,
    /// Resource consumer (`true`) or producer (`false`).
    pub consumer: bool,
}

impl Aml for SpiSerialBus<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        let resource_source_size = self.resource_source.len() + 1;
        let data_length = 9 + SPI_TYPE_DATA_LENGTH as usize + resource_source_size;

        let general_flags: u8 = if self.consumer { 0x02 } else { 0x00 };

        // Type-Specific Flags:
        //   Bit 0: Wire Mode (0=4-wire, 1=3-wire)
        //   Bit 1: Device Polarity (0=active-low, 1=active-high)
        let type_flags: u16 = (self.wire_mode as u16) | ((self.device_polarity as u16) << 1);

        // Common header
        sink.byte(SERIAL_BUS_DESCRIPTOR_TAG);
        sink.word(data_length as u16);
        sink.byte(SERIAL_BUS_REVISION_ID);
        sink.byte(0x00); // Resource Source Index
        sink.byte(SPI_SERIAL_BUS_TYPE);
        sink.byte(general_flags);
        sink.word(type_flags);
        sink.byte(0x01); // Type-Specific Revision ID
        sink.word(SPI_TYPE_DATA_LENGTH);

        // Type-specific data: SPI
        sink.dword(self.connection_speed);
        sink.byte(self.data_bit_length);
        sink.byte(self.clock_phase as u8);
        sink.byte(self.clock_polarity as u8);
        sink.word(self.device_selection);

        // Resource Source Name (null-terminated ASCII)
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
    fn test_spi_descriptor() {
        let spi = SpiSerialBus {
            resource_source: "\\_SB.SPI0",
            connection_speed: 10_000_000,
            data_bit_length: 8,
            clock_phase: SpiClockPhase::First,
            clock_polarity: SpiClockPolarity::Low,
            wire_mode: SpiWireMode::FourWire,
            device_polarity: SpiDevicePolarity::ActiveLow,
            device_selection: 0,
            consumer: true,
        };

        let mut bytes = Vec::new();
        spi.to_aml_bytes(&mut bytes);

        // Tag
        assert_eq!(bytes[0], SERIAL_BUS_DESCRIPTOR_TAG);

        // Revision
        assert_eq!(bytes[3], SERIAL_BUS_REVISION_ID);

        // Bus type: SPI
        assert_eq!(bytes[5], SPI_SERIAL_BUS_TYPE);

        // General flags: consumer
        assert_eq!(bytes[6] & 0x02, 0x02);

        // Type flags: 4-wire, active-low CS
        let tf = u16::from_le_bytes([bytes[7], bytes[8]]);
        assert_eq!(tf, 0);

        // Connection speed at offset 12
        let speed = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        assert_eq!(speed, 10_000_000);

        // Data bit length
        assert_eq!(bytes[16], 8);

        // Clock phase
        assert_eq!(bytes[17], SpiClockPhase::First as u8);

        // Clock polarity
        assert_eq!(bytes[18], SpiClockPolarity::Low as u8);

        // Device selection
        let ds = u16::from_le_bytes([bytes[19], bytes[20]]);
        assert_eq!(ds, 0);

        // Verify length
        let data_len = u16::from_le_bytes([bytes[1], bytes[2]]) as usize;
        assert_eq!(data_len + 3, bytes.len());
    }

    #[test]
    fn test_spi_3wire_active_high() {
        let spi = SpiSerialBus {
            resource_source: "SPI0",
            connection_speed: 1_000_000,
            data_bit_length: 16,
            clock_phase: SpiClockPhase::Second,
            clock_polarity: SpiClockPolarity::High,
            wire_mode: SpiWireMode::ThreeWire,
            device_polarity: SpiDevicePolarity::ActiveHigh,
            device_selection: 1,
            consumer: false,
        };

        let mut bytes = Vec::new();
        spi.to_aml_bytes(&mut bytes);

        // Type flags: 3-wire (bit 0) | active-high (bit 1)
        let tf = u16::from_le_bytes([bytes[7], bytes[8]]);
        assert_eq!(tf, 0b11);

        // Clock phase = Second
        assert_eq!(bytes[17], SpiClockPhase::Second as u8);

        // Clock polarity = High
        assert_eq!(bytes[18], SpiClockPolarity::High as u8);

        // Device selection = 1
        let ds = u16::from_le_bytes([bytes[19], bytes[20]]);
        assert_eq!(ds, 1);
    }
}
