//! x86 platform ACPI table builders.
//!
//! Builds x86-specific tables: MADT (Local APIC + I/O APIC), optional
//! HPET, and provides x86-specific FADT configuration.
//!
//! Gated behind the `x86` feature.
//!
//! # Status
//!
//! This module provides the type definitions and table builder stubs.
//! Full implementation will be added when x86 board support lands.
//!
//! # Differences from ARM
//!
//! | Aspect | ARM (GICv3) | x86 (APIC) |
//! |---|---|---|
//! | MADT | GICC, GICD, GICR, GIC ITS | Local APIC, I/O APIC, ISO, NMI |
//! | Timer | GTDT (generic timers) | HPET / PM Timer (FADT) |
//! | FADT | HW-reduced, PSCI | Legacy or modern (SCI, PM1a/b) |
//! | IORT | GIC ITS mapping | N/A (use DMAR for VT-d instead) |

extern crate alloc;

use alloc::vec::Vec;

use super::FadtConfig;

/// x86 platform configuration for ACPI table generation.
///
/// Describes the APIC interrupt controller, optional HPET, and
/// boot configuration for x86 platforms.
pub struct X86Config {
    /// Number of CPUs.
    pub num_cpus: u32,
    /// Local APIC base address (usually 0xFEE0_0000).
    pub lapic_base: u64,
    /// I/O APIC entries.
    pub ioapics: &'static [IoApicConfig],
    /// Interrupt Source Override entries (ISA IRQ remapping).
    pub isos: &'static [IsoConfig],
    /// HPET base address (optional; `None` uses PM Timer instead).
    pub hpet_base: Option<u64>,
    /// Whether legacy devices (8259 PIC, ISA bus) are present.
    ///
    /// Controls the MADT `PCAT_COMPAT` flag and FADT legacy fields.
    pub legacy_devices: bool,
    /// SCI interrupt number (System Control Interrupt for ACPI events).
    pub sci_irq: u8,
}

/// I/O APIC configuration.
#[derive(Debug, Clone, Copy)]
pub struct IoApicConfig {
    /// I/O APIC ID.
    pub id: u8,
    /// Memory-mapped base address.
    pub base: u64,
    /// Global System Interrupt base (first GSI handled by this I/O APIC).
    pub gsi_base: u32,
}

/// Interrupt Source Override (ISO) configuration.
///
/// Maps an ISA interrupt to a different GSI with specified
/// trigger/polarity settings. The most common override maps
/// ISA IRQ 0 (PIT timer) to GSI 2.
#[derive(Debug, Clone, Copy)]
pub struct IsoConfig {
    /// Bus source (0 = ISA).
    pub bus: u8,
    /// Source IRQ (ISA IRQ number).
    pub source: u8,
    /// Global System Interrupt target.
    pub gsi: u32,
    /// Trigger mode and polarity flags (MADT MPS INTI flags).
    pub flags: u16,
}

/// HPET (High Precision Event Timer) table configuration.
pub struct HpetConfig {
    /// HPET base address.
    pub base: u64,
    /// HPET timer block ID (from HPET registers).
    pub timer_block_id: u32,
    /// Minimum clock tick in femtoseconds.
    pub min_tick: u16,
}

/// Build x86 platform tables (MADT + optional HPET) and FADT configuration.
///
/// Returns `(platform_tables, fadt_config)` where `platform_tables`
/// are pre-serialized MADT and optional HPET bytes, and `fadt_config`
/// carries x86-specific FADT parameters.
pub fn build_platform_tables(config: &X86Config) -> (Vec<Vec<u8>>, FadtConfig) {
    let madt = build_madt(config);
    let mut platform_tables = alloc::vec![madt];

    if let Some(hpet_base) = config.hpet_base {
        let hpet = build_hpet(&HpetConfig {
            base: hpet_base,
            timer_block_id: 0x8086_A201, // default Intel HPET ID
            min_tick: 0,
        });
        platform_tables.push(hpet);
    }

    // PMBASE = 0x0500 is the ICH7 default. The exact value is
    // board-specific but hardcoded in ICH7 early_init.
    let pmbase: u32 = 0x0500;

    // IAPC Boot Arch: 8042 keyboard + legacy devices.
    let mut iapc: u16 = 0;
    if config.legacy_devices {
        iapc |= 0x0003; // FADT_8042 | FADT_LEGACY_DEVICES
    }

    let fadt_config = FadtConfig {
        hw_reduced: false,
        low_power_s0: false,
        arm_psci: false,
        pm_profile: acpi_tables::fadt::PmProfile::Desktop,
        pm1a_evt_blk: pmbase,
        pm1a_cnt_blk: pmbase + 0x04,
        pm_tmr_blk: pmbase + 0x08,
        gpe0_blk: pmbase + 0x28,
        sci_int: config.sci_irq as u16,
        iapc_boot_arch: iapc,
    };

    (platform_tables, fadt_config)
}

