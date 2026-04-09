//! Platform-level ACPI table assembly.
//!
//! The generic assembler ([`assemble`] / [`assemble_and_write`])
//! combines platform tables, per-device DSDT AML, and extra tables
//! into a complete, contiguous ACPI table set.  It is
//! architecture-neutral — RSDP, XSDT, DSDT, and FADT are the same
//! on ARM and x86.
//!
//! Architecture-specific modules provide the platform tables:
//!
//! - [`arm`] — MADT (GICv3), GTDT, ARM FADT flags.  Gated behind
//!   the `arm` feature.

extern crate alloc;

#[cfg(feature = "arm")]
pub mod arm;

#[cfg(feature = "x86")]
pub mod x86;

use alloc::vec;
use alloc::vec::Vec;

use acpi_tables::aml::{Path, Scope};
use acpi_tables::fadt::{FADTBuilder, Flags, PmProfile, FADT};
use acpi_tables::rsdp::Rsdp;
use acpi_tables::sdt::Sdt;
use acpi_tables::xsdt::XSDT;
use acpi_tables::Aml;

use crate::{copy_at, serialize};

/// Size of a standard ACPI SDT header (signature + length + revision +
/// checksum + OEMID + OEM table ID + OEM revision + creator ID + creator
/// revision).
const ACPI_SDT_HEADER_SIZE: usize = 36;

/// Size of each XSDT entry (64-bit physical address pointer).
const XSDT_ENTRY_SIZE: usize = 8;

// Re-export ARM types when available, for convenience.
#[cfg(feature = "arm")]
pub use arm::{ArmConfig, IortConfig, WatchdogConfig};

// Re-export x86 types when available.
#[cfg(feature = "x86")]
pub use x86::{HpetConfig, IoApicConfig, IsoConfig, X86Config};

/// Platform-specific ACPI configuration enum.
///
/// Each variant carries the parameters for platform-level tables.
pub enum PlatformConfig {
    /// ARM platform (GICv3, generic timers, HW-reduced ACPI + PSCI).
    #[cfg(feature = "arm")]
    Arm(ArmConfig),

    /// x86 platform (Local APIC + I/O APIC, optional HPET).
    #[cfg(feature = "x86")]
    X86(X86Config),
}

/// FADT configuration — architecture-neutral parameters that control
/// which FADT flags and fields are set.
///
/// Platform modules populate this; the generic assembler uses it to
/// build the FADT.
pub struct FadtConfig {
    /// Set the HW-Reduced ACPI flag (no legacy hardware).
    pub hw_reduced: bool,
    /// Set the Low Power S0 Idle Capable flag.
    pub low_power_s0: bool,
    /// Set ARM PSCI compliant boot arch flag.
    pub arm_psci: bool,
    /// ACPI PM profile.
    pub pm_profile: PmProfile,
}

/// Assemble a complete ACPI table set and write it to physical memory.
///
/// Returns the total size in bytes of the written table data.
///
/// # Arguments
///
/// * `table_addr` — Physical address in DRAM for the table placement.
/// * `platform` — Platform-level config (determines MADT, GTDT, FADT).
/// * `device_dsdt_aml` — Concatenated AML bytes from all devices
///   (via `AcpiDevice::dsdt_aml()`), to be placed inside `\_SB` scope.
/// * `device_extra_tables` — Pre-serialized standalone tables from
///   devices (SPCR, MCFG). Each `Vec<u8>` is a complete ACPI table.
pub fn assemble_and_write(
    table_addr: u64,
    platform: &PlatformConfig,
    device_dsdt_aml: &[u8],
    device_extra_tables: &[Vec<u8>],
) -> usize {
    // Delegate to the platform module to build platform-specific
    // tables (MADT, GTDT/HPET) and FADT configuration.
    let (platform_tables, fadt_config) = match platform {
        #[cfg(feature = "arm")]
        PlatformConfig::Arm(cfg) => arm::build_platform_tables(cfg),
        #[cfg(feature = "x86")]
        PlatformConfig::X86(cfg) => x86::build_platform_tables(cfg),
    };

    let data = assemble(
        table_addr,
        &fadt_config,
        &platform_tables,
        device_dsdt_aml,
        device_extra_tables,
    );

    let len = data.len();

    // SAFETY: table_addr points to writable DRAM reserved for ACPI tables.
    // The board config guarantees this region is available and does not
    // overlap with stack, heap, or code.
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), table_addr as *mut u8, len);
    }

    len
}

