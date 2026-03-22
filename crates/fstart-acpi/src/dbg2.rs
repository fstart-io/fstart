//! DBG2 (Debug Port Table 2) builder.
//!
//! Microsoft Debug Port Table 2 specification.  Describes the debug
//! port hardware available for early boot debugging.  Paired with
//! SPCR for the console port; DBG2 adds structured device info with
//! ACPI namespace linkage and port type/subtype classification.
//!
//! Required by SBSA for ARM server platforms.

use acpi_tables::sdt::Sdt;

// ---------------------------------------------------------------------------
// Wire-format structs — #[repr(C, packed)] mirrors the DBG2 spec layout.
// ---------------------------------------------------------------------------

/// DBG2 fields after the 36-byte ACPI SDT header.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Dbg2Header {
    devices_offset: u32,
    devices_count: u32,
}

/// DBG2 debug device info fixed structure (22 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct DeviceInfoFixed {
    revision: u8,
    length: u16,
    address_count: u8,
    namespace_string_length: u16,
    namespace_string_offset: u16,
    oem_data_length: u16,
    oem_data_offset: u16,
    port_type: u16,
    port_subtype: u16,
    _reserved: u16,
    base_address_offset: u16,
    address_size_offset: u16,
}

/// ACPI Generic Address Structure (GAS, 12 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Gas {
    space_id: u8,
    bit_width: u8,
    bit_offset: u8,
    access_size: u8,
    address: u64,
}

const ACPI_HDR: usize = 36;
const DBG2_HEADER_SIZE: usize = ACPI_HDR + core::mem::size_of::<Dbg2Header>();
const DEVICE_INFO_FIXED_SIZE: usize = core::mem::size_of::<DeviceInfoFixed>();
const GAS_SIZE: usize = core::mem::size_of::<Gas>();
const ADDR_SIZE_FIELD: usize = 4;

/// Port types.
#[allow(dead_code)]
mod port_type {
    pub const SERIAL: u16 = 0x8000;
    pub const IEEE1394: u16 = 0x8001;
    pub const USB: u16 = 0x8002;
    pub const NET: u16 = 0x8003;
}

/// Serial port subtypes.
#[allow(dead_code)]
mod serial_subtype {
    pub const FULL_16550_IO: u16 = 0x0000;
    pub const FULL_16550_DBGP: u16 = 0x0001;
    pub const ARM_PL011: u16 = 0x0003;
    pub const ARM_SBSA_GENERIC: u16 = 0x000E;
    pub const FULL_16550: u16 = 0x0012;
}

/// DBG2 PL011 configuration.
pub struct Dbg2Pl011Config<'a> {
    /// Physical MMIO base address of the PL011 peripheral.
    pub base_addr: u64,
    /// Size of the MMIO region in bytes (e.g., 0x1000).
    pub addr_size: u32,
    /// ACPI namespace path (e.g., `"\\_SB.COM0"`).
    pub namespace: &'a str,
}

/// Build a DBG2 table for a PL011 UART debug port.
pub fn build_dbg2_pl011(config: &Dbg2Pl011Config<'_>) -> Sdt {
    let ns_bytes = config.namespace.as_bytes();
    let ns_len = ns_bytes.len() + 1; // include NUL terminator

    // Device info total length: fixed struct + GAS + address_size + namespace.
    let device_info_len = DEVICE_INFO_FIXED_SIZE + GAS_SIZE + ADDR_SIZE_FIELD + ns_len;

    let total_size = DBG2_HEADER_SIZE + device_info_len;

    let mut sdt = Sdt::new(
        *b"DBG2",
        total_size as u32,
        0, // DBG2 revision 0
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );

    // DBG2 header.
    crate::write_struct(
        &mut sdt,
        ACPI_HDR,
        &Dbg2Header {
            devices_offset: DBG2_HEADER_SIZE as u32,
            devices_count: 1,
        },
    );

    // Offsets relative to the start of the device info structure.
    let base_addr_off = DEVICE_INFO_FIXED_SIZE as u16; // 22
    let addr_size_off = base_addr_off + GAS_SIZE as u16; // 34
    let ns_off = addr_size_off + ADDR_SIZE_FIELD as u16; // 38

    let dev_off = DBG2_HEADER_SIZE;

    // Debug device info (fixed portion).
    crate::write_struct(
        &mut sdt,
        dev_off,
        &DeviceInfoFixed {
            revision: 0,
            length: device_info_len as u16,
            address_count: 1,
            namespace_string_length: ns_len as u16,
            namespace_string_offset: ns_off,
            oem_data_length: 0,
            oem_data_offset: 0,
            port_type: port_type::SERIAL,
            port_subtype: serial_subtype::ARM_PL011,
            _reserved: 0,
            base_address_offset: base_addr_off,
            address_size_offset: addr_size_off,
        },
    );

    // GAS (Generic Address Structure) for the MMIO base address.
    crate::write_struct(
        &mut sdt,
        dev_off + base_addr_off as usize,
        &Gas {
            space_id: 0, // SystemMemory
            bit_width: 32,
            bit_offset: 0,
            access_size: 3, // DWord
            address: config.base_addr,
        },
    );

    // Address size (u32).
    sdt.write_u32(dev_off + addr_size_off as usize, config.addr_size);

    // Namespace string (NUL-terminated).
    let ns_abs = dev_off + ns_off as usize;
    for (i, &b) in ns_bytes.iter().enumerate() {
        sdt.write_u8(ns_abs + i, b);
    }
    // NUL terminator is already zero from Sdt initialization.

    sdt.update_checksum();
    sdt
}

