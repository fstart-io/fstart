//! Lenovo ThinkPad X61 board metadata and build policy.

use crate::devices::{x61_gpio_config, x61_hda_config, x61_igd_config};
use fstart_platform_intel_gm965_ich8::{
    Gm965Ich8Config, IdeConfig, IoTrapAccess, IoTrapConfig, LpcFixedIoDecode, LpcGenericIoDecode,
    LpcParallelDecode, LpcSerialDecode, PcieRootPort, SataConfig, SataMode, UsbConfig,
};
use fstart_types::smbios::{
    ChassisType, MemoryDeviceType, ProcessorFamily, SmbiosMemoryDevice, SmbiosProcessor,
};
use fstart_types::{
    hstr, hvec, io16, x86_uefi_payload, BoardConfig, BoardInfo, BuildInfo, BusAddress, FlashLayout,
    IntelIfdFlashLayout, IntelIfdRegion, IntelIfdRegionConfig, Platform, SmbiosConfig,
};

pub const BOARD_NAME: &str = "lenovo-x61";
pub const BOARD_PACKAGE: &str = "fstart-board-lenovo-x61";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_NODE: &str = "dock_superio/com1";
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;

pub fn gm965_ich8_config() -> Gm965Ich8Config {
    Gm965Ich8Config::new(BOARD_NAME, BOARD_PACKAGE)
        .payload(x86_uefi_payload())
        .flash_layout(Some(x61_flash_layout()))
        .igd(x61_igd_config())
        .pcie_port(PcieRootPort::Port1, true)
        .pcie_port(PcieRootPort::Port2, true)
        .acpi_print_hex(true)
        .lpc_fixed_io(LpcFixedIoDecode {
            com_a: LpcSerialDecode::Com1,
            com_b: LpcSerialDecode::Com2,
            lpt: Some(LpcParallelDecode::Lpt3bc),
            fdd: None,
        })
        .lpc_generic_io(LpcGenericIoDecode {
            base: 0x1600,
            size: 0x0080,
        })
        .lpc_generic_io(LpcGenericIoDecode {
            base: 0x15e0,
            size: 0x0010,
        })
        .lpc_generic_io(LpcGenericIoDecode {
            base: 0x1680,
            size: 0x0020,
        })
        .gpe0_en(0x0104_0046)
        .gpi_routing([0, 0, 2, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 2, 0, 0])
        .ide(IdeConfig {
            enable_primary: true,
            enable_secondary: false,
        })
        .sata(SataConfig {
            mode: SataMode::Ahci,
            ports: 0x01,
            hotplug_map: 0,
            clock_request: false,
            traffic_monitor: false,
        })
        .usb(UsbConfig {
            ehci: [true, true],
            uhci: [true, true, true, true, true, true],
        })
        .hda(x61_hda_config())
        .gpio(x61_gpio_config())
        .io_trap(IoTrapConfig {
            index: 3,
            base: 0x0800,
            size: 0x10,
            access: IoTrapAccess::Any,
        })
        .superio("dlpc_superio", io16(0x164e), true)
        .superio("dock_superio", io16(0x2e), false)
        .on_smbus(|smbus| {
            smbus.runtime_enabled("ck505", BusAddress::I2c(0x69), false);
        })
        .mainboard()
        .smbios(x61_smbios())
}

#[must_use]
pub fn board_config() -> BoardConfig {
    gm965_ich8_config().board_config()
}

#[must_use]
pub fn board_info() -> BoardInfo {
    gm965_ich8_config().board_info()
}

#[must_use]
pub fn build_info() -> BuildInfo {
    gm965_ich8_config().build_info([
        "intel-gm965",
        "intel-ich8",
        "nsc-pc87382",
        "nsc-pc87392",
        "i2c-ck505",
    ])
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}

pub fn x61_flash_layout() -> FlashLayout {
    FlashLayout::IntelIfd(IntelIfdFlashLayout {
        base: 0xFFC0_0000,
        size: 0x0040_0000,
        regions: hvec([
            IntelIfdRegionConfig {
                kind: IntelIfdRegion::Descriptor,
                offset: 0x000000,
                size: 0x001000,
                file: None,
            },
            IntelIfdRegionConfig {
                kind: IntelIfdRegion::Gbe,
                offset: 0x001000,
                size: 0x002000,
                file: None,
            },
            IntelIfdRegionConfig {
                kind: IntelIfdRegion::Me,
                offset: 0x003000,
                size: 0x27D000,
                file: None,
            },
            IntelIfdRegionConfig {
                kind: IntelIfdRegion::Bios,
                offset: 0x280000,
                size: 0x180000,
                file: None,
            },
        ]),
    })
}

pub fn x61_smbios() -> SmbiosConfig {
    SmbiosConfig {
        bios_vendor: hstr("fstart"),
        bios_version: hstr("0.1.0"),
        bios_release_date: hstr(option_env!("FSTART_SMBIOS_DATE").unwrap_or("04/15/2026")),
        system_manufacturer: hstr("LENOVO"),
        system_product: hstr("ThinkPad X61"),
        system_version: hstr("1.0"),
        system_serial: hstr(""),
        baseboard_manufacturer: hstr("LENOVO"),
        baseboard_product: hstr("ThinkPad X61"),
        chassis_type: ChassisType::Other,
        chassis_manufacturer: hstr("LENOVO"),
        processors: hvec([SmbiosProcessor {
            socket: hstr("Socket M"),
            manufacturer: hstr("Intel"),
            processor_family: ProcessorFamily::X86_64,
            max_speed_mhz: None,
            core_count: None,
            thread_count: None,
            caches: hvec([]),
        }]),
        memory_devices: hvec([
            SmbiosMemoryDevice {
                locator: hstr("DIMM0"),
                size_mb: None,
                speed_mhz: None,
                memory_type: Some(MemoryDeviceType::Unknown),
            },
            SmbiosMemoryDevice {
                locator: hstr("DIMM1"),
                size_mb: None,
                speed_mhz: None,
                memory_type: Some(MemoryDeviceType::Unknown),
            },
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x61_build_features_do_not_select_deleted_mainboard_crate() {
        let build = build_info();
        assert!(!build
            .features
            .iter()
            .any(|feature| feature.as_str() == "lenovo-x61-mainboard"));
    }

    #[test]
    fn x61_board_config_keeps_board_owned_mainboard_hook() {
        let config = board_config();
        let mainboard = config
            .devices
            .iter()
            .find(|device| device.name.as_str() == "mainboard")
            .expect("X61 board hook device is declared");
        assert!(mainboard.enabled);
    }
}
