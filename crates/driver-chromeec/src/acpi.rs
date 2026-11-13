//! What an OS needs to find the EC's devices without probing.

extern crate alloc;
use alloc::vec::Vec;
use fstart_acpi_macros::acpi_dsl;

/// The keyboard behind the EC's 8042 interface, for the LPC/eSPI bridge's
/// scope: a Chromebook keyboard (`GOOG000A`) on the standard i8042 ports.
/// The EC signals IRQ 1 as a level-low virtual wire.
pub fn keyboard_aml() -> Vec<u8> {
    acpi_dsl! {
        Device("PS2K") {
            Name("_HID", "GOOG000A");
            Name("_CID", Package(EisaId("PNP0303"), EisaId("PNP030B")));
            Name("_UID", 0u32);
            Name("_CRS", ResourceTemplate {
                IO(0x0060u16, 0x0060u16, 0x01u8, 0x01u8);
                IO(0x0064u16, 0x0064u16, 0x01u8, 0x01u8);
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared, 1u32);
            });
            Method("_STA", 0, NotSerialized) { Return(0x0Fu32); }
        }
    }
    .into()
}

/// What the EC's ACPI devices say of a board, coreboot's
/// `ec/google/chromeec/acpi` with its board defines.
#[derive(Debug, Clone, Copy)]
pub struct Ec {
    /// The GPE of the EC's SCI (`EC_SCI_GPI`).
    pub sci_gpe: u8,
    /// The GPE the EC wakes the system on (`EC_ENABLE_WAKE_PIN`): `_PRW`
    /// of the lid and of the command device.
    pub wake_gpe: Option<u8>,
    /// A USB-PD controller behind the EC (`GOOG0003`).
    pub pd: bool,
    /// Tablet mode reported by the EC's motion sensors (`GOOG0006`).
    pub tablet_mode: bool,
    /// A keyboard backlight the EC drives (`GOOG0002`).
    pub keyboard_backlight: bool,
}

impl Ec {
    /// The host events this AML handles: what the EC should raise its SCI
    /// for ([`crate::Ec::set_sci_mask`]).
    pub const fn sci_events(&self) -> u32 {
        use crate::HostEvent::*;
        let mut mask = LidClosed.mask()
            | LidOpen.mask()
            | AcConnected.mask()
            | AcDisconnected.mask()
            | BatteryLow.mask()
            | BatteryCritical.mask()
            | Battery.mask()
            | Mkbp.mask();
        if self.pd {
            mask |= PdMcu.mask();
        }
        if self.tablet_mode {
            mask |= ModeChange.mask();
        }
        mask
    }

    /// The EC for the LPC/eSPI bridge's scope: its host interface and
    /// memory map (`SIO`), and the ACPI EC `EC0` with the lid, the AC
    /// adapter, the battery and the command device `CREC` that the OS's
    /// ChromeEC driver binds to. Everything refers to the EC by relative
    /// names, so it works below any bridge.
    ///
    /// Unlike coreboot, `CREC` and `KBLT` are below `EC0`, not in `\_SB`:
    /// `CREC` has no interrupt to keep it from the ISA bridge, and nothing
    /// then needs the bridge's path. No `\PNOT` platform notification, no
    /// thermal or DPTF events and no second battery.
    pub fn lpc_aml(&self) -> Result<Vec<u8>, fstart_acpi::AmlError> {
        let mut aml = sio();
        aml.extend(self.ec0()?);
        Ok(aml)
    }

