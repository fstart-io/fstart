// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ACPI tables of every AMD SoC of the flow, from what its silicon
//! reports.
//!
//! The builders take plain [`Inputs`], so they stay host-testable. A SoC
//! says what its tables differ in ([`SocAcpi`]) and its flow fills the
//! rest from its drivers: the root bridges with their NBIO IOAPIC and
//! IOMMU, the threads, the DRAM of the domains and its own devices. The
//! board adds its part ([`Board`]).
//!
//! | Table | Source | Module |
//! |---|---|---|
//! | FADT, MADT, HPET | [`SocAcpi`], the NBIO IOAPICs | [`fixed`] |
//! | MCFG | the PCI segment | `fstart_acpi::build_mcfg` |
//! | SRAT, MSCT, SLIT | only with more than one domain | [`numa`] |
//! | IVRS | per IOMMU, types 10h/11h (40h for an ACPI-only eMMC) | [`ivrs`] |
//! | SPMI | board IPMI KCS | `fstart_acpi::devices::IpmiKcsAcpi` |
//! | TPM2 / TCPA | discrete TPM and its event log ([`Inputs::tpm`]) | `fstart_acpi::devices::tpm` |
//! | WSMT | with SMM ([`Inputs::acpi_smi`]) | `fstart_acpi::wsmt` |
//! | DSDT | root bridges, `_PRT`, FCH, CPUs, `_PIC`, sleep states | [`dsdt`] |
//!
//! Tables only some SoCs have are theirs to build ([`Inputs::tables`]):
//! SP3's EINJ and PCCT, the VFCT of a posted video ROM.
//!
//! Interrupts: the FCH IOAPIC has GSI 0-23 and every NBIO IOAPIC 32 more,
//! in the order of their IDs. The bridges of a root bus interrupt through
//! its NBIO IOAPIC as the NBIO routes them ([`prt`]). The `_PRT`s describe
//! APIC mode only: the FCH PIC routing isn't programmed, so there is
//! nothing to describe for an OS in PIC mode.

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::AmlError;
use fstart_acpi::devices::IpmiKcsAcpi;
use fstart_acpi::ivrs::IommuRegisters;
use fstart_acpi::platform::{PlatformConfig, X86Fadt};
/// The board's SMBIOS identity, which the board's `main` passes to the
/// flow with its [`Board`].
pub use fstart_acpi::smbios::SmbiosIdentity;
use fstart_core::acpi::{AcpiGenericDevice, AcpiSmiConfig};
use fstart_core::description::PciRootInfo;
use fstart_driver_amd::fch::acpimmio::AcpiPorts;

pub mod dsdt;
pub mod fixed;
pub mod ivrs;
pub mod numa;
pub mod prt;
mod video;

pub use numa::{Domain, MemoryAffinity, Numa};
pub use video::video_rom_aml;

/// FCH `_S5` type, shared by AML and the payload's handoff.
pub const S5_SLEEP_TYPE: u8 = 5;

/// What a SoC's ACPI tables differ in.
#[derive(Debug, Clone, Copy)]
pub struct SocAcpi {
    /// The FCH ACPI I/O blocks.
    pub ports: AcpiPorts,
    /// The SoC's FADT fields; a board's 8042 is added ([`Board::i8042`]).
    pub fadt: X86Fadt,
    /// The bridges of a root bus with their routes into its NBIO IOAPIC,
    /// in the order of the routing registers.
    pub bridges: &'static [prt::Bridge],
    /// IOAPIC GSIs of FCH INTA-D as programmed in the PCI_INTR table, for
    /// the root complex and the FCH's functions on the FCH's root bus.
    /// `None` if they have no `_PRT` entries.
    pub fch_intx_gsi: Option<[u8; 4]>,
    /// The GPE the internal bridges signal PME on (wake), and those
    /// bridges.
    pub wake: Option<Wake>,
    /// An xHCI behind an internal bridge that has a device object:
    /// the bridge and the xHCI's `_ADR`.
    pub xhci: Option<((u8, u8), u32)>,
    /// The sleep states, `(state, SLP_TYP)`. S3 needs a resume path.
    pub sleep_states: &'static [(u8, u8)],
    /// SLIT distance between two domains of a socket (32 between sockets).
    pub socket_distance: u8,
}

/// PME wake of the internal bridges: `_PRW` on each and the GPE's handler
/// notifying them.
#[derive(Debug, Clone, Copy)]
pub struct Wake {
    pub gpe: u8,
    /// The bridges' `(device, function)` on every root bus.
    pub bridges: &'static [(u8, u8)],
}

