//! PCCT (Platform Communications Channel Table), ACPI 6.5 §14.1, with
//! generic (type 0) subspaces.

use acpi_tables::gas::GAS;
use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;

/// A generic communication subspace (type 0, 62 bytes).
#[derive(Debug, Clone, Copy)]
pub struct GenericSubspace {
    /// Shared memory region.
    pub base: u64,
    pub length: u64,
    /// Doorbell register; the OS writes `(old & preserve) | write`.
    pub doorbell: GAS,
    pub doorbell_preserve: u64,
    pub doorbell_write: u64,
    pub nominal_latency_us: u32,
    /// Maximum periodic accesses per minute (0 = no limit).
    pub max_periodic_access_rate: u32,
    pub min_request_turnaround_us: u16,
}

const HEADER: u32 = 48;
const SUBSPACE_LEN: u8 = 62;

fn gas_bytes(g: &GAS) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[0] = g.address_space_id as u8;
    b[1] = g.register_bit_width;
    b[2] = g.register_bit_offset;
    b[3] = g.access_size as u8;
    b[4..].copy_from_slice(&g.address.get().to_le_bytes());
    b
}

impl GenericSubspace {
    fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(usize::from(SUBSPACE_LEN));
        b.push(0);
        b.push(SUBSPACE_LEN);
        b.extend_from_slice(&[0; 6]);
        b.extend_from_slice(&self.base.to_le_bytes());
        b.extend_from_slice(&self.length.to_le_bytes());
        b.extend_from_slice(&gas_bytes(&self.doorbell));
        b.extend_from_slice(&self.doorbell_preserve.to_le_bytes());
        b.extend_from_slice(&self.doorbell_write.to_le_bytes());
        b.extend_from_slice(&self.nominal_latency_us.to_le_bytes());
        b.extend_from_slice(&self.max_periodic_access_rate.to_le_bytes());
        b.extend_from_slice(&self.min_request_turnaround_us.to_le_bytes());
        b
    }
}

/// Build the PCCT (revision 2, no platform interrupt). Subspace `i` is PCC
/// subspace `i` of `_CPC` and other PCC register users.
pub fn build_pcct(subspaces: &[GenericSubspace]) -> Vec<u8> {
    let mut sdt = Sdt::new(
        *b"PCCT",
        HEADER,
        2,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    subspaces.iter().for_each(|s| sdt.append_slice(&s.bytes()));
    crate::serialize(&sdt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use acpi_tables::gas::{AccessSize, AddressSpace};

    #[test]
    fn layout() {
        let t = build_pcct(&[GenericSubspace {
            base: 0x7ff0_0000,
            length: 0x1_0000,
            doorbell: GAS::new(
                AddressSpace::SystemMemory,
                64,
                0,
                AccessSize::QwordAccess,
                0xfe81_0540,
            ),
            doorbell_preserve: 0xffff_ffff_0000_0000,
            doorbell_write: 1,
            nominal_latency_us: 4000,
            max_periodic_access_rate: 60000,
            min_request_turnaround_us: 0,
        }]);
        // The donor's PCCT (am4-re acpi-dxe.json) is 110 bytes.
        assert_eq!(t.len(), 110);
        assert_eq!(&t[..4], b"PCCT");
        assert_eq!(t[8], 2);
        assert_eq!(&t[48..50], &[0, 62]);
        assert_eq!(&t[56..64], &0x7ff0_0000u64.to_le_bytes());
        assert_eq!(
            &t[72..84],
            &[0, 64, 0, 4, 0x40, 0x05, 0x81, 0xfe, 0, 0, 0, 0]
        );
        assert_eq!(&t[100..104], &4000u32.to_le_bytes());
        assert_eq!(t.iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
    }
}
