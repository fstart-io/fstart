//! Lenovo ThinkPad X200 mainboard glue.
//!
//! This crate intentionally contains the board-specific parts that do not
//! belong in the reusable GM45 northbridge or ICH9 southbridge drivers.  The
//! important board hooks are the X200 UltraBase dock GPIO policy and the
//! post-DRAM SMBus mux switch from DIMM SPD to the board EEPROM. Coreboot's
//! X200 Kconfig declares `NO_UART_ON_SUPERIO`, so this crate deliberately does
//! not try to expose a pre-console LPC serial port.

#![allow(clippy::result_unit_err)]
#![no_std]

pub mod smm;

use fstart_services::device::{Device, DeviceError};
use fstart_services::{FinalizeInit, Mainboard, PostDramInit, PreConsoleInit, ServiceError};
use serde::{Deserialize, Serialize};

/// Lenovo ThinkPad X200 mainboard configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LenovoX200MainboardConfig {
    /// Run the dock GPIO policy before console init.
    #[serde(default = "default_true")]
    pub dock_early_console: bool,
    /// ACPI contributor name marker for codegen.
    #[serde(default)]
    pub acpi_name: Option<heapless::String<8>>,
}

impl Default for LenovoX200MainboardConfig {
    fn default() -> Self {
        Self {
            dock_early_console: true,
            acpi_name: None,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Lenovo ThinkPad X200 mainboard hook driver.
pub struct LenovoX200Mainboard {
    config: &'static LenovoX200MainboardConfig,
}

impl Device for LenovoX200Mainboard {
    const NAME: &'static str = "lenovo-x200-mainboard";
    const COMPATIBLE: &'static [&'static str] = &["lenovo,thinkpad-x200"];
    type Config = LenovoX200MainboardConfig;

    fn new(config: &'static Self::Config) -> Result<Self, DeviceError> {
        Ok(Self { config })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }
}

impl PreConsoleInit for LenovoX200Mainboard {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        // X200 has no LPC SuperIO UART. The southbridge-aware Mainboard hook
        // below performs the GPIO side of coreboot's dock_connect() once ICH9
        // GPIO decode is open; failures remain non-fatal before console.
        Ok(())
    }
}

impl PostDramInit for LenovoX200Mainboard {
    fn post_dram_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl FinalizeInit for LenovoX200Mainboard {
    fn finalize_init(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }
}

impl Mainboard for LenovoX200Mainboard {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        PreConsoleInit::pre_console_init(self)
    }

    fn pre_console_init_with_southbridge(
        &mut self,
        southbridge: &mut dyn fstart_services::Southbridge,
    ) -> Result<(), ServiceError> {
        if self.config.dock_early_console && dock::dock_present(southbridge) {
            let _ = dock::dock_connect_with_southbridge(southbridge);
            dock::early_superio_config();
        }
        Ok(())
    }

    fn ramstage_init(&mut self) -> Result<(), ServiceError> {
        PostDramInit::post_dram_init(self)
    }

    fn ramstage_init_with_southbridge(
        &mut self,
        southbridge: &mut dyn fstart_services::Southbridge,
    ) -> Result<(), ServiceError> {
        // H8 init writes CONFIG2 from devicetree defaults; reconnect a present
        // dock afterward just like coreboot's X200 h8_mb_init() hook.
        if dock::dock_present(southbridge) {
            let _ = dock::dock_connect_with_southbridge(southbridge);
        }
        dock::post_raminit_setup(southbridge);
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), ServiceError> {
        FinalizeInit::finalize_init(self)
    }
}

/// X200 dock and SMBus-mux helpers ported from coreboot `mainboard/lenovo/x200`.
pub mod dock {
    /// Return whether an X200 UltraBase dock is attached.
    ///
    /// Coreboot `variants/x200/dock.c::dock_present()` samples GPIO2, GPIO3,
    /// and GPIO4 as a three-bit dock ID and treats `0b111` as undocked.  The
    /// GPIO controller returns logical levels, so this preserves the coreboot
    /// test while asking the reusable southbridge driver for the pin state.
    pub fn dock_present(southbridge: &dyn fstart_services::Southbridge) -> bool {
        let mut id = 0u8;
        for (bit, pin) in [2u8, 3, 4].iter().copied().enumerate() {
            if southbridge.gpio_get(u32::from(pin)).unwrap_or(true) {
                id |= 1 << bit;
            }
        }
        id != 7
    }

