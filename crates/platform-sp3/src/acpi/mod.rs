//! ACPI tables of the SP3 platform (ACPI_SMM).
//!
//! The builders take plain [`Inputs`] that the flow fills from the silicon
//! drivers (`nbio::ioapic::IoapicInfo`, `nbio::iommu::IommuInfo`, the DF
//! root bridges and DRAM map, `ccx::power`), so they stay host-testable.
//!
//! | Table | Source | Module |
//! |---|---|---|
//! | FADT, MADT, HPET | donor values per generation | [`fixed`] |
//! | MCFG | `Generation::ecam` | [`fixed`] |
//! | SRAT, MSCT, SLIT | only with more than one domain | [`numa`] |
//! | IVRS | per IOMMU, types 10h/11h | [`ivrs`] |
//! | SPMI | board IPMI KCS | `fstart_acpi::devices::IpmiKcsAcpi` |
//! | PCCT, `_CPC` | SMU-served CPPC ([`Inputs::cppc`]) | `smu::v11::cppc` |
//! | TPM2 / TCPA | discrete TPM and its event log ([`Inputs::tpm`]) | `fstart_acpi::devices::tpm` |
//! | WSMT | with SMM ([`Inputs::acpi_smi`]) | `fstart_acpi::wsmt` |
//! | DSDT | root bridges, `_PRT`, FCH, CPUs, `_PIC`, `_S5`, board hook | [`dsdt`] |
//!
//! Restricted correctable-memory EINJ is available with the native SMM
//! backend. Not built (OS-first): HEST/BERT/ERST, CRAT/CDIT.

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::AmlError;
use fstart_acpi::devices::IpmiKcsAcpi;
use fstart_acpi::platform::PlatformConfig;
use fstart_core::acpi::AcpiSmiConfig;
use fstart_driver_amd::smu::v11::CppcInfo;
use fstart_driver_amd::smu::v11::cppc;
pub use fstart_driver_amd::sp3::fch::acpi::FchAcpi;
use fstart_driver_amd::sp3::nbio::ioapic::IoapicInfo;
use fstart_driver_amd::sp3::soc::Generation;
use fstart_driver_amd::sp3::topology::RootBridge;

pub mod dsdt;
pub mod einj;
pub mod fixed;
pub mod ivrs;
pub mod numa;
pub mod prt;

pub use ivrs::IommuAcpi;
pub use numa::{Domain, MemoryAffinity, Numa};

/// An enabled thread. The slice order is the ACPI processor UID (MADT and
/// processor-device `_UID`); put the BSP first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpu {
    pub apic_id: u32,
    /// Proximity domain (0 with one domain).
    pub domain: u32,
}

/// An inclusive address window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window<T> {
    pub base: T,
    pub limit: T,
}

/// A root bridge with the resources the DF routes to it.
#[derive(Debug, Clone, Copy)]
pub struct RootBridgeAcpi<'a> {
    pub bridge: RootBridge,
    /// DF IO windows (the primary's legacy range included; 0xCF8–0xCFF is
    /// cut out and claimed as a consumer).
    pub io: &'a [Window<u16>],
    /// DF MMIO windows below and above 4 GiB, ECAM excluded.
    pub mmio: &'a [Window<u64>],
    /// `_PXM` when SRAT is built.
    pub domain: Option<u32>,
    /// SMN address of this NBIO's `IOAPIC::FEATURES_ENABLE` through its own
    /// root bus: `Nbio::at(nbio::ioapic::FEATURES_ENABLE)`.
    pub ioapic_features: u32,
}

/// Processor power data for `_PSS`/`_PCT`/`_PSD`/`_CST` (BSP's view;
/// every thread runs the same table).
pub use fstart_driver_amd::ccx::power::Power;

/// Board contributions.
#[derive(Debug, Clone, Copy, Default)]
pub struct Board<'a> {
    /// BMC KCS interface: `IPI0001` under the LPC bridge and the SPMI.
    pub ipmi: Option<IpmiKcsAcpi>,
    /// An 8042 is present (IAPC_BOOT_ARCH bit 1; the donors set it).
    pub i8042: bool,
    /// AML inside `\_SB.PCI0.LPCB` (Super I/O devices, ...).
    pub lpc_aml: &'a [u8],
    /// AML inside `\_SB`.
    pub sb_aml: &'a [u8],
}

