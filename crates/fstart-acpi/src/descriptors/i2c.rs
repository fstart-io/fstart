//! I2C Serial Bus Connection Descriptor for ACPI Resource Templates.
//!
//! Implements [`I2cSerialBus`] as an [`Aml`] type for use inside
//! `ResourceTemplate`, declaring I2C device connections.
//!
//! # ACPI specification reference
//!
//! Section 6.4.3.8.2 -- Serial Bus Connection Descriptors (Type 0x8E),
//! I2C Serial Bus subtype (0x01).
//!
//! [`Aml`]: acpi_tables::Aml

use acpi_tables::{Aml, AmlSink};

/// Serial Bus Connection Descriptor tag byte.
const SERIAL_BUS_DESCRIPTOR_TAG: u8 = 0x8E;

/// Descriptor revision ID.
const SERIAL_BUS_REVISION_ID: u8 = 0x01;

/// I2C serial bus type code.
const I2C_SERIAL_BUS_TYPE: u8 = 0x01;

/// I2C type-specific data length (connection speed + slave address).
const I2C_TYPE_DATA_LENGTH: u16 = 6;

/// I2C Serial Bus Connection Descriptor.
///
/// Declares an I2C device connection within a `ResourceTemplate`.
/// The descriptor identifies the I2C controller, slave address,
/// and bus speed.
///
/// # Example
///
/// ```ignore
/// use fstart_acpi::descriptors::i2c::I2cSerialBus;
///
/// let i2c_dev = I2cSerialBus {
///     resource_source: "\\_SB.I2C0",
///     slave_address: 0x50,
///     connection_speed: 400_000,  // 400 kHz fast mode
///     address_10bit: false,
///     consumer: true,
/// };
/// ```
pub struct I2cSerialBus<'a> {
    /// I2C controller device path (e.g., `"\\_SB.I2C0"`).
    pub resource_source: &'a str,
    /// Slave address (7-bit or 10-bit).
    pub slave_address: u16,
    /// Connection speed in Hz (e.g., 100000 for standard, 400000 for fast).
    pub connection_speed: u32,
    /// Use 10-bit addressing mode (false = 7-bit).
    pub address_10bit: bool,
    /// Resource consumer (`true`) or producer (`false`).
    pub consumer: bool,
}

impl Aml for I2cSerialBus<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        // Common header: 12 bytes (offset 0-11)
        //   0: tag, 1-2: length, 3: rev, 4: res_src_idx, 5: bus_type,
        //   6: general_flags, 7-8: type_flags, 9: type_rev, 10-11: type_data_len
        // Type-specific data: 6 bytes (speed + slave_addr)
        // Resource source: len + 1 (null terminator)
        let resource_source_size = self.resource_source.len() + 1;

        // Length = bytes after {tag, length[0], length[1]}
        //        = common_header_remainder(9) + type_data(6) + resource_source
        let data_length = 9 + I2C_TYPE_DATA_LENGTH as usize + resource_source_size;

        // General Flags: bit 1 = consumer/producer (bit 0 = slave mode)
        let general_flags: u8 = if self.consumer { 0x02 } else { 0x00 };

        // Type-Specific Flags: bit 0 = 10-bit addressing
        let type_flags: u16 = if self.address_10bit { 1 } else { 0 };

        // Common header
        sink.byte(SERIAL_BUS_DESCRIPTOR_TAG);
        sink.word(data_length as u16);
        sink.byte(SERIAL_BUS_REVISION_ID);
        sink.byte(0x00); // Resource Source Index
        sink.byte(I2C_SERIAL_BUS_TYPE);
        sink.byte(general_flags);
        sink.word(type_flags);
        sink.byte(0x01); // Type-Specific Revision ID
        sink.word(I2C_TYPE_DATA_LENGTH);

        // Type-specific data: I2C
        sink.dword(self.connection_speed);
        sink.word(self.slave_address);

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
    fn test_i2c_descriptor() {
        let i2c = I2cSerialBus {
            resource_source: "\\_SB.I2C0",
            slave_address: 0x50,
            connection_speed: 400_000,
            address_10bit: false,
            consumer: true,
        };

        let mut bytes = Vec::new();
        i2c.to_aml_bytes(&mut bytes);

        // Tag
        assert_eq!(bytes[0], SERIAL_BUS_DESCRIPTOR_TAG);

        // Revision
        assert_eq!(bytes[3], SERIAL_BUS_REVISION_ID);

        // Bus type: I2C
        assert_eq!(bytes[5], I2C_SERIAL_BUS_TYPE);

        // General flags: consumer (bit 1)
        assert_eq!(bytes[6] & 0x02, 0x02);

        // Type flags: 7-bit addressing
        let tf = u16::from_le_bytes([bytes[7], bytes[8]]);
        assert_eq!(tf, 0);

        // Connection speed
        let speed = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        assert_eq!(speed, 400_000);

        // Slave address
        let addr = u16::from_le_bytes([bytes[16], bytes[17]]);
        assert_eq!(addr, 0x50);

        // Resource source name
        let rs_start = 18;
        let rs_end = bytes[rs_start..].iter().position(|&b| b == 0).unwrap() + rs_start;
        let name = core::str::from_utf8(&bytes[rs_start..rs_end]).unwrap();
        assert_eq!(name, "\\_SB.I2C0");

        // Verify length
        let data_len = u16::from_le_bytes([bytes[1], bytes[2]]) as usize;
        assert_eq!(data_len + 3, bytes.len());
    }

    #[test]
    fn test_i2c_10bit_address() {
        let i2c = I2cSerialBus {
            resource_source: "I2C0",
            slave_address: 0x350,
            connection_speed: 100_000,
            address_10bit: true,
            consumer: false,
        };

        let mut bytes = Vec::new();
        i2c.to_aml_bytes(&mut bytes);

        // Type flags: 10-bit addressing (bit 0)
        let tf = u16::from_le_bytes([bytes[7], bytes[8]]);
        assert_eq!(tf, 1);

        // Slave address
        let addr = u16::from_le_bytes([bytes[16], bytes[17]]);
        assert_eq!(addr, 0x350);
    }
}
