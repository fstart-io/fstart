//! Options in an EDK2 variable store: each a variable under coreboot's
//! CFR GUID named by its [`Meta::key`](crate::Meta::key), holding a
//! little-endian `u32`, as coreboot's `get_uint_option()` reads them and
//! CrabEFI's firmware settings menu writes them. Reading needs only the
//! mapped region, so every payload's boot honours them.

use crabefi_efi_types::varstore;

use crate::{Source, SourceError};

/// The longest option key, in characters.
const MAX_KEY: usize = 63;

/// The option overrides stored in a memory-mapped EDK2 variable store.
pub struct VarstoreOptions<'a> {
    region: &'a [u8],
    state: Store,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Store {
    /// Never written: no overrides.
    Erased,
    Valid,
    Corrupt,
}

impl<'a> VarstoreOptions<'a> {
    /// The store in `region` (the whole reserved region, as mapped).
    pub fn new(region: &'a [u8]) -> Self {
        let header = varstore::FV_HEADER_LENGTH + varstore::VS_HEADER_LENGTH;
        let state = match region.get(..header) {
            Some(bytes) if bytes.iter().all(|&b| b == 0xff) => Store::Erased,
            Some(bytes) if varstore::validate(bytes, region.len() as u32).is_some() => Store::Valid,
            _ => Store::Corrupt,
        };
        Self { region, state }
    }
}

impl Source for VarstoreOptions<'_> {
    fn name(&self) -> &'static str {
        match self.state {
            Store::Erased => "flash variable store (empty)",
            Store::Valid => "flash variable store",
            Store::Corrupt => "flash variable store (corrupt)",
        }
    }

    fn read(&self, key: &str) -> Result<Option<u32>, SourceError> {
        match self.state {
            Store::Erased => return Ok(None),
            Store::Corrupt => return Err(SourceError::Corrupt),
            Store::Valid => {}
        }
        let mut name = [0u16; MAX_KEY + 1];
        if key.len() > MAX_KEY || !key.is_ascii() {
            return Err(SourceError::Corrupt);
        }
        for (unit, byte) in name.iter_mut().zip(key.bytes()) {
            *unit = u16::from(byte);
        }
        let mut data = [0u8; 4];
        Ok(varstore::read_variable_data_from_region(
            self.region,
            &varstore::COREBOOT_CFR_GUID,
            &name[..key.len()],
            &mut data,
        )
        .and_then(|len| match len {
            // The menu writes 4 bytes; coreboot also reads a 1-byte value.
            4 => Some(u32::from_le_bytes(data)),
            1 => Some(u32::from(data[0])),
            _ => None,
        }))
    }
}

/// A variable a new store starts with ([`store`]).
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable<'a> {
    pub guid: [u8; 16],
    /// The name, short ASCII.
    pub name: &'a str,
    pub attributes: u32,
    pub data: alloc::vec::Vec<u8>,
}

#[cfg(feature = "alloc")]
impl<'a> Variable<'a> {
    /// Non-volatile, boot-service and runtime access.
    pub const ATTRIBUTES: u32 = 0x7;

    /// Option `key` at `value`, as CrabEFI's menu writes it: under the CFR
    /// GUID, a little-endian `u32`.
    pub fn option(key: &'a str, value: u32) -> Self {
        Self {
            guid: varstore::COREBOOT_CFR_GUID,
            name: key,
            attributes: Self::ATTRIBUTES,
            data: value.to_le_bytes().to_vec(),
        }
    }
}

#[cfg(feature = "alloc")]
mod wire {
    use zerocopy::little_endian::{U16, U32, U64};
    use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

    #[repr(C)]
    #[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
    pub(super) struct FirmwareVolumeHeader {
        pub zero_vector: [u8; 16],
        pub file_system_guid: [u8; 16],
        pub fv_length: U64,
        pub signature: U32,
        pub attributes: U32,
        pub header_length: U16,
        pub checksum: U16,
        pub ext_header_offset: U16,
        pub reserved: u8,
        pub revision: u8,
    }

