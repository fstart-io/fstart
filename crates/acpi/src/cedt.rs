//! CEDT (CXL Early Discovery Table, CXL 3.1 §9.18) and the AML objects of a
//! CXL host: `ACPI0016` host bridges and the `ACPI0017` CXL root device
//! (CXL 3.1 §9.18.1, §9.18.2).
//!
//! The CEDT has one CHBS per CXL host bridge (where its component registers
//! are, or its RCRB for a restricted CXL host) and one CFMWS per window of
//! host physical address the platform decodes to CXL host bridges. Linux's
//! `cxl_acpi` binds to the `ACPI0017` device and reads the CEDT.

use acpi_tables::sdt::Sdt;
use alloc::vec::Vec;
use fstart_pci::cxl::hdm;
use zerocopy::{
    Immutable, IntoBytes,
    little_endian::{U16, U32, U64},
};

/// Length of a CHBS record.
pub const CHBS_LENGTH: u16 = 0x20;
/// Length of the RCRB pair (downstream + upstream port) a restricted CXL
/// host bridge's CHBS describes.
pub const RCRB_LENGTH: u64 = 0x2000;

/// The CXL version a host bridge's CHBS describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChbsVersion {
    /// A restricted CXL host (CXL 1.1): `base` is the downstream port RCRB.
    Rch = 0,
    /// A CXL 2.0 or later virtual hierarchy: `base` is the host bridge's
    /// component register block.
    Vh = 1,
}

/// CFMWS Window Restrictions (CXL 3.1 Table 9-22).
pub mod restrictions {
    pub const DEVICE_COHERENT: u16 = 1 << 0;
    pub const HOST_ONLY_COHERENT: u16 = 1 << 1;
    pub const VOLATILE: u16 = 1 << 2;
    pub const PERSISTENT: u16 = 1 << 3;
    pub const FIXED_DEVICE_CONFIGURATION: u16 = 1 << 4;
}

/// CFMWS Interleave Arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arithmetic {
    /// Standard modulo arithmetic.
    Modulo = 0,
    /// Modulo arithmetic combined with XOR (needs a CXIMS).
    Xor = 1,
}

/// One CEDT structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CedtEntry {
    /// CXL Host Bridge Structure: the host bridge whose `_UID` is `uid`.
    Chbs {
        uid: u32,
        version: ChbsVersion,
        base: u64,
        length: u64,
    },
    /// CXL Fixed Memory Window Structure.
    Cfmws(Cfmws),
}

/// A CXL Fixed Memory Window: `[base, base + size)` interleaved across the
/// host bridges `targets` (their `_UID`s, way 0 first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cfmws {
    pub base: u64,
    pub size: u64,
    pub targets: Vec<u32>,
    pub arithmetic: Arithmetic,
    /// Host bridge interleave granularity in bytes.
    pub granularity: u32,
    /// [`restrictions`] bits.
    pub restrictions: u16,
    /// QoS Throttling Group.
    pub qtg_id: u16,
}

/// Why a CEDT structure cannot be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Window base or size not 256 MiB aligned.
    Alignment,
    /// A target count without an ENIW encoding.
    Ways(usize),
    Granularity(u32),
}

#[derive(IntoBytes, Immutable)]
#[repr(C, packed)]
struct Chbs {
    kind: u8,
    reserved: u8,
    length: U16,
    uid: U32,
    version: U32,
    reserved2: U32,
    base: U64,
    size: U64,
}
const _: () = assert!(size_of::<Chbs>() == CHBS_LENGTH as usize);

#[derive(IntoBytes, Immutable)]
#[repr(C, packed)]
struct CfmwsHeader {
    kind: u8,
    reserved: u8,
    length: U16,
    reserved2: U32,
    base: U64,
    size: U64,
    eniw: u8,
    arithmetic: u8,
    reserved3: U16,
    hbig: U32,
    restrictions: U16,
    qtg_id: U16,
}
const _: () = assert!(size_of::<CfmwsHeader>() == 0x24);

impl CedtEntry {
    /// The structure's bytes.
    pub fn bytes(&self) -> Result<Vec<u8>, Error> {
        match self {
            Self::Chbs {
                uid,
                version,
                base,
                length,
            } => Ok(Chbs {
                kind: 0,
                reserved: 0,
                length: CHBS_LENGTH.into(),
                uid: (*uid).into(),
                version: (*version as u32).into(),
                reserved2: U32::ZERO,
                base: (*base).into(),
                size: (*length).into(),
            }
            .as_bytes()
            .to_vec()),
            Self::Cfmws(window) => window.bytes(),
        }
    }
}

