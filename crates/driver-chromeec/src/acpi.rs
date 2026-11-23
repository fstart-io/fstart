// SPDX-License-Identifier: MIT OR Apache-2.0
//! What an OS needs to find the EC's devices without probing.

extern crate alloc;
use alloc::vec::Vec;
use fstart_acpi_macros::acpi_dsl;

/// The keyboard behind the EC's 8042 interface, for the LPC/eSPI bridge's
/// scope: a Chromebook keyboard (`GOOG000A`) on the standard i8042 ports.
/// The EC signals IRQ 1 as a level-low virtual wire.
pub fn keyboard_aml() -> Vec<u8> {
    keyboard_aml_with_irq(&fstart_acpi::aml::Interrupt::new(
        true, false, true, true, 1,
    ))
}

/// Use the board's IRQ resource: a legacy LPC SERIRQ and an eSPI virtual
/// wire do not have the same polarity, triggering or sharing.
pub fn keyboard_aml_with_irq(irq: &dyn fstart_acpi::Aml) -> Vec<u8> {
    use fstart_acpi::{
        Aml,
        aml::{IO, Name, ResourceTemplate},
        aml_linker::device_vec,
    };
    let mut body: Vec<u8> = acpi_dsl! {
        Name("_HID", "GOOG000A");
        Name("_CID", Package(EisaId("PNP0303"), EisaId("PNP030B")));
        Name("_UID", 0u32);
        Method("_STA", 0, NotSerialized) { Return(0x0Fu32); }
    }
    .into();
    let (data, command) = (IO::new(0x60, 0x60, 1, 1), IO::new(0x64, 0x64, 1, 1));
    Name::new(
        "_CRS".into(),
        &ResourceTemplate::new(alloc::vec![&data, &command, irq]),
    )
    .to_aml_bytes(&mut body);
    device_vec("PS2K", &body).expect("EC keyboard")
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
    /// Matrix Keyboard Protocol child (CKSC), not the LPC i8042 keyboard.
    pub mkbp: bool,
    /// A USB-PD controller behind the EC (`GOOG0003`).
    pub pd: bool,
    /// Tablet mode reported by the EC's motion sensors (`GOOG0006`).
    pub tablet_mode: bool,
    /// A keyboard backlight the EC drives (`GOOG0002`).
    pub keyboard_backlight: bool,
}

/// Board ACPI targets for ChromeEC thermal query notifications (`_Q0A`/`_Q0B`/`_Q10`)
/// and throttle control (`_Q12`/`_Q13`).
#[derive(Default, Clone, Copy)]
pub struct ThermalPolicy {
    /// ACPI object notified with `0x80` on thermal overload/shutdown (`_Q0A`, `_Q0B`, `_Q10`).
    pub notify_target: Option<&'static str>,
    /// One-argument ACPI method invoked with `1` (`_Q12`) / `0` (`_Q13`) for thermal throttling.
    pub throttle_method: Option<&'static str>,
}

/// Board ACPI objects maintained by the EC across `_REG` and AC-state changes.
#[derive(Clone, Copy)]
pub struct PowerNotify {
    /// Zero-argument platform CPU power notification method (e.g. `"\\PNOT"`).
    pub method: &'static str,
    /// Platform AC-state integer updated from `ACEX` (e.g. `"\\PWRS"`).
    pub ac_state: &'static str,
    /// Platform lid-state integer updated from `LIDS` in `_REG` (e.g. `"\\LIDS"`).
    pub lid_state: &'static str,
}

