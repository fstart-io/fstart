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

use fstart_core::acpi::AcpiSmiConfig;

use super::FadtConfig;

pub mod wake;

/// x86 platform configuration for ACPI table generation.
///
/// Describes the APIC interrupt controllers, optional HPET, the legacy ACPI
/// fixed hardware and the FADT feature set of an x86 platform.
pub struct X86Config {
    /// Local APIC IDs in MADT order; the ACPI processor UID is the index.
    pub apic_ids: Vec<u32>,
    /// Local APIC base address (usually 0xFEE0_0000).
    pub lapic_base: u64,
    /// MADT Local APIC NMI for all processors on this LINT pin.
    pub lapic_nmi_lint: Option<u8>,
    /// I/O APIC entries.
    pub ioapics: Vec<IoApicConfig>,
    /// Interrupt Source Override entries (ISA IRQ remapping).
    pub isos: Vec<IsoConfig>,
    /// HPET table (optional; `None` leaves the PM Timer only).
    pub hpet: Option<HpetConfig>,
    /// Whether dual 8259 PICs are present (MADT `PCAT_COMPAT`).
    pub legacy_devices: bool,
    /// SCI interrupt number (System Control Interrupt for ACPI events).
    pub sci_irq: u8,
    /// PMBASE I/O port base (chipset-specific, e.g. 0x500 for ICH7).
    ///
    /// Used to derive PM1a_EVT_BLK, PM1a_CNT_BLK, and PM_TMR_BLK addresses
    /// in the FADT.
    pub pmbase: u16,
    /// Offset of the chipset's GPE0 status block from PMBASE.
    pub gpe0_offset: u16,
    /// Total GPE0 status-plus-enable block length in bytes.
    pub gpe0_length: u8,
    /// Optional SMI command port and ACPI enable/disable values.
    ///
    /// Leave as `None` unless the platform has installed an SMI handler
    /// that handles those commands.
    pub acpi_smi: Option<AcpiSmiConfig>,
    /// FADT boot architecture, feature flags and processor power fields.
    pub fadt: X86Fadt,
}

/// The chipset-specific FADT fields of a legacy (non HW-reduced) x86 platform.
#[derive(Debug, Clone, Copy)]
pub struct X86Fadt {
    pub pm_profile: acpi_tables::fadt::PmProfile,
    /// IAPC_BOOT_ARCH: bit 0 legacy devices, bit 1 8042.
    pub iapc_boot_arch: u16,
    /// FADT feature flags, without `RESET_REG_SUP` (set from the reset
    /// register).
    pub flags: u32,
    pub p_lvl2_lat: u16,
    pub p_lvl3_lat: u16,
    pub duty_offset: u8,
}

impl Default for X86Fadt {
    /// The ICH7 (i82801gx) values, legacy devices and 8042.
    fn default() -> Self {
        let defaults = FadtConfig::default();
        Self {
            pm_profile: acpi_tables::fadt::PmProfile::Desktop,
            iapc_boot_arch: IAPC_LEGACY_DEVICES | IAPC_8042,
            flags: defaults.x86_flags,
            p_lvl2_lat: defaults.p_lvl2_lat,
            p_lvl3_lat: defaults.p_lvl3_lat,
            duty_offset: defaults.duty_offset,
        }
    }
}

/// IAPC_BOOT_ARCH: LPC/ISA legacy devices are present.
pub const IAPC_LEGACY_DEVICES: u16 = 1 << 0;
/// IAPC_BOOT_ARCH: an 8042 keyboard controller is present.
pub const IAPC_8042: u16 = 1 << 1;

/// Runtime provider for chipset/platform-owned x86 ACPI topology.
///
/// Board metadata selects that x86 ACPI tables are needed; the southbridge or
/// platform driver supplies APIC, HPET, PMBASE, and SCI details from the
/// hardware model it initializes.
pub trait X86PlatformProvider {
    /// Build the x86 platform ACPI configuration using the runtime CPU count
    /// discovered by MP init.
    fn x86_platform_config(&self, online_cpus: u32) -> X86Config;
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
#[derive(Debug, Clone, Copy)]
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
    let platform_tables = core::iter::once(build_madt(config))
        .chain(config.hpet.as_ref().map(build_hpet))
        .collect();

    let pmbase = u32::from(config.pmbase);
    let (smi_cmd, acpi_enable, acpi_disable) = config
        .acpi_smi
        .map(|smi| (smi.smi_cmd, smi.acpi_enable, smi.acpi_disable))
        .unwrap_or((0, 0, 0));

