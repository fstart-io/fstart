//! HMAT (Heterogeneous Memory Attribute Table), ACPI 6.5 §5.2.28, revision
//! 2: Memory Proximity Domain Attributes (type 0) and System Locality
//! Latency and Bandwidth Information (type 1) structures.

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;
use zerocopy::{
    Immutable, IntoBytes,
    little_endian::{U16, U32, U64},
};

/// SLLBI data types (ACPI 6.5 Table 5.167), the CDAT's DSLBIS types too.
pub mod data {
    pub const ACCESS_LATENCY: u8 = 0;
    pub const READ_LATENCY: u8 = 1;
    pub const WRITE_LATENCY: u8 = 2;
    pub const ACCESS_BANDWIDTH: u8 = 3;
    pub const READ_BANDWIDTH: u8 = 4;
    pub const WRITE_BANDWIDTH: u8 = 5;
}

/// One HMAT structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HmatEntry {
    /// The memory of `memory` is attached to the initiator `initiator`
    /// (`None`: no attached initiator, e.g. CPU-less CXL memory).
    Attributes { initiator: Option<u32>, memory: u32 },
    /// A matrix of `data_type` from every initiator to every target:
    /// `entries[i * targets.len() + t]` in units of `base_unit` (latency
    /// picoseconds, bandwidth MB/s); 0 is "not provided".
    Locality {
        data_type: u8,
        base_unit: u64,
        initiators: Vec<u32>,
        targets: Vec<u32>,
        entries: Vec<u16>,
    },
}

/// Why a structure cannot be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The matrix does not have initiators × targets entries.
    Matrix,
}

#[derive(IntoBytes, Immutable)]
#[repr(C, packed)]
struct Mpda {
    kind: U16,
    reserved: U16,
    length: U32,
    flags: U16,
    reserved2: U16,
    initiator: U32,
    memory: U32,
    reserved3: U32,
    reserved4: U64,
    reserved5: U64,
}

#[derive(IntoBytes, Immutable)]
#[repr(C, packed)]
struct SllbiHeader {
    kind: U16,
    reserved: U16,
    length: U32,
    /// Memory hierarchy [3:0] (0: memory), access attributes [5:4].
    flags: u8,
    data_type: u8,
    min_transfer: u8,
    reserved2: u8,
    initiators: U32,
    targets: U32,
    reserved3: U32,
    base_unit: U64,
}

/// The MPDA flag: the initiator domain field is valid.
const INITIATOR_VALID: u16 = 1 << 0;

fn entry(e: &HmatEntry) -> Result<Vec<u8>, Error> {
    Ok(match e {
        &HmatEntry::Attributes { initiator, memory } => Mpda {
            kind: 0.into(),
            reserved: 0.into(),
            length: 40.into(),
            flags: if initiator.is_some() {
                INITIATOR_VALID
            } else {
                0
            }
            .into(),
            reserved2: 0.into(),
            initiator: initiator.unwrap_or(0).into(),
            memory: memory.into(),
            reserved3: 0.into(),
            reserved4: 0.into(),
            reserved5: 0.into(),
        }
        .as_bytes()
        .to_vec(),
        HmatEntry::Locality {
            data_type,
            base_unit,
            initiators,
            targets,
            entries,
        } => {
            if entries.len() != initiators.len() * targets.len() {
                return Err(Error::Matrix);
            }
            let length = size_of::<SllbiHeader>() + 4 * (initiators.len() + targets.len());
            let length = length + 2 * entries.len();
            let mut out = SllbiHeader {
                kind: 1.into(),
                reserved: 0.into(),
                length: (length as u32).into(),
                flags: 0,
                data_type: *data_type,
                min_transfer: 0,
                reserved2: 0,
                initiators: (initiators.len() as u32).into(),
                targets: (targets.len() as u32).into(),
                reserved3: 0.into(),
                base_unit: (*base_unit).into(),
            }
            .as_bytes()
            .to_vec();
            for d in initiators.iter().chain(targets) {
                out.extend_from_slice(&d.to_le_bytes());
            }
            for v in entries {
                out.extend_from_slice(&v.to_le_bytes());
            }
            // Structures are 4-byte multiples in practice; pad.
            out.resize(out.len().next_multiple_of(4), 0);
            let padded = out.len() as u32;
            out[4..8].copy_from_slice(&padded.to_le_bytes());
            out
        }
    })
}