/// An enabled thread. The slice order is the ACPI processor UID (MADT and
/// processor-device `_UID`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cpu {
    pub apic_id: u32,
    /// Proximity domain (0 with one domain).
    pub domain: u32,
    /// Further objects of its processor device, after the power objects of
    /// [`Inputs::power`] (`_CPC`).
    pub aml: Vec<u8>,
}

/// An NBIO IOAPIC as it was programmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ioapic {
    pub id: u8,
    pub base: u64,
}

/// An enabled IOMMU (`bus:00.2` of its root bus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Iommu {
    pub base: u64,
    /// Capability/MMIO register values read from the enabled IOMMU.
    pub registers: IommuRegisters,
}

/// A root bridge, a die's root complex (Naples, Zeppelin, Picasso) or an
/// NBIO (Rome, Milan), with the resources it was enumerated with.
#[derive(Debug, Clone, Copy)]
pub struct RootBridgeAcpi<'a> {
    /// The root bridge the allocator enumerated: `_CRS` reports its buses and
    /// windows.
    pub root: &'a fstart_pci::PciRoot,
    /// The root bridge with the FCH: the LPC bridge, the FCH IOAPIC and
    /// the HPET are behind it.
    pub fch: bool,
    /// `_PXM` when SRAT is built.
    pub domain: Option<u32>,
    /// Its NBIO IOAPIC, if the BAR is placed; without it the bridge has no
    /// APIC routing to describe.
    pub ioapic: Option<Ioapic>,
    /// SMN address of this NBIO's `IOAPIC::FEATURES_ENABLE` through its own
    /// root bus.
    pub ioapic_features: u32,
    pub iommu: Option<Iommu>,
}

/// Processor power data for `_PSS`/`_PCT`/`_PSD`/`_CST` (BSP's view;
/// every thread runs the same table).
pub use fstart_driver_amd::ccx::power::Power;

/// Board contributions.
#[derive(Debug, Clone, Default)]
pub struct Board {
    /// BMC KCS interface: `IPI0001` under the LPC bridge and the SPMI.
    pub ipmi: Option<IpmiKcsAcpi>,
    /// An 8042 is present (IAPC_BOOT_ARCH bit 1).
    pub i8042: bool,
    /// AML inside `\_SB.PCI0.LPCB` (Super I/O devices, ...).
    pub lpc_aml: Vec<u8>,
    /// AML inside `\_SB`.
    pub sb_aml: Vec<u8>,
    /// Standalone tables (SPCR, ...).
    pub tables: Vec<Vec<u8>>,
}

/// Everything the tables are built from.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    pub soc: &'a SocAcpi,
    /// The PCI segment (`Init::pci_segment`), as the payload gets it.
    pub pci: PciRootInfo,
    pub cpus: &'a [Cpu],
    /// The ID the FCH IOAPIC was programmed with.
    pub fch_ioapic_id: u8,
    /// HPET general capabilities (dword at 0xFED00000).
    pub hpet_block_id: u32,
    pub root_bridges: &'a [RootBridgeAcpi<'a>],
    pub numa: Option<Numa<'a>>,
    pub power: Option<Power<'a>>,
    /// The SMI command port and ACPI enable/disable values the SMM handler
    /// implements; `None` when firmware hands over with SCI_EN already set.
    pub acpi_smi: Option<AcpiSmiConfig>,
    /// The BMC sleep-state notification of the SMM handler; needs
    /// [`Inputs::acpi_smi`].
    pub bmc_sx_notify: Option<fstart_driver_ipmi::SleepNotify>,
    /// The discrete TPM measured boot started on, with its event log.
    pub tpm: Option<fstart_acpi::devices::tpm::TpmTisAcpi>,
    /// The SoC's enabled fixed-address devices, for `\_SB`.
    pub devices: &'a [AcpiGenericDevice],
    /// The SoC's own AML for `\_SB`, after its [`Inputs::devices`]: FCH
    /// controllers and the devices behind them, a posted video ROM.
    pub sb_aml: &'a [u8],
    /// The SoC's own tables.
    pub tables: &'a [Vec<u8>],
    pub board: &'a Board,
}

/// What a SoC's flow gathers for the tables: its fabric's root bridges,
/// its CPUs, NUMA and power, its own devices, AML and tables. `install`
/// adds what every SoC has.
#[derive(Debug, Clone, Copy)]
pub struct Soc<'a> {
    pub soc: &'a SocAcpi,
    pub pci: PciRootInfo,
    pub cpus: &'a [Cpu],
    pub fch_ioapic_id: u8,
    pub root_bridges: &'a [RootBridgeAcpi<'a>],
    pub numa: Option<Numa<'a>>,
    /// The C-states every core runs.
    pub cstates: Option<fstart_driver_amd::ccx::power::CstateInfo>,
    pub threads_per_core: u8,
    pub devices: &'a [AcpiGenericDevice],
    pub sb_aml: &'a [u8],
    pub tables: &'a [Vec<u8>],
}

