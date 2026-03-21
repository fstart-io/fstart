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

/// IORT table header size (ACPI header + node_count + node_offset + reserved).
const IORT_HEADER_SIZE: usize = 48;

/// IORT node header size (type + length + revision + identifier + mapping_count + mapping_offset).
const NODE_HEADER_SIZE: usize = 16;

/// IORT ID mapping entry size.
const ID_MAPPING_SIZE: usize = 20;

/// IORT node types.
mod node_type {
    pub const ITS_GROUP: u8 = 0x00;
    pub const ROOT_COMPLEX: u8 = 0x02;
}

/// Root Complex node data size (revision 4): 24 bytes.
const RC_DATA_SIZE: usize = 24;

/// ITS Group node configuration.
pub struct ItsGroup {
    /// GIC ITS identifiers (one per ITS in the system).
    pub its_ids: &'static [u32],
}

/// Root Complex node configuration.
pub struct RootComplex {
    /// PCI segment number.
    pub pci_segment: u32,
    /// Memory address limit in bits (e.g., 48 for 256 TiB).
    pub memory_address_limit: u8,
    /// Number of PCI Request IDs to map (e.g., 0x10000 for 64K RIDs).
    pub id_count: u32,
}

/// Build an IORT table with a single ITS Group and Root Complex.
///
/// The Root Complex maps all PCI RIDs [0, id_count) 1:1 to the ITS Group.
///
/// # Arguments
///
/// * `its` — ITS Group configuration (GIC ITS identifiers).
/// * `rc` — Root Complex configuration (PCI segment, address limit, RID count).
pub fn build_iort(its: &ItsGroup, rc: &RootComplex) -> Sdt {
    let its_count = its.its_ids.len();

    // ITS Group node: header + its_count(4) + identifiers(4 * count).
    let its_node_size = NODE_HEADER_SIZE + 4 + 4 * its_count;

    // Root Complex node: header + rc_data(24) + 1 ID mapping(20).
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
    sdt.write_u32(36, 2); // node_count
    sdt.write_u32(40, IORT_HEADER_SIZE as u32); // node_offset
    sdt.write_u32(44, 0); // reserved

    // ITS Group node starts at IORT_HEADER_SIZE.
    let its_off = IORT_HEADER_SIZE;
    let its_node_offset_in_table = its_off; // used by RC's ID mapping

    sdt.write_u8(its_off, node_type::ITS_GROUP); // type
    sdt.write_u16(its_off + 1, its_node_size as u16); // length
    sdt.write_u8(its_off + 3, 1); // revision
    sdt.write_u32(its_off + 4, 0); // identifier
    sdt.write_u32(its_off + 8, 0); // mapping_count (ITS Group has none)
    sdt.write_u32(its_off + 12, 0); // mapping_offset (unused)

    // ITS Group node data: its_count + identifiers[].
    let its_data_off = its_off + NODE_HEADER_SIZE;
    sdt.write_u32(its_data_off, its_count as u32);
    for (i, &id) in its.its_ids.iter().enumerate() {
        sdt.write_u32(its_data_off + 4 + i * 4, id);
    }

    // Root Complex node starts after ITS Group node.
    let rc_off = its_off + its_node_size;
    let rc_mapping_offset = (NODE_HEADER_SIZE + RC_DATA_SIZE) as u32;

    sdt.write_u8(rc_off, node_type::ROOT_COMPLEX); // type
    sdt.write_u16(rc_off + 1, rc_node_size as u16); // length
    sdt.write_u8(rc_off + 3, 4); // revision
    sdt.write_u32(rc_off + 4, 1); // identifier
    sdt.write_u32(rc_off + 8, 1); // mapping_count
    sdt.write_u32(rc_off + 12, rc_mapping_offset); // mapping_offset

    // Root Complex node data.
    let rc_data_off = rc_off + NODE_HEADER_SIZE;
    sdt.write_u64(rc_data_off, 0); // memory_properties
    sdt.write_u32(rc_data_off + 8, 0); // ats_attribute
    sdt.write_u32(rc_data_off + 12, rc.pci_segment); // pci_segment_number
    sdt.write_u8(rc_data_off + 16, rc.memory_address_limit); // memory_address_limit
    sdt.write_u16(rc_data_off + 17, 0); // pasid_capabilities
    sdt.write_u8(rc_data_off + 19, 0); // reserved
    sdt.write_u32(rc_data_off + 20, 0); // flags

    // ID Mapping entry (maps all PCI RIDs 1:1 to ITS).
    let id_map_off = rc_off + NODE_HEADER_SIZE + RC_DATA_SIZE;
    sdt.write_u32(id_map_off, 0); // input_base
                                  // id_count is stored as count - 1 in the IORT spec.
    sdt.write_u32(id_map_off + 4, rc.id_count.saturating_sub(1));
    sdt.write_u32(id_map_off + 8, 0); // output_base (1:1 mapping)
    sdt.write_u32(id_map_off + 12, its_node_offset_in_table as u32); // output_reference
    sdt.write_u32(id_map_off + 16, 0); // flags

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
        let its = ItsGroup { its_ids: &[0] };
        let rc = RootComplex {
            pci_segment: 0,
            memory_address_limit: 0x30,
            id_count: 0x10000,
        };

        let iort = build_iort(&its, &rc);
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
        let its = ItsGroup { its_ids: &[0, 1] };
        let rc = RootComplex {
            pci_segment: 1,
            memory_address_limit: 48,
            id_count: 256,
        };

        let iort = build_iort(&its, &rc);
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
