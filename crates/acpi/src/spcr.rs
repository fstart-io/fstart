//! SPCR (Serial Port Console Redirection) table: an ARM PL011 or a 16550
//! in I/O space.
//!
//! ACPI 6.5 / Microsoft SPCR spec. The upstream `acpi_tables` crate
//! only provides an SBI (RISC-V) constructor.

use acpi_tables::sdt::Sdt;
use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::WireGas;

/// SPCR revision 4 body after the 36-byte ACPI SDT header (52 bytes) + 2-byte ".\0" namespace.
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
struct SpcrBody {
    interface_type: u8,
    reserved: [u8; 3],
    base_address: WireGas,
    interrupt_type: u8,
    irq: u8,
    global_system_interrupt: U32,
    baud_rate: u8,
    parity: u8,
    stop_bits: u8,
    flow_control: u8,
    terminal_type: u8,
    language: u8,
    pci_device_id: U16,
    pci_vendor_id: U16,
    pci_bus: u8,
    pci_device: u8,
    pci_function: u8,
    pci_flags: U32,
    pci_segment: u8,
    clock_frequency: U32,
    precise_baud_rate: U32,
    namespace_string_length: U16,
    namespace_string_offset: U16,
    namespace: [u8; 2],
}

const _: () = assert!(core::mem::size_of::<SpcrBody>() == 54);

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
    // SystemMemory, 8 bits, byte access; ARM GIC interrupt.
    build(subtype::ARM_PL011, (0, base_addr), (0x08, 0, gsiv))
}

/// Build an SPCR table for a 16550 at I/O port `port` on ISA IRQ `irq`
/// (dual 8259 and I/O APIC interrupt, GSI = IRQ), 115200 8N1.
pub fn build_spcr_16550_io(port: u16, irq: u8) -> Sdt {
    build(
        subtype::FULLY_16550,
        (1, port.into()),
        (0x01 | 0x02, irq, irq.into()),
    )
}

/// Generic 16550 MMIO, 32-bit registers at four-byte stride, polling.
/// Used by AMD's AMDI0020 UART; this is not the port-I/O 16550 subtype.
pub fn build_spcr_16550_mmio32(base: u64, clock: u32) -> Sdt {
    build_body(SpcrBody {
        interface_type: 0x12,
        base_address: WireGas::new(0, 32, 0, 3, base),
        clock_frequency: U32::new(clock),
        precise_baud_rate: U32::new(115200),
        ..SpcrBody::default_for(0x12, (0, base), (0, 0, 0))
    })
}

impl SpcrBody {
    const fn default_for(
        interface: u8,
        (space, address): (u8, u64),
        (kind, irq, gsi): (u8, u8, u32),
    ) -> Self {
        Self {
            interface_type: interface,
            reserved: [0; 3],
            base_address: WireGas::new(space, 8, 0, 1, address),
            interrupt_type: kind,
            irq,
            global_system_interrupt: U32::new(gsi),
            baud_rate: baud::B115200,
            parity: 0,
            stop_bits: 1,
            flow_control: 0,
            terminal_type: 0,
            language: 0,
            pci_device_id: U16::new(0xFFFF),
            pci_vendor_id: U16::new(0xFFFF),
            pci_bus: 0,
            pci_device: 0,
            pci_function: 0,
            pci_flags: U32::ZERO,
            pci_segment: 0,
            clock_frequency: U32::ZERO,
            precise_baud_rate: U32::ZERO,
            namespace_string_length: U16::new(2),
            namespace_string_offset: U16::new(52),
            namespace: [b'.', 0],
        }
    }
}

/// `(address space, address)` of the registers, `(interrupt type, IRQ,
/// GSI)`.
fn build(interface: u8, gas: (u8, u64), irq: (u8, u8, u32)) -> Sdt {
    build_body(SpcrBody::default_for(interface, gas, irq))
}

fn build_body(body: SpcrBody) -> Sdt {
    // Layout: 36-byte header + 52-byte serial port info + 2-byte namespace.
    let total_size: u32 = 36 + core::mem::size_of::<SpcrBody>() as u32;

    let mut sdt = Sdt::new(
        *b"SPCR",
        total_size,
        4, // SPCR revision 4
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    sdt.write_bytes(36, body.as_bytes());
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

    #[test]
    fn com1() {
        let mut bytes = Vec::new();
        build_spcr_16550_io(0x3f8, 4).to_aml_bytes(&mut bytes);
        assert_eq!(bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
        assert_eq!((bytes[36], bytes[40]), (0, 1));
        assert_eq!(u64::from_le_bytes(bytes[44..52].try_into().unwrap()), 0x3f8);
        assert_eq!((bytes[52], bytes[53]), (3, 4));
        assert_eq!(u32::from_le_bytes(bytes[54..58].try_into().unwrap()), 4);
    }
}