/// Everything the SP3 tables are built from.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    pub generation: Generation,
    pub cpus: &'a [Cpu],
    /// FCH IOAPIC ID (`nbio::ioapic::fch_ioapic_id` unless reprogrammed).
    pub fch_ioapic_id: u8,
    /// NBIO IOAPICs with a placed BAR (`Nbio::ioapic_info`).
    pub ioapics: &'a [IoapicInfo],
    /// HPET general capabilities (dword at 0xFED00000).
    pub hpet_block_id: u32,
    pub root_bridges: &'a [RootBridgeAcpi<'a>],
    /// Enabled IOMMUs; IVRS is skipped when empty.
    pub iommus: &'a [IommuAcpi],
    pub numa: Option<Numa<'a>>,
    pub power: Option<Power<'a>>,
    /// The SMI command port and ACPI enable/disable values the SMM handler
    /// implements; `None` when firmware hands over with SCI_EN already set.
    pub acpi_smi: Option<AcpiSmiConfig>,
    /// The native SMM PSP backend enabled restricted memory injection.
    pub einj: bool,
    /// FCH devices as the FCH tasks left them.
    pub fch: &'a FchAcpi,
    /// SW SMI base of the BMC sleep-state notification
    /// (`crate::smm::BmcSxNotify`); needs [`Inputs::acpi_smi`].
    pub bmc_sx_notify: Option<u8>,
    /// The discrete TPM measured boot started on
    /// (`crate::board::Board::DISCRETE_TPM`), with its event log.
    pub tpm: Option<fstart_acpi::devices::tpm::TpmTisAcpi>,
    /// SMU-served CPPC: `_CPC` per processor and the PCCT.
    pub cppc: &'a [CppcAcpi<'a>],
    pub board: Board<'a>,
}

/// CPPC inputs (`smu::v11::cppc`).
#[derive(Debug, Clone, Copy)]
pub struct CppcAcpi<'a> {
    pub info: CppcInfo,
    /// PCC region.
    pub base: u64,
    pub smu_bar: u64,
    pub lowest_mhz: u32,
    pub nominal_mhz: u32,
    /// APIC ID of each PCC slot.
    pub slots: &'a [u32],
}

/// The built table set, ready for `fstart_acpi::platform::assemble_into`.
pub struct Tables {
    /// FADT, MADT and HPET parameters.
    pub platform: PlatformConfig,
    /// DSDT body (placed in `\_SB`).
    pub dsdt: Vec<u8>,
    /// MCFG, [SRAT, MSCT, SLIT], [IVRS], [SPMI].
    pub extra: Vec<Vec<u8>>,
}

impl Tables {
    /// Lay the tables out at `table_addr` in `bytes`; returns the length.
    /// The RSDP is at the start.
    pub fn assemble_into(&self, bytes: &mut [u8], table_addr: u64) -> Result<usize, AmlError> {
        fstart_acpi::platform::assemble_into(
            bytes,
            table_addr,
            &self.platform,
            &self.dsdt,
            &self.extra,
        )
    }
}

/// Build every SP3 table from `inputs`.
pub fn build(inputs: &Inputs) -> Result<Tables, AmlError> {
    if inputs.cpus.is_empty()
        || inputs.cppc.len() > 256
        || inputs.cppc.iter().any(|c| c.slots.len() > 256)
        || inputs.cpus.iter().any(|cpu| {
            inputs
                .cppc
                .iter()
                .filter(|c| c.slots.contains(&cpu.apic_id))
                .count()
                > 1
        })
    {
        return Err(AmlError::InvalidOperand);
    }
    let windows = fixed::gsi_windows(inputs.fch_ioapic_id, inputs.ioapics);
    let lpc_aml: Vec<u8> = inputs
        .board
        .ipmi
        .map(|k| k.dsdt_aml())
        .into_iter()
        .flatten()
        .chain(inputs.board.lpc_aml.iter().copied())
        .collect();
    let dsdt = dsdt::build(
        &Inputs {
            board: Board {
                lpc_aml: &lpc_aml,
                ..inputs.board
            },
            ..*inputs
        },
        &windows,
    )?;
    let extra = core::iter::once(fixed::mcfg(inputs.generation))
        .chain(
            inputs
                .numa
                .as_ref()
                .map(|n| numa::tables(inputs.generation, n, inputs.cpus))
                .unwrap_or_default(),
        )
        .chain(ivrs::ivrs(inputs.iommus, &windows))
        .chain(inputs.board.ipmi.map(|k| k.spmi()))
        .chain((inputs.einj && inputs.acpi_smi.is_some()).then(einj::table))
        .chain((!inputs.cppc.is_empty()).then(|| {
            let subspaces: Vec<_> = inputs
                .cppc
                .iter()
                .map(|c| cppc::pcct_subspace(c.base, c.smu_bar))
                .collect();
            fstart_acpi::pcct::build_pcct(&subspaces)
        }))
        .chain(inputs.tpm.map(|tpm| tpm.tables()).unwrap_or_default())
        // EINJ uses a fixed, validated communication page and follows no
        // input pointers. The system-resource lock audit remains open.
        .chain(inputs.acpi_smi.map(|_| {
            use fstart_acpi::wsmt;
            wsmt::build_wsmt(wsmt::FIXED_COMM_BUFFERS | wsmt::COMM_BUFFER_NESTED_PTR_PROTECTION)
        }))
        .collect();
    Ok(Tables {
        platform: PlatformConfig::X86(fixed::x86_config(inputs, &windows)),
        dsdt,
        extra,
    })
}

#[cfg(test)]
mod tests;