    fn ec0(&self) -> Result<Vec<u8>, fstart_acpi::AmlError> {
        use fstart_acpi::aml_linker::device_vec;
        let sci_gpe = u64::from(self.sci_gpe);
        let mut body = Vec::new();
        acpi_dsl! {
            Name("_HID", EisaId("PNP0C09"));
            Name("_UID", 1u8);
            Name("_GPE", #{byte sci_gpe});
            // The ACPI-accessible EC RAM below EC_ACPI_MEM_MAPPED_BEGIN.
            OperationRegion("ERAM", EmbeddedControl, 0x00u32, 0x20u32);
            Field("ERAM", ByteAcc, Lock, Preserve) {
                Offset(0x03),
                KBLV, 8,
                Offset(0x09),
                TBMD, 1,
                Offset(0x0A),
                DFUD, 1,
                , 2,
                KBLE, 1,
                , 2,
                MTNS, 1,
            }
            // The memory map at EC_LPC_ADDR_MEMMAP (emem.asl).
            OperationRegion("EMEM", SystemIO, 0x900u32, 0xFFu32);
            Field("EMEM", ByteAcc, NoLock, Preserve) {
                Offset(0x30),
                LIDS, 1,
                Offset(0x40),
                BTVO, 32,
                BTPR, 32,
                BTRA, 32,
                ACEX, 1,
                BTEX, 1,
                BFDC, 1,
                BFCG, 1,
                BFCR, 1,
                Offset(0x50),
                BTDA, 32,
                BTDV, 32,
                BTDF, 32,
                BTCC, 32,
                BMFG, 64,
                BMOD, 64,
                BSER, 64,
            }
            Name("_CRS", ResourceTemplate {
                IO(0x62u16, 0x62u16, 0u8, 1u8);
                IO(0x66u16, 0x66u16, 0u8, 1u8);
            });
            // Read the tablet mode.
            Method("RCTM", 0, NotSerialized) { Return(TBMD); }

            Method("_Q01", 0, NotSerialized) { Notify(LID0, 0x80u8); }
            Method("_Q02", 0, NotSerialized) {
                Notify(CREC, 0x02u8);
                Notify(LID0, 0x80u8);
            }
            Method("_Q04", 0, NotSerialized) { Notify(AC, 0x80u8); }
            Method("_Q05", 0, NotSerialized) { Notify(AC, 0x80u8); }
            Method("_Q06", 0, NotSerialized) { Notify(BAT0, 0x80u8); }
            Method("_Q07", 0, NotSerialized) { Notify(BAT0, 0x80u8); }
            Method("_Q08", 0, NotSerialized) { Notify(BAT0, 0x81u8); }
            // Key pressed.
            Method("_Q0D", 0, NotSerialized) { Notify(CREC, 0x02u8); }
            // Battery shutdown imminent and battery status.
            Method("_Q11", 0, NotSerialized) { Notify(BAT0, 0x80u8); }
            Method("_Q17", 0, NotSerialized) { Notify(BAT0, 0x80u8); }
            // EC panic and MKBP event, for the ChromeEC driver.
            Method("_Q18", 0, NotSerialized) { Notify(CREC, 0xB0u8); }
            Method("_Q1B", 0, NotSerialized) { Notify(CREC, 0x80u8); }

            Device("AC") {
                Name("_HID", "ACPI0003");
                Name("_PCL", Package(#{const "\\_SB_"}));
                Method("_PSR", 0, NotSerialized) { Return(ACEX); }
                Method("_STA", 0, NotSerialized) { Return(0x0Fu8); }
            }
        }
        .emit(&mut body)?;
        body.extend(lid(self.wake_gpe)?);
        body.extend(battery());
        body.extend(self.crec()?);
        if self.pd {
            body.extend(Vec::<u8>::from(acpi_dsl! {
                // PD and USB mux events.
                Method("_Q16", 0, NotSerialized) { Notify(#{const "^CREC.ECPD"}, 0x80u8); }
                Method("_Q1C", 0, NotSerialized) { Notify(#{const "^CREC.ECPD"}, 0x80u8); }
            }));
        }
        if self.tablet_mode {
            body.extend(Vec::<u8>::from(acpi_dsl! {
                Method("_Q1D", 0, NotSerialized) {
                    Notify(CREC, 0x02u8);
                    Notify(#{const "^CREC.TBMC"}, 0x80u8);
                }
            }));
        }
        if self.keyboard_backlight {
            body.extend(keyboard_backlight());
        }
        device_vec("EC0", &body)
    }

    /// The command device of the OS's ChromeEC driver, with its MKBP
    /// (`GOOG0007`), PD and tablet-mode children.
    fn crec(&self) -> Result<Vec<u8>, fstart_acpi::AmlError> {
        use fstart_acpi::aml_linker::device_vec;
        let mut body: Vec<u8> = acpi_dsl! {
            Name("_HID", "GOOG0004");
            Name("_UID", 1u8);
            Name("_DDN", "EC Command Device");
            Method("_STA", 0, NotSerialized) { Return(0x0Fu8); }
            Device("CKSC") {
                Name("_HID", "GOOG0007");
                Name("_UID", 1u8);
                Name("_DDN", "EC MKBP Device");
                Method("_STA", 0, NotSerialized) { Return(0x0Fu8); }
            }
        }
        .into();
        if let Some(gpe) = self.wake_gpe {
            body.extend(prw(gpe)?);
        }
        if self.pd {
            body.extend(Vec::<u8>::from(acpi_dsl! {
                Device("ECPD") {
                    Name("_HID", "GOOG0003");
                    Name("_UID", 1u8);
                    Name("_DDN", "EC PD Device");
                    Method("_STA", 0, NotSerialized) { Return(0x0Fu8); }
                }
            }));
        }
        if self.tablet_mode {
            body.extend(Vec::<u8>::from(acpi_dsl! {
                Device("TBMC") {
                    Name("_HID", "GOOG0006");
                    Name("_UID", 1u8);
                    Name("_DDN", "Tablet Motion Control");
                    Method("TBMC", 0, NotSerialized) {
                        If (RCTM() == 1u8) { Return(1u8); }
                        Return(0u8);
                    }
                    Method("_STA", 0, NotSerialized) {
                        If (MTNS == 1u8) { Return(0x0Fu8); }
                        Return(0u8);
                    }
                }
            }));
        }
        device_vec("CREC", &body)
    }
}

/// `_PRW`: wake on `gpe` from up to S5.
fn prw(gpe: u8) -> Result<Vec<u8>, fstart_acpi::AmlError> {
    let gpe = u64::from(gpe);
    let mut aml = Vec::new();
    acpi_dsl! { Name("_PRW", Package(#{byte gpe}, 5u8)); }.emit(&mut aml)?;
    Ok(aml)
}

/// The EC's host interface and memory map as motherboard resources
/// (superio.asl with `SIO_EC_MEMMAP_ENABLE` and `SIO_EC_HOST_ENABLE`).
fn sio() -> Vec<u8> {
    acpi_dsl! {
        Device("SIO") {
            Name("_UID", 0u8);
            Name("_ADR", 0u8);
            Device("ECMM") {
                Name("_HID", EisaId("PNP0C02"));
                Name("_UID", 4u8);
                Method("_STA", 0, NotSerialized) { Return(0x0Fu8); }
                Name("_CRS", ResourceTemplate {
                    IO(0x900u16, 0x900u16, 8u8, 0xFFu8);
                });
            }
            Device("ECUI") {
                Name("_HID", EisaId("PNP0C02"));
                Name("_UID", 3u8);
                Method("_STA", 0, NotSerialized) { Return(0x0Fu8); }
                Name("_CRS", ResourceTemplate {
                    IO(0x200u16, 0x200u16, 1u8, 1u8);
                    IO(0x204u16, 0x204u16, 1u8, 1u8);
                    IO(0x800u16, 0x800u16, 8u8, 0x80u8);
                    IO(0x880u16, 0x880u16, 8u8, 0x80u8);
                });
            }
        }
    }
    .into()
}

/// The lid switch the EC reports in its memory map.
fn lid(wake_gpe: Option<u8>) -> Result<Vec<u8>, fstart_acpi::AmlError> {
    use fstart_acpi::aml_linker::device_vec;
    let mut body: Vec<u8> = acpi_dsl! {
        Name("_HID", EisaId("PNP0C0D"));
        Method("_LID", 0, NotSerialized) { Return(LIDS); }
    }
    .into();
    if let Some(gpe) = wake_gpe {
        body.extend(prw(gpe)?);
    }
    device_vec("LID0", &body)
}

/// The one battery of the EC's memory map (battery.asl, without the
/// second battery and the long-string FIFO): 15% warning, 10% low.
fn battery() -> Vec<u8> {
    acpi_dsl! {
        Device("BAT0") {
            Name("_HID", EisaId("PNP0C0A"));
            Name("_UID", 1u8);
            Name("_PCL", Package(#{const "\\_SB_"}));
            Name("PBIF", Package(
                1u32,
                0xFFFFFFFFu32,
                0xFFFFFFFFu32,
                1u32,
                0xFFFFFFFFu32,
                3u32,
                0xFFFFFFFFu32,
                1u32,
                1u32,
                "",
                "",
                "LION",
                "",
            ));
            Name("PBIX", Package(
                0u32,
                1u32,
                0xFFFFFFFFu32,
                0xFFFFFFFFu32,
                1u32,
                0xFFFFFFFFu32,
                3u32,
                0xFFFFFFFFu32,
                0u32,
                0x00018000u32,
                0x000001F4u32,
                0x0000000Au32,
                0xFFFFFFFFu32,
                0xFFFFFFFFu32,
                1u32,
                1u32,
                "",
                "",
                "LION",
                "",
            ));
            Name("PBST", Package(0u32, 0xFFFFFFFFu32, 0xFFFFFFFFu32, 0xFFFFFFFFu32));
            // The last state _BST reported.
            Name("BSTP", 0u8);
            Method("_STA", 0, Serialized) {
                If (BTEX) { Return(0x1Fu8); }
                Return(0x0Fu8);
            }
            Method("_BIX", 0, Serialized) {
                PBIX[3] = BTDF;
                PBIX[5] = BTDV;
                Local0 = BTDA;
                PBIX[2] = Local0;
                PBIX[6] = Local0 * 15u8 / 100u8;
                PBIX[7] = Local0 * 10u8 / 100u8;
                PBIX[8] = BTCC;
                // Up to eight characters each.
                PBIX[16] = ToString(Concatenate(BMOD, 0u8));
                PBIX[17] = ToString(Concatenate(BSER, 0u8));
                PBIX[19] = ToString(Concatenate(BMFG, 0u8));
                Return(PBIX);
            }
            Method("_BIF", 0, Serialized) {
                Local0 = _BIX();
                PBIF[1] = DeRefOf(Index(Local0, 2u8));
                PBIF[2] = DeRefOf(Index(Local0, 3u8));
                PBIF[4] = DeRefOf(Index(Local0, 5u8));
                PBIF[5] = DeRefOf(Index(Local0, 6u8));
                PBIF[6] = DeRefOf(Index(Local0, 7u8));
                PBIF[9] = DeRefOf(Index(Local0, 16u8));
                PBIF[10] = DeRefOf(Index(Local0, 17u8));
                PBIF[12] = DeRefOf(Index(Local0, 19u8));
                Return(PBIF);
            }
            Method("_BST", 0, Serialized) {
                // Bit 0 discharging, 1 charging, 2 critical; always
                // discharging on battery.
                Local1 = 1u8;
                If (ACEX) {
                    Local1 = 0u8;
                    If (BFCG) {
                        Local1 = 2u8;
                    } Else {
                        If (BFDC) { Local1 = 1u8; }
                    }
                }
                If (BFCR) { Local1 = Local1 | 4u8; }
                PBST[0] = Local1;
                If (Local1 != BSTP) {
                    BSTP = Local1;
                    Notify(BAT0, 0x80u8);
                }
                PBST[1] = BTPR;
                PBST[2] = BTRA;
                PBST[3] = BTVO;
                Return(PBST);
            }
        }
    }
    .into()
}

/// The keyboard backlight the EC drives (keyboard_backlight.asl): present
/// if the EC says so or can't say.
fn keyboard_backlight() -> Vec<u8> {
    acpi_dsl! {
        Device("KBLT") {
            Name("_HID", "GOOG0002");
            Name("_UID", 1u8);
            Method("_STA", 0, NotSerialized) {
                If (DFUD == 1u8) { Return(0x0Fu8); }
                If (KBLE == 1u8) { Return(0x0Fu8); }
                Return(0u8);
            }
            Method("KBQC", 0, NotSerialized) { Return(KBLV); }
            Method("KBCM", 1, NotSerialized) { KBLV = Arg0; }
        }
    }
    .into()
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{fs, process::Command, string::String};

    const VILBOZ: Ec = Ec {
        sci_gpe: 3,
        wake_gpe: Some(15),
        pd: true,
        tablet_mode: true,
        keyboard_backlight: true,
    };

    /// A DSDT with `\_SB.PCI0.LPCB` holding the EC, in a fresh directory.
    fn dsdt(name: &str) -> std::path::PathBuf {
        let lpc = fstart_acpi::aml_linker::device_vec("LPCB", &VILBOZ.lpc_aml().unwrap()).unwrap();
        let pci = fstart_acpi::aml_linker::device_vec("PCI0", &lpc).unwrap();
        let dir = std::env::temp_dir().join(std::format!(
            "fstart-chromeec-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("dsdt.aml"),
            fstart_acpi::platform::build_dsdt(&pci),
        )
        .unwrap();
        dir
    }

    fn run(dir: &std::path::Path, program: &str, args: &[&str]) -> Option<String> {
        match Command::new(program).current_dir(dir).args(args).output() {
            Ok(output) => {
                let text = std::format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(output.status.success(), "{program}: {text}");
                Some(text)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::eprintln!("skipping: {program} not installed");
                None
            }
            Err(error) => panic!("{program}: {error}"),
        }
    }

    #[test]
    fn sci_events_are_the_handled_queries() {
        let plain = Ec {
            pd: false,
            tablet_mode: false,
            ..VILBOZ
        };
        assert_eq!(plain.sci_events(), 0x040000ff & !(1 << 2));
        assert_eq!(VILBOZ.sci_events() & !plain.sci_events(), 1 << 21 | 1 << 28);
    }

    #[test]
    fn disassembles_as_coreboots_objects() {
        let dir = dsdt("dsl");
        let Some(log) = run(&dir, "iasl", &["-d", "dsdt.aml"]) else {
            return;
        };
        assert!(!log.contains("Error"), "{log}");
        let dsl = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
        for object in [
            "Device (SIO)",
            "Device (ECMM)",
            "Device (ECUI)",
            "Device (EC0)",
            "Name (_HID, EisaId (\"PNP0C09\")",
            "Name (_GPE, 0x03)",
            "OperationRegion (ERAM, EmbeddedControl, Zero, 0x20)",
            "OperationRegion (EMEM, SystemIO, 0x0900, 0xFF)",
            "Device (LID0)",
            "Name (_PRW, Package (0x02)",
            "Device (BAT0)",
            "Device (AC)",
            "Name (_HID, \"ACPI0003\"",
            "Device (CREC)",
            "Name (_HID, \"GOOG0004\"",
            "Device (CKSC)",
            "Device (ECPD)",
            "Device (TBMC)",
            "Device (KBLT)",
            "Method (_Q1B, 0, NotSerialized)",
            "Notify (^CREC.ECPD, 0x80)",
            "ToString (Concatenate (BMOD, Zero), Ones)",
        ] {
            assert!(dsl.contains(object), "{object} missing:\n{dsl}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn methods_execute() {
        let dir = dsdt("exec");
        let methods = [
            "BAT0._STA",
            "BAT0._BIX",
            "BAT0._BIF",
            "BAT0._BST",
            "LID0._LID",
            "AC._PSR",
            "CREC._STA",
            "CREC.TBMC.TBMC",
            "CREC.TBMC._STA",
            "KBLT._STA",
            "KBLT.KBQC",
            "_Q02",
            "_Q08",
            "_Q16",
            "_Q1D",
        ];
        let batch = methods
            .iter()
            .map(|method| std::format!("execute \\_SB.PCI0.LPCB.EC0.{method}"))
            .collect::<std::vec::Vec<_>>()
            .join(";");
        let Some(log) = run(
            &dir,
            "acpiexec",
            &["-dt", "-fv", "0", "-b", &batch, "dsdt.aml"],
        ) else {
            return;
        };
        assert!(!log.contains("AE_") && !log.contains("ACPI Error"), "{log}");
        assert_eq!(log.matches("Evaluating ").count(), methods.len(), "{log}");
        // Battery absent, on battery: present but no battery, discharging.
        assert!(log.contains("[Integer] = 000000000000000F"), "{log}");
        fs::remove_dir_all(dir).unwrap();
    }
}
