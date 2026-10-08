//! Lenovo ThinkPad X61 mainboard glue.
//!
//! This module intentionally contains the board-specific parts that do not
//! belong in the reusable GM965 northbridge or ICH8 southbridge drivers.  The
//! important pre-console path is the X6 UltraBase dock: ICH8 opens the LPC/GPIO
//! decode windows, then this driver initializes the laptop-side DLPC, connects
//! the dock-side LPC bus when present, and enables the dock PC87392 COM1 before
//! the NS16550 console driver probes port 0x3f8.

#![allow(clippy::result_unit_err)]

#[cfg(all(not(test), any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
use crate::config::Hardware as Gm965Ich8;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
use fstart_core::services::ServiceError;
#[cfg(all(not(test), fstart_stage_env = "car"))]
use fstart_platform_intel::{IntelEarlyBoardHooks, IntelEarlyCtx};
#[cfg(all(not(test), fstart_stage_env = "ram"))]
use fstart_platform_intel::{IntelMainstageBoardCtx, IntelMainstageBoardHooks};

#[cfg(all(not(test), any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
use fstart_driver_lenovo::x6::dock as x6_dock;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
use fstart_platform_intel::{IntelSmbusRouting, SmbusRoute};

/// Board-specific X61 hooks for the GM965/ICH8 flow.
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
#[derive(Default)]
pub struct X61Mainboard {
    #[cfg(fstart_stage_env = "ram")]
    identity: Option<fstart_driver_lenovo::eeprom::Identity>,
    #[cfg(fstart_stage_env = "ram")]
    ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
}

#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
impl<S: fstart_core::services::Southbridge> IntelSmbusRouting<S> for X61Mainboard {
    fn select_smbus(southbridge: &S, route: SmbusRoute) -> Result<(), ServiceError> {
        use fstart_driver_intel::southbridge::gpio_ich::GpioLevel;
        let spd_level = matches!(crate::config::X61_SMBUS_MUX.level, GpioLevel::High);
        let level = match route {
            SmbusRoute::Spd => spd_level,
            SmbusRoute::Eeprom => !spd_level,
        };
        southbridge.gpio_set(crate::config::X61_SMBUS_MUX.pin as u32, level)
    }
}

/// The mainboard contributes ACPI fragments through the same `AcpiDevice`
/// abstraction the chipset drivers use.
#[cfg(fstart_stage_env = "ram")]
mod mainboard_acpi_device {
    extern crate alloc;

    use super::{X61Mainboard, x61_mainboard_dsdt_aml};

    impl fstart_acpi::device::AcpiDevice for X61Mainboard {
        type Config = fstart_platform_intel::gm965::Gm965Ich8AcpiContext;

        fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
            x61_mainboard_dsdt_aml(*config)
        }
    }
}

#[cfg(all(not(test), fstart_stage_env = "car"))]
impl IntelEarlyBoardHooks<Gm965Ich8> for X61Mainboard {
    fn before_console(&mut self, ctx: &mut IntelEarlyCtx<Gm965Ich8>) -> Result<(), ServiceError> {
        setup_dock_console(ctx.southbridge());
        Ok(())
    }
}