/// Assemble a complete ACPI table set into a contiguous buffer.
///
/// This is the generic, architecture-neutral assembler.  It takes
/// pre-built platform tables and FADT config from the platform module,
/// combines them with per-device DSDT AML and extra tables, and
/// produces a contiguous buffer starting with RSDP.
///
/// Table order: RSDP, XSDT, DSDT, FADT, [platform tables...],
/// [extra tables...]
///
/// Returns a `Vec<u8>` ready to be copied to `base_addr` in DRAM.
pub fn assemble(
    base_addr: u64,
    fadt_config: &FadtConfig,
    platform_tables: &[Vec<u8>],
    device_dsdt_aml: &[u8],
    device_extra_tables: &[Vec<u8>],
) -> Vec<u8> {
    // Build DSDT: header + device AML inside \_SB scope.
    let dsdt_bytes = serialize(&build_dsdt(device_dsdt_aml));

    // Phase 1: Calculate layout with 16-byte alignment.
    // XSDT entries: FADT + each platform table + each extra table
    let num_xsdt_entries = 1 + platform_tables.len() + device_extra_tables.len();
    let rsdp_size = Rsdp::len();
    let xsdt_estimate = ACPI_SDT_HEADER_SIZE + XSDT_ENTRY_SIZE * num_xsdt_entries;
    let fadt_size = FADT::len();

    let mut offset: usize = 0;

    let rsdp_off = offset;
    offset += crate::align_up(rsdp_size, 16);

    let xsdt_off = offset;
    offset += crate::align_up(xsdt_estimate, 16);

    let dsdt_off = offset;
    offset += crate::align_up(dsdt_bytes.len(), 16);

    let fadt_off = offset;
    offset += crate::align_up(fadt_size, 16);

    // Platform tables (MADT, GTDT, etc.)
    let mut platform_offsets = Vec::new();
    for pt in platform_tables {
        platform_offsets.push(offset);
        offset += crate::align_up(pt.len(), 16);
    }

    // Device extra tables (SPCR, MCFG, etc.)
    let mut extra_offsets = Vec::new();
    for et in device_extra_tables {
        extra_offsets.push(offset);
        offset += crate::align_up(et.len(), 16);
    }

    let total_size = offset;

    // Phase 2: Build cross-referencing tables.
    let dsdt_addr = base_addr + dsdt_off as u64;
    let fadt_addr = base_addr + fadt_off as u64;
    let xsdt_addr = base_addr + xsdt_off as u64;

    // Build FADT with DSDT reference.
    let fadt_bytes = build_fadt(dsdt_addr, fadt_config);

    // Build XSDT referencing FADT + platform tables + extra tables.
    let mut xsdt = XSDT::new(crate::OEM_ID, crate::OEM_TABLE_ID, crate::OEM_REVISION);
    xsdt.add_entry(fadt_addr);
    for (i, _) in platform_tables.iter().enumerate() {
        xsdt.add_entry(base_addr + platform_offsets[i] as u64);
    }
    for (i, _) in device_extra_tables.iter().enumerate() {
        xsdt.add_entry(base_addr + extra_offsets[i] as u64);
    }
    let xsdt_bytes = serialize(&xsdt);
    assert!(
        xsdt_bytes.len() <= xsdt_estimate,
        "XSDT serialized size ({}) exceeds estimate ({})",
        xsdt_bytes.len(),
        xsdt_estimate,
    );

    // Build RSDP pointing to XSDT.
    let rsdp = Rsdp::new(crate::OEM_ID, xsdt_addr);
    let rsdp_bytes = serialize(&rsdp);

    // Phase 3: Assemble into contiguous buffer.
    let mut buffer = vec![0u8; total_size];

    copy_at(&mut buffer, rsdp_off, &rsdp_bytes);
    copy_at(&mut buffer, xsdt_off, &xsdt_bytes);
    copy_at(&mut buffer, dsdt_off, &dsdt_bytes);
    copy_at(&mut buffer, fadt_off, &fadt_bytes);
    for (i, pt) in platform_tables.iter().enumerate() {
        copy_at(&mut buffer, platform_offsets[i], pt);
    }
    for (i, et) in device_extra_tables.iter().enumerate() {
        copy_at(&mut buffer, extra_offsets[i], et);
    }

    buffer
}

/// Build the FADT from architecture-neutral configuration.
fn build_fadt(dsdt_addr: u64, config: &FadtConfig) -> Vec<u8> {
    // ARM platform has its own build_fadt that handles arm_boot_arch.
    // For generic path, delegate to ARM when the feature is enabled
    // and arm_psci is set.
    #[cfg(feature = "arm")]
    if config.arm_psci {
        return arm::build_fadt(dsdt_addr, config);
    }

    // Generic (x86-like) FADT path.
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

    let fadt = fadt_builder.finalize();
    let mut bytes = Vec::new();
    fadt.to_aml_bytes(&mut bytes);
    bytes
}

