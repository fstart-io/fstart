//! Main-stage fixed-flow adapter for QEMU Q35 UEFI.

use fstart_board_qemu_q35_uefi_facts as facts;
use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_driver_q35_hostbridge::{Q35HostBridge, Q35HostBridgeConfig};
use fstart_driver_qemu_fw_cfg::{QemuFwCfg, QemuFwCfgConfig};
use fstart_services::memory_detect::{E820Entry, MAX_E820_ENTRIES};
use fstart_services::{Device, HardwareInit, InitContext, PciRootBus, ServiceError};
use fstart_stage::crabefi::{MemoryRegion, MemoryType, UefiLaunchConfig};
use fstart_stage::fixed_helpers::{console_ready, MemoryMappedUefiBoot, StaticConsole};
use fstart_stage_runtime::StaticBoard;

static UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Pio {
        base: facts::UART0_PIO_BASE,
    },
    clock_freq: facts::UART0_CLOCK_FREQ,
    baud_rate: facts::UART0_BAUD_RATE,
};

static FW_CFG_CONFIG: QemuFwCfgConfig = QemuFwCfgConfig {
    ctl_port: facts::FW_CFG_CTL_PORT,
    data_port: facts::FW_CFG_DATA_PORT,
};

static Q35_CONFIG: Q35HostBridgeConfig = Q35HostBridgeConfig {
    ecam_base: facts::PCI0_ECAM_BASE,
    ecam_size: facts::PCI0_ECAM_SIZE,
    bus_start: facts::PCI0_BUS_START,
    bus_end: facts::PCI0_BUS_END,
};

static mut ACPI_BUFFER: [u8; facts::ACPI_BUFFER_SIZE] = [0; facts::ACPI_BUFFER_SIZE];

fn device_error_to_service_error(_err: fstart_services::DeviceError) -> ServiceError {
    ServiceError::HardwareError
}

pub struct MainDevices {
    console: StaticConsole<Ns16550>,
    fw_cfg: Option<QemuFwCfg>,
    q35: Option<Q35HostBridge>,
    e820: [E820Entry; MAX_E820_ENTRIES],
    e820_count: usize,
    total_ram: u64,
    acpi_rsdp: Option<u64>,
}

impl MainDevices {
    pub const fn new() -> Self {
        Self {
            console: StaticConsole::new(&UART0_CONFIG),
            fw_cfg: None,
            q35: None,
            e820: [E820Entry::zeroed(); MAX_E820_ENTRIES],
            e820_count: 0,
            total_ram: 0,
            acpi_rsdp: None,
        }
    }

    fn ensure_fw_cfg(&mut self) -> Result<&mut QemuFwCfg, ServiceError> {
        if self.fw_cfg.is_none() {
            let fw_cfg = QemuFwCfg::new(&FW_CFG_CONFIG).map_err(device_error_to_service_error)?;
            self.fw_cfg = Some(fw_cfg);
        }
        Ok(self.fw_cfg.as_mut().expect("fw_cfg constructed"))
    }

    fn ensure_q35(&mut self) -> Result<&mut Q35HostBridge, ServiceError> {
        if self.q35.is_none() {
            let q35 = Q35HostBridge::new(&Q35_CONFIG).map_err(device_error_to_service_error)?;
            self.q35 = Some(q35);
        }
        Ok(self.q35.as_mut().expect("q35 constructed"))
    }

    fn e820(&self) -> &[E820Entry] {
        &self.e820[..self.e820_count]
    }

    fn acpi_rsdp(&self) -> Option<u64> {
        self.acpi_rsdp
    }

    fn console_device(&self) -> Option<&Ns16550> {
        self.console.device()
    }
}

