//! Intel table collection shared by ramstage and the host ACPI check.

#[cfg(any(feature = "ramstage", all(feature = "host", not(target_os = "none"))))]
extern crate alloc;

#[cfg(any(feature = "ramstage", all(feature = "host", not(target_os = "none"))))]
use alloc::vec::Vec;
#[cfg(any(feature = "ramstage", all(feature = "host", not(target_os = "none"))))]
use fstart_acpi::AmlError;
use fstart_acpi::device::AcpiDevice;
#[cfg(any(feature = "ramstage", all(feature = "host", not(target_os = "none"))))]
use fstart_driver_intel::{IntelNorthbridgeDriver, namespace};

/// Board ACPI contribution consumed by the host baseline check.
///
/// Boards implement this under `#[cfg(feature = "acpi")]`, reusing their
/// existing `mainboard::Acpi` device without enabling any host-only features.
pub trait BoardAcpi: crate::IntelBoardFacts {
    type Acpi: AcpiDevice<Config = ()>;
    fn acpi_baseline() -> Self::Acpi;
}

#[cfg(any(feature = "ramstage", all(feature = "host", not(target_os = "none"))))]
pub(crate) struct Tables {
    pub dsdt: Vec<u8>,
    pub extra: Vec<Vec<u8>>,
}

#[cfg(any(feature = "ramstage", all(feature = "host", not(target_os = "none"))))]
pub(crate) fn collect<NB, SB>(
    north: &NB,
    south: &SB,
    root: &fstart_pci::PciRoot,
    board: &impl AcpiDevice<Config = ()>,
    video: &[u8],
) -> Result<Tables, AmlError>
where
    NB: IntelNorthbridgeDriver + AcpiDevice<Config = <NB as IntelNorthbridgeDriver>::Config>,
    SB: namespace::SouthbridgeAcpi,
{
    let mut dsdt = north.dsdt_aml(north.config());
    dsdt.extend(namespace::pci_root_resources::<NB>(root)?);
    dsdt.extend(south.dsdt_aml(south.config(), &const { namespace::pci_root::<NB>() }));
    dsdt.extend(board.dsdt_aml(&()));
    dsdt.extend_from_slice(video);
    let mut extra = north.extra_tables(north.config());
    extra.extend(south.extra_tables(south.config()));
    extra.extend(board.extra_tables(&()));
    Ok(Tables { dsdt, extra })
}

#[cfg(all(feature = "host", not(target_os = "none")))]
mod host_baseline {
    use super::*;
    use crate::facts::BoardFacts;
    use alloc::{format, string::String};
    use fstart_acpi::platform::{PlatformConfig, X86PlatformProvider};
    use fstart_driver_intel::{IntelSouthbridgeDriver, igd::IgdBars};

    const SYNTHETIC_IGD_BARS: IgdBars = IgdBars {
        gtt_mmio: 0xf400_0000,
        gmadr: 0xd000_0000,
        gtt_pte: None,
    };

    /// Chipset-level host ACPI baseline builder, implemented once per Intel
    /// chipset pairing so boards never duplicate platform fixture wiring.
    pub trait HostAcpiChipset: crate::IntelPlatform {
        fn assemble_baseline(
            config: &'static Self::Config,
            facts: BoardFacts,
            board: &impl AcpiDevice<Config = ()>,
            base: u64,
        ) -> Result<Vec<u8>, String>;
    }

    fn assemble_chipset<NB, SB>(
        north: &NB,
        south: &SB,
        cpus: u32,
        board: &impl AcpiDevice<Config = ()>,
        base: u64,
    ) -> Result<Vec<u8>, String>
    where
        NB: IntelNorthbridgeDriver + AcpiDevice<Config = <NB as IntelNorthbridgeDriver>::Config>,
        SB: namespace::SouthbridgeAcpi + X86PlatformProvider,
    {
        let root = north
            .pci_root()
            .map_err(|e| format!("Intel PCI root: {e:?}"))?;
        let platform = PlatformConfig::X86(south.x86_platform_config(cpus));
        // Host baseline has no POSTed VBIOS / video ROM AML.
        let tables = collect(north, south, &root, board, &[])
            .map_err(|e| format!("Intel ACPI collection: {e:?}"))?;
        let mut image = alloc::vec![0; 128 * 1024];
        let layout = fstart_acpi::platform::assemble_into(
            &mut image,
            base,
            &platform,
            &tables.dsdt,
            &tables.extra,
        )
        .map_err(|e| format!("Intel ACPI assembly: {e:?}"))?;
        image.truncate(layout.len);
        Ok(image)
    }

