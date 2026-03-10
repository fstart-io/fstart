//! SBSA (Server Base System Architecture) ACPI table set builder.
//!
//! Assembles a complete set of ACPI tables for QEMU SBSA-ref:
//! RSDP, XSDT, FADT (HW-reduced + PSCI), MADT (GICv3), GTDT,
//! MCFG, SPCR (PL011), and DSDT with AML device definitions.
//!
//! The tables are written contiguously into a single buffer, 16-byte
//! aligned, suitable for placement at a fixed physical address in DRAM.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use acpi_tables::aml::{
    AddressSpace, AddressSpaceCacheable, Device, EISAName, Interrupt, Memory32Fixed, Method, Name,
    Path, ResourceTemplate, Scope,
};
use acpi_tables::fadt::{FADTBuilder, Flags, PmProfile, FADT};
use acpi_tables::madt::{
    EnabledStatus, GicIts, GicVersion, Gicc, Gicd, Gicr, LocalInterruptController, MADT,
};
use acpi_tables::mcfg::MCFG;
use acpi_tables::rsdp::Rsdp;
use acpi_tables::sdt::Sdt;
use acpi_tables::xsdt::XSDT;
use acpi_tables::Aml;

use crate::gtdt;
use crate::spcr;

/// Hardware configuration for the SBSA platform.
///
/// All addresses are physical. Interrupt numbers are GSIVs
/// (GIC System Interrupt Vectors, i.e., SPI number + 32).
#[derive(Debug, Clone)]
pub struct SbsaConfig {
    /// Number of CPUs (from TF-A FDT or board config).
    pub num_cpus: u32,

    // --- GIC ---
    /// GIC Distributor base address.
    pub gic_dist_base: u64,
    /// GIC Redistributor base address.
    pub gic_redist_base: u64,

    // --- UART ---
    /// PL011 UART base address.
    pub uart_base: u64,
    /// PL011 UART GSIV.
    pub uart_gsiv: u32,

    // --- PCIe ---
    /// PCIe ECAM base address.
    pub pcie_ecam_base: u64,
    /// 32-bit MMIO window start.
    pub pcie_mmio32_base: u32,
    /// 32-bit MMIO window end (inclusive).
    pub pcie_mmio32_end: u32,
    /// 64-bit MMIO window start.
    pub pcie_mmio64_base: u64,
    /// 64-bit MMIO window end (inclusive).
    pub pcie_mmio64_end: u64,
    /// PIO window base.
    pub pcie_pio_base: u64,
    /// PCIe interrupt GSIVs (INTA..INTD), typically 4 entries.
    pub pcie_irqs: [u32; 4],

    // --- AHCI ---
    /// AHCI controller base address.
    pub ahci_base: u64,
    /// AHCI GSIV.
    pub ahci_gsiv: u32,

    // --- xHCI ---
    /// xHCI controller base address.
    pub xhci_base: u64,
    /// xHCI GSIV.
    pub xhci_gsiv: u32,

    // --- Watchdog ---
    /// SBSA Generic Watchdog refresh frame base.
    pub watchdog_refresh_base: u64,
    /// SBSA Generic Watchdog control frame base.
    pub watchdog_control_base: u64,
    /// Watchdog GSIV.
    pub watchdog_gsiv: u32,

    // --- GIC ITS ---
    /// GIC ITS base address (optional, for MSI/MSI-X support).
    pub gic_its_base: Option<u64>,
    /// GIC Redistributor discovery range length override.
    pub gic_redist_length: Option<u32>,
}

