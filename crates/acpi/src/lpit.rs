//! LPIT (Low Power Idle Table, Intel ACPI LPIT 1.0): the platform's
//! low-power idle states and the counters that report their residency.

use crate::gas::GAS;
use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;
use zerocopy::{
    Immutable, IntoBytes,
    little_endian::{U16, U32, U64},
};

/// One native C-state LPI descriptor (type 0).
#[derive(Debug, Clone, Copy)]
pub struct NativeCState {
    /// How the OS enters the state (an MWAIT hint as FFixedHW).
    pub entry_trigger: GAS,
    /// Break-even residency, in microseconds.
    pub min_residency_us: u32,
    /// Worst-case exit latency, in microseconds.
    pub max_latency_us: u32,
    /// The residency counter.
    pub residency_counter: GAS,
    /// Counter frequency in Hz; 0 means the TSC frequency.
    pub counter_frequency: u64,
}

#[derive(IntoBytes, Immutable)]
#[repr(C, packed)]
struct NativeCStateDescriptor {
    kind: U32,
    length: U32,
    uid: U16,
    _reserved: U16,
    flags: U32,
    entry_trigger: GAS,
    min_residency_us: U32,
    max_latency_us: U32,
    residency_counter: GAS,
    counter_frequency: U64,
}

const HEADER: usize = 36;
const DESCRIPTOR: usize = core::mem::size_of::<NativeCStateDescriptor>();
const _: () = assert!(DESCRIPTOR == 56);

/// Build the LPIT (revision 1); descriptors get UIDs in order.
pub fn build_lpit(states: &[NativeCState]) -> Vec<u8> {
    let mut sdt = Sdt::new(
        *b"LPIT",
        HEADER as u32,
        1,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    for (uid, state) in states.iter().enumerate() {
        let descriptor = NativeCStateDescriptor {
            kind: 0.into(), // native C-state
            length: (DESCRIPTOR as u32).into(),
            uid: (uid as u16).into(),
            _reserved: 0.into(),
            flags: 0.into(), // enabled, counter available
            entry_trigger: state.entry_trigger,
            min_residency_us: state.min_residency_us.into(),
            max_latency_us: state.max_latency_us.into(),
            residency_counter: state.residency_counter,
            counter_frequency: state.counter_frequency.into(),
        };
        sdt.append_slice(descriptor.as_bytes());
    }
    crate::serialize(&sdt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gas::{AccessSize, AddressSpace};

    #[test]
    fn descriptors_follow_the_header() {
        let state = NativeCState {
            entry_trigger: GAS::new(
                AddressSpace::FunctionalFixedHardware,
                1,
                2,
                AccessSize::Undefined,
                0x60,
            ),
            min_residency_us: 30_000,
            max_latency_us: 3000,
            residency_counter: GAS::new(
                AddressSpace::SystemMemory,
                32,
                0,
                AccessSize::DwordAccess,
                0xfe00_013c,
            ),
            counter_frequency: 10_000,
        };
        let table = build_lpit(&[state, state]);
        assert_eq!(&table[..4], b"LPIT");
        assert_eq!(table.len(), HEADER + 2 * DESCRIPTOR);
        assert_eq!(
            u32::from_le_bytes(table[4..8].try_into().unwrap()) as usize,
            table.len()
        );
        let second = HEADER + DESCRIPTOR;
        assert_eq!(
            u16::from_le_bytes([table[second + 8], table[second + 9]]),
            1
        );
        assert_eq!(table[second + 16], 0x7f);
        assert_eq!(
            u64::from_le_bytes(table[second + 40..second + 48].try_into().unwrap()),
            0xfe00_013c
        );
        assert_eq!(
            u64::from_le_bytes(table[second + 48..second + 56].try_into().unwrap()),
            10_000
        );
        let sum = table.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
        assert_eq!(sum, 0);
    }
}