    /// Connect the dock-side buses.
    ///
    /// Coreboot's X200 variant sets EC register 0x02 bit 0 (`ec_set_bit(0x02, 0)`).
    /// The GPIO28 side is performed by the southbridge-aware hook below.
    pub fn dock_connect() -> Result<(), fstart_services::ServiceError> {
        fstart_driver_lenovo_h8::LenovoH8::standard().dock_connect(true)
    }

    /// Southbridge-aware dock connect helper used from normal fstart stages.
    pub fn dock_connect_with_southbridge(
        southbridge: &dyn fstart_services::Southbridge,
    ) -> Result<(), fstart_services::ServiceError> {
        fstart_driver_lenovo_h8::LenovoH8::standard().dock_connect(true)?;
        southbridge.gpio_set(28, true)
    }

    /// Disconnect the dock-side buses.
    ///
    /// Mirrors coreboot's `dock_disconnect()` EC side.
    pub fn dock_disconnect() {
        let _ = fstart_driver_lenovo_h8::LenovoH8::standard().dock_connect(false);
    }

    /// Southbridge-aware dock disconnect helper.
    pub fn dock_disconnect_with_southbridge(
        southbridge: &dyn fstart_services::Southbridge,
    ) -> Result<(), fstart_services::ServiceError> {
        fstart_driver_lenovo_h8::LenovoH8::standard().dock_connect(false)?;
        southbridge.gpio_set(28, false)
    }

    /// X200 has no LPC SuperIO UART (`NO_UART_ON_SUPERIO` in coreboot Kconfig),
    /// so there is no pre-console dock serial setup to perform.
    pub fn early_superio_config() {}

    /// Post-RAM board hook.
    ///
    /// Coreboot `mainboard/lenovo/x200/romstage.c::mb_post_raminit_setup()`
    /// drives GPIO42 low after DRAM init so the SMBus mux switches away from
    /// DIMM SPD and toward the board EEPROM/AT24RF08C devices.
    pub fn post_raminit_setup(southbridge: &dyn fstart_services::Southbridge) {
        let _ = southbridge.gpio_set(42, false);
    }
}

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;
    use fstart_acpi_macros::acpi_dsl;

    use super::*;

    impl AcpiDevice for LenovoX200Mainboard {
        type Config = LenovoX200MainboardConfig;