impl HardwareInit for MainDevices {
    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.console.console(ctx)
    }

    fn memory_discovery(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.ensure_fw_cfg()?;
        let mut fw_cfg = self.fw_cfg.take().expect("fw_cfg constructed");

        let result = (|| {
            fw_cfg.init().map_err(device_error_to_service_error)?;
            let (count, total) = fstart_capabilities::memory_detect(
                &mut fw_cfg,
                &mut self.e820,
                facts::FW_CFG0_NODE,
            )?;
            self.e820_count = count;
            self.total_ram = total;

            // SAFETY: ACPI_BUFFER is stage-owned static storage and firmware init is single-threaded.
            let acpi_buf = unsafe {
                core::slice::from_raw_parts_mut(
                    core::ptr::addr_of_mut!(ACPI_BUFFER).cast::<u8>(),
                    facts::ACPI_BUFFER_SIZE,
                )
            };
            self.acpi_rsdp = Some(fstart_capabilities::acpi_load(
                &mut fw_cfg,
                acpi_buf,
                facts::FW_CFG0_NODE,
            )?);
            Ok(())
        })();

        self.fw_cfg = Some(fw_cfg);
        result
    }

    fn bus_probe(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let q35 = self.ensure_q35()?;
        q35.init().map_err(device_error_to_service_error)?;
        q35.init_bus()?;
        fstart_log::info!(
            "{}: PCI root ready ({} devices)",
            facts::PCI0_NODE,
            q35.device_count(),
        );
        Ok(())
    }

    fn drivers_ready(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let mut count = 1; // console
        if self.fw_cfg.is_some() {
            count += 1;
        }
        if self.q35.is_some() {
            count += 1;
        }
        fstart_capabilities::driver_init_complete(count);
        Ok(())
    }
}

pub struct MainBoard {
    devices: MainDevices,
    boot: MemoryMappedUefiBoot,
    payload_ready: bool,
}

impl StaticBoard for MainBoard {
    type Devices = MainDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: MainDevices::new(),
            boot: MemoryMappedUefiBoot::new(facts::FFS_BASE, facts::FFS_SIZE, 0),
            payload_ready: false,
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        console_ready(facts::UART0_NODE, "ns16550");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.mount()
    }

    fn verify_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        self.boot.verify()
    }

    fn load_payload(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        // UEFI payload support is linked in via the `crabefi` feature; no external
        // FFS firmware blob needs to be copied for this board.
        self.payload_ready = true;
        Ok(())
    }

    fn boot_payload(self) -> ! {
        if !self.payload_ready {
            fstart_log::error!("UEFI handoff requested before payload setup");
            Self::halt();
        }
        let console = match self.devices.console_device() {
            Some(console) => console,
            None => {
                fstart_log::error!("UEFI handoff requested before console init");
                Self::halt();
            }
        };

        let acpi_base = self.devices.acpi_rsdp().unwrap_or(0) & !0xfff;
        let platform_entries = [
            MemoryRegion {
                base: facts::FLASH_BASE,
                size: facts::FLASH_SIZE_U64,
                region_type: MemoryType::RuntimeServicesCode,
            },
            MemoryRegion {
                base: acpi_base,
                size: facts::ACPI_BUFFER_SIZE as u64,
                region_type: MemoryType::AcpiReclaimable,
            },
        ];

        fstart_log::info!(
            "launching CrabEFI: ram={} MiB, rsdp={:#x}, ecam={:#x}",
            (self.devices.total_ram >> 20) as u32,
            self.devices.acpi_rsdp().unwrap_or(0),
            facts::PCI0_ECAM_BASE,
        );
        fstart_stage::crabefi::launch_x86_uefi(
            UefiLaunchConfig {
                console: Some(console),
                framebuffer: None,
                acpi_rsdp: self.devices.acpi_rsdp(),
                smbios: None,
                fdt: None,
                ecam_base: Some(facts::PCI0_ECAM_BASE),
                runtime_region: Some(fstart_stage::crabefi::compute_runtime_region()),
            },
            self.devices.e820(),
            &platform_entries,
        )
    }
}