#[cfg(test)]
mod tests {
    use super::*;
    use acpi_tables::Aml;
    use alloc::vec::Vec;

    #[test]
    fn test_dbg2_pl011() {
        let dbg2 = build_dbg2_pl011(&Dbg2Pl011Config {
            base_addr: 0x6000_0000,
            addr_size: 0x1000,
            namespace: "\\_SB.COM0",
        });

        let mut bytes = Vec::new();
        dbg2.to_aml_bytes(&mut bytes);

        // Verify checksum.
        let sum = bytes.iter().fold(0u8, |acc, &x| acc.wrapping_add(x));
        assert_eq!(sum, 0, "DBG2 checksum failed");

        // Verify signature.
        assert_eq!(&bytes[0..4], b"DBG2");

        // Verify revision = 0.
        assert_eq!(bytes[8], 0);

        // Verify devices_count = 1.
        assert_eq!(
            u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            1,
            "devices_count"
        );

        // Device info at offset 44.
        let dev = 44;

        // port_type = 0x8000 (SERIAL)
        assert_eq!(
            u16::from_le_bytes(bytes[dev + 12..dev + 14].try_into().unwrap()),
            0x8000,
            "port_type"
        );

        // port_subtype = 0x0003 (PL011)
        assert_eq!(
            u16::from_le_bytes(bytes[dev + 14..dev + 16].try_into().unwrap()),
            0x0003,
            "port_subtype"
        );

        // GAS base address at dev + 22 + 4 = dev + 26 (offset within GAS).
        let gas = dev + 22;
        assert_eq!(bytes[gas], 0, "GAS space_id = SystemMemory");
        let addr = u64::from_le_bytes(bytes[gas + 4..gas + 12].try_into().unwrap());
        assert_eq!(addr, 0x6000_0000, "GAS address");

        // Address size at dev + 34.
        let addr_sz = u32::from_le_bytes(bytes[dev + 34..dev + 38].try_into().unwrap());
        assert_eq!(addr_sz, 0x1000, "address_size");

        // Namespace string at dev + 38.
        let ns_start = dev + 38;
        let ns_end = bytes[ns_start..].iter().position(|&b| b == 0).unwrap() + ns_start;
        let ns = core::str::from_utf8(&bytes[ns_start..ns_end]).unwrap();
        assert_eq!(ns, "\\_SB.COM0", "namespace string");
    }

    #[test]
    fn test_dbg2_short_namespace() {
        let dbg2 = build_dbg2_pl011(&Dbg2Pl011Config {
            base_addr: 0x0900_0000,
            addr_size: 0x1000,
            namespace: ".",
        });

        let mut bytes = Vec::new();
        dbg2.to_aml_bytes(&mut bytes);

        let sum = bytes.iter().fold(0u8, |acc, &x| acc.wrapping_add(x));
        assert_eq!(sum, 0, "DBG2 checksum failed");

        // Namespace length = 2 (dot + NUL).
        let dev = 44;
        let ns_len = u16::from_le_bytes(bytes[dev + 4..dev + 6].try_into().unwrap());
        assert_eq!(ns_len, 2, "namespace_string_length");
    }
}