    impl<C: crate::legacy_cpu::LegacyCpu> HostAcpiChipset for crate::i945::I945Ich7<C> {
        fn assemble_baseline(
            config: &'static Self::Config,
            _facts: BoardFacts,
            board: &impl AcpiDevice<Config = ()>,
            base: u64,
        ) -> Result<Vec<u8>, String> {
            use fstart_driver_intel::{i945::IntelI945, ich7::IntelIch7};
            let north = IntelI945::new_from_config(&config.northbridge)
                .map_err(|e| format!("i945 config: {e:?}"))?;
            let south = IntelIch7::new_from_config(&config.southbridge)
                .map_err(|e| format!("ICH7 config: {e:?}"))?;
            assemble_chipset(&north, &south, 2, board, base)
        }
    }

    impl<C: crate::legacy_cpu::LegacyCpu> HostAcpiChipset for crate::gm965::Gm965Ich8<C> {
        fn assemble_baseline(
            config: &'static Self::Config,
            facts: BoardFacts,
            board: &impl AcpiDevice<Config = ()>,
            base: u64,
        ) -> Result<Vec<u8>, String> {
            use fstart_driver_intel::{gm965::IntelGm965, ich8::IntelIch8};
            let mut north = IntelGm965::new_from_config(&config.northbridge)
                .map_err(|e| format!("GM965 config: {e:?}"))?;
            north.set_host_igd_bars(Some(SYNTHETIC_IGD_BARS));
            north.set_s3_enabled(facts.memory_cache);
            let south = IntelIch8::new_from_config(&config.southbridge)
                .map_err(|e| format!("ICH8 config: {e:?}"))?;
            assemble_chipset(&north, &south, 2, board, base)
        }
    }

    impl<C: crate::legacy_cpu::LegacyCpu> HostAcpiChipset for crate::gm45::Gm45Ich9<C> {
        fn assemble_baseline(
            config: &'static Self::Config,
            facts: BoardFacts,
            board: &impl AcpiDevice<Config = ()>,
            base: u64,
        ) -> Result<Vec<u8>, String> {
            use fstart_driver_intel::{
                gm45::{Gm45GraphicsPolicy, IntelGm45},
                ich9::IntelIch9,
            };
            let mut north = IntelGm45::new_from_config(&config.northbridge)
                .map_err(|e| format!("GM45 config: {e:?}"))?;
            let igd = (!matches!(config.northbridge.graphics, Gm45GraphicsPolicy::Discrete))
                .then_some(SYNTHETIC_IGD_BARS);
            north.set_host_acpi_baseline(igd, true);
            let mut south = IntelIch9::new_from_config(&config.southbridge)
                .map_err(|e| format!("ICH9 config: {e:?}"))?;
            south.set_s3_enabled(facts.memory_cache);
            assemble_chipset(&north, &south, IntelGm45::HOST_CPUS as u32, board, base)
        }
    }

    impl HostAcpiChipset for crate::ironlake::IronlakeIbexPeak {
        fn assemble_baseline(
            config: &'static Self::Config,
            facts: BoardFacts,
            board: &impl AcpiDevice<Config = ()>,
            base: u64,
        ) -> Result<Vec<u8>, String> {
            use fstart_driver_intel::{ibexpeak::IntelIbexPeak, ironlake::IntelIronlake};
            let mut north = IntelIronlake::new_from_config(&config.northbridge)
                .map_err(|e| format!("Ironlake config: {e:?}"))?;
            north.set_host_igd_bars(Some(SYNTHETIC_IGD_BARS));
            let mut south = IntelIbexPeak::new_from_config(&config.southbridge)
                .map_err(|e| format!("Ibex Peak config: {e:?}"))?;
            south.set_s3_enabled(facts.memory_cache);
            assemble_chipset(&north, &south, IntelIronlake::HOST_CPUS as u32, board, base)
        }
    }

