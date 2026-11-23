// SPDX-License-Identifier: MIT OR Apache-2.0
//! The M900 Tiny's flash layout and chipset policy.
//!
//! Wiring: devicetree.cb SHA256
//! 4e865152d729b563a227b863c37fe55a4c991acdef5fc0251c6d60f1bb07d8a1.
//! BIOS/ME/GbE geometry: reference fmap-ifd.fmd. Electrical/map defaults come
//! from native romstage_native.c, power_model.c and memory_map_native.c, not
//! the reference lab build's enlarged CAR/stack or placeholder SMBIOS serial.
use fstart_core::{
    ConstVec, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion as Kind,
    IntelIfdRegionConfig as Region,
};
use fstart_driver_intel::{
    skylake::{
        activation::PowerDown,
        additional_steps::{OltmOverride, OltmPolicy},
        cold_controller::Policy as Controller,
        driver as nb,
        early_sa::EcamConfig,
        memory_map,
        overrides::PostSpdOverrides,
        phy::PhyPolicy,
        sa_dmi, sa_graphics, sa_peg, sa_post_mem, uncore_prmrr,
    },
    sunrise_point::{driver as sb, hda, irq, lpc, me, power, spi},
};
use fstart_platform_intel::{
    facts::{BoardFacts, IntelBoardFacts},
    skylake::{AcpiTimer, CpuPolicy, SkylakeSunrisePoint, SkylakeSunrisePointConfig},
};

pub const FLASH: IntelIfdFlashLayout = {
    let fd = Region {
        kind: Kind::Descriptor,
        offset: 0,
        size: 0x1000,
    };
    IntelIfdFlashLayout::new(
        ConstVec::new(fd)
            .push(fd)
            .push(Region {
                kind: Kind::Gbe,
                offset: 0x1000,
                size: 0x2000,
            })
            .push(Region {
                kind: Kind::Me,
                offset: 0x3000,
                size: 0x7fd000,
            })
            .push(Region {
                kind: Kind::Bios,
                offset: 0x800000,
                size: 0x800000,
            }),
    )
};
pub const ECAM: EcamConfig = EcamConfig {
    base: 0xe0000000,
    buses: 256,
};
const DEVICES: sa_peg::Devices = sa_peg::Devices {
    peg: [false; 3],
    igd: true,
    thermal: true,
    imaging: false,
    chap: false,
    gmm: true,
};
const DMI: sa_dmi::Policy = sa_dmi::Policy {
    pch: fstart_driver_intel::sunrise_point::dmi::Kind::H,
    ecam: ECAM,
    hsio_gen1_only: false,
    vc1: true,
    poll_budget_ms: 100,
};
pub const SPD_ADDRESSES: [[u8; 2]; 2] = [[0x50, 0], [0x52, 0]];

pub const PACKAGE_POWER: fstart_driver_intel::skylake::package_power::Policy =
    fstart_driver_intel::skylake::package_power::Policy {
        pl2_watts: core::num::NonZeroU16::new(65),
        ..fstart_driver_intel::skylake::package_power::Policy::NATIVE
    };

/// Fixed LPC decodes for COM1 and the PnP configuration port, plus distinct
/// 8-byte generic windows for the NCT6687D ACPI and EC LDNs.
pub const LPC_DECODES: lpc::DecodeConfig = lpc::DecodeConfig {
    io_decode: lpc::IoDecode::COM_A.val(0).value, // 3f8
    io_enable: lpc::IoEnable::SUPERIO_2E::SET.value | lpc::IoEnable::COM_A::SET.value,
    generic_io: [
        Some((crate::superio::ACPI_BASE, 8)),
        Some((crate::superio::EC_BASE, 8)),
        None,
        None,
    ],
    generic_memory: None,
};

