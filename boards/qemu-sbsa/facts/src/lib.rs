//! Shared board facts for QEMU SBSA-ref.
//!
//! Host metadata and the board-owned stage both import this module. Keep the
//! actual facts here, and use existing fstart descriptor types where those
//! descriptors are shared by the host model and stage table writers.

#![no_std]
#![allow(dead_code)]

use fstart_acpi::devices::{AhciAcpi, PcieRootAcpi, XhciAcpi};
use fstart_acpi::platform::{ArmConfig, IortConfig, PlatformConfig, WatchdogConfig};
use fstart_smbios::{CacheDesc, MemoryDeviceDesc, ProcessorDesc, SmbiosDesc};

pub const BOARD_NAME: &str = "qemu-sbsa";
pub const BOARD_PACKAGE: &str = "fstart-board-qemu-sbsa";

pub const FLASH_NAME: &str = "flash";
pub const FLASH_BASE: u64 = 0x1000_0000;
pub const FLASH_SIZE: u64 = 0x1000_0000;

pub const RAM_NAME: &str = "ram";
pub const RAM_BASE: u64 = 0x100_0000_0000;
pub const RAM_SIZE: u64 = 0x4000_0000;
pub const RAM_END: u64 = RAM_BASE + RAM_SIZE - 1;

pub const STAGE_LOAD_ADDR: u64 = 0x100_0010_0000;
pub const STAGE_STACK_SIZE: u32 = 0x40000;
pub const STAGE_HEAP_SIZE: u32 = 0x40000;

pub const UART0_NODE: &str = "uart0";
pub const UART0_DRIVER: &str = "pl011";
pub const UART0_ACPI_NAME: &str = "COM0";
pub const UART0_BASE: u64 = 0x6000_0000;
pub const UART0_CLOCK: u32 = 1_843_200;
pub const UART0_BAUD: u32 = 115_200;
pub const UART0_GSIV: u32 = 33;
pub const UART0_DBG2: bool = true;

pub const PCI0_NODE: &str = "pci0";
pub const PCI0_ECAM_SIZE: u64 = 0x1000_0000;
pub const PCI0_MMIO32_SIZE: u64 = 0x7000_0000;
pub const PCI0_MMIO64_SIZE: u64 = 0xff_0000_0000;
pub const PCI0_PIO_SIZE: u64 = 0x10000;

pub const PCI0_ACPI: PcieRootAcpi<'static> = PcieRootAcpi {
    name: "PCI0",
    ecam_base: 0xf000_0000,
    mmio32_base: 0x8000_0000,
    mmio32_end: 0xefff_ffff,
    mmio64_base: 0x1_0000_0000,
    mmio64_end: 0xff_ffff_ffff,
    pio_base: 0x7fff_0000,
    bus_start: 0,
    bus_end: 255,
    irqs: [35, 36, 37, 38],
    segment: 0,
};

pub const AHCI0_ACPI: AhciAcpi<'static> = AhciAcpi {
    name: "AHC0",
    base: 0x6010_0000,
    size: 0x10000,
    gsiv: 42,
};

pub const XHCI0_ACPI: XhciAcpi<'static> = XhciAcpi {
    name: "USB0",
    base: 0x6011_0000,
    size: 0x10000,
    gsiv: 43,
};

pub const IORT_ITS_IDS: [u32; 1] = [0];

pub const ACPI_PLATFORM: PlatformConfig = PlatformConfig::Arm(ArmConfig {
    num_cpus: 1,
    gic_dist_base: 0x4006_0000,
    gic_redist_base: 0x4008_0000,
    gic_redist_length: Some(0x400_0000),
    gic_its_base: Some(0x4408_1000),
    timer_gsivs: (29, 30, 27, 26),
    watchdog: Some(WatchdogConfig {
        refresh_base: 0x5001_0000,
        control_base: 0x5001_1000,
        gsiv: 48,
    }),
    iort: Some(IortConfig {
        its_ids: &IORT_ITS_IDS,
        pci_segment: 0,
        memory_address_limit: 0x30,
        id_count: 0x10000,
    }),
});

pub const SMBIOS_CHASSIS_RACK_MOUNT: u8 = 0x17;
pub const SMBIOS_PROCESSOR_FAMILY_AARCH64: u16 = 0x0119;
pub const SMBIOS_CACHE_ASSOC_WAY4: u8 = 0x05;
pub const SMBIOS_CACHE_ASSOC_WAY8: u8 = 0x07;
pub const SMBIOS_CACHE_TYPE_INSTRUCTION: u8 = 0x03;
pub const SMBIOS_CACHE_TYPE_DATA: u8 = 0x04;
pub const SMBIOS_CACHE_TYPE_UNIFIED: u8 = 0x05;
pub const SMBIOS_MEMORY_TYPE_DDR4: u8 = 0x1a;

pub const SMBIOS_CACHES: [CacheDesc<'static>; 3] = [
    CacheDesc {
        designation: "L1 Instruction Cache",
        level: 1,
        size_kb: 64,
        associativity: SMBIOS_CACHE_ASSOC_WAY4,
        cache_type: SMBIOS_CACHE_TYPE_INSTRUCTION,
    },
    CacheDesc {
        designation: "L1 Data Cache",
        level: 1,
        size_kb: 64,
        associativity: SMBIOS_CACHE_ASSOC_WAY4,
        cache_type: SMBIOS_CACHE_TYPE_DATA,
    },
    CacheDesc {
        designation: "L2 Unified Cache",
        level: 2,
        size_kb: 1024,
        associativity: SMBIOS_CACHE_ASSOC_WAY8,
        cache_type: SMBIOS_CACHE_TYPE_UNIFIED,
    },
];

pub const SMBIOS_PROCESSORS: [ProcessorDesc<'static>; 1] = [ProcessorDesc {
    socket: "CPU0",
    manufacturer: "ARM",
    family: SMBIOS_PROCESSOR_FAMILY_AARCH64,
    max_speed_mhz: 2000,
    core_count: 1,
    thread_count: 1,
    caches: &SMBIOS_CACHES,
}];

pub const SMBIOS_MEMORY_DEVICES: [MemoryDeviceDesc<'static>; 1] = [MemoryDeviceDesc {
    locator: "DIMM0",
    size_mb: 1024,
    speed_mhz: 2400,
    memory_type: SMBIOS_MEMORY_TYPE_DDR4,
}];

pub const SMBIOS_DESC: SmbiosDesc<'static> = SmbiosDesc {
    bios_vendor: "fstart",
    bios_version: "0.1.0",
    bios_release_date: "03/10/2026",
    sys_manufacturer: "QEMU",
    sys_product: "SBSA Reference",
    sys_version: "1.0",
    sys_serial: None,
    bb_manufacturer: "QEMU",
    bb_product: "sbsa-ref",
    chassis_type: SMBIOS_CHASSIS_RACK_MOUNT,
    chassis_manufacturer: "QEMU",
    processors: &SMBIOS_PROCESSORS,
    memory_devices: &SMBIOS_MEMORY_DEVICES,
    ram_base: RAM_BASE,
    ram_end: RAM_END,
};
