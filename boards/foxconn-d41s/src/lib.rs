//! Foxconn D41S Rust board metadata.

use fstart_platform_intel_pineview_ich7::{PcieRootPort, PineviewIch7Board};
use fstart_types::smbios::{ChassisType, ProcessorFamily, SmbiosProcessor};
use fstart_types::{
    build_info_from_config, hstr, hvec, io16, BoardConfig, BoardInfo, BuildInfo, BusAddress,
    Platform, SmbiosConfig,
};

pub const BOARD_NAME: &str = "foxconn-d41s";
pub const BOARD_PACKAGE: &str = "fstart-board-foxconn-d41s";
pub const PLATFORM: Platform = Platform::X86_64;

fn board() -> PineviewIch7Board {
    PineviewIch7Board::new(BOARD_NAME, BOARD_PACKAGE)
        .pcie_port(PcieRootPort::Port0, true)
        .pcie_port(PcieRootPort::Port1, true)
        .superio("superio", io16(0x2e))
        .on_smbus(|smbus| {
            smbus.runtime("ck505", BusAddress::I2c(0x69));
        })
        .smbios(d41s_smbios())
}

#[must_use]
pub fn board_config() -> BoardConfig {
    board().board_config()
}

#[must_use]
pub fn board_info() -> BoardInfo {
    board().board_info()
}

#[must_use]
pub fn build_info() -> BuildInfo {
    let config = board_config();
    build_info_from_config(
        BOARD_NAME,
        BOARD_PACKAGE,
        &config,
        ["intel-pineview", "intel-ich7", "ite8721f", "i2c-ck505"],
    )
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}

pub fn d41s_smbios() -> SmbiosConfig {
    let processors = hvec([SmbiosProcessor {
        socket: hstr("FCBGA559"),
        manufacturer: hstr("Intel"),
        processor_family: ProcessorFamily::X86_64,
        max_speed_mhz: None,
        core_count: None,
        thread_count: None,
        caches: hvec([]),
    }]);

    SmbiosConfig {
        bios_vendor: hstr("fstart"),
        bios_version: hstr("0.1.0"),
        bios_release_date: hstr(option_env!("FSTART_SMBIOS_DATE").unwrap_or("04/15/2026")),
        system_manufacturer: hstr("Foxconn"),
        system_product: hstr("D41S"),
        system_version: hstr("1.0"),
        system_serial: Default::default(),
        baseboard_manufacturer: hstr("Foxconn"),
        baseboard_product: hstr("D41S"),
        chassis_type: ChassisType::Desktop,
        chassis_manufacturer: hstr("Foxconn"),
        processors,
        memory_devices: hvec([]),
    }
}
