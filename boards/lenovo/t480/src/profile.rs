// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 flash layout and Skylake/Kaby Lake-U + Sunrise Point-LP policy.
//!
//! Wiring from coreboot `mainboard/lenovo/sklkbl_thinkpad/{Kconfig,devicetree.cb}`
//! and `variants/t480/{overridetree.cb,gma-mainboard.ads,memory_init_params.c}`.

use fstart_core::{
    ConstVec, FlashLayout, IntelIfdFlashLayout, IntelIfdRegion as Kind,
    IntelIfdRegionConfig as Region,
};
use fstart_driver_intel::{
    gma, igd,
    skylake::{
        activation::PowerDown,
        additional_steps::{OltmOverride, OltmPolicy},
        cold_controller::Policy as Controller,
        driver as nb,
        early_sa::EcamConfig,
        jedec::BoardType,
        memory_map,
        overrides::PostSpdOverrides,
        package_power,
        phy::PhyPolicy,
        sa_dmi, sa_graphics, sa_peg, sa_post_mem, uncore_prmrr,
    },
    sunrise_point::{dmi, driver as sb, hda, irq, lpc, me, pmc, power, spi},
};
use fstart_platform_intel::{
    facts::{BoardFacts, IntelBoardFacts, PciHotplugPort, PciSubsystemId},
    skylake::{AcpiTimer, CpuPolicy, SkylakeSunrisePoint, SkylakeSunrisePointConfig},
};

/// 16 MiB SPI flash with standard ThinkPad T480 IFD regions (9 MiB BIOS region
/// matching coreboot `BOARD_ROMSIZE_KB_16384`, `MAINBOARD_USES_IFD_GBE_REGION`,
/// and `CBFS_SIZE = 0x900000`).
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
                size: 0x6fd000,
            })
            .push(Region {
                kind: Kind::Bios,
                offset: 0x700000,
                size: 0x900000,
            }),
    )
};

pub const ECAM: EcamConfig = EcamConfig {
    base: 0xe000_0000,
    buses: 256,
};

const DEVICES: sa_peg::Devices = sa_peg::Devices {
    peg: [false; 3],
    igd: true,
    thermal: true,
    imaging: false,
    chap: false,
    gmm: false,
};

const DMI: sa_dmi::Policy = sa_dmi::Policy {
    pch: dmi::Kind::Lp,
    ecam: ECAM,
    hsio_gen1_only: false,
    vc1: false,
    poll_budget_ms: 100,
};

/// Two DDR4 SO-DIMM slots: channel 0 DIMM 0 at 0x50, channel 1 DIMM 0 at 0x51.
pub const SPD_ADDRESSES: [[u8; 2]; 2] = [[0x50, 0], [0x51, 0]];

/// Fixed LPC decodes for MEC1653 (0x4e/0x4f), ACPI EC0 (0x62/0x66), i8042 KBC
/// (0x60/0x64), plus generic windows for H8 aux/EC1/EC3 (0x1600/0x80) and PMH7
/// (0x15e0/0x10).
pub const LPC_DECODES: lpc::DecodeConfig = lpc::DecodeConfig {
    io_decode: 0,
    io_enable: lpc::IoEnable::EC_4E::SET.value
        | lpc::IoEnable::EC::SET.value
        | lpc::IoEnable::KBC::SET.value,
    generic_io: [Some((0x1600, 0x80)), Some((0x15e0, 0x10)), None, None],
    generic_memory: None,
};