/// Optional LPC AML behavior, independent of which EC peripherals exist.
#[derive(Default, Clone, Copy)]
pub struct LpcPolicy {
    /// Emit EC thermal sensor/PAT/charger methods and optional thermal event handlers.
    pub thermal: Option<ThermalPolicy>,
    /// Platform power-state fields and CPU notification method maintained on `_REG`/`_Q04`/`_Q05`/`_Q17`.
    pub power_notify: Option<PowerNotify>,
    /// External ACPI path for `CREC` when the board places `crec()` outside `EC0`
    /// (e.g. `Some("\\_SB_.CREC")`). When `None`, `CREC` is emitted directly under `EC0`.
    /// PD/tablet child routing on an external `CREC` path is not implemented.
    pub external_crec: Option<&'static str>,
    /// Older ECs don't compensate remaining capacity near full on AC.
    pub full_battery_workaround: bool,
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
            | Battery.mask();
        if self.mkbp {
            mask |= Mkbp.mask();
        }
        if self.pd {
            mask |= PdMcu.mask();
        }
        if self.tablet_mode {
            mask |= ModeChange.mask();
        }
        mask
    }

    /// Additional SCI bits for a board enabling [`LpcPolicy::thermal`].
    pub const THERMAL_EVENTS: u32 = crate::HostEvent::THERMAL_EVENTS;

    /// The EC for the LPC/eSPI bridge's scope: its host interface and
    /// memory map (`SIO`), and the ACPI EC `EC0` with the lid, the AC
    /// adapter, the battery and the command device `CREC` that the OS's
    /// ChromeEC driver binds to. Everything refers to the EC by relative
    /// names, so it works below any bridge.
    ///
    /// Unlike coreboot, the default `CREC` and `KBLT` are below `EC0`, not in `\_SB`:
    /// `CREC` has no interrupt to keep it from the ISA bridge, and nothing
    /// then needs the bridge's path. No platform notification, thermal or
    /// DPTF events, or second battery are emitted by default.
    pub fn lpc_aml(&self) -> Result<Vec<u8>, fstart_acpi::AmlError> {
        self.lpc_aml_with_policy(LpcPolicy::default())
    }

    pub fn lpc_aml_with_policy(&self, policy: LpcPolicy) -> Result<Vec<u8>, fstart_acpi::AmlError> {
        if policy.external_crec.is_some() && (self.pd || self.tablet_mode) {
            return Err(fstart_acpi::AmlError::InvalidPath);
        }
        let mut aml = sio();
        aml.extend(self.ec0(policy)?);
        Ok(aml)
    }

    fn ec0(&self, policy: LpcPolicy) -> Result<Vec<u8>, fstart_acpi::AmlError> {
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
        }
        .emit(&mut body)?;
        let crec_path = fstart_acpi::aml::Path::new(policy.external_crec.unwrap_or("CREC"));
        {
            use fstart_acpi::{
                Aml,
                aml::{Method, MethodCall, Notify, Path, Store},
            };
            let lid_path = Path::new("LID0");
            let notify_crec_wake = Notify::new(&crec_path, &2u8);
            let notify_lid = Notify::new(&lid_path, &0x80u8);
            Method::new(
                "_Q02".into(),
                0,
                false,
                alloc::vec![&notify_crec_wake, &notify_lid],
            )
            .to_aml_bytes(&mut body);

            if let Some(notify) = policy.power_notify {
                let pntf_call = MethodCall::new(Path::new(notify.method), alloc::vec![]);
                Method::new("PNTF".into(), 0, false, alloc::vec![&pntf_call])
                    .to_aml_bytes(&mut body);
                let ac_state = Path::new(notify.ac_state);
                let lid_state = Path::new(notify.lid_state);
                let acex = Path::new("ACEX");
                let lids = Path::new("LIDS");
                let ac_dev = Path::new("AC__");
                let call_pntf = MethodCall::new(Path::new("PNTF"), alloc::vec![]);
                let store_ac = Store::new(&ac_state, &acex);
                let store_lid = Store::new(&lid_state, &lids);
                let notify_ac = Notify::new(&ac_dev, &0x80u8);
                Method::new(
                    "REGI".into(),
                    0,
                    false,
                    alloc::vec![&store_ac, &store_lid, &call_pntf],
                )
                .to_aml_bytes(&mut body);
                Method::new(
                    "ACUP".into(),
                    0,
                    false,
                    alloc::vec![&store_ac, &notify_ac, &call_pntf],
                )
                .to_aml_bytes(&mut body);
                body.extend_from_slice(&acpi_dsl! {
                    Method("_REG", 2, NotSerialized) { If ((Arg0 == 3u8) && (Arg1 == 1u8)) { REGI(); } }
                    Method("_Q04", 0, NotSerialized) { ACUP(); }
                    Method("_Q05", 0, NotSerialized) { ACUP(); }
                });
            } else {
                body.extend_from_slice(&acpi_dsl! {
                    Method("_Q04", 0, NotSerialized) { Notify(AC, 0x80u8); }
                    Method("_Q05", 0, NotSerialized) { Notify(AC, 0x80u8); }
                });
            }
            body.extend_from_slice(&acpi_dsl! {
                Method("_Q06", 0, NotSerialized) { Notify(BAT0, 0x80u8); }
                Method("_Q07", 0, NotSerialized) { Notify(BAT0, 0x80u8); }
                Method("_Q08", 0, NotSerialized) { Notify(BAT0, 0x81u8); }
            });
            Method::new("_Q0D".into(), 0, false, alloc::vec![&notify_crec_wake])
                .to_aml_bytes(&mut body);
            body.extend_from_slice(&acpi_dsl! {
                Method("_Q11", 0, NotSerialized) { Notify(BAT0, 0x80u8); }
            });
            if policy.power_notify.is_some() {
                body.extend_from_slice(&acpi_dsl! {
                    Method("_Q17", 0, NotSerialized) { Notify(BAT0, 0x80u8); PNTF(); }
                });
            } else {
                body.extend_from_slice(&acpi_dsl! {
                    Method("_Q17", 0, NotSerialized) { Notify(BAT0, 0x80u8); }
                });
            }
            let notify_crec_b0 = Notify::new(&crec_path, &0xb0u8);
            let notify_crec_80 = Notify::new(&crec_path, &0x80u8);
            Method::new("_Q18".into(), 0, false, alloc::vec![&notify_crec_b0])
                .to_aml_bytes(&mut body);
            Method::new("_Q1B".into(), 0, false, alloc::vec![&notify_crec_80])
                .to_aml_bytes(&mut body);
        }
        body.extend_from_slice(&acpi_dsl! { Device("AC") {
            Name("_HID", "ACPI0003"); Name("_PCL", Package(#{const "\\_SB_"}));
            Method("_PSR", 0, NotSerialized) { Return(ACEX); }
            Method("_STA", 0, NotSerialized) { Return(0x0fu8); }
        }});
        if let Some(thermal) = policy.thermal {
            use fstart_acpi::{
                Aml, RawAml,
                aml::{Acquire, Else, If, Method, MethodCall, Notify, Path, Return},
            };
            body.extend_from_slice(&acpi_dsl! {
                Name("TNCA", 0xfcu8); Name("TNOP", 0xfdu8); Name("TBAD", 0xfeu8); Name("TNPR", 0xffu8); Name("TOFS", 200u8);
                Method("TINS", 1, Serialized) {
                    Local0 = 0x900u32;
                    If (Arg0 < 10u32) { Local0 = Local0 + Arg0; }
                    OperationRegion("TMAP", SystemIO, Local0, 1u32);
                    Field("TMAP", ByteAcc, NoLock, Preserve) { TEMP, 8, }
                    Return(TEMP);
                }
                Field("ERAM", ByteAcc, Lock, Preserve) { Offset(0x05), PATI, 8, PATT, 8, PATC, 8, CHGL, 8, }
                Method("TSRD", 1, Serialized) { Local0 = TINS(Arg0); If (Local0 >= TNCA) { Return(0u8); } Return((Local0 + TOFS) * 10u8); }
                Mutex("PATM", 0);
                Method("PAT0", 2, Serialized) { Return(PATS(Arg0, Arg1, 2u8)); }
                Method("PAT1", 2, Serialized) { Return(PATS(Arg0, Arg1, 3u8)); }
                Method("CHGS", 1, Serialized) { CHGL = Arg0; } Method("CHGD", 0, Serialized) { CHGL = 0xffu8; }
            });
            let acquire = Acquire::new("PATM".into(), 1000);
            let zero = Return::new(&0u8);
            let timeout = If::new(&acquire, alloc::vec![&zero]);
            let pats = acpi_dsl! { PATI = Arg0; PATT = (Arg1 / 10u8) - TOFS; PATC = Arg2; Release(PATM); Return(1u8); };
            let patd = acpi_dsl! { PATI = Arg0; PATT = 0u8; PATC = 0u8; PATC = 1u8; Release(PATM); Return(1u8); };
            let query = acpi_dsl! { Local0 = 16u8; While ((Local0 > 0u8) && (PATI != TNPR)) { Local0--; } Release(PATM); };
            for (name, n, ops) in [("PATS", 3, &pats[..]), ("PATD", 1, &patd[..])] {
                Method::new(name.into(), n, true, alloc::vec![&timeout, &RawAml(ops)])
                    .to_aml_bytes(&mut body);
            }
            let skip = If::new(&acquire, alloc::vec![]);
            let query_aml = RawAml(&query);
            let acquired = Else::new(alloc::vec![&query_aml]);
            Method::new("_Q09".into(), 0, true, alloc::vec![&skip, &acquired])
                .to_aml_bytes(&mut body);
            if let Some(target) = thermal.notify_target {
                let path = Path::new(target);
                let notify = Notify::new(&path, &0x80u8);
                for name in ["_Q0A", "_Q0B", "_Q10"] {
                    Method::new(name.into(), 0, false, alloc::vec![&notify])
                        .to_aml_bytes(&mut body);
                }
            }
            if let Some(throttle) = thermal.throttle_method {
                for (name, state) in [("_Q12", 1u8), ("_Q13", 0u8)] {
                    let call = MethodCall::new(Path::new(throttle), alloc::vec![&state]);
                    Method::new(name.into(), 0, false, alloc::vec![&call]).to_aml_bytes(&mut body);
                }
            }
        }
        body.extend(lid(self.wake_gpe)?);
        body.extend(battery(policy.full_battery_workaround));
        if policy.external_crec.is_none() {
            body.extend(self.crec()?);
        }
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
    pub fn crec(&self) -> Result<Vec<u8>, fstart_acpi::AmlError> {
        use fstart_acpi::aml_linker::device_vec;
        let mut body: Vec<u8> = acpi_dsl! {
            Name("_HID", "GOOG0004");
            Name("_UID", 1u8);
            Name("_DDN", "EC Command Device");
            Method("_STA", 0, NotSerialized) { Return(0x0Fu8); }
        }
        .into();
        if self.mkbp {
            body.extend_from_slice(&acpi_dsl! {Device("CKSC") {
                Name("_HID", "GOOG0007");
                Name("_UID", 1u8);
                Name("_DDN", "EC MKBP Device");
                Method("_STA", 0, NotSerialized) { Return(0x0Fu8); }
            }});
        }
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
fn battery(full_workaround: bool) -> Vec<u8> {
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
                Local2 = BTRA;
                If (#{byte u8::from(full_workaround)} && ACEX && !BFDC && !BFCG) {
                    Local3 = BTDF;
                    Local4 = Local3 >> 4u8;
                    If ((Local2 > (Local3 - Local4)) && (Local2 < (Local3 + Local4))) { Local2 = Local3; }
                }
                PBST[2] = Local2;
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
        mkbp: true,
        wake_gpe: Some(15),
        pd: true,
        tablet_mode: true,
        keyboard_backlight: true,
    };

    /// A DSDT with `\_SB.PCI0.LPCB` holding the EC, in a fresh directory.
    fn dsdt(name: &str) -> std::path::PathBuf {
        dsdt_with(name, VILBOZ.lpc_aml().unwrap(), &[])
    }

    fn dsdt_with(name: &str, ec: Vec<u8>, extra: &[u8]) -> std::path::PathBuf {
        let lpc = fstart_acpi::aml_linker::device_vec("LPCB", &ec).unwrap();
        let mut pci = fstart_acpi::aml_linker::device_vec("PCI0", &lpc).unwrap();
        pci.extend_from_slice(extra);
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
    fn full_capacity_workaround_only_for_idle_battery_on_ac() {
        let fixture = acpi_dsl! { Scope("\\_SB_.PCI0.LPCB.EC0_") {
            Method("BSET",2,Serialized) { ACEX=1u8; BFDC=Arg0; BFCG=Arg1; BTRA=975u32; BTDF=1000u32; }
        }};
        for enabled in [true, false] {
            let ec = VILBOZ
                .lpc_aml_with_policy(LpcPolicy {
                    full_battery_workaround: enabled,
                    ..Default::default()
                })
                .unwrap();
            let dir = dsdt_with("bst", ec, &fixture);
            let commands = ["0 0", "0 1", "1 0"].map(|args| std::format!(r"execute \_SB.PCI0.LPCB.EC0.BSET {args};execute \_SB.PCI0.LPCB.EC0.BAT0._BST")).join(";");
            let Some(log) = run(
                &dir,
                "acpiexec",
                &["-dt", "-fv", "0", "-b", &commands, "dsdt.aml"],
            ) else {
                return;
            };
            assert!(!log.contains("AE_") && !log.contains("ACPI Error"), "{log}");
            let values: Vec<_> = log
                .lines()
                .filter_map(|l| l.trim().strip_prefix("[Integer] = "))
                .map(|v| u32::from_str_radix(v, 16).unwrap())
                .collect();
            assert_eq!(
                values,
                [
                    0,
                    0,
                    if enabled { 1000 } else { 975 },
                    0,
                    2,
                    0,
                    975,
                    0,
                    1,
                    0,
                    975,
                    0
                ],
                "{log}"
            );
            fs::remove_dir_all(dir).unwrap();
        }
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
