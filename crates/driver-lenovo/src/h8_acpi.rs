//! Complete H8 DSDT surface, ported from coreboot `ec/lenovo/h8/acpi/*.asl`
//! into `acpi_dsl!` fragments.
//!
//! The fragments are concatenated by [`dsdt_aml`] and expect to be placed
//! inside a single DSDT:
//!
//! 1. root-scope helper names (`\TCRT`, `\TPSV`, `\FLVL`, `\PWRS`, `\PNOT`,
//!    `\PPKG`, brightness stubs) that the EC methods and thermal zones
//!    reference,
//! 2. the `EC__` device under the board's LPC scope (plus the ECMM/ECGS/TWRI
//!    raw-resource devices),
//! 3. the `\_TZ` thermal zones with the fan power resource,
//! 4. the `\_SI._SST` system-status indicator,
//! 5. optionally, HKEY charge-behaviour / threshold extensions.

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::vec::Vec;
use fstart_acpi_macros::acpi_dsl;

use super::h8::H8Config;

/// Produce all H8-related DSDT fragments.
///
/// `lpc_scope` is the board's LPC device path (e.g. `\_SB.PCI0.LPCB`); the
/// EC device is emitted underneath it so `_PTS`/`_WAK` glue and the OS see
/// the canonical ThinkPad paths (`...\LPCB.EC__`, `...\LPCB.EC__.HKEY`).
#[must_use]
pub fn dsdt_aml(cfg: &H8Config, lpc_scope: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(root_helpers_aml(cfg));
    out.extend(ec_device_aml(cfg, lpc_scope));
    out.extend(thermal_zone_aml(cfg));
    out.extend(si_status_aml(cfg));
    if cfg.bat_charge_behaviour {
        out.extend(hkey_charge_behaviour_aml(lpc_scope));
    }
    if cfg.bat_thresholds {
        out.extend(hkey_thresholds_aml(lpc_scope));
    }
    out
}

