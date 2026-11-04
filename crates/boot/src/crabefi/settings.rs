//! Firmware settings through CrabEFI: the variable store as the option
//! storage, and CrabEFI's CFR menu.
//!
//! The settings live where CrabEFI keeps its EFI variables: an EDK2
//! variable store in a reserved flash region. Each option is a variable
//! under coreboot's CFR GUID named by its [`fstart_options::Meta::key`],
//! holding a little-endian `u32`, as coreboot's `get_uint_option()` and
//! CrabEFI's CFR menu store them. The boot reads them through
//! `fstart_options::varstore` from the memory-mapped region; CrabEFI's menu
//! writes them through [`FlashStorage`] and the runtime image.
//!
//! A platform without a flash store may keep the options in CMOS instead
//! ([`fstart_options::cmos`]): the menu then reads and writes them there.

use alloc::vec::Vec;

use fstart_core::services::{FlashRegion, ServiceError};
use fstart_options::Source;
use fstart_options::cmos::{Cmos, Ram};

/// What CrabEFI gets to persist variables and show the settings.
pub struct FirmwareSettings {
    /// The CFR record of the options ([`fstart_options::cfr::encode`]).
    pub cfr: Vec<u8>,
    /// The options the board keeps in CMOS.
    pub cmos: Option<Cmos<&'static (dyn Ram + Sync)>>,
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

/// The menu's view of options kept in CMOS.
struct CmosStore(Cmos<&'static (dyn Ram + Sync)>);

impl crabefi::cfr::OptionStore for CmosStore {
    fn keeps(&self, _option: &crabefi::cfr::CfrOption) -> bool {
        true
    }

    fn read(&self, option: &crabefi::cfr::CfrOption) -> Option<crabefi::cfr::CfrValue> {
        use crabefi::cfr::{CfrOptionType, CfrValue};
        let value = self.0.read(&option.opt_name).ok()??;
        Some(match option.option_type {
            CfrOptionType::Bool { .. } => CfrValue::Bool(value != 0),
            _ => CfrValue::Number(value),
        })
    }

    fn write(
        &self,
        option: &crabefi::cfr::CfrOption,
        value: &crabefi::cfr::CfrValue,
    ) -> Result<(), &'static str> {
        use crabefi::cfr::CfrValue;
        let value = match *value {
            CfrValue::Bool(b) => u32::from(b),
            CfrValue::Number(n) => n,
            CfrValue::Varchar(_) => return Err("CMOS keeps numbers only"),
        };
        self.0
            .write(&option.opt_name, value)
            .map_err(|error| match error {
                fstart_options::cmos::WriteError::Full => "CMOS option space full",
                fstart_options::cmos::WriteError::Readback => "CMOS write failed",
            })
    }
}

/// Parse and keep `cfr` for the menu, and its CMOS store. Called once,
/// before CrabEFI starts.
pub(crate) fn install(settings: &mut FirmwareSettings) -> bool {
    let Some(cfr) = crabefi::cfr::parse_cfr(&settings.cfr) else {
        return false;
    };
    crabefi::cfr::store_cfr(cfr);
    if let Some(cmos) = settings.cmos.take() {
        crabefi::cfr::set_option_store(alloc::boxed::Box::leak(alloc::boxed::Box::new(CmosStore(
            cmos,
        ))));
    }
    true
}