/// Build the tables of `soc` with what every SoC adds: the HPET, the BSP's
/// P-states (every core runs its table), the SMI command and ACPI mode
/// values of its SMM handler (`smi`), the board's BMC sleep notification
/// and contributions, and the TPM measured boot started on; lay them out
/// in `storage`, which is where they stay. Returns the RSDP.
#[cfg(all(feature = "bundle", target_os = "none"))]
pub fn install(
    soc: &Soc,
    smi: Option<&fstart_driver_amd::fch::smi::Config>,
    facts: &crate::board::Facts,
    board: &Board,
    storage: &mut [u8],
) -> Result<u64, AmlError> {
    // SAFETY: the HPET general capabilities register, decoded by the FCH;
    // read only.
    let hpet_block_id =
        unsafe { fstart_core::mmio::read32(fstart_driver_amd::fch::HPET as usize as *const u32) };
    // SAFETY: the BSP, reading the architectural P-state MSRs.
    let msr = unsafe { fstart_arch::x86::cpu::msr::NativeMsr::new() };
    let pstates = fstart_driver_amd::ccx::power::pstates(&msr);
    let tables = build(&Inputs {
        soc: soc.soc,
        pci: soc.pci,
        cpus: soc.cpus,
        fch_ioapic_id: soc.fch_ioapic_id,
        hpet_block_id,
        root_bridges: soc.root_bridges,
        numa: soc.numa,
        power: Some(Power {
            pstates: &pstates,
            cstates: soc.cstates,
            threads_per_core: soc.threads_per_core,
            c2_latency_us: fstart_driver_amd::ccx::power::c2_latency_us(
                fstart_arch::x86::mp::CpuIdentity::current(),
            ),
        }),
        // The OS enables SCI_EN through the SMM handler.
        acpi_smi: smi.map(|smi| AcpiSmiConfig {
            smi_cmd: fstart_driver_amd::fch::smi::APM_CNT.into(),
            acpi_enable: smi.acpi_enable,
            acpi_disable: smi.acpi_disable,
        }),
        bmc_sx_notify: facts.bmc_sx_notify,
        tpm: crate::tpm::acpi(),
        devices: soc.devices,
        sb_aml: soc.sb_aml,
        tables: soc.tables,
        board,
    })?;
    let address = storage.as_ptr() as u64;
    let length = tables.assemble_into(storage, address)?;
    fstart_log::info!("acpi: {} bytes at {:#x}", length as u32, address);
    Ok(address)
}

/// The built table set, ready for `fstart_acpi::platform::assemble_into`.
pub struct Tables {
    /// FADT, MADT and HPET parameters.
    pub platform: PlatformConfig,
    /// DSDT body (placed in `\_SB`).
    pub dsdt: Vec<u8>,
    /// MCFG, [SRAT, MSCT, SLIT], [IVRS], [SPMI], the SoC's, [TPM2/TCPA],
    /// [WSMT], the board's.
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

/// Build every table from `inputs`.
pub fn build(inputs: &Inputs) -> Result<Tables, AmlError> {
    if inputs.cpus.is_empty() {
        return Err(AmlError::InvalidOperand);
    }
    let windows = fixed::gsi_windows(inputs.fch_ioapic_id, inputs.root_bridges);
    let dsdt = dsdt::build(inputs, &windows)?;
    let board = inputs.board;
    let extra = core::iter::once(fstart_acpi::build_mcfg(&[inputs.pci]))
        .chain(
            inputs
                .numa
                .as_ref()
                .map(|n| numa::tables(inputs.soc, n, inputs.cpus))
                .unwrap_or_default(),
        )
        .chain(ivrs::ivrs(inputs, &windows))
        .chain(board.ipmi.map(|k| k.spmi()))
        .chain(inputs.tables.iter().cloned())
        .chain(inputs.tpm.map(|tpm| tpm.tables()).unwrap_or_default())
        // The SMM handlers take no buffers from the OS (SP3's EINJ uses a
        // fixed, validated communication page and follows no input
        // pointers). The system-resource lock audit remains open.
        .chain(inputs.acpi_smi.map(|_| {
            use fstart_acpi::wsmt;
            wsmt::build_wsmt(wsmt::FIXED_COMM_BUFFERS | wsmt::COMM_BUFFER_NESTED_PTR_PROTECTION)
        }))
        .chain(board.tables.iter().cloned())
        .collect();
    Ok(Tables {
        platform: PlatformConfig::X86(fixed::x86_config(inputs, &windows)),
        dsdt,
        extra,
    })
}

#[cfg(test)]
pub(crate) mod tests;