pub static NORTHBRIDGE: nb::Config = nb::Config {
    ecam: ECAM,
    spd_addresses: SPD_ADDRESSES,
    phy: PhyPolicy {
        // Native romstage selects Desktop for PCH-H, despite the SO-DIMM sockets.
        board: fstart_driver_intel::skylake::jedec::BoardType::Desktop,
        dq_pins_interleaved: true,
        ddp_shared_clock: false,
        vdd_mv: 1200,
        vccio_mv: 950,
        rcomp_resistor: [121, 75, 100],
        rcomp_target: [50, 26, 20, 20, 26],
        vtt_odt: false,
    },
    overrides: PostSpdOverrides {
        ecc_support: false,
        rank_interleave: true,
        enhanced_interleave: true,
        vdd_voltage_done: false,
        gdxc_enable: false,
        gdxc_mot_size: 0,
        gdxc_iot_size: 0,
        lpddr_dram_odt: false,
    },
    // Authored DDR4-2133 ceiling, NOT a measured training rate. Actual SPD and
    // silicon fuses further restrict it; no overclock/XMP policy is inferred.
    platform_tck_min_fs: 937_500,
    dram_odt: true,
    ca_vref_config: 2,
    dummy_reads: false,
    thermal: OltmPolicy {
        override_mode: OltmOverride::OpenLoop,
        enable_oltm: false,
        enable_cltm: false,
        refresh_2x: 0,
        user_power_weights: false,
        ddr_thermal_sensor: true,
    },
    // Source memory_map_native.c: 2-GiB MMIO hole; source TSEG is 8 MiB.
    memory_map: memory_map::Policy {
        tseg_mib: 8,
        mmio_mib: 2048,
    },
    prmrr: uncore_prmrr::Policy {
        features_enabled: false,
        maximum_mib: 0,
    },
    controller: Controller::native_defaults(PowerDown::Auto),
    dmi: DMI,
    peg: sa_peg::Policy {
        devices: DEVICES,
        max_width: [0; 3],
        poll_ms: 100,
        disable_polls: 1000,
    },
    graphics: sa_graphics::Policy {
        igd_enabled: true,
        primary: sa_graphics::PrimaryDisplay::Integrated,
        stolen: 2,
        aperture: 1,
        skip_external_scan: false,
    },
    post_memory: sa_post_mem::Policy {
        dmi: DMI,
        devices: DEVICES,
        peg_max_payload: [0; 3],
        legacy_io_low_latency: false,
        dmi_extended_sync: false,
        dmi_iot: false,
    },
    cio2_enabled: false,
    pavp_enabled: false,
    package_power: PACKAGE_POWER,
    // Native HDA requests 96 MHz / 2T through the scoped graphics binding.
    display_audio: Some((true, true)),
    display: Some(fstart_driver_intel::igd::IgdDisplayPolicy {
        // coreboot gma-mainboard.ads: DP1/DP2 and the optional DP3 header,
        // with their paired HDMI/DDC paths. The M900 has no internal panel.
        outputs: &[
            fstart_driver_intel::gma::OutputConfig {
                port: fstart_driver_intel::gma::types::Port::DpB,
                enabled: true,
            },
            fstart_driver_intel::gma::OutputConfig {
                port: fstart_driver_intel::gma::types::Port::DpC,
                enabled: true,
            },
            fstart_driver_intel::gma::OutputConfig {
                port: fstart_driver_intel::gma::types::Port::DpD,
                enabled: true,
            },
            fstart_driver_intel::gma::OutputConfig {
                port: fstart_driver_intel::gma::types::Port::HdmiB,
                enabled: true,
            },
            fstart_driver_intel::gma::OutputConfig {
                port: fstart_driver_intel::gma::types::Port::HdmiC,
                enabled: true,
            },
            fstart_driver_intel::gma::OutputConfig {
                port: fstart_driver_intel::gma::types::Port::HdmiD,
                enabled: true,
            },
        ],
        framebuffer: fstart_driver_intel::gma::FramebufferConfig {
            scaling: fstart_driver_intel::gma::scaler::ScalingPolicy::PreserveAspect,
            ..fstart_driver_intel::gma::FramebufferConfig::edid(
                fstart_driver_intel::gma::FallbackMode {
                    width: 1024,
                    height: 768,
                    refresh_hz: 60,
                },
            )
        },
    }),
    vbt: fstart_driver_intel::igd::VbtSource::ffs("data.vbt"),
    cpu: CpuPolicy {
        eist: true,
        aesni_lock: true,
        tcc_offset: 0,
        acpi_timer: AcpiTimer::Hardware,
    },
    reservations: &[],
    // Authored PCI allocator arena excludes fixed low LPC/ACPI/TCO resources;
    // SMBus's fixed BAR is separately reserved by the canonical allocator.
    pci_io: (0x2000, 0x10000),
};
// Original physical RP7 (1c.6) and RP17 (1b.0), before function swapping.
// UART2 is SkipInit, not an IRQ-managed PCI controller.
// Native devintconfig for the enabled devices, including the SkipInit
// LPSS UART2 (D25:F0, INTA -> IRQ 32) the OS drives as ttyS1.
static INTERRUPTS: [u8; 10] = [0xa0, 0xa2, 0xb0, 0xb8, 0xc8, 0xd8, 0xe6, 0xfb, 0xfc, 0xfe];
pub static SOUTHBRIDGE: sb::Config = sb::Config {
    ecam: ECAM,
    decodes: LPC_DECODES,
    smbus_base: 0xefa0,
    heci1_base: 0xfed1a000,
    // Do not inherit the reference lab image's host DCI permission.
    dci_enabled: false,
    intrusion_smi: false,
    legacy: lpc::LegacyPolicy {
        serial_irq: lpc::SerialIrq::Continuous,
        pcat: true,
        interrupts: irq::Policy::NATIVE,
    },
    interrupt_devices: &INTERRUPTS,
    power: power::Policy {
        after_power_loss: fstart_driver_intel::sunrise_point::pmc::AfterPowerLoss::Off,
        gpe: crate::gpio::GPE_GROUPS,
        s3_assert: power::S3Assert::Ms50,
        s4_assert: power::S4Assert::Seconds4,
        sus_assert: power::SusAssert::Seconds4,
        a_assert: power::AAssert::Seconds2,
        power_cycle: power::PowerCycle::Seconds4,
        stretch_after_suspend_loss: false,
        button_override: 0,
        s0ix: false,
        wol_override: false,
        wowlan: false,
        wowlan_deep: false,
        deep_sleep: power::DeepSleep::S5AcDc,
        lan_wake: true,
        disable_ac_present_pulldown: false,
        wake_pin: true,
        disable_pmc_data_bar: false,
    },
    me: me::Policy {
        heci2: false,
        heci3_without_payload: false,
        amt_sol: false,
        heci1_visible_to_os: true,
    },
    hda: hda::Policy::Enabled {
        dsp: hda::Dsp::Disabled,
        vc1: false,
        verbs: &crate::hda::CONFIG,
    },
    sata: crate::sata::POLICY,
    // Keep the flash writable from the OS so it can be updated in band.
    disable_spi_write_status: false,
    spi_visible_to_os: true,
    bios_writes: spi::BiosWrites::Open,
    usb: &crate::usb::CONFIG,
    pcie: &crate::pcie::CONFIG,
    thermal: crate::pch::THERMAL,
    lan: crate::pch::LAN,
    auxiliary: &crate::pch::AUXILIARY,
    interconnect: crate::pch::interconnect_policy(false),
    s5_delay_ms: 0,
    laptop: false,
    pads: crate::gpio::PAD_TABLE,
};
pub static PLATFORM: SkylakeSunrisePointConfig =
    SkylakeSunrisePointConfig::new(&NORTHBRIDGE, &SOUTHBRIDGE, 8, &[]);
impl IntelBoardFacts for crate::Board {
    type Platform = SkylakeSunrisePoint;
    const CONFIG: &'static SkylakeSunrisePointConfig = &PLATFORM;
    const FACTS: BoardFacts = BoardFacts::new(FlashLayout::IntelIfd(FLASH), 0x1000000)
        .with_data_assets(&["data.vbt"])
        .with_memory_cache();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chipset_policy_is_valid() {
        nb::IntelSkylake::validate_config(&NORTHBRIDGE).unwrap();
        sb::IntelSunrisePoint::validate_config(&SOUTHBRIDGE).unwrap();
        fstart_driver_superio::nct6687d::hwm::validate(&crate::hwm::CONFIG).unwrap();
    }
}
