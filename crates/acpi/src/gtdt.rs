//! GTDT (Generic Timer Description Table) builder.
//!
//! ACPI 6.5 section 5.2.24. Describes the ARM generic timer
//! interrupts and optional platform timers (watchdog, GT block).
//!
//! The upstream `acpi_tables` crate does not provide a GTDT builder;
//! this module fills that gap using the [`Sdt`] generic table type.

use acpi_tables::sdt::Sdt;

// ---------------------------------------------------------------------------
// Wire-format structs — #[repr(C, packed)] mirrors the ACPI 6.5 layout.
// ---------------------------------------------------------------------------

/// GTDT fixed body after the 36-byte ACPI SDT header (revision 3).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GtdtBody {
    cnt_ctrl_base: u64,
    _reserved: u32,
    secure_el1_gsiv: u32,
    secure_el1_flags: u32,
    nonsecure_el1_gsiv: u32,
    nonsecure_el1_flags: u32,
    virtual_gsiv: u32,
    virtual_flags: u32,
    nonsecure_el2_gsiv: u32,
    nonsecure_el2_flags: u32,
    cnt_read_base: u64,
    platform_timer_count: u32,
    platform_timer_offset: u32,
    /// Revision 3 addition (ACPI 6.3+).
    virtual_el2_gsiv: u32,
    virtual_el2_flags: u32,
}

/// SBSA Generic Watchdog subtable (type 1, 28 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct WatchdogSubtable {
    subtable_type: u8,
    length: u16,
    _reserved: u8,
    refresh_base: u64,
    control_base: u64,
    gsiv: u32,
    timer_flags: u32,
}

const ACPI_HDR: usize = 36;
const GTDT_BODY_SIZE: usize = core::mem::size_of::<GtdtBody>();
const WATCHDOG_SUBTABLE_SIZE: usize = core::mem::size_of::<WatchdogSubtable>();
const PLATFORM_TIMERS_OFFSET: u32 = (ACPI_HDR + GTDT_BODY_SIZE) as u32;

/// Timer interrupt flags per ACPI 6.5 Table 5.39.
pub mod flags {
    /// Level-triggered, active-low, always-on.
    ///
    /// Standard for SBSA generic timers.
    pub const LEVEL_LOW_ALWAYS_ON: u32 = 0x0000_0005;
}

/// SBSA Generic Watchdog Timer configuration.
#[derive(Debug, Clone)]
pub struct Watchdog {
    /// Refresh frame physical address.
    pub refresh_base: u64,
    /// Control frame physical address.
    pub control_base: u64,
    /// Watchdog timer GSIV (interrupt number).
    pub gsiv: u32,
    /// Timer flags (edge/level, polarity, always-on).
    pub timer_flags: u32,
}

/// GTDT configuration for ARM generic timers.
#[derive(Debug, Clone)]
pub struct GtdtConfig<'a> {
    /// Counter control base address.
    ///
    /// Use `0xFFFF_FFFF_FFFF_FFFF` when EL3 firmware manages the counter
    /// (the common case when TF-A is present).
    pub cnt_ctrl_base: u64,
    /// Secure EL1 physical timer interrupt GSIV (SBSA: 29).
    pub secure_el1_gsiv: u32,
    /// Non-secure EL1 physical timer interrupt GSIV (SBSA: 30).
    pub nonsecure_el1_gsiv: u32,
    /// Virtual timer interrupt GSIV (SBSA: 27).
    pub virtual_gsiv: u32,
    /// Non-secure EL2 physical timer interrupt GSIV (SBSA: 26).
    pub nonsecure_el2_gsiv: u32,
    /// Flags applied to all four timer interrupts.
    pub timer_flags: u32,
    /// Optional SBSA Generic Watchdog subtables.
    pub watchdogs: &'a [Watchdog],
}

