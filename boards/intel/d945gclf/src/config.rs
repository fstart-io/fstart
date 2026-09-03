//! Intel D945GCLF board metadata and build policy.
//!
//! Facts ported from coreboot `mainboard/intel/d945gclf`: `devicetree.cb`
//! (PIRQ/GPI routing, device enables, LPC decodes, SuperIO LDNs),
//! `gpio.c` (pad tables), `early_init.c` (SuperIO PME at 0x680), and
//! `Kconfig` (512 KiB flash, Atom 230).

use fstart_core::smbios::{ChassisType, ProcessorFamily, SmbiosProcessor};
#[cfg(feature = "host")]
use fstart_core::{dev_security_config, AcpiConfig, AcpiPlatform, BoardBuildPolicy, BoardConfig};
use fstart_core::{hstr, hvec, FlashLayout, X86LegacyFlashLayout, Platform, SmbiosConfig};
use fstart_driver_intel::southbridge::gpio_ich as gpio;
use fstart_driver_superio::smsc_lpc47m15x;
#[cfg(feature = "host")]
use fstart_platform_intel::i945::{i945_ich7_memory, i945_ich7_microcode, i945_ich7_stages};
use fstart_platform_intel::i945::{
    I945Ich7Config, I945Variant, LpcFixedIoDecode, LpcGenericIoDecode, LpcSerialDecode, SataConfig,
    SataMode, UsbConfig,
};

pub const BOARD_NAME: &str = "intel-d945gclf";
pub const BOARD_PACKAGE: &str = "fstart-board-intel-d945gclf";
pub const PLATFORM: Platform = Platform::X86_64;
pub const UART0_NODE: &str = "superio/com1";
pub const UART0_PIO_BASE: u16 = 0x3f8;
pub const UART0_CLOCK_FREQ: u32 = 1_843_200;
pub const UART0_BAUD_RATE: u32 = 115_200;
pub const SUPERIO_NODE: &str = "superio";
pub const SUPERIO_PNP_BASE: u16 = 0x2e;
/// SMSC LPC47M15x PME logical device with its runtime register base.
/// coreboot enables it in `bootblock_mainboard_early_init()` so the ICH7
/// generic decode window at 0x680 has a live target.
pub const SUPERIO_PME_LDN: u8 = 10;
pub const SUPERIO_PME_BASE: u16 = 0x0680;

pub static D945GCLF_PLATFORM: I945Ich7Config = I945Ich7Config::new()
    .variant(I945Variant::DesktopGc)
    .max_cpus(2)
    .gfx_gms(4)
    .pci_mmio_size(768)
    .pcie_port(0, true)
    .pcie_port(1, false)
    .pcie_port(2, true)
    .pcie_port(3, true)
    .lan(false)
    .ac97_audio(false)
    .ac97_modem(false)
    .pirq_routing([0x05, 0x07, 0x05, 0x07, 0x80, 0x80, 0x80, 0x06])
    .gpi_routing([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0])
    .lpc_fixed_io(LpcFixedIoDecode {
        com_a: LpcSerialDecode::Com1,
        com_b: LpcSerialDecode::Com2,
        lpt: None,
        fdd: None,
    })
    .lpc_generic_io([LpcGenericIoDecode {
        base: SUPERIO_PME_BASE,
        size: 0x0080,
    }])
    .sata(SataConfig {
        mode: SataMode::Ahci,
        ports: 0x0f,
    })
    .usb(UsbConfig {
        ehci: true,
        uhci: [true, true, true, false],
    })
    .hda(fstart_driver_intel::southbridge::hda::HdaConfig::new())
    .gpio_pins(d945gclf_gpio_pins())
    .gpe0_en(0x2000_0601)
    .build();