/// Build the x86 MADT (Multiple APIC Description Table).
///
/// Includes Local APIC entries for each CPU, I/O APIC entries,
/// Interrupt Source Overrides, and NMI Source entries.
fn build_madt(config: &X86Config) -> Vec<u8> {
    use acpi_tables::sdt::Sdt;
    use acpi_tables::Aml;

    // MADT header: 36 (SDT header) + 8 (MADT-specific: LAPIC addr + flags)
    let mut madt = Sdt::new(
        *b"APIC",
        44,
        4, // MADT revision 4 (ACPI 6.0+)
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );

    // Local Interrupt Controller Address (32-bit)
    madt.write_u32(36, config.lapic_base as u32);
    // Flags: bit 0 = PCAT_COMPAT (dual 8259 present)
    let flags: u32 = if config.legacy_devices { 1 } else { 0 };
    madt.write_u32(40, flags);

    // Local APIC entries (type 0, length 8)
    for i in 0..config.num_cpus {
        madt.append(0u8); // Type: Processor Local APIC
        madt.append(8u8); // Length
        madt.append(i as u8); // ACPI Processor UID
        madt.append(i as u8); // APIC ID
        let lapic_flags: u32 = 1; // Enabled
        madt.append_slice(&lapic_flags.to_le_bytes());
    }

    // I/O APIC entries (type 1, length 12)
    for ioapic in config.ioapics {
        madt.append(1u8); // Type: I/O APIC
        madt.append(12u8); // Length
        madt.append(ioapic.id); // I/O APIC ID
        madt.append(0u8); // Reserved
        madt.append_slice(&(ioapic.base as u32).to_le_bytes()); // Address
        madt.append_slice(&ioapic.gsi_base.to_le_bytes()); // GSI Base
    }

    // Interrupt Source Override entries (type 2, length 10)
    for iso in config.isos {
        madt.append(2u8); // Type: Interrupt Source Override
        madt.append(10u8); // Length
        madt.append(iso.bus); // Bus
        madt.append(iso.source); // Source
        madt.append_slice(&iso.gsi.to_le_bytes()); // Global System Interrupt
        madt.append_slice(&iso.flags.to_le_bytes()); // MPS INTI Flags
    }

    madt.update_checksum();

    let mut bytes = Vec::new();
    madt.to_aml_bytes(&mut bytes);
    bytes
}

/// Build an HPET (High Precision Event Timer) table.
fn build_hpet(config: &HpetConfig) -> Vec<u8> {
    use acpi_tables::sdt::Sdt;
    use acpi_tables::Aml;

    // HPET table: 36 (SDT header) + 20 (HPET-specific fields)
    let mut hpet = Sdt::new(
        *b"HPET",
        56,
        1, // HPET revision 1
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );

    // Event Timer Block ID
    hpet.write_u32(36, config.timer_block_id);

    // Base Address (GAS: Generic Address Structure)
    // Address Space ID = 0 (System Memory)
    hpet.write_u8(40, 0); // Address Space ID
    hpet.write_u8(41, 64); // Register Bit Width
    hpet.write_u8(42, 0); // Register Bit Offset
    hpet.write_u8(43, 0); // Access Size (undefined)
                          // Address (64-bit)
    let addr_bytes = config.base.to_le_bytes();
    for (i, &b) in addr_bytes.iter().enumerate() {
        hpet.write_u8(44 + i, b);
    }

    // HPET Number
    hpet.write_u8(52, 0);

    // Minimum Clock Tick
    hpet.write_u16(53, config.min_tick);

    // Page Protection
    hpet.write_u8(55, 0);

    hpet.update_checksum();

    let mut bytes = Vec::new();
    hpet.to_aml_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> X86Config {
        static IOAPICS: [IoApicConfig; 1] = [IoApicConfig {
            id: 0,
            base: 0xFEC0_0000,
            gsi_base: 0,
        }];
        static ISOS: [IsoConfig; 1] = [IsoConfig {
            bus: 0,
            source: 0,
            gsi: 2,
            flags: 0,
        }];
        X86Config {
            num_cpus: 2,
            lapic_base: 0xFEE0_0000,
            ioapics: &IOAPICS,
            isos: &ISOS,
            hpet_base: Some(0xFED0_0000),
            legacy_devices: true,
            sci_irq: 9,
        }
    }

    #[test]
    fn test_x86_madt() {
        let config = test_config();
        let madt_bytes = build_madt(&config);

        // Verify signature
        assert_eq!(&madt_bytes[0..4], b"APIC");

        // Verify checksum
        let sum = madt_bytes.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        assert_eq!(sum, 0, "MADT checksum failed");

        // LAPIC address at offset 36
        let lapic = u32::from_le_bytes(madt_bytes[36..40].try_into().unwrap());
        assert_eq!(lapic, 0xFEE0_0000);

        // PCAT_COMPAT flag
        let flags = u32::from_le_bytes(madt_bytes[40..44].try_into().unwrap());
        assert_eq!(flags & 1, 1);
    }

    #[test]
    fn test_x86_platform_tables() {
        let config = test_config();
        let (tables, fadt_cfg) = build_platform_tables(&config);

        // Should have MADT + HPET
        assert_eq!(tables.len(), 2);
        assert_eq!(&tables[0][0..4], b"APIC");
        assert_eq!(&tables[1][0..4], b"HPET");

        // x86 should not be HW-reduced
        assert!(!fadt_cfg.hw_reduced);
        assert!(!fadt_cfg.arm_psci);
    }

    #[test]
    fn test_x86_hpet() {
        let hpet_bytes = build_hpet(&HpetConfig {
            base: 0xFED0_0000,
            timer_block_id: 0x8086_A201,
            min_tick: 0,
        });

        assert_eq!(&hpet_bytes[0..4], b"HPET");

        let sum = hpet_bytes.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        assert_eq!(sum, 0, "HPET checksum failed");

        // Timer block ID at offset 36
        let tbid = u32::from_le_bytes(hpet_bytes[36..40].try_into().unwrap());
        assert_eq!(tbid, 0x8086_A201);
    }
}