impl SbsaConfig {
    /// Default configuration matching QEMU SBSA-ref hardware.
    pub fn qemu_default(num_cpus: u32) -> Self {
        Self {
            num_cpus,
            gic_dist_base: 0x4006_0000,
            gic_redist_base: 0x4008_0000,
            uart_base: 0x6000_0000,
            uart_gsiv: 33,
            pcie_ecam_base: 0xF000_0000,
            pcie_mmio32_base: 0x8000_0000,
            pcie_mmio32_end: 0xEFFF_FFFF,
            pcie_mmio64_base: 0x1_0000_0000,
            pcie_mmio64_end: 0xFF_FFFF_FFFF,
            pcie_pio_base: 0x7FFF_0000,
            pcie_irqs: [35, 36, 37, 38],
            ahci_base: 0x6010_0000,
            ahci_gsiv: 42,
            xhci_base: 0x6011_0000,
            xhci_gsiv: 43,
            watchdog_refresh_base: 0x5001_0000,
            watchdog_control_base: 0x5001_1000,
            watchdog_gsiv: 48,
            gic_its_base: Some(0x4408_1000),
            gic_redist_length: Some(0x400_0000),
        }
    }
}

/// Result of building SBSA ACPI tables.
pub struct AcpiTables {
    /// Contiguous buffer containing all ACPI tables.
    pub data: Vec<u8>,
    /// Byte offset of the RSDP within `data`.
    pub rsdp_offset: usize,
}