    let fadt_config = FadtConfig {
        hw_reduced: false,
        low_power_s0: false,
        arm_psci: false,
        pm_profile: config.fadt.pm_profile,
        pm1a_evt_blk: pmbase,
        pm1a_cnt_blk: pmbase + 0x04,
        pm_tmr_blk: pmbase + 0x08,
        gpe0_blk: pmbase + u32::from(config.gpe0_offset),
        gpe0_blk_len: config.gpe0_length,
        sci_int: config.sci_irq as u16,
        iapc_boot_arch: config.fadt.iapc_boot_arch,
        smi_cmd,
        acpi_enable,
        acpi_disable,
        // CF9 is the common PC reset register. Platforms needing a different
        // mechanism should override table construction rather than duplicating
        // this hardware constant in every chipset configuration.
        reset_reg: Some((0x0cf9, 0x06)),
        x86_flags: config.fadt.flags,
        p_lvl2_lat: config.fadt.p_lvl2_lat,
        p_lvl3_lat: config.fadt.p_lvl3_lat,
        duty_offset: config.fadt.duty_offset,
    };

    (platform_tables, fadt_config)
}

const MADT_ALL_PROCESSORS: u8 = 0xff;
const MADT_X2APIC_ALL_PROCESSORS: u32 = 0xffff_ffff;

/// The (UID, APIC ID) of an xAPIC MADT entry; `None` needs an x2APIC entry.
fn xapic_entry(uid: usize, apic_id: u32) -> Option<(u8, u8)> {
    let id = u8::try_from(apic_id).ok().filter(|&id| id != 0xff)?;
    Some((u8::try_from(uid).ok()?, id))
}

/// Build the x86 MADT (Multiple APIC Description Table).
///
/// Local APIC entries for each CPU (x2APIC entries for IDs an xAPIC entry
/// can't hold), I/O APICs, Interrupt Source Overrides and the Local APIC NMI
/// connection.
fn build_madt(config: &X86Config) -> Vec<u8> {
    use acpi_tables::Aml;
    use acpi_tables::sdt::Sdt;

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

    const LAPIC_ENABLED: u32 = 1;
    let mut x2apic = false;
    for (uid, &apic_id) in config.apic_ids.iter().enumerate() {
        if let Some((uid, id)) = xapic_entry(uid, apic_id) {
            // Processor Local APIC (type 0, length 8)
            madt.append_slice(&[0, 8, uid, id]);
            madt.append_slice(&LAPIC_ENABLED.to_le_bytes());
        } else {
            // Processor Local x2APIC (type 9, length 16)
            x2apic = true;
            madt.append_slice(&[9, 16, 0, 0]);
            madt.append_slice(&apic_id.to_le_bytes());
            madt.append_slice(&LAPIC_ENABLED.to_le_bytes());
            madt.append_slice(&(uid as u32).to_le_bytes());
        }
    }

    // I/O APIC entries (type 1, length 12)
    for ioapic in &config.ioapics {
        madt.append(1u8); // Type: I/O APIC
        madt.append(12u8); // Length
        madt.append(ioapic.id); // I/O APIC ID
        madt.append(0u8); // Reserved
        madt.append_slice(&(ioapic.base as u32).to_le_bytes()); // Address
        madt.append_slice(&ioapic.gsi_base.to_le_bytes()); // GSI Base
    }

    // Interrupt Source Override entries (type 2, length 10)
    for iso in &config.isos {
        madt.append(2u8); // Type: Interrupt Source Override
        madt.append(10u8); // Length
        madt.append(iso.bus); // Bus
        madt.append(iso.source); // Source
        madt.append_slice(&iso.gsi.to_le_bytes()); // Global System Interrupt
        madt.append_slice(&iso.flags.to_le_bytes()); // MPS INTI Flags
    }

    // The Local APIC NMI input of every processor, edge-triggered active-high
    // for all processors.
    if let Some(lint) = config.lapic_nmi_lint {
        const EDGE_HIGH: u16 = 0x5;
        // Local APIC NMI (type 4, length 6)
        madt.append_slice(&[4, 6, MADT_ALL_PROCESSORS]);
        madt.append_slice(&EDGE_HIGH.to_le_bytes());
        madt.append(lint);
        if x2apic {
            // Local x2APIC NMI (type 10, length 12)
            madt.append_slice(&[10, 12]);
            madt.append_slice(&EDGE_HIGH.to_le_bytes());
            madt.append_slice(&MADT_X2APIC_ALL_PROCESSORS.to_le_bytes());
            madt.append_slice(&[lint, 0, 0, 0]);
        }
    }

    madt.update_checksum();

    let mut bytes = Vec::new();
    madt.to_aml_bytes(&mut bytes);
    bytes
}

