//! ARM platform ACPI table builders.
//!
//! Builds ARM-specific tables: MADT (GICv3), GTDT (generic timers),
//! and provides ARM-specific FADT configuration (HW-reduced ACPI
//! with PSCI). These apply to any ARM platform using GICv3 and
//! generic timers, not just SBSA-compliant servers.

extern crate alloc;

use alloc::vec::Vec;

use acpi_tables::fadt::{FADTBuilder, Flags, PmProfile};
use acpi_tables::madt::{
    EnabledStatus, GicIts, GicVersion, Gicc, Gicd, Gicr, LocalInterruptController, MADT,
};
use acpi_tables::sdt::Sdt;
use acpi_tables::Aml;

use super::FadtConfig;
use crate::gtdt;

/// ARM platform configuration for ACPI table generation.
///
/// Describes the GICv3 interrupt controller, ARM generic timer,
/// and optional SBSA watchdog. Applicable to any ARM system with
/// GICv3 and generic timers (SBSA, QEMU virt, real hardware).
pub struct ArmConfig {
    /// Number of CPUs.
    pub num_cpus: u32,
    /// GIC Distributor base address.
    pub gic_dist_base: u64,
    /// GIC Redistributor base address.
    pub gic_redist_base: u64,
    /// GIC Redistributor discovery range length in bytes.
    ///
    /// If `None`, defaults to `num_cpus * 0x20000`.
    pub gic_redist_length: Option<u32>,
    /// GIC Interrupt Translation Service (ITS) base address.
    ///
    /// If `Some`, a GIC ITS subtable (type 0x0F) is added to the MADT.
    /// Required for MSI/MSI-X with PCIe on GICv3.
    pub gic_its_base: Option<u64>,
    /// Timer GSIVs: (secure_el1, nonsecure_el1, virtual, nonsecure_el2).
    pub timer_gsivs: (u32, u32, u32, u32),
    /// SBSA Generic Watchdog (optional).
    pub watchdog: Option<WatchdogConfig>,
    /// IORT configuration (optional).
    ///
    /// If set, an IORT table is generated mapping PCI RIDs to GIC ITS
    /// device IDs. Required for PCIe MSI/MSI-X.
    pub iort: Option<IortConfig>,
}

/// IORT configuration for the ARM platform.
pub struct IortConfig {
    /// GIC ITS identifiers (must match MADT ITS entries).
    pub its_ids: &'static [u32],
    /// PCI segment number.
    pub pci_segment: u32,
    /// Memory address size limit in bits.
    pub memory_address_limit: u8,
    /// Number of PCI Request IDs to map.
    pub id_count: u32,
}

/// Watchdog configuration for GTDT.
pub struct WatchdogConfig {
    /// Refresh frame base address.
    pub refresh_base: u64,
    /// Control frame base address.
    pub control_base: u64,
    /// Watchdog GSIV.
    pub gsiv: u32,
}

/// Build ARM platform tables (MADT + GTDT + optional IORT) and FADT configuration.
///
/// Returns `(platform_tables, fadt_config)` where `platform_tables`
/// are pre-serialized MADT, GTDT, and optionally IORT bytes, and
/// `fadt_config` carries ARM-specific FADT parameters.
pub fn build_platform_tables(config: &ArmConfig) -> (Vec<Vec<u8>>, FadtConfig) {
    let madt = build_madt(config);
    let gtdt_table = build_gtdt(config);

    let mut madt_bytes = Vec::new();
    madt.to_aml_bytes(&mut madt_bytes);
    let mut gtdt_bytes = Vec::new();
    gtdt_table.to_aml_bytes(&mut gtdt_bytes);

    let mut platform_tables = alloc::vec![madt_bytes, gtdt_bytes];

    // IORT: IO Remapping Table (maps PCI RIDs → GIC ITS device IDs).
    if let Some(iort_cfg) = &config.iort {
        let iort = crate::iort::build_iort(&crate::iort::IortConfig {
            its_ids: iort_cfg.its_ids,
            pci_segment: iort_cfg.pci_segment,
            memory_address_limit: iort_cfg.memory_address_limit,
            id_count: iort_cfg.id_count,
        });
        let mut iort_bytes = Vec::new();
        iort.to_aml_bytes(&mut iort_bytes);
        platform_tables.push(iort_bytes);
    }

    let fadt_config = FadtConfig {
        hw_reduced: true,
        low_power_s0: true,
        arm_psci: true,
        pm_profile: PmProfile::PerformanceServer,
    };

    (platform_tables, fadt_config)
}

