//! IORT (IO Remapping Table) builder for ARM platforms.
//!
//! ARM DEN 0049E.e (revision 6).  Describes the relationship between
//! PCI devices and GIC ITS for MSI/MSI-X routing, and optionally
//! SMMUv3 for DMA remapping.
//!
//! Minimum viable IORT for SBSA:
//! - ITS Group node (references GIC ITS identifiers from MADT)
//! - Root Complex node (references ITS Group via ID mapping)
//!
//! Without IORT, the OS cannot route PCI MSI/MSI-X through the GIC ITS.

use acpi_tables::sdt::Sdt;

// ---------------------------------------------------------------------------
// Wire-format structs — #[repr(C, packed)] mirrors the IORT spec layout.
// ---------------------------------------------------------------------------

/// IORT table fields after the 36-byte ACPI SDT header.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IortTableHeader {
    node_count: u32,
    node_offset: u32,
    reserved: u32,
}

/// IORT node header — common to all node types.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IortNodeHeader {
    node_type: u8,
    length: u16,
    revision: u8,
    identifier: u32,
    mapping_count: u32,
    mapping_offset: u32,
}

/// Root Complex node-specific data (revision 4, 24 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RcNodeData {
    memory_properties: u64,
    ats_attribute: u32,
    pci_segment_number: u32,
    memory_address_limit: u8,
    pasid_capabilities: u16,
    _reserved: u8,
    flags: u32,
}

/// IORT ID mapping entry (20 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdMapping {
    input_base: u32,
    /// Stored as count − 1 per the IORT spec.
    id_count: u32,
    output_base: u32,
    output_reference: u32,
    flags: u32,
}

const ACPI_HDR: usize = 36;
const IORT_HEADER_SIZE: usize = ACPI_HDR + core::mem::size_of::<IortTableHeader>();
const NODE_HEADER_SIZE: usize = core::mem::size_of::<IortNodeHeader>();
const RC_DATA_SIZE: usize = core::mem::size_of::<RcNodeData>();
const ID_MAPPING_SIZE: usize = core::mem::size_of::<IdMapping>();

/// IORT node types.
mod node_type {
    pub const ITS_GROUP: u8 = 0x00;
    pub const ROOT_COMPLEX: u8 = 0x02;
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// IORT configuration: ITS Group + Root Complex.
///
/// Describes the ITS-to-PCI RID mapping needed for MSI/MSI-X routing.
/// The Root Complex maps all PCI RIDs `[0, id_count)` 1:1 to the ITS Group.
pub struct IortConfig {
    /// GIC ITS identifiers (one per ITS in the system).
    pub its_ids: &'static [u32],
    /// PCI segment number.
    pub pci_segment: u32,
    /// Memory address limit in bits (e.g., 48 for 256 TiB).
    pub memory_address_limit: u8,
    /// Number of PCI Request IDs to map (e.g., 0x10000 for 64K RIDs).
    pub id_count: u32,
}

/// Build an IORT table with a single ITS Group and Root Complex.
///
/// The Root Complex maps all PCI RIDs `[0, config.id_count)` 1:1 to the
/// ITS Group.
pub fn build_iort(config: &IortConfig) -> Sdt {
    let its_count = config.its_ids.len();

    // ITS Group node: header + its_count(4) + identifiers(4 * count).
    let its_node_size = NODE_HEADER_SIZE + 4 + 4 * its_count;

    // Root Complex node: header + rc_data + 1 ID mapping.
    let rc_node_size = NODE_HEADER_SIZE + RC_DATA_SIZE + ID_MAPPING_SIZE;

    let total_size = IORT_HEADER_SIZE + its_node_size + rc_node_size;

    let mut sdt = Sdt::new(
        *b"IORT",
        total_size as u32,
        6, // IORT revision 6
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );

    // IORT header (after 36-byte ACPI header).
    crate::write_struct(
        &mut sdt,
        ACPI_HDR,
        &IortTableHeader {
            node_count: 2,
            node_offset: IORT_HEADER_SIZE as u32,
            reserved: 0,
        },
    );

    // ITS Group node.
    let its_off = IORT_HEADER_SIZE;
    crate::write_struct(
        &mut sdt,
        its_off,
        &IortNodeHeader {
            node_type: node_type::ITS_GROUP,
            length: its_node_size as u16,
            revision: 1,
            identifier: 0,
            mapping_count: 0,
            mapping_offset: 0,
        },
    );

    // ITS Group node data: its_count + identifiers[].
    let its_data_off = its_off + NODE_HEADER_SIZE;
    sdt.write_u32(its_data_off, its_count as u32);
    for (i, &id) in config.its_ids.iter().enumerate() {
        sdt.write_u32(its_data_off + 4 + i * 4, id);
    }

    // Root Complex node.
    let rc_off = its_off + its_node_size;
    crate::write_struct(
        &mut sdt,
        rc_off,
        &IortNodeHeader {
            node_type: node_type::ROOT_COMPLEX,
            length: rc_node_size as u16,
            revision: 4,
            identifier: 1,
            mapping_count: 1,
            mapping_offset: (NODE_HEADER_SIZE + RC_DATA_SIZE) as u32,
        },
    );

    // Root Complex node data.
    crate::write_struct(
        &mut sdt,
        rc_off + NODE_HEADER_SIZE,
        &RcNodeData {
            memory_properties: 0,
            ats_attribute: 0,
            pci_segment_number: config.pci_segment,
            memory_address_limit: config.memory_address_limit,
            pasid_capabilities: 0,
            _reserved: 0,
            flags: 0,
        },
    );

    // ID Mapping: maps all PCI RIDs 1:1 to the ITS Group node.
    crate::write_struct(
        &mut sdt,
        rc_off + NODE_HEADER_SIZE + RC_DATA_SIZE,
        &IdMapping {
            input_base: 0,
            id_count: config.id_count.saturating_sub(1),
            output_base: 0,
            output_reference: its_off as u32,
            flags: 0,
        },
    );

    sdt.update_checksum();
    sdt
}

#[cfg(test)]
mod tests {
    use super::*;
    use acpi_tables::Aml;
    use alloc::vec::Vec;