pub static NORTHBRIDGE: nb::Config = nb::Config {
    ecam: ECAM,
    spd_addresses: SPD_ADDRESSES,
    phy: PhyPolicy {
        board: BoardType::UltUlx,
        dq_pins_interleaved: true,
        ddp_shared_clock: false,
        vdd_mv: 1200,
        vccio_mv: 950,
        rcomp_resistor: [200, 81, 162],
        rcomp_target: [100, 40, 40, 23, 40],
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
    // DDR4-2400 ceiling for Kaby Lake-R; silicon fuses and DIMM SPD further cap
    // Kaby Lake-U CPUs (DDR4-2133) and slower modules automatically.
    platform_tck_min_fs: 833_333,
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
    pavp_enabled: true,
    package_power: package_power::Policy::NATIVE,
    display_audio: Some((true, true)),
    display: Some(igd::IgdDisplayPolicy {
        // coreboot variants/t480/gma-mainboard.ads: internal eDP panel plus
        // DP1/DP2 and HDMI1/HDMI2 external outputs.
        outputs: &[
            gma::OutputConfig {
                port: gma::types::Port::Edp,
                enabled: true,
            },
            gma::OutputConfig {
                port: gma::types::Port::DpB,
                enabled: true,
            },
            gma::OutputConfig {
                port: gma::types::Port::DpC,
                enabled: true,
            },
            gma::OutputConfig {
                port: gma::types::Port::HdmiB,
                enabled: true,
            },
            gma::OutputConfig {
                port: gma::types::Port::HdmiC,
                enabled: true,
            },
        ],
        framebuffer: gma::FramebufferConfig {
            scaling: gma::scaler::ScalingPolicy::PreserveAspect,
            ..gma::FramebufferConfig::edid(gma::FallbackMode {
                width: 1920,
                height: 1080,
                refresh_hz: 60,
            })
        },
    }),
    vbt: igd::VbtSource::ffs("data.vbt"),
    cpu: CpuPolicy {
        eist: true,
        aesni_lock: true,
        tcc_offset: 0,
        acpi_timer: AcpiTimer::Hardware,
    },
    reservations: &[],
    pci_io: (0x2000, 0x10000),
};

// Enabled bus-0 PCH devices requiring interrupt routing:
// xHCI (14.0), PCH thermal (14.2), SATA (17.0),
// PCIe RP1 (1c.0), RP5 (1c.4), RP7 (1c.6), RP9 (1d.0), RP11 (1d.2),
// HDA (1f.3), SMBus (1f.4), GbE (1f.6).
static INTERRUPTS: [u8; 11] = [
    0xa0, 0xa2, 0xb8, 0xe0, 0xe4, 0xe6, 0xe8, 0xea, 0xfb, 0xfc, 0xfe,
];

pub static SOUTHBRIDGE: sb::Config = sb::Config {
    ecam: ECAM,
    decodes: LPC_DECODES,
    smbus_base: 0xefa0,
    heci1_base: 0xfed1_a000,
    dci_enabled: false,
    intrusion_smi: false,
    legacy: lpc::LegacyPolicy {
        serial_irq: lpc::SerialIrq::Continuous,
        pcat: true,
        interrupts: irq::Policy::NATIVE,
    },
    interrupt_devices: &INTERRUPTS,
    power: power::Policy {
        after_power_loss: pmc::AfterPowerLoss::Off,
        gpe: crate::gpio::GPE_GROUPS,
        s3_assert: power::S3Assert::Ms50,
        s4_assert: power::S4Assert::Seconds1,
        sus_assert: power::SusAssert::Ms500,
        a_assert: power::AAssert::Seconds2,
        power_cycle: power::PowerCycle::Seconds4,
        stretch_after_suspend_loss: false,
        button_override: 0,
        s0ix: false,
        slp_s0_vm: false,
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
    laptop: true,
    pads: crate::gpio::PAD_TABLE,
};

/// `subsystemid 0x17aa 0x225d inherit` from `sklkbl_thinkpad/devicetree.cb`.
const SUBSYSTEM_IDS: &[PciSubsystemId] = {
    const fn t480(device: u8, function: u8) -> PciSubsystemId {
        PciSubsystemId::new(device, function, 0x17aa, 0x225d)
    }
    &[
        t480(0x00, 0),
        t480(0x02, 0),
        t480(0x04, 0),
        t480(0x14, 0),
        t480(0x14, 2),
        t480(0x16, 0),
        t480(0x17, 0),
        t480(0x1c, 0),
        t480(0x1c, 4),
        t480(0x1c, 6),
        t480(0x1d, 0),
        t480(0x1d, 2),
        t480(0x1f, 0),
        t480(0x1f, 2),
        t480(0x1f, 3),
        t480(0x1f, 4),
        t480(0x1f, 6),
    ]
};

/// PCH RP9 (`00:1d.0`) feeds the Alpine Ridge Thunderbolt 3 controller and its
/// hotplug-capable downstream switch port. Reserve 64 GiB of 64-bit
/// prefetchable MMIO above 4 GiB for Thunderbolt eGPUs and docks.
const HOTPLUG_PORTS: &[PciHotplugPort] = &[PciHotplugPort::bdf(0, 0x1d, 0)
    .with_resources(fstart_platform_intel::facts::PciHotplugResources::THUNDERBOLT)];

pub static PLATFORM: SkylakeSunrisePointConfig =
    SkylakeSunrisePointConfig::new(&NORTHBRIDGE, &SOUTHBRIDGE, 8, SUBSYSTEM_IDS)
        .with_pci_hotplug_ports(HOTPLUG_PORTS);

impl IntelBoardFacts for crate::Board {
    type Platform = SkylakeSunrisePoint;
    const CONFIG: &'static SkylakeSunrisePointConfig = &PLATFORM;
    const FACTS: BoardFacts = BoardFacts::new(FlashLayout::IntelIfd(FLASH), 0x100_0000)
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
    }

    #[test]
    fn vbt_asset_has_valid_header_signature() {
        assert_eq!(&include_bytes!("../data.vbt")[..4], b"$VBT");
    }
}
