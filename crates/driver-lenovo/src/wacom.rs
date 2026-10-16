//! ThinkPad tablet digitizer (coreboot `drivers/lenovo/wacom.c`): detection
//! from the EEPROM part number and the `WACF004` serial-attached device.

#[cfg(feature = "acpi")]
extern crate alloc;

/// Machine-type prefixes of the X60t, X200t and X201t tablets.
const TABLET_MACHINE_TYPES: [&str; 26] = [
    // X60t.
    "6363", "6364", "6365", "6366", "6367", "6368", "7762", "7763", "7764", "7767", "7768",
    "7769", // X200t.
    "7448", "7449", "7450", "7453", // X201t.
    "0053", "0831", "2985", "3093", "3113", "3144", "3239", "4184", "2263", "2266",
];

/// Coreboot compares the first four part-number characters with the list.
#[must_use]
pub fn is_tablet(part_number: &str) -> bool {
    TABLET_MACHINE_TYPES
        .iter()
        .any(|machine| part_number.get(..4) == Some(*machine))
}

/// `DTR_` digitizer at I/O 0x200, IRQ 5, for the board's LPC scope.
#[cfg(feature = "acpi")]
#[must_use]
pub fn digitizer_aml() -> alloc::vec::Vec<u8> {
    fstart_acpi_macros::acpi_dsl! {
        Device("DTR_") {
            Name("_HID", EisaId("WACF004"));
            Name("_CRS", ResourceTemplate {
                IO(0x0200u16, 0x0200u16, 0x01u8, 0x08u8);
                IRQNoFlags(5u8);
            });
            Method("_STA", 0, NotSerialized) { Return(0x0Fu32); }
        }
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tablet_detection_uses_the_machine_type_prefix() {
        assert!(is_tablet("7453CTO"));
        assert!(is_tablet("2985A12"));
        assert!(!is_tablet("7454CTO"));
        assert!(!is_tablet("745"));
    }
}