    #[test]
    fn test_iort_basic() {
        let cfg = IortConfig {
            its_ids: &[0],
            pci_segment: 0,
            memory_address_limit: 0x30,
            id_count: 0x10000,
        };

        let iort = build_iort(&cfg);
        let mut bytes = Vec::new();
        iort.to_aml_bytes(&mut bytes);

        // Verify checksum.
        let sum = bytes.iter().fold(0u8, |acc, &x| acc.wrapping_add(x));
        assert_eq!(sum, 0, "IORT checksum failed");

        // Verify signature.
        assert_eq!(&bytes[0..4], b"IORT");

        // Verify revision = 6.
        assert_eq!(bytes[8], 6);

        // Verify node count = 2.
        assert_eq!(
            u32::from_le_bytes(bytes[36..40].try_into().unwrap()),
            2,
            "node_count"
        );

        // Verify node offset = 48.
        assert_eq!(
            u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            48,
            "node_offset"
        );

        // ITS Group node at offset 48.
        assert_eq!(bytes[48], node_type::ITS_GROUP, "ITS Group type");
        let its_node_len = u16::from_le_bytes(bytes[49..51].try_into().unwrap());
        // header(16) + its_count(4) + 1 identifier(4) = 24
        assert_eq!(its_node_len, 24, "ITS Group node length");

        // its_count = 1
        assert_eq!(
            u32::from_le_bytes(bytes[64..68].try_into().unwrap()),
            1,
            "its_count"
        );
        // its_id[0] = 0
        assert_eq!(
            u32::from_le_bytes(bytes[68..72].try_into().unwrap()),
            0,
            "its_id[0]"
        );

        // Root Complex node at offset 48 + 24 = 72.
        assert_eq!(bytes[72], node_type::ROOT_COMPLEX, "RC type");
        let rc_node_len = u16::from_le_bytes(bytes[73..75].try_into().unwrap());
        // header(16) + rc_data(24) + id_mapping(20) = 60
        assert_eq!(rc_node_len, 60, "RC node length");

        // RC mapping_count = 1
        assert_eq!(
            u32::from_le_bytes(bytes[80..84].try_into().unwrap()),
            1,
            "RC mapping_count"
        );

        // RC memory_address_limit at rc_off + 16 + 16 = 72 + 32 = 104
        assert_eq!(bytes[104], 0x30, "memory_address_limit");

        // ID mapping: id_count = 0xFFFF (0x10000 - 1)
        let id_map_off = 72 + 16 + 24; // 112
        assert_eq!(
            u32::from_le_bytes(bytes[id_map_off + 4..id_map_off + 8].try_into().unwrap()),
            0xFFFF,
            "id_count (stored as count-1)"
        );

        // output_reference should point to ITS node at offset 48.
        assert_eq!(
            u32::from_le_bytes(bytes[id_map_off + 12..id_map_off + 16].try_into().unwrap()),
            48,
            "output_reference"
        );
    }

    #[test]
    fn test_iort_multiple_its() {
        let cfg = IortConfig {
            its_ids: &[0, 1],
            pci_segment: 1,
            memory_address_limit: 48,
            id_count: 256,
        };

        let iort = build_iort(&cfg);
        let mut bytes = Vec::new();
        iort.to_aml_bytes(&mut bytes);

        let sum = bytes.iter().fold(0u8, |acc, &x| acc.wrapping_add(x));
        assert_eq!(sum, 0, "IORT checksum failed");

        // ITS node should be larger: header(16) + its_count(4) + 2*4 = 28
        let its_node_len = u16::from_le_bytes(bytes[49..51].try_into().unwrap());
        assert_eq!(its_node_len, 28, "ITS Group node length with 2 ITS");

        // RC node at offset 48 + 28 = 76
        assert_eq!(bytes[76], node_type::ROOT_COMPLEX);

        // PCI segment = 1 at rc_data_off + 12 = 76+16+12 = 104
        assert_eq!(
            u32::from_le_bytes(bytes[104..108].try_into().unwrap()),
            1,
            "pci_segment"
        );
    }
}