/// Build the DSDT from collected device AML bytes.
///
/// Wraps the device AML in a `\_SB` scope and appends to the DSDT header.
fn build_dsdt(device_aml: &[u8]) -> Sdt {
    let mut dsdt = Sdt::new(
        *b"DSDT",
        36,
        2,
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );

    if !device_aml.is_empty() {
        // Build \_SB scope wrapping the device AML.
        // We construct it manually because the pre-serialized device
        // AML bytes can't be passed as `&dyn Aml` references.
        let scope_name = Path::new("\\_SB_");
        let empty_scope = Scope::new(scope_name, vec![]);
        let mut _scope_bytes = Vec::new();
        empty_scope.to_aml_bytes(&mut _scope_bytes);

        // Build scope manually: ScopeOp + PkgLength + "\\_SB_" + device_aml
        let name_aml = encode_name_path(b"\\_SB_");
        let content_len = name_aml.len() + device_aml.len();
        let pkg_len = crate::encode_pkg_length(content_len);

        dsdt.append_slice(&[0x10]); // ScopeOp
        dsdt.append_slice(&pkg_len);
        dsdt.append_slice(&name_aml);
        dsdt.append_slice(device_aml);
    }

    dsdt.update_checksum();
    dsdt
}

/// Encode an AML name path.
fn encode_name_path(name: &[u8]) -> Vec<u8> {
    name.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assemble_generic() {
        // Build with no platform tables and no devices — just RSDP + XSDT + DSDT + FADT.
        let fadt_config = FadtConfig {
            hw_reduced: true,
            low_power_s0: false,
            arm_psci: false,
            pm_profile: PmProfile::Unspecified,
        };

        let data = assemble(0x1000_0000, &fadt_config, &[], &[], &[]);

        // Verify RSDP signature at offset 0.
        assert_eq!(&data[0..8], b"RSD PTR ");

        // Verify RSDP checksum (first 20 bytes).
        let sum = data[..20].iter().fold(0u8, |a, &x| a.wrapping_add(x));
        assert_eq!(sum, 0, "RSDP checksum failed");

        // Should have RSDP + XSDT + DSDT + FADT at minimum.
        assert!(data.len() > 100, "assembled tables too small");

        // DSDT should be present (search for signature).
        assert!(
            data.windows(4).any(|w| w == b"DSDT"),
            "DSDT not found in assembled data"
        );

        // FADT should be present.
        assert!(
            data.windows(4).any(|w| w == b"FACP"),
            "FADT not found in assembled data"
        );
    }

    #[test]
    fn test_assemble_with_platform_and_extra_tables() {
        let fadt_config = FadtConfig {
            hw_reduced: true,
            low_power_s0: true,
            arm_psci: false,
            pm_profile: PmProfile::PerformanceServer,
        };

        // Fake platform table (just needs to be a valid ACPI table-shaped blob).
        let mut fake_platform = vec![0u8; 44]; // minimal ACPI table header
        fake_platform[0..4].copy_from_slice(b"TEST");
        fake_platform[4..8].copy_from_slice(&44u32.to_le_bytes());

        // Fake extra table.
        let mut fake_extra = vec![0u8; 44];
        fake_extra[0..4].copy_from_slice(b"XTRA");
        fake_extra[4..8].copy_from_slice(&44u32.to_le_bytes());

        let data = assemble(
            0x1000_0000,
            &fadt_config,
            &[fake_platform],
            &[],
            &[fake_extra],
        );

        // All four table signatures should be present.
        for sig in [b"DSDT", b"FACP", b"TEST", b"XTRA"] {
            assert!(
                data.windows(4).any(|w| w == sig),
                "{} not found",
                core::str::from_utf8(sig).unwrap()
            );
        }
    }

    #[test]
    fn test_pkg_length_encoding() {
        // 1-byte
        assert_eq!(crate::encode_pkg_length(10), vec![11]);
        // 2-byte boundary
        let pkg = crate::encode_pkg_length(100);
        assert_eq!(pkg.len(), 2);
        assert!(pkg[0] & 0x40 != 0);
    }

    #[cfg(feature = "arm")]
    #[test]
    fn test_assemble_arm_platform() {
        let arm_cfg = ArmConfig {
            num_cpus: 1,
            gic_dist_base: 0x4006_0000,
            gic_redist_base: 0x4008_0000,
            gic_redist_length: None,
            gic_its_base: Some(0x4408_1000),
            timer_gsivs: (29, 30, 27, 26),
            watchdog: Some(WatchdogConfig {
                refresh_base: 0x5001_0000,
                control_base: 0x5001_1000,
                gsiv: 48,
            }),
            iort: None,
        };

        let (platform_tables, fadt_config) = arm::build_platform_tables(&arm_cfg);
        let data = assemble(0x1000_0000, &fadt_config, &platform_tables, &[], &[]);

        assert_eq!(&data[0..8], b"RSD PTR ");
        // Should contain MADT, GTDT, FADT, DSDT.
        for sig in [b"APIC", b"GTDT", b"FACP", b"DSDT"] {
            assert!(
                data.windows(4).any(|w| w == sig),
                "{} not found",
                core::str::from_utf8(sig).unwrap()
            );
        }
    }
}