/// Build an HPET (High Precision Event Timer) table.
fn build_hpet(config: &HpetConfig) -> Vec<u8> {
    use acpi_tables::Aml;
    use acpi_tables::sdt::Sdt;

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
        X86Config {
            apic_ids: alloc::vec![0, 1],
            lapic_base: 0xFEE0_0000,
            lapic_nmi_lint: None,
            ioapics: alloc::vec![IoApicConfig {
                id: 0,
                base: 0xFEC0_0000,
                gsi_base: 0,
            }],
            isos: alloc::vec![IsoConfig {
                bus: 0,
                source: 0,
                gsi: 2,
                flags: 0,
            }],
            hpet: Some(HpetConfig {
                base: 0xFED0_0000,
                timer_block_id: 0x8086_A201,
                min_tick: 0,
            }),
            legacy_devices: true,
            sci_irq: 9,
            pmbase: 0x0500,
            gpe0_offset: 0x28,
            gpe0_length: 8,
            acpi_smi: None,
            fadt: X86Fadt::default(),
        }
    }

    /// The MADT subtables after the 44-byte header, as (type, bytes).
    fn madt_entries(madt: &[u8]) -> Vec<(u8, &[u8])> {
        let mut entries = Vec::new();
        let mut rest = &madt[44..];
        while let [kind, len, ..] = *rest {
            let (entry, tail) = rest.split_at(usize::from(len));
            entries.push((kind, entry));
            rest = tail;
        }
        entries
    }

    #[test]
    fn madt_lists_sparse_and_x2apic_ids_with_the_lint1_nmi() {
        let mut config = test_config();
        config.apic_ids = alloc::vec![0, 8, 0x100];
        config.lapic_nmi_lint = Some(1);
        let madt = build_madt(&config);
        let entries = madt_entries(&madt);
        let of = |kind: u8| entries.iter().filter(move |(k, _)| *k == kind);
        let xapic: Vec<_> = of(0).map(|(_, e)| (e[2], e[3])).collect();
        assert_eq!(xapic, [(0, 0), (1, 8)]);
        let (_, x2apic) = of(9).next().unwrap();
        assert_eq!(&x2apic[4..8], &0x100u32.to_le_bytes());
        assert_eq!(&x2apic[12..16], &2u32.to_le_bytes());
        assert_eq!(of(4).next().unwrap().1, &[4, 6, 0xff, 5, 0, 1]);
        assert_eq!(of(10).count(), 1);
        assert_eq!(madt.iter().fold(0u8, |a, &x| a.wrapping_add(x)), 0);
    }

    #[test]
    fn bounded_assembly_never_copies_an_oversized_table_set() {
        use crate::platform::{PlatformConfig, assemble_into};
        let config = PlatformConfig::X86(test_config());
        let mut short = [0xa5; 36];
        assert_eq!(
            assemble_into(&mut short, 0x100000, &config, &[], &[]),
            Err(crate::AmlError::Capacity)
        );
        assert_eq!(short, [0xa5; 36]);
        let mut storage = [0; 4096];
        let len = assemble_into(&mut storage, 0x100000, &config, &[], &[]).unwrap();
        let mut exact = alloc::vec![0; len];
        assert_eq!(
            assemble_into(&mut exact, 0x100000, &config, &[], &[]),
            Ok(len)
        );
        assert_eq!(exact, storage[..len]);
        assert_eq!(
            assemble_into(&mut short, u64::MAX, &config, &[], &[]),
            Err(crate::AmlError::LengthOverflow)
        );
        assert_eq!(short, [0xa5; 36]);
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
        assert_eq!(fadt_cfg.gpe0_blk, 0x0528);
        assert_eq!(fadt_cfg.gpe0_blk_len, 8);
    }

    #[test]
    fn gpe0_geometry_is_chipset_provided() {
        let mut config = test_config();
        config.gpe0_offset = 0x20;
        config.gpe0_length = 16;
        let (_, fadt) = build_platform_tables(&config);
        assert_eq!(fadt.gpe0_blk, 0x0520);
        assert_eq!(fadt.gpe0_blk_len, 16);
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
