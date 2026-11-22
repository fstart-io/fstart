// SPDX-License-Identifier: MIT OR Apache-2.0
//! Generic host ACPI baseline builder for AMD SoCs.
//!
//! Each discovered AMD board builds one deterministic baseline image per entry
//! in [`Board::SOCS`], exercising the board's real [`Board::acpi_board`]
//! contributions, static SoC FCH/SB AML, `FACP`/`FACS`/`MADT`/`MCFG`/`HPET`,
//! synthetic `IVRS`, discrete TPM (`TPM2` + `_SB_.TPM_`), and board extra
//! tables (`SPMI`, `SPCR`, `DBG2`, `WSMT`).
//!
//! Runtime-discovered paths — P-state/C-state [`acpi::Power`], multi-root/NUMA
//! (`SRAT`/`SLIT`/`MSCT`), APEI (`BERT`/`HEST`), SP3 `CPPC`/`PCCT`, and
//! `SocId::Naples` (no current board lists Naples) — are intentionally omitted
//! from board baselines and covered by unit tests here and in
//! [`crate::sp3::acpi::tests`].

use alloc::{format, string::String, vec, vec::Vec};
use fstart_acpi::{
    devices::tpm::{LogArea, TpmFamily, TpmTisAcpi},
    ivrs::IommuRegisters,
};
use fstart_core::{acpi::AcpiSmiConfig, description::PciRootInfo};
use fstart_driver_amd::{
    fch::{
        aml::{FchAcpi, Uart},
        smi,
    },
    nbio::ioapic::{fch_ioapic_id, nbio_ioapic_id},
};
use fstart_image_build::acpi::AcpiBaseline;
use fstart_pci::{
    PciRole::{Forwarded, Own, Window},
    PciRoot,
};

use crate::{
    acpi::{self, Cpu, Inputs, Ioapic, Iommu, RootBridgeAcpi, SocAcpi},
    board::{Board, SocId},
    rs::Variant as _,
    sp3::silicon::{Generation, hw},
};

/// Matches production AMD ACPI storage (`TABLE_CAPACITY = 0x8_0000`).
const BASELINE_TABLE_CAPACITY: usize = 512 * 1024;

/// Representative enabled IOMMU with EFR support so `IVRS` is exercised in host checks.
const SYNTHETIC_IOMMU: Iommu = Iommu {
    base: 0xfe60_0000,
    registers: IommuRegisters {
        capability: 1 << 27 | 0x0003_000f,
        range: 0,
        misc: 0,
        control: 0,
        efr: 0,
        efr2: 0,
        counter_config: 0,
    },
};

/// Representative powered FCH controllers (`FUR0`, `FUR1`, `GPIO`) for SP3 and RS
/// baselines so board tables such as `DBG2` referencing `\_SB_.FUR1` resolve cleanly.
fn fch_sb_aml() -> Result<Vec<u8>, String> {
    let mut fch = FchAcpi {
        gpio_irq: Some(7),
        ..FchAcpi::default()
    };
    let _ = fch.uarts.push(Uart { index: 0, irq: 3 });
    let _ = fch.uarts.push(Uart { index: 1, irq: 4 });
    fch.sb_aml()
        .map_err(|e| format!("AMD FCH baseline AML: {e:?}"))
}

/// Build one deterministic ACPI baseline image per SoC listed in `B::SOCS`.
pub fn baselines<B: Board>(base: u64) -> Result<Vec<AcpiBaseline>, String> {
    if B::SOCS.is_empty() {
        return Err(format!("AMD board {} lists no SoCs", B::NAME));
    }
    let board = B::acpi_board();
    B::SOCS
        .iter()
        .map(|&soc| {
            let bytes = baseline_for_soc::<B>(soc, &board, base)?;
            Ok(AcpiBaseline {
                scenario: soc.name().into(),
                physical_base: base,
                rsdp: base,
                bytes,
            })
        })
        .collect()
}

