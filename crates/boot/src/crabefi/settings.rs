//! Firmware settings through CrabEFI: the variable store as the option
//! source, a flash region as its storage, and CrabEFI's CFR menu.
//!
//! The settings live where CrabEFI keeps its EFI variables: an EDK2
//! variable store in a reserved flash region. Each option is a variable
//! under coreboot's CFR GUID named by its [`fstart_options::Meta::key`],
//! holding a little-endian `u32`, as coreboot's `get_uint_option()` and
//! CrabEFI's CFR menu store them. The boot reads them through
//! [`VarstoreOptions`] from the memory-mapped region; CrabEFI's menu writes
//! them through [`FlashStorage`] and the runtime image.

use alloc::vec::Vec;

use crabefi::efi::varstore::edk2;
use fstart_core::services::{FlashRegion, ServiceError};
use fstart_options::{Source, SourceError};

/// What CrabEFI gets to persist variables and show the settings.
pub struct FirmwareSettings {
    /// The CFR record of the options ([`fstart_options::cfr::encode`]).
    pub cfr: Vec<u8>,
}

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
        let header = edk2::FV_HEADER_LENGTH + edk2::VS_HEADER_LENGTH;
        let state = match region.get(..header) {
            Some(bytes) if bytes.iter().all(|&b| b == 0xff) => Store::Erased,
            Some(bytes) if edk2::validate_fv(bytes, region.len() as u32).valid => Store::Valid,
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
        let guid = crabefi::cfr::COREBOOT_CFR_GUID;
        Ok(edk2::read_variable_data_from_region(
            self.region,
            guid.as_bytes(),
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

/// A [`FlashRegion`] as CrabEFI's variable storage. The region's erase
/// block size and whether it may be written are the platform's: CrabEFI
/// formats an erased region and never erases outside it.
pub struct FlashStorage<F: FlashRegion + Send> {
    region: F,
    size: u32,
    erase_size: u32,
    writable: bool,
}

impl<F: FlashRegion + Send> FlashStorage<F> {
    pub fn new(region: F, size: u32, erase_size: u32, writable: bool) -> Self {
        Self {
            region,
            size,
            erase_size,
            writable,
        }
    }
}

fn storage_error(error: ServiceError) -> crabefi::StorageError {
    match error {
        ServiceError::InvalidParam => crabefi::StorageError::InvalidArgument,
        ServiceError::NotSupported => crabefi::StorageError::WriteProtected,
        ServiceError::Timeout => crabefi::StorageError::Timeout,
        ServiceError::NotInitialized => crabefi::StorageError::NotInitialized,
        _ => crabefi::StorageError::IoError,
    }
}

impl<F: FlashRegion + Send> crabefi::StorageBackend for FlashStorage<F> {
    fn name(&self) -> &str {
        "fstart flash region"
    }
    fn size(&self) -> u32 {
        self.size
    }
    fn program_granularity(&self) -> u32 {
        1
    }
    fn erase_granularity(&self) -> u32 {
        self.erase_size
    }
    fn is_write_protected(&self) -> bool {
        !self.writable
    }
    fn read(&mut self, offset: u32, buffer: &mut [u8]) -> Result<(), crabefi::StorageError> {
        self.region.read(offset, buffer).map_err(storage_error)
    }
    fn program(&mut self, offset: u32, data: &[u8]) -> Result<(), crabefi::StorageError> {
        self.region.program(offset, data).map_err(storage_error)
    }
    fn erase(&mut self, offset: u32, size: u32) -> Result<(), crabefi::StorageError> {
        self.region.erase(offset, size).map_err(storage_error)
    }
}

/// CrabEFI's firmware-settings entry: the CFR menu over the stored record.
pub(crate) struct SettingsHooks;

impl crabefi::PlatformHooks for SettingsHooks {
    fn firmware_settings_available(&self) -> bool {
        crabefi::cfr::get_cfr().is_some()
    }

    fn show_firmware_settings(&self) -> bool {
        if crabefi::cfr::get_cfr().is_none() {
            return false;
        }
        crabefi::cfr_menu::show_cfr_menu();
        true
    }
}

/// Parse and keep `cfr` for the menu. Called once, before CrabEFI starts.
pub(crate) fn install(settings: &FirmwareSettings) -> bool {
    match crabefi::cfr::parse_cfr(&settings.cfr) {
        Some(cfr) => {
            crabefi::cfr::store_cfr(cfr);
            true
        }
        None => false,
    }
}