    #[repr(C)]
    #[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
    pub(super) struct BlockMapEntry {
        pub num_blocks: U32,
        pub length: U32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
    pub(super) struct FvHeaderWithBlockMap {
        pub header: FirmwareVolumeHeader,
        pub blocks: BlockMapEntry,
        pub terminator: BlockMapEntry,
    }

    #[repr(C)]
    #[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
    pub(super) struct VariableStoreHeader {
        pub signature: [u8; 16],
        pub size: U32,
        pub format: u8,
        pub state: u8,
        pub reserved: U16,
        pub reserved1: U32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
    pub(super) struct AuthenticatedVariableHeader {
        pub start_id: U16,
        pub state: u8,
        pub reserved: u8,
        pub attributes: U32,
        pub monotonic_count: U64,
        pub timestamp: [u8; 16],
        pub pubkey_index: U32,
        pub name_size: U32,
        pub data_size: U32,
        pub vendor_guid: [u8; 16],
    }

    const _: () = assert!(core::mem::size_of::<FvHeaderWithBlockMap>() == 0x48);
    const _: () = assert!(core::mem::size_of::<VariableStoreHeader>() == 0x1c);
    const _: () = assert!(core::mem::size_of::<AuthenticatedVariableHeader>() == 0x3c);
}

/// A variable store of `size` bytes, a multiple of 4 KiB, holding
/// `variables` in an authenticated-format store, as CrabEFI keeps it. What
/// a build seeds a settings store with: options ([`Variable::option`]) and
/// Secure Boot's databases. Timestamps are zero, so any later authenticated
/// update is newer. `None` if a name is not short ASCII or the variables
/// do not fit.
#[cfg(feature = "alloc")]
pub fn store(size: usize, variables: &[Variable]) -> Option<alloc::vec::Vec<u8>> {
    use varstore::{
        AUTH_VAR_HEADER_SIZE, EFI_AUTH_VARIABLE_GUID, EFI_SYSTEM_NV_DATA_FV_GUID, FV_HEADER_LENGTH,
        FV_REVISION, FV_SIGNATURE, HEADER_ALIGNMENT, VAR_ADDED, VARIABLE_DATA,
        VARIABLE_STORE_FORMATTED, VARIABLE_STORE_HEALTHY, VS_HEADER_LENGTH,
    };
    use wire::{
        AuthenticatedVariableHeader, BlockMapEntry, FirmwareVolumeHeader, FvHeaderWithBlockMap,
        VariableStoreHeader,
    };
    use zerocopy::little_endian::{U16, U32, U64};
    use zerocopy::{FromBytes, IntoBytes};

    const BLOCK: usize = 0x1000;
    if size == 0 || !size.is_multiple_of(BLOCK) || u32::try_from(size).is_err() {
        return None;
    }
    let mut out = alloc::vec![0xffu8; size];
    // The firmware volume header, its block map and the terminating entry.
    let mut fv_wire = FvHeaderWithBlockMap {
        header: FirmwareVolumeHeader {
            zero_vector: [0; 16],
            file_system_guid: EFI_SYSTEM_NV_DATA_FV_GUID,
            fv_length: U64::new(size as u64),
            signature: U32::new(FV_SIGNATURE),
            // EDK2's attributes of a variable firmware volume.
            attributes: U32::new(0x0004_feff),
            header_length: U16::new(FV_HEADER_LENGTH as u16),
            checksum: U16::ZERO,
            ext_header_offset: U16::ZERO,
            reserved: 0,
            revision: FV_REVISION,
        },
        blocks: BlockMapEntry {
            num_blocks: U32::new((size / BLOCK) as u32),
            length: U32::new(BLOCK as u32),
        },
        terminator: BlockMapEntry {
            num_blocks: U32::ZERO,
            length: U32::ZERO,
        },
    };
    let words = <[U16]>::ref_from_bytes(fv_wire.as_bytes()).ok()?;
    let sum = words
        .iter()
        .fold(0u16, |acc, word| acc.wrapping_add(word.get()));
    fv_wire.header.checksum = U16::new(sum.wrapping_neg());
    out[..FV_HEADER_LENGTH].copy_from_slice(fv_wire.as_bytes());

    // The variable store header; its size includes it (PI).
    let vs_wire = VariableStoreHeader {
        signature: EFI_AUTH_VARIABLE_GUID,
        size: U32::new((size - FV_HEADER_LENGTH) as u32),
        format: VARIABLE_STORE_FORMATTED,
        state: VARIABLE_STORE_HEALTHY,
        reserved: U16::ZERO,
        reserved1: U32::ZERO,
    };
    out[FV_HEADER_LENGTH..FV_HEADER_LENGTH + VS_HEADER_LENGTH].copy_from_slice(vs_wire.as_bytes());

    let mut at = FV_HEADER_LENGTH + VS_HEADER_LENGTH;
    for variable in variables {
        let key = variable.name;
        if key.is_empty() || key.len() > MAX_KEY || !key.is_ascii() {
            return None;
        }
        let name: alloc::vec::Vec<u8> = key
            .bytes()
            .chain(core::iter::once(0))
            .flat_map(|byte| U16::new(u16::from(byte)).as_bytes().to_vec())
            .collect();
        let data = &variable.data;
        let record = AUTH_VAR_HEADER_SIZE + name.len() + data.len();
        let end = at.checked_add(record)?;
        if end > size {
            return None;
        }
        let header = AuthenticatedVariableHeader {
            start_id: U16::new(VARIABLE_DATA),
            state: VAR_ADDED,
            reserved: 0,
            attributes: U32::new(variable.attributes),
            monotonic_count: U64::ZERO,
            timestamp: [0; 16],
            pubkey_index: U32::ZERO,
            name_size: U32::new(name.len() as u32),
            data_size: U32::new(u32::try_from(data.len()).ok()?),
            vendor_guid: variable.guid,
        };
        out[at..at + AUTH_VAR_HEADER_SIZE].copy_from_slice(header.as_bytes());
        let name_at = at + AUTH_VAR_HEADER_SIZE;
        out[name_at..name_at + name.len()].copy_from_slice(&name);
        out[name_at + name.len()..end].copy_from_slice(data);
        at += varstore::align_up(record as u32, HEADER_ALIGNMENT) as usize;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "alloc")]
    #[test]
    fn a_built_store_reads_back() {
        let bytes = store(
            0x4_0000,
            &[
                Variable::option("s8030_pcie3_bifurcation", 4),
                // Another variable, of a size no option has, in between.
                Variable {
                    guid: [0x5a; 16],
                    name: "PK",
                    attributes: 0x27,
                    data: alloc::vec![0xaa; 1001],
                },
                Variable::option("debug_level", 7),
            ],
        )
        .unwrap();
        let store = VarstoreOptions::new(&bytes);
        assert_eq!(store.name(), "flash variable store");
        assert_eq!(store.read("s8030_pcie3_bifurcation"), Ok(Some(4)));
        assert_eq!(store.read("debug_level"), Ok(Some(7)));
        assert_eq!(store.read("other"), Ok(None));
        assert!(super::store(0x4_0001, &[]).is_none());
        assert!(super::store(0x1000, &[Variable::option("é", 1)]).is_none());
    }

    #[test]
    fn an_erased_store_has_no_overrides_and_garbage_is_corrupt() {
        let erased = [0xffu8; 0x1000];
        let store = VarstoreOptions::new(&erased);
        assert_eq!(store.read("debug_level"), Ok(None));
        let garbage = [0x5au8; 0x1000];
        let store = VarstoreOptions::new(&garbage);
        assert_eq!(store.read("debug_level"), Err(SourceError::Corrupt));
        assert_eq!(store.name(), "flash variable store (corrupt)");
    }
}