impl Cfmws {
    fn bytes(&self) -> Result<Vec<u8>, Error> {
        if !self.base.is_multiple_of(hdm::ALIGN)
            || !self.size.is_multiple_of(hdm::ALIGN)
            || self.size == 0
        {
            return Err(Error::Alignment);
        }
        let ways = self.targets.len();
        let eniw = u8::try_from(ways)
            .ok()
            .and_then(hdm::ways)
            .ok_or(Error::Ways(ways))?;
        let hbig =
            hdm::granularity(self.granularity).ok_or(Error::Granularity(self.granularity))?;
        let mut out = CfmwsHeader {
            kind: 1,
            reserved: 0,
            length: ((0x24 + 4 * ways) as u16).into(),
            reserved2: U32::ZERO,
            base: self.base.into(),
            size: self.size.into(),
            eniw,
            arithmetic: self.arithmetic as u8,
            reserved3: U16::ZERO,
            hbig: u32::from(hbig).into(),
            restrictions: self.restrictions.into(),
            qtg_id: self.qtg_id.into(),
        }
        .as_bytes()
        .to_vec();
        self.targets
            .iter()
            .for_each(|t| out.extend_from_slice(&t.to_le_bytes()));
        Ok(out)
    }
}

/// Build the CEDT (revision 1) with `oem_id`/`oem_table_id`.
pub fn build_cedt(
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    entries: &[CedtEntry],
) -> Result<Vec<u8>, Error> {
    let mut sdt = Sdt::new(*b"CEDT", 36, 1, oem_id, oem_table_id, crate::OEM_REVISION);
    for entry in entries {
        sdt.append_slice(&entry.bytes()?);
    }
    Ok(crate::serialize(&sdt))
}

/// `_HID` and `_CID` of a CXL host bridge: `ACPI0016`, compatible with a
/// PCIe (`PNP0A08`) and PCI (`PNP0A03`) host bridge, so an OS without CXL
/// support binds its PCI driver.
pub fn host_bridge_ids() -> Vec<u8> {
    use crate::aml::{EISAName, Name, Package, Path};
    use acpi_tables::Aml;
    let mut out = Vec::new();
    Name::new(Path::new("_HID"), &"ACPI0016").to_aml_bytes(&mut out);
    let (pcie, pci) = (EISAName::new("PNP0A08"), EISAName::new("PNP0A03"));
    Name::new(Path::new("_CID"), &Package::new(alloc::vec![&pcie, &pci])).to_aml_bytes(&mut out);
    out
}

/// `Device(name)` with `_HID "ACPI0017"`, the CXL root object an OS's CXL
/// driver binds to before it reads the CEDT. Place it in `\_SB`.
pub fn root_device(name: &str) -> Result<Vec<u8>, crate::AmlError> {
    use crate::aml::{Name, Path};
    use acpi_tables::Aml;
    let mut body = Vec::new();
    Name::new(Path::new("_HID"), &"ACPI0017").to_aml_bytes(&mut body);
    // Present, enabled, functioning; not shown in UI.
    Name::new(Path::new("_STA"), &0x0bu8).to_aml_bytes(&mut body);
    crate::aml_linker::device_vec(name, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chbs_and_cfmws_layout() {
        let cedt = build_cedt(
            *b"FSTART",
            *b"FSTARTCX",
            &[
                CedtEntry::Chbs {
                    uid: 2,
                    version: ChbsVersion::Vh,
                    base: 0x100_0000_0000,
                    length: 0x1_0000,
                },
                CedtEntry::Cfmws(Cfmws {
                    base: 0x400_0000_0000,
                    size: 0x40_0000_0000,
                    targets: alloc::vec![2, 3],
                    arithmetic: Arithmetic::Modulo,
                    granularity: 256,
                    restrictions: restrictions::HOST_ONLY_COHERENT | restrictions::VOLATILE,
                    qtg_id: 1,
                }),
            ],
        )
        .unwrap();
        assert_eq!(&cedt[..4], b"CEDT");
        assert_eq!(cedt[8], 1);
        let length = 36 + 0x20 + 0x24 + 8;
        assert_eq!(u32::from_le_bytes(cedt[4..8].try_into().unwrap()), length);
        assert_eq!(cedt.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
        let chbs = &cedt[36..36 + 0x20];
        assert_eq!(&chbs[..4], &[0, 0, 0x20, 0]);
        assert_eq!(&chbs[4..12], &[2, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(&chbs[16..24], &0x100_0000_0000u64.to_le_bytes());
        let cfmws = &cedt[36 + 0x20..];
        assert_eq!(&cfmws[..4], &[1, 0, 0x2c, 0]);
        assert_eq!(cfmws[0x18], 1); // ENIW: 2 ways
        assert_eq!(&cfmws[0x1c..0x20], &[0, 0, 0, 0]); // HBIG 256 B
        assert_eq!(&cfmws[0x20..0x24], &[6, 0, 1, 0]);
        assert_eq!(&cfmws[0x24..], &[2, 0, 0, 0, 3, 0, 0, 0]);

        let bad = CedtEntry::Cfmws(Cfmws {
            base: 0x1000,
            size: 0x1000_0000,
            targets: alloc::vec![0],
            arithmetic: Arithmetic::Modulo,
            granularity: 256,
            restrictions: 0,
            qtg_id: 0,
        });
        assert_eq!(bad.bytes(), Err(Error::Alignment));
    }

    #[test]
    fn host_bridge_and_root_device_aml() {
        let ids = host_bridge_ids();
        let contains = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
        assert!(contains(&ids, b"ACPI0016"));
        assert!(contains(&ids, b"_CID"));
        let root = root_device("CXLD").unwrap();
        assert!(contains(&root, b"CXLD") && contains(&root, b"ACPI0017"));
    }
}
