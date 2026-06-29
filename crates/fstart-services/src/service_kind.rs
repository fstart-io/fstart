//! Build-time service identifiers for driver metadata.
//!
//! Runtime services are represented by traits in this crate. `ServiceKind` is the
//! compact build-time companion used by board metadata and host tooling to say
//! which runtime service traits a configured driver provides.

use serde::{Deserialize, Serialize};

/// Runtime service traits a configured driver can provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceKind {
    Console,
    BlockDevice,
    ClockController,
    MemoryController,
    PciRootBus,
    PciHost,
    SmmOps,
    Framebuffer,
    AcpiTableProvider,
    X86AcpiPlatformProvider,
    MemoryDetector,
    SuperIoHost,
    Southbridge,
    Mainboard,
    PreConsoleInit,
    EarlyInit,
    StageLocalInit,
    PostDramInit,
    FinalizeInit,
    FlashLayoutVerifier,
    FirmwareImageProvider,
    I2cBus,
    SpiBus,
    GpioController,
    SystemManagementBus,
}

impl ServiceKind {
    /// All known service variants in stable display order.
    pub const ALL: &'static [Self] = &[
        Self::Console,
        Self::BlockDevice,
        Self::ClockController,
        Self::MemoryController,
        Self::PciRootBus,
        Self::PciHost,
        Self::SmmOps,
        Self::Framebuffer,
        Self::AcpiTableProvider,
        Self::X86AcpiPlatformProvider,
        Self::MemoryDetector,
        Self::SuperIoHost,
        Self::Southbridge,
        Self::Mainboard,
        Self::PreConsoleInit,
        Self::EarlyInit,
        Self::StageLocalInit,
        Self::PostDramInit,
        Self::FinalizeInit,
        Self::FlashLayoutVerifier,
        Self::FirmwareImageProvider,
        Self::I2cBus,
        Self::SpiBus,
        Self::GpioController,
        Self::SystemManagementBus,
    ];

    /// Stable service name used in diagnostics and metadata reports.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Console => "Console",
            Self::BlockDevice => "BlockDevice",
            Self::ClockController => "ClockController",
            Self::MemoryController => "MemoryController",
            Self::PciRootBus => "PciRootBus",
            Self::PciHost => "PciHost",
            Self::SmmOps => "SmmOps",
            Self::Framebuffer => "Framebuffer",
            Self::AcpiTableProvider => "AcpiTableProvider",
            Self::X86AcpiPlatformProvider => "X86AcpiPlatformProvider",
            Self::MemoryDetector => "MemoryDetector",
            Self::SuperIoHost => "SuperIoHost",
            Self::Southbridge => "Southbridge",
            Self::Mainboard => "Mainboard",
            Self::PreConsoleInit => "PreConsoleInit",
            Self::EarlyInit => "EarlyInit",
            Self::StageLocalInit => "StageLocalInit",
            Self::PostDramInit => "PostDramInit",
            Self::FinalizeInit => "FinalizeInit",
            Self::FlashLayoutVerifier => "FlashLayoutVerifier",
            Self::FirmwareImageProvider => "FirmwareImageProvider",
            Self::I2cBus => "I2cBus",
            Self::SpiBus => "SpiBus",
            Self::GpioController => "GpioController",
            Self::SystemManagementBus => "SmBus",
        }
    }

    const fn bit(self) -> u128 {
        1u128 << (self as u8)
    }
}

/// Compact set of driver-provided services used by board metadata and tooling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServiceSet(u128);

impl ServiceSet {
    /// Construct an empty service set.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Construct a service set from static driver metadata.
    pub const fn from_static(services: &'static [ServiceKind]) -> Self {
        let mut idx = 0;
        let mut bits = 0;
        while idx < services.len() {
            bits |= services[idx].bit();
            idx += 1;
        }
        Self(bits)
    }

    /// Insert a service into the set.
    pub fn insert(&mut self, service: ServiceKind) {
        self.0 |= service.bit();
    }

    /// Return a copy of this set with `service` inserted.
    pub const fn with(self, service: ServiceKind) -> Self {
        Self(self.0 | service.bit())
    }

    /// Remove a service from the set.
    pub fn remove(&mut self, service: ServiceKind) {
        self.0 &= !service.bit();
    }

    /// Return true if the set contains `service`.
    pub const fn contains(self, service: ServiceKind) -> bool {
        self.0 & service.bit() != 0
    }

    /// Iterate over services present in this set.
    pub fn iter(self) -> impl Iterator<Item = ServiceKind> {
        ServiceKind::ALL
            .iter()
            .copied()
            .filter(move |service| self.contains(*service))
    }

    /// Return true if the set contains no services.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}