/// Build the GICv3 MADT.
fn build_madt(config: &ArmConfig) -> MADT {
    let mut madt = MADT::new(
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
        LocalInterruptController::Address(0),
    );

    for i in 0..config.num_cpus {
        let gicc = Gicc::new(EnabledStatus::Enabled)
            .cpu_interface_number(i)
            .acpi_processor_uid(i)
            .mpidr(i as u64);
        madt.add_structure(gicc);
    }

    madt.add_structure(Gicd::new(0, config.gic_dist_base, GicVersion::GICv3));

    // GIC Redistributor: use explicit length if provided, otherwise
    // calculate from CPU count (two 64 KiB frames per CPU for GICv3).
    let redist_length = config
        .gic_redist_length
        .unwrap_or(config.num_cpus * 0x2_0000);
    madt.add_structure(Gicr::new(config.gic_redist_base, redist_length));

    // GIC ITS: required for MSI/MSI-X with PCIe on GICv3.
    if let Some(its_base) = config.gic_its_base {
        madt.add_structure(GicIts::new(0, its_base));
    }

    madt
}

/// Build the GTDT.
fn build_gtdt(config: &ArmConfig) -> Sdt {
    let watchdogs: Vec<gtdt::Watchdog> = config
        .watchdog
        .iter()
        .map(|wd| gtdt::Watchdog {
            refresh_base: wd.refresh_base,
            control_base: wd.control_base,
            gsiv: wd.gsiv,
            timer_flags: gtdt::flags::LEVEL_LOW_ALWAYS_ON,
        })
        .collect();

    let (sel1, nsel1, virt, nsel2) = config.timer_gsivs;
    gtdt::build_gtdt(&gtdt::GtdtConfig {
        cnt_ctrl_base: 0xFFFF_FFFF_FFFF_FFFF,
        secure_el1_gsiv: sel1,
        nonsecure_el1_gsiv: nsel1,
        virtual_gsiv: virt,
        nonsecure_el2_gsiv: nsel2,
        timer_flags: gtdt::flags::LEVEL_LOW_ALWAYS_ON,
        watchdogs: &watchdogs,
    })
}