/// Latency (picoseconds) and bandwidth (MB/s) from an initiator to a
/// target, read and write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Performance {
    pub read_latency: u64,
    pub write_latency: u64,
    pub read_bandwidth: u64,
    pub write_bandwidth: u64,
}

/// `values` as SLLBI entries and their base unit: the unit starts at 1 and
/// grows tenfold while the largest value does not fit below `0xFFFF`
/// (AmdAcpiHmatService's rule); 0 stays "not provided", and a value
/// below the unit is 1 rather than that 0.
pub fn scale(values: &[u64]) -> (u64, Vec<u16>) {
    let mut max = values.iter().copied().max().unwrap_or(0);
    let mut unit = 1u64;
    while max > 0xfffe {
        max /= 10;
        unit *= 10;
    }
    (
        unit,
        values
            .iter()
            .map(|&v| match v {
                0 => 0,
                v => (v / unit).clamp(1, 0xfffe) as u16,
            })
            .collect(),
    )
}

/// The structures of a system whose `initiators` reach `targets` with
/// `perf(initiator, target)` (`None`: not provided): one Memory Proximity
/// Domain Attributes structure per target, its initiator the target itself
/// when that is an initiator too; then the latency and the bandwidth
/// matrices, access values (data types 0 and 3) when every pair reads and
/// writes alike, else read and write ones (1, 2 and 4, 5).
pub fn locality(
    initiators: &[u32],
    targets: &[u32],
    perf: impl Fn(u32, u32) -> Option<Performance>,
) -> Vec<HmatEntry> {
    let mut entries: Vec<HmatEntry> = targets
        .iter()
        .map(|&t| HmatEntry::Attributes {
            initiator: initiators.contains(&t).then_some(t),
            memory: t,
        })
        .collect();
    let cells: Vec<Option<Performance>> = initiators
        .iter()
        .flat_map(|&i| targets.iter().map(move |&t| (i, t)))
        .map(|(i, t)| perf(i, t))
        .collect();
    if cells.iter().all(Option::is_none) {
        return entries;
    }
    let same = |f: fn(&Performance) -> (u64, u64)| {
        cells.iter().flatten().all(|p| {
            let (r, w) = f(p);
            r == w
        })
    };
    let latency_same = same(|p| (p.read_latency, p.write_latency));
    let bandwidth_same = same(|p| (p.read_bandwidth, p.write_bandwidth));
    let mut matrix = |data_type: u8, value: fn(&Performance) -> u64| {
        let values: Vec<u64> = cells.iter().map(|c| c.as_ref().map_or(0, value)).collect();
        let (base_unit, scaled) = scale(&values);
        entries.push(HmatEntry::Locality {
            data_type,
            base_unit,
            initiators: initiators.to_vec(),
            targets: targets.to_vec(),
            entries: scaled,
        });
    };
    if latency_same {
        matrix(data::ACCESS_LATENCY, |p| p.read_latency);
    } else {
        matrix(data::READ_LATENCY, |p| p.read_latency);
        matrix(data::WRITE_LATENCY, |p| p.write_latency);
    }
    if bandwidth_same {
        matrix(data::ACCESS_BANDWIDTH, |p| p.read_bandwidth);
    } else {
        matrix(data::READ_BANDWIDTH, |p| p.read_bandwidth);
        matrix(data::WRITE_BANDWIDTH, |p| p.write_bandwidth);
    }
    entries
}

