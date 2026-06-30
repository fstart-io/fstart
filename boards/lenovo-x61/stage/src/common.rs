use fstart_board_lenovo_x61 as board;
use fstart_driver_intel_gm965::IntelGm965;
use fstart_driver_intel_ich8::IntelIch8;
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
use fstart_mainboard_lenovo_x61::{
    x61_gpio_config, x61_hda_config, x61_igd_config, x61_mainboard_config, LenovoX61Mainboard,
};
use fstart_platform_intel_gm965_ich8::{
    Gm965Ich8RuntimeConfig, Gm965Ich8RuntimePolicy, IdeConfig, IoTrapAccess, IoTrapConfig,
    LpcFixedIoDecode, LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, PcieRootPort,
    SataConfig, SataMode, UsbConfig,
};
use fstart_services::{Device, DeviceError, ServiceError};

pub const UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Pio {
        base: board::UART0_PIO_BASE as u64,
    },
    clock_freq: board::UART0_CLOCK_FREQ,
    baud_rate: board::UART0_BAUD_RATE,
};

pub fn runtime_config() -> Gm965Ich8RuntimeConfig {
    Gm965Ich8RuntimePolicy::new()
        .flash_layout(Some(board::x61_flash_layout()))
        .igd(x61_igd_config())
        .pcie_port(PcieRootPort::Port1, true)
        .pcie_port(PcieRootPort::Port2, true)
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
        .runtime_config()
}

pub fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}

pub fn new_gm965() -> Result<IntelGm965, ServiceError> {
    IntelGm965::new(runtime_config().gm965).map_err(device_error_to_service_error)
}

pub fn new_ich8() -> Result<IntelIch8, ServiceError> {
    IntelIch8::new(runtime_config().ich8).map_err(device_error_to_service_error)
}

pub fn new_mainboard() -> Result<LenovoX61Mainboard, ServiceError> {
    LenovoX61Mainboard::new(x61_mainboard_config()).map_err(device_error_to_service_error)
}
