//! Lenovo ThinkPad X60 mainboard glue.
//!
//! The X60 shares its EC, PMH7 and X6 UltraBase dock with the X61
//! ([`fstart_driver_lenovo::x6`]). The pre-console path routes the dock UART:
//! ICH7 opens the LPC/GPIO decode windows and programs the pads, then the dock
//! code initializes the laptop-side DLPC and, with a dock attached, connects
//! the dock LPC segment and enables the PC87392 COM1 before the console
//! probes 0x3f8.

#[cfg(all(not(test), any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
use crate::config::Hardware as I945Ich7;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
use fstart_core::services::ServiceError;
#[cfg(all(not(test), any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
use fstart_driver_lenovo::x6::dock;
#[cfg(all(not(test), fstart_stage_env = "car"))]
use fstart_platform_intel::{IntelEarlyBoardHooks, IntelEarlyCtx};
#[cfg(all(not(test), fstart_stage_env = "ram"))]
use fstart_platform_intel::{IntelMainstageBoardCtx, IntelMainstageBoardHooks};

/// Board-specific X60 hooks for the i945GM/ICH7-M flow.
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
#[derive(Default)]
pub struct X60Mainboard {
    #[cfg(fstart_stage_env = "ram")]
    identity: Option<fstart_driver_lenovo::eeprom::Identity>,
    #[cfg(fstart_stage_env = "ram")]
    ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
}

/// SPD, CK505 and the identity EEPROM share one SMBus segment.
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
impl<S> fstart_platform_intel::IntelSmbusRouting<S> for X60Mainboard {}

/// The mainboard contributes ACPI fragments through the same `AcpiDevice`
/// abstraction the chipset drivers use.
#[cfg(fstart_stage_env = "ram")]
mod mainboard_acpi_device {
    extern crate alloc;

    use super::{X60Mainboard, x60_mainboard_dsdt_aml};

    impl fstart_acpi::device::AcpiDevice for X60Mainboard {
        type Config = fstart_platform_intel::i945::I945Ich7AcpiContext;

        fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
            x60_mainboard_dsdt_aml(*config)
        }
    }
}

#[cfg(all(not(test), fstart_stage_env = "car"))]
impl IntelEarlyBoardHooks<I945Ich7> for X60Mainboard {
    fn before_console(&mut self, ctx: &mut IntelEarlyCtx<I945Ich7>) -> Result<(), ServiceError> {
        dock::setup_console(ctx.southbridge(), os_ec());
        Ok(())
    }
}

#[cfg(all(not(test), fstart_stage_env = "ram"))]
impl IntelMainstageBoardHooks<I945Ich7> for X60Mainboard {
    fn smbios_identity<'a>(
        &'a self,
        configured: &fstart_platform_intel::tables::SmbiosIdentity<'a>,
    ) -> fstart_platform_intel::tables::SmbiosIdentity<'a> {
        let mut identity = *configured;
        // Like coreboot, the EEPROM fields describe both system and board.
        if let Some(eeprom) = &self.identity {
            identity.sys_product = eeprom.part_number();
            identity.sys_version = eeprom.version();
            identity.sys_serial = Some(eeprom.serial_number());
            identity.sys_uuid = Some(eeprom.uuid);
            identity.bb_product = eeprom.part_number();
            identity.bb_version = eeprom.version();
            identity.bb_serial = Some(eeprom.serial_number());
        }
        identity.oem_string = self.ec_oem_string.as_ref().map(|oem| oem.as_str());
        identity
    }

    fn before_console(
        &mut self,
        ctx: &mut IntelMainstageBoardCtx<I945Ich7>,
    ) -> Result<(), ServiceError> {
        dock::setup_console(ctx.southbridge(), os_ec());
        Ok(())
    }

    fn before_devices(
        &mut self,
        ctx: &mut IntelMainstageBoardCtx<I945Ich7>,
    ) -> Result<(), ServiceError> {
        // Sample the detachable UltraBay before ICH7 programs IDE timings.
        // An absent or disconnected dock cannot supply a primary-channel disk.
        let primary = dock::dock_present(ctx.southbridge()) && dock::ultrabay_present();
        ctx.southbridge().set_ide_primary_enabled(primary);
        Ok(())
    }

    fn after_devices(
        &mut self,
        ctx: &mut IntelMainstageBoardCtx<I945Ich7>,
    ) -> Result<(), ServiceError> {
        // EC/PMH7 hardware setup is independent of ACPI table emission.
        let resume = ctx.resume;
        if !fstart_driver_lenovo::x6::ec_init(ctx.southbridge(), &crate::config::X60_H8, resume) {
            fstart_log::error!("lenovo-x60: H8 EC initialization incomplete");
        }
        if !resume {
            self.ec_oem_string = fstart_driver_lenovo::x6::ec_oem_string(&crate::config::X60_H8);
            if self.ec_oem_string.is_none() {
                fstart_log::error!("lenovo-x60: H8 EC firmware id unavailable");
            }
        }
        dock::mainstage_power_policy(os_ec());
        if !resume {
            if init_ck505(ctx.southbridge()).is_err() {
                fstart_log::error!("lenovo-x60: CK505 programming failed");
            }
            self.identity = match fstart_driver_lenovo::eeprom::Identity::read(ctx.southbridge()) {
                Ok(identity) => Some(identity),
                Err(error) => {
                    fstart_log::error!(
                        "lenovo-x60: EEPROM identity unavailable, code {}",
                        error as u8
                    );
                    None
                }
            };
        }
        if ricoh_sd_write_protect().is_err() {
            fstart_log::error!("lenovo-x60: Ricoh SD policy programming failed");
        }
        Ok(())
    }

    fn before_handoff(
        &mut self,
        ctx: &mut IntelMainstageBoardCtx<I945Ich7>,
    ) -> Result<(), ServiceError> {
        // The shared installer sets SCI_EN directly, without an APMC. Once
        // permanent SMM is installed, route the EC SCI through the board
        // handler's ACPI command (coreboot `i82801gx_set_acpi_mode`).
        use fstart_driver_intel::southbridge::smi::{
            APM_CNT, APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE,
        };
        let command = if ctx.resume {
            APM_CNT_ACPI_ENABLE
        } else {
            APM_CNT_ACPI_DISABLE
        };
        // SAFETY: permanent SMM is installed and owns this command port.
        unsafe { fstart_core::pio::outb(APM_CNT, command) };
        Ok(())
    }
}