/// Build a complete set of ACPI tables for an SBSA platform.
///
/// Tables are laid out contiguously with 16-byte alignment. The caller
/// must copy `result.data` to physical address `base_addr` in guest
/// memory. The RSDP is at `base_addr + result.rsdp_offset`.
///
/// # Arguments
///
/// * `config` — Hardware configuration (addresses, IRQs, CPU count).
/// * `base_addr` — Physical address where the tables will be placed.
pub fn build_sbsa_tables(config: &SbsaConfig, base_addr: u64) -> AcpiTables {
    // Phase 1: Build individual tables (not yet cross-referenced).
    let dsdt = build_dsdt(config);
    let madt = build_madt(config);
    let gtdt_table = build_gtdt(config);
    let mcfg = build_mcfg(config);
    let spcr = spcr::build_spcr_pl011(config.uart_base, config.uart_gsiv);

    // Serialize each table to bytes.
    let dsdt_bytes = serialize(&dsdt);
    let madt_bytes = serialize(&madt);
    let gtdt_bytes = serialize(&gtdt_table);
    let mcfg_bytes = serialize(&mcfg);
    let spcr_bytes = serialize(&spcr);

    // Phase 2: Calculate layout with 16-byte alignment.
    // Order: RSDP, XSDT, DSDT, FADT, MADT, GTDT, MCFG, SPCR.
    let rsdp_size = Rsdp::len();
    let xsdt_estimate = 36 + 8 * 5; // header + 5 table entries (FADT, MADT, GTDT, MCFG, SPCR)
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

    let madt_off = offset;
    offset += crate::align_up(madt_bytes.len(), 16);

    let gtdt_off = offset;
    offset += crate::align_up(gtdt_bytes.len(), 16);

    let mcfg_off = offset;
    offset += crate::align_up(mcfg_bytes.len(), 16);

    let spcr_off = offset;
    offset += crate::align_up(spcr_bytes.len(), 16);

    let total_size = offset;

    // Phase 3: Build cross-referencing tables with known addresses.
    let dsdt_addr = base_addr + dsdt_off as u64;
    let fadt_addr = base_addr + fadt_off as u64;
    let madt_addr = base_addr + madt_off as u64;
    let gtdt_addr = base_addr + gtdt_off as u64;
    let mcfg_addr = base_addr + mcfg_off as u64;
    let spcr_addr = base_addr + spcr_off as u64;
    let xsdt_addr = base_addr + xsdt_off as u64;

    // Build FADT pointing to DSDT, with ARM PSCI flags.
    let mut fadt_builder =
        FADTBuilder::new(crate::OEM_ID, crate::OEM_TABLE_ID, crate::OEM_REVISION)
            .dsdt_64(dsdt_addr)
            .flag(Flags::HwReducedAcpi)
            .flag(Flags::LowPowerS0IdleCapable)
            .preferred_pm_profile(PmProfile::PerformanceServer);
    // ARM_BOOT_ARCH: PSCI compliant (bit 0).
    fadt_builder.arm_boot_arch = 1u16.into();
    let fadt = fadt_builder.finalize();
    let fadt_bytes = serialize(&fadt);

    // Build XSDT referencing all tables (DSDT is NOT in XSDT; it's
    // referenced via FADT's x_dsdt field instead).
    let mut xsdt = XSDT::new(crate::OEM_ID, crate::OEM_TABLE_ID, crate::OEM_REVISION);
    xsdt.add_entry(fadt_addr);
    xsdt.add_entry(madt_addr);
    xsdt.add_entry(gtdt_addr);
    xsdt.add_entry(mcfg_addr);
    xsdt.add_entry(spcr_addr);
    let xsdt_bytes = serialize(&xsdt);

    // Build RSDP pointing to XSDT.
    let rsdp = Rsdp::new(crate::OEM_ID, xsdt_addr);
    let rsdp_bytes = serialize(&rsdp);

    // Phase 4: Assemble everything into a single contiguous buffer.
    let mut buffer = vec![0u8; total_size];

    copy_at(&mut buffer, rsdp_off, &rsdp_bytes);
    copy_at(&mut buffer, xsdt_off, &xsdt_bytes);
    copy_at(&mut buffer, dsdt_off, &dsdt_bytes);
    copy_at(&mut buffer, fadt_off, &fadt_bytes);
    copy_at(&mut buffer, madt_off, &madt_bytes);
    copy_at(&mut buffer, gtdt_off, &gtdt_bytes);
    copy_at(&mut buffer, mcfg_off, &mcfg_bytes);
    copy_at(&mut buffer, spcr_off, &spcr_bytes);

    AcpiTables {
        data: buffer,
        rsdp_offset: rsdp_off,
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Serialize an Aml object to a Vec<u8>.
fn serialize(aml: &dyn Aml) -> Vec<u8> {
    let mut bytes = Vec::new();
    aml.to_aml_bytes(&mut bytes);
    bytes
}

/// Copy `src` into `dst` at the given offset.
fn copy_at(dst: &mut [u8], offset: usize, src: &[u8]) {
    dst[offset..offset + src.len()].copy_from_slice(src);
}

/// Build the GICv3 MADT for the SBSA platform.
fn build_madt(config: &SbsaConfig) -> MADT {
    let mut madt = MADT::new(
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
        LocalInterruptController::Address(0),
    );

    // One GICC per CPU.
    for i in 0..config.num_cpus {
        let gicc = Gicc::new(EnabledStatus::Enabled)
            .cpu_interface_number(i)
            .acpi_processor_uid(i)
            .mpidr(i as u64);
        madt.add_structure(gicc);
    }

    // GIC Distributor.
    madt.add_structure(Gicd::new(0, config.gic_dist_base, GicVersion::GICv3));

    // GIC Redistributor range: use explicit length if provided,
    // otherwise two 64 KiB frames per CPU.
    let redist_length = config
        .gic_redist_length
        .unwrap_or(config.num_cpus * 0x2_0000);
    madt.add_structure(Gicr::new(config.gic_redist_base, redist_length));

    // GIC ITS for MSI/MSI-X support.
    if let Some(its_base) = config.gic_its_base {
        madt.add_structure(GicIts::new(0, its_base));
    }

    madt
}

/// Build the GTDT for the SBSA platform.
fn build_gtdt(config: &SbsaConfig) -> Sdt {
    let watchdog = gtdt::Watchdog {
        refresh_base: config.watchdog_refresh_base,
        control_base: config.watchdog_control_base,
        gsiv: config.watchdog_gsiv,
        timer_flags: gtdt::flags::LEVEL_LOW_ALWAYS_ON,
    };

    gtdt::build_gtdt(
        0xFFFF_FFFF_FFFF_FFFF, // CntControlBase (managed by EL3)
        29,                    // Secure EL1 timer
        30,                    // Non-secure EL1 timer
        27,                    // Virtual timer
        26,                    // Non-secure EL2 timer
        gtdt::flags::LEVEL_LOW_ALWAYS_ON,
        &[watchdog],
    )
}

/// Build the MCFG for PCIe ECAM.
fn build_mcfg(config: &SbsaConfig) -> MCFG {
    let mut mcfg = MCFG::new(crate::OEM_ID, crate::OEM_TABLE_ID, crate::OEM_REVISION);
    mcfg.add_ecam(config.pcie_ecam_base, 0, 0, 0xFF);
    mcfg
}

/// Build the DSDT with AML device definitions for SBSA.
///
/// Devices:
/// - `COM0` — PL011 UART
/// - `AHC0` — AHCI SATA controller
/// - `USB0` — xHCI USB controller
/// - `PCI0` — PCIe Root Complex
fn build_dsdt(config: &SbsaConfig) -> Sdt {
    let mut dsdt = Sdt::new(
        *b"DSDT",
        36, // header only; AML content appended below
        2,  // DSDT revision 2 (ACPI 2.0+)
        crate::OEM_ID,
        crate::OEM_TABLE_ID,
        crate::OEM_REVISION,
    );

    // --- COM0: PL011 UART ---
    let uart_irq = Interrupt::new(true, false, false, false, config.uart_gsiv);
    let uart_mmio = Memory32Fixed::new(true, config.uart_base as u32, 0x1000);
    let uart_crs = ResourceTemplate::new(vec![&uart_mmio, &uart_irq]);
    let uart_hid_val: &'static str = "ARMH0011";
    let uart_hid = Name::new("_HID".into(), &uart_hid_val);
    let uart_uid = Name::new("_UID".into(), &0u32);
    let uart_crs_name = Name::new("_CRS".into(), &uart_crs);
    let uart_dev = Device::new("COM0".into(), vec![&uart_hid, &uart_uid, &uart_crs_name]);

    // --- AHC0: AHCI ---
    let ahci_irq = Interrupt::new(true, false, false, false, config.ahci_gsiv);
    let ahci_mmio = Memory32Fixed::new(true, config.ahci_base as u32, 0x10000);
    let ahci_crs = ResourceTemplate::new(vec![&ahci_mmio, &ahci_irq]);
    let ahci_hid_val: &'static str = "LNRO0015";
    let ahci_hid = Name::new("_HID".into(), &ahci_hid_val);
    let ahci_uid = Name::new("_UID".into(), &0u32);
    let ahci_cls = Name::new(
        "_CLS".into(),
        &acpi_tables::aml::Package::new(vec![&0x01u8, &0x06u8, &0x01u8]),
    );
    let ahci_cca = Name::new("_CCA".into(), &1u32);
    let ahci_crs_name = Name::new("_CRS".into(), &ahci_crs);
    let ahci_dev = Device::new(
        "AHC0".into(),
        vec![&ahci_hid, &ahci_uid, &ahci_cls, &ahci_cca, &ahci_crs_name],
    );

    // --- USB0: xHCI ---
    let xhci_irq = Interrupt::new(true, false, false, false, config.xhci_gsiv);
    let xhci_mmio = Memory32Fixed::new(true, config.xhci_base as u32, 0x10000);
    let xhci_crs = ResourceTemplate::new(vec![&xhci_mmio, &xhci_irq]);
    let xhci_hid_val: &'static str = "PNP0D10";
    let xhci_hid = Name::new("_HID".into(), &xhci_hid_val);
    let xhci_uid = Name::new("_UID".into(), &0u32);
    let xhci_cca = Name::new("_CCA".into(), &1u32);
    let xhci_crs_name = Name::new("_CRS".into(), &xhci_crs);
    let xhci_dev = Device::new(
        "USB0".into(),
        vec![&xhci_hid, &xhci_uid, &xhci_cca, &xhci_crs_name],
    );

    // --- PCI0: PCIe Root Complex ---
    let pci_hid = Name::new("_HID".into(), &EISAName::new("PNP0A08"));
    let pci_cid = Name::new("_CID".into(), &EISAName::new("PNP0A03"));
    let pci_seg = Name::new("_SEG".into(), &0u32);
    let pci_bbn = Name::new("_BBN".into(), &0u32);
    let pci_uid_val: &'static str = "PCI0";
    let pci_uid = Name::new("_UID".into(), &pci_uid_val);
    let pci_cca = Name::new("_CCA".into(), &1u32);

    // _CRS: Bus range + MMIO windows.
    let bus_range = AddressSpace::<u16>::new_bus_number(0, 0xFF);
    let mmio32 = AddressSpace::<u32>::new_memory(
        AddressSpaceCacheable::NotCacheable,
        true,
        config.pcie_mmio32_base,
        config.pcie_mmio32_end,
        None,
    );
    let mmio64 = AddressSpace::<u64>::new_memory(
        AddressSpaceCacheable::NotCacheable,
        true,
        config.pcie_mmio64_base,
        config.pcie_mmio64_end,
        None,
    );
    let pci_crs = ResourceTemplate::new(vec![&bus_range, &mmio32, &mmio64]);
    let pci_crs_name = Name::new("_CRS".into(), &pci_crs);

    // Simplified _OSC method: accept all OS-requested control.
    // Returns Arg3 (capability buffer) unchanged.
    let osc_ret = acpi_tables::aml::Return::new(&acpi_tables::aml::Arg(3));
    let osc_method = Method::new("_OSC".into(), 4, false, vec![&osc_ret]);

    let pci_dev = Device::new(
        "PCI0".into(),
        vec![
            &pci_hid,
            &pci_cid,
            &pci_seg,
            &pci_bbn,
            &pci_uid,
            &pci_cca,
            &pci_crs_name,
            &osc_method,
        ],
    );

    // Wrap all devices in \_SB scope.
    let scope = Scope::new(
        Path::new("\\_SB_"),
        vec![&uart_dev, &ahci_dev, &xhci_dev, &pci_dev],
    );

    scope.to_aml_bytes(&mut dsdt);
    dsdt.update_checksum();
    dsdt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_sbsa_tables() {
        let config = SbsaConfig::qemu_default(4);
        let result = build_sbsa_tables(&config, 0x1000_0008_0000);

        // Verify RSDP signature.
        assert_eq!(
            &result.data[result.rsdp_offset..result.rsdp_offset + 8],
            b"RSD PTR "
        );

        // Verify RSDP checksum (first 20 bytes).
        let rsdp_slice = &result.data[result.rsdp_offset..result.rsdp_offset + 20];
        let sum = rsdp_slice.iter().fold(0u8, |acc, x| acc.wrapping_add(*x));
        assert_eq!(sum, 0, "RSDP checksum failed");

        // Verify total size is reasonable (should be a few KB).
        assert!(result.data.len() > 500, "tables too small");
        assert!(result.data.len() < 16384, "tables too large");
    }

    #[test]
    fn test_dsdt_has_devices() {
        let config = SbsaConfig::qemu_default(1);
        let dsdt = build_dsdt(&config);

        let mut bytes = Vec::new();
        dsdt.to_aml_bytes(&mut bytes);

        // Verify DSDT checksum.
        let sum = bytes.iter().fold(0u8, |acc, x| acc.wrapping_add(*x));
        assert_eq!(sum, 0, "DSDT checksum failed");

        // Verify DSDT signature.
        assert_eq!(&bytes[0..4], b"DSDT");

        // Size should be > 36 (has AML content).
        assert!(bytes.len() > 100, "DSDT too small, missing AML devices");
    }

    #[test]
    fn test_madt_has_gicc() {
        let config = SbsaConfig::qemu_default(2);
        let madt = build_madt(&config);

        let mut bytes = Vec::new();
        madt.to_aml_bytes(&mut bytes);

        // Verify checksum.
        let sum = bytes.iter().fold(0u8, |acc, x| acc.wrapping_add(*x));
        assert_eq!(sum, 0, "MADT checksum failed");

        // Verify signature "APIC".
        assert_eq!(&bytes[0..4], b"APIC");

        // Should contain at least header (36+8=44) + 2 GICCs (82 each)
        // + GICD (24) + GICR (16) + GIC ITS (20).
        assert!(
            bytes.len() >= 44 + 2 * 82 + 24 + 16 + 20,
            "MADT too small for 2-CPU GICv3 with ITS"
        );
    }
}