#[cfg(all(not(test), fstart_stage_env = "ram"))]
impl IntelMainstageBoardHooks<Gm965Ich8> for X61Mainboard {
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
        ctx: &mut IntelMainstageBoardCtx<Gm965Ich8>,
    ) -> Result<(), ServiceError> {
        setup_dock_console(ctx.southbridge());
        Ok(())
    }

    fn before_devices(
        &mut self,
        ctx: &mut IntelMainstageBoardCtx<Gm965Ich8>,
    ) -> Result<(), ServiceError> {
        // Sample the detachable UltraBay before ICH8 programs IDE timings.
        // An absent or disconnected dock cannot supply a primary-channel disk.
        let primary = x6_dock::dock_present(ctx.southbridge()) && x6_dock::ultrabay_present();
        ctx.southbridge().set_ide_primary_enabled(primary);
        Ok(())
    }

    fn after_devices(
        &mut self,
        ctx: &mut IntelMainstageBoardCtx<Gm965Ich8>,
    ) -> Result<(), ServiceError> {
        // EC/PMH7 hardware setup is independent of ACPI table emission.
        let resume = ctx.resume;
        if !fstart_driver_lenovo::x6::ec_init(ctx.southbridge(), &crate::config::X61_H8, resume) {
            fstart_log::error!("lenovo-x61: H8 EC initialization incomplete");
        }
        if !resume {
            self.ec_oem_string = fstart_driver_lenovo::x6::ec_oem_string(&crate::config::X61_H8);
            if self.ec_oem_string.is_none() {
                fstart_log::error!("lenovo-x61: H8 EC firmware id unavailable");
            }
        }
        x6_dock::mainstage_power_policy(os_ec());
        if !resume && init_ck505(ctx.southbridge()).is_err() {
            fstart_log::error!("lenovo-x61: CK505 programming failed");
        }
        if !resume {
            // init_ck505 explicitly restores the EEPROM mux branch. Read-only
            // identity access must not modify EEPROM/RFID protection registers.
            self.identity = match fstart_driver_lenovo::eeprom::Identity::read(ctx.southbridge()) {
                Ok(identity) => Some(identity),
                Err(error) => {
                    fstart_log::error!(
                        "lenovo-x61: EEPROM identity unavailable, code {}",
                        error as u8
                    );
                    None
                }
            };
        }
        if ricoh_sd_write_protect().is_err() {
            fstart_log::error!("lenovo-x61: Ricoh SD policy programming failed");
        }
        Ok(())
    }

    fn before_handoff(
        &mut self,
        ctx: &mut IntelMainstageBoardCtx<Gm965Ich8>,
    ) -> Result<(), ServiceError> {
        // The shared installer sets SCI_EN directly, without an APMC. Once
        // permanent SMM is installed, initialize board EC routing via its
        // existing ACPI command handler (keep ACPI ownership on resume).
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

#[cfg(all(not(test), any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
fn setup_dock_console(southbridge: &mut fstart_driver_intel::ich8::IntelIch8) {
    x6_dock::setup_console(southbridge, os_ec());
}

/// The firmware's legacy EC channel (the OS owns it once ACPI is enabled).
#[cfg(all(not(test), any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
fn os_ec() -> fstart_driver_lenovo::ec::Ec {
    fstart_driver_lenovo::ec::Ec::new(crate::config::X61_H8.resources.os)
}

/// CK505 shares the SPD branch. After selection, check EEPROM restoration
/// even if programming fails; never hide selector errors in a destructor.
#[cfg(fstart_stage_env = "ram")]
fn init_ck505(
    southbridge: &mut (impl fstart_core::services::Southbridge + fstart_core::services::SmBus),
) -> Result<(), ServiceError> {
    use fstart_driver_intel::generic::ck505::I2cCk505;

    let mut clock = I2cCk505::new_at_address(crate::config::x61_ck505_config(), 0x69)
        .map_err(|_| ServiceError::HardwareError)?;
    X61Mainboard::with_spd(southbridge, |bus| {
        clock
            .init_on_smbus(bus)
            .map_err(|_| ServiceError::HardwareError)
    })
}

/// The onboard SD function is device 0, function 2 behind ICH8's PCI bridge.
/// Enumeration assigns the bus number; the board owns the location/polarity,
/// and the R5C822 helper verifies the identity before touching vendor registers.
#[cfg(all(not(test), fstart_stage_env = "ram"))]
fn ricoh_sd_write_protect() -> Result<(), ServiceError> {
    use fstart_driver_intel::ich8::ich8::{PCI_BRIDGE_DEV, PCI_BRIDGE_FUNC};
    use fstart_pci::{EcamDevice, PciType1Config};
    use tock_registers::interfaces::Readable;

    let bridge = EcamDevice::new(0, PCI_BRIDGE_DEV, PCI_BRIDGE_FUNC);
    // SAFETY: the enumerated ICH8 PCI bridge has a mapped Type 1 header.
    let bus = unsafe { bridge.regs::<PciType1Config>() }
        .secondary_bus
        .get();
    if bus == 0 || bus == 0xff {
        return Err(ServiceError::HardwareError);
    }
    fstart_driver_ricoh::r5c822::configure_sd_write_protect(EcamDevice::new(bus, 0, 2), true)
}

static X61_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["Socket M"];

pub static X61_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        // Linux thinkpad_acpi requires a Lenovo-shaped firmware ID before
        // inspecting the product version. Use coreboot's non-OEM ID, retaining
        // our real firmware identity rather than impersonating a Lenovo BIOS.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X61",
        sys_version: "ThinkPad X61",
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X61",
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X61_SMBIOS_PROCESSOR_SOCKETS,
oem_string: None,
    };