/// The firmware's legacy EC channel (the OS owns it once ACPI is enabled).
#[cfg(all(not(test), any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
fn os_ec() -> fstart_driver_lenovo::ec::Ec {
    fstart_driver_lenovo::ec::Ec::new(crate::config::X60_H8.resources.os)
}

#[cfg(all(not(test), fstart_stage_env = "ram"))]
fn init_ck505(bus: &mut impl fstart_core::services::SmBus) -> Result<(), ServiceError> {
    use fstart_driver_intel::generic::ck505::I2cCk505;

    let mut clock = I2cCk505::new_at_address(crate::config::x60_ck505_config(), 0x69)
        .map_err(|_| ServiceError::HardwareError)?;
    clock
        .init_on_smbus(bus)
        .map_err(|_| ServiceError::HardwareError)
}

/// The onboard SD function is device 0, function 2 behind the ICH7 PCI
/// bridge. Enumeration assigns the bus number; the R5C822 helper verifies the
/// identity before touching vendor registers.
#[cfg(all(not(test), fstart_stage_env = "ram"))]
fn ricoh_sd_write_protect() -> Result<(), ServiceError> {
    use fstart_pci::{EcamDevice, PciType1Config};
    use tock_registers::interfaces::Readable;

    let bridge = EcamDevice::new(0, 0x1e, 0);
    // SAFETY: the enumerated ICH7 PCI bridge has a mapped Type 1 header.
    let bus = unsafe { bridge.regs::<PciType1Config>() }
        .secondary_bus
        .get();
    if bus == 0 || bus == 0xff {
        return Err(ServiceError::HardwareError);
    }
    fstart_driver_ricoh::r5c822::configure_sd_write_protect(EcamDevice::new(bus, 0, 2), true)
}

static X60_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["Socket M"];

pub static X60_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        // Linux thinkpad_acpi requires a Lenovo-shaped firmware ID before
        // inspecting the product version. Use coreboot's non-OEM ID, retaining
        // our real firmware identity rather than impersonating a Lenovo BIOS.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X60",
        sys_version: "ThinkPad X60",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X60",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X60_SMBIOS_PROCESSOR_SOCKETS,
        oem_string: None,
    };

#[cfg(fstart_stage_env = "ram")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_platform_intel::i945::I945Ich7AcpiContext;

    /// The X6 family DSDT glue and H8 EC surface (see
    /// [`fstart_driver_lenovo::x6::acpi`]).
    pub fn x60_mainboard_dsdt_aml(context: I945Ich7AcpiContext) -> Vec<u8> {
        fstart_driver_lenovo::x6::acpi::dsdt_aml(&crate::config::X60_H8, context.lpc_scope())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use fstart_acpi::device::AcpiDevice;
        use fstart_driver_intel::i945::IntelI945;
        use fstart_driver_intel::ich7::IntelIch7;
        use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
        use std::fs;
        use std::process::Command;

        fn iasl(dir: &std::path::Path, args: &[&str]) {
            let output = Command::new("iasl")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "iasl {args:?} failed: {}{}",
                std::string::String::from_utf8_lossy(&output.stdout),
                std::string::String::from_utf8_lossy(&output.stderr)
            );
        }

        /// The chipset and board fragments must form one namespace: the
        /// H8 hotkeys call `GFX0.INCB/DECB` from the i945 driver, and the
        /// ICH7 and i945 drivers must not both declare PEGP or GFX0.
        #[test]
        fn complete_dsdt_iasl_round_trip() {
            let north_config = &crate::X60_PLATFORM.northbridge;
            let south_config = &crate::X60_PLATFORM.southbridge;
            let north = IntelI945::new_from_config(north_config).unwrap();
            let south = IntelIch7::new_from_config(south_config).unwrap();

            let mut aml = north.dsdt_aml(north_config);
            aml.extend(south.dsdt_aml(south_config));
            aml.extend(x60_mainboard_dsdt_aml(I945Ich7AcpiContext));
            let dsdt = fstart_acpi::platform::build_dsdt(&aml);

            let dir =
                std::env::temp_dir().join(std::format!("fstart-x60-dsdt-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("dsdt.aml"), &dsdt).unwrap();
            iasl(&dir, &["-d", "dsdt.aml"]);
            let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
            for name in [
                "Device (PEGP)",
                "Device (GFX0)",
                "Device (LCD0)",
                "Device (DOCK)",
            ] {
                assert_eq!(source.matches(name).count(), 1, "{name}");
            }
            // The EC SCI is ICH7 GPIO12, not the X61's GPIO2.
            assert!(source.contains("Name (_GPE, 0x1C)"));
            assert!(source.contains("GFX0.INCB ()"));
            // The ICH7 bridge `_PRT` carries the X60 wiring, not a default.
            assert!(source.contains("0x0008FFFF"));
            fs::rename(dir.join("dsdt.aml"), dir.join("original.aml")).unwrap();
            iasl(&dir, &["-oa", "-tc", "dsdt.dsl"]);
            fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[cfg(fstart_stage_env = "ram")]
pub use acpi_impl::x60_mainboard_dsdt_aml;
