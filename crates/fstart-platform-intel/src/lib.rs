//! Shared Intel platform machinery and chipset-specific handwritten flows.
//!
//! Intel-common code lives here only when it is shared by multiple Intel
//! chipsets. Ordering stays in each chipset module's handwritten flow.

#![no_std]

#[cfg(feature = "stage")]
extern crate ufmt;

#[cfg(any(feature = "acpi", feature = "smbios"))]
pub mod tables;

pub mod gm965;

#[cfg(feature = "stage")]
use fstart_core::services::memory_detect::{E820Entry, MAX_E820_ENTRIES};
#[cfg(feature = "stage")]
use fstart_core::services::ServiceError;
#[cfg(feature = "stage")]
pub use fstart_stage::{payload::MainstagePayload, StageBoard, StageKind};

/// Marker for Intel platform families with handwritten early flows.
#[cfg(feature = "stage")]
pub trait IntelPlatform {}

/// Board contract common to Intel handwritten early flows.
#[cfg(feature = "stage")]
pub trait IntelEarlyBoard: StageBoard {
    type Platform: IntelEarlyPlatform;
    type Hooks: IntelEarlyBoardHooks<Self::Platform>;

    fn hooks() -> Result<Self::Hooks, ServiceError>;
}

/// Fixed Intel early-flow platform contract.
#[cfg(feature = "stage")]
pub trait IntelEarlyPlatform: IntelPlatform {
    type Southbridge;
    type State: Default;
    /// Platform-owned ACPI namespace context handed to `AcpiDevice` emitters.
    #[cfg(feature = "acpi")]
    type AcpiContext;
}

/// Mainboard hooks contribute ACPI fragments through the same [`AcpiDevice`]
/// abstraction chipset drivers use. Vacuous when ACPI is disabled.
///
/// [`AcpiDevice`]: fstart_acpi::device::AcpiDevice
#[cfg(all(feature = "stage", feature = "acpi"))]
pub trait MainboardAcpi<P: IntelEarlyPlatform>:
    fstart_acpi::device::AcpiDevice<Config = P::AcpiContext>
{
}
#[cfg(all(feature = "stage", feature = "acpi"))]
impl<P, T> MainboardAcpi<P> for T
where
    P: IntelEarlyPlatform,
    T: fstart_acpi::device::AcpiDevice<Config = P::AcpiContext>,
{
}
#[cfg(all(feature = "stage", not(feature = "acpi")))]
pub trait MainboardAcpi<P> {}
#[cfg(all(feature = "stage", not(feature = "acpi")))]
impl<P, T> MainboardAcpi<P> for T {}

/// Mutable context passed to board hooks.
#[cfg(feature = "stage")]
pub struct IntelEarlyCtx<'a, P: IntelEarlyPlatform> {
    southbridge: &'a mut P::Southbridge,
}

#[cfg(feature = "stage")]
impl<'a, P: IntelEarlyPlatform> IntelEarlyCtx<'a, P> {
    pub(crate) fn new(southbridge: &'a mut P::Southbridge) -> Self {
        Self { southbridge }
    }

    #[must_use]
    pub fn southbridge(&mut self) -> &mut P::Southbridge {
        self.southbridge
    }
}

/// Board hooks at the fixed Intel early-flow seams.
#[cfg(feature = "stage")]
pub trait IntelEarlyBoardHooks<P: IntelEarlyPlatform>: MainboardAcpi<P> {
    fn before_console(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn before_memory(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn after_memory(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn before_handoff(&mut self, _ctx: &mut IntelEarlyCtx<P>) -> Result<(), ServiceError> {
        Ok(())
    }
}

/// Shared mainstage state owned by the flow, not by board hooks or drivers.
#[cfg(feature = "stage")]
pub struct MainstageCtx {
    e820: [E820Entry; MAX_E820_ENTRIES],
    e820_count: usize,
    total_ram: u64,
    acpi_rsdp: Option<u64>,
    firmware_base: u64,
    firmware_size: usize,
}

#[cfg(feature = "stage")]
impl MainstageCtx {
    pub(crate) fn new(firmware_base: u64, firmware_size: usize) -> Self {
        Self {
            e820: [E820Entry::zeroed(); MAX_E820_ENTRIES],
            e820_count: 0,
            total_ram: 0,
            acpi_rsdp: None,
            firmware_base,
            firmware_size,
        }
    }

    #[must_use]
    pub fn e820(&self) -> &[E820Entry] {
        &self.e820[..self.e820_count]
    }

    #[must_use]
    pub const fn total_ram(&self) -> u64 {
        self.total_ram
    }

    #[must_use]
    pub const fn acpi_rsdp(&self) -> Option<u64> {
        self.acpi_rsdp
    }

    #[must_use]
    pub const fn firmware_region(&self) -> (u64, usize) {
        (self.firmware_base, self.firmware_size)
    }

    pub(crate) fn e820_mut(&mut self) -> &mut [E820Entry; MAX_E820_ENTRIES] {
        &mut self.e820
    }

    pub(crate) fn store_e820(&mut self, count: usize, total: u64) {
        self.e820_count = count;
        self.total_ram = total;
    }

    #[cfg(feature = "acpi")]
    pub(crate) fn set_acpi_rsdp(&mut self, rsdp: Option<u64>) {
        self.acpi_rsdp = rsdp;
    }
}

/// Phase-oriented contract for DRAM-backed mainstage flows.
#[cfg(feature = "stage")]
pub trait MainstagePhases: Sized {
    fn bind() -> Result<Self, ServiceError>;
    fn pre_bus_scan(&mut self) -> Result<(), ServiceError>;
    fn bus_scan(&mut self) -> Result<(), ServiceError>;
    fn init_devices(&mut self) -> Result<(), ServiceError>;
    fn emit_tables(&mut self) -> Result<(), ServiceError>;
    fn finalize(&mut self) -> Result<(), ServiceError>;
}
