//! Core types for the fstart firmware framework.
//!
//! These types define the Rust board configuration schema, firmware filesystem
//! structures, and security primitives.
//!
//! `no_std` by default — uses `heapless` collections for bounded containers.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod acpi;
pub mod board;
pub mod builder;
pub mod device;
pub mod ffs;
pub mod handoff;
pub mod memory;
pub mod security;
pub mod smbios;
pub mod smm;
pub mod stage;
pub mod typed;

pub use acpi::{
    AcpiAhciDevice, AcpiConfig, AcpiGenericDevice, AcpiPcieRootDevice, AcpiPlatform, AcpiResource,
    AcpiWatchdog, AcpiXhciDevice, ArmPlatformAcpi,
};
pub use board::{
    BoardBuildPolicy, BoardConfig, FdtSource, FirmwareConfig, FirmwareImagePolicy, FirmwareKind,
    FitParseMode, PayloadConfig, PayloadKind, Platform, SocImageFormat,
};
pub use builder::{
    board_info_from_config, build_info_from_config, dev_security_config, flow_profile_from_config,
    hstr, hvec, x86_linuxboot_payload, x86_uefi_payload, Board, BoardBlob, BoardDataMode,
    BoardInfo, Build, BuildInfo, BuildProfile, BusChild, DeviceBranch, DeviceTopology, FlowProfile,
    I2cChild, ImageBuildInfo, LpcChild, PayloadInputInfo, PciChild, SmbusChild, SpiChild,
    StageBuildInfo, TopologyChild, BOARD_BLOB_ABI_VERSION,
};
pub use device::{BusAddress, DeviceConfig, DeviceId, DeviceNode, DeviceRole};
pub use ffs::{
    AnchorBlock, Compression, DigestSet, EntryContent, FileType, ImageManifest, KeyBytes, Region,
    RegionContent, RegionEntry, Segment, SegmentFlags, SegmentKind, Signature, SignatureKind,
    SignedManifest, VerificationKey, FFS_MAGIC, FFS_VERSION,
};
pub use memory::{
    CarConfig, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion, IntelIfdRegionConfig, MemoryMap,
    MemoryMapError, MemoryRegion, RegionKind,
};
pub use security::{DigestAlgorithm, SecurityConfig, SignatureAlgorithm};
pub use smbios::SmbiosConfig;
pub use smm::{CorebootSmmCompat, SmmConfig, SmmPlatform};
pub use stage::{
    effective_stage_load_addr, BootMedium, Capability, MonolithicConfig, RunsFrom, StageConfig,
    StageLayout, TempRamBuffer,
};
pub use typed::{
    i2c_child, io16, lpc_child, mmio32, pci_child, spi_child, BusKind, BusPortId, ChildAttachment,
    DeviceEdge, I2cBus, Io16, Io8, IoAddr, Irq, LpcBus, Mmio16, Mmio32, Mmio64, Mmio8, MmioAddr,
    PciBdf, PciBus, PnpBus, SimpleBus, SmbusBus, SpiBus, TypedBus,
};