        fn dsdt_aml(&self, _config: &Self::Config) -> Vec<u8> {
            let p = |s: &str| fstart_acpi::aml::Path::new(s);
            acpi_dsl! {
                Scope("\\") {
                    Name("SMIF", 0u32);
                    OperationRegion("IOT_", SystemIO, 0x0800u32, 0x10u32);
                    Field("IOT_", ByteAcc, NoLock, Preserve) {
                        Offset(0x08),
                        TRP0, 8,
                    }
                    Method("TRAP", 1, Serialized) {
                        SMIF = Arg0;
                        TRP0 = 0u32;
                        Return(SMIF);
                    }

                    Method("_PTS", 1, NotSerialized) {
                        #{p("\\_SB_.PCI0.LPCB.EC__.MUTE")}(1u32);
                        #{p("\\_SB_.PCI0.LPCB.EC__.USBP")}(0u32);
                        #{p("\\_SB_.PCI0.LPCB.EC__.RADI")}(0u32);
                        #{p("\\_SB_.PCI0.LPCB.EC__.HKEY.MHKC")}(0u32);
                    }
                    Method("_WAK", 1, NotSerialized) {
                        #{p("\\_SB_.PCI0.LPCB.EC__.HKEY.MHKC")}(1u32);
                        #{p("\\_SB_.PCI0.LPCB.EC__.HKEY.WAKE")}(Arg0);
                        Return(Package(0u32, 0u32));
                    }
                    Method("GPDK", 1, Serialized) { GP28 = Arg0; }
                    Method("GDID", 1, NotSerialized) {
                        Local0 = GP02 | (GP03 << 1u32) | (GP04 << 2u32);
                        If (Local0 == 0u32) { Local0 = 3u32; }
                        Return(Local0);
                    }
                }

                Scope("\\_SB_.PCI0.LPCB") {
                        Device("EC__") {
                            Name("_HID", EisaId("PNP0C09"));
                            Name("_UID", 0u32);
                            Name("_GPE", 0x18u32);
                            Name("_CRS", ResourceTemplate {
                                IO(0x0062u16, 0x0062u16, 0x01u8, 0x01u8);
                                IO(0x0066u16, 0x0066u16, 0x01u8, 0x01u8);
                            });
                            OperationRegion("ECOR", EmbeddedControl, 0x00u32, 0x100u32);
                            Field("ECOR", ByteAcc, Lock, Preserve) {
                                Offset(0x02),
                                DKR1, 1,
                                Offset(0x0F),
                                , 7,
                                TBSW, 1,
                                Offset(0x2F),
                                , 6,
                                FAND, 1,
                                FANA, 1,
                                Offset(0x30),
                                , 6,
                                ALMT, 1,
                                Offset(0x38),
                                B0ST, 4,
                                , 1,
                                B0CH, 1,
                                B0DI, 1,
                                B0PR, 1,
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
                                Offset(0xA0),
                                BARC, 16,
                                BAFC, 16,
                                Offset(0xA8),
                                BAPR, 16,
                                BAVO, 16,
                            }
                            Method("MUTE", 1, NotSerialized) { AMUT = Arg0; }
                            Method("RADI", 1, NotSerialized) { WLEB = Arg0; WWEB = Arg0; BTEB = Arg0; }
                            Method("USBP", 1, NotSerialized) { USPW = Arg0; }
                            Method("LGHT", 1, NotSerialized) { KBLT = Arg0; }
                            Method("DKST", 1, Serialized) { DKR1 = Arg0; }
                            Method("FANE", 1, NotSerialized) {
                                If (Arg0) {
                                    FAND = 1u32;
                                    FANA = 0u32;
                                } Else {
                                    FAND = 0u32;
                                    FANA = 1u32;
                                }
                            }

                            Device("AC__") {
                                Name("_HID", "ACPI0003");
                                Name("_UID", 0u32);
                                Name("_PCL", Package(#{p("\\_SB_")}));
                                Method("_PSR", 0, NotSerialized) { Return(HPAC); }
                                Method("_STA", 0, NotSerialized) { Return(0x0Fu32); }
                            }
                            Device("LID_") { Name("_HID", EisaId("PNP0C0D")); Method("_LID", 0, NotSerialized) { Return(1u32); } }
                            Device("SLPB") { Name("_HID", EisaId("PNP0C0E")); }
                            Device("HKEY") {
                                Name("_HID", EisaId("IBM0068"));
                                Name("BTN_", 0u32);
                                Name("BTAB", 0u32);
                                Name("DHKN", 0x080Cu32);
                                Name("EMSK", 0u32);
                                Name("ETAB", 0u32);
                                Name("EN__", 0u32);
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
                                    BTN_ = Arg0;
                                    Notify(HKEY, 0x80u32);
                                }
                                Method("RTAB", 1, NotSerialized) {
                                    BTAB = Arg0;
                                    Notify(HKEY, 0x80u32);
                                }
                                Method("MHKC", 1, NotSerialized) {
                                    If (Arg0) {
                                        EMSK = DHKN;
                                        ETAB = 0xFFFFFFFFu32;
                                    } Else {
                                        EMSK = 0u32;
                                        ETAB = 0u32;
                                    }
                                    EN__ = Arg0;
                                }
                                Method("MHKV", 0, NotSerialized) { Return(0x0100u32); }
                                Method("WLSW", 0, NotSerialized) { Return(GSTS); }
                                Method("MHKG", 0, NotSerialized) { Return(TBSW << 3u32); }
                                Method("WAKE", 1, NotSerialized) { Return(0u32); }
                            }
                            Device("BAT0") {
                                Name("_HID", EisaId("PNP0C0A"));
                                Name("_UID", 0u32);
                                Name("_PCL", Package(#{p("\\_SB_")}));
                                Method("_BIF", 0, NotSerialized) { Return(Package(0u32, 0xFFFFFFFFu32, 0xFFFFFFFFu32, 1u32, 10800u32, 0u32, 200u32, 1u32, 1u32, "", "", "", "")); }
                                Method("_BST", 0, NotSerialized) {
                                    If (B0PR) {
                                        If (B0CH) { Return(Package(2u32, 0u32, #{p("BARC")}, #{p("BAVO")})); }
                                        If (B0DI) { Return(Package(1u32, 0u32, #{p("BARC")}, #{p("BAVO")})); }
                                    }
                                    Return(Package(0u32, 0u32, 0u32, 0u32));
                                }
                                Method("_STA", 0, NotSerialized) { If (B0PR) { Return(0x1Fu32); } Else { Return(0x0Fu32); } }
                            }
                            Device("BAT1") {
                                Name("_HID", EisaId("PNP0C0A"));
                                Name("_UID", 1u32);
                                Name("_PCL", Package(#{p("\\_SB_")}));
                                Method("_BIF", 0, NotSerialized) { Return(Package(0u32, 0xFFFFFFFFu32, 0xFFFFFFFFu32, 1u32, 10800u32, 0u32, 200u32, 1u32, 1u32, "", "", "", "")); }
                                Method("_BST", 0, NotSerialized) {
                                    If (B1PR) {
                                        If (B1CH) { Return(Package(2u32, 0u32, #{p("BARC")}, #{p("BAVO")})); }
                                        If (B1DI) { Return(Package(1u32, 0u32, #{p("BARC")}, #{p("BAVO")})); }
                                    }
                                    Return(Package(0u32, 0u32, 0u32, 0u32));
                                }
                                Method("_STA", 0, NotSerialized) { If (B1PR) { Return(0x1Fu32); } Else { Return(0x0Fu32); } }
                            }
                            Method("_Q13", 0, NotSerialized) { Notify(SLPB, 0x80u32); }
                            Method("_Q26", 0, NotSerialized) { Notify(AC__, 0x80u32); }
                            Method("_Q27", 0, NotSerialized) { Notify(AC__, 0x80u32); }
                            Method("_Q2A", 0, NotSerialized) { Notify(LID_, 0x80u32); }
                            Method("_Q2B", 0, NotSerialized) { Notify(LID_, 0x80u32); }
                            Method("_Q24", 0, NotSerialized) { Notify(BAT0, 0x80u32); }
                            Method("_Q25", 0, NotSerialized) { Notify(BAT1, 0x80u32); }
                            Method("_Q4A", 0, NotSerialized) { Notify(BAT0, 0x81u32); }
                            Method("_Q4B", 0, NotSerialized) { Notify(BAT0, 0x80u32); }
                            Method("_Q4C", 0, NotSerialized) { Notify(BAT1, 0x81u32); }
                            Method("_Q4D", 0, NotSerialized) { Notify(BAT1, 0x80u32); }
                            Method("_Q18", 0, NotSerialized) { Notify(#{p("\\_SB_.DOCK")}, 3u32); }
                            Method("_Q37", 0, NotSerialized) { Notify(#{p("\\_SB_.DOCK")}, 0u32); }
                            Method("_Q45", 0, NotSerialized) { Notify(#{p("\\_SB_.DOCK")}, 3u32); }
                            Method("GGID", 0, NotSerialized) {
                                Local0 = #{p("\\GP02")} | (#{p("\\GP03")} << 1u32) | (#{p("\\GP04")} << 2u32);
                                If (Local0 == 0u32) { Local0 = 3u32; }
                                Return(Local0);
                            }
                            Method("_Q50", 0, NotSerialized) {
                                Local0 = #{fstart_acpi::aml::MethodCall::new(p("GGID"), alloc::vec![])};
                                If (Local0 != 7u32) { Notify(#{p("\\_SB_.DOCK")}, 3u32); }
                            }
                            Method("_Q58", 0, NotSerialized) { Notify(#{p("\\_SB_.DOCK")}, 0u32); }
                            Method("_Q5A", 0, NotSerialized) {
                                Local0 = #{fstart_acpi::aml::MethodCall::new(p("GGID"), alloc::vec![])};
                                If (Local0 == 7u32) { Notify(#{p("\\_SB_.DOCK")}, 3u32); }
                                If (Local0 == 3u32) {
                                    Sleep(0x64u32);
                                    If (DKR1 == 1u32) { Notify(#{p("\\_SB_.DOCK")}, 0u32); }
                                }
                            }
                        }

                        Device("ECMM") {
                            Name("_HID", EisaId("PNP0C02"));
                            Name("_UID", 10u32);
                            Name("_CRS", ResourceTemplate {
                                IO(0x1600u16, 0x1600u16, 0x01u8, 0x01u8);
                                IO(0x1604u16, 0x1604u16, 0x01u8, 0x01u8);
                            });
                        }
                        Device("ECGS") {
                            Name("_HID", EisaId("PNP0C02"));
                            Name("_UID", 11u32);
                            Name("_CRS", ResourceTemplate {
                                IO(0x1602u16, 0x1602u16, 0x01u8, 0x01u8);
                                IO(0x1606u16, 0x1606u16, 0x01u8, 0x01u8);
                            });
                        }
                        Device("TWRI") {
                            Name("_HID", EisaId("PNP0C02"));
                            Name("_UID", 12u32);
                            Name("_CRS", ResourceTemplate { IO(0x1610u16, 0x1610u16, 0x01u8, 0x10u8); });
                        }
                        Device("PMH7") {
                            Name("_HID", EisaId("PNP0C02"));
                            Name("_UID", 13u32);
                            Name("_CRS", ResourceTemplate { IO(0x15E0u16, 0x15E0u16, 0x01u8, 0x10u8); });
                        }
                }

                Scope("\\_SB_") {
                    OperationRegion("DLPC", SystemIO, 0x164Cu32, 0x01u32);
                    Field("DLPC", ByteAcc, NoLock, Preserve) {
                        , 3,
                        DSTA, 1,
                    }
                    Device("DOCK") {
                        Name("_HID", "ACPI0003");
                        Name("_UID", 0u32);
                        Name("_PCL", Package(#{p("\\_SB_")}));
                        Name("G_ID", 0xFFFFFFFFu32);
                        Method("_DCK", 1, Serialized) {
                            If (Arg0) {
                                #{p("\\GPDK")}(1u32);
                                #{p("\\_SB_.PCI0.LPCB.EC__.DKST")}(1u32);
                            } Else {
                                #{p("\\GPDK")}(0u32);
                                #{p("\\_SB_.PCI0.LPCB.EC__.DKST")}(0u32);
                            }
                            If (Arg0 == #{p("\\_SB_.PCI0.LPCB.EC__.DKR1")}) { Return(0u32); }
                            Return(1u32);
                        }
                        Method("_STA", 0, NotSerialized) {
                            Return(#{p("\\_SB_.PCI0.LPCB.EC__.DKR1")});
                        }
                        Method("GGID", 0, NotSerialized) {
                            Local0 = G_ID;
                            If (Local0 == 0xFFFFFFFFu32) {
                                Local0 = #{p("\\GP02")} | (#{p("\\GP03")} << 1u32) | (#{p("\\GP04")} << 2u32);
                                If (Local0 == 0u32) { Local0 = 3u32; }
                                G_ID = Local0;
                            }
                            Return(Local0);
                        }
                    }
                }

                Scope("\\_GPE") {
                    Method("_L18", 0, NotSerialized) {
                        // Coreboot X200 only reads WAKE here to clear the EC GPE status;
                        // user-visible notifications are delivered by EC query methods.
                        Local0 = #{p("\\_SB_.PCI0.LPCB.EC__.WAKE")};
                    }
                }
            }
        }

        fn extra_tables(&self, _config: &Self::Config) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }
}
