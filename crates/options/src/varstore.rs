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

/// A variable store of `size` bytes, a multiple of 4 KiB, with `options`
/// stored as CrabEFI's menu writes them: each a non-volatile variable
/// under the CFR GUID named by its key, holding a little-endian `u32`, in
/// an authenticated-format store. What a build seeds a settings store
/// with. `None` if a key is not short ASCII or the options do not fit.
#[cfg(feature = "alloc")]
pub fn store(size: usize, options: &[(&str, u32)]) -> Option<alloc::vec::Vec<u8>> {
    use varstore::{
        AUTH_VAR_HEADER_SIZE, COREBOOT_CFR_GUID, EFI_AUTH_VARIABLE_GUID,
        EFI_SYSTEM_NV_DATA_FV_GUID, FV_HEADER_LENGTH, FV_REVISION, FV_SIGNATURE, HEADER_ALIGNMENT,
        VAR_ADDED, VARIABLE_DATA, VARIABLE_STORE_FORMATTED, VARIABLE_STORE_HEALTHY,
    };
    const BLOCK: usize = 0x1000;
    /// Non-volatile, boot-service and runtime access.
    const ATTRIBUTES: u32 = 0x7;
    if size == 0 || !size.is_multiple_of(BLOCK) || u32::try_from(size).is_err() {
        return None;
    }
    let mut out = alloc::vec![0xffu8; size];
    // The firmware volume header, its block map and the terminating entry.
    let fv = &mut out[..FV_HEADER_LENGTH];
    fv[..0x10].fill(0);
    fv[0x10..0x20].copy_from_slice(&EFI_SYSTEM_NV_DATA_FV_GUID);
    fv[0x20..0x28].copy_from_slice(&(size as u64).to_le_bytes());
    fv[0x28..0x2c].copy_from_slice(&FV_SIGNATURE.to_le_bytes());
    // EDK2's attributes of a variable firmware volume.
    fv[0x2c..0x30].copy_from_slice(&0x0004_feffu32.to_le_bytes());
    fv[0x30..0x32].copy_from_slice(&(FV_HEADER_LENGTH as u16).to_le_bytes());
    fv[0x32..0x37].fill(0);
    fv[0x37] = FV_REVISION;
    fv[0x38..0x3c].copy_from_slice(&((size / BLOCK) as u32).to_le_bytes());
    fv[0x3c..0x40].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    fv[0x40..0x48].fill(0);
    let sum = fv.as_chunks::<2>().0.iter().fold(0u16, |sum, &word| {
        sum.wrapping_add(u16::from_le_bytes(word))
    });
    fv[0x32..0x34].copy_from_slice(&sum.wrapping_neg().to_le_bytes());
    // The variable store header; its size includes it (PI).
    let vs = &mut out[FV_HEADER_LENGTH..FV_HEADER_LENGTH + 0x1c];
    vs[..0x10].copy_from_slice(&EFI_AUTH_VARIABLE_GUID);
    vs[0x10..0x14].copy_from_slice(&((size - FV_HEADER_LENGTH) as u32).to_le_bytes());
    vs[0x14] = VARIABLE_STORE_FORMATTED;
    vs[0x15] = VARIABLE_STORE_HEALTHY;
    vs[0x16..0x1c].fill(0);
    let mut at = FV_HEADER_LENGTH + 0x1c;
    for &(key, value) in options {
        if key.is_empty() || key.len() > MAX_KEY || !key.is_ascii() {
            return None;
        }
        let name: alloc::vec::Vec<u8> = key
            .bytes()
            .chain(core::iter::once(0))
            .flat_map(|byte| u16::from(byte).to_le_bytes())
            .collect();
        let record = AUTH_VAR_HEADER_SIZE + name.len() + 4;
        let end = at + record;
        if end > size {
            return None;
        }
        let header = &mut out[at..at + AUTH_VAR_HEADER_SIZE];
        header.fill(0);
        header[..2].copy_from_slice(&VARIABLE_DATA.to_le_bytes());
        header[2] = VAR_ADDED;
        header[4..8].copy_from_slice(&ATTRIBUTES.to_le_bytes());
        header[0x24..0x28].copy_from_slice(&(name.len() as u32).to_le_bytes());
        header[0x28..0x2c].copy_from_slice(&4u32.to_le_bytes());
        header[0x2c..0x3c].copy_from_slice(&COREBOOT_CFR_GUID);
        let name_at = at + AUTH_VAR_HEADER_SIZE;
        out[name_at..name_at + name.len()].copy_from_slice(&name);
        out[name_at + name.len()..end].copy_from_slice(&value.to_le_bytes());
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
            &[("s8030_pcie3_bifurcation", 4), ("debug_level", 7)],
        )
        .unwrap();
        let store = VarstoreOptions::new(&bytes);
        assert_eq!(store.name(), "flash variable store");
        assert_eq!(store.read("s8030_pcie3_bifurcation"), Ok(Some(4)));
        assert_eq!(store.read("debug_level"), Ok(Some(7)));
        assert_eq!(store.read("other"), Ok(None));
        assert!(super::store(0x4_0001, &[]).is_none());
        assert!(super::store(0x1000, &[("é", 1)]).is_none());
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
