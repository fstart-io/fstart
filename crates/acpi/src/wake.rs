//! Architecture-neutral ACPI wake-vector lookup.
//!
//! Given the physical address of an RSDP and a caller-provided physical-memory
//! reader, this module follows RSDP -> XSDT -> FADT -> FACS and returns the
//! firmware waking vector saved by the operating system. Locating the RSDP is
//! platform-specific and belongs in the architecture platform module.

use zerocopy::byteorder::{LE, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

const MAX_SDT_LEN: usize = 4096;
const RSDP_V1_LEN: usize = 20;

#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Rsdp {
    pub signature: [u8; 8],
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub revision: u8,
    pub rsdt_addr: U32<LE>,
    pub length: U32<LE>,
    pub xsdt_addr: U64<LE>,
    pub extended_checksum: u8,
    pub reserved: [u8; 3],
}

#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
pub struct SdtHeader {
    pub signature: [u8; 4],
    pub length: U32<LE>,
    pub revision: u8,
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: U32<LE>,
    pub creator_id: [u8; 4],
    pub creator_revision: U32<LE>,
}

#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(transparent)]
pub struct XsdtEntry(pub U64<LE>);

#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
pub struct FadtPrefix {
    pub header: SdtHeader,
    pub firmware_ctrl: U32<LE>,
    pub dsdt: U32<LE>,
    pub _pad0: [u8; 68],
    pub flags: U32<LE>,
    pub _pad1: [u8; 16],
    pub x_firmware_ctrl: U64<LE>,
}

#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
pub struct FadtV2Prefix {
    pub prefix: FadtPrefix,
    pub x_dsdt: U64<LE>,
}

#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
pub struct FacsPrefix {
    pub signature: [u8; 4],
    pub length: U32<LE>,
    pub hardware_signature: U32<LE>,
    pub waking_vector: U32<LE>,
}

const _: () = assert!(core::mem::size_of::<Rsdp>() == 36);
const _: () = assert!(core::mem::size_of::<SdtHeader>() == 36);
const _: () = assert!(core::mem::size_of::<FadtPrefix>() == 140);
const _: () = assert!(core::mem::size_of::<FadtV2Prefix>() == 148);
const _: () = assert!(core::mem::size_of::<FacsPrefix>() == 16);

fn checksum_ok(bytes: &[u8]) -> bool {
    fstart_core::checksum::sum8(bytes) == 0
}

fn parse_rsdp(bytes: &[u8]) -> Option<Rsdp> {
    let (rsdp, _) = Rsdp::read_from_prefix(bytes).ok()?;
    (rsdp.signature == *b"RSD PTR "
        && rsdp.revision >= 2
        && rsdp.length.get() as usize == core::mem::size_of::<Rsdp>()
        && checksum_ok(bytes.get(..RSDP_V1_LEN)?)
        && checksum_ok(bytes.get(..core::mem::size_of::<Rsdp>())?)
        && rsdp.xsdt_addr.get() != 0)
        .then_some(rsdp)
}

fn read_sdt(
    read: &impl Fn(u64, &mut [u8]) -> bool,
    address: u64,
    signature: [u8; 4],
    bytes: &mut [u8; MAX_SDT_LEN],
) -> Option<usize> {
    if !read(address, &mut bytes[..core::mem::size_of::<SdtHeader>()]) {
        return None;
    }
    let (header, _) = SdtHeader::read_from_prefix(bytes).ok()?;
    let length = usize::try_from(header.length.get()).ok()?;
    if header.signature != signature
        || !(core::mem::size_of::<SdtHeader>()..=MAX_SDT_LEN).contains(&length)
    {
        return None;
    }
    if !read(address, &mut bytes[..length]) || !checksum_ok(&bytes[..length]) {
        return None;
    }
    Some(length)
}

/// Validate the RSDP at `rsdp_addr` without walking the rest of the table set.
#[must_use]
pub fn rsdp_is_valid_with(read: &impl Fn(u64, &mut [u8]) -> bool, rsdp_addr: u64) -> bool {
    let mut bytes = [0u8; core::mem::size_of::<Rsdp>()];
    read(rsdp_addr, &mut bytes) && parse_rsdp(&bytes).is_some()
}

fn find_fadt_in_xsdt(read: &impl Fn(u64, &mut [u8]) -> bool, xsdt_addr: u64) -> Option<u64> {
    let mut bytes = [0u8; MAX_SDT_LEN];
    let length = read_sdt(read, xsdt_addr, *b"XSDT", &mut bytes)?;
    let (entries, remainder) = bytes[core::mem::size_of::<SdtHeader>()..length].as_chunks::<8>();
    if !remainder.is_empty() {
        return None;
    }
    entries
        .iter()
        .filter_map(|bytes| {
            XsdtEntry::read_from_prefix(bytes)
                .ok()
                .map(|(entry, _)| entry.0.get())
        })
        .filter(|address| *address != 0)
        .find(|address| {
            let mut signature = [0u8; 4];
            read(*address, &mut signature) && signature == *b"FACP"
        })
}

fn fadt_facs_addr(read: &impl Fn(u64, &mut [u8]) -> bool, address: u64) -> Option<u64> {
    let mut bytes = [0u8; MAX_SDT_LEN];
    let length = read_sdt(read, address, *b"FACP", &mut bytes)?;
    let (fadt, _) = FadtPrefix::read_from_prefix(bytes.get(..length)?).ok()?;
    match fadt.x_firmware_ctrl.get() {
        0 => Some(u64::from(fadt.firmware_ctrl.get())).filter(|address| *address != 0),
        address => Some(address),
    }
}

fn facs_wake_vector(read: &impl Fn(u64, &mut [u8]) -> bool, address: u64) -> Option<u32> {
    let mut bytes = [0u8; core::mem::size_of::<FacsPrefix>()];
    if !read(address, &mut bytes) {
        return None;
    }
    let (facs, _) = FacsPrefix::read_from_prefix(&bytes).ok()?;
    (facs.signature == *b"FACS" && facs.length.get() as usize >= core::mem::size_of::<FacsPrefix>())
        .then(|| facs.waking_vector.get())
        .filter(|vector| *vector != 0)
}

/// Follow an ACPI table chain starting at `rsdp_addr` to the OS wake vector.
///
/// `read(addr, buf)` copies physical memory into `buf`. Every externally sized
/// table is bounded and checksum-validated before its fields are consumed.
#[must_use]
pub fn wakeup_vector_from_rsdp_with(
    read: &impl Fn(u64, &mut [u8]) -> bool,
    rsdp_addr: u64,
) -> Option<u32> {
    let mut bytes = [0u8; core::mem::size_of::<Rsdp>()];
    if !read(rsdp_addr, &mut bytes) {
        return None;
    }
    let rsdp = parse_rsdp(&bytes)?;
    let fadt_addr = find_fadt_in_xsdt(read, rsdp.xsdt_addr.get())?;
    let facs_addr = fadt_facs_addr(read, fadt_addr)?;
    facs_wake_vector(read, facs_addr)
}