/// Build the HMAT from `entries`.
pub fn build_hmat(entries: &[HmatEntry]) -> Result<Vec<u8>, Error> {
    let mut sdt = Sdt::new(
        *b"HMAT",
        40,
        2,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );
    for e in entries {
        sdt.append_slice(&entry(e)?);
    }
    Ok(crate::serialize(&sdt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn structures_encode() {
        let hmat = build_hmat(&[
            HmatEntry::Attributes {
                initiator: Some(0),
                memory: 0,
            },
            HmatEntry::Attributes {
                initiator: None,
                memory: 1,
            },
            HmatEntry::Locality {
                data_type: data::ACCESS_LATENCY,
                base_unit: 10,
                initiators: vec![0],
                targets: vec![0, 1],
                entries: vec![12, 51],
            },
        ])
        .unwrap();
        assert_eq!(&hmat[..4], b"HMAT");
        assert_eq!(hmat[8], 2);
        assert_eq!(hmat.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
        // MPDA 0 at 40, MPDA 1 at 80, SLLBI at 120.
        assert_eq!(&hmat[40..48], &[0, 0, 0, 0, 40, 0, 0, 0]);
        assert_eq!(&hmat[48..50], &[1, 0]);
        assert_eq!(&hmat[80 + 8..80 + 10], &[0, 0]);
        assert_eq!(&hmat[80 + 16..80 + 20], &[1, 0, 0, 0]);
        let sllbi = &hmat[120..];
        assert_eq!(&sllbi[..2], &[1, 0]);
        // 32 + 3 domains + 2 entries = 48 bytes.
        assert_eq!(&sllbi[4..8], &48u32.to_le_bytes());
        assert_eq!(&sllbi[12..16], &1u32.to_le_bytes());
        assert_eq!(&sllbi[16..20], &2u32.to_le_bytes());
        assert_eq!(&sllbi[24..32], &10u64.to_le_bytes());
        assert_eq!(&sllbi[44..48], &[12, 0, 51, 0]);
        assert_eq!(hmat.len(), 120 + 48);
        assert_eq!(
            build_hmat(&[HmatEntry::Locality {
                data_type: 0,
                base_unit: 1,
                initiators: vec![0],
                targets: vec![0],
                entries: vec![],
            }]),
            Err(Error::Matrix)
        );
    }

    #[test]
    fn values_scale_into_16_bits() {
        assert_eq!(scale(&[110_000, 225_000]), (10, vec![11_000, 22_500]));
        assert_eq!(scale(&[0xfffe, 3]), (1, vec![0xfffe, 3]));
        assert_eq!(scale(&[0xffff]), (10, vec![6553]));
        assert_eq!(scale(&[]), (1, vec![]));
        // Provided but below the unit: the smallest value, not 0.
        assert_eq!(scale(&[0x1_0000, 5, 0]), (10, vec![6553, 1, 0]));
    }

    #[test]
    fn locality_has_attributes_and_access_or_read_write_matrices() {
        let p = |l: u64, b: u64| Performance {
            read_latency: l,
            write_latency: l,
            read_bandwidth: b,
            write_bandwidth: b,
        };
        // Domains 0 and 1 have CPUs and memory, 2 memory only.
        let all = locality(&[0, 1], &[0, 1, 2], |i, t| {
            Some(if i == t {
                p(110_000, 100)
            } else {
                p(225_000, 50)
            })
        });
        assert_eq!(all.len(), 3 + 2);
        assert_eq!(
            all[2],
            HmatEntry::Attributes {
                initiator: None,
                memory: 2
            }
        );
        assert_eq!(
            all[0],
            HmatEntry::Attributes {
                initiator: Some(0),
                memory: 0
            }
        );
        let HmatEntry::Locality {
            data_type,
            base_unit,
            entries,
            ..
        } = &all[3]
        else {
            panic!()
        };
        assert_eq!((*data_type, *base_unit), (data::ACCESS_LATENCY, 10));
        assert_eq!(
            entries,
            &vec![11_000, 22_500, 22_500, 22_500, 11_000, 22_500]
        );
        // A pair that writes slower: read and write latency matrices.
        let rw = locality(&[0], &[0], |_, _| {
            Some(Performance {
                write_latency: 200,
                ..p(100, 7)
            })
        });
        let types: Vec<u8> = rw
            .iter()
            .filter_map(|e| match e {
                HmatEntry::Locality { data_type, .. } => Some(*data_type),
                _ => None,
            })
            .collect();
        assert_eq!(types, [1, 2, 3]);
        assert!(build_hmat(&rw).is_ok());
        // Nothing provided: attributes only.
        assert_eq!(locality(&[0], &[0], |_, _| None).len(), 1);
    }
}