#[cfg(feature = "host")]
#[must_use]
pub fn board_config() -> BoardConfig {
    let flash_layout = d945gclf_flash_layout();

    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: i945_ich7_memory(Some(flash_layout)),
        stages: i945_ich7_stages(&D945GCLF_PLATFORM),
        security: dev_security_config("keys/dev-signing.pub"),
        payload: None,
        microcode: Some(i945_ich7_microcode()),
        soc_image_format: Default::default(),
        full_flash_image: true,
        acpi: Some(AcpiConfig {
            print_hex: true,
            platform: AcpiPlatform::X86,
        }),
        smbios: Some(d945gclf_smbios()),
        smm: Some(fstart_core::SmmConfig::default()),
        build: BoardBuildPolicy {
            cpu_feature: Some(hstr("cpu-intel-diamondville")),
            ..Default::default()
        },
        boot_hart_id: 0,
    }
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}

pub fn d945gclf_flash_layout() -> FlashLayout {
    FlashLayout::X86Legacy(X86LegacyFlashLayout { size: 0x0008_0000 })
}

pub fn d945gclf_smbios() -> SmbiosConfig {
    SmbiosConfig {
        bios_vendor: hstr("fstart"),
        bios_version: hstr("0.1.0"),
        bios_release_date: hstr(option_env!("FSTART_SMBIOS_DATE").unwrap_or("04/15/2026")),
        system_manufacturer: hstr("Intel"),
        system_product: hstr("D945GCLF"),
        system_version: hstr("1.0"),
        system_serial: hstr(""),
        baseboard_manufacturer: hstr("Intel"),
        baseboard_product: hstr("D945GCLF"),
        chassis_type: ChassisType::Desktop,
        chassis_manufacturer: hstr("Intel"),
        processors: hvec([SmbiosProcessor {
            socket: hstr("Socket 441"),
            manufacturer: hstr("Intel"),
            processor_family: ProcessorFamily::X86_64,
            max_speed_mhz: None,
            core_count: None,
            thread_count: None,
            caches: hvec([]),
        }]),
        memory_devices: hvec([]),
    }
}

pub fn d945gclf_superio_config() -> smsc_lpc47m15x::SmscLpc47m15xConfig {
    smsc_lpc47m15x::SmscLpc47m15xConfig(smsc_lpc47m15x::SuperIoConfig {
        com1: Some(smsc_lpc47m15x::ComPortConfig {
            io_base: UART0_PIO_BASE,
            irq: 4,
            baud_rate: UART0_BAUD_RATE,
        }),
        com2: Some(smsc_lpc47m15x::ComPortConfig {
            io_base: 0x2f8,
            irq: 3,
            baud_rate: UART0_BAUD_RATE,
        }),
        parallel: None,
        env_controller: None,
        keyboard: Some(smsc_lpc47m15x::KbcConfig {
            io_base: 0x60,
            io_ext: 0x64,
            irq: 1,
        }),
        // The LPC47M15x KBC LDN covers keyboard only; coreboot runs
        // `pc_keyboard_init(NO_AUX_DEVICE)` on this board.
        mouse: None,
        cir: None,
        gpio: None,
        acpi_name: Some(hstr("SIO1")),
        console_port: Some(hstr("com1")),
    })
}

/// GPIO pads from coreboot `gpio.c`.
///
/// Only pins the board switches to GPIO mode are listed; the rest stay on
/// their native function. Set 1 pins 13/15 are inverted; outputs 14/16/24/
/// 27/28/29 (set 1) and 38 (set 2) carry the board levels.
pub const fn d945gclf_gpio_pins() -> [gpio::GpioPin; 29] {
    use gpio::{GpioLevel, input, output};
    [
        input(0),
        input(6),
        input(7),
        input(8),
        input(9),
        input(10),
        input(12),
        input(13).inverted(),
        output(14, GpioLevel::High),
        input(15).inverted(),
        output(16, GpioLevel::Low),
        input(18),
        input(19),
        input(20),
        input(21),
        output(24, GpioLevel::Low),
        input(25),
        input(26),
        output(27, GpioLevel::High),
        output(28, GpioLevel::Low),
        output(29, GpioLevel::High),
        input(32),
        input(33),
        input(34),
        input(35),
        input(36),
        input(37),
        output(38, GpioLevel::High),
        input(39),
    ]
}
