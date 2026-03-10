//! GTDT (Generic Timer Description Table) builder.
//!
//! ACPI 6.5 section 5.2.24. Describes the ARM generic timer
//! interrupts and optional platform timers (watchdog, GT block).
//!
//! The upstream `acpi_tables` crate does not provide a GTDT builder;
//! this module fills that gap using the [`Sdt`] generic table type.

use acpi_tables::sdt::Sdt;

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

/// Build a GTDT table for ARM generic timers.
///
/// # Arguments
///
/// * `cnt_ctrl_base` — Counter control base address. Use
///   `0xFFFF_FFFF_FFFF_FFFF` when EL3 firmware manages the counter
///   (the common case when TF-A is present).
/// * `secure_el1_gsiv` — Secure EL1 physical timer interrupt (SBSA: 29).
/// * `nonsecure_el1_gsiv` — Non-secure EL1 physical timer (SBSA: 30).
/// * `virtual_gsiv` — Virtual timer interrupt (SBSA: 27).
/// * `nonsecure_el2_gsiv` — Non-secure EL2 physical timer (SBSA: 26).
/// * `timer_flags` — Flags applied to all four timer interrupts.
/// * `watchdogs` — Optional SBSA Generic Watchdog subtables.
pub fn build_gtdt(
    cnt_ctrl_base: u64,
    secure_el1_gsiv: u32,
    nonsecure_el1_gsiv: u32,
    virtual_gsiv: u32,
    nonsecure_el2_gsiv: u32,
    timer_flags: u32,
    watchdogs: &[Watchdog],
) -> Sdt {
    // Fixed body: 68 bytes after the 36-byte header (revision 3 adds
    // Virtual EL2 Timer GSIV + Flags at offsets 96-103 compared to rev 2).
    // Each watchdog subtable: 28 bytes (type 1).
    let body_size = 68 + watchdogs.len() * 28;
    let total_size = 36 + body_size;

    let mut sdt = Sdt::new(
        *b"GTDT",
        total_size as u32,
        3, // GTDT revision 3 (ACPI 6.5)
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );

    // Fixed fields after header (offsets 36..104 for revision 3).
    // NOTE: write_u* methods use native byte order; targets are LE.
    sdt.write_u64(36, cnt_ctrl_base); // CntControlBase
    sdt.write_u32(44, 0); // Reserved
    sdt.write_u32(48, secure_el1_gsiv);
    sdt.write_u32(52, timer_flags); // Secure EL1 Flags
    sdt.write_u32(56, nonsecure_el1_gsiv);
    sdt.write_u32(60, timer_flags); // Non-Secure EL1 Flags
    sdt.write_u32(64, virtual_gsiv);
    sdt.write_u32(68, timer_flags); // Virtual Timer Flags
    sdt.write_u32(72, nonsecure_el2_gsiv);
    sdt.write_u32(76, timer_flags); // Non-Secure EL2 Flags
    sdt.write_u64(80, 0xFFFF_FFFF_FFFF_FFFF); // CntReadBase (not used)
    sdt.write_u32(88, watchdogs.len() as u32); // Platform Timer Count

    // Platform Timer Offset: byte offset from table start to first
    // platform timer structure. Zero if no platform timers.
    let platform_timer_offset = if watchdogs.is_empty() { 0u32 } else { 104 };
    sdt.write_u32(92, platform_timer_offset);

    // Revision 3 fields: Virtual EL2 Timer (ACPI 6.3+).
    sdt.write_u32(96, 0); // Virtual EL2 Timer GSIV (0 = not available)
    sdt.write_u32(100, 0); // Virtual EL2 Timer Flags

    // Watchdog subtables (type 1, 28 bytes each).
    for (i, wd) in watchdogs.iter().enumerate() {
        let base = 104 + i * 28;
        sdt.write_u8(base, 1); // Type: SBSA Generic Watchdog
        sdt.write_u16(base + 1, 28); // Length
                                     // base+3: Reserved (already zero)
        sdt.write_u64(base + 4, wd.refresh_base);
        sdt.write_u64(base + 12, wd.control_base);
        sdt.write_u32(base + 20, wd.gsiv);
        sdt.write_u32(base + 24, wd.timer_flags);
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
        let gtdt = build_gtdt(
            0xFFFF_FFFF_FFFF_FFFF,
            29,
            30,
            27,
            26,
            flags::LEVEL_LOW_ALWAYS_ON,
            &[],
        );

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

        let gtdt = build_gtdt(
            0xFFFF_FFFF_FFFF_FFFF,
            29,
            30,
            27,
            26,
            flags::LEVEL_LOW_ALWAYS_ON,
            &[watchdog],
        );

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