#[cfg(fstart_stage_env = "ram")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_platform_intel::gm965::Gm965Ich8AcpiContext;

    /// Assemble the X61 DSDT: board glue (TCO dock commands, sleep/wake hooks,
    /// dock, GPE routing) plus the complete H8 EC surface from the Lenovo
    /// driver.
    pub fn x61_mainboard_dsdt_aml(context: Gm965Ich8AcpiContext) -> Vec<u8> {
        fstart_driver_lenovo::x6::acpi::dsdt_aml(&crate::config::X61_H8, context.lpc_scope())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use fstart_acpi::device::AcpiDevice;
        use fstart_driver_intel::gm965::IntelGm965;
        use fstart_driver_intel::ich8::IntelIch8;
        use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
        use std::fs;
        use std::process::Command;

        struct ClockBus {
            mux: std::sync::Mutex<std::vec::Vec<(u32, bool)>>,
            block: [u8; 4],
            fail_transfer: bool,
            fail_level: Option<bool>,
            transfers: usize,
        }

        impl fstart_core::services::Southbridge for ClockBus {
            fn gpio_set(&self, pin: u32, high: bool) -> Result<(), crate::mainboard::ServiceError> {
                self.mux.lock().unwrap().push((pin, high));
                if self.fail_level == Some(high) {
                    Err(crate::mainboard::ServiceError::IoError)
                } else {
                    Ok(())
                }
            }
        }

        impl fstart_core::services::SmBus for ClockBus {
            fn read_byte(&mut self, _: u8, _: u8) -> Result<u8, crate::mainboard::ServiceError> {
                Err(crate::mainboard::ServiceError::NotSupported)
            }
            fn write_byte(
                &mut self,
                _: u8,
                _: u8,
                _: u8,
            ) -> Result<(), crate::mainboard::ServiceError> {
                Err(crate::mainboard::ServiceError::NotSupported)
            }
            fn block_read(
                &mut self,
                _: u8,
                _: u8,
                data: &mut [u8],
            ) -> Result<usize, crate::mainboard::ServiceError> {
                assert_eq!(self.mux.lock().unwrap().last(), Some(&(42, true)));
                self.transfers += 1;
                if self.fail_transfer {
                    return Err(crate::mainboard::ServiceError::IoError);
                }
                data[..4].copy_from_slice(&self.block);
                Ok(4)
            }
            fn block_write(
                &mut self,
                _: u8,
                _: u8,
                data: &[u8],
            ) -> Result<(), crate::mainboard::ServiceError> {
                assert_eq!(self.mux.lock().unwrap().last(), Some(&(42, true)));
                self.block.copy_from_slice(data);
                Ok(())
            }
        }

        #[test]
        fn clock_mux_transitions_are_checked_even_when_the_transfer_fails() {
            use crate::mainboard::ServiceError;
            for fail_transfer in [false, true] {
                for fail_level in [None, Some(true), Some(false)] {
                    let mut bus = ClockBus {
                        mux: std::sync::Mutex::new(std::vec::Vec::new()),
                        block: [0x04, 0xa5, 0x5a, 0xff],
                        fail_transfer,
                        fail_level,
                        transfers: 0,
                    };
                    let expected = if fail_level.is_some() {
                        Err(ServiceError::IoError)
                    } else if fail_transfer {
                        Err(ServiceError::HardwareError)
                    } else {
                        Ok(())
                    };
                    assert_eq!(crate::mainboard::init_ck505(&mut bus), expected);
                    let selections = bus.mux.lock().unwrap();
                    if fail_level == Some(true) {
                        assert_eq!(*selections, [(42, true)]);
                        assert_eq!(bus.transfers, 0);
                    } else {
                        assert_eq!(*selections, [(42, true), (42, false)]);
                        assert!(bus.transfers > 0);
                    }
                }
            }
        }

        #[test]
        fn complete_dsdt_iasl_round_trip() {
            let north_config = &crate::X61_PLATFORM.northbridge;
            let south_config = &crate::X61_PLATFORM.southbridge;
            let north = IntelGm965::new_from_config(north_config).unwrap();
            let south = IntelIch8::new_from_config(south_config).unwrap();

            let mut aml = north.dsdt_aml(north_config);
            aml.extend(south.dsdt_aml(south_config));
            aml.extend(x61_mainboard_dsdt_aml(Gm965Ich8AcpiContext));
            let dsdt = fstart_acpi::platform::build_dsdt(&aml);

            let dir =
                std::env::temp_dir().join(std::format!("fstart-x61-dsdt-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("dsdt.aml"), &dsdt).unwrap();

            let disassemble = Command::new("iasl")
                .current_dir(&dir)
                .args(["-d", "dsdt.aml"])
                .output()
                .unwrap();
            assert!(
                disassemble.status.success(),
                "iasl -d failed: {}{}",
                std::string::String::from_utf8_lossy(&disassemble.stdout),
                std::string::String::from_utf8_lossy(&disassemble.stderr)
            );
            let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
            for method in ["_Q18", "_Q37", "_Q50", "_Q58", "BRTU", "BRTD"] {
                assert!(source.contains(&std::format!("Method ({method},")));
            }
            for (method, event) in [("_Q37", "Zero"), ("_Q58", "Zero"), ("_Q50", "0x03")] {
                let body = source
                    .split(&std::format!("Method ({method},"))
                    .nth(1)
                    .unwrap()
                    .split('}')
                    .next()
                    .unwrap();
                assert!(body.contains(&std::format!("Notify (\\_SB.DOCK, {event})")));
            }
            assert!(source.contains("HKEY.RHK (0x09)"));
            assert!(source.contains("GFX0.INCB ()"));
            assert!(source.contains("GFX0.DECB ()"));
            assert!(!source.contains("SMIF"));
            assert!(!source.contains("Method (TRAP,"));
            fs::rename(dir.join("dsdt.aml"), dir.join("original.aml")).unwrap();
            let compile = Command::new("iasl")
                .current_dir(&dir)
                .args(["-oa", "-tc", "dsdt.dsl"])
                .output()
                .unwrap();
            assert!(
                compile.status.success(),
                "iasl -tc failed: {}{}",
                std::string::String::from_utf8_lossy(&compile.stdout),
                std::string::String::from_utf8_lossy(&compile.stderr)
            );

            // ACPICA narrows pre-existing shared Intel integer encodings on
            // the first pass. Once canonicalized, require stable AML bytes.
            let canonical = fs::read(dir.join("dsdt.aml")).unwrap();
            fs::rename(dir.join("dsdt.dsl"), dir.join("original.dsl")).unwrap();
            for arguments in [&["-d", "dsdt.aml"][..], &["-oa", "-tc", "dsdt.dsl"][..]] {
                let output = Command::new("iasl")
                    .current_dir(&dir)
                    .args(arguments)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "canonical roundtrip failed: {}{}",
                    std::string::String::from_utf8_lossy(&output.stdout),
                    std::string::String::from_utf8_lossy(&output.stderr)
                );
            }
            let recompiled = fs::read(dir.join("dsdt.aml")).unwrap();
            // Ignore only compiler-identification fields in the table header.
            assert_eq!(&recompiled[36..], &canonical[36..]);
            fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[cfg(fstart_stage_env = "ram")]
pub use acpi_impl::x61_mainboard_dsdt_aml;
