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
    /// PM1a Event Block I/O port base (x86 only, 0 = unused).
    pub pm1a_evt_blk: u32,
    /// PM1a Control Block I/O port base.
    pub pm1a_cnt_blk: u32,
    /// PM Timer Block I/O port base.
    pub pm_tmr_blk: u32,
    /// GPE0 Block I/O port base.
    pub gpe0_blk: u32,
    /// SCI interrupt number.
    pub sci_int: u16,
    /// IAPC boot arch flags (8042, legacy devices, etc.).
    pub iapc_boot_arch: u16,
}

impl Default for FadtConfig {
    fn default() -> Self {
        Self {
            hw_reduced: false,
            low_power_s0: false,
            arm_psci: false,
            pm_profile: PmProfile::Unspecified,
            pm1a_evt_blk: 0,
            pm1a_cnt_blk: 0,
            pm_tmr_blk: 0,
            gpe0_blk: 0,
            sci_int: 0,
            iapc_boot_arch: 0,
        }
    }
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
    #[cfg(feature = "arm")]
    if config.arm_psci {
        return arm::build_fadt(dsdt_addr, config);
    }

    // x86-style FADT with PM block addresses.
    //
    // If PM block addresses are set (non-zero), we build a full FADT
    // with legacy PM register pointers (pm1a_evt_blk, pm1a_cnt_blk,
    // pm_tmr_blk, gpe0_blk). Otherwise fall back to the builder for
    // HW-reduced platforms.
    if config.pm1a_evt_blk != 0 {
        return build_x86_fadt(dsdt_addr, config);
    }

    // HW-reduced path (no PM registers).
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

/// Build an x86 FADT with full PM block addresses.
///
/// FADT revision 6.1, 276 bytes. Layout matches coreboot's `acpi_fill_fadt`
/// for ICH7: PM1a event/control blocks, PM timer, GPE0, SCI interrupt,
/// IAPC boot arch flags, and the standard ACPI flags.
fn build_x86_fadt(dsdt_addr: u64, config: &FadtConfig) -> Vec<u8> {
    use acpi_tables::sdt::Sdt;
    use acpi_tables::Aml;

    // FADT is 276 bytes for ACPI 6.1 (revision 6).
    let mut fadt = Sdt::new(
        *b"FACP",
        276,
        6, // FADT revision 6 (ACPI 6.1)
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );

    // Preferred PM Profile (offset 45).
    fadt.write_u8(45, config.pm_profile as u8);
    // SCI_INT (offset 46, u16).
    fadt.write_u16(46, config.sci_int);
    // PM1a_EVT_BLK (offset 56, u32).
    fadt.write_u32(56, config.pm1a_evt_blk);
    // PM1a_CNT_BLK (offset 64, u32).
    fadt.write_u32(64, config.pm1a_cnt_blk);
    // PM_TMR_BLK (offset 76, u32).
    fadt.write_u32(76, config.pm_tmr_blk);
    // GPE0_BLK (offset 80, u32).
    fadt.write_u32(80, config.gpe0_blk);
    // PM1_EVT_LEN (offset 88, u8) = 4.
    fadt.write_u8(88, 4);
    // PM1_CNT_LEN (offset 89, u8) = 2.
    fadt.write_u8(89, 2);
    // PM_TMR_LEN (offset 91, u8) = 4.
    fadt.write_u8(91, 4);
    // GPE0_BLK_LEN (offset 92, u8) = 8.
    fadt.write_u8(92, 8);
    // P_LVL2_LAT (offset 96, u16) = 1.
    fadt.write_u16(96, 1);
    // P_LVL3_LAT (offset 98, u16) = 85.
    fadt.write_u16(98, 85);
    // DUTY_OFFSET (offset 104, u8) = 1.
    fadt.write_u8(104, 1);
    // IAPC_BOOT_ARCH (offset 109, u16).
    fadt.write_u16(109, config.iapc_boot_arch);
    // Flags (offset 112, u32).
    // WBINVD | C1_SUPPORTED | SLEEP_BUTTON | S4_RTC_WAKE |
    // PLATFORM_CLOCK | C2_MP_SUPPORTED
    let flags: u32 = (1 << 0)   // WBINVD
        | (1 << 2)              // C1_SUPPORTED
        | (1 << 5)              // SLEEP_BUTTON
        | (1 << 7)              // S4_RTC_WAKE
        | (1 << 8)              // TMR_VAL_EXT (32-bit PM timer)
        | (1 << 15); // PLATFORM_CLOCK
    fadt.write_u32(112, flags);

    // X_DSDT (offset 140, u64).
    let dsdt_bytes = dsdt_addr.to_le_bytes();
    for (i, &b) in dsdt_bytes.iter().enumerate() {
        fadt.write_u8(140 + i, b);
    }

    // X_PM1a_EVT_BLK (offset 148, GAS — 12 bytes).
    write_gas_io(&mut fadt, 148, config.pm1a_evt_blk, 32);
    // X_PM1a_CNT_BLK (offset 172, GAS).
    write_gas_io(&mut fadt, 172, config.pm1a_cnt_blk, 16);
    // X_PM_TMR_BLK (offset 208, GAS).
    write_gas_io(&mut fadt, 208, config.pm_tmr_blk, 32);
    // X_GPE0_BLK (offset 220, GAS).
    write_gas_io(&mut fadt, 220, config.gpe0_blk, 64);

    fadt.update_checksum();

    let mut bytes = Vec::new();
    fadt.to_aml_bytes(&mut bytes);
    bytes
}

/// Write a Generic Address Structure (GAS) for a System I/O port.
fn write_gas_io(sdt: &mut acpi_tables::sdt::Sdt, offset: usize, port: u32, bit_width: u8) {
    sdt.write_u8(offset, 1); // Address Space ID: System I/O
    sdt.write_u8(offset + 1, bit_width);
    sdt.write_u8(offset + 2, 0); // Bit Offset
    sdt.write_u8(
        offset + 3,
        if bit_width <= 8 {
            1
        } else if bit_width <= 16 {
            2
        } else {
            3
        },
    ); // Access Size
    let addr_bytes = (port as u64).to_le_bytes();
    for (i, &b) in addr_bytes.iter().enumerate() {
        sdt.write_u8(offset + 4 + i, b);
    }
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
            ..Default::default()
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
            ..Default::default()
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

    #[test]
    fn test_pkg_length_boundaries() {
        // 1-byte max: content_len + 1 = 0x3F (63), content = 62
        let pkg = crate::encode_pkg_length(62);
        assert_eq!(pkg.len(), 1);
        assert_eq!(pkg[0], 63);

        // 1-byte max edge: content_len + 1 = 0x3F
        let pkg = crate::encode_pkg_length(62);
        assert_eq!(pkg.len(), 1);

        // Transition to 2-byte: content_len + 1 = 0x40 (doesn't fit 6 bits)
        let pkg = crate::encode_pkg_length(63);
        assert_eq!(pkg.len(), 2);
        // Verify total encodes to 63 + 2 = 65
        let total = (pkg[0] as usize & 0x0F) | ((pkg[1] as usize) << 4);
        assert_eq!(total, 65);

        // 2-byte max: content_len + 2 = 0xFFF (4095), content = 4093
        let pkg = crate::encode_pkg_length(4093);
        assert_eq!(pkg.len(), 2);

        // Transition to 3-byte: content_len + 2 = 0x1000
        let pkg = crate::encode_pkg_length(4094);
        assert_eq!(pkg.len(), 3);
    }
}
