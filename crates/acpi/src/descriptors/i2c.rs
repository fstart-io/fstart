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
use tock_registers::register_bitfields;
use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Serial Bus Connection Descriptor tag byte.
const SERIAL_BUS_DESCRIPTOR_TAG: u8 = 0x8E;

/// Descriptor revision ID.
const SERIAL_BUS_REVISION_ID: u8 = 0x01;

/// I2C serial bus type code.
const I2C_SERIAL_BUS_TYPE: u8 = 0x01;

/// I2C type-specific data length (connection speed + slave address).
const I2C_TYPE_DATA_LENGTH: u16 = 6;

register_bitfields![u8,
    /// Serial Bus General Flags (offset 0x06, 8 bits).
    GeneralFlags [
        SLAVE_MODE OFFSET(0) NUMBITS(1) [],
        CONSUMER OFFSET(1) NUMBITS(1) []
    ]
];

register_bitfields![u16,
    /// I2C Type-Specific Flags (offset 0x07, 16 bits).
    I2cTypeFlags [
        ADDRESS_10BIT OFFSET(0) NUMBITS(1) []
    ]
];

/// Common Serial Bus Connection header + I2C type-specific data (18 bytes).
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct I2cSerialBusHeader {
    tag: u8,
    data_length: U16,
    revision_id: u8,
    resource_source_index: u8,
    serial_bus_type: u8,
    general_flags: u8,
    type_flags: U16,
    type_revision_id: u8,
    type_data_length: U16,
    connection_speed: U32,
    slave_address: U16,
}

const _: () = assert!(core::mem::size_of::<I2cSerialBusHeader>() == 18);

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

        let general_flags = if self.consumer {
            GeneralFlags::CONSUMER::SET.value
        } else {
            0
        };
        let type_flags = if self.address_10bit {
            I2cTypeFlags::ADDRESS_10BIT::SET.value
        } else {
            0
        };

        let header = I2cSerialBusHeader {
            tag: SERIAL_BUS_DESCRIPTOR_TAG,
            data_length: U16::new(data_length as u16),
            revision_id: SERIAL_BUS_REVISION_ID,
            resource_source_index: 0,
            serial_bus_type: I2C_SERIAL_BUS_TYPE,
            general_flags,
            type_flags: U16::new(type_flags),
            type_revision_id: 0x01,
            type_data_length: U16::new(I2C_TYPE_DATA_LENGTH),
            connection_speed: U32::new(self.connection_speed),
            slave_address: U16::new(self.slave_address),
        };
        sink.vec(header.as_bytes());

        // Resource Source Name (null-terminated ASCII)
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