/// Root-scope names and fallback methods referenced across the H8 surface.
fn root_helpers_aml(cfg: &H8Config) -> Vec<u8> {
    let tcrt = cfg.critical_temp_celsius;
    let tpsv = cfg.passive_temp_celsius;
    acpi_dsl! {
        Scope("\\") {
            // Critical/passive trip temperatures in degrees Celsius; zero
            // lets the thermal-zone methods fall back to safe defaults
            // (coreboot reads these from the mainboard devicetree).
            Name("TCRT", #{tcrt});
            Name("TPSV", #{tpsv});

            // Fan level: 1 = disengaged (full speed), 0 = automatic.
            Name("FLVL", 0u32);

            // Power state indicator updated by the AC device.
            Name("PWRS", 0u32);

            // Processor notification; we have no \_PR processor objects yet.
            Method("PNOT", 0, Serialized) { }

            // Passive-cooling processor package. Empty until MP processor
            // objects are declared in the DSDT.
            Method("PPKG", 0, Serialized) {
                Return(Package());
            }

            // Brightness hooks: wired to the display pipeline once native
            // graphics bring-up lands. The EC _Q14/_Q15 events call these.
            Method("BRTU", 0, NotSerialized) { }
            Method("BRTD", 0, NotSerialized) { }
        }
    }
}

/// The `EC__` device plus its sibling raw-resource devices.
fn ec_device_aml(cfg: &H8Config, lpc_scope: &str) -> Vec<u8> {
    let ec_gpe = cfg.ec_gpe as u32;
    let hkey_eisaid = cfg.hkey_eisaid;
    let hbdc: u8 = cfg.has_bluetooth as u8;
    let hwan: u8 = cfg.has_wwan as u8;
    let hklt: u8 = cfg.has_thinklight as u8;
    let hkbl: u8 = cfg.has_keyboard_backlight as u8;
    let huwb: u8 = cfg.has_uwb as u8;
    let hp = |s: &str| fstart_acpi::aml::Path::new(s);
    acpi_dsl! {
        Scope(#{lpc_scope}) {
            Device("EC__") {
                Name("_HID", EisaId("PNP0C09"));
                Name("_UID", 0u32);
                Name("_GPE", #{ec_gpe});
                Mutex("ECLK", 0u8);

                OperationRegion("ERAM", EmbeddedControl, 0x00u32, 0x100u32);
                Field("ERAM", ByteAcc, NoLock, Preserve) {
                    Offset(0x02),
                    DKR1, 1,
                    Offset(0x05),
                    HSPA, 1,
                    Offset(0x06),
                    SNDS, 8,
                    Offset(0x0C),
                    LEDS, 8,
                    Offset(0x0D),
                    , 6,
                    KBBL, 2,
                    Offset(0x0F),
                    , 7,
                    TBSW, 1,
                    Offset(0x1A),
                    DKR2, 1,
                    Offset(0x2A),
                    EVNT, 8,
                    Offset(0x2F),
                    , 6,
                    FAND, 1,
                    FANA, 1,
                    Offset(0x30),
                    , 6,
                    ALMT, 1,
                    Offset(0x31),
                    , 2,
                    UWBE, 1,
                    Offset(0x32),
                    , 2,
                    WKLD, 1,
                    Offset(0x33),
                    , 4,
                    WKFN, 1,
                    Offset(0x38),
                    B0ST, 4,
                    , 1,
                    B0CH, 1,
                    B0DI, 1,
                    B0PR, 1,
                    Offset(0x39),
                    B1ST, 4,
                    , 1,
                    B1CH, 1,
                    B1DI, 1,
                    B1PR, 1,
                    Offset(0x3A),
                    AMUT, 1,
                    , 3,
                    BTEB, 1,
                    WLEB, 1,
                    WWEB, 1,
                    Offset(0x3B),
                    , 1,
                    KBLT, 1,
                    , 2,
                    USPW, 1,
                    Offset(0x46),
                    , 2,
                    LIDS, 1,
                    Offset(0x46),
                    , 4,
                    HPAC, 1,
                    Offset(0x48),
                    HPPI, 1,
                    GSTS, 1,
                    Offset(0x4E),
                    WAKE, 16,
                    Offset(0x78),
                    TMP0, 8,
                    TMP1, 8,
                    Offset(0x81),
                    PAGE, 8,
                    Offset(0x83),
                    FNKY, 8,
                    Offset(0xFE),
                    , 4,
                    DKR3, 1,
                }

                // Battery state bits (coreboot battery.asl).
                Field("ERAM", ByteAcc, NoLock, Preserve) {
                    Offset(0xA0),
                    BARC, 16,
                    BAFC, 16,
                    Offset(0xA8),
                    BAPR, 16,
                    BAVO, 16,
                }

                Field("ERAM", ByteAcc, NoLock, Preserve) {
                    Offset(0xA0),
                    BADC, 16,
                    BADV, 16,
                    , 16,
                    , 16,
                    , 16,
                    BASN, 16,
                }

                Field("ERAM", ByteAcc, NoLock, Preserve) {
                    Offset(0xA0),
                    BATY, 32,
                }

                Field("ERAM", ByteAcc, NoLock, Preserve) {
                    Offset(0xA0),
                    BAOE, 128,
                }

                Field("ERAM", ByteAcc, NoLock, Preserve) {
                    Offset(0xA0),
                    BANA, 128,
                }

                // Called on OperationRegion driver changes.
                Method("_REG", 2, NotSerialized) {
                    If (Arg1 == 1u32) {
                        If (#{hp("HKEY.INIT")} == 0u32) {
                            Store(#{hp("BTEB")}, #{hp("HKEY.WBDC")});
                            Store(#{hp("WWEB")}, #{hp("HKEY.WWAN")});
                            Store(1u32, #{hp("HKEY.INIT")});
                        }
                    }
                }

                Method("_CRS", 0, Serialized) {
                    Name("ECMD", ResourceTemplate {
                        IO(0x0062u16, 0x0062u16, 0x01u8, 0x01u8);
                        IO(0x0066u16, 0x0066u16, 0x01u8, 0x01u8);
                    });
                    Return(ECMD);
                }

                Method("TLED", 1, NotSerialized) { LEDS = Arg0; }
                Method("LED_", 2, NotSerialized) { TLED(Arg0 | Arg1); }
                Method("_INI", 0, NotSerialized) { }
                Method("MUTE", 1, NotSerialized) { AMUT = Arg0; }
                Method("RADI", 1, NotSerialized) { WLEB = Arg0; WWEB = Arg0; BTEB = Arg0; }
                Method("USBP", 1, NotSerialized) { USPW = Arg0; }
                Method("LGHT", 1, NotSerialized) { KBLT = Arg0; }
                Method("BEEP", 1, NotSerialized) { SNDS = Arg0; }

                Method("FANE", 1, Serialized) {
                    If (Arg0) {
                        FAND = 1u32;
                        FANA = 0u32;
                    } Else {
                        FAND = 0u32;
                        FANA = 1u32;
                    }
                }

                // Sleep button pressed.
                Method("_Q13", 0, NotSerialized) { Notify(SLPB, 0x80u32); }
                Method("_Q14", 0, NotSerialized) { BRTU(); }
                Method("_Q15", 0, NotSerialized) { BRTD(); }
                Method("_Q16", 0, NotSerialized) { Notify(#{fstart_acpi::aml::Path::new("\\_SB_.PCI0.GFX0")}, 0x82u32); }

                Method("_Q26", 0, NotSerialized) {
                    Notify(AC__, 0x80u32);
                    PNOT();
                }
                Method("_Q27", 0, NotSerialized) {
                    Notify(AC__, 0x80u32);
                    EVNT = 0x50u32;
                    PNOT();
                }

                Method("_Q2A", 0, NotSerialized) { Notify(LID_, 0x80u32); }
                Method("_Q2B", 0, NotSerialized) { Notify(LID_, 0x80u32); }

                // IBM proprietary hotkeys, routed through the HKEY hub.
                Method("_Q10", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x01u32); }
                Method("_Q64", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x05u32); }
                Method("_Q65", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x06u32); }
                Method("_Q17", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x08u32); }
                Method("_Q66", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x0Au32); }
                Method("_Q6A", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x1Bu32); }
                Method("_Q1A", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x0Bu32); }
                Method("_Q1B", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x0Cu32); }
                Method("_Q62", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x0Du32); }
                Method("_Q60", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x0Eu32); }
                Method("_Q61", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x0Fu32); }
                Method("_Q1F", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x12u32); }
                Method("_Q67", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x13u32); }
                Method("_Q63", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x14u32); }
                Method("_Q19", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x18u32); }
                Method("_Q1C", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x19u32); }
                Method("_Q1D", 0, NotSerialized) { #{hp("HKEY.RHK_")}(0x1Au32); }
                Method("_Q5C", 0, NotSerialized) { #{hp("HKEY.RTAB")}(0x0Bu32); }
                Method("_Q5D", 0, NotSerialized) { #{hp("HKEY.RTAB")}(0x0Cu32); }
                Method("_Q5E", 0, NotSerialized) { #{hp("HKEY.RTAB")}(0x09u32); }
                Method("_Q5F", 0, NotSerialized) { #{hp("HKEY.RTAB")}(0x0Au32); }

                Device("BAT0") {
                    Name("_HID", EisaId("PNP0C0A"));
                    Name("_UID", 0u32);
                    Name("_PCL", Package(#{fstart_acpi::aml::Path::new("\\_SB_")}));
                    Name("BATS", Package(
                        0u32,
                        0xFFFFFFFFu32,
                        0xFFFFFFFFu32,
                        1u32,
                        10800u32,
                        0u32,
                        0u32,
                        1u32,
                        1u32,
                        "",
                        "",
                        "",
                        "",
                    ));
                    Name("BATX", Package(
                        0u32,
                        0u32,
                        0xFFFFFFFFu32,
                        0xFFFFFFFFu32,
                        1u32,
                        0xFFFFFFFFu32,
                        0u32,
                        0u32,
                        0xFFFFFFFFu32,
                        0x00017318u32,
                        0xFFFFFFFFu32,
                        0xFFFFFFFFu32,
                        0x03E8u32,
                        0x01F4u32,
                        0xFFFFFFFFu32,
                        0xFFFFFFFFu32,
                        "",
                        "",
                        "",
                        "",
                    ));
                    Name("BATI", Package(0u32, 0u32, 0u32, 0u32));

                    Method("_BIF", 0, NotSerialized) {
                        Return(BINF(BATS, BATX, 0x00u32));
                    }
                    Method("_BST", 0, NotSerialized) {
                        If (B0PR) {
                            Return(BSTA(0x00u32, BATI, B0CH, B0DI));
                        }
                        Return(Package(0u32, 0u32, 0u32, 0u32));
                    }
                    Method("_STA", 0, NotSerialized) {
                        If (B0PR) { Return(0x1Fu32); }
                        Return(0x0Fu32);
                    }
                }

                Device("BAT1") {
                    Name("_HID", EisaId("PNP0C0A"));
                    Name("_UID", 1u32);
                    Name("_PCL", Package(#{fstart_acpi::aml::Path::new("\\_SB_")}));
                    Name("BATS", Package(
                        0u32,
                        0xFFFFFFFFu32,
                        0xFFFFFFFFu32,
                        1u32,
                        10800u32,
                        0u32,
                        0u32,
                        1u32,
                        1u32,
                        "",
                        "",
                        "",
                        "",
                    ));
                    Name("BATX", Package(
                        0u32,
                        0u32,
                        0xFFFFFFFFu32,
                        0xFFFFFFFFu32,
                        1u32,
                        0xFFFFFFFFu32,
                        0u32,
                        0u32,
                        0xFFFFFFFFu32,
                        0x00017318u32,
                        0xFFFFFFFFu32,
                        0xFFFFFFFFu32,
                        0x03E8u32,
                        0x01F4u32,
                        0xFFFFFFFFu32,
                        0xFFFFFFFFu32,
                        "",
                        "",
                        "",
                        "",
                    ));
                    Name("BATI", Package(0u32, 0u32, 0u32, 0u32));

                    Method("_BIF", 0, NotSerialized) {
                        Return(BINF(BATS, BATX, 0x10u32));
                    }
                    Method("_BST", 0, NotSerialized) {
                        If (B1PR) {
                            Return(BSTA(0x10u32, BATI, B1CH, B1DI));
                        }
                        Return(Package(0u32, 0u32, 0u32, 0u32));
                    }
                    Method("_STA", 0, NotSerialized) {
                        If (B1PR) { Return(0x1Fu32); }
                        Return(0x0Fu32);
                    }
                }

                // Battery critical / attach / state-change events.
                Method("_Q24", 0, NotSerialized) { Notify(BAT0, 0x80u32); }
                Method("_Q25", 0, NotSerialized) { Notify(BAT1, 0x80u32); }
                Method("_Q4A", 0, NotSerialized) { Notify(BAT0, 0x81u32); }
                Method("_Q4B", 0, NotSerialized) { Notify(BAT0, 0x80u32); }
                Method("_Q4C", 0, NotSerialized) { Notify(BAT1, 0x81u32); }
                Method("_Q4D", 0, NotSerialized) { Notify(BAT1, 0x80u32); }

                Device("LID_") {
                    Name("_HID", "PNP0C0D");
                    Method("_LID", 0, NotSerialized) { Return(LIDS); }
                    Method("_PRW", 0, NotSerialized) { Return(Package(0x18u32, 0x03u32)); }
                    Method("_PSW", 1, NotSerialized) {
                        If (Arg0) { WKLD = 1u32; } Else { WKLD = 0u32; }
                    }
                }

                Device("AC__") {
                    Name("_HID", "ACPI0003");
                    Name("_UID", 0u32);
                    Name("_PCL", Package(#{fstart_acpi::aml::Path::new("\\_SB_")}));
                    Method("_PSR", 0, NotSerialized) {
                        Local0 = HPAC;
                        PWRS = Local0;
                        PNOT();
                        Return(Local0);
                    }
                    Method("_STA", 0, NotSerialized) { Return(0x0Fu32); }
                }

                Device("SLPB") {
                    Name("_HID", EisaId("PNP0C0E"));
                    Method("_PRW", 0, NotSerialized) { Return(Package(0x18u32, 0x03u32)); }
                    Method("_PSW", 1, NotSerialized) {
                        If (Arg0) {
                            FNKY = 6u32;
                            WKFN = 1u32;
                        } Else {
                            FNKY = 0u32;
                            WKFN = 0u32;
                        }
                    }
                }

                Device("HKEY") {
                    Name("_HID", #{fstart_acpi::aml::EISAName::new(hkey_eisaid)});
                    Name("BTN_", 0u32);
                    Name("BTAB", 0u32);
                    Name("DHKN", 0x080Cu32);
                    Name("EMSK", 0u32);
                    Name("ETAB", 0u32);
                    Name("EN__", 0u32);
                    Name("DHKC", 0u32);
                    Name("INIT", 0u32);
                    Name("HAST", 0u32);
                    Name("WBDC", 0u32);
                    Name("WWAN", 0u32);

                    Method("_STA", 0, NotSerialized) { Return(0x0Fu32); }

                    Method("MHKP", 0, NotSerialized) {
                        Local0 = BTN_;
                        If (Local0 != 0u32) {
                            BTN_ = 0u32;
                            Local0 = Local0 + 0x1000u32;
                            Return(Local0);
                        }
                        Local0 = BTAB;
                        If (Local0 != 0u32) {
                            BTAB = 0u32;
                            Local0 = Local0 + 0x5000u32;
                            Return(Local0);
                        }
                        Return(0u32);
                    }

                    Method("RHK_", 1, NotSerialized) {
                        Local0 = 1u32 << (Arg0 - 1u32);
                        If (EMSK & Local0) {
                            BTN_ = Arg0;
                            Notify(HKEY, 0x80u32);
                        }
                    }

                    Method("RTAB", 1, NotSerialized) {
                        Local0 = 1u32 << (Arg0 - 1u32);
                        If (ETAB & Local0) {
                            BTAB = Arg0;
                            Notify(HKEY, 0x80u32);
                        }
                    }

                    Method("MHKC", 1, NotSerialized) {
                        DHKC = Arg0;
                        If (Arg0) {
                            EMSK = DHKN;
                            ETAB = 0xFFFFFFFFu32;
                        } Else {
                            EMSK = 0u32;
                            ETAB = 0u32;
                        }
                        EN__ = Arg0;
                    }

                    Method("MHKM", 2, NotSerialized) {
                        If (Arg0 <= 0x20u32) {
                            Local0 = 1u32 << (Arg0 - 1u32);
                            If (Arg1 != 0u32) {
                                DHKN = DHKN | Local0;
                            } Else {
                                DHKN = DHKN & ~Local0;
                            }
                            If (EN__) {
                                EMSK = DHKN;
                            }
                        }
                    }

                    Method("MHKA", 0, NotSerialized) { Return(0x07FFFFFFu32); }
                    Method("MHKG", 0, NotSerialized) { Return(TBSW << 3u32); }
                    Method("SSMS", 1, NotSerialized) { ALMT = Arg0; }
                    Method("MHKV", 0, NotSerialized) { Return(0x0100u32); }
                    Method("WLSW", 0, NotSerialized) { Return(GSTS); }
                    Method("MMTS", 1, NotSerialized) {
                        If (Arg0) {
                            TLED(0x8Eu32);
                        } Else {
                            TLED(0x0Eu32);
                        }
                    }

                    Method("GBDC", 0, NotSerialized) {
                        HAST = 1u32;
                        If (HBDC) {
                            Local0 = 1u32;
                            If (BTEB != 0u32) { Local0 = Local0 | 2u32; }
                            Local0 = Local0 | (WBDC << 2u32);
                            Return(Local0);
                        }
                        Return(0u32);
                    }

                    Method("SBDC", 1, NotSerialized) {
                        HAST = 1u32;
                        If (HBDC) {
                            Local0 = (Arg0 & 2u32) >> 1u32;
                            BTEB = Local0;
                            Local0 = (Arg0 & 4u32) >> 2u32;
                            WBDC = Local0;
                        }
                    }

                    Method("GWAN", 0, NotSerialized) {
                        HAST = 1u32;
                        If (HWAN) {
                            Local0 = 1u32;
                            If (WWEB != 0u32) { Local0 = Local0 | 2u32; }
                            Local0 = Local0 | WWAN << 2u32;
                            Return(Local0);
                        }
                        Return(0u32);
                    }

                    Method("SWAN", 1, NotSerialized) {
                        HAST = 1u32;
                        If (HWAN) {
                            Local0 = (Arg0 & 2u32) >> 1u32;
                            WWEB = Local0;
                            WWAN = (Arg0 & 4u32) >> 2u32;
                        }
                    }

                    Method("MLCG", 1, NotSerialized) {
                        Local0 = 0x0u32;
                        If (HKBL) {
                            Local0 = Local0 | 0x200u32;
                            Local0 = Local0 | KBBL & 0x3u32;
                        }
                        If (HKLT) {
                            Local0 = Local0 | (KBLT & 0x1u32) << 4u32;
                        }
                        Return(Local0);
                    }

                    Method("MLCS", 1, NotSerialized) {
                        If (HKBL) {
                            KBBL = Arg0 & 0x3u32;
                        }
                    }

                    Method("GUWB", 0, NotSerialized) {
                        If (HUWB) {
                            Local0 = 1u32;
                            If (UWBE != 0u32) { Local0 = Local0 | 2u32; }
                            Return(Local0);
                        }
                        Return(0u32);
                    }

                    Method("SUWB", 1, NotSerialized) {
                        If (HUWB) {
                            Local0 = (Arg0 & 2u32) >> 1u32;
                            UWBE = Local0;
                        }
                    }

                    // Called from _WAK: restore radio resume states.
                    Method("WAKE", 1, NotSerialized) {
                        If (HAST) {
                            BTEB = WBDC;
                            WWEB = WWAN;
                        }
                    }
                }

                // Battery information helpers shared by BAT0/BAT1.
                Method("BPAG", 1, NotSerialized) { PAGE = Arg0; }
                Method("BSTA", 4, NotSerialized) {
                    Acquire(ECLK, 0xffffu16);
                    Local0 = 0u32;
                    BPAG(Arg0 | 1u32);
                    Local1 = BAMA;
                    Local1 = Local1 >> 0x0Fu32;
                    BPAG(Arg0);
                    Local2 = BAPR;
                    If (Arg2) {
                        Local0 = Local0 | 2u32;
                    } Else {
                        If (Arg3) {
                            Local0 = Local0 | 1u32;
                            Local2 = 0x10000u32 - Local2;
                        } Else {
                            Local2 = 0u32;
                        }
                    }
                    If (Local2 >= 0x8000u32) {
                        Local2 = 0u32;
                    }
                    Arg1[0] = Local0;
                    If (Local1 != 0u32) {
                        Arg1[2] = BARC * 10u32;
                        Local2 = Local2 * BAVO;
                        Local2 /= 1000u32;
                        Arg1[1] = Local2;
                    } Else {
                        Arg1[2] = BARC;
                        Arg1[1] = Local2;
                    }
                    Arg1[3] = BAVO;
                    Release(ECLK);
                    Return(Arg1);
                }

                Method("BINF", 3, Serialized) {
                    BINX(Arg1, Arg2);
                    Arg0[0] = DeRefOf(Index(Arg1, 0x01u32));
                    Arg0[1] = DeRefOf(Index(Arg1, 0x02u32));
                    Arg0[2] = DeRefOf(Index(Arg1, 0x03u32));
                    Arg0[3] = DeRefOf(Index(Arg1, 0x04u32));
                    Local0 = DeRefOf(Index(Arg1, 0x05u32));
                    If (Local0 != 0xFFFFFFFFu32) {
                        Arg0[4] = Local0;
                    }
                    Arg0[5] = DeRefOf(Index(Arg1, 0x06u32));
                    Arg0[6] = DeRefOf(Index(Arg1, 0x07u32));
                    Local0 = DeRefOf(Index(Arg1, 0x0Eu32));
                    If (Local0 != 0xFFFFFFFFu32) {
                        Arg0[7] = Local0;
                    }
                    Local0 = DeRefOf(Index(Arg1, 0x0Fu32));
                    If (Local0 != 0xFFFFFFFFu32) {
                        Arg0[8] = Local0;
                    }
                    Arg0[9] = DeRefOf(Index(Arg1, 0x10u32));
                    Arg0[10] = DeRefOf(Index(Arg1, 0x11u32));
                    Arg0[11] = DeRefOf(Index(Arg1, 0x12u32));
                    Arg0[12] = DeRefOf(Index(Arg1, 0x13u32));
                    Return(Arg0);
                }

                Method("BINX", 2, Serialized) {
                    Acquire(ECLK, 0xffffu16);
                    BPAG(Arg1 | 1u32);
                    Local0 = BAMA;
                    Local0 = Local0 >> 0x0Fu32;
                    BPAG(Arg1);
                    Local2 = BAFC;
                    BPAG(Arg1 | 2u32);
                    Local1 = BADC;
                    Local3 = BADV;
                    Local4 = 0u32;
                    If (Local0 != 0u32) {
                        Local1 = Local1 * 10u32;
                        Local2 = Local2 * 10u32;
                        Local4 = 200u32;
                    } ElseIf (Local3 != 0u32) {
                        Local4 = 200000u32;
                        Local4 /= Local3;
                    }
                    Arg0[1] = Local0 ^ 1u32;
                    Arg0[2] = Local1;
                    Arg0[3] = Local2;
                    Arg0[5] = Local3;
                    Local5 = Local2;
                    Local5 /= 20u32;
                    Arg0[6] = Local5;
                    Arg0[7] = Local4;
                    Local0 = BASN;
                    Name("SERN", Buffer(#{fstart_acpi::aml::BufferData::new(alloc::vec![
                        0x20u8, 0x20u8, 0x20u8, 0x20u8, 0x20u8, 0x00u8,
                    ])}));
                    Local1 = 4u32;
                    While (Local0 != 0u32) {
                        Local2 = Local0 % 10u32;
                        Local0 /= 10u32;
                        SERN[Local1] = Local2 + 48u32;
                        Local1--;
                    }
                    Arg0[17] = SERN;
                    BPAG(Arg1 | 4u32);
                    Name("TYPE", Buffer(#{fstart_acpi::aml::BufferData::new(alloc::vec![
                        0u8, 0u8, 0u8, 0u8, 0u8,
                    ])}));
                    TYPE = BATY;
                    Arg0[18] = TYPE;
                    BPAG(Arg1 | 5u32);
                    Arg0[19] = BAOE;
                    BPAG(Arg1 | 6u32);
                    Arg0[16] = BANA;
                    Release(ECLK);
                    Return(Arg0);
                }

                // Radio presence flags consumed by HKEY (coreboot ssdt.c).
                Name("HBDC", #{hbdc});
                Name("HWAN", #{hwan});
                Name("HKLT", #{hklt});
                Name("HKBL", #{hkbl});
                Name("HUWB", #{huwb});
            }

            // EC SMM interface.
            Device("ECMM") {
                Name("_HID", EisaId("PNP0C02"));
                Name("_UID", 10u32);
                Name("_CRS", ResourceTemplate {
                    IO(0x1600u16, 0x1600u16, 0x01u8, 0x01u8);
                    IO(0x1604u16, 0x1604u16, 0x01u8, 0x01u8);
                });
            }

            // EC gravity sensor interface.
            Device("ECGS") {
                Name("_HID", EisaId("PNP0C02"));
                Name("_UID", 11u32);
                Name("_CRS", ResourceTemplate {
                    IO(0x1602u16, 0x1602u16, 0x01u8, 0x01u8);
                    IO(0x1606u16, 0x1606u16, 0x01u8, 0x01u8);
                });
            }

            // Battery two-wire interface.
            Device("TWRI") {
                Name("_HID", EisaId("PNP0C02"));
                Name("_UID", 12u32);
                Name("_CRS", ResourceTemplate {
                    IO(0x1610u16, 0x1610u16, 0x10u8, 0x10u8);
                });
            }

            // PMH7 resource window.
            Device("PMH7") {
                Name("_HID", EisaId("PNP0C02"));
                Name("_UID", 13u32);
                Name("_CRS", ResourceTemplate {
                    IO(0x15E0u16, 0x15E0u16, 0x10u8, 0x10u8);
                });
            }
        }
    }
}

/// The `\_TZ` thermal zones (coreboot `thermal.asl`). `X61` selects both
/// zones via `H8_HAS_2ND_THERMAL_ZONE`.
fn thermal_zone_aml(cfg: &H8Config) -> Vec<u8> {
    let hp = |s: &str| fstart_acpi::aml::Path::new(s);
    let tzp = 100u32;
    let tc1 = 0x02u32;
    let tc2 = 0x05u32;

    acpi_dsl! {
        Scope("\\_TZ_") {
            Method("CTOK", 1, NotSerialized) {
                Local0 = Arg0 * 10u32;
                Local0 = Local0 + 2732u32;
                If (Local0 <= 2732u32) { Return(3000u32); }
                If (Local0 > 4012u32) { Return(3000u32); }
                Return(Local0);
            }
            ThermalZone("THM0") {
                Name("_TZP", #{tzp});
                Name("_TSP", #{tzp});
                Name("_TC1", #{tc1});
                Name("_TC2", #{tc2});
                Method("_PSL", 0, Serialized) { Return(PPKG()); }
                Method("GCRT", 0, NotSerialized) {
                    Local0 = TCRT;
                    If (Local0 > 0u32) { Return(Local0); }
                    Return(127u32);
                }
                Method("GPSV", 0, NotSerialized) {
                    Local0 = TPSV;
                    If (Local0 > 0u32) { Return(Local0); }
                    Return(95u32);
                }
                Method("_CRT", 0, NotSerialized) { Return(CTOK(GCRT())); }
                Method("_PSV", 0, NotSerialized) { Return(CTOK(GPSV())); }
                Method("_TMP", 0, NotSerialized) {
                    Local0 = TMP0;
                    If (Local0 == 128u32) { Return(CTOK(40u32)); }
                    Return(CTOK(Local0));
                }
                Method("_AC0", 0, NotSerialized) {
                    Local0 = GPSV();
                    Local0 = Local0 - 10u32;
                    If (FLVL != 0u32) { Local0 = Local0 - 5u32; }
                    Return(CTOK(Local0));
                }
                Name("_AL0", Package(#{hp("FAN_")}));
                PowerResource("FPWR", 0u8, 0u16) {
                    Method("_STA", 0, NotSerialized) { Return(FLVL); }
                    Method("_ON", 0, NotSerialized) {
                        FANE(1u32);
                        FLVL = 1u32;
                        Notify(#{hp("\\_TZ_.THM0")}, 0x82u32);
                    }
                    Method("_OFF", 0, NotSerialized) {
                        FANE(0u32);
                        FLVL = 0u32;
                        Notify(#{hp("\\_TZ_.THM0")}, 0x82u32);
                    }
                }
                Device("FAN_") {
                    Name("_HID", EisaId("PNP0C0B"));
                    Name("_PR0", Package(#{hp("FPWR")}));
                }
            }

            ThermalZone("THM1") {
                Name("_TZP", #{tzp});
                Name("_TSP", #{tzp});
                Name("_TC1", #{tc1});
                Name("_TC2", #{tc2});
                Method("_PSL", 0, Serialized) { Return(PPKG()); }
                Method("_CRT", 0, NotSerialized) { Return(CTOK(99u32)); }
                Method("_PSV", 0, NotSerialized) { Return(CTOK(94u32)); }
                Method("_TMP", 0, NotSerialized) {
                    Local0 = TMP1;
                    If (Local0 == 128u32) { Return(CTOK(40u32)); }
                    Return(CTOK(Local0));
                }
            }
        }
    }
}

/// `\_SI._SST` system status indicator (coreboot `systemstatus.asl`).
fn si_status_aml(_cfg: &H8Config) -> Vec<u8> {
    let tled = |v: u32| fstart_acpi::aml::Path::new("\\_SB_.PCI0.LPCB.EC__.TLED");
    let _ = tled;
    acpi_dsl! {
        Scope("\\_SI") {
            Method("_SST", 1, NotSerialized) {
                If (Arg0 == 0u32) {
                    #{tled(0x00u32)}(0x00u32);
                    #{tled(0x07u32)}(0x07u32);
                }
                If (Arg0 == 1u32) {
                    #{tled(0x80u32)}(0x80u32);
                    #{tled(0x07u32)}(0x07u32);
                }
                If (Arg0 == 2u32) {
                    #{tled(0x80u32)}(0x80u32);
                    #{tled(0xC7u32)}(0xC7u32);
                }
                If (Arg0 == 3u32) {
                    #{tled(0xA0u32)}(0xA0u32);
                    #{tled(0x87u32)}(0x87u32);
                }
            }
        }
    }
}

/// HKEY charge-behaviour extension (coreboot
/// `thinkpad_bat_charge_behaviour.asl`), re-opening the HKEY scope.
fn hkey_charge_behaviour_aml(lpc_scope: &str) -> Vec<u8> {
    let ec = alloc::format!("{lpc_scope}.EC__");
    acpi_dsl! {
        Scope(#{ec.as_str()}) {
            Field("ERAM", ByteAcc, NoLock, Preserve) {
                Offset(0x0F),
                B0IC, 1,
                B1IC, 1,
                B0FD, 1,
                B1FD, 1,
                Offset(0x21),
                BAET, 16,
                Offset(0xB4),
                B0CB, 8,
                Offset(0xB5),
                B1CB, 8,
            }

            // BDSS: set force-discharge state.
            Method("BDSS", 1, NotSerialized) {
                Local0 = Arg0 & 0x1u32;
                Local1 = (Arg0 >> 8u32) & 0x3u32;
                Local2 = (Arg0 >> 1u32) & 0x1u32;
                Local3 = 0x0u32;
                If (Local0 != 0u32) {
                    If (HPAC == 0u32) { Return(0x0u32); }
                    Local3 = 0x6u32;
                } Else {
                    Local3 = 0x3u32;
                }
                If (Local2 == 0u32) {
                    If (Local1 == 1u32) {
                        B0CB = Local3;
                        Return(0x0u32);
                    }
                    If (Local1 == 2u32) {
                        B1CB = Local3;
                        Return(0x0u32);
                    }
                }
                Return(1u32 << 31u32);
            }

            // BDSG: get force-discharge state.
            Method("BDSG", 1, NotSerialized) {
                Local0 = 0x0u32;
                Local1 = 0x0u32;
                Local2 = 0x0u32;
                If (Arg0 == 1u32) {
                    Local0 = B0PR;
                    Local1 = B0FD;
                } ElseIf (Arg0 == 2u32) {
                    Local0 = B1PR;
                    Local1 = B1FD;
                } Else {
                    Return(1u32 << 31u32);
                }
                If (Local0 != 0u32) {
                    Local2 = Local2 | 0x100u32;
                    If (Local1 != 0u32) {
                        Local2 = Local2 | 0x1u32;
                    }
                }
                Return(Local2);
            }

            // BICS: set inhibit-charge state.
            Method("BICS", 1, NotSerialized) {
                Local0 = Arg0 & 0x1u32;
                Local1 = (Arg0 >> 4u32) & 0x3u32;
                Local2 = (Arg0 >> 8u32) & 0xffffu32;
                Local3 = 0x0u32;
                Local4 = 0x0u32;
                If (Local2 != 0xffffu32) {
                    Local4 = Local2 >> 8u32;
                }
                If (Local4 == 0x0u32) {
                    If (Local0 != 0u32) {
                        Local3 = 0x2u32;
                    } Else {
                        Local3 = 0x1u32;
                    }
                    If (Local1 == 1u32) {
                        BAET = Local2;
                        B0CB = Local3;
                        Return(0x0u32);
                    }
                    If (Local1 == 2u32) {
                        BAET = Local2;
                        B1CB = Local3;
                        Return(0x0u32);
                    }
                }
                Return(1u32 << 31u32);
            }

            // BICG: get inhibit-charge state.
            Method("BICG", 1, NotSerialized) {
                Local0 = 0x0u32;
                Local1 = 0x0u32;
                Local2 = 0x0u32;
                If (Arg0 == 1u32) {
                    Local0 = B0PR;
                    Local1 = B0IC;
                } ElseIf (Arg0 == 2u32) {
                    Local0 = B1PR;
                    Local1 = B1IC;
                } Else {
                    Return(1u32 << 31u32);
                }
                If (Local0 != 0u32) {
                    Local2 = Local2 | 0x20u32;
                    If (Local1 != 0u32) {
                        Local2 = Local2 | 0x1u32;
                    }
                }
                Return(Local2);
            }

            // RBCB: reset charge behaviour on AC detach (called from _Q27).
            Method("RBCB", 1, NotSerialized) {
                If (Arg0 == 0u32) {
                    BDSS(0x100u32);
                    BDSS(0x200u32);
                }
            }
        }
    }
}

/// HKEY battery-threshold extension (coreboot
/// `thinkpad_bat_thresholds_b0.asl`), re-opening EC and battery scopes.
fn hkey_thresholds_aml(lpc_scope: &str) -> Vec<u8> {
    let ec = alloc::format!("{lpc_scope}.EC__");
    let bat0 = alloc::format!("{lpc_scope}.EC__.BAT0");
    let bat1 = alloc::format!("{lpc_scope}.EC__.BAT1");
    acpi_dsl! {
        Scope(#{ec.as_str()}) {
            Field("ERAM", ByteAcc, NoLock, Preserve) {
                Offset(0xB0),
                TSL0, 8,
                Offset(0xB1),
                TSH0, 8,
                Offset(0xB2),
                TSL1, 8,
                Offset(0xB3),
                TSH1, 8,
            }
        }
        Scope(#{bat0.as_str()}) {
            Method("SETT", 2, NotSerialized) {
                If (Arg1 <= 100u32) {
                    If (Arg0 == 0u32) {
                        TSL0 = Arg1;
                    }
                    If (Arg0 == 1u32) {
                        TSH0 = Arg1;
                    }
                }
            }
            Method("GETT", 1, NotSerialized) {
                If (Arg0 == 0u32) { Return(TSL0); }
                If (Arg0 == 1u32) { Return(TSH0); }
                Return(0u32);
            }
        }
        Scope(#{bat1.as_str()}) {
            Method("SETT", 2, NotSerialized) {
                If (Arg1 <= 100u32) {
                    If (Arg0 == 0u32) {
                        TSL1 = Arg1;
                    }
                    If (Arg0 == 1u32) {
                        TSH1 = Arg1;
                    }
                }
            }
            Method("GETT", 1, NotSerialized) {
                If (Arg0 == 0u32) { Return(TSL1); }
                If (Arg0 == 1u32) { Return(TSH1 & ~0x80u32); }
                Return(0u32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

    #[test]
    fn dsdt_assembles_and_contains_key_objects() {
        let cfg = H8Config::x61();
        let lpc = "\\_SB_.PCI0.LPCB";
        macro_rules! tryfrag {
            ($name:literal, $expr:expr) => {{
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $expr));
                std::eprintln!("frag {}: {}", $name, if r.is_ok() { "ok" } else { "PANIC" });
            }};
        }
        tryfrag!("root", root_helpers_aml(&cfg));
        tryfrag!("ec", ec_device_aml(&cfg, lpc));
        tryfrag!("tz", thermal_zone_aml(&cfg));
        tryfrag!("si", si_status_aml(&cfg));
        tryfrag!("charge", hkey_charge_behaviour_aml(lpc));
        tryfrag!("thresholds", hkey_thresholds_aml(lpc));
        let aml = dsdt_aml(&cfg, lpc);
        assert!(aml.len() > 2000);
        for name in [
            "EC__", "HKEY", "BAT0", "BAT1", "THM0", "THM1", "FPWR", "ECMM", "PMH7",
        ] {
            assert!(
                aml.windows(4).any(|w| w == name.as_bytes()),
                "missing {name}"
            );
        }
        if env::var_os("H8_DUMP_AML").is_some() {
            // Write the full surface wrapped in a minimal DSDT table so
            // `iasl -d` can validate the encoding end-to-end.
            let aml = dsdt_aml(&cfg, "\\_SB_.PCI0.LPCB");
            let mut table = alloc::vec::Vec::new();
            table.extend_from_slice(b"DSDT");
            let len = 36 + aml.len();
            table.extend((len as u32).to_le_bytes());
            table.push(2); // revision
            table.push(0); // checksum placeholder
            table.extend_from_slice(b"FSTART");
            table.extend_from_slice(b"X61H8___");
            table.extend(0u32.to_le_bytes());
            table.extend_from_slice(b"FSTB");
            table.extend(1u32.to_le_bytes());
            assert_eq!(table.len(), 36);
            let cs = (256 - (table.iter().map(|b| *b as u32).sum::<u32>() % 256)) as u8;
            table[9] = cs;
            table.extend_from_slice(&aml);
            fs::write("/tmp/h8_full_table.aml", &table).unwrap();
        }
    }
}
