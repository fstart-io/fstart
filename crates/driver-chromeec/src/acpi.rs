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
