//! SPCR (Serial Port Console Redirection) table for PL011 UART.
//!
//! ACPI 6.5 / Microsoft SPCR spec. The upstream `acpi_tables` crate
//! only provides an SBI (RISC-V) constructor; this module adds a
//! PL011 constructor for ARM SBSA platforms.

use acpi_tables::sdt::Sdt;

/// SPCR interface subtypes (ACPI DBG2 / SPCR spec).
#[allow(dead_code)]
mod subtype {
    pub const FULLY_16550: u8 = 0;
    pub const ARM_PL011: u8 = 3;
    pub const ARM_SBSA_GENERIC: u8 = 0x0e;
}

/// SPCR baud rate encoding.
#[allow(dead_code)]
mod baud {
    pub const B9600: u8 = 3;
    pub const B19200: u8 = 4;
    pub const B57600: u8 = 6;
    pub const B115200: u8 = 7;
}

/// Build an SPCR table for an ARM PL011 UART.
///
/// # Arguments
///
/// * `base_addr` — Physical MMIO base address of the PL011 peripheral.
/// * `gsiv` — GIC System Interrupt Vector for the UART.
pub fn build_spcr_pl011(base_addr: u64, gsiv: u32) -> Sdt {
    // Layout: 36-byte header + 52-byte serial port info + 2-byte namespace.
    let total_size: u32 = 36 + 52 + 2;

    let mut sdt = Sdt::new(
        *b"SPCR",
        total_size,
        4, // SPCR revision 4
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );

    // Serial Port Info fields (offset 36..88).
    sdt.write_u8(36, subtype::ARM_PL011); // Interface Type
                                          // 37..39: Reserved (already zero)

    // Generic Address Structure for base address (12 bytes at offset 40).
    sdt.write_u8(40, 0); // AddressSpaceId: SystemMemory
    sdt.write_u8(41, 8); // RegisterBitWidth
    sdt.write_u8(42, 0); // RegisterBitOffset
    sdt.write_u8(43, 1); // AccessSize: Byte
    sdt.write_u64(44, base_addr);

    // Interrupt configuration.
    sdt.write_u8(52, 0x08); // InterruptType: ARM GIC interrupt
    sdt.write_u8(53, 0); // IRQ: 0 (using GSI instead)
    sdt.write_u32(54, gsiv); // GlobalSystemInterrupt

    // Serial parameters.
    sdt.write_u8(58, baud::B115200); // BaudRate
    sdt.write_u8(59, 0); // Parity: None
    sdt.write_u8(60, 1); // StopBits: 1
    sdt.write_u8(61, 0); // FlowControl: None
    sdt.write_u8(62, 0); // TerminalType: VT100
    sdt.write_u8(63, 0); // Language

    // PCI identification (0xFFFF = not PCI).
    sdt.write_u16(64, 0xFFFF); // PciDeviceId
    sdt.write_u16(66, 0xFFFF); // PciVendorId
                               // 68..70: PciBus/Device/Function (zero)
                               // 71..74: PciFlags (zero)
                               // 75: PciSegment (zero)
                               // 76..79: ClockFrequency (zero, not specified)
                               // 80..83: PreciseBaudRate (zero, not specified)

    // Namespace string: ".\0" (minimal, meaning root namespace).
    sdt.write_u16(84, 2); // NamespaceStringLength
    sdt.write_u16(86, 52); // NamespaceStringOffset (from info start)
    sdt.write_u8(88, b'.'); // Namespace string
                            // 89: NUL terminator (already zero)

    sdt.update_checksum();
    sdt
}

#[cfg(test)]
mod tests {
    use super::*;
    use acpi_tables::Aml;
    use alloc::vec::Vec;

    #[test]
    fn test_spcr_pl011() {
        let spcr = build_spcr_pl011(0x6000_0000, 33);

        let mut bytes = Vec::new();
        spcr.to_aml_bytes(&mut bytes);

        // Verify checksum
        let sum = bytes.iter().fold(0u8, |acc, x| acc.wrapping_add(*x));
        assert_eq!(sum, 0, "SPCR checksum failed");

        // Verify size
        assert_eq!(bytes.len(), 90);

        // Verify signature
        assert_eq!(&bytes[0..4], b"SPCR");

        // Verify interface type = PL011
        assert_eq!(bytes[36], 3);

        // Verify base address
        assert_eq!(
            u64::from_le_bytes(bytes[44..52].try_into().unwrap()),
            0x6000_0000
        );

        // Verify GSI
        assert_eq!(u32::from_le_bytes(bytes[54..58].try_into().unwrap()), 33);

        // Verify baud rate = 115200
        assert_eq!(bytes[58], 7);
    }
}
