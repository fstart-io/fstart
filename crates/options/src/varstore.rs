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

#[cfg(test)]
mod tests {
    use super::*;

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