/// Build a GTDT table for ARM generic timers.
pub fn build_gtdt(config: &GtdtConfig<'_>) -> Sdt {
    let total_size = ACPI_HDR + GTDT_BODY_SIZE + config.watchdogs.len() * WATCHDOG_SUBTABLE_SIZE;

    let mut sdt = Sdt::new(
        *b"GTDT",
        total_size as u32,
        3, // GTDT revision 3 (ACPI 6.5)
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );

    let pt_offset = if config.watchdogs.is_empty() {
        0
    } else {
        PLATFORM_TIMERS_OFFSET
    };

    crate::write_struct(
        &mut sdt,
        ACPI_HDR,
        &GtdtBody {
            cnt_ctrl_base: config.cnt_ctrl_base,
            _reserved: 0,
            secure_el1_gsiv: config.secure_el1_gsiv,
            secure_el1_flags: config.timer_flags,
            nonsecure_el1_gsiv: config.nonsecure_el1_gsiv,
            nonsecure_el1_flags: config.timer_flags,
            virtual_gsiv: config.virtual_gsiv,
            virtual_flags: config.timer_flags,
            nonsecure_el2_gsiv: config.nonsecure_el2_gsiv,
            nonsecure_el2_flags: config.timer_flags,
            cnt_read_base: 0xFFFF_FFFF_FFFF_FFFF, // not used
            platform_timer_count: config.watchdogs.len() as u32,
            platform_timer_offset: pt_offset,
            virtual_el2_gsiv: 0,
            virtual_el2_flags: 0,
        },
    );

    // Watchdog subtables.
    for (i, wd) in config.watchdogs.iter().enumerate() {
        crate::write_struct(
            &mut sdt,
            ACPI_HDR + GTDT_BODY_SIZE + i * WATCHDOG_SUBTABLE_SIZE,
            &WatchdogSubtable {
                subtable_type: 1, // SBSA Generic Watchdog
                length: WATCHDOG_SUBTABLE_SIZE as u16,
                _reserved: 0,
                refresh_base: wd.refresh_base,
                control_base: wd.control_base,
                gsiv: wd.gsiv,
                timer_flags: wd.timer_flags,
            },
        );
    }

    sdt.update_checksum();
    sdt
}

#[cfg(test)]
mod tests {
    use super::*;
    use acpi_tables::Aml;
    use alloc::vec::Vec;

    #[test]
    fn test_gtdt_no_watchdog() {
        let gtdt = build_gtdt(&GtdtConfig {
            cnt_ctrl_base: 0xFFFF_FFFF_FFFF_FFFF,
            secure_el1_gsiv: 29,
            nonsecure_el1_gsiv: 30,
            virtual_gsiv: 27,
            nonsecure_el2_gsiv: 26,
            timer_flags: flags::LEVEL_LOW_ALWAYS_ON,
            watchdogs: &[],
        });

        let mut bytes = Vec::new();
        gtdt.to_aml_bytes(&mut bytes);

        // Verify checksum
        let sum = bytes.iter().fold(0u8, |acc, x| acc.wrapping_add(*x));
        assert_eq!(sum, 0, "GTDT checksum failed");

        // Verify size: 36 header + 68 fixed body = 104
        assert_eq!(bytes.len(), 104);

        // Verify signature
        assert_eq!(&bytes[0..4], b"GTDT");

        // Verify timer GSIVs
        assert_eq!(u32::from_le_bytes(bytes[48..52].try_into().unwrap()), 29);
        assert_eq!(u32::from_le_bytes(bytes[56..60].try_into().unwrap()), 30);
        assert_eq!(u32::from_le_bytes(bytes[64..68].try_into().unwrap()), 27);
        assert_eq!(u32::from_le_bytes(bytes[72..76].try_into().unwrap()), 26);

        // Platform timer count = 0, offset = 0
        assert_eq!(u32::from_le_bytes(bytes[88..92].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[92..96].try_into().unwrap()), 0);

        // Virtual EL2 Timer GSIV = 0, Flags = 0
        assert_eq!(u32::from_le_bytes(bytes[96..100].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[100..104].try_into().unwrap()), 0);
    }

    #[test]
    fn test_gtdt_with_watchdog() {
        let watchdog = Watchdog {
            refresh_base: 0x5001_0000,
            control_base: 0x5001_1000,
            gsiv: 48,
            timer_flags: flags::LEVEL_LOW_ALWAYS_ON,
        };

        let gtdt = build_gtdt(&GtdtConfig {
            cnt_ctrl_base: 0xFFFF_FFFF_FFFF_FFFF,
            secure_el1_gsiv: 29,
            nonsecure_el1_gsiv: 30,
            virtual_gsiv: 27,
            nonsecure_el2_gsiv: 26,
            timer_flags: flags::LEVEL_LOW_ALWAYS_ON,
            watchdogs: &[watchdog],
        });

        let mut bytes = Vec::new();
        gtdt.to_aml_bytes(&mut bytes);

        // Verify checksum
        let sum = bytes.iter().fold(0u8, |acc, x| acc.wrapping_add(*x));
        assert_eq!(sum, 0, "GTDT checksum failed");

        // Verify size: 104 base + 28 watchdog = 132
        assert_eq!(bytes.len(), 132);

        // Platform timer count = 1, offset = 104
        assert_eq!(u32::from_le_bytes(bytes[88..92].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(bytes[92..96].try_into().unwrap()), 104);

        // Watchdog type = 1 (at offset 104)
        assert_eq!(bytes[104], 1);
        // Watchdog length = 28
        assert_eq!(u16::from_le_bytes(bytes[105..107].try_into().unwrap()), 28);
    }
}