/// Build the FADT with ARM-specific flags.
pub(super) fn build_fadt(dsdt_addr: u64, config: &FadtConfig) -> Vec<u8> {
    let mut fadt_builder =
        FADTBuilder::new(crate::OEM_ID, crate::OEM_TABLE_ID, crate::OEM_REVISION)
            .dsdt_64(dsdt_addr)
            .preferred_pm_profile(config.pm_profile);

    if config.hw_reduced {
        fadt_builder = fadt_builder.flag(Flags::HwReducedAcpi);
    }
    if config.low_power_s0 {
        fadt_builder = fadt_builder.flag(Flags::LowPowerS0IdleCapable);
    }
    if config.arm_psci {
        fadt_builder.arm_boot_arch = 1u16.into(); // PSCI compliant
    }

    let fadt = fadt_builder.finalize();
    let mut bytes = Vec::new();
    fadt.to_aml_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_madt_with_its() {
        let config = ArmConfig {
            num_cpus: 1,
            gic_dist_base: 0x4006_0000,
            gic_redist_base: 0x4008_0000,
            gic_redist_length: Some(0x400_0000),
            gic_its_base: Some(0x4408_1000),
            timer_gsivs: (29, 30, 27, 26),
            watchdog: None,
            iort: None,
        };

        let madt = build_madt(&config);
        let mut bytes = Vec::new();
        madt.to_aml_bytes(&mut bytes);

        // Verify checksum.
        let sum = bytes.iter().fold(0u8, |acc, &x| acc.wrapping_add(x));
        assert_eq!(sum, 0, "MADT checksum failed");

        // Verify signature.
        assert_eq!(&bytes[0..4], b"APIC");

        // Parse subtables to verify ITS is present.
        let mut off = 44usize;
        let mut found_gicr = false;
        let mut found_its = false;
        while off < bytes.len() {
            let st_type = bytes[off];
            let st_len = bytes[off + 1] as usize;
            assert!(st_len > 0, "zero-length subtable at offset {off}");

            match st_type {
                0x0E => {
                    found_gicr = true;
                    assert_eq!(st_len, 16);
                    let length = u32::from_le_bytes(bytes[off + 12..off + 16].try_into().unwrap());
                    assert_eq!(
                        length, 0x400_0000,
                        "GICR discovery range length should use explicit config value"
                    );
                }
                0x0F => {
                    found_its = true;
                    assert_eq!(st_len, 20);
                    let base = u64::from_le_bytes(bytes[off + 8..off + 16].try_into().unwrap());
                    assert_eq!(base, 0x4408_1000, "GIC ITS base address mismatch");
                }
                _ => {}
            }
            off += st_len;
        }
        assert_eq!(
            off,
            bytes.len(),
            "subtable parsing should consume all bytes"
        );
        assert!(found_gicr, "GICR subtable not found");
        assert!(found_its, "GIC ITS subtable not found");
    }

    #[test]
    fn test_madt_without_its() {
        let config = ArmConfig {
            num_cpus: 2,
            gic_dist_base: 0x4006_0000,
            gic_redist_base: 0x4008_0000,
            gic_redist_length: None,
            gic_its_base: None,
            timer_gsivs: (29, 30, 27, 26),
            watchdog: None,
            iort: None,
        };

        let madt = build_madt(&config);
        let mut bytes = Vec::new();
        madt.to_aml_bytes(&mut bytes);

        let sum = bytes.iter().fold(0u8, |acc, &x| acc.wrapping_add(x));
        assert_eq!(sum, 0, "MADT checksum failed");

        let mut off = 44usize;
        let mut found_gicr = false;
        while off < bytes.len() {
            let st_type = bytes[off];
            let st_len = bytes[off + 1] as usize;
            assert!(st_len > 0, "zero-length subtable at offset {off}");

            if st_type == 0x0E {
                found_gicr = true;
                let length = u32::from_le_bytes(bytes[off + 12..off + 16].try_into().unwrap());
                assert_eq!(
                    length, 0x4_0000,
                    "GICR discovery range should default to num_cpus * 0x20000"
                );
            }
            assert_ne!(
                st_type, 0x0F,
                "GIC ITS should not be present when gic_its_base is None"
            );
            off += st_len;
        }
        assert_eq!(
            off,
            bytes.len(),
            "subtable parsing should consume all bytes"
        );
        assert!(found_gicr, "GICR subtable not found");
    }

    #[test]
    fn test_build_platform_tables() {
        let config = ArmConfig {
            num_cpus: 1,
            gic_dist_base: 0x4006_0000,
            gic_redist_base: 0x4008_0000,
            gic_redist_length: None,
            gic_its_base: None,
            timer_gsivs: (29, 30, 27, 26),
            watchdog: None,
            iort: None,
        };

        let (tables, fadt_cfg) = build_platform_tables(&config);

        // Should have MADT + GTDT (no IORT).
        assert_eq!(tables.len(), 2);
        // MADT signature.
        assert_eq!(&tables[0][0..4], b"APIC");
        // GTDT signature.
        assert_eq!(&tables[1][0..4], b"GTDT");
        // ARM FADT config.
        assert!(fadt_cfg.hw_reduced);
        assert!(fadt_cfg.arm_psci);
    }
}
