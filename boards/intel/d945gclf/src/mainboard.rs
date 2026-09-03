//! Intel D945GCLF mainboard hooks.
//!
//! Ported from coreboot `mainboard/intel/d945gclf/early_init.c`: the only
//! board-specific pre-console work is the SMSC LPC47M15x PME logical device
//! (runtime registers at 0x680, the ICH7 generic decode target) plus the
//! COM1/COM2/KBC setup handled by the generic SuperIO driver.

#[cfg(feature = "stage")]
use fstart_core::services::device::BusDevice;
#[cfg(feature = "stage")]
use fstart_core::services::ServiceError;
#[cfg(feature = "stage")]
use fstart_driver_superio::smsc_lpc47m15x::SmscLpc47m15x;
#[cfg(feature = "stage")]
use fstart_platform_intel::i945::I945Ich7;
#[cfg(feature = "stage")]
use fstart_platform_intel::{IntelEarlyBoardHooks, IntelEarlyCtx};

/// Board-specific D945GCLF hooks for the i945/ICH7 flow.
#[cfg(feature = "stage")]
pub struct D945GclfMainboard {
    superio: Option<SmscLpc47m15x>,
}

#[cfg(feature = "acpi")]
mod mainboard_acpi_device {
    extern crate alloc;

    use super::{d945gclf_mainboard_dsdt_aml, D945GclfMainboard};

    impl fstart_acpi::device::AcpiDevice for D945GclfMainboard {
        type Config = fstart_platform_intel::i945::I945Ich7AcpiContext;

        fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
            d945gclf_mainboard_dsdt_aml(*config)
        }
    }
}

#[cfg(feature = "stage")]
impl D945GclfMainboard {
    #[must_use]
    pub const fn new() -> Self {
        Self { superio: None }
    }
}

#[cfg(feature = "stage")]
impl IntelEarlyBoardHooks<I945Ich7> for D945GclfMainboard {
    fn before_console(
        &mut self,
        _ctx: &mut IntelEarlyCtx<I945Ich7>,
    ) -> Result<(), ServiceError> {
        // Match coreboot's bootblock_mainboard_early_init(): PME first so
        // the 0x680 generic decode window has a live target, then COM/KBC.
        // Verbose logging while first bring-up is still in flight.
        unsafe { fstart_log::set_max_level(fstart_log::Level::Debug) };
        pme_init();
        let mut superio =
            SmscLpc47m15x::new_at_base(crate::d945gclf_superio_config().0, crate::SUPERIO_PNP_BASE)
                .map_err(ServiceError::from)?;
        superio.init().map_err(ServiceError::from)?;
        self.superio = Some(superio);
        Ok(())
    }
}

/// Enable the SMSC PME logical device at its runtime base.
///
/// Manual config-mode session mirroring coreboot
/// `lpc47m15x_enable_serial(PME_DEV, 0x680)`; the generic SuperIO driver has
/// no PME function slot, so the board owns these bytes.
#[cfg(all(feature = "stage", target_arch = "x86_64"))]
fn pme_init() {
    // SAFETY: fixed board SuperIO PnP config ports decoded by ICH8 LPC setup.
    unsafe {
        use fstart_core::pio::{inb, outb};
        const IDX: u16 = crate::SUPERIO_PNP_BASE;
        const DATA: u16 = crate::SUPERIO_PNP_BASE + 1;
        outb(IDX, 0x55);
        outb(IDX, 0x07);
        outb(DATA, crate::SUPERIO_PME_LDN);
        outb(IDX, 0x30);
        outb(DATA, 0x00);
        outb(IDX, 0x60);
        outb(DATA, (crate::SUPERIO_PME_BASE >> 8) as u8);
        outb(IDX, 0x61);
        outb(DATA, (crate::SUPERIO_PME_BASE & 0xff) as u8);
        outb(IDX, 0x30);
        outb(DATA, 0x01);
        outb(IDX, 0xaa);
        let _ = inb(DATA);
    }
}

#[cfg(all(feature = "stage", not(target_arch = "x86_64")))]
fn pme_init() {}

const BIOS_RELEASE_DATE: &str = match option_env!("FSTART_SMBIOS_DATE") {
    Some(date) => date,
    None => "05/08/2026",
};

static D945GCLF_SMBIOS_PROCESSORS: [fstart_acpi::smbios::ProcessorDesc<'static>; 1] =
    [fstart_acpi::smbios::ProcessorDesc {
        socket: "Socket 441",
        manufacturer: "Intel",
        family: 0x28,
        max_speed_mhz: 0,
        core_count: 0,
        thread_count: 0,
        caches: &[],
    }];

static D945GCLF_SMBIOS_MEMORY_DEVICES: [fstart_acpi::smbios::MemoryDeviceDesc<'static>; 2] = [
    fstart_acpi::smbios::MemoryDeviceDesc {
        locator: "DIMM0",
        size_mb: 0,
        speed_mhz: 0,
        memory_type: 0x02,
    },
    fstart_acpi::smbios::MemoryDeviceDesc {
        locator: "DIMM1",
        size_mb: 0,
        speed_mhz: 0,
        memory_type: 0x02,
    },
];

pub static D945GCLF_SMBIOS_DESC: fstart_acpi::smbios::SmbiosDesc<'static> =
    fstart_acpi::smbios::SmbiosDesc {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: BIOS_RELEASE_DATE,
        sys_manufacturer: "Intel",
        sys_product: "D945GCLF",
        sys_version: "1.0",
        sys_serial: None,
        bb_manufacturer: "Intel",
        bb_product: "D945GCLF",
        chassis_type: 0x03,
        chassis_manufacturer: "Intel",
        processors: &D945GCLF_SMBIOS_PROCESSORS,
        memory_devices: &D945GCLF_SMBIOS_MEMORY_DEVICES,
        ram_base: 0x0010_0000,
        ram_end: 0x7fff_ffff,
    };

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi_macros::acpi_dsl;
    use fstart_platform_intel::i945::I945Ich7AcpiContext;

    /// Board DSDT fragment: sleep/power buttons (coreboot `mainboard.asl`)
    /// plus the SuperIO scope (coreboot `superio.asl`) under LPCB.
    pub fn d945gclf_mainboard_dsdt_aml(context: I945Ich7AcpiContext) -> Vec<u8> {
        let mut out = Vec::new();

        out.extend(acpi_dsl! {
            Scope("\\_SB_") {
                Device("SLPB") {
                    Name("_HID", "PNP0C0E");
                    Name("_PRW", Package(0x1du32, 0x04u32));
                }
                Device("PWRB") {
                    Name("_HID", "PNP0C0C");
                    Name("_PRW", Package(0x1du32, 0x04u32));
                }
            }
        });

        let config = crate::d945gclf_superio_config();
        let sio = fstart_driver_superio::superio_dsdt_aml(&config);
        out.extend(fstart_acpi::scope_aml(context.lpc_scope(), &sio));
        out
    }
}

#[cfg(feature = "acpi")]
pub use acpi_impl::d945gclf_mainboard_dsdt_aml;