    impl HostAcpiChipset for crate::sandybridge::SandybridgeBd82x6x {
        fn assemble_baseline(
            config: &'static Self::Config,
            facts: BoardFacts,
            board: &impl AcpiDevice<Config = ()>,
            base: u64,
        ) -> Result<Vec<u8>, String> {
            use fstart_driver_intel::{bd82x6x::IntelBd82x6x, sandybridge::IntelSandybridge};
            let north = IntelSandybridge::new_from_config(&config.northbridge)
                .map_err(|e| format!("Sandy Bridge config: {e:?}"))?;
            let mut south = IntelBd82x6x::new_from_config(&config.southbridge)
                .map_err(|e| format!("BD82x6x config: {e:?}"))?;
            south.set_s3_enabled(facts.memory_cache);
            assemble_chipset(
                &north,
                &south,
                IntelSandybridge::HOST_CPUS as u32,
                board,
                base,
            )
        }
    }

    impl<C: crate::legacy_cpu::LegacyCpu> HostAcpiChipset for crate::pineview::PineviewIch7<C> {
        fn assemble_baseline(
            config: &'static Self::Config,
            _facts: BoardFacts,
            board: &impl AcpiDevice<Config = ()>,
            base: u64,
        ) -> Result<Vec<u8>, String> {
            use fstart_driver_intel::{ich7::IntelIch7, pineview::IntelPineview};
            let north = IntelPineview::new_from_config(&config.northbridge)
                .map_err(|e| format!("Pineview config: {e:?}"))?;
            let south = IntelIch7::new_from_config(&config.southbridge)
                .map_err(|e| format!("ICH7 config: {e:?}"))?;
            assemble_chipset(&north, &south, 2, board, base)
        }
    }

    impl HostAcpiChipset for crate::baytrail::Baytrail {
        fn assemble_baseline(
            config: &'static Self::Config,
            facts: BoardFacts,
            board: &impl AcpiDevice<Config = ()>,
            base: u64,
        ) -> Result<Vec<u8>, String> {
            use fstart_driver_intel::baytrail::{BaytrailNorth, BaytrailSouth};
            let north = BaytrailNorth::new_from_config(&config.north)
                .map_err(|e| format!("Bay Trail north: {e:?}"))?;
            let mut south = BaytrailSouth::new_from_config(&config.south)
                .map_err(|e| format!("Bay Trail south: {e:?}"))?;
            south.set_s3_enabled(facts.memory_cache);
            assemble_chipset(&north, &south, BaytrailNorth::HOST_CPUS as u32, board, base)
        }
    }

    impl<const LP: bool> HostAcpiChipset for crate::haswell::HaswellLynxPoint<LP> {
        fn assemble_baseline(
            config: &'static Self::Config,
            facts: BoardFacts,
            board: &impl AcpiDevice<Config = ()>,
            base: u64,
        ) -> Result<Vec<u8>, String> {
            use fstart_driver_intel::{haswell::Haswell, lynxpoint::LynxPoint};
            let north = Haswell::new_from_config(&config.northbridge)
                .map_err(|e| format!("Haswell config: {e:?}"))?;
            let mut south = LynxPoint::new_from_config(&config.southbridge)
                .map_err(|e| format!("Lynx Point config: {e:?}"))?;
            south.set_host_acpi_baseline();
            south.set_s3_enabled(facts.memory_cache);
            assemble_chipset(&north, &south, 1, board, base)
        }
    }

    impl HostAcpiChipset for crate::skylake::SkylakeSunrisePoint {
        fn assemble_baseline(
            config: &'static Self::Config,
            _facts: BoardFacts,
            board: &impl AcpiDevice<Config = ()>,
            base: u64,
        ) -> Result<Vec<u8>, String> {
            use fstart_driver_intel::{
                skylake::driver::IntelSkylake, sunrise_point::driver::IntelSunrisePoint,
            };
            let mut north = IntelSkylake::new_from_config(config.northbridge)
                .map_err(|e| format!("Skylake config: {e:?}"))?;
            north.set_host_acpi_baseline();
            let mut south = IntelSunrisePoint::new_from_config(config.southbridge)
                .map_err(|e| format!("Sunrise Point config: {e:?}"))?;
            south.set_host_acpi_baseline();
            assemble_chipset(&north, &south, IntelSkylake::HOST_CPUS as u32, board, base)
        }
    }
    /// Assemble one baseline ACPI table image for any Intel board.
    pub fn baseline<B: BoardAcpi>(base: u64) -> Result<Vec<u8>, String>
    where
        B::Platform: HostAcpiChipset,
    {
        let board = B::acpi_baseline();
        <B::Platform as HostAcpiChipset>::assemble_baseline(B::CONFIG, B::FACTS, &board, base)
    }
}

#[cfg(all(feature = "host", not(target_os = "none")))]
pub use host_baseline::{HostAcpiChipset, baseline};