/// Build a deterministic baseline image for `soc` with `board`'s actual ACPI
/// contributions and SMM/TPM policy.
pub fn baseline_for_soc<B: Board>(
    soc: SocId,
    board: &acpi::Board,
    base: u64,
) -> Result<Vec<u8>, String> {
    let rs_legacy = !board.lpc_aml.is_empty() || board.i8042;
    let genoa_soc = crate::rs::acpi::soc(crate::genoa::Stones::PMBASE, rs_legacy);
    let siena_soc = crate::rs::acpi::soc(crate::siena::Dense::PMBASE, rs_legacy);

    let (soc_acpi, segment, fch_ioapic, nbio_ioapic, acpi_smi, default_fch_aml): (
        &SocAcpi,
        PciRootInfo,
        u8,
        u8,
        Option<AcpiSmiConfig>,
        bool,
    ) = match soc {
        SocId::Picasso => (
            &crate::picasso::acpi::ACPI,
            PciRootInfo {
                segment: 0,
                ecam_base: crate::picasso::ECAM,
                bus_start: 0,
                bus_end: crate::picasso::ECAM_BUSES - 1,
            },
            crate::noncar::acpi::FCH_IOAPIC_ID,
            1,
            Some(AcpiSmiConfig {
                smi_cmd: smi::APM_CNT.into(),
                acpi_enable: smi::ACPI_ENABLE,
                acpi_disable: smi::ACPI_DISABLE,
            }),
            false,
        ),
        SocId::Zeppelin => (
            &crate::zeppelin::acpi::ACPI,
            PciRootInfo {
                segment: 0,
                ecam_base: crate::zeppelin::ECAM,
                bus_start: 0,
                bus_end: (crate::zeppelin::ECAM_BUSES - 1) as u8,
            },
            crate::noncar::acpi::FCH_IOAPIC_ID,
            1,
            Some(AcpiSmiConfig {
                smi_cmd: smi::APM_CNT.into(),
                acpi_enable: smi::ACPI_ENABLE,
                acpi_disable: smi::ACPI_DISABLE,
            }),
            false,
        ),
        SocId::Naples | SocId::Rome | SocId::Milan => {
            let generation = match soc {
                SocId::Naples => Generation::Naples,
                SocId::Rome => Generation::Rome,
                SocId::Milan => Generation::Milan,
                _ => unreachable!(),
            };
            (
                crate::sp3::acpi::of(generation),
                hw::segment(generation),
                fch_ioapic_id(generation.silicon()),
                nbio_ioapic_id(generation.silicon(), 3),
                Some(AcpiSmiConfig {
                    smi_cmd: smi::APM_CNT.into(),
                    acpi_enable: smi::ACPI_ENABLE,
                    acpi_disable: smi::ACPI_DISABLE,
                }),
                true,
            )
        }
        SocId::Genoa => (
            &genoa_soc,
            PciRootInfo {
                segment: 0,
                ecam_base: crate::rs::ECAM_BASE,
                bus_start: 0,
                bus_end: (crate::rs::ECAM_BUSES - 1) as u8,
            },
            crate::rs::acpi::FCH_IOAPIC_ID,
            crate::rs::acpi::ioapic_id(0),
            None,
            true,
        ),
        SocId::Siena => (
            &siena_soc,
            PciRootInfo {
                segment: 0,
                ecam_base: crate::rs::ECAM_BASE,
                bus_start: 0,
                bus_end: (crate::rs::ECAM_BUSES - 1) as u8,
            },
            crate::rs::acpi::FCH_IOAPIC_ID,
            crate::rs::acpi::ioapic_id(0),
            None,
            true,
        ),
    };

    let ecam_size = (u64::from(segment.bus_end) - u64::from(segment.bus_start) + 1) << 20;
    let mmio_window_base = segment.ecam_base.saturating_sub(0x1000_0000);
    // Keep the synthetic single-root bus window aligned with the segment MCFG range.
    let root = PciRoot::new(
        segment,
        [
            Forwarded.io(0, 0x1000),
            Window.io(0x1000, 0xf000),
            Forwarded.mmio(0xa_0000, 0x2_0000),
            Window.mmio(mmio_window_base, 0x0800_0000),
            Own.mmio(segment.ecam_base, ecam_size),
        ],
    )
    .map_err(|e| format!("AMD PCI root ({}): {e:?}", soc.name()))?;

    let root_bridges = [RootBridgeAcpi {
        root: &root,
        fch: true,
        domain: None,
        ioapic: Some(Ioapic {
            id: nbio_ioapic,
            base: 0xfec0_1000,
        }),
        ioapic_features: 0,
        iommu: Some(SYNTHETIC_IOMMU),
    }];

    let cpu_count = B::MAX_CPUS.clamp(1, 2) as usize;
    let cpus: Vec<Cpu> = (0..cpu_count)
        .map(|i| Cpu {
            apic_id: i as u32,
            domain: 0,
            aml: Vec::new(),
        })
        .collect();

    let mut sb_aml = if default_fch_aml {
        fch_sb_aml()?
    } else {
        Vec::new()
    };
    sb_aml.extend(B::acpi_soc_sb_aml(soc)?);

    let tpm = B::DISCRETE_TPM.then_some(TpmTisAcpi {
        family: TpmFamily::Tpm20,
        server: !matches!(soc, SocId::Picasso),
        log: Some(LogArea {
            base: 0x0010_0000,
            size: 0x1_0000,
        }),
    });

    let tables = acpi::build(&Inputs {
        soc: soc_acpi,
        pci: segment,
        cpus: &cpus,
        fch_ioapic_id: fch_ioapic,
        hpet_block_id: 0x1022_8201,
        root_bridges: &root_bridges,
        numa: None,
        power: None,
        acpi_smi,
        bmc_sx_notify: B::BMC_SX_NOTIFY.filter(|_| acpi_smi.is_some()),
        tpm,
        devices: &[],
        ivrs_devices: &[],
        sb_aml: &sb_aml,
        tables: &[],
        apei: None,
        board,
    })
    .map_err(|e| format!("AMD ACPI collection ({}): {e:?}", soc.name()))?;

    let mut image = vec![0; BASELINE_TABLE_CAPACITY];
    let length = tables
        .assemble_into(&mut image, base)
        .map_err(|e| format!("AMD ACPI assembly ({}): {e:?}", soc.name()))?;
    image.truncate(length);
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_BASE: u64 = 0x7f00_0000;

    struct AllSocsBoard;
    impl Board for AllSocsBoard {
        const NAME: &'static str = "test-all-socs";
        const SOCS: &'static [SocId] = &[
            SocId::Picasso,
            SocId::Zeppelin,
            SocId::Naples,
            SocId::Rome,
            SocId::Milan,
            SocId::Genoa,
            SocId::Siena,
        ];
        const MAX_CPUS: u16 = 16;
        const DISCRETE_TPM: bool = true;

        fn acpi_board() -> acpi::Board {
            acpi::Board::default()
        }
    }

    #[test]
    fn builds_valid_baseline_for_every_soc_including_naples() {
        let images = baselines::<AllSocsBoard>(TEST_BASE).expect("all AMD SoC baselines");
        assert_eq!(images.len(), AllSocsBoard::SOCS.len());
        for (soc, image) in AllSocsBoard::SOCS.iter().zip(&images) {
            assert_eq!(image.scenario, soc.name());
            assert_eq!(image.physical_base, TEST_BASE);
            assert_eq!(image.rsdp, TEST_BASE);
            assert!(image.bytes.starts_with(b"RSD PTR "), "{}", soc.name());
            let sum: u8 = image.bytes[..20].iter().copied().fold(0, u8::wrapping_add);
            assert_eq!(sum, 0, "RSDP v1 checksum for {}", soc.name());
        }
    }

    #[test]
    fn propagates_soc_sb_aml_error() {
        struct FailingBoard;
        impl Board for FailingBoard {
            const NAME: &'static str = "test-failing-sb";
            const SOCS: &'static [SocId] = &[SocId::Picasso];
            const MAX_CPUS: u16 = 4;

            fn acpi_board() -> acpi::Board {
                acpi::Board::default()
            }

            fn acpi_soc_sb_aml(soc: SocId) -> Result<Vec<u8>, String> {
                Err(format!("synthetic FCH error on {}", soc.name()))
            }
        }

        let err = baselines::<FailingBoard>(TEST_BASE).unwrap_err();
        assert!(err.contains("synthetic FCH error on picasso"), "{err}");
    }
}
